//! The chat service's rows (`docs/protocol/fence-targets.md`): the writes mapped to a chat target, and the named
//! carve-outs that reach nobody (typing, read receipts, the account's own view of a chat, its own feed, settings and
//! endpoints). Anything else a mutating request does on the chat service is [`Contact::Unreadable`].

use serde_json::{Map, Value};
use url::Url;

use super::json::{optional, strict_json};
use super::{Contact, Parsed, Policy};
use crate::{Mri, Scope, Target, WriteKind, chat_service_path, is_chat_id, is_group_chat_id};

/// The conversation properties that change only the account's own view of a conversation (a chat or a channel), set
/// with `PUT v1/users/ME/conversations/<id>/properties?name=<n>`: when it was last made a favourite (seen on a
/// channel) and the favourite flag itself.
pub const OWN_VIEW_PROPERTIES: [&str; 3] = ["lastTimeFavorited", "favorite", "isGeneralChannelFavorite"];

/// The keys a typing signal's body may have: what the web client sends, key for key (`chat-shapes`).
const TYPING_KEYS: [&str; 3] = ["content", "contenttype", "messagetype"];

/// The account's own streams whose items it may mark read.
const OWN_FEEDS: [&str; 2] = ["48:notifications", "48:mentions"];

/// The account's own streams whose read horizon it may move: its feeds and its call log, which the client moves when a
/// call ends (`docs/protocol/calling.md`). Nobody else reads them.
const OWN_STREAMS: [&str; 3] = ["48:notifications", "48:mentions", "48:calllogs"];

/// The `mentionType` of a mention of everyone in a group chat (`docs/protocol/chat.md`, "A mention of everyone").
const EVERYONE: &str = "everyone";

/// What a mutating request on the chat service is: a mapped write, a carve-out, or unreadable.
pub(super) fn classify(request: &Parsed<'_>, policy: &Policy) -> Contact {
    let Some((version, path)) = chat_parts(request.url) else {
        return Contact::Unreadable;
    };
    let mut path: Vec<&str> = path.iter().map(String::as_str).collect();
    // `ME` in any case, or the account's own MRI, is the account itself
    if let ["users", user, ..] = path.as_mut_slice()
        && is_me(user, policy)
    {
        *user = "ME";
    }
    if let Some(contact) = carve_out(request, &version, &path) {
        return contact;
    }
    // a mapped write reads its target only from a URL the fence's live member read accepts too
    if version == "v1" && chat_service_path(request.url).is_some() {
        return mapped(request, &path).map_or(Contact::Unreadable, Contact::Write);
    }
    Contact::Unreadable
}

/// The version segment and the decoded segments after it, for a URL on the chat service: an AFD host's
/// `/api/chatsvc/<region>/<version>/…` or a regional host's `/<version>/…`. `None` when it has no version segment, or
/// any segment (the base's too) isn't UTF-8 once decoded, is empty or holds `/ ; ? #` once decoded.
fn chat_parts(url: &Url) -> Option<(String, Vec<String>)> {
    let mut segments = super::decoded_segments(url)?;
    if segments
        .iter()
        .any(|segment| segment.is_empty() || segment.contains(['/', ';', '?', '#']))
    {
        return None;
    }
    if segments.first().is_some_and(|first| first.eq_ignore_ascii_case("api")) {
        // `api`, `chatsvc` and the region
        segments.drain(..3.min(segments.len()));
    }
    if segments.is_empty() {
        return None;
    }
    let version = segments.remove(0);
    Some((version, segments))
}

/// Whether a `users/<user>` segment names the account itself: `ME` in any case, or the account's own MRI.
fn is_me(user: &str, policy: &Policy) -> bool {
    user.eq_ignore_ascii_case("ME") || policy.own_account.as_ref().is_some_and(|own| own.as_str() == user)
}

/// The carve-outs: requests on the chat service that reach nobody, [`Contact::None`].
fn carve_out(request: &Parsed<'_>, version: &str, path: &[&str]) -> Option<Contact> {
    let reached_nobody = match (request.method.as_str(), version, path) {
        ("POST", "v1", ["users", "ME", "conversations", chat, "messages"]) => return typing(request, chat),
        ("PUT", "v1", ["users", "ME", "conversations", id, "properties"]) => own_property(request, id),
        ("PUT", "v1", ["users", "ME", "conversations", feed, "messages", _, "properties"]) => {
            OWN_FEEDS.contains(feed) && request.only_query("name") == Some("isread") && is_read_body(request.body)
        }
        ("PUT", "v1", ["users", "ME", "properties"]) => request.only_query("name").is_some(),
        ("PUT" | "POST" | "DELETE", "v1" | "v2", ["users", "ME", rest @ ..]) => is_own_endpoint(rest),
        _ => false,
    };
    reached_nobody.then_some(Contact::None)
}

