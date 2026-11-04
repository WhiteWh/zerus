//! Remove worktree after Terminate, and the batch cleanup review behind it,
//! ported from the Qt client (`SessionsWindow::terminateSession`,
//! `reviewTerminatedWorktrees`, `WorktreeBatchCleanupDialog`,
//! `WorktreeCleanupStatus`) as a pure state machine the view drives.
//!
//! The rules `AGENTS.md` asks for: removal only after the explicit Remove
//! worktree choice and an exactly successful termination; each folder is
//! reviewed first, and the background pass removes it only when the review
//! is verified, ready for exit cleanup, and its checkout ownership matches
//! the owner recorded at launch; anything else is kept for review with its
//! reason. An unknown outcome is final and never replayed; only a verified
//! refusal is reviewed again ("Check again").
use crate::actions::WorktreeCleanup;
use crate::worktree_review::{
    exit_cleanup_ready, local_data, process_reviewable, removable, Removal, Removed,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

mod capture;
pub use capture::Capture;
mod report;
pub use report::{retained_notice, ArchiveIdentity};

/// The Terminate dialog's removal option and its texts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalOption {
    pub label: String,
    pub tooltip: String,
    pub informative: String,
}

/// Whether Terminate offers to remove the working folder: a live session
/// that keeps its archive (a tracked codex, claude or kimi session) in a
/// linked worktree, as its row says or an inspection of the exact run.
pub fn removal_offered(
    live: bool,
    tracked: bool,
    cmd: &str,
    row_worktree: bool,
    exact_inspection_worktree: bool,
) -> bool {
    let keeps_archive = live && tracked && matches!(cmd, "codex" | "claude" | "kimi");
    keeps_archive && (row_worktree || exact_inspection_worktree)
}

pub fn removal_option(machine: &str) -> RemovalOption {
    RemovalOption {
        label: "Remove the working folder after stopping".into(),
        tooltip: "Cleanup runs in the background. Unsaved work, unknown local files and folders in use are kept for review.".into(),
        informative: format!(
            "Computer: {machine}\nThe conversation and branch are kept. Folders that need review stay on disk."
        ),
    }
}

/// The confirm button with the option shown.
pub fn removal_button(checked: bool) -> &'static str {
    if checked {
        "Stop and remove folder"
    } else {
        "Stop and archive"
    }
}

/// After the dialog the session must still be the one asked about.
pub const STATE_CHANGED: &str = "The session state changed. Open its actions again.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Removed,
    Kept,
    /// Unknown: never repeated.
    Unconfirmed,
}

/// One folder of a cleanup.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub host: String,
    pub machine: String,
    pub common_dir: String,
    /// A cleanup after Terminate (no branch deletion; archive restore kept).
    pub exit_cleanup: bool,
    /// The checkout owner recorded at launch; removal needs the same one.
    pub expected_owner_id: String,
    /// The worktree's review row; `{path, kind}` until reviewed.
    pub row: Value,
    pub status: Option<Status>,
    pub detail: String,
    pub review_error: String,
    pub verified: bool,
    pub offline: bool,
    pub forgotten: bool,
}

impl Entry {
    /// The folder of a session that terminated with the Remove choice
    /// (`reviewTerminatedWorktrees`). `reported` is the cleanup line the
    /// termination itself printed, if any: a removal or an unknown outcome
    /// there is final; a kept folder is reviewed.
    pub fn terminated(
        host: &str,
        machine: &str,
        root: &str,
        common_dir: &str,
        owner_id: &str,
        reported: Option<&WorktreeCleanup>,
    ) -> Option<Entry> {
        if root.is_empty() || common_dir.is_empty() {
            return None;
        }
        let mut entry = Entry {
            host: host.into(),
            machine: machine.into(),
            common_dir: common_dir.into(),
            exit_cleanup: true,
            expected_owner_id: owner_id.into(),
            row: serde_json::json!({"path": root, "kind": "linked"}),
            status: None,
            detail: String::new(),
            review_error: String::new(),
            verified: false,
            offline: false,
            forgotten: false,
        };
        if let Some(reported) = reported {
            if reported.path != root
                || reported.common_dir != common_dir
                || reported.status != "kept"
            {
                entry.status = Some(
                    if reported.path == root
                        && reported.common_dir == common_dir
                        && reported.status == "removed"
                    {
                        Status::Removed
                    } else {
                        Status::Unconfirmed
                    },
                );
                entry.detail = reported.reason.clone().unwrap_or_default();
            }
        }
        Some(entry)
    }

