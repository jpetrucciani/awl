use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

use aws_sdk_ecr::types::ImageIdentifier;
use base64::Engine;

use crate::cli::{EcrCommand, EcrSubcommand, Globals};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};

pub async fn run(globals: &Globals, command: &EcrCommand) -> Result<()> {
    let context = AwsContext::new(globals).await?;
    let client = context.ecr();
    match &command.command {
        EcrSubcommand::Ls => {
            let mut rows = Vec::new();
            let mut pages = client.describe_repositories().into_paginator().send();
            while let Some(page) = pages.next().await {
                rows.extend(
                    page.map_err(AwlError::sdk)?
                        .repositories()
                        .iter()
                        .map(|repo| {
                            serde_json::json!({
                                "repository_name": repo.repository_name(),
                                "repository_uri": repo.repository_uri(),
                                "registry_id": repo.registry_id(),
                            })
                        }),
                );
            }
            output::emit(globals, Output::Many(rows))
        }
        EcrSubcommand::Tags(args) => {
            let mut rows = Vec::new();
            let mut pages = client
                .describe_images()
                .repository_name(&args.repo)
                .into_paginator()
                .send();
            while let Some(page) = pages.next().await {
                for image in page.map_err(AwlError::sdk)?.image_details() {
                    rows.extend(image.image_tags().iter().map(|tag| {
                        serde_json::json!({
                            "tag": tag,
                            "digest": image.image_digest(),
                            "pushed_at": image.image_pushed_at().map(ToString::to_string),
                            "size": image.image_size_in_bytes(),
                        })
                    }));
                }
            }
            output::emit(globals, Output::Many(rows))
        }
        EcrSubcommand::Login(args) => login(globals, &client, args.print_password).await,
        EcrSubcommand::Retag(args) => {
            let src = ImageRef {
                repo: args.repo.clone(),
                id: ImageId::Tag(args.src_tag.clone()),
            };
            copy_manifest(&client, &src, &args.repo, &args.dst_tag).await?;
            output::ack(
                globals,
                Output::one(serde_json::json!({
                    "repository": args.repo,
                    "source_tag": args.src_tag,
                    "destination_tag": args.dst_tag,
                }))?,
            )
        }
        EcrSubcommand::Cp(args) => {
            let src = parse_image_ref(&args.src)?;
            let dst = parse_image_ref(&args.dst)?;
            let ImageId::Tag(dst_tag) = &dst.id else {
                return Err(AwlError::Usage {
                    message: "ecr cp destination must be repo:tag".to_owned(),
                });
            };
            copy_manifest(&client, &src, &dst.repo, dst_tag).await?;
            output::ack(
                globals,
                Output::one(serde_json::json!({
                    "source": args.src,
                    "destination": args.dst,
                }))?,
            )
        }
        EcrSubcommand::Scan(args) => {
            let image = parse_image_ref(&args.image)?;
            let response = client
                .describe_image_scan_findings()
                .repository_name(&image.repo)
                .image_id(image.id.identifier())
                .send()
                .await
                .map_err(AwlError::sdk)?;
            let finding_severity_counts = response.image_scan_findings().and_then(|findings| {
                findings.finding_severity_counts().map(|counts| {
                    counts
                        .iter()
                        .map(|(severity, count)| (severity.as_str().to_owned(), *count))
                        .collect::<BTreeMap<_, _>>()
                })
            });
            output::emit(
                globals,
                Output::one(serde_json::json!({
                    "image": args.image,
                    "status": response.image_scan_status().and_then(|s| s.status()).map(|s| s.as_str()),
                    "finding_severity_counts": finding_severity_counts,
                }))?,
            )
        }
        EcrSubcommand::Digest(args) => {
            let image = parse_image_ref(&args.image)?;
            let response = client
                .describe_images()
                .repository_name(&image.repo)
                .image_ids(image.id.identifier())
                .send()
                .await
                .map_err(AwlError::sdk)?;
            let digest = response
                .image_details()
                .first()
                .and_then(|image| image.image_digest());
            output::emit(
                globals,
                Output::one(serde_json::json!({
                    "image": args.image,
                    "digest": digest,
                }))?,
            )
        }
    }
}

