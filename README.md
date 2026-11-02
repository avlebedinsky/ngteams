# ngteams

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
- **M2 is done**: the GPUI daily driver. A three-pane GPUI app (`teams-app`) on the M1 core: the chat list, the chat
  view and compose, desktop notifications and a tray item, local search, names, pictures and presence (others' shown,
  the account's own set), teams and channels read-only, and the app's diagnostic log; checked live for a, b and c in
  the UI sandbox (send → shown p95 1.85 s). Beside it the personal build (`ngteams`) for the user's own account under the
  rules the user approved (`docs/personal-build.md`), built, installed and first run by the user alone. Images and
  files are a placeholder with a link to the web client (sending stays deferred). Plan
  `docs/plans/completed/20261021-native-teams-m2-gpui-daily-driver.md`, outcome `docs/m2-acceptance.md`.
- **M3 is done in the lab**: calls. 1:1 and group-chat calls, audio and video, made, answered, declined and joined
  under the fence: the web client's calls mapped from captures, then native signaling (`teams-call`), media
  (`teams-media`: Teams' SDP dialect, libwebrtc for the media, PipeWire devices) and
  the app's calls, checked live for a, b and c in the UI sandbox, against each other and against the web client both
  ways (setup p50 2.1 s, a → b latency p50 230 ms, no echo above the recording's resolution). The personal build
  (`ngteams`) makes and answers the user's own calls under the rules the user approved (`docs/personal-build.md`,
  "Calls"), tested on the mock; the user's own real-call run, the checklist in `docs/m3-acceptance.md`, is still to
  come. Plan `docs/plans/completed/20261024-native-teams-m3-calls.md`, outcome `docs/m3-acceptance.md`.
- **Next: M4**, meetings and the calendar (`docs/plans/20261026-native-teams-m4-meetings.md`).

Hard rules for contributors and agents are in [CLAUDE.md](CLAUDE.md).

## Layout

| path | contents |
|---|---|
| `crates/teams-fence` | the write fence of the lab rule: `contacting`, the one classifier the browser fence and the native transport share (a request reaches nobody and is allowed, is a mapped write with its target, or is a mutating request on a contacting service no row reads, denied); `Fence::check`, which permits a contacting write only when every participant of its target, read fresh and complete, is a lab account or an approved service (a new target, a chat or call created, judged by its added participants alone; typing, read receipts and the account's own availability without a member read); the lab config (`lab.toml`, and `chats.toml`, lab data the fence doesn't read), `fence.log` (appends locked across processes), the membership sources (`FailClosed`; `LiveMembershipSource`: 1:1 chats, and group and meeting chats from one fresh member-list read), the native client's credential for them (`CredentialSource`); the calls (M3): the calling rows (`contacting/call.rs`: a chat's call start and the test call's, a join, an answer, a decline, an `add`, the follow-ups, each in its captured shape), the lab's call rule (`CallRule::Entry`: a call judged by its entry, a start by its chat's fresh read, a join by the chat it names, an answer or a decline only of the account's own offered invitation, an `add` by its invitees; what the call sends after that passes, logged), the call registry (`calls`: the calls this run's fence permitted and the links their services handed out, the invitations it offered; every fence's bookkeeping, and its own stricter rule, `CallRule::Registry`, only for tests) and the personal build's fence (`decision/audit.rs`: it logs every write and denies none, reading no members) |
| `crates/fence-seal` | compile checks without `test-support`: no other crate can mint a `WritePermit`, send a request outside the native transport's three classes or hand a plain request over as a write, send a Trouter frame that isn't one of `teams-proto`'s typed `OwnFrame`s, or reach a mock origin; a lab binary has its membership sources |
| `crates/teams-auth` | PKCE, the authorize URL and redirect, the code exchange and refresh, the account check on the ID token, the skypetoken (authz), secret storage (Secret Service through `oo7`), the credential broker (`broker`: one account's secrets served on a socket, and the `BrokerSecretStore` a sandboxed process uses); the `Session`: an access token per audience refreshed before it expires, the skypetoken renewed after its Entra token, throttling shared by waiting callers, renewal notifications |
| `crates/teams-proto` | Teams' internal API and the native HTTP transport. Three request classes only: reads (`GET`, no body: the chat service's, Graph's name and picture, CSA's teams, a message's picture on AMS, the TURN credentials, a reaction's animation on Teams' CDN), plain requests (`Request`, an open trait: any method and body, sent only when `teams_fence::contacting` says it reaches nobody, logged to `fence.log` as `allowed` first: the messaging and calling stacks' endpoint registrations, the middle tier's profile lookup, UPS's presence read and its presence subscription, whoever they name), and sealed writes sent only as `Authorized<_>` with a `WritePermit` for their exact target (`SendMessage`, `EditMessage`, `DeleteMessage`, `SetReaction`/`RemoveReaction`, `ReadReceipt`, `Typing`, `CreateChat`, the account's own availability `SetEndpointAvailability`/`ForceAvailability` naming a `RegisteredEndpoint`; a call's requests, `StartCall`, `JoinCall`, the callee's `AttachCall`/`ReportRinging`, `AcceptCall`, `DeclineCall` and every captured operation on a call, each built whole and classified when made, sent to the calling service with the account's skypetoken, the links exactly as handed out, the fence's call registry fed around the send; a lab fence judges a call by its entry and permits the rest); through a lab fence a plain request the classifier calls a write or can't read is refused before anything is sent (through the personal build's it goes, logged with that classification). Beside them the sealed `OwnState` and `PostRead` entries (the same requests, which a lab binary sends as plain ones and the personal build as its own-state entries and its reads by `POST` under a permit, each logged by its kind). The chat model (`chat`: conversations, messages, versions, reactions, mentions, quotes, a sanitized rich-text tree, paging and sync links) and `MessageText`, which builds a message's markup and its mentioned MRIs from one value. The Trouter frames: a `FrameSink` that sends only the typed `OwnFrame`s this crate builds, never raw text, none of them logged to `fence.log` by a lab binary (the personal build's sink logs each), and answers a call delivery only with a typed response (`AcceptanceAcknowledgement`, `InvitationReceipt`) the fence permitted for its connection (`FrameSink::respond`). One base per service (the chat service, the registrar, Trouter's HTTP side, Graph, the calling service (the flight proxy), the TURN credentials' read, Teams' emoticon CDN with no token and, once given the account's bases, the middle tier, CSA, UPS and AMS), each token only to its own service's host, `https` only, no proxy, no redirects, no hidden retries |
| `crates/teams-store` | the account's SQLite store (`rusqlite`, bundled, WAL, `0600`): conversations, members, messages by version (edits, soft deletes), reactions per user, read horizons, sync state, the outbox; migrations; each open is a run, and a row of another open run is never taken over |
| `crates/teams-trouter` | the native push channel: socket.io 0.9 over WSS to the Trouter hosts only, `user.authenticate` first, keep-alives, the registrar registration of the messaging stack, reconnects with backoff and on token renewal, deliveries turned into typed chat events and never answered (an incoming call included); built `for_calls`, the calling stack's own socket (`SkypeSpaces`, registered as `SkypeSpacesWeb`): its deliveries read by the fence first, an invitation it doesn't offer ignored, the rest handed on and answered only by a typed response the fence permits on the delivery's own connection (refused for the personal fence) |
| `crates/teams-call` | a call's signaling, sans-IO (M3): the state machine of the account's calls, one at a time (`Calls`), building each request a call makes as a sealed `teams-proto` write (the start of a chat's call or the test call, the join of a running group call, an invitation's receipt and `attach`, `progress`, the answer and the decline only on the account's action, the acceptance's acknowledgement as its delivery response or by request when that failed, the endpoint's state and metadata, the echo's `add`, a renegotiation of the account's and its answer's acknowledgement, the answer to a renegotiation delivered to it, the `leave`), handed to the engine's `WriteSink` with the id its outcome comes back under: it sends nothing itself. In: the account's actions, the calling socket's deliveries as the fence read them, the writes' outcomes, the media failing, the engine stopping, time; out: writes (one in flight at a time: beside it the engine takes everything but the next write, and a write refused unacted goes again only while its call wants it), typed delivery responses, events (an invitation that rings, a ring's end, the call's state, the far end's media or its renegotiation's offer, its end as delivered). An invitation during a call isn't offered; the keep-alive interval is kept and told, with no request (none captured) |
| `crates/teams-core` | the headless client: the outbox (a `clientmessageid` per row, a fresh `Fence::check` for every attempt, the retry table, no resend across runs; transient writes; the call kind, `try_call`: no row, the answer back to the call machine, one more attempt only after a 401 or a short throttle), the catch-up sync (conversation list, message sync links, history), and the engine (its own tokio runtime, reached through a channel: actions in, events out); the account's calls (`calling`, M3 Task 14): the calling socket, the `teams-call` machine and the call's media (`teams-media`: the relay's credentials, the session on the devices, Teams' SDP dialect, all before the start, join or answer goes; the far end's SDP applied, renegotiations either way), `EngineRequests::call` (a call from a chat, the test call, a join, an answer or a decline of an invitation it offered, a hang-up, mute, a renegotiation) and `Event::{IncomingCall, RingEnded, Call, CallingRegistration}`, one call at a time, a failed step or failed media ending the call with a `leave`, the engine's stop hanging up; a lab account's engine assembled as every lab binary builds it (`lab`) |
| `crates/teams-media` | a call's media, apart from its signaling: libwebrtc carries it (`rtc`: LiveKit's bindings; ICE through the relay only, DTLS-SRTP, Opus and H.264 in software, the jitter buffer, bandwidth estimation, A/V sync); Teams' SDP dialect (`sdp`: `application/sdp-ngc-1.0`, and the mixer's `-0.5` read only) to and from libwebrtc's standard SDP, both ways and on both sides of a call, the m-lines' mids and labels kept for the call's life, only relayed candidates written, a browser's SDP refused; where the media is relayed (`relay`: Teams' relay host or a test's server, with the relay's credentials); a loopback TURN server for tests (`test_turn` on the STUN codec `stun`, `test-support` only), which can drop and reorder the RTP it relays, and a far end on libwebrtc for the mock's mixer (`rtc::far_end`, `test-support` only); the microphone's echo cancellation, noise suppression and gain control (`processing`: libwebrtc's audio processing module, AEC3, on the 10 ms frames, each frame the speaker plays handed to it as the far end; on in a call, off for a probe that needs a steady tone); a call's media session on threads of its own (`session`: the microphone through the processing to libwebrtc, libwebrtc's audio to the speaker, the camera's frames to libwebrtc and its decoded video to each video line's sink, a group call's control plane on the data channel); the camera's background effects (`video::effects`: blurred or a picture behind the person, cut out by MediaPipe's selfie segmenter run on tract on a thread of its own); the device seam (`devices`: a call's microphone, speaker and camera as 10 ms mono frames at 48 kHz and raw video frames), with a file backend (`devices::file`: a tone, a WAV file or silence in, a WAV recording out, a test pattern carrying each frame's index as the camera, received frames counted; no audio server) and a PipeWire backend (`devices::pipewire`: the microphone, speaker and camera as streams on the one server the build may open, fixed when it is built: the UI sandbox's private one for the lab and every `test-support` build, the desktop's only for `personal` alone); tones and their levels for tests and probes (`tone`), and the speech-like and voice-like signals the processing is judged with (`test_signal`, `test-support` only); a video a message carries played in its card (`player`: GStreamer's `playbin` on an `appsrc` fed with the engine's ranged reads through a read-ahead block cache, its BGRA frames and 48 kHz mono sound to appsinks, the sound on the device seam's speaker). Sends no signaling |
| `crates/teams-ui` | the GPUI views and models over the engine (and, from M3, calls: Call and Join in a chat's header, the incoming call's ringtone, banner and notification, the in-call strip, the call's own window with its video tiles, its participant grid and its controls, the devices in the settings): the bridge (it owns the engine, drains its events into the models on GPUI's foreground, shuts it down off it; views reach it only through `Core`), the models, the root view and window, the shell (rail, sidebar with the chats and the teams, conversation pane), the chat view (a conversation's messages from the store, a page at a time), our own rich text renderer (`teams-proto`'s `RichText` drawn with GPUI's text elements, links opened only through `open_url`), the chat switcher (Ctrl+K), the settings and their file, the desktop (the window, the notifications of messages that come and the tray item with the unread total, both over `zbus` on the session bus the app is given and nowhere else; closing the window keeps the app in the tray), the view trace (ids, kinds and times, never text), the theme (the UI kit's, compact, from a built-in catalog: Teams' light, dark and high contrast and a few well-known palettes, one for each of the system's light and dark modes or one whatever it says, in the font size (Ctrl+=, Ctrl+-, Ctrl+0) and family the settings choose); no HTTP or socket client, GPUI's HTTP client the null one |
| `crates/teams-log` | the app's diagnostic log: one file per account (`0600` in a `0700` dir, rotated at 8 MiB, 4 older kept), our own crates' records only, written from a thread of its own; every line redacted by the logger itself (credential headers, token fields, sign-in's codes, Trouter's `user.authenticate`, a call SDP's ICE credentials, then JWT-shaped strings, `skypetoken=`, `Bearer `); full requests and frames only at `debug`; the "copy diagnostics" text |
| `crates/teams-mock` | tests only (a dev-dependency, never a lab binary's): a local chat service and Trouter built from the synthetic fixtures in `tests/fixtures/{chatsvc,trouter,calling}/`, serving the conversation list, message pages, thread reads, the chat writes and the registrar, delivering each write to the other members' registered sockets; the calling service (the flight proxy's call requests with the account's skypetoken, their invitations, acceptances and ends delivered to the registered calling endpoints, calls of a caller it plays, their ring's end and hang-up, renegotiations); the TURN relay's credentials (`/trap/tokens`); with its `media` feature, a loopback media peer (a `teams-media` session behind the loopback TURN server answering and offering in Teams' dialect), and once a test turns the media on (`start_media`) one behind every party the mock plays (the test call's media controller, a group call's mixer, a caller of its own) and its renegotiations, on a loopback TURN server whose credentials `/trap/tokens` then hands out; clients reach it through the `test-support` mock origins |
| `crates/teams-app` | the lab's GPUI app for one lab account, `teams-app --lab <alias> --secrets-fd <n> [--trace-fd <n>]`: its engine built as `teams-headless` builds one, its tokens only through the broker's socket; it refuses a `test-support` build and a window outside the UI sandbox or with `DISPLAY` set, reports another engine on the account, shows the account signed out when its refresh token is refused; and its credential broker `teams-app secrets --lab <alias> --fd <n>`: run outside the UI sandbox, it serves that account's own `teams-lab` Secret Service items on a socket pair's end and refuses any other account or application |
| `crates/teams-signin` | the sign-in window: Microsoft's sign-in page in a WPE WebKit view (`teams-wpe`) drawn in a GPUI window of the app that asks, behind a request guard that classifies every request a page sends (`teams_signin::classify`: reads, the token endpoint and authz in their captured queries, Entra's sign-in form) and logs it to `fence.log` before it goes; the `nativeclient` redirect is caught in the navigation policy and its code handed to the sign-in, and `teams-auth` redeems it. Not a complete fence (two kinds of redirect never reach the guard, `docs/backlog/webkit-sign-in-guard-redirects.md`), so the lab's `teams-signin --lab` (an app of its own in the UI sandbox) opens only a loopback page and reaches nothing else; the `personal` feature (only `crates/ngteams` enables it) binds the same window, opened in `ngteams` itself, to Entra's and the identity provider's hosts through a host gate |
| `crates/teams-signin-extension` | the sign-in window's WPE WebKit web process extension (a `cdylib`, hand FFI on the GLib `-sys` crates, linked to `libWPEWebKit-2.0`): connects every page's `send-request` to the guard's socket and drops each request the guard doesn't allow, every one when it has no guard |
| `crates/teams-wpe`, `crates/wpe-webkit-sys`, `crates/wpe-platform-sys`, `crates/wpe-javascriptcore-sys` | WPE WebKit offscreen, taken from ngterm (MIT; its `ngterm-wpe` and the gir bindings it generated): web views on an engine of their own, run in a worker process (the app's own executable run again), frames out in shared memory (or dmabufs), input in; the sign-in window's page |
| `crates/ngteams` | the personal build (`docs/personal-build.md`): `ngteams`, the user's own accounts (one or several, all running at once in one window, switched from the rail's avatars and added from its "+") on the user's desktop under the personal fence (`teams-fence`'s `personal` feature, which only this crate enables), its sign-in window included (in its own app, bound to Entra's and the identity provider's hosts through a host gate; its page's engine is `ngteams` run again as a worker). No broker, no sandbox; its data under `~/.local/share/ngteams/` and `~/.config/ngteams/`, its tokens under `ngteams` on the Secret Service. Left out of the root's `default-members`; it refuses a `test-support` build and one `scripts/install-personal.sh` didn't make. Its calls (M3) run on the desktop's PipeWire and its camera through the desktop's camera portal (`ngteams::calling`). Its fence logs every send and denies none. Agents compile and mock-test it, install it only when the user asks, run it on their own only in its read-only mode, and may read its data dirs (never its keyring items or tokens) |
| `crates/teams-headless` | the headless lab binary: one lab account's native client (`teams-core`) from the command line, its fence from `Fence::open_lab`, its tokens from the Secret Service, its store under the lab's data dir; the live acceptance test (`tests/chat_live.rs`) |
| `crates/proto-lab` | the lab harness CLI: Chrome for Testing, the armed browser fence (HTTP and WebSocket), login and status, native auth, read-only probes, captures and the sanitizer, bundle import and search, scenarios |
| `crates/fixture-guard` | scans every `tests/fixtures/` and `docs/` for tokens, signatures, emails, real MRIs and GUIDs, meeting links and passcodes, and a call's network data (public IP addresses, ICE credentials, DTLS fingerprints, TURN credentials), and checks each fixture's `_synthetic` marker |
| `crates/ui-sandbox` | tests, lab runs and the personal build's read-only runs only: the UI sandbox every GUI process runs in, never the desktop's: a headless weston with a niri nested in it (screenshots and window control through `niri msg`), a private D-Bus session bus with no service activation and a fake notification server and tray watcher that record what they get, a virtual keyboard and pointer in the nested niri, a no-op URI opener first on `PATH`, a spawn that passes only the descriptors its caller names; when asked (`SandboxOptions::pipewire`, or `pipewire::PrivatePipeWire` alone), a private PipeWire with its own WirePlumber (policy only), a tone microphone with a loopback of the speaker at a set delay and attenuation, a speaker recorded to a WAV file and a moving test-pattern camera, microphones and speakers added and removed while it runs (a headset plugged in, a keyed or looped voice: `add_microphone`, `add_looped_microphone`, `add_speaker`), each recording's and looped microphone's buffers marked on the wall clock (`pipewire::timeline`), and a fake camera portal on the sandbox's bus; `ui-sandbox-probe`, the window its own tests drive, `ui-sandbox-media-probe`, the PipeWire client its own tests run, and `ui-sandbox-run`, one app in a sandbox of its own (a broker outside it on a socket pair) driven from a control socket (screenshots, keys, text, clicks), which `scripts/run-personal-read-only.sh` uses |
| `spikes/gpui-video` | GPUI on niri (smoke window in a private weston + nested niri), then a 3×3 grid of 720p video (M0 Tasks 15–16) |
| `tests/fixtures/` | synthetic fixtures (made-up values, a `_synthetic` marker), as in every `tests/fixtures/` of a crate or spike: `auth/` (Entra and authz answers), `calling/` (the calls' signaling: the echo call's, a chat's call, an invitation, its answer and decline, a join, renegotiations, group video), `chatsvc/` (the chat service), `csa/` (the account's teams), `graph/` (a profile), `mt/` (the profile lookup's answer), `trouter/` (the push channel), `ups/` (presence) |
| `docs/protocol/` | protocol notes in our own words: `auth.md`, `chat.md` (the chat service's reads and writes as captured), `channels.md`, `graph.md`, `fence-targets.md` (the contacting services, the classifier's rows, member reads), `trouter.md`, `calling-echo.md` (the test call), `calling.md` (lab-to-lab calls: the starts, the ring, answering, declining and joining, renegotiation, group video, the H.264 codec), and `endpoints.md`, one row for every endpoint seen or used, whether it is contacting and how the native client sends it |
| `docs/m0-acceptance.md`, `docs/m1-acceptance.md`, `docs/m2-acceptance.md`, `docs/m3-acceptance.md` | each milestone's acceptance check: numbers, the fence's picture, every live run; M3's also holds the user's real-call checklist for the personal build and its outcome |
| `docs/personal-build.md` | the personal build's rules as the user approved them, and how the code reads them |
| `docs/spikes/` | spike write-ups: `ca-probe.md`, `auth.md`, `gpui-video.md` (+ raw runs), `echo-call.md` (its spike crate is gone: its parts are `teams-media`'s, `teams-call`'s and `teams-core`'s) |
| `docs/backlog/` | deferred work, one file per item |
| `docs/plans/` | milestone plans; finished ones in `docs/plans/completed/` |
| `build/patches/`, `vendor/`, `scripts/vendor-gpui.sh` | GPUI (`gpui-pre` 0.3.9) vendored with agterm-rs's external-surfaces patches, used through `[patch.crates-io]`; `scripts/vendor-gpui.sh --check` verifies it |
| `scripts/lint-report.sh` | refuses inner `#![allow]`/`#![expect]`, lists complexity `#[expect]`s |
| `scripts/own-presence-run.sh` | one `own-presence-*` live run with b's presence reads of a sampled beside it (M2 Task 18) |
| `scripts/install-personal.sh`, `packaging/com.nickalie.ngteams.desktop`, `packaging/icons/` | the personal build's release build and install under `~/.local` (its binary and the sign-in window's web process extension, the install marker, the app's desktop entry and its icon), for the user to run; agents run it only when the user asks |
| `scripts/native-call-run.sh` | one native lab call (`teams-headless` on every end: a caller, callees answering with `watch --answer-lab-calls`, a late joiner), spaced 90 s after the chat's last call, run under the live lock (M3 Task 16) |
| `scripts/reaction-pack.py`, `crates/teams-ui/assets/reactions/` | builds the reaction pack the app draws reactions with: Teams' reaction catalog (its CDN's emoticon metadata) as `catalog.tsv`, and Fluent Emoji 3D's pictures (MIT) packed as WebP into `emoji.bin`; run it again from a `fluentui-emoji` checkout when Teams' catalog version changes |
| `scripts/own-availability-native-run.sh` | one native own-availability run (`teams-headless a availability`) with b's presence reads of a sampled beside it (M2 Task 19) |
| `tools/` | interim Node tools: `ca-probe` (the Conditional Access probe), `lab-login` (the first sign-in of the profiles) |

### WPE WebKit

The sign-in window renders Microsoft's page with the system's WPE WebKit, 2.54 or newer: the distribution's
`wpewebkit` (`sudo pacman -S wpewebkit`), which every build of the WPE crates needs. The build finds it through the
system's pkg-config (`wpe-webkit-2.0`, `wpe-web-process-extension-2.0`), the binaries load `libWPEWebKit-2.0.so` from
the loader's own path, and the engine starts the package's helper processes (`WPEWebProcess`, `WPENetworkProcess`,
`WPEGPUProcess`) from `/usr/lib/wpe-webkit-2.0/`, where their path is compiled into the library; GStreamer loads its
plugins at run time. None of it can be linked in statically, so the installed `ngteams` needs the package at run time
too, and `scripts/install-personal.sh` refuses to build without it.

The worker is the asking program's own executable run again: every `main` that opens a sign-in window
(`ngteams`, `teams-signin`, the window test) calls `teams_wpe::worker::serve_if_asked` early.

## Third-party UI code

The views are built on [`gpui-component`](https://crates.io/crates/gpui-component) 0.8 (Longbridge's GPUI Kit,
Apache-2.0), the UI kit for `gpui-pre` 0.3.9; it resolves to the vendored, patched GPUI above. Its audit (M2 Task 7,
`docs/plans/completed/20261021-native-teams-m2-gpui-daily-driver.md`) found, and `crates/teams-ui/tests/reach.rs` keeps
checking:

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

No Zed code is in the repo: Zed's UI crates are GPL-3.0 and served as a design reference only. The rest of the UI is our
own, on the kit's widgets and gpui-pre's examples (Apache-2.0).

The kit's `Textarea` is tested on GPUI's test context (`crates/teams-ui/tests/kit.rs`: typing, selection, the
clipboard with plain text only, undo, soft wrap, IME composition, Enter and Shift-Enter) and on the real platform in
the UI sandbox (`crates/teams-ui/tests/kit_window.rs`, which also switches the theme live through a fake settings
portal on the sandbox's bus, and clicks a message's link drawn by our rich text renderer, which goes to the sandbox's
no-op opener). The compose box is our own rich-text editor (`crates/teams-ui/src/editor`, tested in
`crates/teams-ui/tests/editor.rs`), drawn with GPUI's text elements as the renderer draws messages.

## The lab

Three dedicated accounts, a, b and c, in the company tenant; nobody uses them but the lab. Everything about them lives
outside the repo:

| where | what |
|---|---|
| `~/.config/teams-lab/lab.toml` | the 3 accounts (alias, UPN, MRI once recorded) and the approved service participants (`echo`, the Teams echo service, with its MRI); optionally the call guests (`[[call_guests]]`, `name` and `mri`: the user's own accounts, which the lab may call, answer and join calls with, and nothing else); optionally `data_dir`. Mode `0600`, never holds a password. `login` and `service-mri` fill in MRIs, nothing else writes it |
| `~/.local/share/teams-lab/` (or `data_dir`) | the lab's data dir |
| `…/chrome/<version>/` | the pinned Chrome for Testing (155.0.8059.84 linux64, checksum-verified) |
| `…/profiles/{a,b,c}/` | each account's persistent Chrome profile, signed in to Microsoft |
| `…/captures/<scenario>/<ts>/events.jsonl` | recorded runs (format in `crates/proto-lab/src/capture.rs`) |
| `…/reference/<name>` | sanitized reference copies made by `export`, read locally to write synthetic fixtures |
| `…/bundles/<version>/` | the unpacked web bundle and its `index.jsonl` |
| `…/fence.log` | JSONL: every fence decision of every run (allows, denies, breaches, with reasons; no tokens, no participants' MRIs, scope ids verbatim), every mutating request let through because it reaches nobody (`allowed`: method, host and path shape, never a query, header or body), and every target the browser fence couldn't arm (its type, failed step, close and containment). Every lab process appends to it at once, each line under an advisory lock on the file |
| `…/chats.toml` | the lab's group chat: the id of the a–b–c chat `abc-chat` created, recorded once as `abc` (`0600`): lab data for the scenarios and clients, which the fence doesn't read |
| `…/store/<alias>.sqlite` | the account's native store (`teams-headless`): conversations, messages, reactions, the outbox (`0600`, its dir `0700`); `<alias>.sqlite-runs/` holds a lock per open run; `<alias>.sqlite-cache/pictures.sqlite` caches the pictures messages show (up to 512 MB, the least recently used let go first) |
| `…/headless/<time>…/` | the native live runs' outputs (`teams-headless` and `chat_live`'s `summary.json`) |
| `…/recordings/` | a native call's audio (`teams-headless … call`): the keyed tone sent and what the far end played (`0700`) |
| `…/call-live/<time>/` | `call_live`'s traces, outputs, recordings and `summary.json` (`0700`); its screenshots go to `…/screenshots/<time>/` |
| `…/live.lock` | the live lock: every live run (a lab browser or a native client against the tenant) holds it with `flock` |
| `…/auth/<alias>.json` | the account's region, partition, region GTMs and skypetoken expiry from `auth` (`0600`) |
| `…/status-<alias>.json` | `login`'s steps, and why it stopped |
| `…/media/tone-440.wav` | the fake microphone's tone for `echo-call` |
| Secret Service, application `teams-lab` | refresh tokens and skypetokens (each with its expiry, capped at its Entra token's), per account |
| lab-automation's `lab-accounts.json` (mode `0600`) | the lab accounts' passwords, read only by `login` |

## Building

`cargo build -p proto-lab` (and `-p teams-headless`) builds a normal lab binary. The tests enable `teams-fence`'s
`test-support` feature through dev-dependencies, and cargo unifies it into everything built together with them
(`cargo test`, `--all-targets`, `--all-features`), the binaries included. Such a binary refuses to run; rebuild with
`cargo build -p <crate>`. A live run's binary is built into a target dir of its own (`cargo build -p teams-headless -p
proto-lab --target-dir target/lab`), so a later `cargo test` can't rebuild it with `test-support` in.

`teams-media`'s calls run on libwebrtc, through LiveKit's `libwebrtc` bindings: a clean build downloads LiveKit's
prebuilt static libwebrtc (about 150 MB, `webrtc-linux-x64-release`) from GitHub, or takes the one `LK_CUSTOM_WEBRTC`
names. A call's H.264 is encoded in software (OpenH264). NVIDIA's NVENC and NVDEC stay out:
the bindings build them only when `CUDA_HOME` (or `/usr/local/cuda`) holds CUDA's headers, so leave it unset for this
build. NVIDIA's decoder library hangs the process at exit while a decoder is still alive.
Its PipeWire binding needs libpipewire's headers and `clang` for `bindgen`.
Which PipeWire a build opens is fixed when it is built: the UI sandbox's private one for the lab binaries and every
`test-support` build, the desktop's only for the personal build (`teams-media`'s `personal` feature, which only
`crates/ngteams` enables and `test-support` overrides); `teams-headless` opens none (its media is files).

A video a message carries is played in its card by the system's GStreamer (`teams-media`'s `player`: `playbin` on an
`appsrc` the engine feeds, its frames and sound to appsinks, the sound on the device seam's speaker). The build needs
GStreamer's development files (`gstreamer`, `gst-plugins-base`, through `pkg-config`); playing needs the plugins for
the video's format at run time: `gst-plugins-base` and `gst-plugins-good` (MP4, playbin's converters) and
`gst-libav` (H.264, AAC and most other codecs). Only software video decoders are used (making a hardware one probes
the GPU's drivers, which can take seconds). A video whose picture no installed plugin decodes says so in its card and
offers the browser; one whose sound none decodes plays silently.

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
proto-lab probe profile a                                          # read-only: the lab accounts' names and pictures from Graph
proto-lab probe teams a                                            # read-only: CSA's teams and channels, a channel's posts and reply chain
proto-lab probe object a <id> [--save <name>]                      # read-only: one AMS object as the account reads it
proto-lab run <scenario> --headless [--timeout 180]                # a scenario under the armed fence (list below)
proto-lab export <capture> <name> [--strict]                       # a capture -> a local reference copy
proto-lab bundles import <msix>           # unpack the web bundle from the Teams MSIX and index it
proto-lab bundles search <regex> [--kind url|host|route|method] [--version <v>]
proto-lab service-mri echo <mri>          # record an approved service's MRI in lab.toml, once
```

**`login`** loads the web client in the account's profile under the armed fence, fail-closed (every write whose
target's members have to be read is denied and logged to `fence.log`, as expected; a new chat naming only lab accounts
is judged by its participants alone and can still pass). When the session has expired it signs in on Microsoft's page
by itself, with the password from `lab-accounts.json`; Entra's sign-in form reaches nobody, so the fence lets it
through. Any multi-factor prompt (a one-time code, a method to pick, an approval in an app, registering a method)
stops it (`docs/backlog/one-time-code-sign-in-under-the-fence.md`). It records the account's MRI from its tokens'
claims. Each step goes to `status-<alias>.json`.

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
the filtered view, the account's own view, then the fence's read (M1 Task 8). `probe profile` reads each lab account's
Graph user record (whole, and as the native client selects it) and picture with the account's Graph token (M2 Task 12).
`probe teams` reads CSA's teams with the `chatsvcagg` token, the conversation list, and up to two channels of the first
team: the thread, the newest messages and one post's reply chain (M2 Task 16). `probe object` reads one media service
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

Every run uses the one classifier of the lab rule (`teams_fence::contacting`, through `proto-lab`'s `Classifier`): a
read passes, a mutating request that reaches nobody passes and is logged `allowed`, a contacting write passes only with
a permit from `Fence::check`, and a mutating request on a contacting service that no row reads is denied. On a Trouter
socket only the session's own socket.io frames and the answers to deliveries pass (a call's receipts and
acknowledgements among them), on a contacting service's socket no frame does, and anywhere else every frame passes
(`fence_ws::classify_frame`). What a scenario holds back on purpose is a labelled forced deny.

A writing scenario first proves enforcement live: a probe post to its chat is force-denied, must fail in the page and
must never show on the other accounts. It reads the target's members live (`LiveMembershipSource`, with the `ic3`
bearer token the page itself sent). It expects only its own forced denies and the web client's requests and frames on
the reviewed expected-deny list (`crates/proto-lab/src/scenario/expected.rs`: bot invokes, the account's own channel
entry and channel properties, the read horizon of the account's own call log, the file browser's prewarm), each with
the reason the scenario doesn't need it. The list only judges the run: everything on it stays denied. A deny on the
scenario's own chat other than its forced denies, one for a partial or refused membership, or a class off the list
fails the run.

- `load`: account a loads the web client, fail-closed (every write whose target's members have to be read is denied;
  a new chat naming only lab accounts can still pass); every deny is expected.
- `search`: account a alone, fail-closed as `load`: the title bar's go-to mode (Ctrl+Alt+G), then a search (Ctrl+E):
  the suggestions as it types, the results and each of their tabs, a message found opened (`docs/protocol/search.md`;
  `PROTO_LAB_SEARCH_QUERY` picks the query, `lab` by default); recorded, every deny expected.
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
  in `chats.toml`, once; after that a messages it and b and c must show it, each write permitted by a fresh read of
  the chat's member list, as for any chat.
- `ab-trouter`, `ab-trouter-quiet`: b's push channel while a sends, edits and reacts (the Trouter session's own
  frames, the endpoints' registrations and the answers to deliveries pass), observed for 16 minutes; the quiet one
  holds back every `user.activity` with a forced deny (`docs/protocol/trouter.md`).
- `chat-shapes`: a and b, as `ab-trouter`: a sets and takes back each quick reaction
  the hover bar offers, types for 45 s, leaves the text for 35 s, empties its compose box and waits 15 s more
  (`TYPE_FOR`, `PAUSE`, `AFTER_EMPTYING`) while b's open chat records its typing
  indicator, sends a message of three lines with a link, and mentions b in the recorded group chat
  (`docs/protocol/chat.md`).
- `abc-everyone`: a and b in the recorded group chat (checked readable first, as `abc-chat`): after the enforcement
  test a mentions everyone, picked from the picker, its post held back by the probe's forced deny and read; only when
  the lab rule reads it as a message to the chat adding nobody does a send a second one, which b must show
  (`docs/protocol/chat.md`, "A mention of everyone").
- `ab-meeting`: account a alone, after the enforcement test: a schedules a Teams meeting with b from the scheduling
  form (the new-meeting deep link, b picked in its attendees), then opens it from the meeting's chat (Details),
  changes its title and sends the update, and cancels it; each write judged by the meeting's attendees, read fresh
  from the middle tier (`docs/protocol/calendar.md`, "Writing an event").
- `channel-create`: account a alone, fail-closed as `load`, every write that could make a channel force-denied on any
  host from before the page loads: a's team's "..." menu and its "Add channel", the "Create a channel" dialog and its types,
  then the dialog filled and Create pressed for each channel type and layout, what it sends recorded and held back
  (`docs/protocol/channels.md`, "Creating a channel"); nothing is made.
- `ab-meeting-join`: account a alone, after the enforcement test: a schedules a Teams meeting with b, opens its join
  link and clicks the pre-join's Join now, only on that meeting; the pre-join's subscription is judged by the meeting's
  chat, read fresh, and everything that would enter a call is held back with a labelled forced deny
  (`docs/protocol/calling.md`, "Joining a meeting").
- `own-presence-activity`, `-endpoint`, `-both`, `-manual-held`, `-manual`: account a alone, as `ab-trouter`. Its
  own availability reaches nobody, so it passes; each run holds back with a labelled forced deny what it leaves out:
  `user.activity` alone, UPS's endpoint `PUT` alone, both, then a manual status (Busy, Do not disturb, Away,
  Available, reset) held back to capture it and let through.
  b's view is read outside the run: `scripts/own-presence-run.sh <scenario> <samples> <report file>`, under the live
  lock, samples `teams-headless b presence` beside the run (`docs/protocol/trouter.md`, "Own presence").
- `echo-call`: account a places the web client's test call to the echo service, with a fake microphone playing a
  440 Hz tone. The run's classifier knows the echo service's MRI (`Classifier::with_test_call_callee`): what sets up
  the account's own endpoint reaches nobody and passes, and the call start maps only in exactly its captured shape, a
  new call of a and the echo (`teams_fence::contacting`'s calling row, `docs/protocol/calling-echo.md`), which
  `Fence::check` permits; the echo's `add` is judged by its invitee, and the call's other sends (the endpoint
  metadata, the channel parameters, the acceptance's acknowledgement over Trouter, `leave`) pass, logged, once its
  start was let in. The run turns the call's noise suppression off (it takes the tone for noise), waits until the
  hooks' audio tap reads the tone coming back in the page, hangs up, and exits 0 only when the start, the `add`, the
  acknowledgement and the `leave` were permitted, the statistics show audio both ways and the tone came back, with no
  deny off the expected-deny list. An incoming call is never answered.
- `ab-call`, `abc-call` (`--call start-held|answer|decline|cancel|timeout|video|dropped|join`): account a calls b
  from their 1:1 chat, or the recorded a–b–c group chat, every page with fake devices, muted. `start-held` (a alone)
  holds back each call start with a labelled forced deny; the other steps let a's start through (a call in the chat it
  carries, the caller and its callees all lab accounts: `docs/protocol/calling.md`), hold back nothing of the call (the
  fence judges each answer, decline and join, and lets the rest of the call through), and play out the ring: `cancel` (a hangs up),
  `timeout` (nobody acts), `decline` (each callee declines; its rejection must have been let through), `answer` (each
  callee answers; the call must carry audio both ways on every page, then everyone hangs up), `video` (as `answer`,
  every camera on), `dropped` (as `answer`, the last callee's calling socket closed for a while, which must close),
  `join` (`abc-call` only: c lets its ring end and joins the running call from the chat's header; it must reach the
  call screen and its join be let through). A failed run hangs up every page that shows its call before it closes,
  and a start the service ends at once because the chat still has a running call fails the step then. A callee clicks
  only on the run's own call (every invitation its page
  received is a's call of this run, from a to that callee in the run's chat, and the page shows one incoming call, or
  for a join the run's chat with one join), and the fence decides each answer and decline again on the account's own
  offered invitation, each join by the lab-only chat it names. `keep-alive` (`ab-call` only) is `answer` held 47 minutes, past the call's keep-alive
  interval, to record the call's keep-alive: every page's fence must let one through, and a call that ends before the
  hold is over fails the run.
- `ab-call-native` (`TEAMS_HEADLESS_BIN` naming a lab build of `teams-headless`, release for timing): b's web client
  and a's native client in their 1:1 chat. a's native client calls b and b's page answers (only on the run's own
  call); b's page calls a and `teams-headless a watch --answer-lab-calls` answers; then a's native client calls b while
  `teams-headless b watch --calls` rings too, and b's page answers: b's native client's ring must end (answered
  elsewhere). Every side plays the page's tone in bursts and must hear the other's (the page by its audio tap, a native
  client in its recording); the native clients' output goes into b's capture dir. The first two calls carry video
  both ways (`--video`): the page turns
  its camera on and must decode the native client's frames, and the native client turns its camera on 5 s in
  (`--camera-after`) and must decode the page's.
- `abc-call-mixed` (`TEAMS_HEADLESS_BIN` as `ab-call-native`'s): a's native client calls the a–b–c group chat with
  video and b's and c's web clients answer (only on the run's own call); each page must hear the tone and decode
  video, and a's native client must show both on receiving lines of their own; c's page hangs up (a must show b
  alone), then joins the running call from the chat's header (a must show both again, c's line bringing frames after
  the join, as the native client's `video_lines` counts say); a's native client must have
  heard the tone and decoded video on two lines. A failed `ab-call-native` or `abc-call-mixed` run hangs up every
  page that shows its call before it stops its native clients. `scripts/native-call-run.sh` runs the same group call
  natively (a calls, b and c answer, or `--join c` for a late join; `EXTRA="--video --camera-after 5"`), a call in a
  chat at least 90 s after that chat's last one, and one the service refused because the chat still had a running call
  placed once more 90 s later.
- `ab-call-app` (`--call answer|place`): b's web client as the far end of a call with a's app, spawned by `call_live`
  (below), which runs the app; the two keep in step by this scenario's progress lines on stdout (`ab-call-app:
  <step>`) and the app's trace. `answer` waits for a's call and answers only the run's own; `place` calls a from the
  chat's header. Either way the page must hear a's tone and decode a's video once its camera is on.

Every lab browser runs with its output muted and Chrome's fake audio manager (`--mute-audio`,
`--disable-audio-output`): nothing is played aloud and the desktop's audio server is never opened; a call scenario's
browser also lets audio start without a click, and reads what it receives in the page.

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
teams-headless b watch --answer-lab-calls|--calls [--tone 1000] [--bursts] [--hear <hz>] [--record <wav>] \
  [--tone-seconds 180]                    # as watch, the calling stack registered: answers each offered lab call, or none
teams-headless a presence [--wait 30] [--mri <mri>]...  # b's and c's presence, and anyone's by MRI, read from UPS
teams-headless a profile <mri>... [--wait 30]           # where the engine's name for each comes from (Graph, the middle tier's lookup)
teams-headless a search <query>                         # the service's search (Substrate): counts of each kind found, the chats by id
teams-headless a series [--keep|--cleanup]              # an appointment series of its own (nobody invited): created, read back, its free/busy read, deleted; --cleanup deletes what a run left
teams-headless a availability [--manual busy|do-not-disturb|away|available] [--manual-after 60] [--reset-after <s>] \
  [--hold 300]                            # a's own availability, from its push channel's endpoint
teams-headless a call echo|b|abc|<id> [--tone 1000] [--bursts] [--hear <hz>] [--record <wav>] [--seconds 45] \
  [--wait 60] [--tone-seconds <s>] [--video [--camera-after <s>]] [--media <movie> [--media-from <s>] [--media-loop]]
                                          # a call through the engine, its media on files; held, hung up, its tone looked for
teams-headless c join abc [--ring-wait 180] [--tone …] [--seconds 45] [--wait 60] \
                                          # the group chat's running call joined late, once its invitation rang out here
teams-headless a join-meeting <join link> [--tone …] [--seconds 45] [--wait 60]
                                          # a meeting's call by its join link, as the calendar's Join: started when nobody has
```

A chat is `a`, `b` or `c` (the 1:1 chat with that account, from the MRIs in `lab.toml`), `abc` (the group chat
recorded in `chats.toml`), a call guest's name (or `guest` when `lab.toml` names one: the 1:1 chat with it, which the
fence lets only a call reach), or a conversation id. Each write first catches up (its summary on stderr), then waits up to
`--wait` seconds (120) for a write that has to wait out a `Retry-After`; it exits non-zero unless the write went
through. `watch` answers no delivery and no call (but with `--answer-lab-calls`, below); it exits 0 on SIGINT or SIGTERM,
non-zero when the push channel
stopped on its own. `presence` reads only (a read sent as a `POST`, which reaches nobody whoever it names); it waits up to
`--wait` seconds (30) for each person's presence to be told, then prints one `presence <alias or MRI> <availability>`
line per person, `Unknown` for one not told in time or whose read failed; it refuses more people (the other lab accounts
and each `--mri` together) than the engine watches at once (20). `profile` reads only too: it asks the
engine for each person, which looks them up (Graph's user record, then the middle tier's profile lookup for a name
Graph doesn't give) unless its store has them fresh or a failed lookup's retry wait runs, waits up to `--wait` seconds
for the lookups it started (none when it started none), and prints
`profile <alias or MRI> <graph|short_profile|message|known|none>`, never the name. `availability` sets the
account's own availability (others see it, and it reaches nobody, so the lab rule allows it): once its push channel's registration went through it
sends the endpoint's presence `PUT` (available), then the manual status at `--manual-after` and its reset at
`--reset-after` seconds after the `PUT`, each under the fence's permit for the account's own availability; it prints
each step with its time and ends `--hold` seconds after the `PUT`, which takes the endpoint down. `call` places one
call (M3 Task 15) once its calling socket registered: `echo` is the test call of `lab.toml`'s echo service, anything
else a chat (a catch-up first); each write of it goes through the fence. Its media is files, never an audio server:
the microphone plays a tone at `--tone` Hz keyed on and off in 200 ms slots of a fixed pattern (written first as
`…-sent.wav`), the speaker records to `--record` (`<data_dir>/recordings/<alias>-call-<ms>.wav` by default); the call is
held `--seconds` once its media connected (45: the echo plays its recording back until about 41 s after that), then
hung up (and hung up when it hasn't connected within `--wait` seconds; one whose media isn't set up by then isn't
placed). It prints each event, `call_summary` (setup time, held, how it ended) and `echo`: what the recording holds of
the keyed tone at `--hear` Hz (its own tone by default): its level, the stretches of it that came back with their
offsets, and the round trips' p50, p95 and max. It exits 0 only when the call connected, was held and hung up, and,
for `echo`, the tone came back. It answers nothing. `--bursts` plays the tone in bursts (1.6 s on, 0.9 s off, a lab
browser's page hears those) instead of keyed, `--tone-seconds` cuts the tone short (silence after it: a long call's
files stay small), `--hear` may be given more than once (one report each, a group call's every other tone), and an
offset or round trip of a far end in another process may be negative. Line up the files with the far end's
`samples` line (`microphone_started_ms` and `speaker_started_ms`, wall-clock times of the first successful source
frame and sink write). Device construction may take time before either stream starts, so the separate `devices`
line records opening diagnostics and is not a sample origin. After the call it also prints `tone` (how much of the
tone at `--hear` the recording holds, however it was keyed). `join` (M3 Task 16) waits
`--ring-wait` seconds for an invitation to the group chat's running call to ring here, lets it ring out unanswered,
then joins the running call (a join is judged like a start in that chat) and holds it as `call` does. `watch
--answer-lab-calls` registers the calling stack too and answers each call the engine offers: only an invitation from
a lab account or a call guest naming only them, which the lab fence offers (every other one rings unanswered and nothing is
sent for it), each answer judged by the fence again; the flag exists in `teams-headless` alone. `watch --calls`
registers the calling stack and answers nothing (an offered call rings until its ring ends). Both play `--tone` (for
`--tone-seconds`, 180 s by default) and record each answered call, and print what the last one's recording holds when
they end, as text: neither takes `--json`.
`--video` (`call`, `join`, `watch`) takes video: a 1:1 call's main line,
a group call's main and receiving lines, the frames received counted per line (`video_lines …` every 2 s, `video …` at
the end); the camera, a test pattern carrying each frame's index, stays off unless `--camera-after` turns it on that
many seconds after the media connected.
`--media <file> [--media-from <seconds>] [--media-loop]` (`call`, `join`, `watch`) makes a movie the microphone and
the camera instead: `ffmpeg` (and `ffprobe`, from `PATH`) decodes it as the call runs from that offset, its first audio
stream mixed down to 48 kHz mono (raised 6 dB) and its first video stream scaled to 1280×720 at 24 fps, each through a
pipe and a short queue at the device's pace (nothing decoded to disk; the decoders are killed with their devices). A
camera turned on later starts as far into the movie as the time since the microphone started; a looped movie wraps at
its end, one played once goes silent on its last picture. It implies `--video`, turns the camera on as soon as the media connected
(unless `--camera-after` says later), prints `media file=<name> from_s=… duration_s=… source=… camera=…` first, and
looks for no tone in the recording (`echo not_applicable media=true`). The movie lives under the lab's data dir
(`<data_dir>/media/`), never in the repo; say `teams-headless b watch --answer-lab-calls --media
~/.local/share/teams-lab/media/bbb/big_buck_bunny_1080p_h264.mov --media-from 120`.
`TEAMS_HEADLESS_LOG=debug` logs our own crates' debug lines on
stderr (Trouter's packets and deliveries by kind and size, never a payload); nothing else is logged.

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

The window is three panes: the rail (Chat, Settings), the sidebar (resizable, under "Chat" and the connection state
while it isn't up: "Chats", the store's chats by last activity with names, previews, times and unread counts, then
"Teams and channels", each team with its channels, a private channel with a lock; a section's header or a team's line folds it, and a team's "..." offers "Add channel", the "Create a channel" dialog as Teams has it) and the conversation
pane, which shows the chat opened in the chat view under its avatar and name: its messages from the store, the newest at the bottom (older pages load as the top
comes near), grouped by sender within 5 minutes, with day separators, a "New messages" divider at the read horizon as
the chat opened, edited and deleted markers, reactions, thread events, a "Jump to newest" pill once scrolled up, and
who is typing. With the message list focused (a click on a message), Up and Down select a message, and `r` (reply),
`a` (react, then `1`–`4` for a quick reaction or `5` for every reaction), `e` (edit, the account's own), Ctrl+C (copy
its text), Delete (the account's own) and Escape (clear the selection) act on it. A message under the pointer shows
Teams' hover bar over its bubble's top right: the quick reactions (the account's most used on its stored messages
first, then Teams' 👍 ❤️ 😆 😮 😢 😡), React, Edit on the account's own, Reply and "More options" (Copy text, Delete),
each named by its tooltip with its key. The compose box is drawn as Teams' (a rounded box with its accent line along
the foot while it has the focus) with Format, Emoji, Attach, Loop and Actions and apps (the last three disabled: not in
this app yet) and, behind a divider, Send inside it on the right: Emoji (also Ctrl+. or Ctrl+;) opens every emoji Teams
offers, searchable, and puts the one picked at the cursor; Send does what Enter does. It is a rich-text editor, its
formats shown as they will look and sent as Teams' own editor sends them: Ctrl+B, Ctrl+I, Ctrl+U, Ctrl+Alt+X
(strikethrough), Ctrl+Shift+C (inline code), Ctrl+Shift+B (a block of code), Ctrl+Shift+7 and Ctrl+Shift+8 (numbered
and bulleted lists; Enter goes on to the next item, Shift+Enter breaks the line, Tab nests), Ctrl+Alt+4 (a quote),
Ctrl+Alt+1–3 and Ctrl+Alt+P (headings, a paragraph), Ctrl+Z and Ctrl+Y; a paste keeps the formats of what another
program or the box itself copied. Format (Ctrl+Shift+X) expands the box with Teams' format bar (the marks, the lists,
highlight, font colour and size, quote, link, code, and More: paragraph styles, indent, a horizontal line, clear
formatting, undo and redo; Discard empties the box); expanded, Enter starts a new paragraph and Ctrl+Enter sends. Ctrl+K opens the chat switcher (fuzzy over the chats' names; Enter opens,
Escape closes). The title bar runs across the top as Teams': the launcher (the app's places), the list pane's toggle, Back
and Forward (Alt+Left, Alt+Right), the search box, Settings and more (the settings, the zoom, Copy diagnostics) and the
account's avatar with its presence (the account menu). Ctrl+E (or Ctrl+Shift+F, or a click) searches: suggestions as you
type (people, chats, channels, files; Up, Down and Enter open one), Enter shows the results with Teams' tabs (All,
Messages, People, Files, Group chats, Teams and channels; a message opens its chat at it), Escape closes or leaves them;
Ctrl+Alt+G goes right to a chat or channel by its name. A click on a picture in a message opens it in a window of its own at its full size, fitted to the
window and never enlarged past it, its sender and time over it: Zoom in and Zoom out (also Ctrl+= and Ctrl+-, Ctrl and
the wheel), fit or its own size (Ctrl+0 fits), dragging or the wheel to pan a picture larger than the window, Previous
and Next (Left and Right) through the chat's pictures, Download (Ctrl+S: the desktop's file chooser, starting in the
downloads folder) and Close (Escape). Settings take the place of the sidebar and the conversation pane, as in Teams: a column of pages
(General, Appearance, Notifications, Privacy, Calls and devices where the app has calls, Diagnostics; Up and Down move
between them) beside the page shown, each a set of titled boxes of rows. The rail's Calendar (Ctrl+4; Ctrl+2 goes back
to the chats) takes their place the same way, as Teams' calendar app: the month and "My calendars" in its own pane, the
toolbar's Today, previous and next (Left, Right and T while it has the focus) and views (Day, Work week, Week, Month;
Ctrl+Alt+1–4), the account's events read from the middle tier's calendar view, and an event's card (Chat with
participants and a split Join, there at any time, whose menu has Join and Copy join link; Join opens the meeting's
pre-join in a window of its own (the camera's preview and switch, background filters, the microphone's switch and the
devices, Cancel, Join now), and Join now starts the meeting's call when nobody has; for the account's own events Edit,
and Cancel meeting, Delete or Remove from calendar, each confirmed). New event (its menu: an event or a Teams meeting), a
double click on an empty half hour of the grid, and an event's Edit (or a double click on it) open the scheduling form
in a window of its own ("New event - Calendar"): Event and Series (a series' repeat: Teams' presets, a custom
recurrence, its end; an occurrence's change edits it or, by Series, the whole series), the title, required and optional
attendees (people the account knows and people the directory finds, or any address), the date and times or all day,
the suggested times and the Scheduler (everyone's busy times beside the form), the location, the Microsoft Teams
meeting switch, the text in the rich-text editor (Format, Emoji), Show as, the reminder, categories, private and Print
(the event's page in the browser, which prints it), the day beside it with the event's block to drag; Save (Send, Send
update; Ctrl+S) writes through the engine and the calendar reads its days again (`docs/protocol/calendar.md`, "Writing
an event"). "Join with an ID" says it isn't available yet. The settings (the selected chat, typing, read
receipts, notifications, the idle time, the manual status, the log's level, the theme, the font and the
time format, the system's clock or a 12-hour or 24-hour one) live in
`<data_dir>/ui/<alias>.toml` (`0600`, its directory `0700`); a file that can't be read gives the defaults, a bad value keeps its default, and either is
printed and shown in the settings view.

The diagnostic log (`teams-log`) is installed before the engine starts, at
`<data_dir>/logs/<alias>/teams-app.log` (`0600`, its directory `0700`); at 8 MiB it is rotated to `teams-app.log.1`
(and on to `.4`; older files are dropped). It takes only our own crates' lines, never a third-party crate's or the UI
kit's, and is written from a thread of its own (lines that would make the window wait are dropped and counted).
Levels: `error`, `warn`, `info` (the default: what happened by kind and id, request lines by method, host, path,
status and latency, the push channel's connects and drops, catch-ups, outbox rows with their `clientmessageid` and the
fence's decision, renewals, migrations, failures, a panic with its backtrace; no body, query, header value, message
text or name) and `debug` (every request and response in full, method, URL with its query, headers and bodies, and
every Trouter frame both ways). The level is the settings' `log_level`, changed live in the settings view;
`TEAMS_APP_LOG=<level>` overrides it for the run (`trace` is taken as `debug`). The logger redacts every credential
itself (the `Authorization`, cookie and token headers, token, secret and signature fields, sign-in's codes,
`user.authenticate`, Trouter's `connectparams` signature, URLs inside JSON by their query, then a credential's
`name=` value on any line in any escaped form, a SAS link's `sig` in a message's HTML say, a call SDP's
`a=ice-pwd:`/`a=ice-ufrag:` values, then any JWT-shaped string, `skypetoken=` or `Bearer ` value). A log that can't be opened is printed and shown in
the settings view, and the app runs without it. "Copy diagnostics" (the settings view, or "Copy diagnostics" in the
Ctrl+K palette) puts the app's version and build profile, the level, the log's last 200 lines (at most 128 KiB of
them, the newest kept; a newest line longer than that keeps its start, one over 1 MiB is left out and said so) and
the last 40 `fence.log` lines on the clipboard; the text is collected off the window's thread. **A debug log holds
message text, names and whole answers of the service: never share it.** When the copied text holds any `debug` line it starts with "contains full requests and responses: not safe
to paste", and the settings view says so too. A lab run's debug log is lab data like a capture: it never goes into the
repo, not even sanitized.

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
chat opened). `crates/teams-app/tests/ui_tour.rs` walks the app on `teams-mock` through its views and screenshots each
as `target/ui-shots/tour-<step>.png` (no chat open, a chat with a message's actions, a reply begun, the account menu,
every settings page with Calls on made-up devices, the appearance and the chat in Teams Dark): the shots a change of
the UI is looked at by.

Calls (M3): the app's engine gets its calling on the UI sandbox's PipeWire (the only one a lab build may open; without
it the app runs without calls). A chat's header has Call once the calling endpoint is registered (Join
where an invitation named a running call in that group chat); an invitation the engine offered rings with a ringtone on
the default output, a banner (Answer, Decline) and a notification with Answer and Decline on the bus the app is given,
the caller named as the invitation names them, all gone once its ring ends. A call opens a window of its own, as
Teams' does, and the main window's chats stay usable beside it: its header names who the call is with, its state and
duration, and has Teams' controls (People: the participants' pane; More: Video effects and Device settings; the camera
and the microphone, each with a menu of the devices, the camera's with Video effects too; Leave); its stage shows who it rings, then a tile per participant (its
video when a line shows it, else its initials, a border while it speaks) with the self view at its bottom right. The
window closes once the call ended; closing it leaves the call, without asking, as Teams' does. The main window keeps an
in-call strip above its panes (who the call is with, its state, "Return to call", the microphone and camera toggles
and Leave), then how the call ended (a 1:1 call the callee declined is taken by their voicemail, as captured, and
says so). The camera is off at every start, join and answer; only the user turns it on.
Keys, in either window: Ctrl+Shift+M mutes or unmutes, Ctrl+Shift+O turns the camera on or off, Ctrl+Shift+H leaves
the call; in the main window Ctrl+Shift+C calls the chat shown, Ctrl+Shift+J joins its running call, Ctrl+Shift+S
answers, Ctrl+Shift+D declines; the
switcher (Ctrl+K) offers the commands that fit the moment ("Call", "Make a test call", "Join call", "Answer call", "Decline
call", "Leave call", "Mute or unmute", "Turn camera on or off"). Nothing answers or declines a call but the user. The settings list
the sandbox's microphones, speakers and cameras ("Follow the system" by default; a choice moves a running call's
device without dropping it), "Make a test call" under them (its results, once it ended, in its window: the
microphone, the speaker, the camera and the network as the call showed them), the video switch ("Video in calls",
on by default) and the video effects, as the call window's Video effects pane has them too: a self-preview (what a
call sends while its camera is on, else the camera turned on for the preview only by the user's Preview) over Teams'
background tiles (None, Blur, "Add new" for a picture of the user's, copied into `backgrounds/` beside the settings
file, six pictures of the app's own drawn by `tools/backgrounds/make.py`); the choice is kept between calls and changes
a running call's camera at once. The person is cut out by MediaPipe's selfie segmenter (Apache-2.0, converted to ONNX
by `tools/selfie-segmenter/convert.py`) on tract, on a thread beside the camera's (`teams_media::video::effects`).
`crates/teams-app/tests/call_window.rs` runs a's and b's apps in two sandboxes, each with its private PipeWire's tone,
on `teams-mock` with its calling media: a calls b by the keyboard, b's ringtone is in b's recording, b answers from the
notification, each tone is in the other's recording, a hangs up; b declines a second call and a's strip says it went to
voicemail; screenshots `teams-app-call-incoming.png`, `teams-app-call-connected.png` (a's call window),
`teams-app-call-side-by-side.png` (a's two windows), `teams-app-call-declined.png`.

The live acceptance of calls (M3 Task 21, `tests/call_live.rs`, `docs/m3-acceptance.md`) is an orchestrator as
`ui_live` is: a UI sandbox per lab account with its private PipeWire and fake camera, the normally built release app
for a, b and c with each one's broker outside, driven by the keyboard and the notifications' actions, read by the
traces, the recordings and `fence.log`; b's web client, where a case needs it, is a spawned `proto-lab run ab-call-app`.
Its cases (`CALL_LIVE_PHASES`, comma-separated, all by default): `test-call` (the echo's round trip), `web` (the app
and b's web client call each other, audio and video), `one-to-one` (ringtone, answer from the notification, audio both
ways, the echo check on a loop of b's speaker into its microphone, a microphone switched mid-call, video, CPU and fps),
`decline` and `group` (a–b–c with video, c leaving and joining again). `CALL_LIVE_APP_LOG` sets the apps' log level.
Outputs go to `<data_dir>/call-live/<time>/`. Run it under the live lock:

```sh
cargo build --release -p teams-app -p proto-lab --target-dir target/lab
flock ~/.local/share/teams-lab/live.lock timeout --signal=INT --kill-after=60 2400 \
  env TEAMS_APP_BIN=$PWD/target/lab/release/teams-app PROTO_LAB_BIN=$PWD/target/lab/release/proto-lab \
  PROTO_LAB_LIVE=1 cargo test -p teams-app --features live --test call_live -- --nocapture
```

The live acceptance test (M2 Task 25, `tests/ui_live.rs`, `docs/m2-acceptance.md`) is an orchestrator only: it opens no
fence, holds no token and sends nothing. It starts a UI sandbox per lab account, each app's broker outside it with the
user's bus, spawns the normally built app for a, b and c, drives them by keyboard (the switcher, the compose box, the
message list's keys, the search, the switcher's status commands) and reads their view traces and `fence.log`;
screenshots go to `<data_dir>/screenshots/<time>/`, traces and outputs to `<data_dir>/ui-live/<time>/`. Run it under the
live lock:

```sh
cargo build -p teams-app --target-dir target/lab
flock ~/.local/share/teams-lab/live.lock timeout --signal=INT --kill-after=60 1800 \
  env TEAMS_APP_BIN=$PWD/target/lab/debug/teams-app PROTO_LAB_LIVE=1 \
  cargo test -p teams-app --features live --test ui_live -- --nocapture
```

## `ngteams` (the personal build)

The user's own Teams, built and installed by the user (`docs/personal-build.md`). An agent installs it, or starts it
for the user, only on the user's request, each time; on its own it runs it only in its read-only mode (which sends
nothing anyone sees or that changes the account, enforced by the app), to look at, screenshot and navigate it and read
its logs and store ([The read-only mode](#the-read-only-mode-scriptsrun-personal-read-onlysh)); it may read
`~/.local/share/ngteams/` and `~/.config/ngteams/`, never signs it in, and never reads the `ngteams` keyring items or
the user's tokens:

```sh
scripts/install-personal.sh            # a release build of ngteams (its sign-in window included), installed under ~/.local
ngteams                                # the first run asks for your work account in its window, then signs it in
ngteams --account you@your-tenant.com  # sign that account in (adding it when it is new), then start
ngteams --forget you@your-tenant.com   # sign out and forget that account (quit Teams first; its store stays)
```

Several accounts run at once in one window: the rail's avatars above Settings switch between them, the "+" under them
adds one through the same sign-in, and an account's menu (a click on its avatar) has "Sign out and remove". Every
account notifies, each notification naming its account, and the tray counts every account's unread; calls are one at
a time across the accounts.

The first run shows what the app sends on its own (typing, read receipts, presence) and sends none of it until that
screen is confirmed. A refresh token Entra refuses shows "signed out", stops the engine and opens the sign-in window
again. It may do whatever the Teams client lets its user do; its fence logs every send to
`~/.local/share/ngteams/fence.log` and denies none. It sends what the user did or a setting the user turned on, and the
follow-ups that needs. A plain `cargo build` at the root leaves it out; `cargo build -p ngteams`
gives binaries that refuse to start (no install marker).

Calls (`docs/personal-build.md`, "Calls"): 1:1 and group-chat calls today, audio and video, on the desktop's PipeWire
(fixed when the build is made) with the camera through the desktop's camera portal; the fence limits none of them. A
call starts or joins on the user's action; any invitation addressed to the account rings (one call at a time across the
accounts, as a Teams client) and is answered or declined only by the user. "Make a test call" (Settings, Calls and devices, or the switcher) calls the echo service named in
the web bundle. The microphone opens only once
the call passed the fence; the camera is off at every start, join and answer. Calls carry video on the OpenH264
built into the app, unless the settings' "Video in calls" is turned off. Without a PipeWire the
app runs without calls. The user's own real-call run is the checklist in `docs/m3-acceptance.md`.

### The read-only mode (`scripts/run-personal-read-only.sh`)

How an agent runs the personal build on its own: the installed `ngteams` in its read-only mode, on a snapshot of the
user's files, in a UI sandbox of its own, its tokens through a broker outside it. The user's own running `ngteams` isn't
touched (no second push connection on the account, no endpoint or presence, the store read only by a consistent copy).

```sh
scripts/run-personal-read-only.sh you@your-tenant.com   # snapshot, sandbox, broker, app; prints where everything is
scripts/run-personal-read-only.sh --shot chats          # the window, to <run>/shots/chats.png (--screen: the output)
scripts/run-personal-read-only.sh --key ctrl+k          # a key (Down, Enter, Escape, Tab, a, ...); --type <text>
scripts/run-personal-read-only.sh --click 300 200       # a click in output pixels (1280x800); --windows: niri's list
scripts/run-personal-read-only.sh --stop                # stops it all and deletes the run (--keep keeps it)
```

The run lives in `~/.local/state/ngteams-read-only/run/`: `shots/`, the view trace `trace.jsonl`, `app.log`,
`broker.log`, and the snapshot (`snapshot/config`, `snapshot/data`), whose `data/fence.log` gets a `refused_read_only`
line (method, host, path shape) for every send refused and whose `data/logs/` is the app's diagnostic log. The script
refuses a binary without the mode (`TEAMS_BIN` overrides `~/.local/lib/ngteams/ngteams`) and builds `ui-sandbox-run` into
`target/lab` first. The pieces, each usable alone:

```sh
ngteams snapshot --to <dir>                                # a copy of your Teams files (stores via VACUUM INTO, read-only)
ngteams secrets --account <upn> --fd <n>                   # the broker: get only; put and delete refused (outside the sandbox)
ngteams --read-only --account <upn> --data <dir> --secrets-fd <n> [--trace-fd <n>]   # in the UI sandbox only
ui-sandbox-run serve --dir <run> --broker <prog> [--broker-arg A]... --app <prog> [--app-arg A]...
```

What the mode refuses, below the views and logged: messages, edits, deletes, reactions, typing, read receipts, chats
created, availability and status, registrations and every call write (the transports are read-only:
`HttpTransport::read_only`); it opens no push channel (it catches up by reads every minute) and no calls (no PipeWire,
camera or portal). Reads of every kind go, and the session's token renewal, whose new tokens stay in memory: the broker
serves `get` only, so the stored refresh token is never rotated or overwritten. An account whose stored token can't be
used shows "signed out"; the mode never opens the sign-in window. The views offer no write (the boxes say "Read-only",
and so does the chat list's header in place of a connection that never comes up), the rail neither adds nor removes an account, and there is no tray item. Its window opens only in the UI sandbox.

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
fixture of the same shape with made-up values. A call's fixture holds made-up network data: addresses in the
documentation ranges (`192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`, `2001:db8::/32`), placeholders or values
that say they are made up (`SyntheticUfrag`) for ICE and TURN credentials, and a placeholder or one byte repeated
(`AA:AA:…`) for a DTLS fingerprint; the guard flags anything else. Fixtures live in any `tests/fixtures/` of the workspace: the top-level
one, a crate's, a spike's (`spikes/*/tests/fixtures/`). Every fixture declares itself synthetic: a JSON file has a
top-level `"_synthetic"` note (what it is shaped like, that the values are made up), a JSONL file has one on its first
line (a header line of its own, or on every line), and any other text file starts with a `_synthetic: …` line, bare or
after its own comment opener. Only an empty `.gitkeep` is not a fixture. `cargo test -p fixture-guard` fails on an
unmarked fixture (`committed_fixtures_are_marked_synthetic`) and on anything the guard finds in any `tests/fixtures/`
or `docs/` (`committed_fixtures_and_docs_are_clean`). The only directories exempt from the marker are test inputs that
are not shaped like anything Teams sends, listed in `fixture_guard::synthetic::TEST_INPUTS` (proto-lab's test pages
and mini bundle); the guard still scans them.

## Tests

The checks, picked per change by what it can break (CLAUDE.md, "Checks"):

```sh
cargo fmt --all --check
cargo check -p <crate> [-p <crate> ...] --all-targets
cargo clippy -p <crate> [-p <crate> ...] --all-targets --all-features -- -D warnings
scripts/lint-report.sh
cargo test -p <crate> [-p <crate> ...] [--test <name>] [<test filter>]
cargo test -p fixture-guard   # what's under any tests/fixtures/ or docs/
```

The whole workspace (`cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test
--workspace`) is the widest check.

### Coverage

Install `cargo-llvm-cov` (`cargo install cargo-llvm-cov --locked`) and the pinned Rust toolchain's LLVM tools
(`rustup component add llvm-tools-preview`). The reporting helper requires Python 3.11 or newer; the wrapper uses
Bash and `flock`. Run a selected crate's tests and local coverage gate with:

```sh
scripts/coverage.sh -p teams-store
scripts/coverage.sh -p teams-store --require-target   # require 90% on every measured metric
scripts/coverage.sh -p teams-log --no-gate            # measure a crate before establishing its baseline
scripts/coverage.sh -p teams-log --test-threads 3     # explicitly change test-harness concurrency
scripts/coverage.sh -p teams-log --profile-mode ordinary  # diagnostic exit-time profiling
scripts/coverage.sh --webrtc-cache /absolute/path/to/linux-x64-release  # reuse a matching SDK
python3 -B -m unittest discover -s scripts -p test_coverage_report.py
```

Coverage runs every selected test with `--test-threads 1` by default. Overlapping harness cases can fork while
another case still holds a generated fake-browser script open for writing, causing `ExecutableFileBusy` when it
starts. Serial execution avoids that overlap in coverage runs; ordinary `cargo test` keeps its configured harness
concurrency. `--test-threads N` accepts a positive integer to change the harness concurrency; concurrency exercised
inside a test stays part of that test.

`coverage.toml` holds the 90% target and measured standalone baseline floors for all 21 runtime crates. Raise floors
as tests improve; never lower them or exclude runnable code to pass. Baselines are measured with each crate selected alone. Some standalone floors remain below 90%; the full
workspace also exercises libraries through their consumers' integration suites and can cover more code.
`--require-target` always enforces 90% for every selected crate, independently of those baseline floors.
The [Coverage CI workflow](docs/ci.md) requires the 90% target on pull requests and `main`, using hosted CPU runners
and software graphics; only the hardware DMA test is excluded there. Running the script without
`-p` selects all 21 crates with runtime code, including the lab and test-support tooling. Generated WPE bindings,
the standalone video spike and `fence-seal` (a compile-test harness with a documentation-only library) are excluded.
`fence-seal`'s compile checks still run through the normal test gate. Third-party code and tests outside `src/` do
not enter the totals.

Baseline floors are measured and validated with one `-p <crate>` alone. Combined runs also count hits from dependent
crates' tests, so use them to find gaps; their higher totals cannot establish or validate an individual crate's floor.

On Linux the wrapper uses LLVM continuous profiling with counter relocation and `%p-%m%c.profraw`, verified on the
pinned Rust toolchain. Counters stay in a mapped profile when a sandbox worker is killed, so its executed code still
counts. Existing `RUSTFLAGS` or the higher-precedence `CARGO_ENCODED_RUSTFLAGS` are preserved when adding relocation.
See [LLVM's continuous profiling documentation](https://clang.llvm.org/docs/SourceBasedCodeCoverage.html#running-the-instrumented-program).
`--profile-mode ordinary` selects exit-time profiling for diagnostics; killed workers can lose their counters in that
mode. Other platforms default to ordinary mode; continuous mode is verified and accepted only on Linux.

Continuous reports go to `target/coverage/continuous/<selection>/llvm.json` and `summary.json`, with instrumented builds
in `target/coverage-continuous-build`. Ordinary mode keeps separate reports in `target/coverage/<selection>/` and builds
in `target/coverage-build`. One crate uses its name, the full workspace uses `all`, and long partial selections use a
short hash. The summary records every selected package and ranks source files by uncovered lines, functions and regions.
Measurements serialize on one lock across both modes. Before each run, the wrapper clears workspace artifacts from
the instrumentation build so LLVM cannot count obsolete code embedded in an unselected consumer; third-party builds
remain cached. `--webrtc-cache DIR` passes an existing SDK with `include/` and
`lib/libwebrtc.a` to the upstream build as `LK_CUSTOM_WEBRTC`; relative paths resolve from the caller's directory.
Choose the SDK version and platform matching the locked dependency. The wrapper does not select or download an SDK
itself, and preserves an existing `LK_CUSTOM_WEBRTC` when this option is absent. `--report-only` rechecks the selected
mode's existing export without running tests; use a fresh measurement for change validation. Every executable selected
`src/**/*.rs` must appear in the LLVM export, including
zero-hit files: missing files fail even with `--no-gate`, so omitted feature-gated code cannot silently inflate
coverage. `coverage.toml` lists individually reviewed declaration/type-only files and separate unit-test modules that
LLVM does not map because `cargo-llvm-cov` filters test filenames such as `tests.rs`. Each entry records its SHA-256
and reason; test modules also pin the file that includes them under `cfg(test)`. A macro invocation whose executable
bodies are mapped at its definition pins that definition's file, which remains required in the report. A content
change invalidates the review and fails the report until inspected again. The summary lists these audited unmapped
files explicitly. If LLVM supplies a mapping for one, its counts still contribute to the totals; the review never
discards reported executable code.

Stable Rust supplies line, function and region coverage here; regions are not statements or branches. Inline unit
test functions in `src/` also appear in LLVM's source totals. This first measurement uses default features and the
features enabled by development dependencies, not a merged feature matrix. Code behind `personal`, `test-support`
or other optional features still needs appropriate mock-backed variant measurements before claiming full coverage.
The wrapper never enables `live` and removes live-test and external TURN-server environment variables. Browser and
GUI tests need the local dependencies below; the wrapper sets `PROTO_LAB_REQUIRE_CHROME=1 GPUI_SMOKE_REQUIRE=1`
so missing dependencies fail rather than skip tests. Never run tenant activity for coverage.

Coverage builds also make the profiling output directory writable inside WebKit's subprocess sandbox; otherwise the
extension cannot persist its profiles in the host's output directory. Ordinary builds do not add this
directory. The extension's standalone suite shares the sign-in window's offline sandbox scenarios to exercise its real
WebKit callbacks, alongside socket, protocol and GLib boundary tests.

The automatic `teams-auth` private-keyring test requires `/usr/bin/dbus-run-session`, `/usr/bin/gnome-keyring-daemon`
and `/usr/bin/kill`. It starts a private bus and keyring service with temporary HOME/XDG directories, without using
the desktop keyring. The separate manual user-keyring test remains ignored by default.

`cargo test --workspace` is offline: the native client is tested against `teams-mock` (two or three native clients
talking through it), the browser fence against local pages in Chrome. The fence suite alone: `cargo test -p
teams-fence -p teams-proto -p fence-seal -p teams-trouter` plus `cargo test -p proto-lab --test fence_http --test
fence_ws --test fence_targets --test calling --test expected_denies`.

| variable / flag | effect |
|---|---|
| `PROTO_LAB_CHROME=<binary>` | the Chrome the browser tests use; by default the pinned install in the lab's `chrome/` dir (`proto-lab chrome install`) |
| `PROTO_LAB_REQUIRE_CHROME=1` | without a Chrome the browser tests fail instead of skipping |
| `GPUI_SMOKE_REQUIRE=1` | without weston ≥ 15.0 (for `--fake-seat`), niri, `dbus-daemon`, `pipewire` and `wireplumber` on `PATH`, and the PipeWire plugins and modules the sandbox's PipeWire links (`spa-0.2`, `pipewire-0.3`, `wireplumber-0.5`, WirePlumber's data dir), the UI sandbox's and the GPUI smoke tests fail instead of skipping |
| `TEAMS_WPE_TEST_SKIP_DMA=1` | CPU-only CI excludes the native WPE hardware DMA subcase; worker and in-process SHM tests still run. Unset locally to require all three modes. This does not exclude any runtime source from coverage. |
| `WGPU_BACKEND=vulkan` | select GPUI's Vulkan backend; CPU CI uses Mesa Lavapipe. The sandbox forwards this graphics choice while keeping the session's display, bus and credentials out. |
| `--features live` + `PROTO_LAB_LIVE=1` | the live lab tests: `crates/proto-lab/tests/auth_live.rs` (a, b and c sign in natively, refresh, and get a skypetoken, tokens kept in memory), `crates/teams-headless/tests/chat_live.rs` (M1's acceptance: it only spawns the normally built `teams-headless` and reads its output and `fence.log`), `crates/teams-app/tests/ui_live.rs` (M2's acceptance: the normally built `teams-app` for a, b and c, each in a UI sandbox of its own with its broker outside, driven by the keyboard and read through its view trace and `fence.log`) and `crates/teams-app/tests/call_live.rs` (M3's acceptance of calls, as `ui_live` with each sandbox's private PipeWire, and b's web client from a spawned `proto-lab run ab-call-app`); without the feature they aren't built, without the variable they skip |
| `TEAMS_HEADLESS_BIN=<binary>` | the normally built `teams-headless` that `chat_live` spawns (required: a test build refuses to run) |
| `TEAMS_APP_BIN=<binary>` | the normally built `teams-app` that `ui_live` and `call_live` spawn, and whose `secrets` subcommand they start as each app's broker (required, as above) |
| `PROTO_LAB_BIN=<binary>` | the normally built `proto-lab` that `call_live` spawns for b's web client (`run ab-call-app`) |
| `CALL_LIVE_PHASES`, `CALL_LIVE_APP_LOG` | `call_live`'s cases to run (comma-separated, all by default) and its apps' diagnostic log level |
| `TEAMS_HEADLESS_LOG=debug` | `teams-headless` logs our own crates' debug lines on stderr, never a payload or a token |
| `TEAMS_APP_LOG=<level>` | `teams-app`'s diagnostic log at `error`, `warn`, `info` or `debug` for this run, over the settings' `log_level` (`debug`: full requests, responses and frames, credentials redacted; never share such a log) |
| `TEAMS_MEDIA_TURN_SERVER`, `TEAMS_MEDIA_TURN_USER`, `TEAMS_MEDIA_TURN_PASSWORD` | also run `crates/teams-media/tests/relay.rs`'s allocation and relay through a real TURN server (coturn on loopback) |
| `GPUI_BENCH_GPU`, `GPUI_BENCH_SECONDS`, `GPUI_BENCH_UPLOAD`, `GPUI_BENCH_OUT` | the video grid benchmark (below) |

The machine's own gate run: `PROTO_LAB_REQUIRE_CHROME=1 GPUI_SMOKE_REQUIRE=1 cargo test --workspace`. The live tests
run one at a time under the live lock: `flock ~/.local/share/teams-lab/live.lock env PROTO_LAB_LIVE=1 cargo test -p
proto-lab --features live --test auth_live`, `chat_live` as in [`teams-headless`](#teams-headless), `ui_live` and
`call_live` as in [`teams-app`](#teams-app). No write ever comes from a `cargo test` process: a live test that needs
one spawns a normally built lab binary, which writes under its own fence.

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

The standalone WPE native engine test runs worker and in-process SHM modes and a DMA worker inside `ui-sandbox`,
with disposable HOME/XDG directories and synthetic URI-scheme pages. Its DMA mode requires a readable
`/dev/dri/renderD128` with a working EGL/DRM driver; it asserts actual DMA frames before testing SHM fallback.
It exercises no external page or network service.

### Known flakes under load

When the machine is loaded, `browser::a_browser_that_exits_early_is_reported` (`crates/proto-lab/tests/browser.rs`)
can fail on timing, and Chrome can be slow enough to start that a test times out at `Browser::launch`. Rerun such a test
alone before calling it a failure. A fence breach is never one of these.
