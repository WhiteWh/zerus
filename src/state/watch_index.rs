//! Watch-owned transcript progress. CLI inspections never enter this scope.
//!
//! A subscription moves its index to its one inspection worker and gets it
//! back with that worker's result. No process-wide map retains old sessions.
use super::*;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::time::{Duration, Instant};

const LINE_BYTES: usize = 1024 * 1024;
const WINDOW: u64 = 8 * 1024 * 1024;
const ANCHOR: usize = 4096;
const COMPARE: Duration = Duration::from_secs(60);

mod views;
use views::Views;

thread_local! {
    static CURRENT: RefCell<Option<Index>> = const { RefCell::new(None) };
}

/// An append-only reader with bounded incomplete-line storage. The anchors
/// detect an in-place rewrite at the beginning or previous end as well as
/// replacement, shrink and a same-size edit. Full comparison covers the
/// remaining middle-of-file rewrite case.
struct Transcript {
    path: PathBuf,
    identity: (u64, u64),
    stamp: (i64, i64),
    length: u64,
    offset: u64,
    prefix: Vec<u8>,
    anchor: Vec<u8>,
    partial: Vec<u8>,
    skipping: bool,
}

#[derive(Default)]
pub(crate) struct Index {
    scope: String,
    transcript: Option<Transcript>,
    views: Views,
    checked: Option<Instant>,
    pub(crate) read_bytes: u64,
}

