# ms-teams

A native Microsoft Teams client for Linux in Rust + GPUI, for work/school accounts. Neither official client has native
UI code to port: the Windows MSIX and the macOS pkg are both a native shell around the same React web app plus the
closed SlimCore media stack. So this one is written against Teams' internal APIs, with the web client as the
reference. Everything is learned and tested in a lab of three dedicated accounts, behind a write fence that lets
nothing reach anyone else in the tenant.

Status: **M0 is done**: the protocol lab with the write fence, and the three risk spikes (native sign-in, video in
GPUI, an audio call). The plan is `docs/plans/completed/20261019-native-teams-m0-lab-and-spikes.md`, the outcome
`docs/m0-acceptance.md`. **Next: M1**, the headless chat core
(`docs/plans/20261020-native-teams-m1-headless-chat-core.md`). Hard rules for contributors and agents are in
[CLAUDE.md](CLAUDE.md).

## Layout

| path | contents |
|---|---|
| `crates/teams-fence` | the write fence: a write is allowed only when every participant of its target, read fresh, is a lab account or an approved service; the lab config (`lab.toml`), `fence.log`, the membership sources (`FailClosed`, `LiveMembershipSource`) |
| `crates/fence-seal` | compile checks without `test-support`: no other crate can mint a `WritePermit` or send a request outside the native request policy, and a lab binary has its membership sources |
| `crates/teams-auth` | PKCE, the authorize URL and redirect, the code exchange and refresh, the account check on the ID token, the skypetoken (authz), secret storage (Secret Service through `oo7`) |
| `crates/teams-proto` | Teams internal API requests and the native HTTP transport: reads (`GET`), a sealed own-state list (empty so far, logged to `fence.log`), and mutating requests (`SendMessage`, `StartCall`, `AddToCall`) that need a `WritePermit` for their exact target; a fixed host allowlist, `https` only, no proxy, no redirects |
| `crates/teams-mock` | tests only (a dev-dependency, never a lab binary's): a local chat service built from the synthetic fixtures in `tests/fixtures/chatsvc/`, serving the conversation list, message pages, thread reads and the chat writes, queuing an event for the other members of each write; clients reach it through the `test-support` mock origins |
| `crates/teams-headless` | the headless lab binary: one lab account's native client (`teams-core`) from the command line, its fence from `Fence::open_lab`, its tokens from the Secret Service, its store under the lab's data dir |
| `crates/proto-lab` | the lab harness CLI: Chrome for Testing, the armed browser fence (HTTP and WebSocket), login and status, native auth, captures and the sanitizer, bundle import and search, scenarios |
| `crates/fixture-guard` | scans every `tests/fixtures/` and `docs/` for tokens, signatures, emails, real MRIs and GUIDs, meeting links and passcodes, and checks each fixture's `_synthetic` marker |
| `spikes/gpui-video` | GPUI on niri (smoke window in a private weston + nested niri), then a 3×3 grid of 720p video (M0 Tasks 15–16) |
| `spikes/media-loop` | two `str0m` peers on loopback carrying an Opus tone, checked with Goertzel (M0 Task 17) |
| `spikes/echo-call` | the native echo call: a TURN client (RFC 8656), `str0m` through the relay, the test call's signaling from the capture, the call through the fence (M0 Task 19; the start sends nothing yet, see `docs/spikes/echo-call.md`) |
| `tests/fixtures/` | synthetic fixtures (made-up values, a `_synthetic` marker), as in every `tests/fixtures/` of a crate or spike |
| `docs/protocol/` | protocol notes in our own words: `auth.md`, `fence-targets.md` (chat service reads and writes), `calling-echo.md` |
| `docs/spikes/` | spike write-ups: `ca-probe.md`, `auth.md`, `gpui-video.md` (+ raw runs), `echo-call.md` |
| `docs/backlog/` | deferred work, one file per item |
| `docs/plans/` | milestone plans; finished ones in `docs/plans/completed/` |
| `build/patches/`, `vendor/`, `scripts/vendor-gpui.sh` | GPUI (`gpui-pre` 0.3.9) vendored with agterm-rs's external-surfaces patches, used through `[patch.crates-io]`; `scripts/vendor-gpui.sh --check` verifies it |
| `scripts/lint-report.sh` | refuses inner `#![allow]`/`#![expect]`, lists complexity `#[expect]`s |
| `tools/` | interim Node tools: `ca-probe` (the Conditional Access probe), `lab-login` (the first sign-in of the profiles) |

## The lab

Three dedicated accounts, a, b and c, in the company tenant; nobody uses them but the lab. Everything about them lives
outside the repo:

| where | what |
|---|---|
| `~/.config/teams-lab/lab.toml` | the 3 accounts (alias, UPN, MRI once recorded) and the approved service participants (`echo`, the Teams echo service, with its MRI); optionally `data_dir`. Mode `0600`, never holds a password. `login` and `service-mri` fill in MRIs, nothing else writes it |
| `~/.local/share/teams-lab/` (or `data_dir`) | the lab's data dir |
| `…/chrome/<version>/` | the pinned Chrome for Testing (155.0.8059.84 linux64, checksum-verified) |
| `…/profiles/{a,b,c}/` | each account's persistent Chrome profile, signed in to Microsoft |
| `…/captures/<scenario>/<ts>/events.jsonl` | recorded runs (format in `crates/proto-lab/src/capture.rs`) |
| `…/reference/<name>` | sanitized reference copies made by `export`, read locally to write synthetic fixtures |
| `…/bundles/<version>/` | the unpacked web bundle and its `index.jsonl` |
| `…/fence.log` | JSONL: every fence decision of every run (allows, denies, breaches, with reasons; no tokens, no participants' MRIs, scope ids verbatim), and every target the browser fence couldn't arm (its type, failed step, close and containment). Every lab process appends to it at once, each line under an advisory lock on the file |
| `…/store/<alias>.sqlite` | the account's native store (`teams-headless`): conversations, messages, reactions, the outbox (`0600`) |
| `…/auth/<alias>.json` | the account's region, partition, region GTMs and skypetoken expiry from `auth` (`0600`) |
| `…/status-<alias>.json` | `login`'s steps, and why it stopped |
| `…/media/tone-440.wav` | the fake microphone's tone for `echo-call` |
| Secret Service, application `teams-lab` | refresh tokens and skypetokens, per account |
| lab-automation's `lab-accounts.json` (mode `0600`) | the lab accounts' passwords, read only by `login` |

## Building

`cargo build -p proto-lab` (and `-p teams-headless`, `-p echo-call`) builds a normal lab binary. The tests enable `teams-fence`'s
`test-support` feature through dev-dependencies, and cargo unifies it into everything built together with them
(`cargo test`, `--all-targets`, `--all-features`), the binaries included. Such a binary refuses to run; rebuild with
`cargo build -p <crate>`.

## `proto-lab`

Every command that opens a browser uses the pinned Chrome on the account's profile, with the fence armed before
anything loads, and one browser per profile at a time. `login`, `status` and `auth` always run it headless; `run` does
only with `--headless` (agents always pass it). `chrome` and `bundles` need no `lab.toml`, and neither does `export`,
whose placeholders are then all numbered (with a `lab.toml` the lab accounts get their aliases); an invalid one is
refused all the same. Every other command needs it.

```sh
proto-lab chrome install                  # download the pinned Chrome for Testing into the lab and verify it
proto-lab chrome path                     # print its browser binary
proto-lab login a [--timeout 300]         # sign account a (b, c alike) in to the web client; record its MRI
proto-lab status [a b c] [--timeout 120]  # ok / login needed / refused, per account; exits non-zero unless all ok
proto-lab auth a [--audiences] [--no-skypetoken] [--timeout 120]   # native sign-in, tokens, skypetoken
proto-lab run load|ab-chat|echo-call --headless [--timeout 180]    # a scenario under the armed fence
proto-lab export <capture> <name> [--strict]                       # a capture -> a local reference copy
proto-lab bundles import <msix>           # unpack the web bundle from the Teams MSIX and index it
proto-lab bundles search <regex> [--kind url|host|route|method] [--version <v>]
proto-lab service-mri echo <mri>          # record an approved service's MRI in lab.toml, once
```

**`login`** loads the web client in the account's profile under the fail-closed fence (its writes denied and logged
to `fence.log`, as expected). When the session has expired it signs in on Microsoft's page by itself, with the password
from `lab-accounts.json`; the fence lets Entra's sign-in form through for `login` only. Any multi-factor prompt (a
one-time code, a method to pick, an approval in an app, registering a method) stops it
(`docs/backlog/one-time-code-sign-in-under-the-fence.md`). It records the account's MRI from its tokens' claims. Each
step goes to `status-<alias>.json`.

