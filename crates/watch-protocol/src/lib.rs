//! Frames and control operations of `hgs [@host] watch --json` (ABI `watch-v1`).
//!
//! The server writes one JSON frame per line (NDJSON, UTF-8) on stdout and reads
//! one control operation per line on stdin. EOF on stdin ends the stream. Every
//! frame carries `v` (always [`VERSION`]), `t` (its type) and `seq`, which grows
//! by one per frame within a stream. A frame line, including its newline, is at
//! most [`MAX_FRAME_BYTES`].
//!
//! Clients ignore fields they do not know. A frame type or topic they do not know
//! is skipped, except that `gap` and a fatal `error` always apply.
//!
//! See the "Event stream" section of `docs/reference.md` for the contract.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Data-only capability exchange with the actual mobile journal owner. This
/// grants no native-operation, session, Stop or delivery authority.
pub mod session_labels;

/// The `v` of every frame.
pub const VERSION: u32 = 1;
/// The ABI name a client looks for before it starts the stream.
pub const ABI: &str = "watch-v1";
/// The line `hgs -h` prints when this ABI is available.
pub const HELP_MARKER: &str = "Event stream ABI: watch-v1";
/// The largest frame line, including its newline.
pub const MAX_FRAME_BYTES: usize = 1 << 20;
/// The largest snapshot or delta page the server builds.
pub const PAGE_BYTES: usize = 512 << 10;
/// The server's outgoing queue; beyond it a topic receives `gap`.
pub const QUEUE_BYTES: usize = 4 << 20;
/// A heartbeat follows this much time without other frames.
pub const HEARTBEAT_MS: u64 = 10_000;
/// Concurrent Activity subscriptions per stream.
pub const ACTIVITY_SUBSCRIPTIONS: usize = 16;
/// Concurrent git subscriptions (checkout folders) per stream.
pub const GIT_SUBSCRIPTIONS: usize = 64;
/// Concurrent processes subscriptions (sessions) per stream.
pub const PROCESSES_SUBSCRIPTIONS: usize = 16;
/// Concurrent tasks subscriptions (projects) per stream.
pub const TASKS_SUBSCRIPTIONS: usize = 8;
/// The team heads the `team` topic carries at most (the most recently changed).
pub const TEAM_ROWS: usize = 1000;
/// The most rows of each stage a windowed tasks subscription carries.
pub const TASKS_WINDOW: usize = 200;
/// The most events an Activity tail or tail snapshot carries.
pub const ACTIVITY_TAIL: usize = 100;
/// The most journal rows one Activity read returns.
pub const ACTIVITY_READ: usize = 200;
/// The most rows one older Activity page (`before`) carries.
pub const ACTIVITY_OLDER: usize = 200;
/// The longest control line the server accepts, without its newline.
pub const CONTROL_LINE_BYTES: usize = 64 << 10;

/// Topic names.
pub mod topic {
    /// Live session rows, keyed by [`super::catalog_key`].
    pub const CATALOG: &str = "catalog";
    /// The listing fields other than `sessions`.
    pub const CATALOG_META: &str = "catalog.meta";
    /// One session's Activity events and inspection summary.
    pub const ACTIVITY: &str = "activity";
    /// One checkout folder's `hgs git-status --json` object, keyed by the
    /// folder as subscribed.
    pub const GIT: &str = "git";
    /// One session's `processes` and `process_work`, as `hgs inspect`
    /// reports them, keyed like Activity.
    pub const PROCESSES: &str = "processes";
    /// One project's task list as `hgs task list --project ID --json` prints
    /// it, keyed by the project as subscribed.
    pub const TASKS: &str = "tasks";
    /// The shared project catalog as `hgs swarm get` shows it, as keyed rows.
    pub const ORGANIZATION: &str = "organization";
    /// Every team's head (id, name, lead, status, revision, updated) as keyed
    /// rows: a client reads a team again with `hgs team` when its revision
    /// changes, instead of polling. One per stream.
    pub const TEAM: &str = "team";
    /// Task #16 of team c536f0e5 (P2.5): the Teams views as keyed rows, each
    /// exactly what its `hgs team <read> --json` returns: `list`, `waiting`
    /// and, for the team a subscription names (`team`), `status:<id>`,
    /// `inbox:<id>` and `feed:<id>`. A row too large for a frame is an error
    /// row (`{"error":{"code":"too_large",...}}`) read with its command. One
    /// per stream; a later subscribe replaces the team.
    pub const TEAM_VIEWS: &str = "team.views";
    /// The separately persisted, capable-only Windows drive-folder projection.
    pub const DRIVE_FOLDERS: &str = "organization.drive_folders";
}

