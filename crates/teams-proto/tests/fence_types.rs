//! Type-level enforcement: a mutating call can't be made without a `WritePermit`, a permit can't be made outside
//! `teams-fence`, and a permit only covers the target it was issued for.
//!
//! MRIs and conversation ids here are placeholders, never real ones.

use std::future::{self, Future};
use std::io;
use std::sync::{Mutex, PoisonError};

use teams_fence::{
    Denied, Fence, FenceLog, Membership, MembershipFuture, MembershipSource, Mri, Roster, Scope, Target, WriteKind,
    WritePermit,
};
use teams_proto::chat::MessageText;
use teams_proto::{
    AddToCall, Author, Authorized, Client, ConversationUrl, Mutating, Read, Request, Response, SendMessage, StartCall,
    Transport, WriteError,
};

const LAB_A: &str = "8:orgid:lab-a";
const LAB_B: &str = "8:orgid:lab-b";
const CHAT_X: &str = "19:lab-chat-x@thread.v2";
const CHAT_Y: &str = "19:lab-chat-y@thread.v2";
const ECHO: &str = "28:echo-service";
const OUTSIDER: &str = "8:orgid:outsider";
/// Placeholder conversation controllers: calls X and Y on one conversation host, and a call on another host whose
/// conversation id is X's.
const CALL_X: &str = "https://proxy.example.com/api/v2/ep/conv-1.example.com/conv/CallX?i=1&e=1";
const CALL_Y: &str = "https://proxy.example.com/api/v2/ep/conv-1.example.com/conv/CallY?i=1&e=1";
const CALL_X_ELSEWHERE: &str = "https://proxy.example.com/api/v2/ep/conv-2.example.com/conv/CallX?i=1&e=1";
/// Where an `add` on call X is posted.
const ADD_X: &str = "https://proxy.example.com/api/v2/ep/conv-1.example.com/conv/CallX/add?i=1&e=1";

/// Answers every fetch with lab accounts only, so every check on an existing scope passes.
struct LabOnly;

impl MembershipSource for LabOnly {
    fn members<'a>(&'a self, _scope: &'a Scope) -> MembershipFuture<'a> {
        Box::pin(async { Membership::Complete(vec![Mri::new(LAB_A), Mri::new(LAB_B)]) })
    }
}

fn fence() -> Fence {
    Fence::new(
        Roster::new([Mri::new(LAB_A), Mri::new(LAB_B)], []),
        FenceLog::from_writer(io::sink()),
    )
}

fn conversation(id: &str) -> Scope {
    Scope::Conversation { id: id.to_owned() }
}

/// A plain "hello" from a to `chat`.
#[cfg(test)]
fn hello(chat: &str) -> SendMessage {
    SendMessage::new(
        chat,
        Author::new(Mri::new(LAB_A), "Lab A"),
        MessageText::plain("hello").expect("plain text"),
    )
}

fn message_to(id: &str) -> Target {
    Target::new(WriteKind::Message, conversation(id))
}

/// The fence's decision on a target whose scope holds lab accounts only.
async fn check(target: Target) -> Result<WritePermit, Denied> {
    fence().check(target, &LabOnly).await
}

/// Records the target of every write it is handed; never touches the network.
#[derive(Default)]
struct Recording {
    delivered: Mutex<Vec<Target>>,
}

impl Recording {
    fn delivered(&self) -> Vec<Target> {
        self.delivered.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("never fails")]
struct Never;

impl Transport for Recording {
    type Error = Never;

    fn read<R: Read>(&self, _request: &R) -> impl Future<Output = Result<Response, Never>> + Send {
        future::ready(Ok(Response::new(200, "{}")))
    }

    fn send<R: Request>(&self, _request: &R) -> impl Future<Output = Result<Response, Never>> + Send {
        future::ready(Ok(Response::new(200, "{}")))
    }

