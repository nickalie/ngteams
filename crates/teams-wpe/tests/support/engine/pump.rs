use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;
use teams_wpe::{
    Backend, Event, FrameContent, Host, HostConfig, Load, LoadEvent, PolicyKind, PolicyToken, ReplyToken, SchemeToken,
    ViewId,
};

use super::{Result, page};

const WAIT: Duration = Duration::from_secs(20);

#[derive(Debug, Default)]
pub(super) struct Frames {
    pub count: usize,
    pub width: u32,
    pub height: u32,
    pub shm: bool,
    pub dma: bool,
    pub corner: Option<[u8; 4]>,
}

pub(super) struct Engine {
    pub host: Host,
    events: async_channel::Receiver<Event>,
    pending: Vec<Event>,
    pub frames: BTreeMap<ViewId, Frames>,
    pub root: PathBuf,
    pub held: Vec<SchemeToken>,
    /// Navigations to a URI ending `/hold-policy`, left unanswered.
    pub held_policies: Vec<PolicyToken>,
    /// Script messages whose action is `hold`, left unanswered.
    pub held_replies: Vec<ReplyToken>,
    pub schemes: Vec<String>,
    pub policies: usize,
    pub replies: usize,
}

impl Engine {
    pub fn start(config: HostConfig, backend: Backend, root: PathBuf) -> Result<Self> {
        let (host, events) = match backend {
            Backend::Worker => Host::start_worker(config)?,
            Backend::InProcess => Host::start(config)?,
        };
        assert_eq!(host.backend(), backend);
        assert_eq!(host.worker_pid().is_some(), backend == Backend::Worker);
        Ok(Self {
            host,
            events,
            pending: Vec::new(),
            frames: BTreeMap::new(),
            root,
            held: Vec::new(),
            held_policies: Vec::new(),
            held_replies: Vec::new(),
            schemes: Vec::new(),
            policies: 0,
            replies: 0,
        })
    }

    fn accept(&mut self, event: Event) -> Result {
        match &event {
            Event::Frame(frame) => {
                assert!(frame.width > 0 && frame.height > 0);
                let seen = self.frames.entry(frame.view).or_default();
                seen.count += 1;
                seen.width = frame.width;
                seen.height = frame.height;
                match &frame.content {
                    FrameContent::Shm(regions) => {
                        assert!(!regions.is_empty());
                        for region in regions {
                            assert_eq!(
                                region.pixels.len(),
                                (region.rect.width * region.rect.height * 4) as usize
                            );
                        }
                        seen.corner = regions
                            .iter()
                            .find(|region| region.rect.x == 0 && region.rect.y == 0)
                            .map(|region| region.pixels[..4].try_into())
                            .transpose()?
                            .or(seen.corner);
                        seen.shm = true;
                    }
                    FrameContent::DmaBuf(buffer) => {
                        assert_eq!((buffer.width, buffer.height), (frame.width, frame.height));
                        assert!(!buffer.planes.is_empty());
                        assert!(buffer.planes.iter().all(|plane| plane.stride > 0));
                        seen.dma = true;
                    }
                }
                self.host.frame_presented(frame.view, frame.buffer);
                return Ok(());
            }
            Event::Policy(request) if matches!(&request.kind, PolicyKind::Navigation { uri, .. } if uri.ends_with("/hold-policy")) =>
            {
                self.held_policies.push(request.token);
            }
            Event::Policy(request) => {
                let allow = match &request.kind {
                    PolicyKind::Navigation { uri, new_window, .. } => !new_window && page::allowed(uri),
                    PolicyKind::Response {
                        uri, mime_supported, ..
                    } => *mime_supported && page::allowed(uri),
                };
                self.host.decide_policy(request.token, allow);
                self.policies += 1;
            }
            Event::SchemeRequest(request) => {
                self.schemes.push(request.uri.clone());
                if request.uri.ends_with("/hold") {
                    self.held.push(request.token);
                } else {
                    self.host
                        .finish_scheme_request(request.token, page::response(&request.uri, &self.root)?);
                }
            }
            Event::ScriptMessage(message) => {
                let body: Value = serde_json::from_str(message.body.as_deref().unwrap_or("null"))?;
                if body["action"] == "hold" {
                    self.held_replies.push(message.token);
                    self.pending.push(event);
                    return Ok(());
                }
                let reply = match body["action"].as_str() {
                    Some("reject") => Err("synthetic rejection".to_owned()),
                    Some("invalid-json") => Ok("synthetic invalid JSON".into()),
                    _ => Ok(serde_json::json!({"echo": body}).to_string()),
                };
                self.host.reply_script_message(message.token, reply);
                self.replies += 1;
            }
            _ => {}
        }
        self.pending.push(event);
        Ok(())
    }

