#!/usr/bin/env bash
# The pkg-config of every cargo build in this repository (.cargo/config.toml sets PKG_CONFIG to it; an exported
# PKG_CONFIG wins): the system pkg-config, with the vendored WPE WebKit prefix of scripts/fetch-wpe.sh first on
# PKG_CONFIG_PATH when the system has no wpe-webkit-2.0 of the bound API (2.54) and the prefix is complete. With
# neither, the WPE crates fail naming `wpe-webkit-2.0` and scripts/fetch-wpe.sh.
#
# Cargo does not see that choice change: after installing or removing either, run
# `cargo clean -p teams-wpe -p wpe-webkit-sys -p wpe-platform-sys -p wpe-javascriptcore-sys -p teams-signin-extension`.
set -euo pipefail

if [[ -n "${HOST:-}" && -n "${TARGET:-}" && "$HOST" != "$TARGET" ]]; then
  echo "scripts/pkg-config.sh is host-only ($HOST, building for $TARGET): export PKG_CONFIG for cross builds" >&2
  exit 1
fi

here="$(cd "$(dirname "$0")" && pwd)"
if ! pkg-config --atleast-version=2.54 wpe-webkit-2.0 2> /dev/null && "$here/fetch-wpe.sh" --verify; then
  prefix="$("$here/fetch-wpe.sh" --print-prefix)"
  export PKG_CONFIG_PATH="$prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
fi
exec pkg-config "$@"
