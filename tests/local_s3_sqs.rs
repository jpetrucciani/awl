use std::env;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

struct ChildGuard {
    child: Child,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test]
async fn local_s3_and_sqs_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let Some(gofakes3) = find_bin("GOFAKES3_BIN", "gofakes3") else {
        eprintln!("skipping local S3/SQS test: gofakes3 not found");
        return Ok(());
    };
    let Some(goaws) = find_bin("GOAWS_BIN", "goaws") else {
        eprintln!("skipping local S3/SQS test: goaws not found");
        return Ok(());
    };

    let temp = TempDir::new()?;
    let s3_addr = SocketAddr::from(([127, 0, 0, 1], 19000));
    let sqs_addr = SocketAddr::from(([127, 0, 0, 1], 4100));

    let _s3 = start_gofakes3(&gofakes3, temp.path(), s3_addr)?;
    let _sqs = start_goaws(&goaws, temp.path())?;
    wait_for_port(s3_addr)?;
    wait_for_port(sqs_addr)?;

    let s3_endpoint = format!("http://{s3_addr}");
    let sqs_endpoint = format!("http://{sqs_addr}");
    let input = temp.path().join("input.txt");
    let output = temp.path().join("output.txt");
    std::fs::write(&input, "hello from awl\n")?;

    awl()
        .envs(local_env(&s3_endpoint, &sqs_endpoint))
        .args([
            "--output",
            "json",
            "s3",
            "put",
            input.to_str().ok_or("input path is not utf-8")?,
            "s3://awl-test/hello.txt",
            "--path-style",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"action\": \"put\""));

    awl()
        .envs(local_env(&s3_endpoint, &sqs_endpoint))
        .args([
            "--output",
            "jsonl",
            "s3",
            "ls",
            "s3://awl-test/",
            "--recursive",
            "--path-style",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello.txt"));

    awl()
        .envs(local_env(&s3_endpoint, &sqs_endpoint))
        .args([
            "s3",
            "get",
            "s3://awl-test/hello.txt",
            output.to_str().ok_or("output path is not utf-8")?,
            "--path-style",
        ])
        .assert()
        .success();
    assert_eq!(std::fs::read_to_string(&output)?, "hello from awl\n");

    create_queue(&sqs_endpoint, "awl-test").await?;

    awl()
        .envs(local_env(&s3_endpoint, &sqs_endpoint))
        .args(["--output", "json", "sqs", "send", "awl-test", "hello sqs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("message_id"));

    awl()
        .envs(local_env(&s3_endpoint, &sqs_endpoint))
        .args([
            "--output", "json", "sqs", "receive", "awl-test", "--wait", "0", "--delete",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello sqs"));

    Ok(())
}

fn awl() -> Command {
    Command::cargo_bin("awl").expect("test binary exists")
}

fn find_bin(env_var: &str, name: &str) -> Option<PathBuf> {
    env::var_os(env_var).map(PathBuf::from).or_else(|| {
        env::var_os("PATH").and_then(|paths| {
            env::split_paths(&paths)
                .map(|path| path.join(name))
                .find(|path| path.is_file())
        })
    })
}

fn start_gofakes3(
    bin: &Path,
    temp: &Path,
    addr: SocketAddr,
) -> Result<ChildGuard, Box<dyn std::error::Error>> {
    let data = temp.join("gofakes3");
    std::fs::create_dir_all(&data)?;
    let child = Command::new(bin)
        .args([
            "-backend",
            "fs",
            "-fs.path",
            data.to_str().ok_or("gofakes3 path is not utf-8")?,
            "-fs.create",
            "-autobucket",
            "-host",
            &addr.to_string(),
            "-quiet",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(ChildGuard { child })
}

fn start_goaws(bin: &Path, temp: &Path) -> Result<ChildGuard, Box<dyn std::error::Error>> {
    let log = std::fs::File::create(temp.join("goaws.log"))?;
    let child = Command::new(bin)
        .args(["-loglevel", "error"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()?;
    Ok(ChildGuard { child })
}

fn wait_for_port(addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!("timed out waiting for {addr}").into())
}

fn local_env(s3_endpoint: &str, sqs_endpoint: &str) -> Vec<(&'static str, OsString)> {
    vec![
        ("AWS_ACCESS_KEY_ID", OsString::from("local")),
        ("AWS_SECRET_ACCESS_KEY", OsString::from("local")),
        ("AWS_REGION", OsString::from("us-east-1")),
        ("AWS_ENDPOINT_URL_S3", OsString::from(s3_endpoint)),
        ("AWS_ENDPOINT_URL_SQS", OsString::from(sqs_endpoint)),
        ("AWL_S3_PATH_STYLE", OsString::from("true")),
    ]
}

async fn create_queue(endpoint: &str, queue_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = endpoint
        .strip_prefix("http://")
        .ok_or("test SQS endpoint must be http")?;
    let (host, port) = endpoint.rsplit_once(':').ok_or("missing SQS port")?;
    let request = format!(
        "GET /?Action=CreateQueue&QueueName={queue_name}&Version=2012-11-05 HTTP/1.1\r\nHost: {endpoint}\r\nConnection: close\r\n\r\n"
    );
    let mut stream = TcpStream::connect((host, port.parse::<u16>()?))?;
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    if !response.starts_with("HTTP/1.1 200") && !response.starts_with("HTTP/1.0 200") {
        return Err(format!("CreateQueue failed: {response}").into());
    }
    Ok(())
}
