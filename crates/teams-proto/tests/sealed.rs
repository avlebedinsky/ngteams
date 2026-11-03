//! Kept compile-fail contracts: typed frames/replies, validated constructors and sanitized message text.

#[test]
fn protocol_types_keep_their_typed_construction_boundaries() {
    trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}