/// Why a catalog key was removed. Clients treat an unknown reason as unknown.
pub mod reason {
    /// The session ended and left no saved row.
    pub const ENDED: &str = "ended";
    /// The run moved to Archive.
    pub const ARCHIVED: &str = "archived";
    /// The saved binding was removed.
    pub const FORGOTTEN: &str = "forgotten";
    /// The same session name now lists under a new run.
    pub const REPLACED: &str = "replaced";
    /// The client fell behind and the server dropped frames.
    pub const OVERFLOW: &str = "overflow";
    /// The Activity cursor is older than the journal's retention.
    pub const RETENTION: &str = "retention";
    /// The Activity cursor is newer than the journal (a replaced journal).
    pub const RESET: &str = "reset";
}

/// Codes of `error` frames that clients act on.
pub mod code {
    /// Fatal: an update replaced the `hgs` executable this stream runs.
    /// Start the stream again to run the new version; rows stay as known.
    pub const HGS_REPLACED: &str = "hgs_replaced";
}

/// One line of the stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub v: u32,
    pub seq: u64,
    #[serde(flatten)]
    pub body: Body,
}

/// A frame's type (`t`) and fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Body {
    Hello(Hello),
    Snapshot(Snapshot),
    Delta(Delta),
    Events(Events),
    Older(Older),
    Event(EventUpdate),
    State(State),
    Heartbeat(Heartbeat),
    Gap(Gap),
    Error(ErrorFrame),
    Pong(Pong),
}

/// The first frame of a stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    /// The computer's native identity; null when unavailable.
    pub machine_id: Option<String>,
    pub host: String,
    pub hgs_version: String,
    /// A random identity of this stream.
    pub stream: String,
    pub heartbeat_ms: u64,
    pub max_frame_bytes: usize,
    pub topics: Vec<String>,
    pub limits: Limits,
}

/// Bounds the server applies to this stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    pub activity_subscriptions: usize,
    pub page_bytes: usize,
    pub queue_bytes: usize,
    pub activity_tail: usize,
    pub activity_read: usize,
    pub control_line_bytes: usize,
    /// 0 from a server without the git topic.
    #[serde(default)]
    pub git_subscriptions: usize,
    /// 0 from a server without the processes topic.
    #[serde(default)]
    pub processes_subscriptions: usize,
    /// 0 from a server without the tasks topic.
    #[serde(default)]
    pub tasks_subscriptions: usize,
    /// The largest `window` a tasks subscription may ask for; 0 from a server
    /// that always sends the whole list.
    #[serde(default)]
    pub tasks_window: usize,
    /// The most rows one older Activity page (`before`) carries; 0 from a
    /// server without older pages, which reads earlier Activity by `after`.
    #[serde(default)]
    pub activity_older: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            activity_subscriptions: ACTIVITY_SUBSCRIPTIONS,
            page_bytes: PAGE_BYTES,
            queue_bytes: QUEUE_BYTES,
            activity_tail: ACTIVITY_TAIL,
            activity_read: ACTIVITY_READ,
            control_line_bytes: CONTROL_LINE_BYTES,
            git_subscriptions: GIT_SUBSCRIPTIONS,
            processes_subscriptions: PROCESSES_SUBSCRIPTIONS,
            tasks_subscriptions: TASKS_SUBSCRIPTIONS,
            tasks_window: TASKS_WINDOW,
            activity_older: ACTIVITY_OLDER,
        }
    }
}

