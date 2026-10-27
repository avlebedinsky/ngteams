//! The chat writes as fenced requests (M1 Task 12): each write's target as `docs/protocol/fence-targets.md` maps it, a
//! permit for one chat never covering a write to another, a message's `content` and `properties.mentions` built from
//! one typed rich-text value, the bodies sent key for key as the synthetic fixtures of the captures hold them, and the
//! writes through the fence and the native transport to the mock chat service (`teams-mock`, through the
//! `test-support` mock origins), the fence's live member read taking the native client's own credential.
//!
//! Every id and token here is made up (the mock's, or a placeholder), never a real one.
#![cfg(test)]

use std::collections::BTreeSet;
use std::future::{self, Future};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_fence::{
    Denied, DenyReason, Fence, FenceLog, FetchFailure, LiveMembershipSource, Membership, MembershipFuture,
    MembershipSource, Mri, PartialReason, Roster, Scope, Target, WriteKind, WritePermit,
};
use teams_mock::{
    AB_CHAT, ABC_CHAT, AC_CHAT, AX_CHAT, CHAT_BASE, ChatServiceMock, EventKind, LAB_A, LAB_B, LAB_C, OUTSIDER,
};
use teams_proto::chat::{
    ComposeError, Image, MAX_CONTENT, MAX_DEPTH, Marks, MessageText, Node, Quote, RichText, parse_html,
};
use teams_proto::{
    Author, Authorized, CreateChat, Credentials, DeleteMessage, EditMessage, HttpTransport, Mutating, Original,
    ReadReceipt, RemoveReaction, Response, SendMessage, SetReaction, TargetMismatch, Transport, Typing,
};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/chatsvc");

/// 2026-01-01T00:00:00Z in milliseconds, the day the fixtures' made-up times are on.
const NEW_YEAR_MS: i64 = 1_767_225_600_000;

/// The path of a chat service request on the mock's chat base, `v1` included.
const BASE_PATH: &str = "/api/chatsvc/amer/v1";

fn oid(mri: &str) -> &str {
    mri.trim_start_matches("8:orgid:")
}

/// A fixture with its placeholders filled in with the mock's made-up ids.
fn fixture(name: &str) -> TestResult<Value> {
    let text = std::fs::read_to_string(Path::new(FIXTURES).join(name))?;
    let text = text
        .replace("<region>", "amer")
        .replace("<guid:a>", oid(LAB_A))
        .replace("<guid:b>", oid(LAB_B))
        .replace("<guid:x>", oid(OUTSIDER));
    Ok(serde_json::from_str(&text)?)
}

/// The case of `writes.json` named `name`, its path made a full one under the chat base.
fn writes_case(name: &str) -> TestResult<Value> {
    let writes = fixture("writes.json")?;
    let mut case = writes["cases"]
        .as_array()
        .and_then(|cases| cases.iter().find(|case| case["name"] == name))
        .ok_or("no such case")?
        .clone();
    let path = format!("{BASE_PATH}{}", case["path"].as_str().ok_or("no path")?);
    case["path"] = json!(path);
    Ok(case)
}

fn mri(text: &str) -> Mri {
    Mri::new(text)
}

fn lab_a() -> Author {
    Author::new(mri(LAB_A), "Lab A")
}

fn rich(nodes: Vec<Node>) -> RichText {
    RichText {
        nodes,
        truncated: false,
    }
}

fn run(text: &str) -> Node {
    Node::Text {
        text: text.to_owned(),
        marks: Marks::NONE,
    }
}

fn mention(item_id: &str, name: &str) -> Node {
    Node::Mention {
        item_id: item_id.to_owned(),
        text: name.to_owned(),
    }
}

fn plain(words: &str) -> MessageText {
    MessageText::plain(words).unwrap()
}

/// "Lab B\u{a0}a made-up mention", mentioning b: `mention.json`'s text (a no-break space after the mention, as the web
/// client wrote it).
fn mentioning_b() -> MessageText {
    mentioning(LAB_B, "Lab B")
}

fn mentioning(who: &str, name: &str) -> MessageText {
    MessageText::new(
        rich(vec![Node::Paragraph(vec![
            mention("m", name),
            run("\u{a0}a made-up mention"),
        ])]),
        [("m".to_owned(), mri(who))],
    )
    .unwrap()
}

/// `reply.json`'s text: a's message quoted, and the reply.
fn reply() -> MessageText {
    let quote = Quote {
        message_id: Some("1760000010000".to_owned()),
        sender: Some(LAB_A.to_owned()),
        sender_name: Some("Lab A".to_owned()),
        time: Some("1760000010000".to_owned()),
        body: vec![Node::Paragraph(vec![run("a made-up message, edited")])],
    };
    MessageText::new(
        rich(vec![Node::Quote(quote), Node::Paragraph(vec![run("a made-up reply")])]),
        [],
    )
    .unwrap()
}

fn chat(id: &str) -> Scope {
    Scope::Conversation { id: id.to_owned() }
}

fn on(kind: WriteKind, id: &str) -> Target {
    Target::new(kind, chat(id))
}

/// One chat write of any kind.
enum ChatWrite {
    Send(SendMessage),
    Edit(EditMessage),
    Delete(DeleteMessage),
    React(SetReaction),
    Unreact(RemoveReaction),
    Receipt(ReadReceipt),
    Typing(Typing),
    Create(CreateChat),
}

