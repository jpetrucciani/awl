use std::io::{self, IsTerminal, Write};

use serde::Serialize;
use serde_json::Value;

use crate::cli::{Globals, OutputFormat};
use crate::error::{AwlError, Result};

#[derive(Debug, Clone)]
pub enum Output {
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

/// Write command results. Always printed, even with `--quiet`.
pub fn emit(globals: &Globals, output: Output) -> Result<()> {
    let selected = select_field(globals, output)?;
    let mut stdout = io::stdout().lock();
    match (globals.output, selected) {
        // Shell snippets such as `sts assume` must stay raw for `eval "$(...)"`
        // unless a structured format was explicitly requested.
        (None, Output::Plain(value)) => writeln!(stdout, "{value}")?,
        (format, selected) => match format.unwrap_or_else(auto_format) {
            OutputFormat::Table => write_table(&mut stdout, selected)?,
            OutputFormat::Json => write_json(&mut stdout, selected)?,
            OutputFormat::Jsonl => write_jsonl(&mut stdout, selected)?,
            OutputFormat::Toml => write_toml(&mut stdout, selected)?,
            OutputFormat::Yaml => write_yaml(&mut stdout, selected)?,
            OutputFormat::Tsv => write_tsv(&mut stdout, selected)?,
            OutputFormat::Plain => write_plain(&mut stdout, selected)?,
        },
    }
    stdout.flush()?;
    Ok(())
}

/// Write the acknowledgement of a mutation. Suppressed by `--quiet`.
pub fn ack(globals: &Globals, output: Output) -> Result<()> {
    if globals.quiet {
        return Ok(());
    }
    emit(globals, output)
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
        Output::One(Value::Object(mut map)) => {
            let Some(value) = map.remove(field) else {
                return Err(AwlError::Usage {
                    message: format!("field {field:?} does not exist"),
                });
            };
            Ok(Output::One(value))
        }
        Output::One(_) => Err(AwlError::Usage {
            message: "--field requires an object output".to_owned(),
        }),
        Output::Many(mut values) if values.len() == 1 => {
            select_field(globals, Output::One(values.remove(0)))
        }
        Output::Many(_) => Err(AwlError::Usage {
            message: "--field cannot select from multiple output records".to_owned(),
        }),
        Output::Plain(_) => Err(AwlError::Usage {
            message: "--field requires structured output".to_owned(),
        }),
    }
}

fn write_json(out: &mut impl Write, output: Output) -> Result<()> {
    match output {
        Output::One(value) => writeln!(out, "{}", serde_json::to_string_pretty(&value)?)?,
        Output::Many(values) => writeln!(out, "{}", serde_json::to_string_pretty(&values)?)?,
        Output::Plain(value) => writeln!(out, "{}", serde_json::to_string(&value)?)?,
    }
    Ok(())
}

fn write_jsonl(out: &mut impl Write, output: Output) -> Result<()> {
    match output {
        Output::One(value) => writeln!(out, "{}", serde_json::to_string(&value)?)?,
        Output::Many(values) => {
            for value in values {
                writeln!(out, "{}", serde_json::to_string(&value)?)?;
            }
        }
        Output::Plain(value) => writeln!(out, "{}", serde_json::to_string(&value)?)?,
    }
    Ok(())
}

/// TOML has no null and no top-level arrays or scalars: drop nulls, wrap lists
/// as `[[items]]`, and print scalars as plain values.
fn write_toml(out: &mut impl Write, output: Output) -> Result<()> {
    let value = match output {
        Output::One(value @ Value::Object(_)) => value,
        Output::One(value) => return write_plain(out, Output::One(value)),
        Output::Many(values) => serde_json::json!({ "items": values }),
        Output::Plain(value) => {
            writeln!(out, "{value}")?;
            return Ok(());
        }
    };
    write!(out, "{}", toml::to_string_pretty(&strip_nulls(value))?)?;
    Ok(())
}

fn strip_nulls(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| (key, strip_nulls(value)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .filter(|value| !value.is_null())
                .map(strip_nulls)
                .collect(),
        ),
        other => other,
    }
}

