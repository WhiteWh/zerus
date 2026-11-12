//! Requests and tokens per model and day from this computer's native DeepSeek
//! session logs (`DSH_HOME/sessions/<project>/<session>/session.vN.jsonl.zstd`).
//! Logs are only read. The index remembers each log's identity, size, mtime
//! and committed byte offset, so a refresh reads only appended frames, within
//! a byte and time budget, newest logs first, and never rereads an unchanged
//! log. A replaced or shortened log is read again from its start. Counts older
//! than the window are dropped, so the index and its projection stay bounded.
use super::*;
use chrono::{Local, NaiveDate};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Days kept and projected, today included: always covers the current month.
const WINDOW_DAYS: i64 = 31;
/// Session directories examined per refresh.
const SESSION_LIMIT: usize = 20_000;
/// Logs handed to one reader run; the rest wait for the next refresh.
const BATCH_LIMIT: usize = 512;
const READ_BYTES: u64 = 32 * 1024 * 1024;
const READ_MS: u64 = 2_500;
const INDEX_BYTES: u64 = 16 * 1024 * 1024;
const HEAD_BYTES: u64 = 4096;
const MODELS_PER_DAY: usize = 8;

type Counts = [u64; 4];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Entry {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: i64,
    offset: u64,
    /// SHA-256 of the first `head_len` bytes: a replacement under the same inode differs.
    head: String,
    head_len: u64,
    /// SHA-256 of the `tail_len` bytes ending at `offset`: what was already
    /// counted. A rewrite of them, even under the same inode and first bytes,
    /// means the log is read again from its start.
    tail: String,
    tail_len: u64,
    done: bool,
    /// Unreadable or malformed until the log changes.
    failed: bool,
    days: BTreeMap<String, BTreeMap<String, Counts>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    schema: u32,
    files: BTreeMap<String, Entry>,
}

/// The canonical current-generation log of one session directory: the highest
/// `session.vN.jsonl.zstd`, or version zero's `session.jsonl.zstd`. Older
/// generations hold the same events again.
fn current_log(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(u64, PathBuf)> = None;
    for entry in fs::read_dir(dir).ok()?.flatten().take(256) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let version = if name == "session.jsonl.zstd" {
            Some(0)
        } else {
            name.strip_prefix("session.v")
                .and_then(|rest| rest.strip_suffix(".jsonl.zstd"))
                .filter(|v| {
                    !v.is_empty() && !v.starts_with('0') && v.bytes().all(|b| b.is_ascii_digit())
                })
                .and_then(|v| v.parse::<u64>().ok())
        };
        if let Some(version) = version {
            if best.as_ref().is_none_or(|(b, _)| version > *b) {
                best = Some((version, entry.path()));
            }
        }
    }
    best.map(|(_, path)| path)
}

/// Each session's current log as (index key, path), and whether the walk stopped at its limit.
fn logs(sessions: &Path) -> (Vec<(String, PathBuf)>, bool) {
    let mut found = Vec::new();
    let mut seen = 0;
    let Ok(projects) = fs::read_dir(sessions) else {
        return (found, false);
    };
    for project in projects.flatten() {
        if !project.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(dirs) = fs::read_dir(project.path()) else {
            continue;
        };
        for session in dirs.flatten() {
            if !session.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            seen += 1;
            if seen > SESSION_LIMIT {
                return (found, true);
            }
            if let Some(path) = current_log(&session.path()) {
                let key = path
                    .strip_prefix(sessions)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                found.push((key, path));
            }
        }
    }
    (found, false)
}

fn head(path: &Path, len: u64) -> Option<String> {
    range_hash(path, 0, len)
}

/// SHA-256 of `len` bytes from `start`, or None when fewer can be read.
fn range_hash(path: &Path, start: u64, len: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut options = fs::OpenOptions::new();
    options.read(true);
    platform::no_follow(&mut options);
    let mut file = options.open(path).ok()?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(len).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 == len).then(|| format!("{:x}", Sha256::digest(&bytes)))
}

#[cfg(unix)]
fn identity(meta: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}
#[cfg(not(unix))]
fn identity(_: &fs::Metadata) -> (u64, u64) {
    (0, 0)
}

