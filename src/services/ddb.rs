use std::collections::HashMap;

use aws_sdk_dynamodb::types::AttributeValue;
use base64::Engine;

use crate::cli::{DdbCommand, DdbCommandSubcommand, Globals, StubArgs};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};
use crate::services::common;

pub async fn run(globals: &Globals, command: &DdbCommand) -> Result<()> {
    let context = AwsContext::new(globals).await?;
    let client = context.ddb();
    match &command.command {
        DdbCommandSubcommand::Ls(_) => ls(globals, &client).await,
        DdbCommandSubcommand::Desc(args) => desc(globals, &client, args).await,
        DdbCommandSubcommand::Get(args) => get(globals, &client, args).await,
        DdbCommandSubcommand::Query(args) => query(globals, &client, args).await,
        DdbCommandSubcommand::Scan(args) => scan(globals, &client, args).await,
        DdbCommandSubcommand::Put(args) => put(globals, &client, args).await,
        DdbCommandSubcommand::Rm(args) => rm(globals, &client, args).await,
    }
}

async fn ls(globals: &Globals, client: &aws_sdk_dynamodb::Client) -> Result<()> {
    let response = client.list_tables().send().await.map_err(AwlError::aws)?;
    let rows = response
        .table_names()
        .iter()
        .map(|name| serde_json::json!({ "table_name": name }))
        .collect::<Vec<_>>();
    output::emit(globals, Output::Many(rows))
}

async fn desc(globals: &Globals, client: &aws_sdk_dynamodb::Client, args: &StubArgs) -> Result<()> {
    let table = common::required(&common::positional(&args.args), 0, "table")?;
    let response = client
        .describe_table()
        .table_name(table)
        .send()
        .await
        .map_err(AwlError::aws)?;
    let table = response.table();
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "table_name": table.and_then(|table| table.table_name()),
            "table_arn": table.and_then(|table| table.table_arn()),
            "status": table.and_then(|table| table.table_status()).map(|status| status.as_str()),
            "item_count": table.and_then(|table| table.item_count()),
            "size_bytes": table.and_then(|table| table.table_size_bytes()),
            "key_schema": table
                .map(|table| {
                    table.key_schema()
                        .iter()
                        .map(|key| {
                            serde_json::json!({
                                "attribute_name": key.attribute_name(),
                                "key_type": key.key_type().as_str(),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        }))?,
    )
}

async fn get(globals: &Globals, client: &aws_sdk_dynamodb::Client, args: &StubArgs) -> Result<()> {
    let positional = common::positional(&args.args);
    let table = common::required(&positional, 0, "table")?;
    let key = parse_item_json(&common::required(&positional, 1, "key-json")?)?;
    let mut request = client.get_item().table_name(table).set_key(Some(key));
    if let Some(projection) = common::option_value(&args.args, "--projection") {
        request = request.projection_expression(projection);
    }
    if let Some(names) = option_string_map(args, "--names")? {
        request = request.set_expression_attribute_names(Some(names));
    }
    let response = request.send().await.map_err(AwlError::aws)?;
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "item": response.item().map(item_to_json),
        }))?,
    )
}

async fn query(
    globals: &Globals,
    client: &aws_sdk_dynamodb::Client,
    args: &StubArgs,
) -> Result<()> {
    let positional = common::positional(&args.args);
    let table = common::required(&positional, 0, "table")?;
    let key_condition = common::option_value(&args.args, "--key-condition")
        .or_else(|| positional.get(1).cloned())
        .ok_or_else(|| AwlError::Usage {
            message: "ddb query requires --key-condition <expr>".to_owned(),
        })?;
    let mut request = client
        .query()
        .table_name(table)
        .key_condition_expression(key_condition);
    if let Some(filter) = common::option_value(&args.args, "--filter") {
        request = request.filter_expression(filter);
    }
    if let Some(limit) =
        common::option_value(&args.args, "--limit").and_then(|value| value.parse::<i32>().ok())
    {
        request = request.limit(limit);
    }
    request = apply_expression_maps(request, args)?;
    let response = request.send().await.map_err(AwlError::aws)?;
    emit_items(globals, response.items())
}

