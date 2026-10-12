use aws_sdk_kms::types::DataKeySpec as SdkDataKeySpec;
use base64::Engine;

use crate::cli::{
    BlobEncoding, DataKeySpec, Globals, KmsCommand, KmsDataKeyArgs, KmsDecryptArgs, KmsEncryptArgs,
    KmsSubcommand,
};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};
use crate::services::common;

pub async fn run(globals: &Globals, command: &KmsCommand) -> Result<()> {
    let context = AwsContext::new(globals).await?;
    let client = context.kms();
    match &command.command {
        KmsSubcommand::Ls => {
            let mut rows = Vec::new();
            let mut pages = client.list_keys().into_paginator().send();
            while let Some(page) = pages.next().await {
                rows.extend(page.map_err(AwlError::sdk)?.keys().iter().map(|key| {
                    serde_json::json!({
                        "key_id": key.key_id(),
                        "key_arn": key.key_arn(),
                    })
                }));
            }
            output::emit(globals, Output::Many(rows))
        }
        KmsSubcommand::Encrypt(args) => encrypt(globals, &client, args).await,
        KmsSubcommand::Decrypt(args) => decrypt(globals, &client, args).await,
        KmsSubcommand::GenerateDataKey(args) => generate_data_key(globals, &client, args).await,
    }
}

async fn encrypt(
    globals: &Globals,
    client: &aws_sdk_kms::Client,
    args: &KmsEncryptArgs,
) -> Result<()> {
    let plaintext = common::stdin_or_bytes(&args.plaintext)?;
    let response = client
        .encrypt()
        .key_id(&args.key_id)
        .plaintext(aws_sdk_kms::primitives::Blob::new(plaintext))
        .set_encryption_context(context_map(&args.context))
        .send()
        .await
        .map_err(AwlError::sdk)?;
    let ciphertext = response
        .ciphertext_blob()
        .map(|blob| encode_bytes(blob.as_ref(), args.encoding))
        .transpose()?;
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "ciphertext": ciphertext,
            "key_id": response.key_id(),
        }))?,
    )
}

async fn decrypt(
    globals: &Globals,
    client: &aws_sdk_kms::Client,
    args: &KmsDecryptArgs,
) -> Result<()> {
    let input = common::stdin_or_string(&args.ciphertext)?;
    let ciphertext = decode_bytes(input.trim(), args.input_encoding)?;
    let response = client
        .decrypt()
        .ciphertext_blob(aws_sdk_kms::primitives::Blob::new(ciphertext))
        .set_key_id(args.key_id.clone())
        .set_encryption_context(context_map(&args.context))
        .send()
        .await
        .map_err(AwlError::sdk)?;
    let plaintext = response
        .plaintext()
        .map(|blob| encode_bytes(blob.as_ref(), args.encoding))
        .transpose()?;
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "plaintext": plaintext,
            "key_id": response.key_id(),
        }))?,
    )
}

async fn generate_data_key(
    globals: &Globals,
    client: &aws_sdk_kms::Client,
    args: &KmsDataKeyArgs,
) -> Result<()> {
    let spec = match args.spec {
        DataKeySpec::Aes256 => SdkDataKeySpec::Aes256,
        DataKeySpec::Aes128 => SdkDataKeySpec::Aes128,
    };
    let response = client
        .generate_data_key()
        .key_id(&args.key_id)
        .key_spec(spec)
        .send()
        .await
        .map_err(AwlError::sdk)?;
    let b64 = |blob: &aws_sdk_kms::primitives::Blob| {
        base64::engine::general_purpose::STANDARD.encode(blob.as_ref())
    };
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "key_id": response.key_id(),
            "plaintext": response.plaintext().map(b64),
            "ciphertext": response.ciphertext_blob().map(b64),
        }))?,
    )
}

fn context_map(pairs: &[(String, String)]) -> Option<std::collections::HashMap<String, String>> {
    (!pairs.is_empty()).then(|| pairs.iter().cloned().collect())
}

fn encode_bytes(bytes: &[u8], encoding: BlobEncoding) -> Result<String> {
    match encoding {
        BlobEncoding::B64 => Ok(base64::engine::general_purpose::STANDARD.encode(bytes)),
        BlobEncoding::Hex => Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect()),
        BlobEncoding::Raw => String::from_utf8(bytes.to_vec()).map_err(|_| AwlError::Usage {
            message: "value is binary; use --encoding b64 or hex".to_owned(),
        }),
    }
}

fn decode_bytes(value: &str, encoding: BlobEncoding) -> Result<Vec<u8>> {
    match encoding {
        BlobEncoding::B64 => base64::engine::general_purpose::STANDARD
            .decode(value)
            .map_err(|error| AwlError::Usage {
                message: format!("invalid base64 ciphertext: {error}"),
            }),
        BlobEncoding::Hex => decode_hex(value),
        BlobEncoding::Raw => Ok(value.as_bytes().to_vec()),
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
            std::str::from_utf8(chunk)
                .ok()
                .and_then(|text| u8::from_str_radix(text, 16).ok())
                .ok_or_else(|| AwlError::Usage {
                    message: "invalid hex input".to_owned(),
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::cli::BlobEncoding;

    #[test]
    fn kms_hex_round_trips_bytes() {
        let bytes = vec![0, 1, 15, 16, 255];
        let encoded =
            crate::services::kms::encode_bytes(&bytes, BlobEncoding::Hex).expect("hex encodes");
        assert_eq!(encoded, "00010f10ff");
        let decoded =
            crate::services::kms::decode_bytes(&encoded, BlobEncoding::Hex).expect("hex decodes");
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn raw_encoding_refuses_binary() {
        assert!(crate::services::kms::encode_bytes(&[0xff, 0xfe], BlobEncoding::Raw).is_err());
    }
}
