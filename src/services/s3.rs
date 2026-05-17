use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use walkdir::WalkDir;

use crate::cli::{
    Globals, PresignMethod, S3Command, S3DuArgs, S3GetArgs, S3HeadArgs, S3LsArgs, S3PresignArgs,
    S3PutArgs, S3RmArgs, S3Subcommand, S3SyncArgs,
};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct S3Uri {
    bucket: String,
    key: String,
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
    action: String,
    source: Option<String>,
    destination: Option<String>,
    bytes: Option<u64>,
    dry_run: bool,
}

#[derive(Debug, Serialize)]
struct SummaryRow {
    succeeded: usize,
    failed: usize,
}

pub async fn run(globals: &Globals, command: &S3Command) -> Result<()> {
    match &command.command {
        S3Subcommand::Ls(args) => ls(globals, args).await,
        S3Subcommand::Get(args) => get(globals, args).await,
        S3Subcommand::Put(args) => put(globals, args).await,
        S3Subcommand::Sync(args) => sync(globals, args).await,
        S3Subcommand::Rm(args) => rm(globals, args).await,
        S3Subcommand::Cp(args) => cp(globals, &args.src, &args.dst, args.common.path_style).await,
        S3Subcommand::Mv(args) => mv(globals, &args.src, &args.dst, args.common.path_style).await,
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
    if let Some(uri) = &args.uri {
        let uri = parse_s3_uri(uri)?;
        let mut request = client
            .list_objects_v2()
            .bucket(&uri.bucket)
            .prefix(normalized_prefix(&uri.key));
        if !args.recursive {
            request = request.delimiter("/");
        }
        let response = request.send().await.map_err(AwlError::aws)?;
        let mut rows = Vec::new();
        for prefix in response.common_prefixes() {
            if let Some(prefix) = prefix.prefix() {
                rows.push(serde_json::to_value(PrefixRow {
                    bucket: uri.bucket.clone(),
                    prefix: prefix.to_owned(),
                })?);
            }
        }
        for object in response.contents() {
            rows.push(serde_json::to_value(ObjectRow {
                bucket: uri.bucket.clone(),
                key: object.key().unwrap_or_default().to_owned(),
                size: object.size().unwrap_or_default(),
                last_modified: object.last_modified().map(ToString::to_string),
                etag: object.e_tag().map(str::to_owned),
            })?);
        }
        output::emit(globals, Output::Many(rows))
    } else {
        let response = client.list_buckets().send().await.map_err(AwlError::aws)?;
        let rows = response
            .buckets()
            .iter()
            .filter_map(|bucket| bucket.name())
            .map(|bucket| BucketRow {
                bucket: bucket.to_owned(),
            })
            .collect::<Vec<_>>();
        output::emit(globals, Output::many(rows)?)
    }
}

async fn get(globals: &Globals, args: &S3GetArgs) -> Result<()> {
    let client = client(globals, args.common.path_style).await?;
    let uri = parse_s3_uri(&args.uri)?;
    let response = client
        .get_object()
        .bucket(&uri.bucket)
        .key(&uri.key)
        .send()
        .await
        .map_err(AwlError::aws)?;
    let bytes = response
        .body
        .collect()
        .await
        .map_err(AwlError::aws)?
        .into_bytes();

    match &args.dest {
        Some(dest) if dest.as_os_str() == "-" => {
            let mut stdout = tokio::io::stdout();
            stdout.write_all(&bytes).await?;
            stdout.flush().await?;
            Ok(())
        }
        Some(dest) => {
            write_file(dest, &bytes).await?;
            output::emit(
                globals,
                Output::one(ActionRow {
                    action: "get".to_owned(),
                    source: Some(format!("s3://{}/{}", uri.bucket, uri.key)),
                    destination: Some(dest.display().to_string()),
                    bytes: Some(bytes.len() as u64),
                    dry_run: false,
                })?,
            )
        }
        None => {
            let dest =
                PathBuf::from(crate::cli::filename(Path::new(&uri.key)).ok_or_else(|| {
                    AwlError::Usage {
                        message: "cannot infer destination filename from S3 key".to_owned(),
                    }
                })?);
            write_file(&dest, &bytes).await?;
            output::emit(
                globals,
                Output::one(ActionRow {
                    action: "get".to_owned(),
                    source: Some(format!("s3://{}/{}", uri.bucket, uri.key)),
                    destination: Some(dest.display().to_string()),
                    bytes: Some(bytes.len() as u64),
                    dry_run: false,
                })?,
            )
        }
    }
}

async fn put(globals: &Globals, args: &S3PutArgs) -> Result<()> {
    let client = client(globals, args.common.path_style).await?;
    let uri = parse_s3_uri(&args.uri)?;
    let bytes = tokio::fs::read(&args.src).await?;
    let mut request = client
        .put_object()
        .bucket(&uri.bucket)
        .key(&uri.key)
        .body(ByteStream::from(bytes.clone()));
    if let Some(content_type) = &args.content_type {
        request = request.content_type(content_type);
    }
    if !args.metadata.is_empty() {
        request = request.set_metadata(Some(args.metadata.iter().cloned().collect()));
    }
    if let Some(storage_class) = &args.storage_class {
        request = request.storage_class(aws_sdk_s3::types::StorageClass::from(
            storage_class.as_str(),
        ));
    }
    if let Some(acl) = &args.acl {
        request = request.acl(aws_sdk_s3::types::ObjectCannedAcl::from(acl.as_str()));
    }
    let response = request.send().await.map_err(AwlError::aws)?;
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "action": "put",
            "source": args.src.display().to_string(),
            "destination": format!("s3://{}/{}", uri.bucket, uri.key),
            "bytes": bytes.len(),
            "etag": response.e_tag(),
        }))?,
    )
}