fn nanos(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// What one refresh may still read. The identity head checks and the reader
/// share it, so a first refresh of many logs stays within the stated bounds.
struct Budget {
    bytes: u64,
    deadline: Instant,
}

impl Budget {
    /// Takes `bytes` if they and the time remain.
    fn take(&mut self, bytes: u64) -> bool {
        if self.bytes < bytes || Instant::now() >= self.deadline {
            return false;
        }
        self.bytes -= bytes;
        true
    }

    fn millis_left(&self) -> u64 {
        self.deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(u64::MAX as u128) as u64
    }
}

/// Brings `index` up to date with the logs on disk and returns the logs to read
/// next, newest first, and whether a log's identity check waits for a later
/// refresh. Logs last written before `window_start` are left out. A log whose
/// check is deferred keeps its previous counts; a new one is not yet indexed.
fn plan(
    index: &mut Index,
    found: Vec<(String, PathBuf)>,
    window_start: SystemTime,
    budget: &mut Budget,
) -> (Vec<(String, PathBuf)>, bool) {
    let mut logs = Vec::new();
    for (key, path) in found {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if meta.is_file() && modified >= window_start {
            let (dev, ino) = identity(&meta);
            logs.push((nanos(modified), meta.len(), dev, ino, key, path));
        }
    }
    // The newest logs get the budget first.
    logs.sort_by_key(|log| std::cmp::Reverse(log.0));
    let mut files = BTreeMap::new();
    let mut work = Vec::new();
    let mut deferred = false;
    for (mtime, size, dev, ino, key, path) in logs {
        let previous = index.files.remove(&key);
        let unchanged = previous
            .as_ref()
            .is_some_and(|e| e.dev == dev && e.ino == ino && e.size == size && e.mtime == mtime);
        let mut entry = match previous {
            Some(e) if unchanged => e,
            previous => {
                // Grown in place: the same inode, a larger size, and the same
                // first bytes and bytes before the committed offset. dsh only
                // appends, so a write that keeps the size is a rewrite.
                let same = previous.as_ref().filter(|e| {
                    e.dev == dev
                        && e.ino == ino
                        && size > e.size
                        && size >= e.offset
                        && size >= e.head_len
                });
                let grown = match same {
                    Some(e) if !budget.take(e.head_len + e.tail_len) => {
                        deferred = true;
                        files.insert(key, e.clone());
                        continue;
                    }
                    Some(e) => {
                        head(&path, e.head_len).as_ref() == Some(&e.head)
                            && (e.tail_len == 0
                                || range_hash(&path, e.offset - e.tail_len, e.tail_len).as_ref()
                                    == Some(&e.tail))
                    }
                    None => false,
                };
                match previous {
                    Some(e) if grown => e,
                    previous => {
                        let head_len = size.min(HEAD_BYTES);
                        if !budget.take(head_len) {
                            deferred = true;
                            if let Some(e) = previous {
                                files.insert(key, e);
                            }
                            continue;
                        }
                        Entry {
                            dev,
                            ino,
                            head_len,
                            head: head(&path, head_len).unwrap_or_default(),
                            ..Entry::default()
                        }
                    }
                }
            }
        };
        if !unchanged {
            entry.size = size;
            entry.mtime = mtime;
            entry.done = false;
            entry.failed = false;
        }
        if !entry.done && !entry.failed && work.len() < BATCH_LIMIT {
            work.push((key.clone(), path));
        }
        files.insert(key, entry);
    }
    index.files = files;
    (work, deferred)
}

/// Applies the reader's results: appended counts add up, a fork's cut replaces
/// what was counted before it, a rewritten log starts over.
fn apply(index: &mut Index, results: &Value) {
    for result in results.as_array().into_iter().flatten() {
        let Some(entry) = result["key"]
            .as_str()
            .and_then(|key| index.files.get_mut(key))
        else {
            continue;
        };
        match result["error"].as_str() {
            Some("rewritten") => {
                *entry = Entry {
                    dev: entry.dev,
                    ino: entry.ino,
                    size: entry.size,
                    mtime: entry.mtime,
                    ..Entry::default()
                };
                continue;
            }
            Some(_) => {
                entry.failed = true;
                continue;
            }
            None => {}
        }
        // A log may have grown since it was planned; the next plan sees its new size.
        let Some(offset) = result["offset"].as_u64().filter(|o| *o >= entry.offset) else {
            entry.failed = true;
            continue;
        };
        if result["cut"] == true {
            entry.days.clear();
        }
        for (day, models) in result["days"].as_object().into_iter().flatten() {
            if NaiveDate::parse_from_str(day, "%Y-%m-%d").is_err() {
                continue;
            }
            for (model, row) in models.as_object().into_iter().flatten() {
                let counts = entry
                    .days
                    .entry(day.clone())
                    .or_default()
                    .entry(model.chars().take(64).collect())
                    .or_default();
                for (i, value) in row.as_array().into_iter().flatten().take(4).enumerate() {
                    counts[i] = counts[i].saturating_add(value.as_u64().unwrap_or(0));
                }
            }
        }
        if let (Some(tail), Some(tail_len)) = (result["tail"].as_str(), result["tail_len"].as_u64())
        {
            if tail_len <= offset && tail.len() == 64 {
                entry.tail = tail.to_owned();
                entry.tail_len = tail_len;
            }
        }
        entry.offset = offset;
        entry.done = result["done"] == true;
    }
}

/// Totals per day, newest first, with each day's busiest models.
/// `reading`: some logs wait for a later refresh, so totals may grow.
/// Unreadable logs are set aside until they change; their requests are not counted.
fn project(index: &Index, today: &str, since: &str, reading: bool) -> Value {
    let unreadable = index.files.values().filter(|e| e.failed).count();
    let mut totals: BTreeMap<&str, BTreeMap<&str, Counts>> = BTreeMap::new();
    for entry in index.files.values() {
        for (day, models) in entry.days.range(since.to_owned()..) {
            for (model, counts) in models {
                let total = totals.entry(day).or_default().entry(model).or_default();
                for i in 0..4 {
                    total[i] = total[i].saturating_add(counts[i]);
                }
            }
        }
    }
    let days: Vec<Value> = totals
        .into_iter()
        .rev()
        .take(WINDOW_DAYS as usize)
        .map(|(day, models)| {
            let mut models: Vec<(&str, Counts)> = models.into_iter().collect();
            models.sort_by(|a, b| b.1[0].cmp(&a.1[0]).then(a.0.cmp(b.0)));
            if models.len() > MODELS_PER_DAY {
                let other = models.split_off(MODELS_PER_DAY - 1).into_iter().fold([0; 4], |mut sum: Counts, (_, c)| {
                    for i in 0..4 {
                        sum[i] = sum[i].saturating_add(c[i]);
                    }
                    sum
                });
                models.push(("other", other));
            }
            let models: Vec<Value> = models
                .into_iter()
                .map(|(model, c)| json!({"model":model,"requests":c[0],"cache_hit":c[1],"cache_miss":c[2],"output":c[3]}))
                .collect();
            json!({"day":day,"models":models})
        })
        .collect();
    json!({"today":today,"partial":reading || unreadable > 0,"reading":reading,"unreadable":unreadable,"days":days})
}

/// Refreshes the index of `home`'s session logs and returns the projection, or
/// None without a sessions directory.
pub(super) fn refresh(config: &Config, id: &str, home: &Path) -> Option<Value> {
    refresh_within(config, id, home, READ_BYTES)
}

fn refresh_within(config: &Config, id: &str, home: &Path, bytes: u64) -> Option<Value> {
    refresh_on(config, id, home, bytes, Local::now().date_naive(), false)
}

// A fixed day and UTC reader keep compressed-frame fixtures independent of
// the machine's date and timezone. Production keeps the local calendar.
fn refresh_on(
    config: &Config,
    id: &str,
    home: &Path,
    bytes: u64,
    today: NaiveDate,
    utc: bool,
) -> Option<Value> {
    let mut budget = Budget {
        bytes,
        deadline: Instant::now() + Duration::from_millis(READ_MS),
    };
    let sessions = home.join("sessions");
    if !fs::symlink_metadata(&sessions).ok()?.is_dir() {
        return None;
    }
    let path = super::deepseek::state_path(config, "account-usage", id, home);
    platform::private_dir(path.parent()?).ok()?;
    let since_date = today - chrono::Duration::days(WINDOW_DAYS - 1);
    let since = since_date.format("%Y-%m-%d").to_string();
    let midnight = since_date.and_hms_opt(0, 0, 0)?;
    let seconds = if utc {
        Some(midnight.and_utc().timestamp())
    } else {
        midnight
            .and_local_timezone(Local)
            .earliest()
            .map(|t| t.timestamp())
    };
    let window_start = seconds
        .map(|t| UNIX_EPOCH + Duration::from_secs(t.max(0) as u64))
        .unwrap_or(UNIX_EPOCH);
    let read = |path: &Path| -> Index {
        super::deepseek::read_private::<Index>(path, INDEX_BYTES)
            .filter(|i| i.schema == 1)
            .unwrap_or(Index {
                schema: 1,
                ..Index::default()
            })
    };
    let Some(_lock) = super::deepseek::try_lock(&path) else {
        // Another refresh is indexing: show what it last saved.
        let index = read(&path);
        return Some(project(
            &index,
            &today.format("%Y-%m-%d").to_string(),
            &since,
            true,
        ));
    };
    let mut index = read(&path);
    let (found, truncated) = logs(&sessions);
    let (work, deferred) = plan(&mut index, found, window_start, &mut budget);
    let millis = budget.millis_left();
    if !work.is_empty() && budget.bytes > 0 && millis > 0 {
        let offsets: BTreeMap<&str, u64> = work
            .iter()
            .filter_map(|(key, _)| Some((key.as_str(), index.files.get(key)?.offset)))
            .collect();
        apply(
            &mut index,
            &read_logs(&work, &offsets, &since, budget.bytes, millis, utc),
        );
    }
    for entry in index.files.values_mut() {
        entry.days = entry.days.split_off(&since);
    }
    let _ = super::deepseek::write_private(&path, &index);
    let reading = truncated || deferred || index.files.values().any(|e| !e.done && !e.failed);
    Some(project(
        &index,
        &today.format("%Y-%m-%d").to_string(),
        &since,
        reading,
    ))
}

/// Runs the reader over `work` from each log's committed offset.
fn read_logs(
    work: &[(String, PathBuf)],
    offsets: &BTreeMap<&str, u64>,
    since: &str,
    bytes: u64,
    millis: u64,
    utc: bool,
) -> Value {
    let files: Vec<Value> = work
        .iter()
        .map(|(key, path)| json!({"key":key,"path":path,"offset":offsets.get(key.as_str()).copied().unwrap_or(0)}))
        .collect();
    let mut command = Command::new("node");
    if utc {
        command.env("TZ", "UTC");
    }
    command.args([
        "--input-type=module",
        "-e",
        include_str!("../dsh/usage-read.mjs"),
    ]);
    let request = json!({"files":files,"since_day":since,"max_bytes":bytes,"max_ms":millis});
    super::inspection::capture(command, request.to_string().as_bytes(), 10)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .map(|reply| reply["results"].clone())
        .unwrap_or(Value::Null)
}

/// The cached projection's known fields only, bounded.
pub(super) fn public(value: &Value) -> Option<Value> {
    let today = value["today"].as_str().filter(|d| d.len() == 10)?;
    let days: Vec<Value> = value["days"]
        .as_array()?
        .iter()
        .take(WINDOW_DAYS as usize)
        .filter_map(|day| {
            let date = day["day"].as_str().filter(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok())?;
            let models: Vec<Value> = day["models"]
                .as_array()?
                .iter()
                .take(MODELS_PER_DAY)
                .filter_map(|m| {
                    let model: String = m["model"].as_str()?.chars().filter(|c| !c.is_control()).take(64).collect();
                    let n = |field: &str| m[field].as_u64();
                    Some(json!({"model":model,"requests":n("requests")?,"cache_hit":n("cache_hit")?,"cache_miss":n("cache_miss")?,"output":n("output")?}))
                })
                .collect();
            Some(json!({"day":date,"models":models}))
        })
        .collect();
    let partial = value["partial"].as_bool().unwrap_or(false);
    Some(
        json!({"today":today,"partial":partial,"reading":value["reading"].as_bool().unwrap_or(partial),
        "unreadable":value["unreadable"].as_u64().unwrap_or(0),"days":days}),
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// One checksummed Zstandard frame of `text`, made by Node like dsh's own writer.
    fn frame(text: &str) -> Vec<u8> {
        let mut command = Command::new("node");
        command.args([
            "-e",
            "const z=require('zlib');let s='';process.stdin.on('data',d=>s+=d).on('end',()=>process.stdout.write(z.zstdCompressSync(Buffer.from(s),{params:{[z.constants.ZSTD_c_checksumFlag]:1}}).toString('base64')))",
        ]);
        let encoded = super::super::inspection::capture(command, text.as_bytes(), 20)
            .expect("node compresses");
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap()
    }

    /// Several frames from one Node run, in order.
    fn frames(texts: &[String]) -> Vec<Vec<u8>> {
        let mut command = Command::new("node");
        command.args([
            "-e",
            "const z=require('zlib');let s='';process.stdin.on('data',d=>s+=d).on('end',()=>process.stdout.write(JSON.stringify(JSON.parse(s).map(t=>z.zstdCompressSync(Buffer.from(t),{params:{[z.constants.ZSTD_c_checksumFlag]:1}}).toString('base64')))))",
        ]);
        let encoded =
            super::super::inspection::capture(command, json!(texts).to_string().as_bytes(), 20)
                .expect("node compresses");
        use base64::Engine;
        serde_json::from_slice::<Vec<String>>(&encoded)
            .unwrap()
            .iter()
            .map(|f| base64::engine::general_purpose::STANDARD.decode(f).unwrap())
            .collect()
    }

    /// A first frame longer than HEAD_BYTES even compressed: hex of hashes.
    fn long_first_frame() -> Vec<u8> {
        let text: String = (0..400u32)
            .map(|i| format!("{:x}", Sha256::digest(i.to_le_bytes())))
            .collect();
        let first = frame(
            &(header(false)
                + &format!(
                    "{}\n",
                    json!({"type":"system/message","seq":0,"time":0,"data":{"text":text}})
                )),
        );
        assert!(first.len() as u64 > HEAD_BYTES);
        first
    }

    fn fixed_day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 1, 15).unwrap()
    }

    fn fixed_noon() -> i64 {
        fixed_day()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
    }

    /// Pick two genuinely different equal-length frames, rather than requiring
    /// the first frame's length to repeat (which depends on timestamp digits).
    fn same_length_messages_at(noon: i64) -> (Vec<u8>, Vec<u8>, u64, u64) {
        let texts: Vec<String> = (1000..1200u64)
            .map(|n| message(noon, "deepseek-flash", n, n, n))
            .collect();
        let all = frames(&texts);
        let mut lengths = BTreeMap::new();
        for (index, frame) in all.iter().enumerate() {
            if let Some(previous) = lengths.insert(frame.len(), index) {
                return (
                    all[previous].clone(),
                    frame.clone(),
                    1000 + previous as u64,
                    1000 + index as u64,
                );
            }
        }
        panic!("a same-length frame");
    }

    fn same_length_messages() -> (Vec<u8>, Vec<u8>, u64, u64) {
        same_length_messages_at(fixed_noon())
    }

    #[test]
    fn same_length_frames_cover_a_calendar_year() {
        let mut day = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let end = NaiveDate::from_ymd_opt(2027, 1, 1).unwrap();
        let mut checked = 0;
        while day < end {
            let noon = day
                .and_hms_opt(12, 0, 0)
                .unwrap()
                .and_local_timezone(Local)
                .earliest()
                .unwrap()
                .timestamp_millis();
            let (before, after, a, b) = same_length_messages_at(noon);
            assert_eq!(before.len(), after.len(), "{day}");
            assert_ne!(before, after, "{day}");
            assert_ne!(a, b);
            checked += 1;
            day = day.succ_opt().unwrap();
        }
        eprintln!(
            "FRAME_CALENDAR days={checked} TZ={}",
            std::env::var("TZ").unwrap_or_else(|_| "system".into())
        );
        assert_eq!(checked, 365);
    }

    fn header(seeded: bool) -> String {
        format!("{{\"type\":\"session\",\"version\":4,\"id\":\"s\",\"createdAt\":0,\"isSeeded\":{seeded},\"delegationDepth\":0}}\n")
    }

    fn message(ms: i64, model: &str, hit: u64, miss: u64, out: u64) -> String {
        format!(
            "{}\n",
            json!({"type":"assistant/message","seq":1,"time":ms,"data":{"message":{"role":"assistant","source":{"kind":"model","provider":"deepseek-official","model":model}},
                "usage":{"inputTokens":miss,"outputTokens":out,"cacheReadTokens":hit,"cacheWriteTokens":0,"totalTokens":hit+miss+out}}})
        )
    }

    struct Fixture {
        _root: tempfile::TempDir,
        config: Config,
        home: PathBuf,
        fixed: bool,
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let config = Config {
            home: root.path().to_string_lossy().into(),
            dir: root.path().join("config"),
            state: root.path().join("state"),
            selves: vec!["fixture".into()],
            peers: vec![],
            base_peers: vec![],
            peers_from_environment: false,
            machines: Default::default(),
            tab: false,
            tab_colors: String::new(),
        };
        platform::private_dir(&config.dir).unwrap();
        let home = root.path().join("dsh");
        fs::create_dir_all(home.join("sessions")).unwrap();
        Fixture {
            _root: root,
            config,
            home,
            fixed: false,
        }
    }

    impl Fixture {
        fn log(&self, project: &str, session: &str, name: &str) -> PathBuf {
            let dir = self.home.join("sessions").join(project).join(session);
            fs::create_dir_all(&dir).unwrap();
            dir.join(name)
        }
        fn refresh(&self) -> Value {
            if self.fixed {
                refresh_on(
                    &self.config,
                    "native-dsh",
                    &self.home,
                    READ_BYTES,
                    fixed_day(),
                    true,
                )
                .unwrap()
            } else {
                refresh(&self.config, "native-dsh", &self.home).unwrap()
            }
        }
    }

    fn append(path: &Path, bytes: &[u8]) {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    fn now_ms() -> i64 {
        Local::now().timestamp_millis()
    }

    fn today() -> String {
        Local::now().date_naive().format("%Y-%m-%d").to_string()
    }

    /// (requests, hit, miss, output) of `model` on `day`.
    fn counts(projection: &Value, day: &str, model: &str) -> Option<(u64, u64, u64, u64)> {
        let day = projection["days"]
            .as_array()?
            .iter()
            .find(|d| d["day"] == day)?;
        let m = day["models"]
            .as_array()?
            .iter()
            .find(|m| m["model"] == model)?;
        Some((
            m["requests"].as_u64()?,
            m["cache_hit"].as_u64()?,
            m["cache_miss"].as_u64()?,
            m["output"].as_u64()?,
        ))
    }

    #[test]
    fn counts_requests_and_tokens_per_model_and_day_and_reads_only_appended_frames() {
        let f = fixture();
        let log = f.log("--w-example--", "session-a", "session.v4.jsonl.zstd");
        append(&log, &frame(&header(false)));
        append(
            &log,
            &frame(
                &(message(now_ms(), "deepseek-flash", 100, 10, 5)
                    + &message(now_ms(), "deepseek-v4-pro", 7, 3, 1)),
            ),
        );
        let first = f.refresh();
        assert_eq!(first["today"], today());
        assert_eq!(first["partial"], false);
        assert_eq!(
            counts(&first, &today(), "deepseek-flash"),
            Some((1, 100, 10, 5))
        );
        assert_eq!(
            counts(&first, &today(), "deepseek-v4-pro"),
            Some((1, 7, 3, 1))
        );
        // An unchanged log is not read again and nothing is counted twice.
        assert_eq!(f.refresh()["days"], first["days"]);
        append(&log, &frame(&message(now_ms(), "deepseek-flash", 50, 0, 2)));
        assert_eq!(
            counts(&f.refresh(), &today(), "deepseek-flash"),
            Some((2, 150, 10, 7))
        );
        let index: Index = super::super::deepseek::read_private(
            &super::super::deepseek::state_path(&f.config, "account-usage", "native-dsh", &f.home),
            INDEX_BYTES,
        )
        .unwrap();
        let entry = &index.files["--w-example--/session-a/session.v4.jsonl.zstd"];
        assert_eq!(
            (entry.offset, entry.done),
            (fs::metadata(&log).unwrap().len(), true)
        );
        // The index holds counts only: no record content.
        let raw = fs::read_to_string(super::super::deepseek::state_path(
            &f.config,
            "account-usage",
            "native-dsh",
            &f.home,
        ))
        .unwrap();
        assert!(!raw.contains("assistant/message") && !raw.contains("inputTokens"));
    }

    #[test]
    fn a_torn_final_frame_and_a_frame_ending_inside_a_line_wait_for_completion() {
        let f = fixture();
        let log = f.log("p", "s", "session.v4.jsonl.zstd");
        append(&log, &frame(&header(false)));
        let line = message(now_ms(), "deepseek-flash", 1, 1, 1);
        let (left, right) = line.split_at(40);
        append(
            &log,
            &frame(&(message(now_ms(), "deepseek-flash", 1, 1, 1) + left)),
        );
        let torn = frame(&(right.to_owned() + &message(now_ms(), "deepseek-flash", 1, 1, 1)));
        append(&log, &torn[..torn.len() - 3]);
        let waiting = f.refresh();
        assert_eq!(
            counts(&waiting, &today(), "deepseek-flash"),
            None,
            "no whole line is committed yet"
        );
        assert_eq!(waiting["partial"], true);
        append(&log, &torn[torn.len() - 3..]);
        assert_eq!(
            counts(&f.refresh(), &today(), "deepseek-flash"),
            Some((3, 3, 3, 3))
        );
    }

    #[test]
    fn a_fork_counts_only_after_its_inherited_prefix() {
        let f = fixture();
        let log = f.log("p", "fork", "session.v3.jsonl.zstd");
        append(&log, &frame(&header(true)));
        let inherited = message(now_ms(), "deepseek-flash", 1000, 100, 10);
        append(
            &log,
            &frame(
                &(inherited.clone()
                    + "{\"type\":\"session/end-seed\",\"seq\":2,\"time\":0,\"data\":{}}\n"
                    + &inherited),
            ),
        );
        // A refresh between the prefix's frames counts it until the cut arrives.
        let before_cut = f.refresh();
        assert_eq!(
            counts(&before_cut, &today(), "deepseek-flash"),
            Some((2, 2000, 200, 20))
        );
        append(&log, &frame("{\"type\":\"session/end-seed\",\"seq\":3,\"time\":0,\"data\":{\"inherited\":true}}\n"));
        append(&log, &frame(&message(now_ms(), "deepseek-flash", 5, 4, 3)));
        assert_eq!(
            counts(&f.refresh(), &today(), "deepseek-flash"),
            Some((1, 5, 4, 3))
        );
    }

    #[test]
    fn only_the_current_generation_counts_and_rewritten_or_removed_logs_are_redone() {
        let f = fixture();
        let old = f.log("p", "s", "session.v3.jsonl.zstd");
        let current = f.log("p", "s", "session.v4.jsonl.zstd");
        for log in [&old, &current] {
            append(log, &frame(&header(false)));
            append(log, &frame(&message(now_ms(), "deepseek-flash", 1, 1, 1)));
        }
        let noncanonical = f.log("p", "s", "session.v04.jsonl.zstd");
        append(
            &noncanonical,
            &frame(&message(now_ms(), "deepseek-flash", 9, 9, 9)),
        );
        assert_eq!(
            counts(&f.refresh(), &today(), "deepseek-flash"),
            Some((1, 1, 1, 1))
        );
        // Replaced by a different log: read again from the start.
        let replacement = f.log("p", "s", "session.v4.jsonl.zstd.new");
        append(&replacement, &frame(&header(true)));
        append(
            &replacement,
            &frame(
                &(message(now_ms(), "deepseek-flash", 2, 2, 2)
                    + &message(now_ms(), "deepseek-flash", 2, 2, 2)),
            ),
        );
        fs::rename(&replacement, &current).unwrap();
        assert_eq!(
            counts(&f.refresh(), &today(), "deepseek-flash"),
            Some((2, 4, 4, 4))
        );
        fs::remove_dir_all(f.home.join("sessions/p/s")).unwrap();
        assert_eq!(f.refresh()["days"], json!([]));
    }

    #[test]
    fn logs_and_records_before_the_window_are_skipped() {
        let f = fixture();
        let log = f.log("p", "s", "session.v4.jsonl.zstd");
        append(&log, &frame(&header(false)));
        let old = now_ms() - 40 * 86_400_000;
        append(
            &log,
            &frame(
                &(message(old, "deepseek-flash", 1, 1, 1)
                    + &message(now_ms(), "deepseek-flash", 2, 2, 2)),
            ),
        );
        let projection = f.refresh();
        assert_eq!(projection["days"].as_array().unwrap().len(), 1);
        assert_eq!(
            counts(&projection, &today(), "deepseek-flash"),
            Some((1, 2, 2, 2))
        );
        let stale = f.log("p", "t", "session.v4.jsonl.zstd");
        append(
            &stale,
            &frame(&(header(false) + &message(old, "deepseek-flash", 1, 1, 1))),
        );
        fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(40 * 86_400))
            .unwrap();
        f.refresh();
        let index: Index = super::super::deepseek::read_private(
            &super::super::deepseek::state_path(&f.config, "account-usage", "native-dsh", &f.home),
            INDEX_BYTES,
        )
        .unwrap();
        assert!(
            !index.files.contains_key("p/t/session.v4.jsonl.zstd"),
            "a log untouched in the window is not indexed"
        );
    }

    #[test]
    fn a_malformed_log_is_set_aside_and_reported_until_it_changes() {
        let f = fixture();
        let log = f.log("p", "s", "session.v4.jsonl.zstd");
        append(&log, b"not a zstd frame at all");
        let projection = f.refresh();
        // Not "still reading": nothing more will come. But the totals are incomplete.
        assert_eq!(
            (
                &projection["partial"],
                &projection["reading"],
                &projection["unreadable"],
                &projection["days"]
            ),
            (&json!(true), &json!(false), &json!(1), &json!([]))
        );
        assert_eq!(public(&projection).unwrap()["unreadable"], 1);
        // Replaced by a valid log: read again, and no longer reported.
        fs::remove_file(&log).unwrap();
        append(
            &log,
            &frame(&(header(false) + &message(now_ms(), "deepseek-flash", 1, 1, 1))),
        );
        let fixed = f.refresh();
        assert_eq!(
            (&fixed["partial"], &fixed["unreadable"]),
            (&json!(false), &json!(0))
        );
        assert_eq!(
            counts(&fixed, &today(), "deepseek-flash"),
            Some((1, 1, 1, 1))
        );
    }

    #[test]
    fn a_log_that_turns_malformed_keeps_what_was_read_and_is_reported() {
        let f = fixture();
        let log = f.log("p", "s", "session.v4.jsonl.zstd");
        append(
            &log,
            &frame(&(header(false) + &message(now_ms(), "deepseek-flash", 1, 1, 1))),
        );
        assert_eq!(f.refresh()["unreadable"], 0);
        let mut bad = frame(&message(now_ms(), "deepseek-flash", 9, 9, 9));
        let last = bad.len() - 1;
        bad[last] ^= 0xff; // a wrong content checksum
        append(&log, &bad);
        let projection = f.refresh();
        assert_eq!(
            (&projection["partial"], &projection["unreadable"]),
            (&json!(true), &json!(1))
        );
        assert_eq!(
            counts(&projection, &today(), "deepseek-flash"),
            Some((1, 1, 1, 1))
        );
    }

    #[test]
    fn identity_head_reads_share_the_refresh_budget() {
        let f = fixture();
        let older = f.log("p", "older", "session.v4.jsonl.zstd");
        append(
            &older,
            &frame(&(header(false) + &message(now_ms(), "older-model", 1, 1, 1))),
        );
        fs::File::options()
            .write(true)
            .open(&older)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(3600))
            .unwrap();
        let newer = f.log("p", "newer", "session.v4.jsonl.zstd");
        append(
            &newer,
            &frame(&(header(false) + &message(now_ms(), "newer-model", 1, 1, 1))),
        );
        let path =
            super::super::deepseek::state_path(&f.config, "account-usage", "native-dsh", &f.home);
        let index = || super::super::deepseek::read_private::<Index>(&path, INDEX_BYTES).unwrap();
        // Exactly the newest log's identity check: no bytes remain for the older
        // log's check or for reading, so the refresh stops there.
        let budget = fs::metadata(&newer).unwrap().len();
        let first = refresh_within(&f.config, "native-dsh", &f.home, budget).unwrap();
        assert_eq!(
            (&first["partial"], &first["reading"], &first["days"]),
            (&json!(true), &json!(true), &json!([]))
        );
        assert_eq!(
            index().files.keys().collect::<Vec<_>>(),
            ["p/newer/session.v4.jsonl.zstd"]
        );
        let complete = refresh_within(&f.config, "native-dsh", &f.home, READ_BYTES).unwrap();
        assert_eq!(
            (&complete["partial"], &complete["reading"]),
            (&json!(false), &json!(false))
        );
        assert_eq!(
            counts(&complete, &today(), "older-model"),
            Some((1, 1, 1, 1))
        );
        assert_eq!(
            counts(&complete, &today(), "newer-model"),
            Some((1, 1, 1, 1))
        );
        // A grown log whose check is deferred keeps its previous counts and says so.
        append(&newer, &frame(&message(now_ms(), "newer-model", 1, 1, 1)));
        let deferred = refresh_within(&f.config, "native-dsh", &f.home, 0).unwrap();
        assert_eq!(deferred["reading"], true);
        assert_eq!(
            counts(&deferred, &today(), "newer-model"),
            Some((1, 1, 1, 1))
        );
        assert_eq!(
            counts(&f.refresh(), &today(), "newer-model"),
            Some((2, 2, 2, 2))
        );
    }

    #[test]
    fn a_refresh_keeps_to_its_byte_budget_newest_log_first() {
        let f = fixture();
        let older = f.log("p", "older", "session.v4.jsonl.zstd");
        append(
            &older,
            &frame(&(header(false) + &message(now_ms(), "older-model", 1, 1, 1))),
        );
        fs::File::options()
            .write(true)
            .open(&older)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(3600))
            .unwrap();
        let newer = f.log("p", "newer", "session.v4.jsonl.zstd");
        append(
            &newer,
            &frame(&(header(false) + &message(now_ms(), "newer-model", 1, 1, 1))),
        );
        // Both identity checks (each log is shorter than HEAD_BYTES), then the newest log's frames.
        let (older_len, newer_len) = (
            fs::metadata(&older).unwrap().len(),
            fs::metadata(&newer).unwrap().len(),
        );
        let budget = older_len + 2 * newer_len;
        let first = refresh_within(&f.config, "native-dsh", &f.home, budget).unwrap();
        assert_eq!(first["partial"], true);
        assert!(counts(&first, &today(), "newer-model").is_some());
        assert!(counts(&first, &today(), "older-model").is_none());
        let second = refresh_within(&f.config, "native-dsh", &f.home, budget).unwrap();
        assert_eq!(second["partial"], false);
        assert_eq!(counts(&second, &today(), "older-model"), Some((1, 1, 1, 1)));
        assert_eq!(counts(&second, &today(), "newer-model"), Some((1, 1, 1, 1)));
    }

    #[test]
    fn a_message_that_quotes_the_seed_marker_is_still_counted() {
        let f = fixture();
        let log = f.log("p", "s", "session.v4.jsonl.zstd");
        let quoting = format!(
            "{}\n",
            json!({"type":"assistant/message","seq":1,"time":now_ms(),"data":{
                "message":{"role":"assistant","source":{"kind":"model","provider":"deepseek-official","model":"deepseek-flash"},
                    "content":[{"type":"tool-call","input":{"pattern":"session/end-seed"}}]},
                "usage":{"inputTokens":3,"outputTokens":2,"cacheReadTokens":1,"cacheWriteTokens":0,"totalTokens":6}}})
        );
        assert!(quoting.contains("\"session/end-seed\""));
        append(&log, &frame(&(header(false) + &quoting)));
        assert_eq!(
            counts(&f.refresh(), &today(), "deepseek-flash"),
            Some((1, 1, 3, 2))
        );
    }

    #[test]
    fn an_in_place_rewrite_with_the_same_size_and_head_is_read_again() {
        use std::io::{Seek, SeekFrom};
        let mut f = fixture();
        f.fixed = true;
        let log = f.log("p", "s", "session.v4.jsonl.zstd");
        let first = long_first_frame();
        let (before, after, old, n) = same_length_messages();
        assert_eq!(before.len(), after.len());
        append(&log, &first);
        append(&log, &before);
        assert_eq!(
            counts(&f.refresh(), &fixed_day().to_string(), "deepseek-flash"),
            Some((1, old, old, old))
        );
        let mut file = fs::OpenOptions::new().write(true).open(&log).unwrap();
        file.seek(SeekFrom::Start(first.len() as u64)).unwrap();
        file.write_all(&after).unwrap();
        file.set_modified(SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        drop(file);
        assert_eq!(
            counts(&f.refresh(), &fixed_day().to_string(), "deepseek-flash"),
            Some((1, n, n, n))
        );
    }

    #[test]
    fn a_rewrite_before_the_committed_offset_is_read_again_even_when_the_log_grows() {
        use std::io::{Seek, SeekFrom};
        let mut f = fixture();
        f.fixed = true;
        let log = f.log("p", "s", "session.v4.jsonl.zstd");
        let first = long_first_frame();
        let (before, after, old, n) = same_length_messages();
        assert_eq!(before.len(), after.len());
        append(&log, &first);
        append(&log, &before);
        assert_eq!(
            counts(&f.refresh(), &fixed_day().to_string(), "deepseek-flash"),
            Some((1, old, old, old))
        );
        let mut file = fs::OpenOptions::new().write(true).open(&log).unwrap();
        file.seek(SeekFrom::Start(first.len() as u64)).unwrap();
        file.write_all(&after).unwrap();
        drop(file);
        append(
            &log,
            &frame(&message(fixed_noon(), "deepseek-flash", 1, 1, 1)),
        );
        assert_eq!(
            counts(&f.refresh(), &fixed_day().to_string(), "deepseek-flash"),
            Some((2, n + 1, n + 1, n + 1))
        );
    }

    #[test]
    fn a_busy_day_keeps_its_busiest_models_and_sums_the_rest() {
        let mut index = Index {
            schema: 1,
            ..Index::default()
        };
        let mut entry = Entry::default();
        let models = entry.days.entry("2026-11-10".into()).or_default();
        for i in 0..10u64 {
            models.insert(format!("m{i}"), [i + 1, 1, 1, 1]);
        }
        index.files.insert("k".into(), entry);
        let projection = project(&index, "2026-11-10", "2026-10-11", false);
        let rows = projection["days"][0]["models"].as_array().unwrap();
        assert_eq!(rows.len(), MODELS_PER_DAY);
        assert_eq!(rows[0]["model"], "m9");
        assert_eq!(
            rows[MODELS_PER_DAY - 1],
            json!({"model":"other","requests":1+2+3,"cache_hit":3,"cache_miss":3,"output":3})
        );
        assert_eq!(
            public(&projection).unwrap()["days"][0]["models"]
                .as_array()
                .unwrap()
                .len(),
            MODELS_PER_DAY
        );
    }
}
