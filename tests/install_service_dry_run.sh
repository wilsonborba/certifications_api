#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fixture="$tmp/source checkout"
caller="$tmp/unrelated caller cwd"
target="$tmp/separate build target"
builder_bin="$tmp/builder-bin"
guard_bin="$tmp/read-only-guard-bin"
mkdir -p "$fixture/scripts" "$caller" "$builder_bin" "$guard_bin"
cp "$repo/scripts/install_service.sh" "$fixture/scripts/install_service.sh"
cp "$repo/Cargo.toml" "$repo/Cargo.lock" "$fixture/"
cp -R "$repo/lib" "$fixture/lib"
cp -R "$repo/migrations" "$fixture/migrations"
mkdir -p "$fixture/tests"
cp -R "$repo/tests/sqlite" "$fixture/tests/sqlite"
if [[ -d "$repo/.cargo" ]]; then cp -R "$repo/.cargo" "$fixture/.cargo"; fi
if [[ -f "$repo/build.rs" ]]; then cp "$repo/build.rs" "$fixture/build.rs"; fi
mkdir -p "$fixture/assets/fonts" "$fixture/assets/branding" "$fixture/assets/templates"
printf 'mock font bytes\n' > "$fixture/assets/fonts/IBMPlexSansThai-Regular.ttf"
printf 'mock certificate branding bytes\n' > "$fixture/assets/branding/certificate-logo.svg"
printf 'mock certificate template bytes\n' > "$fixture/assets/templates/certificate.json"
chmod +x "$fixture/scripts/install_service.sh"
printf 'space-sensitive build input\n' > "$fixture/lib/input file with spaces.txt"
git -C "$fixture" init -q
git -C "$fixture" add .
git -C "$fixture" -c user.name=fixture -c user.email=fixture@example.invalid commit -qm fixture

cat > "$builder_bin/cargo" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
printf '%s|%s\n' "$PWD" "$*" >> "$CARGO_CALLS"
if [[ "${1:-}" == metadata ]]; then
  printf '{"target_directory":"%s"}\n' "$CARGO_TARGET_DIR"
elif [[ "${1:-}" == build ]]; then
  [[ "$PWD" == "$FIXTURE_ROOT" ]]
  mkdir -p "$CARGO_TARGET_DIR/release"
  printf 'synthetic Rust binary from %s\n' "$FIXTURE_ROOT" > "$CARGO_TARGET_DIR/release/certifications-api"
  chmod 755 "$CARGO_TARGET_DIR/release/certifications-api"
else
  printf 'Unexpected fake cargo invocation: %s\n' "$*" >&2
  exit 98
fi
MOCK
chmod +x "$builder_bin/cargo"
export CARGO_CALLS="$tmp/cargo-calls"
export FIXTURE_ROOT="$fixture"
export CARGO_TARGET_DIR="$target"

# Prepare from a different current directory with a separate target directory.
# The bounded fake Cargo writes only the synthetic fixture artifact.
(cd "$caller" && PATH="$builder_bin:$PATH" "$fixture/scripts/install_service.sh" --prepare)
[[ -x "$target/release/certifications-api" ]]
[[ ! -e "$fixture/target" ]]
cmp "$target/release/certifications-api" "$fixture/var/deploy/certifications-api"
[[ "$(stat -c '%a' "$fixture/var/deploy")" == 755 ]]
[[ "$(stat -c '%a' "$fixture/var/deploy/certifications-api")" == 755 ]]
[[ "$(grep -c '^' "$CARGO_CALLS")" == 2 ]]
while IFS='|' read -r cwd _; do [[ "$cwd" == "$fixture" ]]; done < "$CARGO_CALLS"

# Keep snapshots of file content, mode, timestamps, directory entries and link
# targets. Reads may update atime, which is intentionally excluded.
snapshot() {
  (
    cd "$fixture"
    find . -printf '%y %P %m %s %T@ %C@ %u %g %l\n' | LC_ALL=C sort
    find . -type f -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum
  )
}
expect_readonly_failure() {
  local before
  before=$(snapshot)
  if (cd "$caller" && "$@" >/dev/null 2>&1); then
    printf 'Expected command to fail: %s\n' "$*" >&2
    exit 1
  fi
  [[ "$before" == "$(snapshot)" ]]
  [[ ! -e "$MUTATION_ATTEMPTS" ]]
}

