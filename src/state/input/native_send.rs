//! Public-dispatch admission and bounded data for one pending-before-message
//! operation. None of these decoded values grants a native effect.
use super::*;
use crate::state::{repositories::directory::Directory, storage::native_domain::KindProof};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, io::Read};

pub(in crate::state) const REPLY_BYTES: usize = 1 << 20;
pub(in crate::state) const RAW_BYTES: usize = MAX_JSON;
pub(in crate::state) const ERROR_BYTES: usize = 16 << 10;
// Admission follows the measured costs: text keeps its ordinary limit;
// attachment publication count and decoded bytes have separate bounds.
pub(in crate::state) const PENDING_ATTACHMENTS: usize = 1;
pub(in crate::state) const PENDING_ATTACHMENT_BYTES: usize = 1 << 20;

#[derive(Deserialize)]
struct AttachmentSize<'a> {
    #[serde(borrow)]
    data_base64: Cow<'a, str>,
}

#[derive(Deserialize)]
struct Guard<'a> {
    #[serde(borrow)]
    request_id: Cow<'a, str>,
    #[serde(borrow)]
    expected_run_id: Cow<'a, str>,
    #[serde(default, borrow)]
    expected_conversation_id: Cow<'a, str>,
    #[serde(default, borrow)]
    expected_compaction_id: Cow<'a, str>,
    #[serde(default, borrow)]
    attachments: Vec<AttachmentSize<'a>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::state) struct Header {
    pub(in crate::state) dispatch_name: String,
    pub(in crate::state) agent: String,
    pub(in crate::state) request_id: String,
    pub(in crate::state) run: String,
    pub(in crate::state) conversation: String,
    pub(in crate::state) compaction: String,
    pub(in crate::state) raw_len: usize,
    pub(in crate::state) digest: String,
    pub(in crate::state) pending_id: String,
    pub(in crate::state) pending_digest: String,
    pub(in crate::state) machine: String,
}

fn hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn canonical(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok_and(|id| id.to_string() == s)
}
impl Header {
    pub(in crate::state) fn validate(&self) -> Result<()> {
        let bounded = |s: &str, n: usize| s.len() <= n && !s.chars().any(char::is_control);
        if self.raw_len == 0
            || self.raw_len > RAW_BYTES
            || !canonical(&self.request_id)
            || !canonical(&self.pending_id)
            || !hex(&self.digest)
            || !hex(&self.pending_digest)
            || self.dispatch_name.is_empty()
            || !bounded(&self.dispatch_name, 2048)
            || !matches!(self.agent.as_str(), "claude" | "codex" | "kimi")
            || self.run.is_empty()
            || !bounded(&self.run, 128)
            || !bounded(&self.conversation, 2048)
            || !bounded(&self.compaction, 128)
            || !uuid::Uuid::parse_str(&self.machine)
                .is_ok_and(|id| !id.is_nil() && id.to_string() == self.machine)
        {
            return Err("Invalid original Send envelope".into());
        }
        Ok(())
    }

    /// Called once at each end of the transport, never by an owner check.
    pub(in crate::state) fn verify_raw(&self, raw: &[u8]) -> Result<Payload> {
        self.validate()?;
        if raw.len() != self.raw_len || format!("{:x}", Sha256::digest(raw)) != self.digest {
            return Err("Original Send payload length or digest changed".into());
        }
        let payload = Payload::parse(raw)?;
        self.verify_payload(&payload)?;
        Ok(payload)
    }

    pub(in crate::state) fn verify_payload(&self, payload: &Payload) -> Result<()> {
        let r = &payload.request;
        if uuid::Uuid::parse_str(&r.request_id)
            .map_err(|e| e.to_string())?
            .to_string()
            != self.request_id
            || r.expected_run_id != self.run
            || r.expected_conversation_id != self.conversation
            || r.expected_compaction_id != self.compaction
            || !payload.admitted_attachments()
        {
            return Err("Original Send payload and envelope disagree".into());
        }
        Ok(())
    }
}

pub(in crate::state) struct Candidate {
    pub(in crate::state) record: Value,
    pub(in crate::state) header: Header,
    pub(in crate::state) payload: Payload,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::state) enum Reply {
    Submitted {
        receipt: Value,
        analytics: Option<crate::analytics::adapters::DeliveryNotice>,
    },
    CacheReplay {
        uuid: String,
        version: Value,
        sha256: String,
    },
    Failed {
        message: String,
        analytics: Option<crate::analytics::adapters::DeliveryNotice>,
    },
}

