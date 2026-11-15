//! Public-send negative journal publication, reachable only from a live witness
//! under the original input lock. Only an exact public negative may re-enter
//! ordinary guards; all other existing records retain their previous behavior.
use super::*;
use crate::state::repositories::directory::Directory;
use std::io::Read;
use zerus_client_core::desktop::deferred_send::{refusal, Refusal};

fn retained(receipts: &Directory, filename: &str) -> Result<Refusal> {
    if !receipts.private()? {
        return Err("Pre-input refusal store changed".into());
    }
    let mut bytes = Vec::new();
    receipts
        .read_private(filename)?
        .take(refusal::BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    receipts.verify()?;
    Refusal::from_cli_record(&bytes).map_err(str::to_owned)
}

fn bound(
    value: &Refusal,
    id: &str,
    name: &str,
    run: &str,
    conversation: &str,
    digest: &str,
) -> bool {
    value.request_id == id
        && value.session == name
        && value.run_id == run
        && value.conversation_id == conversation
        && value.input_sha256 == digest
}

/// Called only under the input lock. An exact negative is absence of possible
/// input, not a cached success. The caller still runs every ordinary guard.
pub(super) fn admit(
    path: &Path,
    digest: &str,
    name: &str,
    id: &str,
    request: &Request,
    public: bool,
    held: bool,
) -> Existing {
    let previous = super::existing(path, digest, name, held);
    if public && matches!(previous, Existing::Unknown(_)) {
        let exact = (|| -> Result<bool> {
            let receipts = Directory::open(path.parent().ok_or("Invalid input receipt path")?)?;
            let value = retained(&receipts, &format!("{id}.json"))?;
            Ok(bound(
                &value,
                id,
                name,
                &request.expected_run_id,
                &request.expected_conversation_id,
                digest,
            ))
        })();
        if exact == Ok(true) {
            return Existing::Absent;
        }
    }
    previous
}

/// Pending settings precede the message's ordinary input marker. Retire an
/// exact negative before settings can type, including an uncertain failure.
pub(super) fn prepare(witness: &NotOffered) -> Result<()> {
    match fs::symlink_metadata(&witness.receipts) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
        Ok(_) => {}
    }
    let receipts = Directory::open(
        witness
            .receipts
            .parent()
            .ok_or("Invalid input receipt path")?,
    )?;
    let value = retained(&receipts, &format!("{}.json", witness.request_id))?;
    if !bound(
        &value,
        &witness.request_id,
        &witness.name,
        &witness.run_id,
        &witness.conversation_id,
        &witness.digest,
    ) {
        return Err("Pre-input refusal binding changed".into());
    }
    receipt_index::persist_pinned(
        &receipts,
        &format!("{}.json", witness.request_id),
        &json!({
            "request_id": witness.request_id, "name": witness.name,
            "run_id": witness.run_id, "conversation_id": witness.conversation_id,
            "first_message": witness.conversation_id.is_empty(), "digest": witness.digest,
            "status": "preparing", "transport": "terminal"
        }),
    )
}

pub(super) fn remember(witness: &NotOffered) -> Result<()> {
    let value = value(witness)?;
    publish(witness, &value)
}

fn value(witness: &NotOffered) -> Result<Value> {
    let value = Refusal::new(
        &witness.request_id,
        &witness.name,
        &witness.run_id,
        &witness.conversation_id,
        &witness.digest,
        &witness.reason,
    )
    .and_then(|value| value.cli_record())
    .ok_or("Invalid pre-input refusal binding")?;
    Ok(value)
}

/// Only the original live, untyped witness may mint the opted-in terminal form.
/// Missing identity still spends the UUID, but never yields a positive proof.
pub(super) fn remember_terminal(witness: &NotOffered) -> Result<Value> {
    let mut value = value(witness)?;
    value["refusal_schema"] = json!(2);
    value["status"] = json!("not_offered");
    if let Some(binding) = outcome::binding(witness) { value["delivery_binding"] = binding; }
    publish(witness, &value)?;
    outcome::decode(&serde_json::to_vec(&value).map_err(|e| e.to_string())?)
}

