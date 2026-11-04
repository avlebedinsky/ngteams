# The lab's rules in full

[AGENTS.md](../AGENTS.md) carries the short form. The agent checks this rule before every live run of `ngteams` or
`proto-lab`; the application does not enforce a lab roster or inspect `lab.toml`.

**Lab accounts talk and call only each other, the echo service and the user's own account, both ways.** The rule is
unchanged. It covers messages, reactions, chat membership, invitations, meetings, calls and team/channel changes.
The user's own client follows its [personal-build rules](personal-build.md).

## Who the lab may reach

- The dedicated lab accounts in `lab.toml`'s `accounts`, identified by MRI or UPN.
- The approved echo service in `service_participants`, identified by MRI.
- The user's own account in `contacts`, identified by MRI and by UPN when given. It may be reached as a lab account
  is, for messages, chats, membership changes, invitations, meetings and calls either way.

Apps and bots aren't people. Their presence in a target does not expand the set of people the lab may reach. No
account token or credential is sent to a bot, an app's backend or any other third party.

## Before each live run

The agent states the following in its run notice:

1. Which lab accounts it will run and which chats, calls, teams, channels or meetings it will use.
2. Every person those operations reach: everyone on each target and anyone the run adds or invites.
3. That every person is a lab account or the user's own account, and any explicit service test call uses the
   approved echo service.

Check the target's actual participants using the available UI, account configuration or protocol reads; do not infer
that an arbitrary chat or team is a lab target just because a lab account can see it. If the people reached cannot be
established, do not run that operation. Widening the allowed set needs the user's sign-off. A call scenario answers,
declines or joins only its own call.

For example, a two-account smoke run names a and b, their 1:1 chat and a call in it, and confirms that only those two
lab accounts are reached. The check is made again before a later live run, including a web capture.

If a run reaches someone outside the allowed set, stop and retain its output. Report what happened and any later
clean run separately.

## Running the lab client

Use `scripts/run-lab.sh <alias>` for `ngteams`, with `--pipewire` when the test needs media. It builds a normal client
in `target/lab` with the lab marker, starts its credential broker outside the UI sandbox and opens its window inside
it. Each alias has its own data/config under the XDG data home's `ngteams-lab/<alias>/` and uses only the
`ngteams-lab` credential namespace. Several aliases can run together; each holds an exclusive alias lock and a shared
live lock for its lifetime. The runner applies an outer shutdown deadline and keeps diagnostics after stop.

The user's account is only someone the lab may reach; it is never the identity an agent signs into the lab client.
Agents run that account only through `scripts/run-personal-read-only.sh`, and install its client only on an explicit
user request. Every lab or test GUI uses the UI sandbox's display, bus and PipeWire, never the user's own session.

`proto-lab` remains the web-capture and scenario tool. Use its normal build, headless muted browser, exclusive live
lock and an outer timeout. Explicit capture holds keep selected writes unsent while recording their shape; they are
capture tools, not a policy that decides whom the lab may reach.

## Transport and records

- Typed requests go through the transport: `https` only, no proxy or redirects, one token per service's host. Writes
  are single attempts; the outbox owns retries and never blindly resends an unknown outcome or resends across runs
  without `Store::resend`.
- The call tracker retains offer filtering, delivery deduplication and callback links; the core's call floor keeps
  one call or ring across the app's accounts. Responses use the delivery's own connection-bound typed responder.
- At `debug`, every API call has its full request and response and every Trouter frame is logged, credentials
  redacted by the logger. At `info`, logs are payload-free. Captures, tokens and debug logs never enter the repo, even
  sanitized; committed fixtures are synthetic and marked.
- Live tests never send from a `cargo test` process: they spawn the normal lab client. A `test-support` binary
  refuses to run. Offline tests use synthetic mocks and isolated sandboxes.
