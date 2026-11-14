//! A witnessed task input for a hosted DeepSeek session (zerus-hfw2s).
//!
//! `submit_witnessed` offers one task input to a live DeepSeek agent and
//! answers with a fixed code: an exact receipt only when the conversation
//! durably recorded this request's own message; a refusal only when nothing
//! was offered, or when the host's log shows the offered message can never
//! enter the conversation; anything else is unknown. Codes are never folded,
//! retried or promoted: a repeat of the same request replays its stored
//! outcome. The bridge reports facts from the session's in-memory log; the
//! classification below is the only place that decides (docs/reference.md).
// Entry points for the Agent Teams task delivery (`crate::state::dsh::witness`);
// until it routes hosted members here, only the tests call them.
#![cfg_attr(not(test), allow(dead_code))]
use super::*;
use std::time::{Duration, Instant};

/// Why a task input was definitely not delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotOfferedCode {
    /// Refused before anything was offered, by HGS or by the host.
    HostRefused,
    /// Its text holds a native session mention, which the host rewrites or
    /// fails on; refused before anything was offered.
    UnsafeContent,
    /// Queued, then canceled by the host's own disposal of the agent.
    HostDisposed,
    /// Queued, then canceled by another party (the native UI or the host).
    Canceled,
    /// Claimed by a turn that ended without recording it; never queued again.
    DroppedAfterClaim,
}

/// Why the outcome of a task input is unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnknownCode {
    /// Still queued, claimed or not yet durable at the caller's deadline.
    Pending,
    /// The conversation recorded this request's message with other content.
    Changed,
    /// The host or its live agent can no longer answer for the request.
    HostGone,
    /// The host holds no record of the request.
    Unseen,
}

impl NotOfferedCode {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::HostRefused => "host_refused",
            Self::UnsafeContent => "unsafe_content",
            Self::HostDisposed => "host_disposed",
            Self::Canceled => "canceled",
            Self::DroppedAfterClaim => "dropped_after_claim",
        }
    }
    fn parse(code: &str) -> Option<Self> {
        [
            Self::HostRefused,
            Self::UnsafeContent,
            Self::HostDisposed,
            Self::Canceled,
            Self::DroppedAfterClaim,
        ]
        .into_iter()
        .find(|candidate| candidate.code() == code)
    }
}

impl UnknownCode {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Changed => "changed",
            Self::HostGone => "host_gone",
            Self::Unseen => "unseen",
        }
    }
    fn parse(code: &str) -> Option<Self> {
        [Self::Pending, Self::Changed, Self::HostGone, Self::Unseen]
            .into_iter()
            .find(|candidate| candidate.code() == code)
    }
}

/// The exact request a refusal is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Binding {
    pub(crate) request_id: String,
    pub(crate) name: String,
    pub(crate) run_id: String,
    pub(crate) conversation_id: String,
    /// `dsh/receipts` digest of `[name, request]`.
    pub(crate) digest: String,
}

/// What one witnessed task input came to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Witnessed {
    /// The submitted receipt, with the durable message that witnessed it.
    Receipt(Value),
    NotOffered {
        code: NotOfferedCode,
        binding: Binding,
        reason: String,
    },
    Unknown {
        code: UnknownCode,
        detail: String,
    },
}

impl Witnessed {
    /// The fixed code: `received`, or the NotOffered or Unknown code.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Receipt(_) => "received",
            Self::NotOffered { code, .. } => code.code(),
            Self::Unknown { code, .. } => code.code(),
        }
    }
}

/// A session mention the host's SessionReferenceResolver acts on. The literal
/// is a superset of its matcher (dsh-session-reference parseSessionReferenceText),
/// case-sensitive like it.
const SESSION_MENTION: &str = "dsh-session:";
/// One input-witness read waits at most this long on the host.
const READ_WAIT: Duration = Duration::from_secs(25);
/// What a receipt's digest covers: the text of each block and every block's
/// type, never image bytes. A receipt is no proof of attachment content.
const DIGEST_SCOPE: &str = "text_and_block_types";
/// An earlier send may have offered the input, but nothing witnessed it:
/// unknown, never a refusal.
const UNWITNESSED: &str =
    "An earlier send without a witness already offered exactly this input; it is never witnessed or sent again";
/// The caller's deadline budgets the whole call (zerus-hfw2s.8).
const DEADLINE: &str =
    "The deadline passed before the input was offered; nothing was written or sent";
const LOCK_BUSY: &str =
    "Another call held this request until the deadline and may be offering it; this call sends nothing";
/// An offer needs this much of the deadline left: the send and at least one read.
const MIN_OFFER: Duration = Duration::from_secs(1);
/// What a read leaves of its RPC timeout for the host's flush and answer.
const READ_MARGIN: Duration = Duration::from_millis(500);

/// The time left of the caller's deadline.
struct Budget {
    started: Instant,
    deadline: Duration,
}

impl Budget {
    fn remaining(&self) -> Duration {
        self.deadline.saturating_sub(self.started.elapsed())
    }
    /// An RPC timeout: at most `cap` and what is left; None when nothing is.
    fn timeout(&self, cap: Duration) -> Option<Duration> {
        Some(self.remaining().min(cap)).filter(|left| !left.is_zero())
    }
}

const MARKER_UNCERTAIN: &str =
    "The pending marker was recorded although its write reported a failure; nothing was sent, and it is never sent again";
const UNREADABLE: &str =
    "The input's recorded outcome could not be read, so an earlier call may have offered it; it is never sent again";
const NO_WITNESS: &str =
    "This DeepSeek host cannot witness a task input; it can after its next own start";
const NOT_A_FILE: &str = "The receipt path holds no regular file";
const INCOMPLETE: &str = "Session identity is incomplete";

/// The digest both sides compute over message content: each block's type and,
/// for a text block, its text. The host keeps images as attachment references,
/// so their bytes are not compared.
pub(super) fn content_digest(content: &[Value]) -> String {
    let mut hash = Sha256::new();
    for block in content {
        let kind = string(block, "type");
        hash.update(kind.as_bytes());
        hash.update([0]);
        if kind == "text" {
            hash.update(string(block, "text").as_bytes());
        }
        hash.update([0]);
    }
    format!("{:x}", hash.finalize())
}

/// Whether any text block HGS would insert carries a session mention.
pub(super) fn unsafe_content(content: &[Value]) -> bool {
    content.iter().any(|block| {
        string(block, "type") == "text" && string(block, "text").contains(SESSION_MENTION)
    })
}

/// How the host's facts settle one tracked message.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Settled {
    /// Durably recorded with exactly the offered content, at this sequence.
    Received(u64),
    NotOffered(NotOfferedCode),
    Unknown(UnknownCode),
    /// Not settled yet: read again until the deadline.
    Pending,
}

/// Classifies the bridge's facts for one tracked message (`input-witness`).
/// Replays the inbox splices from the one that inserted it: a claim (a
/// removal without an outcome) followed by its `user/message` is delivery; a
/// claim followed by `turn/end` without it is a drop; a canceled removal not
/// followed by a re-insert of the same message is a cancellation, the host's
/// own when it falls in one of the bridge's disposal windows. A removal that
/// re-inserts the same message (an edit) or a later re-insert (a move) keeps
/// tracking it. Only a durable `user/message` with this request ID, the
/// original message ID and the offered digest is a receipt.
///
/// `facts.complete` false means the events stop before the log's end. A
/// conclusion that a later event could still change (a receipt or a changed
/// message, a cancellation that a re-insert could undo) is drawn only from a
/// complete slice; otherwise the message is still pending. A claim followed by
/// `turn/end` without its message is final whatever follows.
pub(super) fn classify(facts: &Value, request_id: &str, digest: &str) -> Settled {
    match settle_events(facts, request_id, digest) {
        Settled::NotOffered(NotOfferedCode::DroppedAfterClaim) => {
            Settled::NotOffered(NotOfferedCode::DroppedAfterClaim)
        }
        settled if facts["complete"] == true => settled,
        Settled::Unknown(UnknownCode::Unseen) => Settled::Unknown(UnknownCode::Unseen),
        _ => Settled::Pending,
    }
}

fn settle_events(facts: &Value, request_id: &str, digest: &str) -> Settled {
    let id = string(facts, "messageId");
    if id.is_empty() {
        return Settled::Unknown(UnknownCode::Unseen);
    }
    let mut position: Option<(String, u64)> = None;
    let mut claimed = false;
    let mut canceled_at: Option<u64> = None;
    for event in facts["events"].as_array().into_iter().flatten() {
        let seq = event["seq"].as_u64().unwrap_or(0);
        match string(event, "type") {
            "agent/inbox/spliced" => {
                let target = string(event, "target");
                let start = event["start"].as_u64().unwrap_or(0);
                let removed = event["removedCount"].as_u64().unwrap_or(0);
                let inserted: Vec<&Value> =
                    event["inserted"].as_array().into_iter().flatten().collect();
                let again = inserted
                    .iter()
                    .position(|message| string(message, "id") == id);
                match position.as_mut() {
                    Some((list, index)) if list == target => {
                        if *index < start {
                        } else if *index < start + removed {
                            if let Some(at) = again {
                                *index = start + at as u64;
                            } else {
                                position = None;
                                if event["outcome"] == "canceled" {
                                    canceled_at = Some(seq);
                                } else {
                                    claimed = true;
                                }
                            }
                        } else {
                            *index = *index - removed + inserted.len() as u64;
                        }
                    }
                    Some(_) => {}
                    None if !claimed => {
                        if let Some(at) = again {
                            position = Some((target.to_owned(), start + at as u64));
                            canceled_at = None;
                        }
                    }
                    None => {}
                }
            }
            "user/message" if string(event, "id") == id => {
                if !claimed || facts["durable"] != true {
                    return Settled::Pending;
                }
                return if string(event, "rpcId") == request_id && string(event, "digest") == digest
                {
                    Settled::Received(seq)
                } else {
                    Settled::Unknown(UnknownCode::Changed)
                };
            }
            "turn/end" if claimed => return Settled::NotOffered(NotOfferedCode::DroppedAfterClaim),
            _ => {}
        }
    }
    if let (None, false, Some(seq)) = (&position, claimed, canceled_at) {
        let own = facts["disposals"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|window| {
                window[0].as_u64().is_some_and(|from| from <= seq)
                    && window[1].as_u64().is_some_and(|to| seq < to)
            });
        return Settled::NotOffered(if own {
            NotOfferedCode::HostDisposed
        } else {
            NotOfferedCode::Canceled
        });
    }
    if position.is_none() && !claimed {
        return Settled::Unknown(UnknownCode::Unseen);
    }
    Settled::Pending
}

