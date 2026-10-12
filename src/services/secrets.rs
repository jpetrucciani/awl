use crate::cli::{Globals, SecretsCommand, SecretsSubcommand};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};
use crate::services::common;

pub async fn run(globals: &Globals, command: &SecretsCommand) -> Result<()> {
    let context = AwsContext::new(globals).await?;
    let client = context.secrets();
    match &command.command {
        SecretsSubcommand::Ls => {
            let mut rows = Vec::new();
            let mut pages = client.list_secrets().into_paginator().send();
            while let Some(page) = pages.next().await {
                rows.extend(
                    page.map_err(AwlError::sdk)?
                        .secret_list()
                        .iter()
                        .map(|secret| {
                            serde_json::json!({
                                "name": secret.name(),
                                "arn": secret.arn(),
                                "description": secret.description(),
                                "last_changed": secret.last_changed_date().map(ToString::to_string),
                            })
                        }),
                );
            }
            output::emit(globals, Output::Many(rows))
        }
        SecretsSubcommand::Get(args) => {
            let response = client
                .get_secret_value()
                .secret_id(&args.name)
                .set_version_id(args.version_id.clone())
                .set_version_stage(args.version_stage.clone())
                .send()
                .await
                .map_err(AwlError::sdk)?;
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
        SecretsSubcommand::Put(args) => {
            let response = client
                .put_secret_value()
                .secret_id(&args.name)
                .secret_string(common::stdin_or_string(&args.value)?)
                .send()
                .await
                .map_err(AwlError::sdk)?;
            output::ack(
                globals,
                Output::one(serde_json::json!({
                    "arn": response.arn(),
                    "name": response.name(),
                    "version_id": response.version_id(),
                }))?,
            )
        }
        SecretsSubcommand::Rotate(args) => {
            let response = client
                .rotate_secret()
                .secret_id(&args.name)
                .send()
                .await
                .map_err(AwlError::sdk)?;
            output::ack(
                globals,
                Output::one(serde_json::json!({
                    "arn": response.arn(),
                    "name": response.name(),
                    "version_id": response.version_id(),
                }))?,
            )
        }
        SecretsSubcommand::Rm(args) => {
            let response = client
                .delete_secret()
                .secret_id(&args.name)
                .set_force_delete_without_recovery(args.force.then_some(true))
                .send()
                .await
                .map_err(AwlError::sdk)?;
            output::ack(
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