/// A bounded replacement of one topic, or of one key of it. A client keeps
/// showing what it had until the page with `last` arrives, then replaces it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub topic: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Keyed collections: catalog rows as [`CatalogItem`] objects, Activity
    /// events as `inspect` returns them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<Value>>,
    /// Single-object topics such as `catalog.meta`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    pub page: u32,
    pub last: bool,
    /// The catalog revision, or the Activity journal `seq`.
    pub cursor: i64,
}

/// A keyed change of the catalog. Upserts carry whole rows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Delta {
    pub topic: String,
    #[serde(default)]
    pub upsert: Vec<CatalogItem>,
    #[serde(default)]
    pub remove: Vec<Removal>,
    pub cursor: i64,
}

/// One catalog row and its key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogItem {
    pub key: String,
    pub row: Value,
}

/// A removed catalog key; `reason` is one of [`reason`] when known.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Removal {
    pub key: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Activity events after the previous cursor, oldest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Events {
    pub topic: String,
    pub key: String,
    pub events: Vec<Value>,
    /// The journal `seq` to resume after.
    pub cursor: i64,
}

/// One page of a followed session's Activity before a journal `seq`, oldest
/// first: the answer to a subscribe with `before`. It never moves the
/// subscription's cursor. A read may span several frames; the one with `last`
/// ends it and says whether older rows remain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Older {
    pub topic: String,
    pub key: String,
    pub events: Vec<Value>,
    /// The `before` of the next older page: the oldest journal `seq` this read
    /// covered, also when every row of it was left out.
    pub cursor: i64,
    /// The read's final frame.
    pub last: bool,
    /// On the final frame: rows older than `cursor` remain.
    #[serde(default)]
    pub more: bool,
}

/// A replacement of one open event by its stable ID; a higher `rev` wins.
/// Activity sends a provider message between tools as `inspect` lists it in
/// `provider_messages`; `id` is its `message_id`, or `{type}@{at}#{hash}`
/// when that is empty (see "Event stream" in docs/reference.md).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventUpdate {
    pub topic: String,
    pub key: String,
    pub id: String,
    pub rev: u64,
    pub event: Value,
}

/// A replacement of one keyed object: a session's inspection summary, or a
/// checkout folder's git status.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub topic: String,
    pub key: String,
    pub value: Value,
    pub rev: u64,
}

/// Sent after [`HEARTBEAT_MS`] without other frames.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Heartbeat {
    /// Seconds since the Unix epoch.
    pub at: f64,
}

/// The client discards what it holds for the topic (or key); a snapshot follows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gap {
    pub topic: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub reason: String,
}

/// A refused operation or failed source. A fatal error ends the stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorFrame {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub fatal: bool,
}

/// The answer to a `ping`, echoing its `id`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pong {
    #[serde(default)]
    pub id: Option<Value>,
}

/// One control line from the client.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Control {
    Subscribe(Subscription),
    Unsubscribe(Subscription),
    Ping(Ping),
}

/// A topic and its key. Activity and processes take `session`, git takes
/// `path` (an absolute folder) and tasks takes `project` (its ID).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    pub topic: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Processes polling interval, clamped by the server to 1–600 seconds.
    /// Older watch-v1 servers ignore this additive field and keep their default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>,
    /// Request one processes read after its minimum gap, without dropping state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<bool>,
    /// Resume Activity after this journal `seq`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
    /// How many recent events a subscription without `after` starts with, or
    /// how many rows an older page (`before`) carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail: Option<u32>,
    /// Activity: read one page of the followed session's rows before this
    /// journal `seq` (at most `tail`, up to `activity_older`), answered with
    /// `older` frames. The subscription itself, its cursor and its state stay
    /// as they are. Only for a server whose `activity_older` limit is above 0:
    /// one without it ignores this additive field and subscribes anew.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<i64>,
    /// Tasks: only the first `window` rows of each stage (1 to `tasks_window`),
    /// with each stage's total and the cursor of its next page, instead of the
    /// whole list. Servers without it ignore this additive field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u32>,
    /// Team views (task #16, P2.5): the team whose status, inbox and feed
    /// rows the subscription adds. Servers without the topic ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
}

