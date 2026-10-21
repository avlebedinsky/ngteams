---
worth: later
where: docs/protocol/chat.md (Images: sending, denied)
added: 2026-10-21
---
# Sending an image: can an AMS upload be bound to its chat without a registry?

M1 Task 20 (`docs/plans/20261020-native-teams-m1-headless-chat-core.md`) asked to capture an image sent in the a–b
chat with the media service's writes denied, then decide whether `SendImage` can be a fenced write. The capture
(`proto-lab run ab-image`, 2026-10-21, written up in `docs/protocol/chat.md`) showed only the object's creation:
`POST https://us-prod.asyncgw.teams.microsoft.com/v1/objects/` with the chat service's `ic3` bearer token and
`X-MS-Migration: True`, body `{"type": "pish/image", "permissions": {"<chat id>": ["read"]}, "sharingMode": "Inline",
"filename": …}`. Denied, it was retried a few times and the message stayed pending; no upload and no message post went
out. Decided with Codex (transcript `discussions/task-20-ams-image-sending/` of the run): **defer**. Nothing is mapped,
nothing is on `HttpTransport`'s allowlist, there is no `SendImage`; every write to an AMS object stays `Unknown` and
denied. OneDrive files go to M2 with Graph either way.

The two blockers:

1. **The upload's audience.** The creation names its chat, so it could be judged as a write to `Conversation(<id>)`.
   The upload (`PUT v1/objects/<id>/content/imgpsh`, bundle only) names only the object. A sealed native
   "create, then upload to the id it answered" handle proves where the object was created, not who can read it now:
   `POST v1/objects/<id>/forward` (bundle only) adds readers to an existing object, from any client. Binding the upload
   needs an authoritative, complete read of the object's current readers at decision time (`GET v1/objects/<id>/`
   exists in the bundle; its answer was never seen), and even then the gap between that read and the upload needs its
   own justification (evidence that the service binds an upload to the object's readers, or that a forward can't race
   it), not just a repeated read.
2. **Uncaptured shapes.** The creation's answer, the upload and its answer, and the message post that references the
   object (`amsreferences`, the `<img>` markup) were never seen; the plan handles a response shape only once a capture
   showed it.

What would settle it, in order:

1. A reviewed one-time mapping of exactly the captured creation (the host `us-prod.asyncgw.teams.microsoft.com` only,
   the captured headers and body, one `permissions` key that is the a–b chat, value `["read"]`), judged by
   `Fence::check`, uploads and forwards still denied; then a read-only `GET v1/objects/<id>/` probe of that object to
   learn whether it lists the readers, completely. Plan for the pending message the web client keeps meanwhile (it
   retries the creation and holds its page with a `beforeunload` dialog; after a reload the image was gone).
2. Only if that read is authoritative: an upload mapping that reads the object's readers fresh at decision time and
   denies anything but exactly one lab conversation, with the forward race assessed; then capture the upload's answer
   and the message post.
3. The native `SendImage` after that, on Task 12's message body (its `MessageText` refuses images today) and Task 19's
   `teams-headless`.

Meanwhile `ab-image` force-denies every write to an AMS object (labelled) and stays a capture scenario; to undo the
deferral, follow the steps above and add the mapping in `crates/proto-lab/src/classify.rs` and the route in
`crates/teams-proto/src/http.rs`.
