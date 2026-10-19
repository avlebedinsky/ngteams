//! Compile checks against `teams-fence` as a lab binary sees it, without the `test-support` feature. The cases under
//! `tests/ui/` must fail: no crate other than `teams-fence` can mint a `WritePermit`, since a fence comes only from
//! `Fence::open_lab` (the roster from `lab.toml`, the log at the lab's `fence.log`) and `MembershipSource` is sealed.
//! The cases under `tests/pass/` must build and run: the sources `teams-fence` defines (`FailClosed`) are usable, so a
//! lab binary can arm the browser fence. This library is empty.
//!
//! They live in a crate of their own because trybuild builds each case with the dev-dependencies and features of the
//! crate that runs it, and the crates whose tests mint permits (`teams-fence`, `teams-proto`, `proto-lab`) enable
//! `test-support` there.
