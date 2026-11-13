//! The API v1 codec, envelope validation and error type always build. The
//! relay server modules need the default `server` feature.
#[cfg(all(test, feature = "server"))]
extern crate self as zerus_relay;
pub mod alerts;
#[cfg(feature = "server")]
pub mod config;
#[cfg(feature = "server")]
pub mod db;
pub mod error;
#[cfg(feature = "server")]
pub mod http;
pub mod json;
#[cfg(feature = "server")]
pub mod notification_highwater;
pub mod protocol;
#[cfg(feature = "server")]
pub mod push;
#[cfg(feature = "server")]
pub mod registry;
pub mod repositories;
pub mod send_receipt;
mod speech_config;
#[cfg(feature = "server")]
pub mod store;
#[cfg(all(test, feature = "server"))]
mod test_backend;
pub mod voice_rejection;
mod voice_video;

/// Validate additive video media without starting a codec. Shared with the outbound connector.
pub fn validate_voice_video(
    value: &serde_json::Value,
    id: &str,
    pcm_bytes: usize,
) -> std::result::Result<serde_json::Value, String> {
    voice_video::parse(value, id, pcm_bytes).map(|video| video.metadata)
}
