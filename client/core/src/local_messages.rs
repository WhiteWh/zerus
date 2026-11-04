//! Messages the client sent but the agent's own history does not show yet,
//! ported from the Qt client's `m_localMessages`: a row per send, kept while
//! it is sending, after it failed, or until the history shows it, at most 50
//! per session. In memory only, as in Qt; the view keeps them across a
//! webview reload.
//!
//! Reference: `tray/src/SessionsWindow.cpp` (`deliverMessage`,
//! `finishMessage`, `reconcileMessages`, `messageAction`, the rename paths,
//! and the voice composer's `messageChanged`).
use crate::actions::Outcome;
use crate::attachments::StoredFile;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Rows kept per session; the oldest go first.
pub const MAX_PER_SESSION: usize = 50;
/// Sessions with rows. Past it, the least recently touched session without
/// a row still sending is dropped; a sending row's outcome is still due.
pub const MAX_SESSIONS: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Sending,
    Sent,
    Error,
}

/// One local message row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LocalMessage {
    /// The send's request ID, or `voice-{message ID}` for a voice message.
    pub id: String,
    pub text: String,
    pub status: Status,
    /// The cached files ([`StoredFile`]); after a send each also has the
    /// receipt's `request_id` and its `index`.
    pub attachments: Vec<Value>,
    pub run_id: String,
    pub conversation_id: String,
    /// Seconds since the epoch.
    pub submitted_at: f64,
    #[serde(default)]
    pub error: String,
    /// The failure may have delivered the message: never retried.
    #[serde(default)]
    pub uncertain: bool,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub submitted_text: Option<String>,
    /// `hgs_voice` for a voice message.
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub voice: Option<Value>,
}

/// The detail of a send that may have delivered, as Qt shows it.
pub fn uncertain_detail(error: &str) -> String {
    format!("Delivery is not confirmed. Check Terminal before retrying. {error}")
}

/// What reconciling removed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reconciled {
    pub removed: usize,
    /// Texts of failed rows the history shows delivered after all: a draft
    /// still holding one of them was sent.
    pub delivered: Vec<String>,
}

/// Where Inspect on a failed row leads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Inspect {
    NativeUi,
    Terminal,
    /// Refresh the session and show this notice.
    Refresh {
        notice: String,
    },
}

/// Inspect for a failed row: DeepSeek's Native UI, else Terminal when the
/// session has one, else a refresh.
pub fn inspect(cmd: &str, terminal: bool, machine: &str) -> Inspect {
    if cmd == "dsh" {
        Inspect::NativeUi
    } else if terminal {
        Inspect::Terminal
    } else {
        Inspect::Refresh {
            notice: format!(
                "Refreshing the native session. Check its provider configuration on {machine} if the error remains."
            ),
        }
    }
}

/// The local rows of every session, by session key.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalMessages {
    rows: HashMap<String, Vec<LocalMessage>>,
    /// When each session was last changed, by a counter.
    touched: HashMap<String, u64>,
    clock: u64,
}

fn files_value(files: &[StoredFile]) -> Vec<Value> {
    files
        .iter()
        .map(|file| {
            let mut value = serde_json::to_value(file).unwrap_or(Value::Null);
            // These are our prepared immutable copies, never a journal path.
            // The existing private-file protocol validates the cache location
            // again when read, including size, ownership and symlink checks.
            if file.image_size.is_some() {
                if let (Some(parent), Some(name)) =
                    (file.local_path.parent(), file.local_path.file_name())
                {
                    if let Some(digest) = parent.file_name() {
                        value["preview_path"] = json!(format!(
                            "cache/{}/{}",
                            digest.to_string_lossy(),
                            name.to_string_lossy()
                        ));
                    }
                }
            }
            value
        })
        .collect()
}

impl LocalMessages {
    pub fn rows(&self, key: &str) -> &[LocalMessage] {
        self.rows.get(key).map_or(&[], Vec::as_slice)
    }

