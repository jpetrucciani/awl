use aws_sdk_lambda::types::{InvocationType, LogType};
use base64::Engine;

use crate::cli::{
    Globals, LambdaCommand, LambdaInvocationType, LambdaInvokeArgs, LambdaSubcommand,
};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};
use crate::services::common;

pub async fn run(globals: &Globals, command: &LambdaCommand) -> Result<()> {
    let context = AwsContext::new(globals).await?;
    let client = context.lambda();
    match &command.command {
        LambdaSubcommand::Ls(args) => {
            let limit = args.limit.unwrap_or(usize::MAX);
            let mut rows = Vec::new();
            let mut pages = client.list_functions().into_paginator().send();
            while rows.len() < limit {
                let Some(page) = pages.next().await else {
                    break;
                };
                rows.extend(
                    page.map_err(AwlError::sdk)?
                        .functions()
                        .iter()
                        .map(|function| {
                            serde_json::json!({
                                "function_name": function.function_name(),
                                "runtime": function.runtime().map(|runtime| runtime.as_str()),
                                "memory_size": function.memory_size(),
                                "timeout": function.timeout(),
                                "last_modified": function.last_modified(),
                                "state": function.state().map(|state| state.as_str()),
                                "function_arn": function.function_arn(),
                            })
                        }),
                );
            }
            rows.truncate(limit);
            output::emit(globals, Output::Many(rows))
        }
        LambdaSubcommand::Invoke(args) => invoke(globals, &client, args).await,
        LambdaSubcommand::Logs(args) => {
            let group = format!("/aws/lambda/{}", args.function);
            crate::services::logs::tail(globals, &context.logs(), &group, &args.options).await
        }
    }
}

async fn invoke(
    globals: &Globals,
    client: &aws_sdk_lambda::Client,
    args: &LambdaInvokeArgs,
) -> Result<()> {
    let payload = match (&args.payload_file, &args.payload) {
        (Some(path), _) => tokio::fs::read(path).await?,
        (None, Some(payload)) => common::stdin_or_bytes(payload)?,
        (None, None) => b"{}".to_vec(),
    };
    let invocation_type = match args.invocation_type {
        LambdaInvocationType::RequestResponse => InvocationType::RequestResponse,
        LambdaInvocationType::Event => InvocationType::Event,
        LambdaInvocationType::DryRun => InvocationType::DryRun,
    };

    let response = client
        .invoke()
        .function_name(&args.function)
        .invocation_type(invocation_type)
        .log_type(if args.tail {
            LogType::Tail
        } else {
            LogType::None
        })
        .payload(aws_smithy_types::Blob::new(payload))
        .set_qualifier(args.qualifier.clone())
        .send()
        .await
        .map_err(AwlError::sdk)?;
    let payload = response.payload().map(|blob| {
        let bytes = blob.as_ref();
        serde_json::from_slice::<serde_json::Value>(bytes).unwrap_or_else(|_| {
            serde_json::Value::String(String::from_utf8_lossy(bytes).to_string())
        })
    });
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "status_code": response.status_code(),
            "executed_version": response.executed_version(),
            "function_error": response.function_error(),
            "log_result": response.log_result().and_then(|value| {
                base64::engine::general_purpose::STANDARD
                    .decode(value)
                    .ok()
                    .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
            }),
            "payload": payload,
        }))?,
    )
}
