---
worth: later
where: docs/protocol/chat.md (Files: sending, denied), crates/proto-lab/src/scenario/ab_file.rs, crates/teams-ui/src/chat_view.rs
added: 2026-10-22
---
# Sending a file: the account's file storage check comes first, then an upload and a share nothing fences yet

M2 Task 23 asked to capture the web client sending a file in the a–b chat with every write of it denied (as M1 Task
20 did for images, `docs/backlog/ams-image-sending.md`), and to decide from that whether a file's sharing can be
fenced. The capture (`proto-lab run ab-file`, 2026-10-22, written up in `docs/protocol/chat.md`, "Files: sending,
denied") never got as far as a write of the file:

- A made-up text file dropped on, or pasted into, the compose box was not taken. The box's "Attach files" button,
  clicked with the page's file pickers made to hand the file over, opened no menu and asked for no file: the page
  said "Sorry, you need OneDrive to share files. See your admin about getting a license."
- What came before it: the client asks the middle tier whether the account's own file storage is ready,
  `POST <middle tier>/beta/me/files/availability` (no body, the `api.spaces.skype.com` token, `x-ms-client-caller:
  IsFluidSiteProvisioned`), on every load. The fence denied it then, so the client never learned the answer; under
  the lab rule it passes (it reaches nobody), and the next `ab-file` run shows what follows it.
- So nothing of a file went out, under any of the run's forced denies: no write to SharePoint or OneDrive, Graph or
  an AMS object, and no message post naming the file. The only post was the caption alone (`properties.files`
  `"[]"`), denied by the run's marker rule.

**Unknown: whether the lab accounts have a OneDrive at all.** The notice may be the client's reading of a denied
check, or the truth (the lab accounts are licensed for Teams; nothing has read their storage). A read-only check
(`GET` of the account's own drive on Graph, a probe like Task 12's `proto-lab probe profile`) would settle it without
any allowance.

## What would settle sending, in order

1. **The storage check.** `me/files/availability` changes nothing and names only the account (`me`): under the lab
   rule it passes, logged `allowed`. `ab-file` runs again to see its answer and what the client does next; if the
   account has no OneDrive, files stay out of the lab for good.
2. **The upload** (bundle only): into the account's own OneDrive, folder "Microsoft Teams Chat Files", by Graph or
   SharePoint's `…/drive/root:/<folder>/<name>:/createUploadSession` and the session's content `PUT`s, or a
   `quickUpload`. SharePoint, OneDrive and Graph are contacting services, so every write there is denied until a row
   carves it out. A file in the account's own drive that nobody else can read reaches nobody, so the upload could
   become such a row (with its test), if the capture shows that the folder grants nobody else access (its permissions
   read fresh) and the request names nothing but the account's drive.
3. **The share** (bundle only, `shareDriveItem`): the item shared with the chat's members. Unlike an AMS upload, the
   share names its readers, so `Fence::check` could judge it like a message: every recipient, fetched fresh, a lab
   account, and the link's scope "specific people" only. A link for "people in your organization" or "anyone" is
   readable beyond the lab and can never pass. The item's readers can also change later from any client (as an AMS
   object's `forward` can), so each share is judged by its own body and nothing is assumed from an earlier one.
4. **The message**: a post to the chat with `properties.files` naming the shared item, mapped `Message` →
   `Conversation(<chat>)`; its shape is only in the bundle (an entry with `fileName`, `fileInfo` (`fileUrl`,
   `shareUrl`), `@type` `http://schema.skype.com/File`, `objectUrl`, `itemid`, `title`, `version`), so it waits for
   a capture.

## Received files meanwhile

No message with a file has ever been read in the lab: every message had `properties.files` `"[]"`. Until a capture
shows one, `teams-proto` keeps a list with an entry as it came (`Message::files` is `Field::Unknown`,
`teams_proto::chat::Unshown`; `properties.cards` and `amsreferences` with an entry likewise), and the app shows such a
message's text with "This message has content that can't be shown here" and an "Open in Teams on the web" button
(`web_chat_link`: the chat in the web client, through `open_url`), never a guess at the file; a channel post shows
the line without the button (no channel deep link has been captured). **The file card waits
here**: once a capture shows a file message (from step 4, or b's read of it), write synthetic fixtures of every form
the capture shows, read the entry into a typed file (name, size, type, link), show it as a card and open its link
through `open_url`; the file's content is never fetched by the app.

To pick this up: start with the read-only check above, then step 1; `ab-file` force-denies every write of a file
meanwhile (the file services, AMS objects, and any write naming a lab file) and stays a capture scenario.
