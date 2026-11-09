#!/usr/bin/env bash
# Install what the Linux release build needs on a hosted Ubuntu 24.04 runner (release.yml's `build` job): the Rust
# release build and packaging tools, what links against the portable WPE prefix, and what the runtime archive copies
# (GStreamer plugins, GIO modules and their library closure).
set -euo pipefail
(($# == 0)) || { echo "usage: scripts/release-linux-deps.sh" >&2; exit 2; }
# The Rust release build needs the disk space hosted runners give only without these.
sudo rm -rf /usr/local/lib/android /opt/ghc /usr/share/dotnet /usr/local/.ghcup
sudo apt-get update
packages=(
  # the build and the packaging tools
  build-essential clang libclang-dev cmake pkg-config python3 ca-certificates curl xz-utils zstd file patchelf desktop-file-utils jq
  # the app's own -sys crates
  libwayland-dev libxkbcommon-dev libfontconfig-dev libfreetype-dev libpipewire-0.3-dev
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libglib2.0-dev
  # linking the portable WPE prefix: pkg-config's Requires (glib, libsoup) and the libraries libWPEWebKit needs resolved
  libsoup-3.0-dev libepoxy0 libatk1.0-0t64 libatk-bridge2.0-0t64 libinput10 libmanette-0.2-0
  libwebpmux3 libwebpdemux2 libwebp7 libharfbuzz0b libharfbuzz-icu0 libjpeg-turbo8 libxslt1.1 liblcms2-2
  libflite1 libjxl0.7 libavif16 libenchant-2-2 libhyphen0
  # what the runtime archive copies: its plugins, GIO modules and the host libraries they resolve
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-libav
  gstreamer1.0-gl gstreamer1.0-alsa gstreamer1.0-pulseaudio glib-networking dconf-gsettings-backend
)
sudo apt-get install -y --no-install-recommends "${packages[@]}"
