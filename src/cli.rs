use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

use crate::error::Result;

#[derive(Debug, Parser)]
#[command(
    name = "awl",
    version,
    about = "A small, sharp tool for AWS operations",
    after_help = "Output defaults to a table on a TTY and JSONL when piped."
)]
#[command(arg_required_else_help = true)]
pub struct Cli {
    #[command(flatten)]
    pub globals: Globals,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args, Clone)]
#[command(next_help_heading = "Global Options")]
pub struct Globals {
    /// AWS profile name
    #[arg(long, global = true, env = "AWS_PROFILE")]
    pub profile: Option<String>,

    /// AWS region override
    #[arg(long, global = true, env = "AWS_REGION")]
    pub region: Option<String>,

    /// Endpoint URL for every service; AWS_ENDPOINT_URL[_<SERVICE>] still apply when unset
    #[arg(long, global = true)]
    pub endpoint_url: Option<String>,

    /// Output format [default: table on a TTY, jsonl when piped]
    #[arg(long, global = true, value_enum, env = "AWL_OUTPUT")]
    pub output: Option<OutputFormat>,

    /// Select one field from single-record structured output
    #[arg(long, global = true)]
    pub field: Option<String>,

    /// Suppress acknowledgements from mutating commands
    #[arg(short, long, global = true, env = "AWL_QUIET")]
    pub quiet: bool,

    /// Increase logging verbosity (-v info, -vv debug, -vvv trace)
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Assume this role before running the command
    #[arg(long, global = true, env = "AWL_ROLE_ARN")]
    pub role_arn: Option<String>,

    /// Session name for --role-arn and `sts assume`
    #[arg(long, global = true, env = "AWL_ROLE_SESSION_NAME")]
    pub role_session_name: Option<String>,

    /// MFA token code for `awl sts assume`
    #[arg(long, global = true)]
    pub mfa_token: Option<String>,

    /// AWS retry mode (legacy is accepted and behaves like standard)
    #[arg(long, global = true, value_enum, env = "AWS_RETRY_MODE")]
    pub retry_mode: Option<RetryMode>,

    /// Maximum total AWS attempts including the first request
    #[arg(long, global = true, env = "AWS_MAX_ATTEMPTS")]
    pub max_attempts: Option<u32>,
}

impl Globals {
    pub fn log_filter(&self) -> &'static str {
        match self.verbose {
            0 => "warn",
            1 => "info",
            2 => "debug",
            _ => "trace",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
    Jsonl,
    Toml,
    Yaml,
    Tsv,
    Plain,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RetryMode {
    Standard,
    Adaptive,
    Legacy,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Generate shell completions
    Completions(CompletionsArgs),
    /// Object storage
    S3(S3Command),
    /// Queues
    Sqs(SqsCommand),
    /// Identity and assume-role
    Sts(StsCommand),
    /// SSO / IAM Identity Center (wraps AWS CLI v2)
    Sso(SsoCommand),
    /// Container registry
    Ecr(EcrCommand),
    /// Parameter Store
    Ssm(SsmCommand),
    /// Secrets Manager
    Secrets(SecretsCommand),
    /// CloudWatch Logs
    Logs(LogsCommand),
    /// Instances, AMIs, and the instance type catalog
    Ec2(Ec2Command),
    /// Functions
    Lambda(LambdaCommand),
    /// DynamoDB
    Ddb(DdbCommand),
    /// DNS
    Route53(Route53Command),
    /// Key Management
    Kms(KmsCommand),
}

#[derive(Debug, Args)]
pub struct CompletionsArgs {
    shell: Shell,
}

impl CompletionsArgs {
    pub fn run(&self) -> Result<()> {
        let mut command = Cli::command();
        let name = command.get_name().to_owned();
        clap_complete::generate(self.shell, &mut command, name, &mut io::stdout());
        Ok(())
    }
}

#[derive(Debug, Args)]
pub struct DryRunArg {
    /// Show what would happen without changing anything
    #[arg(long = "dry-run", visible_alias = "dryrun")]
    pub dry_run: bool,
}

// s3

#[derive(Debug, Args)]
pub struct S3Command {
    #[command(subcommand)]
    pub command: S3Subcommand,
}

#[derive(Debug, Subcommand)]
pub enum S3Subcommand {
    /// List buckets, or objects and prefixes under an s3:// URI
    Ls(S3LsArgs),
    /// Download an object to a file, or to stdout with `-`
    Get(S3GetArgs),
    /// Upload a file (multipart above --part-size)
    Put(S3PutArgs),
    /// One-way sync between a local directory and S3, or S3 and S3
    #[command(
        long_about = "One-way sync between a local directory and S3, or S3 and S3.\n\nS3 paths are treated as directories: s3://b/logs means s3://b/logs/, never s3://b/logs-archive/. Files are transferred when missing at the destination, when sizes differ, or when the source is newer."
    )]
    Sync(S3SyncArgs),
    /// Remove an object, or everything under a prefix with -r
    Rm(S3RmArgs),
    /// Server-side copy between S3 URIs
    Cp(S3CpArgs),
    /// Server-side copy, then delete the source
    Mv(S3CpArgs),
    /// Create a presigned URL
    Presign(S3PresignArgs),
    /// Object count and total size under a prefix
    Du(S3UriArgs),
    /// Read object metadata
    Head(S3UriArgs),
}

