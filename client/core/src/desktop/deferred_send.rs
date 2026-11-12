//! Exact input for the service-owned deferred send (zerus-5wmh.66.4.4, slice
//! 3, its first kind): one original `hgs send <session> --json` with the exact
//! stdin the client built for the deferred-input checkpoint it reserved and
//! saved with its draft, then only scoped reads of its result. The result is
//! reconciled from the CLI's own `input_receipts` record of that request ID,
//! never from the child's exit or its reply.
//!
//! These closed values carry no current writer, frontend, profile, exclusion
//! or execution authority: only the same current native action that durably
//! reserves the checkpoint supplies a one-shot admission. An imported,
//! recovered, reconnected or offered-Unknown original gets the read-only
//! lookup only.
//!
//! v1 carries a plain send to a local session: no subagent target, no
//! compaction (`compact_to_send` is its own kind), no `@alias` or other
//! machine.
use super::question_route;
use crate::{creation, operation_recovery::deferred_input};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use super::creation_clone::Scope;

pub const ABI: &str = "desktop-deferred-send-v1";
pub const HELP_MARKER: &str = "Desktop deferred send ABI: desktop-deferred-send-v1";
pub const PROFILE: &str = "deferred-send-v1";
pub const REPLY_BYTES: usize = 24 << 10;
/// The receipt status the CLI writes after the input's Enter: a submission
/// to the terminal, never proof that the agent read it.
pub const SUBMITTED: &str = "submitted";
/// The closed code of an original child that never started: refused before
/// any child existed. Nothing ran, so the message counts as never sent.
pub const NOT_STARTED: &str = "deferred_send_not_started";
/// The service refused the original before its ledger admission, with
/// its ledger proving no row for the request, and sealed the request for
/// its epoch, so no copy of it ever admits (zerus-tlb9q). The one refusal
/// that settles as never started; every other refusal leaves only the
/// read-only lookup.
pub const NOT_ADMITTED: &str = "deferred_send_not_admitted";
/// The largest CLI input receipt read.
pub const RECEIPT_BYTES: usize = 256 << 10;
/// The CLI receipt says `in_progress`: the input may have reached the
/// terminal without its Enter.
pub const RECEIPT_IN_PROGRESS: &str = "deferred_send_in_progress";
/// The CLI receipt is not this original's: another session, run,
/// conversation, stdin, transport or status.
pub const RECEIPT_MISMATCH: &str = "deferred_send_receipt_mismatch";
/// The CLI receipt is oversized or not a readable receipt.
pub const RECEIPT_INVALID: &str = "deferred_send_receipt_invalid";
/// The session is not a terminal agent's. An OpenCode or DeepSeek session's
/// `hgs send` goes to its own native backend, which writes its own receipts
/// and has its own rules, never `input_receipts`; such a send stays on the
/// direct path, decided before any offer.
pub const AGENT_REFUSED: &str = "deferred_send_agent_refused";
/// The CLI's own bounds on one send, mirrored so that an original the CLI
/// would refuse never spawns.
const MAX_TEXT: usize = 64 * 1024;
const MAX_FILES: usize = 8;
const MAX_FILE: usize = 10 * 1024 * 1024;
const MAX_TOTAL: usize = 20 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The person's deferred send: admitted only by their native UI permit
    /// for a draft checkpoint ([`Intent::verify`]).
    DeferredSend,
    /// The owner-approved, service-originated route of a supervised child's
    /// blocking question to its live parent (zerus-sa9qn.8): the service
    /// originates it under the owner's standing approval, never under a
    /// person's consent or UI permit ([`Intent::verify_route`]).
    RouteQuestion,
}
/// The domain of a route's authority digest, which a route's binding keeps
/// in its `consent_sha256` field: the owner-approved route authority, never a
/// person's consent.
const ROUTE_AUTHORITY: &[u8] = b"desktop-route-question-authority-v1\0";
/// The largest route checkpoint ([`question_route::Record`]).
const ROUTE_CHECKPOINT_BYTES: usize = 8 << 10;

