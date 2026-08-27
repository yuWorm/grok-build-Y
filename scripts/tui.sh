#!/usr/bin/env bash
# Launch the *local* fork TUI (not ~/.grok/bin/grok).
#
#   ./scripts/tui.sh                  # sandbox cwd, --trust
#   ./scripts/tui.sh --model grok-4.6
#   ./scripts/tui.sh --debug-file /tmp/grok-tui.log
#
# Rebuild is incremental via cargo run. Extra args are passed to the pager.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

sandbox="${GROK_TUI_CWD:-/tmp/grok-compat-e2e}"
mkdir -p "$sandbox"
if [[ ! -f "$sandbox/hello.txt" ]]; then
  printf 'hello from e2e fixture\n' >"$sandbox/hello.txt"
fi

echo "fork TUI  (not $(command -v grok 2>/dev/null || echo grok))" >&2
echo "cwd       $sandbox" >&2
echo "try       /provider-login openai   then   /model" >&2

exec cargo run -q -p xai-grok-pager-bin --bin groky -- --cwd "$sandbox" --trust "$@"
