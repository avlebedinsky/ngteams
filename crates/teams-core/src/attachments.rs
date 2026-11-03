//! Files stay private while staged. Send grants this chat's current readers access, verifies the grants, then queues
//! the ordinary message. Uploads/shares use the outbox; an unknown outcome is never retried automatically.

use serde_json::Value;
use std::{io::Read as _, path::PathBuf, sync::Arc};
use teams_fence::{Membership, Mri, one_to_one_pair, parse_members, parse_thread};
use teams_proto::chat::ChatReads;
use teams_proto::{
    Client, Credentials, DriveSite, DriveWrite, HttpError, HttpTransport, MAX_ATTACHMENT_SIZE, OwnDrive,
    ShortProfileLookup, Transport, UploadedFile, direct_file_grants,
};

use crate::engine::Responder;
use crate::{CallRetries, CallTried, EngineError, Outbox};

pub(crate) struct Uploads<C, T> {
    client: Arc<Client<HttpTransport<C>>>,
    outbox: Arc<Outbox<T>>,
    slots: Arc<tokio::sync::Semaphore>,
}

impl<C, T> Uploads<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    pub(crate) fn lab_roster(&self) -> Option<&teams_lab_targets::LabRoster> {
        self.client.transport().lab_roster()
    }
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>, outbox: Arc<Outbox<T>>) -> Self {
        Self {
            client,
            outbox,
            slots: Arc::new(tokio::sync::Semaphore::new(2)),
        }
    }

    pub(crate) fn upload(&self, path: PathBuf, reply: Responder<UploadedFile>) {
        let client = Arc::clone(&self.client);
        let outbox = Arc::clone(&self.outbox);
        let slots = Arc::clone(&self.slots);
        tokio::task::spawn_local(async move {
            let Ok(_slot) = slots.acquire_owned().await else {
                let _ = reply.send(Err(EngineError::Stopped));
                return;
            };
            let answer = upload(&client, &outbox, path).await;
            let _ = reply.send(answer);
        });
    }

    pub(crate) async fn share(&self, chat: &str, files: &[UploadedFile]) -> Result<(), EngineError> {
        if files.is_empty() {
            return Ok(());
        }
        let (recipients, people) = recipients(&self.client, chat, self.outbox.me().mri()).await?;
        if recipients.is_empty() {
            return Ok(());
        }
        for file in files {
            let site = file
                .site()
                .ok_or_else(|| failed("The file's storage details can't be read."))?;
            verify_owner(&self.client, &site, self.outbox.me().mri()).await?;
            let before = self
                .client
                .transport()
                .file_permissions(file)
                .await
                .map_err(|_| failed("Couldn't read who can access this file."))?;
            let before: Value =
                serde_json::from_slice(before.body()).map_err(|_| failed("Couldn't read the file's permissions."))?;
            let owner = teams_proto::object_id(self.outbox.me().mri().as_str())
                .ok_or_else(|| failed("The file's owner can't be read."))?;
            direct_file_grants(&before, owner)
                .ok_or_else(|| failed("This file's access isn't limited to named people."))?;
            let addressed: Vec<(String, String)> = people
                .iter()
                .zip(&recipients)
                .map(|(person, address)| (person.as_str().to_owned(), address.clone()))
                .collect();
            let write = DriveWrite::share(file, chat, &addressed, self.outbox.me().mri().as_str())
                .ok_or_else(|| failed("The file's sharing details can't be read."))?;
            send(&self.outbox, write).await?;
            let answer = self
                .client
                .transport()
                .file_permissions(file)
                .await
                .map_err(|_| failed("Couldn't confirm who can access the file. Try again."))?;
            let grants: Value = serde_json::from_slice(answer.body())
                .map_err(|_| failed("The file's sharing answer can't be read."))?;
            let owner = teams_proto::object_id(self.outbox.me().mri().as_str())
                .ok_or_else(|| failed("The file's owner can't be read."))?;
            let grants = direct_file_grants(&grants, owner)
                .ok_or_else(|| failed("Couldn't confirm that the file is shared only with specific people."))?;
            if self
                .lab_roster()
                .is_some_and(|roster| grants.iter().any(|person| roster.account(person.as_str()).is_none()))
            {
                return Err(failed(
                    "Couldn't confirm that the file is limited to this chat's members.",
                ));
            }
            if people.iter().any(|person| !grants.contains(person)) {
                return Err(failed("Not everyone in this chat can access the file. Try again."));
            }
        }
        Ok(())
    }
}

