use std::collections::HashMap;

use aws_sdk_dynamodb::types::AttributeValue;
use base64::Engine;

use crate::cli::{DdbCommand, DdbExpressionArgs, DdbSubcommand, Globals};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};

/// Largest integer an f64 holds exactly; plain JSON numbers past it lose digits.
const MAX_EXACT_F64: f64 = 9_007_199_254_740_992.0;
const TYPE_TAGS: [&str; 10] = ["S", "N", "B", "BOOL", "NULL", "SS", "NS", "BS", "L", "M"];

pub async fn run(globals: &Globals, command: &DdbCommand) -> Result<()> {
    let context = AwsContext::new(globals).await?;
    let client = context.ddb();
    match &command.command {
        DdbSubcommand::Ls => {
            let mut rows = Vec::new();
            let mut pages = client.list_tables().into_paginator().send();
            while let Some(page) = pages.next().await {
                rows.extend(
                    page.map_err(AwlError::sdk)?
                        .table_names()
                        .iter()
                        .map(|name| serde_json::json!({ "table_name": name })),
                );
            }
            output::emit(globals, Output::Many(rows))
        }
        DdbSubcommand::Desc(args) => {
            let response = client
                .describe_table()
                .table_name(&args.table)
                .send()
                .await
                .map_err(AwlError::sdk)?;
            let table = response.table();
            output::emit(
                globals,
                Output::one(serde_json::json!({
                    "table_name": table.and_then(|table| table.table_name()),
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
                    "table_arn": table.and_then(|table| table.table_arn()),
                }))?,
            )
        }
        DdbSubcommand::Get(args) => {
            let response = client
                .get_item()
                .table_name(&args.table)
                .set_key(Some(parse_item_json(&args.key)?))
                .set_projection_expression(args.projection.clone())
                .set_expression_attribute_names(string_map(args.names.as_deref())?)
                .send()
                .await
                .map_err(AwlError::sdk)?;
            output::emit(
                globals,
                Output::one(serde_json::json!({
                    "item": response.item().map(item_to_json),
                }))?,
            )
        }
        DdbSubcommand::Query(args) => {
            let expression = &args.expression;
            let limit = expression.limit.unwrap_or(usize::MAX);
            let mut items = Vec::new();
            let mut pages = client
                .query()
                .table_name(&args.table)
                .key_condition_expression(&args.key_condition)
                .set_filter_expression(expression.filter.clone())
                .set_limit(page_limit(expression))
                .set_expression_attribute_names(string_map(expression.names.as_deref())?)
                .set_expression_attribute_values(attribute_map(expression.values.as_deref())?)
                .into_paginator()
                .send();
            while items.len() < limit {
                let Some(page) = pages.next().await else {
                    break;
                };
                items.extend(
                    page.map_err(AwlError::sdk)?
                        .items()
                        .iter()
                        .map(item_to_json),
                );
            }
            items.truncate(limit);
            output::emit(globals, Output::Many(items))
        }
        DdbSubcommand::Scan(args) => {
            let expression = &args.expression;
            let limit = expression.limit.unwrap_or(usize::MAX);
            let mut items = Vec::new();
            let mut pages = client
                .scan()
                .table_name(&args.table)
                .set_filter_expression(expression.filter.clone())
                .set_limit(page_limit(expression))
                .set_expression_attribute_names(string_map(expression.names.as_deref())?)
                .set_expression_attribute_values(attribute_map(expression.values.as_deref())?)
                .into_paginator()
                .send();
            while items.len() < limit {
                let Some(page) = pages.next().await else {
                    break;
                };
                items.extend(
                    page.map_err(AwlError::sdk)?
                        .items()
                        .iter()
                        .map(item_to_json),
                );
            }
            items.truncate(limit);
            output::emit(globals, Output::Many(items))
        }
        DdbSubcommand::Put(args) => {
            let item = parse_item_json(&args.item)?;
            client
                .put_item()
                .table_name(&args.table)
                .set_item(Some(item.clone()))
                .send()
                .await
                .map_err(AwlError::sdk)?;
            output::ack(
                globals,
                Output::one(serde_json::json!({
                    "table": args.table,
                    "item": item_to_json(&item),
                    "action": "put",
                }))?,
            )
        }
        DdbSubcommand::Rm(args) => {
            let key = parse_item_json(&args.key)?;
            client
                .delete_item()
                .table_name(&args.table)
                .set_key(Some(key.clone()))
                .send()
                .await
                .map_err(AwlError::sdk)?;
            output::ack(
                globals,
                Output::one(serde_json::json!({
                    "table": args.table,
                    "key": item_to_json(&key),
                    "deleted": true,
                }))?,
            )
        }
    }
}

