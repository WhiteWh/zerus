//! Round video on Windows through Media Foundation: a source reader gives
//! the camera's frames as RGB32 (the reader converts), and a sink writer
//! encodes H.264 at 700 kbit/s and AAC mono into MP4. The microphone comes
//! through cpal, into both the AAC track (16-bit mono at 44.1 or 48 kHz) and
//! the PCM. Preview frames and the poster are cut from the RGB32 frames.
//!
//! Media Foundation's AAC encoder takes 96, 128, 160 or 192 kbit/s, so the
//! audio track uses its lowest, 96 kbit/s, where Qt asks for 64.
use super::{
    closest_format, finish, finish_refused, must_stop, poster_from_rgb, refused, square_rgb,
    CameraFormat, MonoPcm, VideoEvent, MAX_BYTES, NOT_STARTED, NO_DEVICES, NO_FORMAT, NO_STORAGE,
    POSTER_SIDE, PREVIEW_FPS, PREVIEW_SIDE, START_TIMEOUT, TICK,
};
use crate::{device, Event, Session};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use windows::core::{GUID, HSTRING};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFActivate, IMFAttributes, IMFMediaSource, IMFMediaType, IMFSample,
    IMFSinkWriter, IMFSourceReader, MFAudioFormat_AAC, MFAudioFormat_PCM, MFCreateAttributes,
    MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFCreateSinkWriterFromURL,
    MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MFMediaType_Audio, MFMediaType_Video,
    MFShutdown, MFStartup, MFVideoFormat_H264, MFVideoFormat_RGB32, MFVideoInterlace_Progressive,
    MFSTARTUP_FULL, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
    MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS,
    MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
    MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_VERSION,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED,
};

pub const DENIED_ACCESS: &str =
    "Allow camera and microphone access in system settings, then hold Video again.";
/// `E_ACCESSDENIED`: the camera is off in Windows' privacy settings.
const ACCESS_DENIED: i32 = 0x8007_0005_u32 as i32;
/// AAC at Media Foundation's lowest rate, 96 kbit/s.
const AAC_BYTES_PER_SECOND: u32 = 12_000;
const VIDEO_BITRATE: u32 = 700_000;
/// 100 ns units a second, Media Foundation's time base.
const UNITS: i64 = 10_000_000;

fn error_text(error: &windows::core::Error) -> String {
    if error.code().0 == ACCESS_DENIED {
        DENIED_ACCESS.to_owned()
    } else {
        error.message()
    }
}

fn packed(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

fn attributes(count: u32) -> windows::core::Result<IMFAttributes> {
    let mut attributes = None;
    // SAFETY: creates an empty attribute store.
    unsafe { MFCreateAttributes(&mut attributes, count)? };
    attributes.ok_or_else(|| windows::core::Error::from_hresult(windows::core::HRESULT(-1)))
}

/// Media Foundation for this thread, shut down when dropped.
struct Runtime;

impl Runtime {
    fn start() -> Result<Runtime, String> {
        // SAFETY: once per recording thread, undone in Drop.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|error| error.message())?;
        }
        Ok(Runtime)
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // SAFETY: started in Runtime::start on this thread.
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

struct Camera {
    source: IMFMediaSource,
    reader: IMFSourceReader,
    width: u32,
    height: u32,
}

/// The first camera, at the native format closest to 640x480 at 15 fps or
/// more, read as RGB32.
fn camera() -> Result<Camera, String> {
    // SAFETY: device enumeration with a freshly created attribute store;
    // the returned array is freed with CoTaskMemFree.
    unsafe {
        let filter = attributes(1).map_err(|error| error.message())?;
        filter
            .SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )
            .map_err(|error| error.message())?;
        let mut devices: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFEnumDeviceSources(&filter, &mut devices, &mut count).map_err(|error| error.message())?;
        let list = if devices.is_null() {
            Vec::new()
        } else {
            let list: Vec<Option<IMFActivate>> = (0..count as usize)
                .map(|index| (*devices.add(index)).take())
                .collect();
            CoTaskMemFree(Some(devices.cast()));
            list
        };
        let activate = list.into_iter().flatten().next().ok_or(NO_DEVICES)?;
        let source: IMFMediaSource = activate
            .ActivateObject()
            .map_err(|error| error_text(&error))?;
        let options = attributes(1).map_err(|error| error.message())?;
        options
            .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
            .map_err(|error| error.message())?;
        let reader = MFCreateSourceReaderFromMediaSource(&source, &options)
            .map_err(|error| error_text(&error))?;
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let mut offered: Vec<(CameraFormat, IMFMediaType)> = Vec::new();
        let mut index = 0;
        while let Ok(native) = reader.GetNativeMediaType(stream, index) {
            index += 1;
            let (Ok(size), Ok(rate)) = (
                native.GetUINT64(&MF_MT_FRAME_SIZE),
                native.GetUINT64(&MF_MT_FRAME_RATE),
            ) else {
                continue;
            };
            let (numerator, denominator) = ((rate >> 32) as u32, rate as u32);
            offered.push((
                CameraFormat {
                    width: (size >> 32) as u32,
                    height: size as u32,
                    max_fps: f64::from(numerator) / f64::from(denominator.max(1)),
                },
                native,
            ));
        }
        let chosen = closest_format(
            &offered
                .iter()
                .map(|(format, _)| *format)
                .collect::<Vec<_>>(),
        )
        .ok_or(NO_FORMAT)?;
        let (format, native) = &offered[chosen];
        reader
            .SetCurrentMediaType(stream, None, native)
            .map_err(|error| error.message())?;
        let rgb = MFCreateMediaType().map_err(|error| error.message())?;
        rgb.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|error| error.message())?;
        rgb.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
            .map_err(|error| error.message())?;
        rgb.SetUINT64(&MF_MT_FRAME_SIZE, packed(format.width, format.height))
            .map_err(|error| error.message())?;
        reader
            .SetCurrentMediaType(stream, None, &rgb)
            .map_err(|error| error.message())?;
        Ok(Camera {
            source,
            reader,
            width: format.width,
            height: format.height,
        })
    }
}

