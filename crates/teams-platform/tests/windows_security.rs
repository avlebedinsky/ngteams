//! Native token queries and pipe descriptor, without credential access.
#![cfg(test)]
#![cfg(windows)]

use teams_platform::windows_security::{check_user, current_user_sid, pipe_sddl};

#[test]
fn pipes_grant_the_exact_user_and_system_and_label_the_servers_integrity() {
    let user = current_user_sid().unwrap();
    assert!(user.starts_with("S-1-"));
    let sddl = pipe_sddl().unwrap();
    assert!(sddl.starts_with(&format!("O:{user}D:P")));
    assert!(sddl.contains(&format!("(A;;GA;;;{user})")));
    assert!(sddl.contains("(A;;GA;;;SY)"));
    assert!(sddl.ends_with("S:(ML;;NRNW;;;ME)") || sddl.ends_with("S:(ML;;NRNW;;;HI)"));
    assert!(
        !sddl.contains(";;;OW)"),
        "default owner can be a group under an elevated token"
    );
    check_user(std::process::id()).unwrap();
    assert!(check_user(u32::MAX).is_err());
    assert!(
        check_user(4).is_err(),
        "System is never accepted as the current user's peer"
    );
}
