// A `WritePermit` covers one write: it can't be cloned to cover a second one.
use teams_fence::WritePermit;

fn twice(permit: WritePermit) -> (WritePermit, WritePermit) {
    (permit.clone(), permit)
}

fn main() {}
