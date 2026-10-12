use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use aws_sdk_sqs::types::{MessageAttributeValue, QueueAttributeName};
use serde::Serialize;

use crate::cli::{Globals, SqsCommand, SqsReceiveArgs, SqsSendArgs, SqsSubcommand};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};
use crate::services::common;

/// Opt-in for the unsigned SQS Query protocol adapter used with goaws, which
/// does not speak the JSON protocol current SDKs use.
const QUERY_PROTOCOL_ENV: &str = "AWL_SQS_QUERY_PROTOCOL";

#[derive(Debug, Serialize)]
struct QueueRow {
    queue_url: String,
}

#[derive(Debug, Serialize)]
struct SendRow {
    queue_url: String,
    message_id: Option<String>,
    md5_of_body: Option<String>,
}

#[derive(Debug, Serialize)]
struct MessageRow {
    queue_url: String,
    message_id: Option<String>,
    receipt_handle: Option<String>,
    body: Option<String>,
}

pub async fn run(globals: &Globals, command: &SqsCommand) -> Result<()> {
    let sqs = Sqs::new(globals).await?;
    match &command.command {
        SqsSubcommand::Ls(args) => {
            let rows = sqs
                .list_queues(args.prefix.as_deref())
                .await?
                .into_iter()
                .map(|queue_url| QueueRow { queue_url })
                .collect::<Vec<_>>();
            output::emit(globals, Output::many(rows)?)
        }
        SqsSubcommand::Send(args) => send(globals, &sqs, args).await,
        SqsSubcommand::Receive(args) => receive(globals, &sqs, args).await,
        SqsSubcommand::Purge(args) => {
            let queue_url = sqs.queue_url(&args.queue).await?;
            sqs.purge(&queue_url).await?;
            output::ack(
                globals,
                Output::one(serde_json::json!({ "queue_url": queue_url, "purged": true }))?,
            )
        }
        SqsSubcommand::Attrs(args) => {
            let queue_url = sqs.queue_url(&args.queue).await?;
            let attributes = sqs.attributes(&queue_url).await?;
            output::emit(
                globals,
                Output::one(serde_json::json!({
                    "queue_url": queue_url,
                    "attributes": attributes,
                }))?,
            )
        }
        SqsSubcommand::Url(args) => {
            let queue_url = sqs.queue_url(&args.queue).await?;
            output::emit(
                globals,
                Output::one(serde_json::json!({
                    "queue_name": args.queue,
                    "queue_url": queue_url,
                }))?,
            )
        }
    }
}

async fn send(globals: &Globals, sqs: &Sqs, args: &SqsSendArgs) -> Result<()> {
    let queue_url = sqs.queue_url(&args.queue).await?;
    let body = common::stdin_or_string(&args.body)?;
    let row = sqs.send(&queue_url, body, args).await?;
    output::ack(globals, Output::one(row)?)
}

/// Print each batch as it arrives, and only then delete it, so an interrupted
/// or failed write never drops messages.
async fn receive(globals: &Globals, sqs: &Sqs, args: &SqsReceiveArgs) -> Result<()> {
    let queue_url = sqs.queue_url(&args.queue).await?;
    loop {
        let messages = sqs.receive(&queue_url, args).await?;
        if !messages.is_empty() {
            output::emit(globals, Output::many(messages.iter().collect())?)?;
            if args.delete {
                for receipt_handle in messages.iter().filter_map(|m| m.receipt_handle.as_deref()) {
                    sqs.delete(&queue_url, receipt_handle).await?;
                }
            }
        }
        if !args.follow {
            return Ok(());
        }
    }
}

enum Sqs {
    Sdk(aws_sdk_sqs::Client),
    Query(QueryClient),
}

impl Sqs {
    async fn new(globals: &Globals) -> Result<Self> {
        let query = std::env::var(QUERY_PROTOCOL_ENV)
            .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes"));
        if !query {
            return Ok(Self::Sdk(AwsContext::new(globals).await?.sqs()));
        }
        let endpoint = globals
            .endpoint_url
            .clone()
            .or_else(|| std::env::var("AWS_ENDPOINT_URL_SQS").ok())
            .or_else(|| std::env::var("AWS_ENDPOINT_URL").ok())
            .ok_or_else(|| AwlError::Usage {
                message: format!("{QUERY_PROTOCOL_ENV} requires an SQS endpoint URL"),
            })?;
        Ok(Self::Query(QueryClient::new(&endpoint)?))
    }

