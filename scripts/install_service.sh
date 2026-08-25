#!/usr/bin/env bash
set -euo pipefail

# Must run as root
if [ "$(id -u)" -ne 0 ]; then
  echo "⚠️ Please run this script with sudo or as root"
  exit 1
fi

# Service name
SERVICE_NAME="certifications-api"
UNIT_FILE="/etc/systemd/system/${SERVICE_NAME}.service"

# Paths
SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
PROJECT_DIR=$(cd "$SCRIPT_DIR/.." && pwd)
EXEC_START="${PROJECT_DIR}/scripts/app_prod_run.sh"
LOG_FILE="/var/log/${SERVICE_NAME}.log"

# Ensure the launcher exists
if [ ! -x "$EXEC_START" ]; then
  echo "⚠️ Launcher not found or not executable: $EXEC_START"
  exit 1
fi

# Create (or truncate) the log file and set permissions
touch "$LOG_FILE"
chown root:root "$LOG_FILE"
chmod 644 "$LOG_FILE"

# Write the systemd unit (overwrites if already there)
cat > "$UNIT_FILE" <<EOF
[Unit]
Description=Asodya API Service
After=network.target

[Service]
Type=simple
WorkingDirectory=${PROJECT_DIR}
ExecStart=${EXEC_START}
Restart=always
RestartSec=5
StandardOutput=append:${LOG_FILE}
StandardError=append:${LOG_FILE}

[Install]
WantedBy=multi-user.target
EOF

# Reload systemd, enable & start the service
systemctl daemon-reload
systemctl enable "${SERVICE_NAME}"
systemctl start "${SERVICE_NAME}"

echo "✅ ${SERVICE_NAME} installed, enabled, and started."
echo "   Logs are at: ${LOG_FILE}"