/// Use `--limit` as the page size too, so small limits do not over-read.
fn page_limit(expression: &DdbExpressionArgs) -> Option<i32> {
    expression
        .limit
        .map(|limit| i32::try_from(limit).unwrap_or(i32::MAX))
}

fn string_map(value: Option<&str>) -> Result<Option<HashMap<String, String>>> {
    value
        .map(|value| serde_json::from_str(value).map_err(AwlError::from))
        .transpose()
}

fn attribute_map(value: Option<&str>) -> Result<Option<HashMap<String, AttributeValue>>> {
    value.map(parse_item_json).transpose()
}

/// Parse an item, key, or value map. The whole object is DynamoDB JSON when
/// every attribute is a type descriptor such as {"S": "x"}; otherwise it is
/// plain JSON throughout, so a nested {"N": "north"} stays a map.
fn parse_item_json(value: &str) -> Result<HashMap<String, AttributeValue>> {
    let value = serde_json::from_str::<serde_json::Value>(value)?;
    let serde_json::Value::Object(map) = value else {
        return Err(AwlError::Usage {
            message: "expected a JSON object".to_owned(),
        });
    };
    let typed = !map.is_empty() && map.values().all(is_type_descriptor);
    map.into_iter()
        .map(|(key, value)| {
            let value = if typed {
                typed_attr(value)?
            } else {
                plain_attr(value)?
            };
            Ok((key, value))
        })
        .collect()
}

fn is_type_descriptor(value: &serde_json::Value) -> bool {
    value.as_object().is_some_and(|map| {
        map.len() == 1 && map.keys().all(|key| TYPE_TAGS.contains(&key.as_str()))
    })
}

fn plain_attr(value: serde_json::Value) -> Result<AttributeValue> {
    Ok(match value {
        serde_json::Value::Null => AttributeValue::Null(true),
        serde_json::Value::Bool(value) => AttributeValue::Bool(value),
        serde_json::Value::Number(value) => AttributeValue::N(exact_number(&value)?),
        serde_json::Value::String(value) => AttributeValue::S(value),
        serde_json::Value::Array(values) => AttributeValue::L(
            values
                .into_iter()
                .map(plain_attr)
                .collect::<Result<Vec<_>>>()?,
        ),
        serde_json::Value::Object(map) => AttributeValue::M(
            map.into_iter()
                .map(|(key, value)| Ok((key, plain_attr(value)?)))
                .collect::<Result<HashMap<_, _>>>()?,
        ),
    })
}

fn exact_number(value: &serde_json::Number) -> Result<String> {
    if value.is_f64()
        && value
            .as_f64()
            .is_some_and(|number| number.abs() >= MAX_EXACT_F64)
    {
        return Err(AwlError::Usage {
            message: format!(
                "number {value} cannot be represented exactly in plain JSON; use DynamoDB JSON such as {{\"N\": \"12345678901234567890\"}}"
            ),
        });
    }
    Ok(value.to_string())
}

