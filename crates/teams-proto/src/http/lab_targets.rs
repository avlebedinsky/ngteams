//! Operational lab targets, independent of transport authorization. Personal transports carry no lab context.

use std::sync::Arc;

use serde_json::Value;
use teams_fence::{Scope, WriteKind};
use teams_lab_targets::{LabRoster, TargetError, full_members};

use super::{Credentials, HttpError, HttpTransport, Refusal, Transport};
use crate::{Endpoint, Mutating, Read, Service};

pub(super) struct LabContext {
    own: String,
    upn: String,
    roster: Arc<LabRoster>,
    pub(super) calls: Arc<crate::lab_calls::LabCallBindings>,
}

struct NativeRead {
    service: Service,
    endpoint: Endpoint,
}
impl Read for NativeRead {
    fn service(&self) -> Service {
        self.service
    }
    fn endpoint(&self) -> Endpoint {
        self.endpoint.clone()
    }
}

fn refused(error: TargetError) -> HttpError {
    HttpError::Refused(Refusal::LabTarget(error))
}

impl<C: Credentials> HttpTransport<C> {
    /// Marks an implementation transport with the exact three-account roster; personal callers leave it absent.
    ///
    /// # Errors
    /// Rejects an actor or UPN outside the dedicated roster.
    pub fn with_lab_targets(mut self, own: String, upn: String, roster: Arc<LabRoster>) -> Result<Self, TargetError> {
        roster.verify_identity(&own, &own, &upn)?;
        let calls = Arc::new(crate::lab_calls::LabCallBindings::new(own.clone(), Arc::clone(&roster)));
        self.lab_targets = Some(LabContext {
            own,
            upn,
            roster,
            calls,
        });
        Ok(self)
    }

    #[must_use]
    pub fn lab_roster(&self) -> Option<&LabRoster> {
        self.lab_targets.as_ref().map(|context| context.roster.as_ref())
    }

    #[must_use]
    pub fn lab_own(&self) -> Option<&str> {
        self.lab_targets.as_ref().map(|context| context.own.as_str())
    }

