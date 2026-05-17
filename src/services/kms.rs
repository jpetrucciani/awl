use base64::Engine;
use std::io::Read;

use crate::cli::{Globals, KmsCommand, KmsCommandSubcommand, StubArgs};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};
use crate::services::common;

pub async fn run(globals: &Globals, command: &KmsCommand) -> Result<()> {
    let context = AwsContext::new(globals).await?;
    let client = context.kms();
    match &command.command {
        KmsCommandSubcommand::Ls(_) => {
            let response = client.list_keys().send().await.map_err(AwlError::aws)?;
            let rows = response
                .keys()
                .iter()
                .map(|key| {
                    serde_json::json!({
                        "key_id": key.key_id(),
                        "key_arn": key.key_arn(),
                    })
                })
                .collect::<Vec<_>>();
            output::emit(globals, Output::Many(rows))
        }
        KmsCommandSubcommand::Encrypt(args) => encrypt(globals, &client, args).await,
        KmsCommandSubcommand::Decrypt(args) => decrypt(globals, &client, args).await,
        KmsCommandSubcommand::GenerateDataKey(args) => {
            generate_data_key(globals, &client, args).await
        }
    }
}

async fn encrypt(globals: &Globals, client: &aws_sdk_kms::Client, args: &StubArgs) -> Result<()> {
    let positional = common::positional(&args.args);
    let key_id = positional
        .first()
        .cloned()
        .or_else(|| std::env::var("AWL_KMS_KEY").ok())
        .ok_or_else(|| AwlError::Usage {
            message: "missing <key-id> and AWL_KMS_KEY is unset".to_owned(),
        })?;
    let plaintext = read_arg_bytes(&common::required(&positional, 1, "plaintext")?)?;
    let mut request = client
        .encrypt()
        .key_id(key_id)
        .plaintext(aws_sdk_kms::primitives::Blob::new(plaintext));
    for (key, value) in common::key_values(&args.args, "--context")? {
        request = request.encryption_context(key, value);
    }
    let response = request.send().await.map_err(AwlError::aws)?;
    let encoding =
        common::option_value(&args.args, "--encoding").unwrap_or_else(|| "b64".to_owned());
    let blob = response
        .ciphertext_blob()
        .map(|blob| encode_bytes(blob.as_ref(), &encoding))
        .transpose()?;
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "ciphertext": blob,
        }))?,
    )
}

async fn decrypt(globals: &Globals, client: &aws_sdk_kms::Client, args: &StubArgs) -> Result<()> {
    let positional = common::positional(&args.args);
    let ciphertext = common::required(&positional, 0, "ciphertext")?;
    let input =
        common::option_value(&args.args, "--input-encoding").unwrap_or_else(|| "b64".to_owned());
    let bytes = decode_bytes(read_arg_string(&ciphertext)?, &input)?;
    let mut request = client
        .decrypt()
        .ciphertext_blob(aws_sdk_kms::primitives::Blob::new(bytes));
    if let Some(key_id) = common::option_value(&args.args, "--key-id") {
        request = request.key_id(key_id);
    }
    for (key, value) in common::key_values(&args.args, "--context")? {
        request = request.encryption_context(key, value);
    }
    let response = request.send().await.map_err(AwlError::aws)?;
    let plaintext = response
        .plaintext()
        .map(|blob| String::from_utf8_lossy(blob.as_ref()).to_string());
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "plaintext": plaintext,
            "key_id": response.key_id(),
        }))?,
    )
}

fn read_arg_bytes(value: &str) -> Result<Vec<u8>> {
    if value == "-" {
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        Ok(bytes)
    } else {
        Ok(value.as_bytes().to_vec())
    }
}

fn read_arg_string(value: &str) -> Result<String> {
    if value == "-" {
        let mut out = String::new();
        std::io::stdin().read_to_string(&mut out)?;
        Ok(out.trim_end_matches('\n').to_owned())
    } else {
        Ok(value.to_owned())
    }
}

fn encode_bytes(bytes: &[u8], encoding: &str) -> Result<String> {
    match encoding {
        "b64" => Ok(base64::engine::general_purpose::STANDARD.encode(bytes)),
        "hex" => Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect()),
        "raw" => Ok(String::from_utf8_lossy(bytes).to_string()),
        other => Err(AwlError::Usage {
            message: format!("unknown encoding {other:?}; expected b64, hex, or raw"),
        }),
    }
}

fn decode_bytes(value: String, encoding: &str) -> Result<Vec<u8>> {
    match encoding {
        "b64" => base64::engine::general_purpose::STANDARD
            .decode(value)
            .map_err(|error| AwlError::Usage {
                message: format!("invalid base64 ciphertext: {error}"),
            }),
        "hex" => decode_hex(&value),
        "raw" => Ok(value.into_bytes()),
        other => Err(AwlError::Usage {
            message: format!("unknown input encoding {other:?}; expected b64, hex, or raw"),
        }),
    }
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return Err(AwlError::Usage {
            message: "hex input must have an even number of characters".to_owned(),
        });
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|chunk| {
            let text = std::str::from_utf8(chunk).map_err(|error| AwlError::Usage {
                message: format!("invalid hex input: {error}"),
            })?;
            u8::from_str_radix(text, 16).map_err(|error| AwlError::Usage {
                message: format!("invalid hex input: {error}"),
            })
        })
        .collect()
}

async fn generate_data_key(
    globals: &Globals,
    client: &aws_sdk_kms::Client,
    args: &StubArgs,
) -> Result<()> {
    let positional = common::positional(&args.args);
    let key_id = positional
        .first()
        .cloned()
        .or_else(|| std::env::var("AWL_KMS_KEY").ok())
        .ok_or_else(|| AwlError::Usage {
            message: "missing <key-id> and AWL_KMS_KEY is unset".to_owned(),
        })?;
    let spec = common::option_value(&args.args, "--spec").unwrap_or_else(|| "AES_256".to_owned());
    let response = client
        .generate_data_key()
        .key_id(key_id)
        .key_spec(aws_sdk_kms::types::DataKeySpec::from(spec.as_str()))
        .send()
        .await
        .map_err(AwlError::aws)?;
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "key_id": response.key_id(),
            "plaintext": response.plaintext().map(|blob| base64::engine::general_purpose::STANDARD.encode(blob.as_ref())),
            "ciphertext": response.ciphertext_blob().map(|blob| base64::engine::general_purpose::STANDARD.encode(blob.as_ref())),
        }))?,
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn kms_hex_round_trips_bytes() {
        let bytes = vec![0, 1, 15, 16, 255];
        let encoded = crate::services::kms::encode_bytes(&bytes, "hex").expect("hex encodes");
        assert_eq!(encoded, "00010f10ff");
        let decoded = crate::services::kms::decode_bytes(encoded, "hex").expect("hex decodes");
        assert_eq!(decoded, bytes);
    }
}
