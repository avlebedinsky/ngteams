# ms-teams

A native Microsoft Teams client for Linux in Rust + GPUI, for work/school accounts. Neither official client has native
UI code to port: the Windows MSIX and the macOS pkg are both a native shell around the same React web app plus the
closed SlimCore media stack. So this one is written against Teams' internal APIs, with the web client as the
reference. Everything is learned and tested in a lab of three dedicated accounts, behind a write fence that lets
nothing reach anyone else in the tenant.

Status:

- **M0 is done**: the protocol lab with the write fence, and the three risk spikes (native sign-in, video in GPUI, an
  audio call). Plan `docs/plans/completed/20261019-native-teams-m0-lab-and-spikes.md`, outcome `docs/m0-acceptance.md`.
- **M1 is done**: the headless chat core. Our own Rust code signs a lab account in, syncs its chats into a local
  store, sends, edits, deletes and reacts under the fence, and receives over Teams' push channel (Trouter): native
  a → b in p95 1.4 s, edits, reactions, the lab's a–b–c group chat and a catch-up after a disconnect, all checked live.
  Plan `docs/plans/completed/20261020-native-teams-m1-headless-chat-core.md`, outcome `docs/m1-acceptance.md`.
- **Next: M2**, the GPUI daily driver (`docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`).

Hard rules for contributors and agents are in [CLAUDE.md](CLAUDE.md).

## Layout

