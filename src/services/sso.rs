use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::cli::{Globals, SsoCommand, SsoCommandSubcommand, StubArgs};
use crate::error::{AwlError, Result};
use crate::output::{self, Output};
use crate::services::common;

pub async fn run(globals: &Globals, command: &SsoCommand) -> Result<()> {
    match &command.command {
        SsoCommandSubcommand::Login(args) => login(globals, args),
        SsoCommandSubcommand::Logout(args) => logout(globals, args),
        SsoCommandSubcommand::Ls(_) => ls(globals),
    }
}

fn login(globals: &Globals, args: &StubArgs) -> Result<()> {
    let profile = profile_name(globals, args);
    let mut command = aws_command(globals, &profile);
    command.args(["sso", "login"]);
    if common::has_flag(&args.args, "--no-browser")
        || std::env::var("AWL_SSO_NO_BROWSER").is_ok_and(|value| is_true(&value))
    {
        command.arg("--no-browser");
    }
    let status = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        return Err(AwlError::Authentication {
            message: format!("aws sso login failed with status {status}"),
        });
    }
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "profile": profile,
            "logged_in": true,
        }))?,
    )
}

fn logout(globals: &Globals, args: &StubArgs) -> Result<()> {
    let profile = profile_name(globals, args);
    let status = aws_command(globals, &profile)
        .args(["sso", "logout"])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        return Err(AwlError::Authentication {
            message: format!("aws sso logout failed with status {status}"),
        });
    }
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "profile": profile,
            "logged_out": true,
        }))?,
    )
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
        rows.push(serde_json::json!({
            "cache_file": entry.file_name().to_string_lossy(),
            "start_url": value.get("startUrl").and_then(|value| value.as_str()),
            "region": value.get("region").and_then(|value| value.as_str()),
            "expires_at": value.get("expiresAt").and_then(|value| value.as_str()),
            "client_id": value.get("clientId").and_then(|value| value.as_str()),
        }));
    }
    output::emit(globals, Output::Many(rows))
}

fn aws_command(globals: &Globals, profile: &str) -> Command {
    let mut command = Command::new("aws");
    if !profile.is_empty() {
        command.args(["--profile", profile]);
    }
    if let Some(region) = &globals.region {
        command.args(["--region", region]);
    }
    command
}

fn profile_name(globals: &Globals, args: &StubArgs) -> String {
    common::option_value(&args.args, "--profile")
        .or_else(|| globals.profile.clone())
        .or_else(|| std::env::var("AWS_PROFILE").ok())
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

fn is_true(value: &str) -> bool {
    matches!(value, "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON")
}