/// The host's answer for a request it holds no record of, or whose host or
/// live agent is gone, as a final unknown code.
fn unsettled(answer: &Value) -> Option<Witnessed> {
    let detail = |text: &str| text.to_owned();
    match string(answer, "state") {
        "host_gone" => Some(Witnessed::Unknown {
            code: UnknownCode::HostGone,
            detail: detail("The DeepSeek host changed"),
        }),
        "unseen" => Some(Witnessed::Unknown {
            code: UnknownCode::Unseen,
            detail: detail("The DeepSeek host holds no record of this input"),
        }),
        _ => None,
    }
}

fn binding_of(name: &str, request: &Value, digest: &str) -> Binding {
    Binding {
        request_id: string(request, "request_id").to_owned(),
        name: name.to_owned(),
        run_id: string(request, "expected_run_id").to_owned(),
        conversation_id: string(request, "expected_conversation_id").to_owned(),
        digest: digest.to_owned(),
    }
}

/// The outcome a receipt already holds for this exact request, replayed as
/// it was recorded: never promoted, never sent again.
fn replay(receipt: &Value, binding: &Binding) -> Witnessed {
    match string(receipt, "status") {
        "submitted" => Witnessed::Receipt(receipt.clone()),
        "not_offered" => match NotOfferedCode::parse(string(receipt, "code")) {
            Some(code) => Witnessed::NotOffered {
                code,
                binding: binding.clone(),
                reason: string(receipt, "reason").to_owned(),
            },
            None => Witnessed::Unknown {
                code: UnknownCode::Unseen,
                detail: "The recorded outcome is not readable".into(),
            },
        },
        _ => Witnessed::Unknown {
            code: UnknownCode::parse(string(receipt, "code")).unwrap_or(UnknownCode::Pending),
            detail: "This input may already be in DeepSeek; it is never sent again".into(),
        },
    }
}

fn receipt_path(request_id: &str) -> PathBuf {
    root()
        .join("dsh/receipts")
        .join(format!("{request_id}.json"))
}

/// A stored receipt for this request ID, if any. Only a witnessed receipt for
/// exactly this request (digest, name, run and conversation) is its outcome;
/// any other is an identity conflict.
enum Stored {
    Absent,
    Exact(Value),
    /// Exactly this request, recorded by an ordinary send without a witness:
    /// it may have been offered.
    Unwitnessed,
    Conflict,
}

/// Reads the receipt path without following links (zerus-hfw2s.6). Only a
/// missing path is `Absent`. Anything else that cannot prove what this
/// request ID holds is an error, answered `Unknown{unseen}`: a link, dangling
/// or not, or any other non-regular file; a file that does not parse; a parsed
/// value that is not a well-formed receipt of this request ID. Only a
/// well-formed receipt of this request ID whose binding or digest differs is a
/// `Conflict`.
fn stored(binding: &Binding) -> Result<Stored> {
    use std::io::Read;
    let path = receipt_path(&binding.request_id);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Stored::Absent),
        Err(error) => return Err(error.to_string()),
        Ok(meta) if !meta.file_type().is_file() => return Err(NOT_A_FILE.into()),
        Ok(_) => {}
    }
    // Tests replace the path here, between the check above and the open.
    #[cfg(test)]
    if let Some(swap) = tests::BEFORE_OPEN.with(|hook| hook.take()) {
        swap(&path);
    }
    // The opened descriptor is checked again: the path may have changed since.
    let mut file = crate::platform::no_follow(fs::OpenOptions::new().read(true))
        .open(&path)
        .map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err(NOT_A_FILE.into());
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let receipt: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let field = |key: &str| receipt.get(key).and_then(Value::as_str);
    let well_formed = receipt.is_object()
        && field("request_id") == Some(binding.request_id.as_str())
        && ["name", "run_id", "conversation_id", "digest"]
            .iter()
            .all(|key| field(key).is_some());
    if !well_formed {
        return Err("The receipt path holds no well-formed receipt of this request".into());
    }
    let same = string(&receipt, "digest") == binding.digest
        && string(&receipt, "name") == binding.name
        && string(&receipt, "run_id") == binding.run_id
        && string(&receipt, "conversation_id") == binding.conversation_id;
    Ok(match (same, receipt["witness_version"] == json!(1)) {
        (true, true) => Stored::Exact(receipt),
        (true, false) => Stored::Unwitnessed,
        (false, _) => Stored::Conflict,
    })
}

fn unknown(code: UnknownCode, detail: &str) -> Witnessed {
    Witnessed::Unknown {
        code,
        detail: detail.to_owned(),
    }
}

/// Exclusivity for one request ID (zerus-hfw2s.5): `dsh/witness-locks/<id>.lock`,
/// held by a witnessed submit from before its store check until after its
/// send, by `recorded`, and by an ordinary send around its receipt check,
/// marker and send. Ordinary dispatch takes its per-session guard before the
/// request lock, without holding the machine-wide binding lock. The witness
/// path takes neither a session guard nor the machine-wide binding lock.
///
/// On Unix the file is removed while still locked on release, so the number of
/// files stays bounded; a waiter that locked the removed inode sees the path
/// gone or replaced and retries. A crash leaves at most one empty file per
/// in-flight request, removed by that request's next use. Elsewhere the file
/// is kept, so its path always names the file that was locked, and one empty
/// file remains for each request ID used.
pub(crate) struct RequestLock {
    #[cfg_attr(not(unix), allow(dead_code))]
    path: PathBuf,
    _guard: Guard,
}

impl Drop for RequestLock {
    fn drop(&mut self) {
        // Runs before the guard is released.
        #[cfg(unix)]
        let _ = fs::remove_file(&self.path);
    }
}

pub(crate) fn request_lock(request_id: &str) -> Result<RequestLock> {
    request_lock_until(request_id, None).map(|lock| lock.expect("an unbounded wait always locks"))
}

/// `request_lock`, waiting at most until `until`: None when another holder
/// still has it then.
fn request_lock_until(request_id: &str, until: Option<Instant>) -> Result<Option<RequestLock>> {
    uuid::Uuid::parse_str(request_id).map_err(|_| "Invalid request ID".to_owned())?;
    let directory = root().join("dsh/witness-locks");
    private_dir(&directory)?;
    let path = directory.join(format!("{request_id}.lock"));
    loop {
        let file = lock_file(&path)?;
        let opened = file_identity(&file)?;
        let guard = match until {
            None => Guard::exclusive(file).map_err(|e| e.to_string())?,
            Some(until) => loop {
                match fs2::FileExt::try_lock_exclusive(&file) {
                    Ok(()) => break Guard::held(file),
                    Err(error) if crate::platform::lock_contended(&error) => {
                        if Instant::now() >= until {
                            return Ok(None);
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(error) => return Err(error.to_string()),
                }
            },
        };
        // Tests remove or replace the path here, as a racing release would.
        #[cfg(test)]
        if let Some(swap) = tests::AFTER_LOCK.with(|hook| hook.take()) {
            swap(&path);
        }
        // A lock on a file a release removed, or one replaced since, retries.
        if still_named(&path, opened)? {
            return Ok(Some(RequestLock {
                path,
                _guard: guard,
            }));
        }
    }
}

/// The device and inode of an opened lock file.
#[cfg(unix)]
fn file_identity(file: &fs::File) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    Ok((meta.dev(), meta.ino()))
}

/// Whether the lock path still names the file `opened` identifies.
#[cfg(unix)]
fn still_named(path: &Path, opened: (u64, u64)) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    match fs::metadata(path) {
        Ok(meta) => Ok((meta.dev(), meta.ino()) == opened),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

/// Elsewhere a lock file is never removed (`RequestLock`'s Drop), so its path
/// always names the file that was locked.
#[cfg(not(unix))]
fn file_identity(_: &fs::File) -> Result<(u64, u64)> {
    Ok((0, 0))
}
#[cfg(not(unix))]
fn still_named(_: &Path, _: (u64, u64)) -> Result<bool> {
    Ok(true)
}

/// What the receipt store holds for a request, for recovery (`recorded`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Recorded {
    /// No witnessed submit has offered this exact request, and none is
    /// offering it now: the only answer that licenses a new send.
    Absent,
    /// This request ID already holds another input (another payload, name,
    /// run or conversation). Nothing about this request; never send it.
    Conflict,
    /// The stored outcome of exactly this request, replayed as recorded.
    Outcome(Witnessed),
}

/// The recorded outcome of `request` to `name`, for recovery. It reads the
/// store under the same request lock `submit_witnessed` holds from before its
/// pending marker until after its send, so it never sees a submit between the
/// two: only `Recorded::Absent` means that no witnessed submit offered, or is
/// offering, this exact request. A lock or store that cannot be read is an
/// error, never `Absent`.
pub(crate) fn recorded(name: &str, request: &Value) -> Result<Recorded> {
    let digest = request_digest(name, request);
    let binding = binding_of(name, request, &digest);
    let _guard = request_lock(&binding.request_id)?;
    Ok(match stored(&binding)? {
        Stored::Exact(receipt) => Recorded::Outcome(replay(&receipt, &binding)),
        Stored::Unwitnessed => Recorded::Outcome(unknown(UnknownCode::Unseen, UNWITNESSED)),
        Stored::Conflict => Recorded::Conflict,
        Stored::Absent => Recorded::Absent,
    })
}

fn request_digest(name: &str, request: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(json!([name, request]).to_string().as_bytes())
    )
}

