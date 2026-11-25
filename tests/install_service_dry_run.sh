#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fixture="$tmp/checkout"
mockbin="$tmp/mockbin"
mkdir -p "$fixture/scripts" "$fixture/lib" "$mockbin"
cp "$repo/scripts/install_service.sh" "$fixture/scripts/install_service.sh"
cp "$repo/Cargo.toml" "$repo/Cargo.lock" "$fixture/"
cp "$repo/lib/main.rs" "$fixture/lib/main.rs"
chmod +x "$fixture/scripts/install_service.sh"
git -C "$fixture" init -q
git -C "$fixture" add Cargo.toml Cargo.lock lib/main.rs scripts/install_service.sh
git -C "$fixture" -c user.name=fixture -c user.email=fixture@example.invalid commit -qm fixture

for command in cargo install systemctl flock mkdir mv touch chown chmod; do
  cat > "$mockbin/$command" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "${0##*/} $*" >> "$MUTATION_ATTEMPTS"
exit 99
MOCK
  chmod +x "$mockbin/$command"
done
export MUTATION_ATTEMPTS="$tmp/mutation-attempts"
real_path=$PATH
export PATH="$mockbin:$PATH"

snapshot() {
  (cd "$fixture" && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum)
}

before=$(snapshot)
output=$("$fixture/scripts/install_service.sh" --dry-run)
[[ "$output" == *"Rust release inputs are present"* ]]
[[ ! -e "$fixture/var" ]]
[[ "$before" == "$(snapshot)" ]]
[[ ! -e "$MUTATION_ATTEMPTS" ]]

# Make a complete synthetic prepared package. Dry-run validates it without
# invoking any installer or service command and without changing a byte.
export PATH="$real_path"
deploy="$fixture/var/deploy"
mkdir -p "$deploy"
printf 'synthetic executable\n' > "$deploy/certifications-api"
chmod +x "$deploy/certifications-api"
cat > "$deploy/certifications-api.service" <<UNIT
[Service]
WorkingDirectory=$fixture
ExecStart=$deploy/certifications-api
UNIT
git -C "$fixture" ls-files -z -- Cargo.toml Cargo.lock lib scripts/install_service.sh |
  LC_ALL=C sort -z | xargs -0 sha256sum | sha256sum | cut -d ' ' -f1 > "$deploy/source.sha256"
(cd "$deploy" && sha256sum certifications-api certifications-api.service > artifacts.sha256)
before=$(snapshot)
export PATH="$mockbin:$real_path"
output=$("$fixture/scripts/install_service.sh" --dry-run)
[[ "$output" == *"passed read-only validation"* ]]
[[ "$before" == "$(snapshot)" ]]
[[ ! -e "$MUTATION_ATTEMPTS" ]]

# Failure while validating a damaged package is still read-only.
printf 'tampered\n' >> "$deploy/certifications-api"
before=$(snapshot)
if "$fixture/scripts/install_service.sh" --dry-run >/dev/null 2>&1; then
  printf 'Expected damaged package validation to fail.\n' >&2
  exit 1
fi
[[ "$before" == "$(snapshot)" ]]
[[ ! -e "$MUTATION_ATTEMPTS" ]]
printf 'Dry-run synthetic fixture checks passed.\n'