# Mutating commands are replaced with recorders. Valid read-only modes from
# outside the checkout must neither call them nor change the fixture.
for command in cargo install systemctl flock mkdir mv rm touch chown chmod cp ln; do
  cat > "$guard_bin/$command" <<'MOCK'
#!/usr/bin/env bash
printf '%s %s\n' "${0##*/}" "$*" >> "$MUTATION_ATTEMPTS"
exit 99
MOCK
  chmod +x "$guard_bin/$command"
done
export MUTATION_ATTEMPTS="$tmp/mutation-attempts"
guard_path="$guard_bin:$PATH"

before=$(snapshot)
output=$(cd "$caller" && PATH="$guard_path" "$fixture/scripts/install_service.sh" --dry-run)
[[ "$output" == *"passed read-only validation"* ]]
[[ "$before" == "$(snapshot)" ]]
[[ ! -e "$MUTATION_ATTEMPTS" ]]
output=$(cd "$caller" && PATH="$guard_path" "$fixture/scripts/install_service.sh" --check)
[[ "$output" == *"Prepared Rust package verified"* ]]
[[ "$before" == "$(snapshot)" ]]
[[ ! -e "$MUTATION_ATTEMPTS" ]]

# Untracked prototypes and screenshots must not affect the package fingerprint.
mkdir -p "$fixture/assets/branding/prototypes"
printf 'untracked prototype screenshot bytes\n' > "$fixture/assets/branding/prototypes/sample-screenshot.png"
before=$(snapshot)
output=$(cd "$caller" && PATH="$guard_path" "$fixture/scripts/install_service.sh" --dry-run)
[[ "$output" == *"passed read-only validation"* ]]
[[ "$before" == "$(snapshot)" ]]
[[ ! -e "$MUTATION_ATTEMPTS" ]]

mv "$fixture/var/deploy" "$tmp/prepared-package"
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --check
mv "$tmp/prepared-package" "$fixture/var/deploy"
chmod 700 "$fixture/var/deploy"
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --check
chmod 755 "$fixture/var/deploy"

# A self-consistent checksum manifest cannot make a unit that points elsewhere
# valid; the package contract also binds WorkingDirectory and ExecStart.
cat > "$fixture/var/deploy/certifications-api.service" <<UNIT
[Service]
WorkingDirectory=$fixture
ExecStart=$tmp/not-the-prepared-binary
UNIT
chmod 644 "$fixture/var/deploy/certifications-api.service"
(cd "$fixture/var/deploy" && sha256sum certifications-api certifications-api.service > artifacts.sha256)
chmod 644 "$fixture/var/deploy/artifacts.sha256"
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --check
cat > "$fixture/var/deploy/certifications-api.service" <<UNIT
[Service]
WorkingDirectory=$fixture
ExecStart=$fixture/var/deploy/certifications-api
UNIT
chmod 644 "$fixture/var/deploy/certifications-api.service"
(cd "$fixture/var/deploy" && sha256sum certifications-api certifications-api.service > artifacts.sha256)
chmod 644 "$fixture/var/deploy/artifacts.sha256"

# Source and package validation failures remain read-only.
printf 'source changed\n' >> "$fixture/lib/main.rs"
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --dry-run
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --check
git -C "$fixture" checkout -- lib/main.rs

source_code_hash=$(sha256sum "$fixture/lib/main.rs")
printf 'font changed\n' >> "$fixture/assets/fonts/IBMPlexSansThai-Regular.ttf"
[[ "$source_code_hash" == "$(sha256sum "$fixture/lib/main.rs")" ]]
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --dry-run
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --check
git -C "$fixture" checkout -- assets/fonts/IBMPlexSansThai-Regular.ttf
output=$(cd "$caller" && PATH="$guard_path" "$fixture/scripts/install_service.sh" --check)
[[ "$output" == *"Prepared Rust package verified"* ]]

printf 'branding changed\n' >> "$fixture/assets/branding/certificate-logo.svg"
[[ "$source_code_hash" == "$(sha256sum "$fixture/lib/main.rs")" ]]
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --dry-run
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --check
git -C "$fixture" checkout -- assets/branding/certificate-logo.svg
output=$(cd "$caller" && PATH="$guard_path" "$fixture/scripts/install_service.sh" --check)
[[ "$output" == *"Prepared Rust package verified"* ]]