impl ChatWrite {
    fn target(&self) -> Target {
        match self {
            Self::Send(request) => request.target(),
            Self::Edit(request) => request.target(),
            Self::Delete(request) => request.target(),
            Self::React(request) => request.target(),
            Self::Unreact(request) => request.target(),
            Self::Receipt(request) => request.target(),
            Self::Typing(request) => request.target(),
            Self::Create(request) => request.target(),
        }
    }

    /// Pairs the request with `permit` ([`Authorized::new`]) and drops the pair.
    fn authorize(self, permit: WritePermit) -> Result<(), TargetMismatch> {
        match self {
            Self::Send(request) => Authorized::new(request, permit).map(drop),
            Self::Edit(request) => Authorized::new(request, permit).map(drop),
            Self::Delete(request) => Authorized::new(request, permit).map(drop),
            Self::React(request) => Authorized::new(request, permit).map(drop),
            Self::Unreact(request) => Authorized::new(request, permit).map(drop),
            Self::Receipt(request) => Authorized::new(request, permit).map(drop),
            Self::Typing(request) => Authorized::new(request, permit).map(drop),
            Self::Create(request) => Authorized::new(request, permit).map(drop),
        }
    }

    /// Delivers the request under `permit`, which must be its target's.
    async fn deliver<T: Transport>(self, transport: &T, permit: WritePermit) -> Result<Response, T::Error> {
        match self {
            Self::Send(request) => transport.deliver(Authorized::new(request, permit).unwrap()).await,
            Self::Edit(request) => transport.deliver(Authorized::new(request, permit).unwrap()).await,
            Self::Delete(request) => transport.deliver(Authorized::new(request, permit).unwrap()).await,
            Self::React(request) => transport.deliver(Authorized::new(request, permit).unwrap()).await,
            Self::Unreact(request) => transport.deliver(Authorized::new(request, permit).unwrap()).await,
            Self::Receipt(request) => transport.deliver(Authorized::new(request, permit).unwrap()).await,
            Self::Typing(request) => transport.deliver(Authorized::new(request, permit).unwrap()).await,
            Self::Create(request) => transport.deliver(Authorized::new(request, permit).unwrap()).await,
        }
    }
}

fn original() -> Original {
    Original {
        client_message_id: "1111111111111111111".to_owned(),
        compose_time_ms: NEW_YEAR_MS + 10_000,
        arrival_time_ms: NEW_YEAR_MS + 10_000,
    }
}

/// One write of each kind on chat `id` (message `message` for those on a message), each with the target the fence
/// mapping gives it: the message and the edit mention b.
fn every_write(id: &str, message: &str) -> Vec<(Target, ChatWrite)> {
    vec![
        (
            on(WriteKind::Message, id).with_added([mri(LAB_B)]),
            ChatWrite::Send(SendMessage::new(id, lab_a(), mentioning_b())),
        ),
        (on(WriteKind::Typing, id), ChatWrite::Typing(Typing::new(id))),
        (
            on(WriteKind::Edit, id).with_added([mri(LAB_B)]),
            ChatWrite::Edit(EditMessage::new(id, message, lab_a(), mentioning_b(), original())),
        ),
        (
            on(WriteKind::Reaction, id),
            ChatWrite::React(SetReaction::new(id, message, "like")),
        ),
        (
            on(WriteKind::Reaction, id),
            ChatWrite::Unreact(RemoveReaction::new(id, message, "like")),
        ),
        (
            on(WriteKind::ReadReceipt, id),
            ChatWrite::Receipt(ReadReceipt::new(id, message, "1111111111111111111")),
        ),
        (
            on(WriteKind::Delete, id),
            ChatWrite::Delete(DeleteMessage::new(id, message)),
        ),
    ]
}

// --- targets ---

#[test]
fn each_chat_write_targets_its_chat_as_the_fence_maps_it() {
    for (expected, write) in every_write(AB_CHAT, "1760000010000") {
        assert_eq!(write.target(), expected);
    }
    assert_eq!(
        SendMessage::new(AB_CHAT, lab_a(), plain("hello")).target(),
        on(WriteKind::Message, AB_CHAT),
        "a message without a mention adds nobody"
    );
    assert_eq!(
        SendMessage::new(AB_CHAT, lab_a(), reply()).target(),
        on(WriteKind::Message, AB_CHAT),
        "a quote adds nobody, as the mapping of a quoted reply"
    );
}

#[test]
fn a_new_chat_targets_a_new_scope_with_every_member() {
    let one_to_one = CreateChat::one_to_one(mri(LAB_A), mri(LAB_B)).unwrap();
    assert_eq!(
        one_to_one.target(),
        Target::new(WriteKind::ChatCreate, Scope::New).with_added([mri(LAB_A), mri(LAB_B)])
    );
    let group = CreateChat::group(mri(LAB_A), [mri(LAB_B), mri(LAB_C)]).unwrap();
    assert_eq!(
        group.target(),
        Target::new(WriteKind::ChatCreate, Scope::New).with_added([mri(LAB_A), mri(LAB_B), mri(LAB_C)])
    );
    assert_eq!(group.members(), [mri(LAB_A), mri(LAB_B), mri(LAB_C)]);

    assert!(CreateChat::one_to_one(mri(LAB_A), mri(LAB_A)).is_none());
    assert!(
        CreateChat::group(mri(LAB_A), [mri(LAB_B)]).is_none(),
        "a group has three"
    );
    assert!(CreateChat::group(mri(LAB_A), [mri(LAB_B), mri(LAB_A)]).is_none());
    assert!(CreateChat::group(mri(LAB_A), [mri(LAB_B), mri(LAB_B), mri(LAB_C)]).is_none());
}

