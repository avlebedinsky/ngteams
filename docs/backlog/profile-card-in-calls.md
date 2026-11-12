---
worth: yes
where: crates/teams-ui/src/call_stage/people_pane.rs, crates/teams-ui/src/call_stage/meeting.rs, crates/teams-ui/src/shell/profile_card.rs
added: 2026-11-11
---
# Profile card from a call's people

In Teams a person in a call's People pane (and a meeting's participants) opens their profile card. The app's card
(`crate::profile_card`) is hosted by the shell of the main window, which opens the asks of its own window only; a call
runs in a window of its own (`crate::call_window`), which has no card host, and the card's actions (opening a chat,
starting a call) belong to the main window while a call is up. Hosting it there needs a card host in the call window
(the compact card over the stage) whose Chat hands the main window's shell the chat to open, and whose calls are
offered only as Teams offers them from inside a call.
