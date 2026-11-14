//! The generic stream cannot be opened as a thread; a missing individual policy copy is a native 404.

use teams_proto::chat::ChatReads;

use super::client;
use crate::support::{TestServer, backends};

#[tokio::test(flavor = "multi_thread")]
async fn native_generic_stream_reads_get_invalid_thread_400s_and_a_missing_copy_gets_404() {
    for backend in backends() {
        let server = TestServer::start_with(backend, |config| {
            config.public_origin = url::Url::parse("https://own.example.test").unwrap();
        })
        .await;
        let id = server.add_user("a@own.test", "Ada").await;
        let client = client(&server, "a@own.test", &id).await;
        let reads = ChatReads::new(&client);
        let thread = reads.thread("48:generic").await.unwrap_err().status();
        let page = reads.messages("48:generic", 200).await.unwrap_err().status();
        let copy = reads.blocked_copy("1").await.is_ok_and(|copy| copy.is_none());
        assert_eq!((thread, page, copy), (Some(400), Some(400), true));
        server.stop().await;
    }
}
