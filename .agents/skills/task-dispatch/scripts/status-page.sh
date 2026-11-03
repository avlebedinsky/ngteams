#!/usr/bin/env bash
# Starts the dispatch status page with Python's standard library; no build or dependencies.
#
# Usage: status-page.sh --ledger <path> [--port <n>] [--subagents <dir>] [--exit-with <pid>]
# The arguments go to the server unchanged.
set -euo pipefail

page="$(cd "$(dirname "${BASH_SOURCE[0]}")/../dispatch-status" && pwd)"
exec python3 -B "$page/status.py" "$@"
