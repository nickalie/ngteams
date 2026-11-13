//! Public mock controls used by retry, media download and presence tests, verified through real loopback reads.
use super::{LAB_A, LAB_B, LAB_C, Lab, Method, TestResult, Value, json};
use std::time::{Duration, Instant};

#[tokio::test]
async fn an_injected_channel_reply_keeps_its_post_in_both_reads_and_deliveries() -> TestResult {
    let lab = Lab::start().await?;
    let channel = teams_mock::CHANNEL;
    let chain = format!("{channel};messageid=1760000000999");
    let id = lab
        .mock
        .add_message(&chain, (LAB_B, "Synthetic B"), "Event/Call", "synthetic call event")
        .ok_or("no channel message")?;
    let message = lab.message(LAB_A, channel, &id).await?;
    assert_eq!(message["conversationid"], channel);
    assert_eq!(
        message["conversationLink"],
        format!("{}/v1/users/ME/conversations/{chain}", teams_mock::CHAT_BASE)
    );
    let event = lab.mock.take_events(LAB_A).pop().ok_or("no delivery")?;
    assert_eq!(event.conversation, channel);
    assert_eq!(event.resource, message);
    assert!(lab.mock.take_events(LAB_B).is_empty());
    Ok(())
}

#[tokio::test]
async fn ranged_files_preserve_bytes_and_one_shot_refusals_do_not_consume_other_paths() -> TestResult {
    let lab = Lab::start().await?;
    lab.mock
        .serve_ranged("/synthetic.bin", "application/octet-stream", b"0123456789".to_vec())
        .await;
    let url = format!("{}/synthetic.bin", lab.mock.origin());
    lab.mock.refuse_next("GET", "synthetic.bin", 503).await;
    assert_eq!(
        lab.http
            .get(format!("{}/elsewhere", lab.mock.origin()))
            .send()
            .await?
            .status(),
        401
    );
    assert_eq!(lab.http.get(&url).send().await?.status(), 503);
    for (range, status, content, bytes) in [
        (None, 200, None, "0123456789"),
        (Some("nonsense"), 200, None, "0123456789"),
        (Some("bytes=2-4"), 206, Some("bytes 2-4/10"), "234"),
        (Some("bytes=8-99"), 206, Some("bytes 8-9/10"), "89"),
        (Some("bytes=10-20"), 416, None, ""),
        (Some("bytes=4-2"), 416, None, ""),
    ] {
        let mut request = lab.http.get(&url);
        if let Some(range) = range {
            request = request.header("Range", range);
        }
        let answer = request.send().await?;
        assert_eq!(answer.status(), status, "{range:?}");
        assert_eq!(
            answer.headers().get("content-range").map(|v| v.to_str().unwrap()),
            content
        );
        assert_eq!(answer.text().await?, bytes);
    }
    lab.mock
        .serve_ranged("/empty.bin", "application/octet-stream", Vec::new())
        .await;
    assert_eq!(
        lab.http
            .get(format!("{}/empty.bin", lab.mock.origin()))
            .header("Range", "bytes=0-0")
            .send()
            .await?
            .status(),
        416
    );
    Ok(())
}

#[tokio::test]
async fn delayed_and_overridden_answers_apply_once_then_return_to_the_unchanged_world() -> TestResult {
    let lab = Lab::start().await?;
    let path = "users/ME/conversations";
    let baseline = lab.get(LAB_A, path).await?["conversations"].clone();
    let delay = Duration::from_millis(30);
    lab.mock.refuse_next_after("GET", "conversations", 429, delay).await;
    let started = Instant::now();
    assert_eq!(lab.send(LAB_A, Method::GET, path, None).await?.status, 429);
    assert!(started.elapsed() >= delay);
    assert_eq!(lab.get(LAB_A, path).await?["conversations"], baseline);
    lab.mock.answer_next_after("GET", "conversations", delay).await;
    let started = Instant::now();
    assert_eq!(lab.get(LAB_A, path).await?["conversations"], baseline);
    assert!(started.elapsed() >= delay);
    assert_eq!(lab.get(LAB_A, path).await?["conversations"], baseline);
    let overridden = json!({"synthetic": "unreadable by the client"});
    lab.mock.answer_next("GET", "conversations", 202, &overridden).await;
    let answer = lab.send(LAB_A, Method::GET, path, None).await?;
    assert_eq!(answer.status, 202);
    assert_eq!(answer.json()?, overridden);
    assert_eq!(lab.get(LAB_A, path).await?["conversations"], baseline);
    Ok(())
}

