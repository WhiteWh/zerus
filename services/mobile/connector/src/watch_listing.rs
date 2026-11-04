//! The local listing from one long-lived `hgs watch --json` (watch-v1): the
//! catalog rows and `catalog.meta` the stream keeps current, composed as
//! `hgs ls --json --local --no-archives` prints them. While the stream is live
//! the heartbeat reads this instead of starting `ls` on every poll; otherwise it
//! runs `ls` as before. Peers keep their guarded routes.
use futures_util::future::{select, Either};
use serde_json::{Map, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::process::Stdio;
use std::rc::Rc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::time::Instant;
use zerus_watch_protocol as protocol;

/// A stream without any frame for this long, three heartbeats, is not live.
pub const SILENCE: Duration = Duration::from_secs(30);
/// Waits before starting the stream again after it ended or failed.
const RESTART: [u64; 4] = [5, 15, 60, 300];
/// A frame line beyond this is a broken stream.
const LINE_BYTES: usize = protocol::MAX_FRAME_BYTES;

/// The catalog as the frames of one stream built it.
#[derive(Default)]
pub struct Catalog {
    rows: BTreeMap<String, Value>,
    /// Snapshot pages until the last one arrives.
    pending: Option<BTreeMap<String, Value>>,
    meta: Option<Map<String, Value>>,
    /// Presence durations describe this observation, not the last heartbeat.
    meta_at: Option<Instant>,
    snapshot: bool,
    last_frame: Option<Instant>,
}

impl Catalog {
    /// Applies one frame line. An error ends the stream.
    pub fn apply(&mut self, line: &[u8], now: Instant) -> Result<(), String> {
        let frame: Value =
            serde_json::from_slice(line).map_err(|error| format!("invalid frame: {error}"))?;
        if frame["v"] != 1 {
            return Err("unsupported stream version".into());
        }
        self.last_frame = Some(now);
        let topic = frame["topic"].as_str().unwrap_or_default();
        match (frame["t"].as_str().unwrap_or_default(), topic) {
            ("snapshot", protocol::topic::CATALOG) => {
                if frame["page"] == 0 || self.pending.is_none() {
                    self.pending = Some(BTreeMap::new());
                }
                let pending = self.pending.as_mut().expect("set above");
                for item in frame["items"].as_array().into_iter().flatten() {
                    if let Some(key) = item["key"].as_str() {
                        pending.insert(key.to_owned(), item["row"].clone());
                    }
                }
                if frame["last"] == true {
                    self.rows = self.pending.take().unwrap_or_default();
                    self.snapshot = true;
                }
            }
            ("delta", protocol::topic::CATALOG) => {
                for item in frame["upsert"].as_array().into_iter().flatten() {
                    if let Some(key) = item["key"].as_str() {
                        self.rows.insert(key.to_owned(), item["row"].clone());
                    }
                }
                for removal in frame["remove"].as_array().into_iter().flatten() {
                    if let Some(key) = removal["key"].as_str() {
                        self.rows.remove(key);
                    }
                }
            }
            ("snapshot", protocol::topic::CATALOG_META) => {
                if let Value::Object(meta) = &frame["value"] {
                    self.meta = Some(meta.clone());
                    self.meta_at = Some(now);
                }
            }
            ("error", _) if frame["fatal"] == true => {
                return Err(format!(
                    "the stream failed: {}",
                    frame["message"].as_str().unwrap_or_default()
                ));
            }
            // A catalog gap keeps the rows until the replacement snapshot.
            _ => {}
        }
        Ok(())
    }

    /// The listing `ls --json --local --no-archives` prints, once the stream
    /// sent its catalog and meta and is not silent.
    pub fn listing(&self, now: Instant) -> Option<Value> {
        let meta = self.meta.as_ref()?;
        if !self.snapshot
            || self
                .last_frame
                .is_none_or(|at| now.saturating_duration_since(at) > SILENCE)
        {
            return None;
        }
        // `ls` orders sessions by name.
        let mut sessions: Vec<&Value> = self.rows.values().collect();
        sessions.sort_by(|a, b| {
            a["name"]
                .as_str()
                .unwrap_or_default()
                .cmp(b["name"].as_str().unwrap_or_default())
        });
        let mut listing = meta.clone();
        // An otherwise-live stream can keep sending heartbeats or session
        // deltas without another metadata sample. Neither renews presence.
        let presence = self
            .meta_at
            .and_then(|at| now.checked_duration_since(at))
            .and_then(|age| {
                listing
                    .get("desktop_presence")
                    .and_then(|p| aged_presence(p, age))
            });
        match presence {
            Some(presence) => {
                listing.insert("desktop_presence".into(), presence);
            }
            None => {
                listing.remove("desktop_presence");
            }
        }
        listing.insert(
            "sessions".into(),
            Value::Array(sessions.into_iter().cloned().collect()),
        );
        Some(Value::Object(listing))
    }
}

/// Preserve the native relative-duration contract without giving a cached
/// sample a new observation time. Only a fresh metadata frame can renew it.
fn aged_presence(value: &Value, age: Duration) -> Option<Value> {
    let seconds = |value: &Value| value.as_f64().filter(|n| n.is_finite() && *n >= 0.0);
    let age = age.as_secs_f64();
    if age > zerus_relay::alerts::PRESENCE_FRESH {
        return None;
    }
    let idle = seconds(&value["idle_seconds"])?;
    let locked = value["locked"].as_bool()?;
    let lock = match value.get("locked_seconds") {
        None | Some(Value::Null) => None,
        Some(value) => Some(seconds(value)?),
    };
    let mut presence = value.as_object()?.clone();
    presence.insert(
        "idle_seconds".into(),
        serde_json::Number::from_f64(idle + age)?.into(),
    );
    if locked {
        if let Some(lock) = lock {
            presence.insert(
                "locked_seconds".into(),
                serde_json::Number::from_f64(lock + age)?.into(),
            );
        }
    }
    Some(Value::Object(presence))
}

/// Reads one line of at most `LINE_BYTES`; None at the end of the stream.
async fn read_line<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
    line: &mut Vec<u8>,
) -> Result<Option<()>, String> {
    line.clear();
    loop {
        let available = reader.fill_buf().await.map_err(|error| error.to_string())?;
        if available.is_empty() {
            return Ok(None);
        }
        let (used, done) = match available.iter().position(|byte| *byte == b'\n') {
            Some(end) => (end + 1, true),
            None => (available.len(), false),
        };
        line.extend_from_slice(&available[..used]);
        reader.consume(used);
        if line.len() > LINE_BYTES {
            return Err("a frame exceeds 1 MiB".into());
        }
        if done {
            line.pop();
            return Ok(Some(()));
        }
    }
}

