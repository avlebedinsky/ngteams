//! The personal build's call rule (`docs/personal-build.md`, "Calls"; CLAUDE.md, "The personal build's calls are the
//! user's, in the account's own chats"), on the call registry ([`crate::calls`]) the personal fence decides by:
//!
//! - **A start** ([`WriteKind::Call`] on the chat it carries): a 1:1 chat whose id names the account or a group chat
//!   (never a channel, a meeting chat or a stream), whose one fresh, unpaged thread read in a captured shape lists the
//!   account and every callee the start names; the account its caller. **The test call** ([`WriteKind::Call`] on
//!   [`Scope::New`]): exactly the account and the echo service ([`TEAMS_ECHO_BOT_MRI`]).
//! - **A join** of a running call: bound to its chat by an invitation the fence offered (the registry's join), from the
//!   account, and that chat's fresh read lists the account.
//! - **An `add`**: only the echo service, exactly, to the test call (a call this fence permitted for the account and
//!   the echo service).
//! - **Everything else on a call** (its follow-ups, an offered invitation's ring-time sends, its answer and its
//!   decline, a `leave`): only what the registry resolves to a call this run's fence registered, or an invitation it
//!   offered in a state open for the operation, from its own side. An offered invitation is any readable one addressed
//!   to the account, whoever it is from, so its answer and decline read no chat; what decides that they go is the
//!   user's action, which the engine alone turns into them.
//!
//! The roster plays no part. Every decision goes to `fence.log`, an allow that can't be logged is denied.

use std::collections::BTreeSet;

use super::{DenyReason, Fence, Membership, MembershipSource, PartialReason, WritePermit};
use crate::calls::Resolved;
use crate::contacting::call::TEAMS_ECHO_BOT_MRI;
use crate::decision::Denied;
use crate::log::Verdict;
use crate::membership::{is_group_chat_id, one_to_one_pair};
use crate::personal::same;
use crate::target::{CallOperation, Mri, Scope, Target};

impl Fence {
    /// The personal fence's verdict on a call start (see the module docs).
    pub(crate) async fn judge_personal_call_start(&self, target: &Target, source: &dyn MembershipSource) -> Verdict {
        let Some(own) = self.own.as_ref() else {
            return Verdict::Deny(DenyReason::NotOwnAccount);
        };
        if !target.added().iter().any(|mri| same(mri, own)) {
            return Verdict::Deny(DenyReason::CallMismatch);
        }
        let id = match target.scope() {
            Scope::New => return judge_test_call(target.added(), own),
            Scope::Conversation { id } => id,
            _ => return Verdict::Deny(DenyReason::NotPersonalScope),
        };
        match one_to_one_pair(id) {
            Some(pair) if !pair.iter().any(|member| same(member, own)) => {
                return Verdict::Deny(DenyReason::NotAMember);
            }
            Some(_) => {}
            None if is_group_chat_id(id) => {}
            None => return Verdict::Deny(DenyReason::NotPersonalScope),
        }
        match source.members(target.scope()).await {
            Membership::Listed(members) if !lists(&members, own) => Verdict::Deny(DenyReason::NotAMember),
            Membership::Listed(members) if target.added().iter().all(|callee| lists(&members, callee)) => {
                Verdict::Allow
            }
            Membership::Listed(_) => Verdict::Deny(DenyReason::CalleesNotInChat),
            Membership::Partial(reason) => Verdict::Deny(DenyReason::Partial(reason)),
            Membership::Complete(_) | Membership::OrgWide => {
                Verdict::Deny(DenyReason::Partial(PartialReason::Unsupported))
            }
        }
    }

    /// The personal fence's decision on an operation on a call (see the module docs): resolved by the call registry,
    /// a join's chat read fresh, an `add` only the test call's; a permitted one moves what it acts on.
    pub(crate) async fn check_personal_call_operation(
        &self,
        target: Target,
        source: &dyn MembershipSource,
    ) -> Result<WritePermit, Denied> {
        let Some(own) = self.own.clone() else {
            return self.decided(target, Verdict::Deny(DenyReason::NotOwnAccount));
        };
        let resolved = match self.calls.resolve(&target) {
            Ok(resolved) => resolved,
            Err(reason) => return self.decided(target, Verdict::Deny(reason)),
        };
        let operation = match target.scope() {
            Scope::CallOperation { operation, .. } => *operation,
            _ => return self.decided(target, Verdict::Deny(DenyReason::CallMismatch)),
        };
        let verdict = match operation {
            CallOperation::Join => judge_join(&resolved, &own, source).await,
            CallOperation::Add => judge_test_call_add(target.added(), &resolved),
            _ => Verdict::Allow,
        };
        let verdict = match verdict {
            // an offered invitation's ring can end (its end observed on another thread) between its resolution and here
            Verdict::Allow if !self.calls.still_open(&resolved) => {
                Verdict::Deny(DenyReason::Partial(PartialReason::CallEnded))
            }
            verdict => verdict,
        };
        let permit = self.decided(target, verdict)?;
        self.calls.permitted(&resolved);
        Ok(permit)
    }
}

/// Whether `members` lists `mri` (case aside).
fn lists(members: &[Mri], mri: &Mri) -> bool {
    members.iter().any(|member| same(member, mri))
}

/// The test call's start: the account and the echo service, no one else.
fn judge_test_call(added: &BTreeSet<Mri>, own: &Mri) -> Verdict {
    let echo = Mri::new(TEAMS_ECHO_BOT_MRI);
    let exactly = added.len() == 2 && added.contains(&echo) && added.iter().any(|mri| same(mri, own));
    if exactly {
        Verdict::Allow
    } else {
        Verdict::Deny(DenyReason::NotTheTestCall)
    }
}

/// An `add`: exactly the echo service, to a call the fence permitted for the account and the echo service (the test
/// call's start counts it in).
fn judge_test_call_add(added: &BTreeSet<Mri>, resolved: &Resolved) -> Verdict {
    let echo = Mri::new(TEAMS_ECHO_BOT_MRI);
    let only_echo = added.len() == 1 && added.contains(&echo);
    if only_echo && resolved.participants.contains(&echo) {
        Verdict::Allow
    } else {
        Verdict::Deny(DenyReason::NotTheTestCall)
    }
}

/// A join of a running call the registry bound to its group chat: from the account, and the chat's fresh read lists
/// the account.
async fn judge_join(resolved: &Resolved, own: &Mri, source: &dyn MembershipSource) -> Verdict {
    if !resolved.participants.iter().all(|joiner| same(joiner, own)) || resolved.participants.is_empty() {
        return Verdict::Deny(DenyReason::CallMismatch);
    }
    let Some(chat) = resolved.chat.as_deref().filter(|chat| is_group_chat_id(chat)) else {
        return Verdict::Deny(DenyReason::CallMismatch);
    };
    let scope = Scope::Conversation { id: chat.to_owned() };
    match source.members(&scope).await {
        Membership::Listed(members) if lists(&members, own) => Verdict::Allow,
        Membership::Listed(_) => Verdict::Deny(DenyReason::NotAMember),
        Membership::Partial(reason) => Verdict::Deny(DenyReason::Partial(reason)),
        Membership::Complete(_) | Membership::OrgWide => Verdict::Deny(DenyReason::Partial(PartialReason::Unsupported)),
    }
}
