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

- [x] tests first for `classify(method, url, body) -> Own(kind) | Write(kind, target) | Unknown`. The `Own` list is seeded minimally (Entra token endpoint, authz). All other mutating requests are `Unknown` and denied.
  - ➕ `Classification` also has `Read` (`GET`/`HEAD`/`OPTIONS`; every other method, unknown ones included, counts as mutating), and `Write(Target)` carries the kind inside the `Target`. Own rules match method, exact origin and path segment by segment (`*` = one non-empty segment). Seeded: `POST https://login.microsoftonline.com/*/oauth2/v2.0/token`, and `POST` to `authz` and `authzcore` on `teams.microsoft.com/api/authsvc/v1.0/` and `authsvc.teams.microsoft.com/v1.0/` (the commercial-cloud URLs the web bundle names). `Classifier::with_own` adds a rule; the tests use it for a local stand-in.
  - ➕ every request the browser fence decides without a membership check goes to `fence.log` too: `FenceLog::record_request` (teams-fence) logs channel, method and the URL without query, fragment or user info; own allows are logged, and an own allow that can't be logged is denied. Writes are logged by `Fence::check` as before.
- [x] arming: `Fetch.enable` with pattern `*` on every target. Auto-attach with `waitForDebuggerOnStart`: Fetch + WS guard (Task 7) → `runIfWaitingForDebugger`. If arming fails, close the target.
  - ➕ checked on Chrome 155: **shared and service workers take `Fetch` on their own sessions** and their requests pause there; a dedicated worker has none (its requests pause on its page's session); a shared worker can't start a nested worker (`Worker` is undefined there). Page-level auto-attach also attaches every service worker in the page's scope, so a service worker gets one session per page plus the browser's; each is armed.
  - ➕ ⚠️ found by the test: with per-target `Fetch` alone, a `sendBeacon` / `fetch(keepalive)` fired from `pagehide` **when the tab closes reaches the network**, because the tab's session is gone. So arming also enables `Fetch` with `*` on the **browser** session first, which pauses every request of every target (after the target's own session). Both layers run the same decision; a deny at either fails the request.
  - ➕ arming steps per type: page/iframe `Fetch.enable`, `Network.enable`, `Network.setBypassServiceWorker`, `Target.setAutoAttach`; dedicated worker only `Target.setAutoAttach`; shared/service worker and any unknown type `Fetch.enable`, `Target.setAutoAttach`; then `Runtime.runIfWaitingForDebugger`. A failed step closes the target and fails the run; a session that is gone mid-arming ("Session with given id not found": its tab closed, or a duplicate session of a service worker) is dropped without closing the target. The WS guard (Task 7) slots in as one more step before the resume.
- [x] neutralizing the SW at run start: unregister service workers and clear SW storage for Teams origins, plus `Network.setBypassServiceWorker(true)`
  - ➕ Chrome 155 answers `Storage.clearDataForOrigin` on the browser session with "Internal error", so arming opens a blank armed tab, clears `service_workers,cache_storage` for `https://teams.microsoft.com` and `https://teams.cloud.microsoft` through it, and closes it. `Network.setBypassServiceWorker` has no effect without `Network.enable` on the same session (checked: the page's fetch was still answered by the service worker).
- [x] **enforcement** tests on a local page with a mock server:
  - a POST from the page, from a dedicated worker, from a service worker, a `sendBeacon` and a `fetch(keepalive)` fired during `pagehide`
  - all are denied: the mock server receives **nothing**, and the page sees a network error
  - ➕ also a shared worker, and `pagehide` both on navigating away and on closing the tab. Plus a test that leftover service workers and caches are gone at run start and that a service worker registered during the run doesn't answer the page's requests, and fake-browser tests for the arming order, closing an unarmable target, and answering each paused request on its own session. Mutation-checked: without the browser-level `Fetch`, the tab-close beacon reaches the server and the test fails.
- [x] a test that a GET passes and an allowed `Own` POST reaches the mock server
- [x] `cargo test -p proto-lab --test fence_http` passes

### Task 7: Fence layer 2 — WebSocket guard (buffer and replay)
**Files:**
- Create: `crates/proto-lab/assets/ws_guard.js`, `crates/proto-lab/src/fence_ws.rs`
- Create: `crates/proto-lab/tests/fence_ws.rs`

- [x] tests first on a local WS echo server:
  - a denied frame never reaches the server
  - an allowed frame arrives
  - order is preserved across a mix of allow and deny within one socket
  - this works the same in the page, a dedicated worker and a shared worker
  - ➕ and in a service worker. Each scope also checks: a `send` before `open` throws `InvalidStateError`, the binding and `WebSocketStream` aren't reachable, the resolver ignores a call without the token, a poisoned `JSON.stringify` can't change what is asked, and a `Blob` is read before it is decided (the frames after it wait). A direct guard test in an unfenced throwaway browser answers out of order and checks frames leave in send order, only on allow, decided once, with bytes copied at `send`. Mutation-checked: a guard that sends at once, replays as decided, uses the page's `JSON.stringify` or leaves the binding on the global, a worker without the guard, and a page without `Network.enable` all fail the tests.
- [x] `ws_guard.js`: wraps `WebSocket.prototype.send` with a per-socket queue, requests the verdict through `Runtime.addBinding`, replays the original `send` only on allow, drops and reports on deny. Installed before any target's code runs (through arming from Task 6).
  - ➕ the fence answers by calling a non-writable, non-configurable global resolver with a per-run random token in the frame's context (`Runtime.evaluate` with its `contextId`); the guard takes the binding off the global before page code runs, captures every built-in it uses, copies `ArrayBuffer`/view bytes at `send`, and removes `WebSocketStream`. Arming steps: pages and iframes `Runtime.enable`, `Page.enable`, `Runtime.addBinding`, `Page.addScriptToEvaluateOnNewDocument` with `runImmediately`; workers `Network.enable`, `Runtime.addBinding`, `Runtime.evaluate` (must report `armed`); unknown types the same with `Runtime.enable` (the browser's own pages need it). Every arming step now times out after 10 s (the target is closed and the run fails).
  - ➕ ⚠️ found on Chrome 155: a service worker attached twice while paused (through the browser and through a page in its scope) answers no `Network` or `Runtime` command on either session until it runs, so it could not be guarded before its code. Target-level auto-attach now leaves service workers out (`filter`), so each attaches once, through the browser, and is armed there. A binding reaches a page's new contexts only with `Runtime.enable` on, and new-document scripts run only with `Page.enable` on.
  - ➕ known differences from the plain `send`: a waiting frame doesn't count towards `bufferedAmount`, and frames still waiting when `close()` is called are dropped.
