use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::{ByteStream, Length};
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart, Delete, ObjectIdentifier};
use futures::StreamExt;
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use walkdir::WalkDir;

use crate::cli::{
    Globals, PresignMethod, S3Command, S3CpArgs, S3GetArgs, S3LsArgs, S3PresignArgs, S3PutArgs,
    S3RmArgs, S3Subcommand, S3SyncArgs, S3UriArgs,
};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};

const MIN_PART_SIZE: u64 = 5 * 1024 * 1024;
const MAX_PARTS: u64 = 10_000;
/// Largest object CopyObject accepts; bigger objects need a multipart copy.
const MAX_COPY_OBJECT_SIZE: u64 = 5 * 1024 * 1024 * 1024;
const DELETE_BATCH: usize = 1000;

#[derive(Debug, Clone, Eq, PartialEq)]
struct S3Uri {
    bucket: String,
    key: String,
}

impl S3Uri {
    fn child(&self, key: String) -> Self {
        Self {
            bucket: self.bucket.clone(),
            key,
        }
    }

    /// The key as a directory prefix: `logs` -> `logs/`, so prefix operations
    /// never touch siblings such as `logs-archive/`.
    fn dir_prefix(&self) -> String {
        if self.key.is_empty() || self.key.ends_with('/') {
            self.key.clone()
        } else {
            format!("{}/", self.key)
        }
    }

    /// CopySource must be URL-encoded.
    fn copy_source(&self) -> String {
        format!("{}/{}", self.bucket, urlencoding::encode(&self.key))
    }
}

impl std::fmt::Display for S3Uri {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "s3://{}/{}", self.bucket, self.key)
    }
}

#[derive(Debug, Clone)]
enum Location {
    S3(S3Uri),
    Local(PathBuf),
}

#[derive(Debug, Serialize)]
struct BucketRow {
    bucket: String,
}

#[derive(Debug, Serialize)]
struct ObjectRow {
    bucket: String,
    key: String,
    size: i64,
    last_modified: Option<String>,
    etag: Option<String>,
}

#[derive(Debug, Serialize)]
struct PrefixRow {
    bucket: String,
    prefix: String,
}

