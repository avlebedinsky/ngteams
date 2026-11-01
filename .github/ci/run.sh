#!/usr/bin/env bash
set -euo pipefail

# The container has no DRM device. Keep software-rendered GUI tests and skip only real DMA-buffer export.
export TEAMS_WPE_TEST_SKIP_DMA=1
export WGPU_BACKEND=vulkan
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}
# One-shot CI does not reuse incremental objects or need DWARF. LLVM's coverage mappings remain enabled.
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0

CARGO_TARGET_DIR=target/ci-bootstrap-build cargo run --locked -p proto-lab -- chrome install
rm -rf -- target/ci-bootstrap-build
exec scripts/coverage.sh --require-target
