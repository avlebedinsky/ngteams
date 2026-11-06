---
worth: soon
where: Cargo.toml ([patch.crates-io]), crates/teams-media/src/rtc.rs, crates/teams-core/src/calling/share.rs
added: 2026-11-06
---
# sharing the account's screen in a group call

The native client shows someone else's screen in a group call, but doesn't share its own there (Share is offered in
1:1 calls only). The web client's group share is captured (`docs/protocol/calling.md`, "Share in a group call"): no
renegotiation, only `screenSharerAttributes` and `updateMediaDescriptions` with the mixer's sharing line `sendonly`,
labelled, tagged `<participant id>;ss_1`. The screen goes out on the line the mixer offered and the client answered
both ways.

The blocker is libwebrtc's side of that line: our answer is rewritten both ways in Teams' dialect, but libwebrtc's own
transceiver for it was created by the mixer's offer and answered receive-only (it has no track), so it encodes
nothing. To send without an offer of our own, the transceiver must go `sendrecv` with a screen track on its sender
before the answer is made. LiveKit's `libwebrtc` 0.3.55 has `RtpSender::set_track` but no
`RtpTransceiver::set_direction`; `webrtc-sys` 0.3.52 has it (`rtp_transceiver.rs`, `set_direction`). `add_track`
can't be aimed at that line (it takes the first unused video transceiver, a receiving line).

## Plan

1. Vendor `libwebrtc` 0.3.55 (as `scripts/vendor-gpui.sh` vendors GPUI) with `RtpTransceiver::set_direction` exposed
   (a few lines calling `sys_handle.set_direction`), through `[patch.crates-io]`.
2. `Peer::accept_offer`, a group call's: before `create_answer`, give the sharing line's transceiver an idle screen
   track (a `NativeVideoSource` that gets no frames, software H.264, `screen_encoding`) and `set_direction(SendRecv)`.
3. Share in a group call: pick the screen at the portal as today, feed its frames into that source, post
   `screenSharerAttributes`, and describe the lines with the sharing line `sendonly` + label + tag; Stop sharing stops
   the frames and describes it `recvonly`.
4. Live check: a native sharer and a web viewer can't run together under the lab's lock rules; use two native clients
   (a sharing, b and c viewing, which `CallRemoteShare` already shows).