#[derive(Debug, Serialize)]
struct ActionRow {
    action: &'static str,
    source: Option<String>,
    destination: Option<String>,
    bytes: Option<u64>,
    dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub async fn run(globals: &Globals, command: &S3Command) -> Result<()> {
    match &command.command {
        S3Subcommand::Ls(args) => ls(globals, args).await,
        S3Subcommand::Get(args) => get(globals, args).await,
        S3Subcommand::Put(args) => put(globals, args).await,
        S3Subcommand::Sync(args) => sync(globals, args).await,
        S3Subcommand::Rm(args) => rm(globals, args).await,
        S3Subcommand::Cp(args) => cp(globals, args, "cp").await,
        S3Subcommand::Mv(args) => cp(globals, args, "mv").await,
        S3Subcommand::Presign(args) => presign(globals, args).await,
        S3Subcommand::Du(args) => du(globals, args).await,
        S3Subcommand::Head(args) => head(globals, args).await,
    }
}

async fn client(globals: &Globals, path_style: bool) -> Result<aws_sdk_s3::Client> {
    Ok(AwsContext::new(globals).await?.s3(path_style))
}

async fn ls(globals: &Globals, args: &S3LsArgs) -> Result<()> {
    let client = client(globals, args.common.path_style).await?;
    let Some(uri) = &args.uri else {
        let mut rows = Vec::new();
        let mut pages = client.list_buckets().into_paginator().send();
        while let Some(page) = pages.next().await {
            let page = page.map_err(AwlError::sdk)?;
            rows.extend(page.buckets().iter().filter_map(|bucket| {
                bucket.name().map(|name| BucketRow {
                    bucket: name.to_owned(),
                })
            }));
        }
        return output::emit(globals, Output::many(rows)?);
    };

    let uri = parse_s3_uri(uri)?;
    let mut request = client
        .list_objects_v2()
        .bucket(&uri.bucket)
        .prefix(&uri.key);
    if !args.recursive {
        request = request.delimiter("/");
    }
    let mut rows = Vec::new();
    let mut pages = request.into_paginator().send();
    while let Some(page) = pages.next().await {
        let page = page.map_err(AwlError::sdk)?;
        for prefix in page.common_prefixes() {
            if let Some(prefix) = prefix.prefix() {
                rows.push(serde_json::to_value(PrefixRow {
                    bucket: uri.bucket.clone(),
                    prefix: prefix.to_owned(),
                })?);
            }
        }
        for object in page.contents() {
            rows.push(serde_json::to_value(ObjectRow {
                bucket: uri.bucket.clone(),
                key: object.key().unwrap_or_default().to_owned(),
                size: object.size().unwrap_or_default(),
                last_modified: object.last_modified().map(ToString::to_string),
                etag: object.e_tag().map(str::to_owned),
            })?);
        }
    }
    output::emit(globals, Output::Many(rows))
}

async fn get(globals: &Globals, args: &S3GetArgs) -> Result<()> {
    let client = client(globals, args.common.path_style).await?;
    let uri = parse_s3_uri(&args.uri)?;

    if args
        .dest
        .as_deref()
        .is_some_and(|dest| dest.as_os_str() == "-")
    {
        let mut body = client
            .get_object()
            .bucket(&uri.bucket)
            .key(&uri.key)
            .send()
            .await
            .map_err(AwlError::sdk)?
            .body;
        let mut stdout = tokio::io::stdout();
        while let Some(chunk) = body.try_next().await.map_err(AwlError::aws)? {
            stdout.write_all(&chunk).await?;
        }
        stdout.flush().await?;
        return Ok(());
    }

    let dest = match &args.dest {
        Some(dest) => dest.clone(),
        None => PathBuf::from(crate::cli::filename(Path::new(&uri.key)).ok_or_else(|| {
            AwlError::Usage {
                message: "cannot infer destination filename from S3 key".to_owned(),
            }
        })?),
    };
    let bytes = download(&client, &uri, &dest).await?;
    output::ack(
        globals,
        Output::one(ActionRow {
            action: "get",
            source: Some(uri.to_string()),
            destination: Some(dest.display().to_string()),
            bytes: Some(bytes),
            dry_run: false,
            error: None,
        })?,
    )
}

async fn put(globals: &Globals, args: &S3PutArgs) -> Result<()> {
    let client = client(globals, args.common.path_style).await?;
    let uri = parse_s3_uri(&args.uri)?;
    let size = tokio::fs::metadata(&args.src).await?.len();
    let options = PutOptions {
        content_type: args.content_type.clone(),
        metadata: args.metadata.iter().cloned().collect(),
        acl: args.acl.clone(),
        storage_class: args.storage_class.clone(),
    };
    let part_size = checked_part_size(args.transfer.part_size)?;
    upload(&client, &args.src, &uri, size, part_size, &options).await?;
    output::ack(
        globals,
        Output::one(ActionRow {
            action: "put",
            source: Some(args.src.display().to_string()),
            destination: Some(uri.to_string()),
            bytes: Some(size),
            dry_run: false,
            error: None,
        })?,
    )
}

async fn rm(globals: &Globals, args: &S3RmArgs) -> Result<()> {
    let client = client(globals, args.common.path_style).await?;
    let uri = parse_s3_uri(&args.uri)?;
    let keys = if args.recursive {
        list_objects(&client, &uri.bucket, &uri.dir_prefix())
            .await?
            .into_keys()
            .collect::<Vec<_>>()
    } else if uri.key.is_empty() {
        return Err(AwlError::Usage {
            message: "s3 rm needs an object key, or -r to remove a prefix".to_owned(),
        });
    } else {
        vec![uri.key.clone()]
    };

    if !args.dry_run.dry_run {
        delete_keys(&client, &uri.bucket, &keys).await?;
    }
    let rows = keys
        .into_iter()
        .map(|key| ActionRow {
            action: "rm",
            source: Some(uri.child(key).to_string()),
            destination: None,
            bytes: None,
            dry_run: args.dry_run.dry_run,
            error: None,
        })
        .collect::<Vec<_>>();
    output::ack(globals, Output::many(rows)?)
}

async fn cp(globals: &Globals, args: &S3CpArgs, action: &'static str) -> Result<()> {
    let client = client(globals, args.common.path_style).await?;
    let src = parse_s3_uri(&args.src)?;
    let dst = parse_s3_uri(&args.dst)?;
    let head = client
        .head_object()
        .bucket(&src.bucket)
        .key(&src.key)
        .send()
        .await
        .map_err(AwlError::sdk)?;
    let size = head.content_length().unwrap_or_default().max(0) as u64;
    let part_size = checked_part_size(args.transfer.part_size)?;
    copy(&client, &src, &dst, size, part_size, Some(&head)).await?;
    if action == "mv" {
        delete_keys(&client, &src.bucket, std::slice::from_ref(&src.key)).await?;
    }
    output::ack(
        globals,
        Output::one(ActionRow {
            action,
            source: Some(src.to_string()),
            destination: Some(dst.to_string()),
            bytes: Some(size),
            dry_run: false,
            error: None,
        })?,
    )
}

async fn presign(globals: &Globals, args: &S3PresignArgs) -> Result<()> {
    let client = client(globals, args.common.path_style).await?;
    let uri = parse_s3_uri(&args.uri)?;
    let config = PresigningConfig::expires_in(args.expires).map_err(|error| AwlError::Usage {
        message: error.to_string(),
    })?;
    let presigned = match args.method {
        PresignMethod::Get => client
            .get_object()
            .bucket(&uri.bucket)
            .key(&uri.key)
            .presigned(config)
            .await
            .map_err(AwlError::sdk)?,
        PresignMethod::Put => client
            .put_object()
            .bucket(&uri.bucket)
            .key(&uri.key)
            .presigned(config)
            .await
            .map_err(AwlError::sdk)?,
    };
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "url": presigned.uri().to_string(),
            "method": args.method.as_str(),
        }))?,
    )
}

