use std::collections::BTreeSet;
use std::io::{self, IsTerminal, Write};

use serde::Serialize;
use serde_json::Value;

use crate::cli::{Globals, OutputFormat};
use crate::error::{AwlError, Result};

#[derive(Debug, Clone)]
pub enum Output {
    Empty,
    One(Value),
    Many(Vec<Value>),
    Plain(String),
}

impl Output {
    pub fn one<T: Serialize>(value: T) -> Result<Self> {
        Ok(Self::One(serde_json::to_value(value)?))
    }

    pub fn many<T: Serialize>(values: Vec<T>) -> Result<Self> {
        let mut out = Vec::with_capacity(values.len());
        for value in values {
            out.push(serde_json::to_value(value)?);
        }
        Ok(Self::Many(out))
    }
}

pub fn emit(globals: &Globals, output: Output) -> Result<()> {
    if globals.quiet {
        return Ok(());
    }

    let format = globals.output.unwrap_or_else(auto_format);
    let selected = select_field(globals, output)?;

    match format {
        OutputFormat::Table => write_table(selected),
        OutputFormat::Json => write_json(selected),
        OutputFormat::Jsonl => write_jsonl(selected),
        OutputFormat::Toml => write_toml(selected),
        OutputFormat::Yaml => write_yaml(selected),
        OutputFormat::Tsv => write_tsv(selected),
        OutputFormat::Plain => write_plain(selected),
    }
}

fn auto_format() -> OutputFormat {
    if io::stdout().is_terminal() {
        OutputFormat::Table
    } else {
        OutputFormat::Jsonl
    }
}

fn select_field(globals: &Globals, output: Output) -> Result<Output> {
    let Some(field) = &globals.field else {
        return Ok(output);
    };

    match output {
        Output::One(Value::Object(map)) => {
            let Some(value) = map.get(field) else {
                return Err(AwlError::Usage {
                    message: format!("field {field:?} does not exist"),
                });
            };
            Ok(Output::One(value.clone()))
        }
        Output::One(_) => Err(AwlError::Usage {
            message: "--field requires an object output".to_owned(),
        }),
        Output::Many(values) if values.len() == 1 => select_field(
            globals,
            Output::One(values.into_iter().next().ok_or_else(|| AwlError::Usage {
                message: "--field requires one output record".to_owned(),
            })?),
        ),
        Output::Many(_) => Err(AwlError::Usage {
            message: "--field cannot select from multiple output records".to_owned(),
        }),
        Output::Plain(_) | Output::Empty => Err(AwlError::Usage {
            message: "--field requires structured output".to_owned(),
        }),
    }
}

fn write_json(output: Output) -> Result<()> {
    match output {
        Output::Empty => println!("null"),
        Output::One(value) => println!("{}", serde_json::to_string_pretty(&value)?),
        Output::Many(values) => println!("{}", serde_json::to_string_pretty(&values)?),
        Output::Plain(value) => println!("{}", serde_json::to_string(&value)?),
    }
    Ok(())
}

fn write_jsonl(output: Output) -> Result<()> {
    match output {
        Output::Empty => {}
        Output::One(value) => println!("{}", serde_json::to_string(&value)?),
        Output::Many(values) => {
            for value in values {
                println!("{}", serde_json::to_string(&value)?);
            }
        }
        Output::Plain(value) => println!("{}", serde_json::to_string(&value)?),
    }
    Ok(())
}

fn write_toml(output: Output) -> Result<()> {
    match output {
        Output::Empty => {}
        Output::One(value) => println!("{}", toml::to_string_pretty(&value)?),
        Output::Many(values) => println!("{}", toml::to_string_pretty(&values)?),
        Output::Plain(value) => println!("{}", toml::to_string_pretty(&value)?),
    }
    Ok(())
}

fn write_yaml(output: Output) -> Result<()> {
    match output {
        Output::Empty => {}
        Output::One(value) => println!("{}", serde_yaml::to_string(&value)?),
        Output::Many(values) => println!("{}", serde_yaml::to_string(&values)?),
        Output::Plain(value) => println!("{}", serde_yaml::to_string(&value)?),
    }
    Ok(())
}

fn write_plain(output: Output) -> Result<()> {
    match output {
        Output::Empty => {}
        Output::Plain(value) => println!("{value}"),
        Output::One(value) => println!("{}", plain_value(&value)),
        Output::Many(values) => {
            for value in values {
                println!("{}", plain_value(&value));
            }
        }
    }
    Ok(())
}

fn write_tsv(output: Output) -> Result<()> {
    match output {
        Output::Empty => {}
        Output::Plain(value) => println!("{value}"),
        Output::One(value) => println!("{}", tsv_row(&value)),
        Output::Many(values) => {
            for value in values {
                println!("{}", tsv_row(&value));
            }
        }
    }
    Ok(())
}

fn write_table(output: Output) -> Result<()> {
    let rows = match output {
        Output::Empty => return Ok(()),
        Output::Plain(value) => {
            println!("{value}");
            return Ok(());
        }
        Output::One(value) => vec![value],
        Output::Many(values) => values,
    };

    let mut headers = BTreeSet::new();
    for row in &rows {
        if let Value::Object(map) = row {
            headers.extend(map.keys().cloned());
        }
    }

    if headers.is_empty() {
        for row in rows {
            println!("{}", plain_value(&row));
        }
        return Ok(());
    }

    let headers = headers.into_iter().collect::<Vec<_>>();
    let mut widths = headers.iter().map(String::len).collect::<Vec<_>>();
    let rendered = rows
        .iter()
        .map(|row| {
            headers
                .iter()
                .enumerate()
                .map(|(idx, header)| {
                    let value = row.get(header).map(plain_value).unwrap_or_else(String::new);
                    widths[idx] = widths[idx].max(value.len());
                    value
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let mut stdout = io::stdout().lock();
    write_padded_row(&mut stdout, &headers, &widths)?;
    for row in rendered {
        write_padded_row(&mut stdout, &row, &widths)?;
    }
    Ok(())
}

fn write_padded_row(writer: &mut impl Write, row: &[String], widths: &[usize]) -> Result<()> {
    for (idx, cell) in row.iter().enumerate() {
        if idx > 0 {
            write!(writer, "  ")?;
        }
        write!(writer, "{cell:width$}", width = widths[idx])?;
    }
    writeln!(writer)?;
    Ok(())
}

fn plain_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn tsv_row(value: &Value) -> String {
    match value {
        Value::Object(map) => map.values().map(plain_value).collect::<Vec<_>>().join("\t"),
        _ => plain_value(value),
    }
}
