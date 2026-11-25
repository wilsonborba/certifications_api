#!/usr/bin/env bash
set -euo pipefail

SERVICE_NAME="certifications-api"
SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
PROJECT_DIR=$(cd "$SCRIPT_DIR/.." && pwd)
DEPLOY_DIR="$PROJECT_DIR/var/deploy"
MODE=${1:---prepare}

if (( $# > 1 )); then
  printf 'Usage: %s [--prepare|--check|--activate|--dry-run]\n' "${0##*/}" >&2
  exit 2
fi
case "$MODE" in
  --prepare|--check|--activate|--dry-run) ;;
  *) printf 'Usage: %s [--prepare|--check|--activate|--dry-run]\n' "${0##*/}" >&2; exit 2 ;;
esac

source_hash() {
  # Cargo compilation inputs: manifests/config, Rust sources, and the SQL
  # fixtures included by lib/dal/mod.rs. Hash from the checkout root so the
  # digest is independent of the caller's working directory. `-z` keeps paths
  # with whitespace/newlines intact; pipefail propagates enumeration/read errors.
  (
    cd "$PROJECT_DIR"
    git ls-files -co --exclude-standard -z -- \
      Cargo.toml Cargo.lock build.rs .cargo assets/fonts lib migrations tests/sqlite scripts/install_service.sh |
      LC_ALL=C sort -z | xargs -0 -r sha256sum
  ) | sha256sum | cut -d ' ' -f1
}

validate_inputs() {
  local missing=0
  for path in Cargo.toml Cargo.lock lib/main.rs scripts/install_service.sh; do
    if [[ ! -f "$PROJECT_DIR/$path" ]]; then
      printf 'Missing Rust release input: %s\n' "$path" >&2
      missing=1
    fi
  done
  return "$missing"
}

validate_prepared() {
  [[ ! -L "$DEPLOY_DIR" && -d "$DEPLOY_DIR" ]] || {
    printf 'Prepared package directory is missing or is a symbolic link.\n' >&2; return 1;
  }
  [[ "$(stat -c '%a' "$DEPLOY_DIR")" == 755 ]] || {
    printf 'Prepared package directory permissions are invalid.\n' >&2; return 1;
  }
  for path in certifications-api "$SERVICE_NAME.service" source.sha256 artifacts.sha256; do
    [[ ! -L "$DEPLOY_DIR/$path" && -f "$DEPLOY_DIR/$path" ]] || {
      printf 'Prepared package contains a missing or symbolic-link file: %s\n' "$path" >&2; return 1;
    }
  done
  [[ -f "$DEPLOY_DIR/certifications-api" ]] || {
    printf 'No prepared Rust binary. Run install_service.sh --prepare first.\n' >&2; return 1;
  }
  [[ -x "$DEPLOY_DIR/certifications-api" ]] || {
    printf 'Prepared Rust binary is not executable. Run install_service.sh --prepare again.\n' >&2; return 1;
  }
  [[ -f "$DEPLOY_DIR/certifications-api.service" && -f "$DEPLOY_DIR/source.sha256" && -f "$DEPLOY_DIR/artifacts.sha256" ]] || {
    printf 'Prepared Rust package is incomplete. Run install_service.sh --prepare again.\n' >&2; return 1;
  }
  local current_source_hash
  current_source_hash=$(source_hash) || {
    printf 'Could not hash all Rust release inputs. Check the checkout and file readability.\n' >&2; return 1;
  }
  [[ "$current_source_hash" == "$(<"$DEPLOY_DIR/source.sha256")" ]] || {
    printf 'Rust sources changed since prepare. Run install_service.sh --prepare again.\n' >&2; return 1;
  }
  [[ "$(stat -c '%a' "$DEPLOY_DIR/certifications-api")" == 755 &&
     "$(stat -c '%a' "$DEPLOY_DIR/$SERVICE_NAME.service")" == 644 &&
     "$(stat -c '%a' "$DEPLOY_DIR/source.sha256")" == 644 &&
     "$(stat -c '%a' "$DEPLOY_DIR/artifacts.sha256")" == 644 ]] || {
    printf 'Prepared package file permissions are invalid. Run install_service.sh --prepare again.\n' >&2; return 1;
  }
  local expected_artifact_manifest actual_artifact_manifest
  expected_artifact_manifest=$(cd "$DEPLOY_DIR" && sha256sum certifications-api "$SERVICE_NAME.service") || {
    printf 'Could not hash every required prepared artifact.\n' >&2; return 1;
  }
  actual_artifact_manifest=$(<"$DEPLOY_DIR/artifacts.sha256") || {
    printf 'Could not read the prepared artifact manifest.\n' >&2; return 1;
  }
  [[ "$actual_artifact_manifest" == "$expected_artifact_manifest" ]] || {
    printf 'Prepared artifact manifest must cover exactly the Rust binary and service unit. Run install_service.sh --prepare again.\n' >&2; return 1;
  }
  grep -Fxq "WorkingDirectory=$PROJECT_DIR" "$DEPLOY_DIR/$SERVICE_NAME.service" || {
    printf 'Prepared service unit belongs to another checkout. Prepare in this checkout.\n' >&2; return 1;
  }
  grep -Fxq "ExecStart=$DEPLOY_DIR/certifications-api" "$DEPLOY_DIR/$SERVICE_NAME.service" || {
    printf 'Prepared service unit does not launch the Rust binary. Run install_service.sh --prepare again.\n' >&2; return 1;
  }
}

