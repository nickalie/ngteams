//! One call or ringing invitation across the accounts of an app.
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use teams_call::{Delivery, Observed, Tracker};
use teams_proto::Request;

#[derive(Clone, Debug, Default)]
pub struct CallFloor {
    inner: Arc<Inner>,
}
#[derive(Debug, Default)]
struct Inner {
    taking: Mutex<()>,
    accounts: Mutex<Vec<Weak<Tracker>>>,
}
fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}
impl CallFloor {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    pub fn join(&self, tracker: &Arc<Tracker>) {
        let mut accounts = lock(&self.inner.accounts);
        accounts.retain(|account| account.strong_count() > 0);
        if !accounts
            .iter()
            .filter_map(Weak::upgrade)
            .any(|account| Arc::ptr_eq(&account, tracker))
        {
            accounts.push(Arc::downgrade(tracker));
        }
    }
    #[must_use]
    pub fn others_busy(&self, tracker: &Arc<Tracker>) -> bool {
        let accounts: Vec<_> = lock(&self.inner.accounts)
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|account| !Arc::ptr_eq(account, tracker))
            .collect();
        accounts.iter().any(|account| account.holds_floor())
    }
    pub fn observe(&self, tracker: &Arc<Tracker>, delivery: &Delivery) -> Observed {
        let _held = lock(&self.inner.taking);
        tracker.observe_on_floor(delivery, self.others_busy(tracker))
    }
    #[must_use]
    pub fn allows<R: Request>(&self, tracker: &Arc<Tracker>, request: &R) -> bool {
        !Tracker::enters(request) || !self.others_busy(tracker)
    }
}

#[cfg(test)]
mod tests {
    use super::CallFloor;
    use std::sync::{Arc, Barrier};
    use teams_call::{Delivery, Observed, Tracker};
    use teams_mock::{LAB_A, LAB_B, TrouterMock};
    use teams_proto::Mri;

    #[test]
    fn account_start_binds_its_tracker_to_its_cloud() {
        use teams_auth::authz::{CHAT_BASE_GTM, Region};
        use teams_auth::secrets::MemorySecretStore;
        use teams_proto::{CallState as _, Cloud};

        let own_root = "https://calls.example:8443/trouter/v4/f/Endpoint00001/";
        let microsoft_root = "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/Endpoint00001/";
        for cloud in [Cloud::Microsoft, Cloud::own("https://calls.example:8443").unwrap()] {
            let dir = tempfile::tempdir().unwrap();
            let floor = CallFloor::new();
            let account = crate::account::Account {
                upn: "synthetic-unsigned@example.test".into(),
                mri: LAB_A.into(),
                display_name: "Lab A".into(),
                store_file: dir.path().join("account.sqlite"),
                region: Region {
                    name: Some("amer".into()),
                    partition: Some("amer-03".into()),
                    endpoints: [(
                        CHAT_BASE_GTM.into(),
                        cloud
                            .service_url(teams_mock::CHAT_BASE, "/api/chatsvc/amer")
                            .unwrap()
                            .to_string(),
                    )]
                    .into(),
                },
                secrets: MemorySecretStore::new(),
                push: false,
                known_names: Vec::new(),
                calls: None,
                read_only: false,
            };
            let engine = crate::account::start_for_cloud(account, floor.clone(), &cloud).unwrap();
            let tracker = super::lock(&floor.inner.accounts)[0].upgrade().unwrap();
            assert_eq!(tracker.connected("microsoft", microsoft_root), cloud.is_microsoft());
            assert_eq!(tracker.connected("own", own_root), !cloud.is_microsoft());
            assert!(!tracker.connected("other-port", "https://calls.example:9443/trouter/v4/f/Endpoint00001/"));
            engine.handle.shutdown();
        }
    }

    fn invitation(from: &str, to: &str) -> Delivery {
        Delivery {
            connection: format!("{to}/one"),
            id: 1,
            path: "/v4/f/SyntheticEndpoint/".to_owned(),
            trouter_request: Some("synthetic-request".to_owned()),
            chain_id: None,
            message_id: None,
            body: TrouterMock::call_invitation(from, to, |_| {}).to_string().into_bytes(),
        }
    }

