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

  rustTargets = releaseTargets ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
    "aarch64-apple-darwin"
  ];
  # Lean toolchain for nix builds; the dev shell adds editor tooling on top.
  rustBuild = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default.override {
    targets = rustTargets;
  });
  rust = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default.override {
    extensions = [ "rust-src" "rust-analyzer" ];
    targets = rustTargets;
  });

  crane = import
    (fetchTarball {
      name = "crane-v0.24.0";
      url = "https://github.com/ipetkov/crane/archive/refs/tags/v0.24.0.tar.gz";
      sha256 = "06y3dvs9ms4c70r2ia50k77650yrihc09jibqbqrzly23dn42s4m";
    })
    { inherit pkgs; };
  craneLib = crane.overrideToolchain (_: rustBuild);

  srcFiles = pkgs.lib.fileset.unions [
    (craneLib.fileset.commonCargoSources ./.)
    ./data
  ];
  src = pkgs.lib.fileset.toSource { root = ./.; fileset = srcFiles; };

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

  # Release version bumps must not invalidate dependency builds, so the deps
  # derivation sees a fixed version for awl itself in its name and Cargo.lock.
  awlVersion = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
  depsCargoLock = builtins.toFile "Cargo.lock" (builtins.replaceStrings
    [ "name = \"awl-cli\"\nversion = \"${awlVersion}\"" ]
    [ "name = \"awl-cli\"\nversion = \"0.0.0\"" ]
    (builtins.readFile ./Cargo.lock));
  depsDummySrc = craneLib.mkDummySrc {
    src = pkgs.lib.fileset.toSource {
      root = ./.;
      fileset = pkgs.lib.fileset.difference srcFiles ./Cargo.lock;
    };
    cargoLock = depsCargoLock;
    cleanCargoTomlFilter = path:
      path != [ "package" "version" ] && craneLib.filters.cargoTomlDefault path;
  };
  buildDeps = attrs: craneLib.buildDepsOnly (attrs // {
    version = "0";
    dummySrc = depsDummySrc;
  });

  # Cross builds go through cargo-zigbuild, which also cross-compiles the C in
  # aws-lc-sys. Dependencies build once per target in their own derivation, so
  # source-only changes reuse them from the store or a binary cache.
  crossPackage = target:
    let
      common = {
        inherit src;
        pname = name;
        strictDeps = true;
        doCheck = false;
        nativeBuildInputs = [ pkgs.cargo-zigbuild pkgs.zig ];
        cargoExtraArgs = "--locked --target ${target}";
        cargoCheckCommand = "true";
        cargoBuildCommand = "cargo zigbuild --profile release";
        preBuild = ''
          export ZIG_GLOBAL_CACHE_DIR="$TMPDIR/zig-global"
          export ZIG_LOCAL_CACHE_DIR="$TMPDIR/zig-local"
          export XDG_CACHE_HOME="$TMPDIR/cache"
        '';
        # Host strip/patchelf must not touch foreign or static binaries.
        dontStrip = true;
        dontPatchELF = true;
      } // pkgs.lib.optionalAttrs (target == "x86_64-pc-windows-gnu") windowsCrossEnv;
    in
    craneLib.buildPackage (common // {
      cargoArtifacts = buildDeps common;
    });

  packages = pkgs.lib.optionalAttrs pkgs.stdenv.isLinux
    (pkgs.lib.genAttrs releaseTargets crossPackage);

  # CI checks: one shared dev-profile dependency build for clippy and tests.
  checkCommon = {
    inherit src;
    pname = name;
    strictDeps = true;
    CARGO_PROFILE = "dev";
  };
  checkArtifacts = buildDeps checkCommon;
  checks = {
    fmt = craneLib.cargoFmt { inherit src; pname = name; };
    clippy = craneLib.cargoClippy (checkCommon // {
      cargoArtifacts = checkArtifacts;
      cargoClippyExtraArgs = "--all --benches --tests --examples --all-features -- -D warnings";
    });
    test = craneLib.cargoTest (checkCommon // {
      cargoArtifacts = checkArtifacts;
      nativeCheckInputs = [ pkgs.gofakes3 pkgs.goaws ];
      # The sandbox has no system CA store, which debug-build rustls asserts on.
      SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
      cargoTestExtraArgs = "-- --test-threads=1";
    });
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
      cargo test -- --test-threads=1
    '';

    build_static = writers.writeBashBin "build_static" ''
      set -euo pipefail
      nix-build --no-out-link -A packages.${muslTarget} "$PWD/default.nix"
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
            # crane package: dependency builds are cached as their own derivation
            binary="$(nix-build --no-out-link -A "packages.$target" "$PWD/default.nix")/bin/awl"
            ;;
          *)
            cargo build --release --locked --target "$target"
            binary="target/$target/release/awl"
            ;;
        esac
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
      # goaws always listens on 4100.
      sqs_host="127.0.0.1"
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
export AWL_SQS_QUERY_PROTOCOL=true
export AWS_REQUEST_CHECKSUM_CALCULATION=when_required
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
        wait_for_port "$s3_host" "$s3_port"
        wait_for_port "$sqs_host" "$sqs_port"
      }

      wait_for_port() {
        for _ in $(seq 1 50); do
          if (exec 3<>"/dev/tcp/$1/$2") 2>/dev/null; then
            return
          fi
          sleep 0.1
        done
        printf 'timed out waiting for %s:%s\n' "$1" "$2" >&2
        exit 1
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
          eval "$(print_env)"
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

  shellPackages = with pkgs; [
    cargo-zigbuild
    file
    gh
    jfmt
    jq
    python3
    rust
    zig
  ] ++ [

    # aws
    gofakes3
    goaws
  ] ++ builtins.attrValues scripts;

  shell = pkgs.mkShellNoCC ({
    inherit name;
    packages = shellPackages;
    CC = "${pkgs.stdenv.cc}/bin/cc";
    CXX = "${pkgs.stdenv.cc}/bin/c++";
    AR = "${pkgs.binutils}/bin/ar";
    RUST_SRC_PATH = "${rust}/lib/rustlib/src/rust/library";
  } // windowsCrossEnv);
in
(shell.overrideAttrs (_: { inherit name; })) // {
  bashInteractive = pkgs.bashInteractive;
  inherit scripts packages checks;
}
