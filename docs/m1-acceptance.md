# M1 acceptance check

Date: 2026-10-21. M1 Task 21 (`docs/plans/20261020-native-teams-m1-headless-chat-core.md`), on branch
`native-teams-m1-headless-chat-core` after Task 19's live run. Every live command ran from a **normal build**
(`cargo build -p teams-headless -p proto-lab --target-dir target/lab`; the group runs' `proto-lab` from `cargo build -p
proto-lab`; no `test-support`), headless, under the lab's
live lock (`flock ~/.local/share/teams-lab/live.lock`) with an outer `timeout --signal=INT`, pinned to 4 cores, one
process per account at a time; nothing was left running after any of them. Writes went only to the lab's a–b chat
and, after the fence change of 2026-10-21 (below), to the lab's recorded a–b–c group chat. No token was printed: every
output was checked for JWTs, bearer headers and skypetokens. Load average 2–20 on 20 CPUs.

**The group chat rests on an accepted assumption.** The fence admits exactly one group chat, the lab's own (`chats.toml`'s
`abc`), and only when its thread read and its member list, both read fresh within one 10 s budget, list exactly a, b
and c, the same in both, made by a, with `totalMemberCount` 3, equal `eTag`s, no paging key anywhere and the rest of
the shape the service gave live (`CLAUDE.md`'s hard rule "One group chat counts as read in full: the lab's own";
`docs/protocol/fence-targets.md`, "The lab's group chat"). The user set this on 2026-10-21 ("No, we need to fix group
chats. Loosen the fence if needed."). It is an accepted assumption, not evidence: a member the service hides from both
reads would still get the write. Every other group chat, a natively created one included, stays partial and is never
read.

| # | criterion | verdict |
|---|---|---|
| 1 | native a → b, N = 50, send → receive p95 under 2 s | **met** (run 6: p50 1107 ms, p95 1370 ms, max 1499 ms); four runs before failed on two Trouter bugs, both fixed (below) |
| 2 | an edit and a reaction reach b | **met** (1115 ms and 1168 ms from the write's start, after the command's catch-up) |
| 2b | a 3-person group chat of a, b and c, each message reaching both others | **met** (run 7: a's 10 messages each reached b and c natively, p95 1621 and 1615 ms; `abc-chat`: a's message shown by b's and c's web clients), under the accepted assumption above |
| 3 | catch-up: b's socket closed, a sends, b reconnects with every message exactly once | **met** |
| 4 | `fence.log`: allows only on the lab chats, no deny of a mapped write, own-state and frames only from the reviewed lists, no delivery response, lines whole | **met** (run 7: no deny at all) |
| 5 | `proto-lab run ab-chat --headless` and `abc-chat` exit 0 | **met** (`abc-chat` exit 0 at 07:43Z; `ab-chat` exit 0 at 06:16Z and again at 07:52Z) |
| 6 | the gate | **met** (flakes below) |

## The live test

`crates/teams-headless/tests/chat_live.rs` (`--features live`, and only with `PROTO_LAB_LIVE=1`) is an orchestrator:
it opens no fence, holds no token and sends nothing itself. It spawns the binary named by `TEAMS_HEADLESS_BIN`
(a test build has `test-support` and refuses to run, so a wrong path fails rather than writes), reads what each
process prints and parses `fence.log` from the byte where the run began. Its outputs go to
`<data_dir>/headless/<time>-chat-live/` with a `summary.json`, never into the repo. Steps:

1. b runs `watch --json`; the test goes on once b is registered and caught up.
2. a sends 50 messages to b, one `send` process each, one after another; each must print `state sent`, and each must
   reach b's watch as a `message` event (`change new`).
3. a edits its first message and reacts to it (`like`); b must see both.
4. c runs `watch --json` too, in a process of its own, ready by the same rule as b (registered, and a catch-up on
   connect with `failed` 0); a sends 10 messages to the a–b–c group chat (`send abc`), one process each, and each must
   reach both b's and c's watch; c's watch is then stopped. (Until the fence change this step checked that a's group
   message was denied: runs 5 and 6 below.)
5. b's watch is stopped (its socket closes), a sends 3 more, b watches again until registered and caught up.
6. b's store (`show`) must hold every message of the a–b chat's run exactly once, by text (the edited one by its new
   text), and b's and c's stores every group message exactly once.
7. b reads the chat up to the run's last message (`teams-headless b read`, a read receipt).
8. `fence.log` from the run's start: whole lines; allows only for the a–b chat's message, edit, reaction and read
   receipt and for messages on the recorded group chat (a write allowed on any other chat, another group chat
   included, fails the run); no deny at all, so none of a mapped write; own-state requests and frames only of the
   reviewed kinds. The outputs are checked for tokens and the summary written.

## 1. Latency: met

Run 6 (06:09Z, 188 s, load ~4), N = 50, times in ms:

| | p50 | p95 | max | min |
|---|---|---|---|---|
| a's send start → b's receive | 1107 | 1370 | 1499 | 912 |
| the post (start → `OriginalArrivalTime`) | 877 | 1149 | 1301 | 714 |
| the delivery (`OriginalArrivalTime` → b's receive) | 234 | 306 | 363 | 146 |

The send start is taken just before the write is handed to the engine, after the command's catch-up, so it includes
the fence's fresh member read of the chat and the post itself. **Most of the time is the post** (about four fifths);
Trouter's delivery to b takes about a quarter of a second. The two parts compare the service's clock with ours, so
each may be off by their skew; their sum is measured on one clock. Run 5 (06:00Z), the first that passed: p50 1137,
p95 1586, max 1719 (post p50 912, delivery p50 219). The messages that arrived in the failed runs 1, 3 and 4 had the
same profile (p95 1305–1390). Run 7 (07:44Z, after the fence change, with the group part; 210 s, load ~2): p50 1143,
p95 1501, max 1611 (post p50 930 / p95 1279, delivery p50 224 / p95 309).

## 2. Edit and reaction: met; the group chat: met

- Edit: b's watch showed the `edit` change with the new text 1115 ms after the write's start in a's `edit` (after the
  command's catch-up, as for the send start in §1); the reaction 1168 ms after the write's start in a's `react` (run
  5: 1073 and 938; run 7: 1235 and 1044).
- **Group chat: met** (run 7, 07:44Z), under the accepted assumption stated at the top. b's watch kept running from
  the a–b part, c's own watch registered and caught up with `failed` 0, and each of a's 10 messages to the recorded
  a–b–c chat reached both watches over Trouter; b's and c's stores then held each of them exactly once (`show abc`).
  Times in ms, from a's send start:

  | receiver | send → receive p50 | p95 | max | min | delivery p50 | delivery p95 |
  |---|---|---|---|---|---|---|
  | b | 1285 | 1621 | 1621 | 1244 | 226 | 303 |
  | c | 1303 | 1615 | 1615 | 1250 | 221 | 304 |

  The post (start → `OriginalArrivalTime`, the same for both) p50 1074, p95 1394: about 150 ms more than the a–b
  chat's, which fits the group chat's two member reads (the thread and its member list) against the 1:1 chat's one.
  The plan sets no latency target for the group chat; these are reported, not held to a limit (with N = 10 the p95
  is the maximum). The web clients showed the group chat working too: `abc-chat` (§5).
- Before the fence change, runs 5 and 6 checked the fence's answer then: a's group message denied (`partial:
  unsupported`), the outbox row `denied`, nothing sent, b no event in the group chat for 10 s
  (`docs/backlog/group-chat-member-read-completeness.md`, Task 8's evidence, which still stands).

## 3. Catch-up: met

b's second watch caught up on connect (`caught_up`, cause `connected`, the 3 messages sent while it was down inserted),
registered, and the store then held each of the run's 53 messages exactly once (`b show a`).

## 4. `fence.log`: met

Run 7's lines (07:44Z, with the group part), all whole JSON lines (88):

| verdict | what | count |
|---|---|---|
| allow | `message` on the a–b chat | 53 |
| allow | `edit`, `reaction`, `read_receipt` on the a–b chat | 1 each |
| allow | `message` on the recorded a–b–c chat | 10 |
| allow (own-state) | `endpoint_registration` (b twice, c once) | 3 |
| allow (own frame) | `trouter_authenticate` 3, `trouter_message_loss_ack` 9, `trouter_ping` 4, `trouter_disconnect` 3 | 19 |

No deny at all, no write on any other chat, nothing else. Run 6's lines (before the fence change), all whole JSON
lines (73):

| verdict | what | count |
|---|---|---|
| allow | `message` on the a–b chat | 53 |
| allow | `edit`, `reaction`, `read_receipt` on the a–b chat | 1 each |
| deny | `message` on the a–b–c group chat, `partial: unsupported` | 1 |
| allow (own-state) | `endpoint_registration` | 2 |
| allow (own frame) | `trouter_authenticate` 2, `trouter_message_loss_ack` 6, `trouter_ping` 4, `trouter_disconnect` 2 | 14 |

No other line: no deny of a mapped write, no own-state request or frame off the reviewed lists (the test checks them
against `teams_proto`'s `OwnState`/`OwnFrame` kinds), no delivery response (none exists), no breach. The windows of
runs 1–5 were checked the same way: allows only on the a–b chat (50, 0, 50, 50 and 53 messages), own frames and
registrations only, and in run 5 the same group deny; nothing else.

## Runs before the pass, and what they found

| run (UTC) | result | cause |
|---|---|---|
| 1 (05:36Z) | failed: 18 of 50 messages never reached b | b's push channel dropped deliveries whose id was negative |
| 2 (05:45Z) | failed before a sent anything | b's endpoint was never registered: its id held an `_` |
| 3 (05:50Z) | failed: 3 of 50 missing | the negative ids again, now seen in the new debug log |
| 4 (05:55Z) | failed: 3 of 50 missing | the debug log named it: the delivery's `id` a number but not unsigned |
| 5 (06:00Z) | **passed** | both fixes in |
| 6 (06:09Z) | **passed** | with b's read at the end (below) |
| 7 (07:44Z) | **passed** | after the fence change: the group part now a→abc with b and c watching (§2) |

Neither failure was the fence's: every window holds only allowed writes on the a–b chat and own frames. Both were in
`teams-trouter`/`teams-proto` and passed every offline test, because the mock and the fixtures never had them:

- **Delivery ids are signed.** A Trouter delivery's socket.io `id` is a signed 32-bit number; 267 of the 2515
  deliveries in the captures had a negative one. `teams-trouter` read it as unsigned, so such a delivery was an
  "unreadable packet", dropped without an event, and its message reached the store only at the next catch-up. Now read
  signed (`RawDelivery::id`, `Delivery::id`: `i64`), with a test on negative ids.
- **Endpoint ids are base64url.** The push URL's endpoint id is 22 base64url characters; 11 of the 28 ids in the
  captures hold a `-` or an `_`. The registration check accepted letters and digits only, so such a connect went
  unregistered and got no deliveries (the browser fence's own check already took `-` and `_`). Now accepted, with
  tests; `.`, `%`, `+` and `=` stay refused.

To find them, `teams-headless` got an opt-in diagnostic log, `TEAMS_HEADLESS_LOG=debug`: our own crates' debug lines
on stderr (the delivery's number, size and kind, why a delivery didn't read by shape only), never a payload, nothing
from other crates; `log` stays capped at `debug` at compile time, so no `trace!` (tungstenite's frames) exists to log.

## 5. `ab-chat` and `abc-chat`

- `proto-lab run ab-chat --headless`: **exit 0** at 06:16Z (both clients loaded, enforcement passed in a's page and
  in b's copy, a's message reached b; `fence.log`: `message` 2 and `typing` 2 on the a–b chat, 14 forced denies of
  probes, 301 unclassified denies all on the expected list, no breach, no containment). The two runs before it (06:05Z
  and 06:12Z) failed at "b receives the message" with no breach and the same clean fence picture: the native runs had
  left about 200 messages b's web client hadn't read, so it opened the chat at its "last read" mark with "Jump to
  newest" showing, and a's new message wasn't rendered. b's native read receipt (run 6) moved b's stored horizon but
  not the web client's mark. `ab-chat` now clicks "Jump to newest" while it waits (`ab_chat::wait_for_newest`, tested
  on a page that renders the newest message only after the click).
- `ab-chat` again after the group runs: **exit 0** at 07:52Z (the same steps; `fence.log` 450 whole lines: `message`
  2 and `typing` 2 on the a–b chat, 20 forced denies, 420 unclassified denies all accepted by the reviewed list, only
  own-state token requests besides; no breach, no containment). Its run at 07:48Z failed at "b receives the message",
  no breach and the same clean picture (`message` 2 and `typing` 4 on the a–b chat, 25 forced, 668 unclassified, all
  accepted): run 7 had left about 65 messages unread in b's web client, which opened the chat at "last read" again,
  but with a **"New messages"** pill instead of "Jump to newest", which `wait_for_newest` didn't know. It now clicks
  either label, on a `button` or a `role="button"` element (a second page test, with the pill as an icon and text in
  a `role="button"` element).
- `proto-lab run abc-chat --headless`: **exit 0** at 07:43Z (42 s), after the fence change. a loaded the web client at
  the recorded group chat; the live enforcement test passed (a's probe post force-denied and failed in a's page); a's
  message went out; b's and then c's web client opened the chat and showed a's message and not the probe. `fence.log`
  (314 whole lines): on the group chat `message` 2 and `typing` 4 allowed (each request is decided on the page's
  session and again on the browser's, so that's one message post and two typing posts), 10 forced denies (the
  enforcement test's rules: "denied on purpose by the run"), 260 unclassified denies, all accepted by the scenario's
  reviewed list (170 on the Teams origins, the rest telemetry, the push channel's hosts, augloop, substrate, …), and
  only reviewed own-state requests besides. No allow on any other chat, no breach, no containment. Its first attempt (07:42Z) never opened a browser:
  the `TMPDIR` passed to it (`~/.cache/ms-teams-test-tmp`) didn't exist yet, so Chrome exited before its debugging
  endpoint was up (exit status 21); nothing was armed, sent or logged (the `fence.log` window was empty). The run at
  06:06Z, before the fence change, exited 1 by design: the scenario then refused to message the group chat before
  opening a browser, because a's message would have been denied and the web client would have sent it again on
  every later load of a's profile.

## 6. Gate and flakes

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
`scripts/lint-report.sh` and `PROTO_LAB_REQUIRE_CHROME=1 cargo test --workspace` pass (all pinned to 4 cores, load
about 4): 1064 tests passed, none failed, no flake this time (the known Chrome-under-load ones in `CLAUDE.md` didn't
come up), and `cargo test -p fixture-guard` passed after the last doc edits. No breached run: the native runs have no browser to arm,
and none of the `ab-chat` and `abc-chat` runs reported a breach or a containment. After the group runs the gate
passed again (load 9–23): 1081 passed, 0 failed, 3 ignored, no flake, and `cargo test -p fixture-guard` after the doc
edits.
