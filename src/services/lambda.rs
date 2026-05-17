use base64::Engine;

use crate::cli::{Globals, LambdaCommand, LambdaCommandSubcommand, StubArgs};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};
use crate::services::common;

pub async fn run(globals: &Globals, command: &LambdaCommand) -> Result<()> {
    let context = AwsContext::new(globals).await?;
    let client = context.lambda();
    match &command.command {
        LambdaCommandSubcommand::Ls(args) => ls(globals, &client, args).await,
        LambdaCommandSubcommand::Invoke(args) => invoke(globals, &client, args).await,
        LambdaCommandSubcommand::Logs(args) => logs(globals, &context.logs(), args).await,
    }
}

async fn ls(globals: &Globals, client: &aws_sdk_lambda::Client, args: &StubArgs) -> Result<()> {
    let mut request = client.list_functions();
    if let Some(marker) = common::option_value(&args.args, "--marker") {
        request = request.marker(marker);
    }
    if let Some(max_items) =
        common::option_value(&args.args, "--limit").and_then(|value| value.parse::<i32>().ok())
    {
        request = request.max_items(max_items);
    }

    let response = request.send().await.map_err(AwlError::aws)?;
    let rows = response
        .functions()
        .iter()
        .map(|function| {
            serde_json::json!({
                "function_name": function.function_name(),
                "function_arn": function.function_arn(),
                "runtime": function.runtime().map(|runtime| runtime.as_str()),
                "memory_size": function.memory_size(),
                "timeout": function.timeout(),
                "last_modified": function.last_modified(),
                "version": function.version(),
                "state": function.state().map(|state| state.as_str()),
            })
        })
        .collect::<Vec<_>>();
    output::emit(globals, Output::Many(rows))
}

async fn invoke(globals: &Globals, client: &aws_sdk_lambda::Client, args: &StubArgs) -> Result<()> {
    let positional = common::positional(&args.args);
    let function = common::required(&positional, 0, "function")?;
    let payload = match common::option_value(&args.args, "--payload-file") {
        Some(path) => tokio::fs::read(path).await?,
        None => positional
            .get(1)
            .cloned()
            .unwrap_or_else(|| "{}".to_owned())
            .into_bytes(),
    };
    let invocation_type = common::option_value(&args.args, "--type")
        .or_else(|| common::option_value(&args.args, "--invocation-type"))
        .unwrap_or_else(|| "RequestResponse".to_owned());
    let log_type = if common::has_flag(&args.args, "--tail") {
        "Tail".to_owned()
    } else {
        common::option_value(&args.args, "--log-type").unwrap_or_else(|| "None".to_owned())
    };

    let mut request = client
        .invoke()
        .function_name(function)
        .invocation_type(aws_sdk_lambda::types::InvocationType::from(
            invocation_type.as_str(),
        ))
        .log_type(aws_sdk_lambda::types::LogType::from(log_type.as_str()))
        .payload(aws_smithy_types::Blob::new(payload));
    if let Some(qualifier) = common::option_value(&args.args, "--qualifier") {
        request = request.qualifier(qualifier);
    }

    let response = request.send().await.map_err(AwlError::aws)?;
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

async fn logs(
    globals: &Globals,
    client: &aws_sdk_cloudwatchlogs::Client,
    args: &StubArgs,
) -> Result<()> {
    let positional = common::positional(&args.args);
    let function = common::required(&positional, 0, "function")?;
    let group = format!("/aws/lambda/{function}");
    let mut request = client.filter_log_events().log_group_name(group);
    if let Some(limit) =
        common::option_value(&args.args, "--limit").and_then(|value| value.parse::<i32>().ok())
    {
        request = request.limit(limit);
    }
    if let Some(pattern) = common::option_value(&args.args, "--filter") {
        request = request.filter_pattern(pattern);
    }
    let response = request.send().await.map_err(AwlError::aws)?;
    let rows = response
        .events()
        .iter()
        .map(|event| {
            serde_json::json!({
                "timestamp": event.timestamp(),
                "message": event.message(),
                "log_stream_name": event.log_stream_name(),
                "ingestion_time": event.ingestion_time(),
            })
        })
        .collect::<Vec<_>>();
    output::emit(globals, Output::Many(rows))
}