async fn upload<C, T>(
    client: &Client<HttpTransport<C>>,
    outbox: &Outbox<T>,
    path: PathBuf,
) -> Result<UploadedFile, EngineError>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError>,
{
    if client.transport().is_read_only() {
        return Err(failed("This app is read-only."));
    }
    let (name, bytes) = tokio::task::spawn_blocking(move || read_file(&path))
        .await
        .map_err(|_| failed("The file couldn't be read."))??;
    let answer = client
        .read(&OwnDrive)
        .await
        .map_err(|_| failed("Couldn't open your OneDrive. Check your connection and try again."))?;
    let drive: Value =
        serde_json::from_slice(answer.body()).map_err(|_| failed("Your OneDrive details couldn't be read."))?;
    let site = drive
        .get("webUrl")
        .and_then(Value::as_str)
        .and_then(DriveSite::from_web_url)
        .ok_or_else(|| failed("Your OneDrive isn't available. Check your storage with your administrator."))?;
    verify_owner(client, &site, outbox.me().mri()).await?;
    let permissions = client
        .transport()
        .chat_folder_permissions(&site)
        .await
        .map_err(|_| failed("Couldn't read your chat-files folder permissions."))?;
    let permissions: Value = serde_json::from_slice(permissions.body())
        .map_err(|_| failed("Couldn't read your chat-files folder permissions."))?;
    let owner = teams_proto::object_id(outbox.me().mri().as_str())
        .ok_or_else(|| failed("Your file-storage owner can't be read."))?;
    let grants = direct_file_grants(&permissions, owner).ok_or_else(|| {
        failed("Your chat-files folder isn't private. Check its sharing permissions before attaching a file.")
    })?;
    if grants != [outbox.me().mri().clone()] {
        return Err(failed(
            "Your chat-files folder is shared. Make it private before attaching a file.",
        ));
    }
    let write = DriveWrite::upload(&site, &name, bytes, outbox.me().mri().as_str())
        .ok_or_else(|| failed("This file can't be uploaded. Rename it and try again."))?;
    let body = send(outbox, write).await?;
    UploadedFile::from_upload(&site, &body)
        .ok_or_else(|| failed("The upload completed, but its file details couldn't be read. Choose the file again."))
}

async fn verify_owner<C: Credentials>(
    client: &Client<HttpTransport<C>>,
    site: &DriveSite,
    me: &Mri,
) -> Result<(), EngineError> {
    let answer = client
        .transport()
        .drive_details(site)
        .await
        .map_err(|_| failed("Couldn't verify your file-storage owner."))?;
    let drive: Value =
        serde_json::from_slice(answer.body()).map_err(|_| failed("Couldn't verify your file-storage owner."))?;
    if drive.pointer("/owner/user/id").and_then(Value::as_str) != teams_proto::object_id(me.as_str()) {
        return Err(failed("The file storage doesn't belong to this account."));
    }
    Ok(())
}

fn read_file(path: &std::path::Path) -> Result<(String, Vec<u8>), EngineError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| failed("The file's name can't be read."))?
        .to_owned();
    let file = std::fs::File::open(path)
        .map_err(|_| failed("The file can't be read. Check its permissions and choose it again."))?;
    let metadata = file.metadata().map_err(|_| failed("The file can't be read."))?;
    if !metadata.is_file() {
        return Err(failed("Choose a file, rather than a folder."));
    }
    if metadata.len() > MAX_ATTACHMENT_SIZE {
        return Err(failed("Choose a file smaller than 250 MB."));
    }
    let mut bytes = Vec::new();
    file.take(MAX_ATTACHMENT_SIZE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failed("The file couldn't be read. Choose it again."))?;
    if bytes.len() as u64 > MAX_ATTACHMENT_SIZE {
        return Err(failed("Choose a file smaller than 250 MB."));
    }
    Ok((name, bytes))
}

