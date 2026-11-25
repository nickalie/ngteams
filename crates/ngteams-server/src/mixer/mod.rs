//! A group call's media node: the mixer every party of a group call sends its audio to and hears the others' mix
//! from (`serve --role media`, or beside signaling in `--role all`).
//!
//! - **str0m does the transport** (ICE-lite, DTLS, SRTP, SCTP) on one UDP socket, sans-IO in one media thread
//!   ([`node`]); **libopus decodes and encodes**; the mix, its buffer and the dominant speakers are ours
//!   ([`audio`]); **`teams-sdp` speaks the dialect** (its mixer side). Video is forwarded, never transcoded: each
//!   receiving line shows the camera or screen its source requests name, from that source's next keyframe on
//!   ([`video`]).
//! - **Signaling reaches it only over the internal API** ([`api`], [`client`]): a call's row names its node, so any
//!   signaling node reaches any call's room; Redis only lists the live nodes ([`registry`]), for a new room's pick
//!   and the sweeper's look at a call's node. Without Redis the process's own node is the only one.
//! - **The node changes a call only through the controller's steps**: a leg whose media is lost takes its party out
//!   of the call; a room whose call is over goes. A node that dies ends its calls (the sweeper sees its heartbeat
//!   stop): a running call is never moved to another node.

mod api;
mod audio;
mod client;
pub mod config;
mod control;
mod leg;
mod node;
mod registry;
mod room;
mod sps;
mod video;

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, mpsc};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;
use tokio::task::JoinHandle;

pub use client::HttpNodes;
pub use node::LegView;

use crate::app::App;
use crate::calling::MediaHome;
use leg::Making;
use node::{Command, Load, Lost, Settings};
use registry::{Entry, Registry};

/// Which parts of the server a node runs (`serve --role`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Role {
    /// The public API and a media node, in one process: a single node's deployment.
    #[default]
    All,
    /// The public API only: group calls' media goes to the media nodes the registry lists.
    Api,
    /// A media node only: its internal API and its UDP socket.
    Media,
}

impl Role {
    #[must_use]
    pub fn serves_api(self) -> bool {
        self != Self::Media
    }

    #[must_use]
    pub fn serves_media(self) -> bool {
        self != Self::Api
    }
}

/// Why a media node can't start.
#[derive(Debug, thiserror::Error)]
pub enum MixerError {
    #[error("the media node can't listen: {0}")]
    Listen(#[source] std::io::Error),
    #[error("the media node can't reach the signaling side: {0}")]
    Client(String),
}

/// A running media node: its media thread, internal listener, heartbeat and its watch over its rooms.
#[derive(Debug)]
pub struct MediaNode {
    home: MediaHome,
    alive: Arc<AtomicBool>,
    draining: Arc<AtomicBool>,
    commands: mpsc::Sender<Command>,
    thread: Option<std::thread::JoinHandle<()>>,
    server: Option<(oneshot::Sender<()>, JoinHandle<()>)>,
    tasks: Vec<JoinHandle<()>>,
    registry: Option<Registry>,
}

/// The process's crypto for str0m's DTLS, once: aws-lc-rs, as the rest of the server's.
fn install_crypto() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| str0m::crypto::from_feature_flags().install_process_default());
}

/// The addresses a relay reaches the socket bound at `bound` at: the configured ones, else its own address, else the
/// host's outward one (the address a datagram to the internet would leave from; nothing is sent).
fn candidates(public: &[IpAddr], bound: SocketAddr) -> Vec<SocketAddr> {
    if !public.is_empty() {
        return public.iter().map(|ip| SocketAddr::new(*ip, bound.port())).collect();
    }
    if !bound.ip().is_unspecified() {
        return vec![bound];
    }
    let outward = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .and_then(|probe| {
            probe.connect((Ipv4Addr::new(192, 0, 2, 1), 9))?;
            probe.local_addr()
        })
        .map_or(IpAddr::V4(Ipv4Addr::LOCALHOST), |address| address.ip());
    vec![SocketAddr::new(outward, bound.port())]
}

