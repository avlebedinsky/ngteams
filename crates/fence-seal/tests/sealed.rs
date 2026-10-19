//! Without `test-support`, nothing outside `teams-fence` can choose the roster, the log or the member lists a fence
//! decides by.

#[test]
fn a_permit_cant_be_minted_outside_the_fence() {
    trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}