- [x] the classifier for WS frames (`Unknown` → deny), with tests on synthetic frames
  - ➕ `FrameClassifier`: own rules match the socket URL (origin + segment path, as HTTP) and a text prefix; binary and unreadable frames match none; the seeded list is empty, so every frame is denied until writes are mapped (Task 11). Frames go through the same decision as HTTP (`fence_http::judge`, `Verdict` renamed from `HttpVerdict`), logged as channel `web_socket`, method `SEND`, URL without query, never the content.
- [x] an audit: `Network.webSocketFrameSent` is compared with the guard's verdicts, and any frame sent without an allow fails the run
  - ➕ each allow covers one frame with the same opcode and content on the same session; a frame no allow covers is a breach: `fence.log` gets `verdict: breach`, `reason: sent_without_allow` (new `RequestOutcome::SentWithoutAllow` in teams-fence), and `RunFailed` lists it. Tested live: a frame sent from an isolated world (where the guard isn't) reaches the server and fails the run.
- [x] `cargo test -p proto-lab --test fence_ws` passes

### Task 8: Lab login and status under the armed fence
**Files:**
- Create: `crates/proto-lab/src/commands/{login.rs,status.rs}`
- Create: `crates/proto-lab/tests/login.rs`

- [x] tests first: detecting that the web client has loaded and that a session has expired (on recorded synthetic CDP events)
  - ➕ `proto_lab::session::SessionWatch` reads the connection's events (a new `ArmedBrowser::watch()` hands out a copy of every event; the fence still handles them itself). **Loaded**: the tab's main frame is on a Teams origin and a `POST` to Teams' authz endpoint got a 2xx in that document. **Sign-in shown** (session expired): the main frame finished loading on a Microsoft sign-in host; a signed-in profile passes through that host by redirect, which commits no document there. **Refused**: the main frame is on Teams and authz answered 403 (a signed-in account Teams won't serve; a 401 isn't one, the client refreshes its token); a refusal counts after standing 5 s. Child frames, other tabs and out-of-process iframes don't move the state; preflights and requests nobody saw don't count; a new main-frame document starts over. The identity is read from the claims (`oid`, `tid`, `upn`/`preferred_username`/`unique_name`) of `Authorization: Bearer` JWTs the page sends; only those claims are kept, and an account whose tokens name two object ids gets none. The MRI is `8:orgid:<oid>`, only for a GUID-shaped oid.
- [x] `proto-lab login <a|b|c>`: headless, on the persistent profile, **with the fence armed in fail-closed mode**. Sign-in is automatic, following the proven `tools/lab-login/teams-lab-login.mjs`:
  - **credentials:** email and password come from `/path/to/lab-automation/lab-accounts.json`, with the same rules as its `loadAccounts` (the file must be mode 0600; `passwordEnv` or `password`). They're never printed or logged.
  - **sign-in steps:** the email and password fields are typed only when interactive; the password is sent only once, and a rejection stops the run; "Stay signed in" is answered Yes; the run stops if Microsoft asks to register an MFA method.
  - **if MFA is requested:** none was asked on 2026-10-19. If it ever is, write `status-<alias>.json` with `need-code` and wait for `inbox/<alias>.code`; the agent asks the user for the code.
    - ➕ ⚠️ **superseded (review fix, decided with Codex): a one-time-code prompt stops the run like every other MFA prompt** (`SignInStop::MfaCode`); there is no `need-code`, inbox or `--code-wait`. Entering a code needs Entra's `SAS/*` posts (BeginAuth, EndAuth, ProcessAuth), which `Classifier::sign_in` denies, so the code could never be submitted. They stay denied: no MFA exchange has been captured, BeginAuth starts a challenge (a text, a call, a push to a phone) as soon as the page shows the prompt, and whether verifying a method can be told apart from registering one is unestablished (the classifier ignores bodies, and an own allow skips the membership check). Deferred: `docs/backlog/one-time-code-sign-in-under-the-fence.md`.
  - **finishing:** once the session is ready, the web client loads under the armed fence; then record the account's MRI into `lab.toml` and close.
  - ➕ `crates/proto-lab/src/{session.rs,accounts.rs}` hold the session watch, the armed lab session (`LabSession`: pinned Chrome, `Fence::open_lab`, `FailClosed`, one armed tab) and the account-file reader; the commands are thin over them. The sign-in step is a pure `next_action(probe, progress)` in the script's order over what an in-page probe finds (it marks the one usable element of each kind with `data-proto-lab`, so a hidden twin is never typed into); typing is `Input.insertText` a character at a time. Any MFA prompt stops the run: registering a method, picking one, approving in an app, and (superseding the earlier `--code-wait` design, see above) a one-time code. The page is looked at at most every 700 ms, and a click (Next, "Stay signed in?", another account's tile) is never repeated on the same main-frame document (`Clicks`, `SessionWatch::document`), so a form whose post is on its way isn't submitted twice. Each state goes to `status-<alias>.json` (0600). The account file's email must equal the account's UPN in `lab.toml`. The MRI goes into `lab.toml` through `teams_fence::lab::record_account_mri`: only for an account without one (another MRI there is refused, not replaced), comments and mode kept, re-validated, replaced by rename; `lab.toml` now also refuses one MRI on two participants.
  - ➕ **the fence lets Entra's sign-in form through for `login` only** (`Classifier::sign_in()`: `POST` on `https://login.microsoftonline.com` to `/*/GetCredentialType`, `/*/login`, `/kmsi`, `/*/kmsi`, own kind `entra_sign_in`). Typing a password into Microsoft's page under the fence needs the form's posts, and they change only that account's own sign-in session; nothing of MFA (`SAS/*`) or security info is on it. `status` and the scenarios keep the seeded list. Not exercised live yet: no profile needed a password.
  - ➕ **live 2026-10-19 (normal build, Chrome 155): all three profiles are still signed in to Microsoft** (no sign-in page, no password typed, no MFA), and their MRIs are recorded in `lab.toml` (three distinct `8:orgid:` MRIs from the tokens' claims, file still 0600). ⚠️ **But Teams refuses all three accounts**: authz answers 403 (once 500 on b's first visit) and the web client shows "Enjoy your Teams trial — Start trial" (the Exploratory trial). The accounts have no Teams license, so the web client never loads, and `login` exits 1 saying so. Starting the trial is a change in the company tenant an agent may not make (and the fence would deny it). (blocked: no Teams license on the lab accounts — `docs/backlog/lab-accounts-have-no-teams-license.md`)
  - ➕ fence during those loads (expected, fail-closed): 0–3 requests denied per run, all telemetry `POST`s to `browser.events.data.microsoft.com` and `teams.events.data.microsoft.com` (OneCollector); 0 WebSocket frames denied (the client stops before it opens one); no target unarmed, no frame sent without an allow, no lost connection. Allowed own-state requests: the Entra token endpoint and authz. Teams registers its service worker (`/v2/serviceworker.js`) during the run; it is armed and bypassed like any other.