#[derive(Debug, Args)]
pub struct S3CommonArgs {
    /// Use path-style addressing (for S3-compatible services)
    #[arg(long, env = "AWL_S3_PATH_STYLE")]
    pub path_style: bool,
}

#[derive(Debug, Args)]
pub struct S3LsArgs {
    #[command(flatten)]
    pub common: S3CommonArgs,
    /// s3://bucket[/prefix]; omit to list buckets
    pub uri: Option<String>,
    /// List every object under the prefix instead of one level
    #[arg(short, long)]
    pub recursive: bool,
}

#[derive(Debug, Args)]
pub struct S3GetArgs {
    #[command(flatten)]
    pub common: S3CommonArgs,
    /// s3://bucket/key
    pub uri: String,
    /// Destination path, or `-` for stdout [default: the key's file name]
    pub dest: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct S3PutArgs {
    #[command(flatten)]
    pub common: S3CommonArgs,
    /// Local file
    pub src: PathBuf,
    /// s3://bucket/key
    pub uri: String,
    #[arg(long)]
    pub content_type: Option<String>,
    /// Object metadata (repeatable)
    #[arg(long = "metadata", value_name = "KEY=VALUE", value_parser = parse_key_value)]
    pub metadata: Vec<(String, String)>,
    /// Canned ACL, such as private or bucket-owner-full-control
    #[arg(long)]
    pub acl: Option<String>,
    /// Storage class, such as STANDARD_IA or GLACIER_IR
    #[arg(long)]
    pub storage_class: Option<String>,
    #[command(flatten)]
    pub transfer: S3TransferArgs,
}

#[derive(Debug, Args)]
pub struct S3TransferArgs {
    /// Multipart part size in bytes (min 5 MiB)
    #[arg(long, env = "AWL_S3_PART_SIZE", default_value_t = 8 * 1024 * 1024)]
    pub part_size: u64,
}

#[derive(Debug, Args)]
pub struct S3SyncArgs {
    #[command(flatten)]
    pub common: S3CommonArgs,
    /// Source: local directory or s3:// URI
    pub src: String,
    /// Destination: local directory or s3:// URI
    pub dst: String,
    /// Delete destination files that are missing from the source (skipped if any transfer fails)
    #[arg(long)]
    pub delete: bool,
    /// Skip paths matching this glob (repeatable)
    #[arg(long, value_name = "GLOB")]
    pub exclude: Vec<String>,
    /// Only sync paths matching this glob (repeatable)
    #[arg(long, value_name = "GLOB")]
    pub include: Vec<String>,
    #[command(flatten)]
    pub dry_run: DryRunArg,
    /// Maximum concurrent transfers
    #[arg(long, env = "AWL_S3_CONCURRENCY", default_value_t = 32, value_parser = clap::value_parser!(u16).range(1..))]
    pub concurrency: u16,
    #[command(flatten)]
    pub transfer: S3TransferArgs,
    /// Stop starting new transfers after the first failure
    #[arg(long)]
    pub fail_fast: bool,
}

#[derive(Debug, Args)]
pub struct S3RmArgs {
    #[command(flatten)]
    pub common: S3CommonArgs,
    /// s3://bucket/key, or s3://bucket/prefix with -r
    pub uri: String,
    /// Remove every object under the prefix (treated as a directory)
    #[arg(short, long)]
    pub recursive: bool,
    #[command(flatten)]
    pub dry_run: DryRunArg,
}

#[derive(Debug, Args)]
pub struct S3CpArgs {
    #[command(flatten)]
    pub common: S3CommonArgs,
    /// s3://bucket/key
    pub src: String,
    /// s3://bucket/key
    pub dst: String,
    #[command(flatten)]
    pub transfer: S3TransferArgs,
}

#[derive(Debug, Args)]
pub struct S3PresignArgs {
    #[command(flatten)]
    pub common: S3CommonArgs,
    /// s3://bucket/key
    pub uri: String,
    /// Lifetime, such as 90s, 15m, 1h, or 7d
    #[arg(long, default_value = "1h", value_parser = parse_duration)]
    pub expires: Duration,
    /// HTTP method the URL is valid for
    #[arg(long, value_enum, default_value_t = PresignMethod::Get)]
    pub method: PresignMethod,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum PresignMethod {
    Get,
    Put,
}

impl PresignMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            PresignMethod::Get => "GET",
            PresignMethod::Put => "PUT",
        }
    }
}