/// One deferred send: the exact checkpoint the client saved with its draft
/// before this dispatch, and the exact stdin of `hgs send <session> --json`.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    pub scope: Scope,
    /// The local session the send names.
    pub session: String,
    /// The request ID the checkpoint reserved, which the stdin carries.
    pub request_id: String,
    /// The checkpoint's exact JSON.
    pub checkpoint: String,
    /// The exact stdin.
    pub original: String,
}
impl std::fmt::Debug for Intent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeferredSendIntent")
            .field("session", &self.session)
            .field("request_id", &self.request_id)
            .finish_non_exhaustive()
    }
}

/// `hgs send`'s stdin attachment, exactly as the CLI reads it.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Attachment {
    name: String,
    mime: String,
    data_base64: String,
    #[serde(default)]
    reference: String,
}
/// `hgs send`'s stdin, exactly as the CLI reads it.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Send {
    request_id: String,
    text: String,
    expected_run_id: String,
    #[serde(default)]
    expected_conversation_id: String,
    #[serde(default)]
    attachments: Vec<Attachment>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    expected_compaction_id: String,
}

/// The fixed effect: `hgs send <session> --json` with the stdin whose SHA-256
/// is `input_sha256`, which is also the digest the CLI's receipt records.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub session: String,
    pub request_id: String,
    pub run_id: String,
    pub conversation_id: String,
    pub message_sha256: String,
    pub input_sha256: String,
}
impl Plan {
    pub fn valid(&self) -> bool {
        local_session(&self.session)
            && uuid(&self.request_id)
            && identity(&self.run_id)
            && !self.run_id.is_empty()
            && identity(&self.conversation_id)
            && super::digest(&self.message_sha256)
            && super::digest(&self.input_sha256)
    }
    pub fn fingerprint(&self) -> Option<String> {
        self.valid()
            .then(|| hash(&serde_json::to_vec(self).expect("fixed send plan")))
    }
    /// The one original call's arguments.
    pub fn args(&self) -> [String; 3] {
        ["send".into(), self.session.clone(), "--json".into()]
    }
}

/// Persisted per-kind binding: the request, its target and the raw,
/// checkpoint, message and input digests, unrelated to every other kind.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub kind: Kind,
    pub schema: u32,
    pub configuration: String,
    pub session: String,
    pub request_id: String,
    pub run_id: String,
    pub conversation_id: String,
    pub source_sha256: String,
    pub checkpoint_sha256: String,
    pub message_sha256: String,
    pub input_sha256: String,
    pub plan_sha256: String,
    /// A deferred send's digest of the person's consent (its checkpoint and
    /// stdin); a route's digest of the owner-approved route authority over
    /// its record and stdin, never a person's consent.
    pub consent_sha256: String,
}
impl Binding {
    /// Either kind; the kind decides which verification re-derives it.
    pub fn valid(&self) -> bool {
        self.schema == 1
            && super::digest(&self.configuration)
            && local_session(&self.session)
            && uuid(&self.request_id)
            && identity(&self.run_id)
            && !self.run_id.is_empty()
            && identity(&self.conversation_id)
            && [
                &self.source_sha256,
                &self.checkpoint_sha256,
                &self.message_sha256,
                &self.input_sha256,
                &self.plan_sha256,
                &self.consent_sha256,
            ]
            .into_iter()
            .all(|v| super::digest(v))
    }
    pub fn operation_id(&self) -> Option<String> {
        self.valid().then(|| {
            let mut bytes = b"desktop-deferred-send-key-v1\0".to_vec();
            bytes.extend(
                serde_json::to_vec(&(&self.configuration, &self.session, &self.request_id))
                    .expect("fixed send key"),
            );
            hash(&bytes)
        })
    }
    pub fn fingerprint(&self) -> Option<String> {
        self.valid()
            .then(|| hash(&serde_json::to_vec(self).expect("fixed send binding")))
    }
    /// Whether this binding names exactly `plan`: every field it repeats,
    /// and the plan's own fingerprint. A declared digest never overrides a
    /// conflicting original field.
    pub fn agrees(&self, plan: &Plan) -> bool {
        self.session == plan.session
            && self.request_id == plan.request_id
            && self.run_id == plan.run_id
            && self.conversation_id == plan.conversation_id
            && self.message_sha256 == plan.message_sha256
            && self.input_sha256 == plan.input_sha256
            && plan.fingerprint().as_deref() == Some(self.plan_sha256.as_str())
    }
}