/// Answers every fetch with lab accounts only, so every check on an existing scope passes.
struct LabOnly;

impl MembershipSource for LabOnly {
    fn members<'a>(&'a self, _scope: &'a Scope) -> MembershipFuture<'a> {
        Box::pin(async { Membership::Complete(vec![mri(LAB_A), mri(LAB_B)]) })
    }
}

fn lab_fence(log: impl Write + Send + 'static) -> Arc<Fence> {
    Arc::new(Fence::new(
        Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), []),
        FenceLog::from_writer(log),
    ))
}

#[tokio::test]
async fn a_permit_for_one_chat_never_covers_a_write_to_another() {
    let fence = lab_fence(io::sink());
    let elsewhere = every_write(AC_CHAT, "1760000010000");
    for ((target, _), (_, other)) in every_write(AB_CHAT, "1760000010000").into_iter().zip(elsewhere) {
        let permit = fence.check(target.clone(), &LabOnly).await.unwrap();
        let mismatch = other.authorize(permit).unwrap_err();
        assert_eq!(mismatch.permitted(), &target);
        assert_eq!(mismatch.requested().scope(), &chat(AC_CHAT));
    }
    let one_to_one = CreateChat::one_to_one(mri(LAB_A), mri(LAB_B)).unwrap();
    let permit = fence.check(one_to_one.target(), &LabOnly).await.unwrap();
    let group = CreateChat::group(mri(LAB_A), [mri(LAB_B), mri(LAB_C)]).unwrap();
    assert!(
        ChatWrite::Create(group).authorize(permit).is_err(),
        "other members, another target"
    );
}

#[tokio::test]
async fn a_permit_for_one_kind_or_mention_never_covers_another() {
    let fence = lab_fence(io::sink());
    let typing = fence.check(on(WriteKind::Typing, AB_CHAT), &LabOnly).await.unwrap();
    assert!(
        ChatWrite::Send(SendMessage::new(AB_CHAT, lab_a(), plain("hi")))
            .authorize(typing)
            .is_err()
    );

    let plain_message = fence.check(on(WriteKind::Message, AB_CHAT), &LabOnly).await.unwrap();
    let error = ChatWrite::Send(SendMessage::new(AB_CHAT, lab_a(), mentioning_b()))
        .authorize(plain_message)
        .unwrap_err();
    assert_eq!(error.requested().added(), &BTreeSet::from([mri(LAB_B)]));

    let mentioning_c = fence
        .check(on(WriteKind::Edit, AB_CHAT).with_added([mri(LAB_C)]), &LabOnly)
        .await
        .unwrap();
    let edit = EditMessage::new(AB_CHAT, "1", lab_a(), mentioning_b(), original());
    assert!(ChatWrite::Edit(edit).authorize(mentioning_c).is_err());
}

// --- one rich-text value ---

#[test]
fn a_mention_is_in_the_content_and_the_target_from_one_value() {
    let text = mentioning_b();
    assert_eq!(
        text.html(),
        "<p><readonly class=\"skipProofing\" itemtype=\"http://schema.skype.com/Mention\" spellcheck=\"false\"><span \
         itemtype=\"http://schema.skype.com/Mention\" itemscope itemid=\"0\">Lab B</span></readonly>&nbsp;a made-up \
         mention</p>"
    );
    assert_eq!(text.participants(), &BTreeSet::from([mri(LAB_B)]));
    assert_eq!(text.mentioned().get("m"), Some(&mri(LAB_B)));
}

#[test]
fn mentions_are_numbered_as_they_appear() {
    let text = MessageText::new(
        rich(vec![Node::Paragraph(vec![
            mention("second", "C"),
            run(" and "),
            mention("first", "B"),
        ])]),
        [("first".to_owned(), mri(LAB_B)), ("second".to_owned(), mri(LAB_C))],
    )
    .unwrap();
    let html = text.html();
    assert!(html.contains("itemid=\"0\">C</span>"), "{html}");
    assert!(html.contains("itemid=\"1\">B</span>"), "{html}");
    assert_eq!(text.participants(), &BTreeSet::from([mri(LAB_B), mri(LAB_C)]));
}

#[test]
fn a_mention_must_name_someone_and_be_named_once() {
    let one = |nodes: Vec<Node>, mentioned: Vec<(&str, &str)>| {
        MessageText::new(
            rich(vec![Node::Paragraph(nodes)]),
            mentioned
                .into_iter()
                .map(|(item_id, who)| (item_id.to_owned(), mri(who))),
        )
    };
    assert_eq!(
        one(vec![mention("0", "B")], vec![]),
        Err(ComposeError::UnknownMention("0".to_owned()))
    );
    assert_eq!(
        one(vec![run("nobody")], vec![("0", LAB_B)]),
        Err(ComposeError::UnusedMention("0".to_owned())),
        "a mentioned MRI the text doesn't show would be in the target only"
    );
    assert_eq!(
        one(vec![mention("0", "B"), mention("0", "B")], vec![("0", LAB_B)]),
        Err(ComposeError::RepeatedMention("0".to_owned()))
    );
    assert_eq!(
        one(vec![mention("0", "B")], vec![("0", LAB_B), ("0", LAB_C)]),
        Err(ComposeError::RepeatedMention("0".to_owned()))
    );
    for who in [
        "28:bot-service",
        "8:orgid:",
        "8:orgid:not hex",
        LAB_B.trim_start_matches("8:orgid:"),
    ] {
        assert_eq!(
            one(vec![mention("0", "B")], vec![("0", who)]),
            Err(ComposeError::NotAPerson(mri(who))),
            "{who}"
        );
    }
}

