# M1 acceptance check

Date: 2026-10-21. M1 Task 21 (`docs/plans/20261020-native-teams-m1-headless-chat-core.md`), on branch
`native-teams-m1-headless-chat-core` after Task 19's live run. Every live command ran from a **normal build**
(`cargo build -p teams-headless -p proto-lab --target-dir target/lab`, no `test-support`), headless, under the lab's
live lock (`flock ~/.local/share/teams-lab/live.lock`) with an outer `timeout --signal=INT`, pinned to 4 cores, one
process per account at a time; nothing was left running after any of them. Writes went only to the lab's a–b chat;
the one write aimed at the a–b–c group chat was denied by the fence and never sent. No token was printed: every
output was checked for JWTs, bearer headers and skypetokens. Load average 3–20 on 20 CPUs.

| # | criterion | verdict |
|---|---|---|
| 1 | native a → b, N = 50, send → receive p95 under 2 s | **met** (run 6: p50 1107 ms, p95 1370 ms, max 1499 ms); four runs before failed on two Trouter bugs, both fixed (below) |
| 2 | an edit and a reaction reach b | **met** (1115 ms and 1168 ms from the write's start, after the command's catch-up) |
| 2b | a 3-person group chat of a, b and c | **being fixed** (the user's decision of 2026-10-21: M1 doesn't exit on 1:1 chats only; the group parts rerun after the fence change). Today the fence denies a group write, as it must; nothing was sent |
| 3 | catch-up: b's socket closed, a sends, b reconnects with every message exactly once | **met** |
| 4 | `fence.log`: allows only on the lab chats, no deny of a mapped write, own-state and frames only from the reviewed lists, no delivery response, lines whole | **met** (one deny: the group write of 2b) |
| 5 | `proto-lab run ab-chat --headless` and `abc-chat` exit 0 | `ab-chat` **met** (exit 0, the third run); `abc-chat` exits 1 by design, **being fixed** with the group chat (2b) |
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
4. a sends one message to the a–b–c group chat: the command must fail with the row `denied`, and b must see nothing
   in that chat for 10 s.
5. b's watch is stopped (its socket closes), a sends 3 more, b watches again until registered and caught up.
6. b's store (`show`) must hold every message of the run exactly once, by text (the edited one by its new text).
7. b reads the chat up to the run's last message (`teams-headless b read`, a read receipt).
8. `fence.log` and the outputs are checked as in 4 and the summary written.

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
same profile (p95 1305–1390).

## 2. Edit and reaction: met; the group chat: being fixed

- Edit: b's watch showed the `edit` change with the new text 1115 ms after the write's start in a's `edit` (after the
  command's catch-up, as for the send start in §1); the reaction 1168 ms after the write's start in a's `react` (run
  5: 1073 and 938).
- **Group chat: being fixed.** Task 8 found no evidence that a group chat's member read is complete, so the fence
  keeps group chats `Partial(Unsupported)` (`docs/backlog/group-chat-member-read-completeness.md`). The user decided
  on 2026-10-21 that M1 doesn't exit on 1:1 chats only: group chats get fixed as separate work after this task, the
  fence loosened if needed, and the group parts run again after that change. The run shows what the fence does until
  then: a's message
  to the group chat was denied (`fence.log`: a `message` deny on the group chat, reason `partial: unsupported`), the
  outbox row ended `denied`, the command exited non-zero, nothing went out, and b got no event in the group chat. The
  plan's box for the group chat stays open until the group parts pass after the fence change; this task didn't touch
  the fence.

## 3. Catch-up: met

b's second watch caught up on connect (`caught_up`, cause `connected`, the 3 messages sent while it was down inserted),
registered, and the store then held each of the run's 53 messages exactly once (`b show a`).

## 4. `fence.log`: met

Run 6's lines, all whole JSON lines (73):

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
- `proto-lab run abc-chat --headless`: **exit 1 by design** (06:06Z): the scenario refuses to message the group chat
  before it opens a browser, because a's message would be denied and the web client would send it again on every
  later load of a's profile. Being fixed with the group chat (the user's decision of 2026-10-21): its messaging part
  runs again after the fence change (`docs/backlog/group-chat-member-read-completeness.md`).

## 6. Gate and flakes

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
`scripts/lint-report.sh` and `PROTO_LAB_REQUIRE_CHROME=1 cargo test --workspace` pass (all pinned to 4 cores, load
about 4): 1064 tests passed, none failed, no flake this time (the known Chrome-under-load ones in `CLAUDE.md` didn't
come up), and `cargo test -p fixture-guard` passed after the last doc edits. No breached run: the native runs have no browser to arm,
and none of the three `ab-chat` runs reported a breach or a containment.