printf 'template changed\n' >> "$fixture/assets/templates/certificate.json"
[[ "$source_code_hash" == "$(sha256sum "$fixture/lib/main.rs")" ]]
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --dry-run
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --check
git -C "$fixture" checkout -- assets/templates/certificate.json
output=$(cd "$caller" && PATH="$guard_path" "$fixture/scripts/install_service.sh" --check)
[[ "$output" == *"Prepared Rust package verified"* ]]

rm "$fixture/var/deploy/certifications-api"
printf 'outside binary\n' > "$tmp/outside-binary"
chmod 755 "$tmp/outside-binary"
ln -s "$tmp/outside-binary" "$fixture/var/deploy/certifications-api"
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --dry-run
[[ "$(cat "$tmp/outside-binary")" == 'outside binary' ]]
rm "$fixture/var/deploy/certifications-api"
cp "$target/release/certifications-api" "$fixture/var/deploy/certifications-api"
chmod 775 "$fixture/var/deploy/certifications-api"
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --check
chmod 755 "$fixture/var/deploy/certifications-api"

# A manifest containing only the service checksum used to pass even after the
# Rust executable was changed. The manifest must name both expected files and
# no others before any checksum content is accepted.
printf 'changed Rust executable while keeping mode 755\n' > "$fixture/var/deploy/certifications-api"
chmod 755 "$fixture/var/deploy/certifications-api"
(cd "$fixture/var/deploy" && sha256sum certifications-api.service > artifacts.sha256)
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --check
cp "$target/release/certifications-api" "$fixture/var/deploy/certifications-api"
(cd "$fixture/var/deploy" && sha256sum certifications-api certifications-api.service > artifacts.sha256)
cp "$fixture/var/deploy/artifacts.sha256" "$tmp/duplicate-manifest-entries"
cat "$tmp/duplicate-manifest-entries" >> "$fixture/var/deploy/artifacts.sha256"
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --check
cp "$tmp/duplicate-manifest-entries" "$fixture/var/deploy/artifacts.sha256"
printf '%064d  foreign-artifact\n' 0 >> "$fixture/var/deploy/artifacts.sha256"
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --check
cp "$tmp/duplicate-manifest-entries" "$fixture/var/deploy/artifacts.sha256"
printf '%064d  ../outside-artifact\n' 0 >> "$fixture/var/deploy/artifacts.sha256"
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --check
cp "$tmp/duplicate-manifest-entries" "$fixture/var/deploy/artifacts.sha256"

rm -rf "$fixture/var/deploy"
mv "$fixture/var/deploy.lock" "$tmp/deploy.lock"
ln -s "$tmp/nonexistent-target" "$fixture/var/deploy"
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --dry-run
rm "$fixture/var/deploy"
mv "$tmp/deploy.lock" "$fixture/var/deploy.lock"

# Missing tracked inputs and hash/read errors cannot collapse into an empty or
# partial fingerprint and be accepted.
mv "$fixture/lib/main.rs" "$tmp/main.rs.saved"
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --dry-run
mv "$tmp/main.rs.saved" "$fixture/lib/main.rs"

cat > "$guard_bin/sha256sum" <<'MOCK'
#!/usr/bin/env bash
if [[ "${FAIL_SHA256SUM:-}" == yes ]]; then
  printf 'simulated unreadable input\n' >&2
  exit 23
fi
exec /usr/bin/sha256sum "$@"
MOCK
chmod +x "$guard_bin/sha256sum"
before=$(snapshot)
if (cd "$caller" && FAIL_SHA256SUM=yes PATH="$guard_path" "$fixture/scripts/install_service.sh" --check >/dev/null 2>&1); then
  printf 'Expected source hashing failure to fail validation.\n' >&2
  exit 1
fi
[[ "$before" == "$(snapshot)" ]]
[[ ! -e "$MUTATION_ATTEMPTS" ]]

expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --unknown-mode
expect_readonly_failure env PATH="$guard_path" "$fixture/scripts/install_service.sh" --dry-run --activate
printf 'Installer dry-run and negative packaging fixture checks passed.\n'
