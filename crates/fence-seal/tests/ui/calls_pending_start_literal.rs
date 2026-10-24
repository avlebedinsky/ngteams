// A `PendingStart` can't be built outside `teams-fence`: only `Fence::pend_start` makes one, from a call start permit
// of that fence, so no other crate can register a call it didn't get a permit for.
use teams_fence::PendingStart;

fn main() {
    let _ = PendingStart { fence: 1, call: 0 };
}
