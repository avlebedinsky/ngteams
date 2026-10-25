// A start or an add can't be put together outside `teams-proto` from a target and a callee of the caller's choosing:
// their fields are private. Every field gets a value of its type (`Mri::new`, `Target::new`, a parsed URL, and for the
// unnameable built request a generic `any()`), so the literals type-check and only the privacy check refuses them;
// nothing else in this file may fail, since rustc checks a struct literal's field privacy only once the crate has no
// other error.
use teams_fence::{Mri, Scope, Target, WriteKind};
use teams_proto::{AddToCall, StartCall};

fn target() -> Target {
    Target::new(WriteKind::Call, Scope::New).with_added([Mri::new("8:orgid:callee")])
}

fn any<T>() -> T {
    unimplemented!()
}

fn main() {
    let _ = StartCall { caller: Mri::new("8:orgid:caller"), echo: None, target: target(), built: any() };
    let _ = AddToCall {
        invitee: Mri::new("8:orgid:callee"),
        url: "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv/x/add".parse().unwrap(),
        target: target(),
        built: any(),
    };
}