impl Index {
    /// Enters only for watch's explicit entry point. Even a panic restores the
    /// worker-local slot; nested canonical inspections can temporarily leave it.
    fn scoped<T>(&mut self, inspect: impl FnOnce() -> T) -> T {
        let prior = CURRENT.with(|current| current.replace(Some(std::mem::take(self))));
        struct Scope<'a> {
            index: &'a mut Index,
            prior: Option<Index>,
        }
        impl Drop for Scope<'_> {
            fn drop(&mut self) {
                *self.index = CURRENT
                    .with(|current| current.replace(self.prior.take()))
                    .unwrap_or_default();
            }
        }
        let _scope = Scope { index: self, prior };
        inspect()
    }

    /// A warm incremental refresh, with the canonical inspection as a bounded
    /// safety comparison every minute. The worker owns both reads.
    pub(crate) fn refresh(
        &mut self,
        name: &str,
        after: i64,
        native: bool,
        inspect: fn(&str, i64, bool) -> Result<(Value, Option<watch_source::Timeline>)>,
    ) -> Result<(Value, Option<watch_source::Timeline>)> {
        if native {
            return inspect(name, after, native);
        }
        let now = Instant::now();
        let compare = self
            .checked
            .is_some_and(|checked| now.duration_since(checked) >= COMPARE);
        if self.checked.is_none() {
            self.checked = Some(now);
        }
        let before = compare.then(|| binding_stamp(name)).flatten();
        let incremental = self.scoped(|| inspect(name, after, native));
        if self.transcript.is_none() {
            self.checked = Some(now);
            return incremental;
        }
        if !compare || incremental.is_err() {
            return incremental;
        }
        self.checked = Some(now);
        let full = inspect(name, after, native);
        match (&incremental, &full) {
            (Ok((incremental, _)), Ok((canonical, _))) => {
                if before.is_some() && before == binding_stamp(name) && self.unchanged() {
                    // Unsent Claude blobs acquire observation time in both
                    // readers; only these known fallback timestamps differ.
                    let mut left = incremental.clone();
                    let mut right = canonical.clone();
                    let clock = self.views.clock_messages();
                    for value in [&mut left, &mut right] {
                        if let Some(messages) = value["provider_messages"].as_array_mut() {
                            for message in messages {
                                if message["message_id"]
                                    .as_str()
                                    .is_some_and(|id| clock.iter().any(|known| known == id))
                                {
                                    message["at"] = Value::Null;
                                }
                            }
                        }
                    }
                    let fields = differences(&left, &right);
                    if !fields.is_empty() {
                        let key = format!("{:x}", Sha256::digest(name.as_bytes()));
                        let line = json!({"at":super::now(),"command":"watch-inspect-drift","key":key,"fields":fields});
                        let _ = slow_calls::append(&slow_calls::log_path(), &line.to_string());
                        // The full scan wins. Rebuild derived state next time;
                        // a mismatch never becomes a persistent fast answer.
                        *self = Self {
                            checked: Some(now),
                            ..Self::default()
                        };
                    }
                }
                full
            }
            _ => incremental,
        }
    }

    fn unchanged(&self) -> bool {
        self.transcript.as_ref().is_some_and(|transcript| {
            crate::platform::FileIdentity::at(&transcript.path).is_ok_and(|(identity, meta)| {
                (identity.device, identity.inode) == transcript.identity
                    && meta.len() == transcript.length
                    && (identity.modified, identity.modified_nanos) == transcript.stamp
            })
        })
    }

    pub(crate) fn follows_transcript(&self) -> bool {
        self.transcript.is_some() && self.views.verified()
    }

    fn prepare(&mut self, record: &Value) -> Result<()> {
        self.prepare_snapshot(record, None)
    }

    fn prepare_snapshot(&mut self, record: &Value, boundary: Option<u64>) -> Result<()> {
        let agent = string(record, "agent");
        let conversation = string(record, "conversation_id");
        let path = PathBuf::from(string(record, "transcript"));
        if !["claude", "codex"].contains(&agent)
            || string(record,"run_id").is_empty()
            || uuid::Uuid::parse_str(conversation).is_err()
            || !path.is_absolute()
            || !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().contains(conversation))
        {
            self.transcript = None;
            return Ok(());
        }
        let scope = scope(record);
        let mut file = File::open(&path).map_err(|error| error.to_string())?;
        let (identity, meta) = crate::platform::FileIdentity::with_metadata(&file)
            .map_err(|error| error.to_string())?;
        let stamp = (identity.modified, identity.modified_nanos);
        let length = boundary.map_or(meta.len(), |boundary| boundary.min(meta.len()));
        let same = self.scope == scope
            && self.transcript.as_ref().is_some_and(|old| {
                old.path == path
                    && old.identity == (identity.device, identity.inode)
                    && length >= old.length
                    && (length != old.length || stamp == old.stamp)
            });
        let same = same
            && self.transcript.as_ref().is_some_and(|old| {
                let prefix = read_at(&mut file, 0, old.prefix.len());
                let anchor = read_at(
                    &mut file,
                    old.length.saturating_sub(old.anchor.len() as u64),
                    old.anchor.len(),
                );
                prefix.is_ok_and(|prefix| prefix == old.prefix)
                    && anchor.is_ok_and(|anchor| anchor == old.anchor)
            });
        let reset = !same;
        if reset {
            self.scope = scope;
            self.views = Views::default();
            self.views.begin(record, length);
            self.transcript = Some(Transcript {
                path,
                identity: (identity.device, identity.inode),
                stamp,
                length: 0,
                offset: 0,
                prefix: Vec::new(),
                anchor: Vec::new(),
                partial: Vec::new(),
                skipping: false,
            });
        }
        if boundary.is_none() && (reset || !self.views.usage_ready()) {
            self.views.seed_usage(
                usage::watch_seed(record, &mut file, identity, length),
                length,
            );
        }
        let transcript = self.transcript.as_mut().unwrap();
        self.read_bytes = 0;
        file.seek(SeekFrom::Start(transcript.offset))
            .map_err(|error| error.to_string())?;
        // Snapshot EOF: bytes appended while reading belong to the next pass.
        let mut reader = BufReader::new((&mut file).take(length - transcript.offset));
        loop {
            let available = reader.fill_buf().map_err(|error| error.to_string())?;
            if available.is_empty() {
                break;
            }
            let count = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |at| at + 1);
            let piece = &available[..count];
            if !transcript.skipping {
                if transcript.partial.len() + piece.len() <= LINE_BYTES {
                    transcript.partial.extend_from_slice(piece);
                } else {
                    transcript.partial.clear();
                    transcript.skipping = true;
                }
            }
            transcript.offset += count as u64;
            self.read_bytes += count as u64;
            if piece.ends_with(b"\n") {
                if !transcript.skipping {
                    let start = transcript.offset - transcript.partial.len() as u64;
                    self.views
                        .observe(record, start, transcript.offset, &transcript.partial)?;
                } else {
                    self.views.oversized(transcript.offset);
                }
                transcript.partial.clear();
                transcript.skipping = false;
            }
            reader.consume(count);
        }
        drop(reader);
        transcript.prefix = read_at(&mut file, 0, length.min(ANCHOR as u64) as usize)?;
        transcript.anchor = read_at(
            &mut file,
            length.saturating_sub(ANCHOR as u64),
            length.min(ANCHOR as u64) as usize,
        )?;
        transcript.length = length;
        transcript.stamp = stamp;
        self.views.finish(record, length);
        Ok(())
    }
}

