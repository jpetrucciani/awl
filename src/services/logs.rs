use crate::cli::{Globals, LogsCommand, LogsCommandSubcommand, StubArgs};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};
use crate::services::common;

pub async fn run(globals: &Globals, command: &LogsCommand) -> Result<()> {
    let context = AwsContext::new(globals).await?;
    let client = context.logs();
    match &command.command {
        LogsCommandSubcommand::Ls(args) => {
            let mut request = client.describe_log_groups();
            if let Some(prefix) = common::option_value(&args.args, "--prefix") {
                request = request.log_group_name_prefix(prefix);
            }
            let response = request.send().await.map_err(AwlError::aws)?;
            let rows = response
                .log_groups()
                .iter()
                .map(|group| {
                    serde_json::json!({
                        "log_group_name": group.log_group_name(),
                        "stored_bytes": group.stored_bytes(),
                        "retention_days": group.retention_in_days(),
                    })
                })
                .collect::<Vec<_>>();
            output::emit(globals, Output::Many(rows))
        }
        LogsCommandSubcommand::Streams(args) => streams(globals, &client, args).await,
        LogsCommandSubcommand::Tail(args) | LogsCommandSubcommand::Get(args) => {
            events(globals, &client, args).await
        }
    }
}

async fn streams(
    globals: &Globals,
    client: &aws_sdk_cloudwatchlogs::Client,
    args: &StubArgs,
) -> Result<()> {
    let positional = common::positional(&args.args);
    let group = common::required(&positional, 0, "group")?;
    let mut request = client.describe_log_streams().log_group_name(group);
    if let Some(limit) =
        common::option_value(&args.args, "--limit").and_then(|value| value.parse::<i32>().ok())
    {
        request = request.limit(limit);
    }
    let response = request.send().await.map_err(AwlError::aws)?;
    let rows = response
        .log_streams()
        .iter()
        .map(|stream| {
            serde_json::json!({
                "log_stream_name": stream.log_stream_name(),
                "last_event_timestamp": stream.last_event_timestamp(),
            })
        })
        .collect::<Vec<_>>();
    output::emit(globals, Output::Many(rows))
}

async fn events(
    globals: &Globals,
    client: &aws_sdk_cloudwatchlogs::Client,
    args: &StubArgs,
) -> Result<()> {
    let positional = common::positional(&args.args);
    let group = common::required(&positional, 0, "group")?;
    let stream = positional
        .get(1)
        .cloned()
        .or_else(|| common::option_value(&args.args, "--stream"));
    let rows = if let Some(stream) = stream {
        let response = client
            .get_log_events()
            .log_group_name(group)
            .log_stream_name(stream)
            .send()
            .await
            .map_err(AwlError::aws)?;
        response
            .events()
            .iter()
            .map(|event| {
                serde_json::json!({
                    "timestamp": event.timestamp(),
                    "message": event.message(),
                    "ingestion_time": event.ingestion_time(),
                })
            })
            .collect::<Vec<_>>()
    } else {
        let response = client
            .filter_log_events()
            .log_group_name(group)
            .send()
            .await
            .map_err(AwlError::aws)?;
        response
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
            .collect::<Vec<_>>()
    };
    output::emit(globals, Output::Many(rows))
}
