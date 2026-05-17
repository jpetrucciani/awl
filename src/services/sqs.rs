use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;

use aws_sdk_sqs::types::{MessageAttributeValue, QueueAttributeName};
use serde::Serialize;
use tokio::io::AsyncReadExt;

use crate::cli::{Globals, SqsCommand, SqsReceiveArgs, SqsSendArgs, SqsSubcommand};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};

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

#[derive(Debug, Serialize)]
struct PurgeRow {
    queue_url: String,
    purged: bool,
}

#[derive(Debug, Serialize)]
struct UrlRow {
    queue_name: String,
    queue_url: String,
}

pub async fn run(globals: &Globals, command: &SqsCommand) -> Result<()> {
    if let Some(endpoint) = endpoint_for_sqs(globals) {
        return QuerySqs::new(endpoint).run(globals, command).await;
    }

    let context = AwsContext::new(globals).await?;
    let client = context.sqs();
    match &command.command {
        SqsSubcommand::Ls(args) => {
            let mut request = client.list_queues();
            if let Some(prefix) = &args.prefix {
                request = request.queue_name_prefix(prefix);
            }
            let response = request.send().await.map_err(AwlError::aws)?;
            let rows = response
                .queue_urls()
                .iter()
                .map(|queue_url| QueueRow {
                    queue_url: queue_url.to_owned(),
                })
                .collect::<Vec<_>>();
            output::emit(globals, Output::many(rows)?)
        }
        SqsSubcommand::Send(args) => send(globals, &client, args).await,
        SqsSubcommand::Receive(args) => receive(globals, &client, args).await,
        SqsSubcommand::Purge(args) => {
            let queue_url = resolve_queue_url(&client, &args.queue).await?;
            client
                .purge_queue()
                .queue_url(&queue_url)
                .send()
                .await
                .map_err(AwlError::aws)?;
            output::emit(
                globals,
                Output::one(PurgeRow {
                    queue_url,
                    purged: true,
                })?,
            )
        }
        SqsSubcommand::Attrs(args) => {
            let queue_url = resolve_queue_url(&client, &args.queue).await?;
            let response = client
                .get_queue_attributes()
                .queue_url(&queue_url)
                .attribute_names(QueueAttributeName::All)
                .send()
                .await
                .map_err(AwlError::aws)?;
            let attributes = response
                .attributes()
                .map(|attributes| {
                    attributes
                        .iter()
                        .map(|(key, value)| (key.as_str().to_owned(), value.clone()))
                        .collect::<HashMap<_, _>>()
                })
                .unwrap_or_default();
            output::emit(
                globals,
                Output::one(serde_json::json!({
                    "queue_url": queue_url,
                    "attributes": attributes,
                }))?,
            )
        }
        SqsSubcommand::Url(args) => {
            let queue_url = resolve_queue_url(&client, &args.queue_name).await?;
            output::emit(
                globals,
                Output::one(UrlRow {
                    queue_name: args.queue_name.clone(),
                    queue_url,
                })?,
            )
        }
    }
}

fn endpoint_for_sqs(globals: &Globals) -> Option<String> {
    globals
        .endpoint_url
        .clone()
        .or_else(|| std::env::var("AWS_ENDPOINT_URL_SQS").ok())
        .or_else(|| std::env::var("AWS_ENDPOINT_URL").ok())
}

struct QuerySqs {
    endpoint: String,
}

impl QuerySqs {
    fn new(endpoint: String) -> Self {
        Self { endpoint }
    }