/// A post to a chat's messages that is a typing signal: `Some(None)` for `Control/Typing` or `Control/ClearTyping` on
/// a chat by its id ([`is_chat_id`]: 1:1, group or meeting, the rule `Fence::check` permits it by) in the captured form
/// (only [`TYPING_KEYS`], no content), `Some(Unreadable)` for a typing signal in any other form or on any other id,
/// and `None` (so the mapping reads it) for a post that isn't a typing signal.
fn typing(request: &Parsed<'_>, chat: &str) -> Option<Contact> {
    let body = request.body.and_then(strict_json)?;
    let kind = body.get("messagetype")?.as_str()?;
    if !matches!(kind, "Control/Typing" | "Control/ClearTyping") {
        return None;
    }
    let object = body.as_object()?;
    let captured = object.keys().all(|key| TYPING_KEYS.contains(&key.as_str()))
        && object.get("content").is_none_or(|content| content == "");
    Some(
        if captured && is_chat_id(chat) && chat_service_path(request.url).is_some() {
            Contact::None
        } else {
            Contact::Unreadable
        },
    )
}

/// A conversation property that reaches nobody, set with exactly one query key: the read horizon on a chat by its id
/// ([`is_chat_id`], the rule `Fence::check` permits a read receipt by; a read receipt, which others may see) or on one of
/// the account's own streams ([`OWN_STREAMS`]), or one
/// of [`OWN_VIEW_PROPERTIES`] on a chat or a channel; or the favourite flag as the web client sets it on a channel it
/// just made ([`is_favorite_joined`]).
fn own_property(request: &Parsed<'_>, id: &str) -> bool {
    match request.only_query("name") {
        Some("consumptionhorizon") => is_chat_id(id) || OWN_STREAMS.contains(&id),
        Some(name) => OWN_VIEW_PROPERTIES.contains(&name) && is_conversation(id),
        None => is_favorite_joined(request) && is_conversation(id),
    }
}

/// The query `name=favorite&requirePermanentJoinedState=<boolean>`, in that order and nothing else: the web client's
/// favourite flag on each channel it made (`channel-create`, 2026-11-03), the account's own list alone.
fn is_favorite_joined(request: &Parsed<'_>) -> bool {
    matches!(
        request.query.as_slice(),
        [(name, favorite), (joined, flag)]
            if name == "name"
                && favorite == "favorite"
                && joined == "requirePermanentJoinedState"
                && matches!(flag.as_str(), "true" | "false")
    )
}

/// A feed item's read mark: exactly `{"isread": <string>}`.
fn is_read_body(body: Option<&[u8]>) -> bool {
    body.and_then(strict_json)
        .as_ref()
        .and_then(Value::as_object)
        .is_some_and(|object| object.len() == 1 && object.get("isread").is_some_and(Value::is_string))
}

/// The account's own endpoints, after `users/ME`: `endpoints`, `endpoints/<id>`, `endpoints/<id>/active`,
/// `endpoints/<id>/activeendpoint` and `activeendpoint`.
fn is_own_endpoint(rest: &[&str]) -> bool {
    matches!(
        rest,
        ["endpoints" | "activeendpoint"] | ["endpoints", _] | ["endpoints", _, "active" | "activeendpoint"]
    )
}

