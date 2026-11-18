#!/usr/bin/env bash
set -euo pipefail
if [ -z "${DATABASE_URL:-}" ]; then
  echo "DATABASE_URL must be set" >&2
  exit 1
fi
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
for file in "$DIR"/migrations/*.sql; do
  echo "Applying $(basename "$file")..."
  psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f "$file" >/dev/null
done
echo "Migrations complete."
