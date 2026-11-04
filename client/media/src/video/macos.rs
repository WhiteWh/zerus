//! Round video on macOS through AVFoundation: a capture session with the
//! camera and the microphone, whose sample buffers an AVAssetWriter writes
//! straight to MP4 (H.264 at 700 kbit/s, AAC mono at 64 kbit/s). The camera
//! format closest to 640x480 at 15 fps or more is chosen and asked for 24
//! fps. The PCM comes from the same microphone through cpal, as on Linux;
//! preview frames and the poster are cut from the camera's BGRA frames.
//!
//! Like Qt, the first press asks for camera and microphone consent and ends
//! there: a grant needs a fresh press, never a late capture.
use super::{
    closest_format, finish, finish_refused, must_stop, poster_from_rgb, refused, square_rgb,
    CameraFormat, VideoEvent, FINISH_POLL, FINISH_TIMEOUT, MAX_BYTES, NOT_STARTED, NO_DEVICES,
    NO_FORMAT, NO_STORAGE, POSTER_SIDE, PREVIEW_FPS, PREVIEW_SIDE, START_TIMEOUT, TICK,
};
use crate::{device, Event, Session};
use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVAssetWriterStatus, AVAuthorizationStatus,
    AVCaptureAudioDataOutput, AVCaptureAudioDataOutputSampleBufferDelegate, AVCaptureConnection,
    AVCaptureDevice, AVCaptureDeviceInput, AVCaptureDevicePosition, AVCaptureOutput,
    AVCaptureSession, AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate,
    AVFileTypeMPEG4, AVMediaTypeAudio, AVMediaTypeVideo, AVVideoAverageBitRateKey, AVVideoCodecKey,
    AVVideoCodecTypeH264, AVVideoCompressionPropertiesKey, AVVideoExpectedSourceFrameRateKey,
    AVVideoHeightKey, AVVideoMaxKeyFrameIntervalKey, AVVideoWidthKey,
};
use objc2_core_media::{CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetDimensions};
use objc2_core_video::{
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA, CVPixelBufferGetBaseAddress,
    CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight, CVPixelBufferGetWidth,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{
    NSCopying, NSDictionary, NSNumber, NSObject, NSObjectProtocol, NSString, NSURL,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

pub const ASK_ACCESS: &str = "Allow camera and microphone access, then hold Video again.";
pub const DENIED_ACCESS: &str =
    "Allow camera and microphone access in system settings, then hold Video again.";
/// `kAudioFormatMPEG4AAC`.
const AAC: u32 = u32::from_be_bytes(*b"aac ");

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Camera and microphone consent: Ok when both are granted. The first time
/// it asks and fails, so the grant needs a fresh press.
fn consent() -> Result<(), &'static str> {
    // SAFETY: AVFoundation exports these constant strings.
    let (Some(video), Some(audio)) = (unsafe { AVMediaTypeVideo }, unsafe { AVMediaTypeAudio })
    else {
        return Err(DENIED_ACCESS);
    };
    for media in [video, audio] {
        // SAFETY: video and audio are the two media types it accepts.
        let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) };
        if status == AVAuthorizationStatus::NotDetermined {
            let handler = RcBlock::new(|_granted: Bool| {});
            // SAFETY: an accepted media type; the answer is not awaited.
            unsafe {
                AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &handler)
            };
            return Err(ASK_ACCESS);
        }
        if status != AVAuthorizationStatus::Authorized {
            return Err(DENIED_ACCESS);
        }
    }
    Ok(())
}

fn number(value: f64) -> Retained<AnyObject> {
    Retained::<AnyObject>::from(NSNumber::new_f64(value))
}

fn integer(value: i64) -> Retained<AnyObject> {
    Retained::<AnyObject>::from(NSNumber::new_i64(value))
}

