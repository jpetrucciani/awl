use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::process::Command;

#[test]
fn top_level_help_documents_global_flags() {
    Command::cargo_bin("awl")
        .expect("test binary exists")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("AWS profile name"))
        .stdout(predicate::str::contains("Output format"))
        .stdout(predicate::str::contains("Maximum total AWS attempts"));
}

#[test]
fn bash_completions_include_service_commands() {
    Command::cargo_bin("awl")
        .expect("test binary exists")
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("_awl()"))
        .stdout(predicate::str::contains("route53"))
        .stdout(predicate::str::contains("sqs"));
}

#[test]
fn ec2_types_help_documents_embedded_catalog_filters() {
    Command::cargo_bin("awl")
        .expect("test binary exists")
        .args(["ec2", "types", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "embedded EC2 instance type catalog",
        ))
        .stdout(predicate::str::contains("--min-vcpu"))
        .stdout(predicate::str::contains("--price-region"));
}

#[test]
fn ec2_types_reads_embedded_catalog_without_aws_credentials() {
    let output = Command::cargo_bin("awl")
        .expect("test binary exists")
        .args([
            "--output",
            "json",
            "ec2",
            "types",
            "t4g.nano",
            "--current",
            "--no-price",
        ])
        .output()
        .expect("command runs");

    assert!(output.status.success());
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&output.stdout).expect("valid JSON output");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["instance_type"], "t4g.nano");
    assert_eq!(rows[0]["arch"], "arm64");
    assert_eq!(rows[0]["vcpu"], 2);
    assert!(rows[0].get("linux_ondemand_usd_per_hour").is_none());
}

#[test]
fn sts_without_region_does_not_probe_imds_for_region() {
    let home = tempfile::tempdir().expect("temporary home");
    let config = home.path().join("config");
    let credentials = home.path().join("credentials");
    std::fs::write(&config, "").expect("empty config");
    std::fs::write(&credentials, "").expect("empty credentials");

    let output = Command::cargo_bin("awl")
        .expect("test binary exists")
        .env("AWS_ACCESS_KEY_ID", "local")
        .env("AWS_SECRET_ACCESS_KEY", "local")
        .env("AWS_CONFIG_FILE", config)
        .env("AWS_SHARED_CREDENTIALS_FILE", credentials)
        .env("AWS_EC2_METADATA_DISABLED", "false")
        .env("AWS_ENDPOINT_URL_STS", "http://127.0.0.1:1")
        .env_remove("AWS_DEFAULT_REGION")
        .env_remove("AWS_PROFILE")
        .env_remove("AWS_REGION")
        .env_remove("AWS_SESSION_TOKEN")
        .args(["--max-attempts", "1", "sts", "whoami"])
        .output()
        .expect("command runs");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("aws_config::imds::region")
            && !stderr.contains("failed to load region from IMDS"),
        "{stderr}"
    );
}
