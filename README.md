# awl

[![uses nix](https://img.shields.io/badge/uses-nix-%237EBAE4)](https://nixos.org/)
[![license](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)

`awl` is a small/sharp Rust CLI for the AWS operations that come up constantly in day-to-day work. The intent is to be a sharp, minimal set of operational functions, not a full-featured AWS CLI replacement.

The project is early v0.x software. Command names and flags can still change between releases, and structured output schemas are not locked until v1.0. Pin a version in scripts.

## Install

Download a binary from the [GitHub release page](https://github.com/jpetrucciani/awl/releases):

- `awl-linux-amd64`
- `awl-linux-aarch64`
- `awl-windows-amd64.exe`
- `awl-macos-aarch64`

Linux release binaries are statically linked with musl. macOS and Windows assets are single native binaries. Direct Unix downloads need an executable bit:

```sh
chmod +x awl-linux-amd64
./awl-linux-amd64 --help
```

To build from source with nix: `nix-build -A packages.x86_64-unknown-linux-musl`.

A few commands wrap other tools, which must be on `PATH`:

| Command | Needs |
|---|---|
| `awl sso login` / `logout` | AWS CLI v2 (`aws`) |
| `awl ec2 ssh` | `session-manager-plugin` |
| `awl ecr login` | `docker` (or use `--print-password` with another tool) |

## Usage

```sh
awl --help
awl sts whoami
eval "$(awl sts assume arn:aws:iam::123456789012:role/admin)"
awl s3 ls s3://my-bucket/logs/
awl s3 sync ./site s3://my-bucket/site --delete --dry-run
awl sqs receive my-queue --follow --delete
awl logs tail /aws/lambda/my-function --since 1h --follow
awl ssm exec --prefix /app/prod -- ./server
awl ec2 types t4g --current --sort price
```

Every command has `--help` with its flags and examples. Shell completions include every flag:

```sh
awl completions zsh > "${fpath[1]}/_awl"   # also bash, fish, elvish, powershell
```

## Recipes

Piped output is JSONL, `--output plain --field <name>` pulls out a single value, and exit codes mean something, so `awl` composes well with `jq`, `xargs`, and plain shell.

**Credentials and safety rails**

```sh
# refuse to run against the wrong account
[ "$(awl sts whoami --output plain --field account)" = 123456789012 ] || { echo "wrong account" >&2; exit 1; }

# assume a role for one subshell, or for a single awl command
( eval "$(awl sts assume arn:aws:iam::123456789012:role/deploy)"; terraform plan )
awl --role-arn arn:aws:iam::123456789012:role/readonly s3 ls
```

**Config as env vars** (chamber-style): every parameter under the prefix becomes an env var, so `/app/prod/db/password` is `DB_PASSWORD`.

```sh
awl ssm exec --prefix /app/prod -- ./server
awl ssm exec --prefix /app/dev -- docker compose up
```

**S3**

```sh
# preview, then mirror a directory (only new or changed files transfer)
awl s3 sync ./backups s3://bucket/backups --delete --dry-run
awl s3 sync ./backups s3://bucket/backups --delete

# a link that works for a week
awl s3 presign s3://reports/q3.pdf --expires 7d --output plain --field url

# ten biggest objects under a prefix
awl s3 ls s3://bucket/logs/ -r | jq -s 'sort_by(-.size) | .[:10] | .[] | {key, size}'

# exit code 5 means "not found", so polling for a marker is a case statement
awl s3 head s3://bucket/markers/ready >/dev/null 2>&1
case $? in 0) echo ready ;; 5) echo "not yet" ;; *) exit 1 ;; esac
```

**Queues**: received batches are printed before they are deleted, so draining a dead-letter queue to a file is safe to interrupt.

```sh
awl sqs receive orders-dlq --follow --delete >> orders-dlq.jsonl

# replay it later; base64 keeps multi-line bodies intact
jq -r '.body | @base64' orders-dlq.jsonl | while read -r b; do
  echo "$b" | base64 -d | awl -q sqs send orders -
done
```

**Logs and functions**

```sh
awl logs tail /aws/lambda/checkout --since 30m --filter ERROR | jq -r .message
awl lambda logs checkout --follow

# CI smoke test: jq -e fails the step if the function errored
awl lambda invoke healthcheck '{"deep": true}' | jq -e '.function_error == null'
```

**Instances and DNS**

```sh
# stop every running dev-* instance (drop --dry-run to do it)
awl ec2 ls | jq -r 'select(.state == "running" and ((.name // "") | startswith("dev-"))) | .instance_id' \
  | xargs -r awl ec2 stop --dry-run

# TSV for awk: id and name of running instances
awl ec2 ls --output tsv | awk -F'\t' '$3 == "running" { print $1, $2 }'

# cheapest current-gen arm64 type with 16+ GiB, from the embedded catalog (no AWS call)
awl ec2 types --arch arm64 --min-memory 16 --current --sort price --limit 5

# one-line dynamic DNS
awl route53 set example.com home.example.com A "$(curl -fsS https://checkip.amazonaws.com)" --ttl 60
```

**Images and encryption**

```sh
# promote a build without pulling or pushing layers, then pin the digest
awl ecr retag api "sha-$(git rev-parse --short HEAD)" prod
awl ecr digest api:prod --output plain --field digest

# keep a small secrets file (KMS encrypts up to 4 KB) encrypted in the repo
awl kms encrypt - --key-id alias/app --output plain --field ciphertext < .env > .env.kms
awl kms decrypt - --output plain --field plaintext < .env.kms > .env
```

**Containers and CI**: it is one static file.

```sh
curl -fsSL -o /usr/local/bin/awl https://github.com/jpetrucciani/awl/releases/latest/download/awl-linux-amd64
chmod +x /usr/local/bin/awl
```

### Credentials and endpoints

`awl` uses the official AWS SDK credential, region, and config chain, so profiles, SSO, web identity, retries, and endpoint overrides behave as they do in other AWS tools.

- `--profile` / `AWS_PROFILE`, `--region` / `AWS_REGION`
- `--endpoint-url` applies to every service. Without it, the SDK reads `AWS_ENDPOINT_URL_<SERVICE>` and then `AWS_ENDPOINT_URL`.
- `--role-arn` / `AWL_ROLE_ARN` assumes a role before the command runs. It deliberately does not read `AWS_ROLE_ARN`, which web identity setups such as EKS IRSA use for the base credentials.
- `--retry-mode` / `AWS_RETRY_MODE` and `--max-attempts` / `AWS_MAX_ATTEMPTS`

### Output

Output auto-detects stdout: a table on a TTY, JSONL when piped. Force a format with `--output` (`table`, `json`, `jsonl`, `toml`, `yaml`, `tsv`, `plain`) or `AWL_OUTPUT`. `--field <name>` selects one field from single-record output:

```sh
account=$(awl sts whoami --output plain --field account)
```

`--quiet` suppresses the acknowledgement that mutating commands print; read commands always print their results.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success (including EC2 `--dry-run` checks that would succeed) |
| 1 | Other error |
| 2 | Usage error |
| 3 | Authentication (missing or expired credentials) |
| 4 | Authorization (AccessDenied) |
| 5 | Not found |
| 7 | Throttled after retries |
| 8 | Network or endpoint error |
| 9 | Missing external tool |

### S3 notes

- `s3 sync`, `s3 rm -r`, and `s3 du` treat prefixes as directories: `s3://bucket/logs` means `logs/` and never touches `logs-archive/`.
- `s3 sync` transfers a file when it is missing at the destination, its size differs, or the source is newer. `--delete` is skipped if any transfer fails. `--dry-run` lists every planned upload, download, copy, and delete.
- Files larger than `--part-size` (default 8 MiB) upload in parts; copies above 5 GiB use multipart copy.
- Some S3-compatible services reject the SDK's default streaming checksums. Set `AWS_REQUEST_CHECKSUM_CALCULATION=when_required` for those, along with `--path-style` / `AWL_S3_PATH_STYLE=true` where needed.

## Development

The nix shell (`direnv allow` or `nix-shell`) has the toolchain and helper scripts:

```sh
quality              # fmt check, clippy, all tests
nix-build -A checks  # the same checks CI runs, as cached nix builds
update_lock          # refresh the awl-cli entry in Cargo.lock after a version bump
```

Release builds use [crane](https://github.com/ipetkov/crane), so dependencies build once per target as their own derivation (`nix-build -A packages.<target>`). `release_artifacts [version] [targets...]` builds release assets into `dist/`.

### Local S3/SQS harness

The shell wraps `gofakes3` and `goaws` for local testing:

```sh
local_aws start
eval "$(local_aws env)"
local_aws stop
local_aws run -- cargo test --test local_s3_sqs -- --test-threads=1
```

`goaws` only speaks the SQS Query protocol, which current SDKs no longer use, so `local_aws env` sets `AWL_SQS_QUERY_PROTOCOL=true` to switch `awl sqs` to a small unsigned Query client. Leave it unset for real SQS.

### EC2 type catalog

`awl ec2 types` reads a compact generated snapshot at `data/ec2_instance_types.json`, built from the Vantage `instances.vantage.sh` dataset. It keeps basic instance specs and Linux on-demand prices by region, not the full reserved/spot/Windows pricing matrix. Refresh it with:

```sh
refresh_ec2_types
```