pub struct Verified {
    pub intent: Intent,
    pub plan: Plan,
    pub binding: Binding,
}

/// A session name as the launch records hold it, on this computer only: v1
/// refuses any `@alias` or other-machine target.
fn local_name(value: &str) -> bool {
    super::creation_launch::session_name(value) && !value.contains('@')
}
/// Whether the session's agent is a terminal agent
/// ([`super::creation_launch::AGENTS`]: Codex, Claude, Kimi), whose `hgs send`
/// types into the terminal and writes `input_receipts`.
fn terminal_agent(value: &str) -> bool {
    value
        .split('/')
        .next()
        .is_some_and(|agent| super::creation_launch::AGENTS.contains(&agent))
}
/// A local terminal agent's session: the only target of v1.
fn local_session(value: &str) -> bool {
    local_name(value) && terminal_agent(value)
}
fn uuid(value: &str) -> bool {
    creation::canonical_uuid(value) && value != "00000000-0000-0000-0000-000000000000"
}
/// A run, conversation or session identity as the CLI bounds it.
fn identity(value: &str) -> bool {
    value.len() <= 128 && !value.chars().any(char::is_control)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The CLI's own `validate()` of `hgs send`, mirrored: the decoded
/// attachments, or `None` for a message the CLI would refuse.
fn cli_rules(send: &Send) -> Option<Vec<Vec<u8>>> {
    let text = &send.text;
    if send.expected_run_id.is_empty()
        || send.expected_run_id.len() > 128
        || text.len() > MAX_TEXT
        || text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return None;
    }
    // A slash command's first token is /name; an absolute path has another
    // slash in that token and is ordinary text.
    let start = text.trim_start();
    let first = start.split_whitespace().next().unwrap_or("");
    if first
        .strip_prefix('/')
        .is_some_and(|name| !name.contains('/'))
        || start.starts_with('!')
    {
        return None;
    }
    if send.attachments.len() > MAX_FILES || (text.trim().is_empty() && send.attachments.is_empty())
    {
        return None;
    }
    let mut references = std::collections::HashSet::new();
    let mut total = 0;
    let mut files = Vec::with_capacity(send.attachments.len());
    for attachment in &send.attachments {
        let reference = attachment.reference.as_str();
        if !reference.is_empty() && (!reference_label(reference) || !references.insert(reference)) {
            return None;
        }
        if attachment.name.is_empty()
            || attachment.name.len() > 255
            || attachment.name.chars().any(char::is_control)
            || attachment.mime.len() > 100
            || attachment.mime.chars().any(char::is_control)
            || attachment.data_base64.len() > MAX_FILE.div_ceil(3) * 4
        {
            return None;
        }
        let bytes = STANDARD.decode(&attachment.data_base64).ok()?;
        if bytes.is_empty() || bytes.len() > MAX_FILE {
            return None;
        }
        total += bytes.len();
        if total > MAX_TOTAL {
            return None;
        }
        files.push(bytes);
    }
    Some(files)
}
/// A frozen `hgs send` stdin as the CLI takes it: its identities and the
/// digest of its exact message. `compact_to_send` holds its send to exactly
/// these rules.
pub(super) struct Frozen {
    pub request_id: String,
    pub run_id: String,
    pub conversation_id: String,
    pub compaction_id: String,
    pub message_sha256: String,
}
/// Why a stdin is not a send the CLI takes: not its strict shape, or a
/// message its own rules refuse.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Unsendable {
    Invalid,
    Refused,
}
/// Decodes `original` strictly and applies the CLI's own rules.
pub(super) fn frozen(original: &str) -> Result<Frozen, Unsendable> {
    let send: Send = serde_json::from_str(original).map_err(|_| Unsendable::Invalid)?;
    let files = cli_rules(&send).ok_or(Unsendable::Refused)?;
    let message_sha256 = deferred_input::message_sha256(
        &send.text,
        send.attachments.iter().zip(&files).map(|(a, bytes)| {
            (
                a.name.as_str(),
                a.mime.as_str(),
                a.reference.as_str(),
                bytes.as_slice(),
            )
        }),
    );
    Ok(Frozen {
        request_id: send.request_id,
        run_id: send.expected_run_id,
        conversation_id: send.expected_conversation_id,
        compaction_id: send.expected_compaction_id,
        message_sha256,
    })
}