#[derive(Debug, Args)]
pub struct S3UriArgs {
    #[command(flatten)]
    pub common: S3CommonArgs,
    /// s3://bucket[/key]
    pub uri: String,
}

// sqs

#[derive(Debug, Args)]
pub struct SqsCommand {
    #[command(subcommand)]
    pub command: SqsSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum SqsSubcommand {
    /// List queues
    Ls(SqsLsArgs),
    /// Send a message
    Send(SqsSendArgs),
    /// Receive messages, optionally deleting them after they are printed
    Receive(SqsReceiveArgs),
    /// Purge a queue
    Purge(SqsQueueArg),
    /// Read queue attributes
    Attrs(SqsQueueArg),
    /// Resolve a queue name to its URL
    Url(SqsQueueArg),
}

#[derive(Debug, Args)]
pub struct SqsLsArgs {
    /// Only queues whose name starts with this prefix
    #[arg(long)]
    pub prefix: Option<String>,
}

#[derive(Debug, Args)]
pub struct SqsQueueArg {
    /// Queue name or URL
    pub queue: String,
}

#[derive(Debug, Args)]
pub struct SqsSendArgs {
    /// Queue name or URL
    pub queue: String,
    /// Message body, or `-` to read stdin
    pub body: String,
    /// Delay delivery by this many seconds
    #[arg(long)]
    pub delay: Option<i32>,
    /// String message attribute (repeatable)
    #[arg(long = "attribute", value_name = "KEY=VALUE", value_parser = parse_key_value)]
    pub attribute: Vec<(String, String)>,
    /// FIFO message group ID
    #[arg(long)]
    pub group_id: Option<String>,
    /// FIFO deduplication ID
    #[arg(long)]
    pub dedup_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct SqsReceiveArgs {
    /// Queue name or URL
    pub queue: String,
    /// Messages per receive call (1-10)
    #[arg(long, env = "AWL_SQS_MAX_MESSAGES", default_value_t = 10, value_parser = clap::value_parser!(i32).range(1..=10))]
    pub max: i32,
    /// Long-poll wait in seconds (0-20)
    #[arg(long, env = "AWL_SQS_WAIT_SECONDS", default_value_t = 20, value_parser = clap::value_parser!(i32).range(0..=20))]
    pub wait: i32,
    /// Visibility timeout in seconds for received messages
    #[arg(long)]
    pub visibility: Option<i32>,
    /// Delete each batch after it has been printed
    #[arg(long)]
    pub delete: bool,
    /// Keep polling and print messages as they arrive
    #[arg(short, long)]
    pub follow: bool,
}

// sts

#[derive(Debug, Args)]
pub struct StsCommand {
    #[command(subcommand)]
    pub command: StsSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum StsSubcommand {
    /// Show the caller identity
    #[command(visible_alias = "get-caller-identity")]
    Whoami,
    /// Assume a role and print credentials
    #[command(
        after_help = "Example: eval \"$(awl sts assume arn:aws:iam::123456789012:role/admin)\""
    )]
    Assume(StsAssumeArgs),
    /// Decode an encoded authorization failure message
    Decode(StsDecodeArgs),
}

#[derive(Debug, Args)]
pub struct StsAssumeArgs {
    pub role_arn: String,
    /// Session duration in seconds
    #[arg(long, default_value_t = 3600)]
    pub duration: i32,
    /// Session name [default: --role-session-name or awl-<unix-ts>]
    #[arg(long)]
    pub session_name: Option<String>,
    /// env prints shell exports; json prints a credentials object
    #[arg(long, value_enum, default_value_t = CredentialFormat::Env)]
    pub format: CredentialFormat,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CredentialFormat {
    Env,
    Json,
}

#[derive(Debug, Args)]
pub struct StsDecodeArgs {
    pub encoded_message: String,
}

// sso

#[derive(Debug, Args)]
pub struct SsoCommand {
    #[command(subcommand)]
    pub command: SsoSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum SsoSubcommand {
    /// Run `aws sso login` for the selected profile
    Login(SsoLoginArgs),
    /// Run `aws sso logout`
    Logout,
    /// List cached SSO sessions
    Ls,
}

#[derive(Debug, Args)]
pub struct SsoLoginArgs {
    /// Print the device URL instead of opening a browser
    #[arg(long, env = "AWL_SSO_NO_BROWSER")]
    pub no_browser: bool,
}

// ecr

#[derive(Debug, Args)]
pub struct EcrCommand {
    #[command(subcommand)]
    pub command: EcrSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum EcrSubcommand {
    /// List repositories
    Ls,
    /// List image tags in a repository
    Tags(EcrRepoArg),
    /// Log docker in to the registry
    Login(EcrLoginArgs),
    /// Point a new tag at an existing image
    Retag(EcrRetagArgs),
    /// Copy an image manifest to another repository or tag
    #[command(
        long_about = "Copy an image manifest to another repository or tag in the same registry.\n\nECR only accepts a manifest whose layers already exist in the destination repository; copying to a repository without them fails with a clear error. Use a registry tool such as crane or skopeo for full copies."
    )]
    Cp(EcrCpArgs),
    /// Show image scan finding counts
    Scan(EcrImageArg),
    /// Resolve an image tag to its digest
    Digest(EcrImageArg),
}

#[derive(Debug, Args)]
pub struct EcrRepoArg {
    pub repo: String,
}

#[derive(Debug, Args)]
pub struct EcrLoginArgs {
    /// Print only the registry password instead of running `docker login`
    #[arg(long)]
    pub print_password: bool,
}

#[derive(Debug, Args)]
pub struct EcrRetagArgs {
    pub repo: String,
    pub src_tag: String,
    pub dst_tag: String,
}

#[derive(Debug, Args)]
pub struct EcrCpArgs {
    /// repo:tag or repo@sha256:digest
    pub src: String,
    /// repo:tag
    pub dst: String,
}

#[derive(Debug, Args)]
pub struct EcrImageArg {
    /// repo:tag or repo@sha256:digest
    pub image: String,
}

// ssm

#[derive(Debug, Args)]
pub struct SsmCommand {
    #[command(subcommand)]
    pub command: SsmSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum SsmSubcommand {
    /// Read a parameter (decrypted by default)
    Get(SsmGetArgs),
    /// List parameters under a path
    Ls(SsmLsArgs),
    /// Create or update a parameter
    Put(SsmPutArgs),
    /// Delete a parameter
    Rm(SsmNameArg),
    /// Run a command with every parameter under a path exported as env vars
    #[command(
        long_about = "Run a command with every parameter under a path exported as env vars.\n\nNames are made relative to --prefix, uppercased, and non-alphanumerics become `_`: /app/db/password under /app becomes DB_PASSWORD.",
        after_help = "Example: awl ssm exec --prefix /app/prod -- ./server"
    )]
    Exec(SsmExecArgs),
}

#[derive(Debug, Args)]
pub struct SsmGetArgs {
    pub name: String,
    /// Return SecureString values still encrypted
    #[arg(long)]
    pub no_decrypt: bool,
}

#[derive(Debug, Args)]
pub struct SsmLsArgs {
    #[arg(default_value = "/")]
    pub path: String,
    /// Include parameters in nested paths
    #[arg(short, long)]
    pub recursive: bool,
    /// Include decrypted values
    #[arg(long)]
    pub values: bool,
}

#[derive(Debug, Args)]
pub struct SsmPutArgs {
    pub name: String,
    /// Value, or `-` to read stdin
    pub value: String,
    #[arg(long = "type", value_enum, default_value_t = SsmParameterType::String)]
    pub parameter_type: SsmParameterType,
    /// Replace an existing parameter
    #[arg(long)]
    pub overwrite: bool,
    /// KMS key for SecureString parameters
    #[arg(long, env = "AWL_SSM_KMS_KEY")]
    pub kms_key: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SsmParameterType {
    String,
    StringList,
    SecureString,
}

#[derive(Debug, Args)]
pub struct SsmNameArg {
    pub name: String,
}

#[derive(Debug, Args)]
pub struct SsmExecArgs {
    /// Parameter path to export
    #[arg(long, env = "AWL_SSM_EXEC_PREFIX")]
    pub prefix: String,
    /// Fail when no parameters are found
    #[arg(long)]
    pub strict: bool,
    /// Command and arguments to run
    #[arg(last = true, required = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

// secrets

#[derive(Debug, Args)]
pub struct SecretsCommand {
    #[command(subcommand)]
    pub command: SecretsSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum SecretsSubcommand {
    /// Read a secret value
    Get(SecretsGetArgs),
    /// List secrets
    Ls,
    /// Store a new secret value
    Put(SecretsPutArgs),
    /// Start rotation
    Rotate(SecretsNameArg),
    /// Schedule deletion, or delete immediately with --force
    Rm(SecretsRmArgs),
}

#[derive(Debug, Args)]
pub struct SecretsGetArgs {
    pub name: String,
    #[arg(long)]
    pub version_id: Option<String>,
    /// Version stage, such as AWSPREVIOUS
    #[arg(long)]
    pub version_stage: Option<String>,
}

#[derive(Debug, Args)]
pub struct SecretsPutArgs {
    pub name: String,
    /// Secret string, or `-` to read stdin
    pub value: String,
}

#[derive(Debug, Args)]
pub struct SecretsNameArg {
    pub name: String,
}

#[derive(Debug, Args)]
pub struct SecretsRmArgs {
    pub name: String,
    /// Delete immediately without a recovery window
    #[arg(long)]
    pub force: bool,
}

// logs

#[derive(Debug, Args)]
pub struct LogsCommand {
    #[command(subcommand)]
    pub command: LogsSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum LogsSubcommand {
    /// List log groups
    Ls(LogsLsArgs),
    /// List streams in a group, most recent first
    Streams(LogsStreamsArgs),
    /// Show recent events, optionally following new ones
    Tail(LogsTailArgs),
    /// Read a whole stream from the start
    Get(LogsGetArgs),
}

#[derive(Debug, Args)]
pub struct LogsLsArgs {
    #[arg(long)]
    pub prefix: Option<String>,
}

#[derive(Debug, Args)]
pub struct LogsStreamsArgs {
    pub group: String,
    /// Maximum streams to list
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Debug, Args)]
pub struct LogsTailArgs {
    pub group: String,
    #[command(flatten)]
    pub options: TailOptions,
}

#[derive(Debug, Args)]
pub struct TailOptions {
    /// Keep polling for new events
    #[arg(short, long)]
    pub follow: bool,
    /// Start this far back, such as 30s, 5m, 1h, or 2d
    #[arg(long, env = "AWL_LOGS_SINCE", default_value = "10m", value_parser = parse_duration)]
    pub since: Duration,
    /// CloudWatch Logs filter pattern
    #[arg(long)]
    pub filter: Option<String>,
    /// Only these streams (repeatable)
    #[arg(long)]
    pub stream: Vec<String>,
    /// Stop after this many events (ignored with --follow)
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Debug, Args)]
pub struct LogsGetArgs {
    pub group: String,
    pub stream: String,
    /// Stop after this many events
    #[arg(long)]
    pub limit: Option<usize>,
}

// ec2

#[derive(Debug, Args)]
pub struct Ec2Command {
    #[command(subcommand)]
    pub command: Ec2Subcommand,
}

#[derive(Debug, Subcommand)]
pub enum Ec2Subcommand {
    /// List instances
    Ls(Ec2LsArgs),
    /// Open a shell through SSM Session Manager (needs session-manager-plugin)
    Ssh(Ec2InstanceArg),
    /// Start instances
    Start(Ec2InstancesArgs),
    /// Stop instances
    Stop(Ec2StopArgs),
    /// Reboot instances
    Reboot(Ec2InstancesArgs),
    /// Read console output
    Console(Ec2InstanceArg),
    /// List AMIs you own, or describe specific AMIs
    Ami(Ec2AmiArgs),
    /// Search the embedded EC2 instance type catalog (no AWS calls)
    Types(Ec2TypesArgs),
}

#[derive(Debug, Args)]
pub struct Ec2LsArgs {
    /// Only these instance IDs
    pub instance_ids: Vec<String>,
}

#[derive(Debug, Args)]
pub struct Ec2InstanceArg {
    pub instance_id: String,
}

#[derive(Debug, Args)]
pub struct Ec2InstancesArgs {
    #[arg(required = true)]
    pub instance_ids: Vec<String>,
    #[command(flatten)]
    pub dry_run: DryRunArg,
}

#[derive(Debug, Args)]
pub struct Ec2StopArgs {
    #[command(flatten)]
    pub instances: Ec2InstancesArgs,
    /// Force stop without a graceful OS shutdown
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct Ec2AmiArgs {
    /// Only these image IDs
    pub image_ids: Vec<String>,
    /// Owner filter, such as self, amazon, or an account ID [default: self when no IDs are given]
    #[arg(long)]
    pub owner: Vec<String>,
}

#[derive(Debug, Args)]
pub struct Ec2TypesArgs {
    /// Filter by instance type, family, processor, or display name
    pub query: Option<String>,

