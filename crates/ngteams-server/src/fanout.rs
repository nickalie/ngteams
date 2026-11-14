//! Fan-out between the nodes: a payload published on a topic (such as `push:<mri>`) reaches every subscriber of that
//! topic, on any node. [`Local`] is one process's broadcast (a single node, SQLite); [`Redis`] carries it through Redis
//! pub/sub to every node, each node's subscribers fed from one Redis subscription per topic. Delivery is at most once:
//! a subscriber that falls behind learns how much it missed ([`Missed`]), and replays from its own record.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use bytes::Bytes;
use redis::aio::ConnectionManager;
use redis::{AsyncCommands as _, PushInfo, PushKind, Value};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::mpsc;

/// Payloads a subscriber may fall behind by before it misses some.
pub const BACKLOG: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum FanoutError {
    #[error("the fan-out is unavailable: {0}")]
    Unavailable(String),
}

impl From<redis::RedisError> for FanoutError {
    fn from(error: redis::RedisError) -> Self {
        Self::Unavailable(error.to_string())
    }
}

/// Why [`Subscription::recv`] returned no payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Missed {
    /// The subscriber fell this many payloads behind; the next ones follow.
    #[error("missed {0} payloads")]
    Lagged(u64),
    /// The fan-out is gone.
    #[error("the fan-out closed")]
    Closed,
}

/// The fan-out every node holds one of.
#[async_trait]
pub trait Fanout: Send + Sync + std::fmt::Debug {
    /// Sends `payload` to every subscriber of `topic`, on every node.
    async fn publish(&self, topic: &str, payload: Bytes) -> Result<(), FanoutError>;

    /// Payloads published on `topic` from now on, until the subscription is dropped.
    async fn subscribe(&self, topic: &str) -> Result<Subscription, FanoutError>;

    /// Whether it can deliver (readiness).
    async fn ready(&self) -> Result<(), FanoutError>;
}

/// One subscriber's payloads.
pub struct Subscription {
    receiver: broadcast::Receiver<Bytes>,
    _release: Option<Release>,
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription").finish_non_exhaustive()
    }
}

impl Subscription {
    /// The next payload.
    ///
    /// # Errors
    /// [`Missed`]: payloads were dropped for falling behind, or the fan-out closed.
    pub async fn recv(&mut self) -> Result<Bytes, Missed> {
        self.receiver.recv().await.map_err(|error| match error {
            RecvError::Lagged(missed) => Missed::Lagged(missed),
            RecvError::Closed => Missed::Closed,
        })
    }
}

/// The topics a node delivers to, each a broadcast channel.
#[derive(Debug, Default)]
struct Topics(Mutex<HashMap<String, broadcast::Sender<Bytes>>>);

impl Topics {
    /// A receiver of `topic`, and whether it is the topic's first.
    fn receiver(&self, topic: &str) -> (broadcast::Receiver<Bytes>, bool) {
        let mut topics = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(sender) = topics.get(topic)
            && sender.receiver_count() > 0
        {
            return (sender.subscribe(), false);
        }
        let (sender, receiver) = broadcast::channel(BACKLOG);
        topics.insert(topic.to_owned(), sender);
        (receiver, true)
    }

    fn deliver(&self, topic: &str, payload: Bytes) {
        let mut topics = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(sender) = topics.get(topic)
            && sender.send(payload).is_err()
        {
            topics.remove(topic);
        }
    }

    /// Forgets `topic` when nobody listens to it any more; whether it did.
    fn release(&self, topic: &str) -> bool {
        let mut topics = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let idle = topics.get(topic).is_none_or(|sender| sender.receiver_count() == 0);
        if idle {
            topics.remove(topic);
        }
        idle
    }
}

/// The in-process fan-out: one node.
#[derive(Debug, Default)]
pub struct Local {
    topics: Topics,
}

impl Local {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Fanout for Local {
    async fn publish(&self, topic: &str, payload: Bytes) -> Result<(), FanoutError> {
        self.topics.deliver(topic, payload);
        Ok(())
    }

    async fn subscribe(&self, topic: &str) -> Result<Subscription, FanoutError> {
        Ok(Subscription {
            receiver: self.topics.receiver(topic).0,
            _release: None,
        })
    }

    async fn ready(&self) -> Result<(), FanoutError> {
        Ok(())
    }
}

/// Drops a node's Redis subscription to a topic once its last local subscriber is gone.
struct Release {
    inner: Arc<RedisInner>,
    topic: String,
}

impl Drop for Release {
    fn drop(&mut self) {
        let inner = Arc::clone(&self.inner);
        let topic = std::mem::take(&mut self.topic);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { inner.release(&topic).await });
        }
    }
}

#[derive(Debug)]
struct RedisInner {
    commands: ConnectionManager,
    subscriber: ConnectionManager,
    topics: Topics,
    /// Held while a topic is subscribed or released, so a release can't undo a subscription made meanwhile.
    changes: tokio::sync::Mutex<()>,
}