/// The send `template` names `original` as its run; the same send naming
/// `run` instead, every other field exactly as decoded. `session_resume`
/// binds its send to the resumed run this way, and only this way. A template
/// the CLI would not take, or one naming another run, is refused.
pub(super) fn rebound(template: &str, original: &str, run: &str) -> Result<String, Unsendable> {
    let mut send: Send = serde_json::from_str(template).map_err(|_| Unsendable::Invalid)?;
    cli_rules(&send).ok_or(Unsendable::Refused)?;
    if send.expected_run_id != original
        || run.is_empty()
        || run.len() > 128
        || run.chars().any(char::is_control)
    {
        return Err(Unsendable::Invalid);
    }
    send.expected_run_id = run.into();
    serde_json::to_string(&send).map_err(|_| Unsendable::Invalid)
}

/// `[Image #n]` or `[File #n]`, n from 1 with at most nine digits.
fn reference_label(value: &str) -> bool {
    let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) else {
        return false;
    };
    let Some(number) = inner
        .strip_prefix("Image #")
        .or_else(|| inner.strip_prefix("File #"))
    else {
        return false;
    };
    (1..=9).contains(&number.len())
        && number.bytes().all(|b| b.is_ascii_digit())
        && !number.starts_with('0')
}

impl Intent {
    /// Verifies a saved original by the kind its binding was admitted as.
    pub fn verify_kind(self, kind: Kind) -> Result<Verified, &'static str> {
        match kind {
            Kind::DeferredSend => self.verify(),
            Kind::RouteQuestion => self.verify_route(),
        }
    }
    /// The owner-approved route of a supervised child's question
    /// (zerus-sa9qn.8). The checkpoint is the route's canonical
    /// [`question_route::Record`] and the stdin exactly the one it derives,
    /// so a route can carry no other text, target or identity. No person's
    /// permit, draft or consent is involved or implied.
    pub fn verify_route(self) -> Result<Verified, &'static str> {
        if !self.scope.valid()
            || !local_session(&self.session)
            || !uuid(&self.request_id)
            || self.checkpoint.is_empty()
            || self.checkpoint.len() > ROUTE_CHECKPOINT_BYTES
            || self.original.is_empty()
        {
            return Err("invalid_question_route_intent");
        }
        let record: question_route::Record =
            serde_json::from_str(&self.checkpoint).map_err(|_| "invalid_question_route_record")?;
        if !record.valid()
            || serde_json::to_string(&record).ok().as_deref() != Some(self.checkpoint.as_str())
            || record.request_id() != self.request_id
            || record.parent.session != self.session
        {
            return Err("invalid_question_route_record");
        }
        if record.stdin().as_deref() != Some(self.original.as_str()) {
            return Err("question_route_original_changed");
        }
        let send = frozen(&self.original).map_err(|_| "question_route_message_refused")?;
        if send.request_id != self.request_id
            || send.run_id != record.parent.run_id
            || send.conversation_id != record.parent.conversation_id
            || !send.compaction_id.is_empty()
        {
            return Err("question_route_original_changed");
        }
        let plan = Plan {
            session: self.session.clone(),
            request_id: self.request_id.clone(),
            run_id: record.parent.run_id.clone(),
            conversation_id: record.parent.conversation_id.clone(),
            message_sha256: send.message_sha256,
            input_sha256: hash(self.original.as_bytes()),
        };
        if !plan.valid() {
            return Err("invalid_question_route_plan");
        }
        let encoded = serde_json::to_vec(&plan).map_err(|_| "invalid_question_route_plan")?;
        let mut authority = ROUTE_AUTHORITY.to_vec();
        authority.extend_from_slice(self.checkpoint.as_bytes());
        authority.push(0);
        authority.extend_from_slice(self.original.as_bytes());
        let binding = Binding {
            kind: Kind::RouteQuestion,
            schema: 1,
            configuration: self.scope.configuration.clone(),
            session: self.session.clone(),
            request_id: self.request_id.clone(),
            run_id: plan.run_id.clone(),
            conversation_id: plan.conversation_id.clone(),
            source_sha256: hash(self.original.as_bytes()),
            checkpoint_sha256: hash(self.checkpoint.as_bytes()),
            message_sha256: plan.message_sha256.clone(),
            input_sha256: plan.input_sha256.clone(),
            plan_sha256: hash(&encoded),
            consent_sha256: hash(&authority),
        };
        Ok(Verified {
            intent: self,
            plan,
            binding,
        })
    }
    /// The person's deferred send, admitted only by their native UI permit.
    pub fn verify(self) -> Result<Verified, &'static str> {
        if !self.scope.valid()
            || !local_name(&self.session)
            || !uuid(&self.request_id)
            || self.checkpoint.is_empty()
            || self.original.is_empty()
        {
            return Err("invalid_deferred_send_intent");
        }
        if !terminal_agent(&self.session) {
            return Err(AGENT_REFUSED);
        }
        // The complete service line must fit the existing control frame; a
        // larger original stays on the direct path, decided before any offer.
        if !super::ui_requests::fits_frontend_line(&super::ui_requests::Request::DeferredSend {
            intent: Box::new(self.clone()),
        }) {
            return Err("deferred_send_original_too_large");
        }
        // Typed, strict decoding: unknown and duplicate keys are refused, as
        // the CLI refuses them.
        let checkpoint: deferred_input::Checkpoint = serde_json::from_str(&self.checkpoint)
            .map_err(|_| "invalid_deferred_send_checkpoint")?;
        if !checkpoint.valid()
            || checkpoint.kind != deferred_input::Kind::Send
            || checkpoint.request_id != self.request_id
        {
            return Err("invalid_deferred_send_checkpoint");
        }
        let send: Send =
            serde_json::from_str(&self.original).map_err(|_| "invalid_deferred_send_original")?;
        if send.request_id != self.request_id
            || send.expected_run_id != checkpoint.run_id
            || send.expected_conversation_id != checkpoint.conversation_id
        {
            return Err("deferred_send_intent_changed");
        }
        if !send.expected_compaction_id.is_empty() {
            return Err("deferred_send_kind_refused");
        }
        let files = cli_rules(&send).ok_or("deferred_send_message_refused")?;
        let message = deferred_input::message_sha256(
            &send.text,
            send.attachments.iter().zip(&files).map(|(a, bytes)| {
                (
                    a.name.as_str(),
                    a.mime.as_str(),
                    a.reference.as_str(),
                    bytes.as_slice(),
                )
            }),
        );
        if message != checkpoint.message_sha256 {
            return Err("deferred_send_message_changed");
        }
        let plan = Plan {
            session: self.session.clone(),
            request_id: self.request_id.clone(),
            run_id: checkpoint.run_id.clone(),
            conversation_id: checkpoint.conversation_id.clone(),
            message_sha256: message,
            input_sha256: hash(self.original.as_bytes()),
        };
        if !plan.valid() {
            return Err("invalid_deferred_send_plan");
        }
        let encoded = serde_json::to_vec(&plan).map_err(|_| "invalid_deferred_send_plan")?;
        let mut consent = b"desktop-deferred-send-consent-v1\0".to_vec();
        consent.extend_from_slice(self.checkpoint.as_bytes());
        consent.push(0);
        consent.extend_from_slice(self.original.as_bytes());
        let binding = Binding {
            kind: Kind::DeferredSend,
            schema: 1,
            configuration: self.scope.configuration.clone(),
            session: self.session.clone(),
            request_id: self.request_id.clone(),
            run_id: plan.run_id.clone(),
            conversation_id: plan.conversation_id.clone(),
            source_sha256: hash(self.original.as_bytes()),
            checkpoint_sha256: hash(self.checkpoint.as_bytes()),
            message_sha256: plan.message_sha256.clone(),
            input_sha256: plan.input_sha256.clone(),
            plan_sha256: hash(&encoded),
            consent_sha256: hash(&consent),
        };
        Ok(Verified {
            intent: self,
            plan,
            binding,
        })
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Admission {
    pub abi: String,
    pub request_id: String,
    pub scope: Scope,
    pub accepted: bool,
    pub durable: bool,
    pub code: String,
    pub receipt: Option<Receipt>,
}
impl Admission {
    /// Whether this is exactly the closed [`NOT_ADMITTED`] refusal of
    /// `intent`: this kind's ABI, the same request and scope, refused, not
    /// durable, with no receipt. Any other refusal or reply is not.
    pub fn not_admitted(&self, intent: &Intent) -> bool {
        self.abi == ABI
            && self.request_id == intent.request_id
            && self.scope == intent.scope
            && !self.accepted
            && !self.durable
            && self.code == NOT_ADMITTED
            && self.receipt.is_none()
    }
    pub fn matches(&self, intent: &Intent) -> bool {
        self.abi == ABI
            && self.request_id == intent.request_id
            && self.scope == intent.scope
            && self.accepted
            && self.durable
            && self.code.is_empty()
            && self.receipt.as_ref().is_some_and(|r| r.matches(intent))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Queued,
    Running,
    /// The CLI's own receipt of this exact request says `submitted`: the
    /// input was submitted to the terminal. Never proof that the agent read
    /// or answered it.
    Recorded,
    /// Only at the closed no-spawn boundary ([`NOT_STARTED`]): nothing ran.
    Failed,
    /// Anything else after a possibly started child: an `in_progress`,
    /// missing, corrupt, mismatched, inaccessible or replaced receipt, a
    /// timeout or a lost worker. Never resent.
    Unknown,
}
impl Phase {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Recorded | Self::Failed | Self::Unknown)
    }
}