    fn path(&self) -> &str {
        self.row["path"].as_str().unwrap_or("")
    }

    fn terminal(&self) -> bool {
        matches!(self.status, Some(Status::Removed | Status::Unconfirmed))
    }

    fn discardable(&self) -> bool {
        self.row["discardable"] == Value::Bool(true)
            && !self.row["discard_fingerprint"]
                .as_str()
                .unwrap_or("")
                .is_empty()
    }

    fn eligible(&self) -> bool {
        !self.terminal()
            && self.verified
            && (removable(&self.row) || self.discardable() || process_reviewable(&self.row))
    }

    /// Ready for the background pass: verified, ready for exit cleanup, and
    /// owned by the same checkout owner recorded at launch.
    fn automatic_ready(&self) -> bool {
        self.eligible()
            && self.exit_cleanup
            && !self.expected_owner_id.is_empty()
            && self.row["ownership_id"].as_str() == Some(self.expected_owner_id.as_str())
            && exit_cleanup_ready(&self.row)
    }
}

/// The user's explicit consents for a manual removal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consents {
    pub local_files: bool,
    pub discard_changes: bool,
    /// Remove even if some running programs cannot be checked (five minutes,
    /// this folder only, held by `hgs`).
    pub unchecked_processes: bool,
    pub merged_branches: bool,
}

/// What the cleanup asks of its caller.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Review this folder: `hgs worktrees review --path COMMON_DIR --worktree
    /// PATH` on `host`.
    Review {
        index: usize,
        host: String,
        common_dir: String,
        worktree: String,
    },
    /// Remove this folder.
    Remove {
        index: usize,
        host: String,
        removal: Removal,
    },
    /// Waiting for the user.
    Idle,
    /// Nothing left.
    Done,
}

/// A cleanup of several folders: reviews first, then removals one at a time.
#[derive(Clone, Debug, PartialEq)]
pub struct Cleanup {
    pub entries: Vec<Entry>,
    background: bool,
    review_index: usize,
    index: usize,
    running: bool,
    automatic: bool,
    auto_scheduled: bool,
    stop: bool,
    consents: Consents,
}

impl Cleanup {
    /// The background cleanup after Terminate.
    pub fn after_termination(entries: Vec<Entry>) -> Cleanup {
        Cleanup::new(entries, true)
    }

    /// A cleanup the user started from a review.
    pub fn manual(entries: Vec<Entry>) -> Cleanup {
        Cleanup::new(entries, false)
    }

    /// Outcomes already observed by Archive. Inspecting them never reviews or
    /// removes again; a verified refusal needs the existing Check again flow.
    pub fn retained(entries: Vec<Entry>) -> Cleanup {
        let count = entries.len();
        let mut cleanup = Cleanup::new(entries, false);
        cleanup.review_index = count;
        cleanup.auto_scheduled = true;
        cleanup
    }

    fn new(entries: Vec<Entry>, background: bool) -> Cleanup {
        Cleanup {
            entries,
            background,
            review_index: 0,
            index: 0,
            running: false,
            automatic: false,
            auto_scheduled: false,
            stop: false,
            consents: Consents::default(),
        }
    }

    pub fn busy(&self) -> bool {
        self.running || self.review_index < self.entries.len()
    }

