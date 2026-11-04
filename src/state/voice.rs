//! Conversation-owned voice messages. Acceptance commits audio and intent;
//! a separately supervised worker recognizes and delivers without a client.
use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    process::{Command, Stdio},
    time::Duration,
};
#[path = "../../services/mobile/relay/src/voice_video.rs"]
mod video;
#[path = "../../services/mobile/relay/src/voice_rejection.rs"]
mod rejection;
#[path = "voice/acceptance.rs"]
mod acceptance;

/// Voice messages need the speech service, the peer transport and a detached
/// worker, which Phase 2 leaves out on Windows; `dispatch` refuses there.
const UNSUPPORTED: &str =
    "Voice messages are not supported on Windows yet; send them to a Linux or macOS machine.";
#[cfg(unix)]
use crate::speech;
/// The speech calls of voice messages, which Windows builds do not make.
#[cfg(not(unix))]
mod speech {
    use super::{Result, Value, UNSUPPORTED};
    use crate::config::Config;
    use std::time::Duration;
    pub(super) use crate::speech::validate_request;

    pub(super) fn voice_computers(_: &Config) -> Result<Value> {
        Err(UNSUPPORTED.into())
    }
    pub(super) fn voice_route(_: &Config, _: &str) -> Result<String> {
        Err(UNSUPPORTED.into())
    }
    pub(super) fn voice_request(_: &Config, _: &str, _: &Value) -> Result<Value> {
        Err(UNSUPPORTED.into())
    }
    pub(super) fn capture(_: &[String], _: &[u8], _: Duration) -> Result<Value> {
        Err(UNSUPPORTED.into())
    }
}

const MAX_JOBS: i64 = 10_000;
const MAX_AUDIO: i64 = 2 * 1024 * 1024 * 1024;
const TERMINAL: &[&str] = &["submitted", "failed", "cancelled", "uncertain"];
const MAX_AGE: f64 = 24. * 60. * 60.;
const WAVEFORM_BINS: usize = 64;
const WAVEFORM_BATCH: usize = 4;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn uuid(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        uuid::Uuid::parse_str(s).is_ok_and(|id| !id.is_nil() && id.to_string() == s)
    })
}
fn bounded(value: &Value, max: usize, empty: bool) -> bool {
    value.as_str().is_some_and(|s| {
        (empty || !s.is_empty()) && s.len() <= max && !s.chars().any(char::is_control)
    })
}
#[cfg(unix)]
pub(crate) fn validate_request(value: &Value) -> Result<()> {
    prepare_request(value).map(|_| ())
}
fn prepare_request(value: &Value) -> Result<Option<video::Video>> {
    let object = value.as_object().ok_or("Invalid voice request")?;
    let mut fields = vec![
        "action",
        "request_id",
        "expected_run_id",
        "expected_conversation_id",
    ];
    match string(value, "action") {
        "submit" => {
            fields.extend(["speech", "audio"]);
            if object.contains_key("video") { fields.push("video"); }
            if object.contains_key("rejection") || object.contains_key("message_id") { fields.extend(["message_id","rejection"]); }
        },
        "status" | "cancel" => fields.push("message_id"),
        "profiles" => (),
        _ => return Err("Unknown voice action".into()),
    }
    if object.len() != fields.len()
        || fields.iter().any(|key| !object.contains_key(*key))
        || !uuid(&value["request_id"])
        || !bounded(&value["expected_run_id"], 128, false)
        || !bounded(
            &value["expected_conversation_id"],
            256,
            string(value, "action") == "profiles",
        )
    {
        return Err("Invalid voice request fields or exact conversation identity".into());
    }
    if matches!(string(value, "action"), "status" | "cancel") && !uuid(&value["message_id"]) {
        return Err("Invalid voice message UUID".into());
    }
    if string(value, "action") == "submit" {
        if value["speech"].as_object().is_none_or(|o| {
            o.len() != 2 + usize::from(o.contains_key("profile_revision"))
                || !o.contains_key("profile_id")
        }) || !uuid(&value["speech"]["machine_id"])
        {
            return Err("Choose a speech computer and profile".into());
        }
        let mut asr = json!({"action":"submit","request_id":value["request_id"],"profile_id":value["speech"]["profile_id"],"audio":value["audio"]});
        if let Some(revision) = value["speech"].get("profile_revision") {
            asr["profile_revision"] = revision.clone();
        }
        speech::validate_request(&asr).map_err(err)?;
        rejection::validate_retry(value)?;
        if let Some(media) = value.get("video") {
            let pcm = STANDARD.decode(string(&value["audio"], "data_base64")).map_err(err)?;
            return video::parse(media, rejection::message_id(value).as_str().unwrap_or_default(), pcm.len()).map(Some);
        }
    }
    Ok(None)
}

