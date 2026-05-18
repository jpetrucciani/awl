# awl

[![build](https://github.com/jpetrucciani/awl/actions/workflows/build.yml/badge.svg)](https://github.com/jpetrucciani/awl/actions/workflows/build.yml)
[![release](https://github.com/jpetrucciani/awl/actions/workflows/release.yml/badge.svg)](https://github.com/jpetrucciani/awl/actions/workflows/release.yml)
[![license](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
[![uses nix](https://img.shields.io/badge/uses-nix-%237EBAE4)](https://nixos.org/)
![rust](https://img.shields.io/badge/Rust-1.95%2B-orange.svg)

`awl` is a small/sharp Rust CLI for the AWS operations that come up constantly in day-to-day work. The intent is to be a sharp, minimal set of operational functions, not a full-featured AWS CLI replacement.

The project is early v0.x software. Command names are usable now, but structured output schemas are not locked until v1.0.

## Install

Download a binary from the GitHub release page:

- `awl-linux-amd64`
- `awl-linux-aarch64`
- `awl-windows-amd64.exe`
- `awl-macos-aarch64`

Linux release binaries are statically linked with musl. macOS and Windows assets are single native binaries. Direct Unix downloads may need an executable bit:
```sh
chmod +x awl-linux-amd64
./awl-linux-amd64 --help
```

## Usage

`awl` follows the official AWS SDK credential and config chain. Profiles, regions, role assumption, retry settings, and endpoint overrides use the same AWS environment variables where AWS defines them.

```sh
awl --help
awl sts whoami
awl s3 ls
awl sqs ls
awl logs tail /aws/lambda/my-function --follow
awl ec2 types t4g --current --sort price
```

Output auto-detects stdout:

- TTY: table output
- pipe: JSONL output

You can force a format with:

```sh
awl --output json sts whoami
awl --output table ec2 types --gpu --current
```

Supported output formats are `table`, `json`, `jsonl`, `toml`, `yaml`, `tsv`,
and `plain`.

## Local S3/SQS Harness

The development shell provides a local test harness around `gofakes3` and
`goaws`:

```sh
local_aws start
local_aws env
local_aws stop
```

`local_aws run -- <command>` starts both services, exports safe local AWS env
vars for the child command, and stops the services afterward.

Useful development checks:

```sh
update_lock
direnv exec . cargo fmt --check
direnv exec . cargo clippy --all --benches --tests --examples --all-features -- -D warnings
direnv exec . cargo test --test local_s3_sqs -- --test-threads=1
```

Run `update_lock` after bumping `[package].version` in `Cargo.toml`; it updates
the local package entry in `Cargo.lock` without rebuilding.

## Embedded EC2 Type Catalog

`awl ec2 types` reads a compact generated snapshot at
`data/ec2_instance_types.json`. Refresh it from the Vantage
`instances.vantage.sh` dataset with:

```sh
refresh_ec2_types
```

The embedded snapshot keeps basic instance specs and Linux on-demand prices by
region. It intentionally drops the full reserved/spot/Windows pricing matrix.
