---
worth: later
where: scripts/test-windows-app.ps1
added: 2026-11-14
---
# Make scripts/test-windows-app.ps1 survive a cold build and a Linux-only target

Three faults of the script, seen on a cold `target/lab` in the win11 VM:

- **The build deadline is too short.** Each target builds with `-j1` under a 1800 s deadline. The first target of a
  crate on a cold target dir takes longer, so the run fails and has to be started again (three times in one QA
  round) until the cache is warm. Give the first build of a crate a deadline that fits a cold build, or build the
  crate's dependencies once before the per-target loop.
- **The kill leaves cargo compiling.** `Run-Child` ends with `$p.Kill($true)`, yet the cargo it "killed" kept
  compiling. Run the build in a job object that kills its whole tree, as the
  runner does for the app, and check nothing of it is left.
- **`-Group app` counts a Linux-only target as a failure.** The script lists every file in `crates/ngteams/tests`.
  `own_server` is `#![cfg(target_os = "linux")]`, so on Windows it builds to a binary with 0 tests, and the
  "at least one test passed" check fails it. Skip a target that is compiled out on Windows (add it to `$separate`
  with the other Linux harnesses, or take a 0-test result as a skip for targets named there).