    async fn run(&self, globals: &Globals, command: &SqsCommand) -> Result<()> {
        match &command.command {
            SqsSubcommand::Ls(args) => {
                let mut params = vec![("Action".to_owned(), "ListQueues".to_owned())];
                if let Some(prefix) = &args.prefix {
                    params.push(("QueueNamePrefix".to_owned(), prefix.clone()));
                }
                let xml = self.request(params).await?;
                let rows = tag_values(&xml, "QueueUrl")
                    .into_iter()
                    .map(|queue_url| QueueRow { queue_url })
                    .collect::<Vec<_>>();
                output::emit(globals, Output::many(rows)?)
            }
            SqsSubcommand::Send(args) => {
                let queue_url = self.resolve_queue(&args.queue).await?;
                let mut params = vec![
                    ("Action".to_owned(), "SendMessage".to_owned()),
                    ("QueueUrl".to_owned(), queue_url.clone()),
                    ("MessageBody".to_owned(), read_body(&args.body).await?),
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
                let xml = self.request(params).await?;
                output::emit(
                    globals,
                    Output::one(SendRow {
                        queue_url,
                        message_id: first_tag_value(&xml, "MessageId"),
                        md5_of_body: first_tag_value(&xml, "MD5OfMessageBody"),
                    })?,
                )
            }
            SqsSubcommand::Receive(args) => {
                let queue_url = self.resolve_queue(&args.queue).await?;
                let mut rows = Vec::new();
                loop {
                    let mut params = vec![
                        ("Action".to_owned(), "ReceiveMessage".to_owned()),
                        ("QueueUrl".to_owned(), queue_url.clone()),
                        ("MaxNumberOfMessages".to_owned(), args.max.to_string()),
                        ("WaitTimeSeconds".to_owned(), args.wait.to_string()),
                    ];
                    if let Some(visibility) = args.visibility {
                        params.push(("VisibilityTimeout".to_owned(), visibility.to_string()));
                    }
                    let xml = self.request(params).await?;
                    let messages = message_rows(&queue_url, &xml);
                    if messages.is_empty() && !args.follow {
                        break;
                    }
                    for message in &messages {
                        if args.delete
                            && let Some(receipt_handle) = &message.receipt_handle
                        {
                            self.request(vec![
                                ("Action".to_owned(), "DeleteMessage".to_owned()),
                                ("QueueUrl".to_owned(), queue_url.clone()),
                                ("ReceiptHandle".to_owned(), receipt_handle.clone()),
                            ])
                            .await?;
                        }
                    }
                    rows.extend(messages);
                    if !args.follow {
                        break;
                    }
                }
                output::emit(globals, Output::many(rows)?)
            }
            SqsSubcommand::Purge(args) => {
                let queue_url = self.resolve_queue(&args.queue).await?;
                self.request(vec![
                    ("Action".to_owned(), "PurgeQueue".to_owned()),
                    ("QueueUrl".to_owned(), queue_url.clone()),
                ])
                .await?;
                output::emit(
                    globals,
                    Output::one(PurgeRow {
                        queue_url,
                        purged: true,
                    })?,
                )
            }
            SqsSubcommand::Attrs(args) => {
                let queue_url = self.resolve_queue(&args.queue).await?;
                let xml = self
                    .request(vec![
                        ("Action".to_owned(), "GetQueueAttributes".to_owned()),
                        ("QueueUrl".to_owned(), queue_url.clone()),
                        ("AttributeName".to_owned(), "All".to_owned()),
                    ])
                    .await?;
                output::emit(
                    globals,
                    Output::one(serde_json::json!({
                        "queue_url": queue_url,
                        "attributes": attributes_from_xml(&xml),
                    }))?,
                )
            }
            SqsSubcommand::Url(args) => {
                let queue_url = self.resolve_queue(&args.queue_name).await?;
                output::emit(
                    globals,
                    Output::one(UrlRow {
                        queue_name: args.queue_name.clone(),
                        queue_url,
                    })?,
                )
            }
        }
    }

    async fn resolve_queue(&self, queue: &str) -> Result<String> {
        if queue.starts_with("http://") || queue.starts_with("https://") {
            return Ok(queue.to_owned());
        }
        let xml = self
            .request(vec![
                ("Action".to_owned(), "GetQueueUrl".to_owned()),
                ("QueueName".to_owned(), queue.to_owned()),
            ])
            .await?;
        Ok(first_tag_value(&xml, "QueueUrl").unwrap_or_else(|| queue.to_owned()))
    }

    async fn request(&self, params: Vec<(String, String)>) -> Result<String> {
        let endpoint = self.endpoint.clone();
        tokio::task::spawn_blocking(move || query_request(&endpoint, &params))
            .await
            .map_err(|error| AwlError::Endpoint {
                message: error.to_string(),
            })?
    }
}

async fn send(globals: &Globals, client: &aws_sdk_sqs::Client, args: &SqsSendArgs) -> Result<()> {
    let queue_url = resolve_queue_url(client, &args.queue).await?;
    let body = read_body(&args.body).await?;
    let mut request = client
        .send_message()
        .queue_url(&queue_url)
        .message_body(body);
    if let Some(delay) = args.delay {
        request = request.delay_seconds(delay);
    }
    if let Some(group_id) = &args.group_id {
        request = request.message_group_id(group_id);
    }
    if let Some(dedup_id) = &args.dedup_id {
        request = request.message_deduplication_id(dedup_id);
    }
    if !args.attribute.is_empty() {
        request = request.set_message_attributes(Some(message_attributes(&args.attribute)?));
    }

    let response = request.send().await.map_err(AwlError::aws)?;
    output::emit(
        globals,
        Output::one(SendRow {
            queue_url,
            message_id: response.message_id().map(str::to_owned),
            md5_of_body: response.md5_of_message_body().map(str::to_owned),
        })?,
    )
}

async fn receive(
    globals: &Globals,
    client: &aws_sdk_sqs::Client,
    args: &SqsReceiveArgs,
) -> Result<()> {
    let queue_url = resolve_queue_url(client, &args.queue).await?;
    let mut rows = Vec::new();

    loop {
        let mut request = client
            .receive_message()
            .queue_url(&queue_url)
            .max_number_of_messages(args.max)
            .wait_time_seconds(args.wait);
        if let Some(visibility) = args.visibility {
            request = request.visibility_timeout(visibility);
        }
        let response = request.send().await.map_err(AwlError::aws)?;
        let messages = response.messages();
        if messages.is_empty() && !args.follow {
            break;
        }
        for message in messages {
            let receipt_handle = message.receipt_handle().map(str::to_owned);
            rows.push(MessageRow {
                queue_url: queue_url.clone(),
                message_id: message.message_id().map(str::to_owned),
                receipt_handle: receipt_handle.clone(),
                body: message.body().map(str::to_owned),
            });
            if args.delete
                && let Some(receipt_handle) = receipt_handle
            {
                client
                    .delete_message()
                    .queue_url(&queue_url)
                    .receipt_handle(receipt_handle)
                    .send()
                    .await
                    .map_err(AwlError::aws)?;
            }
        }
        if !args.follow {
            break;
        }
    }

    output::emit(globals, Output::many(rows)?)
}

async fn resolve_queue_url(client: &aws_sdk_sqs::Client, queue: &str) -> Result<String> {
    if queue.starts_with("http://") || queue.starts_with("https://") {
        return Ok(queue.to_owned());
    }
    let response = client
        .get_queue_url()
        .queue_name(queue)
        .send()
        .await
        .map_err(AwlError::aws)?;
    response
        .queue_url()
        .map(str::to_owned)
        .ok_or_else(|| AwlError::NotFound {
            message: format!("queue {queue:?} resolved without a queue URL"),
        })
}

async fn read_body(body: &str) -> Result<String> {
    if body != "-" {
        return Ok(body.to_owned());
    }
    let mut value = String::new();
    tokio::io::stdin().read_to_string(&mut value).await?;
    Ok(value)
}

fn message_attributes(
    pairs: &[(String, String)],
) -> Result<HashMap<String, MessageAttributeValue>> {
    let mut attributes = HashMap::new();
    for (key, value) in pairs {
        let attribute = MessageAttributeValue::builder()
            .data_type("String")
            .string_value(value)
            .build()
            .map_err(AwlError::aws)?;
        attributes.insert(key.clone(), attribute);
    }
    Ok(attributes)
}

fn query_request(endpoint: &str, params: &[(String, String)]) -> Result<String> {
    let parsed = HttpEndpoint::parse(endpoint)?;
    let mut query = vec![("Version".to_owned(), "2012-11-05".to_owned())];
    query.extend(params.iter().cloned());
    let query = query
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    let path = if parsed.path.is_empty() {
        format!("/?{query}")
    } else {
        format!("{}?{query}", parsed.path)
    };

    let mut stream = TcpStream::connect((parsed.host.as_str(), parsed.port))?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        parsed.host_header
    );
    stream.write_all(request.as_bytes())?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| AwlError::Endpoint {
            message: "SQS query response was not valid HTTP".to_owned(),
        })?;
    let ok = head
        .lines()
        .next()
        .is_some_and(|status| status.contains(" 200 "));
    if ok {
        Ok(body.to_owned())
    } else {
        Err(AwlError::Aws {
            message: body.to_owned(),
        })
    }
}