**`status`** prints, per account, `ok`, `login needed`, `signed in, but Teams refused the account (authz answered
403)`, or `unknown (…)`; it signs nothing in.

**`auth`** opens the authorize URL (native client `1fec8e78-…`, the `nativeclient` redirect, PKCE) in the account's
profile under the armed fence, which catches Entra's redirect and hands the code to Rust; nothing is typed. The refresh
token is stored only once the ID token names the account. Then a refresh and the skypetoken from authz; `--audiences`
also asks for a token for each other Teams audience, `--no-skypetoken` stops after Entra. It prints only each token's
`aud`, `exp`, scopes and tenant, plus the skypetoken's expiry and region (`docs/spikes/auth.md`).

**`run`** exits non-zero when the scenario's goal isn't reached, the fence was breached, or the fence denied something
the scenario doesn't expect. Runs are recorded under `captures/<scenario>/<ts>/`. Without `--headless` the browsers
are visible. A target the fence can't arm in time (a step unanswered for 10 s under load) is never resumed: it is
closed, or, when its close isn't confirmed, the browser is killed. That is a warning, not a breach (the target was held
and never ran), but a killed browser fails the run; a target whose resume was sent, one that wasn't held, or one that
could be neither closed nor killed is a breach. Each such target is in `fence.log` and the capture.

