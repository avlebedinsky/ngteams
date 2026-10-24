// A `DeliveryPermit` can't be built outside `teams-fence`: only `Fence::check_delivery_response` makes one, for the
// response to a pending call delivery.
use teams_fence::DeliveryPermit;

fn main() {
    let _ = DeliveryPermit { delivery: 1 };
}
