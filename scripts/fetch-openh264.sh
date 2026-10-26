#!/usr/bin/env bash
# Fetches Cisco's prebuilt OpenH264 library for the lab's calls' video (M3 Task 18): "OpenH264 Video Codec provided by
# Cisco Systems, Inc." The lab binaries load it from <data_dir>/openh264/ (teams_core::lab::openh264), and only when its
# SHA-256 is the one the binding (openh264-sys2 0.9.9) lists for Cisco's 2.6.0 Linux x86-64 build; a normal build has
# no H.264 code of its own (docs/protocol/calling.md, "The H.264 codec"). Cisco's arrangement with the H.264 patent
# pool covers its own binary, downloaded from Cisco apart from the program that loads it: this is that download. Its
# licence (BINARY_LICENSE.txt) is saved beside it.
#
# Run it by hand, once (again to replace a damaged copy). Nothing is fetched by a test or a build. The personal build
# (`teams`) loads its own copy from ~/.local/share/teams/openh264/ (teams::calling::openh264): the user runs
# `fetch-openh264.sh ~/.local/share/teams` for it; agents never do, as they never touch the personal build's data.
#
# Usage: fetch-openh264.sh [DATA_DIR]   (default: ${XDG_DATA_HOME:-$HOME/.local/share}/teams-lab, the lab's data dir
#                                        unless lab.toml names another)
set -euo pipefail

DATA_DIR=${1:-${XDG_DATA_HOME:-$HOME/.local/share}/teams-lab}
DIR=$DATA_DIR/openh264
LIBRARY=libopenh264-2.6.0-linux64.8.so
URL=https://ciscobinary.openh264.org/$LIBRARY.bz2
SHA256=2f0cde7c6a6abcf5cae76942894ea42897fa677bce4ed6c91a24dd1b041d5f04
LICENSE_URL=https://www.openh264.org/BINARY_LICENSE.txt

mkdir -p "$DIR"
chmod 0700 "$DIR"
TMP=$(mktemp -d "$DIR/.fetch.XXXXXX")
trap 'rm -rf "$TMP"' EXIT

fetch() {
  curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --tlsv1.2 \
    --max-time 120 -o "$2" "$1"
}

fetch "$URL" "$TMP/$LIBRARY.bz2"
bunzip2 -c "$TMP/$LIBRARY.bz2" >"$TMP/$LIBRARY"
# the decompressed library is the one checked: what the binding checks again before it loads it
echo "$SHA256  $TMP/$LIBRARY" | sha256sum --check --quiet
fetch "$LICENSE_URL" "$TMP/BINARY_LICENSE.txt"

chmod 0644 "$TMP/$LIBRARY" "$TMP/BINARY_LICENSE.txt"
mv -f "$TMP/BINARY_LICENSE.txt" "$DIR/BINARY_LICENSE.txt"
mv -f "$TMP/$LIBRARY" "$DIR/$LIBRARY"
echo "OpenH264 Video Codec provided by Cisco Systems, Inc."
echo "$DIR/$LIBRARY (licence: $DIR/BINARY_LICENSE.txt)"
