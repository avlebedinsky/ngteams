---
worth: soon
where: crates/ui-sandbox/src/pipewire/media.rs, crates/teams-media/src/devices/pipewire/streams.rs
added: 2026-11-06
---
# The UI sandbox's synthetic camera and screen deliver no frames

Found while checking the group call's share live (task #33): in a lab run with `--pipewire`, the screen picked at the
sandbox's fake screen-cast portal opened but its stream never handed the call a frame (the call's close log: `0 of a
shared screen sent`), so the far ends showed the share as the sharing line's black keep-alive frames (decoded, 1280
by 720) rather than the sandbox's 720p pattern. The sandbox's own tests show the same for both its video nodes:
`cargo test -p ui-sandbox --test device_choice` fails `the_camera_comes_through_the_sandbox_s_camera_portal_only_when_allowed`
(`camera=open`, `frames=0`) and `a_shared_screen_comes_through_the_sandbox_s_screen_cast_portal` (no `size`), while
its five audio tests pass. Neither test touches the share's code.

To find out: whether the sandbox's media helper still drives its video streams (its timer's `trigger_process`) on
the host's PipeWire 1.6.10 (installed 2026-11-01), and whether the reader's format negotiation
(`streams::video_capture`) reaches a format. Then run a lab group call again with a sharing and b and c viewing, and
check that they show the pattern.