    async fn list_queues(&self, prefix: Option<&str>) -> Result<Vec<String>> {
        match self {
            Sqs::Sdk(client) => {
                let mut urls = Vec::new();
                let mut pages = client
                    .list_queues()
                    .set_queue_name_prefix(prefix.map(str::to_owned))
                    .into_paginator()
                    .send();
                while let Some(page) = pages.next().await {
                    urls.extend(page.map_err(AwlError::sdk)?.queue_urls().iter().cloned());
                }
                Ok(urls)
            }
            Sqs::Query(client) => {
                let mut params = vec![("Action", "ListQueues")];
                if let Some(prefix) = prefix {
                    params.push(("QueueNamePrefix", prefix));
                }
                Ok(tag_values(&client.request(&params).await?, "QueueUrl"))
            }
        }
    }

    async fn queue_url(&self, queue: &str) -> Result<String> {
        if queue.starts_with("http://") || queue.starts_with("https://") {
            return Ok(queue.to_owned());
        }
        let url = match self {
            Sqs::Sdk(client) => client
                .get_queue_url()
                .queue_name(queue)
                .send()
                .await
                .map_err(AwlError::sdk)?
                .queue_url()
                .map(str::to_owned),
            Sqs::Query(client) => first_tag_value(
                &client
                    .request(&[("Action", "GetQueueUrl"), ("QueueName", queue)])
                    .await?,
                "QueueUrl",
            ),
        };
        url.ok_or_else(|| AwlError::NotFound {
            message: format!("queue {queue:?} resolved without a queue URL"),
        })
    }

