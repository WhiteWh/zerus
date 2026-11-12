//! Each hosted DeepSeek agent's own session markers (zerus-ppd26). One native
//! host serves every DeepSeek session of its agent home from one process, so
//! no process environment can say which session a tool call belongs to: the
//! host starts without any session's markers (`host_command`), and its
//! bridge sets the calling session's markers on each tool call's processes.
//!
//! This module writes what the bridge reads: one small file per native
//! conversation, `shells/<conversation_id>.json` in the host folder, written
//! after every binding save (`dsh::save`), so it always names the run that
//! save established, and removed when the binding is forgotten. It holds
//! claims, never credentials; a same-user agent can change it like any other
//! state file. A missing or invalid file gives that conversation's tool
//! calls no session markers at all, never a partial or inherited set.
use super::*;
use std::io::{self, Read};

/// What the bridge accepts, at most; a larger file names no session.
pub(super) const MAX_BYTES: usize = 4096;
/// The longest value the bridge accepts in a marker.
const MAX_VALUE: usize = 2048;
/// DeepSeek has only its native account (`account_context`).
const ACCOUNT: &str = "native-dsh";

/// The folder of one host's identity files, beside its socket.
pub(super) fn folder(record: &Value) -> PathBuf {
    host_dir(record).join("shells")
}

/// The identity file of the record's conversation, when its ID is the
/// native `session-<uuid>` the bridge also checks.
fn path(record: &Value) -> Option<PathBuf> {
    let conversation = string(record, "conversation_id");
    let id = conversation.strip_prefix("session-")?;
    uuid::Uuid::parse_str(id)
        .is_ok_and(|uuid| !uuid.is_nil() && uuid.to_string() == id)
        .then(|| folder(record).join(format!("{conversation}.json")))
}

fn marker(value: &str) -> Option<&str> {
    (!value.is_empty() && value.len() <= MAX_VALUE && !value.chars().any(char::is_control))
        .then_some(value)
}

/// The file's text for this binding, or none when it lacks a complete
/// identity (name, run and session ID).
pub(super) fn content(record: &Value) -> Option<String> {
    let session = marker(string(record, "name"))?;
    let run = marker(string(record, "run_id"))?;
    let session_id = marker(string(record, "session_id"))?;
    let text = json!({"version":1,"conversation":record["conversation_id"],"session":session,
        "run":run,"session_id":session_id,"account":ACCOUNT})
    .to_string();
    (text.len() <= MAX_BYTES).then_some(text)
}

/// Writes the record's identity file when it changed. A record without a
/// complete identity removes any older file, so a stale run is never named.
pub(super) fn write(record: &Value) -> Result<()> {
    let Some(path) = path(record) else {
        return Ok(());
    };
    let Some(text) = content(record) else {
        return match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        };
    };
    let current = crate::platform::no_follow(fs::OpenOptions::new().read(true))
        .open(&path)
        .ok()
        .and_then(|file| {
            let mut bytes = Vec::new();
            file.take(MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            Some(bytes)
        });
    if current.as_deref() == Some(text.as_bytes()) {
        return Ok(());
    }
    atomic(&path, &text)
}

/// Removes the record's identity file; a forgotten binding names nothing.
pub(super) fn remove(record: &Value) {
    if let Some(path) = path(record) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs one fixture alone in a child test process with a private state
    /// root, so no environment change reaches another test.
    fn isolated(fixture: &str) {
        let state = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("state::dsh::shell_identity::tests::{fixture}"),
                "--ignored",
                "--test-threads=1",
            ])
            .env("HGS_STATE_DIR", state.path().join("state"))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && text.contains("1 passed"),
            "{text}{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn record(home: &Path, conversation: &str) -> Value {
        json!({"backend":"dsh","name":"dsh/project/identity","agent":"dsh","agent_home":home,
            "conversation_id":conversation,"run_id":"run-1","session_id":"6a4c5887-557a-4f31-b1c8-ad0a94c3c705"})
    }

    #[test]
    fn the_identity_file_names_the_current_run_privately_and_goes_with_the_binding() {
        isolated("current_run_fixture");
    }

    #[test]
    fn only_a_native_conversation_id_and_plain_markers_are_written() {
        isolated("refusals_fixture");
    }

    #[test]
    #[ignore = "run by its isolated wrapper"]
    fn current_run_fixture() {
        let home = root().join("native");
        let conversation = "session-91250d76-ca68-48b6-87f8-3a0e4400c314";
        let mut binding = record(&home, conversation);
        write(&binding).unwrap();
        let file = folder(&binding).join(format!("{conversation}.json"));
        let value: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            value,
            json!({"version":1,"conversation":conversation,"session":"dsh/project/identity","run":"run-1",
                "session_id":"6a4c5887-557a-4f31-b1c8-ad0a94c3c705","account":"native-dsh"})
        );
        assert_eq!(folder(&binding), host_dir(&binding).join("shells"));
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(folder(&binding)).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        // An unchanged save keeps the file; a new run and a rename rewrite it.
        let inode = |path: &Path| std::os::unix::fs::MetadataExt::ino(&fs::metadata(path).unwrap());
        let before = inode(&file);
        write(&binding).unwrap();
        assert_eq!(inode(&file), before);
        binding["run_id"] = json!("run-2");
        binding["name"] = json!("dsh/project/renamed");
        write(&binding).unwrap();
        let value: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            (value["run"].as_str(), value["session"].as_str()),
            (Some("run-2"), Some("dsh/project/renamed"))
        );
        // A binding without a complete identity leaves no stale run behind.
        binding["run_id"] = json!("");
        write(&binding).unwrap();
        assert!(!file.exists());
        binding["run_id"] = json!("run-3");
        write(&binding).unwrap();
        assert!(file.exists());
        remove(&binding);
        assert!(!file.exists());
    }

    #[test]
    #[ignore = "run by its isolated wrapper"]
    fn refusals_fixture() {
        let home = root().join("native");
        for conversation in [
            "",
            "session-",
            "session-../../escape",
            "session-91250D76-CA68-48B6-87F8-3A0E4400C314",
            "session-00000000-0000-0000-0000-000000000000",
            "91250d76-ca68-48b6-87f8-3a0e4400c314",
        ] {
            write(&record(&home, conversation)).unwrap();
        }
        assert!(!folder(&record(&home, "")).exists());
        let conversation = "session-91250d76-ca68-48b6-87f8-3a0e4400c314";
        let long = "x".repeat(MAX_VALUE + 1);
        for (key, value) in [
            ("name", "dsh/a\nb"),
            ("run_id", ""),
            ("session_id", ""),
            ("name", long.as_str()),
        ] {
            let mut binding = record(&home, conversation);
            binding[key] = json!(value);
            assert!(content(&binding).is_none(), "{key}");
            write(&binding).unwrap();
            assert!(
                !folder(&binding)
                    .join(format!("{conversation}.json"))
                    .exists(),
                "{key}"
            );
        }
    }
}
