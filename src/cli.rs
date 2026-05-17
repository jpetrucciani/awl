use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

use crate::error::Result;

#[derive(Debug, Parser)]
#[command(
    name = "awl",
    version,
    about = "A small, sharp tool for AWS operations"
)]
#[command(arg_required_else_help = true)]
pub struct Cli {
    #[command(flatten)]
    pub globals: Globals,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args, Clone)]
pub struct Globals {
    #[arg(long, global = true, env = "AWS_PROFILE", help = "AWS profile name")]
    pub profile: Option<String>,

    #[arg(long, global = true, env = "AWS_REGION", help = "AWS region override")]
    pub region: Option<String>,

    #[arg(
        long,
        global = true,
        env = "AWS_ENDPOINT_URL",
        help = "Custom AWS endpoint URL"
    )]
    pub endpoint_url: Option<String>,

    #[arg(
        long,
        global = true,
        value_enum,
        env = "AWL_OUTPUT",
        help = "Output format"
    )]
    pub output: Option<OutputFormat>,

    #[arg(
        long,
        global = true,
        help = "Select one field from single-record structured output"
    )]
    pub field: Option<String>,

    #[arg(
        long,
        global = true,
        action = clap::ArgAction::SetTrue,
        help = "Disable ANSI color"
    )]
    pub no_color: bool,

    #[arg(
        short,
        long,
        global = true,
        env = "AWL_QUIET",
        help = "Suppress non-essential output"
    )]
    pub quiet: bool,

    #[arg(
        short,
        long,
        global = true,
        action = clap::ArgAction::Count,
        help = "Increase logging verbosity"
    )]
    pub verbose: u8,

    #[arg(
        long,
        global = true,
        env = "AWS_ROLE_ARN",
        help = "Assume this role before running the command"
    )]
    pub role_arn: Option<String>,

    #[arg(
        long,
        global = true,
        env = "AWS_ROLE_SESSION_NAME",
        help = "Session name for --role-arn"
    )]
    pub role_session_name: Option<String>,

    #[arg(long, global = true, help = "MFA token for `awl sts assume`")]
    pub mfa_token: Option<String>,

    #[arg(long, global = true, env = "AWS_RETRY_MODE", help = "AWS retry mode")]
    pub retry_mode: Option<String>,

    #[arg(
        long,
        global = true,
        env = "AWS_MAX_ATTEMPTS",
        help = "Maximum total AWS attempts including the first request"
    )]
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

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "Generate shell completions")]
    Completions(CompletionsArgs),
    #[command(about = "Object storage")]
    S3(S3Command),
    #[command(about = "Queues")]
    Sqs(SqsCommand),
    #[command(about = "Identity and assume-role")]
    Sts(StsCommand),
    #[command(about = "SSO / IAM Identity Center")]
    Sso(SsoCommand),
    #[command(about = "Container registry")]
    Ecr(EcrCommand),
    #[command(about = "Parameter Store")]
    Ssm(SsmCommand),
    #[command(about = "Secrets Manager")]
    Secrets(SecretsCommand),
    #[command(about = "CloudWatch Logs")]
    Logs(LogsCommand),
    #[command(about = "Instances")]
    Ec2(Ec2Command),
    #[command(about = "Functions")]
    Lambda(LambdaCommand),
    #[command(about = "DynamoDB")]
    Ddb(DdbCommand),
    #[command(about = "DNS")]
    Route53(Route53Command),
    #[command(about = "Key Management")]
    Kms(KmsCommand),
}

impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Command::Completions(_) => "completions",
            Command::S3(_) => "s3",
            Command::Sqs(_) => "sqs",
            Command::Sts(_) => "sts",
            Command::Sso(_) => "sso",
            Command::Ecr(_) => "ecr",
            Command::Ssm(_) => "ssm",
            Command::Secrets(_) => "secrets",
            Command::Logs(_) => "logs",
            Command::Ec2(_) => "ec2",
            Command::Lambda(_) => "lambda",
            Command::Ddb(_) => "ddb",
            Command::Route53(_) => "route53",
            Command::Kms(_) => "kms",
        }
    }
}

