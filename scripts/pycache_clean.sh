#!/usr/bin/env bash
set -euo pipefail

# Get absolute path to the script location and go to project root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.."

echo "Searching for __pycache__ directories..."
found=$(find . -type d -name "__pycache__")

if [[ -z "$found" ]]; then
    echo "No __pycache__ directories found."
else
    find . -type d -name "__pycache__" -exec rm -rf {} +
    echo "Cleanup complete."
fi
