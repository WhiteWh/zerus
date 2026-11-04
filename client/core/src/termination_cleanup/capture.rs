//! Pure identity evidence for the explicit Terminate checkbox. This neither
//! stops a process nor reads/removes a worktree; the existing CLI owns those
//! checks. A receipt is accepted only for this exact confirmed run and archive.
use super::{Entry, STATE_CHANGED};
use crate::{actions, paths, Exit, Invocation, Outcome, Target, WorktreeCleanup};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct Capture {
    pub target: Target,
    pub machine: String,
    pub root: String,
    pub common_dir: String,
    /// The nonce recorded in this run at launch; never inferred from a path.
    pub owner_id: String,
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn bounded(text: &str, limit: usize) -> bool {
    text.len() <= limit && !text.chars().any(char::is_control)
}
fn source_path(path: &str) -> bool {
    bounded(path, 32 * 1024)
        && (paths::absolute(path, paths::Style::Posix)
            || paths::absolute(path, paths::Style::Windows))
}
impl Capture {
    /// A fresh bounded inspection, already read through the existing reader.
    /// Unsupported sessions have no checkbox. An identity mismatch is never
    /// silently substituted with another row or the computer's current folder.
    pub fn from_inspection(
        target: &Target,
        machine: &str,
        value: &Value,
    ) -> Result<Option<Self>, String> {
        if target.name.is_empty()
            || target.run_id.is_empty()
            || target.conversation_id.is_empty()
            || !bounded(&target.name, 512)
            || !bounded(&target.run_id, 128)
            || !bounded(&target.conversation_id, 256)
            || text(value, "name") != target.name
            || text(value, "run_id") != target.run_id
            || text(value, "conversation_id") != target.conversation_id
        {
            return Err(STATE_CHANGED.into());
        }
        if !super::removal_offered(
            text(value, "state") == "running",
            value["tracked"] == true,
            text(value, "cmd"),
            false,
            value["git_worktree"] == true,
        ) {
            return Ok(None);
        }
        let (root, common_dir, owner_id) = (
            text(value, "git_root"),
            text(value, "git_common_dir"),
            text(value, "worktree_owner_id"),
        );
        if !source_path(root)
            || !source_path(common_dir)
            || !bounded(owner_id, 1024)
            || !bounded(machine, 1024)
        {
            return Err(STATE_CHANGED.into());
        }
        // A missing/legacy owner remains missing. Cleanup's existing automatic
        // readiness gate keeps it for review even if the path is unchanged.
        Ok(Some(Self {
            target: target.clone(),
            machine: machine.into(),
            root: root.into(),
            common_dir: common_dir.into(),
            owner_id: owner_id.into(),
        }))
    }
    pub fn recheck(&self, value: &Value) -> bool {
        Self::from_inspection(&self.target, &self.machine, value)
            .is_ok_and(|fresh| fresh.as_ref() == Some(self))
    }
    /// Scoped native action, reusing the existing lifecycle envelope and lane.
    /// Normal legacy Terminate remains unchanged. There is no retry call here.
    pub fn invocation(&self, request_id: &str) -> Result<Invocation, String> {
        if !crate::creation::canonical_uuid(request_id) {
            return Err(STATE_CHANGED.into());
        }
        actions::scoped_lifecycle_invocation(&self.target, "terminate", request_id, 45, 60)
    }
    /// A zero exit alone never admits cleanup. Require a scoped completed
    /// acknowledgement and its exact newly archived result, without accepting
    /// a missing/mismatched archive or a replayed request id.
    pub fn completed(&self, invocation: &Invocation, exit: Exit) -> Outcome {
        let cleanup = match &exit {
            Exit::Code {
                code: 0,
                stdout,
                stderr,
            } if stdout.len() <= 64 * 1024 => actions::cleanup_line(stderr),
            Exit::Code { code: 0, .. } => {
                return actions::failed(
                    "Invalid acknowledgement. Check the session before retrying.",
                )
            }
            _ => None,
        };
        let outcome = actions::classify_scoped_lifecycle(
            &self.target,
            invocation,
            exit,
            "The session was not terminated.",
        );
        let Outcome::Done { receipt, .. } = outcome else {
            return outcome;
        };
        let result = &receipt["result_target"];
        let archive = text(result, "archive_id");
        if text(result, "name") != self.target.name
            || text(result, "run_id") != self.target.run_id
            || text(result, "conversation_id") != self.target.conversation_id
            || !crate::creation::canonical_uuid(archive)
        {
            return actions::failed("Invalid acknowledgement. Check the session before retrying.");
        }
        Outcome::Done { receipt, cleanup }
    }
    pub fn entry(&self, reported: Option<&WorktreeCleanup>) -> Option<Entry> {
        Entry::terminated(
            &self.target.computer,
            &self.machine,
            &self.root,
            &self.common_dir,
            &self.owner_id,
            reported,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const REQUEST: &str = "11111111-1111-4111-8111-111111111111";
    const ARCHIVE: &str = "22222222-2222-4222-8222-222222222222";
    fn target() -> Target {
        Target {
            computer: "example-peer".into(),
            name: "codex/example/review".into(),
            run_id: "run-1".into(),
            conversation_id: "conversation-1".into(),
        }
    }
    fn inspection() -> Value {
        json!({"name":"codex/example/review","run_id":"run-1","conversation_id":"conversation-1","tracked":true,"state":"running","cmd":"codex","git_worktree":true,"git_root":"/srv/example/work-1","git_common_dir":"/srv/example/main/.git","worktree_owner_id":"launch-owner-1"})
    }
    fn capture() -> Capture {
        Capture::from_inspection(&target(), "example-peer", &inspection())
            .unwrap()
            .unwrap()
    }
    fn receipt() -> Value {
        json!({"request_id":REQUEST,"name":"codex/example/review","run_id":"run-1","conversation_id":"conversation-1","status":"completed","result_target":{"name":"codex/example/review","run_id":"run-1","conversation_id":"conversation-1","archive_id":ARCHIVE}})
    }
    fn exited(value: Value) -> Exit {
        Exit::Code {
            code: 0,
            stdout: serde_json::to_vec(&value).unwrap(),
            stderr: String::new(),
        }
    }
    #[test]
    fn cleanup_capture_requires_exact_identity_and_never_invents_missing_ownership() {
        let capture = capture();
        assert!(capture.recheck(&inspection()));
        for field in [
            "name",
            "run_id",
            "conversation_id",
            "git_root",
            "git_common_dir",
            "worktree_owner_id",
        ] {
            let mut changed = inspection();
            changed[field] = json!("different");
            assert!(!capture.recheck(&changed), "{field} changed");
        }
        for field in ["name", "run_id", "conversation_id"] {
            let mut changed = inspection();
            changed.as_object_mut().unwrap().remove(field);
            assert!(Capture::from_inspection(&target(), "example-peer", &changed).is_err());
        }
        let mut legacy = inspection();
        legacy.as_object_mut().unwrap().remove("worktree_owner_id");
        let legacy = Capture::from_inspection(&target(), "example-peer", &legacy)
            .unwrap()
            .unwrap();
        assert!(legacy.owner_id.is_empty());
        assert!(legacy.entry(None).unwrap().expected_owner_id.is_empty());
        for (field, value) in [
            ("tracked", json!(false)),
            ("state", json!("paused")),
            ("cmd", json!("opencode")),
            ("git_worktree", json!(false)),
        ] {
            let mut unsupported = inspection();
            unsupported[field] = value;
            assert_eq!(
                Capture::from_inspection(&target(), "example-peer", &unsupported).unwrap(),
                None
            );
        }
        for path in ["relative", "/srv/example/\nfile", ""] {
            let mut changed = inspection();
            changed["git_root"] = json!(path);
            assert!(Capture::from_inspection(&target(), "example-peer", &changed).is_err());
        }
    }
    #[test]
    fn explicit_cleanup_termination_uses_the_existing_scoped_envelope_without_a_legacy_fallback() {
        let capture = capture();
        let invocation = capture.invocation(REQUEST).unwrap();
        assert_eq!(
            invocation.args,
            [
                "@example-peer",
                "session-action",
                "codex/example/review",
                "--json"
            ]
        );
        assert_eq!(invocation.timeout.as_secs(), 60);
        assert_eq!(
            serde_json::from_slice::<Value>(invocation.stdin.as_ref().unwrap()).unwrap(),
            json!({"request_id":REQUEST,"action":"terminate","expected_run_id":"run-1","expected_conversation_id":"conversation-1"})
        );
        for id in [
            "",
            "request",
            "00000000-0000-0000-0000-000000000000",
            "11111111111141118111111111111111",
        ] {
            assert!(capture.invocation(id).is_err());
        }
    }
    #[test]
    fn only_the_exact_completed_archive_receipt_can_start_cleanup() {
        let capture = capture();
        let invocation = capture.invocation(REQUEST).unwrap();
        assert!(matches!(
            capture.completed(&invocation, exited(receipt())),
            Outcome::Done { .. }
        ));
        for field in [
            "request_id",
            "name",
            "run_id",
            "conversation_id",
            "status",
            "result_target",
        ] {
            let mut changed = receipt();
            changed[field] = json!("different");
            assert!(
                matches!(
                    capture.completed(&invocation, exited(changed)),
                    Outcome::Failed { .. }
                ),
                "{field}"
            );
        }
        for field in ["name", "run_id", "conversation_id", "archive_id"] {
            let mut changed = receipt();
            changed["result_target"][field] = json!("different");
            assert!(
                matches!(
                    capture.completed(&invocation, exited(changed)),
                    Outcome::Failed { .. }
                ),
                "result {field}"
            );
        }
        for exit in [
            Exit::TimedOut,
            Exit::WriteFailed,
            Exit::StartFailed("synthetic".into()),
            Exit::Code {
                code: 0,
                stdout: vec![b'x'; 65537],
                stderr: String::new(),
            },
        ] {
            assert!(matches!(
                capture.completed(&invocation, exit),
                Outcome::Failed { .. }
            ));
        }
        for status in ["claimed", "failed", "uncertain"] {
            let mut pending = receipt();
            pending["status"] = json!(status);
            assert!(matches!(
                capture.completed(&invocation, exited(pending)),
                Outcome::Failed { .. }
            ));
        }
    }
    #[test]
    fn a_report_for_another_checkout_in_the_same_repository_is_unknown() {
        let capture = capture();
        for status in ["removed", "kept", "unconfirmed"] {
            let report = WorktreeCleanup {
                status: status.into(),
                path: "/srv/example/another-checkout".into(),
                common_dir: capture.common_dir.clone(),
                reason: Some("synthetic".into()),
                archive: None,
                review: None,
                details_omitted: false,
            };
            assert_eq!(
                capture.entry(Some(&report)).unwrap().status,
                Some(super::super::Status::Unconfirmed)
            );
        }
    }
}