async fn rm(globals: &Globals, args: &S3RmArgs) -> Result<()> {
    let client = client(globals, args.common.path_style).await?;
    let uri = parse_s3_uri(&args.uri)?;
    let targets = if args.recursive {
        list_object_keys(&client, &uri.bucket, normalized_prefix(&uri.key)).await?
    } else {
        vec![uri.key.clone()]
    };

    let mut rows = Vec::new();
    for key in targets {
        if !args.dry_run {
            client
                .delete_object()
                .bucket(&uri.bucket)
                .key(&key)
                .send()
                .await
                .map_err(AwlError::aws)?;
        }
        rows.push(ActionRow {
            action: "rm".to_owned(),
            source: Some(format!("s3://{}/{}", uri.bucket, key)),
            destination: None,
            bytes: None,
            dry_run: args.dry_run,
        });
    }

    output::emit(globals, Output::many(rows)?)
}

async fn cp(globals: &Globals, src: &str, dst: &str, path_style: bool) -> Result<()> {
    let client = client(globals, path_style).await?;
    let src = parse_s3_uri(src)?;
    let dst = parse_s3_uri(dst)?;
    client
        .copy_object()
        .bucket(&dst.bucket)
        .key(&dst.key)
        .copy_source(format!("{}/{}", src.bucket, src.key))
        .send()
        .await
        .map_err(AwlError::aws)?;
    output::emit(
        globals,
        Output::one(ActionRow {
            action: "cp".to_owned(),
            source: Some(format!("s3://{}/{}", src.bucket, src.key)),
            destination: Some(format!("s3://{}/{}", dst.bucket, dst.key)),
            bytes: None,
            dry_run: false,
        })?,
    )
}

async fn mv(globals: &Globals, src: &str, dst: &str, path_style: bool) -> Result<()> {
    cp(globals, src, dst, path_style).await?;
    let client = client(globals, path_style).await?;
    let src = parse_s3_uri(src)?;
    client
        .delete_object()
        .bucket(&src.bucket)
        .key(&src.key)
        .send()
        .await
        .map_err(AwlError::aws)?;
    Ok(())
}

async fn presign(globals: &Globals, args: &S3PresignArgs) -> Result<()> {
    let client = client(globals, args.common.path_style).await?;
    let uri = parse_s3_uri(&args.uri)?;
    let config = PresigningConfig::expires_in(parse_duration(&args.expires)?).map_err(|error| {
        AwlError::Usage {
            message: error.to_string(),
        }
    })?;
    let presigned = match args.method {
        PresignMethod::Get => client
            .get_object()
            .bucket(&uri.bucket)
            .key(&uri.key)
            .presigned(config)
            .await
            .map_err(AwlError::aws)?,
        PresignMethod::Put => client
            .put_object()
            .bucket(&uri.bucket)
            .key(&uri.key)
            .presigned(config)
            .await
            .map_err(AwlError::aws)?,
    };
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "url": presigned.uri().to_string(),
            "method": args.method.as_str(),
        }))?,
    )
}

async fn du(globals: &Globals, args: &S3DuArgs) -> Result<()> {
    let client = client(globals, args.common.path_style).await?;
    let uri = parse_s3_uri(&args.uri)?;
    let objects = list_objects(&client, &uri.bucket, normalized_prefix(&uri.key)).await?;
    let bytes = objects
        .iter()
        .map(|object| object.size.max(0) as u64)
        .sum::<u64>();
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "bucket": uri.bucket,
            "prefix": uri.key,
            "objects": objects.len(),
            "bytes": bytes,
        }))?,
    )
}

