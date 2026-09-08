#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cd "$SCRIPT_DIR/.."

source .venv/bin/activate
export PYTHONPATH="$(pwd)"

export ENVIRONMENT=production
HOST=0.0.0.0
PORT=8103
WORKERS=${WORKERS:-4}

echo "Starting Gunicorn + Uvicorn workers on ${HOST}:${PORT} with ${WORKERS} workers…"

exec gunicorn \
     -k uvicorn.workers.UvicornWorker \
     main:app \
     -b "$HOST:$PORT" \
     -w "$WORKERS" \
     --log-level info