async fn login(
    globals: &Globals,
    client: &aws_sdk_ecr::Client,
    print_password: bool,
) -> Result<()> {
    let response = client
        .get_authorization_token()
        .send()
        .await
        .map_err(AwlError::sdk)?;
    let Some(token) = response.authorization_data().first() else {
        return Err(AwlError::Authentication {
            message: "ECR returned no authorization data".to_owned(),
        });
    };
    let decoded = token
        .authorization_token()
        .map(|token| base64::engine::general_purpose::STANDARD.decode(token))
        .transpose()
        .map_err(|error| AwlError::Authentication {
            message: format!("invalid ECR authorization token: {error}"),
        })?
        .ok_or_else(|| AwlError::Authentication {
            message: "ECR returned no authorization token".to_owned(),
        })?;
    let decoded = String::from_utf8_lossy(&decoded);
    let (_, password) = decoded.split_once(':').unwrap_or(("AWS", decoded.as_ref()));
    let endpoint = token.proxy_endpoint().unwrap_or_default();

    if print_password {
        return output::emit(globals, Output::Plain(password.to_owned()));
    }

    // docker prints "Login Succeeded" on stdout; keep stdout for awl's own output.
    let mut child = Command::new("docker")
        .args(["login", "--username", "AWS", "--password-stdin", endpoint])
        .stdin(Stdio::piped())
        .stdout(std::io::stderr())
        .spawn()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => AwlError::MissingDependency {
                message:
                    "`awl ecr login` needs docker on PATH; use --print-password for other tools"
                        .to_owned(),
            },
            _ => AwlError::Io(error),
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(password.as_bytes())?;
    }
    let status = child.wait()?;
    if !status.success() {
        return Err(AwlError::Aws {
            message: format!("docker login failed with status {status}"),
        });
    }
    output::ack(
        globals,
        Output::one(serde_json::json!({ "registry": endpoint, "logged_in": true }))?,
    )
}

#[derive(Debug)]
enum ImageId {
    Tag(String),
    Digest(String),
}

impl std::fmt::Display for ImageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageId::Tag(tag) => write!(f, ":{tag}"),
            ImageId::Digest(digest) => write!(f, "@{digest}"),
        }
    }
}

impl ImageId {
    fn identifier(&self) -> ImageIdentifier {
        match self {
            ImageId::Tag(tag) => ImageIdentifier::builder().image_tag(tag).build(),
            ImageId::Digest(digest) => ImageIdentifier::builder().image_digest(digest).build(),
        }
    }
}

#[derive(Debug)]
struct ImageRef {
    repo: String,
    id: ImageId,
}

async fn copy_manifest(
    client: &aws_sdk_ecr::Client,
    src: &ImageRef,
    dst_repo: &str,
    dst_tag: &str,
) -> Result<()> {
    let source = client
        .batch_get_image()
        .repository_name(&src.repo)
        .image_ids(src.id.identifier())
        .send()
        .await
        .map_err(AwlError::sdk)?;
    let Some(image) = source.images().first() else {
        return Err(AwlError::NotFound {
            message: format!("image {}{} not found", src.repo, src.id),
        });
    };
    let Some(manifest) = image.image_manifest() else {
        return Err(AwlError::NotFound {
            message: format!("image {}{} has no manifest", src.repo, src.id),
        });
    };
    let response = client
        .put_image()
        .repository_name(dst_repo)
        .image_tag(dst_tag)
        .image_manifest(manifest)
        .set_image_manifest_media_type(image.image_manifest_media_type().map(str::to_owned))
        .send()
        .await;
    match response {
        Ok(_) => Ok(()),
        Err(error) => match AwlError::sdk(error) {
            AwlError::NotFound { message } | AwlError::Aws { message }
                if message.starts_with("LayersNotFoundException")
                    || message.starts_with("ReferencedImagesNotFoundException") =>
            {
                Err(AwlError::NotFound {
                    message: format!(
                        "{message}; ECR only accepts a manifest whose layers already exist in {dst_repo}, so copy across repositories with a registry tool such as crane or skopeo"
                    ),
                })
            }
            other => Err(other),
        },
    }
}

/// Parse `repo:tag` or `repo@sha256:digest`.
fn parse_image_ref(value: &str) -> Result<ImageRef> {
    if let Some((repo, digest)) = value.split_once('@') {
        return Ok(ImageRef {
            repo: repo.to_owned(),
            id: ImageId::Digest(digest.to_owned()),
        });
    }
    match value.rsplit_once(':') {
        Some((repo, tag)) if !repo.is_empty() && !tag.is_empty() => Ok(ImageRef {
            repo: repo.to_owned(),
            id: ImageId::Tag(tag.to_owned()),
        }),
        _ => Err(AwlError::Usage {
            message: format!("expected repo:tag or repo@sha256:digest, got {value:?}"),
        }),
    }
}
