# ngteams

A native Microsoft Teams client for Linux in Rust + GPUI, for work/school accounts. It uses Teams' internal APIs,
with the web client as the protocol reference. `ngteams` is the only client binary: installed for the user's own
accounts, or run with isolated lab accounts through `scripts/run-lab.sh`. `proto-lab` remains the browser capture,
scenario and bundle-analysis tool.

The app has chats, rich-text compose, reactions, search, teams and channels, notifications and tray support,
audio/video calls, and the calendar and meeting flows. Earlier milestone outcomes and measurements remain in
[the acceptance records](docs/m3-acceptance.md) and [completed plans](docs/plans/completed/); they describe the code
and live runs at those milestones, rather than today's commands. Shared rules for Claude Code and Codex are in
[AGENTS.md](AGENTS.md).

The client also stages dropped files as private attachments, retains compose drafts while sends are pending,
opens people search results as chats, and keeps unread badges in sync with read receipts. Editing message text
keeps its existing file references and cards. Channel menus provide management, notification and section
preferences, email and workflow actions. Calls and meetings support Teams
live captions and spoken-language controls (Alt+Shift+C); caption text stays in memory and clears on hide or call
end, unless Settings → Calls → "Save live captions of every call" is on: then every call's captions are turned on
(their pane stays as the user has it) and saved in the store as the call's transcript, with nothing sent to Teams
or shown to the others. A saved transcript is linked from its call's end row in the chat (a row of the app's own for a 1:1
call, which Teams posts none for; "Saved on this computer")
and opens in the chat's Recap tab with search, .vtt download, the AI summary and Delete transcript; Teams' own
transcript of the same call wins. Captions can be translated on this computer ("Translate to" in the caption settings) by Claude Code or the Codex
CLI, with any model the tool lists or one typed (by default Claude Haiku or GPT-6-Luna; Settings → Calls → Captions);
the caption text then goes to that tool and its provider. A chat or channel can be read and written in another
language (the header's translation button): its messages are shown translated by the same tools (Settings → General
→ Chat translation), the account's own are sent in the chat's language and shown as written, and the translations
are kept in the store, so a chat opened again is not translated twice. Its last message in the chat list, its new
messages' desktop notifications, its messages found by the search and a reply's quote read in that language too (a
notification waits at most a few seconds for its translation); the language menu filters as you type. See [channel management](docs/protocol/channels.md#managing-an-existing-channel),
[workflows](docs/protocol/workflows.md), and [live captions](docs/protocol/live-captions.md).

## Layout

| path | contents |
|---|---|
| `crates/ngteams` | sole client, account files and startup, selected credential store, lab/read-only modes, build markers and sign-in integration |
| `crates/teams-auth` | PKCE, code exchange/refresh, identity checks, authz/skypetokens, sessions and scoped credential brokers |
| `crates/teams-proto` | IDs and service constants, typed API requests and delivery responses, HTTP transport, chat/rich-text models and shared URL/path log shaping |
| `crates/teams-store` | per-account SQLite/WAL stores, migrations, durable outbox and engine locks |
| `crates/teams-trouter` | messaging and calling push sockets, registration, reconnect and typed connection-bound call replies |
| `crates/teams-call` | call state machine and tracking: offers, redelivery/ended rings, acceptance and media links, one call per account |
| `crates/teams-core` | account engines, sync, outbox and retries, calendar/team operations and the cross-account call floor |
| `crates/teams-media` | libwebrtc, Teams' SDP dialect, relay-only ICE, audio processing, video effects, file/PipeWire devices and GStreamer playback |
| `crates/teams-ui` | GPUI views/models over `Core`, rich-text editor/rendering, accounts, calendar, notifications and call windows |
| `crates/teams-log` | private rotated diagnostic files, full debug HTTP/frame records and logger-owned credential redaction |
| `crates/teams-signin`, `crates/teams-signin-extension` | in-process host-gated WPE sign-in window and its web-process request guard |
| `crates/teams-wpe`, `crates/wpe-*-sys` | system WPE WebKit integration and generated bindings |
| `crates/teams-mock` | synthetic HTTP/Trouter/calling/media services for tests, dev-dependencies only |
| `crates/proto-lab` | lab config, persistent browser profiles, native token provisioning, captures/holds, scenarios, probes, export and bundle search |
| `crates/ui-sandbox` | private headless Wayland compositor, bus, input, URI opener, optional PipeWire/camera portal and `ui-sandbox-run` controls |
| `crates/fixture-guard` | synthetic-fixture markers and credential/private-data scans |
| `scripts/run-lab.sh`, `scripts/run-personal-read-only.sh` | isolated sending lab sessions and snapshot-based read-only personal sessions |
| `scripts/install-personal.sh`, `packaging/` | user-requested personal release install, desktop entry and icons |
| `docs/protocol/` | protocol observations in our own words and the [endpoint inventory](docs/protocol/endpoints.md) |
| `docs/personal-build.md`, `docs/lab-rules.md` | personal account authorization and the agent's lab run check |
| `spikes/gpui-video`, `tools/`, `vendor/` | standalone experiments, supporting asset tools and patched dependencies |

The native transport uses HTTPS only, no proxy or redirects, and sends each token only to its own service.
Writes are typed requests through the outbox; unknown outcomes are checked and rows are never resent across runs
without `Store::resend`. Trouter replies are typed and bound to the delivery's connection. Call tracking and the
cross-account floor are app behavior, independent of lab configuration.

### WPE WebKit

The sign-in window renders Microsoft's page with the system's WPE WebKit, 2.54 or newer: the distribution's
`wpewebkit` (`sudo pacman -S wpewebkit`), which every build of the WPE crates needs. The build finds it through the
system's pkg-config (`wpe-webkit-2.0`, `wpe-web-process-extension-2.0`), the binaries load `libWPEWebKit-2.0.so` from
the loader's own path, and the engine starts the package's helper processes (`WPEWebProcess`, `WPENetworkProcess`,
`WPEGPUProcess`) from `/usr/lib/wpe-webkit-2.0/`, where their path is compiled into the library; GStreamer loads its
plugins at run time. None of it can be linked in statically, so the installed `ngteams` needs the package at run time
too, and `scripts/install-personal.sh` refuses to build without it.

The worker is the asking program's own executable run again: every `main` that opens a sign-in window
(`ngteams` and the window tests) calls `teams_wpe::worker::serve_if_asked` early.

## Third-party UI code

The views are built on [`gpui-component`](https://crates.io/crates/gpui-component) 0.8 (Longbridge's GPUI Kit,
Apache-2.0), the UI kit for `gpui-pre` 0.3.9; it resolves to the workspace's vendored GPUI patch. Its audit (M2 Task 7,
`docs/plans/completed/20261021-native-teams-m2-gpui-daily-driver.md`) found, and `crates/teams-ui/tests/reach.rs` keeps
checking:

