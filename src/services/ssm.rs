use std::collections::HashMap;
use std::process::Command;

use crate::cli::{Globals, SsmCommand, SsmCommandSubcommand, StubArgs};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};
use crate::services::common;

pub async fn run(globals: &Globals, command: &SsmCommand) -> Result<()> {
    let context = AwsContext::new(globals).await?;
    let client = context.ssm();
    match &command.command {
        SsmCommandSubcommand::Get(args) => get(globals, &client, args).await,
        SsmCommandSubcommand::Ls(args) => ls(globals, &client, args).await,
        SsmCommandSubcommand::Put(args) => put(globals, &client, args).await,
        SsmCommandSubcommand::Rm(args) => {
            let name = common::required(&common::positional(&args.args), 0, "name")?;
            client
                .delete_parameter()
                .name(&name)
                .send()
                .await
                .map_err(AwlError::aws)?;
            output::emit(
                globals,
                Output::one(serde_json::json!({ "name": name, "deleted": true }))?,
            )
        }
        SsmCommandSubcommand::Exec(args) => exec(&client, args).await,
    }
}

async fn get(globals: &Globals, client: &aws_sdk_ssm::Client, args: &StubArgs) -> Result<()> {
    let name = common::required(&common::positional(&args.args), 0, "name")?;
    let decrypt = !common::has_flag(&args.args, "--no-decrypt");
    let response = client
        .get_parameter()
        .name(name)
        .with_decryption(decrypt)
        .send()
        .await
        .map_err(AwlError::aws)?;
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

async fn ls(globals: &Globals, client: &aws_sdk_ssm::Client, args: &StubArgs) -> Result<()> {
    let prefix = common::positional(&args.args)
        .first()
        .cloned()
        .unwrap_or_else(|| "/".to_owned());
    let recursive =
        common::has_flag(&args.args, "--recursive") || common::has_flag(&args.args, "-r");
    let values = common::has_flag(&args.args, "--values");
    let request = client
        .get_parameters_by_path()
        .path(prefix)
        .recursive(recursive)
        .with_decryption(values);
    let response = request.send().await.map_err(AwlError::aws)?;
    let rows = response
        .parameters()
        .iter()
        .map(|parameter| {
            serde_json::json!({
                "name": parameter.name(),
                "type": parameter.r#type().map(|t| t.as_str()),
                "value": if values { parameter.value() } else { None },
                "version": parameter.version(),
            })
        })
        .collect::<Vec<_>>();
    output::emit(globals, Output::Many(rows))
}

async fn put(globals: &Globals, client: &aws_sdk_ssm::Client, args: &StubArgs) -> Result<()> {
    let positional = common::positional(&args.args);
    let name = common::required(&positional, 0, "name")?;
    let value = common::required(&positional, 1, "value")?;
    let parameter_type =
        common::option_value(&args.args, "--type").unwrap_or_else(|| "String".to_owned());
    let mut request = client
        .put_parameter()
        .name(name.clone())
        .value(value)
        .r#type(aws_sdk_ssm::types::ParameterType::from(
            parameter_type.as_str(),
        ))
        .overwrite(common::has_flag(&args.args, "--overwrite"));
    if let Some(kms_key) = common::option_value(&args.args, "--kms-key")
        .or_else(|| std::env::var("AWL_SSM_KMS_KEY").ok())
    {
        request = request.key_id(kms_key);
    }
    let response = request.send().await.map_err(AwlError::aws)?;
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "name": name,
            "version": response.version(),
        }))?,
    )
}

async fn exec(client: &aws_sdk_ssm::Client, args: &StubArgs) -> Result<()> {
    let prefix = common::option_value(&args.args, "--prefix")
        .or_else(|| std::env::var("AWL_SSM_EXEC_PREFIX").ok())
        .ok_or_else(|| AwlError::Usage {
            message: "ssm exec requires --prefix or AWL_SSM_EXEC_PREFIX".to_owned(),
        })?;
    let marker = args
        .args
        .iter()
        .position(|arg| arg == "--")
        .ok_or_else(|| AwlError::Usage {
            message: "ssm exec requires -- <cmd> [args...]".to_owned(),
        })?;
    let command = args
        .args
        .get(marker + 1)
        .cloned()
        .ok_or_else(|| AwlError::Usage {
            message: "ssm exec requires a command after --".to_owned(),
        })?;
    let command_args = args
        .args
        .iter()
        .skip(marker + 2)
        .cloned()
        .collect::<Vec<_>>();
    let strict = common::has_flag(&args.args, "--strict");
    let response = client
        .get_parameters_by_path()
        .path(prefix.clone())
        .recursive(true)
        .with_decryption(true)
        .send()
        .await
        .map_err(AwlError::aws)?;
    let params = response.parameters();
    if strict && params.is_empty() {
        return Err(AwlError::NotFound {
            message: format!("no parameters found under {prefix:?}"),
        });
    }

    let mut envs = HashMap::new();
    for parameter in params {
        let Some(name) = parameter.name() else {
            continue;
        };
        let Some(value) = parameter.value() else {
            return Err(AwlError::NotFound {
                message: format!("parameter {name:?} has no readable value"),
            });
        };
        let env_name = normalize_env_name(&prefix, name)?;
        if envs.insert(env_name.clone(), value.to_owned()).is_some() {
            return Err(AwlError::Usage {
                message: format!("parameter names collide after env normalization: {env_name}"),
            });
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = Command::new(command).args(command_args).envs(envs).exec();
        Err(AwlError::Io(error))
    }

    #[cfg(not(unix))]
    {
        let status = Command::new(command)
            .args(command_args)
            .envs(envs)
            .status()?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

fn normalize_env_name(prefix: &str, name: &str) -> Result<String> {
    let stripped = name
        .strip_prefix(prefix.trim_end_matches('/'))
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