    /// The keys of the sessions that have rows, at most [`MAX_SESSIONS`].
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.rows.keys().map(String::as_str)
    }

    fn touch(&mut self, key: &str) {
        self.clock += 1;
        self.touched.insert(key.to_owned(), self.clock);
    }

    fn drop_key(&mut self, key: &str) {
        self.rows.remove(key);
        self.touched.remove(key);
    }

    /// Keeps at most [`MAX_PER_SESSION`] rows in `key`, and at most
    /// [`MAX_SESSIONS`] sessions, never dropping `key` or a session with a
    /// row still sending.
    fn trim(&mut self, key: &str) {
        if let Some(rows) = self.rows.get_mut(key) {
            let excess = rows.len().saturating_sub(MAX_PER_SESSION);
            rows.drain(..excess);
            if rows.is_empty() {
                self.drop_key(key);
            }
        }
        while self.rows.len() > MAX_SESSIONS {
            let oldest = self
                .rows
                .iter()
                .filter(|(other, rows)| {
                    other.as_str() != key && rows.iter().all(|row| row.status != Status::Sending)
                })
                .min_by_key(|(other, _)| self.touched.get(other.as_str()).copied().unwrap_or(0))
                .map(|(other, _)| other.clone());
            match oldest {
                Some(oldest) => self.drop_key(&oldest),
                None => break,
            }
        }
    }

    /// A send started. Earlier failed rows of the same retry, or with the
    /// same text and files, are replaced.
    #[allow(clippy::too_many_arguments)]
    pub fn sending(
        &mut self,
        key: &str,
        request_id: &str,
        retry_id: Option<&str>,
        text: &str,
        files: &[StoredFile],
        run_id: &str,
        conversation_id: &str,
        now: f64,
    ) {
        let attachments = files_value(files);
        let rows = self.rows.entry(key.to_owned()).or_default();
        rows.retain(|row| {
            !(row.status == Status::Error
                && (Some(row.id.as_str()) == retry_id
                    || (row.text == text && row.attachments == attachments)))
        });
        rows.push(LocalMessage {
            id: request_id.into(),
            text: text.into(),
            status: Status::Sending,
            attachments,
            run_id: run_id.into(),
            conversation_id: conversation_id.into(),
            submitted_at: now,
            error: String::new(),
            uncertain: false,
            message_id: None,
            submitted_text: None,
            source: None,
            voice: None,
        });
        self.touch(key);
        self.trim(key);
    }

    /// A send ended. Returns the detail the composer shows, empty on
    /// success.
    pub fn finished(&mut self, key: &str, request_id: &str, outcome: &Outcome) -> String {
        let detail = match outcome {
            Outcome::Done { .. } => String::new(),
            Outcome::Failed { detail } => detail.clone(),
            Outcome::Uncertain { detail } => uncertain_detail(detail),
        };
        if self.rows.contains_key(key) {
            self.touch(key);
        }
        let Some(row) = self
            .rows
            .get_mut(key)
            .and_then(|rows| rows.iter_mut().find(|row| row.id == request_id))
        else {
            return detail;
        };
        match outcome {
            Outcome::Done { receipt, .. } => {
                row.status = Status::Sent;
                row.error.clear();
                row.uncertain = false;
                let request = receipt.get("request_id").cloned().unwrap_or(Value::Null);
                row.message_id = request.as_str().map(str::to_owned);
                for (index, file) in row.attachments.iter_mut().enumerate() {
                    file["request_id"] = request.clone();
                    file["index"] = json!(index);
                }
                row.submitted_text = receipt
                    .get("submitted_text")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(at) = receipt.get("submitted_at").and_then(Value::as_f64) {
                    row.submitted_at = at;
                }
            }
            Outcome::Failed { .. } | Outcome::Uncertain { .. } => {
                row.status = Status::Error;
                row.error = detail.clone();
                row.uncertain = matches!(outcome, Outcome::Uncertain { .. });
            }
        }
        detail
    }

    /// Drops rows the agent's own history now shows, and rows of another run
    /// or conversation (except those still sending). `details` and `events`
    /// are the session's current inspection.
    pub fn reconcile(&mut self, key: &str, details: &Value, events: &[Value]) -> Reconciled {
        let mut result = Reconciled::default();
        let Some(rows) = self.rows.get_mut(key) else {
            return result;
        };
        let run = details["run_id"].as_str().unwrap_or("");
        let conversation = details["conversation_id"].as_str().unwrap_or("");
        let listed = |list: &str, id: &Option<String>| {
            id.as_deref().is_some_and(|id| {
                !id.is_empty()
                    && details[list].as_array().is_some_and(|items| {
                        items.iter().any(|item| item["message_id"] == json!(id))
                    })
            })
        };
        rows.retain(|row| {
            if row.status == Status::Sending {
                return true;
            }
            if (!run.is_empty() && row.run_id != run)
                || (!conversation.is_empty() && row.conversation_id != conversation)
            {
                result.removed += 1;
                return false;
            }
            if row.source.as_deref() == Some("hgs_voice") {
                let shown = listed("voice_messages", &row.message_id);
                result.removed += usize::from(shown);
                return !shown;
            }
            let failed = row.status == Status::Error;
            if listed("attachment_messages", &row.message_id) {
                result.removed += 1;
                return false;
            }
            // A file card stays until the durable attachment metadata arrives.
            if !row.attachments.is_empty() {
                return true;
            }
            let submitted = row
                .submitted_text
                .as_deref()
                .unwrap_or(&row.text)
                .trim()
                .to_owned();
            if submitted.is_empty() {
                return true;
            }
            let prefix: String = submitted.chars().take(1000).collect();
            let shown = events.iter().any(|event| {
                let detail = event["detail"].as_str().unwrap_or("").trim();
                matches!(
                    event["type"].as_str(),
                    Some("UserPromptSubmit" | "UserPromptQueued" | "TurnStarted")
                ) && event["agent_id"].as_str().unwrap_or("").is_empty()
                    && event["at"].as_f64().unwrap_or(0.0) >= row.submitted_at - 2.0
                    && !detail.is_empty()
                    && if failed {
                        detail == submitted
                    } else {
                        detail.starts_with(&prefix)
                    }
            });
            if shown {
                result.removed += 1;
                if failed {
                    result.delivered.push(row.text.clone());
                }
            }
            !shown
        });
        if rows.is_empty() {
            self.drop_key(key);
        }
        result
    }

    /// The row Retry would send: a definite failure of the run and
    /// conversation the session has now. The caller also checks that the
    /// session takes messages and that no retry read is in flight.
    pub fn retryable(
        &self,
        key: &str,
        id: &str,
        run_id: &str,
        conversation_id: &str,
    ) -> Option<&LocalMessage> {
        self.rows(key).iter().find(|row| {
            row.id == id
                && row.status == Status::Error
                && !row.uncertain
                && row.run_id == run_id
                && row.conversation_id == conversation_id
        })
    }

    /// The retried row's cached files, to read back with
    /// [`crate::attachments::read_stored`].
    pub fn stored_files(row: &LocalMessage) -> Option<Vec<StoredFile>> {
        row.attachments
            .iter()
            .map(|file| serde_json::from_value(file.clone()).ok())
            .collect()
    }

    /// Delete removes a failed row only.
    pub fn delete(&mut self, key: &str, id: &str) -> bool {
        let Some(rows) = self.rows.get_mut(key) else {
            return false;
        };
        let before = rows.len();
        rows.retain(|row| !(row.id == id && row.status == Status::Error));
        let removed = rows.len() != before;
        if rows.is_empty() {
            self.drop_key(key);
        } else if removed {
            self.touch(key);
        }
        removed
    }

    /// A voice message the voice composer submitted or updated.
    pub fn voice_message(&mut self, key: &str, message: &Value) {
        let id = message["id"].as_str().unwrap_or("").to_owned();
        let rows = self.rows.entry(key.to_owned()).or_default();
        rows.retain(|row| row.message_id.as_deref() != Some(id.as_str()));
        rows.push(LocalMessage {
            id: format!("voice-{id}"),
            text: message["transcript"].as_str().unwrap_or("").into(),
            status: Status::Sent,
            attachments: vec![message["audio"].clone()],
            run_id: message["run_id"].as_str().unwrap_or("").into(),
            conversation_id: message["conversation_id"].as_str().unwrap_or("").into(),
            submitted_at: message["created_at"].as_f64().unwrap_or(0.0),
            error: String::new(),
            uncertain: false,
            message_id: Some(id),
            submitted_text: None,
            source: Some("hgs_voice".into()),
            voice: Some(message.clone()),
        });
        self.touch(key);
        self.trim(key);
    }

    /// A session renamed in place, or a run continued under a new key: its
    /// rows move unless the new key already has rows of its own.
    pub fn rename(&mut self, old: &str, new: &str) {
        if self.rows.get(new).is_some_and(|rows| !rows.is_empty()) {
            return;
        }
        if let Some(rows) = self.rows.remove(old) {
            self.touched.remove(old);
            self.rows.insert(new.to_owned(), rows);
            self.touch(new);
        }
    }

    /// Drops a session's rows, as when it leaves the catalog.
    pub fn forget(&mut self, key: &str) {
        self.drop_key(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "studio\nclaude/sample-project/review";

    fn file(name: &str) -> StoredFile {
        StoredFile {
            name: name.into(),
            mime: "text/plain".into(),
            bytes: 5,
            reference: "[File #1]".into(),
            local_path: "/home/example/.cache/attachments/abc/a.txt".into(),
            image_size: None,
        }
    }

    fn send(messages: &mut LocalMessages, id: &str, text: &str, files: &[StoredFile]) {
        messages.sending(KEY, id, None, text, files, "run-1", "conv-1", 100.0);
    }

    #[test]
    fn keys_name_every_session_with_rows_and_follow_its_bound() {
        let mut messages = LocalMessages::default();
        assert_eq!(messages.keys().count(), 0);
        for n in 0..MAX_SESSIONS + 3 {
            let key = format!("studio\nclaude/sample-project/s{n}");
            messages.sending(&key, "1", None, "hi", &[], "run-1", "conv-1", 100.0);
            messages.finished(
                &key,
                "1",
                &Outcome::Failed {
                    detail: "refused".into(),
                },
            );
        }
        let keys: Vec<String> = messages.keys().map(str::to_owned).collect();
        assert_eq!(keys.len(), MAX_SESSIONS);
        assert!(keys.iter().all(|key| !messages.rows(key).is_empty()));
        assert!(
            !keys
                .iter()
                .any(|key| key == "studio\nclaude/sample-project/s0"),
            "the oldest went"
        );
        messages.forget(&keys[0]);
        assert_eq!(messages.keys().count(), MAX_SESSIONS - 1);
    }

    #[test]
    fn a_send_is_kept_until_finished_and_failures_are_replaced_by_their_retry() {
        let mut messages = LocalMessages::default();
        send(&mut messages, "1", "hello", &[]);
        assert_eq!(messages.rows(KEY)[0].status, Status::Sending);
        let detail = messages.finished(
            KEY,
            "1",
            &Outcome::Failed {
                detail: "no such session".into(),
            },
        );
        assert_eq!(detail, "no such session");
        let row = &messages.rows(KEY)[0];
        assert_eq!((row.status, row.uncertain), (Status::Error, false));
        assert!(messages.retryable(KEY, "1", "run-1", "conv-1").is_some());
        assert!(messages.retryable(KEY, "1", "run-2", "conv-1").is_none());
        // The retry replaces the failed row.
        messages.sending(KEY, "2", Some("1"), "hello", &[], "run-1", "conv-1", 101.0);
        assert_eq!(
            messages
                .rows(KEY)
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["2"]
        );
        let detail = messages.finished(
            KEY,
            "2",
            &Outcome::Uncertain {
                detail: "Delivery timed out.".into(),
            },
        );
        assert_eq!(
            detail,
            "Delivery is not confirmed. Check Terminal before retrying. Delivery timed out."
        );
        assert!(messages.rows(KEY)[0].uncertain);
        assert!(
            messages.retryable(KEY, "2", "run-1", "conv-1").is_none(),
            "never retried"
        );
        // The same text and files replace a failure too.
        messages.sending(KEY, "3", None, "hello", &[], "run-1", "conv-1", 102.0);
        assert_eq!(messages.rows(KEY).len(), 1);
        assert!(!messages.delete(KEY, "3"), "a sending row is not deleted");
    }

    #[test]
    fn a_receipt_names_the_message_and_its_files() {
        let mut messages = LocalMessages::default();
        send(
            &mut messages,
            "1",
            "see [File #1]",
            &[file("a.txt"), file("b.txt")],
        );
        let receipt = json!({"request_id": "req-1", "submitted_text": "see [File #1] (2 files)", "submitted_at": 105.0});
        let detail = messages.finished(
            KEY,
            "1",
            &Outcome::Done {
                receipt,
                cleanup: None,
            },
        );
        assert_eq!(detail, "");
        let row = &messages.rows(KEY)[0];
        assert_eq!(row.status, Status::Sent);
        assert_eq!(row.message_id.as_deref(), Some("req-1"));
        assert_eq!(row.submitted_at, 105.0);
        assert_eq!(
            (
                row.attachments[1]["request_id"].clone(),
                row.attachments[1]["index"].clone()
            ),
            (json!("req-1"), json!(1))
        );
        assert_eq!(LocalMessages::stored_files(row).unwrap()[0].name, "a.txt");
    }

    #[test]
    fn rows_leave_when_the_history_shows_them() {
        let mut messages = LocalMessages::default();
        send(&mut messages, "1", "  ship it  ", &[]);
        messages.finished(
            KEY,
            "1",
            &Outcome::Done {
                receipt: json!({"request_id": "m1"}),
                cleanup: None,
            },
        );
        send(&mut messages, "2", "exact failure", &[]);
        messages.finished(KEY, "2", &Outcome::Failed { detail: "x".into() });
        send(&mut messages, "3", "with a file", &[file("a.txt")]);
        messages.finished(
            KEY,
            "3",
            &Outcome::Done {
                receipt: json!({"request_id": "m3"}),
                cleanup: None,
            },
        );
        send(&mut messages, "4", "still sending", &[]);
        let details = json!({"run_id": "run-1", "conversation_id": "conv-1"});
        let events = [
            json!({"type": "UserPromptSubmit", "detail": "ship it and more", "at": 101.0}),
            json!({"type": "UserPromptSubmit", "detail": "exact failure", "at": 99.0}),
            // A subagent's prompt or one long before the send never matches.
            json!({"type": "TurnStarted", "detail": "with a file", "at": 101.0, "agent_id": "a"}),
        ];
        let reconciled = messages.reconcile(KEY, &details, &events);
        assert_eq!(
            reconciled,
            Reconciled {
                removed: 2,
                delivered: vec!["exact failure".into()]
            }
        );
        let left: Vec<_> = messages
            .rows(KEY)
            .iter()
            .map(|row| row.id.as_str())
            .collect();
        assert_eq!(left, ["3", "4"]);
        // The durable attachment metadata removes the file card.
        let details = json!({"run_id": "run-1", "conversation_id": "conv-1", "attachment_messages": [{"message_id": "m3"}]});
        assert_eq!(messages.reconcile(KEY, &details, &[]).removed, 1);
        // A new conversation drops the rest except what is still sending.
        messages.finished(KEY, "4", &Outcome::Failed { detail: "x".into() });
        send(&mut messages, "5", "fresh", &[]);
        let details = json!({"run_id": "run-1", "conversation_id": "conv-2"});
        messages.reconcile(KEY, &details, &[]);
        let left: Vec<_> = messages
            .rows(KEY)
            .iter()
            .map(|row| row.id.as_str())
            .collect();
        assert_eq!(left, ["5"]);
    }

    #[test]
    fn sessions_are_capped_without_dropping_a_send_in_flight() {
        let mut messages = LocalMessages::default();
        let key = |index: usize| format!("studio\nclaude/p/s{index}");
        // The first session still has a send in flight; the second failed.
        messages.sending(&key(0), "r0", None, "pending", &[], "run", "conv", 1.0);
        messages.sending(&key(1), "r1", None, "failed", &[], "run", "conv", 1.0);
        messages.finished(&key(1), "r1", &Outcome::Failed { detail: "x".into() });
        for index in 2..MAX_SESSIONS {
            messages.sending(&key(index), "r", None, "old", &[], "run", "conv", 1.0);
            messages.finished(&key(index), "r", &Outcome::Failed { detail: "x".into() });
        }
        // Touching the failed session keeps it newer than the rest.
        messages.finished(
            &key(1),
            "r1",
            &Outcome::Failed {
                detail: "again".into(),
            },
        );
        messages.sending(
            &key(MAX_SESSIONS),
            "r",
            None,
            "new",
            &[],
            "run",
            "conv",
            1.0,
        );
        assert_eq!(messages.rows.len(), MAX_SESSIONS);
        assert_eq!(messages.touched.len(), MAX_SESSIONS);
        assert_eq!(messages.rows(&key(0)).len(), 1, "a send in flight stays");
        assert_eq!(messages.rows(&key(1)).len(), 1, "recently touched stays");
        assert!(
            messages.rows(&key(2)).is_empty(),
            "the least recently touched goes"
        );
        assert_eq!(messages.rows(&key(MAX_SESSIONS)).len(), 1);
        messages.forget(&key(0));
        assert_eq!(
            (messages.rows.len(), messages.touched.len()),
            (MAX_SESSIONS - 1, MAX_SESSIONS - 1)
        );
    }

    #[test]
    fn voice_rows_bounds_renames_and_inspect() {
        let mut messages = LocalMessages::default();
        let voice = json!({"id": "v1", "transcript": "hello", "run_id": "run-1", "conversation_id": "conv-1",
                           "created_at": 100.0, "audio": {"name": "voice.wav"}});
        messages.voice_message(KEY, &voice);
        messages.voice_message(KEY, &voice);
        assert_eq!(messages.rows(KEY).len(), 1, "an update replaces the row");
        assert_eq!(messages.rows(KEY)[0].id, "voice-v1");
        let details = json!({"run_id": "run-1", "conversation_id": "conv-1", "voice_messages": [{"message_id": "v1"}]});
        assert_eq!(messages.reconcile(KEY, &details, &[]).removed, 1);
        assert!(messages.rows(KEY).is_empty());
        for index in 0..60 {
            send(&mut messages, &index.to_string(), &format!("m{index}"), &[]);
        }
        assert_eq!(messages.rows(KEY).len(), MAX_PER_SESSION);
        assert_eq!(messages.rows(KEY)[0].id, "10");
        messages.rename(KEY, "studio\nclaude/sample-project/renamed");
        assert!(messages.rows(KEY).is_empty());
        assert_eq!(
            messages.rows("studio\nclaude/sample-project/renamed").len(),
            50
        );
        assert_eq!(inspect("dsh", true, "studio"), Inspect::NativeUi);
        assert_eq!(inspect("claude", true, "studio"), Inspect::Terminal);
        assert_eq!(
            inspect("opencode", false, "studio"),
            Inspect::Refresh {
                notice: "Refreshing the native session. Check its provider configuration on studio if the error remains.".into()
            }
        );
    }
}