/// Reserve the exact JSON shape plus the longest finite f64 spelling for
/// each timestamp refreshed before paste. Framing is the i32 outcome and u32
/// byte count. No mutable timestamp can turn an admitted reply into overflow.
pub(in crate::state) fn check_reply_size(reply: &Reply) -> Result<()> {
    let value = serde_json::to_value(reply).map_err(|e| e.to_string())?;
    let mut bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?.len() + 8;
    let widest = serde_json::to_vec(&json!(-f64::MAX))
        .map_err(|e| e.to_string())?
        .len();
    for timestamp in [&value["receipt"]["submitted_at"], &value["analytics"]["at"]] {
        if timestamp.is_number() {
            bytes += widest
                - serde_json::to_vec(timestamp)
                    .map_err(|e| e.to_string())?
                    .len();
        }
    }
    if bytes > REPLY_BYTES {
        return Err("Send reply exceeds 1 MiB".into());
    }
    Ok(())
}

pub(in crate::state) fn file_version(file: &std::fs::File) -> Result<Value> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata().map_err(|e| e.to_string())?;
    Ok(json!([
        m.dev(),
        m.ino(),
        m.mode(),
        m.uid(),
        m.gid(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec()
    ]))
}

fn cached_bytes(
    header: &Header,
    expected: Option<&Value>,
    nonblocking: bool,
) -> Result<Option<(Value, String, Value)>> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = crate::platform::canonical(root())
        .map_err(|e| e.to_string())?
        .join("input_receipts")
        .join(format!("{}.json", header.request_id));
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
        Ok(_) => {}
    }
    // Intentionally follow symlinks and preserve existing()'s unbounded cache
    // schema/size semantics. No cached body goes through the bounded wire.
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    if nonblocking {
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = options.open(path).map_err(|e| e.to_string())?;
    let before = file_version(&file)?;
    if expected.is_some_and(|version| *version != before) {
        return Err("Original cached Send receipt changed".into());
    }
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(|e| e.to_string())?;
    if file_version(&file)? != before {
        return Err("Original cached Send receipt changed".into());
    }
    let value: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let sha = format!("{:x}", Sha256::digest(text.as_bytes()));
    Ok(Some((before, sha, value)))
}

pub(in crate::state) fn cached(header: &Header) -> Result<Option<Vec<u8>>> {
    let Some((version, sha256, value)) = cached_bytes(header, None, false)? else {
        return Ok(None);
    };
    match existing_value(value, &header.digest, &header.dispatch_name) {
        Existing::Submitted(_) => serde_json::to_vec(&Reply::CacheReplay {
            uuid: header.request_id.clone(),
            version,
            sha256,
        })
        .map(Some)
        .map_err(|e| e.to_string()),
        Existing::Unknown(error) => Err(error),
        Existing::Absent => Err("Original cached Send receipt is unavailable".into()),
    }
}

pub(in crate::state) fn reconcile(header: &Header) -> Result<Option<Value>> {
    let Some((_, _, value)) = cached_bytes(header, None, true)? else {
        return Ok(None);
    };
    match existing_value(value, &header.digest, &header.dispatch_name) {
        Existing::Submitted(value) => Ok(Some(value)),
        Existing::Unknown(_) | Existing::Absent => Ok(None),
    }
}

pub(in crate::state) fn decode(
    header: &Header,
    bytes: &[u8],
    analytics_root: Option<&Path>,
) -> Result<Value> {
    let reply: Reply = serde_json::from_slice(bytes).map_err(|e| uncertain(e.to_string()))?;
    match reply {
        Reply::Submitted { receipt, analytics } => {
            if let (Some(root), Some(notice)) = (analytics_root, analytics) {
                crate::analytics::adapters::emit_delivery_notice(root, notice);
            }
            Ok(receipt)
        }
        Reply::CacheReplay {
            uuid,
            version,
            sha256,
        } => {
            if uuid != header.request_id {
                return Err(uncertain("Original cached request changed".into()));
            }
            let Some((now, hash, value)) =
                cached_bytes(header, Some(&version), true).map_err(uncertain)?
            else {
                return Err(uncertain("Original cached Send receipt disappeared".into()));
            };
            if now != version || hash != sha256 {
                return Err(uncertain("Original cached Send receipt changed".into()));
            }
            match existing_value(value, &header.digest, &header.dispatch_name) {
                Existing::Submitted(value) => Ok(value),
                Existing::Unknown(error) => Err(uncertain(error)),
                Existing::Absent => Err(uncertain(
                    "Original cached Send receipt is unavailable".into(),
                )),
            }
        }
        Reply::Failed { message, analytics } => {
            if let (Some(root), Some(notice)) = (analytics_root, analytics) {
                crate::analytics::adapters::emit_delivery_notice(root, notice);
            }
            Err(uncertain(message))
        }
    }
}