- It takes `gpui-pre` with its default features (`x11`, `font-kit`, `windows-manifest`), so the workspace's GPUI has
  them on. In `gpui-pre` 0.3.9 `x11` only lets GPUI name an X11 display it could use and `font-kit` switches nothing;
  the X11 client is `gpui-pre-linux`'s, which stays Wayland-only (`crates/teams-ui/tests/reach.rs` checks it), and
  `ngteams` lab and read-only modes refuse a set `DISPLAY` all the same. `windows-manifest` acts on Windows builds only.
- Its tree for the host holds no HTTP or WebSocket client (`gpui-pre-reqwest` is for wasm only) and only licences an
  Apache-2.0/MIT project can take: MIT, Apache-2.0, BSD, ISC, Zlib, Unicode-3.0, 0BSD, CC0, and MPL-2.0 for
  `option-ext`, which a build-time macro of the kit's assets uses (`reach.rs` checks both).
- It starts no thread or process of its own and touches no file in our use: its theme directory watcher
  (`ThemeRegistry::watch_dir`, `notify`) is never called. It reads the reduced-motion setting from the settings portal
  on the bus the app is given (the sandbox's private bus, which has none). The app installs its diagnostic logger
  before startup; third-party payload logging is excluded by the compile-time log cap and the logger's target filter (`reach.rs` checks the app and dependency boundary).
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

Three dedicated accounts, a, b and c, live in the company tenant. Before **each live run**, the agent lists every
chat, call and person the run can reach and verifies that all are lab accounts, the approved echo service or the
user's own account. This includes automatic UI actions and scenario follow-ups. The agent applies
[the lab rule](docs/lab-rules.md); the application does not read `lab.toml` or enforce a lab roster.

The browser/capture harness keeps its own configuration, outside the repository:

```toml
# ~/.config/teams-lab/lab.toml (0600); passwords never belong here
# data_dir = "/absolute/lab-data"  # optional; default $XDG_DATA_HOME/teams-lab
[[accounts]]
alias = "a"                      # also b and c, each exactly once
upn = "bot-a@lab.example.com"     # all three in one tenant domain
# mri = "8:orgid:<object-id>"     # recorded after login

# Repeat [[accounts]] for b and c.
[[service_participants]]
name = "echo"
# mri = "28:<captured-bot-id>"

[[contacts]]
name = "me"                     # only the user's own account
mri = "8:orgid:<object-id>"
# upn = "me@example.com"         # optional, for calendar attendee flows
```

| location | contents |
|---|---|
| `$XDG_CONFIG_HOME/teams-lab/lab.toml` | harness accounts, MRIs, service participants, contacts and optional browser data root |
| `<browser data>/chrome/`, `profiles/{a,b,c}/` | checksum-verified pinned Chrome and persistent signed-in profiles |
| `<browser data>/captures/<scenario>/<timestamp>/events.jsonl` | raw recordings; never committed |
| `<browser data>/reference/`, `bundles/`, `chats.toml` | local reference copies, unpacked/indexed bundles and recorded lab chat IDs |
| `<browser data>/auth/<alias>.json` | region metadata for native HTTP probes, never tokens |
| `$XDG_CONFIG_HOME/teams-lab/accounts.json` | password input used by `proto-lab login`, mode `0600`, never committed |
| `$XDG_DATA_HOME/ngteams-lab/<alias>/accounts.toml` | the alias's one provisioned native account, including identity/name/optional `idp_hosts` |
| `$XDG_DATA_HOME/ngteams-lab/<alias>/regions/<oid>.json` | native region metadata written by `proto-lab auth` |
| `$XDG_DATA_HOME/ngteams-lab/<alias>/store/<oid>.sqlite` | native account store and adjacent engine-lock/picture-cache files |
| `$XDG_DATA_HOME/ngteams-lab/<alias>/settings.toml`, `logs/` | app settings and diagnostic logs |
| `$XDG_STATE_HOME/ngteams-lab/<alias>/` | runner holder/broker/app logs, trace and screenshots; retained after stop |
| Secret Service application `ngteams-lab` | native refresh tokens/skypetokens, scoped by account and audience |
| `$HOME/.local/share/teams-lab/live.lock` | fixed shared runner lifetime lock, excluding exclusively locked browser/capture runs |

Absolute XDG homes override their defaults (`~/.config`, `~/.local/share`, `~/.local/state`). Native config and data
for each alias share its `ngteams-lab/<alias>/` directory. Browser `data_dir` does not relocate native files. Account
and store files are private (`0600` in `0700` directories). `accounts.json` uses the shape
`{"accounts": [{"profile": "a", "email": "bot-a@lab.example.com", "password": "<password>"}]}` (repeat for b/c);
real values stay outside the repository.

After the per-run check, provision native lab tokens from a signed-in browser profile with `proto-lab auth a`.
It verifies identity, writes tokens under `ngteams-lab`, and creates the account/region layout above. A provisioning
file failure can occur after tokens are stored; its error is reported. `--no-skypetoken` omits authz and therefore
its region. Passwords are handled by `proto-lab login`, never typed through the runner's control socket.

## Building

Agents build runnable `ngteams` only through the lab runner:

```sh
scripts/run-lab.sh --build-only  # normal debug build in target/lab; opens no account, broker or window
cargo build -p proto-lab --target-dir target/lab  # browser/protocol tool, not the client
```

The runner sets `TEAMS_LAB_BUILD=1` and removes `TEAMS_PERSONAL_INSTALL` from that build environment. A lab-marked
binary runs only in explicit lab mode; an installed build may also select lab mode. The install marker remains
required for other app/broker modes. Binaries built with `test-support` refuse to run. Cargo unifies dev features
for tests/`--all-targets`/`--all-features`, so use the normal runner build for live work, not a test executable.
The workspace's default members exclude `ngteams`; never use `cargo build -p ngteams` as an agent shortcut.

The personal release is built and installed with `scripts/install-personal.sh` **only when the user requests it**.
It installs under `~/.local`, starts nothing and requires the user's own first sign-in. See
[the personal-build guide](docs/personal-build.md).

`teams-media`'s calls run on libwebrtc, through LiveKit's `libwebrtc` bindings: a clean build downloads LiveKit's
prebuilt static libwebrtc (about 150 MB, `webrtc-linux-x64-release`) from GitHub, or takes the one `LK_CUSTOM_WEBRTC`
names. A call's H.264 is encoded in software (OpenH264). NVIDIA's NVENC and NVDEC stay out:
the bindings build them only when `CUDA_HOME` (or `/usr/local/cuda`) holds CUDA's headers, so leave it unset for this
build. NVIDIA's decoder library hangs the process at exit while a decoder is still alive.
Its PipeWire binding needs libpipewire's headers and `clang` for `bindgen`.
PipeWire reach is selected at run time: desktop for the user's normal app, sandbox for lab/test runs.
Read-only mode opens no media devices.

A video a message carries is played in its card by the system's GStreamer (`teams-media`'s `player`: `playbin` on an
`appsrc` the engine feeds, its frames and sound to appsinks, the sound on the device seam's speaker). The build needs
GStreamer's development files (`gstreamer`, `gst-plugins-base`, through `pkg-config`); playing needs the plugins for
the video's format at run time: `gst-plugins-base` and `gst-plugins-good` (MP4, playbin's converters) and
`gst-libav` (H.264, AAC and most other codecs). Only software video decoders are used (making a hardware one probes
the GPU's drivers, which can take seconds). A video whose picture no installed plugin decodes says so in its card and
offers the browser; one whose sound none decodes plays silently.

## `proto-lab`

The protocol tool uses recorded browser sessions and explicit capture holds. A hold pauses and fails only its
matching request/frame so a scenario can record the shape without sending it. Current and legacy queued held
messages stay held on later sessions. There is no browser write policy or membership gate; the agent's per-run
lab check is required before navigation or interaction.

`login`, `status` and `auth` run headless; agents also give `run` its `--headless` flag. Use only one browser per
profile and hold the fixed live lock **exclusively** around browser/capture work, with an outer timeout. A browser
run and native aliases must not share that lock concurrently. `chrome`, `bundles` and `export` can work without a
lab config; an invalid config is reported.

```sh
proto-lab chrome install
proto-lab chrome path
proto-lab login a [--timeout 300]
proto-lab status [a b c] [--timeout 120]
proto-lab auth a [--audiences] [--no-skypetoken] [--timeout 120]
proto-lab probe chat a [--with b] [--save <name>]
proto-lab probe thread a [--save <name>]
proto-lab probe profile a
proto-lab probe teams a
proto-lab probe object a <id> [--save <name>]
proto-lab run <scenario> --headless [--timeout 180] [--call <step>]
proto-lab export <capture> <name> [--strict]
proto-lab bundles import <msix>
proto-lab bundles search <regex> [--kind url|host|route|method] [--version <v>]
proto-lab service-mri echo <mri>
```

`login` uses the password input outside the repository, records its steps in `status-<alias>.json`, and records the
account's MRI. `auth` uses the profile's existing sign-in, catches Entra's native redirect, then exchanges and
refreshes tokens in Rust; it never types a password. A profile needing interaction is reported for `login`.
Probes make plain HTTP GETs with the account's tokens and print outlines/claims, never response bodies or tokens.
`run` exits non-zero when its goal is not reached or capture setup fails. Captures are raw private lab data.

| scenarios | purpose |
|---|---|
| `load`, `search` | web load and title-bar suggestions/results |
| `ab-chat`, `ab-chat-full`, `abc-chat`, `chat-shapes`, `abc-everyone` | messages, edits, reactions, quotes, typing, group creation and mentions; explicit holds where shapes are captured |
| `ab-image`, `ab-file` | synthetic uploads held for shape capture |
| `ab-trouter`, `ab-trouter-quiet` | push capture; quiet variant holds `user.activity` frames |
| `echo-call`, `ab-call`, `abc-call` | browser echo, 1:1 and group call captures; `--call` chooses the step (`--call steps`: once the call is up, the caller's page is driven over `PROTO_LAB_STEPS_SOCKET` with `team-manage`'s steps, a callee's by a step that starts `@b ` or `@c `) |
| `ab-meeting`, `ab-meeting-join`, `meeting-steps` | scheduling, pre-join and step-controlled meeting captures |
| `channel-create`, `team-manage` | held channel/team writes; team sending requires explicit scenario configuration. `team-manage` is also how an agent looks at a feature in browser Teams before building it: steps over `PROTO_LAB_STEPS_SOCKET` (`go`, `click`, `point`, `xy`, `wheel`, `type`, `insert`, `press`, `js`, `shot`, `seen`, `hold`, `quit`) |
| `own-presence-activity`, `own-presence-endpoint`, `own-presence-both`, `own-presence-manual-held`, `own-presence-manual` | compare activity/presence paths, with the requested capture holds |

Protocol details and scenario controls are in [the protocol notes](docs/protocol/).

## `ngteams`

### Lab runs (`scripts/run-lab.sh`)

After the per-run lab check and native provisioning, start each alias through the runner. These are live tenant
sessions, so run only the accounts and interactions already checked:

```sh
scripts/run-lab.sh a --pipewire --timeout 1800
scripts/run-lab.sh b --pipewire --timeout 1800
scripts/run-lab.sh a --shot chats
scripts/run-lab.sh a --key ctrl+k
scripts/run-lab.sh a --type '<lab chat name>'
scripts/run-lab.sh a --click 300 200
scripts/run-lab.sh a --windows
scripts/run-lab.sh a --status
scripts/run-lab.sh a --stop
scripts/run-lab.sh b --stop
```

The broker runs outside each sandbox and serves only its one configured account in `ngteams-lab` with `get`, `put`
and `delete`. The app runs inside the private compositor/bus; `--pipewire` adds private audio/video devices.
An alias selects one account directly, allows normal sending and sign-out/forget, and offers no add-account prompt.
Lab windows require the sandbox compositor with `DISPLAY` unset; lab paths overlapping personal data/config are refused.

Each alias has an exclusive lock in `<state>/ngteams-lab/.locks/`; live holders share the fixed legacy live lock so
several aliases can coexist. The build happens before acquiring the shared live lock. The detached holder keeps
both lifetime locks through cleanup. The deadline defaults to 1800 seconds (`--timeout` overrides it) and requests
ordered TERM shutdown. A stuck holder keeps its locks and diagnostic state until cleanup finishes. Stop retains
account files, stores and logs. `--shot` captures a window, `--screen <name>` the whole output; `--key`, `--type`,
`--click`, `--windows` and `--status` address only that alias. Never type credentials through `--type`.

### Personal accounts

The user installs and runs their own app:

```sh
scripts/install-personal.sh             # only on the user's explicit request
ngteams                                 # ask for the work account, then sign in
ngteams --account you@example.com        # sign in this account, adding it if new
ngteams --forget you@example.com         # remove credentials/config/region; quit its engine first; store stays
```

Several accounts run in one window, switched by the rail's avatars. The personal app uses
`~/.config/ngteams/accounts.toml`, data under `~/.local/share/ngteams/` and Secret Service namespace `ngteams`.
It does not read `lab.toml`. Its normal calls use desktop PipeWire and the camera portal. Agents may read personal
data/config directories, but may run this account themselves only through the read-only runner below; no personal
sign-in, sending-mode testing or access to its keyring items/tokens. See [the full rules](docs/personal-build.md).

### The read-only mode (`scripts/run-personal-read-only.sh`)

```sh
scripts/run-personal-read-only.sh you@example.com
scripts/run-personal-read-only.sh --shot chats
scripts/run-personal-read-only.sh --key ctrl+k
scripts/run-personal-read-only.sh --click 300 200
scripts/run-personal-read-only.sh --windows
scripts/run-personal-read-only.sh --status
scripts/run-personal-read-only.sh --stop       # --keep retains the run instead of deleting it
```

It runs the installed app over a consistent snapshot in `~/.local/state/ngteams-read-only/run/snapshot/`, in its own
UI sandbox. The outside broker serves `get` only; refresh rotations live in memory. No personal source file or
stored token is changed. Transports refuse writes, registrations and call replies below the views, recording
`refused_read_only` with method, host and path shape in `snapshot/data/logs/` (also see `app.log`). It opens no push
channel, calls, PipeWire or camera, catches up by reads every minute and never opens sign-in. The UI offers no
writes, add/remove account or tray item. It does not disturb the user's running instance. The script builds only
`ui-sandbox-run`; it requires an installed binary with read-only support (`TEAMS_BIN` can select its path).

### UI and diagnostics

The three-pane window provides chats, teams/channels, a rich-text compose box, search and calendar. Ctrl+K opens
the chat/command switcher, Ctrl+E searches, Ctrl+2 focuses the chat list, and Ctrl+4 opens Calendar. Message-list
keys include `r` reply, `a` react, `e` edit own message, Delete own message, Ctrl+C copy and Escape clear selection.
The compose editor supports formatting, emoji, mentions and quotes; Enter sends, Shift+Enter inserts a line break,
and expanded formatting mode uses Ctrl+Enter to send. A meeting chat's header has Teams' Chat, Shared and Recap tabs:
Recap plays the meeting's recordings with their transcripts (search, download), as a recording's "View recap" opens
it, and its More options downloads a recording the account may download
([docs/protocol/recap.md](docs/protocol/recap.md)). Its AI summary is made on this computer when asked for, laid out
as Fathom's call summaries (purpose, key takeaways, topics with their moments, next steps with owners, open
questions), by Claude Code or the Codex CLI with any model it takes (by default Claude Sonnet or GPT-6.1-Terra;
Settings → Calls → Meeting recaps, the same tool-and-model control as the captions'): the transcript then goes to that
tool and its provider. It is kept per recording, copied as Markdown, or sent to the
meeting's chat after a preview. Settings control appearance, notifications, privacy,
calls/devices and diagnostics. Native sandbox tests in `crates/ngteams/tests/{window,accounts_window,call_window,ui_tour}.rs`
exercise these views against the mock.

Calls open a separate window; closing it leaves the call. Its toolbar follows Teams': Transfer and Hold (1:1 calls;
Resume while held), Chat (the call's chat beside the stage), People, Raise and React (group calls and meetings), View (Gallery or
Speaker, Focus on content while a share shows, Show videos first; More options: Turn off other videos, Hide me, Full
screen), More, camera, microphone, Share (1:1 calls with video: "Share content", the screen or window picked at the
desktop's screen-cast portal; Stop sharing while shared) and Leave (End meeting, after a confirmation, for a meeting's
organizer). A 1:1 call the far end holds shows its avatar over "On hold"; the far end's shared screen takes the stage,
the people beside it. People's "Invite someone" adds someone to a chat's call (a 1:1 call becomes a group call in a new
chat of the three); who doesn't answer shows "No response" with Call again. Transfer (1:1 calls) holds the call and
transfers it to someone picked, Consult holds it and opens their chat; a call the far end transfers calls the target
at once. More's Record and transcribe starts a recording with its transcript after asking the spoken language (Stop
recording, after asking, stops anyone's); the recording lands in the chat as Teams saves it. More → Settings → Call
health shows the call's network, audio, video and screen sharing measures as its media reads them (nothing goes to
Teams). The main window keeps an in-call strip.
Ctrl+Shift+M mutes, Ctrl+Shift+O toggles camera, Ctrl+Shift+K raises or lowers the hand and Ctrl+Shift+H leaves.
Camera starts off; only the user's action turns it on.
Notifications, tray and device access use the session selected at startup: desktop for the user's normal app,
private sandbox for lab/tests. One call or ring is admitted at a time across the app's accounts.