    pub fn count(&self, status: Status) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.status == Some(status))
            .count()
    }

    /// Whether a refused folder can be reviewed again.
    pub fn can_refresh(&self) -> bool {
        self.entries.iter().any(|entry| !entry.terminal())
    }

    /// The next step: the next review, then (all reviewed) the background
    /// pass or waiting for the user.
    pub fn step(&mut self) -> Step {
        if self.running {
            return self.next_removal();
        }
        while self.review_index < self.entries.len() {
            let index = self.review_index;
            let entry = &mut self.entries[index];
            if entry.terminal() {
                self.review_index += 1;
                continue;
            }
            entry.verified = false;
            if entry.offline || entry.common_dir.is_empty() {
                entry.review_error = "Reconnect this computer to check the folder.".into();
                self.finish_review();
                continue;
            }
            return Step::Review {
                index,
                host: entry.host.clone(),
                common_dir: entry.common_dir.clone(),
                worktree: entry.path().to_owned(),
            };
        }
        let automatic = !self.auto_scheduled
            && (self.background
                || (!self.entries.is_empty() && self.entries.iter().all(Entry::automatic_ready)));
        if automatic {
            self.auto_scheduled = true;
            return self.start(true, Consents::default());
        }
        if self.entries.iter().all(Entry::terminal) {
            Step::Done
        } else {
            Step::Idle
        }
    }

    /// A review's reply for the folder under review: verified only when it
    /// names exactly one row for that path in that repository. Returns the
    /// next step.
    pub fn reviewed(
        &mut self,
        host: &str,
        common_dir: &str,
        reply: Result<&Value, String>,
    ) -> Step {
        let Some(entry) = self.entries.get_mut(self.review_index) else {
            return self.step();
        };
        match reply {
            Ok(data) => {
                let target = entry.path().to_owned();
                let rows: Vec<&Value> = if host == entry.host
                    && common_dir == entry.common_dir
                    && data["state"] == "ok"
                    && data["common_dir"].as_str() == Some(entry.common_dir.as_str())
                {
                    data["worktrees"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|row| row["path"].as_str() == Some(target.as_str()))
                        .collect()
                } else {
                    Vec::new()
                };
                if rows.len() == 1 {
                    entry.row = rows[0].clone();
                    entry.verified = true;
                } else {
                    entry.review_error =
                        "Could not verify this folder. Check again after reconnecting.".into();
                }
            }
            Err(error) => entry.review_error = error,
        }
        self.finish_review();
        self.step()
    }

    fn finish_review(&mut self) {
        if let Some(entry) = self.entries.get_mut(self.review_index) {
            if !entry.eligible() {
                entry.status = Some(Status::Kept);
            }
        }
        self.review_index += 1;
    }

    /// The consents the reviewed folders need before the user may remove
    /// them: local files, uncommitted changes, unchecked programs.
    pub fn required(&self) -> Consents {
        let mut required = Consents::default();
        for entry in self.entries.iter().filter(|entry| entry.eligible()) {
            required.local_files |= local_data(&entry.row);
            required.discard_changes |= entry.discardable();
            required.unchecked_processes |= process_reviewable(&entry.row);
        }
        required
    }

    /// The user removes the reviewed folders with these consents; nothing
    /// starts while one the folders need is missing.
    pub fn remove(&mut self, consents: Consents) -> Step {
        let required = self.required();
        let missing = (required.local_files && !consents.local_files)
            || (required.discard_changes && !consents.discard_changes)
            || (required.unchecked_processes && !consents.unchecked_processes);
        if self.busy() || missing || !self.entries.iter().any(Entry::eligible) {
            return Step::Idle;
        }
        self.start(false, consents)
    }

    fn start(&mut self, automatic: bool, consents: Consents) -> Step {
        self.running = true;
        self.automatic = automatic;
        self.consents = consents;
        self.index = 0;
        self.next_removal()
    }

    fn next_removal(&mut self) -> Step {
        while self.index < self.entries.len() {
            let index = self.index;
            let automatic = self.automatic;
            let consents = self.consents;
            let entry = &mut self.entries[index];
            if entry.terminal() {
                self.index += 1;
                continue;
            }
            if self.stop || !entry.eligible() || (automatic && !entry.automatic_ready()) {
                entry.status = Some(Status::Kept);
                if self.stop {
                    entry.verified = false;
                }
                self.index += 1;
                continue;
            }
            let row = &entry.row;
            let text = |key: &str| row[key].as_str().unwrap_or("").to_owned();
            let removal = Removal {
                path: text("path"),
                common_dir: entry.common_dir.clone(),
                fingerprint: text("fingerprint"),
                delete_branch: !automatic
                    && consents.merged_branches
                    && row["merged"] == Value::Bool(true),
                ignored_fingerprint: if (automatic || consents.local_files) && local_data(row) {
                    text("ignored_fingerprint")
                } else {
                    String::new()
                },
                discard_fingerprint: if !automatic && consents.discard_changes {
                    text("discard_fingerprint")
                } else {
                    String::new()
                },
                process_fingerprint: if !automatic
                    && consents.unchecked_processes
                    && process_reviewable(row)
                {
                    text("process_fingerprint")
                } else {
                    String::new()
                },
                preserve_archive_restore: entry.exit_cleanup
                    && row["archive_restore_ready"] == Value::Bool(true),
            };
            return Step::Remove {
                index,
                host: entry.host.clone(),
                removal,
            };
        }
        self.running = false;
        self.automatic = false;
        if self.entries.iter().all(Entry::terminal) {
            Step::Done
        } else {
            Step::Idle
        }
    }

    /// The removal's outcome for the folder in flight, and the next step. A
    /// verified refusal needs a new review before another attempt.
    pub fn removed(&mut self, outcome: &Removed) -> Step {
        let Some(entry) = self.entries.get_mut(self.index) else {
            return self.step();
        };
        match outcome {
            Removed::Done { reply } => {
                entry.status = Some(Status::Removed);
                entry.detail = reply["branch_error"].as_str().unwrap_or("").into();
                entry.forgotten = reply["status"] == "forgotten";
            }
            Removed::Refused { reply, detail } => {
                entry.status = Some(if reply["status"] == "kept" {
                    Status::Kept
                } else {
                    Status::Unconfirmed
                });
                entry.detail = detail.clone();
                entry.verified = false;
            }
            Removed::Unknown { detail } => {
                entry.status = Some(Status::Unconfirmed);
                entry.detail = detail.clone();
                entry.verified = false;
            }
        }
        self.index += 1;
        self.next_removal()
    }

    /// Keep the folders not yet removed; the one in flight finishes.
    pub fn stop(&mut self) {
        self.stop = true;
    }

    /// "Check again": review every folder that is not final.
    pub fn review_remaining(&mut self) {
        if self.busy() || !self.can_refresh() {
            return;
        }
        self.auto_scheduled = true;
        self.stop = false;
        self.index = 0;
        self.review_index = 0;
        for entry in &mut self.entries {
            if !entry.terminal() {
                entry.verified = false;
                entry.review_error.clear();
                entry.detail.clear();
                entry.status = None;
            }
        }
    }

    /// Why the first kept folder stayed.
    pub fn retained_reason(&self) -> Option<String> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.status == Some(Status::Kept))?;
        let first = |text: &str| -> String {
            text.split('\n')
                .next()
                .unwrap_or("")
                .chars()
                .take(180)
                .collect()
        };
        if !entry.detail.is_empty() {
            return Some(first(&entry.detail));
        }
        if !entry.review_error.is_empty() {
            return Some(first(&entry.review_error));
        }
        let row = &entry.row;
        let has = |code: &str| crate::worktree_review::has_reason(row, &[code]);
        Some(if has("processes") {
            "A running program uses the folder.".into()
        } else if has("sessions") {
            "Another session uses the folder.".into()
        } else if has("processes_unavailable") {
            "Some running programs could not be checked.".into()
        } else if entry.discardable() {
            "Uncommitted changes need review.".into()
        } else if local_data(row) {
            "Local files need review.".into()
        } else if entry.expected_owner_id.is_empty()
            || row["ownership_id"].as_str() != Some(entry.expected_owner_id.as_str())
        {
            "Checkout ownership needs review.".into()
        } else {
            crate::worktree_review::summary(row)
                .join(" ")
                .chars()
                .take(180)
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ready_row(owner: &str) -> Value {
        json!({"kind": "linked", "verdict": "ready", "path": "/home/example/repo-work-1", "fingerprint": "f1",
               "ownership_id": owner, "zerus_owned": true, "archive_restore_ready": true, "reasons": [],
               "changes": 0, "untracked": 0, "disk_complete": true, "ignored_complete": true, "ignored_count": 0,
               "integration": {"kind": "ancestor", "base": "main"}, "merged": true})
    }

    fn review_of(row: Value) -> Value {
        json!({"state": "ok", "common_dir": "/home/example/repo/.git", "worktrees": [row]})
    }

    fn terminated(owner: &str) -> Entry {
        Entry::terminated(
            "studio",
            "studio",
            "/home/example/repo-work-1",
            "/home/example/repo/.git",
            owner,
            None,
        )
        .unwrap()
    }

    #[test]
    fn retained_archive_outcomes_never_start_another_review_or_removal() {
        let mut kept = terminated("owner-1");
        kept.status = Some(Status::Kept);
        kept.row = ready_row("owner-1");
        kept.verified = true;
        let mut cleanup = Cleanup::retained(vec![kept]);
        assert!(!cleanup.busy());
        assert_eq!(cleanup.step(), Step::Idle);
        assert!(cleanup.can_refresh());
        cleanup.entries[0].status = Some(Status::Unconfirmed);
        assert!(!cleanup.can_refresh());
        assert_eq!(cleanup.step(), Step::Done);
    }

    #[test]
    fn terminate_offers_removal_with_qt_texts() {
        assert!(removal_offered(true, true, "claude", true, false));
        assert!(removal_offered(true, true, "codex", false, true));
        assert!(
            !removal_offered(true, true, "opencode", true, true),
            "no archive kept"
        );
        assert!(
            !removal_offered(true, true, "claude", false, false),
            "not a worktree"
        );
        assert!(
            !removal_offered(false, true, "claude", true, true),
            "not live"
        );
        let option = removal_option("studio");
        assert_eq!(option.label, "Remove the working folder after stopping");
        assert_eq!(option.informative, "Computer: studio\nThe conversation and branch are kept. Folders that need review stay on disk.");
        assert_eq!(
            (removal_button(true), removal_button(false)),
            ("Stop and remove folder", "Stop and archive")
        );
    }

    #[test]
    fn the_background_pass_removes_only_an_owned_ready_folder() {
        let mut cleanup = Cleanup::after_termination(vec![terminated("owner-1")]);
        let Step::Review {
            index,
            host,
            common_dir,
            worktree,
        } = cleanup.step()
        else {
            panic!("review first")
        };
        assert_eq!(
            (index, host.as_str(), worktree.as_str()),
            (0, "studio", "/home/example/repo-work-1")
        );
        cleanup.reviewed(&host, &common_dir, Ok(&review_of(ready_row("owner-1"))));
        let Step::Remove { removal, .. } = cleanup.step() else {
            panic!("removal")
        };
        assert_eq!(removal.fingerprint, "f1");
        assert!(
            removal.preserve_archive_restore && !removal.delete_branch,
            "branches stay after Terminate"
        );
        assert!(removal.process_fingerprint.is_empty() && removal.discard_fingerprint.is_empty());
        cleanup.removed(&Removed::Done {
            reply: json!({"status": "removed"}),
        });
        assert_eq!(cleanup.step(), Step::Done);
        assert_eq!(cleanup.count(Status::Removed), 1);
    }

    #[test]
    fn anything_uncertain_is_kept_for_review_with_its_reason() {
        let cases: Vec<(Value, &str)> = vec![
            (
                ready_row("someone-else"),
                "Checkout ownership needs review.",
            ),
            (
                {
                    let mut row = ready_row("owner-1");
                    row["verdict"] = json!("review");
                    row["changes"] = json!(2);
                    row
                },
                "",
            ),
            (
                {
                    let mut row = ready_row("owner-1");
                    row["verdict"] = json!("blocked");
                    row["reasons"] = json!([{"code": "processes", "message": "x"}]);
                    row
                },
                "A running program uses the folder.",
            ),
        ];
        for (row, reason) in cases {
            let mut cleanup = Cleanup::after_termination(vec![terminated("owner-1")]);
            let Step::Review {
                host, common_dir, ..
            } = cleanup.step()
            else {
                panic!()
            };
            assert_eq!(
                cleanup.reviewed(&host, &common_dir, Ok(&review_of(row))),
                Step::Idle,
                "nothing removed"
            );
            assert_eq!(cleanup.count(Status::Kept), 1);
            if !reason.is_empty() {
                assert_eq!(cleanup.retained_reason().as_deref(), Some(reason));
            }
        }
        // A missing owner recorded at launch never removes.
        let mut cleanup = Cleanup::after_termination(vec![terminated("")]);
        let Step::Review {
            host, common_dir, ..
        } = cleanup.step()
        else {
            panic!()
        };
        assert_eq!(
            cleanup.reviewed(&host, &common_dir, Ok(&review_of(ready_row("")))),
            Step::Idle
        );
        // An unverified review keeps the folder with its error.
        let mut cleanup = Cleanup::after_termination(vec![terminated("owner-1")]);
        let Step::Review {
            host, common_dir, ..
        } = cleanup.step()
        else {
            panic!()
        };
        cleanup.reviewed(
            &host,
            &common_dir,
            Ok(&json!({"state": "ok", "common_dir": "/other/.git", "worktrees": []})),
        );
        assert_eq!(cleanup.step(), Step::Idle);
        assert_eq!(
            cleanup.retained_reason().as_deref(),
            Some("Could not verify this folder. Check again after reconnecting.")
        );
    }

    #[test]
    fn an_unknown_outcome_is_never_replayed() {
        let mut cleanup = Cleanup::after_termination(vec![terminated("owner-1")]);
        let Step::Review {
            host, common_dir, ..
        } = cleanup.step()
        else {
            panic!()
        };
        cleanup.reviewed(&host, &common_dir, Ok(&review_of(ready_row("owner-1"))));
        assert!(matches!(cleanup.step(), Step::Remove { .. }));
        cleanup.removed(&Removed::Unknown {
            detail: "timed out".into(),
        });
        assert_eq!(cleanup.step(), Step::Done);
        cleanup.review_remaining();
        assert_eq!(cleanup.step(), Step::Done, "no review, no removal again");
        assert_eq!(cleanup.count(Status::Unconfirmed), 1);
        // A verified refusal can be checked again.
        let mut refused = Cleanup::after_termination(vec![terminated("owner-1")]);
        let Step::Review {
            host, common_dir, ..
        } = refused.step()
        else {
            panic!()
        };
        refused.reviewed(&host, &common_dir, Ok(&review_of(ready_row("owner-1"))));
        refused.step();
        refused.removed(&Removed::Refused {
            reply: json!({"status": "kept"}),
            detail: "A program started.".into(),
        });
        assert_eq!(refused.step(), Step::Idle);
        assert_eq!(
            refused.retained_reason().as_deref(),
            Some("A program started.")
        );
        refused.review_remaining();
        assert!(matches!(refused.step(), Step::Review { .. }));
    }

    #[test]
    fn the_termination_s_own_report_is_respected() {
        let reported = |status: &str, common_dir: &str| WorktreeCleanup {
            status: status.into(),
            path: "/r-w".into(),
            common_dir: common_dir.into(),
            reason: Some("policy".into()),
            archive: None,
            review: None,
            details_omitted: false,
        };
        let removed = Entry::terminated(
            "",
            "arch",
            "/r-w",
            "/r/.git",
            "o",
            Some(&reported("removed", "/r/.git")),
        )
        .unwrap();
        assert_eq!(removed.status, Some(Status::Removed));
        let other = Entry::terminated(
            "",
            "arch",
            "/r-w",
            "/r/.git",
            "o",
            Some(&reported("removed", "/x/.git")),
        )
        .unwrap();
        assert_eq!(other.status, Some(Status::Unconfirmed));
        let kept = Entry::terminated(
            "",
            "arch",
            "/r-w",
            "/r/.git",
            "o",
            Some(&reported("kept", "/r/.git")),
        )
        .unwrap();
        assert_eq!(kept.status, None, "a kept folder is reviewed");
        assert!(Entry::terminated("", "arch", "", "/r/.git", "o", None).is_none());
        let mut cleanup = Cleanup::after_termination(vec![removed]);
        assert_eq!(cleanup.step(), Step::Done);
    }

    #[test]
    fn a_manual_removal_uses_only_the_consents_given() {
        let unchecked = |fingerprint: &str| {
            let mut row = ready_row("o");
            row["verdict"] = json!("blocked");
            row["reasons"] = json!([{"code": "processes_unavailable", "message": "x"}]);
            row["process_fingerprint"] = json!(fingerprint);
            row
        };
        let mut entry = terminated("o");
        entry.exit_cleanup = false;
        let mut cleanup = Cleanup::manual(vec![entry]);
        let Step::Review {
            host, common_dir, ..
        } = cleanup.step()
        else {
            panic!()
        };
        assert_eq!(
            cleanup.reviewed(&host, &common_dir, Ok(&review_of(unchecked("p1")))),
            Step::Idle,
            "waits for the user"
        );
        assert!(cleanup.required().unchecked_processes);
        assert_eq!(
            cleanup.remove(Consents::default()),
            Step::Idle,
            "the consent is missing"
        );
        let consents = Consents {
            unchecked_processes: true,
            merged_branches: true,
            ..Consents::default()
        };
        let Step::Remove { removal, .. } = cleanup.remove(consents) else {
            panic!()
        };
        assert_eq!(removal.process_fingerprint, "p1");
        assert!(removal.delete_branch);
        assert!(!removal.preserve_archive_restore);
        // A verified refusal is reviewed again before another removal.
        assert_eq!(
            cleanup.removed(&Removed::Refused {
                reply: json!({"status": "kept"}),
                detail: "Programs not checked.".into()
            }),
            Step::Idle
        );
        cleanup.review_remaining();
        let Step::Review {
            host, common_dir, ..
        } = cleanup.step()
        else {
            panic!()
        };
        cleanup.reviewed(&host, &common_dir, Ok(&review_of(unchecked("p2"))));
        let Step::Remove { removal, .. } = cleanup.remove(consents) else {
            panic!()
        };
        assert_eq!(
            removal.process_fingerprint, "p2",
            "the new review's fingerprint"
        );
    }
}
