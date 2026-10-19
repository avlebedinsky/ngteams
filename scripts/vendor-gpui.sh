#!/usr/bin/env bash
# Vendor the GPUI crates we patch into vendor/<crate> with build/patches/<crate> applied, for the workspace's
# [patch.crates-io] (M0 Task 16, approach (b) of the video spike). Taken from agterm-rs (scripts/vendor-gpui.sh) with
# its external-surfaces patches only, unchanged:
#   gpui-pre       `ExternalSurface` (a dmabuf or CPU pixels) as the Linux payload of `PaintSurface`, and
#                  `Window::paint_external_surface` (0001)
#   gpui-pre-wgpu  the renderer's side: dmabufs imported through wgpu-hal Vulkan and copied on the GPU, CPU pixels
#                  uploaded into a texture kept per surface, by damaged rectangle (0001)
#
# Each source is the crates.io archive of GPUI_VERSION, verified against SHA256 below (the release's crates.io
# checksum: the `cksum` of its line in https://index.crates.io/gp/ui/<crate>, also what Cargo.lock recorded for the
# unpatched crate). It is unpacked verbatim apart from its Cargo.lock and, for gpui-pre, its examples/ directory, then
# patched. Bump GPUI_VERSION and the sums together with the workspace's `gpui-pre` pin, run this, and check that the
# patches still apply.
#
# Usage: scripts/vendor-gpui.sh           (re)create vendor/gpui-pre and vendor/gpui-pre-wgpu
#        scripts/vendor-gpui.sh --check   rebuild into a scratch dir and fail if a vendored crate differs
set -euo pipefail
shopt -s nullglob

GPUI_VERSION="0.3.9"
declare -A SHA256=(
  [gpui-pre]="34f61c1cfd36fc9c436af09308c963822ae27a45a84feb21a77ba5c9e051f1c0"
  [gpui-pre-wgpu]="5b1cdb26c1503bf9662ee8fbc5ab0b44b2deeee4b5c12dc67491ba6fb5fc544b"
)
CRATES=(gpui-pre gpui-pre-wgpu)

root="$(cd "$(dirname "$0")/.." && pwd)"

die() {
  echo "$*" >&2
  exit 1
}

mode=vendor
case "${1:-}" in
  "") ;;
  --check) mode=check ;;
  -h | --help) sed -n '2,17s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"; exit 0 ;;
  *) die "unknown argument: $1 (see --help)" ;;
esac

scratch="$(mktemp -d "${TMPDIR:-/tmp}/ms-teams-gpui-XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

# The verified crates.io source of $1 with its patches applied, in $scratch/built/$1.
built() {
  local crate="$1" archive sum patch
  archive="$scratch/$crate-$GPUI_VERSION.crate"
  curl -sSfL -A ms-teams-dev -o "$archive" "https://static.crates.io/crates/$crate/$crate-$GPUI_VERSION.crate"
  sum="$(sha256sum "$archive" | cut -d' ' -f1)"
  [[ "$sum" == "${SHA256[$crate]}" ]] || die "$crate $GPUI_VERSION: sha256 $sum, expected ${SHA256[$crate]}"
  mkdir -p "$scratch/unpack" "$scratch/built"
  tar -xzf "$archive" -C "$scratch/unpack"
  mv "$scratch/unpack/$crate-$GPUI_VERSION" "$scratch/built/$crate"
  rm -f "$scratch/built/$crate/Cargo.lock"
  if [[ "$crate" == gpui-pre ]]; then
    rm -rf "$scratch/built/$crate/examples"
  fi
  for patch in "$root/build/patches/$crate"/*.patch; do
    patch -s -p1 -d "$scratch/built/$crate" <"$patch" || die "$(basename "$patch") does not apply to $crate $GPUI_VERSION"
  done
}

for crate in "${CRATES[@]}"; do
  built "$crate"
  target="$root/vendor/$crate"
  case "$mode" in
    check)
      diff -r "$scratch/built/$crate" "$target" >&2 || die "vendor/$crate differs from $crate $GPUI_VERSION + build/patches/$crate"
      echo "vendor/$crate is $crate $GPUI_VERSION + build/patches/$crate"
      ;;
    vendor)
      mkdir -p "$root/vendor"
      rm -rf "$target.new"
      mv "$scratch/built/$crate" "$target.new"
      rm -rf "$target"
      mv "$target.new" "$target"
      echo "vendored $crate $GPUI_VERSION into $target"
      ;;
  esac
done