- [x] tests first for the account-file reader: mode 0600 is enforced, `passwordEnv` vs `password` both work, `Debug` and errors never contain the password
  - ➕ also: the bare-array and `{accounts: [...]}` forms, the `claude-` prefix, unsupported or repeated profiles, a bad email, an empty or unset password, a non-string password and broken JSON (whose errors never quote the file: the JSON is read as plain values first, and a syntax error reports only line and column). The password is `Zeroizing` and moved out of the parsed file; `Debug` shows `<redacted>`.
- [x] `proto-lab status`: headless with the armed fence; prints `ok` / `login needed` per account
  - ➕ also `signed in, but Teams refused the account (authz answered 403)`, and `unknown (...)` with the last authz status for a client that didn't load in time or a page on another host; exits non-zero unless every account is `ok`, and on any fence breach. Live 2026-10-19: all three print the refusal (see above).
- [x] `cargo test -p proto-lab` passes
  - ➕ `tests/login.rs`: 35 tests, among them the sign-in driver live on a local stand-in for the sign-in page in a throwaway armed browser (email, password once, "Stay signed in", a rejected password, an MFA-registration stop, an invisible password field never typed into, no deny caused by the driver).

### Task 9: Capture recorder and sanitizer
**Files:**
- Create: `crates/proto-lab/src/{capture.rs,sanitize.rs,commands/export.rs}`, `crates/proto-lab/assets/rtc_hooks.js`
- Create: `crates/proto-lab/tests/sanitize.rs`
- ➕ also `crates/proto-lab/tests/capture.rs` and `crates/proto-lab/tests/fixtures/pages/{capture,rtc}.html`; `arming.rs` gets the capture steps; `base64` 0.23.1 joins the workspace dependencies

