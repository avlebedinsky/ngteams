---
worth: later
where: docs/protocol/chat.md (Images: sending, denied; The object created once)
added: 2026-10-21
---
# Sending an image: can an AMS upload be bound to its chat without a registry?

M1 Task 20 (`docs/plans/completed/20261020-native-teams-m1-headless-chat-core.md`) asked to capture an image sent in
the a–b chat with the media service's writes denied, then decide whether `SendImage` can be a fenced write. The
capture (`proto-lab run ab-image`, 2026-10-21, written up in `docs/protocol/chat.md`) showed only the object's
creation: `POST https://us-prod.asyncgw.teams.microsoft.com/v1/objects/` with the chat service's `ic3` bearer token and
`X-MS-Migration: True`, body `{"type": "pish/image", "permissions": {"<chat id>": ["read"]}, "sharingMode": "Inline",
"filename": …}`. Decided with Codex then (transcript `discussions/task-20-ams-image-sending/` of the M1 run):
**defer**, with the steps below to undo it.

The two blockers:

1. **The upload's audience.** The creation names its chat, so it could be judged as a write to `Conversation(<id>)`.
   The upload (`PUT v1/objects/<id>/content/imgpsh`) names only the object. A sealed native "create, then upload to
   the id it answered" handle proves where the object was created, not who can read it now: `POST
   v1/objects/<id>/forward` (bundle only) adds readers to an existing object, from any client. Binding the upload needs
   an authoritative, complete read of the object's current readers at decision time, and even then the gap between
   that read and the upload needs its own justification (evidence that the service binds an upload to the object's
   readers, or that a forward can't race it), not just a repeated read.
2. **Uncaptured shapes.** The upload's answer and the message post that references the object (`amsreferences`, the
   `<img>` markup) were never seen.

## What M2 Task 21 learned (2026-10-22)

Step 1 below was taken: the creation was mapped once in its exact captured shape for the a–b chat, judged by
`Fence::check`, and let out once per run (`proto-lab run ab-image-object`, two runs: the first breached by a system
suspend after the creation, the second clean); then the object was read with `proto-lab probe object`
(`docs/protocol/chat.md`, "The object created once"):

- The creation answers `201` `{"id": "<object>"}`; the client then tries the upload (`PUT …/content/imgpsh`,
  `application/octet-stream`, the image's bytes), and retries the creation when the upload fails. No message post goes
  out while the upload fails.
- `GET v1/objects/<id>/` answers its creator and the chat's other member `200` with eight keys of metadata
  (`contents`, `creation_date`, `expiry_date` 25 days on, `filename`, `geo`, `id`, `tenant_id`, `type`) and **names no
  reader**; it answers c, a lab account the object wasn't shared with, `403`.
- Decided with Codex (transcript `discussions/task-21-ams-object-read/` of the M2 run): **the read is not
  authoritative**. It lists no reader at all; per-account answers test only the accounts the lab holds and can never
  show that nobody else can read the object. Images stay deferred, M2 Task 22 was skipped, and the app shows an image
  as a placeholder with a link to its message in Teams on the web.
- The creation mapping was removed again in the same task (least privilege: no use is left for it). Its exact shape,
  the scenario and their tests are in the task's first commit (`feat: the AMS object creation mapped once …`); the
  read-only `proto-lab probe object` stays.

## What would settle it, in order

1. A read that answers an object's **complete current grants** (no paging, no hidden keys), found in a newer bundle
   or capture; to be tried with `proto-lab probe object` on an object created as in M2 Task 21 (the mapping restored
   from that commit, reviewed again, judged by `Fence::check`, uploads and forwards still denied).
2. Only if that read is authoritative: an upload mapping that reads the object's grants fresh at decision time and
   denies anything but exactly one lab conversation, with the forward race assessed; then capture the upload's answer
   and the message post.
3. The native `SendImage` after that, on M2's message body (`MessageText` refuses images today) and `teams-headless`.

Meanwhile every write to an AMS object stays `Unknown` and denied, and `ab-image` force-denies all of them (labelled)
and stays a capture scenario.