/// Whether the internal URL `url` names this host only (a loopback address or `localhost`): another host can't reach it.
fn loopback(url: &str) -> bool {
    match url::Url::parse(url)
        .ok()
        .and_then(|url| url.host().map(|host| host.to_owned()))
    {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

fn registry_of(app: &App) -> Option<Registry> {
    let dead_after = Duration::from_secs(app.config.mixer.dead_after);
    app.redis.clone().map(|shared| Registry::new(shared, dead_after))
}

impl MediaNode {
    /// Starts the node of `app`'s `[mixer]` configuration.
    ///
    /// # Errors
    /// A socket that can't be bound.
    pub async fn start(app: &Arc<App>) -> Result<Self, MixerError> {
        install_crypto();
        let config = app.config.mixer.clone();
        let socket = UdpSocket::bind(config.udp_listen).map_err(MixerError::Listen)?;
        let udp = socket.local_addr().map_err(MixerError::Listen)?;
        let listener = tokio::net::TcpListener::bind(config.internal_listen)
            .await
            .map_err(MixerError::Listen)?;
        let internal = listener.local_addr().map_err(MixerError::Listen)?;
        let url = config.internal_url.as_ref().map_or_else(
            || format!("http://{internal}"),
            |url| url.as_str().trim_end_matches('/').to_owned(),
        );
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX));
        let home = MediaHome {
            node: config
                .node_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            url,
            epoch,
        };
        let settings = Settings {
            making: Making {
                candidates: candidates(&config.public_addrs, udp),
                ice_lite: config.ice_lite,
            },
            max_rooms: config.max_rooms,
            max_legs_per_room: config.max_legs_per_room,
            media_timeout: Duration::from_secs(config.media_timeout),
            connect_timeout: Duration::from_secs(config.connect_timeout),
        };
        let (commands, commanded) = mpsc::channel();
        let (lost, losses) = tokio::sync::mpsc::unbounded_channel();
        let thread = std::thread::Builder::new()
            .name("mixer".into())
            .spawn(move || node::run(socket, settings, &commanded, lost))
            .map_err(MixerError::Listen)?;
        let api = api::router(Arc::new(api::Internal {
            app: Arc::clone(app),
            epoch,
            commands: commands.clone(),
        }));
        let (stop, stopped) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let serving = axum::serve(listener, api).with_graceful_shutdown(async move {
                let _ = stopped.await;
            });
            if let Err(error) = serving.await {
                tracing::warn!(%error, "the media node's internal API stopped");
            }
        });
        let mut node = Self {
            home,
            alive: Arc::new(AtomicBool::new(true)),
            draining: Arc::new(AtomicBool::new(false)),
            commands,
            thread: Some(thread),
            server: Some((stop, server)),
            tasks: Vec::new(),
            registry: registry_of(app),
        };
        node.tasks.push(tokio::spawn(take_out_lost(Arc::clone(app), losses)));
        node.tasks.push(tokio::spawn(node.reconciling(Arc::clone(app))));
        node.start_heartbeat(Duration::from_secs(config.heartbeat));
        tracing::info!(node = %node.home.node, internal = %internal, media = %udp, "media node up");
        Ok(node)
    }

    /// Where the node's rooms are, as a call's row names it. Only the e2e tests ask, to reach its internal API.
    #[must_use]
    pub fn home(&self) -> &MediaHome {
        &self.home
    }

    /// The call `call`'s legs, while the node has its room. Only the e2e tests ask.
    pub async fn room(&self, call: &str) -> Option<Vec<LegView>> {
        let (reply, replied) = oneshot::channel();
        self.commands
            .send(Command::Room {
                call: call.to_owned(),
                reply,
            })
            .ok()?;
        replied.await.ok().flatten()
    }

    async fn load(commands: &mpsc::Sender<Command>) -> Option<Load> {
        let (reply, replied) = oneshot::channel();
        commands.send(Command::Load { reply }).ok()?;
        replied.await.ok()
    }

    /// Starts the heartbeat when there's a registry. A loopback URL there gets a warning: other hosts can't reach it.
    fn start_heartbeat(&mut self, every: Duration) {
        if self.registry.is_none() {
            return;
        }
        if loopback(&self.home.url) {
            tracing::warn!(
                url = %self.home.url,
                "the media node's internal URL is a loopback address, but Redis shares it with other hosts: they \
                 can't reach this node. Set mixer.internal_listen to a private network's address, or set \
                 mixer.internal_url"
            );
        }
        self.tasks.push(tokio::spawn(self.beating(every)));
    }

    /// The heartbeat: what the node carries, in the registry.
    fn beating(&self, every: Duration) -> impl Future<Output = ()> + Send + 'static {
        let (commands, registry) = (self.commands.clone(), self.registry.clone());
        let (home, draining) = (self.home.clone(), Arc::clone(&self.draining));
        async move {
            let Some(registry) = registry else { return };
            let mut ticks = tokio::time::interval(every);
            loop {
                ticks.tick().await;
                let Some(load) = Self::load(&commands).await else {
                    return;
                };
                let entry = Entry {
                    id: home.node.clone(),
                    url: home.url.clone(),
                    epoch: home.epoch,
                    rooms: load.rooms.len(),
                    legs: load.legs,
                    draining: draining.load(Ordering::SeqCst),
                };
                if let Err(error) = registry.beat(&entry).await {
                    tracing::warn!(%error, "the media node's heartbeat wasn't written");
                }
            }
        }
    }

    /// Now and then: each room whose call is over, missing or another node's two looks in a row goes.
    fn reconciling(&self, app: Arc<App>) -> impl Future<Output = ()> + Send + 'static {
        let (commands, home) = (self.commands.clone(), self.home.clone());
        let every = Duration::from_secs(app.config.mixer.reconcile);
        async move {
            let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
            // a late tick mustn't bring the next look closer: two looks in a row drop a room
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut unwanted = BTreeSet::new();
            loop {
                ticks.tick().await;
                let Some(load) = Self::load(&commands).await else {
                    return;
                };
                unwanted = drop_unwanted(&app, &commands, &home, load.rooms, &unwanted).await;
            }
        }
    }

    /// Takes no new room, and waits until its rooms are over or `limit` passed; then stops.
    pub async fn drain(self, limit: Duration) {
        self.draining.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            match Self::load(&self.commands).await {
                Some(load) if !load.rooms.is_empty() => tokio::time::sleep(Duration::from_millis(500)).await,
                _ => break,
            }
        }
        self.stop().await;
    }

    /// Stops at once: its rooms are gone, and the calls in them end once the sweeper sees the node gone.
    pub async fn stop(mut self) {
        self.alive.store(false, Ordering::SeqCst);
        for task in self.tasks.drain(..) {
            task.abort();
        }
        if let Some(registry) = &self.registry
            && let Err(error) = registry.remove(&self.home.node).await
        {
            tracing::warn!(%error, "the media node wasn't taken out of the registry");
        }
        if let Some((stop, server)) = self.server.take() {
            let _ = stop.send(());
            let _ = server.await;
        }
        let _ = self.commands.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }
    }

    fn local(&self) -> client::Local {
        client::Local {
            home: self.home.clone(),
            alive: Arc::clone(&self.alive),
        }
    }
}

