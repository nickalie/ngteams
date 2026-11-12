//! One pipeline per device, owned by its pump thread. Only this thread starts/stops pipelines. A choice or hotplug
//! replaces the pipeline before the next read/write; losing a named device falls back to the default without
//! forgetting the choice. A fatal pipeline error closes the queue, so consumers never wait forever.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use gstreamer_app::{AppSink, AppSrc};

use super::{Entry, Key, POLL, WindowsDevices, error};
use crate::devices::catalog::DeviceKind;
use crate::devices::{
    AudioFrame, AudioSink, AudioSource, DeviceError, FRAME_SAMPLES, PixelFormat, VideoFrame, VideoSource,
};
use crate::queue::Queue;
use gst::prelude::*;
use gstreamer as gst;

struct Running(gst::Pipeline);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

struct Pump {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Drop for Pump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn start(
    devices: WindowsDevices,
    kind: DeviceKind,
    mut make: impl FnMut(gst::Element) -> Result<Tick, DeviceError> + Send + 'static,
    close: impl FnOnce() + Send + 'static,
) -> Result<Pump, DeviceError> {
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let (ready, response) = mpsc::sync_channel(1);
    let thread = thread::Builder::new()
        .name(format!("windows-{}", kind.key()))
        .spawn(move || {
            let result = pump(&devices, kind, &stopped, &ready, &mut make);
            if let Err(failure) = result {
                let _ = ready.try_send(Err(failure.to_string()));
                log::warn!("a Windows {} stream stopped: {failure}", kind.key());
            }
            close();
        })?;
    let guard = Pump {
        stop,
        thread: Some(thread),
    };
    match response.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => Ok(guard),
        Ok(Err(failure)) => Err(error(failure)),
        Err(failure) => Err(error(failure)),
    }
}

type Tick = Box<dyn FnMut() -> Result<(), DeviceError>>;
type Make = dyn FnMut(gst::Element) -> Result<Tick, DeviceError>;
fn pump(
    devices: &WindowsDevices,
    kind: DeviceKind,
    stop: &AtomicBool,
    ready: &mpsc::SyncSender<Result<(), String>>,
    make: &mut Make,
) -> Result<(), DeviceError> {
    let mut current = None;
    let mut tick: Option<Tick> = None;
    let mut announced = false;
    while !stop.load(Ordering::Acquire) {
        let entry = devices.shared.selected(kind);
        let name = entry.as_ref().map(Entry::key);
        if tick.is_none() || name != current {
            // Drop the old stream first: exclusive cameras cannot be opened twice. All queues survive the switch.
            drop(tick.take());
            let element = match entry {
                Some(entry) => entry.device.create_element(None).map_err(error)?,
                None => default_element(kind)?,
            };
            if element.find_property("low-latency").is_some() {
                element.set_property("low-latency", true);
            }
            tick = Some(make(element)?);
            current = name;
        }
        if !announced {
            let _ = ready.send(Ok(()));
            announced = true;
        }
        if let Some(tick) = tick.as_mut()
            && let Err(failure) = tick()
        {
            // Device removal can error the old pipeline just before the monitor publishes its new default.
            // Give that event a bounded chance to arrive, then rebuild only if the effective device changed.
            if !changed_after_removal(devices, kind, current.as_ref(), stop) {
                return Err(failure);
            }
        }
    }
    Ok(())
}

fn changed_after_removal(devices: &WindowsDevices, kind: DeviceKind, current: Option<&Key>, stop: &AtomicBool) -> bool {
    for _ in 0..20 {
        if stop.load(Ordering::Acquire) {
            return true;
        }
        let next = devices.shared.selected(kind);
        if next.as_ref().map(Entry::key).as_ref() != current {
            return true;
        }
        thread::sleep(POLL);
    }
    false
}

fn default_element(kind: DeviceKind) -> Result<gst::Element, DeviceError> {
    gst::ElementFactory::make(super::factory(kind)).build().map_err(error)
}

fn audio_caps() -> gst::Caps {
    gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("layout", "interleaved")
        .field("rate", 48_000i32)
        .field("channels", 1i32)
        .build()
}