    async fn send(&self, queue_url: &str, body: String, args: &SqsSendArgs) -> Result<SendRow> {
        match self {
            Sqs::Sdk(client) => {
                let mut attributes = std::collections::HashMap::new();
                for (key, value) in &args.attribute {
                    let attribute = MessageAttributeValue::builder()
                        .data_type("String")
                        .string_value(value)
                        .build()
                        .map_err(AwlError::aws)?;
                    attributes.insert(key.clone(), attribute);
                }
                let response = client
                    .send_message()
                    .queue_url(queue_url)
                    .message_body(body)
                    .set_delay_seconds(args.delay)
                    .set_message_group_id(args.group_id.clone())
                    .set_message_deduplication_id(args.dedup_id.clone())
                    .set_message_attributes((!attributes.is_empty()).then_some(attributes))
                    .send()
                    .await
                    .map_err(AwlError::sdk)?;
                Ok(SendRow {
                    queue_url: queue_url.to_owned(),
                    message_id: response.message_id().map(str::to_owned),
                    md5_of_body: response.md5_of_message_body().map(str::to_owned),
                })
            }
            Sqs::Query(client) => {
                let mut params = vec![
                    ("Action".to_owned(), "SendMessage".to_owned()),
                    ("QueueUrl".to_owned(), queue_url.to_owned()),
                    ("MessageBody".to_owned(), body),
                ];
                if let Some(delay) = args.delay {
                    params.push(("DelaySeconds".to_owned(), delay.to_string()));
                }
                if let Some(group_id) = &args.group_id {
                    params.push(("MessageGroupId".to_owned(), group_id.clone()));
                }
                if let Some(dedup_id) = &args.dedup_id {
                    params.push(("MessageDeduplicationId".to_owned(), dedup_id.clone()));
                }
                for (idx, (key, value)) in args.attribute.iter().enumerate() {
                    let n = idx + 1;
                    params.push((format!("MessageAttribute.{n}.Name"), key.clone()));
                    params.push((
                        format!("MessageAttribute.{n}.Value.DataType"),
                        "String".to_owned(),
                    ));
                    params.push((
                        format!("MessageAttribute.{n}.Value.StringValue"),
                        value.clone(),
                    ));
                }
                let params = params
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str()))
                    .collect::<Vec<_>>();
                let xml = client.request(&params).await?;
                Ok(SendRow {
                    queue_url: queue_url.to_owned(),
                    message_id: first_tag_value(&xml, "MessageId"),
                    md5_of_body: first_tag_value(&xml, "MD5OfMessageBody"),
                })
            }
        }
    }

    async fn receive(&self, queue_url: &str, args: &SqsReceiveArgs) -> Result<Vec<MessageRow>> {
        match self {
            Sqs::Sdk(client) => {
                let response = client
                    .receive_message()
                    .queue_url(queue_url)
                    .max_number_of_messages(args.max)
                    .wait_time_seconds(args.wait)
                    .set_visibility_timeout(args.visibility)
                    .send()
                    .await
                    .map_err(AwlError::sdk)?;
                Ok(response
                    .messages()
                    .iter()
                    .map(|message| MessageRow {
                        queue_url: queue_url.to_owned(),
                        message_id: message.message_id().map(str::to_owned),
                        receipt_handle: message.receipt_handle().map(str::to_owned),
                        body: message.body().map(str::to_owned),
                    })
                    .collect())
            }
            Sqs::Query(client) => {
                let (max, wait) = (args.max.to_string(), args.wait.to_string());
                let visibility = args.visibility.map(|value| value.to_string());
                let mut params = vec![
                    ("Action", "ReceiveMessage"),
                    ("QueueUrl", queue_url),
                    ("MaxNumberOfMessages", max.as_str()),
                    ("WaitTimeSeconds", wait.as_str()),
                ];
                if let Some(visibility) = &visibility {
                    params.push(("VisibilityTimeout", visibility));
                }
                Ok(message_rows(queue_url, &client.request(&params).await?))
            }
        }
    }

    async fn delete(&self, queue_url: &str, receipt_handle: &str) -> Result<()> {
        match self {
            Sqs::Sdk(client) => {
                client
                    .delete_message()
                    .queue_url(queue_url)
                    .receipt_handle(receipt_handle)
                    .send()
                    .await
                    .map_err(AwlError::sdk)?;
            }
            Sqs::Query(client) => {
                client
                    .request(&[
                        ("Action", "DeleteMessage"),
                        ("QueueUrl", queue_url),
                        ("ReceiptHandle", receipt_handle),
                    ])
                    .await?;
            }
        }
        Ok(())
    }

    async fn purge(&self, queue_url: &str) -> Result<()> {
        match self {
            Sqs::Sdk(client) => {
                client
                    .purge_queue()
                    .queue_url(queue_url)
                    .send()
                    .await
                    .map_err(AwlError::sdk)?;
            }
            Sqs::Query(client) => {
                client
                    .request(&[("Action", "PurgeQueue"), ("QueueUrl", queue_url)])
                    .await?;
            }
        }
        Ok(())
    }

    async fn attributes(&self, queue_url: &str) -> Result<BTreeMap<String, String>> {
        match self {
            Sqs::Sdk(client) => Ok(client
                .get_queue_attributes()
                .queue_url(queue_url)
                .attribute_names(QueueAttributeName::All)
                .send()
                .await
                .map_err(AwlError::sdk)?
                .attributes()
                .map(|attributes| {
                    attributes
                        .iter()
                        .map(|(key, value)| (key.as_str().to_owned(), value.clone()))
                        .collect()
                })
                .unwrap_or_default()),
            Sqs::Query(client) => Ok(attributes_from_xml(
                &client
                    .request(&[
                        ("Action", "GetQueueAttributes"),
                        ("QueueUrl", queue_url),
                        ("AttributeName", "All"),
                    ])
                    .await?,
            )),
        }
    }
}

struct QueryClient {
    host: String,
    host_header: String,
    port: u16,
    path: String,
}