/// Offers one task input to `name` and witnesses it until `deadline`.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn submit_witnessed(name: &str, request: &Value, deadline: Duration) -> Witnessed {
    submit_with(
        name,
        request,
        deadline,
        &mut |record, method, params, wait| rpc_within(record, method, params, wait),
    )
}

pub(super) type Rpc<'a> = dyn FnMut(&Value, &str, Value, Duration) -> Result<Value> + 'a;

pub(super) fn submit_with(
    name: &str,
    request: &Value,
    deadline: Duration,
    rpc: &mut Rpc,
) -> Witnessed {
    let started = Instant::now();
    let budget = Budget { started, deadline };
    let digest = request_digest(name, request);
    let binding = binding_of(name, request, &digest);
    let refuse = |code: NotOfferedCode, reason: String| Witnessed::NotOffered {
        code,
        binding: binding.clone(),
        reason,
    };
    // A request ID that is not a UUID was never offered by HGS.
    if uuid::Uuid::parse_str(&binding.request_id).is_err() {
        return refuse(NotOfferedCode::HostRefused, "Invalid request ID".into());
    }
    // Only this request's lock, held from before the store check until after
    // the send; never the machine-wide bindings lock, so another session's
    // slow host never delays this one. Without the lock the receipt store
    // cannot be read: an earlier call may have offered this input.
    let guard = match request_lock_until(&binding.request_id, Some(started + deadline)) {
        Ok(Some(guard)) => guard,
        Ok(None) => return unknown(UnknownCode::Unseen, LOCK_BUSY),
        Err(error) => return unknown(UnknownCode::Unseen, &format!("{UNREADABLE}: {error}")),
    };
    let prepared = match prepare(name, request, &digest, &budget, rpc) {
        Ok(Prepared::Offer(prepared)) => prepared,
        Ok(Prepared::Recorded(receipt)) => return replay(&receipt, &binding),
        Ok(Prepared::Unknown(witnessed)) => return witnessed,
        Err((code, reason)) => return refuse(code, reason),
    };
    let Offer {
        record,
        mut receipt,
        content,
        generation,
        content_digest,
    } = prepared;
    let path = receipt_path(&binding.request_id);
    // Too little of the deadline left to offer and read: nothing is written.
    if budget.remaining() < MIN_OFFER {
        return refuse(NotOfferedCode::HostRefused, DEADLINE.into());
    }
    // The pending marker: from here nothing is refused by HGS and nothing is
    // ever sent again for this request.
    if let Err(error) = write_marker(&path, &receipt.to_string()) {
        // The write may have failed after its rename (zerus-hfw2s.7): answer by
        // what the store now holds, still under this request's lock.
        return match stored(&binding) {
            Ok(Stored::Absent) => refuse(NotOfferedCode::HostRefused, error),
            Ok(_) => unknown(UnknownCode::Pending, MARKER_UNCERTAIN),
            Err(read) => unknown(UnknownCode::Unseen, &format!("{UNREADABLE}: {read}")),
        };
    }
    let sent = rpc(
        &record,
        "send",
        json!({"sessionId":record["conversation_id"],"requestId":binding.request_id,
        "content":content,"mode":"steer"}),
        budget.remaining().min(Duration::from_secs(35)),
    );
    drop(guard);
    let outcome = witness(
        &record,
        &binding,
        &generation,
        &content_digest,
        started,
        deadline,
        sent.err(),
        rpc,
    );
    settle(&path, &mut receipt, outcome)
}

struct Offer {
    record: Value,
    receipt: Value,
    content: Vec<Value>,
    generation: Value,
    content_digest: String,
}

enum Prepared {
    Offer(Offer),
    Recorded(Value),
    /// The store says this input may already have been offered.
    Unknown(Witnessed),
}

/// Writes the pending marker. Tests can make it fail before writing anything,
/// or after its rename, as a failed directory sync would.
fn write_marker(path: &Path, contents: &str) -> Result<()> {
    #[cfg(test)]
    if let Some(after_rename) = tests::MARKER_FAULT.with(|fault| fault.take()) {
        if after_rename {
            atomic(path, contents)?;
        }
        return Err("injected marker failure".into());
    }
    atomic(path, contents)
}

/// Every check before the pending marker. Each failure is a definite refusal,
/// except a store that may already hold this input (Prepared::Unknown).
fn prepare(
    name: &str,
    request: &Value,
    digest: &str,
    budget: &Budget,
    rpc: &mut Rpc,
) -> std::result::Result<Prepared, (NotOfferedCode, String)> {
    let host = |error: String| (NotOfferedCode::HostRefused, error);
    // A host call bounded by what is left of the deadline; running out is the
    // deadline's own refusal, with nothing written.
    let mut ask = |record: &Value, method: &str, params: Value, cap: Duration| {
        let timeout = budget.timeout(cap).ok_or_else(|| host(DEADLINE.into()))?;
        rpc(record, method, params, timeout).map_err(|error| {
            host(if budget.remaining().is_zero() {
                DEADLINE.into()
            } else {
                error
            })
        })
    };
    if uuid::Uuid::parse_str(string(request, "request_id")).is_err() {
        return Err(host("Invalid request ID".into()));
    }
    // A request ID already recorded answers only for exactly that request,
    // whatever the session holds now; nothing is sent or overwritten.
    let stored = match stored(&binding_of(name, request, digest)) {
        Ok(stored) => stored,
        Err(error) => {
            return Ok(Prepared::Unknown(unknown(
                UnknownCode::Unseen,
                &format!("{UNREADABLE}: {error}"),
            )))
        }
    };
    match stored {
        Stored::Exact(receipt) => return Ok(Prepared::Recorded(receipt)),
        Stored::Unwitnessed => return Ok(Prepared::Unknown(unknown(UnknownCode::Unseen, UNWITNESSED))),
        Stored::Conflict => {
            return Err(host("This request ID was already used for another input (identity conflict); nothing was sent".into()))
        }
        Stored::Absent => {}
    }
    let record = binding(name).map_err(host)?;
    // A receipt without the session's run and conversation would no longer
    // read as well-formed, whatever the request expects.
    if string(&record, "run_id").is_empty() || string(&record, "conversation_id").is_empty() {
        return Err(host(INCOMPLETE.into()));
    }
    if record["permissions_pending"] == true {
        return Err(host(
            "Resume this DeepSeek session to finish permission setup before sending".into(),
        ));
    }
    if request["expected_run_id"] != record["run_id"]
        || request["expected_conversation_id"] != record["conversation_id"]
    {
        return Err(host("Session changed; refresh before trying again".into()));
    }
    if !string(request, "agent_id").is_empty() {
        return Err(host(
            "A task input goes to the session itself, not to a child".into(),
        ));
    }
    let content = message_content(request).map_err(host)?;
    if unsafe_content(&content) {
        return Err((NotOfferedCode::UnsafeContent, "The task text mentions a native DeepSeek session (dsh-session:), which the host rewrites".into()));
    }
    let ping = ask(&record, "ping", json!({}), Duration::from_secs(10))?;
    if ping["inputWitness"] != true {
        return Err(host(NO_WITNESS.into()));
    }
    let readiness = ask(
        &record,
        "inspect",
        json!({"sessionId":record["conversation_id"],"includeProcesses":false}),
        Duration::from_secs(35),
    )?;
    if readiness["authRequired"] == true {
        return Err(host(
            "Sign in to DeepSeek in the native UI before sending a message".into(),
        ));
    }
    if readiness["live"] != true {
        return Err(host("Resume this session before sending a message".into()));
    }
    let content_digest = content_digest(&content);
    let mut receipt = json!({"request_id":request["request_id"],"name":name,"run_id":record["run_id"],
        "conversation_id":record["conversation_id"],"at":now(),"digest":digest,"witness_version":1,
        "content_digest":content_digest,"digest_scope":DIGEST_SCOPE,"generation":ping["generation"],"text":request["text"],
        "submitted_at":now(),"status":"pending"});
    receipt["attachments"] = attachments::stage_native(request).map_err(host)?;
    if let Some(target) = session_graph::target_receipt(&record) {
        receipt["graph_target"] = target;
    }
    Ok(Prepared::Offer(Offer {
        generation: ping["generation"].clone(),
        record,
        receipt,
        content,
        content_digest,
    }))
}

