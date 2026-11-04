//! Round video on Linux through GStreamer: the camera from GStreamer's
//! device monitor (PipeWire's where it runs, otherwise V4L2), an H.264 and
//! an AAC encoder found at run time, and the microphone through cpal into
//! the AAC track (appsrc) and the PCM at once.
use super::{
    closest_format, finish, finish_refused, must_stop, poster, refused, square_crop, CameraFormat,
    VideoEvent, FINISH_POLL, FINISH_TIMEOUT, MAX_BYTES, NOT_STARTED, NO_DEVICES, NO_FORMAT,
    NO_STORAGE, POSTER_QUALITIES, POSTER_SIDE, PREVIEW_FPS, PREVIEW_SIDE, START_TIMEOUT, TICK,
};
use crate::{device, Event, Session};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::{Duration, Instant};
use zerus_client_core::voice::{InputFormat, SampleFormat};

/// H.264 encoders in order of preference, with 700 kbit/s settings.
const H264: [(&str, &str); 4] = [
    (
        "x264enc",
        "x264enc bitrate=700 speed-preset=veryfast tune=zerolatency key-int-max=48",
    ),
    (
        "openh264enc",
        "openh264enc rate-control=bitrate bitrate=700000",
    ),
    ("vah264enc", "vah264enc bitrate=700"),
    ("vaapih264enc", "vaapih264enc bitrate=700"),
];
/// AAC encoders in order of preference, at 64 kbit/s.
const AAC: [(&str, &str); 3] = [
    ("avenc_aac", "avenc_aac bitrate=64000"),
    ("fdkaacenc", "fdkaacenc bitrate=64000"),
    ("voaacenc", "voaacenc bitrate=64000"),
];
/// Everything else the pipeline uses.
const BASE: [&str; 17] = [
    "appsrc",
    "appsink",
    "tee",
    "queue",
    "capsfilter",
    "videoconvert",
    "videoscale",
    "videorate",
    "videocrop",
    "jpegenc",
    "jpegdec",
    "audioconvert",
    "audioresample",
    "h264parse",
    "aacparse",
    "mp4mux",
    "filesink",
];

fn init() -> Result<(), String> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| gst::init().map_err(|error| format!("GStreamer could not start: {error}")))
        .clone()
}

fn found(name: &str) -> bool {
    gst::ElementFactory::find(name).is_some()
}

fn encoder(choices: &[(&'static str, &'static str)]) -> Option<&'static str> {
    choices
        .iter()
        .find(|(name, _)| found(name))
        .map(|(_, description)| *description)
}

/// Why video cannot be recorded here, None when it can: GStreamer must
/// start and have an H.264 and an AAC encoder and the other elements.
/// Probed once per process (the first probe scans GStreamer's registry).
pub fn missing() -> Option<String> {
    static PROBE: OnceLock<Option<String>> = OnceLock::new();
    PROBE.get_or_init(probe).clone()
}

fn probe() -> Option<String> {
    if let Err(error) = init() {
        return Some(error);
    }
    if encoder(&H264).is_none() {
        return Some(
            "Video needs a GStreamer H.264 encoder: x264enc, openh264enc or VA-API.".into(),
        );
    }
    if encoder(&AAC).is_none() {
        return Some("Video needs a GStreamer AAC encoder: avenc_aac or fdkaacenc.".into());
    }
    let absent: Vec<&str> = BASE.into_iter().filter(|name| !found(name)).collect();
    (!absent.is_empty()).then(|| {
        format!(
            "Video needs these GStreamer elements: {}.",
            absent.join(", ")
        )
    })
}

/// Where the frames come from.
pub enum Source {
    /// The system's camera.
    Camera,
    /// A moving test pattern at 640x480: tests, with no camera.
    Test,
}

struct Camera {
    element: gst::Element,
    caps: gst::Caps,
    jpeg: bool,
    width: u32,
    height: u32,
}

