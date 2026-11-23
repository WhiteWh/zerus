//! Activity subscriptions: the inspection summary as `state`, journal events
//! as `events`, and bounds that keep every frame under the frame limit.
use super::catalog::{self, Digest32};
use crate::state::watch_source::TranscriptTail;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::time::Instant;
use zerus_watch_protocol as protocol;

/// Message IDs whose `event` revision a subscription remembers.
const MESSAGE_IDS: usize = 256;

/// Sections of an inspection that `state` leaves out: Activity rows travel as
/// `events`, and process sets belong to a later topic.
const NOT_STATE: [&str; 6] = ["events", "processes", "process_work", "cursor", "history_truncated", "replace_events"];
/// Fields that differ on every inspection without anything having changed.
/// Task #24: `timings_ms` (the inspection's own phase timings) made every
/// refresh of a busy session send its whole `state` again, about 180 KB
/// every 2 s, which held the stream's private memory growing.
const VOLATILE_STATE: [&[&str]; 3] = [&["goal_observed_at"], &["process_work", "sampled_at"], &["timings_ms"]];

/// One session the client follows.
pub(super) struct Subscription {
    pub key: String,
    /// The current name; a rename keeps the key and updates the name.
    pub name: String,
    pub run_id: Option<String>,
    /// DeepSeek and OpenCode: each read replaces the event window.
    pub native: bool,
    pub cursor: i64,
    pub tail: usize,
    pub rev: u64,
    pub state: Option<Digest32>,
    pub events: Option<Digest32>,
    pub refreshed: Instant,
    /// A source changed: refresh the summary soon.
    pub dirty: bool,
    /// The last summary showed a busy agent: refresh it more often.
    pub busy: bool,
    /// The last journal read was full: read again on the next pulse.
    pub more: bool,
    /// Frames were dropped: send `gap` and a tail snapshot once drained.
    pub resync: Option<&'static str>,
    /// The first frames are still owed after a failed inspection: the cursor
    /// to read after, when to read again and the wait that led there.
    pub start: Option<(i64, Instant, std::time::Duration)>,
    /// The catalog listed the key at least once.
    pub listed: bool,
    /// The digest of the session's catalog row when last seen.
    pub row: Option<Digest32>,
    /// The journal name its rows carry; a rename keeps it.
    pub journal: Option<String>,
    /// The Codex or Claude transcript followed for messages between tools.
    pub transcript: Option<TranscriptTail>,
    /// When to look for the transcript again after none was found.
    pub transcript_at: Instant,
    /// The `event` revision and digest of each message ID sent, oldest first.
    pub messages: VecDeque<(String, u64, Digest32)>,
    /// P1.5: the token of the summary refresh running on a thread; only its
    /// result applies.
    pub inspecting: Option<u64>,
}

impl Subscription {
    pub(super) fn new(key: String, name: String, run_id: Option<String>, native: bool, tail: usize) -> Self {
        Self {
            key,
            name,
            run_id,
            native,
            cursor: 0,
            tail,
            rev: 0,
            state: None,
            events: None,
            refreshed: Instant::now(),
            dirty: false,
            busy: false,
            more: false,
            resync: None,
            start: None,
            listed: false,
            row: None,
            journal: None,
            transcript: None,
            transcript_at: Instant::now(),
            messages: VecDeque::new(),
            inspecting: None,
        }
    }

    /// The `event` revision for a message, or None when that revision was
    /// already sent with the same content.
    pub(super) fn message_rev(&mut self, id: &str, message: &Value) -> Option<u64> {
        let digest = catalog::digest(message);
        let sent = self.messages.iter().position(|(sent, _, _)| sent == id).and_then(|index| self.messages.remove(index));
        let (rev, changed) = match sent {
            Some((_, rev, previous)) if previous == digest => (rev, false),
            Some((_, rev, _)) => (rev + 1, true),
            None => (1, true),
        };
        // The newest entry: the oldest ID is forgotten first.
        self.messages.push_back((id.to_owned(), rev, digest));
        if self.messages.len() > MESSAGE_IDS {
            self.messages.pop_front();
        }
        changed.then_some(rev)
    }
}

/// The stable ID of a provider message in `event` frames: its `message_id`,
/// or `{type}@{at}#{hash}` of its text when it has none.
pub(super) fn message_id(message: &Value) -> String {
    match message["message_id"].as_str().filter(|id| !id.is_empty()) {
        Some(id) => id.to_owned(),
        None => {
            let detail = message["detail"].as_str().unwrap_or_default();
            let hash = Sha256::digest(detail.as_bytes());
            let hex: String = hash[..8].iter().map(|byte| format!("{byte:02x}")).collect();
            format!("{}@{}#{hex}", message["type"].as_str().unwrap_or_default(), message["at"])
        }
    }
}

