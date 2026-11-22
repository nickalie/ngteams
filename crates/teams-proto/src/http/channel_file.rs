//! A channel file's writes on its team's site (`crate::channel_file`): each one checked again against the channel's
//! folder as the chat service names it now, sent once with the site's own token, never redirected.

use super::{Auth, Credentials, HttpError, HttpTransport, Refusal, Response};
use crate::{ChannelFileStep, ChannelFileWrite, ChannelFolder, TeamSite};

impl<C: Credentials> HttpTransport<C> {
    /// The channel's folder now, which must be the write's, and the file the write names, which must be right in it.
    pub(super) async fn preflight_channel_file(&self, write: &ChannelFileWrite) -> Result<(), HttpError> {
        let refused = || HttpError::Refused(Refusal::ChannelFile);
        let folder = write.folder();
        if !folder.site().holds(write.url()) {
            return Err(refused());
        }
        let now = self.channel_folder(folder.channel()).await?;
        if now != *folder {
            return Err(refused());
        }
        if *write.step() == ChannelFileStep::AddStub {
            return Ok(());
        }
        let file = write.file().ok_or_else(refused)?;
        let url = folder.file_read(file).ok_or_else(refused)?;
        let answer = self.team_site_read(folder.site(), &url).await?;
        let path = crate::channel_file::read_file_path(answer.body(), file).ok_or_else(refused)?;
        if !folder.holds_path(&path) {
            return Err(refused());
        }
        Ok(())
    }

    /// The folder of `channel` as its thread on the chat service names it now.
    ///
    /// # Errors
    /// Refused for a thread that names no folder of this cloud; else transport, credentials or service answers.
    pub async fn channel_folder(&self, channel: &str) -> Result<ChannelFolder, HttpError> {
        let refused = || HttpError::Refused(Refusal::ChannelFile);
        let thread = self
            .preflight_read(
                crate::Service::ChatService,
                crate::Endpoint::new(["v1", "threads", channel]).with_query("view", "msnp24Equivalent"),
            )
            .await?;
        let thread: serde_json::Value = serde_json::from_slice(thread.body()).map_err(|_| refused())?;
        ChannelFolder::from_thread_in(&self.cloud, channel, &thread).ok_or_else(refused)
    }

    pub(super) async fn deliver_team_site(&self, write: &ChannelFileWrite) -> Result<Response, HttpError> {
        let site = write.folder().site();
        if !site.holds(write.url()) {
            return Err(HttpError::Refused(Refusal::NotAllowed));
        }
        let audience = site.audience();
        let token = self
            .credentials
            .bearer(&audience)
            .await
            .map_err(HttpError::Credentials)?;
        let mut headers = vec![("accept".to_owned(), "application/json".to_owned())];
        if *write.step() == ChannelFileStep::StartUpload {
            headers.push(("content-type".to_owned(), "application/octet-stream".to_owned()));
        }
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

    async fn team_site_read(&self, site: &TeamSite, url: &url::Url) -> Result<Response, HttpError> {
        if !site.holds(url) {
            return Err(HttpError::Refused(Refusal::NotAllowed));
        }
        let headers = [("accept".to_owned(), "application/json".to_owned())];
        self.site_read(&site.audience(), url, &headers, "team-site").await
    }
}
