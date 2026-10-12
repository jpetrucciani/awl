use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::cli::{Globals, SsoCommand, SsoSubcommand};
use crate::error::{AwlError, Result};
use crate::output::{self, Output};

pub async fn run(globals: &Globals, command: &SsoCommand) -> Result<()> {
    match &command.command {
        SsoSubcommand::Login(args) => {
            let mut command = aws_command(globals);
            command.args(["sso", "login"]);
            if args.no_browser {
                command.arg("--no-browser");
            }
            run_aws(command, "login")?;
            output::ack(
                globals,
                Output::one(
                    serde_json::json!({ "profile": profile_name(globals), "logged_in": true }),
                )?,
            )
        }
        SsoSubcommand::Logout => {
            let mut command = aws_command(globals);
            command.args(["sso", "logout"]);
            run_aws(command, "logout")?;
            output::ack(
                globals,
                Output::one(
                    serde_json::json!({ "profile": profile_name(globals), "logged_out": true }),
                )?,
            )
        }
        SsoSubcommand::Ls => ls(globals),
    }
}

fn run_aws(mut command: Command, action: &str) -> Result<()> {
    let status = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => AwlError::MissingDependency {
                message: "`awl sso` needs AWS CLI v2 (`aws`) on PATH".to_owned(),
            },
            _ => AwlError::Io(error),
        })?;
    if !status.success() {
        return Err(AwlError::Authentication {
            message: format!("aws sso {action} failed with status {status}"),
        });
    }
    Ok(())
}

fn ls(globals: &Globals) -> Result<()> {
    let cache = sso_cache_dir()?;
    let mut rows = Vec::new();
    if !cache.exists() {
        return output::emit(globals, Output::Many(rows));
    }
    for entry in std::fs::read_dir(cache)? {
        let entry = entry?;
        if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let contents = std::fs::read_to_string(entry.path())?;
        let value = serde_json::from_str::<serde_json::Value>(&contents)?;
        // Client registration files share the directory but are not sessions.
        let Some(start_url) = value.get("startUrl").and_then(|value| value.as_str()) else {
            continue;
        };
        rows.push(serde_json::json!({
            "start_url": start_url,
            "region": value.get("region").and_then(|value| value.as_str()),
            "expires_at": value.get("expiresAt").and_then(|value| value.as_str()),
            "cache_file": entry.file_name().to_string_lossy(),
        }));
    }
    output::emit(globals, Output::Many(rows))
}

fn aws_command(globals: &Globals) -> Command {
    let mut command = Command::new("aws");
    command.args(["--profile", &profile_name(globals)]);
    if let Some(region) = &globals.region {
        command.args(["--region", region]);
    }
    command
}

fn profile_name(globals: &Globals) -> String {
    globals
        .profile
        .clone()
        .unwrap_or_else(|| "default".to_owned())
}

fn sso_cache_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| AwlError::Usage {
            message: "cannot locate home directory for AWS SSO cache".to_owned(),
        })?;
    Ok(PathBuf::from(home).join(".aws").join("sso").join("cache"))
}
