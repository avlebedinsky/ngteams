# Continuous integration

The [Coverage workflow](../.github/workflows/coverage.yml) runs on pull requests, pushes to `main`, merge queues and
manual dispatch. Its `Coverage ≥90%` job runs `scripts/coverage.sh --require-target` over all runtime crates
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

## Windows portability

The [Windows portability workflow](../.github/workflows/windows.yml) checks, lints and runs the offline tests of
`teams-platform`, `teams-log`, `teams-store` and `teams-auth` on Windows Server 2025 (`windows-2025`) and Linux; local verification
also runs on the Windows 11 VM.
These are foundational stages of the native Windows port; the app and its release packages still require the remaining
platform integrations. These jobs do not build or launch `ngteams` or access a tenant.

`teams-platform::private` creates files and directories with Unix modes or native protected Windows ACLs.
Windows tests inspect actual ACLs for files, log rotations, databases, WAL/SHM, run locks, picture caches and
snapshots; the shared SQLite/outbox tests also run natively. Only Unix mode-bit, symlink and Linux `/proc` lock
inspection tests are platform gated. Invalid Unicode tests use unpaired UTF-16 on Windows and invalid UTF-8 on Unix.
Windows filenames cannot contain `?`, so the portable snapshot path test covers the legal URI characters `%` and
`#`; source URI paths normalize Windows directory separators before SQLite opens them.

SQLite's Windows side files use the creating process token's default owner (the Administrators group under an
elevated token); their inherited DACL still grants only the current user and SYSTEM access. The Windows test checks
that owner against the actual process token and verifies both grants. Files created through `teams-platform`
explicitly name the current user as owner. See [Windows object ownership](https://learn.microsoft.com/en-us/windows/win32/secauthz/owner-of-a-new-object).

The Linux coverage inventory records the Windows-only filesystem source with the existing hash-checked
`unmapped_sources` mechanism: it has no Linux LLVM mapping. Native Windows tests exercise that source in the VM
and hosted job; this entry neither removes mapped counts nor changes any existing coverage floor.
Authentication uses Secret Service on Unix and Windows Credential Manager on Windows. The Windows backend selects
its native store explicitly, persists only on this machine, and hashes length-delimited application/account/audience
keys to avoid Credential Manager's case-insensitive-name collisions. Tests use unique synthetic namespaces and exact
entry operations, never enumeration. Third-party credential errors can contain secret bytes, so the adapter reduces
them to fixed error categories and zeroes secret buffers. The keyring-core debug records contain entry metadata,
not values; the app logger ignores this third-party target. Interprocess is used only in byte mode (its message-mode
receive diagnostic is unused and prints a mode number only).

Broker scope, get-only behavior, framing, closure and session contracts run unchanged over Unix sockets or Windows
named pipes. The caller supplies the established stream and is responsible for endpoint permissions. The app's
Windows broker launch/handle inheritance and isolated runner are still pending; this library support does not make
Windows application startup available yet. Named pipes have no half-close, so malformed-request tests close the
complete synthetic peer when testing truncation.

The VM's SSH token cannot access Credential Manager (Windows error 1312); native credential tests and coverage run
through a temporary scheduled task in its existing interactive console session. No user credential is read or used.
Hosted Windows tests likewise require the runner's interactive user profile. Linux uses a private D-Bus and temporary
keyring, with the job installing the test daemon dependencies. The Windows-only credential adapter has a hash-checked
unmapped-source entry for Linux measurement and is measured natively in Windows.

The per-crate Linux gate includes `teams-platform` at 90%. The Windows job additionally measures that crate natively
with `cargo-llvm-cov` 0.9.1 and requires 90% in lines, functions and regions, including its Win32 implementation.
It also measures `teams-auth` natively and enforces that crate's existing floors: 96% lines, 96% functions and
94% regions, including the Windows credential backend.


## Releases

The [release workflow](../.github/workflows/release.yml) runs on every push to `main` and on manual dispatch. Each
run makes a GitHub release tagged `v<workspace version>.<run number>` (`Cargo.toml`'s `[workspace.package] version`
and `github.run_number`, monotonic for package upgrades), marked latest, targeting the pushed commit, from Linux
x86_64 builds on `ubuntu-24.04`: `.deb`, `.rpm`, Arch `.pkg.tar.zst`, `.AppImage`, a portable `.tar.gz` and
`SHA256SUMS`, with GitHub build-provenance attestations over every asset, including the runtime below.

Packages carry no web engine: `ngteams-web` picks the system's WPE WebKit (2.54 or newer) or a downloaded runtime when
it starts. Only `ngteams-web` and the sign-in extension link the engine; `scripts/check-linux-package.py` fails a tree
where `ngteams` or `ngteamsctl` needs one of its libraries or a host library not on its list, any shipped file has an
RPATH or RUNPATH, or the tree holds a library at all. The Arch package depends on the system's engine, deb and rpm
recommend it, and the runtime is the fallback, so the `.deb` and `.rpm` work where the distribution has none.

The runtime is `ngteams-web-engine-<key>-linux-x86_64.tar.zst` (MANIFEST with `key`, `arch` and `wpe_version`,
`lib/`, `lib/wpe-webkit-2.0/`, `lib/gio/modules`, `lib/gstreamer-1.0`, `libexec/gstreamer-1.0/gst-plugin-scanner`).
It is built from ngterm's portable WPE prefix (`scripts/release-wpe-pull.sh`: pulled from GHCR by digest with pinned,
checksum-verified ORAS, unpacked only after `gh attestation verify` ties it to ngterm's `release-wpe-cache.yml` on
`main`; WPE is never built here) plus Ubuntu's GStreamer plugins, GIO modules and library closure (`scripts/web-engine-closure.py`).
Its key hashes the pinned digest, the exact Ubuntu package versions copied and the packaging inputs
(`packaging/linux/gstreamer-plugins.txt`, `runtime-libraries.txt`, and the packaging scripts). It is published once,
on its own `web-engine-<key>` prerelease (never latest), and reused while the key is unchanged: a release compiles
that exact file's URL, size and SHA-256 into the app, so a published archive is never replaced.

`build` runs the tests of the packaging scripts, builds, packages and smoke-tests: the build identity in the archive,
the AppImage and the installed `.deb`, `--install-web-engine` into a private `XDG_DATA_HOME`, `ngteams-web --probe`
on that installed runtime with the build's WPE prefix moved away, and `apt` installing the `.deb`. It holds only a
read token; artifacts move to `publish`, which alone may write.

### Updating the pinned WPE prefix

ngterm's cache workflow publishes a new prefix when its WPE build inputs change. To move to it, take its tag:
`ubuntu-24.04-` followed by the output of `scripts/build-release-wpe.sh --print-key` in ngterm. Resolve its manifest
digest (the tag's anonymous registry answer's `Docker-Content-Digest`), and put the tag and digest in [`pins.env`](../packaging/release/pins.env), keeping
`RELEASE_WPE_PREFIX` as the path it was built at. The runtime's key changes with it, so the next release publishes a new runtime.

### Local checks

`scripts/test_web_engine_closure.py` and `scripts/test_check_linux_package.py` run under
`python3 -B -m unittest discover -s scripts -p 'test_*.py'`; they need `cc`, `ldd`, `patchelf` and `zstd`.