async fn du(globals: &Globals, args: &S3UriArgs) -> Result<()> {
    let client = client(globals, args.common.path_style).await?;
    let uri = parse_s3_uri(&args.uri)?;
    let prefix = uri.dir_prefix();
    let objects = list_objects(&client, &uri.bucket, &prefix).await?;
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "bucket": uri.bucket,
            "prefix": prefix,
            "objects": objects.len(),
            "bytes": objects.values().map(|object| object.size).sum::<u64>(),
        }))?,
    )
}

async fn head(globals: &Globals, args: &S3UriArgs) -> Result<()> {
    let client = client(globals, args.common.path_style).await?;
    let uri = parse_s3_uri(&args.uri)?;
    let response = client
        .head_object()
        .bucket(&uri.bucket)
        .key(&uri.key)
        .send()
        .await
        .map_err(AwlError::sdk)?;
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "bucket": uri.bucket,
            "key": uri.key,
            "content_length": response.content_length(),
            "content_type": response.content_type(),
            "last_modified": response.last_modified().map(ToString::to_string),
            "etag": response.e_tag(),
            "metadata": response.metadata(),
        }))?,
    )
}

// sync

/// Size and modification time (whole seconds since the epoch) of one entry,
/// keyed by its `/`-separated path relative to the sync root.
#[derive(Debug, Clone, Copy)]
struct Entry {
    size: u64,
    modified: i64,
}

#[derive(Debug)]
enum Transfer {
    Upload { path: PathBuf, dst: S3Uri },
    Download { src: S3Uri, path: PathBuf },
    Copy { src: S3Uri, dst: S3Uri },
}

