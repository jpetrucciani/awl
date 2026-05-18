{ pkgs ? import
    (fetchTarball {
      name = "jpetrucciani-2026-05-06";
      url = "https://github.com/jpetrucciani/nix/archive/a437184f4ad2a8686dfc11c96926fa767668b6a8.tar.gz";
      sha256 = "1xja8bsvprcgw6rk5qmvfhapwbx80v1y4rg68whqcdll4ciax1kh";
    })
    { overlays = [ rustOverlay ]; }
, rustOverlay ? import
    (fetchTarball {
      name = "oxalica-2026-05-06";
      url = "https://github.com/oxalica/rust-overlay/archive/adf987c76af8d17b8256d23631bcf203f81e1a63.tar.gz";
      sha256 = "0qr1w3knjchkqqrbnx9sy6mh5sxx2c4qg4dhva7f64g08cxc168i";
    })
}:
let
  name = "awl";
  muslTarget = "x86_64-unknown-linux-musl";
  releaseTargets = [
    muslTarget
    "aarch64-unknown-linux-musl"
    "x86_64-pc-windows-gnu"
  ];

  rust = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default.override {
    extensions = [ "rust-src" "rustc-dev" "rust-analyzer" ];
    targets = [
      muslTarget
      "aarch64-unknown-linux-musl"
      "x86_64-pc-windows-gnu"
    ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
      "aarch64-apple-darwin"
    ];
  });

  mingw = pkgs.pkgsCross.mingwW64;
  # This is a Windows target library, but the Linux cross linker needs it in scope.
  mingwPthreads = mingw.windows.pthreads.overrideAttrs (old: {
    meta = (old.meta or { }) // {
      platforms = pkgs.lib.platforms.all;
    };
  });
  windowsCrossEnv = pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
    CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = "${mingw.stdenv.cc}/bin/x86_64-w64-mingw32-gcc";
    CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS = "-L native=${mingwPthreads}/lib";
  };

  scripts = with pkgs; {
    fmt = writers.writeBashBin "fmt" ''
      set -euo pipefail
      cargo fmt
    '';

    clippy_all = writers.writeBashBin "clippy_all" ''
      set -euo pipefail
      cargo clippy --all --benches --tests --examples --all-features -- -D warnings
    '';

    test_all_features = writers.writeBashBin "test_all_features" ''
      set -euo pipefail
      cargo test --all-features
    '';

    test_no_default_features = writers.writeBashBin "test_no_default_features" ''
      set -euo pipefail
      cargo test --no-default-features
    '';

    refresh_ec2_types = writers.writeBashBin "refresh_ec2_types" ''
      set -euo pipefail
      python3 tools/extract_ec2_types.py "$@"
    '';

    update_lock = writers.writeBashBin "update_lock" ''
      set -euo pipefail
      cargo update -p awl-cli --offline
    '';

    quality = writers.writeBashBin "quality" ''
      set -euo pipefail
      cargo fmt --check
      cargo clippy --all --benches --tests --examples --all-features -- -D warnings
      cargo test --all-features
      cargo test --no-default-features
    '';

    build_static = writers.writeBashBin "build_static" ''
      set -euo pipefail
      export ZIG_LOCAL_CACHE_DIR="''${ZIG_LOCAL_CACHE_DIR:-$PWD/.zig-cache/local}"
      export ZIG_GLOBAL_CACHE_DIR="''${ZIG_GLOBAL_CACHE_DIR:-$PWD/.zig-cache/global}"
      mkdir -p "$ZIG_LOCAL_CACHE_DIR" "$ZIG_GLOBAL_CACHE_DIR"
      cargo zigbuild --release --locked --all-features --target ${muslTarget}
    '';

    release_artifacts = writers.writeBashBin "release_artifacts" ''
      set -euo pipefail

      asset_name() {
        case "$1" in
          x86_64-unknown-linux-musl)
            printf 'awl-linux-amd64\n'
            ;;
          aarch64-unknown-linux-musl)
            printf 'awl-linux-aarch64\n'
            ;;
          x86_64-pc-windows-gnu)
            printf 'awl-windows-amd64.exe\n'
            ;;
          aarch64-apple-darwin)
            printf 'awl-macos-aarch64\n'
            ;;
          *)
            printf 'awl-%s\n' "$1"
            ;;
        esac
      }

      version="''${1:-}"
      export ZIG_LOCAL_CACHE_DIR="''${ZIG_LOCAL_CACHE_DIR:-$PWD/.zig-cache/local}"
      export ZIG_GLOBAL_CACHE_DIR="''${ZIG_GLOBAL_CACHE_DIR:-$PWD/.zig-cache/global}"
      mkdir -p "$ZIG_LOCAL_CACHE_DIR" "$ZIG_GLOBAL_CACHE_DIR"

      if test -z "$version"; then
        version="$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')"
      else
        shift
      fi

      if test "$#" -gt 0; then
        targets="$*"
      elif test "$(uname -s)" = "Darwin"; then
        targets="aarch64-apple-darwin"
      else
        targets="${builtins.concatStringsSep " " releaseTargets}"
      fi
      dist="dist/v$version"
      rm -rf "$dist"
      mkdir -p "$dist"

      for target in $targets; do
        case "$target" in
          *linux-musl|*windows-gnu)
            cargo zigbuild --release --locked --all-features --target "$target"
            ;;
          *)
            cargo build --release --locked --all-features --target "$target"
            ;;
        esac

        binary="target/$target/release/awl"
        if test "$target" = "x86_64-pc-windows-gnu"; then
          binary="$binary.exe"
        fi

        asset="$dist/$(asset_name "$target")"
        cp "$binary" "$asset"
        chmod 755 "$asset" 2>/dev/null || true
      done

      (cd "$dist" && sha256sum awl-* > SHA256SUMS)
      printf '%s\n' "$dist"
    '';

    upload_release = writers.writeBashBin "upload_release" ''
      set -euo pipefail

      tag="''${1:?usage: upload_release <tag> <dist-dir>}"
      dist="''${2:?usage: upload_release <tag> <dist-dir>}"
      gh release upload "$tag" "$dist"/awl-* "$dist"/SHA256SUMS --clobber
    '';

    local_aws = writers.writeBashBin "local_aws" ''
      set -euo pipefail

      state_dir="''${AWL_LOCAL_STATE_DIR:-.awl-local}"
      s3_host="''${AWL_LOCAL_S3_HOST:-127.0.0.1}"
      s3_port="''${AWL_LOCAL_S3_PORT:-9000}"
      sqs_host="''${AWL_LOCAL_SQS_HOST:-127.0.0.1}"
      sqs_port="4100"

      mkdir -p "$state_dir/logs" "$state_dir/gofakes3"

      is_alive() {
        pid_file="$1"
        test -f "$pid_file" && kill -0 "$(cat "$pid_file")" 2>/dev/null
      }

      start_one() {
        name="$1"
        shift
        pid_file="$state_dir/$name.pid"
        if is_alive "$pid_file"; then
          printf '%s already running with pid %s\n' "$name" "$(cat "$pid_file")"
          return
        fi
        "$@" >"$state_dir/logs/$name.log" 2>&1 &
        pid="$!"
        printf '%s\n' "$pid" >"$pid_file"
        printf 'started %s with pid %s\n' "$name" "$pid"
      }

      stop_one() {
        name="$1"
        pid_file="$state_dir/$name.pid"
        if ! is_alive "$pid_file"; then
          rm -f "$pid_file"
          return
        fi
        pid="$(cat "$pid_file")"
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
        rm -f "$pid_file"
        printf 'stopped %s with pid %s\n' "$name" "$pid"
      }

      print_env() {
        cat <<EOF
export AWS_ACCESS_KEY_ID=local
export AWS_SECRET_ACCESS_KEY=local
export AWS_REGION=us-east-1
export AWS_ENDPOINT_URL_S3=http://$s3_host:$s3_port
export AWS_ENDPOINT_URL_SQS=http://$sqs_host:$sqs_port
export AWL_S3_PATH_STYLE=true
EOF
      }

      start_all() {
        start_one gofakes3 \
          gofakes3 \
            -backend fs \
            -fs.path "$state_dir/gofakes3" \
            -fs.create \
            -autobucket \
            -host "$s3_host:$s3_port"
        start_one goaws \
          goaws \
            -loglevel error
      }

      stop_all() {
        stop_one goaws
        stop_one gofakes3
      }

      case "''${1:-}" in
        start)
          start_all
          ;;
        stop)
          stop_all
          ;;
        restart)
          stop_all
          start_all
          ;;
        status)
          for name in gofakes3 goaws; do
            pid_file="$state_dir/$name.pid"
            if is_alive "$pid_file"; then
              printf '%s running with pid %s\n' "$name" "$(cat "$pid_file")"
            else
              printf '%s stopped\n' "$name"
            fi
          done
          ;;
        env)
          print_env
          ;;
        run)
          shift
          if test "''${1:-}" = "--"; then
            shift
          fi
          if test "$#" -eq 0; then
            echo "usage: local_aws run -- <command> [args...]" >&2
            exit 2
          fi
          start_all
          trap stop_all EXIT
          export AWS_ACCESS_KEY_ID=local
          export AWS_SECRET_ACCESS_KEY=local
          export AWS_REGION=us-east-1
          export AWS_ENDPOINT_URL_S3="http://$s3_host:$s3_port"
          export AWS_ENDPOINT_URL_SQS="http://$sqs_host:$sqs_port"
          export AWL_S3_PATH_STYLE=true
          "$@"
          ;;
        *)
          cat >&2 <<EOF
usage: local_aws <start|stop|restart|status|env|run -- command...>

Environment:
  AWL_LOCAL_STATE_DIR  default .awl-local
  AWL_LOCAL_S3_PORT    default 9000
  goaws listens on port 4100.
EOF
          exit 2
          ;;
      esac
    '';
  };

  packages = with pkgs; [
    cargo-zigbuild
    file
    gh
    jfmt
    jq
    python3
    rust
    zig
  ] ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
    mingwPthreads
  ] ++ [

    # aws
    gofakes3
    goaws
  ] ++ builtins.attrValues scripts;

  shell = pkgs.mkShellNoCC ({
    inherit name packages;
    CC = "${pkgs.stdenv.cc}/bin/cc";
    CXX = "${pkgs.stdenv.cc}/bin/c++";
    AR = "${pkgs.binutils}/bin/ar";
    RUST_SRC_PATH = "${rust}/lib/rustlib/src/rust/library";
  } // windowsCrossEnv);
in
(shell.overrideAttrs (_: { inherit name; })) // {
  bashInteractive = pkgs.bashInteractive;
  inherit scripts;
}