/// Fully validated original input, including today's decoded attachment bounds.
pub(in crate::state) struct Payload {
    request: Request,
    files: Vec<Vec<u8>>,
}
impl Payload {
    fn admitted_attachments(&self) -> bool {
        if self.files.len() > PENDING_ATTACHMENTS {
            return false;
        }
        self.files
            .iter()
            .try_fold(0usize, |size, bytes| size.checked_add(bytes.len()))
            .is_some_and(|size| size <= PENDING_ATTACHMENT_BYTES)
    }

    pub(in crate::state) fn parse(raw: &[u8]) -> Result<Self> {
        if raw.len() > RAW_BYTES {
            return Err("message payload exceeds 29 MiB".into());
        }
        let request: Request =
            serde_json::from_slice(raw).map_err(|e| format!("invalid send payload: {e}"))?;
        let files = validate(&request)?;
        Ok(Self { request, files })
    }

    pub(in crate::state) fn files(&self) -> impl Iterator<Item = (&str, &str, &str, &[u8])> {
        self.request
            .attachments
            .iter()
            .zip(&self.files)
            .map(|(a, bytes)| {
                (
                    a.name.as_str(),
                    a.mime.as_str(),
                    a.reference.as_str(),
                    bytes.as_slice(),
                )
            })
    }

    pub(in crate::state) fn terminal_text(&self, paths: &[Value], agent: &str) -> (String, String) {
        let text = attachments::terminal_text(self.request.text.trim(), paths);
        let pasted = if (matches!(agent, "claude" | "kimi") && text.ends_with('\\'))
            || (agent == "codex"
                && text
                    .split_whitespace()
                    .next_back()
                    .is_some_and(|s| s.starts_with('@')))
        {
            format!("{text}\n")
        } else {
            text.clone()
        };
        (text, pasted)
    }

    pub(in crate::state) fn receipt(
        &self,
        header: &Header,
        record: &Value,
        paths: &[Value],
        text: &str,
        at: f64,
    ) -> Value {
        let mut value = json!({"request_id":header.request_id,"name":header.dispatch_name,"run_id":self.request.expected_run_id,
            "conversation_id":string(record,"conversation_id"),"first_message":string(record,"conversation_id").is_empty(),
            "digest":header.digest,"status":"in_progress","submitted_at":at,"submitted_text":text,
            "text":self.request.text.trim(),"attachments":paths,"transport":"terminal"});
        if let Some(target) = session_graph::target_receipt_for_machine(record, &header.machine) {
            value["graph_target"] = target;
        }
        value
    }
}

pub(in crate::state) fn pending_digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}

/// Pure metadata rule, deliberately identical to pending_applies readiness.
/// In particular Background work never enters this pending-only lane.
pub(in crate::state) fn ready(record: &Value) -> bool {
    // A malformed group sum must select legacy, never panic in disposition.
    if record["kimi_native_roster"] != true
        && record["subagent_groups"].as_object().is_some_and(|groups| {
            groups
                .values()
                .try_fold(0u64, |sum, group| {
                    sum.checked_add(group["active_count"].as_u64().unwrap_or(0))
                })
                .is_none()
        })
    {
        return false;
    }
    telemetry::settled(record) || first_message_ready(record)
}

/// Nullable native CID correlation never coerces malformed JSON into an
/// unprimed conversation. The retained record still compares its raw value.
pub(in crate::state) fn cid_matches(value: &Value, expected: &str) -> bool {
    match value {
        Value::Null => expected.is_empty(),
        Value::String(cid) => cid == expected,
        _ => false,
    }
}