async fn sync(globals: &Globals, args: &S3SyncArgs) -> Result<()> {
    let src = parse_location(&args.src)?;
    let dst = parse_location(&args.dst)?;
    if let (Location::Local(_), Location::Local(_)) = (&src, &dst) {
        return Err(AwlError::Usage {
            message: "s3 sync requires at least one s3:// location".to_owned(),
        });
    }
    let client = client(globals, args.common.path_style).await?;
    let filter = PathFilter::new(&args.include, &args.exclude)?;
    let part_size = checked_part_size(args.transfer.part_size)?;
    let dry_run = args.dry_run.dry_run;

    // A missing source would look empty and turn --delete into a wipe.
    if let Location::Local(root) = &src
        && !root.is_dir()
    {
        return Err(AwlError::Usage {
            message: format!("sync source {} is not a directory", root.display()),
        });
    }
    let source = list_location(&client, &src).await?;
    let destination = list_location(&client, &dst).await?;

    let mut transfers = Vec::new();
    for (relative, entry) in &source {
        if !filter.matches(relative) {
            continue;
        }
        let unchanged = destination.get(relative).is_some_and(|existing| {
            existing.size == entry.size && existing.modified >= entry.modified
        });
        if !unchanged {
            transfers.push((transfer_for(&src, &dst, relative)?, entry.size));
        }
    }
    let deletes = if args.delete {
        destination
            .keys()
            .filter(|relative| !source.contains_key(*relative) && filter.matches(relative))
            .cloned()
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let mut rows = Vec::new();
    let mut failed = 0usize;
    if dry_run {
        rows.extend(
            transfers
                .iter()
                .map(|(transfer, size)| transfer_row(transfer, *size, true, None)),
        );
    } else {
        let mut results = futures::stream::iter(transfers.into_iter().map(|(transfer, size)| {
            let client = &client;
            async move {
                let error = run_transfer(client, &transfer, size, part_size)
                    .await
                    .err()
                    .map(|error| error.to_string());
                transfer_row(&transfer, size, false, error)
            }
        }))
        .buffer_unordered(usize::from(args.concurrency));
        while let Some(row) = results.next().await {
            if let Some(error) = &row.error {
                failed += 1;
                eprintln!(
                    "{} {} failed: {error}",
                    row.action,
                    row.source.as_deref().unwrap_or_default()
                );
            }
            rows.push(row);
            if failed > 0 && args.fail_fast {
                break;
            }
        }
    }

    // A failed or interrupted transfer pass means the source view is not fully
    // mirrored, so deleting would remove data that was never replaced.
    if failed == 0 {
        rows.extend(delete_relative(&client, &dst, &deletes, dry_run).await?);
    }

    output::ack(globals, Output::many(rows)?)?;
    if failed > 0 {
        return Err(AwlError::Aws {
            message: format!(
                "{failed} transfer(s) failed{}",
                if args.delete { "; skipped deletes" } else { "" }
            ),
        });
    }
    Ok(())
}

fn transfer_for(src: &Location, dst: &Location, relative: &str) -> Result<Transfer> {
    Ok(match (src, dst) {
        (Location::Local(root), Location::S3(dst)) => Transfer::Upload {
            path: local_path(root, relative)?,
            dst: dst.child(format!("{}{relative}", dst.dir_prefix())),
        },
        (Location::S3(src), Location::Local(root)) => Transfer::Download {
            src: src.child(format!("{}{relative}", src.dir_prefix())),
            path: local_path(root, relative)?,
        },
        (Location::S3(src), Location::S3(dst)) => Transfer::Copy {
            src: src.child(format!("{}{relative}", src.dir_prefix())),
            dst: dst.child(format!("{}{relative}", dst.dir_prefix())),
        },
        (Location::Local(_), Location::Local(_)) => {
            return Err(AwlError::Usage {
                message: "s3 sync requires at least one s3:// location".to_owned(),
            });
        }
    })
}

async fn run_transfer(
    client: &aws_sdk_s3::Client,
    transfer: &Transfer,
    size: u64,
    part_size: u64,
) -> Result<()> {
    match transfer {
        Transfer::Upload { path, dst } => {
            upload(client, path, dst, size, part_size, &PutOptions::default()).await
        }
        Transfer::Download { src, path } => download(client, src, path).await.map(|_| ()),
        Transfer::Copy { src, dst } => copy(client, src, dst, size, part_size, None).await,
    }
}

fn transfer_row(transfer: &Transfer, size: u64, dry_run: bool, error: Option<String>) -> ActionRow {
    let (action, source, destination) = match transfer {
        Transfer::Upload { path, dst } => ("upload", path.display().to_string(), dst.to_string()),
        Transfer::Download { src, path } => {
            ("download", src.to_string(), path.display().to_string())
        }
        Transfer::Copy { src, dst } => ("copy", src.to_string(), dst.to_string()),
    };
    ActionRow {
        action,
        source: Some(source),
        destination: Some(destination),
        bytes: Some(size),
        dry_run,
        error,
    }
}

async fn delete_relative(
    client: &aws_sdk_s3::Client,
    location: &Location,
    relatives: &[String],
    dry_run: bool,
) -> Result<Vec<ActionRow>> {
    let targets = match location {
        Location::S3(uri) => relatives
            .iter()
            .map(|relative| format!("{}{relative}", uri.dir_prefix()))
            .collect::<Vec<_>>(),
        Location::Local(root) => relatives
            .iter()
            .map(|relative| Ok(local_path(root, relative)?.display().to_string()))
            .collect::<Result<Vec<_>>>()?,
    };
    if !dry_run {
        match location {
            Location::S3(uri) => delete_keys(client, &uri.bucket, &targets).await?,
            Location::Local(_) => {
                for path in &targets {
                    tokio::fs::remove_file(path).await?;
                }
            }
        }
    }
    Ok(targets
        .into_iter()
        .map(|target| ActionRow {
            action: "delete",
            source: None,
            destination: Some(match location {
                Location::S3(uri) => uri.child(target).to_string(),
                Location::Local(_) => target,
            }),
            bytes: None,
            dry_run,
            error: None,
        })
        .collect())
}

async fn list_location(
    client: &aws_sdk_s3::Client,
    location: &Location,
) -> Result<BTreeMap<String, Entry>> {
    match location {
        Location::S3(uri) => {
            let prefix = uri.dir_prefix();
            Ok(list_objects(client, &uri.bucket, &prefix)
                .await?
                .into_iter()
                .filter_map(|(key, entry)| {
                    // Zero-byte `dir/` markers are not files on either side.
                    key.strip_prefix(&prefix)
                        .filter(|relative| !relative.is_empty() && !relative.ends_with('/'))
                        .map(|relative| (relative.to_owned(), entry))
                })
                .collect())
        }
        Location::Local(root) => list_local(root),
    }
}

fn list_local(root: &Path) -> Result<BTreeMap<String, Entry>> {
    let mut out = BTreeMap::new();
    if !root.exists() {
        return Ok(out);
    }
    for entry in WalkDir::new(root) {
        let entry = entry.map_err(|error| AwlError::Io(std::io::Error::other(error)))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|error| AwlError::Usage {
                message: error.to_string(),
            })?;
        let metadata = entry
            .metadata()
            .map_err(|error| AwlError::Io(std::io::Error::other(error)))?;
        let modified = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or_default();
        out.insert(
            path_to_key(relative)?,
            Entry {
                size: metadata.len(),
                modified,
            },
        );
    }
    Ok(out)
}