async fn recipients<C: Credentials>(
    client: &Client<HttpTransport<C>>,
    chat: &str,
    me: &Mri,
) -> Result<(Vec<String>, Vec<Mri>), EngineError> {
    if let Some(members) = client
        .transport()
        .preflight_lab_chat(chat)
        .await
        .map_err(|_| failed("Couldn't verify everyone in this chat. Try again."))?
    {
        let people: Vec<Mri> = members
            .into_iter()
            .filter(|member| member != me.as_str())
            .map(Mri::new)
            .collect();
        let roster = client
            .transport()
            .lab_roster()
            .ok_or_else(|| failed("The implementation roster isn't available."))?;
        let addresses = people
            .iter()
            .map(|mri| {
                roster
                    .account(mri.as_str())
                    .map(|account| account.upn.clone())
                    .ok_or_else(|| failed("A recipient isn't an implementation account."))
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok((addresses, people));
    }
    let reads = ChatReads::new(client);
    let members = if one_to_one_pair(chat).is_some() {
        let thread = reads
            .thread(chat)
            .await
            .map_err(|_| failed("Couldn't read this chat's members."))?;
        parse_thread(
            chat,
            &serde_json::to_value(thread).map_err(|_| failed("Couldn't read this chat's members."))?,
        )
    } else {
        let members = reads
            .members(chat)
            .await
            .map_err(|_| failed("Couldn't read this chat's members."))?;
        parse_members(&serde_json::to_value(members).map_err(|_| failed("Couldn't read this chat's members."))?)
    };
    let Membership::Complete(members) = members else {
        return Err(failed("Couldn't read everyone in this chat. Try again."));
    };
    let people: Vec<Mri> = members
        .into_iter()
        .filter(|member| member != me && !member.is_bot())
        .collect();
    if people.is_empty() {
        return Ok((Vec::new(), people));
    }
    let lookup = ShortProfileLookup::new(people.clone()).ok_or_else(|| failed("Couldn't read file recipients."))?;
    let answer = client
        .lookup(lookup.clone())
        .await
        .map_err(|_| failed("Couldn't read file recipients."))?;
    let profiles = lookup
        .read_answer(answer.body())
        .map_err(|_| failed("Couldn't read file recipients."))?;
    let mut addresses = Vec::new();
    for person in &people {
        let profile = profiles
            .iter()
            .find(|profile| profile.mri == *person)
            .ok_or_else(|| failed("Couldn't read a file recipient."))?;
        let email = profile
            .email
            .as_ref()
            .or(profile.user_principal_name.as_ref())
            .ok_or_else(|| failed("A file recipient has no email address."))?;
        addresses.push(email.clone());
    }
    Ok((addresses, people))
}

async fn send<T: Transport<Error = HttpError>>(outbox: &Outbox<T>, write: DriveWrite) -> Result<Vec<u8>, EngineError> {
    let mut retries = CallRetries::default();
    loop {
        let tried = outbox.try_attachment(write.clone()).await;
        if let Some(wait) = retries.again(&tried) {
            tokio::time::sleep(wait).await;
            continue;
        }
        return match tried {
            CallTried::Answered(answer) if answer.is_success() => Ok(answer.body().to_vec()),
            CallTried::Answered(answer) => Err(failed(&format!(
                "The file service answered {}. Try again.",
                answer.status()
            ))),
            CallTried::Denied(reason) | CallTried::NotSent(reason) => Err(failed(&reason)),
            CallTried::Uncertain(_) => Err(failed(
                "The file service didn't confirm the upload or share. Check your connection before trying again.",
            )),
            CallTried::Throttled(_) => Err(failed("The file service is busy. Try again.")),
        };
    }
}

fn failed(why: &str) -> EngineError {
    EngineError::Attachment { why: why.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::read_file;
    #[test]
    fn local_reader_rejects_directories_missing_and_oversized_files_and_keeps_empty_files() {
        let temp = tempfile::tempdir().unwrap();
        assert!(read_file(temp.path()).is_err());
        let path = temp.path().join("demo.txt");
        assert!(read_file(&path).is_err());
        std::fs::write(&path, []).unwrap();
        let (name, bytes) = read_file(&path).unwrap();
        assert_eq!(name, "demo.txt");
        assert!(bytes.is_empty());
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(teams_proto::MAX_ATTACHMENT_SIZE + 1)
            .unwrap();
        assert!(read_file(&path).is_err());
    }
}

#[cfg(test)]
#[path = "attachments_tests.rs"]
mod integration_tests;
