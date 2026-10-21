# Review profile: ms-teams

## What it is

- A native Microsoft Teams client for Linux (Rust + GPUI) written against Teams' internal APIs, with the web client
  as the reference. Personal project, one maintainer, most code written by agents.
- M0 (the protocol lab: Chrome for Testing under an armed fence) and M1 (the headless chat core: `teams-auth`,
  `teams-proto`, `teams-store`, `teams-trouter`, `teams-core`, `teams-headless`) are done. M2, the GPUI client, is
  next.
- It runs live against a **real company tenant** through three dedicated lab accounts (a, b, c). Nobody else may
  ever notice anything it does.

## What a real failure looks like

- **A fence escape (always critical).** A request or frame that others in the tenant could notice goes out without
  passing through the fence. That covers a message, edit, delete, reaction, typing, read receipt, presence, call,
  invite or member change. Also critical:
  - anything that mints a `WritePermit` outside `Fence::check`;
  - a path-only allowance that ignores the query or body;
  - a shape treated as known that no capture showed;
  - an `Unknown` treated as allowed;
  - a group chat other than the recorded `abc` admitted.
- **A request outside the sealed policy.** The native client may send only reads, sealed `OwnState` entries
  (logged before they're sent) and `Authorized<_>` writes. Anything else is a defect: a raw request, a hidden retry,
  an answer to a Trouter delivery, a redirect or proxy, a host outside the allowlist.
- **A leak.** A token, cookie or payload reaching a log, stdout, an error, `Debug` or the repo. Captures and
  tokens never enter the repo, not even sanitized; committed fixtures are synthetic, marked `_synthetic`.
- **A message sent twice or lost.**
  - The outbox resends without its arrival check.
  - A row of another run is sent without an explicit `Store::resend`.
  - An older version overwrites a newer one.
  - A field in a form no capture showed is read as absent, 0 or newer instead of kept as `Unknown`.
- **A token trusted past its expiry**, or a credential handed to a third party (a bot, an app backend).
- **A lab browser target resumed unarmed.** Arming order and close-or-kill containment are in `CLAUDE.md`. A breach
  is never a flake.
- **A test that only covers the mock's form of a field.** Two M1 bugs lost messages live this way. A fixture must
  hold every form the captures show.

## Blast radius

- Fence, policy, auth and secrets code: the user's employer tenant and colleagues. This is the reason the project
  exists in this shape. Treat any doubt here as material.
- Store, outbox and sync: the lab accounts' data and the correctness of what the client shows.
- `proto-lab` scenarios, spikes and `tools/`: lab harness only. A defect matters when it weakens the fence, misjudges
  a run (a breach recorded as a pass) or leaks; ergonomics are minor.

## The reporting bar

- Material, not merely true. Report a defect with a concrete failing sequence, a deviation from a written rule, or a
  test that can't catch what it claims to.
- Noise here: style preferences rustfmt and clippy already settle; asking for more comments; suggesting a
  dependency or an abstraction for its own sake; "consider handling X" with no reachable X.
- Severity: critical for anything in the first two failure classes above; major for loss, duplication, a trusted
  stale token, or a broken documented rule; minor for the rest.
- Finding nothing is a valid answer.

## Where the rules live

- `CLAUDE.md`: the hard rules (fence, allowances, native request policy, captures and fixtures, live runs),
  the patterns learned in M0/M1, the lint rules and the gate. A deviation is always worth reporting.
- `docs/protocol/*.md`: protocol notes in our own words. `endpoints.md` must gain or update a row in the same change
  that captures, maps, allowlists, uses or re-treats an endpoint.
- `docs/backlog/`: accepted, known gaps (one file each). Don't re-report one as new; do report a change that widens
  it.
- `docs/plans/`: historical records of decisions. They aren't a description of the current system and aren't
  updated to match code.
- `clippy.toml`, `scripts/lint-report.sh`, the workspace `[lints]`.

## Conventions that are deliberate

- Fail closed everywhere: unknown, partial, paginated or unreadable means denied, unstored or untrusted, never
  guessed. A value no capture showed is kept as `Field::Unknown(raw)`.
- No hidden retries: `reqwest` retries are off, the transport retries a read once, and the outbox owns the write
  retry table. An extra retry is a defect, not robustness.
- Trouter deliveries are never answered, presence is never set and an incoming call is never answered: all by
  design.
- `#[allow]` and inner `#![allow]`/`#![expect]` are refused. The only exception is an outer
  `#[expect(clippy::…, reason = "…")]` on one item. Over-limit functions are split. `clippy.toml` thresholds only go
  down.
- A released SQLite migration is never edited; a change adds a new one.
- `teams-trouter` caps `log` at `debug` at compile time; a new dependency logging at `debug` or below gets the same
  check.
- `test-support` features exist only under `[dev-dependencies]`. A lab binary refuses to run with them, and live
  runs use `cargo build -p`.
- Doc comments are full sentences explaining why and are dense by design; a long doc comment isn't a finding.
- Our own CDP client, not `chromiumoxide`, since that one resumes held targets by itself.

## Languages a change may touch

- Rust for almost everything.
- JavaScript only in `crates/proto-lab/assets/` (`ws_guard.js`, `rtc_hooks.js`, injected into lab pages) and `tools/` (interim Node
  tools). Never Microsoft JS.
- SQL migrations in `crates/teams-store/src/migrations/`, shell in `scripts/`, Markdown in `docs/`.
