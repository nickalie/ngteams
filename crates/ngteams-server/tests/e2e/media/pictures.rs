//! A picture pasted into a message: made and filled by one engine, read by the chat's other member.

use teams_core::engine::Action;
use teams_proto::chat::{MessageText, Node, RichText};
use teams_proto::{MessagePicture, PastedFormat, PastedPicture, sent_picture_in};
use teams_store::OutboxState;

use super::{Trio, cloud, png, status};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_pastes_a_picture_and_only_the_chats_members_read_it() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let bytes = png(48, 32);
        let picture = PastedPicture {
            format: PastedFormat::Png,
            bytes: bytes.clone(),
        };
        let object = trio.a.upload_picture(trio.chat.clone(), picture).await.unwrap();
        let image = sent_picture_in(&cloud(), &object, (48, 32)).unwrap();
        let src = image.src.clone();
        let text = RichText {
            nodes: vec![Node::Paragraph(vec![Node::Image(image)])],
            truncated: false,
        };
        let row = trio
            .a
            .write(Action::Send {
                conversation: trio.chat.clone(),
                text: MessageText::new_in(&cloud(), text, []).unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        assert!(trio.b.sync().await.unwrap().failed.is_empty());
        let message = trio
            .b
            .message(&trio.chat, row.message_id.unwrap())
            .await
            .unwrap()
            .unwrap();
        assert!(message.content.contains(&src), "{}", message.content);
        let read = trio.b.message_picture(src.clone()).await.unwrap().unwrap();
        assert_eq!(read.bytes.as_slice(), bytes.as_slice());

        let read = MessagePicture::of_in(&cloud(), &src).unwrap();
        let outsider = trio.outsider().await;
        assert_eq!(status(&outsider.read(&read).await.unwrap_err()), Some(403));
        let anonymous = reqwest::Client::new()
            .get(trio.server.url(&format!("/ams/v1/objects/{object}/views/imgo")))
            .send()
            .await
            .unwrap();
        assert_eq!(anonymous.status(), 401);
        trio.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_picture_pasted_in_a_channel_is_read_by_the_channels_people_alone() {
    use crate::teams::Seat;

    let (server, _blobs) = super::server(crate::support::Backend::Sqlite, |_| {}).await;
    let ada = Seat::new(&server, "ada@own.test", "Ada").await;
    let bert = Seat::new(&server, "bert@own.test", "Bert").await;
    let cora = Seat::new(&server, "cora@own.test", "Cora").await;
    let (team, group) = ada.make_team("Rocket", false).await;
    ada.add(&team, &group, &[&bert]).await;
    let channel = ada.make_channel(&team, &group, "Launch", true).await;

    let dir = tempfile::tempdir().unwrap();
    let sender = super::engine(&server, "ada@own.test", &ada.id, dir.path()).await;
    let bytes = png(30, 20);
    let picture = PastedPicture {
        format: PastedFormat::Png,
        bytes: bytes.clone(),
    };
    let object = sender.upload_picture(channel, picture).await.unwrap();
    let src = sent_picture_in(&cloud(), &object, (30, 20)).unwrap().src;
    let read = MessagePicture::of_in(&cloud(), &src).unwrap();
    let member = super::client(&server, "bert@own.test", &bert.id).await;
    assert_eq!(member.read(&read).await.unwrap().body(), bytes.as_slice());
    let outsider = super::client(&server, "cora@own.test", &cora.id).await;
    assert_eq!(status(&outsider.read(&read).await.unwrap_err()), Some(403));
    sender.shutdown();
    for seat in [ada, bert, cora] {
        seat.stop();
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_sends_a_sticker_of_its_own_and_the_other_member_reads_it_as_a_sticker() {
    use teams_proto::chat::{Sticker, parse_html, sticker_message};

    for backend in backends() {
        let trio = Trio::start(backend).await;
        let bytes = png(120, 90);
        let picture = PastedPicture {
            format: PastedFormat::Png,
            bytes: bytes.clone(),
        };
        // as the compose box sends a sticker the user imported: its picture first, then the message naming it
        let object = trio.a.upload_picture(trio.chat.clone(), picture).await.unwrap();
        let sticker = Sticker::of_picture(&cloud(), &object, (120, 90), "Happy cat").unwrap();
        let row = trio
            .a
            .write(Action::Send {
                conversation: trio.chat.clone(),
                text: MessageText::new_in(&cloud(), sticker_message(sticker), []).unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        assert!(trio.b.sync().await.unwrap().failed.is_empty());
        let message = trio
            .b
            .message(&trio.chat, row.message_id.unwrap())
            .await
            .unwrap()
            .unwrap();
        let read = parse_html(&message.content)
            .nodes
            .into_iter()
            .find_map(|node| match node {
                Node::Sticker(sticker) => Some(sticker),
                _ => None,
            })
            .unwrap_or_else(|| panic!("a sticker: {}", message.content));
        assert_eq!(read.label.as_deref(), Some("Happy cat"));
        assert_eq!(read.image.size, Some((120, 90)));
        assert_eq!(read.image.item_id.as_deref(), Some(object.as_str()));
        let shown = trio.b.message_picture(read.image.src.clone()).await.unwrap().unwrap();
        assert_eq!(
            shown.bytes.as_slice(),
            bytes.as_slice(),
            "the sticker's picture, from the server's media store"
        );
        trio.stop().await;
    }
}