#[tokio::test]
async fn picture_upload_controls_replay_the_object_creation_put_and_download() -> TestResult {
    let lab = Lab::start().await?;
    let bytes = b"synthetic picture bytes";
    lab.mock.serve_picture_sends("synthetic-object", bytes.to_vec()).await;
    let base = lab.mock.origin();
    let created = lab.http.post(format!("{base}/v1/objects/")).send().await?;
    assert_eq!(created.status(), 201);
    assert_eq!(
        serde_json::from_str::<Value>(&created.text().await?)?,
        json!({"id": "synthetic-object"})
    );
    let put = lab
        .http
        .put(format!("{base}/v1/objects/synthetic-object/content/imgpsh"))
        .body(bytes.to_vec())
        .send()
        .await?;
    assert_eq!(put.status(), 201);
    let image = lab
        .http
        .get(format!("{base}/v1/objects/synthetic-object/views/imgo"))
        .send()
        .await?;
    assert_eq!(image.status(), 200);
    assert_eq!(image.headers()["content-type"], "image/png");
    assert_eq!(image.bytes().await?.as_ref(), bytes);
    let requests = lab.mock.received_requests().await;
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].body, bytes);
    Ok(())
}

#[tokio::test]
async fn presence_controls_report_unknown_people_offline_and_distinguish_polling_subscriptions() -> TestResult {
    let lab = Lab::start().await?;
    lab.mock.serve_presence(&[(LAB_A, "Available")]).await;
    lab.mock.serve_presence_subscriptions(&[LAB_C]).await;
    let base = format!("{}/ups/noam/v1", lab.mock.origin());
    let answer = lab
        .send_raw(
            Method::POST,
            &format!("{base}/presence/getpresence/"),
            &[],
            Some(&json!([
                {"mri": LAB_A}, {"mri": LAB_B}, {"no-mri": "ignored"}
            ])),
        )
        .await?
        .json()?;
    assert_eq!(answer.as_array().map(Vec::len), Some(2));
    assert_eq!(answer[0]["presence"]["availability"], "Available");
    assert_eq!(answer[1]["presence"]["availability"], "Offline");
    let subscription = format!("{base}/pubsub/subscriptions/00000000-0000-4000-8000-000000000001");
    let body = json!({
        "trouterUri": "https://synthetic.invalid/endpoint",
        "shouldPurgePreviousSubscriptions": true,
        "subscriptionsToAdd": [{"mri": LAB_A}, {"mri": LAB_C}, {"missing": "ignored"}],
        "subscriptionsToRemove": [{"mri": LAB_B}]
    });
    let answer = lab
        .send_raw(Method::POST, &subscription, &[], Some(&body))
        .await?
        .json()?;
    assert_eq!(
        answer["addStatuses"],
        json!([
            {"mri": LAB_A, "source": "ups", "status": 20200},
            {"mri": LAB_C, "source": "ups", "status": 30010}
        ])
    );
    assert_eq!(
        answer["removeStatuses"],
        json!([{"mri": LAB_B, "source": "ups", "status": 20200}])
    );
    for invalid in ["not-json".to_owned(), "[]".to_owned(), "{}".to_owned(), {
        let mut extra = body.clone();
        extra["extra"] = json!(true);
        extra.to_string()
    }] {
        assert_eq!(lab.http.post(&subscription).body(invalid).send().await?.status(), 400);
    }
    Ok(())
}

#[tokio::test]
async fn injected_messages_keep_the_original_when_copied_and_queue_only_for_other_members() -> TestResult {
    let lab = Lab::start().await?;
    let group = lab
        .mock
        .add_group("Synthetic group", &[LAB_A, LAB_B])
        .ok_or("no group")?;
    let id = lab
        .mock
        .add_message(&group, (LAB_A, "Synthetic A"), "Text", "first")
        .ok_or("no message")?;
    let message = lab.message(LAB_B, &group, &id).await?;
    let client_id = message["clientmessageid"].as_str().ok_or("no client id")?;
    assert!(lab.mock.take_events(LAB_A).is_empty());
    let events = lab.mock.take_events(LAB_B);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].resource, message);
    assert!(
        lab.mock
            .change_message(&group, client_id, |message| message["content"] = json!("changed"))
    );
    assert_eq!(lab.message(LAB_B, &group, &id).await?["content"], "changed");
    assert!(lab.mock.copy_message(&group, client_id, |message| {
        message["id"] = json!("synthetic-copy");
        message["content"] = json!("copy");
    }));
    assert_eq!(lab.message(LAB_B, &group, &id).await?["content"], "changed");
    assert_eq!(lab.message(LAB_B, &group, "synthetic-copy").await?["content"], "copy");
    for chat in [&group, "no-such-chat"] {
        assert!(
            !lab.mock
                .change_message(chat, "no-such-message", |_| panic!("missing message changed"))
        );
        assert!(
            !lab.mock
                .copy_message(chat, "no-such-message", |_| panic!("missing message copied"))
        );
    }
    assert!(
        lab.mock
            .add_message("no-such-chat", (LAB_A, "Synthetic A"), "Text", "missing")
            .is_none()
    );
    let own = lab
        .mock
        .post_as(LAB_A, &group, "<p>another</p>")
        .ok_or("post refused")?;
    assert_eq!(lab.message(LAB_B, &group, &own).await?["content"], "<p>another</p>");
    let event = lab.mock.take_events(LAB_B).pop().ok_or("no event")?;
    lab.mock.queue_event(LAB_B, event.clone());
    assert_eq!(lab.mock.take_events(LAB_B), [event]);
    Ok(())
}