struct HttpEndpoint {
    host: String,
    host_header: String,
    port: u16,
    path: String,
}

impl HttpEndpoint {
    fn parse(endpoint: &str) -> Result<Self> {
        let Some(rest) = endpoint.strip_prefix("http://") else {
            return Err(AwlError::Endpoint {
                message: "local SQS query adapter only supports http:// endpoints".to_owned(),
            });
        };
        let (host_port, path) = rest
            .split_once('/')
            .map(|(host_port, path)| (host_port, format!("/{path}")))
            .unwrap_or((rest, String::new()));
        let (host, port) = if let Some((host, port)) = host_port.rsplit_once(':') {
            let port = port.parse::<u16>().map_err(|error| AwlError::Endpoint {
                message: format!("invalid SQS endpoint port in {endpoint:?}: {error}"),
            })?;
            (host.to_owned(), port)
        } else {
            (host_port.to_owned(), 80)
        };
        Ok(Self {
            host,
            host_header: host_port.to_owned(),
            port,
            path,
        })
    }
}

fn first_tag_value(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(unescape_xml(xml[start..end].trim()))
}

fn tag_values(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut values = Vec::new();
    let mut rest = xml;
    while let Some(start_idx) = rest.find(&open) {
        let start = start_idx + open.len();
        let Some(end_idx) = rest[start..].find(&close) else {
            break;
        };
        let end = start + end_idx;
        values.push(unescape_xml(rest[start..end].trim()));
        rest = &rest[end + close.len()..];
    }
    values
}

