#!/usr/bin/env bash
# Package a release from one tree of built binaries: the .tar.gz, .AppImage, and nfpm's deb, rpm and archlinux packages.
#
#   scripts/package-release.sh TARGET_DIR VERSION OUTPUT_DIR
#
# TARGET_DIR holds cargo's release output (`ngteams`, `ngteams-web`, `ngteamsctl`, `libteams_signin_extension.so`, the
# last possibly under deps/). VERSION is the release's four-part `<workspace version>.<run number>`. The app finds its
# worker and the sign-in extension beside its real path, so all four files stay together in usr/bin. Only checksum-pinned
# tools run (packaging/release/pins.env); AppImages run extracted, without FUSE.
set -euo pipefail
(($# == 3)) || { echo "usage: scripts/package-release.sh TARGET_DIR VERSION OUTPUT_DIR" >&2; exit 2; }

repo="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=lib/fetch-tool.sh
source "$repo/scripts/lib/fetch-tool.sh"
# shellcheck source=../packaging/release/pins.env
source "$repo/packaging/release/pins.env"

target="$(cd "$1" && pwd)"
version="$2"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "expected MAJOR.MINOR.PATCH.RUN, got '$version'"
[[ "$(uname -m)" == x86_64 ]] || die "the release builds on x86_64 only"
mkdir -p "$3"
dist="$(cd "$3" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

extension="$target/libteams_signin_extension.so"
[[ -f "$extension" ]] || extension="$target/deps/libteams_signin_extension.so"
for file in "$target/ngteams" "$target/ngteams-web" "$target/ngteamsctl" "$extension"; do
  [[ -f "$file" ]] || die "missing built file: $file"
done

app_id=com.nickalie.ngteams
root="$work/ngteams"
mkdir -p "$root/usr/bin" "$root/usr/share/doc/ngteams" "$root/usr/share/applications" "$root/usr/share/icons"
install -m 0755 "$target/ngteams" "$target/ngteams-web" "$target/ngteamsctl" "$root/usr/bin/"
install -m 0755 "$extension" "$root/usr/bin/libteams_signin_extension.so"
[[ ! -f "$repo/LICENSE" ]] || install -m 0644 "$repo/LICENSE" "$root/usr/share/doc/ngteams/LICENSE"
install -m 0644 "$repo/packaging/linux/apparmor/ngteams" "$root/usr/share/doc/ngteams/apparmor.profile.in"
cp -R "$repo/packaging/icons/hicolor" "$root/usr/share/icons/"
install -m 0644 "$repo/packaging/$app_id.desktop" "$root/usr/share/applications/$app_id.desktop"
# AppImage's entry, desktop file and icon sit at the AppDir root.
install -m 0755 "$repo/packaging/linux/AppRun" "$root/AppRun"
install -m 0644 "$repo/packaging/$app_id.desktop" "$root/$app_id.desktop"
install -m 0644 "$repo/packaging/icons/hicolor/256x256/apps/$app_id.png" "$root/$app_id.png"
# Writing a script runs only `ngteamsctl completions`, which asks no app.
for completion in "bash bash-completion/completions/ngteamsctl" "fish fish/vendor_completions.d/ngteamsctl.fish" \
  "zsh zsh/site-functions/_ngteamsctl"; do
  shell="${completion%% *}"
  file="$root/usr/share/${completion#* }"
  install -d -m 0755 "$(dirname "$file")"
  "$root/usr/bin/ngteamsctl" completions "$shell" > "$file"
  chmod 0644 "$file"
  [[ -s "$file" ]] || die "ngteamsctl completions $shell wrote nothing"
done

python3 "$repo/scripts/check-linux-package.py" --prepare "$root"

tar -C "$work" -czf "$dist/ngteams-$version-linux-x86_64.tar.gz" ngteams

download() {
  github_download "$1" "$2" || die "the download failed: $1"
  echo "$3  $2" | sha256sum --check --quiet || die "$(basename "$2"): the sha256 does not match the pin"
}

export PACKAGE_ROOT="$root" PACKAGE_VERSION="$version" PACKAGE_ARCH=amd64
python3 - "$repo/packaging/linux/nfpm.yaml" "$work/nfpm.yaml" <<'PY'
import os
import pathlib
import sys

pathlib.Path(sys.argv[2]).write_text(os.path.expandvars(pathlib.Path(sys.argv[1]).read_text()))
PY
download "https://github.com/goreleaser/nfpm/releases/download/v$NFPM_VERSION/nfpm_${NFPM_VERSION}_Linux_x86_64.tar.gz" \
  "$work/nfpm.tar.gz" "$NFPM_X86_64_SHA256"
tar -xf "$work/nfpm.tar.gz" -C "$work" nfpm
for format in deb rpm archlinux; do
  (cd "$repo" && "$work/nfpm" package --config "$work/nfpm.yaml" --packager "$format" --target "$dist/")
done

download "https://github.com/AppImage/appimagetool/releases/download/$APPIMAGETOOL_VERSION/appimagetool-x86_64.AppImage" \
  "$work/appimagetool.AppImage" "$APPIMAGETOOL_X86_64_SHA256"
download "https://github.com/AppImage/type2-runtime/releases/download/$APPIMAGE_RUNTIME_VERSION/runtime-x86_64" \
  "$work/appimage-runtime" "$APPIMAGE_RUNTIME_X86_64_SHA256"
chmod +x "$work/appimagetool.AppImage"
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$work/appimagetool.AppImage" \
  --runtime-file "$work/appimage-runtime" --no-appstream "$root" "$dist/ngteams-$version-linux-x86_64.AppImage"