fn playing(elements: &[&gst::Element]) -> Result<Running, DeviceError> {
    let running = Running(gst::Pipeline::new());
    running.0.add_many(elements.iter().copied()).map_err(error)?;
    gst::Element::link_many(elements.iter().copied()).map_err(error)?;
    running.0.set_state(gst::State::Playing).map_err(error)?;
    let (state, _, _) = running.0.state(gst::ClockTime::from_seconds(5));
    state.map_err(error)?;
    Ok(running)
}

fn healthy(pipeline: &gst::Pipeline) -> Result<(), DeviceError> {
    if let Some(bus) = pipeline.bus() {
        for message in bus.iter() {
            match message.view() {
                gst::MessageView::Error(failure) => return Err(error(failure.error())),
                gst::MessageView::Eos(_) => return Err(DeviceError::Closed),
                _ => (),
            }
        }
    }
    Ok(())
}

fn element(name: &str) -> Result<gst::Element, DeviceError> {
    gst::ElementFactory::make(name).build().map_err(error)
}

fn capture(source: &gst::Element, video: bool) -> Result<(Running, AppSink), DeviceError> {
    let sink = AppSink::builder().max_buffers(3u32).drop(true).sync(false).build();
    let convert = element(if video { "videoconvert" } else { "audioconvert" })?;
    let adjust = element(if video { "videoscale" } else { "audioresample" })?;
    let caps = if video {
        // BGRA has an exact width*4 stride. Arbitrary camera dimensions remain intact.
        gst::Caps::builder("video/x-raw").field("format", "BGRA").build()
    } else {
        audio_caps()
    };
    sink.set_caps(Some(&caps));
    let running = playing(&[source, &convert, &adjust, sink.upcast_ref()])?;
    Ok((running, sink))
}

fn sample(sink: &AppSink) -> Option<gst::Sample> {
    sink.try_pull_sample(gst::ClockTime::from_mseconds(20))
}

struct Microphone {
    frames: Queue<AudioFrame>,
    _pump: Pump,
}
impl AudioSource for Microphone {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<AudioFrame>, DeviceError> {
        self.frames.recv(timeout)
    }
}

pub(super) fn microphone(devices: WindowsDevices) -> Result<Box<dyn AudioSource>, DeviceError> {
    let frames = Queue::new(50);
    let (write, close) = (frames.clone(), frames.clone());
    let pump = start(
        devices,
        DeviceKind::Microphone,
        move |source| {
            let (pipeline, sink) = capture(&source, false)?;
            let mut pending = Vec::new();
            let write = write.clone();
            Ok(Box::new(move || {
                healthy(&pipeline.0)?;
                if let Some(sample) = sample(&sink) {
                    audio_frames(&sample, &mut pending, &write)?;
                }
                Ok(())
            }))
        },
        move || close.close(),
    )?;
    Ok(Box::new(Microphone { frames, _pump: pump }))
}

fn audio_frames(sample: &gst::Sample, pending: &mut Vec<f32>, write: &Queue<AudioFrame>) -> Result<(), DeviceError> {
    let map = sample
        .buffer()
        .ok_or(DeviceError::Closed)?
        .map_readable()
        .map_err(error)?;
    pending.extend(
        map.as_slice()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| f32::from_le_bytes(*bytes)),
    );
    for samples in pending.as_chunks::<FRAME_SAMPLES>().0 {
        if let Some(frame) = AudioFrame::from_samples(samples) {
            write.push(frame);
        }
    }
    let whole = pending.len() / FRAME_SAMPLES * FRAME_SAMPLES;
    pending.drain(..whole);
    Ok(())
}

struct Camera {
    frames: Queue<VideoFrame>,
    _pump: Pump,
}
impl VideoSource for Camera {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, DeviceError> {
        self.frames.recv(timeout)
    }
}

