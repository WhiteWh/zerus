//! Scoped lifecycle commands with durable per-UUID claims. The receipt lock is
//! separate from the state writer lock; helpers check identity at their own
//! mutation boundary and never replay an abandoned native attempt.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cell::{Cell, RefCell};

mod status;
// Dormant original-claim resource support. Production freshness minting and
// all producer/adapter wiring remain deliberately unavailable.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(dead_code)]
mod writer_claim;
pub(super) use status::dispatch as status_dispatch;
pub(crate) use status::machine as status_machine;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use status::observe_held;
pub(crate) use status::observe_original;

const INPUT_LIMIT: usize = 8192;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    request_id: String,
    action: String,
    expected_run_id: String,
    expected_conversation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_identity: Option<zerus_client_core::untracked_shell::Identity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
}

pub(super) struct Scope {
    name: String,
    run: String,
    conversation: String,
    archive: Option<String>,
    changed: Cell<bool>,
    /// OpenCode service diagnostics are excluded from durable action errors.
    opencode_error: Cell<bool>,
    target: RefCell<Option<Value>>,
}
impl Scope {
    pub(super) fn check_base(&self, record: &Value) -> Result<()> {
        if string(record, "name") != self.name
            || string(record, "run_id") != self.run
            || string(record, "conversation_id") != self.conversation
        {
            return Err("Session identity changed; no replacement session was selected".into());
        }
        Ok(())
    }
    pub(super) fn check(&self, record: &Value) -> Result<()> {
        self.check_base(record)?;
        terminating::check_mutation(record)?;
        if record["archive_id"].as_str().filter(|s| !s.is_empty()) != self.archive.as_deref() {
            return Err("Archive identity changed; select the exact saved archive".into());
        }
        Ok(())
    }
    pub(super) fn changing(&self) {
        self.changed.set(true);
    }
    pub(super) fn opencode_error(&self) {
        self.opencode_error.set(true);
    }
    /// Only a supported native adapter may explicitly attest zero side effects,
    /// or a resume whose new terminal is gone with no record of its run
    /// (zerus-5sl7r): it started nothing and left nothing.
    pub(super) fn unchanged(&self) {
        self.changed.set(false);
    }
    pub(super) fn result(&self, name: &str, run: &str, conversation: &str, archive: Option<&str>) {
        let mut target = json!({"name":name,"run_id":run,"conversation_id":conversation});
        if let Some(id) = archive {
            target["archive_id"] = json!(id);
        }
        *self.target.borrow_mut() = Some(target);
    }
    pub(super) fn result_record(&self, record: &Value) {
        self.result(
            string(record, "name"),
            string(record, "run_id"),
            string(record, "conversation_id"),
            record["archive_id"].as_str().filter(|s| !s.is_empty()),
        );
    }
}
#[cfg(test)]
impl Scope {
    /// The scope `attempt` builds for these expected identities.
    pub(super) fn expecting(name: &str, run: &str, conversation: &str) -> Self {
        Scope {
            name: name.into(),
            run: run.into(),
            conversation: conversation.into(),
            archive: None,
            changed: Cell::new(false),
            opencode_error: Cell::new(false),
            target: RefCell::new(None),
        }
    }
    /// Whether a failure would be reported as uncertain rather than failed.
    pub(super) fn was_changing(&self) -> bool {
        self.changed.get()
    }
    pub(super) fn has_result(&self) -> bool {
        self.target.borrow().is_some()
    }
}

/// Why an archive that ended before its first message cannot restore.
pub(super) const NO_CONVERSATION: &str =
    "This session ended before its first message, so it saved no conversation to restore";