#[tokio::test]
async fn ending_one_ring_preserves_other_calls_and_a_group_remains_joinable_until_hangup() -> TestResult {
    use teams_mock::{ABC_CHAT, CallPhase, TIMED_OUT};
    let lab = Lab::start().await?;
    let one = lab.mock.ring(LAB_A, LAB_B);
    let group = lab.mock.ring_group(LAB_A, ABC_CHAT, &[LAB_B, LAB_C]);
    let calls = lab.mock.calls();
    assert_ne!(one, group);
    assert_eq!(calls[0].phase, CallPhase::Ringing);
    assert_eq!(
        calls[1].legs.iter().map(|leg| leg.callee.as_str()).collect::<Vec<_>>(),
        [LAB_B, LAB_C]
    );
    assert!(lab.mock.end_ring(&one, 408, TIMED_OUT));
    let calls = lab.mock.calls();
    assert_eq!(calls[0].phase, CallPhase::Ended);
    assert_eq!(calls[1].phase, CallPhase::Ringing);
    assert!(lab.mock.end_ring(&group, 408, TIMED_OUT));
    assert_eq!(lab.mock.calls()[1].phase, CallPhase::Connected);
    assert!(lab.mock.hang_up_call(&group));
    assert!(lab.mock.calls().iter().all(|call| call.phase == CallPhase::Ended));
    assert!(!lab.mock.end_ring("no-call", 408, TIMED_OUT));
    assert!(!lab.mock.hang_up_call("no-call"));
    Ok(())
}

async fn channel_request(lab: &Lab, method: Method, path: &str, body: &Value) -> TestResult<super::Answer> {
    let token = format!("Bearer {}", lab.mock.token(LAB_A).ok_or("no synthetic token")?);
    lab.send_raw(
        method,
        &format!("{}{path}", lab.mock.origin()),
        &[("Authorization", &token)],
        Some(body),
    )
    .await
}

#[tokio::test]
async fn channel_controls_record_successful_writes_and_report_duplicate_names_and_member_failures() -> TestResult {
    let lab = Lab::start().await?;
    let standard = "/api/mt/part/amer-03/beta/teams/synthetic-team/channels";
    let body = json!({"displayName": "Synthetic standard"});
    let first = channel_request(&lab, Method::POST, standard, &body).await?;
    assert_eq!(first.status, 201);
    assert!(first.json()?["value"]["objectId"].as_str().is_some());
    let duplicate = channel_request(&lab, Method::POST, standard, &body).await?;
    assert_eq!(duplicate.status, 409);
    assert_eq!(duplicate.json()?["errorCode"], "ChannelNameAlreadyExist");
    let private = "/fabric/amer/templates/api/teams/synthetic-group/channels";
    let made = channel_request(
        &lab,
        Method::POST,
        private,
        &json!({
            "displayName": "Synthetic private", "spaceThreadId": "synthetic-team"
        }),
    )
    .await?;
    assert_eq!(made.status, 202);
    let group = made.json()?["groupId"].as_str().ok_or("no private group")?.to_owned();
    let poll = channel_request(&lab, Method::GET, &format!("{private}/{group}"), &Value::Null).await?;
    assert_eq!(poll.status, 200);
    assert!(poll.json()?["threadId"].as_str().is_some());
    assert_eq!(
        channel_request(&lab, Method::GET, &format!("{private}/no-group"), &Value::Null)
            .await?
            .status,
        404
    );
    let added = channel_request(
        &lab,
        Method::POST,
        &format!("{private}/{group}/users"),
        &json!({
            "users": [{"id": LAB_B}, {"id": teams_mock::NOT_ADDED}]
        }),
    )
    .await?;
    assert_eq!(
        added.json()?["multipleStatus"],
        json!([{
            "id": teams_mock::NOT_ADDED, "statusSubCode": "UserNotFound"
        }])
    );
    let found = channel_request(
        &lab,
        Method::POST,
        "/api/userprofilesvc/amer/api/v1.0/teams/synthetic-team/users/profilesearch",
        &json!({"keyword": "lab b"}),
    )
    .await?;
    assert_eq!(found.json()?[0]["mri"], LAB_B);
    assert_eq!(
        channel_request(
            &lab,
            Method::PUT,
            "/api/mt/part/amer-03/beta/teams/synthetic-channel/setDefaultChannel",
            &json!({"favorite": true})
        )
        .await?
        .status,
        200
    );
    let writes = lab.mock.channel_writes();
    assert_eq!(writes.len(), 4);
    assert_eq!(writes[0].body, body);
    assert_eq!(writes[2].method, "POST");
    assert_eq!(writes[3].method, "PUT");
    Ok(())
}