/// The exact session a subscription follows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
}

impl SessionRef {
    /// The session's key, the same as its catalog key.
    pub fn key(&self) -> String {
        key_for(self.run_id.as_deref(), &self.name)
    }
}

/// A liveness probe; the server answers with `pong`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Ping {
    #[serde(default)]
    pub id: Option<Value>,
}

/// `run:<run_id>`, or `name:<name>` for a session without a run.
pub fn key_for(run_id: Option<&str>, name: &str) -> String {
    match run_id.filter(|run| !run.is_empty()) {
        Some(run) => format!("run:{run}"),
        None => format!("name:{name}"),
    }
}

/// The catalog key of an `ls --json --local` row; None without a name.
pub fn catalog_key(row: &Value) -> Option<String> {
    let name = row.get("name")?.as_str()?;
    Some(key_for(row.get("run_id").and_then(Value::as_str), name))
}

/// A frame that cannot be written or read.
#[derive(Debug)]
pub enum FrameError {
    /// The line, with its newline, would exceed [`MAX_FRAME_BYTES`].
    TooLarge(usize),
    /// A line that does not start like a serialized [`Body`].
    Malformed,
    Json(serde_json::Error),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge(bytes) => write!(f, "frame of {bytes} bytes exceeds {MAX_FRAME_BYTES}"),
            Self::Malformed => f.write_str("malformed frame body"),
            Self::Json(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<serde_json::Error> for FrameError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Whether a line of `bytes` (without its newline) fits in a frame.
pub fn fits(bytes: usize) -> bool {
    bytes < MAX_FRAME_BYTES
}

/// Fails unless `line` (without its newline) fits in a frame.
pub fn check_size(line: &str) -> Result<(), FrameError> {
    if fits(line.len()) {
        Ok(())
    } else {
        Err(FrameError::TooLarge(line.len() + 1))
    }
}

/// The most bytes [`envelope`] adds to a body.
pub const ENVELOPE_BYTES: usize = 40;

/// A body as JSON without `v` and `seq`, for a writer that numbers frames
/// only when it writes them. [`envelope`] completes it.
pub fn encode_body(body: &Body) -> Result<String, FrameError> {
    let text = serde_json::to_string(body)?;
    if !fits(text.len() + ENVELOPE_BYTES) {
        return Err(FrameError::TooLarge(text.len() + ENVELOPE_BYTES + 1));
    }
    Ok(text)
}

/// The frame line (without its newline) of a body from [`encode_body`].
pub fn envelope(seq: u64, body: &str) -> Result<String, FrameError> {
    let rest = body.strip_prefix("{\"t\":").ok_or(FrameError::Malformed)?;
    Ok(format!("{{\"v\":{VERSION},\"seq\":{seq},\"t\":{rest}"))
}

/// One frame line, without its newline.
pub fn encode(frame: &Frame) -> Result<String, FrameError> {
    let line = serde_json::to_string(frame)?;
    check_size(&line)?;
    Ok(line)
}

/// One frame from a line, with or without its newline.
pub fn decode(line: &str) -> Result<Frame, FrameError> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    check_size(line)?;
    Ok(serde_json::from_str(line)?)
}

/// One control operation from a line, with or without its newline.
pub fn decode_control(line: &str) -> Result<Control, FrameError> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    if line.len() > CONTROL_LINE_BYTES {
        return Err(FrameError::TooLarge(line.len() + 1));
    }
    Ok(serde_json::from_str(line)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn frames() -> Vec<Body> {
        vec![
            Body::Hello(Hello {
                machine_id: Some("00000000-0000-4000-8000-000000000001".into()),
                host: "workstation".into(),
                hgs_version: "1.56.0".into(),
                stream: "stream-1".into(),
                heartbeat_ms: HEARTBEAT_MS,
                max_frame_bytes: MAX_FRAME_BYTES,
                topics: vec![
                    topic::CATALOG.into(),
                    topic::CATALOG_META.into(),
                    topic::ACTIVITY.into(),
                    topic::GIT.into(),
                ],
                limits: Limits::default(),
            }),
            Body::Snapshot(Snapshot {
                topic: topic::CATALOG.into(),
                key: None,
                items: Some(vec![
                    json!({"key":"run:r1","row":{"name":"codex/p/t","run_id":"r1","at":1.5}}),
                ]),
                value: None,
                page: 0,
                last: true,
                cursor: 1,
            }),
            Body::Snapshot(Snapshot {
                topic: topic::CATALOG_META.into(),
                key: None,
                items: None,
                value: Some(json!({"host":"workstation","projects":{}})),
                page: 0,
                last: true,
                cursor: 1,
            }),
            Body::Delta(Delta {
                topic: topic::CATALOG.into(),
                upsert: vec![CatalogItem {
                    key: "name:sh/p".into(),
                    row: json!({"name":"sh/p"}),
                }],
                remove: vec![
                    Removal {
                        key: "run:r0".into(),
                        reason: Some(reason::REPLACED.into()),
                    },
                    Removal {
                        key: "run:r2".into(),
                        reason: None,
                    },
                ],
                cursor: 2,
            }),
            Body::Events(Events {
                topic: topic::ACTIVITY.into(),
                key: "run:r1".into(),
                events: vec![json!({"type":"Stop","seq":41,"at":1787840176.25})],
                cursor: 41,
            }),
            Body::Older(Older {
                topic: topic::ACTIVITY.into(),
                key: "run:r1".into(),
                events: vec![json!({"type":"Stop","seq":7,"at":1787840100.25})],
                cursor: 7,
                last: true,
                more: true,
            }),
            Body::Event(EventUpdate {
                topic: topic::ACTIVITY.into(),
                key: "run:r1".into(),
                id: "message-1".into(),
                rev: 3,
                event: json!({"text":"partial"}),
            }),
            Body::State(State {
                topic: topic::ACTIVITY.into(),
                key: "run:r1".into(),
                value: json!({"phase":"idle"}),
                rev: 4,
            }),
            Body::State(State {
                topic: topic::GIT.into(),
                key: "/work/sample-project".into(),
                value: json!({"state":"ok","branch":"main","changed_files":2,"sampled_at":1787840176.5}),
                rev: 5,
            }),
            Body::Heartbeat(Heartbeat { at: 1787840176.5 }),
            Body::Gap(Gap {
                topic: topic::ACTIVITY.into(),
                key: Some("run:r1".into()),
                reason: reason::RETENTION.into(),
            }),
            Body::Error(ErrorFrame {
                code: "too_many_subscriptions".into(),
                message: "at most 16".into(),
                topic: Some(topic::ACTIVITY.into()),
                key: None,
                fatal: false,
            }),
            Body::Pong(Pong { id: Some(json!(7)) }),
        ]
    }

    #[test]
    fn processes_options_are_additive_and_absent_on_older_controls() {
        let Control::Subscribe(old) = decode_control(
            r#"{"op":"subscribe","topic":"processes","session":{"name":"codex/p/t"}}"#,
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(old.interval_ms, None);
        assert_eq!(old.refresh, None);
        let value = serde_json::to_value(Control::Subscribe(old)).unwrap();
        assert!(value.get("interval_ms").is_none() && value.get("refresh").is_none());
        let Control::Subscribe(new) = decode_control(r#"{"op":"subscribe","topic":"processes","session":{"name":"codex/p/t"},"interval_ms":2500,"refresh":true}"#).unwrap() else { panic!() };
        assert_eq!(new.interval_ms, Some(2500));
        assert_eq!(new.refresh, Some(true));
    }

    #[test]
    fn every_frame_round_trips_with_version_type_and_sequence() {
        for (index, body) in frames().into_iter().enumerate() {
            let frame = Frame {
                v: VERSION,
                seq: index as u64 + 1,
                body,
            };
            let line = encode(&frame).unwrap();
            assert!(!line.contains('\n'));
            let value: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(value["v"], 1);
            assert_eq!(value["seq"], index as u64 + 1);
            assert!(value["t"].is_string(), "{line}");
            assert_eq!(decode(&format!("{line}\n")).unwrap(), frame);
        }
    }

    #[test]
    fn envelope_numbers_an_encoded_body_like_a_whole_frame() {
        for body in frames() {
            let encoded = encode_body(&body).unwrap();
            let line = envelope(u64::MAX, &encoded).unwrap();
            let whole = encode(&Frame {
                v: VERSION,
                seq: u64::MAX,
                body: body.clone(),
            })
            .unwrap();
            assert_eq!(line, whole);
            assert!(line.len() <= encoded.len() + ENVELOPE_BYTES);
        }
        assert!(matches!(envelope(1, "{}"), Err(FrameError::Malformed)));
    }

    #[test]
    fn wire_names_match_the_contract() {
        let line = encode(&Frame {
            v: 1,
            seq: 9,
            body: Body::Heartbeat(Heartbeat { at: 2.0 }),
        })
        .unwrap();
        assert_eq!(line, r#"{"v":1,"seq":9,"t":"heartbeat","at":2.0}"#);
        let gap = encode(&Frame {
            v: 1,
            seq: 10,
            body: Body::Gap(Gap {
                topic: "catalog".into(),
                key: None,
                reason: "overflow".into(),
            }),
        })
        .unwrap();
        assert_eq!(
            gap,
            r#"{"v":1,"seq":10,"t":"gap","topic":"catalog","reason":"overflow"}"#
        );
    }

    #[test]
    fn unknown_fields_are_ignored_for_forward_compatibility() {
        let frame = decode(r#"{"v":1,"seq":3,"t":"heartbeat","at":1.0,"later":true}"#).unwrap();
        assert_eq!(frame.body, Body::Heartbeat(Heartbeat { at: 1.0 }));
        let control = decode_control(r#"{"op":"ping","id":"a","later":[1]}"#).unwrap();
        assert_eq!(
            control,
            Control::Ping(Ping {
                id: Some(json!("a"))
            })
        );
    }

    #[test]
    fn control_operations_parse_with_their_keys() {
        let line = r#"{"op":"subscribe","topic":"activity","session":{"name":"codex/p/t","run_id":"r1","conversation_id":"c1"},"after":40,"tail":20}"#;
        let Control::Subscribe(subscription) = decode_control(line).unwrap() else {
            panic!()
        };
        assert_eq!(subscription.topic, "activity");
        assert_eq!(subscription.after, Some(40));
        assert_eq!(subscription.tail, Some(20));
        let session = subscription.session.unwrap();
        assert_eq!(session.key(), "run:r1");
        assert_eq!(
            SessionRef {
                name: "sh/p".into(),
                ..Default::default()
            }
            .key(),
            "name:sh/p"
        );
        let Control::Unsubscribe(gone) =
            decode_control(r#"{"op":"unsubscribe","topic":"activity","session":{"name":"x"}}"#)
                .unwrap()
        else {
            panic!()
        };
        assert_eq!(gone.session.unwrap().key(), "name:x");
        assert_eq!(
            decode_control(r#"{"op":"ping"}"#).unwrap(),
            Control::Ping(Ping { id: None })
        );
        assert!(decode_control(r#"{"op":"launch"}"#).is_err());
        assert!(decode_control(r#"{"topic":"activity"}"#).is_err());
        let long = format!(
            r#"{{"op":"ping","id":"{}"}}"#,
            "x".repeat(CONTROL_LINE_BYTES)
        );
        assert!(matches!(
            decode_control(&long),
            Err(FrameError::TooLarge(_))
        ));
    }

    #[test]
    fn git_subscriptions_take_a_path_and_older_hellos_offer_none() {
        let Control::Subscribe(subscription) =
            decode_control(r#"{"op":"subscribe","topic":"git","path":"/work/sample-project"}"#)
                .unwrap()
        else {
            panic!()
        };
        assert_eq!(subscription.topic, topic::GIT);
        assert_eq!(subscription.path.as_deref(), Some("/work/sample-project"));
        assert!(subscription.session.is_none());
        let Control::Unsubscribe(gone) =
            decode_control(r#"{"op":"unsubscribe","topic":"git","path":"/work/sample-project"}"#)
                .unwrap()
        else {
            panic!()
        };
        assert_eq!(gone.path.as_deref(), Some("/work/sample-project"));
        assert_eq!(Limits::default().git_subscriptions, GIT_SUBSCRIPTIONS);
        // A watch-v1 server without the git topic sends no git limit.
        let older: Limits = serde_json::from_value(json!({"activity_subscriptions":16,"page_bytes":524288,
            "queue_bytes":4194304,"activity_tail":100,"activity_read":200,"control_line_bytes":65536}))
        .unwrap();
        assert_eq!(older.git_subscriptions, 0);
        assert_eq!(older.processes_subscriptions, 0);
        assert_eq!(older.tasks_subscriptions, 0);
        assert_eq!(older.tasks_window, 0);
        assert_eq!(older.activity_older, 0);
        assert_eq!(Limits::default().activity_older, ACTIVITY_OLDER);
        // An older page is an additive option of an Activity subscribe.
        let Control::Subscribe(page) = decode_control(
            r#"{"op":"subscribe","topic":"activity","session":{"name":"codex/p/t","run_id":"r1"},"before":41,"tail":100}"#,
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!((page.before, page.tail, page.after), (Some(41), Some(100), None));
        let Control::Subscribe(plain) =
            decode_control(r#"{"op":"subscribe","topic":"activity","session":{"name":"codex/p/t"}}"#).unwrap()
        else {
            panic!()
        };
        assert_eq!(plain.before, None);
        assert_eq!(
            Limits::default().processes_subscriptions,
            PROCESSES_SUBSCRIPTIONS
        );
        assert_eq!(Limits::default().tasks_window, TASKS_WINDOW);
    }

    #[test]
    fn a_tasks_window_is_additive_on_the_wire() {
        let old: Subscription = serde_json::from_str(r#"{"topic":"tasks","project":"p"}"#).unwrap();
        assert_eq!(old.window, None);
        let plain = serde_json::to_value(&old).unwrap();
        assert!(
            plain.get("window").is_none(),
            "an unwindowed request is unchanged"
        );
        let windowed: Subscription =
            serde_json::from_str(r#"{"topic":"tasks","project":"p","window":50}"#).unwrap();
        assert_eq!(windowed.window, Some(50));
        assert_eq!(serde_json::to_value(&windowed).unwrap()["window"], 50);
    }

    #[test]
    fn catalog_keys_prefer_the_run() {
        assert_eq!(
            catalog_key(&json!({"name":"codex/p/t","run_id":"r1"})).as_deref(),
            Some("run:r1")
        );
        assert_eq!(
            catalog_key(&json!({"name":"sh/p","run_id":""})).as_deref(),
            Some("name:sh/p")
        );
        assert_eq!(
            catalog_key(&json!({"name":"sh/p","run_id":null})).as_deref(),
            Some("name:sh/p")
        );
        assert_eq!(
            catalog_key(&json!({"name":"sh/p"})).as_deref(),
            Some("name:sh/p")
        );
        assert_eq!(catalog_key(&json!({"run_id":"r1"})), None);
    }

    #[test]
    fn frames_are_capped_at_one_mebibyte_including_the_newline() {
        assert!(fits(MAX_FRAME_BYTES - 1));
        assert!(!fits(MAX_FRAME_BYTES));
        let small = "x".repeat(MAX_FRAME_BYTES - 1);
        assert!(check_size(&small).is_ok());
        let large = "x".repeat(MAX_FRAME_BYTES);
        assert!(
            matches!(check_size(&large), Err(FrameError::TooLarge(bytes)) if bytes == MAX_FRAME_BYTES + 1)
        );
        let body = Body::Events(Events {
            topic: topic::ACTIVITY.into(),
            key: "run:r".into(),
            events: vec![json!({"detail":"x".repeat(MAX_FRAME_BYTES)})],
            cursor: 1,
        });
        assert!(matches!(encode_body(&body), Err(FrameError::TooLarge(_))));
        assert!(matches!(
            encode(&Frame { v: 1, seq: 1, body }),
            Err(FrameError::TooLarge(_))
        ));
        assert!(decode(&large).is_err());
    }
}