/// Drops each of `rooms` whose call is over, missing or in another node's room, and was so at the last look too
/// (`before`): a new call's room is made before its row is stored. The rooms unwanted at this look.
async fn drop_unwanted(
    app: &App,
    commands: &mpsc::Sender<Command>,
    home: &MediaHome,
    rooms: Vec<String>,
    before: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut unwanted = BTreeSet::new();
    for call in rooms {
        match crate::calling::room_wanted(app, &call, home).await {
            Ok(true) => {}
            Ok(false) if before.contains(&call) => {
                let (reply, _) = oneshot::channel();
                let _ = commands.send(Command::DeleteRoom { call, reply });
            }
            Ok(false) => {
                unwanted.insert(call);
            }
            Err(error) => tracing::warn!(%error, "a media room's call couldn't be read"),
        }
    }
    unwanted
}

/// The parties whose legs lost their media are taken out of their calls.
async fn take_out_lost(app: Arc<App>, mut losses: tokio::sync::mpsc::UnboundedReceiver<Lost>) {
    while let Some(lost) = losses.recv().await {
        crate::calling::leg_lost(&app, &lost.call, &lost.leg).await;
    }
}

/// Installs the media nodes `app`'s group calls go to: the registry's (with Redis) and `local`, this process's own.
/// `false` when some were installed already.
///
/// # Errors
/// The HTTP client can't be built.
pub fn install(app: &Arc<App>, local: Option<&MediaNode>) -> Result<bool, MixerError> {
    let nodes = HttpNodes::new(app, registry_of(app), local.map(MediaNode::local)).map_err(MixerError::Client)?;
    Ok(app.mixers.install(Arc::new(nodes)))
}

#[cfg(test)]
mod tests {
    use super::loopback;

    #[test]
    fn only_a_loopback_host_is_loopback() {
        assert!(loopback("http://127.0.0.1:4000"));
        assert!(loopback("http://[::1]:4000"));
        assert!(loopback("http://LocalHost:4000"));
        assert!(!loopback("http://10.0.0.5:4000"));
        assert!(!loopback("https://media-1.internal"));
        assert!(!loopback("not a url"));
    }
}