/// Every object under `prefix`, keyed by full key.
async fn list_objects(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    prefix: &str,
) -> Result<BTreeMap<String, Entry>> {
    let mut out = BTreeMap::new();
    let mut pages = client
        .list_objects_v2()
        .bucket(bucket)
        .prefix(prefix)
        .into_paginator()
        .send();
    while let Some(page) = pages.next().await {
        let page = page.map_err(AwlError::sdk)?;
        for object in page.contents() {
            if let Some(key) = object.key() {
                out.insert(
                    key.to_owned(),
                    Entry {
                        size: object.size().unwrap_or_default().max(0) as u64,
                        modified: object
                            .last_modified()
                            .map(|time| time.secs())
                            .unwrap_or_default(),
                    },
                );
            }
        }
    }
    Ok(out)
}

async fn delete_keys(client: &aws_sdk_s3::Client, bucket: &str, keys: &[String]) -> Result<()> {
    for batch in keys.chunks(DELETE_BATCH) {
        let objects = batch
            .iter()
            .map(|key| {
                ObjectIdentifier::builder()
                    .key(key)
                    .build()
                    .map_err(AwlError::aws)
            })
            .collect::<Result<Vec<_>>>()?;
        let delete = Delete::builder()
            .set_objects(Some(objects))
            .quiet(true)
            .build()
            .map_err(AwlError::aws)?;
        let response = client
            .delete_objects()
            .bucket(bucket)
            .delete(delete)
            .send()
            .await
            .map_err(AwlError::sdk)?;
        if let Some(error) = response.errors().first() {
            return Err(AwlError::Aws {
                message: format!(
                    "delete s3://{bucket}/{} failed: {}: {} ({} of {} failed)",
                    error.key().unwrap_or_default(),
                    error.code().unwrap_or_default(),
                    error.message().unwrap_or_default(),
                    response.errors().len(),
                    batch.len()
                ),
            });
        }
    }
    Ok(())
}