fn highest_rate(value: &gst::glib::SendValue) -> Option<gst::Fraction> {
    if let Ok(rate) = value.get::<gst::Fraction>() {
        return Some(rate);
    }
    if let Ok(range) = value.get::<gst::FractionRange>() {
        return Some(range.max());
    }
    if let Ok(list) = value.get::<gst::List>() {
        return list
            .iter()
            .filter_map(|item| item.get::<gst::Fraction>().ok())
            .max_by(|a, b| {
                let rate = |f: &gst::Fraction| f64::from(f.numer()) / f64::from(f.denom().max(1));
                rate(a).total_cmp(&rate(b))
            });
    }
    None
}

fn side(structure: &gst::StructureRef, field: &str, wanted: i32) -> Option<u32> {
    if let Ok(value) = structure.get::<i32>(field) {
        return u32::try_from(value).ok();
    }
    let range = structure.get::<gst::IntRange<i32>>(field).ok()?;
    u32::try_from(wanted.clamp(range.min(), range.max())).ok()
}

/// The camera formats a device offers, with the caps that fix each.
fn formats(caps: &gst::Caps) -> Vec<(CameraFormat, gst::Caps, bool)> {
    let mut found = Vec::new();
    for structure in caps.iter() {
        let name = structure.name().as_str();
        let jpeg = name == "image/jpeg";
        if name != "video/x-raw" && !jpeg {
            continue;
        }
        let (Some(width), Some(height)) = (
            side(structure, "width", 640),
            side(structure, "height", 480),
        ) else {
            continue;
        };
        let Some(rate) = structure.value("framerate").ok().and_then(highest_rate) else {
            continue;
        };
        let fps = f64::from(rate.numer()) / f64::from(rate.denom().max(1));
        let mut fixed = gst::Caps::builder(name)
            .field("width", width as i32)
            .field("height", height as i32)
            .field("framerate", rate);
        if let Ok(format) = structure.get::<String>("format") {
            fixed = fixed.field("format", format);
        }
        found.push((
            CameraFormat {
                width,
                height,
                max_fps: fps,
            },
            fixed.build(),
            jpeg,
        ));
    }
    found
}

fn camera() -> Result<Camera, &'static str> {
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Video/Source"), None);
    monitor.start().map_err(|_| NO_DEVICES)?;
    let mut devices: Vec<gst::Device> = monitor.devices().into_iter().collect();
    monitor.stop();
    // PipeWire's camera where PipeWire runs: it shares the camera with
    // other applications; V4L2's otherwise.
    let api = |device: &gst::Device| {
        device
            .properties()
            .and_then(|properties| properties.get::<String>("device.api").ok())
            .unwrap_or_default()
    };
    devices.sort_by_key(|device| u8::from(api(device) != "pipewire"));
    let device = devices.first().ok_or(NO_DEVICES)?;
    let caps = device.caps().ok_or(NO_FORMAT)?;
    let offered = formats(&caps);
    let chosen = closest_format(
        &offered
            .iter()
            .map(|(format, _, _)| *format)
            .collect::<Vec<_>>(),
    )
    .ok_or(NO_FORMAT)?;
    let (format, caps, jpeg) = offered[chosen].clone();
    let element = device.create_element(None).map_err(|_| NO_DEVICES)?;
    Ok(Camera {
        element,
        caps,
        jpeg,
        width: format.width,
        height: format.height,
    })
}

fn audio_caps(format: InputFormat) -> String {
    let little = cfg!(target_endian = "little");
    let name = match format.sample_format {
        SampleFormat::U8 => "U8",
        SampleFormat::I16 if little => "S16LE",
        SampleFormat::I16 => "S16BE",
        SampleFormat::I32 if little => "S32LE",
        SampleFormat::I32 => "S32BE",
        SampleFormat::F32 if little => "F32LE",
        SampleFormat::F32 => "F32BE",
    };
    format!(
        "audio/x-raw,format={name},rate={},channels={},layout=interleaved",
        format.sample_rate, format.channels
    )
}