    pub(super) async fn lab_identity(&self) -> Result<(), HttpError> {
        let Some(context) = &self.lab_targets else {
            return Ok(());
        };
        let answer = self
            .read(&NativeRead {
                service: Service::Graph,
                endpoint: Endpoint::new(["me"]).with_query("$select", "id,userPrincipalName"),
            })
            .await?;
        let body: Value = serde_json::from_slice(answer.body()).map_err(|_| refused(TargetError::Identity))?;
        let id = body
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| refused(TargetError::Identity))?;
        let upn = body
            .get("userPrincipalName")
            .and_then(Value::as_str)
            .ok_or_else(|| refused(TargetError::Identity))?;
        context
            .roster
            .verify_identity(&context.own, &format!("8:orgid:{id}"), upn)
            .map_err(refused)?;
        if !context.upn.eq_ignore_ascii_case(upn) {
            return Err(refused(TargetError::Identity));
        }
        Ok(())
    }

    /// Fresh exact-thread proof immediately before an implementation action. `None` is the personal path.
    ///
    /// # Errors
    /// Rejects incomplete, mismatched or non-lab membership and failed identity reads.
    pub async fn preflight_lab_chat(&self, chat: &str) -> Result<Option<Vec<String>>, HttpError> {
        let Some(context) = &self.lab_targets else {
            return Ok(None);
        };
        self.lab_identity().await?;
        let answer = self
            .read(&NativeRead {
                service: Service::ChatService,
                endpoint: Endpoint::new(["v1", "threads", chat]).with_query("view", "msnp24Equivalent"),
            })
            .await?;
        let thread: Value = serde_json::from_slice(answer.body()).map_err(|_| refused(TargetError::Membership))?;
        if thread.get("id").and_then(Value::as_str) != Some(chat) {
            return Err(refused(TargetError::Conversation));
        }
        let members = if chat.ends_with("@unq.gbl.spaces") {
            full_members(chat, &thread, false).map_err(refused)?
        } else {
            let answer = self
                .read(&NativeRead {
                    service: Service::ChatService,
                    endpoint: Endpoint::new(["v1", "threads", chat, "members"])
                        .with_query("view", "msnp24Equivalent")
                        .with_query("pageSize", "100"),
                })
                .await?;
            let body: Value = serde_json::from_slice(answer.body()).map_err(|_| refused(TargetError::Membership))?;
            full_members(chat, &body, true).map_err(refused)?
        };
        context
            .roster
            .verify_chat(&context.own, chat, chat, &members)
            .map_err(refused)?;
        Ok(Some(members))
    }

    pub(super) fn preflight_lab_write<'a, R: Mutating>(
        &'a self,
        write: &R,
    ) -> impl std::future::Future<Output = Result<(), HttpError>> + Send + 'a {
        let target = write.target();
        let files = if self.lab_targets.is_some() {
            write_wire_files(write)
        } else {
            Ok(Vec::new())
        };
        async move {
            let Some(context) = &self.lab_targets else {
                return Ok(());
            };
            if target.sender().is_some_and(|sender| sender.as_str() != context.own) {
                return Err(refused(TargetError::Identity));
            }
            if matches!(
                target.kind(),
                WriteKind::Call
                    | WriteKind::CallAnswer
                    | WriteKind::CallRinging
                    | WriteKind::CallDecline
                    | WriteKind::CallFollowUp
                    | WriteKind::Invite
            ) {
                return Err(refused(TargetError::Recipients));
            }
            match target.scope() {
                Scope::Conversation { id } => {
                    let members = self
                        .preflight_lab_chat(id)
                        .await?
                        .ok_or_else(|| refused(TargetError::Membership))?;
                    if target
                        .added()
                        .iter()
                        .any(|person| !members.iter().any(|member| member == person.as_str()))
                    {
                        return Err(refused(TargetError::Recipients));
                    }
                    for file in files? {
                        self.preflight_lab_file(&file, &members).await?;
                    }
                    Ok(())
                }
                Scope::New if target.kind() == WriteKind::ChatCreate => {
                    self.preflight_lab_creation(target.added().iter().map(|mri| mri.as_str().to_owned()).collect())
                        .await
                }
                Scope::Account { mri } if target.kind() == WriteKind::OwnAvailability && mri == &context.own => Ok(()),
                // Unknown/new meeting, echo and call-operation audiences are not a lab preflight proof.
                _ => Err(refused(TargetError::Recipients)),
            }
        }
    }

    async fn preflight_lab_creation(&self, mut members: Vec<String>) -> Result<(), HttpError> {
        let Some(context) = &self.lab_targets else {
            return Ok(());
        };
        self.lab_identity().await?;
        if !members.contains(&context.own) {
            members.push(context.own.clone());
        }
        context
            .roster
            .verify_recipients(&context.own, &members)
            .map_err(refused)?;
        for peer in members.iter().filter(|mri| **mri != context.own) {
            let account = context
                .roster
                .account(peer)
                .ok_or_else(|| refused(TargetError::Recipients))?;
            self.lab_peer(peer, &account.upn).await?;
        }
        Ok(())
    }

    pub(super) async fn lab_peer(&self, mri: &str, upn: &str) -> Result<(), HttpError> {
        let Some(context) = &self.lab_targets else {
            return Ok(());
        };
        let id = crate::object_id(mri).ok_or_else(|| refused(TargetError::Identity))?;
        let answer = self
            .read(&NativeRead {
                service: Service::Graph,
                endpoint: Endpoint::new(["users", id]).with_query("$select", "id,userPrincipalName"),
            })
            .await?;
        let body: Value = serde_json::from_slice(answer.body()).map_err(|_| refused(TargetError::Identity))?;
        let observed = body
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| refused(TargetError::Identity))?;
        let actual_upn = body
            .get("userPrincipalName")
            .and_then(Value::as_str)
            .ok_or_else(|| refused(TargetError::Identity))?;
        context
            .roster
            .verify_identity(mri, &format!("8:orgid:{observed}"), actual_upn)
            .map_err(refused)?;
        if !actual_upn.eq_ignore_ascii_case(upn) {
            return Err(refused(TargetError::Identity));
        }
        Ok(())
    }

    async fn preflight_lab_file(&self, file: &crate::UploadedFile, recipients: &[String]) -> Result<(), HttpError> {
        let context = self
            .lab_targets
            .as_ref()
            .ok_or_else(|| refused(TargetError::Identity))?;
        let site = file.site().ok_or_else(|| refused(TargetError::Membership))?;
        let owner = self.drive_details(&site).await?;
        let owner: Value = serde_json::from_slice(owner.body()).map_err(|_| refused(TargetError::Membership))?;
        let id = crate::object_id(&context.own).ok_or_else(|| refused(TargetError::Identity))?;
        if owner.pointer("/owner/user/id").and_then(Value::as_str) != Some(id) {
            return Err(refused(TargetError::Identity));
        }
        let grants = self.file_permissions(file).await?;
        let grants: Value = serde_json::from_slice(grants.body()).map_err(|_| refused(TargetError::Membership))?;
        let grants = crate::direct_file_grants(&grants, id).ok_or_else(|| refused(TargetError::Membership))?;
        let grants: Vec<String> = grants.iter().map(|mri| mri.as_str().to_owned()).collect();
        context
            .roster
            .verify_recipients(&context.own, &grants)
            .map_err(refused)?;
        if recipients.iter().any(|member| !grants.contains(member)) {
            return Err(refused(TargetError::Recipients));
        }
        Ok(())
    }
}

fn write_wire_files<R: Mutating>(write: &R) -> Result<Vec<crate::UploadedFile>, HttpError> {
    let Some(Ok(wire)) = crate::write::wire_or_call(write) else {
        return Ok(Vec::new());
    };
    let Some(bytes) = wire.body_bytes() else {
        return Ok(Vec::new());
    };
    let body: Value = serde_json::from_slice(&bytes).map_err(|_| refused(TargetError::Membership))?;
    let Some(files) = body.pointer("/properties/files") else {
        return Ok(Vec::new());
    };
    let files = match files {
        Value::String(text) => serde_json::from_str::<Value>(text).map_err(|_| refused(TargetError::Membership))?,
        value => value.clone(),
    };
    files
        .as_array()
        .ok_or_else(|| refused(TargetError::Membership))?
        .iter()
        .map(|entry| crate::UploadedFile::from_entry(entry).ok_or_else(|| refused(TargetError::Membership)))
        .collect()
}
