# ms-teams

A native Microsoft Teams client for Linux in Rust + GPUI, for work/school accounts. Neither official client has native
UI code to port, so this one is written against Teams' internal APIs, with the web client as the reference.

Status: M0, the protocol lab and risk spikes (`docs/plans/20261019-native-teams-m0-lab-and-spikes.md`). Hard rules for
contributors and agents are in [CLAUDE.md](CLAUDE.md).

## Layout

| path | contents |
|---|---|
| `crates/teams-fence` | the write fence: a write is allowed only within the 3 lab accounts; the lab config (`lab.toml`) |
| `crates/fence-seal` | compile checks without `test-support`: no other crate can mint a `WritePermit`, and a lab binary has its membership sources (`FailClosed`, `LiveMembershipSource`) |
| `crates/teams-auth` | PKCE code exchange, token refresh and storage, skypetoken |
| `crates/teams-proto` | Teams internal API requests; mutating ones need a `WritePermit` |
| `crates/proto-lab` | the lab harness CLI |
| `crates/fixture-guard` | scans every `tests/fixtures/` and `docs/` for tokens, signatures, emails, real MRIs and GUIDs |
| `spikes/gpui-video` | GPUI spike: the Wayland smoke window (M0 Task 15), then video rendering |
| `spikes/echo-call` | echo call spike (M0 Task 19): TURN client, `str0m` through the relay, the test call's signaling from the Task 18 capture, the call through the fence (the start sends nothing yet) |
| `tests/fixtures/` | synthetic fixtures (made-up values, a `_synthetic` marker), as in every `tests/fixtures/` of a crate or spike |
| `docs/protocol/`, `docs/spikes/` | protocol notes and spike write-ups |
| `tools/` | interim Node tools (lab sign-in, Conditional Access probe) |