/// Reads the host's witness of the offered message until it settles or the
/// deadline passes. Each read is bounded; nothing is sent again.
#[allow(clippy::too_many_arguments)]
fn witness(
    record: &Value,
    binding: &Binding,
    generation: &Value,
    digest: &str,
    started: Instant,
    deadline: Duration,
    send_error: Option<String>,
    rpc: &mut Rpc,
) -> Witnessed {
    // Events accumulate page by page from the inserting splice; the next read
    // starts where the last one stopped, without waiting while a page was cut.
    let mut events: Vec<Value> = Vec::new();
    let mut from: Option<u64> = None;
    let mut bound: Option<(Value, Value)> = None;
    let mut truncated = false;
    loop {
        // Each read is bounded by what is left of the deadline (zerus-hfw2s.8).
        let remaining = deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Witnessed::Unknown {
                code: UnknownCode::Pending,
                detail: "DeepSeek has not recorded the input yet; it is never sent again".into(),
            };
        }
        let wait = if truncated {
            Duration::ZERO
        } else {
            remaining.saturating_sub(READ_MARGIN).min(READ_WAIT)
        };
        let mut params = json!({"sessionId":record["conversation_id"],"requestId":binding.request_id,
            "generation":generation,"waitMs":wait.as_millis() as u64});
        if let Some(from) = from {
            params["fromSeq"] = json!(from);
        }
        let asked = Instant::now();
        let mut answer = match rpc(record, "input-witness", params, remaining) {
            Ok(answer) => answer,
            Err(error) => {
                return Witnessed::Unknown {
                    code: UnknownCode::HostGone,
                    detail: error,
                }
            }
        };
        if let Some(unsettled) = unsettled(&answer) {
            return unsettled;
        }
        if answer["state"] == "refused" {
            return Witnessed::NotOffered {
                code: NotOfferedCode::HostRefused,
                binding: binding.clone(),
                reason: string(&answer, "reason").to_owned(),
            };
        }
        if answer["state"] != "tracked" {
            return Witnessed::Unknown {
                code: UnknownCode::Unseen,
                detail: "The DeepSeek host gave no witness".into(),
            };
        }
        // Every page must be about the same bound message.
        let identity = (answer["messageId"].clone(), answer["spliceSeq"].clone());
        if bound.get_or_insert_with(|| identity.clone()) != &identity {
            return Witnessed::Unknown {
                code: UnknownCode::Unseen,
                detail: "The DeepSeek host's witness changed".into(),
            };
        }
        let next = from.unwrap_or(0);
        events.extend(
            answer["events"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|e| e["seq"].as_u64().is_some_and(|seq| seq >= next))
                .cloned(),
        );
        from = answer["through"].as_u64();
        answer["events"] = json!(events);
        truncated = answer["complete"] != true;
        match classify(&answer, &binding.request_id, digest) {
            Settled::Pending if truncated && started.elapsed() < deadline => continue,
            Settled::Received(seq) => {
                return Witnessed::Receipt(
                    json!({"seq":seq,"message_id":answer["messageId"],"generation":generation}),
                )
            }
            Settled::NotOffered(code) => {
                return Witnessed::NotOffered {
                    code,
                    binding: binding.clone(),
                    reason: answer_reason(code),
                }
            }
            Settled::Unknown(code) => {
                return Witnessed::Unknown {
                    code,
                    detail: send_error.clone().unwrap_or_default(),
                }
            }
            Settled::Pending if started.elapsed() >= deadline => {
                return Witnessed::Unknown {
                    code: UnknownCode::Pending,
                    detail: "DeepSeek has not recorded the input yet; it is never sent again"
                        .into(),
                }
            }
            // A complete page that settled nothing came back at once (a flush
            // that failed, a turn still ending): pause before reading again.
            Settled::Pending => {
                if !truncated && asked.elapsed() < Duration::from_millis(100) {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
}

fn answer_reason(code: NotOfferedCode) -> String {
    match code {
        NotOfferedCode::HostDisposed => {
            "The DeepSeek host stopped the agent before the input reached the conversation"
        }
        NotOfferedCode::Canceled => {
            "The queued input was canceled before it reached the conversation"
        }
        NotOfferedCode::DroppedAfterClaim => "The agent's turn ended without recording the input",
        NotOfferedCode::HostRefused | NotOfferedCode::UnsafeContent => "The input was refused",
    }
    .into()
}

/// Records the outcome on the request's receipt. A receipt that cannot be
/// written is reported as pending: nothing claims more than the store holds.
fn settle(path: &Path, receipt: &mut Value, outcome: Witnessed) -> Witnessed {
    match &outcome {
        Witnessed::Receipt(witness) => {
            receipt["digest_scope"] = json!(DIGEST_SCOPE);
            receipt["status"] = json!("submitted");
            receipt["transport"] = json!("native_dsh");
            receipt["witness"] = witness.clone();
        }
        Witnessed::NotOffered { code, reason, .. } => {
            receipt["status"] = json!("not_offered");
            receipt["code"] = json!(code.code());
            receipt["reason"] = json!(reason);
        }
        Witnessed::Unknown { code, .. } => {
            receipt["code"] = json!(code.code());
        }
    }
    if let Err(error) = atomic(path, &receipt.to_string()) {
        return Witnessed::Unknown {
            code: UnknownCode::Pending,
            detail: error,
        };
    }
    match outcome {
        Witnessed::Receipt(_) => Witnessed::Receipt(receipt.clone()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUEST: &str = "11111111-2222-4333-8444-555555555555";

    fn splice(
        seq: u64,
        target: &str,
        start: u64,
        removed: u64,
        outcome: Option<&str>,
        inserted: &[(&str, Option<&str>)],
    ) -> Value {
        json!({"seq":seq,"type":"agent/inbox/spliced","target":target,"start":start,"removedCount":removed,"outcome":outcome,
            "inserted":inserted.iter().map(|(id, rpc)| json!({"id":id,"rpcId":rpc})).collect::<Vec<_>>()})
    }
    fn user(seq: u64, id: &str, rpc: Option<&str>, digest: &str) -> Value {
        json!({"seq":seq,"type":"user/message","id":id,"rpcId":rpc,"digest":digest})
    }
    fn turn_end(seq: u64) -> Value {
        json!({"seq":seq,"type":"turn/end","turn":1,"reason":"error"})
    }
    fn facts(events: Vec<Value>, durable: bool, disposals: Value) -> Value {
        json!({"state":"tracked","messageId":"m1","spliceSeq":10,"durable":durable,"complete":true,"events":events,"disposals":disposals})
    }
    fn insert() -> Value {
        splice(10, "next-step", 1, 0, None, &[("m1", Some(REQUEST))])
    }
    fn claim(seq: u64) -> Value {
        splice(seq, "next-step", 0, 2, None, &[])
    }
    const DIGEST: &str = "d1";

    #[test]
    fn classifies_every_fixed_code_from_the_hosts_facts() {
        let settle =
            |events: Vec<Value>| classify(&facts(events, true, json!([])), REQUEST, DIGEST);
        // Delivered: claimed, then its own durable user/message with the offered digest.
        assert_eq!(
            settle(vec![
                insert(),
                claim(12),
                user(15, "m1", Some(REQUEST), DIGEST)
            ]),
            Settled::Received(15)
        );
        // Not durable yet: read again.
        assert_eq!(
            classify(
                &facts(
                    vec![insert(), claim(12), user(15, "m1", Some(REQUEST), DIGEST)],
                    false,
                    json!([])
                ),
                REQUEST,
                DIGEST
            ),
            Settled::Pending
        );
        // Recorded with other content: changed, never a receipt.
        assert_eq!(
            settle(vec![
                insert(),
                claim(12),
                user(15, "m1", Some(REQUEST), "edited")
            ]),
            Settled::Unknown(UnknownCode::Changed)
        );
        // A person's edit is one record that re-inserts the same message: still tracked.
        let edit = splice(
            11,
            "next-step",
            1,
            1,
            Some("canceled"),
            &[("m1", Some(REQUEST))],
        );
        assert_eq!(settle(vec![insert(), edit.clone()]), Settled::Pending);
        assert_eq!(
            settle(vec![
                insert(),
                edit,
                claim(12),
                user(15, "m1", Some(REQUEST), "edited")
            ]),
            Settled::Unknown(UnknownCode::Changed)
        );
        // A move: canceled, then re-inserted elsewhere; still tracked and delivered.
        let moved = vec![
            insert(),
            splice(11, "next-step", 1, 1, Some("canceled"), &[]),
            splice(12, "next-turn", 0, 0, None, &[("m1", Some(REQUEST))]),
        ];
        assert_eq!(settle(moved.clone()), Settled::Pending);
        let mut delivered = moved;
        delivered.extend([
            splice(13, "next-turn", 0, 1, None, &[]),
            user(14, "m1", Some(REQUEST), DIGEST),
        ]);
        assert_eq!(settle(delivered), Settled::Received(14));
        // Canceled without a re-insert: the host's own disposal, or anyone else's.
        let canceled = vec![
            insert(),
            splice(13, "next-step", 0, 2, Some("canceled"), &[]),
        ];
        assert_eq!(
            settle(canceled.clone()),
            Settled::NotOffered(NotOfferedCode::Canceled)
        );
        assert_eq!(
            classify(
                &facts(canceled.clone(), true, json!([[12, 14]])),
                REQUEST,
                DIGEST
            ),
            Settled::NotOffered(NotOfferedCode::HostDisposed)
        );
        assert_eq!(
            classify(&facts(canceled, true, json!([[14, 20]])), REQUEST, DIGEST),
            Settled::NotOffered(NotOfferedCode::Canceled)
        );
        // Claimed by a turn that ended without recording it: dropped, and final.
        assert_eq!(
            settle(vec![insert(), claim(12), turn_end(13)]),
            Settled::NotOffered(NotOfferedCode::DroppedAfterClaim)
        );
        // Queued or claimed and still running: pending.
        assert_eq!(settle(vec![insert()]), Settled::Pending);
        assert_eq!(settle(vec![insert(), claim(12)]), Settled::Pending);
        // Another message (a foreign re-send with the same request ID) is never this one.
        assert_eq!(
            settle(vec![
                insert(),
                splice(11, "next-step", 2, 0, None, &[("m2", Some(REQUEST))]),
                claim(12),
                user(15, "m2", Some(REQUEST), DIGEST)
            ]),
            Settled::Pending
        );
        // A removal before this message's position leaves it queued.
        assert_eq!(
            settle(vec![
                insert(),
                splice(11, "next-step", 0, 1, Some("canceled"), &[])
            ]),
            Settled::Pending
        );
        // A user/message of this message but another request is not a receipt.
        assert_eq!(
            settle(vec![
                insert(),
                claim(12),
                user(15, "m1", Some("other"), DIGEST)
            ]),
            Settled::Unknown(UnknownCode::Changed)
        );
        // No bound message: unseen.
        assert_eq!(
            classify(&json!({"state":"tracked","events":[]}), REQUEST, DIGEST),
            Settled::Unknown(UnknownCode::Unseen)
        );
    }

    /// A slice cut by the bridge's bound is no evidence of absence.
    #[test]
    fn a_truncated_slice_concludes_only_what_no_later_event_can_change() {
        let cut = |events: Vec<Value>| {
            let mut facts = facts(events, true, json!([[12, 14]]));
            facts["complete"] = json!(false);
            classify(&facts, REQUEST, DIGEST)
        };
        // The cap falls between a canceled removal and its re-insert: never NotOffered.
        let canceled = vec![
            insert(),
            splice(13, "next-step", 0, 2, Some("canceled"), &[]),
        ];
        assert_eq!(cut(canceled.clone()), Settled::Pending);
        assert_eq!(
            classify(&facts(canceled, true, json!([[12, 14]])), REQUEST, DIGEST),
            Settled::NotOffered(NotOfferedCode::HostDisposed)
        );
        // A receipt and a changed message wait for the complete slice too.
        assert_eq!(
            cut(vec![
                insert(),
                claim(12),
                user(15, "m1", Some(REQUEST), DIGEST)
            ]),
            Settled::Pending
        );
        assert_eq!(
            cut(vec![
                insert(),
                claim(12),
                user(15, "m1", Some(REQUEST), "edited")
            ]),
            Settled::Pending
        );
        // A claim, then turn/end without the message, is final from any prefix.
        assert_eq!(
            cut(vec![insert(), claim(12), turn_end(13)]),
            Settled::NotOffered(NotOfferedCode::DroppedAfterClaim)
        );
    }

    /// The bridge's own `input-witness` answers on the real harness, captured
    /// by tests/test_dsh_native.py NativeInputWitness.
    #[test]
    fn classifies_the_real_hosts_captured_facts() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/dsh-input-witness-facts.json");
        let fixture: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        // zerus-hfw2s.9: captured on the pinned reviewed host, with the disposed
        // record answered from its frozen final facts.
        assert_eq!(fixture["host"]["version"], "0.2.1-rc.2");
        assert_eq!(
            fixture["scenarios"]["host_disposed"]["facts"]["frozen"],
            true
        );
        let scenarios = fixture["scenarios"].as_object().unwrap();
        let expected = |code: &str| match code {
            "received" => None,
            "pending" => Some(Settled::Pending),
            other => Some(Settled::NotOffered(NotOfferedCode::parse(other).unwrap())),
        };
        for name in [
            "received",
            "pending",
            "host_disposed",
            "dropped_after_claim",
        ] {
            let case = &scenarios[name];
            let settled = classify(
                &case["facts"],
                string(case, "request_id"),
                string(case, "digest"),
            );
            match expected(string(case, "expected")) {
                None => assert!(
                    matches!(settled, Settled::Received(_)),
                    "{name}: {settled:?}"
                ),
                Some(want) => assert_eq!(settled, want, "{name}"),
            }
        }
    }

    #[test]
    fn content_digest_and_unsafe_content_cover_every_text_block() {
        let text = |t: &str| json!({"type":"text","text":t});
        assert_ne!(
            content_digest(&[text("ab")]),
            content_digest(&[text("a"), text("b")])
        );
        assert_eq!(
            content_digest(&[json!({"type":"image","data":"x"})]),
            content_digest(&[json!({"type":"image","data":"y"})])
        );
        assert!(unsafe_content(&[
            text("plain"),
            text("see dsh-session:session-1")
        ]));
        assert!(!unsafe_content(&[
            text("DSH-SESSION:x"),
            json!({"type":"image","name":"dsh-session:x"})
        ]));
        // The digest the bridge computes for the same blocks (tests/test_dsh_bridge.py InputWitness).
        assert_eq!(
            content_digest(&[text("WITNESS")]),
            format!("{:x}", Sha256::digest(b"text\0WITNESS\0"))
        );
    }

    /// Runs the flow fixture below in a child process with private state.
    #[test]
    fn submissions_refuse_before_the_marker_and_replay_recorded_outcomes() {
        let state = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "state::dsh::witness::tests::flow",
                "--ignored",
                "--test-threads=1",
            ])
            .env("HGS_STATE_DIR", state.path())
            .env("ZERUS_DSH_WITNESS_FIXTURE", "1")
            .env_remove("HGS_SESSION")
            .env_remove("HGS_SESSION_ID")
            .env_remove("HGS_RUN_ID")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("test result: ok. 1 passed"),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Windows' words for refusing to open a directory as a file:
    /// `ERROR_ACCESS_DENIED` (zerus-hfw2s.10).
    #[cfg(windows)]
    fn directory_open_refusal() -> String {
        std::io::Error::from_raw_os_error(
            windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32,
        )
        .to_string()
    }

    /// With the receipt reader's own options, Windows refuses to open a
    /// directory before any descriptor check, in the words the flow expects.
    #[cfg(windows)]
    #[test]
    fn windows_refuses_a_directory_at_the_receipt_open() {
        let folder = tempfile::tempdir().unwrap();
        let error = crate::platform::no_follow(fs::OpenOptions::new().read(true))
            .open(folder.path())
            .unwrap_err();
        assert_eq!(
            error.raw_os_error(),
            Some(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32)
        );
        assert_eq!(error.to_string(), directory_open_refusal());
    }

    #[test]
    #[ignore = "run by submissions_refuse_before_the_marker_and_replay_recorded_outcomes"]
    fn flow() {
        if std::env::var_os("ZERUS_DSH_WITNESS_FIXTURE").is_none() {
            return;
        }
        let mut record = json!({"name":"dsh/project/witness","backend":"dsh","agent":"dsh","conversation_id":"session-W","run_id":"run-W",
            "session_id":"22222222-3333-4444-8555-666666666666","agent_home":"/nonexistent/dsh-home","cwd":"/nonexistent","created":1.0});
        save(&mut record).unwrap();
        let name = "dsh/project/witness";
        let request = |text: &str| json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_run_id":"run-W","expected_conversation_id":"session-W","text":text});
        let sent_digest = content_digest(&[json!({"type":"text","text":"WITNESS"})]);
        // A scripted host: ping/inspect/send answer, input-witness answers `witness`.
        let run = |request: &Value,
                   ping: Value,
                   witness: std::result::Result<Value, String>,
                   deadline: Duration| {
            let mut calls = Vec::new();
            let outcome = submit_with(name, request, deadline, &mut |_, method, params, _| {
                calls.push(method.to_owned());
                match method {
                    "ping" => Ok(ping.clone()),
                    "inspect" => Ok(json!({"live":true,"authRequired":false})),
                    "send" => {
                        assert_eq!(params["requestId"], request["request_id"]);
                        Ok(json!({"accepted":true}))
                    }
                    "input-witness" => witness.clone(),
                    other => Err(format!("{other} was not expected")),
                }
            });
            (outcome, calls)
        };
        let receipt = |request: &Value| receipt_path(string(request, "request_id")).exists();
        let witnessing = json!({"generation":"g1","inputWitness":true});
        let tracked = |events: Vec<Value>| {
            Ok(
                json!({"generation":"g1","state":"tracked","messageId":"m1","spliceSeq":10,"through":20,"complete":true,
                "durable":true,"events":events,"disposals":[]}),
            )
        };
        // An older host without the capability: refused before the marker, nothing sent.
        let old = request("WITNESS");
        let (outcome, calls) = run(
            &old,
            json!({"generation":"g1"}),
            Err("unreachable".into()),
            Duration::from_secs(5),
        );
        assert!(
            matches!(&outcome, Witnessed::NotOffered { code: NotOfferedCode::HostRefused, reason, .. } if reason == NO_WITNESS),
            "{outcome:?}"
        );
        assert_eq!((calls, receipt(&old)), (vec!["ping".to_owned()], false));
        // A native session mention: refused before anything, the host not even asked.
        let mention = request("see dsh-session:session-1");
        let (outcome, calls) = run(
            &mention,
            witnessing.clone(),
            Err("unreachable".into()),
            Duration::from_secs(5),
        );
        assert_eq!(
            (outcome.code(), calls.len(), receipt(&mention)),
            ("unsafe_content", 0, false)
        );
        // A changed session: refused before the marker.
        let mut stale = request("WITNESS");
        stale["expected_run_id"] = json!("run-old");
        assert_eq!(
            run(
                &stale,
                witnessing.clone(),
                Err("unreachable".into()),
                Duration::from_secs(5)
            )
            .0
            .code(),
            "host_refused"
        );
        assert!(!receipt(&stale));
        // A binding without its run ID, and a request expecting exactly that:
        // refused before the host is asked, nothing written. The binding read
        // itself refuses one without a conversation ID.
        let incomplete = |label: &str, run: Value, conversation: Value, session: &str| {
            let mut record = json!({"name":label,"backend":"dsh","agent":"dsh","conversation_id":conversation,"run_id":run,
                "session_id":session,"agent_home":"/nonexistent/dsh-incomplete","cwd":"/nonexistent","created":1.0});
            save(&mut record).unwrap();
            let request = json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_run_id":record["run_id"],
                "expected_conversation_id":record["conversation_id"],"text":"WITNESS"});
            let mut calls = Vec::new();
            let outcome = submit_with(
                label,
                &request,
                Duration::from_secs(5),
                &mut |_, method, _, _| {
                    calls.push(method.to_owned());
                    match method {
                        "ping" => Ok(json!({"generation":"g1","inputWitness":true})),
                        "inspect" => Ok(json!({"live":true})),
                        "send" => Ok(json!({"accepted":true})),
                        other => Err(format!("{other} was not expected")),
                    }
                },
            );
            assert!(calls.is_empty(), "{calls:?}");
            assert!(!receipt(&request));
            match outcome {
                Witnessed::NotOffered {
                    code: NotOfferedCode::HostRefused,
                    reason,
                    ..
                } => reason,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(
            incomplete(
                "dsh/project/runless",
                Value::Null,
                json!("session-R"),
                "44444444-5555-4666-8777-888888888888"
            ),
            INCOMPLETE
        );
        assert_ne!(
            incomplete(
                "dsh/project/conversationless",
                json!("run-C"),
                Value::Null,
                "55555555-6666-4777-8888-999999999999"
            ),
            ""
        );
        // Received: the receipt records the witness, and a repeat replays it unsent.
        let good = request("WITNESS");
        let delivered = vec![
            insert(),
            claim(12),
            user(15, "m1", Some(string(&good, "request_id")), &sent_digest),
        ];
        let (outcome, calls) = run(
            &good,
            witnessing.clone(),
            tracked(delivered),
            Duration::from_secs(5),
        );
        let Witnessed::Receipt(stored) = &outcome else {
            panic!("{outcome:?}")
        };
        assert_eq!(
            (stored["status"].as_str(), stored["witness"]["seq"].as_u64()),
            (Some("submitted"), Some(15))
        );
        assert_eq!(calls, ["ping", "inspect", "send", "input-witness"]);
        let (again, calls) = run(
            &good,
            witnessing.clone(),
            Err("unreachable".into()),
            Duration::from_secs(5),
        );
        assert_eq!((again.code(), calls.len()), ("received", 0));
        // The host's own refusal of this exact request, after the marker.
        let refused = request("WITNESS");
        let answer = Ok(
            json!({"generation":"g1","state":"refused","reason":"Resume this session before sending a message"}),
        );
        let (outcome, _) = run(&refused, witnessing.clone(), answer, Duration::from_secs(5));
        assert!(
            matches!(&outcome, Witnessed::NotOffered { code: NotOfferedCode::HostRefused, binding, .. } if binding.request_id == string(&refused, "request_id"))
        );
        // Dropped after its claim: final, and replayed as such.
        let dropped = request("WITNESS");
        let (outcome, _) = run(
            &dropped,
            witnessing.clone(),
            tracked(vec![insert(), claim(12), turn_end(13)]),
            Duration::from_secs(5),
        );
        assert_eq!(outcome.code(), "dropped_after_claim");
        assert_eq!(
            run(
                &dropped,
                witnessing.clone(),
                Err("unreachable".into()),
                Duration::from_secs(5)
            )
            .0
            .code(),
            "dropped_after_claim"
        );
        // Still queued at the deadline: pending, never promoted by a later read.
        let queued = request("WITNESS");
        let (outcome, calls) = run(
            &queued,
            witnessing.clone(),
            tracked(vec![insert()]),
            Duration::from_millis(1500),
        );
        assert_eq!(
            (
                outcome.code(),
                calls.iter().filter(|c| *c == "send").count()
            ),
            ("pending", 1)
        );
        let later = vec![
            insert(),
            claim(12),
            user(15, "m1", Some(string(&queued, "request_id")), &sent_digest),
        ];
        let (again, calls) = run(
            &queued,
            witnessing.clone(),
            tracked(later),
            Duration::from_secs(5),
        );
        assert_eq!((again.code(), calls.len()), ("pending", 0));
        // A host that no longer answers, or no longer holds the request.
        let gone = request("WITNESS");
        assert_eq!(
            run(
                &gone,
                witnessing.clone(),
                Err("connection refused".into()),
                Duration::from_secs(5)
            )
            .0
            .code(),
            "host_gone"
        );
        let unseen = request("WITNESS");
        assert_eq!(
            run(
                &unseen,
                witnessing.clone(),
                Ok(json!({"generation":"g1","state":"unseen"})),
                Duration::from_secs(5)
            )
            .0
            .code(),
            "unseen"
        );
        // The same request ID with another payload or binding: an identity conflict,
        // refused before the marker, nothing sent, the stored outcome untouched.
        let kept = fs::read(receipt_path(string(&good, "request_id"))).unwrap();
        let mut reused = good.clone();
        reused["text"] = json!("WITNESS again");
        let mut rebound = good.clone();
        rebound["expected_run_id"] = json!("run-other");
        for conflict in [reused, rebound] {
            let (outcome, calls) = run(
                &conflict,
                witnessing.clone(),
                Err("unreachable".into()),
                Duration::from_secs(5),
            );
            assert!(
                matches!(&outcome, Witnessed::NotOffered { code: NotOfferedCode::HostRefused, reason, .. } if reason.contains("identity conflict")),
                "{outcome:?}"
            );
            assert!(calls.is_empty(), "{calls:?}");
            assert_eq!(recorded(name, &conflict).unwrap(), Recorded::Conflict);
        }
        assert_eq!(
            fs::read(receipt_path(string(&good, "request_id"))).unwrap(),
            kept
        );
        // Pages: the first is cut after a canceled removal, the next holds the
        // re-insert and delivery. Read on from the first page's end, then receive.
        let paged = request("WITNESS");
        let rid = string(&paged, "request_id").to_owned();
        let pages = std::cell::RefCell::new(vec![
            json!({"generation":"g1","state":"tracked","messageId":"m1","spliceSeq":10,"through":14,"complete":false,"durable":true,"disposals":[],
                "events":[insert(), splice(13, "next-step", 0, 2, Some("canceled"), &[])]}),
            json!({"generation":"g1","state":"tracked","messageId":"m1","spliceSeq":10,"through":18,"complete":true,"durable":true,"disposals":[],
                "events":[splice(14, "next-turn", 0, 0, None, &[("m1", Some(rid.as_str()))]), splice(15, "next-turn", 0, 1, None, &[]),
                    user(16, "m1", Some(rid.as_str()), &sent_digest)]}),
        ]);
        let mut froms = Vec::new();
        let outcome = submit_with(
            name,
            &paged,
            Duration::from_secs(5),
            &mut |_, method, params, _| match method {
                "ping" => Ok(witnessing.clone()),
                "inspect" => Ok(json!({"live":true})),
                "send" => Ok(json!({"accepted":true})),
                "input-witness" => {
                    froms.push(params["fromSeq"].clone());
                    Ok(pages.borrow_mut().remove(0))
                }
                other => Err(format!("{other} was not expected")),
            },
        );
        assert_eq!(outcome.code(), "received", "{outcome:?}");
        assert_eq!(froms, [Value::Null, json!(14)]);
        let Witnessed::Receipt(stored) = outcome else {
            unreachable!()
        };
        assert_eq!(stored["digest_scope"], DIGEST_SCOPE);
        // zerus-hfw2s.3: recorded() answers Absent, Conflict or the exact Outcome.
        let code = |recorded: Recorded| match recorded {
            Recorded::Outcome(witnessed) => witnessed.code(),
            other => panic!("{other:?}"),
        };
        assert_eq!(code(recorded(name, &good).unwrap()), "received");
        assert_eq!(
            code(recorded(name, &dropped).unwrap()),
            "dropped_after_claim"
        );
        assert_eq!(recorded(name, &old).unwrap(), Recorded::Absent);
        let mut other = good.clone();
        other["text"] = json!("another input under the same request ID");
        assert_eq!(recorded(name, &other).unwrap(), Recorded::Conflict);
        // A held request lock: recorded() waits for its holder and then sees the
        // marker the holder wrote, never Absent in between (as submit_with holds
        // the lock from before its marker until after its send).
        let held = request("WITNESS");
        let holder = request_lock(string(&held, "request_id")).unwrap();
        let (done, answered) = std::sync::mpsc::channel();
        let reader = {
            let (name, held) = (name.to_owned(), held.clone());
            std::thread::spawn(move || {
                let answer = recorded(&name, &held);
                done.send(()).unwrap();
                answer
            })
        };
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            answered.try_recv().is_err(),
            "recorded() answered while the lock was held"
        );
        atomic(
            &receipt_path(string(&held, "request_id")),
            &json!({"request_id":held["request_id"],"name":name,"run_id":"run-W","conversation_id":"session-W",
                "digest":request_digest(name, &held),"witness_version":1,"status":"pending"})
                .to_string(),
        )
        .unwrap();
        drop(holder);
        assert_eq!(code(reader.join().unwrap().unwrap()), "pending");
        // zerus-hfw2s.1: an earlier call may have offered exactly this input, so the
        // answer is unknown, never a refusal, and nothing is asked or written.
        let unknown_unsent = |request: &Value, detail: &str| {
            let path = receipt_path(string(request, "request_id"));
            let before = fs::read(&path).ok();
            let (outcome, calls) = run(
                request,
                witnessing.clone(),
                Err("unreachable".into()),
                Duration::from_secs(5),
            );
            assert!(
                matches!(&outcome, Witnessed::Unknown { code: UnknownCode::Unseen, detail: d } if d.starts_with(detail)),
                "{outcome:?}"
            );
            assert!(calls.is_empty(), "{calls:?}");
            assert_eq!(fs::read(&path).ok(), before);
        };
        // (a) Exactly this request, recorded by an ordinary send without a witness.
        let ordinary = request("WITNESS");
        let digest = request_digest(name, &ordinary);
        atomic(
            &receipt_path(string(&ordinary, "request_id")),
            &json!({"request_id":ordinary["request_id"],"name":name,"run_id":"run-W","conversation_id":"session-W",
                "digest":digest,"status":"submitted","transport":"native_dsh"})
                .to_string(),
        )
        .unwrap();
        unknown_unsent(&ordinary, UNWITNESSED);
        assert_eq!(code(recorded(name, &ordinary).unwrap()), "unseen");
        // (b) A receipt that cannot be read.
        let torn = request("WITNESS");
        atomic(&receipt_path(string(&torn, "request_id")), "{\"status\":").unwrap();
        unknown_unsent(&torn, UNREADABLE);
        assert!(recorded(name, &torn).is_err());
        // (b) A lock that cannot be taken: the store was never read.
        let blocked = request("WITNESS");
        let lock_path = root()
            .join("dsh/witness-locks")
            .join(format!("{}.lock", string(&blocked, "request_id")));
        fs::create_dir_all(&lock_path).unwrap();
        unknown_unsent(&blocked, UNREADABLE);
        // recorded() without its lock is an error, never Absent.
        assert!(recorded(name, &blocked).is_err());
        fs::remove_dir(&lock_path).unwrap();
        // zerus-hfw2s.6: a link or another non-regular file at the receipt path, or
        // a parsed value that is no well-formed receipt of this request, cannot
        // prove what was offered: unknown, no host call, nothing written.
        #[cfg(unix)]
        {
            let dangling = request("WITNESS");
            let path = receipt_path(string(&dangling, "request_id"));
            std::os::unix::fs::symlink(root().join("dsh/receipts/missing.json"), &path).unwrap();
            unknown_unsent(&dangling, UNREADABLE);
            assert!(fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink());
            assert!(recorded(name, &dangling).is_err());
            let linked = request("WITNESS");
            std::os::unix::fs::symlink(
                receipt_path(string(&good, "request_id")),
                receipt_path(string(&linked, "request_id")),
            )
            .unwrap();
            unknown_unsent(&linked, UNREADABLE);
            assert!(recorded(name, &linked).is_err());
        }
        for shape in ["{}", "[]", "\"submitted\"", "null"] {
            let odd = request("WITNESS");
            atomic(&receipt_path(string(&odd, "request_id")), shape).unwrap();
            unknown_unsent(&odd, UNREADABLE);
            assert!(recorded(name, &odd).is_err(), "{shape}");
        }
        let foreign = request("WITNESS");
        atomic(
            &receipt_path(string(&foreign, "request_id")),
            &json!({"request_id":uuid::Uuid::new_v4().to_string(),"name":name,"run_id":"run-W",
                "conversation_id":"session-W","digest":"x","witness_version":1,"status":"submitted"})
            .to_string(),
        )
        .unwrap();
        unknown_unsent(&foreign, UNREADABLE);
        // The opened descriptor is checked too: a regular file replaced by a
        // directory after the path check is no receipt, by that same check.
        // Windows refuses to open a directory as a file, so there the open's
        // own refusal answers first (zerus-hfw2s.10). Either way nothing is
        // sent and the input is never offered again.
        let swapped = request("WITNESS");
        let digest = request_digest(name, &swapped);
        let swapped_path = receipt_path(string(&swapped, "request_id"));
        atomic(
            &swapped_path,
            &json!({"request_id":swapped["request_id"],"name":name,"run_id":"run-W","conversation_id":"session-W",
                "digest":digest,"witness_version":1,"status":"submitted"})
            .to_string(),
        )
        .unwrap();
        BEFORE_OPEN.with(|hook| {
            hook.set(Some(Box::new(|path: &Path| {
                fs::remove_file(path).unwrap();
                fs::create_dir(path).unwrap();
            })))
        });
        let (outcome, calls) = run(
            &swapped,
            witnessing.clone(),
            Err("unreachable".into()),
            Duration::from_secs(5),
        );
        #[cfg(windows)]
        let refusal = directory_open_refusal();
        #[cfg(not(windows))]
        let refusal = NOT_A_FILE.to_owned();
        assert!(
            matches!(&outcome, Witnessed::Unknown { code: UnknownCode::Unseen, detail } if *detail == format!("{UNREADABLE}: {refusal}")),
            "{outcome:?}"
        );
        assert!(calls.is_empty(), "{calls:?}");
        assert!(swapped_path.is_dir());
        fs::remove_dir(&swapped_path).unwrap();
        // zerus-hfw2s.7: a marker write that failed after its rename is answered by
        // the store (pending, nothing sent); one that wrote nothing is a refusal.
        let faulted = request("WITNESS");
        MARKER_FAULT.with(|fault| fault.set(Some(true)));
        let (outcome, calls) = run(
            &faulted,
            witnessing.clone(),
            Err("unreachable".into()),
            Duration::from_secs(5),
        );
        assert!(
            matches!(&outcome, Witnessed::Unknown { code: UnknownCode::Pending, detail } if detail == MARKER_UNCERTAIN),
            "{outcome:?}"
        );
        assert_eq!(calls, ["ping", "inspect"]);
        assert_eq!(code(recorded(name, &faulted).unwrap()), "pending");
        let unwritten = request("WITNESS");
        MARKER_FAULT.with(|fault| fault.set(Some(false)));
        let (outcome, calls) = run(
            &unwritten,
            witnessing.clone(),
            Err("unreachable".into()),
            Duration::from_secs(5),
        );
        assert!(
            matches!(
                &outcome,
                Witnessed::NotOffered {
                    code: NotOfferedCode::HostRefused,
                    ..
                }
            ),
            "{outcome:?}"
        );
        assert_eq!(calls, ["ping", "inspect"]);
        assert!(!receipt_path(string(&unwritten, "request_id")).exists());
        assert_eq!(recorded(name, &unwritten).unwrap(), Recorded::Absent);
        // A crash's leftover lock file is used, then removed, by the next use.
        // Only Unix removes a lock file on release; elsewhere it is used and
        // kept (`RequestLock`'s Drop, zerus-hfw2s.10).
        let leftover = request("WITNESS");
        let lock_path = root()
            .join("dsh/witness-locks")
            .join(format!("{}.lock", string(&leftover, "request_id")));
        fs::write(&lock_path, b"").unwrap();
        assert_eq!(recorded(name, &leftover).unwrap(), Recorded::Absent);
        assert_eq!(lock_path.exists(), cfg!(not(unix)));
        // A lock on a file that a racing release removed or replaced after this
        // open is retried: the lock held is always on the file the path names.
        // Elsewhere no release removes the file, so the path always names it,
        // and a held lock still excludes another holder.
        #[cfg(not(unix))]
        {
            let held_request = request("WITNESS");
            let held_path = root()
                .join("dsh/witness-locks")
                .join(format!("{}.lock", string(&held_request, "request_id")));
            let held = request_lock(string(&held_request, "request_id")).unwrap();
            let other = fs::File::open(&held_path).unwrap();
            let error = fs2::FileExt::try_lock_exclusive(&other).unwrap_err();
            assert!(crate::platform::lock_contended(&error), "{error}");
            drop(other);
            drop(held);
            assert!(held_path.exists());
        }
        #[cfg(unix)]
        for replace in [false, true] {
            let raced = request("WITNESS");
            let raced_path = root()
                .join("dsh/witness-locks")
                .join(format!("{}.lock", string(&raced, "request_id")));
            AFTER_LOCK.with(|hook| {
                hook.set(Some(Box::new(move |path: &Path| {
                    fs::remove_file(path).unwrap();
                    if replace {
                        fs::write(path, b"").unwrap();
                    }
                })))
            });
            let held = request_lock(string(&raced, "request_id")).unwrap();
            let other = fs::File::open(&raced_path).unwrap();
            let error = fs2::FileExt::try_lock_exclusive(&other).unwrap_err();
            assert!(crate::platform::lock_contended(&error), "{error}");
            drop(other);
            drop(held);
            assert!(!raced_path.exists());
        }
        // Every lock taken above was released and removed. Elsewhere the files
        // stay, and each one is free to lock again.
        let locks: Vec<PathBuf> = fs::read_dir(root().join("dsh/witness-locks"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(locks.is_empty(), cfg!(unix), "{locks:?}");
        for lock in &locks {
            let file = fs::File::open(lock).unwrap();
            fs2::FileExt::try_lock_exclusive(&file).unwrap();
        }
        // Before concurrency(): its last test rebinds the name to another session.
        deadlines(name);
        concurrency(name);
    }

    /// zerus-hfw2s.8: the caller's deadline budgets the whole call.
    fn deadlines(name: &str) {
        let request = |text: &str| json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_run_id":"run-W","expected_conversation_id":"session-W","text":text});
        let witnessing = json!({"generation":"g1","inputWitness":true});
        // Another call holds this request: the wait ends at the deadline, unknown.
        // The holder is another thread that lets go after 2 s, so a wait that
        // ignored the deadline ends late instead of never.
        let held = request("WITNESS");
        let (taken, waiting) = std::sync::mpsc::channel();
        let (outcome, calls, waited) = std::thread::scope(|scope| {
            let id = string(&held, "request_id").to_owned();
            scope.spawn(move || {
                let holder = request_lock(&id).unwrap();
                taken.send(()).unwrap();
                std::thread::sleep(Duration::from_secs(2));
                drop(holder);
            });
            waiting.recv().unwrap();
            let (started, mut calls) = (Instant::now(), 0);
            let outcome = submit_with(
                name,
                &held,
                Duration::from_millis(300),
                &mut |_, _, _, _| {
                    calls += 1;
                    Err("unreachable".into())
                },
            );
            (outcome, calls, started.elapsed())
        });
        assert!(
            matches!(&outcome, Witnessed::Unknown { code: UnknownCode::Unseen, detail } if detail == LOCK_BUSY),
            "{outcome:?}"
        );
        assert_eq!(calls, 0);
        assert!(
            waited >= Duration::from_millis(300) && waited < Duration::from_secs(1),
            "{waited:?}"
        );
        // A host slower than the deadline before the marker: the deadline's own
        // refusal, every call bounded by it, nothing written.
        let early = request("WITNESS");
        let (started, mut timeouts) = (Instant::now(), Vec::new());
        let outcome = submit_with(
            name,
            &early,
            Duration::from_millis(300),
            &mut |_, _, _, timeout| {
                timeouts.push(timeout);
                let delay = Duration::from_secs(2);
                std::thread::sleep(delay.min(timeout));
                if delay > timeout {
                    return Err("read timed out".into());
                }
                Ok(json!({"generation":"g1","inputWitness":true}))
            },
        );
        let waited = started.elapsed();
        assert!(
            matches!(&outcome, Witnessed::NotOffered { code: NotOfferedCode::HostRefused, reason, .. } if reason == DEADLINE),
            "{outcome:?}"
        );
        assert!(waited < Duration::from_secs(1), "{waited:?}");
        assert!(
            timeouts
                .iter()
                .all(|timeout| *timeout <= Duration::from_millis(300)),
            "{timeouts:?}"
        );
        assert!(!receipt_path(string(&early, "request_id")).exists());
        assert_eq!(recorded(name, &early).unwrap(), Recorded::Absent);
        // Too little left to offer and read once: refused, nothing written.
        let short = request("WITNESS");
        let mut calls = Vec::new();
        let outcome = submit_with(
            name,
            &short,
            Duration::from_millis(800),
            &mut |_, method, _, _| {
                calls.push(method.to_owned());
                match method {
                    "ping" => Ok(witnessing.clone()),
                    "inspect" => Ok(json!({"live":true})),
                    other => Err(format!("{other} was not expected")),
                }
            },
        );
        assert!(
            matches!(&outcome, Witnessed::NotOffered { reason, .. } if reason == DEADLINE),
            "{outcome:?}"
        );
        assert_eq!(calls, ["ping", "inspect"]);
        assert!(!receipt_path(string(&short, "request_id")).exists());
        // After the marker the send and the reads share what is left: pending at
        // the deadline, one send, every timeout within the budget.
        let later = request("WITNESS");
        let (started, mut timeouts, mut sends) = (Instant::now(), Vec::new(), 0);
        let outcome = submit_with(
            name,
            &later,
            Duration::from_millis(1500),
            &mut |_, method, _, timeout| {
                timeouts.push(timeout);
                match method {
                    "ping" => Ok(witnessing.clone()),
                    "inspect" => Ok(json!({"live":true})),
                    "send" => {
                        sends += 1;
                        Ok(json!({"accepted":true}))
                    }
                    "input-witness" => Ok(
                        json!({"generation":"g1","state":"tracked","messageId":"m1","spliceSeq":10,"through":20,
                    "complete":true,"durable":true,"disposals":[],"events":[insert()]}),
                    ),
                    other => Err(format!("{other} was not expected")),
                }
            },
        );
        let waited = started.elapsed();
        assert_eq!((outcome.code(), sends), ("pending", 1), "{outcome:?}");
        assert!(waited < Duration::from_millis(2000), "{waited:?}");
        assert!(
            timeouts
                .iter()
                .all(|timeout| *timeout <= Duration::from_millis(1500)),
            "{timeouts:?}"
        );
    }

    /// Replaces a path in place (`BEFORE_OPEN`).
    pub(super) type PathSwap = Box<dyn FnOnce(&Path)>;

    thread_local! {
        /// Makes the next pending-marker write fail: `true` after its rename,
        /// `false` before writing anything (`write_marker`).
        pub(super) static MARKER_FAULT: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
        /// Runs once in `stored`, after its path check and before its open.
        pub(super) static BEFORE_OPEN: std::cell::Cell<Option<PathSwap>> = const { std::cell::Cell::new(None) };
        /// Runs once in `request_lock_until`, after its lock and before its path check.
        pub(super) static AFTER_LOCK: std::cell::Cell<Option<PathSwap>> = const { std::cell::Cell::new(None) };
    }

    /// A scripted host answering every call after `delay`, delivering
    /// `request`'s message, and counting the sends it receives.
    fn host(
        delay: Duration,
        request: Value,
        sends: &std::sync::atomic::AtomicUsize,
    ) -> impl FnMut(&Value, &str, Value, Duration) -> Result<Value> + '_ {
        let digest = content_digest(&[json!({"type":"text","text":string(&request, "text")})]);
        let answer = json!({"generation":"g1","state":"tracked","messageId":"m1","spliceSeq":10,"through":20,"complete":true,
            "durable":true,"disposals":[],"events":[insert(), claim(12), user(15, "m1", Some(string(&request, "request_id")), &digest)]});
        move |_, method, _, _| {
            std::thread::sleep(delay);
            match method {
                "ping" => Ok(json!({"generation":"g1","inputWitness":true})),
                "inspect" => Ok(json!({"live":true})),
                "send" => {
                    sends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(json!({"accepted":true}))
                }
                "input-witness" => Ok(answer.clone()),
                other => Err(format!("{other} was not expected")),
            }
        }
    }

    /// zerus-hfw2s.5: exclusivity per request ID, never per machine.
    fn concurrency(name: &str) {
        let request = |text: &str| json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_run_id":"run-W","expected_conversation_id":"session-W","text":text});
        let mut slow = json!({"name":"dsh/project/slow","backend":"dsh","agent":"dsh","conversation_id":"session-S","run_id":"run-S",
            "session_id":"33333333-4444-4555-8666-777777777777","agent_home":"/nonexistent/dsh-slow","cwd":"/nonexistent","created":1.0});
        save(&mut slow).unwrap();
        let sends = std::sync::atomic::AtomicUsize::new(0);
        // A slow host on session S does not delay a submit or recorded() on W.
        let mut slow_request = request("SLOW");
        slow_request["expected_run_id"] = json!("run-S");
        slow_request["expected_conversation_id"] = json!("session-S");
        std::thread::scope(|scope| {
            let slow_host = host(Duration::from_secs(3), slow_request.clone(), &sends);
            let slowly = scope.spawn(move || {
                let mut rpc = slow_host;
                submit_with(
                    "dsh/project/slow",
                    &slow_request,
                    Duration::from_secs(20),
                    &mut rpc,
                )
            });
            std::thread::sleep(Duration::from_millis(300));
            let fast = request("WITNESS");
            let started = Instant::now();
            assert_eq!(recorded(name, &fast).unwrap(), Recorded::Absent);
            let mut fast_host = host(Duration::ZERO, fast.clone(), &sends);
            assert_eq!(
                submit_with(name, &fast, Duration::from_secs(5), &mut fast_host).code(),
                "received"
            );
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "waited {:?} behind another session's host",
                started.elapsed()
            );
            assert_eq!(slowly.join().unwrap().code(), "received");
        });
        // The same request on two concurrent callers is offered exactly once.
        let same = request("WITNESS");
        let sends = std::sync::atomic::AtomicUsize::new(0);
        let outcomes: Vec<&'static str> = std::thread::scope(|scope| {
            let callers: Vec<_> = (0..2)
                .map(|_| {
                    let mut rpc = host(Duration::from_millis(300), same.clone(), &sends);
                    let same = same.clone();
                    scope.spawn(move || {
                        submit_with(name, &same, Duration::from_secs(20), &mut rpc).code()
                    })
                })
                .collect();
            callers
                .into_iter()
                .map(|caller| caller.join().unwrap())
                .collect()
        });
        assert_eq!(
            sends.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "{outcomes:?}"
        );
        assert!(outcomes.contains(&"received"), "{outcomes:?}");
        // recorded() waits for an in-flight submit of the same request and then
        // sees its marker, never Absent while it is being offered.
        let inflight = request("WITNESS");
        let sends = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let mut rpc = host(Duration::from_millis(600), inflight.clone(), &sends);
            let offering = inflight.clone();
            let submit = scope.spawn(move || {
                submit_with(name, &offering, Duration::from_secs(20), &mut rpc).code()
            });
            std::thread::sleep(Duration::from_millis(300));
            let answer = recorded(name, &inflight).unwrap();
            assert!(matches!(answer, Recorded::Outcome(_)), "{answer:?}");
            assert_eq!(submit.join().unwrap(), "received");
        });
        // A rename or forget racing the submit: the binding read before the
        // marker is the gate, and the offer and its witness stay with the
        // request's own conversation, never another session's.
        let raced = request("WITNESS");
        let mut calls = Vec::new();
        let outcome = submit_with(
            name,
            &raced,
            Duration::from_secs(5),
            &mut |_, method, params, _| {
                calls.push((method.to_owned(), params["sessionId"].clone()));
                match method {
                    "ping" => {
                        // The name now binds another session, as a forget and new launch would.
                        let mut other = json!({"name":name,"backend":"dsh","agent":"dsh","conversation_id":"session-OTHER","run_id":"run-OTHER",
                        "session_id":"44444444-5555-4666-8777-888888888888","agent_home":"/nonexistent/dsh-home","cwd":"/nonexistent","created":2.0});
                        save(&mut other).unwrap();
                        Ok(json!({"generation":"g1","inputWitness":true}))
                    }
                    "inspect" => Ok(json!({"live":true})),
                    "send" => Ok(json!({"accepted":true})),
                    "input-witness" => Ok(
                        json!({"generation":"g1","state":"tracked","messageId":"m1","spliceSeq":10,"through":20,"complete":true,
                    "durable":false,"disposals":[[12,14]],"events":[insert(), splice(13, "next-step", 0, 2, Some("canceled"), &[])]}),
                    ),
                    other => Err(format!("{other} was not expected")),
                }
            },
        );
        assert_eq!(outcome.code(), "host_disposed", "{outcome:?}");
        assert!(
            calls
                .iter()
                .filter(|(method, _)| method != "ping")
                .all(|(_, session)| session == "session-W"),
            "{calls:?}"
        );
        // With the name already bound to the other session: refused before the marker.
        let after = request("WITNESS");
        let outcome = submit_with(name, &after, Duration::from_secs(5), &mut |_, _, _, _| {
            Err("unreachable".into())
        });
        assert!(
            matches!(&outcome, Witnessed::NotOffered { code: NotOfferedCode::HostRefused, reason, .. } if reason.contains("Session changed")),
            "{outcome:?}"
        );
        assert!(!receipt_path(string(&after, "request_id")).exists());
    }

    /// Lock order (zerus-hfw2s.5): the witness path never takes the machine-wide
    /// bindings lock, so it can never hold a request lock while waiting for it.
    #[test]
    fn the_witness_path_never_takes_the_bindings_lock() {
        // A CRLF checkout (Windows) keeps the same split.
        let source = include_str!("witness.rs").replace("\r\n", "\n");
        let production = source.split("#[cfg(test)]\nmod tests").next().unwrap();
        assert!(!production.contains("bindings.lock"));
        assert!(!production.contains("lock(Some("));
        assert!(!production.contains("dispatch_lock::"));
        // Ordinary send serializes its own session before the shared request
        // check, without a machine-wide binding lock held across either wait.
        let dispatch = include_str!("../dsh.rs").split("pub(crate) fn dispatch(").nth(1).unwrap();
        let session = dispatch.find("dispatch_lock::acquire(name)").unwrap();
        let request = dispatch.find("witness::request_lock(").unwrap();
        assert!(session < request);
        assert!(!dispatch.contains("dsh/bindings.lock"));
        assert_eq!(dispatch.matches("witness::request_lock(").count(), 1);
    }
}
