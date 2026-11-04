//! Round video messages, ported from the Qt client's `RoundVideoRecorder`:
//! H.264 and AAC in MP4 at 24 fps, 700 kbit/s video and 64 kbit/s mono
//! audio, from the camera format closest to 640x480 at 15 fps or more. A
//! recording stops at 59 s or 10 MiB less 128 KiB. The microphone feeds both
//! the AAC track and the PCM the speech computer transcribes, so nothing is
//! decoded afterwards. The preview is a square crop, about 15 frames a
//! second as 240 px JPEG; the poster a 160 px JPEG of at most 8 KiB.
//!
//! This module holds Qt's rules; [`linux`] records through GStreamer (the
//! `video` feature). macOS (M2) and Windows (M4) follow.
use base64::Engine;
use serde_json::{json, Map, Value};
use std::time::Duration;

#[cfg(all(target_os = "linux", feature = "video"))]
pub mod linux;
#[cfg(all(target_os = "macos", feature = "video"))]
pub mod macos;
#[cfg(all(windows, feature = "video"))]
pub mod windows;

/// The largest video file.
pub const MAX_BYTES: u64 = 10 * 1024 * 1024;
/// Recording stops at this size, leaving room for the file's index.
pub const STOP_BYTES: u64 = MAX_BYTES - 128 * 1024;
/// Recording stops at this length.
pub const STOP_MS: u64 = 59_000;
/// The camera must start within this time.
pub const START_TIMEOUT: Duration = Duration::from_secs(15);
/// How long stopping may take to finish the file before it is refused: a
/// loaded machine may need a while to encode what is queued and write it,
/// and the window shows Preparing meanwhile. Waits check Cancel every
/// [`FINISH_POLL`].
pub const FINISH_TIMEOUT: Duration = Duration::from_secs(30);
pub const FINISH_POLL: Duration = Duration::from_millis(100);
/// How often the length and size are checked.
pub const TICK: Duration = Duration::from_millis(200);
/// The poster's largest size.
pub const POSTER_BYTES: usize = 8192;
/// The poster's JPEG qualities, tried in order until one fits.
pub const POSTER_QUALITIES: [u8; 3] = [70, 50, 30];
/// The preview's side and rate.
pub const PREVIEW_SIDE: u32 = 240;
pub const PREVIEW_FPS: u32 = 15;
pub const POSTER_SIDE: u32 = 160;

pub const NO_DEVICES: &str = "A camera and microphone are required to record video.";
pub const NO_FORMAT: &str = "The camera has no supported recording format (up to 1920 pixels).";
pub const NOT_STARTED: &str = "The camera did not start. Try recording again.";
pub const INVALID: &str = "The video recording is invalid or exceeds 10 MiB.";

/// A refused recording: the user sees Qt's one text, and the log says which
/// check refused it (never a path or the recording's content).
#[cfg(feature = "video")]
pub(crate) fn refused(why: impl std::fmt::Display) -> String {
    eprintln!("zerus-client-media: video recording refused: {why}");
    INVALID.to_owned()
}

/// [`finish`]'s error, logged when it is the recording's own check.
#[cfg(feature = "video")]
pub(crate) fn finish_refused(error: &str, duration_ms: u64, width: u32, height: u32) -> String {
    if error == INVALID {
        refused(format!("{duration_ms} ms at {width}x{height}"))
    } else {
        error.to_owned()
    }
}
/// A fresh name for a recording's private folder. The per-process sequence keeps
/// two recordings started in the same clock tick apart.
#[cfg(feature = "video")]
pub(crate) fn folder_name() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_nanos());
    let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("zerus-video-{}-{nanos}-{sequence}", std::process::id())
}
pub const AUDIO_UNPREPARED: &str = "The video audio could not be prepared.";
pub const NO_STORAGE: &str = "Private recording storage is unavailable.";
/// Why video is unavailable in a build without a recorder for its platform.
pub const UNAVAILABLE: &str = "Video messages are not available in this version yet.";

/// What a video recording reports.
#[derive(Clone, Debug, PartialEq)]
pub enum VideoEvent {
    Started,
    Elapsed(u32),
    /// The microphone's level.
    Level {
        rms: f32,
        peak: f32,
    },
    /// A preview frame: a square JPEG, mirrored for a front camera.
    Frame {
        jpeg: Vec<u8>,
        mirrored: bool,
    },
    /// The recording stopped and its file is being checked.
    Preparing,
    /// The recording: its PCM, the MP4 and the metadata the message
    /// carries (`format`, `duration_ms`, `width`, `height`, `poster_base64`).
    Completed {
        pcm: Vec<u8>,
        mp4: Vec<u8>,
        meta: Map<String, Value>,
    },
    Failed(String),
}

