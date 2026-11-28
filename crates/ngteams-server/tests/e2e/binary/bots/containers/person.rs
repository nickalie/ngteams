//! A person signed in at one node over TLS, with the client's own engine on the HTTPS transport an own server's
//! account gets: every service base from authz, pinned to that node.

use std::sync::Arc;
use std::time::Duration;

use teams_auth::secrets::MemorySecretStore;
use teams_auth::session::Session;
use teams_core::Outbox;
use teams_core::account::{AMS_GTM, CSA_GTM, MIDDLE_TIER_GTM, PROVISIONING_GTM, USER_PROFILE_GTM};
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Parts};
use teams_proto::chat::MessageText;
use teams_proto::{Author, Client, Cloud, HttpTransport, Mri, NewTeam, TeamRole, TeamWrite};
use teams_store::{OutboxState, Store};

use super::super::super::support::Node;
use crate::support::PASSWORD;

type Transport = HttpTransport<Session<MemorySecretStore>>;

pub struct Person {
    pub mri: String,
    engine: Option<EngineHandle>,
    _dir: tempfile::TempDir,
}

impl Person {
    pub async fn sign_in(node: &Node, upn: &str) -> Self {
        let session = Arc::new(node.sign_in(upn, PASSWORD).await);
        let skype = session.skypetoken().await.unwrap();
        let mri = format!("8:{}", skype.skype_id.as_deref().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let engine = engine::start(Parts {
            store: Store::open(&dir.path().join("account.sqlite")).unwrap(),
            client: Arc::new(Client::new(transport(&session, node, &mri))),
            outbox: Outbox::new(
                Arc::new(transport(&session, node, &mri)),
                Author::new(Mri::new(&mri), upn),
            ),
            calls: None,
            push: None,
            options: EngineOptions {
                tick: Duration::from_millis(20),
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })
        .unwrap();
        Self {
            mri,
            engine: Some(engine),
            _dir: dir,
        }
    }

    pub fn engine(&self) -> &EngineHandle {
        self.engine.as_ref().unwrap()
    }

    pub async fn sync(&self) {
        let report = self.engine().sync().await.unwrap();
        assert!(report.failed.is_empty(), "{report:?}");
    }

    /// A message the service took; its id.
    pub async fn send(&self, conversation: &str, text: &str) -> String {
        self.write(Action::Send {
            conversation: conversation.into(),
            text: MessageText::plain(text).unwrap(),
        })
        .await
    }

    /// A write the service took; the id it made.
    pub async fn write(&self, action: Action) -> String {
        let row = self.engine().write(action).await.unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        row.message_id.unwrap()
    }

    /// Makes a private team with `others` as members; its thread.
    pub async fn make_team(&self, name: &str, others: &[&Self]) -> String {
        let create = TeamWrite::Create(NewTeam {
            name: name.into(),
            description: format!("{name}'s people"),
            first_channel: "General".into(),
            public: false,
        });
        let written = self.engine().team_write(create, None).await.unwrap();
        let (team, group) = written.created.unwrap();
        let add = TeamWrite::AddMembers {
            team: team.clone(),
            group: group.unwrap(),
            members: others
                .iter()
                .map(|person| (Mri::new(&person.mri), TeamRole::Member))
                .collect(),
        };
        let written = self.engine().team_write(add, None).await.unwrap();
        assert!(written.not_added.is_empty(), "{written:?}");
        team
    }
}

impl Drop for Person {
    fn drop(&mut self) {
        if let Some(engine) = self.engine.take() {
            engine.shutdown();
        }
    }
}

fn transport(session: &Arc<Session<MemorySecretStore>>, node: &Node, mri: &str) -> Transport {
    // authz names the services on the public origin; this person stays on its own node
    let pinned = |gtm: &str| {
        let base = session
            .service_base(gtm)
            .unwrap_or_else(|| panic!("authz names no {gtm}"));
        format!("{}{}", node.endpoint, url::Url::parse(&base).unwrap().path())
    };
    HttpTransport::for_cloud(
        Arc::clone(session),
        &pinned(teams_auth::authz::CHAT_BASE_GTM),
        Cloud::own(&node.endpoint).unwrap(),
    )
    .unwrap()
    .with_ams(&pinned(AMS_GTM))
    .unwrap()
    .for_account(Mri::new(mri))
    .with_middle_tier(&pinned(MIDDLE_TIER_GTM))
    .unwrap()
    .with_csa(&pinned(CSA_GTM))
    .unwrap()
    .with_provisioning(&pinned(PROVISIONING_GTM))
    .unwrap()
    .with_user_profile(&pinned(USER_PROFILE_GTM))
    .unwrap()
}
