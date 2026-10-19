# Native Teams for Linux — M0: Lab and Risk Spikes

## Overview
- A native Microsoft Teams client for Linux in Rust + GPUI, aiming at full parity including large meetings. Work/school accounts in the commercial cloud; personal use in a private repo.
- Neither official client has native UI code to port. Both the Windows MSIX and the macOS pkg are a native shell around the same React web app plus the closed SlimCore media stack. So this is a new client written against Teams' internal APIs, with the web client as the reference implementation.
- This plan covers **M0**: an autonomous protocol lab with a hard write fence, plus three spikes on the riskiest assumptions (native auth against the real tenant, video rendering in GPUI, an audio call through Teams media). M1–M5 are listed as a roadmap at the end; each gets its own plan.
- Hard requirement: autonomy. The only manual step is MFA once per lab account. Browser scenarios, captures and tests run without a human.

## Context (from discovery)
- Project dir `/home/nick/develop/projects/my/ms-teams`: not a git repo; it contains only `MicrosoftTeams.pkg` (~348 MB) and `MSTeams-x64.msix` (~287 MB).
- Windows MSIX (analyzed with rea, 2026-10-19):
  - `ms-teams.exe` (C++, WebView2 host)
  - `desktop-assets/`: the offline React web app, ~6,900 `.js.gz` chunks, 213 MB, plus `serve-configs/*.json` and `maglev-configs/*.json`
  - `client-bindings/`: `scwv2_bundle.js`, `calling_electron_bundle.js`, `rt_js_bindings_bundle.js`, `bundle.js`. These are the JS bindings to the native media stack: `skypert.dll`, `Rtm*.dll`, `SlimCoreWebview2.dll`, `SlimCV.dll`.
  - `ic3rustcore.dll`: Microsoft's own Rust code, DTLS 1.2/1.3 on `str0m-wincrypto` + `dimpl`
  - `onnxruntime.dll`: video effects
- macOS pkg:
  - the `MSTeams` shell, `MSWebView2.framework`, and the `Microsoft Teams WebView.app` helper (854 MB, Chromium)
  - the same SlimCore dylibs
  - no bundled web assets
  - the payload is pbzx (xz chunks) wrapping a cpio archive
- Backend hosts seen in the bundles:
  - `chatsvcagg.teams.microsoft.com`
  - `notifications.skype.net` (Trouter)
  - `*.presence.teams.microsoft.com`
  - `*.asm.skype.com`
  - `api.spaces.skype.com`
  - `config.teams.microsoft.com`
  - `calling.web.skype.com`
  - Microsoft Graph
  - more regional hosts are handed out at runtime by the authz response
- Host machine:
  - niri (Wayland), PipeWire 1.6.9, xdg-desktop-portal-gnome
  - Intel Iris Xe + RTX 4070 Mobile
  - ffmpeg 9, openh264 2.6, libvpx, opus, libva, webkit2gtk-4.1
  - rustc 1.98.1
  - **no Chromium/Chrome installed**
- Lab: 3 dedicated test accounts (no human uses them) in the user's real company tenant, which has other members.

## Development Approach
- **testing approach**: TDD (tests first)
- complete each task fully before moving to the next
- make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
  - tests are not optional - they are a required part of the checklist
  - write unit tests for new functions/methods
  - write unit tests for modified functions/methods
  - add new test cases for new code paths
  - update existing test cases if behavior changes
  - tests cover both success and error scenarios
- **CRITICAL: all tests must pass before starting next task** - no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- run tests after each change
- **nothing runs live against the tenant before the fence's browser layer is armed (Tasks 6–7).** Until the write-target mapping exists (Task 11), the fence runs **fail-closed**: every mutating request is denied except a minimal seeded own-state list (Entra token endpoint, authz).
- spikes (Tasks 15–19) end with a decision write-up in `docs/spikes/`. Spike code lives in `spikes/` and doesn't need production polish, but its measurement code and helpers are tested.

## Testing Strategy
- **unit / replay**: `cargo test --workspace`, offline. HTTP is mocked with `wiremock`. Fixtures in `tests/fixtures/` are sanitized captures.
- **fence suite**: `cargo test -p teams-fence -p proto-lab --test 'fence_*'`. It covers both the **decision** (`Fence::check` denies) and the **enforcement** (the denied request never leaves the browser: `failRequest` fired and the page saw a network error; a denied WS frame was dropped).
- **fixture guard**: a test scans `tests/fixtures/**` and `docs/**` for JWTs, `Bearer`, skypetokens, email addresses and real MRIs, and fails on any hit.
- **live lab**: `cargo test --features live` and `proto-lab` commands against the 3 lab accounts. Opt-in; requires a valid lab session.
- **UI e2e**: there's no UI in M0. From M2 on: GPUI's test harness plus screenshots.

## Progress Tracking
- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- keep plan in sync with actual work done

## Solution Overview

**Approach A: native protocol-first.** We learn each protocol from the Teams web client. Static bundle analysis comes first; an automated CDP capture then confirms the live variant and produces fixtures. Protocol notes are written in our own words in `docs/protocol/`. Meetings stay in the browser until native calls exist. No CEF, no headless web core. An embedded WebKitGTK is used only for tabs, Loop and Whiteboard (M5).

