use std::collections::BTreeMap;

use base64::Engine;

use crate::cli::{EcrCommand, EcrCommandSubcommand, Globals, StubArgs};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};
use crate::services::common;

pub async fn run(globals: &Globals, command: &EcrCommand) -> Result<()> {
    let context = AwsContext::new(globals).await?;
    let client = context.ecr();
    match &command.command {
        EcrCommandSubcommand::Ls(_) => {
            let response = client
                .describe_repositories()
                .send()
                .await
                .map_err(AwlError::aws)?;
            let rows = response
                .repositories()
                .iter()
                .map(|repo| {
                    serde_json::json!({
                        "repository_name": repo.repository_name(),
                        "repository_uri": repo.repository_uri(),
                        "registry_id": repo.registry_id(),
                    })
                })
                .collect::<Vec<_>>();
            output::emit(globals, Output::Many(rows))
        }
        EcrCommandSubcommand::Tags(args) => tags(globals, &client, args).await,
        EcrCommandSubcommand::Login(args) => login(globals, &client, args).await,
        EcrCommandSubcommand::Retag(args) => retag(globals, &client, args).await,
        EcrCommandSubcommand::Cp(args) => cp(globals, &client, args).await,
        EcrCommandSubcommand::Scan(args) => scan(globals, &client, args).await,
        EcrCommandSubcommand::Digest(args) => digest(globals, &client, args).await,
    }
}

async fn tags(globals: &Globals, client: &aws_sdk_ecr::Client, args: &StubArgs) -> Result<()> {
    let repo = common::required(&common::positional(&args.args), 0, "repo")?;
    let response = client
        .describe_images()
        .repository_name(repo)
        .send()
        .await
        .map_err(AwlError::aws)?;
    let rows = response
        .image_details()
        .iter()
        .flat_map(|image| {
            image.image_tags().iter().map(|tag| {
                serde_json::json!({
                    "tag": tag,
                    "digest": image.image_digest(),
                    "pushed_at": image.image_pushed_at().map(ToString::to_string),
                    "size": image.image_size_in_bytes(),
                })
            })
        })
        .collect::<Vec<_>>();
    output::emit(globals, Output::Many(rows))
}

async fn login(globals: &Globals, client: &aws_sdk_ecr::Client, args: &StubArgs) -> Result<()> {
    let response = client
        .get_authorization_token()
        .send()
        .await
        .map_err(AwlError::aws)?;
    let Some(token) = response.authorization_data().first() else {
        return Err(AwlError::Authentication {
            message: "ECR returned no authorization data".to_owned(),
        });
    };
    let Some(auth_token) = token.authorization_token() else {
        return Err(AwlError::Authentication {
            message: "ECR returned no authorization token".to_owned(),
        });
    };
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(auth_token)
        .map_err(|error| AwlError::Authentication {
            message: format!("invalid ECR authorization token: {error}"),
        })?;
    let decoded = String::from_utf8_lossy(&decoded);
    let (_, password) = decoded.split_once(':').unwrap_or(("AWS", decoded.as_ref()));
    let endpoint = token.proxy_endpoint().unwrap_or_default();
    let command = format!("docker login --username AWS --password-stdin {endpoint}");
    if common::has_flag(&args.args, "--exec") {
        let mut child = std::process::Command::new("docker")
            .args(["login", "--username", "AWS", "--password-stdin", endpoint])
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        if let Some(stdin) = child.stdin.as_mut() {
            use std::io::Write;
            stdin.write_all(password.as_bytes())?;
        }
        let status = child.wait()?;
        if !status.success() {
            return Err(AwlError::Aws {
                message: format!("docker login failed with status {status}"),
            });
        }
    }
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "command": command,
            "proxy_endpoint": endpoint,
        }))?,
    )
}

