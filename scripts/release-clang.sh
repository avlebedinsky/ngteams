#!/usr/bin/env bash
# Unpack the pinned clang (packaging/release/pins.env) under DIR, only after its archive's sha256 matches, and print its
# bin directory. Only the compiler and its own headers are taken from the 2 GB archive: a hosted runner is short of disk.
#
# Usage: scripts/release-clang.sh DIR
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=lib/fetch-tool.sh
source "$root/scripts/lib/fetch-tool.sh"
# shellcheck source=../packaging/release/pins.env
source "$root/packaging/release/pins.env"

(($# == 1)) || die "usage: scripts/release-clang.sh DIR"
[[ "$(uname -m)" == x86_64 ]] || die "no pinned clang for $(uname -m)"
dir="$1/llvm-$LLVM_VERSION"
if [[ ! -x "$dir/bin/clang++" ]]; then
  name="LLVM-$LLVM_VERSION-Linux-X64"
  archive="$1/$name.tar.xz"
  github_download "https://github.com/llvm/llvm-project/releases/download/llvmorg-$LLVM_VERSION/$name.tar.xz" "$archive"
  echo "$LLVM_X86_64_SHA256  $archive" | sha256sum --check --quiet >&2 || die "$archive: not the pinned clang"
  rm -rf "$dir"
  mkdir -p "$dir"
  tar -xJf "$archive" -C "$dir" --strip-components=1 --wildcards \
    "$name/bin/clang" "$name/bin/clang++" "$name/bin/clang-[0-9]*" "$name/lib/clang/*"
  rm -f "$archive"
fi
echo "$dir/bin"