    /// Filter to an architecture, such as x86_64 or arm64
    #[arg(long)]
    pub arch: Option<String>,

    /// Filter by family text, such as general or gpu
    #[arg(long)]
    pub family: Option<String>,

    /// Only show current-generation instance types
    #[arg(long)]
    pub current: bool,

    /// Only show instance types with GPUs
    #[arg(long)]
    pub gpu: bool,

    /// Minimum vCPU count
    #[arg(long)]
    pub min_vcpu: Option<u32>,

    /// Maximum vCPU count
    #[arg(long)]
    pub max_vcpu: Option<u32>,

    /// Minimum memory in GiB
    #[arg(long)]
    pub min_memory: Option<f64>,

    /// Maximum memory in GiB
    #[arg(long)]
    pub max_memory: Option<f64>,

    /// Sort output
    #[arg(long, value_enum, default_value_t = Ec2TypesSort::Type)]
    pub sort: Ec2TypesSort,

    /// Limit the number of rows returned
    #[arg(long)]
    pub limit: Option<usize>,

    /// Include processor, EBS, VPC, and GPU detail columns
    #[arg(long)]
    pub wide: bool,

    /// Region for the Linux on-demand price column [default: --region, AWS_REGION, AWS_DEFAULT_REGION, then us-east-1]
    #[arg(long)]
    pub price_region: Option<String>,