- [x] tests first for the sanitizer:
  - JWTs, skypetokens, cookies and `Authorization` are removed
  - emails, MRIs, names and GUIDs become stable placeholders, consistent within one capture
  - sanitizing is idempotent
  - ➕ secrets become `<token:N>` (a scheme and a cookie's name are kept: `Bearer <token:1>`), found by JWT shape, `Bearer`/`Basic`, any value under a secret-looking key (`*token*`, `*secret*`, `*password*`, `*cookie*`, `*credential*`, `authorization`, `sig`, ICE `pwd`/`ufrag`, TURN `username`, …) in JSON, form/query strings and text, a long value under `code`/`key`/`state`, and any long high-entropy run left; `headersText` is dropped and binary content becomes `<binary:LEN>`. Identities: emails `<user:N>`, MRIs `<mri:N>` (also `28:`, `4:`, `tel:`), conversation ids `19:<thread:N>@…`, GUIDs `<guid:N>` (an MRI shares its GUID's number), tenant domains (`*.onmicrosoft.com`, `*.sharepoint.com`, the lab's) `<domain:N>`, IPs other than loopback `<ip:N>`, names `<name:N>` (learned from name fields across the whole capture, then replaced in every text, any case, as whole words), other personal fields (titles, topics, phones) `<pii:N>`; percent-encoded forms too. The lab's accounts get their alias (`<user:a>`, `<mri:b>`, `<guid:c>`) and approved services their name (`<mri:echo>`) from `lab.toml`. JSON inside strings (bodies, frames, after a `3:::` prefix) is sanitized as JSON. Tests: `crates/proto-lab/tests/sanitize.rs` (also checks the fixture guard finds nothing in a sanitized capture that trips every one of its rules raw)
- [x] recorder: `Network.*` events plus WS frames plus the fence verdicts → JSONL under `captures/<scenario>/<ts>/` (outside the repo)
  - ➕ format (`crates/proto-lab/src/capture.rs`): `events.jsonl`, entries with `seq`, `ts_ms`, `type`, `session`: `cdp` (every `Network.*` event and `Target.attachedToTarget`/`detachedFromTarget`, raw), `request_body` / `response_body` (fetched; not for scripts, styles, images, media, fonts, manifests, or over 8 MiB), `verdict` (`http`: both layers' decisions, a deny with the whole paused request incl. body; `web_socket`: socket, frame and verdict, denied frames' content included), `rtc`. `<ts>` is UTC `YYYYMMDDTHHMMSSZ` (`-2`, … if taken); dir `0700`, file `0600`. A run records when armed with `ArmedBrowser::arm_capturing(…, Capture)`; `arm` is unchanged
  - ➕ live test `crates/proto-lab/tests/capture.rs` (not in the plan's file list) on a local page and echo server: GET allowed on both layers with its body, POST denied on the page's layer with its request, frames allowed/denied in order with sent/received frames
- [x] `rtc_hooks.js` (SDP/ICE/`getStats` capture), installed through the same arming. Write a test on a local page with a loopback `RTCPeerConnection`.
  - ➕ new arming steps `CaptureBinding` (`Runtime.addBinding` `__labCaptureRtc`) and `CaptureHooks` (`Page.addScriptToEvaluateOnNewDocument`, `runImmediately`) on pages and iframes after the guard, skipped when the run doesn't record. The hooks wrap the constructor (proxy) and `createOffer`/`createAnswer`/`setLocalDescription`/`setRemoteDescription` (reporting the description as applied, so an implicit `setLocalDescription()` shows its SDP)/`addIceCandidate`/`close`, listen to `icecandidate` and the state changes, and report `getStats()` every interval (1 s by default) until closed; they only observe. Tested live (loopback data channel between two connections made on the page's first line; mutation-checked: without the step, or without the constructor hook, the test fails)
- [x] `proto-lab export <capture> <fixture-name>`: sanitize, write to `tests/fixtures/`, then run the fixture guard. Nothing is written if the guard fails.
  - ➕ the guard scans the text before anything touches the disk (and once more where the file lands); a refusal names lines and kinds, never values, and leaves an existing fixture as it was. `<fixture-name>` is a relative path (`auth/authz` → `.jsonl` added; a `.json` name gets one pretty JSON array); `<capture>` is a capture dir or its `events.jsonl`, or a path relative to the captures dir; `--fixtures-dir` overrides the repo's `tests/fixtures`. Without `lab.toml` every identity is numbered
- [x] `cargo test --workspace` passes

### Task 10: Bundle import and index
**Files:**
- Create: `crates/proto-lab/src/bundles/{import.rs,index.rs}`
- Create: `crates/proto-lab/tests/bundles.rs`, `crates/proto-lab/tests/fixtures/mini-bundle/`
- ➕ also `crates/proto-lab/src/bundles.rs` (module file) and `crates/proto-lab/src/bundles/literals.rs` (a small string-literal lexer: strings, templates with nested `${…}`, comments, regex vs division)

- [x] tests first on a small synthetic bundle: extract URL templates, hosts, HTTP methods and route constants into an index
- [x] `proto-lab bundles import <msix>`: unpack `desktop-assets` and `client-bindings` into `bundles/<version>/` (gunzip only, nothing executed) and build the index
  - ➕ the version is the manifest's `<Identity Version>`; the index is `bundles/<version>/index.jsonl`; a method is paired with the nearest url/route literal by proximity (a guess, not data flow); strings holding JSON or HTML are looked into. Imported 26260.1717.5229.7049 from `MSTeams-x64.msix` (7,043 files, 908 MB, 6,709 entries; misses 1 of 2,187 quoted https literals a naive grep finds). A debug build inflates very slowly; use `--release` for a real import
- [x] `proto-lab bundles search <pattern>`: search the index, to help with classification and protocol notes. The agent uses rea (`analyze_javascript_application`, `trace_javascript_semantics`) for deeper analysis.
- [x] `cargo test -p proto-lab` passes

### Task 11: Scenario runner, `load`, and the fence target mapping
**Files:**
- Create: `crates/proto-lab/src/scenario/{mod.rs,dsl.rs,load.rs,ab_chat.rs}`, `crates/proto-lab/src/membership.rs`, `docs/protocol/fence-targets.md`
- Create: `crates/proto-lab/tests/{scenario_dsl.rs,membership.rs,fence_targets.rs}`

- [x] tests first for scenario helpers against a local page: wait for a `data-tid` selector, wait for a request matching a pattern, and a timeout gives a clear error naming the step
  - ➕ `proto_lab::scenario::dsl::Driver` over a tab and a watch of the armed connection: `wait_for` (`Selector::Tid` / `Selector::Css`), `wait_for_request` (a `RequestPattern`: method + URL regex; requests sent before the wait count, none is matched twice), `wait_for_failed_request`, `wait_for_failure` (errorText, e.g. `net::ERR_BLOCKED_BY_CLIENT`), `wait_for_text`, `click`, `type_into` (`Input.insertText`), `press_enter`. A timeout is `StepError::Timeout` ("step `<name>`: timed out after … waiting for …"). Tests: `crates/proto-lab/tests/scenario_dsl.rs` (local page in a throwaway armed browser)
- [x] scenario `load` (account A, fail-closed fence): open the web client, wait for it to load, record the traffic. Every denied write goes into the capture as data for mapping.
  - ➕ `crates/proto-lab/src/scenario/load.rs`: `LabSession::open_with` (new `SessionOptions`: classifier, live members or `FailClosed`, capture, display) records into `captures/load/<ts>/`, settles, lingers 20 s once loaded; every deny is expected (`DenyPolicy::AllExpected`), a breach or a client that didn't load fails it. Live 2026-10-20 (`proto-lab run load --headless`, normal build): authz 403 `UserLicenseNotPresentTrialEligible`, no chat service request, 3 telemetry posts denied, exit 1 (the lab accounts had no Teams license then)
  - ➕ live again 2026-10-20 after the licenses (Business Basic): `load: ok`, exit 0; the client reaches the loaded web client (chat service, CSA and middle-tier reads answer 200) and the fail-closed fence denies its unmapped requests (telemetry, middle-tier lookups by `POST`, presence, own properties, the async gateway's token exchanges, `augloop`/`trouter` socket frames). A client still holding a skypetoken skips authz, so `SessionWatch` also counts a 2xx chat service or CSA read in the current document as loaded (unless authz was refused); before that fix `ab-chat` timed out at load

- [x] from the bundle index and the `load` capture: find the read-only endpoint for a conversation's/call's member list (including pagination) and the table of write endpoints → `WriteKind` + how to extract the `Target` from the URL or body. Write it up in `docs/protocol/fence-targets.md`.
  - ➕ checked against the live `load` and `ab-chat` captures (2026-10-20, read locally, nothing exported): the chat service base is `https://<teams origin>/api/chatsvc/amer/` as mapped; **corrected**: the client sends the chat service `Authorization: Bearer` (an Entra token for `https://ic3.teams.office.com`), not a skypetoken, so `ChatCredential` takes the bearer form too and the live read sends it back as such (tests on synthetic data); the client reads threads with `GET v1/threads?view=msnp24Equivalent&threadIds=<id>` (both members inline, no paging or count) and the fence's single read of the a–b chat came back complete; chat create (`POST v1/threads` with both members), message (`RichText/Html`), typing and read receipt are sent as the table says; no endpoint registration or `RegistrationToken`; own properties seen (`messageDensity`, `favorites`, `firstLoginInformation`, `locale`) stay off the own-state list; group-chat reads and `thread_members` paging not seen. The doc's "To confirm by capture" became "What the captures showed"
  - ➕ (first version) from the bundle only (the `load` capture of the unlicensed accounts had no chat traffic). Member reads: `GET v1/threads/<id>?view=msnp24Equivalent` (members inline), `v1/threads/<id>/members?…&pageSize=` (paged), `…/filtered` (a filtered view). Writes on chats mapped: message and typing (by `messagetype`), edit, delete, reaction (`properties?name=emotions`), read receipt (`properties?name=consumptionhorizon`), chat create (`POST v1/threads`, members from the body), member changes; channels, calls, bots, endpoint registration and own properties stay `Unknown`. A "to confirm by capture" list closes the doc
- [x] tests first, then the implementation: `LiveMembershipSource` (a read-only request from the same profile; anything paginated or truncated → `Partial`) and the classifier extended with the mapping. Tests use sanitized fixtures.
  - ➕ as the note below says, in teams-fence: `LiveMembershipSource` (sealed impl, own reqwest client: no proxy, no redirects, `https_only`, 10 s timeout, 1 MiB body cap) reads `GET <base>/v1/threads/<id>?view=msnp24Equivalent` with `Authentication: skypetoken=…`; `ChatCredential::from_request(url, headers)` (redacted `Debug`, `Zeroizing` token) accepts only `https://{teams.microsoft.com,teams.cloud.microsoft}/api/chatsvc/<region>/v1/…` or `https://<name>.msg.teams.microsoft.com/v1/…` with `Authentication: skypetoken=` / `X-Skypetoken`. **Only 1:1 chats** (`19:<oid>_<oid>@unq.gbl.spaces`) are supported: `Complete` needs the answer's `id` to be that thread and its members to be exactly the two `8:orgid:<oid>` the id names (the evidence that the list is whole, not filtered); every other scope is `Partial(Unsupported)` (new), no credential `Partial(NoCredential)` (new). Pagination markers → `Paginated`, a member count above the list or a body over the cap → `Truncated`, anything else off → `Malformed`. Mock origin only under `test-support` (`with_mock_origin`); fence-seal gets `tests/pass/live_source.rs` and `tests/ui/live_mock_origin.rs`. proto-lab: `Classifier::mapped()` (seeded + the write table; `seeded()`/`sign_in()` map nothing), `HttpFence::with_credentials` takes the credential from each `Fetch.requestPaused`'s headers before `PausedRequest` is built. Fixtures (synthetic, sanitized form): `tests/fixtures/chatsvc/{thread-one-to-one,writes}.json`; tests `crates/teams-fence/tests/membership.rs`, `crates/proto-lab/tests/fence_targets.rs`
- [x] scenario `ab-chat`: A sends a message in the A–B chat (create or open, inside the fence) and B receives it. Before the run, a **live enforcement test**: one A–B request with a forced deny verdict must not reach the server (B doesn't receive it), and the page gets an error. After the run, `fence.log` has no unexpected denies.
  - ➕ written (`crates/proto-lab/src/scenario/ab_chat.rs`): both accounts on `Classifier::mapped()` + live members, the chat deep link `teams.microsoft.com/l/chat/0/0?users=<upn>`, a `ForcedDeny` (new: method + URL regex + body text, denied before classification, `fence.log` reason `forced`, `DenyCause::Forced`) on a's probe post, which must fail with `net::ERR_BLOCKED_BY_CLIENT` while b never shows it, then the real message must show on b; denies other than the forced one and telemetry fail it. The forced-deny mechanism is tested offline on a local page (`fence_targets.rs`: the server gets nothing, the page sees a `TypeError`). The compose-box selector is unverified. Not run live then (the lab accounts had no Teams license)
  - ➕ run live 2026-10-20 (`proto-lab run ab-chat --headless`, normal build): both clients load; a opens the chat by b's MRI (`l/chat/0/0?users=<mri>`: by UPN the client needs the middle tier's `fetchShortProfile`, a `POST` the fence denies, and by the chat id a page that loaded before the chat existed refuses it), the client creates the a–b 1:1 chat with `POST v1/threads` (allowed as `ChatCreate`, both lab accounts in the body); **enforcement**: the probe post to the a–b chat was force-denied (`fence.log` reason `forced`) and failed in a's page with `net::ERR_BLOCKED_BY_CLIENT`; **delivery**: a's message, typing and read receipt to the a–b chat were allowed after a complete live member read, and b, opening the chat afterwards, showed the message and not the probe. No write to any other conversation was allowed: the client's writes on the tenant's teams (own member entry and favourite time in a team channel) were denied as `Unknown`. Exit 1 all the same: the scenario's policy counts the client's ~450 unmapped fail-closed denies per run (the same families as `load`) as unexpected until they are reviewed; no mapped write and nothing else on the a–b chat was denied. Found on the way: a failed send stays unsent in the client and goes out again on a later load (an earlier run's probe reached b that way, between lab accounts, after its run was over), so the forced deny now covers any write carrying the probe marker for the whole run, from before the page loads; an iframe target that couldn't be armed in time (`Resume`/`Page` got no answer within 10 s, the target closed, never resumed) shows as a breach in some runs. Changed: `ab_chat.rs` (open by MRI, `send` re-presses Enter until the compose box empties, probe check by the chat id in the denied URL, `failure.png` screenshots in the capture dir), `dsl.rs` (`Driver::screenshot`), `session.rs` (loaded by a service read)
- [x] `proto-lab run <scenario> [--headless]`; any unexpected deny → non-zero exit
  - ➕ `ScenarioReport` fails on a goal not reached, a breach, or a deny its `DenyPolicy` doesn't expect (`load`: all expected; `ab-chat`: forced + `*.events.data.microsoft.com`); `run` prints the notes and exits 1 on failure. Without `--headless` the browsers are visible
- [x] `cargo test -p proto-lab` passes, including `fence_*`
- ➕ **`LiveMembershipSource` moves to teams-fence** (`crates/teams-fence/src/membership.rs`, tests in `crates/teams-fence/tests/membership.rs`), not `crates/proto-lab/src/membership.rs`. `MembershipSource` is sealed (fence-api fix), so only teams-fence can implement it, and the seal is worth keeping only if teams-fence owns where the answer comes from: a transport or CDP session that proto-lab supplies could return a made-up member list (a fake CDP websocket can answer `Runtime.evaluate` with anything), so neither the CDP client nor a caller-supplied transport moves in.
  - teams-fence builds the read request from the `Scope` (scope ids as encoded path segments; it chooses the auth header and scheme), sends it with its own HTTPS client (no proxy, no redirects; hosts parsed and matched against a fixed allowlist of Teams service suffixes in teams-fence, no userinfo, no unexpected port, no caller-chosen path or query), and parses the response. Unsupported scopes, failures, pagination and truncation are `Partial`; start with the scopes `ab-chat` needs.
  - proto-lab supplies only the lab account's credential, captured from the armed page's own requests to that service, into a teams-fence newtype with a redacted `Debug` that never reaches logs. Take it from the `Fetch.requestPaused` event before it becomes a `PausedRequest` (which drops headers and derives `Debug`). A made-up credential gets a 401 and is denied. `Complete` also needs evidence from the capture that the endpoint returns the full membership of exactly that scope, not a view filtered by the caller's permissions.
  - a local mock base URL exists only under `test-support`; a `crates/fence-seal/tests/pass/` case proves a normal build can build the source. Until then the browser fence is armed with `teams_fence::FailClosed` (every fetched scope is `Partial(NoSource)`).
  - if member lists turn out to need browser-bound state a direct request can't reproduce, stop and decide again; don't add a caller-supplied transport. (decision: fixer fence-api r1, with Codex)

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

- [x] take GPUI the way agterm-rs does: the crates.io `gpui-pre` releases rather than a Zed git rev, so the patches from agterm-rs apply as they are. Check its experience with tracing: calloop requests tracing's `log` feature, which inflates `cognitive_complexity` in every tracing macro. agterm-rs vendors calloop without that request (`scripts/vendor-calloop.sh`, `scripts/check-tracing-features.sh`); take the same approach if clippy hits it.
  - `gpui-pre` and `gpui-pre-platform` `=0.3.9` (the newest on crates.io, the same snapshot agterm-rs pins), stock, `wayland` only: no `x11` feature, and the app is built with `WindowingModes::WAYLAND`, so GPUI has no X11 or headless fallback (an unset `WAYLAND_DISPLAY` is refused, an unreachable one panics at startup).
  - calloop 0.14.5 does turn on tracing's `log` in the graph, but no workspace crate calls tracing macros yet, so clippy is clean and nothing is vendored. The first crate that logs with tracing takes agterm-rs's calloop vendoring.
  - ➕ oo7 switched to its async-io flavour (`default-features = false`, `async-std` + `native_crypto`, the set GPUI's Linux platform uses). GPUI's ashpd/zbus refuse `async-io` and `tokio` together, and workspace builds unify features, so oo7's default `tokio` broke every `--workspace` build. The real-keyring test passes on it.
- [x] stock GPUI opens a window on niri/Wayland and renders text plus an image on the iGPU and the dGPU (`DRI_PRIME` / Vulkan device selection)
  - run on a niri 26.04 nested in a private headless weston 15.0.1 (`--renderer=gl --fake-seat`), never on the live session. The screenshot taken through the nested niri shows the text and the checkerboard `RenderImage`.
  - GPU choice: GPUI ranks adapters itself (`ZED_DEVICE_ID` first, then the compositor's GPU from dmabuf feedback, then discrete over integrated), so `DRI_PRIME` has no effect on it. `ZED_DEVICE_ID=0xa7a0` gives Intel Iris Xe (RPL-P, Vulkan, Mesa), `ZED_DEVICE_ID=0x2820` gives the RTX 4070 Laptop GPU (Vulkan, NVIDIA). By default it takes the iGPU, because the compositor runs on it.
- [x] a test (with `#[ignore]` when there's no Wayland session): the window starts and closes within N seconds without errors
  - `spikes/gpui-video/tests/smoke.rs` starts its own weston plus nested niri, so it needs no session and isn't `#[ignore]`d. Without weston ≥ 15 or niri it skips with a note, and `GPUI_SMOKE_REQUIRE=1` makes that a failure. It checks: ready within 30 s, the window listed by niri, the image and text pixels in the screenshot, exit within 5 s of niri's close request with status 0 and no `ERROR` in GPUI's log. This runs on every render-node GPU. It also checks the refusals without a compositor.
- [x] if there's a windowing blocker, add ⚠️ to the plan and stop before Task 16 (no blocker: Task 16 can start)

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

- [x] tests first: the tone generator → Goertzel detects its frequency; an Opus encode/decode round trip (`opus`/`audiopus`) keeps the tone within tolerance
- [x] two `str0m` peers in one process. We drive the UDP sockets and the clock ourselves (sans-IO), and Opus payloads go out as RTP through `str0m` (ICE, DTLS, SRTP).
- [x] the loopback test checks that the received frequency equals the sent one and records the latency
- [x] `cargo test -p media-loop` passes
- ➕ crates: `str0m` 0.24.1 with `default-features = false, features = ["rust-crypto"]` (DTLS via dimpl and SRTP in pure Rust, no OpenSSL, no example web server; dimpl still generates its certificate with rcgen on aws-lc-rs, so the aws-lc-sys C build remains) and `opus` 0.4.0 (libopus 1.x built from source by `opusic-sys`; `audiopus` is unmaintained since 2021). Two extra modules beside the planned ones: `src/codec.rs` (Opus 48 kHz mono, 20 ms frames) and `src/loopback.rs` (the harness and `LatencyStats`).
- ➕ shape for Task 19: `peer::Peer` is sans-IO (datagrams, `now` and SDP in; datagrams, events and its deadline out), so a TURN/TCP transport can drive it; `peer::UdpPeer` puts it on a UDP socket. The loop passes the SDP through its text form, A sends a 440 Hz tone in real time, B decodes it and echoes each Opus packet back, and A decodes the echo.
- ➕ measured on loopback (debug build, 100 frames of 20 ms, latency = A's encode start → decode end): 100/100 frames received and echoed, 440 Hz detected at both ends; ICE + DTLS connect 25–32 ms; one-way p50 2.9–3.1 ms, p95 3.8–6.5 ms; round trip p50 5.1–5.8 ms, p95 6.3–11.4 ms, max 12.9–20.8 ms (two runs). The test bounds round-trip p95 under 100 ms. Those figures included an extra loop turn and idle nap per hop (str0m packetizes a written frame only on its next timeout); after `write_audio` started advancing the clock itself (review round 1), quiet runs give connect 22–28 ms, one-way p50 1.7–1.8 ms, p95 2.1–3.0 ms; round trip p50 2.5–2.8 ms, p95 3.2–4.9 ms (three runs). Runs under heavy parallel build load (load average 15–28) give one-way p50 5–6 ms and round trip p50 9–12 ms with or without the change.

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