fn publish(witness: &NotOffered, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > refusal::BYTES {
        return Err("Pre-input refusal exceeds its bound".into());
    }
    let root = witness
        .receipts
        .parent()
        .and_then(Path::parent)
        .ok_or("Invalid pre-input refusal store")?;
    let root = Directory::open(root)?;
    let receipts = root.ensure_child("input_receipts")?;
    let filename = format!("{}.json", witness.request_id);
    if receipts.path.join(&filename) != witness.receipts || !receipts.private()? {
        return Err("Pre-input refusal store changed".into());
    }
    // Only the original exact negative can be renewed under caller-held input
    // exclusion. Never replace positive, unknown, malformed or foreign evidence.
    if retained(&receipts, &filename).is_ok_and(|old| {
        bound(
            &old,
            &witness.request_id,
            &witness.name,
            &witness.run_id,
            &witness.conversation_id,
            &witness.digest,
        )
    }) {
        receipts.replace_file(&filename, &bytes)?;
    } else {
        receipts.write_new(&filename, &bytes)?;
    }
    root.verify()?;
    receipts.verify()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn witness(root: &Path, reason: &str) -> NotOffered {
        let request_id = uuid::Uuid::new_v4().to_string();
        NotOffered {
            receipts: root
                .join("input_receipts")
                .join(format!("{request_id}.json")),
            request_id,
            name: "claude/private-send".into(),
            run_id: "run-1".into(),
            conversation_id: "conversation-1".into(),
            digest: "a".repeat(64),
            reason: reason.into(),
            waiting: true,
            dialog: None,
        }
    }
    #[test]
    fn only_the_public_untyped_witness_publishes_and_an_exact_refusal_can_be_renewed() {
        let private = tempfile::tempdir().unwrap();
        let root = private.path().canonicalize().unwrap();
        let w = witness(&root, "original draft refusal");
        let path = w.receipts.clone();
        let digest = w.digest.clone();
        let name = w.name.clone();
        assert!(matches!(
            remember_before_input(false, w, true),
            Failure::NotOffered(_)
        ));
        let before = fs::read(&path).unwrap();
        let record: Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(record["status"], refusal::STATUS);
        assert_eq!(record["reason"], "original draft refusal");
        assert_eq!(
            record["request_id"],
            path.file_stem().unwrap().to_str().unwrap()
        );
        assert!(before.len() <= refusal::BYTES);
        assert!(
            matches!(existing(&path, &digest, &name, false), Existing::Unknown(_)),
            "negative evidence is never a submitted acknowledgement"
        );
        let mut duplicate = witness(&root, "replacement reason");
        duplicate.request_id = record["request_id"].as_str().unwrap().into();
        duplicate.receipts = path.clone();
        assert!(matches!(
            remember_before_input(false, duplicate, true),
            Failure::NotOffered(_)
        ));
        let renewed: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(renewed["reason"], "replacement reason");
        for (typed, retain) in [(true, true), (false, false)] {
            let w = witness(&root, "never a public pre-input negative");
            let path = w.receipts.clone();
            let failure = remember_before_input(typed, w, retain);
            assert_eq!(matches!(failure, Failure::Unknown(_)), typed);
            assert!(
                !path.exists(),
                "possible input and internal callers never create a negative"
            );
        }
    }
    #[test]
    fn publication_failure_keeps_unknown_and_retains_the_original_object() {
        let private = tempfile::tempdir().unwrap();
        let root = private.path().canonicalize().unwrap();
        let w = witness(&root, "draft refusal");
        fs::write(root.join("input_receipts"), b"foreign object").unwrap();
        assert!(matches!(
            remember_before_input(false, w, true),
            Failure::Unknown(_)
        ));
        assert_eq!(
            fs::read(root.join("input_receipts")).unwrap(),
            b"foreign object"
        );
    }

    fn request(w: &NotOffered) -> Request {
        Request {
            request_id: w.request_id.clone(),
            text: "same original message".into(),
            expected_run_id: w.run_id.clone(),
            expected_conversation_id: w.conversation_id.clone(),
            attachments: vec![],
            expected_compaction_id: String::new(),
            queue_when_unconfirmed: false,
        }
    }

    #[test]
    fn only_an_exact_public_negative_may_retry_and_all_other_records_keep_their_behavior() {
        let private = tempfile::tempdir().unwrap();
        let root = private.path().canonicalize().unwrap();
        let w = witness(&root, "a real composer draft");
        remember(&w).unwrap();
        let original: Value = serde_json::from_slice(&fs::read(&w.receipts).unwrap()).unwrap();
        let inspect = |public| {
            admit(
                &w.receipts,
                &w.digest,
                &w.name,
                &w.request_id,
                &request(&w),
                public,
                false,
            )
        };
        assert!(matches!(inspect(true), Existing::Absent));
        assert!(
            matches!(inspect(false), Existing::Unknown(_)),
            "internal callers keep their contract"
        );
        for (key, value) in [
            ("request_id", json!(uuid::Uuid::new_v4().to_string())),
            ("name", json!("claude/another-session")),
            ("run_id", json!("another-run")),
            ("conversation_id", json!("another-conversation")),
            ("digest", json!("b".repeat(64))),
            ("transport", json!("host")),
            ("first_message", json!(true)),
            ("refusal_schema", json!(2)),
            ("status", json!("in_progress")),
            ("status", json!("preparing")),
            ("status", json!("unknown")),
        ] {
            let mut changed = original.clone();
            changed[key] = value;
            let bytes = serde_json::to_vec(&changed).unwrap();
            fs::write(&w.receipts, &bytes).unwrap();
            assert!(
                matches!(inspect(true), Existing::Unknown(_)),
                "retry accepted different {key}"
            );
            assert!(remember(&w).is_err(), "renewal replaced different {key}");
            assert_eq!(fs::read(&w.receipts).unwrap(), bytes);
        }
        let mut submitted = original.clone();
        submitted["status"] = json!("submitted");
        fs::write(&w.receipts, submitted.to_string()).unwrap();
        assert!(matches!(inspect(true), Existing::Submitted(value) if value == submitted));
        assert!(remember(&w).is_err());
        for bytes in [
            b"{unreadable JSON".to_vec(),
            {
                let mut large = original.clone();
                large["additive_field"] = json!("x".repeat(refusal::BYTES));
                serde_json::to_vec(&large).unwrap()
            },
            {
                let body = original.to_string();
                format!("{{\"run_id\":\"another-run\",{}", &body[1..]).into_bytes()
            },
        ] {
            fs::write(&w.receipts, &bytes).unwrap();
            assert!(matches!(inspect(true), Existing::Unknown(_)));
            assert!(remember(&w).is_err());
            assert_eq!(fs::read(&w.receipts).unwrap(), bytes);
        }
        #[cfg(unix)]
        {
            let target = root.join("aliased-refusal.json");
            fs::write(&target, original.to_string()).unwrap();
            fs::remove_file(&w.receipts).unwrap();
            std::os::unix::fs::symlink(&target, &w.receipts).unwrap();
            assert!(
                matches!(inspect(true), Existing::Unknown(_)),
                "aliases are not retry evidence"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn pending_settings_retirement_keeps_the_original_directory_pinned() {
        let private = tempfile::tempdir().unwrap();
        let root = private.path().canonicalize().unwrap();
        let witness = witness(&root, "a real composer draft");
        remember(&witness).unwrap();
        let original = Directory::open(witness.receipts.parent().unwrap()).unwrap();
        let moved = root.join("replaced-receipts");
        assert!(fs::rename(&original.path, &moved).is_err());
        prepare(&witness).unwrap();
        assert!(fs::rename(&original.path, &moved).is_err(), "retirement released directory custody");
        let retained: Value = serde_json::from_slice(&fs::read(&witness.receipts).unwrap()).unwrap();
        assert_eq!(retained["status"], "preparing");
        assert_eq!(retained["request_id"], witness.request_id);
        assert!(retained.get("refusal_schema").is_none());
        original.verify().unwrap();
    }

    #[test]
    fn pending_settings_retire_negative_evidence_before_any_possible_input() {
        let private = tempfile::tempdir().unwrap();
        let root = private.path().canonicalize().unwrap();
        let w = witness(&root, "a real composer draft");
        remember(&w).unwrap();
        prepare(&w).unwrap();
        let bytes = fs::read(&w.receipts).unwrap();
        let record: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(record["status"], "preparing");
        assert!(record.get("refusal_schema").is_none());
        assert!(matches!(
            admit(
                &w.receipts,
                &w.digest,
                &w.name,
                &w.request_id,
                &request(&w),
                true,
                false,
            ),
            Existing::Unknown(_)
        ));
        assert!(
            remember(&w).is_err(),
            "possible settings input must never become retryable negative evidence"
        );
        assert_eq!(fs::read(&w.receipts).unwrap(), bytes);
    }
}
