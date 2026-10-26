---
worth: later
where: crates/teams-media/src/video/codec.rs, crates/teams/
added: 2026-10-26
---
# should the personal build's work calls rely on Cisco's H.264 patent cover, and how does it meet Cisco's conditions

M3 Task 18 (`docs/plans/20261024-native-teams-m3-calls.md`) chose H.264 through OpenH264 and settled the licence
and patent question with Codex (`docs/protocol/calling.md`, "The H.264 codec"): a normal build loads only Cisco's
prebuilt library at run time (its SHA-256 checked), the bundled source is compiled only for `test-support`, and no
patent licence is claimed for anything but Cisco's binary under its conditions. The lab meets those conditions now
(Cisco's download by `scripts/fetch-openh264.sh`, its licence saved beside it, `teams-headless --video` and the lab
app's settings switch as the control, the attribution beside it).

Two things are left to the user, and owed before the personal build's calls carry video (Task 22):

- **The value call**: Cisco's binary is licensed under its AVC patent portfolio licence "for the personal use of a
  consumer or other uses in which it does not receive remuneration"; the user's calls are work calls. Claude's first
  position was that a self-built, unpublished client used privately is practically unexposed; Codex's (which Claude
  took) was that neither private use nor `publish = false` settles a patent question, so the docs claim no clearance
  and the choice is the user's: rely on Cisco's binary as it is, obtain a licence of their own, or keep video off in
  the personal build. Nobody in the run can check the user's employer's position or the applicable law.
- **The personal build's provisioning**: where the binary is downloaded to (`~/.local/share/teams/openh264/`, by the
  user, never by an agent), the settings' switch and the attribution beside it, and Cisco's licence text where the
  app shows licensing information (condition 4 of `BINARY_LICENSE.txt`).

Meanwhile: the personal build loads Cisco's binary only from the user's own download in its data dir's `openh264/`
(`teams::calling::openh264`, its SHA-256 checked), with the settings' switch and Cisco's attribution beside it; no
download, and its calls carry audio only. Nothing ships a codec to the user; the value call above is still the user's.

Transcript: `~/.local/state/plan-exec-revmux/20261024-native-teams-m3-calls/discussions/task-18-openh264/transcript.md`.
