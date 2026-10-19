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
use teams_proto::{Authorized, Client, Mutating, SendMessage, Transport, WriteError};

const LAB_A: &str = "8:orgid:lab-a";
const LAB_B: &str = "8:orgid:lab-b";
const CHAT_X: &str = "19:lab-chat-x@thread.v2";
const CHAT_Y: &str = "19:lab-chat-y@thread.v2";

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

    fn deliver<R: Mutating + Send>(&self, write: Authorized<R>) -> impl Future<Output = Result<(), Never>> + Send {
        let target = write.permit().target().clone();
        self.delivered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(target);
        future::ready(Ok(()))
    }
}

/// Fails every delivery.
struct Offline;

impl Transport for Offline {
    type Error = io::Error;

    fn deliver<R: Mutating + Send>(&self, _write: Authorized<R>) -> impl Future<Output = Result<(), io::Error>> + Send {
        future::ready(Err(io::Error::other("offline")))
    }
}

#[test]
fn mutating_calls_and_permits_are_enforced_by_the_compiler() {
    trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}

#[test]
fn send_message_targets_its_conversation() {
    let request = SendMessage::new(CHAT_X, "hello");
    assert_eq!(request.target(), message_to(CHAT_X));
    assert_eq!(request.conversation(), CHAT_X);
    assert_eq!(request.content(), "hello");
}

#[tokio::test]
async fn a_permit_for_the_request_target_is_delivered() {
    let client = Client::new(Recording::default());
    let permit = check(message_to(CHAT_X)).await.expect("a lab-only target is allowed");

    client
        .write(SendMessage::new(CHAT_X, "hello"), permit)
        .await
        .expect("matching permit");

    assert_eq!(client.transport().delivered(), vec![message_to(CHAT_X)]);
}

#[tokio::test]
async fn a_permit_for_conversation_x_is_rejected_on_conversation_y() {
    let client = Client::new(Recording::default());
    let permit = check(message_to(CHAT_X)).await.expect("a lab-only target is allowed");

    let error = client
        .write(SendMessage::new(CHAT_Y, "hello"), permit)
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

    let error = client
        .write(SendMessage::new(CHAT_X, "hello"), permit)
        .await
        .expect_err("another kind");

    assert!(matches!(error, WriteError::TargetMismatch(_)), "{error:?}");
    assert!(client.transport().delivered().is_empty());
}

#[tokio::test]
async fn a_permit_for_a_new_chat_does_not_cover_a_message_in_an_existing_one() {
    let client = Client::new(Recording::default());
    let new_chat = Target::new(WriteKind::ChatCreate, Scope::New).with_added([Mri::new(LAB_A), Mri::new(LAB_B)]);
    let permit = check(new_chat).await.expect("a lab-only target is allowed");

    let error = client
        .write(SendMessage::new(CHAT_X, "hello"), permit)
        .await
        .expect_err("another target");

    assert!(matches!(error, WriteError::TargetMismatch(_)), "{error:?}");
    assert!(client.transport().delivered().is_empty());
}

#[tokio::test]
async fn authorized_pairs_a_request_only_with_its_own_permit() {
    let matching = Authorized::new(
        SendMessage::new(CHAT_X, "hello"),
        check(message_to(CHAT_X)).await.expect("a lab-only target is allowed"),
    )
    .expect("matching permit");
    assert_eq!(matching.request().conversation(), CHAT_X);
    assert_eq!(matching.permit().target(), &matching.request().target());

    let mismatch = Authorized::new(
        SendMessage::new(CHAT_Y, "hello"),
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
        .write(SendMessage::new(CHAT_X, "hello"), permit)
        .await
        .expect_err("the transport fails");

    assert!(
        matches!(&error, WriteError::Transport(inner) if inner.to_string() == "offline"),
        "{error:?}"
    );
}
