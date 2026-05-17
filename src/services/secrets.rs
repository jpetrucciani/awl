use crate::cli::{Globals, SecretsCommand, SecretsCommandSubcommand, StubArgs};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};
use crate::services::common;

pub async fn run(globals: &Globals, command: &SecretsCommand) -> Result<()> {
    let context = AwsContext::new(globals).await?;
    let client = context.secrets();
    match &command.command {
        SecretsCommandSubcommand::Ls(_) => {
            let response = client.list_secrets().send().await.map_err(AwlError::aws)?;
            let rows = response
                .secret_list()
                .iter()
                .map(|secret| {
                    serde_json::json!({
                        "name": secret.name(),
                        "arn": secret.arn(),
                        "description": secret.description(),
                        "last_changed": secret.last_changed_date().map(ToString::to_string),
                    })
                })
                .collect::<Vec<_>>();
            output::emit(globals, Output::Many(rows))
        }
        SecretsCommandSubcommand::Get(args) => get(globals, &client, args).await,
        SecretsCommandSubcommand::Put(args) => put(globals, &client, args).await,
        SecretsCommandSubcommand::Rotate(args) => {
            let name = common::required(&common::positional(&args.args), 0, "name")?;
            let response = client
                .rotate_secret()
                .secret_id(name)
                .send()
                .await
                .map_err(AwlError::aws)?;
            output::emit(
                globals,
                Output::one(serde_json::json!({
                    "arn": response.arn(),
                    "name": response.name(),
                    "version_id": response.version_id(),
                }))?,
            )
        }
        SecretsCommandSubcommand::Rm(args) => {
            let positional = common::positional(&args.args);
            let name = common::required(&positional, 0, "name")?;
            let mut request = client.delete_secret().secret_id(name);
            if common::has_flag(&args.args, "--force") {
                request = request.force_delete_without_recovery(true);
            }
            let response = request.send().await.map_err(AwlError::aws)?;
            output::emit(
                globals,
                Output::one(serde_json::json!({
                    "arn": response.arn(),
                    "name": response.name(),
                    "deletion_date": response.deletion_date().map(ToString::to_string),
                }))?,
            )
        }
    }
}

async fn get(
    globals: &Globals,
    client: &aws_sdk_secretsmanager::Client,
    args: &StubArgs,
) -> Result<()> {
    let positional = common::positional(&args.args);
    let name = common::required(&positional, 0, "name")?;
    let mut request = client.get_secret_value().secret_id(name);
    if let Some(version_id) = common::option_value(&args.args, "--version-id") {
        request = request.version_id(version_id);
    }
    if let Some(stage) = common::option_value(&args.args, "--version-stage") {
        request = request.version_stage(stage);
    }
    let response = request.send().await.map_err(AwlError::aws)?;
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "arn": response.arn(),
            "name": response.name(),
            "version_id": response.version_id(),
            "secret_string": response.secret_string(),
        }))?,
    )
}

async fn put(
    globals: &Globals,
    client: &aws_sdk_secretsmanager::Client,
    args: &StubArgs,
) -> Result<()> {
    let positional = common::positional(&args.args);
    let name = common::required(&positional, 0, "name")?;
    let value = common::required(&positional, 1, "value")?;
    let request = client
        .put_secret_value()
        .secret_id(name)
        .secret_string(value);
    let response = request.send().await.map_err(AwlError::aws)?;
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "arn": response.arn(),
            "name": response.name(),
            "version_id": response.version_id(),
        }))?,
    )
}
