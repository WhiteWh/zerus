//! Negative startup exclusion protocol. Neither a frame nor configuration is
//! current ownership, a profile proof or permission to prepare a producer.
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub use crate::run::startup::Bootstrap;
pub const ABI: &str = "desktop-startup-v1";
pub const HELP_MARKER: &str = "Desktop startup ABI: desktop-startup-v1";
pub const FRAME_BYTES: usize = 4096;
pub const WAIT: Duration = Duration::from_secs(10);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    abi: String,
    version: u32,
    op: String,
    nonce: String,
    configuration: Option<String>,
}
impl Frame {
    pub fn encode(op: &str, nonce: &str, configuration: Option<&str>) -> Option<Vec<u8>> {
        let frame = Self {
            abi: ABI.into(),
            version: 1,
            op: op.into(),
            nonce: nonce.into(),
            configuration: configuration.map(str::to_owned),
        };
        frame.checked(op, nonce)?;
        let mut bytes = serde_json::to_vec(&frame).ok()?;
        bytes.push(b'\n');
        (bytes.len() <= FRAME_BYTES).then_some(bytes)
    }
    pub fn checked(&self, op: &str, nonce: &str) -> Option<&Self> {
        (self.abi == ABI
            && self.version == 1
            && self.op == op
            && self.nonce == nonce
            && zerus_desktop_migration::canonical_uuid(nonce)
            && match op {
                "hold" => self.configuration.is_none(),
                "ready" | "release" | "released" => {
                    self.configuration.as_deref().is_some_and(super::digest)
                }
                _ => false,
            })
        .then_some(self)
    }
    pub fn nonce(&self) -> &str {
        &self.nonce
    }
    pub fn configuration(&self) -> Option<&str> {
        self.configuration.as_deref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Known absence of the new command capability; no startup helper ran.
    Unsupported,
    Invalid,
    Unavailable,
    TimedOut,
    Unknown,
}