| path | contents |
|---|---|
| `crates/teams-fence` | the write fence: a write is allowed only when every participant of its target, read fresh, is a lab account or an approved service; the lab config (`lab.toml`, `chats.toml`), `fence.log` (appends locked across processes), the membership sources (`FailClosed`; `LiveMembershipSource`: 1:1 chats, and the lab's recorded group chat when both member reads agree on exactly a, b and c), the native client's credential for them (`CredentialSource`) |
| `crates/fence-seal` | compile checks without `test-support`: no other crate can mint a `WritePermit`, send a request outside the native request policy, send a Trouter frame that isn't a reviewed own frame, or reach a mock origin; a lab binary has its membership sources |
| `crates/teams-auth` | PKCE, the authorize URL and redirect, the code exchange and refresh, the account check on the ID token, the skypetoken (authz), secret storage (Secret Service through `oo7`), the credential broker (`broker`: one account's secrets served on a socket, and the `BrokerSecretStore` a sandboxed process uses); the `Session`: an access token per audience refreshed before it expires, the skypetoken renewed after its Entra token, throttling shared by waiting callers, renewal notifications |
| `crates/teams-proto` | Teams' internal API and the native HTTP transport. Four request classes only: reads (`GET`), a sealed own-state list logged to `fence.log` before it is sent (one entry: the messaging stack's endpoint registration), a sealed read-by-`POST` list (`PostRead`, one entry: the middle tier's `fetchShortProfile`, `ShortProfileLookup`) sent only as `AuthorizedPostRead` with the permit `Fence::check_post_read` gave for its exact MRIs, logged to `fence.log` first and sent once, and sealed writes sent only as `Authorized<_>` with a `WritePermit` for their exact target (`SendMessage`, `EditMessage`, `DeleteMessage`, `SetReaction`/`RemoveReaction`, `ReadReceipt`, `Typing`, `CreateChat`; `StartCall`/`AddToCall` have no route yet). The chat model (`chat`: conversations, messages, versions, reactions, mentions, quotes, a sanitized rich-text tree, paging and sync links) and `MessageText`, which builds a message's markup and its mentioned MRIs from one value. The Trouter frame policy: a `FrameSink` that sends only the reviewed `OwnFrame`s, each logged first. A fixed host allowlist (the chat service, the registrar, Trouter's HTTP side and, once given the account's middle-tier base, the middle tier's `beta/users/…`), `https` only, no proxy, no redirects, no hidden retries |
| `crates/teams-store` | the account's SQLite store (`rusqlite`, bundled, WAL, `0600`): conversations, members, messages by version (edits, soft deletes), reactions per user, read horizons, sync state, the outbox; migrations; each open is a run, and a row of another open run is never taken over |
| `crates/teams-trouter` | the native push channel: socket.io 0.9 over WSS to the Trouter hosts only, `user.authenticate` first, keep-alives, the registrar registration of the messaging stack, reconnects with backoff and on token renewal, deliveries turned into typed chat events and never answered (an incoming call included) |
| `crates/teams-core` | the headless client: the outbox (a `clientmessageid` per row, a fresh `Fence::check` for every attempt, the retry table, no resend across runs), the catch-up sync (conversation list, message sync links, history), and the engine (its own tokio runtime, reached through a channel: actions in, events out); a lab account's engine assembled as every lab binary builds it (`lab`) |
| `crates/teams-ui` | the GPUI views and models over the engine: the bridge (it owns the engine, drains its events into the models on GPUI's foreground, shuts it down off it; views reach it only through `Core`), the models, the root view and window, the shell (rail, chat list, conversation pane), the chat view (a conversation's messages from the store, a page at a time), our own rich text renderer (`teams-proto`'s `RichText` drawn with GPUI's text elements, links opened only through `open_url`), the chat switcher (Ctrl+K), the settings and their file, the desktop (the window, the notifications of messages that come and the tray item with the unread total, both over `zbus` on the session bus the app is given and nowhere else; closing the window keeps the app in the tray), the view trace (ids, kinds and times, never text), the theme (the UI kit's, Teams-like and compact, light and dark following the window); no HTTP or socket client, GPUI's HTTP client the null one |
| `crates/teams-mock` | tests only (a dev-dependency, never a lab binary's): a local chat service and Trouter built from the synthetic fixtures in `tests/fixtures/{chatsvc,trouter}/`, serving the conversation list, message pages, thread reads, the chat writes and the registrar, delivering each write to the other members' registered sockets; clients reach it through the `test-support` mock origins |
| `crates/teams-app` | the lab's GPUI app for one lab account, `teams-app --lab <alias> --secrets-fd <n> [--trace-fd <n>]`: its engine built as `teams-headless` builds one, its tokens only through the broker's socket; it refuses a `test-support` build and a window outside the UI sandbox or with `DISPLAY` set, reports another engine on the account, shows the account signed out when its refresh token is refused; and its credential broker `teams-app secrets --lab <alias> --fd <n>`: run outside the UI sandbox, it serves that account's own `teams-lab` Secret Service items on a socket pair's end and refuses any other account or application |
| `crates/teams-headless` | the headless lab binary: one lab account's native client (`teams-core`) from the command line, its fence from `Fence::open_lab`, its tokens from the Secret Service, its store under the lab's data dir; the live acceptance test (`tests/chat_live.rs`) |
| `crates/proto-lab` | the lab harness CLI: Chrome for Testing, the armed browser fence (HTTP and WebSocket), login and status, native auth, read-only probes, captures and the sanitizer, bundle import and search, scenarios |
| `crates/fixture-guard` | scans every `tests/fixtures/` and `docs/` for tokens, signatures, emails, real MRIs and GUIDs, meeting links and passcodes, and checks each fixture's `_synthetic` marker |
| `crates/ui-sandbox` | tests and lab runs only: the UI sandbox every GUI process runs in, never the desktop's: a headless weston with a niri nested in it (screenshots and window control through `niri msg`), a private D-Bus session bus with no service activation and a fake notification server and tray watcher that record what they get, a virtual keyboard and pointer in the nested niri, a no-op URI opener first on `PATH`, a spawn that passes only the descriptors its caller names; `ui-sandbox-probe`, the window its own tests drive |
| `spikes/gpui-video` | GPUI on niri (smoke window in a private weston + nested niri), then a 3×3 grid of 720p video (M0 Tasks 15–16) |
| `spikes/media-loop` | two `str0m` peers on loopback carrying an Opus tone, checked with Goertzel (M0 Task 17) |
| `spikes/echo-call` | the native echo call: a TURN client (RFC 8656), `str0m` through the relay, the test call's signaling from the capture, the call through the fence (M0 Task 19; the start sends nothing yet, see `docs/spikes/echo-call.md`) |
| `tests/fixtures/` | synthetic fixtures (made-up values, a `_synthetic` marker), as in every `tests/fixtures/` of a crate or spike: `chatsvc/` (the chat service), `trouter/` (the push channel), `mt/` (profile lookups) |
| `docs/protocol/` | protocol notes in our own words: `auth.md`, `chat.md` (the chat service's reads and writes as captured), `fence-targets.md` (how the fence maps them, member reads, reads by `POST`), `trouter.md`, `calling-echo.md`, and `endpoints.md`, one row for every endpoint seen or used and how the browser fence and the native client treat it |
| `docs/m0-acceptance.md`, `docs/m1-acceptance.md` | each milestone's acceptance check: numbers, the fence's picture, every live run |
| `docs/spikes/` | spike write-ups: `ca-probe.md`, `auth.md`, `gpui-video.md` (+ raw runs), `echo-call.md` |
| `docs/backlog/` | deferred work, one file per item |
| `docs/plans/` | milestone plans; finished ones in `docs/plans/completed/` |
| `build/patches/`, `vendor/`, `scripts/vendor-gpui.sh` | GPUI (`gpui-pre` 0.3.9) vendored with agterm-rs's external-surfaces patches, used through `[patch.crates-io]`; `scripts/vendor-gpui.sh --check` verifies it |
| `scripts/lint-report.sh` | refuses inner `#![allow]`/`#![expect]`, lists complexity `#[expect]`s |
| `tools/` | interim Node tools: `ca-probe` (the Conditional Access probe), `lab-login` (the first sign-in of the profiles) |

## Third-party UI code

The views are built on [`gpui-component`](https://crates.io/crates/gpui-component) 0.8 (Longbridge's GPUI Kit,
Apache-2.0), the UI kit for `gpui-pre` 0.3.9; it resolves to the vendored, patched GPUI above. Its audit (M2 Task 7,
in the plan) found:

- It takes `gpui-pre` with its default features (`x11`, `font-kit`, `windows-manifest`), so the workspace's GPUI has
  them on. In `gpui-pre` 0.3.9 `x11` only lets GPUI name an X11 display it could use and `font-kit` switches nothing;
  the X11 client is `gpui-pre-linux`'s, which stays Wayland-only (`crates/teams-ui/tests/reach.rs` checks it), and
  `teams-app` refuses a set `DISPLAY` all the same. `windows-manifest` acts on Windows builds only.
- Its tree for the host holds no HTTP or WebSocket client (`gpui-pre-reqwest` is for wasm only) and only licences an
  Apache-2.0/MIT project can take: MIT, Apache-2.0, BSD, ISC, Zlib, Unicode-3.0, 0BSD, CC0, and MPL-2.0 for
  `option-ext`, which a build-time macro of the kit's assets uses (`reach.rs` checks both).
- It starts no thread or process of its own and touches no file in our use: its theme directory watcher
  (`ThemeRegistry::watch_dir`, `notify`) is never called. It reads the reduced-motion setting from the settings portal
  on the bus the app is given (the sandbox's private bus, which has none). Its `tracing` and `log` lines go nowhere:
  the app crates install no logger and no subscriber (`reach.rs` checks the sources).
- We don't use its Markdown/HTML text view: message text is rendered from `teams-proto`'s `RichText` only.

The kit's `Textarea` (the compose box) is tested on GPUI's test context (`crates/teams-ui/tests/kit.rs`: typing,
selection, the clipboard with plain text only, undo, soft wrap, IME composition, Enter and Shift-Enter) and on the
real platform in the UI sandbox (`crates/teams-ui/tests/kit_window.rs`, which also switches the theme live through a
fake settings portal on the sandbox's bus, and clicks a message's link drawn by our rich text renderer, which goes to
the sandbox's no-op opener).

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
| `…/chats.toml` | the lab's group chat: the id of the a–b–c chat `abc-chat` created, recorded once as `abc` (`0600`); the only group chat the fence reads |
| `…/store/<alias>.sqlite` | the account's native store (`teams-headless`): conversations, messages, reactions, the outbox (`0600`, its dir `0700`); `<alias>.sqlite-runs/` holds a lock per open run |
| `…/headless/<time>…/` | the native live runs' outputs (`teams-headless` and `chat_live`'s `summary.json`) |
| `…/live.lock` | the live lock: every live run (a lab browser or a native client against the tenant) holds it with `flock` |
| `…/auth/<alias>.json` | the account's region, partition, region GTMs and skypetoken expiry from `auth` (`0600`) |
| `…/status-<alias>.json` | `login`'s steps, and why it stopped |
| `…/media/tone-440.wav` | the fake microphone's tone for `echo-call` |
| Secret Service, application `teams-lab` | refresh tokens and skypetokens (each with its expiry, capped at its Entra token's), per account |
| lab-automation's `lab-accounts.json` (mode `0600`) | the lab accounts' passwords, read only by `login` |

## Building

`cargo build -p proto-lab` (and `-p teams-headless`, `-p echo-call`) builds a normal lab binary. The tests enable `teams-fence`'s
`test-support` feature through dev-dependencies, and cargo unifies it into everything built together with them
(`cargo test`, `--all-targets`, `--all-features`), the binaries included. Such a binary refuses to run; rebuild with
`cargo build -p <crate>`. A live run's binary is built into a target dir of its own (`cargo build -p teams-headless -p
proto-lab --target-dir target/lab`), so a later `cargo test` can't rebuild it with `test-support` in.

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
proto-lab probe chat a [--with b] [--save <name>]                  # read-only: does the chat service take our ic3 token
proto-lab probe thread a [--save <name>]                           # read-only: the group chat's member reads
proto-lab probe object a <id> [--save <name>]                      # read-only: one AMS object as the account reads it
proto-lab run <scenario> --headless [--timeout 180]                # a scenario under the armed fence (list below)
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

**`probe`** makes read-only `GET`s with the account's own tokens and prints statuses, header names, key names, counts
and token claims only, never a token or a body. `probe chat` refreshes the account's `ic3` token and reads one page of
the conversation list in each header form, then the 1:1 chat's thread the way the fence reads it (M1 Task 3). `probe
thread` reads the group chat `chats.toml` records: the thread, the batch read, the member list in one page and paged,
the filtered view, the account's own view, then the fence's read (M1 Task 8). `probe object` reads one media service
object by its id and prints its answer's outline, lab ids named for what they are (M2 Task 21). `--save <name>`
writes the answers as a sanitized reference copy through `export`'s checks, never inside a git work tree.

**`run`** exits non-zero when the scenario's goal isn't reached, the fence was breached, or the fence denied something
the scenario doesn't expect. Runs are recorded under `captures/<scenario>/<ts>/`. Without `--headless` the browsers
are visible. A target the fence can't arm in time (a step unanswered for 10 s under load) is never resumed: it is
closed, or, when its close isn't confirmed, the browser is killed. That is a warning, not a breach (the target was held
and never ran), but a killed browser fails the run; a target whose resume was sent, one that wasn't held, or one that
could be neither closed nor killed is a breach. Each such target is in `fence.log` and the capture. Every page call
and navigation is bounded (a `beforeunload` dialog holding a navigation is accepted after 5 s), and SIGINT or SIGTERM
stops the run, kills its browsers and lets the fence's decisions still running land in `fence.log` (exit 130 or 143).

A writing scenario first proves enforcement live: a probe post to its chat is force-denied, must fail in the page and
must never show on the other accounts. It maps the chat service's writes (`Classifier::mapped`), reads the target's
members live (`LiveMembershipSource`, with the `ic3` bearer token the page itself sent), and lets the mention picker's
profile lookups out only when every MRI they name is a lab account (the read-by-`POST` list,
`crates/proto-lab/src/post_read.rs`). It expects only its probes' forced denies and the web client's requests and
frames on the reviewed expected-deny list (`crates/proto-lab/src/scenario/expected.rs`: telemetry, lookups beyond the
lab, presence, the account's own settings, marking its own feed items read, bot invokes, Trouter and AugLoop frames, …), each with the
reason the scenario doesn't need it. The list only judges the run: everything on it stays denied. A deny on the
scenario's own chat other than the probes, one for a partial or refused membership, or a class off the list fails
the run.

- `load`: account a loads the web client, fail-closed; every deny is expected.
- `ab-chat`: a and b; a opens the a–b 1:1 chat by b's MRI and sends a message, and b must show it (clicking "Jump to
  newest" or "New messages" when the native runs left it unread).
- `ab-chat-full`: the a–b chat beyond a message, as captured for `docs/protocol/chat.md`: a sends, edits, reacts and
  takes it back, mentions b (the picker's suggestion checked to be b), replies with a quote, reacts to the reply and
  deletes; b reads and reacts.
- `ab-image`: a pastes a made-up image into the a–b chat with every write to the media service force-denied, to
  capture what sending an image sends (sending stays deferred: `docs/backlog/ams-image-sending.md`).
- `ab-file`: a drops a made-up text file on the a–b chat's compose box with every write of it force-denied (the file
  services, AMS objects, and any write naming a lab file), to capture what sending a file sends
  (`docs/backlog/file-sending.md`).
- `abc-chat`: with no group chat recorded, a creates the a–b–c chat (its first message force-denied) and records it
  in `chats.toml`, once; after that a messages it and b and c must show it, under the group-chat rule in
  [CLAUDE.md](CLAUDE.md).
- `ab-trouter`, `ab-trouter-quiet`: b's push channel while a sends, edits and reacts, under the messaging classifier
  (the Trouter session frames and the messaging stack's registration only), observed for 16 minutes; the quiet one
  holds back every `user.activity` (`docs/protocol/trouter.md`).
- `chat-shapes`: a and b under the messaging classifier, as `ab-trouter`: a sets and takes back each quick reaction
  the hover bar offers, types for 45 s, leaves the text for 35 s, empties its compose box and waits 15 s more
  (`TYPE_FOR`, `PAUSE`, `AFTER_EMPTYING`) while b's open chat records its typing
  indicator, sends a message of three lines with a link, and mentions b in the recorded group chat
  (`docs/protocol/chat.md`).
- `echo-call`: account a places the web client's test call to the echo service, with a fake microphone playing a
  440 Hz tone. The calling classifiers allow the account's reviewed own-state (its settings and policy reads, the
  Trouter session, the endpoint registration) and the call start in exactly its captured shape, a new call of a and
  the echo (`crates/proto-lab/src/calling.rs`, `docs/protocol/calling-echo.md`). Every write on the started call is
  still denied (no authoritative call-membership read: `docs/backlog/fence-call-follow-ups.md`), so the echo never
  joins and the run exits 1: it is judged by the same expected-deny list, and the call's follow-ups are needed, so
  they are not on it. An incoming call is never answered.

Live runs go one at a time under the live lock, with an outer bound, from a normal build:
`flock ~/.local/share/teams-lab/live.lock timeout --signal=INT --kill-after=60 900 target/lab/debug/proto-lab run
ab-chat --headless`. `pgrep -af chrome` shows no lab profile afterwards.

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
teams-headless b read a <message>         # b's read horizon in the a–b chat moved up to that message (a read receipt)
teams-headless b watch [--json]           # every event of the push channel with its receive time, until SIGINT/SIGTERM
teams-headless a presence [--wait 30]     # b's and c's presence, read from UPS (a read by POST, lab accounts only)
```

A chat is `a`, `b` or `c` (the 1:1 chat with that account, from the MRIs in `lab.toml`), `abc` (the group chat
recorded in `chats.toml`), or a conversation id. Each write first catches up (its summary on stderr), then waits up to
`--wait` seconds (120) for a write that has to wait out a `Retry-After`; it exits non-zero unless the write went
through. `watch` answers no delivery and no call; it exits 0 on SIGINT or SIGTERM, non-zero when the push channel
stopped on its own. `presence` reads only, under the fence's permit for the exact MRIs it names; it waits up to
`--wait` seconds (30) for each account's presence to be told, then prints one `presence <alias> <availability>` line
per account, `Unknown` for one not told in time or whose read failed. `TEAMS_HEADLESS_LOG=debug` logs our own crates' debug lines on stderr (Trouter's packets and
deliveries by kind and size, never a payload); nothing else is logged.

The live acceptance test (M1 Task 21, `tests/chat_live.rs`) only spawns a normally built binary and reads its output
and `fence.log`; run it under the live lock:

```sh
cargo build -p teams-headless --target-dir target/lab
flock ~/.local/share/teams-lab/live.lock timeout --signal=INT --kill-after=60 1500 \
  env TEAMS_HEADLESS_BIN=$PWD/target/lab/debug/teams-headless PROTO_LAB_LIVE=1 \
  cargo test -p teams-headless --features live --test chat_live -- --nocapture
```

## `teams-app`

The lab app for one lab account, in the UI sandbox only (it refuses any other compositor, and `DISPLAY` set), with
its credential broker started outside the sandbox on the other end of its socket pair:

```sh
teams-app --lab a --secrets-fd 3 [--trace-fd 4]   # a's window; tokens only through the broker on descriptor 3
```

It is built as `teams-headless` builds its client (`teams_core::lab::start`) and refuses a `test-support` build, so a
lab run uses `cargo build -p teams-app --target-dir target/lab`. Another engine on the account (a second `teams-app`,
a `teams-headless`) is reported and the app exits. `--trace-fd` gets the view trace, one JSON line per view change
(ids, kinds and times, never text), on a descriptor of its own (one named for both is refused). When the session's
refresh token is refused, the window shows the account signed out (run `proto-lab auth <alias>`) and the engine stops.
Closing the window shuts the engine down, then quits; an engine still waiting on the broker after 5 s (an unlock prompt
nobody answers) has the broker's socket shut down under it.

The window is three panes: the rail (Chat, Teams, Settings), the chat list (the store's chats by last activity, with
names, previews, times and unread counts, and the connection state; resizable) and the conversation pane, which shows
the chat opened in the chat view: its messages from the store, the newest at the bottom (older pages load as the top
comes near), grouped by sender within 5 minutes, with day separators, a "New messages" divider at the read horizon as
the chat opened, edited and deleted markers, reactions, thread events, a "Jump to newest" pill once scrolled up, and
who is typing. With the message list focused (a click on a message), Up and Down select a message, and `r` (reply),
`a` (react), `e` (edit, the account's own), Delete (the account's own) and Escape (clear the selection) act on it; the
same actions show over a message under the pointer. Ctrl+K opens the chat switcher (fuzzy over the chats' names;
Enter opens, Escape closes). The settings (the selected chat,
typing, read receipts, notifications, the idle time, the manual status) live in `<data_dir>/ui/<alias>.toml` (`0600`,
its directory `0700`); a file that can't be read gives the defaults, a bad value keeps its default, and either is
printed and shown in the settings view.
Messages from others notify on the desktop as the "notifications" setting says (all, mentions only, off; a mention is
marked; in a team's channel only a mention), never for the chat shown in a focused window, the account's own, one
already stored or older history; a catch-up's (or a replayed delivery the store didn't have) only when it arrived
after the push channel was last heard before it went down and is unread; more than three of one chat collapse into
one. Activating one opens its chat. The tray item shows the unread total ("Open Teams", "Quit") and follows the tray's
watcher (registered again with a new one); closing the window keeps the app in the tray while a watcher has the item,
and quits it when none does (the window comes back if the item goes while it is closed). Both go only to the session bus the app is given (its `DBUS_SESSION_BUS_ADDRESS`, the sandbox's private bus),
never `$XDG_RUNTIME_DIR/bus` or another, and are left out without one.
`crates/teams-app/tests/window.rs` runs the window in the sandbox on `teams-mock` (the chat list filled by the
catch-up, the switcher driven by the virtual keyboard, the chat it opens shown in the chat view with that chat's
messages from the store; a message of c's notifying on the sandbox's fake server and its activation opening its chat;
the tray item keeping the app when the window closes, the window back when the watcher leaves and the item
registered again with a new one, opening it again and quitting it) and keeps its two screenshots
under `target/ui-shots/`: `teams-app-window.png` (the window as the catch-up filled it) and `teams-app-chat.png` (the
chat opened).

## `teams-app secrets`

The credential broker for a lab app in the UI sandbox, whose session bus is the sandbox's own: started outside the
sandbox, with the user's bus, by whoever starts the app, on one end of a socket pair whose other end the app gets.

```sh
teams-app secrets --lab a --fd 3   # serves a's own teams-lab items on descriptor 3 until the socket closes
```

It serves `get`, `put` and `delete` of the account's own items, by audience, and answers any other account or
application "refused" without reaching a store; a malformed or oversized frame closes the socket. It connects the
Secret Service only on the first request it serves, opens no window, prints nothing on stdout, notes on stderr only
the requests it refused or failed (operation, account, audience; never a value), and exits when the socket closes (0
when the peer closed it between requests, or while a request waited on the store, an unlock prompt included). The
starter keeps no copy of the broker's end.

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

`cargo test --workspace` is offline: the native client is tested against `teams-mock` (two or three native clients
talking through it), the browser fence against local pages in Chrome. The fence suite alone: `cargo test -p
teams-fence -p teams-proto -p fence-seal -p teams-trouter` plus `cargo test -p proto-lab --test fence_http --test
fence_ws --test fence_targets --test calling --test post_reads --test expected_denies`.

| variable / flag | effect |
|---|---|
| `PROTO_LAB_CHROME=<binary>` | the Chrome the browser tests use; by default the pinned install in the lab's `chrome/` dir (`proto-lab chrome install`) |
| `PROTO_LAB_REQUIRE_CHROME=1` | without a Chrome the browser tests fail instead of skipping |
| `GPUI_SMOKE_REQUIRE=1` | without weston ≥ 15.0 (for `--fake-seat`), niri and `dbus-daemon` on `PATH` the UI sandbox's and the GPUI smoke tests fail instead of skipping |
| `--features live` + `PROTO_LAB_LIVE=1` | the live lab tests: `crates/proto-lab/tests/auth_live.rs` (a, b and c sign in natively, refresh, and get a skypetoken, tokens kept in memory) and `crates/teams-headless/tests/chat_live.rs` (M1's acceptance: it only spawns the normally built `teams-headless` and reads its output and `fence.log`); without the feature they aren't built, without the variable they skip |
| `TEAMS_HEADLESS_BIN=<binary>` | the normally built `teams-headless` that `chat_live` spawns (required: a test build refuses to run) |
| `TEAMS_HEADLESS_LOG=debug` | `teams-headless` logs our own crates' debug lines on stderr, never a payload or a token |
| `ECHO_CALL_TURN_SERVER`, `ECHO_CALL_TURN_USER`, `ECHO_CALL_TURN_PASSWORD` | also run `spikes/echo-call/tests/relay.rs`'s echo through a real TURN server (coturn on loopback) |
| `GPUI_BENCH_GPU`, `GPUI_BENCH_SECONDS`, `GPUI_BENCH_UPLOAD`, `GPUI_BENCH_OUT` | the video grid benchmark (below) |

The machine's own gate run: `PROTO_LAB_REQUIRE_CHROME=1 GPUI_SMOKE_REQUIRE=1 cargo test --workspace`. The live tests
run one at a time under the live lock: `flock ~/.local/share/teams-lab/live.lock env PROTO_LAB_LIVE=1 cargo test -p
proto-lab --features live --test auth_live`, and `chat_live` as in [`teams-headless`](#teams-headless). No write ever
comes from a `cargo test` process: a live test that needs one spawns a normally built lab binary, which writes under
its own fence.

The browser tests run headless Chrome for Testing on throwaway profiles, never the lab's. The lab's `chrome/` dir is
under `data_dir` when `lab.toml` sets one, else `~/.local/share/teams-lab/chrome/`; no lab config is needed for this,
but an invalid one is refused.

Every GUI test runs in the UI sandbox (`crates/ui-sandbox`): its own headless weston with a niri nested in it, its own
D-Bus session bus, never the desktop session's compositor, bus or URI opener (`crates/ui-sandbox/tests/sandbox.rs`
checks that with canaries planted on the inherited addresses). Without weston 15, niri or `dbus-daemon` the sandbox
tests say so and pass; `GPUI_SMOKE_REQUIRE=1` makes them fail instead. The GPUI smoke tests
(`spikes/gpui-video/tests/smoke.rs`) run the window once on every GPU, chosen with `ZED_DEVICE_ID`. A sandboxed GUI
binary (`gpui-video`, `ui-sandbox-probe`, `teams-app`, the kit window test's child) refuses to open a window unless `WAYLAND_DISPLAY` is an absolute socket path
inside the dir `UI_SANDBOX_DIR` names, which only the sandbox sets. The video grid benchmark (`spikes/gpui-video/tests/bench.rs`)
runs in the same sandbox; its measurement is ignored by default and run by hand with a release build:
`cargo test --release -p gpui-video --test bench -- --ignored --nocapture`. Results and the decision are in
`docs/spikes/gpui-video.md`.

Known flakes under heavy machine load are listed in [CLAUDE.md](CLAUDE.md#chrome-under-load).