// transfers

#[derive(Debug, Default)]
struct PutOptions {
    content_type: Option<String>,
    metadata: HashMap<String, String>,
    acl: Option<String>,
    storage_class: Option<String>,
}

fn checked_part_size(part_size: u64) -> Result<u64> {
    if part_size < MIN_PART_SIZE {
        return Err(AwlError::Usage {
            message: format!("--part-size must be at least {MIN_PART_SIZE} bytes (5 MiB)"),
        });
    }
    Ok(part_size)
}

/// Grow the part size when needed to stay under S3's 10,000 part limit.
fn effective_part_size(size: u64, part_size: u64) -> u64 {
    part_size.max(size.div_ceil(MAX_PARTS))
}

async fn upload(
    client: &aws_sdk_s3::Client,
    path: &Path,
    dst: &S3Uri,
    size: u64,
    part_size: u64,
    options: &PutOptions,
) -> Result<()> {
    if size <= part_size {
        let body = ByteStream::from_path(path).await.map_err(AwlError::aws)?;
        client
            .put_object()
            .bucket(&dst.bucket)
            .key(&dst.key)
            .body(body)
            .set_content_type(options.content_type.clone())
            .set_metadata((!options.metadata.is_empty()).then(|| options.metadata.clone()))
            .set_acl(options.acl.as_deref().map(Into::into))
            .set_storage_class(options.storage_class.as_deref().map(Into::into))
            .send()
            .await
            .map_err(AwlError::sdk)?;
        return Ok(());
    }

    let created = client
        .create_multipart_upload()
        .bucket(&dst.bucket)
        .key(&dst.key)
        .set_content_type(options.content_type.clone())
        .set_metadata((!options.metadata.is_empty()).then(|| options.metadata.clone()))
        .set_acl(options.acl.as_deref().map(Into::into))
        .set_storage_class(options.storage_class.as_deref().map(Into::into))
        .send()
        .await
        .map_err(AwlError::sdk)?;
    let upload_id = created.upload_id().unwrap_or_default().to_owned();
    let part_size = effective_part_size(size, part_size);
    let result = async {
        let mut parts = Vec::new();
        let mut offset = 0;
        let mut number = 1;
        while offset < size {
            let length = part_size.min(size - offset);
            let body = ByteStream::read_from()
                .path(path)
                .offset(offset)
                .length(Length::Exact(length))
                .build()
                .await
                .map_err(AwlError::aws)?;
            let part = client
                .upload_part()
                .bucket(&dst.bucket)
                .key(&dst.key)
                .upload_id(&upload_id)
                .part_number(number)
                .body(body)
                .send()
                .await
                .map_err(AwlError::sdk)?;
            parts.push(
                CompletedPart::builder()
                    .part_number(number)
                    .set_e_tag(part.e_tag().map(str::to_owned))
                    .build(),
            );
            offset += length;
            number += 1;
        }
        complete_multipart(client, dst, &upload_id, parts).await
    }
    .await;
    abort_on_error(client, dst, &upload_id, result).await
}

