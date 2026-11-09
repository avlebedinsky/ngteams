#!/usr/bin/env bash
# Build the downloadable Linux web engine on the Ubuntu release runner. NGTEAMS_RELEASE_WPE must name the portable WPE
# prefix from scripts/release-wpe-pull.sh. --print-key resolves the same inputs and package versions without copying
# or archiving them.
# Usage: scripts/package-web-engine.sh OUTPUT_DIR
#        scripts/package-web-engine.sh --print-key
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
exec python3 "$root/scripts/web-engine-closure.py" package --prefix "${NGTEAMS_RELEASE_WPE:?set NGTEAMS_RELEASE_WPE}" "$@"