/// The write a chat service request makes, if the table maps it (`docs/protocol/fence-targets.md`).
fn mapped(request: &Parsed<'_>, path: &[&str]) -> Option<Target> {
    let body = request.body;
    let on = |kind, chat: &str| chat_scope(chat).map(|scope| Target::new(kind, scope));
    match (request.method.as_str(), path) {
        ("POST", ["users", "ME", "conversations", chat, "messages"]) => message_write(body?, chat, Post::New),
        ("PUT", ["users", "ME", "conversations", chat, "messages", _]) => message_write(body?, chat, Post::Edit),
        ("DELETE", ["users", "ME", "conversations", chat, "messages", _]) => on(WriteKind::Delete, chat),
        ("PUT" | "DELETE", ["users", "ME", "conversations", chat, "messages", _, "properties"])
            if request.only_query("name") == Some("emotions") =>
        {
            on(WriteKind::Reaction, chat)
        }
        ("PUT", ["users", "ME", "conversations", channel, "messages", post, "properties"])
            if request.only_query("name") == Some("meeting") =>
        {
            meeting_title(body?, channel, post)
        }
        ("POST", ["threads"]) => Some(Target::new(WriteKind::ChatCreate, Scope::New).with_added(body_members(body?)?)),
        ("PUT", ["threads", chat, "members"]) => {
            Some(on(WriteKind::MemberChange, chat)?.with_added(body_members(body?)?))
        }
        ("PUT", ["threads", chat, "members", member]) if is_member(member) => {
            Some(on(WriteKind::MemberChange, chat)?.with_added([Mri::new(*member)]))
        }
        ("DELETE", ["threads", chat, "members", member]) if is_member(member) => on(WriteKind::MemberChange, chat),
        _ => None,
    }
}

/// `PUT v1/users/ME/conversations/<channel>/messages/<post>/properties?name=meeting` with exactly `{"meeting":
/// {"meetingtitle": <text>}}`: the title a channel's Meet now gives its meeting's post once the service posted it (the
/// pre-join's subject, edited or not, seen 2026-11-03: `docs/protocol/calling.md`, "A channel's meeting"). Everyone on
/// the team sees the post, so it is an [`WriteKind::Edit`] on the channel's [`Scope::Team`], judged by the team's
/// members. A post id that isn't digits, a chat for a channel, or any other key is unreadable.
fn meeting_title(body: &[u8], channel: &str, post: &str) -> Option<Target> {
    if !crate::membership::is_team_thread_id(channel) || post.is_empty() || !post.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let body = strict_json(body)?;
    let object = body.as_object()?;
    let (1, Some(Value::Object(meeting))) = (object.len(), object.get("meeting")) else {
        return None;
    };
    match (meeting.len(), meeting.get("meetingtitle")) {
        (1, Some(Value::String(_))) => Some(Target::new(WriteKind::Edit, Scope::Team { id: channel.to_owned() })),
        _ => None,
    }
}

/// A mapped write's chat: `19:…@unq.gbl.spaces` (1:1) or `19:…@thread.v2` (group and meeting chats), never a
/// channel or team (`@thread.tacv2`, `@thread.skype`) or a reply chain within one (`;messageid=`). Looser than
/// [`is_chat_id`] on purpose: a mapped write is judged by its chat's live member read, which refuses an id it doesn't
/// know; the carve-outs, which skip that read, take only [`is_chat_id`].
fn chat_scope(id: &str) -> Option<Scope> {
    let chat = id.starts_with("19:")
        && (id.ends_with("@unq.gbl.spaces") || id.ends_with("@thread.v2"))
        && !id.contains([';', '/', '?', '#']);
    chat.then(|| Scope::Conversation { id: id.to_owned() })
}

/// A conversation of the account's own view: a chat or a channel (`19:…@thread.*`), never a `48:` stream, a person
/// (`8:…`) or a bot (`28:…`).
fn is_conversation(id: &str) -> bool {
    id.starts_with("19:")
        && (id.ends_with("@unq.gbl.spaces") || id.contains("@thread."))
        && !id.contains([';', '/', '?', '#'])
}

/// Which of the two message writes a request is.
#[derive(Clone, Copy)]
enum Post {
    /// A new message, posted to the chat.
    New,
    /// An edit: the whole message again, as the web client sends it.
    Edit,
}

/// A message post or an edit ([`Post`]) on the chat `chat`, its body read by [`strict_json`]; whoever it mentions is
/// added ([`mentioned`]). A body that isn't a message ([`is_message`]), and one that could reach beyond the chat
/// ([`chat_only`]), are unreadable.
fn message_write(body: &[u8], chat: &str, post: Post) -> Option<Target> {
    let body = strict_json(body)?;
    chat_only(&body)?;
    if !is_message(&body) {
        return None;
    }
    let kind = match post {
        Post::New => WriteKind::Message,
        Post::Edit => WriteKind::Edit,
    };
    Some(Target::new(kind, chat_scope(chat)?).with_added(mentioned(&body, chat)?))
}

/// Whether a body is a message by its `messagetype`: `Text`, `RichText` or `RichText/…`.
fn is_message(body: &Value) -> bool {
    body.get("messagetype")
        .and_then(Value::as_str)
        .is_some_and(|kind| matches!(kind, "Text" | "RichText") || kind.starts_with("RichText/"))
}

