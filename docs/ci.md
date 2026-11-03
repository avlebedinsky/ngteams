# Continuous integration

The [Coverage workflow](../.github/workflows/coverage.yml) runs on pull requests, pushes to `main`, merge queues and
manual dispatch. Its `Coverage ≥90%` job runs `scripts/coverage.sh --require-target` over all 17 runtime crates
selected by `coverage-report.py list`. A crate
below 90% in **any** of LLVM's lines, functions or regions fails the job, even if the workspace average or that
crate's saved standalone baseline is higher or lower. Exactly 90% passes. Failed tests, missing dependencies and
missing source mappings also fail; the workflow does not skip packages or tolerate a failed coverage step.
The hardware DMA subcase is excluded in GitHub Actions, as described below; its runtime source still counts.

The workspace has one client, `ngteams`; its UI tests run against synthetic services in `ui-sandbox`.
`proto-lab` tests browser capture/hold behavior against local pages. Typed-construction compile contracts live in
`teams-proto`; production-only mock-origin/registrar contracts live in `teams-trouter`. No tenant account or native
lab session is part of coverage. The lab runner is checked separately by `scripts/test_run_lab.py` using synthetic
commands, locks and holders.

The hosted `Coverage gate tests` job checks the reporter and the wrapper's failure propagation, including 89.99%
in each metric and a second crate whose coverage is lower than the first one's. This fast job does not replace the
full measurement. The coverage artifact contains only `summary.json`, including per-crate counts and gaps. Raw
profiles, screenshots, captures and runtime logs are not uploaded.
The job summary displays each crate's measured lines, functions and regions. If tests fail, the wrapper attempts
to export the current profiles for diagnosis and labels those results as partial; it still exits with the original
test failure. A failed build or export cannot reuse a previous measurement's summary.

## Hosted CPU runner

Both jobs use GitHub-hosted Ubuntu runners. The full measurement runs in the
[Arch test image](../.github/ci/Dockerfile), which supplies WPE WebKit ≥2.54, Weston ≥15, niri, PipeWire, the private
keyring tools and Mesa's software OpenGL/Vulkan drivers. Tests run as a non-root user. The container is privileged
so WebKit can create its own sandbox namespaces, and `/dev/dri` is hidden explicitly. It runs on a disposable
hosted VM, with only the checkout mounted; no self-hosted runner, personal files or tenant credentials are used.
The entrypoint also creates a fresh device filesystem: privileged container runtimes can otherwise repopulate DRM
nodes after mounting `/dev/dri`. Only the container's shared-memory mount is carried into that device filesystem.
Ubuntu 24.04 also restricts unprivileged user namespaces on the host, including those requested from within the
container. The workflow disables that restriction only for the disposable VM's current boot, using the setting
documented in [Ubuntu's release notes](https://documentation.ubuntu.com/release-notes/24.04/#unprivileged-user-namespace-restrictions).
Chrome's and WebKit's internal sandboxes remain enabled. Before compiling Rust, a preflight checks user-ID mapping
and bubblewrap; after installing Chrome, it renders a synthetic page with no network or desktop access. These
checks fail early with diagnostics if the hosted environment cannot support the browser tests.

`TEAMS_WPE_TEST_SKIP_DMA=1` excludes only the native WPE test's real DMA-buffer subcase. Its worker and in-process
SHM cases still run, as do the other native GUI, media and browser tests. Local runs leave this variable unset and
continue requiring actual DMA frames. This flag excludes a hardware test, not source files: the 90% gate still
measures all runtime crates and keeps the DMA code in the denominator.

CI selects `WGPU_BACKEND=vulkan` so GPUI uses Mesa's Lavapipe software adapter without probing unavailable GLES
devices. The sandbox forwards this graphics setting alongside its existing small environment allowlist.

The image installs the repository's pinned Rust toolchain and LLVM tools plus `cargo-llvm-cov` 0.9.1. The entry
script downloads checksum-verified Chrome using the normal lab CLI, then executes the full strict coverage gate.
Before measurement it also runs the patched Wayland clipboard's ownership regressions; these vendor-library tests
are separate from the runtime-crate selection. The native UI test still exercises copy, cut and paste end to end.
The image sets `TZ=Etc/UTC` so calendar fixtures do not depend on the host's `/etc/localtime` configuration.
Tool installation uses the network; test traffic stays synthetic and offline from the tenant. No lab configuration
or secret is required. The workflow frees unused hosted SDKs to make space for the instrumented native binaries.
The one-shot build disables incremental caches and DWARF debug information while retaining LLVM coverage maps and
debug assertions. It also removes the temporary Chrome-installer build before compiling instrumented tests.
A historical instrumented build with these settings measured about 23 GiB locally, before temporary test outputs;
actual size depends on the selected sources and dependencies.

To block merges on coverage failures, make `Coverage ≥90%` a required status check in branch rules; the hosted
reporter tests alone are not a coverage measurement.
