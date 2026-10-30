#!/usr/bin/env bash
# Unpacks the pinned WPE WebKit (Arch's `wpewebkit` binary package) into the vendored prefix
# ${NGTEAMS_VENDOR:-~/.cache/ngteams/vendor}/wpe-<version>-<release>/, for a machine with an Arch-compatible userland
# and no system wpewebkit (no root needed). The sign-in window (`teams-signin` over `teams-wpe`) renders Microsoft's
# page with it.
#
# Nothing is built: the package is checked against its pinned sha256 and unpacked, and its pkg-config files are
# relocated to the prefix; scripts/pkg-config.sh (every cargo build's pkg-config, .cargo/config.toml) then puts the
# prefix on PKG_CONFIG_PATH unless a system wpe-webkit-2.0 of the bound API exists (the system package wins). A
# distribution build has its helper path compiled in (/usr/lib/wpe-webkit-2.0), so pages from this prefix run only with
# the prefix's lib/ overlaid on /usr/lib (`bwrap --ro-overlay`), which `teams_wpe::launcher` does for the engine's
# worker process alone.
#
# Usage: scripts/fetch-wpe.sh                 unpack the prefix unless it is complete, then print its path
#        scripts/fetch-wpe.sh --print-prefix  print the prefix path only
#        scripts/fetch-wpe.sh --verify        exit 0 when the prefix is complete, 1 otherwise
set -euo pipefail

# The pins (the same package ngterm vendors).
WPE_VERSION="2.54.1"
WPE_RELEASE="2"
WPE_ARCH_PACKAGE_URL="https://archive.archlinux.org/packages/w/wpewebkit/wpewebkit-${WPE_VERSION}-${WPE_RELEASE}-x86_64.pkg.tar.zst"
WPE_ARCH_PACKAGE_SHA256="2279cfe4f842b693625b7ad70b81b13fbee1022bed51f5f6319a76855b9273a9"
WPE_ARCH_GLIBC="2.44"

die() {
  echo "$*" >&2
  exit 1
}

vendor="${NGTEAMS_VENDOR:-$HOME/.cache/ngteams/vendor}"
prefix="$vendor/wpe-$WPE_VERSION-$WPE_RELEASE"

complete() {
  local dir="$1"
  [[ -s "$dir/lib/libWPEWebKit-2.0.so.1" && -x "$dir/lib/wpe-webkit-2.0/WPEWebProcess" \
    && -s "$dir/lib/pkgconfig/wpe-webkit-2.0.pc" && -s "$dir/lib/pkgconfig/wpe-web-process-extension-2.0.pc" \
    && -s "$dir/share/ngteams/wpe-PINS" ]]
}

case "${1:-}" in
  --print-prefix)
    echo "$prefix"
    exit 0
    ;;
  --verify)
    complete "$prefix"
    exit
    ;;
  "") ;;
  *) die "usage: scripts/fetch-wpe.sh [--print-prefix | --verify]" ;;
esac

if complete "$prefix"; then
  echo "$prefix"
  exit 0
fi

[[ "$(uname -m)" == x86_64 ]] || die "the pinned WPE WebKit package is x86_64 only: install the distro's wpewebkit"
for command in curl tar zstd sha256sum getconf ldd; do
  command -v "$command" > /dev/null || die "$command is required to unpack WPE WebKit"
done
glibc="$(getconf GNU_LIBC_VERSION | awk '{print $2}')"
[[ "$(printf '%s\n%s\n' "$WPE_ARCH_GLIBC" "$glibc" | sort -V | head -n1)" == "$WPE_ARCH_GLIBC" ]] \
  || die "the pinned WPE WebKit package needs glibc $WPE_ARCH_GLIBC, this host has $glibc: install the distro's wpewebkit"

mkdir -p "$vendor"
work="$(mktemp -d "$vendor/.wpe-fetch.XXXXXX")"
stage="$(mktemp -d "$vendor/.wpe-stage.XXXXXX")"
trap 'rm -rf "$work" "$stage"' EXIT

echo "fetching WPE WebKit $WPE_VERSION..." >&2
curl -fsSLo "$work/wpewebkit.pkg.tar.zst" "$WPE_ARCH_PACKAGE_URL"
echo "$WPE_ARCH_PACKAGE_SHA256  $work/wpewebkit.pkg.tar.zst" | sha256sum --check --quiet >&2 \
  || die "WPE WebKit package sha256 mismatch ($WPE_ARCH_PACKAGE_URL)"
mkdir "$work/pkg"
tar --zstd -xf "$work/wpewebkit.pkg.tar.zst" -C "$work/pkg" usr
for dir in lib include share; do
  mv "$work/pkg/usr/$dir" "$stage/$dir"
done
for pc in "$stage"/lib/pkgconfig/*.pc; do
  sed -i -e "s|^prefix=/usr\$|prefix=$prefix|" -e 's|^libdir=/usr/lib|libdir=${prefix}/lib|' \
    -e 's|^moduledir=/usr/lib|moduledir=${prefix}/lib|' "$pc"
  if grep -E '^[a-z_]+=/usr' "$pc" >&2; then
    die "$(basename "$pc") still points into /usr after relocation"
  fi
done
missing="$(ldd "$stage/lib/libWPEWebKit-2.0.so.1" | awk '/not found/ {print $1}' | sort -u | tr '\n' ' ')"
[[ -z "$missing" ]] || die "WPE WebKit needs libraries this host lacks: ${missing% }; install the distro's wpewebkit"
mkdir -p "$stage/share/ngteams"
printf 'WPE_VERSION=%s\nWPE_RELEASE=%s\nWPE_ARCH_PACKAGE_SHA256=%s\n' "$WPE_VERSION" "$WPE_RELEASE" \
  "$WPE_ARCH_PACKAGE_SHA256" > "$stage/share/ngteams/wpe-PINS"

complete "$stage" || die "the staged WPE WebKit prefix is incomplete"
chmod 755 "$stage"
rm -rf "$prefix"
mv "$stage" "$prefix"
echo "vendored WPE WebKit $WPE_VERSION into $prefix" >&2
echo "$prefix"