fn key(name: Option<&'static NSString>) -> Result<&'static NSString, String> {
    name.ok_or_else(|| "AVFoundation is missing a video setting.".to_owned())
}

fn video_settings(
    width: u32,
    height: u32,
) -> Result<Retained<NSDictionary<NSString, AnyObject>>, String> {
    // SAFETY: AVFoundation exports these constant strings.
    unsafe {
        let compression = NSDictionary::from_retained_objects(
            &[
                key(AVVideoAverageBitRateKey)?,
                key(AVVideoExpectedSourceFrameRateKey)?,
                key(AVVideoMaxKeyFrameIntervalKey)?,
            ],
            &[integer(700_000), integer(24), integer(48)],
        );
        let codec = AVVideoCodecTypeH264.ok_or("AVFoundation has no H.264 codec.")?;
        Ok(NSDictionary::from_retained_objects(
            &[
                key(AVVideoCodecKey)?,
                key(AVVideoWidthKey)?,
                key(AVVideoHeightKey)?,
                key(AVVideoCompressionPropertiesKey)?,
            ],
            &[
                Retained::<AnyObject>::from(codec.copy()),
                integer(i64::from(width)),
                integer(i64::from(height)),
                Retained::<AnyObject>::from(compression),
            ],
        ))
    }
}

fn audio_settings() -> Retained<NSDictionary<NSString, AnyObject>> {
    // The AVFAudio keys, by their values.
    NSDictionary::from_retained_objects(
        &[
            &*NSString::from_str("AVFormatIDKey"),
            &*NSString::from_str("AVNumberOfChannelsKey"),
            &*NSString::from_str("AVSampleRateKey"),
            &*NSString::from_str("AVEncoderBitRateKey"),
        ],
        &[
            integer(i64::from(AAC)),
            integer(1),
            number(48_000.0),
            integer(64_000),
        ],
    )
}

/// What the capture queue and the recorder thread share.
struct Writing {
    writer: Retained<AVAssetWriter>,
    video: Retained<AVAssetWriterInput>,
    audio: Retained<AVAssetWriterInput>,
    /// The first video frame's time: the writer's session start.
    start: Option<CMTime>,
    /// Seconds written so far.
    seconds: f64,
    finishing: bool,
    failed: Option<String>,
    last_frame: Option<Instant>,
    last_poster: Option<Instant>,
    poster: Option<Vec<u8>>,
}

// SAFETY: the writer and its inputs are used only under the Mutex that
// holds them, one caller at a time (the capture queue or the recorder
// thread), as AVAssetWriter requires.
unsafe impl Send for Writing {}

struct Shared {
    writing: Mutex<Writing>,
    /// The video data output, to tell its buffers from the microphone's.
    video_output: usize,
    mirrored: bool,
    events: Box<dyn Fn(VideoEvent) + Send + Sync>,
}

impl Shared {
    fn frame(&self, buffer: &CMSampleBuffer, writing: &mut Writing) {
        let now = Instant::now();
        let preview_due = writing
            .last_frame
            .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(1) / PREVIEW_FPS);
        let poster_due = writing
            .last_poster
            .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(1));
        if !preview_due && !poster_due {
            return;
        }
        // SAFETY: the buffer is valid for this callback.
        let Some(image) = (unsafe { buffer.image_buffer() }) else {
            return;
        };
        // SAFETY: locked read-only here and unlocked below.
        if unsafe { CVPixelBufferLockBaseAddress(&image, CVPixelBufferLockFlags::ReadOnly) } != 0 {
            return;
        }
        let (width, height, stride) = (
            CVPixelBufferGetWidth(&image),
            CVPixelBufferGetHeight(&image),
            CVPixelBufferGetBytesPerRow(&image),
        );
        let base = CVPixelBufferGetBaseAddress(&image);
        if !base.is_null() && height > 0 {
            // SAFETY: the locked buffer holds `stride * height` bytes.
            let bgra = unsafe { std::slice::from_raw_parts(base.cast::<u8>(), stride * height) };
            if preview_due {
                writing.last_frame = Some(now);
                if let Some(jpeg) = square_rgb(bgra, width, height, stride, PREVIEW_SIDE as usize)
                    .and_then(|rgb| super::jpeg(&rgb, PREVIEW_SIDE as u16, 70))
                {
                    (self.events)(VideoEvent::Frame {
                        jpeg,
                        mirrored: self.mirrored,
                    });
                }
            }
            if poster_due {
                writing.last_poster = Some(now);
                writing.poster = square_rgb(bgra, width, height, stride, POSTER_SIDE as usize);
            }
        }
        // SAFETY: locked above.
        unsafe { CVPixelBufferUnlockBaseAddress(&image, CVPixelBufferLockFlags::ReadOnly) };
    }

    fn sample(&self, output: &AVCaptureOutput, buffer: &CMSampleBuffer) {
        let mut writing = lock(&self.writing);
        if writing.finishing || writing.failed.is_some() {
            return;
        }
        let video = std::ptr::from_ref(output) as usize == self.video_output;
        // SAFETY: the buffer is valid for this callback.
        let time = unsafe { buffer.presentation_time_stamp() };
        // SAFETY: the writer and its inputs are used on this serial queue
        // and under the lock.
        unsafe {
            if video {
                if writing.start.is_none() {
                    if !writing.writer.startWriting() {
                        writing.failed = Some(writing.writer.error().map_or_else(
                            || super::INVALID.to_owned(),
                            |error| error.localizedDescription().to_string(),
                        ));
                        return;
                    }
                    writing.writer.startSessionAtSourceTime(time);
                    writing.start = Some(time);
                    (self.events)(VideoEvent::Started);
                }
                if writing.video.isReadyForMoreMediaData() {
                    writing.video.appendSampleBuffer(buffer);
                }
                if let Some(start) = writing.start {
                    writing.seconds = time.seconds() - start.seconds();
                }
                self.frame(buffer, &mut writing);
            } else if writing.start.is_some() && writing.audio.isReadyForMoreMediaData() {
                writing.audio.appendSampleBuffer(buffer);
            }
            if writing.writer.status() == AVAssetWriterStatus::Failed {
                writing.failed = Some(writing.writer.error().map_or_else(
                    || super::INVALID.to_owned(),
                    |error| error.localizedDescription().to_string(),
                ));
            }
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "ZerusRoundVideoCapture"]
    #[ivars = Arc<Shared>]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for Delegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn did_output(
            &self,
            output: &AVCaptureOutput,
            buffer: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            self.ivars().sample(output, buffer);
        }
    }

    unsafe impl AVCaptureAudioDataOutputSampleBufferDelegate for Delegate {}
);

