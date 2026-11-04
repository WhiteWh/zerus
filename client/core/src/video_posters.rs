//! Small video posters retained outside Activity's JSON projection. No disk or
//! remote reads: a missing/evicted poster keeps the view's existing placeholder.
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::Value;

use crate::ComputerId;

pub const POSTER_BYTES: usize = 8192;
pub const CACHE_BYTES: usize = 2 * 1024 * 1024;
pub const CACHE_ITEMS: usize = 512;
type Key = (ComputerId, String, String);

#[derive(Debug, Default)]
pub struct Posters {
    entries: BTreeMap<Key, Arc<[u8]>>,
    order: VecDeque<Key>,
    bytes: usize,
}

fn bounded(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

pub fn valid_reference(computer: &str, key: &str, message: &str) -> bool {
    (computer.is_empty() || crate::fleet::valid_alias(computer))
        && bounded(key, 1024)
        && bounded(message, 160)
}

impl Posters {
    pub fn get(&self, computer: &ComputerId, key: &str, message: &str) -> Option<Arc<[u8]>> {
        if !valid_reference(&computer.0, key, message) {
            return None;
        }
        self.entries
            .get(&(computer.clone(), key.into(), message.into()))
            .cloned()
    }

    fn remove(&mut self, key: &Key) {
        if let Some(old) = self.entries.remove(key) {
            self.bytes -= old.len();
        }
        self.order.retain(|old| old != key);
    }

    pub fn forget(&mut self, computer: &ComputerId, session: Option<&str>) {
        let keys: Vec<_> = self
            .entries
            .keys()
            .filter(|(host, key, _)| {
                host == computer && session.is_none_or(|session| session == key)
            })
            .cloned()
            .collect();
        for key in keys {
            self.remove(&key);
        }
    }

    fn event(&mut self, computer: &ComputerId, key: &str, event: &mut Value) {
        let message = event["message_id"]
            .as_str()
            .or_else(|| event["voice"]["id"].as_str())
            .unwrap_or_default()
            .to_owned();
        let Some(video) = event
            .get_mut("voice")
            .and_then(|voice| voice.get_mut("video"))
            .and_then(Value::as_object_mut)
        else {
            return;
        };
        let Some(encoded) = video.remove("poster_base64") else {
            return;
        };
        video.insert("poster_available".into(), Value::Bool(false));
        if !valid_reference(&computer.0, key, &message) {
            return;
        }
        let id = (computer.clone(), key.to_owned(), message);
        self.remove(&id);
        let Some(encoded) = encoded
            .as_str()
            .filter(|s| s.len() <= POSTER_BYTES.div_ceil(3) * 4)
        else {
            return;
        };
        let Ok(bytes) = STANDARD.decode(encoded) else {
            return;
        };
        // Same JPEG envelope and cap as the existing voice-video contract.
        if bytes.len() > POSTER_BYTES
            || !bytes.starts_with(&[0xff, 0xd8])
            || !bytes.ends_with(&[0xff, 0xd9])
        {
            return;
        }
        while self.entries.len() >= CACHE_ITEMS || self.bytes + bytes.len() > CACHE_BYTES {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some(bytes) = self.entries.remove(&old) {
                self.bytes -= bytes.len();
            }
        }
        self.bytes += bytes.len();
        self.entries.insert(id.clone(), Arc::from(bytes));
        self.order.push_back(id);
        video.insert("poster_available".into(), Value::Bool(true));
    }

    /// An Activity event or summary with its existing bounded event arrays.
    /// All other fields stay as supplied by old or new hgs versions.
    pub fn strip(&mut self, computer: &ComputerId, key: &str, value: &mut Value) {
        self.event(computer, key, value);
        for field in ["events", "provider_messages"] {
            if let Some(events) = value.get_mut(field).and_then(Value::as_array_mut) {
                for event in events {
                    self.event(computer, key, event);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn event(id: &str, bytes: &[u8]) -> Value {
        json!({"message_id":id,"type":"UserMessage","source":"hgs_voice","voice":{"id":id,"unknown":"keep","video":{"duration_ms":7,"poster_base64":STANDARD.encode(bytes)}}})
    }
    #[test]
    fn bytes_are_private_exact_scoped_and_bounded() {
        let mut cache = Posters::default();
        let host = ComputerId::from("example-peer");
        let mut bytes = vec![1; POSTER_BYTES];
        bytes[..2].copy_from_slice(&[255, 216]);
        bytes[POSTER_BYTES - 2..].copy_from_slice(&[255, 217]);
        for n in 0..CACHE_ITEMS + 1 {
            let mut value = event(&format!("message-{n}"), &bytes);
            cache.strip(&host, "run:one", &mut value);
            assert!(value["voice"]["video"].get("poster_base64").is_none());
            assert_eq!(value["voice"]["video"]["poster_available"], true);
            assert_eq!(value["voice"]["unknown"], "keep");
        }
        assert!(cache.bytes <= CACHE_BYTES && cache.entries.len() <= CACHE_ITEMS);
        assert!(cache.get(&host, "run:one", "message-0").is_none());
        let id = format!("message-{}", CACHE_ITEMS);
        assert_eq!(&*cache.get(&host, "run:one", &id).unwrap(), bytes);
        assert!(cache.get(&ComputerId::local(), "run:one", &id).is_none());
        assert!(cache.get(&host, "run:two", &id).is_none());
        cache.forget(&host, Some("run:one"));
        assert!(cache.entries.is_empty());
        assert_eq!(cache.bytes, 0);
    }
    #[test]
    fn malformed_and_oversized_posters_are_stripped_without_decoding_them() {
        let mut cache = Posters::default();
        let host = ComputerId::local();
        for encoded in [
            json!("not base64"),
            json!(STANDARD.encode([1, 2, 3])),
            json!("A".repeat(POSTER_BYTES * 2)),
            json!({"unknown":"large"}),
        ] {
            let mut value = event("message", &[255, 216, 255, 217]);
            cache.strip(&host, "run:one", &mut value);
            let mut value = event("message", &[255, 216, 255, 217]);
            value["voice"]["video"]["poster_base64"] = encoded;
            cache.strip(&host, "run:one", &mut value);
            assert!(value["voice"]["video"].get("poster_base64").is_none());
            assert_eq!(value["voice"]["video"]["poster_available"], false);
            assert!(cache.get(&host, "run:one", "message").is_none());
        }
        let mut old = json!({"events":[event("message", &[255,216,255,217])],"provider_messages":[event("provider", &[255,216,255,217])],"unknown":8});
        cache.strip(&host, "run:one", &mut old);
        assert!(!old.to_string().contains("poster_base64"));
        assert_eq!(old["unknown"], 8);
    }
}
