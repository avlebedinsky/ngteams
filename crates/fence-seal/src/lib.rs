//! Compile checks against `teams-fence` and `teams-proto` as a lab binary sees them, without their `test-support`
//! features. The cases under `tests/ui/` must fail: no crate other than `teams-fence` can mint a `WritePermit`, since a
//! fence comes only from `Fence::open_lab` (the roster from `lab.toml`, the log at the lab's `fence.log`) and
//! `MembershipSource` is sealed; and the native transport sends only a `Read`, a plain `Request` (which it hands to the
//! fence's classifier first) or an `Authorized` write, to its services' bases only (`tests/ui/proto_*.rs`: no request
//! of another class, no plain request handed over as a write, no entry of the personal build's sealed lists from
//! outside, no mock origin). The cases under `tests/pass/` must build and run:
//! the sources `teams-fence` defines (`FailClosed`, and `LiveMembershipSource` with a credential parsed from a chat
//! service request) are usable, so a lab binary can arm the browser fence; the live source's local-mock origin is not
//! (`tests/ui/live_mock_origin.rs`). A fence's own account (whose own availability it permits) comes only from
//! `lab.toml` (`Fence::open_lab_account`), and the account's own availability names only a `RegisteredEndpoint` that a
//! registration over the native HTTP transport gave: none written out by hand, none from a transport of another
//! crate's making, directly or through the push channel (`tests/ui/fence_own_account.rs`,
//! `proto_registered_endpoint.rs`, `proto_register_fake_transport.rs`, `trouter_fake_transport.rs`, M2 Task 19). The
//! personal build's fence can't be opened or built here at all: `Fence::open_personal` and `Fence::personal` exist only
//! with `teams-fence`'s `personal` feature, which only the personal binary's crate enables
//! (`tests/ui/fence_open_personal.rs`, `fence_personal_constructor.rs`, M2 Task 27). This library is empty.
//!
//! They live in a crate of their own because trybuild builds each case with the dev-dependencies and features of the
//! crate that runs it, and the crates whose tests mint permits or reach a mock (`teams-fence`, `teams-proto`,
//! `proto-lab`) enable `test-support` there.