async fn retag(globals: &Globals, client: &aws_sdk_ecr::Client, args: &StubArgs) -> Result<()> {
    let positional = common::positional(&args.args);
    let repo = common::required(&positional, 0, "repo")?;
    let src_tag = common::required(&positional, 1, "src-tag")?;
    let dst_tag = common::required(&positional, 2, "dst-tag")?;
    copy_manifest(client, &repo, &src_tag, &repo, &dst_tag).await?;
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "repository": repo,
            "source_tag": src_tag,
            "destination_tag": dst_tag,
        }))?,
    )
}

async fn cp(globals: &Globals, client: &aws_sdk_ecr::Client, args: &StubArgs) -> Result<()> {
    let positional = common::positional(&args.args);
    let src = common::required(&positional, 0, "src-repo:tag")?;
    let dst = common::required(&positional, 1, "dst-repo:tag")?;
    let (src_repo, src_tag) = split_image_ref(&src)?;
    let (dst_repo, dst_tag) = split_image_ref(&dst)?;
    copy_manifest(client, &src_repo, &src_tag, &dst_repo, &dst_tag).await?;
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "source": src,
            "destination": dst,
        }))?,
    )
}

async fn scan(globals: &Globals, client: &aws_sdk_ecr::Client, args: &StubArgs) -> Result<()> {
    let positional = common::positional(&args.args);
    let image = common::required(&positional, 0, "repo:tag")?;
    let (repo, tag) = split_image_ref(&image)?;
    let image_id = aws_sdk_ecr::types::ImageIdentifier::builder()
        .image_tag(tag)
        .build();
    let response = client
        .describe_image_scan_findings()
        .repository_name(repo)
        .image_id(image_id)
        .send()
        .await
        .map_err(AwlError::aws)?;
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
            "image": image,
            "status": response.image_scan_status().and_then(|s| s.status()).map(|s| s.as_str()),
            "finding_severity_counts": finding_severity_counts,
        }))?,
    )
}

async fn digest(globals: &Globals, client: &aws_sdk_ecr::Client, args: &StubArgs) -> Result<()> {
    let positional = common::positional(&args.args);
    let image = common::required(&positional, 0, "repo:tag")?;
    let (repo, tag) = split_image_ref(&image)?;
    let response = client
        .describe_images()
        .repository_name(repo)
        .image_ids(
            aws_sdk_ecr::types::ImageIdentifier::builder()
                .image_tag(tag)
                .build(),
        )
        .send()
        .await
        .map_err(AwlError::aws)?;
    let digest = response
        .image_details()
        .first()
        .and_then(|image| image.image_digest());
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "image": image,
            "digest": digest,
        }))?,
    )
}

async fn copy_manifest(
    client: &aws_sdk_ecr::Client,
    src_repo: &str,
    src_tag: &str,
    dst_repo: &str,
    dst_tag: &str,
) -> Result<()> {
    let source = client
        .batch_get_image()
        .repository_name(src_repo)
        .image_ids(
            aws_sdk_ecr::types::ImageIdentifier::builder()
                .image_tag(src_tag)
                .build(),
        )
        .send()
        .await
        .map_err(AwlError::aws)?;
    let Some(image) = source.images().first() else {
        return Err(AwlError::NotFound {
            message: format!("image {src_repo}:{src_tag} not found"),
        });
    };
    let Some(manifest) = image.image_manifest() else {
        return Err(AwlError::NotFound {
            message: format!("image {src_repo}:{src_tag} has no manifest"),
        });
    };
    client
        .put_image()
        .repository_name(dst_repo)
        .image_tag(dst_tag)
        .image_manifest(manifest)
        .send()
        .await
        .map_err(AwlError::aws)?;
    Ok(())
}

fn split_image_ref(value: &str) -> Result<(String, String)> {
    let Some((repo, tag)) = value.rsplit_once(':') else {
        return Err(AwlError::Usage {
            message: format!("expected repo:tag, got {value:?}"),
        });
    };
    Ok((repo.to_owned(), tag.to_owned()))
}