/// A presentation gate, never a substitute for the locked action checks.
/// `resumed` is the summary's single verification of a pending resume.
/// Ordinary listing supplies name availability from its bounded observation.
pub(super) fn summary(
    record: &Value,
    live_pane: bool,
    resumed: &Option<Result<()>>,
    archive_name_taken: Option<bool>,
) -> Value {
    let archived = !string(record, "archive_id").is_empty();
    let terminating = terminating::pending(record);
    let changing = record.get("pausing").is_some() || record["resume_pending"] == true;
    let confirmed = !string(record, "conversation_id").is_empty();
    let native_archive = archived && record["backend"] == "dsh";
    let recipe = if native_archive {
        Path::new(string(record, "cwd")).is_dir() || super::worktree_lifecycle::restorable(record)
    } else {
        record["base"].is_array() && (Path::new(string(record, "launch_dir")).is_dir() || super::worktree_lifecycle::restorable(record))
    };
    let cleanup_pending = clear_context::cleanup_pending(record);
    let restore_pending = !string(record, "restore_archive_id").is_empty() || cleanup_pending;
    let name = string(record, "name");
    let native_pending = native_archive && dsh::retirement::restore_source_held(record) == Ok(true);
    let restorable = native_pending
        || archived
            && recipe
            && !cleanup_pending
            && if native_archive {
                dsh::retirement::restore_name_busy(name) == Ok(false)
            } else {
                !archive_name_taken.unwrap_or_else(|| record_path(name).exists())
            };
    // The restore's own history check decides, through the shared code:
    // one bounded metadata read, only for a Claude or Codex archive that
    // may otherwise restore (zerus-63rru.70). Kimi, DeepSeek and OpenCode
    // keep today's decision and are not read here.
    let conversation = !restorable || recipes::conversation_persisted(record);
    let decisions = [
        (
            "pause",
            !archived && live_pane && !changing && (confirmed || lifecycle::unconfirmed_fresh(record))
                && lifecycle::pausable(record, || {
                    resume_input::idle_checked(record, resumed.as_ref().is_some_and(Result::is_ok))
                }),
        ),
        (
            "resume",
            !archived && !live_pane && !changing && confirmed && recipe && !cleanup_pending,
        ),
        ("archive", !archived && !live_pane && !changing && confirmed && !cleanup_pending),
        ("rename", !changing && !native_pending),
        (
            "fork",
            !changing && fork::summary(record, live_pane)["fork_supported"] == true,
        ),
        ("terminate", !archived),
        ("restore", restorable && conversation),
        ("forget", !changing && !restore_pending && !native_pending),
    ];
    let mut allowed = Vec::new();
    let mut reasons = serde_json::Map::new();
    for (action, enabled) in decisions {
        if enabled && !terminating {
            allowed.push(action);
        } else {
            reasons.insert(
                action.into(),
                json!(if terminating { terminating::WAIT } else { match action {
                    "pause" => "Wait for a confirmed idle live session before pausing",
                    "resume" =>
                        "Resume requires a saved session and available native launch recipe",
                    "archive" => "Archive requires a stopped or paused confirmed session",
                    "restore" if !conversation => NO_CONVERSATION,
                    "restore" => "Restore requires an archive recipe and an unused session name",
                    "terminate" => "An archive has no running session to terminate",
                    "fork" => "This session cannot fork its confirmed native context yet",
                    "forget" =>
                        "Wait for the active pause or restore before forgetting this session",
                    _ => "Wait for the active pause or restore before renaming",
                }}),
            );
        }
    }
    let mut result = json!({"allowed_actions":allowed,"action_reasons":reasons});
    if native_pending {
        result["restore_pending"] = json!(true);
    }
    result
}

