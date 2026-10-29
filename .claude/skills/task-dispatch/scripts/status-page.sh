#!/usr/bin/env bash
# Starts the dispatch status page (crates/dispatch-status), building it first only when it was never built or its
# sources or Cargo.lock changed since: a plain `cargo build` resolves the whole workspace, seconds even with nothing
# to do. The build goes to its own target dir, so it never waits on another build's lock.
#
# Usage: status-page.sh --ledger <path> [--port <n>] [--subagents <dir>] [--exit-with <pid>]
# (the arguments go to the server as they are; DISPATCH_STATUS_TARGET_DIR overrides the target dir)
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
target="${DISPATCH_STATUS_TARGET_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/ngteams-dispatch-status}"
bin="$target/release/dispatch-status"

if [[ ! -x $bin ]] || [[ -n $(find "$repo/crates/dispatch-status" "$repo/Cargo.lock" -newer "$bin" -print -quit) ]]; then
    # rustup picks the toolchain by the working dir (rust-toolchain.toml at the root)
    (cd "$repo" && cargo build -q --release -p dispatch-status --target-dir "$target")
    # a build that changed nothing leaves the binary's time as it was: mark it current
    touch "$bin"
fi
exec "$bin" "$@"
