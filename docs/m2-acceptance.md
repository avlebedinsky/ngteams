# M2 acceptance check (the lab)

Date: 2026-10-23. M2 Task 25 (`docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`), on branch
`native-teams-m2-gpui-daily-driver`. Every live command ran from a **normal build** (`cargo build -p teams-app -p
teams-headless -p proto-lab --target-dir target/lab`: debug, no `test-support`), headless, under the lab's live lock
(`flock ~/.local/share/teams-lab/live.lock`) with an outer `timeout --signal=INT`; one app (and one broker) per lab
account, one browser per lab profile in the `proto-lab` runs; nothing was left running after any of them (checked). No
call was answered. Writes went only to the lab's a–b chat, the recorded a–b–c group chat and the lab accounts' own
availability. From 10:09Z the run's processes were pinned to 4 CPUs (the user's cap, load average 17–42 from other
work on the machine).

The group chat rests on the accepted assumption `docs/m1-acceptance.md` states (CLAUDE.md's "One group chat counts as
read in full: the lab's own").

| # | criterion | verdict |
|---|---|---|
| 1 | a types and sends N = 20 to b, each shown in b's chat view, send → shown p95 under 2 s | **met** (run 5: p50 1326 ms, p95 1850 ms, max 1959 ms); runs 3 and 4 were over (2030 and 2345 ms, below) |
| 2 | an edit, a reply and a reaction shown by b | **met** (1521, 1506 and 2478 ms) |
| 3 | a message to the group chat shown by b and c | **met** (c 1567 ms; b from its notification) |
| 4 | b's notification on the sandbox's bus for a chat it isn't viewing | **met** (1725 ms, kind `message`, app name `Teams (lab b)`) |
| 5 | names and presence: b sees a online, then Busy when a sets it | **met** (b's header: a named from people's names, a's Graph picture, `available`; `busy` 28 s after a chose it, `available` again 55 s after Automatic) |
| 6 | a search in b finds a's message | **met** |
| 7 | a channel shown read-only | **met** (200 posts and replies, no compose) |
| 8 | b's app closed, a sends, b's app reopened shows every message once and notifies once | **met**, in two parts (below): the window closed into the tray notified once (1006 ms) and showed the message when opened again; the app quit and started again showed every one of the run's 25 messages in the a–b chat exactly once |
| 9 | images and files only as far as Tasks 21–23 got | **met**: none sent (sending stays deferred: `docs/backlog/ams-image-sending.md`, `file-sending.md`); a channel post with a part no capture showed is drawn as `unshown-post` |
| 10 | `fence.log`: allows only on the lab chats and the accounts' own presence, no deny of a mapped write, own-state, `PostRead` and frames only from the reviewed lists | **met** (run 5: 171 lines, every one an allow) |
| 11 | screenshots of each step under `<data_dir>/screenshots/<time>/`, never the repo | **met** (19 in run 5) |
| 12 | `proto-lab run ab-chat --headless` and `abc-chat` exit 0; `teams-headless`'s `chat_live` passes | **met** (10:31–10:40Z) |
| 13 | the gate | **met** (no flake) |

## The live test

`crates/teams-app/tests/ui_live.rs` (`--features live`, and only with `PROTO_LAB_LIVE=1`) is an orchestrator: it opens
no fence, holds no token and sends nothing itself.

- **A UI sandbox per account** (`ui-sandbox`: a nested niri on a headless weston at 20 Hz, a private bus with the fake
  notification server and tray watcher, the no-op opener), so each app's window is the focused window of its own
  compositor, as a user's would be, and each sandbox's virtual keyboard reaches only its app. The plan's line says "the
  UI sandbox"; one per app is the same sandbox three times.
- **Credentials**: for each account it starts `teams-app secrets --lab <alias>` outside the sandbox, with the session
  bus (the user's, where the lab's tokens are) and nothing of a display in its environment, and hands the other end of
  the socket pair to the app (`--secrets-fd`); both ends are closed in the orchestrator once the two processes hold
  them. Each app's environment is read from `/proc` once it runs: its bus is its sandbox's, its `WAYLAND_DISPLAY` its
  sandbox's niri, no `DISPLAY`, no `NIRI_SOCKET`. Every broker exited cleanly once its app quit (checked, the restarted
  b's included), and no output of the run (traces, the brokers' and apps' output) holds a JWT, a bearer header or a
  skypetoken.
- **The keyboard only**: the chat switcher (Ctrl+K, Down to the chat's row in the order the chat list's trace gives,
  Enter), the compose box (typing, Enter; Up in the empty box edits the last message), the message list (Escape from
  the empty box, Down to the newest, `r` reply, `a` then `1` like, Escape back), the search (Ctrl+Shift+F), the
  switcher's channels ("General") and its "Set status: Busy" / "Set status: Automatic" commands. The tray (closing the
  window through niri, the item's `Activate`, its menu's "Quit") and the notification's action go through the sandbox's
  compositor and bus, as a desktop would.
- **What it reads**: each app's view trace (ids and kinds, never text) on a pipe (`--trace-fd`), and `fence.log` from
  the byte the run began at. Outputs: traces, the apps' and brokers' output and `summary.json` under
  `<data_dir>/ui-live/<time>/`, screenshots under `<data_dir>/screenshots/<time>/`.

What the run needed in the app (its own commit, with tests): the compose box hands an Escape it has nothing to cancel
to the chat view, which moves the focus to the message list (and an Escape there with nothing open or selected moves
it back), since no key reached the list before; the switcher lists the teams' channels and the "Set status: …"
commands (the account menu had only the pointer); and the trace tells which shown messages carry reactions or quote
another (`chat-view shown`'s `reacted`, `replies`), what the chat's header shows (`header shown`: the other member's
presence by its key, whether the name came from people's names and the avatar from a picture, never the name), and
each change of the compose box's text (`compose changed`, no text). The trace held no presence before (Task 17's test
said so); it now holds the header's presence as a key, nothing more.

## 1. Latency: met (run 5)

Send is a's Enter, pressed once the app has taken in what was typed (its `compose changed` lines quiet for 1.5 s);
shown is b's `chat-view shown` first naming the message. Both are this machine's clock.

| run 5 (10:19Z, 4 CPUs, load ~18–28), ms | p50 | p95 | max | min |
|---|---|---|---|---|
| a's Enter → b's chat view shows it | 1326 | 1850 | 1959 | 954 |
| Enter → a's compose box takes the send (`compose sent`) | 82 | 149 | 240 | 57 |
| `compose sent` → b shows it | 1265 | 1651 | 1851 | 897 |

The rest of the path, from the apps' logs of run 4: the row made at once (2–20 ms after `compose sent`), the fence's
fresh member read and the post 0.5–0.7 s, the service to b's push channel 15–160 ms (one sample −1 ms: the two clocks
of the service and of us), and **b's own work from the delivery to its chat view 0.4–1.5 s** under the CPU cap, against
about 0.3 s in run 3 before it. b's view reads the chat's page back from the store and lays it out again on each
change; in a debug build on a starved CPU that is the largest part.

The runs before:

- **Run 1** (09:44Z): Enter → shown p50 2499, p95 4608, max 4625. The service path was fast (`compose sent` → shown
  0.82–1.34 s); the rest was the debug app still working through the virtual keyboard's keystrokes when Enter was
  pressed. The run then failed in the orchestrator (below).
- **Run 3** (09:51Z, with a fixed 4 s wait after typing): p50 1012, p95 2030, max 6479; Enter → `compose sent` p95 1128,
  max 5408 (the first message, the app busy with its start); `compose sent` → shown p95 1236. Hence the wait for the
  compose box's own trace instead of a fixed time.
- **Run 4** (10:09Z, the CPU cap applied during it, load ~41): p50 1632, p95 2345, max 3087; Enter → `compose sent` p95
  345; `compose sent` → shown p95 2168, b's delivery → shown up to 1.5 s. The sandboxes then ran at 20 Hz instead of 60
  (three compositor pairs repainting took CPU the apps needed; the trace records a view's change when its model
  changes, not when a frame is presented, so the rate doesn't move the measure), and run 5 passed.

The debug build's keystroke cost is recorded in `docs/backlog/debug-build-keystroke-lag.md` (worth: later): whether a
release build (the personal build is one) keeps up with fast typing wasn't measured.

`teams-headless`'s `chat_live` the same afternoon (10:35Z, N = 50, no UI): send start → receive p50 1134, p95 1365, max
1443 (the post p50 918, the delivery p50 232), as in M1.

## 2. Edit, reply, reaction; the group chat; the notification

Run 5, each from a's Enter (the edit, the reply) or a's `1` (the reaction) to b's `chat-view shown` naming it as such:
the edit (kind `edited`) 1521 ms, the reply (in `replies`) 1506 ms, the like on the reply (in `reacted`) 2478 ms.

The group chat: a opened it by the switcher and sent; c, showing it, showed the message 1567 ms after a's Enter; b,
showing the a–b chat, raised a notification on its sandbox's bus 1725 ms after (kind `message`, its body the message's
text, app name `Teams (lab b)`); the notification's `default` action opened the group chat in b, which showed the
message.

## 3. Names and presence

b's header for the a–b chat (`header shown`): a's presence `available`, the name from people's names (`named`: a's
Graph name), a's picture (`picture`: Graph's 96×96, the only lab account with one). a chose "Set status: Busy" in the
switcher; a's own presence sent the manual status (`own-presence sent busy`), and b's header showed `busy` 28 s later
(run 3: 19 s, run 4: 13 s; the engine reads presence once a minute). a chose "Set status: Automatic"; the reset went
(`own-presence sent reset`) and b's header showed `available` again 55 s later. No run ended with a account's manual
status set: runs 1 and 2 stopped before it, runs 3–5 reset it.

