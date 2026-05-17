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