## Test

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/lint-report.sh
cargo test --workspace
```

The browser tests run headless Chrome for Testing on throwaway profiles. Install the pinned build once with
`cargo run -p proto-lab -- chrome install`, or point `PROTO_LAB_CHROME` at a Chrome binary; without one they're skipped,
unless `PROTO_LAB_REQUIRE_CHROME=1`. The build goes into the lab's `chrome/` dir: under `data_dir` when
`~/.config/teams-lab/lab.toml` sets one, else `~/.local/share/teams-lab/chrome/`. No lab config is needed for this, but
an invalid one is refused; the tests look in the same place.

The GPUI smoke tests (`spikes/gpui-video/tests/smoke.rs`) start their own headless weston with a niri nested in it and
never touch the desktop session. They need weston 15.0 or later (for `--fake-seat`) and niri on `PATH`; without them
they're skipped, unless `GPUI_SMOKE_REQUIRE=1`. They run the window once on every GPU, chosen with `ZED_DEVICE_ID`.
The `gpui-video` binary itself refuses to open a window unless `WAYLAND_DISPLAY` is an absolute socket path inside the
dir `GPUI_VIDEO_SANDBOX` names, which only the sandbox sets.
The video grid benchmark (`spikes/gpui-video/tests/bench.rs`, M0 Task 16) runs in the same sandbox; its measurement is
ignored by default and run by hand with a release build:
`cargo test --release -p gpui-video --test bench -- --ignored --nocapture` (`GPUI_BENCH_GPU=<pci device id>`,
`GPUI_BENCH_SECONDS`, `GPUI_BENCH_OUT=<file>`). Results and the decision are in `docs/spikes/gpui-video.md`.

## Captures and fixtures

A recorded run writes `captures/<scenario>/<ts>/events.jsonl` under the lab's data dir, never into the repo (the format
is described in `crates/proto-lab/src/capture.rs`). Captures never enter the repo, not even sanitized.

`cargo run -p proto-lab -- export <capture> <name>` sanitizes one into a local **reference copy**,
`reference/<name>.jsonl` under the lab's data dir (`~/.local/share/teams-lab/reference/` by default; one JSON array for
a `.json` name; the file `0600`, its directories `0700`). It always refuses any destination inside a git work tree
(checked with `..` and symlinks resolved), a destination that is a symlink, and a `..` after a directory not yet
created. The copy never enters the repo, so what export's checks find in it is printed as warnings (kinds, placeholders,
event and line numbers, never a value) and the whole copy is written: a word (3 characters or more) of a name found in
the capture left in the result at any layer of encoding (an ordinary word that is also a name's word counts too; a
name never seen under a name field, or a shorter word, is outside that check), a line too deeply encoded to check, and
anything the fixture guard finds. A word of a name is never replaced inside a host, so the copy keeps every host
intact: a public Microsoft protocol host such as `teams.microsoft.com` is never taken for a name, and a word of a name
that is also a label of any other host is left there and counted in the warnings (it is still replaced everywhere
else). Only a real host counts: a URL's authority (as written or encoded), a `Host`-style header's value, a cookie's
`Domain=`; a dotted name (`Mei.Li`, `jun.hao.li`), a file name (`quill.notes.md`) or prose gets the name replaced,
and that is counted in the warnings too, since such a run may be a host nothing around it gave away (`quill.com` or
`cdn.quill.example` in prose). `--strict` turns every one of these warnings back into a refusal that writes
nothing. Teams meeting codes and join passcodes (of letters only too) become `<meeting:N>` and `<token:N>` wherever
they show, a host included, invitations read in every language the web client has locale strings for, and the fixture
guard flags a join link, a meeting id or a passcode left in the clear. The sanitizer is defence in depth, with known
gaps (`docs/backlog/sanitizer-defence-in-depth-known-gaps.md`).

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

## Test-only builds

The tests enable `teams-fence`'s `test-support` feature through dev-dependencies, and cargo unifies it into everything
built together with them (`cargo test`, `--all-targets`, `--all-features`), the `proto-lab` binary included. Such a
binary refuses to run; `cargo build -p proto-lab` or `cargo run -p proto-lab` builds one without it.

## Lab accounts

```sh
cargo run -p proto-lab -- login a     # sign lab account a in (b, c alike) and record its MRI in lab.toml
cargo run -p proto-lab -- status      # ok / login needed, per account
cargo run -p proto-lab -- auth a      # native sign-in: Entra tokens and the skypetoken, kept in the Secret Service
```

`auth` opens the authorize URL in the account's profile under the armed fence, which catches Entra's redirect and
hands the code to Rust; nothing is typed. It prints only each token's `aud`, `exp`, scopes and tenant, plus the
skypetoken's expiry and region (`docs/spikes/auth.md`). `--audiences` also asks for a token for each other Teams
audience. The live test (`PROTO_LAB_LIVE=1 cargo test -p proto-lab --features live --test auth_live`) does the same for
a, b and c with the tokens kept in memory; without `PROTO_LAB_LIVE=1` it skips.

Both run the pinned Chrome headless on the account's profile under `~/.local/share/teams-lab/profiles/`, with the fence
armed before anything loads. Until writes are mapped the fence is fail-closed, so the web client loads with its writes
denied; the denies are logged to `fence.log` and expected. `login` signs in on Microsoft's page by itself when the
session has expired, with the password from lab-automation's `lab-accounts.json` (mode `0600`). Any multi-factor
prompt (a one-time code, a method to pick, an approval in an app, registering a method) stops it: multi-factor
requests aren't on the fence's reviewed list. Each step goes to `status-<alias>.json` in the lab's data dir, and a stop
leaves `failed` there with the reason.

## Scenarios

```sh
cargo run -p proto-lab -- run load --headless      # account a loads the web client, fail-closed; recorded
cargo run -p proto-lab -- run ab-chat --headless   # a messages b in their 1:1 chat, after a live enforcement test
cargo run -p proto-lab -- run echo-call --headless # a places the test call to the echo service, fake microphone; recorded
```

`echo-call` uses the calling classifiers (`Classifier::calling`, `FrameClassifier::calling`): the write mapping, the
reviewed own-state the web client's calling stack needs for the account's own endpoint (its registration, settings and
policy reads, the Trouter session), and one more write, the test call's start in exactly its captured shape
(`crates/proto-lab/src/calling.rs`, `docs/protocol/calling-echo.md`). The fence lets the call through only when its
participants are a and the echo service,
whose MRI `lab.toml` records once established: `cargo run -p proto-lab -- service-mri echo <mri>` (only a bot's
`28:<guid>`, only for a service `lab.toml` approves by name and records no MRI for yet).

`run` exits non-zero when the scenario's goal isn't reached, the fence was breached, or the fence denied anything the
scenario doesn't expect (`load` expects every deny; `ab-chat` only its forced probe and telemetry). Runs are recorded
under `captures/<scenario>/<ts>/`. `ab-chat` maps the chat service's writes and reads each target's members live
(`docs/protocol/fence-targets.md`); everything else stays fail-closed. Without `--headless` the browsers are visible.
