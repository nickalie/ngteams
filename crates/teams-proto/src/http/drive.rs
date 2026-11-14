//! The absolute, validated own-drive route; no redirects, proxies, credential forwarding or implicit write retry.

use super::{Auth, Credentials, HttpError, HttpTransport, Refusal, Response};
use crate::{DriveSite, DriveWrite, UploadedFile};

impl<C: Credentials> HttpTransport<C> {
    pub(super) async fn preflight_attachment(&self, write: &DriveWrite) -> Result<(), HttpError> {
        let refused = || HttpError::Refused(Refusal::Attachment);
        let owner = crate::object_id(&write.owner).ok_or_else(refused)?;
        if self.account.as_ref().is_none_or(|own| own.as_str() != write.owner) {
            return Err(refused());
        }
        let site = DriveSite::from_web_url_in(&self.cloud, write.url().as_str()).ok_or_else(refused)?;
        self.verify_account_actor().await?;
        let details = self.drive_details(&site).await?;
        let details: serde_json::Value = serde_json::from_slice(details.body()).map_err(|_| refused())?;
        if details.pointer("/owner/user/id").and_then(serde_json::Value::as_str) != Some(owner) {
            return Err(refused());
        }
        let grants = match &write.file {
            Some(file) => self.file_permissions(file).await?,
            None => self.chat_folder_permissions(&site).await?,
        };
        let grants: serde_json::Value = serde_json::from_slice(grants.body()).map_err(|_| refused())?;
        let grants = crate::direct_file_grants(&grants, owner).ok_or_else(refused)?;
        if write.file.is_none() {
            if grants.len() != 1 || grants[0].as_str() != write.owner {
                return Err(refused());
            }
            return Ok(());
        }
        let chat = write.conversation.as_deref().ok_or_else(refused)?;
        let members = self.attachment_recipients(chat).await?;
        let people: Vec<_> = members.iter().filter(|person| !person.is_bot()).collect();
        if !people.iter().any(|person| person.as_str() == write.owner)
            || people.len() != write.recipients.len() + 1
            || grants.iter().any(|grant| !people.contains(&grant))
            || write
                .recipients
                .iter()
                .any(|(mri, _)| mri == &write.owner || !people.iter().any(|person| person.as_str() == mri))
            || write
                .recipients
                .iter()
                .map(|(mri, _)| mri)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != write.recipients.len()
        {
            return Err(refused());
        }
        for (mri, email) in &write.recipients {
            let id = crate::object_id(mri).ok_or_else(refused)?;
            let answer = self
                .preflight_read(
                    crate::Service::Graph,
                    crate::Endpoint::new(["users", id]).with_query("$select", "id,userPrincipalName,mail"),
                )
                .await?;
            let observed: serde_json::Value = serde_json::from_slice(answer.body()).map_err(|_| refused())?;
            if observed.get("id").and_then(serde_json::Value::as_str) != Some(id)
                || !["userPrincipalName", "mail"].into_iter().any(|field| {
                    observed
                        .get(field)
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|upn| upn.eq_ignore_ascii_case(email))
                })
            {
                return Err(refused());
            }
        }
        Ok(())
    }

    /// Reads the exact chat and its full recipient list for an explicit attachment share.
    ///
    /// # Errors
    /// An incomplete or mismatched target, or a transport failure.
    pub async fn attachment_recipients(&self, chat: &str) -> Result<Vec<crate::Mri>, HttpError> {
        let refused = || HttpError::Refused(Refusal::Attachment);
        let thread = self
            .preflight_read(
                crate::Service::ChatService,
                crate::Endpoint::new(["v1", "threads", chat]).with_query("view", "msnp24Equivalent"),
            )
            .await?;
        let thread: serde_json::Value = serde_json::from_slice(thread.body()).map_err(|_| refused())?;
        if thread.get("id").and_then(serde_json::Value::as_str) != Some(chat) {
            return Err(refused());
        }
        if crate::one_to_one_pair(chat).is_some() {
            return crate::chat_members(chat, &thread, false).ok_or_else(refused);
        }
        let members = self
            .preflight_read(
                crate::Service::ChatService,
                crate::Endpoint::new(["v1", "threads", chat, "members"])
                    .with_query("view", "msnp24Equivalent")
                    .with_query("pageSize", "100"),
            )
            .await?;
        let members: serde_json::Value = serde_json::from_slice(members.body()).map_err(|_| refused())?;
        crate::chat_members(chat, &members, true).ok_or_else(refused)
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

    /// A meeting transcript's `WebVTT`, read on the personal site that holds it with that site's token
    /// ([`crate::TranscriptLocation`]).
    ///
    /// # Errors
    /// Returns transport, credentials, or unsuccessful service responses.
    pub async fn meeting_transcript(&self, transcript: &crate::TranscriptLocation) -> Result<Response, HttpError> {
        self.drive_read(transcript.site(), transcript.url()).await
    }

    /// A meeting transcript's item's transcripts, read on the personal site that holds it with that site's token: their
    /// languages ([`crate::recap::transcript_language`]).
    ///
    /// # Errors
    /// Returns transport, credentials, or unsuccessful service responses.
    pub async fn meeting_transcript_languages(
        &self,
        transcript: &crate::TranscriptLocation,
    ) -> Result<Response, HttpError> {
        self.drive_read(transcript.site(), transcript.languages_url()).await
    }

    /// A meeting recording's DASH presentation, read on the personal site that holds it with that site's token and
    /// the headers the web client's player asks it with ([`crate::RecordingManifest`],
    /// [`crate::recording::MANIFEST_HEADERS`]); the answer is read by [`crate::recording::read_manifest`].
    ///
    /// # Errors
    /// Returns transport, credentials, or unsuccessful service responses (a recording the account may not watch is a
    /// 403 or 404).
    pub async fn recording_manifest(&self, manifest: &crate::RecordingManifest) -> Result<Response, HttpError> {
        let headers: Vec<(String, String)> = crate::recording::MANIFEST_HEADERS
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        self.drive_read_with(manifest.site(), manifest.url(), &headers).await
    }

    /// A meeting recording's item, read on the personal site that holds it with that site's token
    /// ([`crate::RecordingFile`]): whether the account may download it, its name and size; the answer is read by
    /// [`crate::recording::read_recording_file`].
    ///
    /// # Errors
    /// Returns transport, credentials, or unsuccessful service responses (an item the account may not read is a 403
    /// or 404).
    pub async fn recording_file(&self, file: &crate::RecordingFile) -> Result<Response, HttpError> {
        self.drive_read(file.site(), file.url()).await
    }

    /// A meeting recording's item's expiry, read on the personal site that holds it with that site's token
    /// ([`crate::RecordingExpiration`]); the answer is read by [`crate::recording::read_expiration`].
    ///
    /// # Errors
    /// Returns transport, credentials, or unsuccessful service responses (a recording the account may not read is a
    /// 403 or 404).
    pub async fn recording_expiration(&self, read: &crate::RecordingExpiration) -> Result<Response, HttpError> {
        self.drive_read(read.site(), read.url()).await
    }

    async fn drive_read(&self, site: &DriveSite, url: &str) -> Result<Response, HttpError> {
        self.drive_read_with(site, url, &[]).await
    }

    async fn drive_read_with(
        &self,
        site: &DriveSite,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<Response, HttpError> {
        let url = url::Url::parse(url).map_err(|_| HttpError::Refused(Refusal::NotAllowed))?;
        if DriveSite::from_web_url_in(&self.cloud, url.as_str()).as_ref() != Some(site) {
            return Err(HttpError::Refused(Refusal::NotAllowed));
        }
        let audience = site.audience();
        let mut token = self
            .credentials
            .bearer(&audience)
            .await
            .map_err(HttpError::Credentials)?;
        let first = self
            .send_as(reqwest::Method::GET, &url, headers, Auth::Bearer(&token), None)
            .await?;
        let retry = match first.status() {
            401 => {
                token = self
                    .credentials
                    .bearer_rejected(&audience, &token)
                    .await
                    .map_err(HttpError::Credentials)?;
                true
            }
            status @ (429 | 503) => {
                let wait = first
                    .retry_after()
                    .unwrap_or(super::DEFAULT_RETRY_WAIT)
                    .min(self.max_retry_wait);
                log::info!(
                    "own-drive read: HTTP {status}, sent once more after {} ms",
                    wait.as_millis()
                );
                tokio::time::sleep(wait).await;
                token = self
                    .credentials
                    .bearer(&audience)
                    .await
                    .map_err(HttpError::Credentials)?;
                true
            }
            _ => false,
        };
        let answer = if retry {
            self.send_as(reqwest::Method::GET, &url, headers, Auth::Bearer(&token), None)
                .await?
        } else {
            first
        };
        if answer.is_success() {
            Ok(answer)
        } else {
            Err(HttpError::Status(Box::new(answer)))
        }
    }
    pub(super) async fn deliver_drive(&self, write: &DriveWrite) -> Result<Response, HttpError> {
        let site = DriveSite::from_web_url_in(&self.cloud, write.url().as_str())
            .ok_or(HttpError::Refused(Refusal::NotAllowed))?;
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
        let site = file
            .site_in(&self.cloud)
            .ok_or(HttpError::Refused(Refusal::NotAllowed))?;
        self.drive_read(&site, &format!("{}/permissions", file.item_url()))
            .await
    }
}
