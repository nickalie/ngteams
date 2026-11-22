//! Files in a channel: the real own-cloud engine uploads one into the channel's folder on its team's site (`addStub`,
//! `StartUpload`, `FinishUpload`), posts it, and another member of the team reads it through Graph by its address.
//! Someone outside the team gets 403 on every step, and a folder that isn't a channel's is refused.

use teams_core::ChannelDraft;
use teams_core::engine::Action;
use teams_proto::chat::{ChatReads, MessageText};
use teams_proto::{FileDownload, FileThumbnails, NewChannelKind, UploadedFile};
use teams_store::OutboxState;

use super::{ORIGIN, cloud, png, raw, status, token};
use crate::chatsvc::activity::RawPost;
use crate::support::{Backend, TestServer, backends};
use crate::teams::{Seat, new_channel};

/// Ada's team with Bert on it, and Cora outside it.
struct Crew {
    server: TestServer,
    ada: Seat,
    bert: Seat,
    cora: Seat,
    team: String,
    group: String,
    _blobs: tempfile::TempDir,
    files: tempfile::TempDir,
}

impl Crew {
    async fn start(backend: Backend) -> Self {
        Self::start_with(backend, |_| {}).await
    }

    async fn start_with(backend: Backend, configure: impl FnOnce(&mut ngteams_server::config::Config)) -> Self {
        let (server, blobs) = super::server(backend, configure).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        Self {
            server,
            ada,
            bert,
            cora,
            team,
            group,
            _blobs: blobs,
            files: tempfile::tempdir().unwrap(),
        }
    }

    fn site(&self) -> String {
        format!("/sp/sites/{}", self.group)
    }