/// The closed outcome a matching `submitted` CLI receipt proves: its request
/// ID, status and stdin digest; no text, files or terminal data.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    pub request_id: String,
    pub status: String,
    /// The CLI receipt's `digest`: the SHA-256 of the exact stdin it
    /// submitted.
    pub input_sha256: String,
}
/// The fields of the CLI's `input_receipts/<request_id>.json` record this rule
/// reads; others are kept by the CLI and ignored here. A duplicate of any of
/// these refuses.
#[derive(Deserialize)]
struct CliReceipt {
    request_id: String,
    name: String,
    run_id: String,
    conversation_id: String,
    digest: String,
    status: String,
    transport: String,
}
impl Outcome {
    pub fn matches(&self, plan: &Plan) -> bool {
        self.request_id == plan.request_id
            && self.status == SUBMITTED
            && self.input_sha256 == plan.input_sha256
    }
    /// The one rule for a matching CLI receipt. The CLI keys its receipt by
    /// request ID alone and keeps an earlier `submitted` record when a later
    /// submit with other content or another session is refused, so the
    /// receipt proves this original only when its session (`name`), run,
    /// conversation, stdin digest and terminal transport are all this
    /// plan's and its status is `submitted`. `in_progress` and anything else
    /// is never an outcome.
    pub fn from_receipt(plan: &Plan, bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > RECEIPT_BYTES {
            return Err(RECEIPT_INVALID);
        }
        let receipt: CliReceipt = serde_json::from_slice(bytes).map_err(|_| RECEIPT_INVALID)?;
        let exact = receipt.request_id == plan.request_id
            && receipt.name == plan.session
            && receipt.run_id == plan.run_id
            && receipt.conversation_id == plan.conversation_id
            && receipt.digest == plan.input_sha256
            && receipt.transport == "terminal";
        if !exact {
            return Err(RECEIPT_MISMATCH);
        }
        match receipt.status.as_str() {
            SUBMITTED => Ok(Self {
                request_id: receipt.request_id,
                status: receipt.status,
                input_sha256: receipt.digest,
            }),
            "in_progress" => Err(RECEIPT_IN_PROGRESS),
            _ => Err(RECEIPT_MISMATCH),
        }
    }
}

