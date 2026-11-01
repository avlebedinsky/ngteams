# Continuous integration

The [Coverage workflow](../.github/workflows/coverage.yml) runs on pull requests, pushes to `main`, merge queues and
manual dispatch. Its `Coverage ≥90%` job runs `scripts/coverage.sh --require-target` over every runtime crate. A crate
below 90% in **any** of LLVM's lines, functions or regions fails the job, even if the workspace average or that
crate's saved standalone baseline is higher or lower. Exactly 90% passes. Failed tests, missing dependencies and
missing source mappings also fail; the workflow does not skip packages or tolerate a failed coverage step.
The hardware DMA subcase is excluded in GitHub Actions, as described below; its runtime source still counts.

The hosted `Coverage gate tests` job checks the reporter and the wrapper's failure propagation, including 89.99%
in each metric and a second crate whose coverage is lower than the first one's. This fast job does not replace the
full measurement. The coverage artifact contains only `summary.json`, including per-crate counts and gaps. Raw
profiles, screenshots, captures and runtime logs are not uploaded.

## Hosted CPU runner

Both jobs use GitHub-hosted Ubuntu runners. The full measurement runs in the
[Arch test image](../.github/ci/Dockerfile), which supplies WPE WebKit ≥2.54, Weston ≥15, niri, PipeWire, the private
keyring tools and Mesa's software OpenGL/Vulkan drivers. Tests run as a non-root user. The container is privileged
so WebKit can create its own sandbox namespaces, and `/dev/dri` is hidden explicitly. It runs on a disposable
hosted VM, with only the checkout mounted; no self-hosted runner, personal files or tenant credentials are used.

`TEAMS_WPE_TEST_SKIP_DMA=1` excludes only the native WPE test's real DMA-buffer subcase. Its worker and in-process
SHM cases still run, as do the other native GUI, media and browser tests. Local runs leave this variable unset and
continue requiring actual DMA frames. This flag excludes a hardware test, not source files: the 90% gate still
measures all runtime crates and keeps the DMA code in the denominator.

CI selects `WGPU_BACKEND=vulkan` so GPUI uses Mesa's Lavapipe software adapter without probing unavailable GLES
devices. The sandbox forwards this graphics setting alongside its existing small environment allowlist.

The image installs the repository's pinned Rust toolchain and LLVM tools plus `cargo-llvm-cov` 0.9.1. The entry
script downloads checksum-verified Chrome using the normal lab CLI, then executes the full strict coverage gate.
Tool installation uses the network; test traffic stays synthetic and offline from the tenant. No lab configuration
or secret is required. The workflow frees unused hosted SDKs to make space for the instrumented native binaries.
The one-shot build disables incremental caches and DWARF debug information while retaining LLVM coverage maps and
debug assertions. It also removes the temporary Chrome-installer build before compiling instrumented tests.
A fresh instrumented build with these settings measured about 23 GiB locally, before temporary test outputs.

To block merges on coverage failures, make `Coverage ≥90%` a required status check in branch rules; the hosted
reporter tests alone are not a coverage measurement.