The diagnostic file is `<data>/logs/teams-app.log` (the retained filename), mode `0600` in a `0700` directory,
rotated at 8 MiB with four older files. `TEAMS_APP_LOG=<error|warn|info|debug>` overrides the settings' level;
`trace` is treated as `debug`. Info stays payload-free. Debug includes every API request/response, including token
auth and authz calls, and both directions of every Trouter frame, with credentials redacted by `teams-log`.
It still holds message text, names and service answers: **never share a debug log or commit it, even sanitized**.
Copy diagnostics includes app/build/log details and recent diagnostic lines; any debug content is labelled unsafe
to paste. There is no separate write-decision log.

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

Choose checks for what the change can break; [AGENTS.md](AGENTS.md) defines the scope. Typical checks:

```sh
cargo fmt --all --check
cargo check -p <crate> --all-targets
cargo clippy -p <crate> --all-targets -- -D warnings
scripts/lint-report.sh
cargo test -p <crate> [--test <name>] [<filter>]
cargo test -p fixture-guard
python3 -B -m unittest discover -s scripts -p test_run_lab.py
```

The widest local gates are `cargo clippy --workspace --all-targets -- -D warnings` and
`PROTO_LAB_REQUIRE_CHROME=1 GPUI_SMOKE_REQUIRE=1 cargo test --workspace`. Ordinary tests use synthetic mocks and
throwaway browser profiles. No live tenant write may come from a `cargo test` process; native live tests must use
the normally built lab app through `run-lab.sh` after the agent's check.

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