fn binding_stamp(name: &str) -> Option<[u8; 32]> {
    read(name)
        .ok()
        .and_then(|record| serde_json::to_vec(&record).ok())
        .map(|bytes| Sha256::digest(bytes).into())
}

fn differences(left: &Value, right: &Value) -> Vec<String> {
    let mut keys = std::collections::BTreeSet::new();
    for object in [left.as_object(), right.as_object()].into_iter().flatten() {
        keys.extend(object.keys().cloned());
    }
    keys.into_iter()
        .filter(|key| {
            !["timings_ms", "goal_observed_at"].contains(&key.as_str()) && left[key] != right[key]
        })
        .take(20)
        .collect()
}

fn read_at(file: &mut File, offset: u64, size: usize) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| error.to_string())?;
    let mut bytes = vec![0; size];
    file.read_exact(&mut bytes)
        .map_err(|error| error.to_string())?;
    Ok(bytes)
}

fn scope(record: &Value) -> String {
    json!([
        record["agent"],
        record["conversation_id"],
        record["run_id"],
        record["transcript"],
        record["fork_parent_id"],
        record["created"]
    ])
    .to_string()
}

fn with<R>(record: &Value, read: impl FnOnce(&Index) -> Option<R>) -> Option<R> {
    CURRENT.with(|current| {
        let current = current.borrow();
        let index = current.as_ref()?;
        let transcript = index.transcript.as_ref()?;
        if transcript.path != PathBuf::from(string(record, "transcript"))
            || index.scope != scope(record)
        {
            return None;
        }
        read(index)
    })
}

pub(super) fn usage(record: &Value) -> Option<Value> {
    with(record, |index| {
        index.views.usage(
            record,
            index
                .transcript
                .as_ref()
                .is_some_and(|transcript| !transcript.partial.is_empty() || transcript.skipping),
        )
    })
}

pub(super) fn messages(record: &Value) -> Option<Vec<Value>> {
    with(record, |index| index.views.messages(record))
}

pub(super) fn settings(record: &Value) -> Option<(String, String, u64)> {
    if record["agent"] == "claude"
        && Path::new(string(record, "transcript"))
            .file_name()
            .is_none_or(|name| {
                name.to_string_lossy() != format!("{}.jsonl", string(record, "conversation_id"))
            })
    {
        return None;
    }
    with(record, |index| index.views.settings())
}

pub(super) fn errors(record: &Value) -> Option<(Vec<Value>, f64)> {
    with(record, |index| index.views.errors(record))
}

pub(super) fn questions(
    record: &Value,
    deadline: Option<Instant>,
) -> Option<Result<Option<Value>>> {
    CURRENT.with(|current| {
        let mut current = current.borrow_mut();
        let index = current.as_mut()?;
        if index.transcript.is_none() || index.scope != scope(record) {
            return None;
        }
        if index
            .transcript
            .as_ref()
            .is_some_and(|transcript| std::str::from_utf8(&transcript.partial).is_err())
        {
            return None;
        }
        index.views.questions(record, deadline)
    })
}

/// Called once after the canonical binding was read, before summary phases.
#[cfg(test)]
mod tests;

pub(super) fn prepare(record: &Value) {
    CURRENT.with(|current| {
        if let Some(index) = current.borrow_mut().as_mut() {
            if index.prepare(record).is_err() {
                index.transcript = None;
                index.views = Views::default();
            }
        }
    });
}
