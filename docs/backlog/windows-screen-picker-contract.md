---
worth: next
where: crates/teams-media/src/devices/native/screen.rs, scripts/test-windows-media.ps1
added: 2026-11-11
---
# Supply and drive the Windows screen-picker fixture

Round 1 Windows review B4 is real. The ignored picker contract expects a solid dark-blue fixture window, but the
repository supplies neither that window nor a runner invoking the test with `--ignored --exact`. The normal native
media script skips it. Its claimed automatic hardware-contract coverage has not been established.

Defer this minor because a reliable native fixture and picker driver need a separate console/capture task. Add a
bounded console runner that owns a solid dark-blue window, verifies its PID and visible geometry, selects only that
window in the system picker, and executes the exact ignored test. Set `NGTEAMS_MEDIA_PICKER_SHOT` to retained private
evidence. Reject other windows, monitors with unrelated content, cancellation and empty captures; dispose both the
fixture and picker on every failure. Assert BGRA channel order and bounded idle drop using the actual captured frame.

Do not remove the assertions, weaken source accounting or claim the ordinary synthetic GStreamer tests cover this
native picker. The coverage floors and existing exclusions remain unchanged pending this explicit hardware check.