`coverage.toml` holds the 90% target and measured standalone baseline floors for all 17 runtime crates. Raise floors
as tests improve; never lower them or exclude runnable code to pass. Baselines are measured with each crate
selected alone. Some standalone floors remain below 90%; the full workspace also exercises libraries through their consumers' integration suites and can cover more code.
`--require-target` always enforces 90% for every selected crate, independently of those baseline floors.
The [Coverage CI workflow](docs/ci.md) requires the 90% target on pull requests and `main`, using hosted CPU runners
and software graphics; only the hardware DMA test is excluded there. Running the script without
`-p` selects all 17 crates with runtime code, including the lab and test-support tooling. Generated WPE bindings and
the standalone video spike are excluded. Retained typed-construction compile checks run in `teams-proto` and
production-only mock-origin/registrar checks run in `teams-trouter` as part of the normal test gate.
Third-party code and tests outside `src/` do not enter the totals.

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
test functions in `src/` also appear in LLVM's source totals. Measurement uses default features and the
features enabled by development dependencies, not a merged feature matrix. Code behind `test-support`
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

`cargo test --workspace` is offline: native APIs and calls use `teams-mock`, browser captures/holds use local
pages in Chrome, and GUI/media tests use their private sandbox. No personal files, profiles, credentials or keyring
are needed. The automatic auth keyring test uses a private bus/keyring under temporary HOME/XDG; the separate
user-keyring test is ignored by default.

