// A `WritePermit` can't be built outside `teams-fence` to feed the call registry: its fields (the target, the issuing
// fence and its serial) are private, so `Fence::pend_start` takes only a permit `Fence::check` issued.
use teams_fence::{Fence, Mri, Scope, Target, WriteKind, WritePermit};

fn pend(fence: &Fence) {
    let target = Target::new(WriteKind::Call, Scope::New).with_added([Mri::new("8:orgid:lab-a")]);
    let permit = WritePermit { target, issuer: 1, serial: 0 };
    let _ = fence.pend_start(&permit, b"{}");
}

fn main() {}