#[test]
fn what_isnt_a_messages_text_is_refused() {
    let image = Node::Image(Image {
        src: "https://example.com/image.png".to_owned(),
        alt: None,
        item_type: None,
        ..Image::default()
    });
    assert_eq!(MessageText::new(rich(vec![image]), []), Err(ComposeError::Image));
    for href in ["javascript:alert(1)", "data:text/html,hi", "not a url"] {
        let link = Node::Link {
            href: href.to_owned(),
            children: vec![run("x")],
        };
        assert_eq!(
            MessageText::new(rich(vec![link]), []),
            Err(ComposeError::LinkScheme),
            "{href}"
        );
    }
    let incomplete = Quote {
        message_id: Some("1760000010000".to_owned()),
        sender: Some(LAB_A.to_owned()),
        ..Quote::default()
    };
    assert_eq!(
        MessageText::new(rich(vec![Node::Quote(incomplete)]), []),
        Err(ComposeError::Quote)
    );
    let mut truncated = rich(vec![run("cut")]);
    truncated.truncated = true;
    assert_eq!(MessageText::new(truncated, []), Err(ComposeError::Truncated));

    let mut deep = run("deep");
    for _ in 0..=MAX_DEPTH {
        deep = Node::Paragraph(vec![deep]);
    }
    assert_eq!(MessageText::new(rich(vec![deep]), []), Err(ComposeError::TooDeep));
    assert_eq!(MessageText::plain(&"x".repeat(MAX_CONTENT)), Err(ComposeError::TooLong));
}

/// A reply quoting a message whose preview is `levels` paragraphs deep.
fn reply_previewing(levels: usize) -> Result<MessageText, ComposeError> {
    let mut preview = run("deep");
    for _ in 0..levels {
        preview = Node::Paragraph(vec![preview]);
    }
    let quote = Quote {
        message_id: Some("1760000010000".to_owned()),
        sender: Some(LAB_A.to_owned()),
        sender_name: Some("Lab A".to_owned()),
        time: Some("1760000010000".to_owned()),
        body: vec![preview],
    };
    MessageText::new(rich(vec![Node::Quote(quote)]), [])
}

#[test]
fn a_replys_preview_is_held_to_the_same_depth_as_the_rest() {
    // the preview is sent as its plain text, but kept as given: too deep a preview could be stored and not read back
    for levels in [MAX_DEPTH, 128] {
        assert_eq!(reply_previewing(levels), Err(ComposeError::TooDeep), "{levels} deep");
    }
    let deepest = reply_previewing(MAX_DEPTH - 1).unwrap();
    let stored = serde_json::to_string(&deepest).unwrap();
    assert_eq!(serde_json::from_str::<MessageText>(&stored).unwrap(), deepest);
}

#[test]
fn text_is_escaped_so_no_markup_comes_from_it() {
    let text = plain("<script>alert(1)</script> & \"<b>\"");
    assert_eq!(
        text.html(),
        "<p>&lt;script&gt;alert(1)&lt;/script&gt; &amp; &quot;&lt;b&gt;&quot;</p>"
    );
    let name = MessageText::new(
        rich(vec![mention("0", "<img src=x onerror=alert(1)>")]),
        [("0".to_owned(), mri(LAB_B))],
    )
    .unwrap();
    assert!(!name.html().contains("<img"), "{}", name.html());
}

#[test]
fn what_is_sent_reads_back_as_the_same_rich_text() {
    let nodes = vec![
        Node::Paragraph(vec![
            Node::Text {
                text: "bold".to_owned(),
                marks: Marks::BOLD,
            },
            Node::Text {
                text: " and code".to_owned(),
                marks: Marks::ITALIC.with(Marks::CODE),
            },
            Node::Break,
            mention("0", "Lab B"),
            run(" see "),
            Node::Link {
                href: "https://example.com/a?b=c&d=e".to_owned(),
                children: vec![run("this")],
            },
        ]),
        Node::Quote(Quote {
            body: vec![Node::Paragraph(vec![run("a plain quote")])],
            ..Quote::default()
        }),
    ];
    let text = MessageText::new(rich(nodes.clone()), [("0".to_owned(), mri(LAB_B))]).unwrap();
    assert_eq!(parse_html(text.html()), rich(nodes));

    let reply = reply();
    assert_eq!(&parse_html(reply.html()), reply.rich_text());
}

#[test]
fn a_stored_text_is_checked_again_when_read() {
    let text = mentioning_b();
    let stored = serde_json::to_string(&text).unwrap();
    assert_eq!(serde_json::from_str::<MessageText>(&stored).unwrap(), text);

    let mut tampered: Value = serde_json::from_str(&stored).unwrap();
    tampered["mentioned"] = json!({});
    assert!(
        serde_json::from_value::<MessageText>(tampered).is_err(),
        "a mention naming nobody"
    );
}