    /// Do not include a selected-region price column
    #[arg(long, conflicts_with = "all_prices")]
    pub no_price: bool,

    /// Include all embedded Linux on-demand prices in each output row
    #[arg(long)]
    pub all_prices: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Ec2TypesSort {
    Type,
    Vcpu,
    Memory,
    Price,
}

// lambda

#[derive(Debug, Args)]
pub struct LambdaCommand {
    #[command(subcommand)]
    pub command: LambdaSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum LambdaSubcommand {
    /// List functions
    Ls(LambdaLsArgs),
    /// Invoke a function
    Invoke(LambdaInvokeArgs),
    /// Show recent log events for a function
    Logs(LambdaLogsArgs),
}

#[derive(Debug, Args)]
pub struct LambdaLsArgs {
    /// Maximum functions to list
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Debug, Args)]
pub struct LambdaInvokeArgs {
    pub function: String,
    /// JSON payload, or `-` to read stdin [default: {}]
    #[arg(conflicts_with = "payload_file")]
    pub payload: Option<String>,
    /// Read the payload from a file
    #[arg(long)]
    pub payload_file: Option<PathBuf>,
    #[arg(long = "type", value_enum, default_value_t = LambdaInvocationType::RequestResponse)]
    pub invocation_type: LambdaInvocationType,
    /// Include the last 4 KB of the execution log
    #[arg(long)]
    pub tail: bool,
    /// Version or alias
    #[arg(long)]
    pub qualifier: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LambdaInvocationType {
    RequestResponse,
    Event,
    DryRun,
}

#[derive(Debug, Args)]
pub struct LambdaLogsArgs {
    pub function: String,
    #[command(flatten)]
    pub options: TailOptions,
}

// ddb

#[derive(Debug, Args)]
#[command(
    after_help = "Items and keys are plain JSON ({\"pk\": \"a\", \"n\": 1}) or DynamoDB JSON ({\"pk\": {\"S\": \"a\"}}) when every attribute is typed. Use DynamoDB JSON for numbers beyond 15 significant digits."
)]
pub struct DdbCommand {
    #[command(subcommand)]
    pub command: DdbSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum DdbSubcommand {
    /// List tables
    Ls,
    /// Describe a table
    Desc(DdbTableArg),
    /// Read one item by key
    Get(DdbGetArgs),
    /// Query by key condition
    Query(DdbQueryArgs),
    /// Scan a table
    Scan(DdbScanArgs),
    /// Write an item
    Put(DdbItemArgs),
    /// Delete an item by key
    Rm(DdbKeyArgs),
}

#[derive(Debug, Args)]
pub struct DdbTableArg {
    pub table: String,
}

#[derive(Debug, Args)]
pub struct DdbGetArgs {
    pub table: String,
    /// Key as JSON
    pub key: String,
    #[arg(long)]
    pub projection: Option<String>,
    /// Expression attribute names as a JSON object
    #[arg(long)]
    pub names: Option<String>,
}

#[derive(Debug, Args)]
pub struct DdbExpressionArgs {
    #[arg(long)]
    pub filter: Option<String>,
    /// Maximum items to return
    #[arg(long)]
    pub limit: Option<usize>,
    /// Expression attribute names as a JSON object
    #[arg(long)]
    pub names: Option<String>,
    /// Expression attribute values as a JSON object
    #[arg(long)]
    pub values: Option<String>,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: awl ddb query users 'pk = :pk' --values '{\":pk\": \"u#1\"}'")]
pub struct DdbQueryArgs {
    pub table: String,
    pub key_condition: String,
    #[command(flatten)]
    pub expression: DdbExpressionArgs,
}

#[derive(Debug, Args)]
pub struct DdbScanArgs {
    pub table: String,
    #[command(flatten)]
    pub expression: DdbExpressionArgs,
}

#[derive(Debug, Args)]
pub struct DdbItemArgs {
    pub table: String,
    /// Item as JSON
    pub item: String,
}

#[derive(Debug, Args)]
pub struct DdbKeyArgs {
    pub table: String,
    /// Key as JSON
    pub key: String,
}

// route53

#[derive(Debug, Args)]
pub struct Route53Command {
    #[command(subcommand)]
    pub command: Route53Subcommand,
}

#[derive(Debug, Subcommand)]
pub enum Route53Subcommand {
    /// List hosted zones
    Zones,
    /// List records in a zone
    Ls(Route53LsArgs),
    /// Read one record set
    Get(Route53GetArgs),
    /// Create, upsert, or delete a record set
    #[command(
        after_help = "Example: awl route53 set example.com api.example.com A 10.0.0.1 10.0.0.2 --ttl 60"
    )]
    Set(Route53SetArgs),
}

#[derive(Debug, Args)]
pub struct Route53LsArgs {
    /// Zone ID or name
    pub zone: String,
    /// Only records with this name
    #[arg(long)]
    pub name: Option<String>,
    /// Only records of this type
    #[arg(long = "type")]
    pub record_type: Option<String>,
}

#[derive(Debug, Args)]
pub struct Route53GetArgs {
    /// Zone ID or name
    pub zone: String,
    pub name: String,
    #[arg(default_value = "A")]
    pub record_type: String,
}

#[derive(Debug, Args)]
pub struct Route53SetArgs {
    /// Zone ID or name
    pub zone: String,
    pub name: String,
    pub record_type: String,
    /// Record values (TXT values need their own quotes)
    #[arg(required = true)]
    pub values: Vec<String>,
    #[arg(long, default_value_t = 300)]
    pub ttl: i64,
    #[arg(long, value_enum, default_value_t = Route53Action::Upsert)]
    pub action: Route53Action,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Route53Action {
    Create,
    Upsert,
    Delete,
}

// kms

#[derive(Debug, Args)]
pub struct KmsCommand {
    #[command(subcommand)]
    pub command: KmsSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum KmsSubcommand {
    /// List keys
    Ls,
    /// Encrypt a value
    Encrypt(KmsEncryptArgs),
    /// Decrypt a ciphertext
    Decrypt(KmsDecryptArgs),
    /// Generate a data key
    GenerateDataKey(KmsDataKeyArgs),
}

#[derive(Debug, Args)]
pub struct KmsEncryptArgs {
    /// Plaintext, or `-` to read stdin
    pub plaintext: String,
    /// Key ID, ARN, or alias
    #[arg(long, env = "AWL_KMS_KEY")]
    pub key_id: String,
    /// Encryption context (repeatable)
    #[arg(long, value_name = "KEY=VALUE", value_parser = parse_key_value)]
    pub context: Vec<(String, String)>,
    /// Ciphertext encoding
    #[arg(long, value_enum, default_value_t = BlobEncoding::B64)]
    pub encoding: BlobEncoding,
}

#[derive(Debug, Args)]
pub struct KmsDecryptArgs {
    /// Ciphertext, or `-` to read stdin
    pub ciphertext: String,
    /// Ciphertext encoding
    #[arg(long, value_enum, default_value_t = BlobEncoding::B64)]
    pub input_encoding: BlobEncoding,
    /// Plaintext encoding; raw requires UTF-8 plaintext
    #[arg(long, value_enum, default_value_t = BlobEncoding::Raw)]
    pub encoding: BlobEncoding,
    /// Expected key ID, ARN, or alias
    #[arg(long)]
    pub key_id: Option<String>,
    /// Encryption context (repeatable)
    #[arg(long, value_name = "KEY=VALUE", value_parser = parse_key_value)]
    pub context: Vec<(String, String)>,
}

#[derive(Debug, Args)]
pub struct KmsDataKeyArgs {
    /// Key ID, ARN, or alias
    #[arg(long, env = "AWL_KMS_KEY")]
    pub key_id: String,
    #[arg(long, value_enum, default_value_t = DataKeySpec::Aes256)]
    pub spec: DataKeySpec,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum BlobEncoding {
    B64,
    Hex,
    Raw,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DataKeySpec {
    #[value(name = "AES_256", alias = "aes-256")]
    Aes256,
    #[value(name = "AES_128", alias = "aes-128")]
    Aes128,
}

fn parse_key_value(value: &str) -> std::result::Result<(String, String), String> {
    let Some((key, value)) = value.split_once('=') else {
        return Err(format!(
            "expected KEY=VALUE, got {value:?}; value must contain '='"
        ));
    };
    if key.is_empty() {
        return Err("key cannot be empty".to_owned());
    }
    Ok((key.to_owned(), value.to_owned()))
}

pub fn parse_duration(value: &str) -> std::result::Result<Duration, String> {
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
    let seconds = number
        .parse::<u64>()
        .map_err(|error| format!("invalid duration {value:?}: {error}"))?;
    Ok(Duration::from_secs(seconds.saturating_mul(multiplier)))
}

pub fn filename(path: &std::path::Path) -> Option<&OsStr> {
    path.file_name().filter(|name| !name.is_empty())
}