fn candidate(record: &Value, guard: &Guard<'_>, pending_receipt: &Value) -> bool {
    let pending = &record["pending_settings"];
    let id = string(pending, "request_id");
    crate::state::storage::in_id_layout(record)
        && matches!(string(record, "agent"), "claude" | "codex" | "kimi")
        // Codex's unprimed startup-dialog checks and exact Settings walk do
        // not leave the measured margin. Preserve its ordinary send path.
        && (string(record, "agent") != "codex" || !string(record, "conversation_id").is_empty())
        && matches!(string(record, "backend"), "" | "tmux")
        && record["state"] != "archived"
        && record.get("archive_id").is_none()
        && record["creation_pending"] != true
        && record.get("pausing").is_none()
        && (record["conversation_id"].is_null() || record["conversation_id"].is_string())
        && !terminating::pending(record)
        && string(record, "error").is_empty()
        && !resume_input::pending(record)
        && string(record, "expected_id").is_empty()
        && string(record, "requested_id").is_empty()
        && !record
            .get("input_pending_at")
            .and_then(Value::as_f64)
            .is_some_and(|pending| record["last_event_at"].as_f64().unwrap_or(0.0) <= pending)
        && !settings_unconfirmed_metadata(record)
        && ready(record)
        && compact_context::check_continuation(record, &guard.expected_compaction_id).is_ok()
        && string(record, "run_id") == guard.expected_run_id
        && string(record, "conversation_id") == guard.expected_conversation_id
        && pending.is_object()
        && canonical(id)
        && string(pending, "run_id") == guard.expected_run_id
        && cid_matches(&pending["conversation_id"], &guard.expected_conversation_id)
        && pending_receipt["request_id"] == id
        && pending_receipt["status"] == "scheduled"
        && pending_receipt["model"] == pending["model"]
        && pending_receipt["effort"] == pending["effort"]
        && pending_receipt["name"] == pending["receipt_name"]
        && pending_receipt["run_id"] == pending["receipt_run_id"]
        && cid_matches(
            &pending_receipt["conversation_id"],
            &guard.expected_conversation_id,
        )
}

