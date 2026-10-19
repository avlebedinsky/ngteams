//! Without `test-support`, nothing outside `teams-fence` can choose the roster, the log or the member lists a fence
//! decides by, and a lab binary can still arm the browser fence with the sources `teams-fence` defines.

#[test]
fn a_permit_cant_be_minted_outside_the_fence() {
    trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}

#[test]
fn a_lab_binary_has_a_membership_source() {
    trybuild::TestCases::new().pass("tests/pass/*.rs");
}
