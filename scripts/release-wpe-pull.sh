#!/usr/bin/env bash
# The portable WPE WebKit prefix that release.yml builds and links against, unpacked into NGTEAMS_RELEASE_WPE. It is
# ngterm's public GHCR artifact, pinned by digest in packaging/release/pins.env; only `gh attestation verify` passing
# (ngterm's cache workflow on main built it) lets it unpack, and a failure fails the job. This repo never builds WPE.
# Needs GH_TOKEN for `gh attestation verify`. Pulls and unpacks by digest, so a moved tag changes nothing.
#
# Usage: NGTEAMS_RELEASE_WPE=<prefix> scripts/release-wpe-pull.sh
set -euo pipefail
(($# == 0)) || { echo "usage: scripts/release-wpe-pull.sh" >&2; exit 2; }

repo="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=lib/fetch-tool.sh
source "$repo/scripts/lib/fetch-tool.sh"
# shellcheck source=../packaging/release/pins.env
source "$repo/packaging/release/pins.env"

prefix="${NGTEAMS_RELEASE_WPE:?set NGTEAMS_RELEASE_WPE to the pinned prefix path}"
# The .pc files inside the artifact name this path.
[[ "$prefix" == "$RELEASE_WPE_PREFIX" ]] || die "NGTEAMS_RELEASE_WPE must be $RELEASE_WPE_PREFIX, the path the artifact was built for"
[[ "$RELEASE_WPE_DIGEST" =~ ^sha256:[0-9a-f]{64}$ ]] || die "not a sha256 digest: $RELEASE_WPE_DIGEST"
[[ "$(uname -m)" == x86_64 ]] || die "the release builds on x86_64 only"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

url="https://github.com/oras-project/oras/releases/download/v$ORAS_VERSION/oras_${ORAS_VERSION}_linux_amd64.tar.gz"
oras="$(fetch_tool "$work" oras "$ORAS_VERSION" "$url" "$ORAS_X86_64_SHA256" oras)/oras"

reference="$RELEASE_WPE_REPOSITORY@$RELEASE_WPE_DIGEST"
echo "$reference ($RELEASE_WPE_TAG)"
gh attestation verify "oci://$reference" --repo nickalie/ngterm \
  --signer-workflow nickalie/ngterm/.github/workflows/release-wpe-cache.yml --source-ref refs/heads/main
"$oras" pull "$reference" -o "$work/pulled"
[[ "$(ls -A "$work/pulled")" == release-wpe.tar.zst ]] || die "$reference holds more or other than release-wpe.tar.zst"

rm -rf "$prefix"
mkdir -p "$prefix"
tar --zstd -xf "$work/pulled/release-wpe.tar.zst" -C "$prefix" --no-same-owner
key="${RELEASE_WPE_TAG##*-}"
[[ "$(cat "$prefix/.release-pins")" == "$key" ]] || die "$reference is not the prefix tagged $RELEASE_WPE_TAG"
grep -qx "prefix=$prefix" "$prefix/lib/pkgconfig/wpe-webkit-2.0.pc" \
  || die "$reference: its pkg-config files do not name $prefix"
# The staleness stamp scripts/web-engine-closure.py compares with the pin.
echo "$RELEASE_WPE_DIGEST" > "$prefix/.release-wpe-digest"
echo "$prefix: unpacked from $reference"