fn read_json(parent: &Directory, name: &str, limit: u64) -> Result<Value> {
    let mut file = parent.read_private(name)?;
    let before = crate::platform::FileIdentity::of(&file).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit
        || before != crate::platform::FileIdentity::of(&file).map_err(|e| e.to_string())?
    {
        return Err("Send disposition metadata changed".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

fn snapshot(name: &str, raw: &[u8]) -> Option<Candidate> {
    if raw.len() > RAW_BYTES {
        return None;
    }
    let guard: Guard<'_> = serde_json::from_slice(raw).ok()?;
    // This coarse pure gate avoids decoding requests that cannot fit. Exact
    // decoded sizes are checked on the validated Payload before selection.
    if guard.attachments.len() > PENDING_ATTACHMENTS
        || guard.attachments.iter().any(|attachment| {
            attachment.data_base64.len() > PENDING_ATTACHMENT_BYTES.div_ceil(3) * 4
        })
    {
        return None;
    }
    let request_id = uuid::Uuid::parse_str(&guard.request_id).ok()?.to_string();
    let direct = crate::state::storage::read_metadata(name);
    if direct.as_ref().is_ok_and(|r| {
        string(r, "run_id") == guard.expected_run_id
            && string(r, "conversation_id") == guard.expected_conversation_id
            && (!r["pending_settings"].is_object() || !ready(r))
    }) {
        return None;
    }
    let mut records = if !crate::state::session_id::has_deferred(name)
        && crate::state::session_id::selected().is_some()
    {
        vec![direct.ok()?]
    } else {
        let candidates = labels::candidates_metadata(name).ok()?;
        let mut records = candidates.by_id;
        if candidates.first {
            let first = direct.ok()?;
            if !crate::state::storage::in_id_layout(&first) {
                records.push(first);
            }
        }
        records
    };
    records.retain(|r| {
        string(r, "name") == name
            && string(r, "run_id") == guard.expected_run_id
            && string(r, "conversation_id") == guard.expected_conversation_id
    });
    if records.len() != 1 {
        return None;
    }
    let record = records.pop()?;
    // The common zero-pending case performs no proof, receipt or slot work.
    if !record["pending_settings"].is_object() || !ready(&record) {
        return None;
    }
    if !std::fs::symlink_metadata(
        crate::platform::canonical(root())
            .ok()?
            .join("input_receipts")
            .join(format!("{request_id}.json")),
    )
    .is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
    {
        return None;
    }
    let state = Directory::open(&crate::platform::canonical(root()).ok()?).ok()?;
    let inputs = state.child("native-input").ok()?;
    let proof = KindProof::open(&state, &inputs, string(&record, "session_id")).ok()?;
    proof.verify(&state, &inputs).ok()?;
    let receipts = state.child("settings_receipts").ok()?;
    let pending_receipt = read_json(
        &receipts,
        &format!("{}.json", string(&record["pending_settings"], "request_id")),
        storage::ID_RECORD_BYTES,
    )
    .ok()?;
    if !candidate(&record, &guard, &pending_receipt) {
        return None;
    }
    let payload = Payload::parse(raw).ok()?;
    if !payload.admitted_attachments() {
        return None;
    }
    let home = crate::platform::home()?;
    let catalog = Directory::open(
        &crate::platform::canonical(crate::config::config_dir(&home).join("swarm")).ok()?,
    )
    .ok()?;
    let mut file = catalog.read_private("catalog.json").ok()?;
    let before = file_version(&file).ok()?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(128 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 128 * 1024 * 1024 {
        return None;
    }
    if file_version(&file).ok()? != before
        || file_version(&catalog.read_private("catalog.json").ok()?).ok()? != before
    {
        return None;
    }
    let machine = crate::swarm::existing_node_id(&bytes).ok()?;
    let header = Header {
        dispatch_name: name.into(),
        agent: string(&record, "agent").into(),
        request_id,
        run: guard.expected_run_id.into_owned(),
        conversation: guard.expected_conversation_id.into_owned(),
        compaction: guard.expected_compaction_id.into_owned(),
        raw_len: raw.len(),
        digest: format!("{:x}", Sha256::digest(raw)),
        pending_id: string(&record["pending_settings"], "request_id").into(),
        pending_digest: pending_digest(&record["pending_settings"]),
        machine,
    };
    header.validate().ok()?;
    Some(Candidate {
        record,
        header,
        payload,
    })
}

pub(super) fn front(public: &PublicSend, name: &str, raw: &[u8]) -> Option<Result<Value>> {
    let candidate = snapshot(name, raw)?;
    // An unavailable account catalog is knowable before selection/offer.
    // Preserve the ordinary path, including its pre-input-receipt refusal.
    if !effort::pending_catalog_available(&candidate.record) {
        return None;
    }
    effort::front_pending_send(public, candidate, raw)
}

pub(in crate::state) fn uncertain(reason: String) -> String {
    if reason
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("delivery uncertain")
    {
        format!(
            "delivery uncertain{}",
            &reason.trim_start()["delivery uncertain".len()..]
        )
    } else {
        format!("delivery uncertain: {reason}")
    }
}

pub(in crate::state) fn bounded_error(mut reason: String) -> String {
    if reason.len() > ERROR_BYTES {
        let mut end = ERROR_BYTES - 32;
        while !reason.is_char_boundary(end) {
            end -= 1;
        }
        reason.truncate(end);
        reason.push_str("; further diagnostic omitted");
    }
    reason
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_non_admitted_submission_keeps_the_original_core_body() {
        let source = include_str!("../input.rs");
        let body = source
            .split_once("fn submit_core(")
            .unwrap()
            .1
            .split_once("/// Codex on Windows reads console key events")
            .unwrap()
            .0;
        let original = format!("fn submit_core({body}");
        assert_eq!(
            format!("{:x}", Sha256::digest(original.as_bytes())),
            "d788180c8a2af8b34c151f67f3cceb7e7c69275f8d19379a2b9b2894eb35e200"
        );
    }
    fn fixture() -> (Value, Value, Guard<'static>) {
        let pending = uuid::Uuid::new_v4().to_string();
        let record = json!({"session_id":uuid::Uuid::new_v4().to_string(),"layout":"id","agent":"kimi",
            "name":"kimi/example/ready","run_id":"run","conversation_id":"cid","activity":"idle","phase":"idle",
            "pending_settings":{"request_id":pending,"run_id":"run","conversation_id":"cid","model":"model","effort":"high","receipt_name":"kimi/example/ready","receipt_run_id":"run"}});
        let receipt = json!({"request_id":pending,"status":"scheduled","model":"model","effort":"high","name":"kimi/example/ready","run_id":"run","conversation_id":"cid"});
        let guard = Guard {
            request_id: Cow::Owned(uuid::Uuid::new_v4().to_string()),
            expected_run_id: Cow::Borrowed("run"),
            expected_conversation_id: Cow::Borrowed("cid"),
            expected_compaction_id: Cow::Borrowed(""),
            attachments: Vec::new(),
        };
        (record, receipt, guard)
    }
    #[test]
    fn disposition_admits_only_one_scheduled_exact_target() {
        let (record, receipt, guard) = fixture();
        assert!(candidate(&record, &guard, &receipt));
        for (key, value) in [
            ("pending_settings", Value::Null),
            ("agent", json!("dsh")),
            ("backend", json!("api")),
            ("layout", json!("legacy")),
            ("state", json!("archived")),
            ("archive_id", json!("saved")),
            ("creation_pending", json!(true)),
            ("pausing", Value::Null),
            ("error", json!("failed")),
            ("run_id", json!("other")),
            ("conversation_id", json!("other")),
            ("activity", json!("busy")),
            ("phase", json!("working")),
            ("input_pending_at", json!(5.0)),
            ("settings_unconfirmed_at", json!(5.0)),
        ] {
            let mut other = record.clone();
            other[key] = value;
            assert!(!candidate(&other, &guard, &receipt), "record gate: {key}");
        }
        for (key, value) in [
            ("request_id", json!(uuid::Uuid::new_v4().to_string())),
            ("status", json!("applied")),
            ("model", json!("other")),
            ("effort", json!("low")),
            ("name", json!("other")),
            ("run_id", json!("other")),
            ("conversation_id", json!("other")),
        ] {
            let mut other = receipt.clone();
            other[key] = value;
            assert!(!candidate(&record, &guard, &other), "receipt gate: {key}");
        }
        for value in [
            Value::Null,
            json!([]),
            json!(0),
            json!(true),
            json!("record"),
        ] {
            assert!(!candidate(&value, &guard, &receipt));
        }
    }
    #[test]
    fn background_with_one_pending_keeps_the_legacy_route() {
        let (record, receipt, guard) = fixture();
        for (key, value) in [
            ("active_tools", json!({"one":{}})),
            ("subagents", json!({"one":{"state":"working"}})),
            ("subagent_groups", json!({"one":{"active_count":1}})),
            (
                "subagent_groups",
                json!({"one":{"active_count":u64::MAX},"two":{"active_count":u64::MAX}}),
            ),
        ] {
            let mut record = record.clone();
            record[key] = value;
            assert!(
                !candidate(&record, &guard, &receipt),
                "Background gate: {key}"
            );
        }
    }
    #[test]
    fn unprimed_raw_null_and_empty_cids_are_not_fabricated() {
        let (mut record, mut receipt, mut guard) = fixture();
        record["run_identity_version"] = json!(1);
        record["supervisor"] = json!({});
        guard.expected_conversation_id = Cow::Borrowed("");
        receipt["conversation_id"] = json!("");
        record["pending_settings"]["conversation_id"] = json!("");
        for raw in [Value::Null, json!("")] {
            record["conversation_id"] = raw.clone();
            assert!(candidate(&record, &guard, &receipt));
            assert_eq!(record["conversation_id"], raw);
        }
        for raw in [json!(1), json!({}), json!([]), json!(true)] {
            record["conversation_id"] = raw;
            assert!(!candidate(&record, &guard, &receipt));
        }
    }
    #[test]
    fn unprimed_codex_keeps_legacy_while_unprimed_kimi_remains_eligible() {
        let (mut record, mut receipt, mut guard) = fixture();
        record["run_identity_version"] = json!(1);
        record["supervisor"] = json!({});
        guard.expected_conversation_id = Cow::Borrowed("");
        receipt["conversation_id"] = json!("");
        record["pending_settings"]["conversation_id"] = json!("");
        for cid in [Value::Null, json!("")] {
            record["conversation_id"] = cid;
            record["agent"] = json!("codex");
            assert!(!candidate(&record, &guard, &receipt));
            record["agent"] = json!("kimi");
            assert!(candidate(&record, &guard, &receipt));
        }
    }

    #[test]
    fn pending_and_scheduled_receipt_cids_reject_malformed_json_types() {
        let (mut record, mut receipt, mut guard) = fixture();
        record["run_identity_version"] = json!(1);
        record["supervisor"] = json!({});
        record["conversation_id"] = Value::Null;
        guard.expected_conversation_id = Cow::Borrowed("");
        for pending in [Value::Null, json!("")] {
            for scheduled in [Value::Null, json!("")] {
                record["pending_settings"]["conversation_id"] = pending.clone();
                receipt["conversation_id"] = scheduled;
                assert!(candidate(&record, &guard, &receipt));
            }
        }
        for malformed in [json!(1), json!({}), json!([]), json!(true)] {
            record["pending_settings"]["conversation_id"] = malformed.clone();
            receipt["conversation_id"] = Value::Null;
            assert!(
                !candidate(&record, &guard, &receipt),
                "pending CID: {malformed}"
            );
            record["pending_settings"]["conversation_id"] = Value::Null;
            receipt["conversation_id"] = malformed.clone();
            assert!(
                !candidate(&record, &guard, &receipt),
                "scheduled CID: {malformed}"
            );
        }
    }

    #[test]
    fn pending_send_admission_counts_files_and_decoded_bytes_without_limiting_text() {
        let mut request = json!({"request_id":uuid::Uuid::new_v4().to_string(),
            "expected_run_id":"r","text":"x".repeat(MAX_TEXT)});
        let raw = serde_json::to_vec(&request).unwrap();
        assert!(Payload::parse(&raw).unwrap().admitted_attachments());
        let file = |bytes: usize| {
            json!({"name":"a","mime":"",
            "data_base64":STANDARD.encode(vec![b'x';bytes])})
        };
        request["attachments"] = json!([file(PENDING_ATTACHMENT_BYTES)]);
        let raw = serde_json::to_vec(&request).unwrap();
        assert!(Payload::parse(&raw).unwrap().admitted_attachments());
        request["attachments"] = json!([file(PENDING_ATTACHMENT_BYTES + 1)]);
        let raw = serde_json::to_vec(&request).unwrap();
        assert!(!Payload::parse(&raw).unwrap().admitted_attachments());
        request["attachments"] = json!(vec![file(1); PENDING_ATTACHMENTS + 1]);
        let raw = serde_json::to_vec(&request).unwrap();
        assert!(!Payload::parse(&raw).unwrap().admitted_attachments());
        // The count gate precedes state reads and selection, even without a root.
        assert!(snapshot("codex/example/size", &raw).is_none());
        assert!(snapshot("codex/example/size", &vec![b' '; RAW_BYTES + 1]).is_none());
    }

    #[test]
    fn send_payload_preserves_raw_digest_and_existing_attachment_limits() {
        let raw=br#"{ "request_id":"00000000-0000-0000-0000-000000000000", "expected_run_id":"run", "text":"ordinary message\\" }"#;
        let payload = Payload::parse(raw).unwrap();
        let h = Header {
            dispatch_name: "claude/example/a".into(),
            agent: "claude".into(),
            request_id: "00000000-0000-0000-0000-000000000000".into(),
            run: "run".into(),
            conversation: String::new(),
            compaction: String::new(),
            raw_len: raw.len(),
            digest: format!("{:x}", Sha256::digest(raw)),
            pending_id: uuid::Uuid::new_v4().to_string(),
            pending_digest: "a".repeat(64),
            machine: uuid::Uuid::new_v4().to_string(),
        };
        assert!(h.verify_raw(raw).is_ok());
        for (key, value) in [
            ("request_id", json!(uuid::Uuid::new_v4().to_string())),
            ("expected_run_id", json!("other")),
            ("expected_conversation_id", json!("other")),
            ("expected_compaction_id", json!("other")),
        ] {
            let mut request: Value = serde_json::from_slice(raw).unwrap();
            request[key] = value;
            let payload = Payload::parse(&serde_json::to_vec(&request).unwrap()).unwrap();
            assert!(h.verify_payload(&payload).is_err(), "envelope gate: {key}");
        }
        let mut changed = raw.to_vec();
        changed.push(b' ');
        assert!(h.verify_raw(&changed).is_err());
        let (text, pasted) = payload.terminal_text(&[], "claude");
        assert_eq!(pasted, format!("{text}\n"));
        let receipt = payload.receipt(&h, &json!({"conversation_id":null}), &[], &text, 1.0);
        assert_eq!(receipt["digest"], h.digest);
        assert!(receipt["first_message"].as_bool().unwrap());
    }
    #[test]
    fn maximum_legal_payload_expands_each_hostile_attachment_path_once() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let encoded = STANDARD.encode(vec![b'x'; MAX_TOTAL / 8]);
        let references: Vec<_> = (1..=8).map(|i| format!("[File #{i}]")).collect();
        let label_text = references.join(" ");
        let mut text = label_text.repeat(MAX_TEXT / label_text.len() + 1);
        text.truncate(MAX_TEXT);
        let files: Vec<_> = references.iter().enumerate().map(|(i, reference)| json!({
            "name":format!("file-{i}.txt"),"mime":"text/plain","reference":reference,"data_base64":encoded,
        })).collect();
        let mut request = json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_run_id":"run",
            "expected_conversation_id":"cid","text":text,"attachments":files});
        let raw = serde_json::to_vec(&request).unwrap();
        assert!(raw.len() <= RAW_BYTES);
        let payload = Payload::parse(&raw).unwrap();
        assert_eq!(
            payload
                .files()
                .map(|(_, _, _, bytes)| bytes.len())
                .sum::<usize>(),
            MAX_TOTAL
        );
        let paths: Vec<_> = references.iter().enumerate().map(|(i, reference)|json!({
            "name":format!("file-{i}.txt"),"mime":"text/plain","reference":reference,
            "path":format!("/private/{}file-{i}.txt", "\\\"".repeat(1900)),"bytes":MAX_TOTAL/8,
        })).collect();
        let (submitted, pasted) = payload.terminal_text(&paths, "codex");
        assert!(pasted.len() <= 1 << 20);
        for path in &paths {
            let escaped = serde_json::to_string(&path["path"]).unwrap();
            assert_eq!(submitted.matches(&escaped).count(), 1);
        }
        let header = Header {
            dispatch_name: "codex/example/max".into(),
            agent: "codex".into(),
            request_id: request["request_id"].as_str().unwrap().into(),
            run: "run".into(),
            conversation: "cid".into(),
            compaction: String::new(),
            raw_len: raw.len(),
            digest: format!("{:x}", Sha256::digest(&raw)),
            pending_id: uuid::Uuid::new_v4().to_string(),
            pending_digest: "a".repeat(64),
            machine: uuid::Uuid::new_v4().to_string(),
        };
        let receipt = payload.receipt(
            &header,
            &json!({"conversation_id":"cid","agent":"codex"}),
            &paths,
            &submitted,
            1.0,
        );
        check_reply_size(&Reply::Submitted {
            receipt,
            analytics: None,
        })
        .unwrap();
        request["text"] = json!("x".repeat(MAX_TEXT + 1));
        assert!(Payload::parse(&serde_json::to_vec(&request).unwrap()).is_err());
        request["text"] = json!("");
        request["attachments"][7]["data_base64"] =
            json!(STANDARD.encode(vec![b'x'; MAX_TOTAL / 8 + 1]));
        assert!(Payload::parse(&serde_json::to_vec(&request).unwrap()).is_err());
        request["attachments"] = json!([]);
        request["text"] = json!("x");
        let mut oversized = serde_json::to_vec(&request).unwrap();
        oversized.resize(RAW_BYTES + 1, b' ');
        assert!(Payload::parse(&oversized).is_err());
    }

    #[test]
    fn reply_size_counts_framing_escaping_and_refreshed_timestamps() {
        let mut reply = Reply::Failed {
            message: String::new(),
            analytics: None,
        };
        let overhead = serde_json::to_vec(&reply).unwrap().len() + 8;
        if let Reply::Failed { message, .. } = &mut reply {
            *message = "x".repeat(REPLY_BYTES - overhead);
        }
        assert!(check_reply_size(&reply).is_ok());
        if let Reply::Failed { message, .. } = &mut reply {
            message.push('x');
        }
        assert!(check_reply_size(&reply).is_err());
        if let Reply::Failed { message, .. } = &mut reply {
            *message = "\u{1}".repeat((REPLY_BYTES - overhead) / 6 + 1);
        }
        assert!(check_reply_size(&reply).is_err());
        let receipt = json!({"submitted_at":1.0,"status":"submitted","padding":""});
        let analytics = Some(crate::analytics::adapters::DeliveryNotice::Event {
            request_id: "id".into(),
            agent: "claude".into(),
            conversation: "cid".into(),
            child: String::new(),
            at: 1.0,
            outcome: crate::analytics::adapters::Delivery::Submitted,
        });
        let reply = Reply::Submitted { receipt, analytics };
        assert!(check_reply_size(&reply).is_ok());
        // Pin the exact boundary with both refreshed timestamp spellings.
        let mut value = serde_json::to_value(&reply).unwrap();
        let widest = serde_json::to_vec(&json!(-f64::MAX)).unwrap().len();
        let reserved = serde_json::to_vec(&value).unwrap().len() + 8 + 2 * (widest - 3);
        value["receipt"]["padding"] = json!("x".repeat(REPLY_BYTES - reserved));
        let boundary: Reply = serde_json::from_value(value.clone()).unwrap();
        assert!(check_reply_size(&boundary).is_ok());
        let mut over = value.clone();
        over["receipt"]["padding"] = json!(format!(
            "{}x",
            value["receipt"]["padding"].as_str().unwrap()
        ));
        assert!(check_reply_size(&serde_json::from_value(over).unwrap()).is_err());
        value["receipt"]["submitted_at"] = json!(f64::MAX);
        value["analytics"]["at"] = json!(f64::MAX);
        assert!(serde_json::to_vec(&value).unwrap().len() + 8 <= REPLY_BYTES);
        assert!(check_reply_size(&serde_json::from_value(value).unwrap()).is_ok());
    }

    #[test]
    fn uncertain_prefix_and_error_reply_bound_are_stable() {
        assert_eq!(
            uncertain("delivery uncertain: draft".into()),
            "delivery uncertain: draft"
        );
        assert_eq!(
            uncertain("Delivery uncertain: draft".into()),
            "delivery uncertain: draft"
        );
        assert!(uncertain("draft".into()).starts_with("delivery uncertain:"));
        let error = bounded_error("界".repeat(ERROR_BYTES));
        assert!(error.len() <= ERROR_BYTES);
        let bytes = serde_json::to_vec(&Reply::Failed {
            message: error,
            analytics: None,
        })
        .unwrap();
        assert!(bytes.len() + 8 <= REPLY_BYTES);
    }
}
