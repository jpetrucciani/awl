use std::collections::HashSet;
use std::time::Duration;

use crate::cli::{Globals, LogsCommand, LogsGetArgs, LogsSubcommand, TailOptions};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};

const FOLLOW_POLL: Duration = Duration::from_secs(2);

pub async fn run(globals: &Globals, command: &LogsCommand) -> Result<()> {
    let context = AwsContext::new(globals).await?;
    let client = context.logs();
    match &command.command {
        LogsSubcommand::Ls(args) => {
            let mut rows = Vec::new();
            let mut pages = client
                .describe_log_groups()
                .set_log_group_name_prefix(args.prefix.clone())
                .into_paginator()
                .send();
            while let Some(page) = pages.next().await {
                rows.extend(
                    page.map_err(AwlError::sdk)?
                        .log_groups()
                        .iter()
                        .map(|group| {
                            serde_json::json!({
                                "log_group_name": group.log_group_name(),
                                "stored_bytes": group.stored_bytes(),
                                "retention_days": group.retention_in_days(),
                            })
                        }),
                );
            }
            output::emit(globals, Output::Many(rows))
        }
        LogsSubcommand::Streams(args) => {
            let limit = args.limit.unwrap_or(usize::MAX);
            let mut rows = Vec::new();
            let mut pages = client
                .describe_log_streams()
                .log_group_name(&args.group)
                .order_by(aws_sdk_cloudwatchlogs::types::OrderBy::LastEventTime)
                .descending(true)
                .into_paginator()
                .send();
            while rows.len() < limit {
                let Some(page) = pages.next().await else {
                    break;
                };
                rows.extend(
                    page.map_err(AwlError::sdk)?
                        .log_streams()
                        .iter()
                        .map(|stream| {
                            serde_json::json!({
                                "log_stream_name": stream.log_stream_name(),
                                "last_event": stream.last_event_timestamp().map(format_millis),
                            })
                        }),
                );
            }
            rows.truncate(limit);
            output::emit(globals, Output::Many(rows))
        }
        LogsSubcommand::Tail(args) => tail(globals, &client, &args.group, &args.options).await,
        LogsSubcommand::Get(args) => get(globals, &client, args).await,
    }
}

/// Print events from `--since` ago, page by page, then keep polling with
/// `--follow`. Events at the last seen timestamp are de-duplicated by ID.
pub async fn tail(
    globals: &Globals,
    client: &aws_sdk_cloudwatchlogs::Client,
    group: &str,
    options: &TailOptions,
) -> Result<()> {
    let since = i64::try_from(options.since.as_millis()).unwrap_or(i64::MAX);
    let mut start = chrono::Utc::now().timestamp_millis().saturating_sub(since);
    let mut seen_at_start = HashSet::new();
    let mut remaining = if options.follow {
        usize::MAX
    } else {
        options.limit.unwrap_or(usize::MAX)
    };

    loop {
        let mut pages = client
            .filter_log_events()
            .log_group_name(group)
            .start_time(start)
            .set_filter_pattern(options.filter.clone())
            .set_log_stream_names((!options.stream.is_empty()).then(|| options.stream.clone()))
            .into_paginator()
            .send();
        while let Some(page) = pages.next().await {
            let page = page.map_err(AwlError::sdk)?;
            let mut rows = Vec::new();
            for event in page.events() {
                let timestamp = event.timestamp().unwrap_or_default();
                let id = event.event_id().unwrap_or_default().to_owned();
                if timestamp == start && !seen_at_start.insert(id.clone()) {
                    continue;
                }
                if timestamp > start {
                    start = timestamp;
                    seen_at_start.clear();
                    seen_at_start.insert(id);
                }
                rows.push(event_row(
                    event.timestamp(),
                    event.log_stream_name(),
                    event.message(),
                ));
                remaining -= 1;
                if remaining == 0 {
                    break;
                }
            }
            if !rows.is_empty() {
                output::emit(globals, Output::Many(rows))?;
            }
            if remaining == 0 {
                return Ok(());
            }
        }
        if !options.follow {
            return Ok(());
        }
        tokio::time::sleep(FOLLOW_POLL).await;
    }
}

async fn get(
    globals: &Globals,
    client: &aws_sdk_cloudwatchlogs::Client,
    args: &LogsGetArgs,
) -> Result<()> {
    let mut remaining = args.limit.unwrap_or(usize::MAX);
    let mut token: Option<String> = None;
    while remaining > 0 {
        let response = client
            .get_log_events()
            .log_group_name(&args.group)
            .log_stream_name(&args.stream)
            .start_from_head(true)
            .set_next_token(token.clone())
            .send()
            .await
            .map_err(AwlError::sdk)?;
        let rows = response
            .events()
            .iter()
            .take(remaining)
            .map(|event| event_row(event.timestamp(), Some(&args.stream), event.message()))
            .collect::<Vec<_>>();
        remaining -= rows.len();
        if !rows.is_empty() {
            output::emit(globals, Output::Many(rows))?;
        }
        // The forward token repeats once the end of the stream is reached.
        let next = response.next_forward_token().map(str::to_owned);
        if next.is_none() || next == token {
            break;
        }
        token = next;
    }
    Ok(())
}

fn event_row(
    timestamp: Option<i64>,
    stream: Option<&str>,
    message: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "timestamp": timestamp.map(format_millis),
        "log_stream_name": stream,
        "message": message.map(|message| message.trim_end_matches(['\r', '\n'])),
    })
}

fn format_millis(millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_else(|| millis.to_string())
}