fn write_yaml(out: &mut impl Write, output: Output) -> Result<()> {
    match output {
        Output::One(value) => write!(out, "{}", serde_yaml::to_string(&value)?)?,
        Output::Many(values) => write!(out, "{}", serde_yaml::to_string(&values)?)?,
        Output::Plain(value) => write!(out, "{}", serde_yaml::to_string(&value)?)?,
    }
    Ok(())
}

fn write_plain(out: &mut impl Write, output: Output) -> Result<()> {
    match output {
        Output::Plain(value) => writeln!(out, "{value}")?,
        Output::One(value) => writeln!(out, "{}", plain_value(&value))?,
        Output::Many(values) => {
            for value in values {
                writeln!(out, "{}", plain_value(&value))?;
            }
        }
    }
    Ok(())
}

fn write_tsv(out: &mut impl Write, output: Output) -> Result<()> {
    let rows = match output {
        Output::Plain(value) => {
            writeln!(out, "{value}")?;
            return Ok(());
        }
        Output::One(value) => vec![value],
        Output::Many(values) => values,
    };
    let headers = headers(&rows);
    for row in &rows {
        if headers.is_empty() {
            writeln!(out, "{}", plain_value(row))?;
        } else {
            let cells = headers
                .iter()
                .map(|header| row.get(header).map(plain_value).unwrap_or_default())
                .collect::<Vec<_>>();
            writeln!(out, "{}", cells.join("\t"))?;
        }
    }
    Ok(())
}

fn write_table(out: &mut impl Write, output: Output) -> Result<()> {
    let rows = match output {
        Output::Plain(value) => {
            writeln!(out, "{value}")?;
            return Ok(());
        }
        Output::One(value) => vec![value],
        Output::Many(values) => values,
    };

    let headers = headers(&rows);
    if headers.is_empty() {
        for row in rows {
            writeln!(out, "{}", plain_value(&row))?;
        }
        return Ok(());
    }

    let mut widths = headers
        .iter()
        .map(|header| header.chars().count())
        .collect::<Vec<_>>();
    let rendered = rows
        .iter()
        .map(|row| {
            headers
                .iter()
                .enumerate()
                .map(|(idx, header)| {
                    let value = row.get(header).map(plain_value).unwrap_or_default();
                    widths[idx] = widths[idx].max(value.chars().count());
                    value
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    write_padded_row(out, &headers, &widths)?;
    for row in rendered {
        write_padded_row(out, &row, &widths)?;
    }
    Ok(())
}

/// Column order is the first-seen key order across rows.
fn headers(rows: &[Value]) -> Vec<String> {
    let mut headers = Vec::<String>::new();
    for row in rows {
        if let Value::Object(map) = row {
            for key in map.keys() {
                if !headers.contains(key) {
                    headers.push(key.clone());
                }
            }
        }
    }
    headers
}

fn write_padded_row(out: &mut impl Write, row: &[String], widths: &[usize]) -> Result<()> {
    let last = row.len().saturating_sub(1);
    for (idx, cell) in row.iter().enumerate() {
        if idx > 0 {
            write!(out, "  ")?;
        }
        if idx == last {
            write!(out, "{cell}")?;
        } else {
            write!(out, "{cell:width$}", width = widths[idx])?;
        }
    }
    writeln!(out)?;
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn toml_wraps_lists_and_drops_nulls() {
        let mut out = Vec::new();
        crate::output::write_toml(
            &mut out,
            crate::output::Output::Many(vec![json!({ "name": "a", "missing": null })]),
        )
        .expect("toml renders");
        let text = String::from_utf8(out).expect("utf-8");
        assert!(text.contains("[[items]]"), "{text}");
        assert!(text.contains("name = \"a\""), "{text}");
        assert!(!text.contains("missing"), "{text}");
    }

    #[test]
    fn table_keeps_field_order() {
        let mut out = Vec::new();
        crate::output::write_table(
            &mut out,
            crate::output::Output::One(json!({ "zeta": 1, "alpha": 2 })),
        )
        .expect("table renders");
        let text = String::from_utf8(out).expect("utf-8");
        assert!(text.starts_with("zeta  alpha"), "{text}");
    }
}