| variable / flag | effect |
|---|---|
| `PROTO_LAB_CHROME=<binary>` | browser for offline tests; defaults to the harness's pinned install |
| `PROTO_LAB_REQUIRE_CHROME=1` | missing Chrome fails instead of skipping |
| `GPUI_SMOKE_REQUIRE=1` | missing Weston/niri/private bus/PipeWire dependencies fail instead of skipping |
| `TEAMS_WPE_TEST_SKIP_DMA=1` | CPU CI omits only native WPE's real DMA subcase; runtime source remains counted; unset locally |
| `WGPU_BACKEND=vulkan` | CPU CI uses Mesa Lavapipe; sandbox forwards this graphics setting |
| `--features live` and `PROTO_LAB_LIVE=1` | explicitly enable proto-lab's optional auth live test; use the live lock and outer timeout |
| `TEAMS_APP_LOG=<level>` | diagnostic level for a client run; debug logs remain private |
| `TEAMS_MEDIA_TURN_SERVER`, `TEAMS_MEDIA_TURN_USER`, `TEAMS_MEDIA_TURN_PASSWORD` | optional explicit real TURN relay tests, outside the ordinary offline gate |
| `GPUI_BENCH_GPU`, `GPUI_BENCH_SECONDS`, `GPUI_BENCH_UPLOAD`, `GPUI_BENCH_OUT` | standalone video-grid benchmark settings |

Every GUI test uses its own headless Weston/niri compositor, private bus and no-op URI opener. PipeWire tests use
private synthetic microphones/speakers/cameras; host-session canaries check isolation. The native WPE tests use
synthetic pages in worker/in-process SHM and real DMA modes. DMA needs a working render node/EGL driver; no external
page or network service is used. Coverage explicitly requires browser and sandbox dependencies.

### Known flakes under load

Chrome startup and `browser::a_browser_that_exits_early_is_reported` can fail on timing under load. Rerun the failed
test alone to establish its cause; do not treat a lab-contact violation as a timing flake.
