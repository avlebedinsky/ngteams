// Call constructors keep their captured wire representation private. Generic values type-check the private fields.
use teams_proto::{AddToCall, Mri, StartCall};

fn any<T>() -> T {
    unimplemented!()
}

fn main() {
    let _ = StartCall { caller: Mri::new("8:orgid:caller"), echo: None, built: any() };
    let _ = AddToCall {
        invitee: Mri::new("8:orgid:callee"),
        url: "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv/x/add".parse().unwrap(),
        built: any(),
    };
}
