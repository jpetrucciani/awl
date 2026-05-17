use serde::Serialize;

use crate::cli::{Globals, StsCommand, StsCommandSubcommand, StubArgs};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};
use crate::services::common;

#[derive(Debug, Serialize)]
struct IdentityRow {
    account: Option<String>,
    arn: Option<String>,
    user_id: Option<String>,
}

pub async fn run(globals: &Globals, command: &StsCommand) -> Result<()> {
    let context = AwsContext::new(globals).await?;
    let client = context.sts();
    match &command.command {
        StsCommandSubcommand::Whoami(_) => {
            let response = client
                .get_caller_identity()
                .send()
                .await
                .map_err(AwlError::aws)?;
            output::emit(
                globals,
                Output::one(IdentityRow {
                    account: response.account().map(str::to_owned),
                    arn: response.arn().map(str::to_owned),
                    user_id: response.user_id().map(str::to_owned),
                })?,
            )
        }
        StsCommandSubcommand::Assume(args) => assume(globals, &client, args).await,
        StsCommandSubcommand::Decode(args) => {
            let encoded = common::required(&args.args, 0, "encoded-msg")?;
            let response = client
                .decode_authorization_message()
                .encoded_message(encoded)
                .send()
                .await
                .map_err(AwlError::aws)?;
            output::emit(
                globals,
                Output::one(serde_json::json!({
                    "decoded_message": response.decoded_message(),
                }))?,
            )
        }
    }
}

async fn assume(globals: &Globals, client: &aws_sdk_sts::Client, args: &StubArgs) -> Result<()> {
    let role_arn = common::required(&args.args, 0, "role-arn")?;
    let duration = common::option_value(&args.args, "--duration")
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(3600);
    let session_name = common::option_value(&args.args, "--session-name")
        .or_else(|| globals.role_session_name.clone())
        .unwrap_or_else(|| format!("awl-{}", chrono::Utc::now().timestamp()));
    let format = common::option_value(&args.args, "--format").unwrap_or_else(|| "env".to_owned());
    let mut request = client
        .assume_role()
        .role_arn(role_arn)
        .role_session_name(session_name)
        .duration_seconds(duration);
    if let Some(token) = &globals.mfa_token {
        request = request.token_code(token);
    }
    let response = request.send().await.map_err(AwlError::aws)?;
    let Some(credentials) = response.credentials() else {
        return Err(AwlError::Authentication {
            message: "AssumeRole returned no credentials".to_owned(),
        });
    };

    if format == "env" {
        output::emit(
            globals,
            Output::Plain(format!(
                "export AWS_ACCESS_KEY_ID={}\nexport AWS_SECRET_ACCESS_KEY={}\nexport AWS_SESSION_TOKEN={}\nexport AWS_EXPIRATION={}",
                credentials.access_key_id(),
                credentials.secret_access_key(),
                credentials.session_token(),
                credentials.expiration()
            )),
        )
    } else {
        output::emit(
            globals,
            Output::one(serde_json::json!({
                "access_key_id": credentials.access_key_id(),
                "secret_access_key": credentials.secret_access_key(),
                "session_token": credentials.session_token(),
                "expiration": credentials.expiration().to_string(),
            }))?,
        )
    }
}
