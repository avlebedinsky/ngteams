//! Compile-fail checks that no crate other than `teams-fence` can mint a `WritePermit`: without the `test-support`
//! feature, a fence comes only from `Fence::open_lab` (the roster from `lab.toml`, the log at the lab's `fence.log`),
//! and `MembershipSource` is sealed. The cases are under `tests/ui/`; this library is empty.
//!
//! They live in a crate of their own because trybuild builds each case with the dev-dependencies and features of the
//! crate that runs it, and the crates whose tests mint permits (`teams-fence`, `teams-proto`, `proto-lab`) enable
//! `test-support` there.
