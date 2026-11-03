//! Explicit, synthetic full-recipient responses for offline call scenarios; no network or credentials.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;
use teams_lab_targets::{LabAccount, LabRoster, one_to_one_id};

use crate::action_targets::ActionTargets;
use crate::session::LabSession;

pub(crate) const A: &str = "8:orgid:00000000-0000-4000-8000-000000000001";
pub(crate) const B: &str = "8:orgid:00000000-0000-4000-8000-000000000002";
pub(crate) const C: &str = "8:orgid:00000000-0000-4000-8000-000000000003";

pub(crate) fn chat() -> String {
    one_to_one_id(A, B).unwrap()
}

pub(crate) fn allow(session: LabSession, own: &str) -> LabSession {
    with_members(session, own, &[A, B])
}

pub(crate) fn with_members(mut session: LabSession, own: &str, members: &[&str]) -> LabSession {
    let roster = LabRoster::new([A, B, C].map(|mri| LabAccount {
        mri: mri.to_owned(),
        upn: format!("{}@lab.example.com", &mri[mri.len() - 1..]),
    }))
    .unwrap();
    let mut replies = BTreeMap::new();
    let chat = chat();
    let members: Vec<_> = members.iter().map(|id| json!({"id":id})).collect();
    replies.insert(chat.clone(), json!({"id":chat,"members":members}));
    for group in [
        "19:synthetic@thread.v2",
        "19:synthetic-target@thread.v2",
        "19:synthetic-group@thread.v2",
    ] {
        replies.insert(
            group.to_owned(),
            json!({"isDeleted":false,"totalMemberCount":3,"members":[{"id":A},{"id":B},{"id":C}]}),
        );
    }
    session.set_action_targets(Arc::new(ActionTargets::synthetic(own, roster, replies).unwrap()));
    session
}