**Target workspace architecture** (M0 creates only what it needs):
```
teams-ui (gpui) ──commands──▶ teams-core ──▶ teams-auth / teams-proto / teams-trouter / teams-call / teams-media
       ▲                                                   │ writes
       └──── observes ──── teams-store (SQLite) ◀──────────┘
teams-fence (write guard, lab builds)   teams-mock (fixture replay, M1)   proto-lab (lab harness CLI)
```
- The UI never touches the network: it reads the store and dispatches commands.
- tokio runs on its own thread and is bridged to the GPUI executor through channels.
- GPUI comes as the pinned crates.io `gpui-pre` release, the same as in agterm-rs, with agterm-rs's patches vendored if we need them (external surfaces for video).

### The write fence (core safety mechanism)

**What counts as a write.** Anything others could notice:
- messages, edits, deletes, reactions, mentions
- typing indicators, read receipts / consumption horizon
- calls, **answering an incoming call**, invites, meetings
- member changes, chat creation, channel posts, bot replies

**The rule.** A write is allowed only if every participant of the target is one of the 3 lab accounts, or a Microsoft service participant the user has explicitly approved (such as the echo bot). Membership is fetched fresh, and anything unknown is denied, including a membership list that is paginated or truncated.

**Layer 1, type-level in Rust:**
- Mutating `teams-proto` methods require a `WritePermit`, which only `Fence::check` can construct.
- Lab binaries have no other way to get one.
- A personal build would use a separate permit provider in its own binary crate. Agents never build or run it, and it isn't part of M0.

**Layer 2, in the lab browser.** Deny by default, armed before any page code runs.
- **HTTP: every host is intercepted.** `Fetch.enable` uses the pattern `*`, and the host list is only an input to the classifier, never an interception filter. Every mutating request (POST, PUT, PATCH, DELETE, including `sendBeacon` and `fetch(keepalive)`) is classified as one of:
  - `Own`: a seeded and reviewed list
  - `Write(target)`: goes through `Fence::check`
  - `Unknown`: denied
- **Targets and workers.** `Target.setAutoAttach({autoAttach: true, waitForDebuggerOnStart: true, flatten: true})`. On every new target (pages, iframes, dedicated, shared and service workers):
  1. enable `Fetch`
  2. install the WS guard with `Runtime.evaluate`
  3. only then `Runtime.runIfWaitingForDebugger`

  If arming fails, the target is closed.
- **The service worker is neutralized.** At the start of every run, unregister the Teams SW and clear its storage (no Background Sync or queued-write replay), and set `Network.setBypassServiceWorker(true)`.
- **WebSocket guard.** Wrappers around `WebSocket.prototype.send` in every scope, including workers:
  - nothing is sent synchronously; the frame is buffered in that socket's queue
  - the verdict is requested through `Runtime.addBinding`
  - the original `send` is replayed **only on an explicit allow**
  - on deny, the frame is dropped and the run fails

  Order within a socket is preserved. `Network.webSocketFrameSent` events only observe traffic and are used for auditing, not enforcement.
- Every deny is appended to `fence.log` (JSONL) and fails the run.

**Account-level side effects.** These are side effects of signing in, not conversation writes:
- **Presence:** signed-in clients report activity on their own, so lab accounts show "Available" during runs. The lab never sets presence or a status message.
- **Trouter and calling-endpoint registration:** makes a lab account reachable by coworkers. An incoming call is **never answered**: the native client ignores it, and in the browser, answering is a write that the fence blocks.

**Login always happens in a browser engine**, as in the official clients. We never draw our own username, password or MFA forms. "Native" means only that the PKCE code exchange, token storage and refresh are done by our Rust code. In the product (M2) the interactive step runs in an embedded WebKitGTK window; in the lab it runs in Chromium over CDP.

**Native login with no clicks (lab).** Each lab account's session lives in a persistent Chromium profile, created once with `proto-lab login` and MFA. `teams-auth` opens the authorize URL in that profile over CDP. The authorization code is taken from the redirect's `Location` header at the `Fetch` response stage, before navigation. This also works when the registered redirect is a custom scheme Chrome won't navigate to.

## Technical Details
- **Lab directories (outside the repo):**
  - `~/.config/teams-lab/lab.toml`: the 3 accounts (alias `a|b|c`, UPN; MRIs filled in after first login) and approved service participants
  - `~/.local/share/teams-lab/`: `chrome/` (pinned Chrome for Testing), `profiles/{a,b,c}/`, `captures/<scenario>/<ts>/`, `bundles/<version>/`, `fence.log`
- **Secrets:** refresh tokens and skypetokens are stored in Secret Service (gnome-keyring) via the `oo7` crate; tests use an in-memory backend. Tokens never reach logs or stdout. Commands print only aud, exp, scopes and tenant.
- **Capture format:** JSONL events (request/response metadata and bodies, WS frames).
  - For calls: injected hooks on `RTCPeerConnection` (`createOffer`/`createAnswer`, `setLocalDescription`/`setRemoteDescription`, `onicecandidate`) plus periodic `getStats()`. `chrome://webrtc-internals` can't be exported over CDP.
  - The sanitizer replaces tokens, emails, MRIs, display names and GUIDs with stable placeholders (`<user:a>`, `<mri:b>`, `<guid:1>`).
  - A fixture is exported only after sanitizing.
- **Redirect URI:** we enumerate the URIs registered for the official client ID (strings and config in `ms-teams.exe`, the Mac OneAuth config, the web bundle). An unregistered URI gives `AADSTS50011`.
  - We prefer a native-client redirect, because a SPA redirect only issues 24-hour refresh tokens.
  - If only a SPA redirect works, we accept a daily silent re-login through the lab profile.