    pub fn pump(&mut self) -> Result {
        match self.events.try_recv() {
            Ok(event) => self.accept(event),
            Err(async_channel::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(2));
                Ok(())
            }
            Err(async_channel::TryRecvError::Closed) => Err("native engine event channel closed".into()),
        }
    }

    pub fn wait(&mut self, predicate: impl Fn(&Event) -> bool) -> Result<Event> {
        let end = Instant::now() + WAIT;
        loop {
            if let Some(index) = self.pending.iter().position(&predicate) {
                return Ok(self.pending.remove(index));
            }
            if Instant::now() >= end {
                let pending: Vec<_> = self.pending.iter().map(std::mem::discriminant).collect();
                return Err(format!("event wait expired; pending={pending:?}").into());
            }
            self.pump()?;
        }
    }

    pub fn eval(&mut self, view: ViewId, script: &str, world: Option<&str>) -> Result<Value> {
        let answer = self.host.evaluate(view, script.to_owned(), world.map(str::to_owned));
        let end = Instant::now() + WAIT;
        loop {
            match answer.try_recv() {
                Ok(Ok(value)) => {
                    return value.map_or(Ok(Value::Null), |value| {
                        serde_json::from_str(&value).map_err(Into::into)
                    });
                }
                Ok(Err(error)) => return Err(format!("evaluating {script}: {error}").into()),
                Err(async_channel::TryRecvError::Closed) => return Err(format!("evaluation closed: {script}").into()),
                Err(async_channel::TryRecvError::Empty) => {}
            }
            if Instant::now() >= end {
                return Err(format!("evaluation expired: {script}").into());
            }
            self.pump()?;
        }
    }

    pub fn until(&mut self, view: ViewId, script: &str, predicate: impl Fn(&Value) -> bool) -> Result<Value> {
        let end = Instant::now() + WAIT;
        loop {
            let value = self.eval(view, script, None)?;
            if predicate(&value) {
                return Ok(value);
            }
            if Instant::now() >= end {
                return Err(format!("condition expired: {script}, got {value}").into());
            }
            self.pump()?;
        }
    }

    pub fn load(&mut self, view: ViewId, load: Load) -> Result {
        self.pending
            .retain(|event| !matches!(event, Event::Load { view: id, .. } if *id == view));
        self.host.load(view, load);
        self.wait(|event| matches!(event, Event::Load { view: id, event: LoadEvent::Started } if *id == view))?;
        self.pending
            .retain(|event| !matches!(event, Event::Load { view: id, .. } if *id == view));
        self.loaded(view)
    }

    pub fn loaded(&mut self, view: ViewId) -> Result {
        self.wait(|event| matches!(event, Event::Load { view: id, event: LoadEvent::Finished } if *id == view))?;
        Ok(())
    }

    pub fn frame(&mut self, view: ViewId, width: u32, height: u32) -> Result {
        let end = Instant::now() + WAIT;
        while !self
            .frames
            .get(&view)
            .is_some_and(|frame| frame.width == width && frame.height == height)
        {
            if Instant::now() >= end {
                return Err(format!("frame {width}x{height} not received: {:?}", self.frames).into());
            }
            self.pump()?;
        }
        Ok(())
    }

    pub fn shutdown(&mut self) {
        let pid = self.host.worker_pid();
        self.host.shutdown();
        self.host.shutdown();
        if let (None, Some(view)) = (pid, self.frames.keys().next().copied()) {
            let late = self.host.evaluate(view, "1".to_owned(), None);
            assert!(
                matches!(late.try_recv(), Err(async_channel::TryRecvError::Closed)),
                "an evaluation after the shutdown is not dropped"
            );
        }
        if let Some(pid) = pid {
            assert!(
                !std::path::Path::new(&format!("/proc/{pid}")).exists(),
                "worker not reaped"
            );
        }
    }
}