// --- the bodies sent, as captured ---

/// The token the tests hand the transport, renewed when they say so; `None` is signed out. A refused token is
/// renewed to the one set with [`Token::renew_when_refused`], if any (else the same token comes back), and the
/// refusals it was told of are counted.
#[derive(Default)]
struct Token {
    current: Mutex<Option<String>>,
    when_refused: Mutex<Option<String>>,
    refused: Mutex<Vec<String>>,
}

impl Token {
    fn new(token: Option<String>) -> Self {
        Self {
            current: Mutex::new(token),
            ..Self::default()
        }
    }

    fn renew(&self, token: &str) {
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Some(token.to_owned());
    }

    fn renew_when_refused(&self, token: &str) {
        *self.when_refused.lock().unwrap_or_else(PoisonError::into_inner) = Some(token.to_owned());
    }

    /// The tokens it was told were refused, in order.
    fn refused(&self) -> Vec<String> {
        self.refused.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl Credentials for Token {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        let token = self.current.lock().unwrap_or_else(PoisonError::into_inner).clone();
        future::ready(token.map(Secret::new).ok_or(TokenError::NotSignedIn {
            account: "lab-a".to_owned(),
        }))
    }

    fn bearer_rejected(
        &self,
        audience: &str,
        rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.refused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(rejected.expose().to_owned());
        if let Some(renewed) = self.when_refused.lock().unwrap_or_else(PoisonError::into_inner).take() {
            self.renew(&renewed);
        }
        self.bearer(audience)
    }
}

/// What the transport sent: the method, the decoded path, the query and the body.
#[derive(Debug, PartialEq)]
struct Sent {
    method: String,
    path: String,
    query: Option<String>,
    body: Option<Value>,
}

/// Sends `write` through the native transport under a permit from a fence over the lab, to a server that answers
/// everything with 200, and returns what reached it.
async fn sent(write: ChatWrite) -> TestResult<Sent> {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let fence = lab_fence(io::sink());
    let token = Arc::new(Token::new(Some("a-made-up-token-for-the-transport".to_owned())));
    let transport = HttpTransport::with_mock_origin(token, Arc::clone(&fence), CHAT_BASE, &server.uri())?;
    let permit = fence.check(write.target(), &LabOnly).await?;
    write.deliver(&transport, permit).await?;
    let requests = server.received_requests().await.unwrap_or_default();
    let [request] = requests.as_slice() else {
        return Err(format!("{} requests", requests.len()).into());
    };
    let body = (!request.body.is_empty())
        .then(|| serde_json::from_slice(&request.body))
        .transpose()?;
    Ok(Sent {
        method: request.method.as_str().to_owned(),
        path: percent_decode_str(request.url.path()).decode_utf8()?.into_owned(),
        query: request.url.query().map(str::to_owned),
        body,
    })
}

/// A recorded request (a fixture's `request`, or a `writes.json` case), as [`Sent`].
fn recorded(request: &Value) -> TestResult<Sent> {
    let path = request["path"].as_str().ok_or("no path")?;
    let (path, query) = match path.split_once('?') {
        Some((path, query)) => (path, Some(query.to_owned())),
        None => (path, request["query"].as_str().map(str::to_owned)),
    };
    Ok(Sent {
        method: request["method"].as_str().ok_or("no method")?.to_owned(),
        path: percent_decode_str(path).decode_utf8()?.into_owned(),
        query,
        body: Some(request["body"].clone()).filter(|body| !body.is_null()),
    })
}

/// A message body with its JSON-string properties (`mentions`, `qtdMsgs`) read, so their entries compare key for key.
fn read_listed(mut sent: Sent) -> TestResult<Sent> {
    if let Some(properties) = sent
        .body
        .as_mut()
        .and_then(|body| body.get_mut("properties"))
        .and_then(Value::as_object_mut)
    {
        for key in ["mentions", "qtdMsgs"] {
            if let Some(value) = properties.get_mut(key) {
                *value = serde_json::from_str(value.as_str().ok_or("not a string")?)?;
            }
        }
    }
    Ok(sent)
}

#[tokio::test]
async fn a_message_with_a_mention_is_sent_as_the_mention_fixture() -> TestResult {
    let request = SendMessage::new(AB_CHAT, lab_a(), mentioning_b())
        .composed_at(NEW_YEAR_MS + 40_000)
        .with_client_message_id("6666666666666666666");

    let sent = read_listed(sent(ChatWrite::Send(request)).await?)?;

    assert_eq!(sent, read_listed(recorded(&fixture("mention.json")?["request"])?)?);
    Ok(())
}

#[tokio::test]
async fn a_quoted_reply_is_sent_as_the_reply_fixture() -> TestResult {
    let request = SendMessage::new(AB_CHAT, lab_a(), reply())
        .composed_at(NEW_YEAR_MS + 49_500)
        .with_client_message_id("4444444444444444444");

    let sent = read_listed(sent(ChatWrite::Send(request)).await?)?;

    assert_eq!(sent, read_listed(recorded(&fixture("reply.json")?["request"])?)?);
    Ok(())
}

#[tokio::test]
async fn a_plain_message_is_sent_as_the_captured_one() -> TestResult {
    let request = SendMessage::new(AB_CHAT, lab_a(), plain("made-up text"))
        .composed_at(NEW_YEAR_MS)
        .with_client_message_id("1111111111111111111");

    let sent = read_listed(sent(ChatWrite::Send(request)).await?)?;

    let case = writes_case("a message as the web client sends it, no mention (shape of ab-chat-full, 2026-10-20)")?;
    assert_eq!(sent, read_listed(recorded(&case)?)?);
    Ok(())
}

#[tokio::test]
async fn an_edit_is_sent_as_the_edit_fixture() -> TestResult {
    let new_text = MessageText::new(rich(vec![Node::Paragraph(vec![run("a made-up message, edited")])]), [])?;
    let edited_at = 1_760_000_020_000;
    let request = EditMessage::new(AB_CHAT, "1760000010000", lab_a(), new_text, original()).edited_at(edited_at);

    let sent = read_listed(sent(ChatWrite::Edit(request)).await?)?;

    let mut expected = read_listed(recorded(&fixture("edit.json")?["request"])?)?;
    // The fixture's made-up `version` and `edittime` aren't one instant; an edit sends one time as both.
    let body = expected.body.as_mut().ok_or("no body")?;
    body["version"] = json!("2025-10-09T08:53:40.000Z");
    body["properties"]["edittime"] = json!(edited_at);
    assert_eq!(sent, expected);
    Ok(())
}

#[tokio::test]
async fn a_delete_is_sent_as_the_delete_fixture() -> TestResult {
    let sent = sent(ChatWrite::Delete(DeleteMessage::new(AB_CHAT, "1760000060000"))).await?;

    assert_eq!(sent, recorded(&fixture("delete.json")?["request"])?);
    Ok(())
}

#[tokio::test]
async fn a_reaction_and_taking_it_back_are_sent_as_the_reaction_fixture() -> TestResult {
    let reaction = fixture("reaction.json")?;
    let add = SetReaction::new(AB_CHAT, "1760000010000", "like").at(1_760_000_025_000);
    let remove = RemoveReaction::new(AB_CHAT, "1760000010000", "like");

    assert_eq!(
        sent(ChatWrite::React(add)).await?,
        recorded(&reaction["add"]["request"])?
    );
    assert_eq!(
        sent(ChatWrite::Unreact(remove)).await?,
        recorded(&reaction["remove"]["request"])?
    );
    Ok(())
}

#[tokio::test]
async fn a_read_receipt_typing_and_a_new_chat_are_sent_as_captured() -> TestResult {
    let receipt = ReadReceipt::new(AB_CHAT, "1760000000000", "1").at(1_760_000_000_000);
    assert_eq!(receipt.horizon(), "1760000000000;1760000000000;1");
    assert_eq!(
        sent(ChatWrite::Receipt(receipt)).await?,
        recorded(&writes_case("a read receipt")?)?
    );
    assert_eq!(
        sent(ChatWrite::Typing(Typing::new(AB_CHAT))).await?,
        recorded(&writes_case("typing")?)?
    );
    let create = CreateChat::one_to_one(mri(LAB_A), mri(LAB_B)).ok_or("one chat")?;
    assert_eq!(
        sent(ChatWrite::Create(create)).await?,
        recorded(&writes_case("a new chat")?)?
    );
    Ok(())
}

#[tokio::test]
async fn a_new_group_chat_is_sent_as_the_web_client_created_one() -> TestResult {
    let create = CreateChat::group(mri(LAB_A), [mri(LAB_B), mri(LAB_C)]).ok_or("a group")?;

    let sent = sent(ChatWrite::Create(create)).await?;

    // `docs/protocol/fence-targets.md`: the group's create has no `fixedRoster` and no `uniquerosterthread`
    let members: Vec<Value> = [LAB_A, LAB_B, LAB_C]
        .iter()
        .map(|id| json!({"id": id, "role": "Admin"}))
        .collect();
    assert_eq!(
        sent,
        Sent {
            method: "POST".to_owned(),
            path: format!("{BASE_PATH}/threads"),
            query: None,
            body: Some(json!({"members": members, "properties": {"threadType": "chat"}})),
        }
    );
    Ok(())
}

// --- on the mock chat service, through the fence ---

/// A shared `fence.log`, its lines read back.
#[derive(Clone, Default)]
struct SharedLog(Arc<Mutex<Vec<u8>>>);

impl SharedLog {
    fn lines(&self) -> Vec<Value> {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn verdicts(&self) -> Vec<String> {
        self.lines()
            .iter()
            .map(|line| line["verdict"].as_str().unwrap_or_default().to_owned())
            .collect()
    }
}

impl Write for SharedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The mock chat service, a's native transport to it, and a fence over the lab whose live member read takes a's
/// credential from that transport.
struct Lab {
    mock: ChatServiceMock,
    fence: Arc<Fence>,
    log: SharedLog,
    source: LiveMembershipSource,
    token: Arc<Token>,
    transport: Arc<HttpTransport<Token>>,
}

impl Lab {
    /// The lab with a's token, the member read asking the transport for its credential.
    async fn start() -> TestResult<Self> {
        let mock = ChatServiceMock::start().await?;
        let token = mock.token(LAB_A).ok_or("no a")?;
        let lab = Self::without_credential(mock, Some(token))?;
        lab.source.set_credential_source(lab.transport.clone());
        Ok(lab)
    }