fn validate(request: &Request, name: &str) -> Result<()> {
    let canonical =
        uuid::Uuid::parse_str(&request.request_id).map_err(|_| "Invalid request UUID")?;
    if canonical.is_nil() || canonical.to_string() != request.request_id {
        return Err("Invalid request UUID".into());
    }
    for (value, maximum, nonempty) in [
        (name, 512, true),
        (
            request.expected_run_id.as_str(),
            128,
            request.action != "terminate_shell",
        ),
        (request.expected_conversation_id.as_str(), 256, false),
    ] {
        if value.len() > maximum
            || (nonempty && value.is_empty())
            || value.chars().any(char::is_control)
        {
            return Err("Invalid session identity".into());
        }
    }
    if request.action == "terminate_shell" {
        if !request.expected_run_id.is_empty()
            || !request.expected_conversation_id.is_empty()
            || !request
                .terminal_identity
                .as_ref()
                .is_some_and(|identity| identity.valid())
            || request.archive_id.is_some()
            || request.new_name.is_some()
            || request.tag.is_some()
            || request.message_id.is_some()
        {
            return Err(zerus_client_core::untracked_shell::UNVERIFIED.into());
        }
        return Ok(());
    }
    if request.terminal_identity.is_some() {
        return Err("Unexpected terminal identity".into());
    }
    let archived = matches!(
        request.action.as_str(),
        "restore" | "forget" | "rename" | "fork"
    );
    if !matches!(
        request.action.as_str(),
        "pause"
            | "resume"
            | "archive"
            | "rename"
            | "fork"
            | "terminate"
            | "restore"
            | "forget"
            | "remove_queued"
    ) || (request.archive_id.is_some() && !archived)
        || (request.action == "restore" && request.archive_id.is_none())
        || (request.new_name.is_some() != (request.action == "rename"))
        || (request.tag.is_some() && request.action != "fork")
        || (request.message_id.is_some() != (request.action == "remove_queued"))
    {
        return Err("Unsupported action or unexpected action argument".into());
    }
    for (value, maximum) in [
        (request.new_name.as_deref(), 512),
        (request.tag.as_deref(), 120),
    ] {
        if value.is_some_and(|value| {
            value.is_empty()
                || value.len() > maximum
                || value.trim() != value
                || value.chars().any(char::is_control)
        }) {
            return Err("Invalid bounded action name".into());
        }
    }
    if request
        .message_id
        .as_deref()
        .is_some_and(|id| !opencode::queued_message_id(id))
    {
        return Err("Invalid queued message identity".into());
    }
    if let Some(id) = &request.archive_id {
        let uuid = uuid::Uuid::parse_str(id).map_err(|_| "Invalid archive UUID")?;
        if uuid.is_nil() || uuid.to_string() != *id {
            return Err("Invalid archive UUID".into());
        }
    }
    Ok(())
}