# This branch is deliberately before locks, mkdir, cargo, install, or service
# commands. It only reads the checkout and reports the package/activation plan.
if [[ "$MODE" == --dry-run ]]; then
  validate_inputs
  source_hash >/dev/null || {
    printf 'Could not hash all Rust release inputs. Check the checkout and file readability.\n' >&2; exit 1;
  }
  if [[ -e "$DEPLOY_DIR" || -L "$DEPLOY_DIR" ]]; then
    validate_prepared
    printf 'Dry run: prepared Rust binary and unit passed read-only validation.\n'
    printf 'Plan: install %s and %s; reload systemd; enable and start %s.\n' \
      "$DEPLOY_DIR/certifications-api" "/etc/systemd/system/$SERVICE_NAME.service" "$SERVICE_NAME"
  else
    printf 'Dry run: no prepared package exists; Rust release inputs are present.\n'
    printf 'Plan: cargo build --release; prepare %s/certifications-api and its service unit.\n' "$DEPLOY_DIR"
    printf 'Plan: activation would install the binary and unit, reload systemd, enable and start %s.\n' "$SERVICE_NAME"
  fi
  printf 'Dry run complete. No files, services, databases, caches, or FSM objects were changed.\n'
  exit 0
fi

if [[ "$MODE" == --activate && "$(id -u)" != 0 ]]; then
  printf 'Administrator rights are required to activate %s.\n' "$SERVICE_NAME" >&2
  exit 1
fi

if [[ "$MODE" == --prepare ]]; then
  validate_inputs
  mkdir -p "$PROJECT_DIR/var"
  exec 9>"$PROJECT_DIR/var/deploy.lock"
  flock -n 9 || { printf 'Another deployment is in progress.\n' >&2; exit 1; }
  before=$(source_hash) || {
    printf 'Could not hash all Rust release inputs before building.\n' >&2; exit 1;
  }
  (cd "$PROJECT_DIR" && cargo build --release --locked --bin certifications-api)
  after=$(source_hash) || {
    printf 'Could not hash all Rust release inputs after building.\n' >&2; exit 1;
  }
  [[ "$before" == "$after" ]] || {
    printf 'Rust source changed during the build. Prepare again.\n' >&2; exit 1;
  }
  target_dir=$(cd "$PROJECT_DIR" && cargo metadata --no-deps --locked --format-version 1 --manifest-path Cargo.toml |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
  BINARY="$target_dir/release/certifications-api"
  [[ -s "$BINARY" && -x "$BINARY" ]] || {
    printf 'Cargo did not produce an executable Rust binary at %s.\n' "$BINARY" >&2; exit 1;
  }
  stage=$(mktemp -d "$PROJECT_DIR/var/deploy.XXXXXX")
  trap 'rm -rf "$stage"' EXIT
  install -m 755 "$BINARY" "$stage/certifications-api"
  cat > "$stage/$SERVICE_NAME.service" <<UNIT
[Unit]
Description=Certifications Rust API
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=$PROJECT_DIR
EnvironmentFile=$PROJECT_DIR/.env
ExecStart=$DEPLOY_DIR/certifications-api
Restart=on-failure
RestartSec=5
NoNewPrivileges=yes
PrivateTmp=yes
UMask=027

[Install]
WantedBy=multi-user.target
UNIT
  source_hash > "$stage/source.sha256"
  (cd "$stage" && sha256sum certifications-api "$SERVICE_NAME.service" > artifacts.sha256)
  chmod 644 "$stage/$SERVICE_NAME.service" "$stage/source.sha256" "$stage/artifacts.sha256"
  chmod 755 "$stage"
  rm -rf "$DEPLOY_DIR"
  mv "$stage" "$DEPLOY_DIR"
  trap - EXIT
  printf 'Rust binary and service unit prepared in var/deploy. No service changed.\n'
  exit 0
fi

validate_prepared
if [[ "$MODE" == --check ]]; then
  printf 'Prepared Rust package verified. No service changed.\n'
  exit 0
fi

# --activate is the only mode that installs files or changes systemd state.
install -D -m 755 "$DEPLOY_DIR/certifications-api" "$DEPLOY_DIR/certifications-api.new"
mv -f "$DEPLOY_DIR/certifications-api.new" "$DEPLOY_DIR/certifications-api"
install -D -m 644 "$DEPLOY_DIR/$SERVICE_NAME.service" "/etc/systemd/system/$SERVICE_NAME.service"
systemctl daemon-reload
systemctl enable "$SERVICE_NAME.service"
systemctl restart "$SERVICE_NAME.service"

expected=$(sha256sum "$DEPLOY_DIR/certifications-api" | cut -d ' ' -f1)
matched=false
for _ in {1..30}; do
  pid=$(systemctl show "$SERVICE_NAME.service" --property=MainPID --value)
  if systemctl is-active --quiet "$SERVICE_NAME.service" && [[ "$pid" != 0 ]] &&
     [[ "$(sha256sum "/proc/$pid/exe" 2>/dev/null | cut -d ' ' -f1)" == "$expected" ]]; then
    matched=true
    break
  fi
  sleep 0.2
done
[[ "$matched" == true ]] || {
  printf 'Running Rust binary verification failed for %s.\n' "$SERVICE_NAME.service" >&2; exit 1;
}
printf '%s is active and running the prepared Rust binary.\n' "$SERVICE_NAME.service"