/// The inspection without its event and process sections.
pub(super) fn state_value(inspection: &Value) -> Value {
    let mut value = inspection.clone();
    if let Some(object) = value.as_object_mut() {
        for key in NOT_STATE {
            object.remove(key);
        }
    }
    value
}

/// A summary's digest with its observation timestamps masked.
pub(super) fn state_digest(value: &Value) -> Digest32 {
    catalog::digest(&catalog::masked(value, &VOLATILE_STATE))
}

pub(super) fn busy(value: &Value) -> bool {
    value["activity"] == "busy"
}

fn encoded_len(value: &Value) -> usize {
    serde_json::to_string(value).map_or(usize::MAX, |text| text.len())
}

/// Cuts a string to at most `bytes`, on a character boundary.
fn cut(text: &str, bytes: usize) -> String {
    let mut end = bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Fits one event in `limit` encoded bytes by cutting its longest strings,
/// marked with `"truncated": true`; the whole event remains readable through
/// `hgs inspect` and `hgs history`.
pub(super) fn shrink_event(event: &mut Value, limit: usize) {
    for _ in 0..16 {
        let size = encoded_len(event);
        if size <= limit {
            return;
        }
        let Some(object) = event.as_object_mut() else { break };
        let longest = object
            .iter()
            .filter_map(|(key, value)| value.as_str().map(|text| (key.clone(), text.len())))
            .max_by_key(|(_, len)| *len);
        let Some((key, len)) = longest.filter(|(_, len)| *len > 64) else { break };
        let keep = len.saturating_sub(size - limit + 64);
        let text = cut(object[&key].as_str().unwrap_or(""), keep);
        object.insert(key, json!(text));
        object.insert("truncated".into(), json!(true));
    }
    if encoded_len(event) > limit {
        let keep = |key: &str| event.get(key).cloned().unwrap_or(Value::Null);
        *event = json!({"seq":keep("seq"),"type":keep("type"),"at":keep("at"),"truncated":true});
    }
}

/// Fits a summary in `limit` encoded bytes: the largest lists lose their oldest
/// half until it fits, and `stream_truncated` names them. A client reads the
/// whole summary with `hgs inspect`.
pub(super) fn shrink_state(value: &mut Value, limit: usize) {
    let mut cut_lists = Vec::new();
    for _ in 0..64 {
        if encoded_len(value) <= limit {
            break;
        }
        let Some(object) = value.as_object_mut() else { break };
        let largest = object
            .iter()
            .filter(|(_, value)| value.as_array().is_some_and(|items| !items.is_empty()))
            .map(|(key, value)| (key.clone(), encoded_len(value)))
            .max_by_key(|(_, len)| *len);
        let Some((key, _)) = largest else { break };
        if let Some(items) = object.get_mut(&key).and_then(Value::as_array_mut) {
            let drop = items.len().div_ceil(2);
            items.drain(..drop);
        }
        if !cut_lists.contains(&key) {
            cut_lists.push(key);
        }
    }
    if encoded_len(value) > limit {
        let keep = |key: &str| value.get(key).cloned().unwrap_or(Value::Null);
        *value = json!({"name":keep("name"),"run_id":keep("run_id"),"conversation_id":keep("conversation_id"),
            "tracked":keep("tracked"),"stream_truncated":true});
    } else if !cut_lists.is_empty() {
        value["stream_truncated"] = json!(cut_lists);
    }
}

/// Splits events into frames of at most `limit` encoded bytes of events,
/// cutting any single event that alone exceeds it.
pub(super) fn event_pages(events: Vec<Value>, limit: usize) -> Vec<Vec<Value>> {
    let events = events
        .into_iter()
        .map(|mut event| {
            shrink_event(&mut event, limit);
            event
        })
        .collect();
    catalog::pages(events, encoded_len, limit)
}

/// The largest event or summary payload one frame carries.
pub(super) const PAYLOAD_LIMIT: usize = protocol::MAX_FRAME_BYTES - 64 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_leaves_out_events_processes_and_cursors_and_ignores_observation_times() {
        let inspection = json!({"name":"codex/p/t","phase":"idle","activity":"idle","events":[{"seq":1}],
            "processes":{"items":[]},"cursor":1,"history_truncated":false,"goal_observed_at":5.0,"message_events":[]});
        let value = state_value(&inspection);
        assert_eq!(value, json!({"name":"codex/p/t","phase":"idle","activity":"idle","goal_observed_at":5.0,"message_events":[]}));
        let mut later = value.clone();
        later["goal_observed_at"] = json!(6.0);
        // Task #24: an inspection's own timings are no change either.
        later["timings_ms"] = json!({"total":912,"summary":400,"questions":380});
        assert_eq!(state_digest(&value), state_digest(&later));
        later["phase"] = json!("working");
        assert_ne!(state_digest(&value), state_digest(&later));
        assert!(!busy(&value));
        assert!(busy(&json!({"activity":"busy"})));
    }

    #[test]
    fn messages_are_identified_by_message_id_or_type_time_and_text_and_revised_when_changed() {
        let message = json!({"type":"AgentMessage","at":1.5,"message_id":"m-1","detail":"Checking."});
        assert_eq!(message_id(&message), "m-1");
        let anonymous = json!({"type":"AgentMessage","at":1.5,"message_id":"","detail":"Checking."});
        let id = message_id(&anonymous);
        assert!(id.starts_with("AgentMessage@1.5#"), "{id}");
        assert_eq!(id.len(), "AgentMessage@1.5#".len() + 16);
        assert_eq!(id, message_id(&json!({"type":"AgentMessage","at":1.5,"detail":"Checking."})));
        assert_ne!(id, message_id(&json!({"type":"AgentMessage","at":1.5,"detail":"Other."})));
        let mut sub = Subscription::new("run:r".into(), "codex/p/t".into(), None, false, 100);
        assert_eq!(sub.message_rev("m-1", &message), Some(1));
        assert_eq!(sub.message_rev("m-1", &message), None);
        let mut changed = message.clone();
        changed["detail"] = json!("Checked.");
        assert_eq!(sub.message_rev("m-1", &changed), Some(2));
        assert_eq!(sub.message_rev("m-1", &changed), None);
        for n in 0..MESSAGE_IDS {
            assert_eq!(sub.message_rev(&format!("other-{n}"), &message), Some(1));
        }
        // The oldest ID is forgotten first: it starts again at revision 1.
        assert_eq!(sub.messages.len(), MESSAGE_IDS);
        assert_eq!(sub.message_rev("m-1", &changed), Some(1));
    }

    #[test]
    fn oversized_events_are_cut_with_a_marker_and_paged() {
        let mut event = json!({"seq":7,"type":"Stop","at":1.0,"detail":"é".repeat(400_000)});
        shrink_event(&mut event, 10_000);
        assert!(encoded_len(&event) <= 10_000);
        assert_eq!(event["truncated"], true);
        assert_eq!(event["seq"], 7);
        assert!(event["detail"].as_str().unwrap().ends_with('…'));
        let mut small = json!({"seq":8,"detail":"short"});
        shrink_event(&mut small, 10_000);
        assert_eq!(small, json!({"seq":8,"detail":"short"}));
        let mut bulky = json!({"seq":9,"type":"X","at":2.0,"list":vec![1; 5_000]});
        shrink_event(&mut bulky, 1_000);
        assert_eq!(bulky, json!({"seq":9,"type":"X","at":2.0,"truncated":true}));
        let events = (0..10).map(|seq| json!({"seq":seq,"detail":"x".repeat(300)})).collect();
        let pages = event_pages(events, 1_000);
        assert!(pages.len() >= 4);
        assert!(pages.iter().all(|page| page.iter().map(encoded_len).sum::<usize>() <= 1_000));
        assert_eq!(pages.concat().len(), 10);
    }

    #[test]
    fn oversized_summaries_drop_their_oldest_list_items() {
        let messages: Vec<_> = (0..200).map(|n| json!({"n":n,"text":"m".repeat(100)})).collect();
        let mut value = json!({"name":"codex/p/t","run_id":"r","provider_messages":messages,"phase":"idle"});
        shrink_state(&mut value, 8_000);
        assert!(encoded_len(&value) <= 8_000);
        assert_eq!(value["stream_truncated"], json!(["provider_messages"]));
        let kept = value["provider_messages"].as_array().unwrap();
        assert_eq!(kept.last().unwrap()["n"], 199);
        let mut text = json!({"name":"n","run_id":"r","prompt":"p".repeat(20_000)});
        shrink_state(&mut text, 1_000);
        assert_eq!(text["stream_truncated"], true);
        assert_eq!(text["name"], "n");
    }
}