/// A camera format: its size and highest frame rate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraFormat {
    pub width: u32,
    pub height: u32,
    pub max_fps: f64,
}

/// The format closest to 640x480 in area, 16 to 1920 px a side and at least
/// 15 fps; the first of equals wins.
pub fn closest_format(formats: &[CameraFormat]) -> Option<usize> {
    let mut best: Option<(usize, u64)> = None;
    for (index, format) in formats.iter().enumerate() {
        let sides = 16..=1920;
        if !sides.contains(&format.width)
            || !sides.contains(&format.height)
            || format.max_fps < 15.0
        {
            continue;
        }
        let score = (u64::from(format.width) * u64::from(format.height)).abs_diff(640 * 480);
        if best.is_none_or(|(_, distance)| score < distance) {
            best = Some((index, score));
        }
    }
    best.map(|(index, _)| index)
}

/// The centered square of a frame: left, right, top and bottom crops.
pub fn square_crop(width: u32, height: u32) -> (u32, u32, u32, u32) {
    let side = width.min(height);
    let left = (width - side) / 2;
    let top = (height - side) / 2;
    (left, width - side - left, top, height - side - top)
}

/// Whether a recording must stop now.
pub fn must_stop(duration_ms: u64, file_bytes: u64) -> bool {
    duration_ms >= STOP_MS || file_bytes >= STOP_BYTES
}

/// The poster: the first encoding (by [`POSTER_QUALITIES`]) that fits in
/// 8 KiB, or none.
pub fn poster(encodings: impl IntoIterator<Item = Vec<u8>>) -> Option<Vec<u8>> {
    encodings
        .into_iter()
        .find(|jpeg| !jpeg.is_empty() && jpeg.len() <= POSTER_BYTES)
}

/// Qt's checks of a finished recording: 0.1-60 s, 16-1920 px a side, at
/// most 10 MiB, and PCM of 0.1-60 s within a second of the video. The
/// metadata the message carries, or why the recording is not kept.
pub fn finish(
    duration_ms: u64,
    width: u32,
    height: u32,
    mp4_bytes: u64,
    pcm_bytes: usize,
    poster: Option<&[u8]>,
) -> Result<Map<String, Value>, &'static str> {
    let sides = 16..=1920;
    if !(100..=60_000).contains(&duration_ms)
        || !sides.contains(&width)
        || !sides.contains(&height)
        || mp4_bytes > MAX_BYTES
    {
        return Err(INVALID);
    }
    // 32 bytes of 16 kHz 16-bit PCM are a millisecond.
    if !(3200..=1_920_000).contains(&pcm_bytes)
        || (pcm_bytes as u64 / 32).abs_diff(duration_ms) > 1000
    {
        return Err(AUDIO_UNPREPARED);
    }
    let mut meta = Map::new();
    meta.insert("format".into(), json!("mp4"));
    meta.insert("duration_ms".into(), json!(duration_ms));
    meta.insert("width".into(), json!(width));
    meta.insert("height".into(), json!(height));
    if let Some(poster) = poster {
        meta.insert(
            "poster_base64".into(),
            json!(base64::engine::general_purpose::STANDARD.encode(poster)),
        );
    }
    Ok(meta)
}

/// The centered square of a BGRA frame (`stride` bytes a row), scaled to
/// `side` px by averaging, as RGB.
pub fn square_rgb(
    bgra: &[u8],
    width: usize,
    height: usize,
    stride: usize,
    side: usize,
) -> Option<Vec<u8>> {
    let square = width.min(height);
    if side == 0
        || square == 0
        || stride < width * 4
        || bgra.len() < stride * (height - 1) + width * 4
    {
        return None;
    }
    let (left, top) = ((width - square) / 2, (height - square) / 2);
    let mut rgb = Vec::with_capacity(side * side * 3);
    for row in 0..side {
        let (y0, y1) = (
            top + row * square / side,
            top + ((row + 1) * square / side).max(row * square / side + 1),
        );
        for column in 0..side {
            let (x0, x1) = (
                left + column * square / side,
                left + ((column + 1) * square / side).max(column * square / side + 1),
            );
            let mut sum = [0u32; 3];
            for y in y0..y1.min(top + square) {
                for x in x0..x1.min(left + square) {
                    let at = y * stride + x * 4;
                    sum[0] += u32::from(bgra[at + 2]);
                    sum[1] += u32::from(bgra[at + 1]);
                    sum[2] += u32::from(bgra[at]);
                }
            }
            let count = ((y1.min(top + square) - y0) * (x1.min(left + square) - x0)).max(1) as u32;
            rgb.extend(sum.map(|channel| (channel / count) as u8));
        }
    }
    Some(rgb)
}