- **Skypetoken:** we exchange the Teams access token through the authz endpoint, which we confirm from the bundle and a capture. The response gives us the regional service endpoints, which we persist.
- **str0m:** it's sans-IO, so the spike owns the UDP/TCP sockets and the clock itself. `str0m` handles ICE, DTLS, SRTP and RTP/RTCP but not codecs, so Opus goes through a separate crate (`opus`/`audiopus`).
- **Spike metrics:**
  - GPUI video: fps, frame time p50/p95, CPU% and VRAM for a 3×3 grid of 1280×720 streams at 30 fps, on the iGPU and the dGPU.
  - Audio: round-trip delay and the detected tone frequency (Goertzel) through the echo service.

## What Goes Where
- **Implementation Steps**: code, tests and docs in this repo, plus lab commands the agent runs on its own.
- **Post-Completion**: steps that need a human (MFA) or a decision.

## Implementation Steps

### Task 1: Bootstrap repo and workspace
**Files:**
- Create: `.gitignore`, `Cargo.toml` (workspace), `rust-toolchain.toml`, `CLAUDE.md`, `README.md`
- Create: `crates/teams-fence/`, `crates/teams-auth/`, `crates/teams-proto/`, `crates/proto-lab/` (skeletons)
- Create: `tests/fixture_guard.rs`, `tests/fixtures/.gitkeep`, `docs/protocol/.gitkeep`, `docs/spikes/.gitkeep`

- [x] `git init` (already a repository); `.gitignore` covers `/target`, `*.pkg`, `*.msix`, `*.har`, `captures/`, `.env*`. The installers stay on disk but are never committed.
- [x] ➕ lint setup identical to agterm-rs, already in the repo:
  - `clippy.toml` (complexity thresholds, unwrap/expect/panic allowed only in tests)
  - `[workspace.lints]` in `Cargo.toml` (`all` + `pedantic` deny, unwrap/expect/panic/todo/dbg deny, unsafe-block lints, `allow_attributes` deny)
  - `rustfmt.toml` (`max_width = 120`)
  - `rust-toolchain.toml` (1.98.1)
  - `scripts/lint-report.sh` (refuses inner `#![allow]`/`#![expect]`, lists complexity `#[expect]`s; scans `crates/` and `spikes/`)
  - `members` is an explicit list, because cargo refuses a glob that matches nothing
- [x] every crate, spikes included, gets `[lints] workspace = true`; add each crate to `members` as it's created; add shared dependency versions to `[workspace.dependencies]`
- [x] `CLAUDE.md` with the hard rules:
  - writes go only through the fence and only within the 3 lab accounts
  - nothing runs live before the browser fence is armed
  - raw captures and tokens never enter the repo
  - agents never build or run a personal build and never receive the user's tokens
  - protocol notes are written in our own words; Microsoft JS is never copied in
  - lint rules, as in agterm-rs:
    - a function over a complexity limit is split, not excused; the only exception is an outer `#[expect(clippy::..., reason = "...")]` on that one item
    - `#[allow]` and inner `#![allow]`/`#![expect]` are refused
    - `clippy.toml` thresholds are lowered, never raised
    - a toolchain bump is a separate change
  - the gate: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `scripts/lint-report.sh`, `cargo test --workspace`
- [x] write the failing fixture-guard test first (a planted JWT in a temp directory must be detected), then implement the scanner
  - the scanner is a library crate, `crates/fixture-guard`, so `proto-lab export` (Task 9) can reuse it. The repo-wide check is `crates/fixture-guard/tests/fixture_guard.rs` rather than a root `tests/fixture_guard.rs`, which would need the workspace root to be a package. Fixtures stay in the root `tests/fixtures/`.
- [x] `cargo test --workspace` passes

### Task 2: Lab config and secret storage
**Files:**
- Create: `crates/proto-lab/src/config.rs`, `crates/teams-auth/src/secrets.rs`
- Create: `crates/proto-lab/tests/config.rs`, `crates/teams-auth/tests/secrets.rs`

- [x] tests first for parsing `lab.toml`:
  - exactly 3 accounts with unique aliases
  - all UPNs in the same tenant domain
  - errors on 0, 2 or 4 accounts, duplicates, or a mixed domain
- [x] implement `LabConfig` (accounts, approved service participants, lab dir paths)
- [x] ➕ write the real `~/.config/teams-lab/lab.toml` (agent, no human):
  - a = labbot, b = labbot2, c = labbot3 in the lab.example.com tenant
  - UPNs taken from the `idTokenUpn` in `~/.local/share/teams-lab/ca-probe-*.json`
  - the approved service participants list contains the echo service (approved by the user on 2026-10-19); its MRI is filled in from the Task 18 capture
  - passwords are never stored in `lab.toml`
- [x] tests for the `SecretStore` trait: put/get/delete per account and audience; `Debug` never prints secrets
- [x] implement `MemorySecretStore` (tests) and `KeyringSecretStore` (`oo7` / Secret Service)
- [x] `cargo test --workspace` passes
  - ➕ `crates/proto-lab/src/lib.rs` added so the integration tests can reach `config`; the binary stays a thin CLI over it
  - ➕ `SecretStore` is async (`fn -> impl Future + Send`), because `oo7` has no blocking API; callers use it as a generic, not `dyn`. Items carry the attributes `application`, `account`, `audience`; the real-keyring test (`#[ignore]`) uses `application = teams-lab-test` and removes its items