async fn scan(globals: &Globals, client: &aws_sdk_dynamodb::Client, args: &StubArgs) -> Result<()> {
    let positional = common::positional(&args.args);
    let table = common::required(&positional, 0, "table")?;
    let mut request = client.scan().table_name(table);
    if let Some(filter) = common::option_value(&args.args, "--filter") {
        request = request.filter_expression(filter);
    }
    if let Some(limit) =
        common::option_value(&args.args, "--limit").and_then(|value| value.parse::<i32>().ok())
    {
        request = request.limit(limit);
    }
    if let Some(names) = option_string_map(args, "--names")? {
        request = request.set_expression_attribute_names(Some(names));
    }
    if let Some(values) = option_attribute_map(args, "--values")? {
        request = request.set_expression_attribute_values(Some(values));
    }
    let response = request.send().await.map_err(AwlError::aws)?;
    emit_items(globals, response.items())
}

async fn put(globals: &Globals, client: &aws_sdk_dynamodb::Client, args: &StubArgs) -> Result<()> {
    let positional = common::positional(&args.args);
    let table = common::required(&positional, 0, "table")?;
    let item = parse_item_json(&common::required(&positional, 1, "item-json")?)?;
    client
        .put_item()
        .table_name(table.clone())
        .set_item(Some(item.clone()))
        .send()
        .await
        .map_err(AwlError::aws)?;
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "table": table,
            "item": item_to_json(&item),
            "action": "put",
        }))?,
    )
}

async fn rm(globals: &Globals, client: &aws_sdk_dynamodb::Client, args: &StubArgs) -> Result<()> {
    let positional = common::positional(&args.args);
    let table = common::required(&positional, 0, "table")?;
    let key = parse_item_json(&common::required(&positional, 1, "key-json")?)?;
    client
        .delete_item()
        .table_name(table.clone())
        .set_key(Some(key.clone()))
        .send()
        .await
        .map_err(AwlError::aws)?;
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "table": table,
            "key": item_to_json(&key),
            "deleted": true,
        }))?,
    )
}

fn apply_expression_maps(
    mut request: aws_sdk_dynamodb::operation::query::builders::QueryFluentBuilder,
    args: &StubArgs,
) -> Result<aws_sdk_dynamodb::operation::query::builders::QueryFluentBuilder> {
    if let Some(names) = option_string_map(args, "--names")? {
        request = request.set_expression_attribute_names(Some(names));
    }
    if let Some(values) = option_attribute_map(args, "--values")? {
        request = request.set_expression_attribute_values(Some(values));
    }
    Ok(request)
}

fn emit_items(globals: &Globals, items: &[HashMap<String, AttributeValue>]) -> Result<()> {
    output::emit(
        globals,
        Output::Many(items.iter().map(item_to_json).collect::<Vec<_>>()),
    )
}

fn option_string_map(args: &StubArgs, name: &str) -> Result<Option<HashMap<String, String>>> {
    common::option_value(&args.args, name)
        .map(|value| serde_json::from_str(&value).map_err(AwlError::from))
        .transpose()
}

fn option_attribute_map(
    args: &StubArgs,
    name: &str,
) -> Result<Option<HashMap<String, AttributeValue>>> {
    common::option_value(&args.args, name)
        .map(|value| parse_item_json(&value))
        .transpose()
}

fn parse_item_json(value: &str) -> Result<HashMap<String, AttributeValue>> {
    let value = serde_json::from_str::<serde_json::Value>(value)?;
    let serde_json::Value::Object(map) = value else {
        return Err(AwlError::Usage {
            message: "expected a JSON object".to_owned(),
        });
    };
    map.into_iter()
        .map(|(key, value)| Ok((key, json_to_attr(value)?)))
        .collect()
}

fn json_to_attr(value: serde_json::Value) -> Result<AttributeValue> {
    match value {
        serde_json::Value::Null => Ok(AttributeValue::Null(true)),
        serde_json::Value::Bool(value) => Ok(AttributeValue::Bool(value)),
        serde_json::Value::Number(value) => Ok(AttributeValue::N(value.to_string())),
        serde_json::Value::String(value) => Ok(AttributeValue::S(value)),
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(json_to_attr)
            .collect::<Result<Vec<_>>>()
            .map(AttributeValue::L),
        serde_json::Value::Object(mut map) => {
            if map.len() == 1 {
                let key = map.keys().next().cloned().unwrap_or_default();
                let value = map.remove(&key).unwrap_or(serde_json::Value::Null);
                return typed_attr(&key, value);
            }
            map.into_iter()
                .map(|(key, value)| Ok((key, json_to_attr(value)?)))
                .collect::<Result<HashMap<_, _>>>()
                .map(AttributeValue::M)
        }
    }
}

