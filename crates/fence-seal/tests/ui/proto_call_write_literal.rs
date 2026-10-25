// A call write is built only whole, by its constructor, which classifies the request it builds and keeps the target
// the fence's classifier gives: no other crate can put a write together from a target and a body of its own choosing
// (each call write's fields are private), name its wire form, or make a link the call's services never handed out
// (`CallLink` has no constructor but `parse`, which takes only a link on the flight proxy). The call writes with named
// fields are tried in `proto_call_write_struct_literal.rs`: rustc checks a struct literal's field privacy only once
// the crate has no other error.
use teams_proto::{
    AcceptCall, AcknowledgeAcceptance, AcknowledgeMediaAnswer, AnswerMedia, ApplyChannelParameters, AttachCall,
    Authorized, CallLink, DeclineCall, JoinCall, LeaveCall, Mutating, RenegotiateMedia, ReportRinging,
    UpdateEndpointMetadata, UpdateEndpointState, UpdateMediaDescriptions, UpdateParticipantProperties,
};

fn wire<R: Mutating>(write: &Authorized<R>) {
    let _ = write.call();
    let _ = write.wire();
}

fn main() {
    let _ = DeclineCall(());
    let _ = AcceptCall(());
    let _ = AttachCall(());
    let _ = JoinCall(());
    let _ = LeaveCall(());
    let _ = ReportRinging(());
    let _ = UpdateEndpointState(());
    let _ = AcknowledgeAcceptance(());
    let _ = ApplyChannelParameters(());
    let _ = UpdateEndpointMetadata(());
    let _ = UpdateParticipantProperties(());
    let _ = RenegotiateMedia(());
    let _ = AnswerMedia(());
    let _ = AcknowledgeMediaAnswer(());
    let _ = UpdateMediaDescriptions(());
    let _ = CallLink(());
    let _: Option<teams_proto::CallWire> = None;
}
