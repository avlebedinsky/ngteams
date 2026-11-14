use crate::support::Lab;
use teams_mock::LAB_A;

#[path = "../../../../tests/support/call_acceptance.rs"]
mod assertion;

pub async fn assert_operations(lab: &Lab, expected: &[&str]) -> bool {
    let responses = lab
        .trouter
        .sockets()
        .iter()
        .filter(|socket| socket.account.as_deref() == Some(LAB_A))
        .flat_map(|socket| socket.frames.iter())
        .filter(|frame| assertion::is_acceptance_response(frame))
        .count();
    assertion::assert_operations(
        &lab.sent_by(LAB_A),
        &lab.mock.received_requests().await,
        &lab.last_call(),
        responses,
        expected,
    )
}