impl QueryClient {
    fn new(endpoint: &str) -> Result<Self> {
        let Some(rest) = endpoint.strip_prefix("http://") else {
            return Err(AwlError::Endpoint {
                message: format!("{QUERY_PROTOCOL_ENV} only supports http:// endpoints"),
            });
        };
        let (host_port, path) = rest
            .split_once('/')
            .map(|(host_port, path)| (host_port, format!("/{path}")))
            .unwrap_or((rest, "/".to_owned()));
        let (host, port) = match host_port.rsplit_once(':') {
            Some((host, port)) => (
                host.to_owned(),
                port.parse::<u16>().map_err(|error| AwlError::Endpoint {
                    message: format!("invalid SQS endpoint port in {endpoint:?}: {error}"),
                })?,
            ),
            None => (host_port.to_owned(), 80),
        };
        Ok(Self {
            host,
            host_header: host_port.to_owned(),
            port,
            path,
        })
    }

    async fn request(&self, params: &[(&str, &str)]) -> Result<String> {
        let mut query = vec!["Version=2012-11-05".to_owned()];
        query.extend(params.iter().map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        }));
        // HTTP/1.0 keeps the response un-chunked and closes the connection.
        let request = format!(
            "GET {}?{} HTTP/1.0\r\nHost: {}\r\n\r\n",
            self.path,
            query.join("&"),
            self.host_header
        );
        let address = (self.host.clone(), self.port);
        tokio::task::spawn_blocking(move || exchange(address, &request))
            .await
            .map_err(|error| AwlError::Endpoint {
                message: error.to_string(),
            })?
    }
}

fn exchange(address: (String, u16), request: &str) -> Result<String> {
    let mut stream = TcpStream::connect(address).map_err(|error| AwlError::Endpoint {
        message: error.to_string(),
    })?;
    // Long polls wait up to 20s; anything much longer is a hung endpoint.
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    stream.write_all(request.as_bytes())?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let response = String::from_utf8_lossy(&response);
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| AwlError::Endpoint {
            message: "SQS query response was not valid HTTP".to_owned(),
        })?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1));
    if status == Some("200") {
        return Ok(body.to_owned());
    }
    let code = first_tag_value(body, "Code").unwrap_or_else(|| "UnknownError".to_owned());
    let message = first_tag_value(body, "Message").unwrap_or_default();
    let message = format!("{code}: {message}");
    Err(if code.contains("NonExistent") {
        AwlError::NotFound { message }
    } else {
        AwlError::Aws { message }
    })
}

fn first_tag_value(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(unescape_xml(xml[start..end].trim()))
}

fn sections<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start_idx) = rest.find(&open) {
        let start = start_idx + open.len();
        let Some(end_idx) = rest[start..].find(&close) else {
            break;
        };
        out.push(&rest[start..start + end_idx]);
        rest = &rest[start + end_idx + close.len()..];
    }
    out
}

fn tag_values(xml: &str, tag: &str) -> Vec<String> {
    sections(xml, tag)
        .into_iter()
        .map(|value| unescape_xml(value.trim()))
        .collect()
}

fn message_rows(queue_url: &str, xml: &str) -> Vec<MessageRow> {
    sections(xml, "Message")
        .into_iter()
        .map(|message| MessageRow {
            queue_url: queue_url.to_owned(),
            message_id: first_tag_value(message, "MessageId"),
            receipt_handle: first_tag_value(message, "ReceiptHandle"),
            body: first_tag_value(message, "Body"),
        })
        .collect()
}

fn attributes_from_xml(xml: &str) -> BTreeMap<String, String> {
    sections(xml, "Attribute")
        .into_iter()
        .filter_map(|attribute| {
            let name = first_tag_value(attribute, "Name")?;
            Some((
                name,
                first_tag_value(attribute, "Value").unwrap_or_default(),
            ))
        })
        .collect()
}

/// Decode the five named XML entities plus numeric ones (`&#34;`, `&#xD;`),
/// which goaws uses for quotes and line breaks.
fn unescape_xml(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let decoded = rest.find(';').and_then(|end| {
            let entity = &rest[1..end];
            let ch = match entity {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                    .and_then(char::from_u32),
            };
            ch.map(|ch| (ch, end))
        });
        match decoded {
            Some((ch, end)) => {
                out.push(ch);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn unescape_xml_decodes_named_and_numeric_entities() {
        assert_eq!(
            crate::services::sqs::unescape_xml("{&#34;id&#34;: 1}&#xA;a &amp;&lt;b&gt; & c"),
            "{\"id\": 1}\na &<b> & c"
        );
    }
}
