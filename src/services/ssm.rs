use std::collections::HashMap;
use std::process::Command;

use aws_sdk_ssm::types::{Parameter, ParameterType};

use crate::cli::{
    Globals, SsmCommand, SsmExecArgs, SsmLsArgs, SsmParameterType, SsmPutArgs, SsmSubcommand,
};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};
use crate::services::common;

pub async fn run(globals: &Globals, command: &SsmCommand) -> Result<()> {
    let context = AwsContext::new(globals).await?;
    let client = context.ssm();
    match &command.command {
        SsmSubcommand::Get(args) => {
            let response = client
                .get_parameter()
                .name(&args.name)
                .with_decryption(!args.no_decrypt)
                .send()
                .await
                .map_err(AwlError::sdk)?;
            let parameter = response.parameter();
            output::emit(
                globals,
                Output::one(serde_json::json!({
                    "name": parameter.and_then(|p| p.name()),
                    "type": parameter.and_then(|p| p.r#type()).map(|t| t.as_str()),
                    "value": parameter.and_then(|p| p.value()),
                    "version": parameter.map(|p| p.version()),
                }))?,
            )
        }
        SsmSubcommand::Ls(args) => ls(globals, &client, args).await,
        SsmSubcommand::Put(args) => put(globals, &client, args).await,
        SsmSubcommand::Rm(args) => {
            client
                .delete_parameter()
                .name(&args.name)
                .send()
                .await
                .map_err(AwlError::sdk)?;
            output::ack(
                globals,
                Output::one(serde_json::json!({ "name": args.name, "deleted": true }))?,
            )
        }
        SsmSubcommand::Exec(args) => exec(&client, args).await,
    }
}

async fn parameters_by_path(
    client: &aws_sdk_ssm::Client,
    path: &str,
    recursive: bool,
    decrypt: bool,
) -> Result<Vec<Parameter>> {
    let mut out = Vec::new();
    let mut pages = client
        .get_parameters_by_path()
        .path(path)
        .recursive(recursive)
        .with_decryption(decrypt)
        .into_paginator()
        .send();
    while let Some(page) = pages.next().await {
        out.extend(page.map_err(AwlError::sdk)?.parameters().iter().cloned());
    }
    Ok(out)
}

async fn ls(globals: &Globals, client: &aws_sdk_ssm::Client, args: &SsmLsArgs) -> Result<()> {
    let rows = parameters_by_path(client, &args.path, args.recursive, args.values)
        .await?
        .iter()
        .map(|parameter| {
            serde_json::json!({
                "name": parameter.name(),
                "type": parameter.r#type().map(|t| t.as_str()),
                "value": if args.values { parameter.value() } else { None },
                "version": parameter.version(),
            })
        })
        .collect::<Vec<_>>();
    output::emit(globals, Output::Many(rows))
}

async fn put(globals: &Globals, client: &aws_sdk_ssm::Client, args: &SsmPutArgs) -> Result<()> {
    let parameter_type = match args.parameter_type {
        SsmParameterType::String => ParameterType::String,
        SsmParameterType::StringList => ParameterType::StringList,
        SsmParameterType::SecureString => ParameterType::SecureString,
    };
    let response = client
        .put_parameter()
        .name(&args.name)
        .value(common::stdin_or_string(&args.value)?)
        .r#type(parameter_type)
        .overwrite(args.overwrite)
        .set_key_id(args.kms_key.clone())
        .send()
        .await
        .map_err(AwlError::sdk)?;
    output::ack(
        globals,
        Output::one(serde_json::json!({
            "name": args.name,
            "version": response.version(),
        }))?,
    )
}

async fn exec(client: &aws_sdk_ssm::Client, args: &SsmExecArgs) -> Result<()> {
    let params = parameters_by_path(client, &args.prefix, true, true).await?;
    if args.strict && params.is_empty() {
        return Err(AwlError::NotFound {
            message: format!("no parameters found under {:?}", args.prefix),
        });
    }

    let mut envs = HashMap::new();
    for parameter in &params {
        let Some(name) = parameter.name() else {
            continue;
        };
        let Some(value) = parameter.value() else {
            return Err(AwlError::NotFound {
                message: format!("parameter {name:?} has no readable value"),
            });
        };
        let env_name = normalize_env_name(&args.prefix, name)?;
        if envs.insert(env_name.clone(), value.to_owned()).is_some() {
            return Err(AwlError::Usage {
                message: format!("parameter names collide after env normalization: {env_name}"),
            });
        }
    }

    let (program, program_args) = args.command.split_first().ok_or_else(|| AwlError::Usage {
        message: "ssm exec requires a command after --".to_owned(),
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = Command::new(program).args(program_args).envs(envs).exec();
        Err(AwlError::Io(error))
    }

    #[cfg(not(unix))]
    {
        let status = Command::new(program)
            .args(program_args)
            .envs(envs)
            .status()?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

fn normalize_env_name(prefix: &str, name: &str) -> Result<String> {
    let prefix = prefix.trim_end_matches('/');
    let stripped = name
        .strip_prefix(prefix)
        .filter(|rest| rest.starts_with('/'))
        .unwrap_or(name)
        .trim_start_matches('/');
    let out = stripped
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    if out.is_empty() {
        Err(AwlError::Usage {
            message: format!("parameter {name:?} normalizes to an empty env name"),
        })
    } else {
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn env_names_are_relative_to_the_prefix() {
        assert_eq!(
            crate::services::ssm::normalize_env_name("/app/", "/app/db/password").expect("valid"),
            "DB_PASSWORD"
        );
    }
}