#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub binding: Binding,
    pub admission_epoch: String,
    pub revision: u64,
    pub phase: Phase,
    pub drained: bool,
    pub code: String,
    pub outcome: Option<Outcome>,
}
impl std::fmt::Debug for Receipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeferredSendReceipt")
            .field("binding", &self.binding)
            .field("revision", &self.revision)
            .field("phase", &self.phase)
            .field("drained", &self.drained)
            .field("code", &self.code)
            .finish_non_exhaustive()
    }
}
impl Receipt {
    pub fn valid(&self) -> bool {
        self.binding.valid()
            && uuid(&self.admission_epoch)
            && (1..=i64::MAX as u64).contains(&self.revision)
            && self.code.len() <= 80
            && self
                .code
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            && (!self.drained || self.phase.terminal())
            && match self.phase {
                Phase::Recorded => self.drained && self.outcome.is_some(),
                // Only the closed no-spawn boundary fails.
                Phase::Failed => self.outcome.is_none() && self.code == NOT_STARTED,
                _ => self.outcome.is_none(),
            }
    }
    pub fn matches(&self, intent: &Intent) -> bool {
        self.valid()
            && intent
                .clone()
                .verify()
                .is_ok_and(|v| v.binding == self.binding)
    }
    /// Whether this exact original's drained receipt proves that its child
    /// never started. Nothing ran, so the message counts as never sent.
    pub fn not_started(&self, plan: &Plan) -> bool {
        self.valid() && self.drained && self.binding.agrees(plan) && self.phase == Phase::Failed
    }
    /// Only the exact original's drained receipt settles it: Recorded is the
    /// CLI's own submission receipt, Failed the closed no-spawn boundary.
    /// Queued, Running and Unknown never settle, and nothing is resent.
    pub fn result(&self, plan: &Plan) -> Option<Result<(), String>> {
        if !self.valid() || !self.drained || !self.binding.agrees(plan) {
            return None;
        }
        match self.phase {
            Phase::Recorded => self
                .outcome
                .as_ref()
                .is_some_and(|outcome| outcome.matches(plan))
                .then_some(Ok(())),
            Phase::Failed => Some(Err(self.code.clone())),
            _ => None,
        }
    }
}