async fn head(globals: &Globals, args: &S3HeadArgs) -> Result<()> {
    let client = client(globals, args.common.path_style).await?;
    let uri = parse_s3_uri(&args.uri)?;
    let response = client
        .head_object()
        .bucket(&uri.bucket)
        .key(&uri.key)
        .send()
        .await
        .map_err(AwlError::aws)?;
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "bucket": uri.bucket,
            "key": uri.key,
            "content_length": response.content_length(),
            "content_type": response.content_type(),
            "etag": response.e_tag(),
            "metadata": response.metadata(),
        }))?,
    )
}

async fn sync(globals: &Globals, args: &S3SyncArgs) -> Result<()> {
    let client = client(globals, args.common.path_style).await?;
    let src = parse_location(&args.src);
    let dst = parse_location(&args.dst);
    let filter = PathFilter::new(&args.include, &args.exclude)?;
    let _requested_concurrency = args.concurrency;
    let _part_size = args.part_size;

    let result = match (src, dst) {
        (Location::Local(src), Location::S3(dst)) => {
            sync_local_to_s3(&client, &src, &dst, &filter, args).await?
        }
        (Location::S3(src), Location::Local(dst)) => {
            sync_s3_to_local(&client, &src, &dst, &filter, args).await?
        }
        (Location::S3(src), Location::S3(dst)) => {
            sync_s3_to_s3(&client, &src, &dst, &filter, args).await?
        }
        (Location::Local(_), Location::Local(_)) => {
            return Err(AwlError::Usage {
                message: "s3 sync requires at least one s3:// location".to_owned(),
            });
        }
    };

    if result.failed > 0 {
        return Err(AwlError::Aws {
            message: format!("{} succeeded, {} failed", result.succeeded, result.failed),
        });
    }

    output::emit(
        globals,
        Output::one(SummaryRow {
            succeeded: result.succeeded,
            failed: result.failed,
        })?,
    )
}

#[derive(Default)]
struct SyncResult {
    succeeded: usize,
    failed: usize,
}

async fn sync_local_to_s3(
    client: &aws_sdk_s3::Client,
    src: &Path,
    dst: &S3Uri,
    filter: &PathFilter,
    args: &S3SyncArgs,
) -> Result<SyncResult> {
    let mut result = SyncResult::default();
    let mut seen = HashSet::new();
    for entry in WalkDir::new(src) {
        let entry = entry.map_err(|error| AwlError::Io(std::io::Error::other(error)))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(src)
            .map_err(|error| AwlError::Usage {
                message: error.to_string(),
            })?;
        let relative_key = path_to_key(relative);
        if !filter.matches(&relative_key) {
            continue;
        }
        let key = join_key(&dst.key, &relative_key);
        seen.insert(key.clone());
        if args.dry_run {
            result.succeeded += 1;
            continue;
        }
        match tokio::fs::read(entry.path()).await {
            Ok(bytes) => {
                let send = client
                    .put_object()
                    .bucket(&dst.bucket)
                    .key(&key)
                    .body(ByteStream::from(bytes))
                    .send()
                    .await;
                if send.is_ok() {
                    result.succeeded += 1;
                } else {
                    result.failed += 1;
                    if args.fail_fast {
                        break;
                    }
                }
            }
            Err(_) => {
                result.failed += 1;
                if args.fail_fast {
                    break;
                }
            }
        }
    }

    if args.delete {
        for key in list_object_keys(client, &dst.bucket, normalized_prefix(&dst.key)).await? {
            if !seen.contains(&key) && filter.matches(&strip_prefix_key(&dst.key, &key)) {
                if !args.dry_run {
                    client
                        .delete_object()
                        .bucket(&dst.bucket)
                        .key(&key)
                        .send()
                        .await
                        .map_err(AwlError::aws)?;
                }
                result.succeeded += 1;
            }
        }
    }

    Ok(result)
}

async fn sync_s3_to_local(
    client: &aws_sdk_s3::Client,
    src: &S3Uri,
    dst: &Path,
    filter: &PathFilter,
    args: &S3SyncArgs,
) -> Result<SyncResult> {
    let mut result = SyncResult::default();
    let mut seen = HashSet::new();
    for object in list_objects(client, &src.bucket, normalized_prefix(&src.key)).await? {
        let relative = strip_prefix_key(&src.key, &object.key);
        if !filter.matches(&relative) {
            continue;
        }
        seen.insert(relative.clone());
        let destination = dst.join(relative);
        if args.dry_run {
            result.succeeded += 1;
            continue;
        }
        let response = client
            .get_object()
            .bucket(&src.bucket)
            .key(&object.key)
            .send()
            .await;
        match response {
            Ok(response) => {
                let bytes = response
                    .body
                    .collect()
                    .await
                    .map_err(AwlError::aws)?
                    .into_bytes();
                write_file(&destination, &bytes).await?;
                result.succeeded += 1;
            }
            Err(_) => {
                result.failed += 1;
                if args.fail_fast {
                    break;
                }
            }
        }
    }

    if args.delete && dst.exists() {
        for entry in WalkDir::new(dst) {
            let entry = entry.map_err(|error| AwlError::Io(std::io::Error::other(error)))?;
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(dst)
                .map_err(|error| AwlError::Usage {
                    message: error.to_string(),
                })?;
            let relative_key = path_to_key(relative);
            if !seen.contains(&relative_key) && filter.matches(&relative_key) {
                if !args.dry_run {
                    tokio::fs::remove_file(entry.path()).await?;
                }
                result.succeeded += 1;
            }
        }
    }

    Ok(result)
}

