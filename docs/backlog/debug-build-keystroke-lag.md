---
worth: later
where: crates/teams-ui/src/chat_view.rs, crates/teams-ui/src/compose.rs, crates/teams-app/tests/ui_live.rs
added: 2026-10-23
---
# the debug build takes in keystrokes more slowly than the virtual keyboard types them

M2 Task 25's live run (`docs/m2-acceptance.md`) drove the lab app (a debug build, as every lab binary is) with the UI
sandbox's virtual keyboard, which types a key every ~35 ms. The app fell behind: in run 3 an Enter pressed 4 s after
the last of ~20 typed characters still waited 5.4 s before the compose box took the send (the first message, with the
app busy with its start), and the p95 of Enter → `compose sent` was 1.1 s. Once the run waited for the box's `compose
changed` lines to go quiet before pressing Enter (runs 4–5), Enter → `compose sent` was p50 82 ms, p95 149 ms. Under
the 4-CPU cap with three apps and three nested compositors, b's delivery → chat view shown also grew from ~0.3 s to
0.4–1.5 s (run 4), so the cost is the app's own work per change on a starved CPU.

Not measured: whether a release build (the personal build is one) keeps up with fast typing, and how much of each
keystroke's cost is the window laid out again (every view rendered each frame: the chat view's 50 messages included,
since no view is cached).

Suggested next step: time a keystroke's frame in a release build of the app on `teams-mock` in the UI sandbox (the
window test's harness), and if a frame costs more than a few ms, cache the message list's rows as views of their own
(GPUI's cached views) so typing in the box lays out only the box.