- `load`: account a loads the web client, fail-closed; every deny is expected.
- `ab-chat`: a and b; a opens the a–b 1:1 chat by b's MRI and sends a message, and b must show it. First, a live
  enforcement test: a probe post to the chat is force-denied, must fail in a's page and must never show on b. It maps
  the chat service's writes (`Classifier::mapped`) and reads the target's members live (`LiveMembershipSource`, with
  the `ic3` bearer token the page itself sent). It expects only its two probes' forced denies and the web client's
  requests and frames on the reviewed expected-deny list (`crates/proto-lab/src/scenario/expected.rs`: telemetry,
  middle-tier lookups by `POST`, presence, the account's own chat properties, Trouter and AugLoop frames, …), each
  with the reason the scenario doesn't need it. The list only judges the run: everything on it stays denied. A deny on
  the a–b chat other than the probes, one for a partial or refused membership, or a class off the list fails the run.
- `echo-call`: account a places the web client's test call to the echo service, with a fake microphone playing a
  440 Hz tone. The calling classifiers allow the account's reviewed own-state (its settings and policy reads, the
  Trouter session, the endpoint registration) and the call start in exactly its captured shape, a new call of a and
  the echo (`crates/proto-lab/src/calling.rs`, `docs/protocol/calling-echo.md`). Every write on the started call is
  still denied (no authoritative call-membership read: `docs/backlog/fence-call-follow-ups.md`), so the echo never
  joins and the run exits 1: it is judged by the same expected-deny list, and the call's follow-ups are needed, so
  they are not on it. An incoming call is never answered.