#[derive(Debug, Args)]
pub struct CompletionsArgs {
    #[arg(value_enum)]
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
pub struct S3Command {
    #[command(subcommand)]
    pub command: S3Subcommand,
}

#[derive(Debug, Subcommand)]
pub enum S3Subcommand {
    #[command(about = "List buckets or objects")]
    Ls(S3LsArgs),
    #[command(about = "Download an object")]
    Get(S3GetArgs),
    #[command(about = "Upload an object")]
    Put(S3PutArgs),
    #[command(about = "One-way source-to-destination sync")]
    Sync(S3SyncArgs),
    #[command(about = "Remove an object")]
    Rm(S3RmArgs),
    #[command(about = "Server-side copy")]
    Cp(S3CpArgs),
    #[command(about = "Move an object")]
    Mv(S3MvArgs),
    #[command(about = "Create a presigned URL")]
    Presign(S3PresignArgs),
    #[command(about = "Recursive object size summary")]
    Du(S3DuArgs),
    #[command(about = "Read object metadata")]
    Head(S3HeadArgs),
}

#[derive(Debug, Args)]
pub struct S3CommonArgs {
    #[arg(long, env = "AWL_S3_PATH_STYLE")]
    pub path_style: bool,
}

#[derive(Debug, Args)]
pub struct S3LsArgs {
    #[command(flatten)]
    pub common: S3CommonArgs,
    pub uri: Option<String>,
    #[arg(short, long)]
    pub recursive: bool,
    #[arg(short = 'H', long)]
    pub human: bool,
    #[arg(short, long)]
    pub long: bool,
}

#[derive(Debug, Args)]
pub struct S3GetArgs {
    #[command(flatten)]
    pub common: S3CommonArgs,
    pub uri: String,
    pub dest: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct S3PutArgs {
    #[command(flatten)]
    pub common: S3CommonArgs,
    pub src: PathBuf,
    pub uri: String,
    #[arg(long)]
    pub content_type: Option<String>,
    #[arg(long = "metadata", value_parser = parse_key_value)]
    pub metadata: Vec<(String, String)>,
    #[arg(long)]
    pub acl: Option<String>,
    #[arg(long)]
    pub storage_class: Option<String>,
}

#[derive(Debug, Args)]
pub struct S3SyncArgs {
    #[command(flatten)]
    pub common: S3CommonArgs,
    pub src: String,
    pub dst: String,
    #[arg(long)]
    pub delete: bool,
    #[arg(long)]
    pub exclude: Vec<String>,
    #[arg(long)]
    pub include: Vec<String>,
    #[arg(long = "dry-run", alias = "dryrun")]
    pub dry_run: bool,
    #[arg(long, env = "AWL_S3_CONCURRENCY", default_value_t = 32)]
    pub concurrency: usize,
    #[arg(long, env = "AWL_S3_PART_SIZE", default_value = "8388608")]
    pub part_size: u64,
    #[arg(long)]
    pub fail_fast: bool,
}

#[derive(Debug, Args)]
pub struct S3RmArgs {
    #[command(flatten)]
    pub common: S3CommonArgs,
    pub uri: String,
    #[arg(short, long)]
    pub recursive: bool,
    #[arg(long = "dry-run", alias = "dryrun")]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct S3CpArgs {
    #[command(flatten)]
    pub common: S3CommonArgs,
    pub src: String,
    pub dst: String,
}

#[derive(Debug, Args)]
pub struct S3MvArgs {
    #[command(flatten)]
    pub common: S3CommonArgs,
    pub src: String,
    pub dst: String,
}

#[derive(Debug, Args)]
pub struct S3PresignArgs {
    #[command(flatten)]
    pub common: S3CommonArgs,
    pub uri: String,
    #[arg(long, default_value = "1h")]
    pub expires: String,
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
pub struct S3DuArgs {
    #[command(flatten)]
    pub common: S3CommonArgs,
    pub uri: String,
}

#[derive(Debug, Args)]
pub struct S3HeadArgs {
    #[command(flatten)]
    pub common: S3CommonArgs,
    pub uri: String,
}

#[derive(Debug, Args)]
pub struct SqsCommand {
    #[command(subcommand)]
    pub command: SqsSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum SqsSubcommand {
    #[command(about = "List queues")]
    Ls(SqsLsArgs),
    #[command(about = "Send a message")]
    Send(SqsSendArgs),
    #[command(about = "Receive messages")]
    Receive(SqsReceiveArgs),
    #[command(about = "Purge a queue")]
    Purge(SqsQueueArg),
    #[command(about = "Read queue attributes")]
    Attrs(SqsQueueArg),
    #[command(about = "Resolve queue name to URL")]
    Url(SqsUrlArgs),
}

#[derive(Debug, Args)]
pub struct SqsLsArgs {
    #[arg(long)]
    pub prefix: Option<String>,
}

#[derive(Debug, Args)]
pub struct SqsQueueArg {
    pub queue: String,
}

#[derive(Debug, Args)]
pub struct SqsUrlArgs {
    pub queue_name: String,
}

#[derive(Debug, Args)]
pub struct SqsSendArgs {
    pub queue: String,
    pub body: String,
    #[arg(long)]
    pub delay: Option<i32>,
    #[arg(long = "attribute", value_parser = parse_key_value)]
    pub attribute: Vec<(String, String)>,
    #[arg(long)]
    pub group_id: Option<String>,
    #[arg(long)]
    pub dedup_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct SqsReceiveArgs {
    pub queue: String,
    #[arg(long, env = "AWL_SQS_MAX_MESSAGES", default_value_t = 10)]
    pub max: i32,
    #[arg(long, env = "AWL_SQS_WAIT_SECONDS", default_value_t = 20)]
    pub wait: i32,
    #[arg(long)]
    pub visibility: Option<i32>,
    #[arg(long)]
    pub delete: bool,
    #[arg(short, long)]
    pub follow: bool,
}

macro_rules! stub_command {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Debug, Args)]
        pub struct $name {
            #[command(subcommand)]
            pub command: paste::paste! { [<$name Subcommand>] },
        }

