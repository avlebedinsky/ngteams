---
worth: soon
where: crates/teams-core/src/account.rs (account_calls), crates/teams-core/src/calling/media.rs (Relay), docs/server.md (The TURN relay)
added: 2026-11-15
---
# Own-server calls use the server's relay

An own-server account's calls have no media. The server hands out its coturn's credential and the relay's URLs at
`GET /trap/tokens` (`urls`, from `calling.turn.urls`; `docs/server.md`, "The TURN relay"), but the client reads only
the credential: `teams_core::account::account_calls` gives every account `Relay::Teams`, whose servers are Teams'
relay host (`teams_media::relay::teams_relay`). `Relay::Fixed` exists only under `test-support`, which is how the
server's call tests (`crates/ngteams-server/tests/e2e/calling/engines.rs`) get their media through a relay of their
own. So a shipped client signals the call through the own server, gets its credential, and then allocates on
Microsoft's relay with it: the allocation is refused, the call connects silent, and the own server's credential
(its user name is `<expiry>:<account id>`) has gone to a host it wasn't issued for, against the rule that an
own-server account's tokens go only to its own server.

Give an own-server account (`Cloud::Own`) a relay that takes its servers from the `/trap/tokens` answer's `urls`
(`turn:`/`turns:` URLs with their transport), and no relay at all, so no allocation anywhere, when the answer has
none or isn't the own server's. A Microsoft account keeps `Relay::Teams` byte for byte. Cover it with an engine e2e
test against the in-process server that names no relay itself: two own-server engines connect audio through the
loopback TURN the server's `calling.turn.urls` names, and a test that an own-server account never resolves or
contacts Teams' relay host. Then drop the "Not in the client yet" note from `docs/server.md`.

Deferred from task 184 (the server's call controller), whose scope excluded the client crates.
