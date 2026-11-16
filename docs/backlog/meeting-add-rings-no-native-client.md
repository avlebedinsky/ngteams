# A host's add in a meeting does not ring the native client

On the own server, a meeting host's `conv/<id>/add` (`crates/ngteams-server/src/calling/group.rs`, `add` and
`ring_added`) seats the invitee and sends it a group call invitation (`evt` 109) whose `groupChat.threadId` is the
meeting's chat (`19:meeting_…@thread.v2`). The native client does not show it: `read_invitation_in` in
`crates/teams-call/src/track/offer.rs` accepts only a thread id that `teams_proto::is_group_chat_id` accepts, and that
function refuses a meeting chat's id. Thus the client sends no `attach`, and the invitee rings until
`calling.ring_timeout`. The e2e test `only_a_meetings_hosts_let_in_or_mute_and_an_unknown_meeting_is_none` asserts
only the `202` of the host's add for this reason.

To do: capture what Teams sends when a host adds someone to a meeting (the invitation shape, `meetingInfo`, the
thread id). Make the server send that shape, and make the client's tracker and machine offer it as a ring into the
meeting. Then add the ring and the answer to the e2e test.