        paste::paste! {
            #[derive(Debug, Subcommand)]
            pub enum [<$name Subcommand>] {
                $($variant(StubArgs),)+
            }
        }
    };
}

#[derive(Debug, Args)]
pub struct StubArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

stub_command!(StsCommand {
    Whoami,
    Assume,
    Decode
});
stub_command!(SsoCommand { Login, Logout, Ls });
stub_command!(EcrCommand {
    Ls,
    Tags,
    Login,
    Retag,
    Cp,
    Scan,
    Digest
});
stub_command!(SsmCommand {
    Get,
    Ls,
    Put,
    Rm,
    Exec
});
stub_command!(SecretsCommand {
    Get,
    Ls,
    Put,
    Rotate,
    Rm
});
stub_command!(LogsCommand {
    Ls,
    Streams,
    Tail,
    Get
});
#[derive(Debug, Args)]
pub struct Ec2Command {
    #[command(subcommand)]
    pub command: Ec2CommandSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum Ec2CommandSubcommand {
    #[command(about = "List instances")]
    Ls(StubArgs),
    #[command(about = "Open a shell through SSM Session Manager")]
    Ssh(StubArgs),
    #[command(about = "Start instances")]
    Start(StubArgs),
    #[command(about = "Stop instances")]
    Stop(StubArgs),
    #[command(about = "Reboot instances")]
    Reboot(StubArgs),
    #[command(about = "Read console output")]
    Console(StubArgs),
    #[command(about = "List or describe AMIs")]
    Ami(StubArgs),
    #[command(about = "Search the embedded EC2 instance type catalog")]
    Types(Ec2TypesArgs),
}

#[derive(Debug, Args)]
pub struct Ec2TypesArgs {
    #[arg(help = "Filter by instance type, family, processor, or display name")]
    pub query: Option<String>,

    #[arg(long, help = "Filter to an architecture, such as x86_64 or arm64")]
    pub arch: Option<String>,

    #[arg(long, help = "Filter by family text, such as general or gpu")]
    pub family: Option<String>,

    #[arg(long, help = "Only show current-generation instance types")]
    pub current: bool,

    #[arg(long, help = "Only show instance types with GPUs")]
    pub gpu: bool,

    #[arg(long, help = "Minimum vCPU count")]
    pub min_vcpu: Option<u32>,

    #[arg(long, help = "Maximum vCPU count")]
    pub max_vcpu: Option<u32>,

    #[arg(long, help = "Minimum memory in GiB")]
    pub min_memory: Option<f64>,

    #[arg(long, help = "Maximum memory in GiB")]
    pub max_memory: Option<f64>,

    #[arg(
        long,
        value_enum,
        default_value_t = Ec2TypesSort::Type,
        help = "Sort output"
    )]
    pub sort: Ec2TypesSort,

    #[arg(long, help = "Limit the number of rows returned")]
    pub limit: Option<usize>,

    #[arg(long, help = "Include processor, EBS, VPC, and GPU detail columns")]
    pub wide: bool,

    #[arg(
        long,
        help = "Region used for the Linux on-demand price column; defaults to --region, AWS_REGION, AWS_DEFAULT_REGION, then us-east-1"
    )]
    pub price_region: Option<String>,

    #[arg(long, help = "Do not include a selected-region price column")]
    pub no_price: bool,

    #[arg(
        long,
        help = "Include all embedded Linux on-demand prices in each output row"
    )]
    pub all_prices: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Ec2TypesSort {
    Type,
    Vcpu,
    Memory,
    Price,
}
stub_command!(LambdaCommand { Ls, Invoke, Logs });
stub_command!(DdbCommand {
    Ls,
    Desc,
    Get,
    Query,
    Scan,
    Put,
    Rm
});
stub_command!(Route53Command {
    Zones,
    Ls,
    Get,
    Set
});
stub_command!(KmsCommand {
    Ls,
    Encrypt,
    Decrypt,
    GenerateDataKey
});

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

pub fn filename(path: &std::path::Path) -> Option<&OsStr> {
    path.file_name().filter(|name| !name.is_empty())
}