fn description(
    jpeg: bool,
    width: u32,
    height: u32,
    audio: &str,
    path: &Path,
) -> Result<String, String> {
    let h264 = encoder(&H264)
        .ok_or("Video needs a GStreamer H.264 encoder: x264enc, openh264enc or VA-API.")?;
    let aac =
        encoder(&AAC).ok_or("Video needs a GStreamer AAC encoder: avenc_aac or fdkaacenc.")?;
    let (left, right, top, bottom) = square_crop(width, height);
    let decode = if jpeg { "jpegdec ! " } else { "" };
    let location = path.to_str().ok_or(NO_STORAGE)?.replace('"', "");
    Ok(format!(
        // The preview and poster sinks keep one frame nobody pulls once stopping
        // starts; waiting for it on EOS would hold back the pipeline's EOS.
        "capsfilter name=camcaps ! {decode}videoconvert ! videorate ! video/x-raw,framerate=24/1 ! tee name=t \
         t. ! queue ! videoconvert ! {h264} ! h264parse ! queue ! mux. \
         t. ! queue leaky=downstream max-size-buffers=1 ! videorate drop-only=true max-rate={PREVIEW_FPS} \
            ! videocrop left={left} right={right} top={top} bottom={bottom} ! videoscale ! videoconvert \
            ! video/x-raw,format=I420,width={PREVIEW_SIDE},height={PREVIEW_SIDE} ! tee name=p \
         p. ! queue leaky=downstream max-size-buffers=1 ! jpegenc quality=70 \
            ! appsink name=preview max-buffers=1 drop=true sync=false wait-on-eos=false \
         p. ! queue leaky=downstream max-size-buffers=1 ! videoscale ! videoconvert \
            ! video/x-raw,format=RGB,width={POSTER_SIDE},height={POSTER_SIDE} \
            ! appsink name=poster max-buffers=1 drop=true sync=false wait-on-eos=false \
         appsrc name=audio is-live=true do-timestamp=true format=time caps=\"{audio}\" \
            ! audioconvert ! audioresample ! audio/x-raw,channels=1 ! {aac} ! aacparse ! queue ! mux. \
         mp4mux name=mux faststart=true ! filesink location=\"{location}\""
    ))
}

/// A private folder for the recording, removed when dropped.
struct Folder(PathBuf);

