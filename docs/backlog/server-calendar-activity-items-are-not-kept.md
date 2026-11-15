# Keep the own server's meeting activity items in the feed

`crates/ngteams-server/src/calendar/activity.rs` (`tell`) tells an invited account of a meeting's invitation,
change and cancellation as a `NewMessage` of its `48:notifications` stream on `push:<mri>`. The push journal
replays it to a socket that reconnects within its window, but the item is not written to the account's stream, so
a client that was offline for longer, or a second device, never lists it.

Write the item through the chat service's own stream storage once it has one (the activity feed of the chat
service: the same thread a mention's or a reaction's item goes to), in the transaction-and-publish path that
storage uses, so read marks, the all-read bookmark, removal and the feed's sync apply to it. Keep `tell` as the
calendar's only entry to it. Cover it with the engine: an invited account that signs in later lists "<organizer>
invited you" with the meeting's time, and opening it finds the event.
