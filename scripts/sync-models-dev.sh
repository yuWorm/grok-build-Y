#!/usr/bin/env bash
# Fetch models.dev and compact model metadata into the committed snapshot.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
exec python3 "$root/scripts/sync_models_dev.py" "$@"