    /// A lab whose member read has no credential yet, and whose transport has `token`.
    fn without_credential(mock: ChatServiceMock, token: Option<String>) -> TestResult<Self> {
        let log = SharedLog::default();
        let fence = lab_fence(log.clone());
        let token = Arc::new(Token::new(token));
        let transport =
            HttpTransport::with_mock_origin(Arc::clone(&token), Arc::clone(&fence), CHAT_BASE, &mock.origin())?;
        let source = LiveMembershipSource::with_mock_origin(&mock.origin())?;
        Ok(Self {
            mock,
            fence,
            log,
            source,
            token,
            transport: Arc::new(transport),
        })
    }

    /// The fence's decision on `write`, with a fresh member read, and the answer to it if it was allowed.
    async fn write(&self, write: ChatWrite) -> Result<Response, Denied> {
        let permit = self.fence.check(write.target(), &self.source).await?;
        Ok(write.deliver(&*self.transport, permit).await.unwrap())
    }

    /// How many member reads (`GET`s) reached the mock.
    async fn reads_received(&self) -> usize {
        self.mock
            .received_requests()
            .await
            .iter()
            .filter(|request| request.method.as_str() == "GET")
            .count()
    }

    /// How many writes (anything but a `GET`) reached the mock.
    async fn writes_received(&self) -> usize {
        self.mock
            .received_requests()
            .await
            .iter()
            .filter(|request| request.method.as_str() != "GET")
            .count()
    }
}

fn arrival(answer: &Response) -> TestResult<String> {
    Ok(serde_json::from_slice::<Value>(answer.body())?["OriginalArrivalTime"].to_string())
}

#[tokio::test]
async fn each_write_to_a_lab_chat_is_allowed_and_sent_once() -> TestResult {
    let lab = Lab::start().await?;
    let posted = lab
        .write(ChatWrite::Send(SendMessage::new(AB_CHAT, lab_a(), mentioning_b())))
        .await?;
    assert_eq!(posted.status(), 201);
    assert_eq!(lab.writes_received().await, 1);
    let id = arrival(&posted)?;

    let mut sent = 1;
    for (_, write) in every_write(AB_CHAT, &id).into_iter().skip(1) {
        let answer = lab.write(write).await?;
        sent += 1;
        assert!(answer.is_success(), "{answer:?}");
        assert_eq!(lab.writes_received().await, sent, "each write sent once");
    }
    let create = CreateChat::one_to_one(mri(LAB_A), mri(LAB_B)).ok_or("one chat")?;
    assert_eq!(lab.write(ChatWrite::Create(create)).await?.status(), 201);
    assert_eq!(lab.writes_received().await, sent + 1);

    assert_eq!(lab.log.verdicts(), vec!["allow"; sent + 1], "every decision logged");
    let told: Vec<EventKind> = lab.mock.take_events(LAB_B).iter().map(|event| event.kind).collect();
    assert_eq!(
        told,
        [
            EventKind::Message,
            EventKind::Typing,
            EventKind::Edit,
            EventKind::Reaction,
            EventKind::Reaction,
            EventKind::ReadReceipt,
            EventKind::Delete,
        ],
        "b was told each write, and of no new chat: theirs with a exists"
    );
    Ok(())
}

fn outsiders(denied: &Denied) -> bool {
    matches!(denied.reason(), DenyReason::Outsiders { .. })
}

#[tokio::test]
async fn a_write_to_a_chat_with_an_outsider_is_denied_logged_and_never_sent() -> TestResult {
    let lab = Lab::start().await?;
    let mut denials = 0;
    let mut signals = 0;
    for (target, write) in every_write(AX_CHAT, "1760000010000") {
        if target.kind().is_contacting() {
            let denied = lab.write(write).await.unwrap_err();
            assert!(outsiders(&denied), "{denied:?}");
            denials += 1;
        } else {
            // typing and a read receipt reach nobody: they go out without a member read
            let answer = lab.write(write).await?;
            assert!(answer.is_success(), "{target:?}: {answer:?}");
            signals += 1;
        }
    }
    assert_eq!(signals, 2);
    assert_eq!(
        lab.reads_received().await,
        denials,
        "a member read for each contacting write alone"
    );
    let to_outsider = SendMessage::new(AB_CHAT, lab_a(), mentioning(OUTSIDER, "Someone"));
    let denied = lab.write(ChatWrite::Send(to_outsider)).await.unwrap_err();
    assert!(outsiders(&denied), "a mention of an outsider: {denied:?}");
    let create = CreateChat::one_to_one(mri(LAB_A), mri(OUTSIDER)).ok_or("one chat")?;
    let denied = lab.write(ChatWrite::Create(create)).await.unwrap_err();
    assert!(outsiders(&denied), "{denied:?}");
    denials += 2;

    assert_eq!(lab.writes_received().await, signals, "no contacting write was sent");
    let lines = lab.log.lines();
    assert_eq!(lines.len(), denials + signals);
    let denied: Vec<_> = lines.iter().filter(|line| line["verdict"] == "deny").collect();
    assert_eq!(denied.len(), denials);
    for line in denied {
        assert!(line["reason"].get("outsiders").is_some(), "{line}");
    }
    let told: Vec<EventKind> = lab.mock.take_events(OUTSIDER).iter().map(|event| event.kind).collect();
    assert!(
        told.iter()
            .all(|kind| matches!(kind, EventKind::Typing | EventKind::ReadReceipt)),
        "nothing but typing and a read receipt reached the outsider: {told:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_lab_only_group_chat_is_written_to_a_new_one_included() -> TestResult {
    let lab = Lab::start().await?;
    for write in [
        ChatWrite::Send(SendMessage::new(ABC_CHAT, lab_a(), plain("hello"))),
        ChatWrite::Typing(Typing::new(ABC_CHAT)),
    ] {
        assert_eq!(lab.write(write).await?.status(), 201);
    }
    assert_eq!(lab.writes_received().await, 2);
    assert_eq!(lab.reads_received().await, 1, "the message's member read alone");

    // a new group of lab accounts is judged by its members alone …
    let create = CreateChat::group(mri(LAB_A), [mri(LAB_B), mri(LAB_C)]).ok_or("a group")?;
    let created = lab.write(ChatWrite::Create(create)).await?;
    assert_eq!(created.status(), 201);
    let group = serde_json::from_slice::<Value>(created.body())?["threadResource"]["id"]
        .as_str()
        .ok_or("no id")?
        .to_owned();
    // … and written to like any chat, by its fresh member read
    let posted = lab
        .write(ChatWrite::Send(SendMessage::new(&group, lab_a(), plain("hello"))))
        .await?;
    assert_eq!(posted.status(), 201);
    assert_eq!(lab.writes_received().await, 4);
    assert_eq!(lab.log.verdicts(), ["allow"; 4]);
    Ok(())
}

#[tokio::test]
async fn without_the_native_clients_credential_every_contacting_write_is_partial() -> TestResult {
    let lab = Lab::without_credential(ChatServiceMock::start().await?, None)?;
    for (target, write) in every_write(AB_CHAT, "1760000010000") {
        let verdict = lab.fence.check(write.target(), &lab.source).await;
        if target.kind().is_contacting() {
            let denied = verdict.unwrap_err();
            assert_eq!(denied.reason(), &DenyReason::Partial(PartialReason::NoCredential));
        } else {
            // typing and a read receipt need no member read, so no credential either
            assert!(verdict.is_ok(), "{target:?}: {verdict:?}");
        }
    }

    // a source that has no token (signed out) gives no credential either
    lab.source.set_credential_source(lab.transport.clone());
    let denied = lab
        .write(ChatWrite::Send(SendMessage::new(AB_CHAT, lab_a(), plain("hello"))))
        .await
        .unwrap_err();
    assert_eq!(denied.reason(), &DenyReason::Partial(PartialReason::NoCredential));

    assert!(lab.mock.received_requests().await.is_empty(), "not even a member read");
    Ok(())
}

#[tokio::test]
async fn the_member_read_takes_the_transports_token_as_it_is_now() -> TestResult {
    let lab = Lab::start().await?;
    let post = || ChatWrite::Send(SendMessage::new(AB_CHAT, lab_a(), plain("hello")));
    assert_eq!(lab.write(post()).await?.status(), 201);

    // the service takes only a new token from now on: the transport still has the old one, and its session gives the
    // same one again, so the read is refused twice and the write denied …
    let renewed = "a-renewed-made-up-token-for-lab-a";
    assert!(lab.mock.set_token(LAB_A, renewed));
    let denied = lab.write(post()).await.unwrap_err();
    assert_eq!(
        denied.reason(),
        &DenyReason::Partial(PartialReason::FetchFailed(FetchFailure::Status(401)))
    );
    // … until the session renews it: the next read asks the transport again and takes the new one
    lab.token.renew(renewed);
    assert_eq!(lab.write(post()).await?.status(), 201);

    assert_eq!(lab.writes_received().await, 2);
    assert_eq!(lab.log.verdicts(), ["allow", "deny", "allow"]);
    Ok(())
}

#[tokio::test]
async fn a_refused_member_read_renews_the_token_and_reads_once_more() -> TestResult {
    let lab = Lab::start().await?;
    let first = lab.mock.token(LAB_A).ok_or("no a")?;
    let post = || ChatWrite::Send(SendMessage::new(AB_CHAT, lab_a(), plain("hello")));

    // the service refuses a's token before it expires (revoked): the read tells the session, which renews it, and the
    // read sent once more with the new token lets the write through
    let renewed = "a-renewed-made-up-token-for-lab-a";
    assert!(lab.mock.set_token(LAB_A, renewed));
    lab.token.renew_when_refused(renewed);
    assert_eq!(lab.write(post()).await?.status(), 201);
    assert_eq!(
        lab.token.refused(),
        std::slice::from_ref(&first),
        "the refused token, told once"
    );
    assert_eq!(lab.reads_received().await, 2, "the refused read and one more");

    // refused again, and the renewal is refused too: no third read, the write denied and never sent
    let revoked = "a-revoked-made-up-token-for-lab-a";
    lab.token.renew_when_refused(revoked);
    assert!(lab.mock.set_token(LAB_A, "a-later-made-up-token-for-lab-a"));
    let denied = lab.write(post()).await.unwrap_err();
    assert_eq!(
        denied.reason(),
        &DenyReason::Partial(PartialReason::FetchFailed(FetchFailure::Status(401)))
    );
    assert_eq!(lab.token.refused(), [first, renewed.to_owned()]);
    assert_eq!(lab.reads_received().await, 4, "two reads for the denied check, no more");

    assert_eq!(lab.writes_received().await, 1);
    assert_eq!(lab.log.verdicts(), ["allow", "deny"]);
    Ok(())
}