impl Delegate {
    fn new(shared: Arc<Shared>) -> Retained<Delegate> {
        let this = Self::alloc().set_ivars(shared);
        // SAFETY: NSObject's init on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

struct Camera {
    device: Retained<AVCaptureDevice>,
    width: u32,
    height: u32,
    mirrored: bool,
}

/// The default camera at the format closest to 640x480, asked for 24 fps
/// where the format allows; it stays locked for configuration until the
/// session runs.
fn camera() -> Result<Camera, &'static str> {
    // SAFETY: AVFoundation exports this constant string.
    let video = unsafe { AVMediaTypeVideo }.ok_or(NO_DEVICES)?;
    // SAFETY: a plain device lookup.
    let device = unsafe { AVCaptureDevice::defaultDeviceWithMediaType(video) }.ok_or(NO_DEVICES)?;
    // SAFETY: the device's formats, read-only.
    let formats = unsafe { device.formats() };
    let mut offered = Vec::new();
    for format in &formats {
        // SAFETY: a capture format always has a video description.
        let dimensions =
            unsafe { CMVideoFormatDescriptionGetDimensions(&format.formatDescription()) };
        // SAFETY: read-only properties of the format.
        let max_fps = unsafe { format.videoSupportedFrameRateRanges() }
            .iter()
            .map(|range| unsafe { range.maxFrameRate() })
            .fold(0.0, f64::max);
        offered.push((
            CameraFormat {
                width: u32::try_from(dimensions.width).unwrap_or(0),
                height: u32::try_from(dimensions.height).unwrap_or(0),
                max_fps,
            },
            format,
        ));
    }
    let chosen = closest_format(
        &offered
            .iter()
            .map(|(format, _)| *format)
            .collect::<Vec<_>>(),
    )
    .ok_or(NO_FORMAT)?;
    let (format, active) = &offered[chosen];
    // SAFETY: locked before configuring; unlocked in `record` once the
    // session runs, so the format stays.
    unsafe {
        device.lockForConfiguration().map_err(|_| NO_DEVICES)?;
        device.setActiveFormat(active);
        let rate = active
            .videoSupportedFrameRateRanges()
            .iter()
            .any(|range| range.minFrameRate() <= 24.0 && range.maxFrameRate() >= 24.0);
        if rate {
            let frame = CMTime::new(1, 24);
            device.setActiveVideoMinFrameDuration(frame);
            device.setActiveVideoMaxFrameDuration(frame);
        }
    }
    // SAFETY: a read-only property.
    let mirrored = unsafe { device.position() } == AVCaptureDevicePosition::Front;
    Ok(Camera {
        device,
        width: format.width,
        height: format.height,
        mirrored,
    })
}

/// The microphone with the settings' ID (cpal's device ID carries Core
/// Audio's unique ID after its host), else the default one.
fn microphone(id: &str) -> Option<Retained<AVCaptureDevice>> {
    // SAFETY: AVFoundation exports this constant string.
    let audio = unsafe { AVMediaTypeAudio }?;
    let unique = id.split_once(':').map_or(id, |(_, unique)| unique);
    if !unique.is_empty() {
        #[allow(deprecated)]
        // SAFETY: a plain device list.
        let found = unsafe { AVCaptureDevice::devicesWithMediaType(audio) }
            .iter()
            .find(|device| unsafe { device.uniqueID() }.to_string() == unique);
        if found.is_some() {
            return found;
        }
    }
    // SAFETY: a plain device lookup.
    unsafe { AVCaptureDevice::defaultDeviceWithMediaType(audio) }
}

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
    /// Starts recording from the default camera and the microphone with
    /// this ID (empty for the default).
    pub fn start(
        microphone: &str,
        events: impl Fn(VideoEvent) + Send + Sync + 'static,
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
                let report: Arc<dyn Fn(VideoEvent) + Send + Sync> = Arc::new(report);
                if let Err(error) = record(&microphone, feed, &inbox, &ended, Arc::clone(&report)) {
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

fn writer(path: &Path, width: u32, height: u32) -> Result<Writing, String> {
    // SAFETY: plain object creation with constant AVFoundation strings.
    unsafe {
        let url = NSURL::fileURLWithPath(&NSString::from_str(path.to_str().ok_or(NO_STORAGE)?));
        let kind = AVFileTypeMPEG4.ok_or(NO_STORAGE)?;
        let writer = AVAssetWriter::assetWriterWithURL_fileType_error(&url, kind)
            .map_err(|error| error.localizedDescription().to_string())?;
        let video_type = AVMediaTypeVideo.ok_or(NO_DEVICES)?;
        let audio_type = AVMediaTypeAudio.ok_or(NO_DEVICES)?;
        let (video_settings, audio_settings) = (video_settings(width, height)?, audio_settings());
        let video = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
            video_type,
            Some(&*video_settings),
        );
        let audio = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
            audio_type,
            Some(&*audio_settings),
        );
        video.setExpectsMediaDataInRealTime(true);
        audio.setExpectsMediaDataInRealTime(true);
        if !writer.canAddInput(&video) || !writer.canAddInput(&audio) {
            return Err(super::INVALID.into());
        }
        writer.addInput(&video);
        writer.addInput(&audio);
        Ok(Writing {
            writer,
            video,
            audio,
            start: None,
            seconds: 0.0,
            finishing: false,
            failed: None,
            last_frame: None,
            last_poster: None,
            poster: None,
        })
    }
}

fn record(
    microphone_id: &str,
    feed: mpsc::Sender<Message>,
    inbox: &mpsc::Receiver<Message>,
    cancelled: &AtomicBool,
    events: Arc<dyn Fn(VideoEvent) + Send + Sync>,
) -> Result<(), String> {
    consent().map_err(str::to_owned)?;
    let camera = camera().map_err(str::to_owned)?;
    let unlock = |camera: &Camera| {
        // SAFETY: locked in `camera`.
        unsafe { camera.device.unlockForConfiguration() };
    };
    let Some(microphone) = microphone(microphone_id) else {
        unlock(&camera);
        return Err(NO_DEVICES.into());
    };
    let folder = Folder::create().ok_or(NO_STORAGE)?;
    let path = folder.0.join("video.mp4");
    let error_feed = feed.clone();
    let (stream, format) = device::open(
        microphone_id,
        move |bytes| {
            let _ = feed.send(Message::Chunk(bytes));
        },
        move || {
            let _ = error_feed.send(Message::StreamFailed);
        },
    )
    .map_err(|(_, text)| text.to_owned())?;
    let mut pcm = Session::new(format, Instant::now()).ok_or(crate::UNSUPPORTED)?;
    // SAFETY: objects created and configured on this thread; the delegate
    // queue only appends samples under the shared lock.
    let (session, shared) = unsafe {
        let session = AVCaptureSession::new();
        let camera_input = AVCaptureDeviceInput::deviceInputWithDevice_error(&camera.device)
            .map_err(|error| error.localizedDescription().to_string())?;
        let microphone_input = AVCaptureDeviceInput::deviceInputWithDevice_error(&microphone)
            .map_err(|error| error.localizedDescription().to_string())?;
        let video_output = AVCaptureVideoDataOutput::new();
        // CFString and NSString are toll-free bridged.
        let pixel_key: &NSString =
            &*std::ptr::from_ref(kCVPixelBufferPixelFormatTypeKey).cast::<NSString>();
        let pixel = NSDictionary::from_retained_objects(
            &[pixel_key],
            &[integer(i64::from(kCVPixelFormatType_32BGRA))],
        );
        video_output.setVideoSettings(Some(&pixel));
        video_output.setAlwaysDiscardsLateVideoFrames(true);
        let audio_output = AVCaptureAudioDataOutput::new();
        let shared = Arc::new(Shared {
            writing: Mutex::new(writer(&path, camera.width, camera.height)?),
            video_output: Retained::as_ptr(&video_output) as usize,
            mirrored: camera.mirrored,
            events: Box::new({
                let events = Arc::clone(&events);
                move |event| events(event)
            }),
        });
        let delegate = Delegate::new(Arc::clone(&shared));
        let queue = DispatchQueue::new("dev.zerus.round-video", None);
        video_output.setSampleBufferDelegate_queue(
            Some(ProtocolObject::from_ref(&*delegate)),
            Some(&queue),
        );
        audio_output.setSampleBufferDelegate_queue(
            Some(ProtocolObject::from_ref(&*delegate)),
            Some(&queue),
        );
        session.beginConfiguration();
        let video_out: &objc2_av_foundation::AVCaptureOutput = &video_output;
        let audio_out: &objc2_av_foundation::AVCaptureOutput = &audio_output;
        if !session.canAddInput(&camera_input)
            || !session.canAddInput(&microphone_input)
            || !session.canAddOutput(video_out)
            || !session.canAddOutput(audio_out)
        {
            session.commitConfiguration();
            unlock(&camera);
            return Err(NO_DEVICES.into());
        }
        session.addInput(&camera_input);
        session.addInput(&microphone_input);
        session.addOutput(video_out);
        session.addOutput(audio_out);
        session.commitConfiguration();
        session.startRunning();
        unlock(&camera);
        (session, shared)
    };
    let stop_session = || {
        // SAFETY: the session was started on this thread.
        unsafe { session.stopRunning() };
    };
    let opened = Instant::now();
    let mut last_tick = Instant::now();
    let mut seconds = 0;
    loop {
        match inbox.recv_timeout(Duration::from_millis(20)) {
            Ok(Message::Chunk(bytes)) => {
                for event in pcm.chunk(&bytes, Instant::now()) {
                    if let Event::Level { rms, peak } = event {
                        events(VideoEvent::Level { rms, peak });
                    }
                }
            }
            Ok(Message::StreamFailed) => {
                stop_session();
                return Err(crate::FAILED.into());
            }
            Ok(Message::Cancel) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                stop_session();
                return Ok(());
            }
            Ok(Message::Stop) => {
                if lock(&shared.writing).start.is_some() {
                    break;
                }
                stop_session();
                return Ok(());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if cancelled.load(Ordering::SeqCst) {
            stop_session();
            return Ok(());
        }
        if last_tick.elapsed() >= TICK {
            last_tick = Instant::now();
            let (started, written, failed) = {
                let writing = lock(&shared.writing);
                (
                    writing.start.is_some(),
                    writing.seconds,
                    writing.failed.clone(),
                )
            };
            if let Some(error) = failed {
                stop_session();
                return Err(error);
            }
            if !started && opened.elapsed() >= START_TIMEOUT {
                stop_session();
                return Err(NOT_STARTED.into());
            }
            if started {
                let now = written.max(0.0) as u32;
                if now != seconds {
                    seconds = now;
                    events(VideoEvent::Elapsed(seconds));
                }
                let size = std::fs::metadata(&path).map_or(0, |meta| meta.len());
                if must_stop((written.max(0.0) * 1000.0) as u64, size) {
                    break;
                }
            }
        }
    }
    events(VideoEvent::Preparing);
    stop_session();
    drop(stream);
    while let Ok(message) = inbox.try_recv() {
        match message {
            Message::Chunk(bytes) => {
                pcm.chunk(&bytes, Instant::now());
            }
            Message::Cancel => return Ok(()),
            _ => {}
        }
    }
    let (done, finished) = mpsc::channel();
    let (duration_ms, poster_rgb) = {
        let mut writing = lock(&shared.writing);
        writing.finishing = true;
        let handler = RcBlock::new(move || {
            let _ = done.send(());
        });
        // SAFETY: the session stopped, so nothing appends any more.
        unsafe {
            writing.video.markAsFinished();
            writing.audio.markAsFinished();
            writing.writer.finishWritingWithCompletionHandler(&handler);
        }
        (
            (writing.seconds.max(0.0) * 1000.0) as u64,
            writing.poster.take(),
        )
    };
    // Wait for the writer in steps that check Cancel, so Cancel while
    // Preparing is prompt.
    let deadline = Instant::now() + FINISH_TIMEOUT;
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Ok(());
        }
        match finished.recv_timeout(FINISH_POLL) {
            Ok(()) => break,
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(refused(format!(
                    "the file was not finished within {} s",
                    FINISH_TIMEOUT.as_secs()
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(refused("the writer never reported its end"))
            }
        }
    }
    if cancelled.load(Ordering::SeqCst) {
        return Ok(());
    }
    // SAFETY: a read-only property after the writing finished.
    let status = unsafe { lock(&shared.writing).writer.status() };
    if status != AVAssetWriterStatus::Completed {
        return Err(refused(format!("writer status {}", status.0)));
    }
    let mp4 = std::fs::read(&path).map_err(|error| refused(format!("cannot read it: {error}")))?;
    if mp4.len() as u64 > MAX_BYTES {
        return Err(refused(format!("{} bytes", mp4.len())));
    }
    let pcm = pcm.into_pcm();
    let poster = poster_rgb.and_then(|rgb| poster_from_rgb(&rgb));
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
