//! The absolute, validated own-drive route; no redirects, proxies, credential forwarding or implicit write retry.

use super::{Auth, Credentials, HttpError, HttpTransport, Refusal, Response};
use crate::{DriveSite, DriveWrite, UploadedFile};

impl<C: Credentials> HttpTransport<C> {
    pub(super) async fn preflight_attachment(&self, write: &DriveWrite) -> Result<(), HttpError> {
        use teams_lab_targets::TargetError;
        let refused = |error| HttpError::Refused(Refusal::LabTarget(error));
        let owner = crate::object_id(&write.owner).ok_or_else(|| refused(TargetError::Identity))?;
        let site = DriveSite::from_web_url(write.url().as_str()).ok_or_else(|| refused(TargetError::Membership))?;
        if let Some(own) = self.lab_own() {
            if own != write.owner {
                return Err(refused(TargetError::Identity));
            }
            self.lab_identity().await?;
        }
        let details = self.drive_details(&site).await?;
        let details: serde_json::Value =
            serde_json::from_slice(details.body()).map_err(|_| refused(TargetError::Membership))?;
        if details.pointer("/owner/user/id").and_then(serde_json::Value::as_str) != Some(owner) {
            return Err(refused(TargetError::Identity));
        }
        let grants = match &write.file {
            Some(file) => self.file_permissions(file).await?,
            None => self.chat_folder_permissions(&site).await?,
        };
        let grants: serde_json::Value =
            serde_json::from_slice(grants.body()).map_err(|_| refused(TargetError::Membership))?;
        let grants = crate::direct_file_grants(&grants, owner).ok_or_else(|| refused(TargetError::Membership))?;
        if write.file.is_none() && (grants.len() != 1 || grants[0].as_str() != write.owner) {
            return Err(refused(TargetError::Recipients));
        }
        if let Some(roster) = self.lab_roster() {
            if grants.iter().any(|mri| roster.account(mri.as_str()).is_none()) {
                return Err(refused(TargetError::Recipients));
            }
            if write.file.is_some() {
                let chat = write
                    .conversation
                    .as_deref()
                    .ok_or_else(|| refused(TargetError::Conversation))?;
                let fresh = self
                    .preflight_lab_chat(chat)
                    .await?
                    .ok_or_else(|| refused(TargetError::Membership))?;
                if fresh.len() != write.recipients.len() + 1
                    || write.recipients.iter().any(|(mri, email)| {
                        mri == &write.owner
                            || !fresh.contains(mri)
                            || roster
                                .account(mri)
                                .is_none_or(|account| !account.upn.eq_ignore_ascii_case(email))
                    })
                    || fresh
                        .iter()
                        .any(|mri| mri != &write.owner && !write.recipients.iter().any(|(peer, _)| peer == mri))
                {
                    return Err(refused(TargetError::Recipients));
                }
                for (mri, email) in &write.recipients {
                    self.lab_peer(mri, email).await?;
                }
            }
        }
        Ok(())
    }
    /// The site's drive owner, used to establish that its effective-grants answer is complete for this account.
    ///
    /// # Errors
    /// Returns transport, credentials, or unsuccessful service responses.
    pub async fn drive_details(&self, site: &DriveSite) -> Result<Response, HttpError> {
        self.drive_read(site, &site.drive_base()).await
    }

    /// The Teams chat-files folder's effective grants, or the root's grants before that folder is created.
    ///
    /// # Errors
    /// Returns transport errors or an unrecognized/missing folder permission response.
    pub async fn chat_folder_permissions(&self, site: &DriveSite) -> Result<Response, HttpError> {
        let folder = format!("{}/root:/Microsoft%20Teams%20Chat%20Files", site.drive_base());
        let url = match self.drive_read(site, &folder).await {
            Ok(_) => format!("{folder}:/permissions"),
            Err(HttpError::Status(answer)) if answer.status() == 404 => {
                format!("{}/root/permissions", site.drive_base())
            }
            Err(error) => return Err(error),
        };
        self.drive_read(site, &url).await
    }

    async fn drive_read(&self, site: &DriveSite, url: &str) -> Result<Response, HttpError> {
        let url = url::Url::parse(url).map_err(|_| HttpError::Refused(Refusal::NotAllowed))?;
        if DriveSite::from_web_url(url.as_str()).as_ref() != Some(site) {
            return Err(HttpError::Refused(Refusal::NotAllowed));
        }
        let token = self
            .credentials
            .bearer(&site.audience())
            .await
            .map_err(HttpError::Credentials)?;
        let answer = self
            .send_as(reqwest::Method::GET, &url, &[], Auth::Bearer(&token), None)
            .await?;
        if answer.is_success() {
            Ok(answer)
        } else {
            Err(HttpError::Status(Box::new(answer)))
        }
    }
    pub(super) async fn deliver_drive(&self, write: &DriveWrite) -> Result<Response, HttpError> {
        let site = DriveSite::from_web_url(write.url().as_str()).ok_or(HttpError::Refused(Refusal::NotAllowed))?;
        let audience = site.audience();
        let token = self
            .credentials
            .bearer(&audience)
            .await
            .map_err(HttpError::Credentials)?;
        let headers = vec![
            ("content-type".to_owned(), write.content_type().to_owned()),
            ("accept".to_owned(), "application/json".to_owned()),
        ];
        let answer = self
            .send_as(
                write.method().to_reqwest(),
                write.url(),
                &headers,
                Auth::Bearer(&token),
                Some(write.body().to_vec()),
            )
            .await?;
        if answer.status() == 401 {
            let _ = self.credentials.bearer_rejected(&audience, &token).await;
        }
        Ok(answer)
    }

    /// Reads the authoritative effective grants after an explicit direct share; never follows a response link.
    ///
    /// # Errors
    /// Returns an invalid site, transport error, or unsuccessful service response.
    pub async fn file_permissions(&self, file: &UploadedFile) -> Result<Response, HttpError> {
        let site = file.site().ok_or(HttpError::Refused(Refusal::NotAllowed))?;
        self.drive_read(&site, &format!("{}/permissions", file.item_url()))
            .await
    }
}