/// Follows the local stream once started and keeps it running.
#[derive(Default)]
pub struct WatchListing {
    catalog: Rc<RefCell<Catalog>>,
    started: std::cell::Cell<bool>,
}

impl WatchListing {
    /// The current listing while the stream is live.
    pub fn listing(&self) -> Option<Value> {
        self.catalog.borrow().listing(Instant::now())
    }

    /// Starts following `hgs watch --json` with `command` (the native CLI and
    /// its environment) on the current local task set, once. Each run that
    /// ends or fails clears the catalog and starts again after 5, 15 and
    /// 60 s, then every 5 minutes; a run that went live resets the wait.
    /// `stopping` ends it.
    pub fn follow(
        &self,
        command: impl Fn() -> Command + 'static,
        stopping: Rc<crate::runtime::Event>,
    ) {
        if self.started.replace(true) {
            return;
        }
        let catalog = Rc::clone(&self.catalog);
        tokio::task::spawn_local(async move {
            let mut failures = 0usize;
            while !stopping.is_set() {
                let live = run(&mut command(), &catalog, &stopping).await;
                *catalog.borrow_mut() = Catalog::default();
                if stopping.is_set() {
                    return;
                }
                failures = if live { 1 } else { failures + 1 };
                let wait = Duration::from_secs(RESTART[(failures - 1).min(RESTART.len() - 1)]);
                if let Either::Left(_) = select(
                    Box::pin(stopping.wait()),
                    Box::pin(tokio::time::sleep(wait)),
                )
                .await
                {
                    return;
                }
            }
        });
    }
}

