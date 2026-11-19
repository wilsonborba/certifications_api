#!/usr/bin/env bash
set -euo pipefail

CLAMAV_VERSION="1.5.5"
CLAMAV_DEB_NAME="clamav-${CLAMAV_VERSION}.linux.x86_64.deb"
CLAMAV_URL="https://github.com/Cisco-Talos/clamav/releases/download/clamav-${CLAMAV_VERSION}/${CLAMAV_DEB_NAME}"
CLAMAV_SHA256="fff17fb1f8917d1ea1308c9736dabe376077458aba000a5dadcce7193a8c538c"

DATA_ROOT="${CLAMAV_DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/certifications_api/clamav}"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/clamav-install.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

DEB_PATH="${TMP_DIR}/${CLAMAV_DEB_NAME}"
echo "Downloading ${CLAMAV_URL}..."
curl -fsSL "$CLAMAV_URL" -o "$DEB_PATH"

echo "Verifying SHA-256..."
echo "${CLAMAV_SHA256}  ${DEB_PATH}" | sha256sum -c -

mkdir -p "$DATA_ROOT" "$DATA_ROOT/signatures" "$DATA_ROOT/etc"
dpkg-deb -x "$DEB_PATH" "$DATA_ROOT"

cat > "$DATA_ROOT/etc/freshclam.conf" <<EOF
DatabaseDirectory $DATA_ROOT/signatures
DatabaseMirror database.clamav.net
Foreground yes
EOF

chmod 755 "$DATA_ROOT/usr/local/bin/clamscan" "$DATA_ROOT/usr/local/bin/freshclam"
echo "Installed ClamAV ${CLAMAV_VERSION} (app-owned, no systemd/daemon) into $DATA_ROOT"