async fn sync_s3_to_s3(
    client: &aws_sdk_s3::Client,
    src: &S3Uri,
    dst: &S3Uri,
    filter: &PathFilter,
    args: &S3SyncArgs,
) -> Result<SyncResult> {
    let mut result = SyncResult::default();
    let mut seen = HashSet::new();
    for object in list_objects(client, &src.bucket, normalized_prefix(&src.key)).await? {
        let relative = strip_prefix_key(&src.key, &object.key);
        if !filter.matches(&relative) {
            continue;
        }
        let destination_key = join_key(&dst.key, &relative);
        seen.insert(destination_key.clone());
        if args.dry_run {
            result.succeeded += 1;
            continue;
        }
        let send = client
            .copy_object()
            .bucket(&dst.bucket)
            .key(&destination_key)
            .copy_source(format!("{}/{}", src.bucket, object.key))
            .send()
            .await;
        if send.is_ok() {
            result.succeeded += 1;
        } else {
            result.failed += 1;
            if args.fail_fast {
                break;
            }
        }
    }

    if args.delete {
        for key in list_object_keys(client, &dst.bucket, normalized_prefix(&dst.key)).await? {
            if !seen.contains(&key) && filter.matches(&strip_prefix_key(&dst.key, &key)) {
                if !args.dry_run {
                    client
                        .delete_object()
                        .bucket(&dst.bucket)
                        .key(&key)
                        .send()
                        .await
                        .map_err(AwlError::aws)?;
                }
                result.succeeded += 1;
            }
        }
    }

    Ok(result)
}

#[derive(Debug, Clone)]
struct ListedObject {
    key: String,
    size: i64,
}

async fn list_objects(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    prefix: String,
) -> Result<Vec<ListedObject>> {
    let mut token = None;
    let mut out = Vec::new();
    loop {
        let response = client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(&prefix)
            .set_continuation_token(token)
            .send()
            .await
            .map_err(AwlError::aws)?;
        for object in response.contents() {
            if let Some(key) = object.key() {
                out.push(ListedObject {
                    key: key.to_owned(),
                    size: object.size().unwrap_or_default(),
                });
            }
        }
        if response.is_truncated().unwrap_or(false) {
            token = response.next_continuation_token().map(str::to_owned);
        } else {
            break;
        }
    }
    Ok(out)
}

async fn list_object_keys(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    prefix: String,
) -> Result<Vec<String>> {
    Ok(list_objects(client, bucket, prefix)
        .await?
        .into_iter()
        .map(|object| object.key)
        .collect())
}

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

fn parse_location(value: &str) -> Location {
    if value.starts_with("s3://") {
        match parse_s3_uri(value) {
            Ok(uri) => Location::S3(uri),
            Err(_) => Location::Local(PathBuf::from(value)),
        }
    } else {
        Location::Local(PathBuf::from(value))
    }
}

fn normalized_prefix(prefix: &str) -> String {
    prefix.trim_start_matches('/').to_owned()
}

fn join_key(prefix: &str, key: &str) -> String {
    let prefix = prefix.trim_matches('/');
    let key = key.trim_start_matches('/');
    if prefix.is_empty() {
        key.to_owned()
    } else if key.is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix}/{key}")
    }
}

fn strip_prefix_key(prefix: &str, key: &str) -> String {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        return key.to_owned();
    }
    key.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(key)
        .to_owned()
}

fn path_to_key(path: &Path) -> String {
    path.components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

async fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, bytes).await?;
    Ok(())
}

fn parse_duration(value: &str) -> Result<Duration> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix('s') {
        (number, 1)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60)
    } else if let Some(number) = value.strip_suffix('h') {
        (number, 60 * 60)
    } else if let Some(number) = value.strip_suffix('d') {
        (number, 60 * 60 * 24)
    } else {
        (value, 1)
    };
    let seconds = number.parse::<u64>().map_err(|error| AwlError::Usage {
        message: format!("invalid duration {value:?}: {error}"),
    })?;
    Ok(Duration::from_secs(seconds.saturating_mul(multiplier)))
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