async fn copy(
    client: &aws_sdk_s3::Client,
    src: &S3Uri,
    dst: &S3Uri,
    size: u64,
    part_size: u64,
    head: Option<&aws_sdk_s3::operation::head_object::HeadObjectOutput>,
) -> Result<()> {
    if size <= MAX_COPY_OBJECT_SIZE {
        client
            .copy_object()
            .bucket(&dst.bucket)
            .key(&dst.key)
            .copy_source(src.copy_source())
            .send()
            .await
            .map_err(AwlError::sdk)?;
        return Ok(());
    }

    // Multipart copies do not carry metadata over on their own.
    let head = match head {
        Some(head) => head.clone(),
        None => client
            .head_object()
            .bucket(&src.bucket)
            .key(&src.key)
            .send()
            .await
            .map_err(AwlError::sdk)?,
    };
    let created = client
        .create_multipart_upload()
        .bucket(&dst.bucket)
        .key(&dst.key)
        .set_content_type(head.content_type().map(str::to_owned))
        .set_metadata(head.metadata().cloned())
        .send()
        .await
        .map_err(AwlError::sdk)?;
    let upload_id = created.upload_id().unwrap_or_default().to_owned();
    let part_size = effective_part_size(size, part_size);
    let result = async {
        let mut parts = Vec::new();
        let mut offset = 0;
        let mut number = 1;
        while offset < size {
            let end = (offset + part_size).min(size) - 1;
            let part = client
                .upload_part_copy()
                .bucket(&dst.bucket)
                .key(&dst.key)
                .upload_id(&upload_id)
                .part_number(number)
                .copy_source(src.copy_source())
                .copy_source_range(format!("bytes={offset}-{end}"))
                .send()
                .await
                .map_err(AwlError::sdk)?;
            parts.push(
                CompletedPart::builder()
                    .part_number(number)
                    .set_e_tag(
                        part.copy_part_result()
                            .and_then(|result| result.e_tag())
                            .map(str::to_owned),
                    )
                    .build(),
            );
            offset = end + 1;
            number += 1;
        }
        complete_multipart(client, dst, &upload_id, parts).await
    }
    .await;
    abort_on_error(client, dst, &upload_id, result).await
}

async fn complete_multipart(
    client: &aws_sdk_s3::Client,
    dst: &S3Uri,
    upload_id: &str,
    parts: Vec<CompletedPart>,
) -> Result<()> {
    client
        .complete_multipart_upload()
        .bucket(&dst.bucket)
        .key(&dst.key)
        .upload_id(upload_id)
        .multipart_upload(
            CompletedMultipartUpload::builder()
                .set_parts(Some(parts))
                .build(),
        )
        .send()
        .await
        .map_err(AwlError::sdk)?;
    Ok(())
}

async fn abort_on_error(
    client: &aws_sdk_s3::Client,
    dst: &S3Uri,
    upload_id: &str,
    result: Result<()>,
) -> Result<()> {
    if result.is_err() {
        // Best effort: an abandoned upload only costs storage until a lifecycle rule clears it.
        let _ = client
            .abort_multipart_upload()
            .bucket(&dst.bucket)
            .key(&dst.key)
            .upload_id(upload_id)
            .send()
            .await;
    }
    result
}

