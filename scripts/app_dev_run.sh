#!/usr/bin/env bash
set -euo pipefail


# sudo systemctl enable --now clamav-freshclam
# sudo systemctl enable --now clamav-daemon


SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cd "$SCRIPT_DIR/.."

source .venv/bin/activate

export PYTHONPATH="$(pwd)"

export ENVIRONMENT=development
HOST=0.0.0.0
PORT=8103

echo "Starting Uvicorn on ${HOST}:${PORT} (reload enabled)…"
exec uvicorn main:app --reload --host "$HOST" --port "$PORT"