    #[test]
    fn concurrent_invitations_offer_only_one_account_and_redelivery_still_passes() {
        let floor = CallFloor::new();
        let a = Arc::new(Tracker::new(Mri::new(LAB_A)));
        let b = Arc::new(Tracker::new(Mri::new(LAB_B)));
        floor.join(&a);
        floor.join(&b);
        let barrier = Arc::new(Barrier::new(2));
        let workers: Vec<_> = [(a.clone(), LAB_B, LAB_A), (b.clone(), LAB_A, LAB_B)]
            .into_iter()
            .map(|(tracker, from, to)| {
                let (floor, barrier) = (floor.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let delivery = invitation(from, to);
                    barrier.wait();
                    (tracker.clone(), delivery.clone(), floor.observe(&tracker, &delivery))
                })
            })
            .collect();
        let outcomes: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
        assert_eq!(
            outcomes
                .iter()
                .filter(|(_, _, observed)| *observed == Observed::Offered)
                .count(),
            1
        );
        let (tracker, delivery, _) = outcomes
            .iter()
            .find(|(_, _, observed)| *observed == Observed::Offered)
            .unwrap();
        assert_eq!(floor.observe(tracker, delivery), Observed::Offered);
    }

    #[test]
    fn dropping_an_account_releases_its_floor_membership() {
        let floor = CallFloor::new();
        let a = Arc::new(Tracker::new(Mri::new(LAB_A)));
        let b = Arc::new(Tracker::new(Mri::new(LAB_B)));
        floor.join(&a);
        floor.join(&b);
        assert_eq!(floor.observe(&a, &invitation(LAB_B, LAB_A)), Observed::Offered);
        assert!(floor.others_busy(&b));
        drop(a);
        assert!(!floor.others_busy(&b));
        assert_eq!(floor.observe(&b, &invitation(LAB_A, LAB_B)), Observed::Offered);
    }
}

#[cfg(test)]
mod entry_tests {
    use super::CallFloor;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use teams_call::{Delivery, Observed, Tracker};
    use teams_mock::{LAB_A, LAB_B, TrouterMock};
    use teams_proto::Mri;
    use teams_proto::{Endpoint, Method, Request, Service};

    struct Entry {
        url: reqwest::Url,
        media: bool,
    }
    impl Request for Entry {
        fn service(&self) -> Service {
            Service::Calling
        }
        fn method(&self) -> Method {
            Method::Post
        }
        fn endpoint(&self) -> Endpoint {
            Endpoint::new([] as [&str; 0])
        }
        fn body(&self) -> Option<Value> {
            self.media.then(|| json!({"callInvitation":{}}))
        }
        fn call_url(&self) -> Option<&reqwest::Url> {
            Some(&self.url)
        }
    }

    #[test]
    fn another_accounts_ring_blocks_starts_media_joins_and_answers_but_allows_followups() {
        let floor = CallFloor::new();
        let a = Arc::new(Tracker::new(Mri::new(LAB_A)));
        let b = Arc::new(Tracker::new(Mri::new(LAB_B)));
        floor.join(&a);
        floor.join(&b);
        let invitation = Delivery {
            connection: "a/one".into(),
            id: 1,
            path: "/v4/f/SyntheticEndpoint/".into(),
            trouter_request: None,
            chain_id: None,
            message_id: None,
            body: TrouterMock::call_invitation(LAB_B, LAB_A, |_| {})
                .to_string()
                .into_bytes(),
        };
        assert_eq!(floor.observe(&a, &invitation), Observed::Offered);
        for (path, media) in [
            ("epconv", false),
            ("ep/conv-host/conv/call", true),
            ("ep/cc-host/cc/v1/call/accept", false),
        ] {
            let request = Entry {
                url: reqwest::Url::parse(&format!("https://api.flightproxy.teams.microsoft.com/api/v2/{path}"))
                    .unwrap(),
                media,
            };
            assert!(!floor.allows(&b, &request), "{path}");
            assert!(
                floor.allows(&a, &request),
                "the same account retains its machine's single-call rule"
            );
        }
        let followup = Entry {
            url: reqwest::Url::parse("https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-host/conv/call/leave")
                .unwrap(),
            media: false,
        };
        assert!(floor.allows(&b, &followup));
        drop(a);
        let start = Entry {
            url: reqwest::Url::parse(teams_proto::CALL_START_URL).unwrap(),
            media: false,
        };
        assert!(floor.allows(&b, &start));
    }
}