/// Stream an object to `path` through a temporary sibling file, so a failed
/// download never leaves a truncated file in place.
async fn download(client: &aws_sdk_s3::Client, src: &S3Uri, path: &Path) -> Result<u64> {
    let response = client
        .get_object()
        .bucket(&src.bucket)
        .key(&src.key)
        .send()
        .await
        .map_err(AwlError::sdk)?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await?;
    }
    let file_name = crate::cli::filename(path).ok_or_else(|| AwlError::Usage {
        message: format!("invalid destination path {}", path.display()),
    })?;
    let temp = path.with_file_name(format!(".{}.awl-partial", file_name.to_string_lossy()));
    let result = async {
        let mut file = tokio::fs::File::create(&temp).await?;
        let mut body = response.body;
        let mut bytes = 0u64;
        while let Some(chunk) = body.try_next().await.map_err(AwlError::aws)? {
            file.write_all(&chunk).await?;
            bytes += chunk.len() as u64;
        }
        file.flush().await?;
        tokio::fs::rename(&temp, path).await?;
        Ok(bytes)
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result
}

// paths

fn parse_s3_uri(uri: &str) -> Result<S3Uri> {
    let Some(rest) = uri.strip_prefix("s3://") else {
        return Err(AwlError::Usage {
            message: format!("expected s3:// URI, got {uri:?}"),
        });
    };
    let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
    if bucket.is_empty() {
        return Err(AwlError::Usage {
            message: "S3 URI is missing bucket".to_owned(),
        });
    }
    Ok(S3Uri {
        bucket: bucket.to_owned(),
        key: key.to_owned(),
    })
}

fn parse_location(value: &str) -> Result<Location> {
    if value.starts_with("s3://") {
        parse_s3_uri(value).map(Location::S3)
    } else {
        Ok(Location::Local(PathBuf::from(value)))
    }
}

/// Map a relative S3 key under `root`, refusing anything that could escape it
/// (`..`, absolute paths, empty segments, drive prefixes).
fn local_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    for segment in relative.split('/') {
        let mut components = Path::new(segment).components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(part)), None) if part == segment => path.push(part),
            _ => {
                return Err(AwlError::Usage {
                    message: format!("refusing unsafe local path for S3 key {relative:?}"),
                });
            }
        }
    }
    Ok(path)
}

fn path_to_key(path: &Path) -> Result<String> {
    path.components()
        .map(|component| {
            component
                .as_os_str()
                .to_str()
                .ok_or_else(|| AwlError::Usage {
                    message: format!("path is not valid UTF-8: {}", path.display()),
                })
        })
        .collect::<Result<Vec<_>>>()
        .map(|parts| parts.join("/"))
}

struct PathFilter {
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
}

impl PathFilter {
    fn new(include: &[String], exclude: &[String]) -> Result<Self> {
        Ok(Self {
            include: build_glob_set(include)?,
            exclude: build_glob_set(exclude)?,
        })
    }

    fn matches(&self, path: &str) -> bool {
        let excluded = self.exclude.as_ref().is_some_and(|set| set.is_match(path));
        let included = self.include.as_ref().is_none_or(|set| set.is_match(path));
        included && !excluded
    }
}

fn build_glob_set(patterns: &[String]) -> Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).map_err(|error| AwlError::Usage {
            message: format!("invalid glob {pattern:?}: {error}"),
        })?);
    }
    Ok(Some(builder.build().map_err(|error| AwlError::Usage {
        message: error.to_string(),
    })?))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn dir_prefix_never_matches_siblings() {
        let uri = crate::services::s3::parse_s3_uri("s3://bucket/logs").expect("valid uri");
        assert_eq!(uri.dir_prefix(), "logs/");
        let root = crate::services::s3::parse_s3_uri("s3://bucket").expect("valid uri");
        assert_eq!(root.dir_prefix(), "");
    }

    #[test]
    fn local_path_rejects_escaping_keys() {
        let root = Path::new("out");
        assert_eq!(
            crate::services::s3::local_path(root, "a/b.txt").expect("safe key"),
            Path::new("out/a/b.txt")
        );
        for key in ["/etc/passwd", "../x", "a/../../x", "a//b", "./a"] {
            assert!(
                crate::services::s3::local_path(root, key).is_err(),
                "{key} should be rejected"
            );
        }
    }
}