/// A scoped read-only lookup of one original's receipt.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub scope: Scope,
    pub session: String,
    pub request_id: String,
    pub plan_sha256: String,
}
impl Query {
    pub fn valid(&self) -> bool {
        self.scope.valid()
            && local_session(&self.session)
            && uuid(&self.request_id)
            && super::digest(&self.plan_sha256)
    }
    pub fn for_plan(scope: Scope, plan: &Plan) -> Option<Self> {
        let query = Self {
            scope,
            session: plan.session.clone(),
            request_id: plan.request_id.clone(),
            plan_sha256: plan.fingerprint()?,
        };
        query.valid().then_some(query)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Lookup {
    pub abi: String,
    pub request_id: String,
    pub scope: Scope,
    pub read_only: bool,
    pub found: bool,
    pub receipt: Option<Receipt>,
}
impl Lookup {
    pub fn matches(&self, query: &Query) -> bool {
        self.abi == ABI
            && self.read_only
            && self.request_id == query.request_id
            && self.scope == query.scope
            && self.found == self.receipt.is_some()
            && self.receipt.as_ref().is_none_or(|receipt| {
                // The receipt's own immutable configuration is the queried
                // one; its admission epoch stays its own.
                receipt.valid()
                    && receipt.binding.configuration == query.scope.configuration
                    && receipt.binding.session == query.session
                    && receipt.binding.request_id == query.request_id
                    && receipt.binding.plan_sha256 == query.plan_sha256
            })
    }
}

#[cfg(test)]
pub(crate) mod tests;