fn message_rows(queue_url: &str, xml: &str) -> Vec<MessageRow> {
    let mut rows = Vec::new();
    let mut rest = xml;
    while let Some(start_idx) = rest.find("<Message>") {
        let start = start_idx + "<Message>".len();
        let Some(end_idx) = rest[start..].find("</Message>") else {
            break;
        };
        let end = start + end_idx;
        let message = &rest[start..end];
        rows.push(MessageRow {
            queue_url: queue_url.to_owned(),
            message_id: first_tag_value(message, "MessageId"),
            receipt_handle: first_tag_value(message, "ReceiptHandle"),
            body: first_tag_value(message, "Body"),
        });
        rest = &rest[end + "</Message>".len()..];
    }
    rows
}

fn attributes_from_xml(xml: &str) -> HashMap<String, String> {
    let mut attributes = HashMap::new();
    let mut rest = xml;
    while let Some(start_idx) = rest.find("<Attribute>") {
        let start = start_idx + "<Attribute>".len();
        let Some(end_idx) = rest[start..].find("</Attribute>") else {
            break;
        };
        let end = start + end_idx;
        let attribute = &rest[start..end];
        if let Some(name) = first_tag_value(attribute, "Name") {
            attributes.insert(
                name,
                first_tag_value(attribute, "Value").unwrap_or_default(),
            );
        }
        rest = &rest[end + "</Attribute>".len()..];
    }
    attributes
}

fn unescape_xml(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}