impl RedisInner {
    async fn release(&self, topic: &str) {
        let _change = self.changes.lock().await;
        if self.topics.release(topic) {
            let mut subscriber = self.subscriber.clone();
            if let Err(error) = subscriber.unsubscribe(topic).await {
                tracing::warn!(%error, "can't drop a Redis subscription");
            }
        }
    }
}

/// The fan-out through Redis pub/sub (RESP3 pushes; the connection manager resubscribes after a reconnect).
#[derive(Debug, Clone)]
pub struct Redis {
    inner: Arc<RedisInner>,
}

impl Redis {
    /// Connects to `url` (its RESP3 form is used for the subscriber) and starts delivering.
    ///
    /// # Errors
    /// Redis can't be reached.
    pub async fn connect(url: &str) -> Result<Self, FanoutError> {
        let client = redis::Client::open(resp3(url))?;
        let commands = ConnectionManager::new(client.clone()).await?;
        let (pushes, mut received) = mpsc::unbounded_channel::<PushInfo>();
        let config = redis::aio::ConnectionManagerConfig::new()
            .set_push_sender(pushes)
            .set_automatic_resubscription();
        let subscriber = ConnectionManager::new_with_config(client, config).await?;
        let inner = Arc::new(RedisInner {
            commands,
            subscriber,
            topics: Topics::default(),
            changes: tokio::sync::Mutex::new(()),
        });
        let delivering = Arc::downgrade(&inner);
        tokio::spawn(async move {
            while let Some(push) = received.recv().await {
                let Some(inner) = delivering.upgrade() else { break };
                if let Some((topic, payload)) = message(push) {
                    inner.topics.deliver(&topic, payload);
                }
            }
        });
        Ok(Self { inner })
    }

    /// Its command connection, for other shared state (login counters).
    #[must_use]
    pub fn connection(&self) -> ConnectionManager {
        self.inner.commands.clone()
    }
}

#[async_trait]
impl Fanout for Redis {
    async fn publish(&self, topic: &str, payload: Bytes) -> Result<(), FanoutError> {
        let mut commands = self.inner.commands.clone();
        commands.publish::<_, _, ()>(topic, payload.as_ref()).await?;
        Ok(())
    }

    async fn subscribe(&self, topic: &str) -> Result<Subscription, FanoutError> {
        let _change = self.inner.changes.lock().await;
        let (receiver, first) = self.inner.topics.receiver(topic);
        if first {
            let mut subscriber = self.inner.subscriber.clone();
            if let Err(error) = subscriber.subscribe(topic).await {
                self.inner.topics.release(topic);
                return Err(error.into());
            }
        }
        Ok(Subscription {
            receiver,
            _release: Some(Release {
                inner: Arc::clone(&self.inner),
                topic: topic.to_owned(),
            }),
        })
    }

    async fn ready(&self) -> Result<(), FanoutError> {
        let mut commands = self.inner.commands.clone();
        redis::cmd("PING").query_async::<()>(&mut commands).await?;
        Ok(())
    }
}

/// The URL with `protocol=resp3`, which pushes need.
fn resp3(url: &str) -> String {
    if url.contains("protocol=") {
        url.to_owned()
    } else if url.contains('?') {
        format!("{url}&protocol=resp3")
    } else {
        format!("{url}?protocol=resp3")
    }
}

/// A pub/sub message push's channel and payload.
fn message(push: PushInfo) -> Option<(String, Bytes)> {
    if push.kind != PushKind::Message {
        return None;
    }
    let mut data = push.data.into_iter();
    let (Some(Value::BulkString(channel)), Some(Value::BulkString(payload))) = (data.next(), data.next()) else {
        return None;
    };
    Some((String::from_utf8(channel).ok()?, Bytes::from(payload)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_before_release_still_releases_an_idle_topic() {
        let topics = Topics::default();
        let (receiver, _) = topics.receiver("push:gone");
        drop(receiver);
        topics.deliver("push:gone", Bytes::from_static(b"late"));
        assert!(topics.release("push:gone"));
        let (_receiver, first) = topics.receiver("push:gone");
        assert!(first);
        assert!(!topics.release("push:gone"));
    }

    #[tokio::test]
    async fn redis_unsubscribes_when_delivery_precedes_release() {
        let Ok(url) = std::env::var("NGTEAMS_SERVER_REDIS_URL") else {
            return;
        };
        let node = Redis::connect(&url).await.unwrap();
        let topic = format!("push:{}", uuid::Uuid::new_v4());
        let subscription = node.subscribe(&topic).await.unwrap();
        let change = node.inner.changes.lock().await;
        drop(subscription);
        node.inner.topics.deliver(&topic, Bytes::from_static(b"late"));
        drop(change);
        let released = tokio::time::timeout(std::time::Duration::from_secs(5), wait_for_release(&node, &topic)).await;
        assert!(released.is_ok());
    }
    async fn wait_for_release(node: &Redis, topic: &str) {
        let mut commands = node.connection();
        loop {
            let counts: Vec<(String, u64)> = redis::cmd("PUBSUB")
                .arg("NUMSUB")
                .arg(topic)
                .query_async(&mut commands)
                .await
                .unwrap();
            if counts[0].1 == 0 {
                return;
            }
            tokio::task::yield_now().await;
        }
    }
}
