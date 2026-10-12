use serde::Serialize;

use crate::cli::{CredentialFormat, Globals, StsAssumeArgs, StsCommand, StsSubcommand};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};

#[derive(Debug, Serialize)]
struct IdentityRow {
    account: Option<String>,
    arn: Option<String>,
    user_id: Option<String>,
}

pub async fn run(globals: &Globals, command: &StsCommand) -> Result<()> {
    let context = AwsContext::new_without_imds_region(globals).await?;
    let client = context.sts();
    match &command.command {
        StsSubcommand::Whoami => {
            let response = client
                .get_caller_identity()
                .send()
                .await
                .map_err(AwlError::sdk)?;
            output::emit(
                globals,
                Output::one(IdentityRow {
                    account: response.account().map(str::to_owned),
                    arn: response.arn().map(str::to_owned),
                    user_id: response.user_id().map(str::to_owned),
                })?,
            )
        }
        StsSubcommand::Assume(args) => assume(globals, &client, args).await,
        StsSubcommand::Decode(args) => {
            let response = client
                .decode_authorization_message()
                .encoded_message(&args.encoded_message)
                .send()
                .await
                .map_err(AwlError::sdk)?;
            let decoded = response.decoded_message().unwrap_or_default();
            // The decoded message is a JSON document; show it as one.
            let decoded = serde_json::from_str::<serde_json::Value>(decoded)
                .unwrap_or_else(|_| serde_json::Value::String(decoded.to_owned()));
            output::emit(
                globals,
                Output::one(serde_json::json!({ "decoded_message": decoded }))?,
            )
        }
    }
}

async fn assume(
    globals: &Globals,
    client: &aws_sdk_sts::Client,
    args: &StsAssumeArgs,
) -> Result<()> {
    let session_name = args
        .session_name
        .clone()
        .unwrap_or_else(|| crate::client::session_name(globals));
    let response = client
        .assume_role()
        .role_arn(&args.role_arn)
        .role_session_name(session_name)
        .duration_seconds(args.duration)
        .set_token_code(globals.mfa_token.clone())
        .send()
        .await
        .map_err(AwlError::sdk)?;
    let Some(credentials) = response.credentials() else {
        return Err(AwlError::Authentication {
            message: "AssumeRole returned no credentials".to_owned(),
        });
    };

    match args.format {
        CredentialFormat::Env => output::emit(
            globals,
            Output::Plain(format!(
                "export AWS_ACCESS_KEY_ID='{}'\nexport AWS_SECRET_ACCESS_KEY='{}'\nexport AWS_SESSION_TOKEN='{}'\nexport AWS_EXPIRATION='{}'",
                credentials.access_key_id(),
                credentials.secret_access_key(),
                credentials.session_token(),
                credentials.expiration()
            )),
        ),
        CredentialFormat::Json => output::emit(
            globals,
            Output::one(serde_json::json!({
                "access_key_id": credentials.access_key_id(),
                "secret_access_key": credentials.secret_access_key(),
                "session_token": credentials.session_token(),
                "expiration": credentials.expiration().to_string(),
            }))?,
        ),
    }
}
