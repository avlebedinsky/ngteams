use super::*;
use serde_json::json;

fn accounts() -> [LabAccount; 3] {
    ["a", "b", "c"].map(|id| LabAccount {
        mri: format!("8:orgid:00000000-0000-4000-8000-00000000000{id}"),
        upn: format!("bot-{id}@lab.example.com"),
    })
}

#[test]
fn only_the_three_explicit_accounts_and_the_exact_identity_are_accepted() {
    let [a, b, c] = accounts();
    let roster = LabRoster::new(accounts()).unwrap();
    assert!(roster.verify_identity(&a.mri, &a.mri, &a.upn.to_uppercase()).is_ok());
    assert!(roster.verify_identity(&a.mri, &b.mri, &a.upn).is_err());
    assert!(roster.verify_identity(&a.mri, &a.mri, &b.upn).is_err());
    for members in [
        vec![a.mri.clone(), b.mri.clone()],
        vec![a.mri.clone(), b.mri.clone(), c.mri],
    ] {
        assert!(roster.verify_chat(&a.mri, "expected", "expected", &members).is_ok());
        assert_eq!(
            roster.verify_chat(&a.mri, "expected", "restored-personal", &members),
            Err(TargetError::Conversation)
        );
        for outsider in [
            "8:orgid:00000000-0000-4000-8000-0000000000f0",
            "28:echo",
            "28:bot",
            "someone@external.example.com",
        ] {
            let mut mixed = members.clone();
            mixed.push(outsider.to_owned());
            assert_eq!(roster.verify_recipients(&a.mri, &mixed), Err(TargetError::Recipients));
        }
    }
    for members in [
        vec![],
        vec![a.mri.clone()],
        vec![b.mri.clone()],
        vec![a.mri.clone(), a.mri.clone()],
    ] {
        assert_eq!(roster.verify_recipients(&a.mri, &members), Err(TargetError::Recipients));
    }
    assert_eq!(roster.account(&b.mri), Some(&b));
    assert!(roster.account("28:echo").is_none());
}

#[test]
fn unknown_or_duplicate_lab_config_cannot_start_a_contacting_run() {
    for value in ["", "28:echo", "8:orgid:placeholder"] {
        let mut entries = accounts();
        entries[0].mri = value.to_owned();
        assert!(LabRoster::new(entries).is_err());
    }
    let mut entries = accounts();
    entries[0] = entries[1].clone();
    assert!(LabRoster::new(entries).is_err());
    let mut entries = accounts();
    entries[0].upn = entries[1].upn.to_uppercase();
    assert!(LabRoster::new(entries).is_err());
}

#[test]
fn full_members_requires_exact_thread_complete_counts_and_no_hidden_or_paged_members() {
    let [a, b, _] = accounts();
    let thread = json!({"id":"expected", "memberCount":2, "members":[{"id":a.mri},{"id":b.mri}]});
    assert_eq!(full_members("expected", &thread, false).unwrap().len(), 2);
    for change in [
        json!({"id":"other"}),
        json!({"memberCount":3}),
        json!({"membersCount":"bad"}),
        json!({"_metadata":{"nextLink":null}}),
        json!({"properties":{"nested":{"continuationToken":"x"}}}),
        json!({"members":[{"id":a.mri},{"id":b.mri,"hidden":true}]}),
        json!({"members":[{"id":a.mri},{"id":b.mri,"isDeleted":true}]}),
        json!({"members":[{"id":a.mri},{"id":a.mri}]}),
        json!({"members":[{"id":a.mri},{"id":"28:echo"}]}),
        json!({"members":[{}]}),
    ] {
        let mut changed = thread.clone();
        changed
            .as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        assert_eq!(
            full_members("expected", &changed, false),
            Err(TargetError::Membership),
            "{change}"
        );
    }
    let list = json!({"isDeleted":false,"totalMemberCount":2,"members":thread["members"]});
    assert_eq!(full_members("expected", &list, true).unwrap().len(), 2);
    for key in ["isDeleted", "totalMemberCount", "members"] {
        let mut missing = list.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert_eq!(full_members("expected", &missing, true), Err(TargetError::Membership));
    }
}

#[test]
fn one_to_one_targets_are_strict_and_symmetric() {
    let [a, b, _] = accounts();
    assert_eq!(one_to_one_id(&a.mri, &b.mri), one_to_one_id(&b.mri, &a.mri));
    assert!(one_to_one_id(&a.mri, &a.mri).is_none());
    assert!(one_to_one_id(&a.mri, "28:echo").is_none());
    assert!(one_to_one_id(&a.mri, "8:orgid:bad").is_none());
}