fn typed_attr(value: serde_json::Value) -> Result<AttributeValue> {
    let serde_json::Value::Object(map) = value else {
        return Err(AwlError::Usage {
            message: "DynamoDB JSON values must be type descriptors such as {\"S\": \"x\"}"
                .to_owned(),
        });
    };
    let Some((tag, value)) = map.into_iter().next() else {
        return Err(AwlError::Usage {
            message: "empty DynamoDB JSON type descriptor".to_owned(),
        });
    };
    Ok(match tag.as_str() {
        "S" => AttributeValue::S(require_string(value, "S")?),
        "N" => AttributeValue::N(require_number(value)?),
        "BOOL" => AttributeValue::Bool(require_bool(value, "BOOL")?),
        "NULL" => AttributeValue::Null(require_bool(value, "NULL")?),
        "SS" => AttributeValue::Ss(require_string_array(value, "SS")?),
        "NS" => AttributeValue::Ns(require_string_array(value, "NS")?),
        "B" => AttributeValue::B(aws_smithy_types::Blob::new(decode_base64(require_string(
            value, "B",
        )?)?)),
        "BS" => AttributeValue::Bs(
            require_string_array(value, "BS")?
                .into_iter()
                .map(|value| decode_base64(value).map(aws_smithy_types::Blob::new))
                .collect::<Result<Vec<_>>>()?,
        ),
        "L" => match value {
            serde_json::Value::Array(values) => AttributeValue::L(
                values
                    .into_iter()
                    .map(typed_attr)
                    .collect::<Result<Vec<_>>>()?,
            ),
            _ => {
                return Err(AwlError::Usage {
                    message: "DynamoDB L value must be an array".to_owned(),
                });
            }
        },
        "M" => match value {
            serde_json::Value::Object(map) => AttributeValue::M(
                map.into_iter()
                    .map(|(key, value)| Ok((key, typed_attr(value)?)))
                    .collect::<Result<HashMap<_, _>>>()?,
            ),
            _ => {
                return Err(AwlError::Usage {
                    message: "DynamoDB M value must be an object".to_owned(),
                });
            }
        },
        other => {
            return Err(AwlError::Usage {
                message: format!("unknown DynamoDB type {other:?}"),
            });
        }
    })
}

/// Numbers that round-trip through JSON stay numbers; anything that would
/// lose digits is returned as a string.
fn number_to_json(value: &str) -> serde_json::Value {
    if let Ok(number) = value.parse::<i64>() {
        return number.into();
    }
    if let Ok(number) = value.parse::<u64>() {
        return number.into();
    }
    value
        .parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
        .filter(|number| number.to_string() == value)
        .map(serde_json::Value::Number)
        .unwrap_or_else(|| serde_json::Value::String(value.to_owned()))
}

fn attr_to_json(value: &AttributeValue) -> serde_json::Value {
    match value {
        AttributeValue::S(value) => serde_json::Value::String(value.clone()),
        AttributeValue::N(value) => number_to_json(value),
        AttributeValue::Bool(value) => serde_json::Value::Bool(*value),
        AttributeValue::Null(_) => serde_json::Value::Null,
        AttributeValue::L(values) => {
            serde_json::Value::Array(values.iter().map(attr_to_json).collect())
        }
        AttributeValue::M(values) => item_to_json(values),
        AttributeValue::Ss(values) => serde_json::json!(values),
        AttributeValue::Ns(values) => {
            serde_json::Value::Array(values.iter().map(|value| number_to_json(value)).collect())
        }
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

fn require_number(value: serde_json::Value) -> Result<String> {
    let text = match value {
        serde_json::Value::Number(number) => exact_number(&number)?,
        serde_json::Value::String(text) => text,
        _ => String::new(),
    };
    if text.parse::<f64>().is_err() {
        return Err(AwlError::Usage {
            message: format!("DynamoDB N value must be numeric, got {text:?}"),
        });
    }
    Ok(text)
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

#[cfg(test)]
mod tests {
    use aws_sdk_dynamodb::types::AttributeValue;

    #[test]
    fn nested_type_like_maps_stay_plain() {
        let item = crate::services::ddb::parse_item_json(r#"{"pk": "a", "dir": {"N": "north"}}"#)
            .expect("plain item parses");
        let AttributeValue::M(dir) = &item["dir"] else {
            panic!("dir should be a map: {item:?}");
        };
        assert_eq!(dir["N"], AttributeValue::S("north".to_owned()));
    }

    #[test]
    fn large_numbers_keep_their_digits() {
        let item = crate::services::ddb::parse_item_json(
            r#"{"pk": {"S": "a"}, "id": {"N": "12345678901234567890123"}}"#,
        )
        .expect("typed item parses");
        assert_eq!(
            crate::services::ddb::item_to_json(&item)["id"],
            serde_json::json!("12345678901234567890123")
        );
        assert!(
            crate::services::ddb::parse_item_json(r#"{"id": 12345678901234567890123}"#).is_err()
        );
    }
}