**`export`** sanitizes a capture (its dir or `events.jsonl`, or `<scenario>/<ts>` under the captures dir) into
`reference/<name>` (`.jsonl` added without an extension; a `.json` name gets one JSON array), file `0600`, dirs `0700`.
See [Captures and fixtures](#captures-and-fixtures).

**`bundles`** unpacks `desktop-assets` and `client-bindings` (gunzip only, nothing executed) and indexes their url,
host, route and method literals. Use a release build for a real import.

**`service-mri`** works only for a participant `lab.toml` approves by name and has no MRI for yet, and only takes a
bot's `28:<guid>`.

The spike binary `echo-call` (`cargo build -p echo-call`; `--caller a` by default) runs the native call flow as far as
it goes: the fence allows the test call, the skypetoken is read, and the start isn't sent
(`CallError::FollowUpsUnfenced`).

## `teams-headless`

One lab account's native client, no browser: the M1 core with its push channel (Trouter), its catch-up sync and its
outbox. Its fence is `Fence::open_lab`'s, so every write goes out under a fresh `Fence::check` of its target's members
and is logged to the lab's `fence.log`; its tokens are the ones `proto-lab auth` stored in the Secret Service, and its
chat base is the region `auth` saved. It prints no token. One process per account at a time; a, b and c may each run
their own at once.

```sh
teams-headless a sync                     # catch up: the conversation list and changed conversations, into the store
teams-headless a list [--limit 20]        # the stored conversations (the store only; nothing goes out)
teams-headless a show b [--limit 20]      # the a–b chat's newest stored messages, with reactions (the store only)
teams-headless a send b 'hello'           # a post; prints start_ms, original_arrival_time, message_id and state
teams-headless a edit b <message> 'text'  # one of a's messages, edited
teams-headless a delete b <message>       # one of a's messages, deleted
teams-headless a react b <message> like [--remove]
teams-headless b watch [--json]           # every event of the push channel with its receive time, until SIGINT/SIGTERM
```

A chat is `a`, `b` or `c` (the 1:1 chat with that account, from the MRIs in `lab.toml`), `abc` (the group chat
recorded in `chats.toml`), or a conversation id. Each write first catches up (its summary on stderr), then waits up to
`--wait` seconds (120) for a write that has to wait out a `Retry-After`; it exits non-zero unless the write went
through. `watch` answers no delivery and no call; it exits 0 on SIGINT or SIGTERM, non-zero when the push channel
stopped on its own.

## Captures and fixtures

A recorded run writes `captures/<scenario>/<ts>/events.jsonl` under the lab's data dir, never into the repo. Captures
never enter the repo, not even sanitized.

`export` writes a sanitized **reference copy**. It always refuses any destination inside a git work tree (checked with
`..` and symlinks resolved), a destination that is a symlink, and a `..` after a directory not yet created. The copy
never enters the repo, so what export's checks find in it is printed as warnings (kinds, placeholders, event and line
numbers, never a value) and the whole copy is written: a word (3 characters or more) of a name found in the capture
left in the result at any layer of encoding (an ordinary word that is also a name's word counts too; a name never seen
under a name field, or a shorter word, is outside that check), a line too deeply encoded to check, and anything the
fixture guard finds. A word of a name is never replaced inside a host, so the copy keeps every host intact: a public
Microsoft protocol host such as `teams.microsoft.com` is never taken for a name, and a word of a name that is also a
label of any other host is left there and counted in the warnings (it is still replaced everywhere else). Only a real
host counts: a URL's authority (as written or encoded), a `Host`-style header's value, a cookie's `Domain=`; a dotted
name (`Mei.Li`, `jun.hao.li`), a file name (`quill.notes.md`) or prose gets the name replaced, and that is counted in
the warnings too, since such a run may be a host nothing around it gave away. `--strict` turns every one of these
warnings back into a refusal that writes nothing. Teams meeting codes and join passcodes (of letters only too) become
`<meeting:N>` and `<token:N>` wherever they show, a host included, invitations read in every language the web client
has locale strings for, and the fixture guard flags a join link, a meeting id or a passcode left in the clear. The
sanitizer is defence in depth, with known gaps (`docs/backlog/sanitizer-defence-in-depth-known-gaps.md`).

