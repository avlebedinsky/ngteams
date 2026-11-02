#!/usr/bin/env bash
# Starts the dispatch status page (../dispatch-status), building it first when needed. The crate is a workspace of its
# own, so cargo has next to nothing to resolve when nothing changed, and its build goes to its own target dir, so it
# never waits on another build's lock.
#
# Usage: status-page.sh --ledger <path> [--port <n>] [--subagents <dir>] [--exit-with <pid>]
# (the arguments go to the server as they are; DISPATCH_STATUS_TARGET_DIR overrides the target dir)
set -euo pipefail

crate="$(cd "$(dirname "${BASH_SOURCE[0]}")/../dispatch-status" && pwd)"
target="${DISPATCH_STATUS_TARGET_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/ngteams-dispatch-status}"

# rustup picks the toolchain by the working dir (rust-toolchain.toml at the repo root)
(cd "$crate" && cargo build -q --release --target-dir "$target")
exec "$target/release/dispatch-status" "$@"