/// One run of the stream; true when it went live before it ended.
async fn run(
    command: &mut Command,
    catalog: &Rc<RefCell<Catalog>>,
    stopping: &Rc<crate::runtime::Event>,
) -> bool {
    command
        .arg("watch")
        .arg("--json")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    // Kept open: the end of its input ends the stream.
    let _input = child.stdin.take();
    let Some(output) = child.stdout.take() else {
        return false;
    };
    let mut reader = BufReader::new(output);
    let mut line = Vec::new();
    let mut live = false;
    loop {
        let read = match select(
            Box::pin(stopping.wait()),
            Box::pin(tokio::time::timeout(
                SILENCE,
                read_line(&mut reader, &mut line),
            )),
        )
        .await
        {
            Either::Left(_) => break,
            Either::Right((read, _)) => read,
        };
        match read {
            Ok(Ok(Some(()))) => {
                let now = Instant::now();
                let mut catalog = catalog.borrow_mut();
                if catalog.apply(&line, now).is_err() {
                    break;
                }
                live |= catalog.listing(now).is_some();
            }
            // Silence, the end of the stream or an unreadable frame.
            _ => break,
        }
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
    live
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(value: Value) -> Vec<u8> {
        serde_json::to_vec(&value).unwrap()
    }

    #[test]
    fn frames_build_the_listing_ls_prints_once_catalog_and_meta_arrived() {
        let now = Instant::now();
        let mut catalog = Catalog::default();
        catalog
            .apply(
                &line(json!({"v":1,"seq":1,"t":"hello","topics":["catalog"]})),
                now,
            )
            .unwrap();
        assert!(catalog.listing(now).is_none());
        catalog.apply(&line(json!({"v":1,"seq":2,"t":"snapshot","topic":"catalog","page":0,"last":false,"cursor":1,
            "items":[{"key":"run:b","row":{"name":"codex/zeta","run_id":"b"}}]})), now).unwrap();
        catalog.apply(&line(json!({"v":1,"seq":3,"t":"snapshot","topic":"catalog","page":1,"last":true,"cursor":1,
            "items":[{"key":"run:a","row":{"name":"claude/VaRest","run_id":"a"}},{"key":"run:c","row":{"name":"claude/alpha","run_id":"c"}}]})), now).unwrap();
        assert!(catalog.listing(now).is_none(), "meta has not arrived");
        catalog.apply(&line(json!({"v":1,"seq":4,"t":"snapshot","topic":"catalog.meta","page":0,"last":true,"cursor":1,
            "value":{"host":"fixture","ok":true,"peers":["mac"]}})), now).unwrap();
        let listing = catalog.listing(now).unwrap();
        assert_eq!(listing["host"], "fixture");
        assert_eq!(listing["peers"], json!(["mac"]));
        // Byte order of names, as `ls` sorts them.
        let names: Vec<&str> = listing["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["claude/VaRest", "claude/alpha", "codex/zeta"]);
        // Deltas upsert and remove by key; a rename keeps the key.
        catalog
            .apply(
                &line(
                    json!({"v":1,"seq":5,"t":"delta","topic":"catalog","cursor":2,
            "upsert":[{"key":"run:b","row":{"name":"codex/renamed","run_id":"b"}}],
            "remove":[{"key":"run:c","reason":"archived"}]}),
                ),
                now,
            )
            .unwrap();
        let names: Vec<String> = catalog.listing(now).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["claude/VaRest", "codex/renamed"]);
        // A gap keeps the rows until the next snapshot; Activity frames change nothing.
        catalog
            .apply(
                &line(json!({"v":1,"seq":6,"t":"gap","topic":"catalog","reason":"overflow"})),
                now,
            )
            .unwrap();
        catalog.apply(&line(json!({"v":1,"seq":7,"t":"events","topic":"activity","key":"run:b","events":[],"cursor":9})), now).unwrap();
        assert_eq!(
            catalog.listing(now).unwrap()["sessions"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        // A silent stream is not live.
        assert!(catalog
            .listing(now + SILENCE + Duration::from_secs(1))
            .is_none());
        assert!(catalog.listing(now + SILENCE).is_some());
    }

    #[test]
    fn broken_or_failing_streams_end() {
        let now = Instant::now();
        let mut catalog = Catalog::default();
        assert!(catalog.apply(b"not json", now).is_err());
        assert!(catalog
            .apply(&line(json!({"v":2,"seq":1,"t":"hello"})), now)
            .is_err());
        assert!(catalog
            .apply(
                &line(json!({"v":1,"seq":1,"t":"error","code":"x","message":"gone","fatal":true})),
                now
            )
            .is_err());
        catalog
            .apply(
                &line(
                    json!({"v":1,"seq":2,"t":"error","code":"x","message":"later","fatal":false}),
                ),
                now,
            )
            .unwrap();
    }

    fn presence_catalog(presence: Value, now: Instant) -> Catalog {
        let mut catalog = Catalog::default();
        catalog
            .apply(
                &line(json!({"v":1,"seq":1,"t":"snapshot","topic":"catalog",
            "page":0,"last":true,"items":[]})),
                now,
            )
            .unwrap();
        catalog
            .apply(
                &line(json!({"v":1,"seq":2,"t":"snapshot","topic":"catalog.meta",
            "page":0,"last":true,"value":{"host":"fixture","desktop_presence":presence}})),
                now,
            )
            .unwrap();
        catalog
    }

    #[test]
    fn heartbeat_and_catalog_deltas_age_presence_without_renewing_its_sample() {
        let now = Instant::now();
        let original =
            json!({"idle_seconds":4,"locked":true,"locked_seconds":7,"future":{"kept":true}});
        let mut catalog = presence_catalog(original.clone(), now);
        for seconds in [10, 20, 120, 121] {
            let at = now + Duration::from_secs(seconds);
            let frame = if seconds == 20 {
                json!({"v":1,"seq":3,"t":"delta","topic":"catalog","upsert":[],"remove":[]})
            } else {
                json!({"v":1,"seq":3,"t":"heartbeat"})
            };
            catalog.apply(&line(frame), at).unwrap();
            let listing = catalog.listing(at).expect("session stream remains live");
            if seconds <= 120 {
                assert_eq!(
                    listing["desktop_presence"]["idle_seconds"],
                    4.0 + seconds as f64
                );
                assert_eq!(
                    listing["desktop_presence"]["locked_seconds"],
                    7.0 + seconds as f64
                );
                assert_eq!(listing["desktop_presence"]["future"], original["future"]);
            } else {
                assert!(listing.get("desktop_presence").is_none());
            }
            assert_eq!(
                catalog.meta.as_ref().unwrap()["desktop_presence"],
                original,
                "listing calls must not repeatedly age or mutate the retained sample"
            );
        }
    }

    #[test]
    fn only_fresh_metadata_can_renew_presence_and_missing_presence_clears_it() {
        let now = Instant::now();
        let mut catalog = presence_catalog(
            json!({"idle_seconds":0,"locked":false,"locked_seconds":null}),
            now,
        );
        let at = now + Duration::from_secs(121);
        catalog
            .apply(&line(json!({"v":1,"t":"heartbeat"})), at)
            .unwrap();
        assert!(catalog
            .listing(at)
            .unwrap()
            .get("desktop_presence")
            .is_none());
        catalog.apply(&line(json!({"v":1,"t":"snapshot","topic":"catalog.meta","page":0,"last":true,
            "value":{"host":"fixture","desktop_presence":{"idle_seconds":2,"locked":false,"locked_seconds":null}}})), at).unwrap();
        let aged = catalog.listing(at + Duration::from_secs(3)).unwrap();
        assert_eq!(aged["desktop_presence"]["idle_seconds"], 5.0);
        assert_eq!(aged["desktop_presence"]["locked_seconds"], Value::Null);
        catalog
            .apply(
                &line(
                    json!({"v":1,"t":"snapshot","topic":"catalog.meta","page":0,"last":true,
            "value":{"host":"fixture"}}),
                ),
                at,
            )
            .unwrap();
        assert!(catalog
            .listing(at)
            .unwrap()
            .get("desktop_presence")
            .is_none());
    }

    #[test]
    fn malformed_presence_never_becomes_an_active_or_away_default() {
        let now = Instant::now();
        for value in [
            Value::Null,
            json!([]),
            json!({"state":"active"}),
            json!({"idle_seconds":-1,"locked":false}),
            json!({"idle_seconds":"0","locked":false}),
            json!({"idle_seconds":0}),
            json!({"idle_seconds":0,"locked":"false"}),
            json!({"idle_seconds":0,"locked":true,"locked_seconds":-1}),
        ] {
            let catalog = presence_catalog(value, now);
            assert!(catalog
                .listing(now)
                .unwrap()
                .get("desktop_presence")
                .is_none());
        }
    }

    #[test]
    fn no_writer_does_not_republish_old_presence_despite_a_live_stream() {
        use zerus_relay::alerts::{away_since, decide, Decision, Delivery, Presence};
        let now = Instant::now();
        let sample = json!({"idle_seconds":0,"locked":false});
        let mut catalog = presence_catalog(sample.clone(), now);
        // Writer gone since the Qt switch: liveness alone is not a new sample.
        let later = now + Duration::from_secs(86400);
        catalog
            .apply(&line(json!({"v":1,"t":"heartbeat"})), later)
            .unwrap();
        let listing = catalog.listing(later).unwrap();
        assert!(listing.get("desktop_presence").is_none());
        assert_eq!(listing["host"], "fixture");
        assert_eq!(listing["sessions"], json!([]));
        assert!(catalog
            .listing(now - Duration::from_secs(1))
            .unwrap()
            .get("desktop_presence")
            .is_none());
        // Preserve D2: the last real activity is only a historical timer.
        // With no writer since the switch, every away_N timer has elapsed;
        // neither a heartbeat nor repeated evaluation resets that sample.
        let historical = Presence::from_value(&sample, 0.0).unwrap();
        assert_eq!(
            Presence::from_value(&listing["desktop_presence"], 86_400.0),
            None
        );
        for minutes in [0, 5, 10, 15, 30] {
            assert_eq!(
                decide(Delivery::Away(minutes), away_since(&[historical]), 86_400.0),
                Decision::Deliver
            );
        }
    }

    #[test]
    fn lines_are_bounded_and_end_with_the_stream() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(bounded_lines());
    }

    async fn bounded_lines() {
        let mut long = vec![b'x'; LINE_BYTES + 2];
        long.push(b'\n');
        let input: &[u8] = b"{\"a\":1}\n{\"b\":2}";
        let mut reader = BufReader::new(input);
        let mut line = Vec::new();
        assert_eq!(read_line(&mut reader, &mut line).await, Ok(Some(())));
        assert_eq!(line, b"{\"a\":1}");
        // A last line without its newline is incomplete: the stream ended.
        assert_eq!(read_line(&mut reader, &mut line).await, Ok(None));
        let mut reader = BufReader::new(long.as_slice());
        assert!(read_line(&mut reader, &mut line).await.is_err());
    }
}