fn media_type(major: &GUID, subtype: &GUID) -> windows::core::Result<IMFMediaType> {
    // SAFETY: a fresh media type, filled here.
    unsafe {
        let kind = MFCreateMediaType()?;
        kind.SetGUID(&MF_MT_MAJOR_TYPE, major)?;
        kind.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        Ok(kind)
    }
}

struct Writer {
    sink: IMFSinkWriter,
    video: u32,
    audio: u32,
}

/// The sink writer for the MP4: H.264 from RGB32 frames at 24 fps, AAC
/// from 16-bit mono PCM at `rate`.
fn writer(path: &Path, width: u32, height: u32, rate: u32) -> windows::core::Result<Writer> {
    // SAFETY: Media Foundation objects created and configured here.
    unsafe {
        let options = attributes(1)?;
        options.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        let sink = MFCreateSinkWriterFromURL(&HSTRING::from(path), None, &options)?;
        let size = packed(width, height);
        let h264 = media_type(&MFMediaType_Video, &MFVideoFormat_H264)?;
        h264.SetUINT32(&MF_MT_AVG_BITRATE, VIDEO_BITRATE)?;
        h264.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
        h264.SetUINT64(&MF_MT_FRAME_RATE, packed(24, 1))?;
        h264.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, packed(1, 1))?;
        h264.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        let video = sink.AddStream(&h264)?;
        let rgb = media_type(&MFMediaType_Video, &MFVideoFormat_RGB32)?;
        rgb.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
        rgb.SetUINT64(&MF_MT_FRAME_RATE, packed(24, 1))?;
        rgb.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, packed(1, 1))?;
        rgb.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        sink.SetInputMediaType(video, &rgb, None)?;
        let aac = media_type(&MFMediaType_Audio, &MFAudioFormat_AAC)?;
        aac.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        aac.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)?;
        aac.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 1)?;
        aac.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AAC_BYTES_PER_SECOND)?;
        let audio = sink.AddStream(&aac)?;
        let pcm = media_type(&MFMediaType_Audio, &MFAudioFormat_PCM)?;
        pcm.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        pcm.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)?;
        pcm.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 1)?;
        pcm.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 2)?;
        pcm.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, rate * 2)?;
        sink.SetInputMediaType(audio, &pcm, None)?;
        sink.BeginWriting()?;
        Ok(Writer { sink, video, audio })
    }
}