Committed fixtures are written **synthetic-from-capture**: an agent reads the reference copy locally and writes a
fixture of the same shape with made-up values. Fixtures live in any `tests/fixtures/` of the workspace: the top-level
one, a crate's, a spike's (`spikes/*/tests/fixtures/`). Every fixture declares itself synthetic: a JSON file has a
top-level `"_synthetic"` note (what it is shaped like, that the values are made up), a JSONL file has one on its first
line (a header line of its own, or on every line), and any other text file starts with a `_synthetic: …` line, bare or
after its own comment opener. Only an empty `.gitkeep` is not a fixture. `cargo test -p fixture-guard` fails on an
unmarked fixture (`committed_fixtures_are_marked_synthetic`) and on anything the guard finds in any `tests/fixtures/`
or `docs/` (`committed_fixtures_and_docs_are_clean`). The only directories exempt from the marker are test inputs that
are not shaped like anything Teams sends, listed in `fixture_guard::synthetic::TEST_INPUTS` (proto-lab's test pages
and mini bundle); the guard still scans them.

## Tests

The gate, which every change passes:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/lint-report.sh
cargo test --workspace
```

`cargo test --workspace` is offline. The fence suite alone: `cargo test -p teams-fence -p teams-proto -p fence-seal`
plus `cargo test -p proto-lab --test fence_http --test fence_ws --test fence_targets --test calling`.

| variable / flag | effect |
|---|---|
| `PROTO_LAB_CHROME=<binary>` | the Chrome the browser tests use; by default the pinned install in the lab's `chrome/` dir (`proto-lab chrome install`) |
| `PROTO_LAB_REQUIRE_CHROME=1` | without a Chrome the browser tests fail instead of skipping |
| `GPUI_SMOKE_REQUIRE=1` | without weston ≥ 15.0 (for `--fake-seat`) and niri on `PATH` the GPUI smoke tests fail instead of skipping |
| `--features live` + `PROTO_LAB_LIVE=1` | the live lab tests (`crates/proto-lab/tests/auth_live.rs`: a, b and c sign in natively, refresh, and get a skypetoken, tokens kept in memory); without the feature they aren't built, without the variable they skip |
| `ECHO_CALL_TURN_SERVER`, `ECHO_CALL_TURN_USER`, `ECHO_CALL_TURN_PASSWORD` | also run `spikes/echo-call/tests/relay.rs`'s echo through a real TURN server (coturn on loopback) |
| `GPUI_BENCH_GPU`, `GPUI_BENCH_SECONDS`, `GPUI_BENCH_UPLOAD`, `GPUI_BENCH_OUT` | the video grid benchmark (below) |

The machine's own gate run: `PROTO_LAB_REQUIRE_CHROME=1 GPUI_SMOKE_REQUIRE=1 cargo test --workspace`. The live test:
`PROTO_LAB_LIVE=1 cargo test -p proto-lab --features live --test auth_live`.

The browser tests run headless Chrome for Testing on throwaway profiles, never the lab's. The lab's `chrome/` dir is
under `data_dir` when `lab.toml` sets one, else `~/.local/share/teams-lab/chrome/`; no lab config is needed for this,
but an invalid one is refused.

The GPUI smoke tests (`spikes/gpui-video/tests/smoke.rs`) start their own headless weston with a niri nested in it and
never touch the desktop session. They run the window once on every GPU, chosen with `ZED_DEVICE_ID`. The `gpui-video`
binary itself refuses to open a window unless `WAYLAND_DISPLAY` is an absolute socket path inside the dir
`GPUI_VIDEO_SANDBOX` names, which only the sandbox sets. The video grid benchmark (`spikes/gpui-video/tests/bench.rs`)
runs in the same sandbox; its measurement is ignored by default and run by hand with a release build:
`cargo test --release -p gpui-video --test bench -- --ignored --nocapture`. Results and the decision are in
`docs/spikes/gpui-video.md`.

Known flakes under heavy machine load are listed in [CLAUDE.md](CLAUDE.md#chrome-under-load).
