---
worth: maybe
where: crates/ngteams-server/src/presence, crates/ngteams-server/src/people/loki.rs
added: 2026-11-15
---
# Own server: presence and schedule follow-ups

- **In a call.** The server shows `Busy`/`InACall` only when a client reports it on its endpoint, and ngteams
  reports `Available` only. Once the call controller is on main, have it report the account's endpoint as in a
  call while a call is up.
- **An endpoint's end with its socket.** An endpoint lapses `presence.endpoint_ttl` (320 s) after its last report,
  so an account shows available up to that long after its last client quit. Teams took an endpoint offline within
  25 s of its socket's close when no `user.activity` was sent (`docs/protocol/trouter.md`, "What b saw"). Ending
  the endpoint a short while after its push socket closed needs the client's reconnect and re-report timing
  checked first, or a reconnect would flap the account offline.
- **The schedule.** Loki's `api/v1/schedule` answers `Unknown` and no working hours. With the calendar service on
  main, answer the person's free/busy now and next from their events, and working hours from a setting.