/// An audio sample of 16-bit mono PCM at `time` (100 ns units).
fn audio_sample(pcm: &[u8], time: i64, rate: u32) -> windows::core::Result<IMFSample> {
    // SAFETY: a buffer of exactly `pcm.len()` bytes, filled while locked.
    unsafe {
        let buffer = MFCreateMemoryBuffer(pcm.len() as u32)?;
        let mut data = std::ptr::null_mut();
        buffer.Lock(&mut data, None, None)?;
        std::ptr::copy_nonoverlapping(pcm.as_ptr(), data, pcm.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(pcm.len() as u32)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(time)?;
        sample.SetSampleDuration((pcm.len() as i64 / 2) * UNITS / i64::from(rate))?;
        Ok(sample)
    }
}

/// The frame's RGB32 pixels as top-down BGRA rows with their stride (a
/// bottom-up frame has a negative pitch).
fn bgra(sample: &IMFSample, width: u32, height: u32) -> Option<(Vec<u8>, usize)> {
    // SAFETY: the buffer is locked while its pixels are copied.
    unsafe {
        let buffer = sample.ConvertToContiguousBuffer().ok()?;
        let row = width as usize * 4;
        let rows = height as usize;
        let mut pixels = vec![0u8; row * rows];
        if let Ok(two_d) = windows::core::Interface::cast::<IMF2DBuffer>(&buffer) {
            let mut scanline = std::ptr::null_mut();
            let mut pitch = 0i32;
            two_d.Lock2D(&mut scanline, &mut pitch).ok()?;
            for y in 0..rows {
                let source = scanline.offset(y as isize * pitch as isize);
                std::ptr::copy_nonoverlapping(source, pixels[y * row..].as_mut_ptr(), row);
            }
            let _ = two_d.Unlock2D();
        } else {
            let mut data = std::ptr::null_mut();
            let mut length = 0u32;
            buffer.Lock(&mut data, None, Some(&mut length)).ok()?;
            if (length as usize) >= row * rows {
                std::ptr::copy_nonoverlapping(data, pixels.as_mut_ptr(), row * rows);
            }
            let _ = buffer.Unlock();
        }
        Some((pixels, row))
    }
}

struct Folder(PathBuf);

impl Folder {
    fn create() -> Option<Folder> {
        // The user's own temporary folder, which only they can read.
        let path = std::env::temp_dir().join(super::folder_name());
        std::fs::create_dir(&path).ok()?;
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
    /// Starts recording from the first camera and the microphone with this
    /// ID (empty for the default).
    pub fn start(microphone: &str, events: impl Fn(VideoEvent) + Send + 'static) -> VideoRecorder {
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
                if let Err(error) = record(&microphone, feed, &inbox, &ended, &report) {
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

fn record(
    microphone: &str,
    feed: mpsc::Sender<Message>,
    inbox: &mpsc::Receiver<Message>,
    cancelled: &AtomicBool,
    events: &dyn Fn(VideoEvent),
) -> Result<(), String> {
    let _runtime = Runtime::start()?;
    let camera = camera()?;
    let shut = |camera: &Camera| {
        // SAFETY: the source is ours to shut down.
        unsafe {
            let _ = camera.source.Shutdown();
        }
    };
    let folder = Folder::create().ok_or(NO_STORAGE)?;
    let path = folder.0.join("video.mp4");
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
    .map_err(|(_, text)| text.to_owned())?;
    let mut pcm = Session::new(format, Instant::now()).ok_or(crate::UNSUPPORTED)?;
    let mut mono = MonoPcm::new(format);
    let writer =
        writer(&path, camera.width, camera.height, mono.rate()).map_err(|error| error.message())?;
    let video_stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    let opened = Instant::now();
    let mut first: Option<i64> = None;
    let mut written_ms = 0u64;
    let mut audio_samples: i64 = 0;
    let mut last_tick = Instant::now();
    let mut last_frame: Option<Instant> = None;
    let mut last_poster: Option<Instant> = None;
    let mut poster_rgb: Option<Vec<u8>> = None;
    let mut seconds = 0;
    let fail = |camera: &Camera, text: String| {
        shut(camera);
        Err(text)
    };
    'recording: loop {
        // Audio and commands first: a frame read waits up to a frame.
        loop {
            match inbox.try_recv() {
                Ok(Message::Chunk(bytes)) => {
                    for event in pcm.chunk(&bytes, Instant::now()) {
                        if let Event::Level { rms, peak } = event {
                            events(VideoEvent::Level { rms, peak });
                        }
                    }
                    let samples = mono.push(&bytes);
                    // Audio before the first frame is not part of the file.
                    if first.is_some() && !samples.is_empty() {
                        let time = audio_samples * UNITS / i64::from(mono.rate());
                        audio_samples += samples.len() as i64 / 2;
                        if let Ok(sample) = audio_sample(&samples, time, mono.rate()) {
                            // SAFETY: a sample for the audio stream.
                            if let Err(error) =
                                unsafe { writer.sink.WriteSample(writer.audio, &sample) }
                            {
                                return fail(&camera, error.message());
                            }
                        }
                    }
                }
                Ok(Message::StreamFailed) => return fail(&camera, crate::FAILED.into()),
                Ok(Message::Cancel) | Err(mpsc::TryRecvError::Disconnected) => {
                    shut(&camera);
                    return Ok(());
                }
                Ok(Message::Stop) if first.is_some() => break 'recording,
                Ok(Message::Stop) => {
                    shut(&camera);
                    return Ok(());
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        if cancelled.load(Ordering::SeqCst) {
            shut(&camera);
            return Ok(());
        }
        let mut flags = 0u32;
        let mut time = 0i64;
        let mut sample: Option<IMFSample> = None;
        // SAFETY: a synchronous read of the video stream.
        let read = unsafe {
            camera.reader.ReadSample(
                video_stream,
                0,
                None,
                Some(&mut flags),
                Some(&mut time),
                Some(&mut sample),
            )
        };
        if let Err(error) = read {
            return fail(&camera, error_text(&error));
        }
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            return fail(&camera, NOT_STARTED.into());
        }
        if let Some(sample) = sample {
            let start = *first.get_or_insert_with(|| {
                events(VideoEvent::Started);
                time
            });
            // SAFETY: re-times the frame from the recording's start.
            unsafe {
                let _ = sample.SetSampleTime(time - start);
                if let Err(error) = writer.sink.WriteSample(writer.video, &sample) {
                    return fail(&camera, error.message());
                }
            }
            written_ms = ((time - start).max(0) / 10_000) as u64;
            let now = Instant::now();
            let preview = last_frame.is_none_or(|last| {
                now.duration_since(last) >= Duration::from_secs(1) / PREVIEW_FPS
            });
            let poster =
                last_poster.is_none_or(|last| now.duration_since(last) >= Duration::from_secs(1));
            if preview || poster {
                if let Some((pixels, stride)) = bgra(&sample, camera.width, camera.height) {
                    let (width, height) = (camera.width as usize, camera.height as usize);
                    if preview {
                        last_frame = Some(now);
                        if let Some(jpeg) =
                            square_rgb(&pixels, width, height, stride, PREVIEW_SIDE as usize)
                                .and_then(|rgb| super::jpeg(&rgb, PREVIEW_SIDE as u16, 70))
                        {
                            events(VideoEvent::Frame {
                                jpeg,
                                mirrored: false,
                            });
                        }
                    }
                    if poster {
                        last_poster = Some(now);
                        poster_rgb =
                            square_rgb(&pixels, width, height, stride, POSTER_SIDE as usize);
                    }
                }
            }
        }
        if last_tick.elapsed() >= TICK {
            last_tick = Instant::now();
            if first.is_none() && opened.elapsed() >= START_TIMEOUT {
                return fail(&camera, NOT_STARTED.into());
            }
            if first.is_some() {
                let now = (written_ms / 1000) as u32;
                if now != seconds {
                    seconds = now;
                    events(VideoEvent::Elapsed(seconds));
                }
                let size = std::fs::metadata(&path).map_or(0, |meta| meta.len());
                if must_stop(written_ms, size) {
                    break;
                }
            }
        }
    }
    events(VideoEvent::Preparing);
    drop(stream);
    while let Ok(message) = inbox.try_recv() {
        match message {
            Message::Chunk(bytes) => {
                pcm.chunk(&bytes, Instant::now());
            }
            Message::Cancel => {
                shut(&camera);
                return Ok(());
            }
            _ => {}
        }
    }
    // SAFETY: writes the file's index and closes it.
    let finalized = unsafe { writer.sink.Finalize() };
    shut(&camera);
    if cancelled.load(Ordering::SeqCst) {
        return Ok(());
    }
    // Media Foundation's Finalize is synchronous: no wait to bound here.
    finalized.map_err(|error| refused(format!("the sink did not finalize: {error}")))?;
    let mp4 = std::fs::read(&path).map_err(|error| refused(format!("cannot read it: {error}")))?;
    if mp4.len() as u64 > MAX_BYTES {
        return Err(refused(format!("{} bytes", mp4.len())));
    }
    let pcm = pcm.into_pcm();
    let poster = poster_rgb.and_then(|rgb| poster_from_rgb(&rgb));
    let meta = finish(
        written_ms,
        camera.width,
        camera.height,
        mp4.len() as u64,
        pcm.len(),
        poster.as_deref(),
    )
    .map_err(|error| finish_refused(error, written_ms, camera.width, camera.height))?;
    events(VideoEvent::Completed { pcm, mp4, meta });
    drop(folder);
    Ok(())
}