    fn file(&self, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = self.files.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    async fn upload(&self, seat: &Seat, channel: &str, name: &str, bytes: &[u8]) -> UploadedFile {
        seat.engine
            .upload_channel_file(channel.to_owned(), self.file(name, bytes))
            .await
            .unwrap()
    }

    /// Posts `file` in `channel` as Ada; the file's address as Bert reads it from the post.
    async fn post(&self, channel: &str, file: UploadedFile) -> String {
        let text = MessageText::plain("the file")
            .unwrap()
            .with_files(vec![file])
            .for_cloud(&cloud())
            .unwrap();
        let row = self
            .ada
            .engine
            .write(Action::Send {
                conversation: channel.to_owned(),
                text,
            })
            .await
            .unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        let page = ChatReads::new(&*self.bert.client).messages(channel, 1).await.unwrap();
        let files = page.messages[0].files_in(&cloud());
        assert!(files.unread.is_none(), "{:?}", files.unread);
        assert_eq!(files.files.len(), 1);
        files.files[0].open_link().unwrap().to_owned()
    }

    /// A raw request on the site's REST API as `upn`: the status and the body.
    async fn rest(
        &self,
        upn: &str,
        method: reqwest::Method,
        rest: &str,
        body: Option<Vec<u8>>,
    ) -> (u16, serde_json::Value) {
        let token = token(&self.server, upn, ORIGIN).await;
        let path = format!("{}/_api/web/{rest}", self.site());
        let (status, _, body) = raw(&self.server, method, &path, Some(&token), &[], body).await;
        (status, serde_json::from_slice(&body).unwrap_or_default())
    }

    async fn stop(self) {
        self.ada.stop();
        self.bert.stop();
        self.cora.stop();
        self.server.stop().await;
    }
}

/// `addStub`'s path and query for `name` in the folder at `folder`, as the client encodes them.
fn add_stub(folder: &str, name: &str) -> String {
    let odata = |text: &str| {
        let quoted = format!("'{}'", text.replace('\'', "''"));
        percent_encoding::utf8_percent_encode(&quoted, percent_encoding::NON_ALPHANUMERIC).to_string()
    };
    format!(
        "GetFolderByServerRelativeUrl(@v)/Files/addStub(@fn)?@v={}&@fn={}",
        odata(folder),
        odata(name)
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_uploads_a_channel_file_and_another_member_reads_it_through_graph() {
    for backend in backends() {
        let crew = Crew::start(backend).await;
        let folder = crew.bert.client.transport().channel_folder(&crew.team).await.unwrap();
        assert_eq!(folder.path(), format!("{}/Shared Documents/General", crew.site()));
        assert_eq!(folder.site().url(), format!("{ORIGIN}{}", crew.site()));

        let bytes = png(1200, 600);
        let file = crew.upload(&crew.ada, &crew.team, "photo.png", &bytes).await;
        assert_eq!((file.name(), file.size()), ("photo.png", bytes.len() as u64));
        assert_eq!(file.channel(), Some(crew.team.as_str()));
        let link = crew.post(&crew.team, file).await;
        assert_eq!(
            link,
            format!("{ORIGIN}{}/Shared Documents/General/photo.png", crew.site())
        );

        assert!(crew.bert.engine.sync().await.unwrap().failed.is_empty());
        assert_eq!(
            crew.bert.engine.file_size(link.clone()).await.unwrap(),
            bytes.len() as u64
        );
        let part = crew.bert.engine.file_range(link.clone(), 100, 50).await.unwrap();
        assert_eq!(part.bytes.as_slice(), &bytes[100..150]);
        let thumbnail = crew.bert.engine.file_thumbnail(link.clone()).await.unwrap().unwrap();
        let picture = image::load_from_memory(&thumbnail.bytes).unwrap();
        assert_eq!((picture.width(), picture.height()), (800, 400));

        for refused in [
            crew.cora
                .client
                .read(&FileDownload::of_in(&cloud(), &link).unwrap())
                .await,
            crew.cora
                .client
                .read(&FileThumbnails::of_in(&cloud(), &link).unwrap())
                .await,
        ] {
            assert_eq!(status(&refused.unwrap_err()), Some(403));
        }
        crew.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_file_of_the_same_name_is_numbered_and_a_removed_one_is_gone() {
    let crew = Crew::start(Backend::Sqlite).await;
    let channel = crew.ada.make_channel(&crew.team, &crew.group, "Launch", true).await;
    let first = crew.upload(&crew.ada, &channel, "notes.txt", b"first").await;
    let second = crew.upload(&crew.bert, &channel, "notes.txt", b"second file").await;
    assert_eq!((first.name(), second.name()), ("notes.txt", "notes 1.txt"));
    assert_eq!(
        second.entry()["objectUrl"],
        format!("{ORIGIN}{}/Shared Documents/Launch/notes 1.txt", crew.site())
    );

    let link = crew.post(&channel, first.clone()).await;
    assert_eq!(
        crew.bert.engine.file_range(link.clone(), 0, 64).await.unwrap().bytes,
        b"first"
    );
    let read = format!("GetFileById('{}')", first.unique_id());
    let (status, body) = crew.rest("bert@own.test", reqwest::Method::GET, &read, None).await;
    assert_eq!(status, 200);
    assert_eq!(
        body["ServerRelativeUrl"],
        format!("{}/Shared Documents/Launch/notes.txt", crew.site())
    );

    crew.ada.engine.discard_channel_file(first.clone()).await.unwrap();
    let gone = crew
        .bert
        .client
        .read(&FileDownload::of_in(&cloud(), &link).unwrap())
        .await;
    assert_eq!(super::status(&gone.unwrap_err()), Some(404));
    let (status, _) = crew.rest("ada@own.test", reqwest::Method::GET, &read, None).await;
    assert_eq!(status, 404);
    // the name is free again
    let again = crew.upload(&crew.ada, &channel, "notes.txt", b"third").await;
    assert_eq!(again.name(), "notes.txt");
    crew.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn someone_outside_the_team_is_refused_every_step() {
    let crew = Crew::start(Backend::Sqlite).await;
    let file = crew.upload(&crew.ada, &crew.team, "plan.txt", b"made-up plan").await;
    let folder = format!("{}/Shared Documents/General", crew.site());
    let id = file.unique_id();
    let upload = "1c8e6b9d-0000-4000-8000-00000000000b";
    let steps = [
        (reqwest::Method::POST, add_stub(&folder, "mine.txt"), None),
        (
            reqwest::Method::POST,
            format!("GetFileById('{id}')/StartUpload(uploadId=guid'{upload}')"),
            Some(b"not mine".to_vec()),
        ),
        (
            reqwest::Method::POST,
            format!("GetFileById('{id}')/FinishUpload(uploadId=guid'{upload}',fileOffset=8)"),
            None,
        ),
        (
            reqwest::Method::GET,
            format!("GetFileById('{id}')?$select=ServerRelativeUrl,UniqueId"),
            None,
        ),
        (reqwest::Method::DELETE, format!("GetFileById('{id}')/recycle"), None),
    ];
    for (method, rest, body) in steps {
        let (status, _) = crew.rest("cora@own.test", method, &rest, body).await;
        assert_eq!(status, 403, "{rest}");
    }
    // a site no team has
    let token = token(&crew.server, "cora@own.test", ORIGIN).await;
    let path = format!("/sp/sites/00000000-0000-4000-8000-000000000000/_api/web/GetFileById('{id}')");
    let (status, _, _) = raw(&crew.server, reqwest::Method::GET, &path, Some(&token), &[], None).await;
    assert_eq!(status, 404);
    // the file is still whole for the team
    let (status, body) = crew
        .rest(
            "bert@own.test",
            reqwest::Method::GET,
            &format!("GetFileById('{id}')"),
            None,
        )
        .await;
    assert_eq!((status, body["Length"].as_str()), (200, Some("12")));
    crew.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_that_isnt_a_channels_is_refused_and_a_taken_name_answers_sharepoints_error() {
    let crew = Crew::start(Backend::Sqlite).await;
    let site = crew.site();
    for folder in [
        format!("{site}/Shared Documents/Elsewhere"),
        format!("{site}/Shared Documents/General/inner"),
        format!("{site}/Documents/General"),
        "/sp/sites/other/Shared Documents/General".to_owned(),
    ] {
        let (status, _) = crew
            .rest("ada@own.test", reqwest::Method::POST, &add_stub(&folder, "a.txt"), None)
            .await;
        assert_eq!(status, 404, "{folder}");
    }
    let general = format!("{site}/Shared Documents/General");
    let (status, _) = crew
        .rest(
            "ada@own.test",
            reqwest::Method::POST,
            &add_stub(&general, "a#b.txt"),
            None,
        )
        .await;
    assert_eq!(status, 400);

    let (status, body) = crew
        .rest(
            "ada@own.test",
            reqwest::Method::POST,
            &add_stub(&general, "Ada's.txt"),
            None,
        )
        .await;
    assert_eq!(
        (status, body["Name"].as_str(), body["Length"].as_str()),
        (200, Some("Ada's.txt"), Some("0"))
    );
    let (status, body) = crew
        .rest(
            "bert@own.test",
            reqwest::Method::POST,
            &add_stub(&general, "Ada's.txt"),
            None,
        )
        .await;
    assert_eq!(status, 400);
    assert!(
        body["odata.error"]["code"]
            .as_str()
            .unwrap()
            .starts_with(teams_proto::channel_file::NAME_TAKEN)
    );
    // only the account that added the empty file fills it
    let id = body_id(&crew, &general, "Ada's.txt").await;
    let upload = "1c8e6b9d-0000-4000-8000-00000000000b";
    let start = format!("GetFileById('{id}')/StartUpload(uploadId=guid'{upload}')");
    let (status, _) = crew
        .rest("bert@own.test", reqwest::Method::POST, &start, Some(b"bert".to_vec()))
        .await;
    assert_eq!(status, 403);
    let finish = format!("GetFileById('{id}')/FinishUpload(uploadId=guid'{upload}',fileOffset=3)");
    let (status, body) = crew
        .rest("ada@own.test", reqwest::Method::POST, &start, Some(b"ada".to_vec()))
        .await;
    assert_eq!((status, body["value"].as_str()), (200, Some("3")));
    let (status, _) = crew
        .rest("ada@own.test", reqwest::Method::POST, &start, Some(b"again".to_vec()))
        .await;
    assert_eq!(status, 400);
    let (status, body) = crew.rest("ada@own.test", reqwest::Method::POST, &finish, None).await;
    assert_eq!((status, body["Length"].as_str()), (200, Some("3")));
    crew.stop().await;
}

/// The id of the file `name` in the folder at `folder`, as Ada lists it through its upload's answer.
async fn body_id(crew: &Crew, folder: &str, name: &str) -> String {
    let link = format!("{ORIGIN}{folder}/{name}");
    let share = FileDownload::of_in(&cloud(), &link).unwrap();
    let answer = crew.ada.client.read(&share).await.unwrap();
    let item: serde_json::Value = serde_json::from_slice(answer.body()).unwrap();
    item["sharepointIds"]["listItemUniqueId"].as_str().unwrap().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_private_channels_files_stay_with_its_members() {
    let crew = Crew::start(Backend::Sqlite).await;
    let draft = ChannelDraft {
        channel: new_channel(&crew.team, &crew.group, "Secret", NewChannelKind::Private),
        show_by_default: false,
    };
    let secret = crew.ada.engine.create_channel(draft).await.unwrap().thread.unwrap();
    let file = crew.upload(&crew.ada, &secret, "secret.txt", b"made-up secret").await;
    let link = file.entry()["objectUrl"].as_str().unwrap().to_owned();
    assert!(link.ends_with("/Shared Documents/Secret/secret.txt"), "{link}");
    let read = crew
        .bert
        .client
        .read(&FileDownload::of_in(&cloud(), &link).unwrap())
        .await;
    assert_eq!(status(&read.unwrap_err()), Some(403));
    let folder = format!("{}/Shared Documents/Secret", crew.site());
    let (status, _) = crew
        .rest(
            "bert@own.test",
            reqwest::Method::POST,
            &add_stub(&folder, "mine.txt"),
            None,
        )
        .await;
    assert_eq!(status, 403);
    crew.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_over_the_limit_is_refused_and_its_empty_file_recycled() {
    let crew = Crew::start_with(Backend::Sqlite, |config| config.media.max_file_bytes = 8).await;
    let refused = crew
        .ada
        .engine
        .upload_channel_file(crew.team.clone(), crew.file("big.txt", b"more than eight bytes"))
        .await;
    assert!(
        matches!(&refused, Err(teams_core::EngineError::Attachment { why }) if why.contains("too large to upload")),
        "{refused:?}"
    );
    let small = crew.upload(&crew.ada, &crew.team, "big.txt", b"small").await;
    assert_eq!(small.name(), "big.txt");
    crew.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_channel_file_shared_on_to_a_chat_gives_nobody_outside_the_channel_access() {
    let crew = Crew::start(Backend::Sqlite).await;
    let file = crew.upload(&crew.ada, &crew.team, "plan.txt", b"made-up plan").await;
    let link = crew.post(&crew.team, file.clone()).await;
    let row = crew
        .ada
        .engine
        .write(Action::CreateChat {
            others: vec![teams_proto::Mri::new(&crew.cora.mri)],
            topic: None,
        })
        .await
        .unwrap();
    let chat = row.message_id.unwrap();
    // the file's entry posted again, as "Share this file" posts it
    let files = serde_json::Value::String(serde_json::json!([file.entry()]).to_string());
    let post = RawPost {
        chat,
        body: serde_json::json!({"messagetype": "RichText/Html", "content": "<p>have a look</p>",
            "clientmessageid": "4242", "properties": {"files": files}}),
    };
    assert_eq!(crew.ada.client.write(post).await.unwrap().status(), 201);
    let read = crew
        .cora
        .client
        .read(&FileDownload::of_in(&cloud(), &link).unwrap())
        .await;
    assert_eq!(status(&read.unwrap_err()), Some(403));
    crew.stop().await;
}