fn perform(request: &Request, name: &str, scope: &Scope) -> Result<()> {
    if request.action == "terminate_shell" {
        return untracked_shell::terminate(
            name,
            request
                .terminal_identity
                .as_ref()
                .ok_or(zerus_client_core::untracked_shell::UNVERIFIED)?,
            scope,
        );
    }
    if opencode::exists(name) {
        scope.opencode_error();
        return opencode::session_action(name, &request.action, request.new_name.as_deref(), request.message_id.as_deref(), scope);
    }
    if request.action == "remove_queued" {
        return Err("Only OpenCode sessions have removable queued messages".into());
    }
    if request.archive_id.is_none() && dsh::exists(name) {
        if request.action == "fork" {
            return dsh::fork(name, request.tag.as_deref(), Some(&request.expected_run_id),
                Some(&request.expected_conversation_id), false, Some(scope)).map(|_| ());
        }
        return dsh::session_action(name, &request.action, request.new_name.as_deref(), scope);
    }
    match request.action.as_str() {
        "pause" => {
            lifecycle::pause_scoped(&[name.into()], false, Some(scope))?;
        }
        "resume" => {
            lifecycle::resume_scoped(&[name.into()], false, Some(scope))?;
        }
        "restore" => {
            lifecycle::resume_scoped(
                &[
                    name.into(),
                    "--archive".into(),
                    request.archive_id.clone().unwrap(),
                ],
                false,
                Some(scope),
            )?;
        }
        "archive" => {
            archive::manual_scoped(name, false, Some(scope))?;
        }
        "rename" => {
            let mut args = vec![name.into(), request.new_name.clone().unwrap()];
            if let Some(id) = &request.archive_id {
                args.extend(["--archive".into(), id.clone()]);
            }
            rename::dispatch_scoped(&args, false, Some(scope))?;
        }
        "fork" => {
            fork::start_scoped(
                name,
                request.tag.as_deref(),
                request.archive_id.as_deref(),
                Some(&request.expected_run_id),
                Some(&request.expected_conversation_id),
                false,
                Some(scope),
            )?;
        }
        "terminate" => {
            terminate::dispatch_scoped(&[name.into()], false, Some(scope), false)?;
        }
        "forget" => {
            if let Some(id) = &request.archive_id {
                archive::forget_scoped(name, id, false, Some(scope))?;
            } else {
                terminate::dispatch_scoped(&[name.into()], false, Some(scope), true)?;
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn receipt(request: &Request, name: &str, hash: &str, status: &str) -> Value {
    let mut result=json!({"request_id":request.request_id,"name":name,"run_id":request.expected_run_id,
        "conversation_id":request.expected_conversation_id,"status":status,"request_hash":hash,"at":now()});
    if let Some(id)=&request.archive_id {result["archive_id"]=json!(id);}
    result
}

fn attempt<F>(path: &Path, request: &Request, name: &str, hash: &str, invoke: F) -> Result<Value>
where
    F: FnOnce(&Scope) -> Result<()>,
{
    if path.exists() {
        let old: Value = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        if old["request_hash"] != hash {
            return Err("Request UUID was already used for another action".into());
        }
        if old["status"] == "claimed" {
            let mut uncertain = receipt(request, name, hash, "uncertain");
            uncertain["error"] =
                json!("The earlier native attempt was interrupted; it was not replayed");
            atomic(path, &uncertain.to_string())?;
            return Ok(uncertain);
        }
        return Ok(old);
    }
    // zerus-ucck P3c: the request's identities select among sessions that
    // share the label, before anything is claimed.
    if request.archive_id.is_some() && name.starts_with("dsh/") {
        let _ = session_id::take_deferred(name);
    } else {
        expect(name, &request.expected_run_id, &request.expected_conversation_id)?;
    }
    atomic(path, &receipt(request, name, hash, "claimed").to_string())?;
    let scope = Scope {
        name: name.into(),
        run: request.expected_run_id.clone(),
        conversation: request.expected_conversation_id.clone(),
        archive: request.archive_id.clone(),
        changed: Cell::new(false),
        opencode_error: Cell::new(false),
        target: RefCell::new(None),
    };
    let outcome = invoke(&scope);
    let status = if outcome.is_ok() {
        "completed"
    } else if scope.changed.get() {
        "uncertain"
    } else {
        "failed"
    };
    let mut answer = receipt(request, name, hash, status);
    if let Err(ref error) = outcome {
        if scope.opencode_error.get() {
            answer["error_source"] = json!("opencode");
            answer["error"] = json!(if status == "uncertain" {
                "OpenCode action outcome is uncertain; inspect the session before creating a new request"
            } else {
                "OpenCode could not complete this action; inspect the session on its computer"
            });
        } else {
            answer["error"] = json!(error.chars().take(2048).collect::<String>());
        }
    }
    if outcome.is_ok() {
        if let Some(identity) = &request.terminal_identity {
            answer["terminal_identity"] = json!(identity);
            answer["terminal_closed"] = json!(true);
        }
        if let Some(target) = scope.target.into_inner() {
            answer["result_target"] = target;
        }
    }
    // An unwritten acknowledgement leaves the durable claim, which is uncertain
    // on the next lookup. It must never cause another lifecycle handoff.
    atomic(path, &answer.to_string())?;
    Ok(answer)
}

fn input_bytes(args: &[String], command: &str) -> Result<Vec<u8>> {
    if args.len() != 2 || args[1] != "--json" {
        return Err(format!("usage: hgs {command} <session> --json"));
    }
    let mut bytes = Vec::new();
    io::stdin()
        .take((INPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > INPUT_LIMIT {
        return Err("Action request is too large".into());
    }
    Ok(bytes)
}

fn decode_request(bytes: &[u8], name: &str) -> Result<Request> {
    if bytes.len() > INPUT_LIMIT {
        return Err("Action request is too large".into());
    }
    let request: Request = serde_json::from_slice(crate::state::without_bom(bytes)).map_err(|e| e.to_string())?;
    validate(&request, name)?;
    Ok(request)
}

fn request_input(args: &[String], command: &str) -> Result<Request> {
    let bytes = input_bytes(args, command)?;
    decode_request(&bytes, &args[0])
}

/// The exact action receipt `hgs session-action` writes for the original
/// stdin `original` of `name` with `status` (and, when completed, the
/// resumed `run` as its result target), for other modules' fixtures.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn fixture_receipt(
    name: &str,
    original: &[u8],
    status: &str,
    run: Option<&str>,
) -> Value {
    let request = decode_request(original, name).unwrap();
    let mut value = receipt(&request, name, &request_hash(&request, name), status);
    if let Some(run) = run {
        value["result_target"] = json!({"name":name,"run_id":run,
            "conversation_id":request.expected_conversation_id});
    }
    value
}

fn request_hash(request: &Request, name: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(json!([name, request]).to_string().as_bytes())
    )
}

pub(super) fn dispatch(args: &[String]) -> Result<i32> {
    let request = request_input(args, "session-action")?;
    let name = &args[0];
    let hash = request_hash(&request, name);
    let directory = absolute_root()?.join("action_receipts");
    let path = directory.join(format!("{}.json", request.request_id));
    let _receipt_guard = lock(Some(
        &directory.join(format!("{}.lock", request.request_id)),
    ))?;
    let mut answer = attempt(&path, &request, name, &hash, |scope| {
        perform(&request, name, scope)
    })?;
    answer.as_object_mut().unwrap().remove("request_hash");
    println!("{answer}");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> Request {
        serde_json::from_value(json!({"request_id":"11111111-1111-4111-8111-111111111111",
        "action":"pause","expected_run_id":"run","expected_conversation_id":"conversation"}))
        .unwrap()
    }
    #[test]
    fn action_scope_and_arguments_reject_replacements() {
        let r = request();
        validate(&r, "codex/project/session").unwrap();
        let directory = tempfile::tempdir().unwrap();
        attempt(&directory.path().join("receipt"),&r,"session","hash",|scope| {
            scope.check(&json!({"name":"session","run_id":"run","conversation_id":"conversation"}))?;
            assert!(scope.check(&json!({"name":"session","run_id":"run","conversation_id":"other"})).is_err());
            assert!(scope.check(&json!({"name":"session","run_id":"run","conversation_id":"conversation","archive_id":"archive"})).is_err());
            Ok(())
        }).unwrap();
        let mut invalid = request();
        invalid.archive_id = Some("11111111-1111-4111-8111-111111111111".into());
        assert!(validate(&invalid, "session").is_err());
    }
    #[test]
    fn remove_queued_needs_exactly_one_message_id() {
        let mut r = request();
        r.action = "remove_queued".into();
        assert!(validate(&r, "opencode/p/t").is_err());
        r.message_id = Some("msg_gVmstesSuU81H2vTHdmxp65tNI".into());
        validate(&r, "opencode/p/t").unwrap();
        r.message_id = Some("../x".into());
        assert!(validate(&r, "opencode/p/t").is_err());
        // `msg_` alone names no message; the relay and the connector refuse it too.
        r.message_id = Some("msg_".into());
        assert!(validate(&r, "opencode/p/t").is_err());
        r.message_id = Some("msg_a-b_c".into());
        validate(&r, "opencode/p/t").unwrap();
        r.action = "forget".into();
        r.message_id = Some("msg_A".into());
        assert!(validate(&r, "opencode/p/t").is_err());
    }
    #[test]
    fn duplicate_and_abandoned_claim_never_repeat_handoff() {
        let r = request();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("receipt");
        let first = attempt(&path, &r, "session", "hash", |scope| {
            scope.changing();
            Err("after handoff".into())
        })
        .unwrap();
        assert_eq!(first["status"], "uncertain");
        assert_eq!(
            attempt(&path, &r, "session", "hash", |_| panic!("replayed")).unwrap(),
            first
        );
        assert!(attempt(&path, &r, "session", "different", |_| panic!("conflict")).is_err());
        atomic(
            &path,
            &receipt(&r, "session", "hash", "claimed").to_string(),
        )
        .unwrap();
        assert_eq!(
            attempt(&path, &r, "session", "hash", |_| panic!("abandoned replay")).unwrap()
                ["status"],
            "uncertain"
        );
    }
    #[test]
    fn rejection_before_first_change_is_definite_failure() {
        let r = request();
        let directory = tempfile::tempdir().unwrap();
        let answer = attempt(
            &directory.path().join("receipt"),
            &r,
            "session",
            "hash",
            |_| Err("unsupported".into()),
        )
        .unwrap();
        assert_eq!(answer["status"], "failed");
        assert!(answer["result_target"].is_null());
    }
    #[test]
    fn opencode_diagnostics_never_enter_action_receipts_or_change_duplicate_results() {
        let secret = "https://provider.example.invalid/v1?key=fixture-YO85-ACTION\u{1b}[31m";
        for changed in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("receipt");
            let first = attempt(&path, &request(), "session", "hash", |scope| {
                scope.opencode_error();
                if changed { scope.changing(); }
                Err(secret.into())
            }).unwrap();
            assert_eq!(first["status"], if changed { "uncertain" } else { "failed" });
            assert_eq!(first["error_source"], "opencode");
            let stored = std::fs::read_to_string(&path).unwrap();
            assert!(!stored.contains("provider.example.invalid"));
            assert!(!stored.contains("fixture-YO85-ACTION"));
            assert_eq!(serde_json::from_str::<Value>(&stored).unwrap(), first);
            assert_eq!(attempt(&path, &request(), "session", "hash", |_| panic!("replayed")).unwrap(), first);
        }
    }
    #[test]
    fn successful_opencode_actions_keep_their_original_receipt_and_target() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("receipt");
        let first = attempt(&path, &request(), "session", "hash", |scope| {
            scope.opencode_error();
            scope.result("renamed", "run", "conversation", None);
            Ok(())
        }).unwrap();
        assert_eq!(first["status"], "completed");
        assert_eq!(first["result_target"], json!({"name":"renamed","run_id":"run","conversation_id":"conversation"}));
        assert!(first.get("error").is_none());
        assert!(first.get("error_source").is_none());
        assert_eq!(attempt(&path, &request(), "session", "hash", |_| panic!("replayed")).unwrap(), first);
    }

    /// A Claude or Codex archive that exited before its first message wrote
    /// no transcript: Restore is not offered, and says why (zerus-wyrqg).
    #[test]
    fn an_archive_without_a_saved_conversation_offers_no_restore() {
        use std::fs;
        let folder = tempfile::tempdir().unwrap();
        let transcript = folder.path().join("conversation.jsonl");
        let record = |agent: &str, launch_dir: &Path| {
            json!({"name": format!("{agent}/example/archived-{}", uuid::Uuid::new_v4()),
                "agent": agent, "archive_id": "archive-1", "conversation_id": "conversation-1",
                "base": [agent], "launch_dir": launch_dir, "transcript": transcript})
        };
        let restore = |record: &Value| {
            let summary = summary(record, false, &None, None);
            let allowed = summary["allowed_actions"]
                .as_array()
                .unwrap()
                .contains(&json!("restore"));
            (
                allowed,
                summary["action_reasons"]["restore"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned(),
            )
        };
        for agent in ["claude", "codex"] {
            // Missing, a folder, then empty: nothing to restore.
            let _ = fs::remove_file(&transcript);
            assert_eq!(
                restore(&record(agent, folder.path())),
                (false, NO_CONVERSATION.to_owned()),
                "{agent}"
            );
            fs::create_dir(&transcript).unwrap();
            assert_eq!(
                restore(&record(agent, folder.path())),
                (false, NO_CONVERSATION.to_owned()),
                "{agent}"
            );
            fs::remove_dir(&transcript).unwrap();
            fs::write(&transcript, "").unwrap();
            assert_eq!(
                restore(&record(agent, folder.path())),
                (false, NO_CONVERSATION.to_owned()),
                "{agent}"
            );
            // The first message saved it.
            fs::write(&transcript, "{}\n").unwrap();
            assert_eq!(
                restore(&record(agent, folder.path())),
                (true, String::new()),
                "{agent}"
            );
            // Without a recipe the usual reason stays.
            let _ = fs::remove_file(&transcript);
            let reason = restore(&record(agent, &folder.path().join("gone"))).1;
            assert_eq!(
                reason,
                "Restore requires an archive recipe and an unused session name"
            );
        }
        // Kimi keeps its own check in the restore itself.
        assert!(restore(&record("kimi", folder.path())).0);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn offered_restore(summary: &Value) -> bool {
        summary["allowed_actions"]
            .as_array()
            .unwrap()
            .contains(&json!("restore"))
    }

    /// Runs one ignored archive fixture alone in a child of this test binary
    /// with a private state, configuration and tmux root: the archive writer
    /// needs its own root, never the machine's, and never a real tmux server.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn isolated_archive_fixture(test: &str) {
        let temp = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--ignored", "--test-threads=1"])
            .env("HGS_STATE_DIR", temp.path().join("state"))
            .env("HGS_CONFIG_DIR", temp.path().join("config"))
            .env("TMUX_TMPDIR", temp.path().join("tmux"))
            .env_remove("HGS_SESSION")
            .env_remove("HGS_SESSION_ID")
            .env_remove("HGS_RUN_ID")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && text.contains("1 passed"),
            "{text}{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A Claude archive through the state's own archive writer, then the
    /// state's own archive reader: every field the summary decides on is
    /// product output.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn archived_record(name: &str, transcript: &Path) -> Value {
        let unique = uuid::Uuid::new_v4().to_string();
        let mut record = crate::state::listing_fixture::launched_record(
            "claude",
            name,
            &uuid::Uuid::new_v4().to_string(),
            &uuid::Uuid::new_v4().to_string(),
            &unique,
            "/",
            "account-example",
        );
        crate::state::listing_fixture::confirmed(&mut record, &format!("conversation-{unique}"));
        record["transcript"] = json!(transcript);
        let id = crate::state::archive_for_test(&record).unwrap();
        crate::state::archive::read_archive(name, &id).unwrap()
    }

    /// zerus-63rru.70: a Claude archive that exited before its first message
    /// (a conversation ID but no transcript) offers no restore, with the
    /// pinned reason; a saved one offers restore; the restore's own history
    /// check still refuses the first.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn a_claude_archive_offers_restore_only_for_a_saved_conversation() {
        isolated_archive_fixture(
            "state::session_action::tests::a_claude_archive_never_saved_fixture",
        );
    }

    #[test]
    #[ignore = "run by its isolated wrapper"]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn a_claude_archive_never_saved_fixture() {
        let dir = tempfile::tempdir().unwrap();
        let saved = dir.path().join("saved.jsonl");
        std::fs::write(&saved, "{\"type\":\"user\"}\n").unwrap();
        // The conversation that was never saved: no transcript file.
        let before_first =
            archived_record("claude/example/never-saved", &dir.path().join("missing.jsonl"));
        let offered = summary(&before_first, false, &None, None);
        assert!(!offered_restore(&offered), "{offered}");
        assert_eq!(offered["action_reasons"]["restore"], NO_CONVERSATION);
        // The restore command's own check still refuses it, unchanged.
        assert!(crate::state::recipes::verify_history(&before_first, true).is_err());
        // The saved conversation offers restore and passes its own check.
        let persisted = archived_record("claude/example/saved", &saved);
        let offered = summary(&persisted, false, &None, None);
        assert!(offered_restore(&offered), "{offered}");
        assert!(crate::state::recipes::verify_history(&persisted, true).is_ok());
    }

    /// zerus-63rru.70: the real summary, over an archive the state's own
    /// writer and reader produced, reads the transcript metadata exactly once.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn deciding_one_archives_actions_reads_the_transcript_at_most_once() {
        isolated_archive_fixture(
            "state::session_action::tests::deciding_one_archives_actions_reads_the_transcript_at_most_once_fixture",
        );
    }

    #[test]
    #[ignore = "run by its isolated wrapper"]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn deciding_one_archives_actions_reads_the_transcript_at_most_once_fixture() {
        let dir = tempfile::tempdir().unwrap();
        let saved = dir.path().join("saved.jsonl");
        std::fs::write(&saved, "{\"type\":\"user\"}\n").unwrap();
        let record = archived_record("claude/example/counted", &saved);
        crate::state::journal::read_order::take();
        let offered = summary(&record, false, &None, None);
        let steps = crate::state::journal::read_order::take();
        assert!(offered_restore(&offered), "{offered}");
        assert_eq!(
            steps.iter().filter(|step| **step == "transcript").count(),
            1,
            "{steps:?}"
        );
        // A missing conversation ID refuses before any transcript metadata I/O.
        let mut unconfirmed = crate::state::listing_fixture::launched_record(
            "claude",
            "claude/example/unconfirmed",
            &uuid::Uuid::new_v4().to_string(),
            &uuid::Uuid::new_v4().to_string(),
            &uuid::Uuid::new_v4().to_string(),
            "/",
            "account-example",
        );
        crate::state::listing_fixture::confirmed(&mut unconfirmed, "conversation-unconfirmed");
        unconfirmed["conversation_id"] = json!("");
        unconfirmed["transcript"] = json!(saved);
        let id = crate::state::archive_for_test(&unconfirmed).unwrap();
        let unconfirmed =
            crate::state::archive::read_archive("claude/example/unconfirmed", &id).unwrap();
        crate::state::journal::read_order::take();
        let offered = summary(&unconfirmed, false, &None, None);
        let steps = crate::state::journal::read_order::take();
        assert!(!offered_restore(&offered), "{offered}");
        assert!(
            steps.iter().all(|step| *step != "transcript"),
            "{steps:?}"
        );
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn other_agents_keep_todays_restore_decision_without_a_transcript_read() {
        isolated_archive_fixture(
            "state::session_action::tests::other_agents_keep_todays_restore_decision_without_a_transcript_read_fixture",
        );
    }

    #[test]
    #[ignore = "run by its isolated wrapper"]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn other_agents_keep_todays_restore_decision_without_a_transcript_read_fixture() {
        let dir = tempfile::tempdir().unwrap();
        {
            let agent = "kimi";
            let unique = uuid::Uuid::new_v4().to_string();
            let name = format!("{agent}/example/member-{unique}");
            let mut record = crate::state::listing_fixture::launched_record(
                agent,
                &name,
                &uuid::Uuid::new_v4().to_string(),
                &uuid::Uuid::new_v4().to_string(),
                &unique,
                "/",
                "account-example",
            );
            crate::state::listing_fixture::confirmed(&mut record, &format!("conversation-{unique}"));
            record["transcript"] = json!(dir.path().join("missing.jsonl"));
            let record = {
                let id = crate::state::archive_for_test(&record).unwrap();
                crate::state::archive::read_archive(&name, &id).unwrap()
            };
            crate::state::journal::read_order::take();
            let offered = summary(&record, false, &None, None);
            let steps = crate::state::journal::read_order::take();
            assert!(offered_restore(&offered), "{agent}: {offered}");
            assert!(
                steps.iter().all(|step| *step != "transcript"),
                "{agent}: {steps:?}"
            );
        }
    }
}
