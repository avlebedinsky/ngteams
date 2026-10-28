# Microsoft Graph: people's names and pictures

Status: 2026-10-21, M2 Task 12. Written from `proto-lab probe profile` run live as lab accounts a and b (each with its
own Graph token, reading a's, b's and c's records), which printed statuses, header names, content types, sizes and
key names with their JSON types, never a value, and kept nothing. The native client's Graph reads are in
`crates/teams-proto/src/read.rs` (`GraphUser`, `GraphPicture`), the transport's check of them in
`crates/teams-proto/src/http.rs` (`Service::Graph`), and their use in `crates/teams-core/src/profiles.rs`.

## Graph is our own reach, not the web client's

The web client never asks Graph for a person. It names people through the middle tier's profile lookups
(`beta/users/fetchShortProfile` and `beta/users/fetch`, reads sent as `POST`, [fence-targets.md](fence-targets.md#reads-by-post))
and draws their pictures from the middle tier's `profilepicturev2`, authenticated by a cookie it first gets from
`beta/users/<oid>/cookiev2` (a `POST` the browser fence denies). The only Graph request in the lab's captures is the
tenant's domain list (`v1.0/domains`, [endpoints.md](endpoints.md#microsoft-365-around-teams)).

So no capture shows the reads below. They are reads (`GET`, no body) of our own choosing, and the probe's answers are
the reviewed shape: the transport sends nothing to Graph but these two reads, in exactly the form the probe sent.

## The token

The native client (`1fec8e78-…`) gets a Graph token from the account's refresh token like any other audience
([auth.md](auth.md#open-questions), question 1): the scope `https://graph.microsoft.com/.default`. The lab accounts'
tokens carried `User.ReadBasic.All` among their scopes, which is what reading another account's name and picture
needs. No Conditional Access block, for either account.

## What the probe read

Each with `Authorization: Bearer <the Graph token>` and `Accept: application/json`, nothing else (no `X-MS-…`
header, no cookie). `<oid>` is the account's object id, the GUID its MRI `8:orgid:<oid>` names.

| Read | Status | Answer |
|---|---|---|
| `GET https://graph.microsoft.com/v1.0/users/<oid>` | 200 for a, b and c | `application/json`, about 370 bytes: `@odata.context`, `id`, `displayName`, `userPrincipalName`, `mail` (strings), `businessPhones` (an array), `givenName` and `surname` (strings, or `null` for the account without them), `jobTitle`, `mobilePhone`, `officeLocation`, `preferredLanguage` (`null`) |
| `GET …/v1.0/users/<oid>?$select=id,displayName` | 200 for a, b and c | `application/json`, about 160 bytes: `@odata.context`, `id`, `displayName`, all strings |
| `GET …/v1.0/users/<oid>/photo/$value` | 200 for a; 404 for b and c | a: `image/jpeg`, about 230 KB (the largest size Graph has); b, c: `application/json`, `{"error": {…}}` with the code `ImageNotFound` |
| `GET …/v1.0/users/<oid>/photos/96x96/$value` | 200 for a; 404 for b and c | a: `image/jpeg`, about 7 KB; b, c: as above |

The answers' headers were `cache-control`, `client-request-id`, `content-type`, `date`, `odata-version` (the user
reads), `etag` (a picture), `request-id`, `strict-transport-security`, `transfer-encoding`, `x-ms-ags-diagnostic` and
`x-ms-resource-unit` (the user reads). No redirect, no throttling.

## What the native client reads

- **A name**: `GET https://graph.microsoft.com/v1.0/users/<oid>?$select=id,displayName`. The `$select` form asks for
  nothing of the person's record but the name shown (no address, phone or title), and is read only when its `id` is
  the one asked for (any case); a missing, `null` or blank `displayName` is no name.
- **A picture**: `GET https://graph.microsoft.com/v1.0/users/<oid>/photos/96x96/$value`, the size an avatar needs
  (the probe's 96×96 picture was a thirtieth of the largest). Kept only as `image/jpeg` (what the probe saw) or
  `image/png`, up to 256 KB; a 404 is no picture, and the view draws initials.
- **Only for an MRI `8:orgid:<GUID>`**, and with a shared file's thumbnails (below) the only Graph reads the native
  client builds; the transport refuses any request to Graph that isn't a read.

## A shared file's thumbnail

A file a message carries (`properties.files`, [chat.md](chat.md#files-received)) is drawn as a card, a video's or an
image's with a picture of it. The web client draws that picture from SharePoint's own API with a SharePoint token;
the native client asks for no SharePoint token and reads it through Graph instead, by the file's sharing link, in
the form Microsoft documents. Like the person reads above, no capture or probe shows it: it is our own reach, two
reads, and a file the account can't read is just a card without its picture.

- **The list**: `GET https://graph.microsoft.com/v1.0/shares/u!<sharing link>/driveItem/thumbnails` with the
  account's Graph token, `<sharing link>` the message's `fileInfo.shareUrl` in base64url without padding (Graph's
  sharing token), only for an `https` link on a SharePoint host. The answer is documented as `value`, a list of
  thumbnail sets, each with an `id` and a `small`, `medium` and `large` thumbnail (`width`, `height`, `url`); an empty
  list for a file without one. The fixture `tests/fixtures/graph/thumbnails.json` is written from that documentation.
- **The picture**: the first set's `large` thumbnail (up to 800 pixels on its longer side; else its `medium` one),
  read at its `url` as Graph handed it out (`teams_proto::ThumbnailLink`): a pre-authenticated link that carries its
  own credential in its query, on SharePoint's media host (`https://<name>.svc.ms/transform/thumbnail?…`) or a
  SharePoint host, and nothing else. It goes with no token of the account's (`Service::FileThumbnail`); the log
  writes it at `info` by host and path alone, and at `debug` with its query's credentials redacted (a `tempauth`, an
  `access_token` in a link nested in `docid`, any JWT-shaped value: `teams-log`'s redactor); and it is kept only when
  its bytes are a PNG, JPEG, GIF or WebP picture.
- **Kept for the run** in the engine's memory (`teams-core`'s `thumbnails.rs`), once per file: an answer that isn't a
  success (403, 404), an empty list or bytes that aren't a picture are no picture, and the card shows its placeholder;
  a read that failed on the way is asked again later. The bytes reach the card only from the engine.

## Where a name comes from

`teams-core`'s engine (`profiles.rs`) asks, first that answers: Graph's name; the middle tier's profile lookup
(`users/fetchShortProfile`, a `PostRead` sent only with the fence's permit: in a lab build every MRI a lab account,
so someone outside the lab is never looked up by `POST`); the name the person's newest stored message carried
(`imdisplayname`); a lab account's name as `lab.toml` has it (its UPN's local part). The picture is Graph's alone.
What a read found is kept in the store and read again after a day; a read that failed on the way (the network, a 5xx,
a refused or throttled token) keeps the last one and is tried again after five minutes; one person is looked up once
at a time. The bytes of a picture reach a view only from the engine.
