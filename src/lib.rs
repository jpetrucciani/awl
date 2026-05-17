pub mod cli;
pub mod client;
pub mod error;
pub mod output;
pub mod services;

use clap::Parser;
use cli::{Cli, Command};
use error::Result;

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(cli.globals.log_filter())
        .with_writer(std::io::stderr)
        .without_time()
        .try_init()
        .ok();

    match &cli.command {
        Command::Completions(args) => args.run(),
        Command::S3(command) => services::s3::run(&cli.globals, command).await,
        Command::Sqs(command) => services::sqs::run(&cli.globals, command).await,
        Command::Sts(command) => services::sts::run(&cli.globals, command).await,
        Command::Sso(command) => services::sso::run(&cli.globals, command).await,
        Command::Ecr(command) => services::ecr::run(&cli.globals, command).await,
        Command::Ssm(command) => services::ssm::run(&cli.globals, command).await,
        Command::Secrets(command) => services::secrets::run(&cli.globals, command).await,
        Command::Logs(command) => services::logs::run(&cli.globals, command).await,
        Command::Ec2(command) => services::ec2::run(&cli.globals, command).await,
        Command::Lambda(command) => services::lambda::run(&cli.globals, command).await,
        Command::Ddb(command) => services::ddb::run(&cli.globals, command).await,
        Command::Route53(command) => services::route53::run(&cli.globals, command).await,
        Command::Kms(command) => services::kms::run(&cli.globals, command).await,
    }
}