pub(super) fn camera(devices: WindowsDevices) -> Result<Box<dyn VideoSource>, DeviceError> {
    let frames = Queue::new(3);
    let (write, close) = (frames.clone(), frames.clone());
    let pump = start(
        devices,
        DeviceKind::Camera,
        move |source| {
            let (pipeline, sink) = capture(&source, true)?;
            let write = write.clone();
            Ok(Box::new(move || {
                healthy(&pipeline.0)?;
                if let Some(sample) = sample(&sink) {
                    write.push(video_frame(&sample)?);
                }
                Ok(())
            }))
        },
        move || close.close(),
    )?;
    Ok(Box::new(Camera { frames, _pump: pump }))
}

fn video_frame(sample: &gst::Sample) -> Result<VideoFrame, DeviceError> {
    let caps = sample
        .caps()
        .and_then(|caps| caps.structure(0))
        .ok_or(DeviceError::Closed)?;
    let width = u32::try_from(caps.get::<i32>("width").map_err(error)?).map_err(error)?;
    let height = u32::try_from(caps.get::<i32>("height").map_err(error)?).map_err(error)?;
    let stride = width.checked_mul(4).ok_or_else(|| error("video width overflow"))?;
    let data = sample
        .buffer()
        .ok_or(DeviceError::Closed)?
        .map_readable()
        .map_err(error)?
        .as_slice()
        .to_vec();
    let length = u64::from(stride) * u64::from(height);
    if u64::try_from(data.len()).ok() != Some(length) {
        return Err(error("unexpected BGRA frame length"));
    }
    Ok(VideoFrame {
        width,
        height,
        stride,
        format: PixelFormat::Bgra,
        data,
    })
}

struct Speaker {
    frames: Queue<AudioFrame>,
    _pump: Pump,
}
impl AudioSink for Speaker {
    fn play(&mut self, frame: &AudioFrame) -> Result<(), DeviceError> {
        self.frames.extend([frame.clone()]).map(drop)
    }
}

pub(super) fn speaker(devices: WindowsDevices) -> Result<Box<dyn AudioSink>, DeviceError> {
    let frames: Queue<AudioFrame> = Queue::new(20);
    let (read, close) = (frames.clone(), frames.clone());
    let pump = start(
        devices,
        DeviceKind::Speaker,
        move |sink| {
            // A live call has no preroll before its first received frame. Waiting for it here delays opening the
            // speaker (and therefore ringing or answering) until the state wait expires.
            sink.set_property("async", false);
            let source = AppSrc::builder()
                .caps(&audio_caps())
                .is_live(true)
                .format(gst::Format::Time)
                .do_timestamp(true)
                .block(false)
                .max_bytes(3_840u64)
                .build();
            // The appsrc queue drops old audio if a driver stops consuming.
            source.set_property_from_str("leaky-type", "downstream");
            let convert = element("audioconvert")?;
            let resample = element("audioresample")?;
            let pipeline = playing(&[source.upcast_ref(), &convert, &resample, &sink])?;
            let read = read.clone();
            Ok(Box::new(move || {
                healthy(&pipeline.0)?;
                let frame = read.recv(POLL)?;
                if let Some(frame) = frame {
                    let bytes: Vec<u8> = frame.samples().iter().flat_map(|s| s.to_le_bytes()).collect();
                    let mut buffer = gst::Buffer::from_mut_slice(bytes);
                    if let Some(buffer) = buffer.get_mut() {
                        buffer.set_duration(gst::ClockTime::from_mseconds(10));
                    }
                    source.push_buffer(buffer).map_err(error)?;
                }
                // WASAPI's sink clock paces playback; the input queue waits for arriving call frames. Adding a
                // sleep here would serialize that clock with Windows' timer resolution and cause underruns.
                Ok(())
            }))
        },
        move || close.close(),
    )?;
    Ok(Box::new(Speaker { frames, _pump: pump }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_failure_retains_its_cause_while_eos_is_closed() {
        gst::init().unwrap();
        let pipeline = gst::Pipeline::new();
        pipeline
            .post_message(gst::message::Error::builder(gst::ResourceError::Read, "synthetic device vanished").build())
            .unwrap();
        assert!(
            healthy(&pipeline)
                .unwrap_err()
                .to_string()
                .contains("synthetic device vanished")
        );
        pipeline.post_message(gst::message::Eos::new()).unwrap();
        assert!(matches!(healthy(&pipeline), Err(DeviceError::Closed)));
    }
}