impl Folder {
    fn create() -> Option<Folder> {
        use std::os::unix::fs::DirBuilderExt;
        let path = std::env::temp_dir().join(super::folder_name());
        std::fs::DirBuilder::new().mode(0o700).create(&path).ok()?;
        Some(Folder(path))
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Encodes the last 160 px RGB frame as JPEG at each poster quality.
fn posters(rgb: &[u8]) -> Vec<Vec<u8>> {
    let mut encoded = Vec::new();
    for quality in POSTER_QUALITIES {
        let description = format!(
            "appsrc name=src format=time caps=video/x-raw,format=RGB,width={POSTER_SIDE},height={POSTER_SIDE},framerate=1/1 \
             ! videoconvert ! jpegenc quality={quality} ! appsink name=out sync=false"
        );
        let Ok(pipeline) = gst::parse::launch(&description) else {
            return encoded;
        };
        let Ok(pipeline) = pipeline.downcast::<gst::Pipeline>() else {
            return encoded;
        };
        let source = pipeline
            .by_name("src")
            .and_then(|element| element.downcast::<gst_app::AppSrc>().ok());
        let sink = pipeline
            .by_name("out")
            .and_then(|element| element.downcast::<gst_app::AppSink>().ok());
        let (Some(source), Some(sink)) = (source, sink) else {
            return encoded;
        };
        if pipeline.set_state(gst::State::Playing).is_err() {
            return encoded;
        }
        let _ = source.push_buffer(gst::Buffer::from_slice(rgb.to_vec()));
        let _ = source.end_of_stream();
        let jpeg = sink
            .try_pull_sample(gst::ClockTime::from_seconds(2))
            .and_then(|sample| {
                sample
                    .buffer()
                    .and_then(|buffer| buffer.map_readable().ok().map(|map| map.to_vec()))
            })
            .unwrap_or_default();
        let _ = pipeline.set_state(gst::State::Null);
        encoded.push(jpeg);
    }
    encoded
}

/// The microphone a recording holds; dropping it stops the audio.
enum Microphone {
    Device(#[allow(dead_code)] cpal::Stream),
    Tone(Arc<AtomicBool>),
}

impl Drop for Microphone {
    fn drop(&mut self) {
        if let Microphone::Tone(stop) = self {
            stop.store(true, Ordering::SeqCst);
        }
    }
}

/// A 440 Hz stereo tone in real time, in 20 ms chunks: the test source's
/// microphone, so a test never opens a real one.
fn test_tone(feed: mpsc::Sender<Message>) -> (Microphone, InputFormat) {
    let stop = Arc::new(AtomicBool::new(false));
    let ended = Arc::clone(&stop);
    let _ = std::thread::Builder::new()
        .name("video-test-tone".into())
        .spawn(move || {
            let start = Instant::now();
            let mut frame: u64 = 0;
            while !ended.load(Ordering::SeqCst) {
                let due = (start.elapsed().as_secs_f64() * 48_000.0) as u64;
                let mut bytes = Vec::new();
                while frame < due {
                    let value =
                        0.3 * (frame as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin();
                    bytes.extend_from_slice(&value.to_ne_bytes());
                    bytes.extend_from_slice(&value.to_ne_bytes());
                    frame += 1;
                }
                if !bytes.is_empty() && feed.send(Message::Chunk(bytes)).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
    let format = InputFormat {
        sample_rate: 48_000,
        channels: 2,
        sample_format: SampleFormat::F32,
    };
    (Microphone::Tone(stop), format)
}

enum Message {
    Chunk(Vec<u8>),
    StreamFailed,
    Stop,
    Cancel,
}

/// One round video recording on its own thread; dropping it cancels the
/// recording without waiting.
pub struct VideoRecorder {
    commands: mpsc::Sender<Message>,
    cancelled: Arc<AtomicBool>,
}

impl VideoRecorder {
    /// Starts recording from `source` and the microphone with this ID
    /// (empty for the default).
    pub fn start(
        source: Source,
        microphone: &str,
        events: impl Fn(VideoEvent) + Send + 'static,
    ) -> VideoRecorder {
        let (commands, inbox) = mpsc::channel();
        let feed = commands.clone();
        let microphone = microphone.to_owned();
        let cancelled = Arc::new(AtomicBool::new(false));
        let (live, ended) = (Arc::clone(&cancelled), Arc::clone(&cancelled));
        let report = move |event| {
            if !live.load(Ordering::SeqCst) {
                events(event);
            }
        };
        let started = std::thread::Builder::new()
            .name("video-capture".into())
            .spawn(move || {
                if let Err(error) = record(source, &microphone, feed, &inbox, &ended, &report) {
                    report(VideoEvent::Failed(error));
                }
            });
        if started.is_err() {
            cancelled.store(true, Ordering::SeqCst);
        }
        VideoRecorder {
            commands,
            cancelled,
        }
    }

    pub fn stop(&self) {
        let _ = self.commands.send(Message::Stop);
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let _ = self.commands.send(Message::Cancel);
    }
}

impl Drop for VideoRecorder {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn element<T: IsA<gst::Element>>(pipeline: &gst::Pipeline, name: &str) -> Result<T, String> {
    pipeline
        .by_name(name)
        .and_then(|element| element.downcast::<T>().ok())
        .ok_or_else(|| format!("The video pipeline has no {name}."))
}

fn pull(sink: &gst_app::AppSink) -> Option<Vec<u8>> {
    let sample = sink.try_pull_sample(gst::ClockTime::ZERO)?;
    let buffer = sample.buffer()?;
    let map = buffer.map_readable().ok()?;
    Some(map.to_vec())
}

fn record(
    source: Source,
    microphone: &str,
    feed: mpsc::Sender<Message>,
    inbox: &mpsc::Receiver<Message>,
    cancelled: &AtomicBool,
    events: &dyn Fn(VideoEvent),
) -> Result<(), String> {
    if let Some(reason) = super::video_reason() {
        return Err(reason);
    }
    let camera = match source {
        Source::Camera => camera().map_err(str::to_owned)?,
        Source::Test => Camera {
            element: gst::ElementFactory::make("videotestsrc")
                .property("is-live", true)
                .property_from_str("pattern", "ball")
                .build()
                .map_err(|error| error.to_string())?,
            caps: gst::Caps::builder("video/x-raw")
                .field("width", 640)
                .field("height", 480)
                .field("framerate", gst::Fraction::new(30, 1))
                .build(),
            jpeg: false,
            width: 640,
            height: 480,
        },
    };
    let folder = Folder::create().ok_or(NO_STORAGE)?;
    let path = folder.0.join("video.mp4");
    let (stream, format) = match source {
        Source::Camera => {
            let error_feed = feed.clone();
            let (stream, format) = device::open(
                microphone,
                move |bytes| {
                    let _ = feed.send(Message::Chunk(bytes));
                },
                move || {
                    let _ = error_feed.send(Message::StreamFailed);
                },
            )
            .map_err(|(_, text)| {
                if text == crate::UNAVAILABLE {
                    NO_DEVICES.to_owned()
                } else {
                    text.to_owned()
                }
            })?;
            (Microphone::Device(stream), format)
        }
        Source::Test => test_tone(feed),
    };
    let mut session = Session::new(format, Instant::now()).ok_or(crate::UNSUPPORTED)?;
    let description = description(
        camera.jpeg,
        camera.width,
        camera.height,
        &audio_caps(format),
        &path,
    )?;
    let pipeline = gst::parse::launch(&description)
        .map_err(|error| error.to_string())?
        .downcast::<gst::Pipeline>()
        .map_err(|_| "The video pipeline could not be built.".to_owned())?;
    let teardown = |pipeline: &gst::Pipeline| {
        let _ = pipeline.set_state(gst::State::Null);
    };
    let filter: gst::Element = element(&pipeline, "camcaps")?;
    filter.set_property("caps", &camera.caps);
    pipeline
        .add(&camera.element)
        .map_err(|error| error.to_string())?;
    camera
        .element
        .link(&filter)
        .map_err(|error| error.to_string())?;
    let audio: gst_app::AppSrc = element(&pipeline, "audio")?;
    let preview: gst_app::AppSink = element(&pipeline, "preview")?;
    let poster_sink: gst_app::AppSink = element(&pipeline, "poster")?;
    let bus = pipeline.bus().ok_or("The video pipeline has no bus.")?;
    if let Err(error) = pipeline.set_state(gst::State::Playing) {
        teardown(&pipeline);
        return Err(error.to_string());
    }
    let opened = Instant::now();
    let mut started = false;
    let mut last_tick = Instant::now();
    let mut seconds = 0;
    let mut duration_ms = 0;
    let mut last_poster: Option<Vec<u8>> = None;
    let fail = |pipeline: &gst::Pipeline, text: String| {
        let _ = pipeline.set_state(gst::State::Null);
        Err(text)
    };
    loop {
        match inbox.recv_timeout(Duration::from_millis(20)) {
            Ok(Message::Chunk(bytes)) => {
                for event in session.chunk(&bytes, Instant::now()) {
                    if let Event::Level { rms, peak } = event {
                        events(VideoEvent::Level { rms, peak });
                    }
                }
                let _ = audio.push_buffer(gst::Buffer::from_slice(bytes));
            }
            Ok(Message::StreamFailed) => return fail(&pipeline, crate::FAILED.into()),
            Ok(Message::Cancel) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                teardown(&pipeline);
                return Ok(());
            }
            Ok(Message::Stop) if started => break,
            Ok(Message::Stop) => {
                // Stopped before the camera started: nothing to keep.
                teardown(&pipeline);
                return Ok(());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if cancelled.load(Ordering::SeqCst) {
            teardown(&pipeline);
            return Ok(());
        }
        while let Some(message) = bus.timed_pop(gst::ClockTime::ZERO) {
            match message.view() {
                gst::MessageView::Error(error) => {
                    return fail(&pipeline, error.error().to_string())
                }
                gst::MessageView::StateChanged(change)
                    if !started
                        && change.current() == gst::State::Playing
                        && message
                            .src()
                            .is_some_and(|src| src == pipeline.upcast_ref::<gst::Object>()) =>
                {
                    started = true;
                    events(VideoEvent::Started);
                }
                _ => {}
            }
        }
        if let Some(jpeg) = pull(&preview) {
            events(VideoEvent::Frame {
                jpeg,
                mirrored: false,
            });
        }
        if let Some(rgb) = pull(&poster_sink) {
            last_poster = Some(rgb);
        }
        if last_tick.elapsed() >= TICK {
            last_tick = Instant::now();
            if !started && opened.elapsed() >= START_TIMEOUT {
                return fail(&pipeline, NOT_STARTED.into());
            }
            if started {
                duration_ms = pipeline
                    .query_position::<gst::ClockTime>()
                    .map_or(duration_ms, gst::ClockTime::mseconds);
                let now = (duration_ms / 1000) as u32;
                if now != seconds {
                    seconds = now;
                    events(VideoEvent::Elapsed(seconds));
                }
                let size = std::fs::metadata(&path).map_or(0, |meta| meta.len());
                if must_stop(duration_ms, size) {
                    break;
                }
            }
        }
    }
    events(VideoEvent::Preparing);
    duration_ms = pipeline
        .query_position::<gst::ClockTime>()
        .map_or(duration_ms, gst::ClockTime::mseconds);
    // Stop the microphone, take what it already delivered, then end both
    // tracks and wait for the file to be finished.
    drop(stream);
    while let Ok(message) = inbox.try_recv() {
        match message {
            Message::Chunk(bytes) => {
                session.chunk(&bytes, Instant::now());
                let _ = audio.push_buffer(gst::Buffer::from_slice(bytes));
            }
            Message::Cancel => {
                teardown(&pipeline);
                return Ok(());
            }
            _ => {}
        }
    }
    let _ = audio.end_of_stream();
    pipeline.send_event(gst::event::Eos::new());
    // A loaded machine may need a while to encode what is queued and write
    // the file; the window shows Preparing meanwhile.
    let deadline = Instant::now() + FINISH_TIMEOUT;
    let mut ended = false;
    while !ended && Instant::now() < deadline {
        if let Some(message) =
            bus.timed_pop(gst::ClockTime::from_mseconds(FINISH_POLL.as_millis() as u64))
        {
            match message.view() {
                gst::MessageView::Eos(..) => ended = true,
                gst::MessageView::Error(error) => {
                    return fail(&pipeline, error.error().to_string())
                }
                _ => {}
            }
        }
        if cancelled.load(Ordering::SeqCst) {
            teardown(&pipeline);
            return Ok(());
        }
    }
    teardown(&pipeline);
    if !ended {
        return Err(refused(format!(
            "the file was not finished within {} s",
            FINISH_TIMEOUT.as_secs()
        )));
    }
    let mp4 = std::fs::read(&path).map_err(|error| refused(format!("cannot read it: {error}")))?;
    if mp4.len() as u64 > MAX_BYTES {
        return Err(refused(format!("{} bytes", mp4.len())));
    }
    let pcm = session.into_pcm();
    let poster = last_poster.and_then(|rgb| poster(posters(&rgb)));
    let meta = finish(
        duration_ms,
        camera.width,
        camera.height,
        mp4.len() as u64,
        pcm.len(),
        poster.as_deref(),
    )
    .map_err(|error| finish_refused(error, duration_ms, camera.width, camera.height))?;
    events(VideoEvent::Completed { pcm, mp4, meta });
    drop(folder);
    Ok(())
}