### Task 3: teams-fence decision engine
**Files:**
- Create: `crates/teams-fence/src/{lib.rs,target.rs,decision.rs,log.rs}`
- Create: `crates/teams-fence/tests/fence_core.rs`

- [x] tests first, as a table of `WriteKind` × participant sets:
  - only lab accounts → allow
  - lab accounts + an approved service → allow
  - any outside member → deny
  - membership fetch failed, empty, or paginated/truncated → deny
  - org-wide team or channel → deny
  - answering an inbound call from a non-lab caller → deny
- [x] implement `WriteKind` (message, edit, delete, reaction, typing, read receipt, call, call answer, invite, meeting, member change, chat create, channel post, bot reply) and `Target`
- [x] implement `Fence::check(target, &dyn MembershipSource) -> Result<WritePermit, Denied>`. `WritePermit` has a private constructor, and membership is always fetched, never cached. `MembershipSource` returns `Complete(members)` or `Partial`, and `Partial` → deny.
- [x] implement the `fence.log` JSONL writer; a test checks that every deny produces an entry with the reason and no tokens
- [x] `cargo test -p teams-fence` passes
  - ➕ `MembershipSource` is dyn-compatible but async: `fn members(&self, &Scope) -> Pin<Box<dyn Future<Output = Membership> + Send>>`, so `Fence::check` is `async` (the live source in Task 11 fetches over the network). `Membership` has a third variant, `OrgWide`, besides `Complete`/`Partial`; `Partial` carries `FetchFailed(Network|Timeout|Status|Malformed)`, `Paginated` or `Truncated`, with no free text, so nothing from a response can reach the log
  - ➕ `Target` = `WriteKind` + `Scope` (`Conversation`, `Channel`, `Team`, `Call`, `New`) + the participants the write adds (invitees, a new chat's members). The fence judges the fetched members together with the added ones; `Scope::New` isn't fetched, its participants are exactly the added ones, and only `ChatCreate`, `Call` and `Meeting` may name it (any other kind on `Scope::New` → `NotCreatable`); an empty fetch is denied even when lab members are added
  - ➕ the `Roster` (lab and service MRIs) matches MRIs exactly; an account or service without an MRI in `lab.toml` is left out and matches nobody. `fence.log` also records allows (no reason), as the run's audit; an allow that can't be logged is denied. Entries carry kind, scope ids, the added count and the reason, never tokens or the participants' MRIs; scope ids are logged verbatim, and a 1:1 chat id embeds both members' object ids, so `fence.log` isn't sanitized material. The file is created 0600; after a failed append the log refuses further entries, and reopening terminates a half-written last line

### Task 4: Type-level enforcement in teams-proto
**Files:**
- Create: `crates/teams-proto/src/{lib.rs,client.rs,write.rs}`
- Create: `crates/teams-proto/tests/fence_types.rs`, `crates/teams-proto/tests/ui/*.rs` (trybuild)

- [x] tests first with `trybuild`: calling a mutating method without a `WritePermit` doesn't compile, and `WritePermit` can't be constructed outside `teams-fence`
- [x] a `Mutating` request trait in `teams-proto`: every mutating call takes a `WritePermit` bound to one `Target`, and a runtime check verifies the permit's target matches the request's target
- [x] a test checks that a permit issued for conversation X is rejected on a request for conversation Y
- [x] `cargo test -p teams-proto` passes
  - ➕ `Mutating::target()` names the request's one `Target`; `Authorized::new(request, permit)` is the only way to pair them (private fields), checks `permit.target() == request.target()` (kind, scope and added participants) and consumes the permit, so one permit covers one write. `Client::write(request, permit)` builds the `Authorized` before anything reaches the `Transport` trait, which only ever receives an `Authorized`; there is no live transport yet (it comes with the protocol work). The first request is `SendMessage` (a `Message` on a `Conversation`)
  - ➕ the trybuild cases (`tests/ui/`): a write without a permit, a `WritePermit` literal, a `WritePermit::new`, cloning a permit, and an `Authorized` literal all fail to compile

### Task 5: Chrome for Testing and lab profiles
**Files:**
- Create: `crates/proto-lab/src/{browser.rs,cdp.rs,commands/chrome.rs}`
- Create: `crates/proto-lab/tests/browser.rs`

- [x] tests first: building Chromium launch args per profile (user-data-dir, remote-debugging-port, headless/headed, fake-media flags) and verifying a download's checksum
- [x] `proto-lab chrome install`: downloads a pinned Chrome for Testing build into `~/.local/share/teams-lab/chrome/` and verifies its checksum. No root needed.
  - ➕ pinned: Chrome for Testing Stable **155.0.8059.84** linux64 (2026-10-19), SHA-256 `cf0f0498…f8bafcbc`. The CfT JSON endpoints carry no checksums, so the hash was taken at pin time from a download whose MD5 matched GCS's `x-goog-hash`. Installed and verified on the workstation.
- [x] choose the CDP client: `chromiumoxide`, or a thin `tokio-tungstenite` client of our own. It must support flat-mode auto-attach with `waitForDebuggerOnStart` and `Fetch` on worker targets. Record the choice in this plan.
  - ➕ **choice: a thin `tokio-tungstenite` client of our own** (`crates/proto-lab/src/cdp.rs`: one browser websocket, flat sessions, untyped JSON commands and events, nothing done on its own). `chromiumoxide` 0.9.1 is ruled out: its handler answers every `Target.attachedToTarget` with `waitingForDebugger` by sending `runIfWaitingForDebugger` itself, before the caller can arm the target, and it detaches service workers — the opposite of what the fence needs.
  - ➕ found by the test: a **dedicated worker has no `Fetch` domain** (`Fetch.enable` on its session: "wasn't found"); its requests pause on its **page's** session. Task 6 arms dedicated workers through the page's `Fetch`; whether shared and service workers take `Fetch` on their own sessions is for Task 6 to check.
- [x] a test against a local HTML page with a dedicated worker: a new target is held paused until we explicitly call `runIfWaitingForDebugger`
- [x] `cargo test -p proto-lab` passes
- [x] note: `login` and `status` are not here; they need the armed fence (Task 8)
- ➕ every browser leads its own process group (killed whole on drop; `SIGTERM` then `SIGKILL` on close), so the crash handler doesn't outlive it; crash dumps go to the profile (`BREAKPAD_DUMP_LOCATION`). The Chrome test needs Chrome at `$PROTO_LAB_CHROME` or the pinned install, and is skipped without one unless `PROTO_LAB_REQUIRE_CHROME=1`.
- ➕ the profiles `~/.local/share/teams-lab/profiles/{a,b,c}` already exist and are signed in to Microsoft (2026-10-19, through `tools/lab-login`). They were created by Playwright's bundled Chromium from lab-automation (Playwright 1.62.1). Pin Chrome for Testing **no older** than that Chromium, because an older Chrome can't open a profile from a newer one. Otherwise just run `proto-lab login` again, which is automatic (Task 8).

### Task 6: Fence layer 2 — HTTP interception (all hosts, all targets)
**Files:**
- Create: `crates/proto-lab/src/{fence_http.rs,classify.rs,arming.rs}`
- Create: `crates/proto-lab/tests/fence_http.rs`, `crates/proto-lab/tests/fixtures/pages/*` (local test pages)

- [ ] tests first for `classify(method, url, body) -> Own(kind) | Write(kind, target) | Unknown`. The `Own` list is seeded minimally (Entra token endpoint, authz). All other mutating requests are `Unknown` and denied.
- [ ] arming: `Fetch.enable` with pattern `*` on every target. Auto-attach with `waitForDebuggerOnStart`: Fetch + WS guard (Task 7) → `runIfWaitingForDebugger`. If arming fails, close the target.
- [ ] neutralizing the SW at run start: unregister service workers and clear SW storage for Teams origins, plus `Network.setBypassServiceWorker(true)`
- [ ] **enforcement** tests on a local page with a mock server:
  - a POST from the page, from a dedicated worker, from a service worker, a `sendBeacon` and a `fetch(keepalive)` fired during `pagehide`
  - all are denied: the mock server receives **nothing**, and the page sees a network error
- [ ] a test that a GET passes and an allowed `Own` POST reaches the mock server
- [ ] `cargo test -p proto-lab --test fence_http` passes

### Task 7: Fence layer 2 — WebSocket guard (buffer and replay)
**Files:**
- Create: `crates/proto-lab/assets/ws_guard.js`, `crates/proto-lab/src/fence_ws.rs`
- Create: `crates/proto-lab/tests/fence_ws.rs`

- [ ] tests first on a local WS echo server:
  - a denied frame never reaches the server
  - an allowed frame arrives
  - order is preserved across a mix of allow and deny within one socket
  - this works the same in the page, a dedicated worker and a shared worker
- [ ] `ws_guard.js`: wraps `WebSocket.prototype.send` with a per-socket queue, requests the verdict through `Runtime.addBinding`, replays the original `send` only on allow, drops and reports on deny. Installed before any target's code runs (through arming from Task 6).
- [ ] the classifier for WS frames (`Unknown` → deny), with tests on synthetic frames
- [ ] an audit: `Network.webSocketFrameSent` is compared with the guard's verdicts, and any frame sent without an allow fails the run
- [ ] `cargo test -p proto-lab --test fence_ws` passes

### Task 8: Lab login and status under the armed fence
**Files:**
- Create: `crates/proto-lab/src/commands/{login.rs,status.rs}`
- Create: `crates/proto-lab/tests/login.rs`

- [ ] tests first: detecting that the web client has loaded and that a session has expired (on recorded synthetic CDP events)
- [ ] `proto-lab login <a|b|c>`: headless, on the persistent profile, **with the fence armed in fail-closed mode**. Sign-in is automatic, following the proven `tools/lab-login/teams-lab-login.mjs`:
  - **credentials:** email and password come from `/path/to/lab-automation/lab-accounts.json`, with the same rules as its `loadAccounts` (the file must be mode 0600; `passwordEnv` or `password`). They're never printed or logged.
  - **sign-in steps:** the email and password fields are typed only when interactive; the password is sent only once, and a rejection stops the run; "Stay signed in" is answered Yes; the run stops if Microsoft asks to register an MFA method.
  - **if MFA is requested:** none was asked on 2026-10-19. If it ever is, write `status-<alias>.json` with `need-code` and wait for `inbox/<alias>.code`; the agent asks the user for the code.
  - **finishing:** once the session is ready, the web client loads under the armed fence; then record the account's MRI into `lab.toml` and close.
- [ ] tests first for the account-file reader: mode 0600 is enforced, `passwordEnv` vs `password` both work, `Debug` and errors never contain the password
- [ ] `proto-lab status`: headless with the armed fence; prints `ok` / `login needed` per account
- [ ] `cargo test -p proto-lab` passes

### Task 9: Capture recorder and sanitizer
**Files:**
- Create: `crates/proto-lab/src/{capture.rs,sanitize.rs,commands/export.rs}`, `crates/proto-lab/assets/rtc_hooks.js`
- Create: `crates/proto-lab/tests/sanitize.rs`

- [ ] tests first for the sanitizer:
  - JWTs, skypetokens, cookies and `Authorization` are removed
  - emails, MRIs, names and GUIDs become stable placeholders, consistent within one capture
  - sanitizing is idempotent
- [ ] recorder: `Network.*` events plus WS frames plus the fence verdicts → JSONL under `captures/<scenario>/<ts>/` (outside the repo)
- [ ] `rtc_hooks.js` (SDP/ICE/`getStats` capture), installed through the same arming. Write a test on a local page with a loopback `RTCPeerConnection`.
- [ ] `proto-lab export <capture> <fixture-name>`: sanitize, write to `tests/fixtures/`, then run the fixture guard. Nothing is written if the guard fails.
- [ ] `cargo test --workspace` passes

### Task 10: Bundle import and index
**Files:**
- Create: `crates/proto-lab/src/bundles/{import.rs,index.rs}`
- Create: `crates/proto-lab/tests/bundles.rs`, `crates/proto-lab/tests/fixtures/mini-bundle/`

- [ ] tests first on a small synthetic bundle: extract URL templates, hosts, HTTP methods and route constants into an index
- [ ] `proto-lab bundles import <msix>`: unpack `desktop-assets` and `client-bindings` into `bundles/<version>/` (gunzip only, nothing executed) and build the index
- [ ] `proto-lab bundles search <pattern>`: search the index, to help with classification and protocol notes. The agent uses rea (`analyze_javascript_application`, `trace_javascript_semantics`) for deeper analysis.
- [ ] `cargo test -p proto-lab` passes

### Task 11: Scenario runner, `load`, and the fence target mapping
**Files:**
- Create: `crates/proto-lab/src/scenario/{mod.rs,dsl.rs,load.rs,ab_chat.rs}`, `crates/proto-lab/src/membership.rs`, `docs/protocol/fence-targets.md`
- Create: `crates/proto-lab/tests/{scenario_dsl.rs,membership.rs,fence_targets.rs}`

- [ ] tests first for scenario helpers against a local page: wait for a `data-tid` selector, wait for a request matching a pattern, and a timeout gives a clear error naming the step
- [ ] scenario `load` (account A, fail-closed fence): open the web client, wait for it to load, record the traffic. Every denied write goes into the capture as data for mapping.
- [ ] from the bundle index and the `load` capture: find the read-only endpoint for a conversation's/call's member list (including pagination) and the table of write endpoints → `WriteKind` + how to extract the `Target` from the URL or body. Write it up in `docs/protocol/fence-targets.md`.
- [ ] tests first, then the implementation: `LiveMembershipSource` (a read-only request from the same profile; anything paginated or truncated → `Partial`) and the classifier extended with the mapping. Tests use sanitized fixtures.
- [ ] scenario `ab-chat`: A sends a message in the A–B chat (create or open, inside the fence) and B receives it. Before the run, a **live enforcement test**: one A–B request with a forced deny verdict must not reach the server (B doesn't receive it), and the page gets an error. After the run, `fence.log` has no unexpected denies.
- [ ] `proto-lab run <scenario> [--headless]`; any unexpected deny → non-zero exit
- [ ] `cargo test -p proto-lab` passes, including `fence_*`

### Task 12: Auth protocol notes from bundle and capture
**Files:**
- Create: `docs/protocol/auth.md`
- Create: `tests/fixtures/auth/*.json` (sanitized)

- [x] ➕ already settled by the CA probe (`docs/spikes/ca-probe.md`, 2026-10-19):
  - native client `1fec8e78-bce4-4aaf-ab1b-5451cc387264` with redirect `https://login.microsoftonline.com/common/oauth2/nativeclient` (OneAuth in `ms-teams.exe`); its refresh token isn't SPA-limited
  - the web client `5e3ce6c0-…` is a SPA, so it isn't used
  - skypetoken resource `https://api.spaces.skype.com`; endpoints `…/api/authsvc/v1.0/authz` and `…/authzcore`
- [ ] from the bundle and the binaries, determine what's still open:
  - the scopes and audiences of the other services (chatsvc, presence, Graph, Outlook, substrate) and whether one refresh token can get all of them
  - the authz/skypetoken flow (request, response, regional endpoints), and when the client uses `authz` vs `authzcore`
  - the Mac OneAuth config, as a cross-check of the redirect
- [ ] confirm with the `load` capture (account A); export sanitized fixtures
- [ ] `auth.md`: the flow in our own words, an audience × service table, the redirect decision (native vs SPA and refresh-token lifetime; custom scheme → catch `Location`), open questions
- [ ] the fixture guard passes on the new fixtures

### Task 13: teams-auth token core (offline)
**Files:**
- Create: `crates/teams-auth/src/{pkce.rs,token.rs,refresh.rs}`
- Create: `crates/teams-auth/tests/{pkce.rs,token.rs}`

- [ ] tests first:
  - PKCE verifier/challenge against RFC 7636 test vectors
  - building the authorize URL
  - code → token and refresh → token on `wiremock`, using the Task 12 fixtures
  - error paths: `invalid_grant`, `AADSTS50011`, a Conditional Access block (`AADSTS53003` and similar), 429 with `Retry-After`
- [ ] implement PKCE, token exchange and refresh, storing into `SecretStore`
- [ ] `cargo test -p teams-auth` passes

### Task 14: Authorize via the lab profile, skypetoken, CLI (spike 1)
**Files:**
- Create: `crates/teams-auth/src/authz.rs`, `crates/proto-lab/src/commands/auth.rs`, `docs/spikes/auth.md`
- Create: `crates/teams-auth/tests/authz.rs`, `crates/proto-lab/tests/auth_redirect.rs`

- [ ] tests first: extracting `code`/`state`/`error` from a recorded 302 `Location` (https and custom scheme); validating `state`; the skypetoken exchange and parsing the regional endpoints on `wiremock`
- [ ] the authorize step: CDP opens the URL in profile X with the fence armed; `Fetch` at the response stage catches the redirect to the registered URI, takes the code, and fails the navigation. No human. ➕ The flow itself is proven by `tools/ca-probe/ca-probe.mjs` (Playwright, catching the code on navigation to `nativeclient`): all 3 accounts get a token for `api.spaces.skype.com` with no MFA and no device ID, and refresh works. This task carries it over to Rust/CDP; the Conditional Access question is already answered for the lab accounts.
- [ ] the skypetoken exchange via authz; persist the regional endpoints
- [ ] `proto-lab auth <a|b|c>` prints aud, exp, scopes, tenant, skypetoken expiry and region, never the tokens themselves
- [ ] live test (`--features live`): all 3 accounts get a skypetoken. Write the outcome, including what Conditional Access did, to `docs/spikes/auth.md`.
- [ ] `cargo test -p teams-auth -p proto-lab` passes

### Task 15: GPUI on niri smoke check
**Files:**
- Create: `spikes/gpui-video/{Cargo.toml,src/main.rs}`

- [ ] take GPUI the way agterm-rs does: the crates.io `gpui-pre` releases rather than a Zed git rev, so the patches from agterm-rs apply as they are. Check its experience with tracing: calloop requests tracing's `log` feature, which inflates `cognitive_complexity` in every tracing macro. agterm-rs vendors calloop without that request (`scripts/vendor-calloop.sh`, `scripts/check-tracing-features.sh`); take the same approach if clippy hits it.
- [ ] stock GPUI opens a window on niri/Wayland and renders text plus an image on the iGPU and the dGPU (`DRI_PRIME` / Vulkan device selection)
- [ ] a test (with `#[ignore]` when there's no Wayland session): the window starts and closes within N seconds without errors
- [ ] if there's a windowing blocker, add ⚠️ to the plan and stop before Task 16

### Task 16: GPUI video rendering spike (spike 2)
**Files:**
- Create: `spikes/gpui-video/src/{frames.rs,bench.rs,grid.rs}`, `docs/spikes/gpui-video.md`
- Create: `spikes/gpui-video/tests/frames.rs`

- [ ] tests first for the synthetic frame generator (1280×720 NV12/BGRA, with a burned-in frame counter that can be read back from the pixels) and the stats aggregator (p50/p95, fps)
- [ ] approach (a): stock GPUI, uploading each frame as an image into a 3×3 grid; measure on the iGPU and the dGPU
- [ ] approach (b), only if (a) misses the target: patching GPUI is acceptable (user's decision, 2026-10-19). Start from agterm-rs's existing patch rather than writing our own:
  - `build/patches/gpui-pre/0001-external-surfaces.patch` and `build/patches/gpui-pre-wgpu/0001`: `Window::paint_external_surface`, dmabuf import through wgpu-hal Vulkan, CPU frames uploaded by damaged rectangles
  - vendored by `scripts/vendor-gpui.sh` from a crates.io archive with a verified checksum into `vendor/` + `[patch.crates-io]`, checked with `--check`
  - on top of it we may need NV12→RGB in a shader and zero-copy from VA-API/NVDEC dmabufs
  - measure
- [ ] `docs/spikes/gpui-video.md`: the numbers, the chosen approach, the cost of maintaining the patch
- [ ] target: 9 × 720p at 30 fps with p95 frame time under 33 ms. If we miss it, add ⚠️ and the decision to this plan.

### Task 17: Offline str0m media loop (spike 3a)
**Files:**
- Create: `spikes/media-loop/{Cargo.toml,src/lib.rs,src/tone.rs,src/goertzel.rs,src/peer.rs}`
- Create: `spikes/media-loop/tests/loopback.rs`

- [ ] tests first: the tone generator → Goertzel detects its frequency; an Opus encode/decode round trip (`opus`/`audiopus`) keeps the tone within tolerance
- [ ] two `str0m` peers in one process. We drive the UDP sockets and the clock ourselves (sans-IO), and Opus payloads go out as RTP through `str0m` (ICE, DTLS, SRTP).
- [ ] the loopback test checks that the received frequency equals the sent one and records the latency
- [ ] `cargo test -p media-loop` passes

### Task 18: Echo call signaling capture and notes (spike 3b, part 1)
**Files:**
- Create: `docs/protocol/calling-echo.md`, `crates/proto-lab/src/scenario/echo_call.rs`
- Create: `tests/fixtures/calling/echo/*.json` (sanitized)

- [ ] gate: if the echo service participant isn't approved in `lab.toml`, stop and ask the user (see Post-Completion)
- [ ] scenario `echo-call`: account A's lab Chromium places the test call with fake audio (a WAV tone). The fence must allow the call (A + approved echo bot). The capture records the HTTP/WS signaling and the `rtc_hooks` data (SDP, ICE, codecs, `getStats`).
- [ ] `calling-echo.md`: the order of signaling steps, SDP details (codecs, extensions, BUNDLE, rtcp-mux), the TURN credential flow, and how the endpoint is registered. Document that incoming calls are never answered.
- [ ] export sanitized fixtures; the fixture guard passes

### Task 19: Native echo call (spike 3b, part 2)
**Files:**
- Create: `spikes/echo-call/{Cargo.toml,src/main.rs,src/signaling.rs,src/turn.rs}`, `docs/spikes/echo-call.md`
- Create: `spikes/echo-call/tests/{signaling.rs,turn.rs}`

- [ ] tests first: parse and build the signaling messages from the fixtures; parse TURN allocate/refresh/channel-bind exchanges (RFC 8656) on recorded and synthetic packets; an incoming-call event is ignored, never answered
- [ ] native client: skypetoken (Task 14) → start the echo call through the fence (`WriteKind::Call`, a permit for A + echo) → `str0m` + our own TURN client over TCP 443 when UDP fails → send an Opus tone → detect it in the echo
- [ ] `docs/spikes/echo-call.md`: whether it worked, the round-trip delay, which transport was used, open questions for M3
- [ ] `cargo test --workspace` passes

### Task 20: Verify M0 acceptance criteria
- [ ] `proto-lab status` is `ok` for a, b and c; `proto-lab auth` gets a skypetoken for all 3
- [ ] the fence suite (decision + enforcement, HTTP + WS, page + workers + SW + beacon) passes; `proto-lab run ab-chat` passes, including the live enforcement test
- [ ] GPUI runs on niri and meets 9×720p30, or the decision and patch are documented
- [ ] the echo call returns our tone, or the blocker and next step are documented
- [ ] the full gate passes: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `scripts/lint-report.sh`, `cargo test --workspace`; the fixture guard is green

### Task 21: [Final] Update documentation
- [ ] README.md: what this is, the lab layout, `proto-lab` commands, how to run tests
- [ ] CLAUDE.md: patterns discovered during M0 (fence arming, fixtures, scenarios)
- [ ] write the M1 plan (headless chat core), informed by the spike results
- [ ] move this plan to `docs/plans/completed/`

## Roadmap after M0 (each milestone gets its own plan)
- **M1 — headless chat core:** `teams-store` (SQLite), `teams-trouter`, chatsvc (conversations, messages, send/edit/delete/react, rich text, mentions, attachments), an outbox with `clientmessageid` dedupe, catch-up sync, `teams-mock`. *Exit:* A→B delivery in under 2 s, covering edits, reactions and a 3-person group chat, automated.
- **M2 — GPUI daily driver:** a sign-in window on an embedded WebKitGTK webview, the same way the official clients use WebView2/WKWebView: Microsoft's page handles MFA and Conditional Access, and the webview catches the redirect to the official client's native redirect URI. Then chat list, chat view and compose, desktop notifications, presence, search, teams and channels, files and calendar (Graph), Adaptive Cards, tray. *Exit:* GPUI UI tests plus screenshots against the live lab. From here the user can switch to a personal build.
- **M3 — calls:** 1:1 and group audio, video and screen share (portal ScreenCast), TURN/TCP, `webrtc-audio-processing`, jitter buffer + PLC, H.264 via VA-API/NVENC + openh264. *Exit:* A↔B media assertions pass; a 3-way video call is stable for 10 minutes.
- **M4 — meetings:** joining scheduled meetings, lobby, roster, raise hand, meeting chat, simulcast for large meetings, content sharing. *Exit:* A schedules with B and C, all three join natively, roster and media assertions pass.
- **M5 — long tail:** breakout rooms, live captions, recording indicator, background blur (ONNX), PSTN, `teams-webhost` (WebKitGTK + JS SDK shim).
- **Cross-cutting:**
  - resilience: proactive refresh, 401 → refresh + retry, 429 handling, Trouter reconnect + catch-up, `Unknown(String)` + `serde(flatten)` tolerance, an "open in browser" fallback
  - drift detection: scheduled `bundles fetch` + a `bundles diff` report (deferred from M0)

## Post-Completion
*Items requiring a human or a decision — informational only.*

**Manual steps:**
- Create `~/.config/teams-lab/lab.toml` with the 3 account UPNs, or hand them over and the agent writes it.
- None in the normal case: login is automatic, and no MFA was asked on 2026-10-19. If Microsoft ever asks for a 2FA code, the agent asks the user for it.
- Before an autonomous run, the user adds permission rules for `proto-lab` and the lab tools, because the auto-mode classifier blocks commands that use lab credentials.

**Decisions made by the user (2026-10-19):**
- The Teams echo service is approved as a fence participant (only it, by MRI).
- Lab accounts showing "Available" and being reachable by coworkers' calls (never answered) is acceptable.
- Conditional Access doesn't block the native client for the lab accounts (CA probe). The user's own account is a separate check, before M2.