    fn deliver<R: Mutating + Send>(
        &self,
        write: Authorized<R>,
    ) -> impl Future<Output = Result<Response, Never>> + Send {
        let target = write.permit().target().clone();
        self.delivered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(target);
        future::ready(Ok(Response::new(201, "")))
    }
}

/// Fails every request.
struct Offline;

impl Transport for Offline {
    type Error = io::Error;

    fn read<R: Read>(&self, _request: &R) -> impl Future<Output = Result<Response, io::Error>> + Send {
        future::ready(Err(io::Error::other("offline")))
    }

    fn send<R: Request>(&self, _request: &R) -> impl Future<Output = Result<Response, io::Error>> + Send {
        future::ready(Err(io::Error::other("offline")))
    }

    fn deliver<R: Mutating + Send>(
        &self,
        _write: Authorized<R>,
    ) -> impl Future<Output = Result<Response, io::Error>> + Send {
        future::ready(Err(io::Error::other("offline")))
    }
}

#[test]
fn mutating_calls_and_permits_are_enforced_by_the_compiler() {
    trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}

#[test]
fn send_message_targets_its_conversation() {
    let request = hello(CHAT_X);
    assert_eq!(request.target(), message_to(CHAT_X));
    assert_eq!(request.conversation(), CHAT_X);
    assert_eq!(request.text().html(), "<p>hello</p>");
}

#[tokio::test]
async fn a_permit_for_the_request_target_is_delivered() {
    let client = Client::new(Recording::default());
    let permit = check(message_to(CHAT_X)).await.expect("a lab-only target is allowed");

    let answer = client.write(hello(CHAT_X), permit).await.expect("matching permit");

    assert_eq!(answer.status(), 201, "the transport's answer comes back");
    assert_eq!(client.transport().delivered(), vec![message_to(CHAT_X)]);
}

#[tokio::test]
async fn a_permit_for_conversation_x_is_rejected_on_conversation_y() {
    let client = Client::new(Recording::default());
    let permit = check(message_to(CHAT_X)).await.expect("a lab-only target is allowed");

    let error = client
        .write(hello(CHAT_Y), permit)
        .await
        .expect_err("another conversation");

    let WriteError::TargetMismatch(mismatch) = &error else {
        panic!("expected a target mismatch, got {error:?}");
    };
    assert_eq!(mismatch.permitted(), &message_to(CHAT_X));
    assert_eq!(mismatch.requested(), &message_to(CHAT_Y));
    assert!(
        client.transport().delivered().is_empty(),
        "nothing reaches the transport"
    );
    let text = error.to_string();
    assert!(text.contains(CHAT_X) && text.contains(CHAT_Y), "{text}");
}

#[tokio::test]
async fn a_permit_for_another_kind_on_the_same_conversation_is_rejected() {
    let client = Client::new(Recording::default());
    let permit = check(Target::new(WriteKind::Typing, conversation(CHAT_X)))
        .await
        .expect("a lab-only target is allowed");

    let error = client.write(hello(CHAT_X), permit).await.expect_err("another kind");

    assert!(matches!(error, WriteError::TargetMismatch(_)), "{error:?}");
    assert!(client.transport().delivered().is_empty());
}

#[tokio::test]
async fn a_permit_for_a_new_chat_does_not_cover_a_message_in_an_existing_one() {
    let client = Client::new(Recording::default());
    let new_chat = Target::new(WriteKind::ChatCreate, Scope::New).with_added([Mri::new(LAB_A), Mri::new(LAB_B)]);
    let permit = check(new_chat).await.expect("a lab-only target is allowed");

    let error = client.write(hello(CHAT_X), permit).await.expect_err("another target");

    assert!(matches!(error, WriteError::TargetMismatch(_)), "{error:?}");
    assert!(client.transport().delivered().is_empty());
}

#[tokio::test]
async fn authorized_pairs_a_request_only_with_its_own_permit() {
    let matching = Authorized::new(
        hello(CHAT_X),
        check(message_to(CHAT_X)).await.expect("a lab-only target is allowed"),
    )
    .expect("matching permit");
    assert_eq!(matching.request().conversation(), CHAT_X);
    assert_eq!(matching.permit().target(), &matching.request().target());

    let mismatch = Authorized::new(
        hello(CHAT_Y),
        check(message_to(CHAT_X)).await.expect("a lab-only target is allowed"),
    )
    .expect_err("another conversation");
    assert_eq!(mismatch.permitted(), &message_to(CHAT_X));
    assert_eq!(mismatch.requested(), &message_to(CHAT_Y));
}

#[tokio::test]
async fn a_transport_failure_is_reported_as_such() {
    let client = Client::new(Offline);
    let permit = check(message_to(CHAT_X)).await.expect("a lab-only target is allowed");

    let error = client
        .write(hello(CHAT_X), permit)
        .await
        .expect_err("the transport fails");

    assert!(
        matches!(&error, WriteError::Transport(inner) if inner.to_string() == "offline"),
        "{error:?}"
    );
}

/// A fence whose roster also approves the echo service.
fn fence_with_echo() -> Fence {
    Fence::new(
        Roster::new([Mri::new(LAB_A), Mri::new(LAB_B)], [Mri::new(ECHO)]),
        FenceLog::from_writer(io::sink()),
    )
}

fn call_target(caller: &str, echo: &str) -> Target {
    Target::new(WriteKind::Call, Scope::New).with_added([Mri::new(caller), Mri::new(echo)])
}

#[test]
fn the_test_call_targets_a_new_call_of_the_caller_and_the_echo_it_adds() {
    let request = StartCall::test_call(Mri::new(LAB_A), Mri::new(ECHO));
    assert_eq!(request.target(), call_target(LAB_A, ECHO));
    assert_eq!(request.caller(), &Mri::new(LAB_A));
    assert_eq!(request.echo(), Some(&Mri::new(ECHO)));
}

#[tokio::test]
async fn a_test_call_with_an_approved_echo_gets_a_permit_that_covers_it() {
    let request = StartCall::test_call(Mri::new(LAB_A), Mri::new(ECHO));
    let permit = fence_with_echo()
        .check(request.target(), &LabOnly)
        .await
        .expect("lab account + approved service");

    let authorized = Authorized::new(request, permit).expect("the permit covers the call");
    assert_eq!(authorized.request().echo(), Some(&Mri::new(ECHO)));
}

#[tokio::test]
async fn a_test_call_with_an_echo_the_roster_does_not_approve_is_denied() {
    let request = StartCall::test_call(Mri::new(LAB_A), Mri::new(ECHO));
    let denied = fence()
        .check(request.target(), &LabOnly)
        .await
        .expect_err("the echo service isn't approved in this roster");
    assert_eq!(denied.reason(), &teams_fence::DenyReason::Outsiders { count: 1 });
}

#[tokio::test]
async fn a_call_permit_does_not_cover_a_test_call_for_someone_else() {
    let permit = fence_with_echo()
        .check(call_target(LAB_A, ECHO), &LabOnly)
        .await
        .expect("lab account + approved service");

    let mismatch =
        Authorized::new(StartCall::test_call(Mri::new(LAB_A), Mri::new(OUTSIDER)), permit).expect_err("another echo");
    assert_eq!(mismatch.permitted(), &call_target(LAB_A, ECHO));
    assert_eq!(mismatch.requested(), &call_target(LAB_A, OUTSIDER));
}

#[tokio::test]
async fn a_call_from_outside_the_lab_is_denied() {
    let request = StartCall::test_call(Mri::new(OUTSIDER), Mri::new(ECHO));
    let denied = fence_with_echo()
        .check(request.target(), &LabOnly)
        .await
        .expect_err("the caller isn't a lab account");
    assert_eq!(denied.reason(), &teams_fence::DenyReason::Outsiders { count: 1 });
}

/// A placeholder call's conversation.
#[cfg(test)]
fn call(url: &str) -> ConversationUrl {
    ConversationUrl::parse(url).unwrap()
}

#[test]
fn adding_to_a_call_is_an_invite_on_that_calls_conversation_url() {
    let request = AddToCall::new(call(CALL_X), Mri::new(ECHO));
    assert_eq!(
        request.target(),
        Target::new(WriteKind::Invite, Scope::Call { id: CALL_X.into() }).with_added([Mri::new(ECHO)])
    );
    assert_eq!(
        request.url().as_str(),
        ADD_X,
        "posted to the URL the target names, plus /add"
    );
    assert_eq!(request.conversation().map(ConversationUrl::id), Some("CallX"));
}

#[test]
fn a_conversation_url_is_https_with_a_host_and_a_conversation_id() {
    assert_eq!(call(CALL_X).as_str(), CALL_X);
    for url in [
        "https://proxy.example.com",
        "https://proxy.example.com/",
        "https://proxy.example.com/conv/",
        "https://proxy.example.com/conv/?i=1",
        "http://proxy.example.com/conv/CallX",
        "https://proxy.example.com/conv/CallX#fragment",
        "https://user:secret@proxy.example.com/conv/CallX",
        "proxy.example.com:443",
        "CallX",
        "",
    ] {
        assert_eq!(ConversationUrl::parse(url), None, "{url:?}");
    }
}

#[test]
fn calls_on_other_hosts_or_routes_are_other_targets() {
    let target = |url: &str| AddToCall::new(call(url), Mri::new(ECHO)).target();
    assert_ne!(target(CALL_X), target(CALL_Y));
    assert_ne!(
        target(CALL_X),
        target(CALL_X_ELSEWHERE),
        "the same conversation id on another conversation host"
    );
    assert_ne!(
        target(CALL_X),
        target("https://proxy.example.com/api/v2/ep/conv-1.example.com/conv/CallX?i=2&e=1"),
        "the same path routed elsewhere"
    );
}

#[tokio::test]
async fn adding_the_echo_to_a_call_is_denied_while_no_source_reads_a_call() {
    let request = AddToCall::new(call(CALL_X), Mri::new(ECHO));
    let denied = fence_with_echo()
        .check(request.target(), &teams_fence::FailClosed)
        .await
        .expect_err("a call's members can't be read");
    assert!(
        matches!(denied.reason(), teams_fence::DenyReason::Partial(_)),
        "{:?}",
        denied.reason()
    );
}

#[tokio::test]
async fn with_the_call_read_in_full_adding_the_echo_gets_a_permit_that_covers_it() {
    // what the fence would need: a complete read of the call's members (LabOnly stands in for one)
    let request = AddToCall::new(call(CALL_X), Mri::new(ECHO));
    let permit = fence_with_echo()
        .check(request.target(), &LabOnly)
        .await
        .expect("lab accounts + approved service");
    let authorized = Authorized::new(request, permit).expect("the permit covers the add");
    assert_eq!(
        authorized.request().url().as_str(),
        ADD_X,
        "the add goes to the call the permit was issued for"
    );
}

#[tokio::test]
async fn a_permit_for_one_call_does_not_cover_an_add_to_another() {
    for other in [CALL_Y, CALL_X_ELSEWHERE] {
        let permit = fence_with_echo()
            .check(AddToCall::new(call(CALL_X), Mri::new(ECHO)).target(), &LabOnly)
            .await
            .expect("lab accounts + approved service");
        let mismatch = Authorized::new(AddToCall::new(call(other), Mri::new(ECHO)), permit)
            .expect_err("a permit covers its own call only");
        assert_eq!(mismatch.permitted().scope(), &Scope::Call { id: CALL_X.into() });
        assert_eq!(mismatch.requested().scope(), &Scope::Call { id: other.into() });
    }
}