/// RGB pixels of a `side` px square as JPEG.
pub fn jpeg(rgb: &[u8], side: u16, quality: u8) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, quality)
        .encode(rgb, side, side, jpeg_encoder::ColorType::Rgb)
        .ok()?;
    Some(out)
}

/// A poster from a 160 px RGB square: the first quality that fits 8 KiB.
pub fn poster_from_rgb(rgb: &[u8]) -> Option<Vec<u8>> {
    poster(
        POSTER_QUALITIES
            .iter()
            .filter_map(|quality| jpeg(rgb, POSTER_SIDE as u16, *quality)),
    )
}

/// Device audio as 16-bit mono at a rate an AAC encoder takes: the device's
/// own when it is 44.1 or 48 kHz, otherwise 48 kHz by linear
/// interpolation, which carries over between chunks.
pub struct MonoPcm {
    format: zerus_client_core::voice::InputFormat,
    rate: u32,
    partial: Vec<u8>,
    /// Input frames consumed, and where the next output falls between the
    /// last input frame and the next.
    previous: f32,
    position: f64,
}

impl MonoPcm {
    pub fn new(format: zerus_client_core::voice::InputFormat) -> MonoPcm {
        let rate = if matches!(format.sample_rate, 44_100 | 48_000) {
            format.sample_rate
        } else {
            48_000
        };
        MonoPcm {
            format,
            rate,
            partial: Vec::new(),
            previous: 0.0,
            position: 0.0,
        }
    }

    /// The output rate.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    fn frames(&mut self, bytes: &[u8]) -> Vec<f32> {
        use zerus_client_core::voice::SampleFormat;
        let width = match self.format.sample_format {
            SampleFormat::U8 => 1,
            SampleFormat::I16 => 2,
            SampleFormat::I32 | SampleFormat::F32 => 4,
        };
        let channels = usize::from(self.format.channels.max(1));
        self.partial.extend_from_slice(bytes);
        let whole = self.partial.len() / (width * channels) * (width * channels);
        let frames = self.partial[..whole]
            .chunks_exact(width * channels)
            .map(|frame| {
                let sum: f32 = frame
                    .chunks_exact(width)
                    .map(|sample| match self.format.sample_format {
                        SampleFormat::U8 => (f32::from(sample[0]) - 128.0) / 128.0,
                        SampleFormat::I16 => {
                            f32::from(i16::from_ne_bytes([sample[0], sample[1]])) / 32768.0
                        }
                        SampleFormat::I32 => {
                            i32::from_ne_bytes([sample[0], sample[1], sample[2], sample[3]]) as f32
                                / 2_147_483_648.0
                        }
                        SampleFormat::F32 => {
                            let value =
                                f32::from_ne_bytes([sample[0], sample[1], sample[2], sample[3]]);
                            if value.is_finite() {
                                value
                            } else {
                                0.0
                            }
                        }
                    })
                    .sum();
                (sum / channels as f32).clamp(-1.0, 1.0)
            })
            .collect();
        self.partial.drain(..whole);
        frames
    }

    /// The next device chunk as little-endian 16-bit mono samples.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        let frames = self.frames(bytes);
        let to_i16 = |value: f32| ((value * 32767.0).round() as i16).to_le_bytes();
        if self.rate == self.format.sample_rate {
            return frames.into_iter().flat_map(to_i16).collect();
        }
        let step = f64::from(self.format.sample_rate) / f64::from(self.rate);
        let mut out = Vec::new();
        // `position` runs from the previous chunk's last frame (index -1).
        while self.position < frames.len() as f64 - 1.0 {
            let at = self.position;
            let index = at.floor();
            let fraction = (at - index) as f32;
            let before = if index < 0.0 {
                self.previous
            } else {
                frames[index as usize]
            };
            let after = frames[(index + 1.0) as usize];
            out.extend(to_i16(before + (after - before) * fraction));
            self.position += step;
        }
        if let Some(last) = frames.last() {
            self.previous = *last;
            self.position -= frames.len() as f64;
        }
        out
    }
}

/// Why this build cannot record video, None when it can.
pub fn video_reason() -> Option<String> {
    #[cfg(all(target_os = "linux", feature = "video"))]
    {
        linux::missing()
    }
    #[cfg(all(any(target_os = "macos", windows), feature = "video"))]
    {
        None
    }
    #[cfg(not(all(
        any(target_os = "linux", target_os = "macos", windows),
        feature = "video"
    )))]
    {
        Some(UNAVAILABLE.into())
    }
}