/// `Some` for a message body whose fields that could take it beyond the chat are as the captures showed them, or left
/// out: `crossPostChannels` (a post into channels too) an empty list, `properties.onbehalfof` (a message sent for
/// someone else) `null`. Any other value of either, or either key in another case ([`optional`]), is `None`: the
/// mapping's target is the chat alone, and what the service does with those forms wasn't seen.
fn chat_only(body: &Value) -> Option<()> {
    let body = body.as_object()?;
    match optional(body, "crossPostChannels").ok()? {
        None => {}
        Some(Value::Array(channels)) if channels.is_empty() => {}
        Some(_) => return None,
    }
    let Some(properties) = optional(body, "properties").ok()? else {
        return Some(());
    };
    match optional(properties.as_object()?, "onbehalfof").ok()? {
        None | Some(Value::Null) => Some(()),
        Some(_) => None,
    }
}

/// The MRIs a message body mentions: its `properties.mentions`, a JSON list sent as a string, each entry naming one
/// `mri` (`docs/protocol/chat.md`). They are the write's added participants, so the fence judges whoever is mentioned
/// as it judges the chat's members. An entry of `mentionType` `everyone` names the group chat it is posted to (`chat`)
/// as its `mri` and adds nobody ([`mention_participant`]): it reaches the chat's members, whom the fence reads fresh for
/// the write anyway. No `properties`, or no `mentions` in them, is no mention. Anything else is `None`: `properties`
/// that aren't an object, `mentions` that aren't a string holding a JSON list read by [`strict_json`], an entry that
/// isn't an object with a string `mri`, an `everyone` entry naming anything but `chat` or posted anywhere but a group
/// chat, and any of those keys (`mentionType` too) in another case ([`optional`]).
fn mentioned(body: &Value, chat: &str) -> Option<Vec<Mri>> {
    let Some(properties) = optional(body.as_object()?, "properties").ok()? else {
        return Some(Vec::new());
    };
    let Some(mentions) = optional(properties.as_object()?, "mentions").ok()? else {
        return Some(Vec::new());
    };
    let entries = strict_json(mentions.as_str()?.as_bytes())?;
    let mut added = Vec::new();
    for entry in entries.as_array()? {
        match mention_participant(entry.as_object()?, chat)? {
            Mentioned::Person(mri) => added.push(mri),
            Mentioned::EveryoneHere => {}
        }
    }
    Some(added)
}

/// Whom one `properties.mentions` entry names ([`mention_participant`]).
enum Mentioned {
    /// Its `mri`, added to the write's target.
    Person(Mri),
    /// Everyone in the group chat the write is posted to: nobody added.
    EveryoneHere,
}

/// Whom one `properties.mentions` entry names: [`Mentioned::EveryoneHere`] for a mention of everyone in the group chat
/// `chat` it is posted to (`mentionType` [`EVERYONE`], `mri` the chat's id), else its `mri`; `None` for an entry the
/// fence can't read ([`mentioned`]).
fn mention_participant(entry: &Map<String, Value>, chat: &str) -> Option<Mentioned> {
    let mri = optional(entry, "mri").ok()??.as_str()?;
    match optional(entry, "mentionType").ok()? {
        Some(Value::String(kind)) if kind == EVERYONE => {
            (mri == chat && is_group_chat_id(chat)).then_some(Mentioned::EveryoneHere)
        }
        _ => Some(Mentioned::Person(Mri::new(mri))),
    }
}

/// The members a thread body names (`{"members": [{"id": "<mri>", …}, …]}`): all of them, or `None` when the body
/// isn't read by [`strict_json`], or the list is missing, empty, or has a member that isn't an object with a string id.
fn body_members(body: &[u8]) -> Option<Vec<Mri>> {
    let body = strict_json(body)?;
    let members: Vec<Mri> = body
        .get("members")?
        .as_array()?
        .iter()
        .map(|member| member.as_object().and_then(member_id))
        .collect::<Option<_>>()?;
    (!members.is_empty()).then_some(members)
}

/// A member entry's MRI: its string `id`.
fn member_id(member: &Map<String, Value>) -> Option<Mri> {
    member.get("id")?.as_str().map(Mri::new)
}

/// One member named in a path: an MRI, not the `ALL` every-member form.
fn is_member(segment: &str) -> bool {
    segment.contains(':') && !segment.eq_ignore_ascii_case("ALL")
}