fn typed_attr(key: &str, value: serde_json::Value) -> Result<AttributeValue> {
    match key {
        "S" => Ok(AttributeValue::S(require_string(value, "S")?)),
        "N" => Ok(AttributeValue::N(match value {
            serde_json::Value::Number(number) => number.to_string(),
            other => require_string(other, "N")?,
        })),
        "BOOL" => Ok(AttributeValue::Bool(require_bool(value, "BOOL")?)),
        "NULL" => Ok(AttributeValue::Null(require_bool(value, "NULL")?)),
        "SS" => Ok(AttributeValue::Ss(require_string_array(value, "SS")?)),
        "NS" => Ok(AttributeValue::Ns(require_string_array(value, "NS")?)),
        "B" => Ok(AttributeValue::B(aws_smithy_types::Blob::new(
            decode_base64(require_string(value, "B")?)?,
        ))),
        "BS" => Ok(AttributeValue::Bs(
            require_string_array(value, "BS")?
                .into_iter()
                .map(decode_base64)
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .map(aws_smithy_types::Blob::new)
                .collect(),
        )),
        "L" => match value {
            serde_json::Value::Array(values) => values
                .into_iter()
                .map(json_to_attr)
                .collect::<Result<Vec<_>>>()
                .map(AttributeValue::L),
            _ => Err(AwlError::Usage {
                message: "DynamoDB L value must be an array".to_owned(),
            }),
        },
        "M" => match value {
            serde_json::Value::Object(map) => map
                .into_iter()
                .map(|(key, value)| Ok((key, json_to_attr(value)?)))
                .collect::<Result<HashMap<_, _>>>()
                .map(AttributeValue::M),
            _ => Err(AwlError::Usage {
                message: "DynamoDB M value must be an object".to_owned(),
            }),
        },
        _ => serde_json::Map::from_iter([(key.to_owned(), value)])
            .into_iter()
            .map(|(key, value)| Ok((key, json_to_attr(value)?)))
            .collect::<Result<HashMap<_, _>>>()
            .map(AttributeValue::M),
    }
}

fn attr_to_json(value: &AttributeValue) -> serde_json::Value {
    match value {
        AttributeValue::S(value) => serde_json::Value::String(value.clone()),
        AttributeValue::N(value) => {
            serde_json::from_str(value).unwrap_or_else(|_| serde_json::Value::String(value.clone()))
        }
        AttributeValue::Bool(value) => serde_json::Value::Bool(*value),
        AttributeValue::Null(_) => serde_json::Value::Null,
        AttributeValue::L(values) => {
            serde_json::Value::Array(values.iter().map(attr_to_json).collect())
        }
        AttributeValue::M(values) => item_to_json(values),
        AttributeValue::Ss(values) => serde_json::json!(values),
        AttributeValue::Ns(values) => serde_json::Value::Array(
            values
                .iter()
                .map(|value| {
                    serde_json::from_str(value)
                        .unwrap_or_else(|_| serde_json::Value::String(value.clone()))
                })
                .collect(),
        ),
        AttributeValue::B(value) => serde_json::Value::String(
            base64::engine::general_purpose::STANDARD.encode(value.as_ref()),
        ),
        AttributeValue::Bs(values) => serde_json::Value::Array(
            values
                .iter()
                .map(|value| {
                    serde_json::Value::String(
                        base64::engine::general_purpose::STANDARD.encode(value.as_ref()),
                    )
                })
                .collect(),
        ),
        _ => serde_json::Value::Null,
    }
}

fn item_to_json(item: &HashMap<String, AttributeValue>) -> serde_json::Value {
    serde_json::Value::Object(
        item.iter()
            .map(|(key, value)| (key.clone(), attr_to_json(value)))
            .collect(),
    )
}

fn require_string(value: serde_json::Value, name: &str) -> Result<String> {
    match value {
        serde_json::Value::String(value) => Ok(value),
        _ => Err(AwlError::Usage {
            message: format!("DynamoDB {name} value must be a string"),
        }),
    }
}

fn require_bool(value: serde_json::Value, name: &str) -> Result<bool> {
    match value {
        serde_json::Value::Bool(value) => Ok(value),
        _ => Err(AwlError::Usage {
            message: format!("DynamoDB {name} value must be a bool"),
        }),
    }
}

fn require_string_array(value: serde_json::Value, name: &str) -> Result<Vec<String>> {
    match value {
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(|value| require_string(value, name))
            .collect(),
        _ => Err(AwlError::Usage {
            message: format!("DynamoDB {name} value must be an array of strings"),
        }),
    }
}

fn decode_base64(value: String) -> Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|error| AwlError::Usage {
            message: format!("invalid base64 value: {error}"),
        })
}
