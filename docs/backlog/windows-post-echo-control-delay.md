---
worth: later
where: crates/ngteams/src/ctl/mod.rs, crates/ngteams/src/ctl/listener.rs, crates/teams-ctl/src/transport/windows.rs
added: 2026-11-11
---
# A Windows calls read once returned after its incoming invitation expired

During Windows-port lab validation, after a's Echo test call ended, a bounded b-to-a decline scenario found both
call lists empty and placed b's call. The next `ngteamsctl calls` read from a returned about **35.6 seconds later**,
after the incoming invitation expired. No answer or decline was sent. The engine recorded the invitation arriving
and progressing around 12:53:47–48 UTC, its ring timeout at 12:54:18, and command completion around 12:54:22.
Those historical timestamps do not separate process launch, pipe connection, authentication, foreground dispatch,
reply and process exit, so they cannot identify which stage stalled.

The user chose to park this investigation. It remains unresolved and no longer gates completion of the Windows
port. The later timing runs below did not reproduce that delay; none establishes its cause or a fix.

| Later run | Measured result | What the result establishes |
| --- | --- | --- |
| First direct owned-invitation decline, helper 22 | Ringing query 279 ms; decline 599 ms; both call lists cleared | A complete native incoming-call/decline scenario can finish promptly. The unsuffixed result was later overwritten; the first run's full timing output survives only in its original session history. |
| Same owned-invitation scenario after another Echo call, helper 22 | Decline 456 ms; a cleared; b still ringing at the two-second check | The original control stall did not recur. The final caller-clear assertion failed; this is not a full scenario pass. |
| Read-only transport probe 22, 120 connections | Maximum connect 7 ms, hello 951 ms, calls/drop 976 ms, cumulative from each probe's start | Connect, hello, calls and drop all completed within a second in these samples. Its diagnostic read bound was 60 seconds; product and scenario deadlines were unchanged. |
| Synthetic local named-pipe server, helper 121, eight exchanges | Cold process-start return 2,798.5 ms and total 4,515.2 ms; warm start return 4.8–51.0 ms and totals 819.7–1,767.7 ms | Launch, pipe connection, hello, calls request/reply and exit were timed separately. These totals include PowerShell/server startup and scheduling; they do not measure a live engine. |
| Signed-in idle engine, helper 253, six staged calls reads | App ready in 5,856.77 ms; first hello response about 123 ms; each calls request/reply 4–11 ms; full proxy trials 835.7–1,234.7 ms, all with zero calls | The real engine and normal CLI respond promptly while idle, using the existing one-second connect and ten-second reply/exit bounds. No Echo or incoming call was initiated in this run. |

These measurements rule out an unavoidable 35-second cost on every pipe connection or every idle calls read.
They also show that an Echo call does not always produce the failure. They do not rule out an intermittent launch,
peer-authentication, foreground/event-loop or OS scheduling stall during the original incoming call. Defender's
inspected operational-event interval contained no matching event; absence of an event does not exclude Defender or
establish another cause. An independent cold native runner spawn once took 9,253 ms in CreateProcess and 9,536 ms
overall, followed by fast launches and a passing unchanged rerun. That separate launch measurement is not the
35.6-second calls failure.

The final-stream-event fix in `709b4c71` addressed another defect: disconnect could discard a slow subscriber's
queued final event. Native application event tests passed 21 cases after bounded draining was added, and native
control tests/coverage passed. This provides evidence about subscription shutdown, not a diagnosis of the calls
command delay. Later UI tests, picker checks and coverage runs likewise are not reproductions of the original
post-Echo sequence. Helpers 193 and 236 provide no additional valid live timing: 193 was prepared but not run, and
236 reused stale roster output after provisioning failed. Attempt 250 refused a test-support proto-lab before app
startup; attempt 252 failed an arbitrary five-second readiness checkpoint before application logging began.
Neither measures an engine control stall. Helper 253 replaced that checkpoint with the existing 90-second app
startup bound while keeping the control deadlines unchanged.

The post-Echo **caller-clear observation is separate**. Reinspection of the retained helper-22 evidence found that
the successful decline returned at 13:09:37.4801355 UTC. The last failing query still showed the caller ringing at
13:09:39.7223191; the caller logged remote end 603 at 13:09:40.040, **318 ms after that query and 2,560 ms after
decline**. The two-second observation failed, but it does not establish a caller that never clears. A declined 1:1
call can also continue into voicemail, so immediate emptiness is not a universal protocol requirement.

Private evidence pointers, without tenant data, capture contents, credentials or log excerpts:

- The guest task root is `C:/src/ngteams-windows-port-3`. Original failure: `target/decline20-result.json` and
  `target/decline20-status.txt`, plus the retained alias-a/b diagnostic files. Original chronology and procedural
  limits are in dispatch report `3-previous-15.md`.
- Later owned declines: `target/ngteams-decline22.ps1`, `target/decline22-postecho-result.json` and
  `target/decline22-postecho-status.txt`. The unsuffixed helper-22 result was overwritten; do not present it as the
  first run's retained result. Read-only probes: `target/ctl22-timing.rs`, its executable,
  `target/ngteams-timing22-run.ps1` and `target/ctl22-timing.txt` (120 samples). Dispatch report `3-previous-16.md`
  records the initial complete pass and its evidence limitation.
- Historical caller-clear analysis: host `/tmp/ngteams-postecho36-analysis.json`, with private inputs
  `/tmp/ngteams-a36-inherited.log`, `/tmp/ngteams-b36-inherited.log` and `/tmp/ngteams-a36-inherited-trace.jsonl`.
  Dispatch report `3-previous-21.md` records the corrected timing.
- Synthetic staging: guest `target/ctl121.ps1` and `target/ctl121-stages.json`; host
  `/tmp/ngteams-ctl121.ps1` and `/tmp/ngteams-ctl121-stages.json`. Dispatch report `3-previous-23.md` records the run.
- Real idle staging: guest `target/native253.ps1`, `target/ctl253.ps1`, `target/ctl253-stages.json`,
  `target/control253-startup.json` and per-alias `target/control253-*-reach.json`; host
  `/tmp/ngteams-native253.ps1`, `/tmp/ngteams-ctl253.ps1`, `/tmp/ngteams-ctl253-stages.json` and
  `/tmp/ngteams-control253-startup.json`. Dispatch report `3-previous-26.md` distinguishes the rejected/stale attempts
  from this actual measurement. Helpers time process-start return, both connections, hello request/reply,
  calls request/reply, forwarding and exit without copying response payloads into the timing summary.

Dispatch reports live under the private `.dispatch/20261109-121157-478f4b/reports/` directory, outside tracked source.
These paths are retained investigation aids, not committed fixtures or proof that old binaries match current source.
Any future live attempt must use normally built tools, a normal client through the lab runner, freshly checked
allowed target membership and initial selection, and the per-run lab-contact notice. It may decline only its own
invitation; the channel with outsiders stays unopened. Preserve the original failed observations and keep launch,
request, reply and exit timing separate, within existing product/scenario bounds.