struct Store {
    db: Connection,
}
impl Store {
    fn open_at(directory: &Path) -> Result<Self> {
        private_dir(directory)?;
        // #569: made private again when only too open, else reported.
        if !crate::private_stores::ensure(directory, crate::platform::Private::Directory)
            .map_err(err)?
        {
            return Err("Voice storage must be private to this user".into());
        }
        // SQLite NOFOLLOW rejects symlinks in every path component, including
        // macOS's ordinary /var -> /private/var parent. Resolve the verified
        // directory only; keep refusing a symlink at the database itself.
        let path = crate::platform::canonical(directory)
            .map_err(err)?
            .join("messages.sqlite3");
        if !path.try_exists().map_err(err)? {
            match crate::platform::refuse_link(crate::platform::private_open(
                OpenOptions::new().create_new(true).write(true),
            ))
            .open(&path)
            {
                Ok(file) => drop(file),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(err(error)),
            }
        }
        if !crate::platform::verify_private(&path, crate::platform::Private::File).map_err(err)? {
            return Err("Voice database must be a private regular file".into());
        }
        // Never open/close a raw descriptor for an existing SQLite inode: that
        // would release POSIX locks owned by another connection in this process.
        let mut db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(err)?;
        db.busy_timeout(Duration::from_secs(3)).map_err(err)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;
            CREATE TABLE IF NOT EXISTS messages(id TEXT PRIMARY KEY,digest TEXT NOT NULL,state TEXT NOT NULL,created REAL NOT NULL,revision INTEGER NOT NULL,conversation TEXT NOT NULL,body TEXT NOT NULL,audio BLOB NOT NULL);
            CREATE INDEX IF NOT EXISTS voice_pending ON messages(state,created);
            CREATE INDEX IF NOT EXISTS voice_conversation ON messages(conversation,created);").map_err(err)?;
        let columns = db
            .prepare("PRAGMA table_info(messages)")
            .map_err(err)?
            .query_map([], |r| r.get::<_, String>(1))
            .map_err(err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(err)?;
        if !columns.iter().any(|name| name == "change_seq") {
            db.execute_batch(
                "ALTER TABLE messages ADD COLUMN change_seq INTEGER NOT NULL DEFAULT 0;
                UPDATE messages SET change_seq=rowid;",
            )
            .map_err(err)?;
        }
        db.execute_batch("CREATE TABLE IF NOT EXISTS voice_clock(singleton INTEGER PRIMARY KEY CHECK(singleton=1),value INTEGER NOT NULL);
            INSERT OR IGNORE INTO voice_clock SELECT 1,COALESCE(max(change_seq),0) FROM messages;
            CREATE INDEX IF NOT EXISTS voice_changes ON messages(conversation,change_seq);").map_err(err)?;
        if !columns.iter().any(|name| name == "waveform_version") {
            let tx = db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(err)?;
            // Another client can finish the one-time migration while this
            // opener waits for the writer; recheck under the transaction.
            let migrated: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('messages') WHERE name='waveform_version')",
                [], |row| row.get(0),
            ).map_err(err)?;
            if !migrated {
                tx.execute_batch(
                    "ALTER TABLE messages ADD COLUMN waveform_version INTEGER NOT NULL DEFAULT 0;",
                )
                .map_err(err)?;
            }
            tx.commit().map_err(err)?;
        }
        if !columns.iter().any(|name| name == "video") {
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(err)?;
            let migrated: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('messages') WHERE name='video')", [], |row| row.get(0)).map_err(err)?;
            if !migrated { tx.execute_batch("ALTER TABLE messages ADD COLUMN video BLOB NOT NULL DEFAULT X'';").map_err(err)?; }
            tx.commit().map_err(err)?;
        }
        db.execute_batch("CREATE INDEX IF NOT EXISTS voice_waveform_pending ON messages(id) WHERE waveform_version=0;")
            .map_err(err)?;
        db.execute_batch("CREATE INDEX IF NOT EXISTS voice_native_time ON messages(conversation,json_extract(body,'$.native_submitted_at')) WHERE state='submitted';
            CREATE INDEX IF NOT EXISTS voice_native_provider ON messages(conversation,json_extract(body,'$.native_binding.provider_source'),json_extract(body,'$.native_binding.provider_message_id')) WHERE state='submitted';
            CREATE INDEX IF NOT EXISTS voice_native_journal ON messages(conversation,json_extract(body,'$.native_binding.journal_seq')) WHERE state='submitted';").map_err(err)?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS voice_refusals(id TEXT PRIMARY KEY,digest TEXT NOT NULL,owner TEXT NOT NULL,proof TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS voice_admissions(id TEXT PRIMARY KEY,digest TEXT NOT NULL,message TEXT NOT NULL,reply TEXT NOT NULL);").map_err(err)?;
        Ok(Self { db })
    }
    fn open() -> Result<Self> {
        Self::open_at(&root().join("voice"))
    }
    fn get(&self, id: &str) -> Result<Option<Value>> {
        let body: Option<String> = self
            .db
            .query_row("SELECT body FROM messages WHERE id=?", [id], |r| r.get(0))
            .optional()
            .map_err(err)?;
        body.map(|s| serde_json::from_str(&s).map_err(err))
            .transpose()
    }
    fn save(&self, job: &mut Value) -> Result<bool> {
        let previous = job["revision"].as_i64().ok_or("Invalid voice revision")?;
        job["revision"] = json!(previous + 1);
        let tx = self.db.unchecked_transaction().map_err(err)?;
        let sequence: i64 = tx
            .query_row(
                "UPDATE voice_clock SET value=value+1 WHERE singleton=1 RETURNING value",
                [],
                |r| r.get(0),
            )
            .map_err(err)?;
        let n = tx
            .execute(
                "UPDATE messages SET body=?,state=?,revision=?,change_seq=?,waveform_version=? WHERE id=? AND revision=?",
                params![
                    job.to_string(),
                    string(job, "state"),
                    previous + 1,
                    sequence,
                    waveform_version(job),
                    string(job, "id"),
                    previous
                ],
            )
            .map_err(err)?;
        tx.commit().map_err(err)?;
        Ok(n == 1)
    }
    #[cfg(all(test, unix))]
    fn insert(&mut self, job: &Value, digest: &str, audio: &[u8]) -> Result<Value> {
        self.insert_media(job, digest, audio, &[])
    }
    #[cfg(all(test, unix))]
    fn insert_media(&mut self, job: &Value, digest: &str, audio: &[u8], video: &[u8]) -> Result<Value> {
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(err)?;
        let accepted = Self::insert_media_in(&tx,job,digest,audio,video)?;
        tx.commit().map_err(err)?;
        Ok(accepted)
    }
    fn insert_media_in(tx: &rusqlite::Transaction<'_>, job: &Value, digest: &str, audio: &[u8], video: &[u8]) -> Result<Value> {
        let previous: Option<(String, String)> = tx
            .query_row(
                "SELECT digest,body FROM messages WHERE id=?",
                [string(job, "id")],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(err)?;
        if let Some((old, body)) = previous {
            if old != digest {
                return Err("Voice request ID was already used for different content".into());
            }
            return serde_json::from_str(&body).map_err(err);
        }
        let (count, size): (i64, i64) = tx
            .query_row(
                "SELECT count(*),COALESCE(sum(length(audio)+length(video)),0) FROM messages",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(err)?;
        if count >= MAX_JOBS || size + audio.len() as i64 + video.len() as i64 > MAX_AUDIO {
            return Err("Voice storage is full on this conversation computer".into());
        }
        let pending:i64=tx.query_row("SELECT count(*) FROM messages WHERE state NOT IN ('submitted','failed','cancelled','uncertain')",[],|r|r.get(0)).map_err(err)?;
        if pending >= 32 {
            return Err("Voice processing queue is full on this conversation computer".into());
        }
        let sequence: i64 = tx
            .query_row(
                "UPDATE voice_clock SET value=value+1 WHERE singleton=1 RETURNING value",
                [],
                |r| r.get(0),
            )
            .map_err(err)?;
        tx.execute(
            "INSERT INTO messages(id,digest,state,created,revision,conversation,body,audio,change_seq,waveform_version,video) VALUES(?,?,?,?,?,?,?,?,?,?,?)",
            params![
                string(job, "id"),
                digest,
                string(job, "state"),
                job["created_at"].as_f64(),
                1,
                string(job, "conversation_id"),
                job.to_string(),
                audio,
                sequence,
                waveform_version(job),
                video
            ],
        )
        .map_err(err)?;
        Ok(job.clone())
    }
    fn audio(&self, id: &str) -> Result<Vec<u8>> {
        self.db
            .query_row("SELECT audio FROM messages WHERE id=?", [id], |r| r.get(0))
            .map_err(err)
    }
    fn video(&self, id: &str) -> Result<Vec<u8>> {
        self.db.query_row("SELECT video FROM messages WHERE id=?", [id], |r| r.get(0)).map_err(err)
    }
    fn backfill_waveforms(&self) -> Result<usize> {
        // The partial index bounds every idle pass, including after completion.
        // Read only one accepted WAV at a time (at most 3,840,044 bytes).
        let candidates = self.db.prepare(
            "SELECT id,body,length(audio) FROM messages WHERE waveform_version=0 ORDER BY id LIMIT ?"
        ).map_err(err)?.query_map([WAVEFORM_BATCH], |row| Ok((
            row.get::<_,String>(0)?, row.get::<_,String>(1)?, row.get::<_,usize>(2)?
        ))).map_err(err)?.collect::<std::result::Result<Vec<_>,_>>().map_err(err)?;
        let mut changed = 0;
        for (id, body, bytes) in candidates {
            let mut job: Value = serde_json::from_str(&body).map_err(err)?;
            if !(44..=3_840_044).contains(&bytes) || bytes % 2 != 0 {
                self.db.execute("UPDATE messages SET waveform_version=-1 WHERE id=? AND revision=? AND waveform_version=0",
                    params![id,job["revision"].as_i64()]).map_err(err)?;
                continue;
            }
            let audio = self.audio(&id)?;
            if audio[..44] != wav_header(audio.len() - 44) {
                self.db.execute("UPDATE messages SET waveform_version=-1 WHERE id=? AND revision=? AND waveform_version=0",
                    params![id,job["revision"].as_i64()]).map_err(err)?;
                continue;
            }
            job["audio"]["waveform"] = json!(waveform(&audio[44..]));
            changed += usize::from(self.save(&mut job)?);
        }
        Ok(changed)
    }
    fn jobs(&self, conversation: Option<&str>, pending: bool) -> Result<Vec<Value>> {
        let sql = if pending {
            "SELECT body FROM messages WHERE state NOT IN ('submitted','failed','cancelled','uncertain') OR json_extract(body,'$.asr_cancel_pending')=1 ORDER BY created,id LIMIT 64"
        } else {
            "SELECT body FROM messages WHERE conversation=? ORDER BY created DESC,id DESC LIMIT 100"
        };
        let mut query = self.db.prepare(sql).map_err(err)?;
        let collect = |rows: rusqlite::MappedRows<
            '_,
            fn(&rusqlite::Row<'_>) -> rusqlite::Result<String>,
        >|
         -> Result<Vec<Value>> {
            rows.map(|r| serde_json::from_str(&r.map_err(err)?).map_err(err))
                .collect()
        };
        fn body(row: &rusqlite::Row<'_>) -> rusqlite::Result<String> {
            row.get(0)
        }
        if pending {
            collect(
                query
                    .query_map(
                        [],
                        body as fn(&rusqlite::Row<'_>) -> rusqlite::Result<String>,
                    )
                    .map_err(err)?,
            )
        } else {
            collect(
                query
                    .query_map(
                        [conversation.unwrap_or("")],
                        body as fn(&rusqlite::Row<'_>) -> rusqlite::Result<String>,
                    )
                    .map_err(err)?,
            )
        }
    }
    fn changes(&self, conversation: &str, after: i64) -> Result<(Vec<Value>, i64, bool)> {
        let mut query=self.db.prepare("SELECT body,change_seq FROM messages WHERE conversation=? AND change_seq>? ORDER BY change_seq LIMIT 100").map_err(err)?;
        let rows = query
            .query_map(params![conversation, after], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(err)?;
        let mut next = after;
        let mut jobs = Vec::new();
        let mut bytes = 0;
        for row in rows {
            let (body, sequence) = row.map_err(err)?;
            if bytes + body.len() > 512 * 1024 && !jobs.is_empty() {
                break;
            }
            bytes += body.len();
            next = sequence;
            jobs.push(serde_json::from_str(&body).map_err(err)?);
        }
        let more: bool = self
            .db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE conversation=? AND change_seq>?)",
                params![conversation, next],
                |r| r.get(0),
            )
            .map_err(err)?;
        Ok((jobs, next, !more))
    }
}

fn wav(pcm: &[u8]) -> Vec<u8> {
    let mut bytes = wav_header(pcm.len());
    bytes.reserve(pcm.len());
    bytes.extend(pcm);
    bytes
}
fn wav_header(pcm_bytes: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(44);
    bytes.extend(b"RIFF");
    bytes.extend((pcm_bytes as u32 + 36).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(16000u32.to_le_bytes());
    bytes.extend(32000u32.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((pcm_bytes as u32).to_le_bytes());
    bytes
}
fn waveform(pcm: &[u8]) -> Vec<u8> {
    let samples = pcm.len() / 2;
    if samples == 0 {
        return vec![0; WAVEFORM_BINS];
    }
    let mut rms = [0u32; WAVEFORM_BINS];
    for (index, value) in rms.iter_mut().enumerate() {
        let start = index * samples / WAVEFORM_BINS;
        let end = ((index + 1) * samples / WAVEFORM_BINS).max(start + 1);
        let power: u64 = pcm[start * 2..end * 2]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|sample| {
                let value = i64::from(i16::from_le_bytes([sample[0], sample[1]]));
                (value * value) as u64
            })
            .sum();
        *value = (power / (end - start) as u64).isqrt() as u32;
    }
    let peak = rms.iter().copied().max().unwrap_or(0);
    rms.into_iter()
        .map(|value| (value * 255 + peak / 2).checked_div(peak).unwrap_or(0) as u8)
        .collect()
}
fn waveform_version(job: &Value) -> i64 {
    i64::from(job["audio"]["waveform"].as_array().is_some_and(|bins| {
        bins.len() == WAVEFORM_BINS
            && bins
                .iter()
                .all(|value| value.as_u64().is_some_and(|v| v <= 255))
    }))
}
fn public(job: &Value) -> Value {
    let mut value = json!({});
    for key in [
        "id",
        "run_id",
        "conversation_id",
        "created_at",
        "revision",
        "state",
        "audio",
        "video",
        "transcript",
        "transcript_truncated",
        "error_code",
        "error",
        "native_binding",
    ] {
        if let Some(v) = job.get(key) {
            value[key] = v.clone();
        }
    }
    value
}
fn record(name: &str) -> Result<Value> {
    if opencode::exists(name) {opencode::binding(name)} else if dsh::exists(name) {
        dsh::binding(name)
    } else {
        read(name)
    }
}
fn checked(name: &str, request: &Value) -> Result<Value> {
    let value = record(name)?;
    if value["run_id"] != request["expected_run_id"]
        || value["conversation_id"] != request["expected_conversation_id"]
        || value["state"] == "archived"
    {
        return Err("Voice target changed; refresh the exact conversation".into());
    }
    Ok(value)
}
fn belongs(job: &Value, record: &Value) -> bool {
    !string(record, "conversation_id").is_empty()
        && job["conversation_id"] == record["conversation_id"]
        && job["agent"] == record["agent"]
        && job["agent_home"] == record["agent_home"]
        && job["account_id"] == record["account_id"]
}
fn failure(job: &mut Value, code: &str, message: &str) {
    job["state"] = json!("failed");
    job["error_code"] = json!(code);
    job["error"] = json!(message);
}
fn cancel_job(job: &mut Value) -> bool {
    if TERMINAL.contains(&string(job, "state")) || job["state"] == "delivering" {
        return false;
    }
    job["state"] = json!("cancelled");
    job["asr_cancel_pending"] = json!(job["asr_id"].is_string());
    true
}
pub(super) fn dispatch(args: &[String]) -> Result<i32> {
    if !cfg!(unix) {
        return Err(UNSUPPORTED.into());
    }
    if args == ["worker"] {
        return worker();
    }
    if args.len() != 2 || args[1] != "--json" {
        return Err("usage: hgs voice <session> --json | hgs voice worker".into());
    }
    let mut bytes = Vec::new();
    io::stdin()
        .take((video::MAX_REQUEST_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(err)?;
    if bytes.len() > video::MAX_REQUEST_BYTES {
        return Err("Voice request is too large".into());
    }
    let request: Value = serde_json::from_slice(crate::state::without_bom(&bytes)).map_err(err)?;
    let video = prepare_request(&request)?;
    let target = checked(&args[0], &request)?;
    let config = crate::config::Config::load().map_err(|e| e.message)?;
    if request["action"] == "profiles" {
        let mut profiles = speech::voice_computers(&config)?;
        profiles["features"] = json!(["voice_video","voice_rejection_v1"]);
        println!("{}", profiles);
        return Ok(0);
    }
    let mut store = Store::open()?;
    let job = if request["action"] == "submit" {
        let route_unavailable = speech::voice_route(&config, string(&request["speech"], "machine_id")).is_err();
        let pcm = STANDARD
            .decode(string(&request["audio"], "data_base64"))
            .map_err(err)?;
        let audio = wav(&pcm);
        let id = rejection::message_id(&request).as_str().ok_or("Missing voice message identity")?;
        let mut job = json!({"id":id,"name":args[0],"run_id":target["run_id"],"conversation_id":target["conversation_id"],"agent":target["agent"],"backend":target["backend"],"agent_home":target["agent_home"],"account_id":target["account_id"],
            "created_at":now(),"revision":1,"state":"queued","speech":request["speech"],"attempt":0,
            "audio":{"request_id":id,"index":0,"name":"voice.wav","mime":"audio/wav","bytes":audio.len(),"duration_ms":(pcm.len()*1000/32000).max(1),"waveform":waveform(&pcm)}});
        if let Some(video) = &video { job["video"] = video.metadata.clone(); }
        let accepted = match store.admit(&request,&target,&job,&audio,video.as_ref().map_or(&[], |v|v.bytes.as_slice()),route_unavailable)? {
            acceptance::Admission::Accepted(job) => job,
            acceptance::Admission::Refused(proof) => {
                println!("{}",json!({"schema":1,"rejection":proof}));
                return Ok(0);
            }
        };
        // Failure to start a worker must not hide an accepted durable message.
        if ensure_worker().is_err() {
            eprintln!("hgs: voice message saved; worker startup is pending");
        }
        accepted
    } else {
        let mut job = match store.get(string(&request,"message_id"))? {
            Some(job) => job,
            None => {
                if let Some(proof) = store.refusal(&request,&target)? {
                    println!("{}",json!({"schema":1,"rejection":proof}));
                    return Ok(0);
                }
                return Err("Voice message was not found".into());
            }
        };
        if !belongs(&job, &target) {
            return Err("Voice message belongs to another conversation".into());
        }
        if request["action"] == "cancel" && cancel_job(&mut job) && !store.save(&mut job)? {
            job = store
                .get(string(&request, "message_id"))?
                .ok_or("Voice message was not found")?;
        }
        job
    };
    // Do not expose local target names, account paths, routes or delivery bytes.
    println!("{}", json!({"schema":1,"request_id":request["request_id"],"message":public(&job)}));
    Ok(0)
}

pub(super) fn messages(record: &Value, agent: Option<&str>) -> Vec<Value> {
    if agent.is_some_and(|a| !a.is_empty()) || !root().join("voice/messages.sqlite3").exists() {
        return vec![];
    }
    let jobs = Store::open()
        .and_then(|s| s.jobs(Some(string(record, "conversation_id")), false))
        .unwrap_or_default();
    let mut rows = Vec::new();
    let mut bytes = 0;
    for job in jobs.into_iter().filter(|j| belongs(j, record)) {
        let row = message_row(&job);
        let size = row.to_string().len();
        if bytes + size > 256 * 1024 {
            break;
        }
        bytes += size;
        rows.push(row);
    }
    rows.reverse();
    rows
}
fn message_row(job: &Value) -> Value {
    let mut attachments = vec![job["audio"].clone()];
    if let Some(video) = job.get("video") {
        let mut file = video.clone();
        if let Some(object) = file.as_object_mut() { object.remove("poster_base64"); }
        attachments.push(file);
    }
    json!({"source":"hgs_voice","type":"UserMessage","message_id":job["id"],"request_id":job["id"],"at":job["created_at"],"detail":job.get("transcript").cloned().unwrap_or(json!("")),"voice":public(job),"attachments":attachments})
}
/// Incremental private history source; bool means the current page is complete.
/// Updates receive a fresh global sequence so an already indexed pending row
/// is revisited when recognition, cancellation or native binding changes it.
pub(super) fn history_page(
    record: &Value,
    agent: Option<&str>,
    after: i64,
) -> Result<(Vec<Value>, i64, bool)> {
    if after < 0 {
        return Err("Invalid voice history sequence".into());
    }
    if agent.is_some_and(|a| !a.is_empty()) || !root().join("voice/messages.sqlite3").exists() {
        return Ok((vec![], after, true));
    }
    let (jobs, next, complete) =
        Store::open()?.changes(string(record, "conversation_id"), after)?;
    Ok((
        jobs.iter()
            .filter(|j| belongs(j, record))
            .map(message_row)
            .collect(),
        next,
        complete,
    ))
}
pub(super) fn read_attachment(
    record: &Value,
    agent: Option<&str>,
    message_id: &str,
    index: usize,
) -> Result<Option<Value>> {
    if agent.is_some_and(|a| !a.is_empty()) || !root().join("voice/messages.sqlite3").exists() {
        return Ok(None);
    }
    let store = Store::open()?;
    let Some(job) = store.get(message_id)? else {
        return Ok(None);
    };
    if index > 1 || index == 1 && job.get("video").is_none() || !belongs(&job, record) {
        return Err("This voice recording belongs to another conversation".into());
    }
    let mut result = job[if index == 0 { "audio" } else { "video" }].clone();
    result["data_base64"] = json!(STANDARD.encode(if index == 0 { store.audio(message_id)? } else { store.video(message_id)? }));
    Ok(Some(result))
}
pub(super) fn correlation(record: &Value, message_id: &str) -> Option<Value> {
    let job = Store::open().ok()?.get(message_id).ok()??;
    if !belongs(&job, record) {
        return None;
    }
    Some(correlation_value(&job))
}

fn correlation_value(job: &Value) -> Value {
    json!({"native_request_id":job["id"],"native_payload_sha256":job["native_payload_sha256"],"native_submitted_at":job["native_submitted_at"],"boundary":job["boundary"],"native_receipt_status":job["native_receipt_status"],"native_binding":job["native_binding"],"state":job["state"]})
}

/// Independent from the client preview and history-import cursors. The existing
/// indexed change sequence covers old rows and later native-binding revisions.
pub(super) type ReconciliationRows = Vec<(Value, Value)>;
pub(super) fn reconciliation_page(record: &Value, after: i64) -> Result<(ReconciliationRows, i64, bool)> {
    if !root().join("voice/messages.sqlite3").exists() { return Ok((vec![],after,true)); }
    let (jobs,next,complete) = Store::open()?.changes(string(record,"conversation_id"),after)?;
    Ok((jobs.iter().filter(|job| belongs(job,record)).map(|job| (message_row(job),correlation_value(job))).collect(),next,complete))
}

/// Find potential competing voices across every batch using native time or an
/// exact saved binding. Over-budget metadata refuses correlation, never guesses.
pub(super) fn reconciliation_candidates(record: &Value, native: &Value) -> Result<Option<Vec<(Value,Value)>>> {
    let store = Store::open()?;
    let conversation = string(record,"conversation_id");
    let mut bodies = std::collections::BTreeMap::new();
    let mut bytes = 0usize;
    let mut collect = |query: &str, values: &[&dyn rusqlite::ToSql]| -> Result<bool> {
        let mut statement = store.db.prepare(query).map_err(err)?;
        let rows = statement.query_map(values,|row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))).map_err(err)?;
        for row in rows {
            let (id,body) = row.map_err(err)?;
            if bodies.contains_key(&id) { continue; }
            bytes += body.len();
            if bodies.len() >= 64 || bytes > 512*1024 { return Ok(false); }
            bodies.insert(id,body);
        }
        Ok(true)
    };
    let at = native["at"].as_f64().unwrap_or(0.);
    if !collect("SELECT id,body FROM messages WHERE state='submitted' AND conversation=? AND json_extract(body,'$.native_submitted_at') BETWEEN ? AND ? LIMIT 65",
        &[&conversation,&(at-30.),&(at+1.)])? { return Ok(None); }
    if !string(native,"message_id").is_empty() && !collect(
        "SELECT id,body FROM messages WHERE state='submitted' AND conversation=? AND json_extract(body,'$.native_binding.provider_source')=? AND json_extract(body,'$.native_binding.provider_message_id')=? LIMIT 65",
        &[&conversation,&string(native,"source"),&string(native,"message_id")])? { return Ok(None); }
    let mut sequences = Vec::new();
    if let Some(seq) = native["seq"].as_i64() { sequences.push(seq); }
    for alias in native["original_ids"].as_array().into_iter().flatten().filter_map(Value::as_str).take(8) {
        if alias.starts_with("journal:") {
            if let Some(seq) = alias.rsplit_once(":native:").and_then(|(_,seq)|seq.parse::<i64>().ok()) { sequences.push(seq); }
        }
    }
    sequences.sort_unstable(); sequences.dedup();
    for seq in sequences {
        if !collect("SELECT id,body FROM messages WHERE state='submitted' AND conversation=? AND json_extract(body,'$.native_binding.journal_seq')=? LIMIT 65",
            &[&conversation,&seq])? { return Ok(None); }
    }
    let mut candidates = Vec::new();
    for body in bodies.into_values() {
        let job:Value = serde_json::from_str(&body).map_err(err)?;
        if belongs(&job,record) { candidates.push((message_row(&job),correlation_value(&job))); }
    }
    Ok(Some(candidates))
}

pub(super) fn bind_native(record: &Value, message_id: &str, binding: &Value) -> Result<bool> {
    let object = binding.as_object().ok_or("Invalid native voice binding")?;
    if object.is_empty()
        || object.keys().any(|k| {
            !matches!(
                k.as_str(),
                "provider_message_id"
                    | "provider_source_offset"
                    | "provider_source"
                    | "journal_seq"
            )
        })
        || object.iter().any(|(k, v)| {
            if k == "journal_seq" {
                v.as_i64().is_none_or(|v| v < 1)
            } else {
                !bounded(v, 512, false)
            }
        })
    {
        return Err("Invalid native voice provenance".into());
    }
    let store = Store::open()?;
    let Some(mut job) = store.get(message_id)? else {
        return Ok(false);
    };
    if !belongs(&job, record) || job["state"] != "submitted" {
        return Ok(false);
    }
    if job["native_binding"].is_object() {
        if job["native_binding"] == *binding {
            return Ok(true);
        }
        if job["native_binding"]
            .as_object()
            .unwrap()
            .iter()
            .any(|(key, value)| binding.get(key) != Some(value))
        {
            return Ok(false);
        }
    }
    job["native_binding"] = binding.clone();
    store.save(&mut job)
}

fn current(job: &Value) -> Result<Value> {
    let matches = |r: &Value| belongs(job, r) && r["run_id"] == job["run_id"];
    if let Ok(r) = record(string(job, "name")) {
        if matches(&r) {
            return Ok(r);
        }
    }
    let mut candidates = records()?.into_iter().filter(matches).collect::<Vec<_>>();
    // DSH bindings are separate; inspect only private bounded manifests.
    for entry in ["dsh/bindings","opencode/bindings"].into_iter().flat_map(|directory| fs::read_dir(root().join(directory)))
        .flatten()
        .flatten()
        .take(10000)
    {
        if let Ok(file) = File::open(entry.path()) {
            let mut bytes = Vec::new();
            if file.take(1_048_577).read_to_end(&mut bytes).is_ok() && bytes.len() <= 1_048_576 {
                if let Ok(r) = serde_json::from_slice::<Value>(&bytes) {
                    if matches(&r) {
                        candidates.push(r);
                    }
                }
            }
        }
    }
    if candidates.len() == 1 {
        Ok(candidates.remove(0))
    } else {
        Err("The original voice conversation is no longer current".into())
    }
}
fn boundary(record: &Value) -> Value {
    let seq = journal::event_db()
        .and_then(|db| {
            db.query_row("SELECT COALESCE(max(seq),0) FROM events", [], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(err)
        })
        .ok();
    let path = string(record, "transcript");
    let source = crate::platform::FileIdentity::at(Path::new(path)).ok();
    json!({"journal_seq":seq,"transcript":path,"source_bytes":source.as_ref().map(|(_, m)|m.len()),"source_device":source.as_ref().map(|(i, _)|i.device),"source_inode":source.as_ref().map(|(i, _)|i.inode),"at":now()})
}
fn receipt(job: &Value) -> Result<Option<Value>> {
    let directory = if job["backend"]=="opencode" || job["agent"]=="opencode" {"opencode/receipts"} else if job["backend"] == "dsh" || job["agent"] == "dsh" {
        "dsh/receipts"
    } else {
        "input_receipts"
    };
    let path = root()
        .join(directory)
        .join(format!("{}.json", string(job, "id")));
    let file = match crate::platform::refuse_link(OpenOptions::new().read(true)).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(err(e)),
    };
    let mut bytes = Vec::new();
    file.take(131073).read_to_end(&mut bytes).map_err(err)?;
    if bytes.len() > 131072 {
        return Err("Native voice receipt is oversized".into());
    }
    let mut value: Value = serde_json::from_slice(&bytes).map_err(err)?;
    if value["request_id"] != job["id"]
        || value["run_id"] != job["run_id"]
        || value["conversation_id"] != job["conversation_id"]
        || value["text"] != job["transcript"]
    {
        return Err("Native request UUID conflicts with this voice message".into());
    }
    if job["agent"] == "opencode" || job["backend"] == "opencode" {
        let record = current(job)?;
        reconciled_opencode_receipt(&mut value, |value| {
            opencode::reconcile_receipt(&record, value)
        })?;
    }
    Ok(Some(value))
}
/// A definite refusal or explicit native delivery uncertainty is an answer
/// for settle. Unknown failures and identity errors still propagate.
fn reconciled_opencode_receipt(
    value: &mut Value,
    reconcile: impl FnOnce(&mut Value) -> Result<bool>,
) -> Result<()> {
    if let Err(error) = reconcile(value) {
        // These are the pinned native adapter's uncertainty classifications,
        // not arbitrary errors containing "uncertain". A pending receipt is
        // retained after an ambiguous POST/DELETE; it is never retry authority.
        let uncertain = matches!(string(value, "status"), "pending" | "uncertain")
            && matches!(error.as_str(),
                "Native delivery state is uncertain"
                | "Delivery uncertain; this request will not be replayed. Inspect native history before creating a new request");
        if value["status"] != "refused" && !uncertain {
            return Err(error);
        }
    }
    Ok(())
}
fn settle(job: &mut Value, receipt: Option<&Value>, interrupted: bool) {
    if let Some(receipt) = receipt {
        job["native_receipt_status"] = receipt["status"].clone();
        job["native_submitted_at"] = receipt["submitted_at"].clone();
        if receipt["status"] == "submitted" {
            job["state"] = json!("submitted");
            // OpenCode's message ID is the exact native row of this message.
            let id = string(receipt, "native_message_id");
            if (job["agent"] == "opencode" || job["backend"] == "opencode")
                && opencode::native_id(id, "msg_")
                && !job["native_binding"].is_object()
            {
                job["native_binding"] =
                    json!({"provider_message_id": id, "provider_source": "opencode_native"});
            }
            return;
        }
        // OpenCode records a definite 4xx refusal; it was never admitted.
        if receipt["status"] == "refused" {
            let reason = receipt["reason"]
                .as_str()
                .filter(|reason| !reason.is_empty())
                .unwrap_or("OpenCode refused this message");
            failure(job, "delivery_refused", reason);
            return;
        }
    }
    if receipt.is_some() || interrupted {
        job["state"] = json!("uncertain");
        job["error_code"] = json!("delivery_uncertain");
        job["error"]=json!("Delivery may have reached the agent. Inspect the conversation before sending anything again.");
    } else {
        job["state"] = json!("waiting_agent");
        job["error_code"] = json!("agent_not_ready");
        job["error"]=json!("Waiting for the original agent to accept input. Existing native drafts and permission dialogs are preserved.");
    }
}

fn speech_result(job: &mut Value, value: &Value) -> Result<()> {
    if value["schema"] != 1 || value["job_id"] != job["asr_id"] {
        return Err("Invalid speech result identity".into());
    }
    match string(value, "state") {
        "queued" | "running" => (),
        "completed" => {
            let text = string(value, "text").trim();
            let mut end = text.len().min(32000);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            job["transcript"] = json!(&text[..end]);
            if end < text.len() {
                job["transcript_truncated"] = json!(true);
            }
            if text.is_empty() {
                failure(
                    job,
                    "no_speech",
                    "No speech was recognized. The original recording is available.",
                );
            } else if text.len() > 32000
                || text.starts_with(['/', '!'])
                || text
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t')
            {
                failure(
                    job,
                    "invalid_transcript",
                    "Recognition returned text that cannot be sent safely to the native agent.",
                );
            } else {
                job["transcript"] = json!(text);
                job["state"] = json!("ready");
                job.as_object_mut().unwrap().remove("error");
                job.as_object_mut().unwrap().remove("error_code");
            }
        }
        "failed" | "uncertain"
            if matches!(
                string(value, "error_code"),
                "service_restarted" | "result_expired"
            ) && job["attempt"].as_u64().unwrap_or(0) < 3 =>
        {
            job["state"] = json!("queued");
        }
        "failed" | "uncertain" | "cancelled" => {
            failure(
                job,
                if string(value, "error_code").is_empty() {
                    "recognition_failed"
                } else {
                    string(value, "error_code")
                },
                "Speech recognition failed. The original recording is available.",
            );
        }
        _ => return Err("Invalid speech job state".into()),
    }
    Ok(())
}
fn speech_unavailable(store: &Store, job: &mut Value) -> Result<()> {
    if job["error_code"] != "speech_unavailable" {
        job["error_code"] = json!("speech_unavailable");
        job["error"]=json!("The selected speech computer or service is unavailable. Recognition will retry automatically; the recording is preserved.");
        store.save(job)?;
    }
    Ok(())
}
fn step(store: &Store, config: &crate::config::Config, mut job: Value) -> Result<()> {
    if job["state"] == "cancelled" {
        if job["asr_cancel_pending"] == true {
            let attempts = job["asr_cancel_attempts"].as_u64().unwrap_or(0) + 1;
            let cancelled = speech::voice_request(
                config,
                string(&job["speech"], "machine_id"),
                &json!({"action":"cancel","job_id":job["asr_id"]}),
            )
            .is_ok();
            job["asr_cancel_pending"] = json!(!cancelled && attempts < 3);
            job["asr_cancel_attempts"] = json!(attempts);
            store.save(&mut job)?;
        }
        return Ok(());
    }
    if job["state"] == "delivering" {
        match receipt(&job) {
            Ok(r) => settle(&mut job, r.as_ref(), true),
            Err(_) => failure(
                &mut job,
                "receipt_conflict",
                "Native delivery identity could not be verified.",
            ),
        }
        store.save(&mut job)?;
        return Ok(());
    }
    if now() - job["created_at"].as_f64().unwrap_or(0.) > MAX_AGE {
        failure(&mut job,"processing_expired","Voice processing could not finish within 24 hours. The original recording is available.");
        store.save(&mut job)?;
        return Ok(());
    }
    let target = match current(&job) {
        Ok(r) => r,
        Err(_) => {
            failure(
                &mut job,
                "target_changed",
                "The original conversation or agent run changed. The recording was not sent.",
            );
            store.save(&mut job)?;
            return Ok(());
        }
    };
    if job["state"] == "queued" {
        job["attempt"] = json!(job["attempt"].as_u64().unwrap_or(0) + 1);
        job["asr_id"] = json!(uuid::Uuid::new_v4().to_string());
        job["asr_submitted"] = json!(false);
        job["state"] = json!("transcribing");
        if !store.save(&mut job)? {
            return Ok(());
        }
    }
    if job["state"] == "transcribing" {
        let machine = string(&job["speech"], "machine_id").to_owned();
        // Always recover by status first. Not-found proves no accepted ASR
        // receipt, so resending this same attempt UUID cannot duplicate work.
        let status = speech::voice_request(
            config,
            &machine,
            &json!({"action":"status","job_id":job["asr_id"]}),
        );
        let mut response = match status {
            Ok(v) => v,
            Err(_) => return speech_unavailable(store, &mut job),
        };
        if response["error_code"] == "not_found" {
            let audio = store.audio(string(&job, "id"))?;
            if audio.len() < 44 {
                return Err("Stored voice audio is invalid".into());
            }
            if !store
                .get(string(&job, "id"))?
                .is_some_and(|j| j["revision"] == job["revision"] && j["state"] == "transcribing")
            {
                return Ok(());
            }
            let mut asr = json!({"action":"submit","request_id":job["asr_id"],"profile_id":job["speech"]["profile_id"],"audio":{"format":"pcm_s16le","sample_rate":16000,"channels":1,"data_base64":STANDARD.encode(&audio[44..])}});
            if let Some(revision) = job["speech"].get("profile_revision") {
                asr["profile_revision"] = revision.clone();
            }
            response = match speech::voice_request(config, &machine, &asr) {
                Ok(v) => v,
                Err(_) => return speech_unavailable(store, &mut job),
            };
            // Submit receipts deliberately omit completed text, even on replay.
            if response["state"] == "completed" {
                return Ok(());
            }
        }
        let previous = job.clone();
        if job["error_code"] == "speech_unavailable" {
            job.as_object_mut().unwrap().remove("error_code");
            job.as_object_mut().unwrap().remove("error");
        }
        speech_result(&mut job, &response)?;
        if job != previous {
            store.save(&mut job)?;
        }
        return Ok(());
    }
    if !matches!(string(&job, "state"), "ready" | "waiting_agent") {
        return Ok(());
    }
    job["name"] = target["name"].clone();
    let payload=json!({"request_id":job["id"],"expected_run_id":job["run_id"],"expected_conversation_id":job["conversation_id"],"text":job["transcript"]}).to_string();
    job["native_payload"] = json!(payload);
    job["native_payload_sha256"] = json!(format!("{:x}", Sha256::digest(payload.as_bytes())));
    job["boundary"] = boundary(&target);
    job["state"] = json!("delivering");
    job.as_object_mut().unwrap().remove("error");
    job.as_object_mut().unwrap().remove("error_code");
    if !store.save(&mut job)? {
        return Ok(());
    }
    // A crash after this marker never causes native delivery to replay. The
    // existing send path owns identity, cold-cache consent and PTY guards.
    let result = if ["dsh","opencode"].contains(&string(&target,"backend")) || ["dsh","opencode"].contains(&string(&target,"agent")) {
        speech::capture(
            &[
                "send".into(),
                string(&target, "name").into(),
                "--json".into(),
            ],
            payload.as_bytes(),
            Duration::from_secs(35),
        )
    } else {
        input::submit(string(&target, "name"), payload.as_bytes(), None)
    };
    let received = receipt(&job);
    match received {
        Ok(r) => {
            settle(&mut job, r.as_ref(), result.is_ok() && r.is_none());
            if job["state"] == "waiting_agent" {
                if let Err(error) = &result {
                    let (code, message) = native_refusal(error);
                    job["error_code"] = json!(code);
                    job["error"] = json!(message);
                }
            }
        }
        Err(_) => failure(
            &mut job,
            "receipt_conflict",
            "Native delivery identity could not be verified.",
        ),
    }
    store.save(&mut job)?;
    Ok(())
}
fn native_refusal(error: &str) -> (&'static str, &'static str) {
    const NOT_READY: (&str, &str) = (
        "agent_not_ready",
        "The original agent could not accept this message yet. Check Activity or Terminal; the recording is preserved.",
    );
    // The guarded busy refusal is a readiness wait even when its detail
    // mentions a draft, input mode or a choice. Receipts govern replay elsewhere.
    if error.starts_with("agent busy:") {
        NOT_READY
    } else if error.contains("previous input has not been confirmed") {
        ("previous_input_pending","Waiting for the agent to confirm its previous input. Check the conversation or Terminal.")
    } else if error.contains("draft, dialog")
        || error.contains("input mode")
        || error.contains("input terminal")
    {
        ("native_input_blocked","The native Terminal has a draft, dialog or unrecognized prompt. Return it to an empty agent prompt to continue.")
    } else if error.contains("waiting for")
        || error.contains("approval")
        || error.contains("choice")
    {
        ("native_choice_pending","The agent needs a choice or approval in Activity or Terminal before this voice message can continue.")
    } else if error.contains("not running") || error.contains("resume it") {
        (
            "agent_stopped",
            "The original agent is stopped. Voice delivery does not resume agents automatically.",
        )
    } else {
        NOT_READY
    }
}
/// The installed service runs `hgs voice worker` under this unit or label.
const SERVICE: service_worker::Service = service_worker::Service {
    unit: "hgs-voice.service",
    label: "com.hgdev.hgs-voice",
};
/// Held by the worker that processes voice messages.
fn worker_lock() -> PathBuf {
    root().join("voice/worker.lock")
}
/// Held by the service's worker while it waits for the worker lock.
fn standby_lock() -> PathBuf {
    root().join("voice/service.lock")
}
/// Opens a lock file of the worker, never through a symbolic link.
fn open_lock(path: &Path) -> Result<File> {
    crate::platform::refuse_link(crate::platform::private_open(
        OpenOptions::new().create(true).truncate(false).write(true),
    ))
    .open(path)
    .map_err(err)
}
fn worker() -> Result<i32> {
    let store = Store::open()?;
    let executable = std::env::current_exe().map_err(err)?;
    let identity = |path: &Path| crate::platform::file_identity(path).map(|i| (i.device, i.inode));
    let original = identity(&executable);
    let replaced = || identity(&executable) != original;
    // launchd and systemd start their worker again 10 s after it exits, so it
    // waits for a worker a client started; that worker then gives way to it.
    let Some(owner) = service_worker::acquire(
        &worker_lock(),
        &standby_lock(),
        open_lock,
        || SERVICE.is_current(),
        replaced,
        Duration::from_secs(1),
    )?
    else {
        return Ok(0);
    };
    loop {
        if replaced() || owner.yields() {
            return Ok(0);
        }
        let config = crate::config::Config::load().map_err(|e| e.message)?;
        if store.backfill_waveforms().is_err() {
            eprintln!("hgs voice: waveform metadata backfill postponed");
        }
        let mut active = std::collections::HashSet::new();
        for job in store.jobs(None, true)? {
            let key = json!([
                job["agent"],
                job["agent_home"],
                job["account_id"],
                job["conversation_id"]
            ])
            .to_string();
            if !active.insert(key) {
                continue;
            }
            if step(&store, &config, job).is_err() {
                eprintln!("hgs voice: processing postponed; durable message retained");
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}
fn ensure_worker() -> Result<()> {
    #[cfg(target_os = "linux")]
    if home()
        .join(".config/systemd/user/hgs-voice.service")
        .is_file()
        && crate::platform::silent(
            "systemctl",
            &["--user", "start", "--no-block", "hgs-voice.service"],
        )
    {
        return Ok(());
    }
    // A running worker, or the service's worker waiting to take over, needs
    // no detached one.
    if crate::status::locked(&worker_lock()) || service_worker::waiting(&standby_lock()) {
        return Ok(());
    }
    let mut cmd = Command::new(std::env::current_exe().map_err(err)?);
    cmd.args(["voice", "worker"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::platform::spawn_detached(&mut cmd).map_err(err)?;
    Ok(())
}

// Voice messages are a Phase 2 non-goal on Windows.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    const ID: &str = "11111111-1111-4111-8111-111111111111";
    #[test]
    fn video_acceptance_keeps_one_durable_media_identity_and_audio_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private");
        let media = video::parse(&video::tests::request(), ID, 32_000).unwrap();
        let audio = wav(&vec![0; 32_000]);
        let mut original = job();
        original["video"] = media.metadata.clone();
        {
            let mut store = Store::open_at(&path).unwrap();
            store.insert_media(&original, "video-and-audio-digest", &audio, &media.bytes).unwrap();
            assert_eq!(store.insert_media(&original, "video-and-audio-digest", &audio, &media.bytes).unwrap(), original);
            assert!(store.insert_media(&original, "different-video", &audio, &media.bytes).is_err());
        }
        let store = Store::open_at(&path).unwrap();
        assert_eq!(store.audio(ID).unwrap(), audio);
        assert_eq!(store.video(ID).unwrap(), media.bytes);
        let restored = store.get(ID).unwrap().unwrap();
        let public = public(&restored);
        assert_eq!(public["video"], media.metadata);
        assert_eq!(public["audio"]["index"], 0);
        assert!(public.get("data_base64").is_none());
        let history = message_row(&restored);
        assert_eq!(history["message_id"], ID);
        assert_eq!(history["attachments"].as_array().unwrap().len(), 2);
        assert_eq!(history["attachments"][1]["request_id"], ID);
        assert_eq!(history["attachments"][1]["index"], 1);
        assert_eq!(history["voice"]["video"], media.metadata);
    }
    #[test]
    fn video_payload_is_additive_and_does_not_change_the_speech_route() {
        let mut value = request();
        assert!(validate_request(&value).is_ok());
        value["audio"]["data_base64"] = json!(STANDARD.encode(vec![0; 32_000]));
        value["video"] = video::tests::request();
        assert!(validate_request(&value).is_ok());
        value["video"]["duration_ms"] = json!(60_001);
        assert!(validate_request(&value).is_err());
        value["action"] = json!("status");
        assert!(validate_request(&value).is_err());
    }
    #[test]
    fn video_schema_upgrade_preserves_audio_during_concurrent_openers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private");
        let audio = wav(&[1, 0]);
        {
            let mut store = Store::open_at(&path).unwrap();
            store.insert(&job(), "old-audio", &audio).unwrap();
            store.db.execute_batch("ALTER TABLE messages DROP COLUMN video;").unwrap();
        }
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let openers: Vec<_> = (0..2).map(|_| {
            let path = path.clone(); let barrier = barrier.clone();
            std::thread::spawn(move || { barrier.wait(); Store::open_at(&path).unwrap(); })
        }).collect();
        for opener in openers { opener.join().unwrap(); }
        let store = Store::open_at(&path).unwrap();
        assert_eq!(store.audio(ID).unwrap(), audio);
        assert!(store.video(ID).unwrap().is_empty());
        assert_eq!(store.get(ID).unwrap().unwrap(), job());
    }

    #[test]
    fn waveform_is_exact_rms_envelope_and_handles_silence_and_short_audio() {
        assert_eq!(waveform(&[]), vec![0; 64]);
        assert_eq!(waveform(&[0; 256]), vec![0; 64]);
        assert_eq!(waveform(&i16::MIN.to_le_bytes()), vec![255; 64]);
        let mut pcm = Vec::new();
        for index in 1..=64i16 {
            for sample in [index * 100, -index * 100, index * 100, -index * 100] {
                pcm.extend(sample.to_le_bytes());
            }
        }
        let expected: Vec<u8> = (1..=64)
            .map(|value| ((value * 255 + 32) / 64) as u8)
            .collect();
        assert_eq!(waveform(&pcm), expected);
        let mut impulse = vec![0; 128];
        impulse[32..34].copy_from_slice(&i16::MIN.to_le_bytes());
        let bins = waveform(&impulse);
        assert_eq!(bins.iter().filter(|value| **value > 0).count(), 1);
        assert_eq!(bins[16], 255);
    }

    #[test]
    fn waveform_backfill_is_bounded_revisioned_and_skips_malformed_audio() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open_at(&dir.path().join("private")).unwrap();
        let original = wav(&[1, 0, 2, 0]);
        for index in 0..6 {
            let mut value = job();
            value["id"] = json!(format!("00000000-0000-4000-8000-{index:012}"));
            value["state"] = json!("submitted");
            value["transcript"] = json!("Synthetic retained text");
            value["native_payload"] = json!("exact native receipt bytes");
            store
                .insert(
                    &value,
                    "fingerprint",
                    if index == 0 { &[0; 46] } else { &original },
                )
                .unwrap();
        }
        let (_, before, _) = store.changes("conversation", 0).unwrap();
        assert_eq!(store.backfill_waveforms().unwrap(), 3);
        assert_eq!(store.backfill_waveforms().unwrap(), 2);
        assert_eq!(store.backfill_waveforms().unwrap(), 0);
        let (changed, after, complete) = store.changes("conversation", before).unwrap();
        assert!(complete && after > before);
        assert_eq!(changed.len(), 5);
        for value in changed {
            assert_eq!(value["revision"], 2);
            assert_eq!(value["state"], "submitted");
            assert_eq!(value["native_payload"], "exact native receipt bytes");
            assert_eq!(value["audio"]["waveform"], json!(waveform(&original[44..])));
            assert_eq!(store.audio(string(&value, "id")).unwrap(), original);
            let row = message_row(&value);
            assert_eq!(
                row["voice"]["audio"]["waveform"],
                row["attachments"][0]["waveform"]
            );
        }
        let skipped: i64 = store
            .db
            .query_row(
                "SELECT count(*) FROM messages WHERE waveform_version=-1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(skipped, 1);
    }

    #[test]
    fn waveform_schema_upgrade_preserves_legacy_rows_with_concurrent_openers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private");
        let audio = wav(&[1, 0]);
        {
            let mut old = Store::open_at(&path).unwrap();
            old.insert(&job(), "fingerprint", &audio).unwrap();
            old.db.execute_batch("DROP INDEX voice_waveform_pending; ALTER TABLE messages DROP COLUMN waveform_version;").unwrap();
        }
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let openers: Vec<_> = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let reopened = Store::open_at(&path).unwrap();
                    assert_eq!(reopened.get(ID).unwrap().unwrap()["revision"], 1);
                })
            })
            .collect();
        for opener in openers {
            opener.join().unwrap();
        }
        let upgraded = Store::open_at(&path).unwrap();
        assert_eq!(upgraded.audio(ID).unwrap(), audio);
        assert_eq!(upgraded.backfill_waveforms().unwrap(), 1);
        assert_eq!(
            upgraded.get(ID).unwrap().unwrap()["audio"]["waveform"],
            json!(vec![255; 64])
        );
    }
    fn job() -> Value {
        json!({"id":ID,"name":"codex/fixture","run_id":"run","conversation_id":"conversation","agent":"codex","agent_home":"/private/fixture","account_id":"fixture","created_at":1.,"revision":1,"state":"queued","attempt":1,"asr_id":ID,"speech":{"machine_id":ID,"profile_id":"fixture"},"audio":{"request_id":ID,"index":0,"name":"voice.wav","mime":"audio/wav","bytes":48,"duration_ms":1}})
    }
    fn request() -> Value {
        json!({"action":"submit","request_id":ID,"expected_run_id":"run","expected_conversation_id":"conversation","speech":{"machine_id":ID,"profile_id":"fixture"},"audio":{"format":"pcm_s16le","sample_rate":16000,"channels":1,"data_base64":"AAAAAA=="}})
    }
    #[test]
    fn wav_original_preserves_every_sample() {
        let pcm = [0, 0, 255, 127, 0, 128, 255, 255];
        let value = wav(&pcm);
        assert_eq!(&value[44..], &pcm);
        assert_eq!(&value[..4], b"RIFF");
        assert_eq!(&value[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(value[40..44].try_into().unwrap()), 8);
    }
    #[test]
    fn request_rejects_paths_children_and_unbound_conversation() {
        assert!(validate_request(&request()).is_ok());
        for (key, value) in [
            ("agent_id", json!("child")),
            ("path", json!("/tmp/audio")),
            ("archive_id", json!(ID)),
        ] {
            let mut v = request();
            v[key] = value;
            assert!(validate_request(&v).is_err());
        }
        let mut v = request();
        v["expected_conversation_id"] = json!("");
        assert!(validate_request(&v).is_err());
        v = json!({"action":"profiles","request_id":ID,"expected_run_id":"run","expected_conversation_id":""});
        assert!(validate_request(&v).is_ok());
    }
    #[test]
    fn request_pins_exact_positive_profile_revision() {
        let mut value = request();
        value["speech"]["profile_revision"] = json!(7);
        assert!(validate_request(&value).is_ok());
        for invalid in [
            json!(0),
            json!(-1),
            json!(1.0),
            json!("7"),
            json!(9_007_199_254_740_992u64),
        ] {
            value["speech"]["profile_revision"] = invalid;
            assert!(validate_request(&value).is_err());
        }
    }
    #[test]
    fn accept_is_atomic_persistent_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let audio = wav(&[0, 1, 2, 3]);
        {
            let mut store = Store::open_at(&dir.path().join("private")).unwrap();
            store.insert(&job(), "fingerprint", &audio).unwrap();
            assert_eq!(store.insert(&job(), "fingerprint", &audio).unwrap(), job());
            assert!(store.insert(&job(), "different", &audio).is_err());
            assert_eq!(store.jobs(Some("conversation"), false).unwrap().len(), 1);
        }
        let store = Store::open_at(&dir.path().join("private")).unwrap();
        assert_eq!(store.audio(ID).unwrap(), audio);
        assert_eq!(store.get(ID).unwrap().unwrap()["state"], "queued");
    }
    #[test]
    fn cancellation_wins_against_late_asr_result() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open_at(&dir.path().join("private")).unwrap();
        let mut stale = store.insert(&job(), "fingerprint", &wav(&[0, 0])).unwrap();
        let mut cancelled = stale.clone();
        assert!(cancel_job(&mut cancelled));
        assert!(store.save(&mut cancelled).unwrap());
        speech_result(
            &mut stale,
            &json!({"schema":1,"job_id":ID,"state":"completed","text":"Late transcript"}),
        )
        .unwrap();
        assert!(!store.save(&mut stale).unwrap());
        assert_eq!(store.get(ID).unwrap().unwrap()["state"], "cancelled");
        assert!(!cancel_job(&mut cancelled));
        assert_eq!(store.audio(ID).unwrap(), wav(&[0, 0]));
    }
    #[test]
    fn native_busy_diagnostics_keep_the_readiness_code_and_message() {
        let expected = (
            "agent_not_ready",
            "The original agent could not accept this message yet. Check Activity or Terminal; the recording is preserved.",
        );
        for error in [
            "agent busy: terminal queue prompt is unrecognized or contains a draft; wait for the agent or open Terminal; nothing was submitted",
            "agent busy: terminal has a draft, dialog, or unrecognized prompt",
            "agent busy: terminal is not in the expected agent input mode; waiting for a choice",
        ] {
            assert_eq!(native_refusal(error), expected, "{error}");
        }
        for (error, code) in [
            ("terminal has a draft, dialog, or unrecognized prompt", "native_input_blocked"),
            ("terminal is not in the expected agent input mode", "native_input_blocked"),
            ("the previous input has not been confirmed", "previous_input_pending"),
            ("agent is waiting for approval", "native_choice_pending"),
            ("the original agent is not running; resume it", "agent_stopped"),
        ] {
            assert_eq!(native_refusal(error).0, code, "{error}");
        }
    }
    #[test]
    fn native_crash_boundary_never_replays() {
        let mut before = job();
        before["state"] = json!("delivering");
        settle(&mut before, None, true);
        assert_eq!(before["state"], "uncertain");
        let mut pending = job();
        settle(
            &mut pending,
            Some(&json!({"status":"in_progress","submitted_at":2.})),
            false,
        );
        assert_eq!(pending["state"], "uncertain");
        let mut accepted = job();
        settle(
            &mut accepted,
            Some(&json!({"status":"submitted","submitted_at":2.})),
            true,
        );
        assert_eq!(accepted["state"], "submitted");
        let mut refused = job();
        settle(&mut refused, None, false);
        assert_eq!(refused["state"], "waiting_agent");
    }
    #[test]
    fn a_refused_opencode_receipt_fails_the_job_definitely() {
        let mut refused = job();
        refused["agent"] = json!("opencode");
        settle(
            &mut refused,
            Some(&json!({"status":"refused","reason":"HTTP 400: bad","submitted_at":2.})),
            true,
        );
        assert_eq!(refused["state"], "failed");
        assert_eq!(refused["error_code"], "delivery_refused");
        assert_eq!(refused["error"], "HTTP 400: bad");
        assert_eq!(refused["native_receipt_status"], "refused");
        let mut bare = job();
        settle(&mut bare, Some(&json!({"status":"refused"})), false);
        assert_eq!(bare["state"], "failed");
        assert_eq!(bare["error"], "OpenCode refused this message");
    }
    #[test]
    fn a_refused_receipt_passes_reconciliation_for_settle() {
        // Stored or newly discovered refusals reach settle; other errors do not.
        let mut stored = json!({"status":"refused","reason":"HTTP 400: bad"});
        assert!(reconciled_opencode_receipt(&mut stored, |_| Err("refused".into())).is_ok());
        let mut discovered = json!({"status":"pending"});
        assert!(reconciled_opencode_receipt(&mut discovered, |value| {
            value["status"] = json!("refused");
            Err("refused".into())
        })
        .is_ok());
        assert_eq!(discovered["status"], "refused");
        let mut unknown = json!({"status":"pending"});
        assert_eq!(
            reconciled_opencode_receipt(&mut unknown, |_| Err("unreachable".into())).unwrap_err(),
            "unreachable"
        );
        let mut submitted = json!({"status":"submitted"});
        assert!(reconciled_opencode_receipt(&mut submitted, |_| Ok(true)).is_ok());
    }
    #[test]
    fn explicit_opencode_uncertainty_reaches_settle_without_becoming_refusal_proof() {
        for status in ["pending", "uncertain"] {
            for error in ["Native delivery state is uncertain", "Delivery uncertain; this request will not be replayed. Inspect native history before creating a new request"] {
                let mut native = json!({"status":status,"submitted_at":2.});
                assert!(reconciled_opencode_receipt(&mut native, |_| Err(error.into())).is_ok());
                assert_eq!(native["status"],status);
                let mut voice=job();voice["agent"]=json!("opencode");
                settle(&mut voice,Some(&native),true);
                assert_eq!(voice["state"],"uncertain");
                assert_eq!(voice["error_code"],"delivery_uncertain");
                assert_eq!(voice["native_receipt_status"],status);
                assert_ne!(voice["error_code"],"delivery_refused");
                assert!(voice["native_binding"].is_null());
            }
            for error in [
                "Native receipt identity changed",
                "Native request UUID conflicts with this voice message",
                "unreachable",
            ] {
                let mut native = json!({"status":status});
                assert_eq!(
                    reconciled_opencode_receipt(&mut native, |_| Err(error.into())).unwrap_err(),
                    error
                );
                assert_eq!(native["status"], status);
            }
        }
        for status in ["submitted", "unknown"] {
            let mut native = json!({"status":status});
            assert!(reconciled_opencode_receipt(&mut native, |_| Err(
                "Native delivery state is uncertain".into()
            ))
            .is_err());
        }
    }
    #[test]
    fn a_submitted_opencode_voice_job_is_bound_to_its_native_message() {
        let id = "msg_gVmstesSuU81H2vTHdmxp65tNI";
        let receipt = |message: &str| json!({"status":"submitted","submitted_at":2.,"native_message_id":message,"transport":"native_opencode"});
        let mut voice = job();
        voice["agent"] = json!("opencode");
        settle(&mut voice, Some(&receipt(id)), true);
        assert_eq!(voice["state"], "submitted");
        let binding = json!({"provider_message_id":id,"provider_source":"opencode_native"});
        assert_eq!(voice["native_binding"], binding);
        assert_eq!(public(&voice)["native_binding"], binding);
        let mut invalid = job();
        invalid["agent"] = json!("opencode");
        settle(&mut invalid, Some(&receipt("../x")), true);
        assert!(invalid.get("native_binding").is_none());
        let mut terminal = job();
        settle(&mut terminal, Some(&receipt(id)), true);
        assert!(terminal.get("native_binding").is_none());
    }
    #[test]
    fn restart_keeps_exact_native_payload_and_audio() {
        let dir = tempfile::tempdir().unwrap();
        let mut interrupted = job();
        interrupted["native_payload"] = json!("{\"text\":\"Exact \\\\ input\"}");
        interrupted["boundary"] = json!({"journal_seq":12,"source_bytes":900});
        interrupted["state"] = json!("delivering");
        {
            let mut store = Store::open_at(&dir.path().join("private")).unwrap();
            store.insert(&interrupted, "hash", &wav(&[1, 2])).unwrap();
        }
        let store = Store::open_at(&dir.path().join("private")).unwrap();
        let mut recovered = store.get(ID).unwrap().unwrap();
        assert_eq!(recovered, interrupted);
        settle(&mut recovered, None, true);
        assert!(store.save(&mut recovered).unwrap());
        assert_eq!(store.get(ID).unwrap().unwrap()["state"], "uncertain");
    }
    #[test]
    fn empty_or_command_transcripts_cannot_submit() {
        for text in ["", "  ", "/clear", "!rm", "bad\u{1b}text"] {
            let mut job = job();
            speech_result(
                &mut job,
                &json!({"schema":1,"job_id":ID,"state":"completed","text":text}),
            )
            .unwrap();
            assert_eq!(job["state"], "failed");
        }
        let mut job = job();
        speech_result(
            &mut job,
            &json!({"schema":1,"job_id":ID,"state":"completed","text":"Привет, мир"}),
        )
        .unwrap();
        assert_eq!(job["state"], "ready");
        assert_eq!(job["transcript"], "Привет, мир");
    }
    #[test]
    fn asr_recovery_has_a_bounded_separate_attempt_budget() {
        let mut v = job();
        speech_result(
            &mut v,
            &json!({"schema":1,"job_id":ID,"state":"uncertain","error_code":"service_restarted"}),
        )
        .unwrap();
        assert_eq!(v["state"], "queued");
        assert_eq!(v["id"], ID);
        v["attempt"] = json!(3);
        speech_result(
            &mut v,
            &json!({"schema":1,"job_id":ID,"state":"uncertain","error_code":"service_restarted"}),
        )
        .unwrap();
        assert_eq!(v["state"], "failed");
        assert!(speech_result(
            &mut job(),
            &json!({"schema":1,"job_id":"different","state":"completed","text":"hello"})
        )
        .is_err());
    }
    #[test]
    fn public_message_never_exposes_private_delivery_material() {
        let mut value = job();
        value["native_payload"] = json!("payload");
        value["boundary"] = json!({"transcript":"/private/path"});
        value["native_binding"] = json!({"journal_seq":2});
        let value = public(&value);
        assert!(value.get("name").is_none());
        assert!(value.get("agent_home").is_none());
        assert!(value.get("native_payload").is_none());
        assert!(value.get("boundary").is_none());
        assert_eq!(value["native_binding"]["journal_seq"], 2);
    }
    #[test]
    fn account_conversation_scope_survives_rename_but_not_rebinding() {
        let job = job();
        let mut target = job.clone();
        target["name"] = json!("codex/renamed");
        target["run_id"] = json!("new-run");
        assert!(belongs(&job, &target));
        target["conversation_id"] = json!("another");
        assert!(!belongs(&job, &target));
        target["conversation_id"] = job["conversation_id"].clone();
        target["account_id"] = json!("another");
        assert!(!belongs(&job, &target));
    }
    #[test]
    fn delivery_started_and_terminal_messages_cannot_be_cancelled() {
        for state in [
            "delivering",
            "submitted",
            "uncertain",
            "failed",
            "cancelled",
        ] {
            let mut v = job();
            v["state"] = json!(state);
            assert!(!cancel_job(&mut v));
            assert_eq!(v["state"], state);
        }
    }
    #[test]
    fn history_changes_are_bounded_and_revisit_revised_rows() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open_at(&dir.path().join("private")).unwrap();
        for index in 0..125 {
            let mut v = job();
            v["id"] = json!(uuid::Uuid::new_v4().to_string());
            v["created_at"] = json!(index as f64);
            v["state"] = json!("submitted");
            store.insert(&v, "hash", &wav(&[0, 0])).unwrap();
        }
        let (first, next, complete) = store.changes("conversation", 0).unwrap();
        assert_eq!(first.len(), 100);
        assert!(!complete);
        let (last, end, complete) = store.changes("conversation", next).unwrap();
        assert_eq!(last.len(), 25);
        assert!(complete);
        let mut changed = first[0].clone();
        changed["native_binding"] = json!({"journal_seq":3});
        assert!(store.save(&mut changed).unwrap());
        let (revised, sequence, complete) = store.changes("conversation", end).unwrap();
        assert_eq!(revised, vec![changed]);
        assert!(sequence > end);
        assert!(complete);
        assert_eq!(store.jobs(Some("conversation"), false).unwrap().len(), 100);
    }
    #[test]
    fn connection_reopen_preserves_live_wal_visibility() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private");
        let worker = Store::open_at(&path).unwrap();
        {
            let mut client = Store::open_at(&path).unwrap();
            client.insert(&job(), "hash", &wav(&[0, 0])).unwrap();
        }
        {
            let inspector = Store::open_at(&path).unwrap();
            assert!(inspector.get(ID).unwrap().is_some());
        }
        let mut job = worker.get(ID).unwrap().unwrap();
        job["state"] = json!("transcribing");
        assert!(worker.save(&mut job).unwrap());
        let reopened = Store::open_at(&path).unwrap();
        assert_eq!(reopened.get(ID).unwrap().unwrap()["state"], "transcribing");
    }

    #[test]
    fn parent_symlink_is_supported_but_database_symlink_is_rejected() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real");
        fs::create_dir(&target).unwrap();
        let alias = dir.path().join("alias");
        symlink(&target, &alias).unwrap();
        let path = alias.join("private");
        {
            let mut store = Store::open_at(&path).unwrap();
            store.insert(&job(), "hash", &wav(&[0, 0])).unwrap();
        }
        assert!(Store::open_at(&path).unwrap().get(ID).unwrap().is_some());
        let db = path.join("messages.sqlite3");
        let moved = path.join("saved.sqlite3");
        fs::rename(&db, &moved).unwrap();
        symlink(&moved, &db).unwrap();
        assert!(Store::open_at(&path).is_err());
    }
}