## 4. Search, a channel

b's search (Ctrl+Shift+F) for the run's nonce and `m07` found a's seventh message (1 hit; run 3's query found 22
because its words were shorter). The switcher's channels, queried "General", opened a team's general channel
read-only: 200 posts and replies, two of them `unshown-post` (a part no capture showed), and no compose line in the
trace while it was shown.

## 5. Closed and reopened

- **The window closed into the tray** (niri's close-window): the app stayed (the tray watcher holds its item); a sent;
  b notified once (1006 ms; no second notification within 5 s), and the window opened again from the item's `Activate`
  showed the message.
- **The app quit** from the tray's menu: the app and its broker exited cleanly; a sent 3 messages; b's app started
  again (a new broker, the same sandbox) opened at the a–b chat (its kept selection) and showed the 3 within 5.7 s of
  its start. Then b's view held each of the run's 25 messages to b (20, the reply, the tray's, the 3) exactly once. It
  raised no notification for the 3: they arrived before the app started, and Task 14's rule notifies a catch-up's
  messages only after the app's start (and the chat was shown in the focused window anyway). The plan's "notifies
  once" is the tray case above.

## 6. `fence.log`: met

Run 5, from the byte the run began: 171 whole lines, every one an allow.

| | count |
|---|---|
| a–b chat: message / edit / reaction / read receipt / typing | 25 / 1 / 1 / 23 / 26 |
| group chat: message / read receipt / typing | 1 / 2 / 1 |
| own availability (`Scope::Account`, each a lab account's own) | 13 |
| `PostRead` `presence` (decisions / requests) | 14 / 14 |
| own-state: `endpoint_registration` | 4 |
| frames: authenticate / ping / message-loss ack / disconnect | 4 / 26 / 12 / 4 |

The one deny the test accepts is the fence refusing the middle tier's profile lookup (`PostRead` `short_profile`) of
someone outside the lab, nothing sent: run 3 logged 36 such decisions when b opened a channel (its senders are tenant
users the engine asks names for, M2 Task 12's design: the fence is asked and refuses). Run 5 had none (the profiles
run 3 read were still fresh: a profile is read again once it is a day old). Any other deny fails the run.

## 7. The other live checks: met

- `proto-lab run ab-chat --headless` (10:31Z): exit 0, `ok`; `proto-lab run abc-chat --headless` (10:33Z): exit 0,
  `ok`. No breach.
- `teams-headless`'s `chat_live` (10:35Z): passed; `fence.log` 89 lines, all allows (53 messages, 1 edit, 1 reaction,
  1 read receipt on the a–b chat, 10 messages on the group chat), no deny.

## The failed runs

None was a breach: the app runs no browser, and the `proto-lab` runs above were clean. Each failed run's output is kept
under `<data_dir>/ui-live/`.

- **Run 1** (09:44Z): failed at the reaction: the reply had left its message selected, so the orchestrator's Escape and
  Up selected the message before it and the like went there (allowed, on the a–b chat). The orchestrator now uses Down
  to the newest. The latency of this run is above.
- **Run 2** (09:49Z): failed at the start: the orchestrator read c's `/proc/<pid>/environ` right after the fork, before
  the exec. It now reads it once the window is up.
- **Run 3** (09:51Z): every step passed; failed on `fence.log` (the 36 refused lookups above) and would have failed the
  latency (p95 2030).
- **Run 4** (10:09Z): every step and `fence.log` passed; failed the latency (p95 2345) under the CPU cap.

## The gate

On the final tree (10:41–10:53Z, 4 CPUs): `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
--all-features -- -D warnings`, `scripts/lint-report.sh` (one reasoned complexity `expect`, unchanged), `cargo test -p
fixture-guard` and `PROTO_LAB_REQUIRE_CHROME=1 GPUI_SMOKE_REQUIRE=1 cargo test --workspace` (every test binary ok, the
sandbox and Chrome tests run, not skipped) all passed. No flake needed a rerun.
