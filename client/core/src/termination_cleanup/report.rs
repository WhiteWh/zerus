//! Bounded structured archive-cleanup evidence. Display text never parses the
//! CLI's free-text reason, and an archive identity is not removal authority.
use crate::{creation::canonical_uuid, worktree_review, Target};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveIdentity {
    pub version: u32,
    pub archive_id: String,
    pub name: String,
    pub run_id: String,
    pub conversation_id: String,
    pub owner_id: String,
    pub launch_owned: bool,
}
impl ArchiveIdentity {
    /// A report can name Restore only for the exact source of this action.
    /// The actual restore still needs the existing fresh archive guards.
    pub fn matches(&self, target: &Target) -> bool {
        self.version == 1
            && canonical_uuid(&self.archive_id)
            && !self.name.is_empty()
            && !self.run_id.is_empty()
            && !self.conversation_id.is_empty()
            && self.name == target.name
            && self.run_id == target.run_id
            && self.conversation_id == target.conversation_id
            && [
                (&self.name, 512),
                (&self.run_id, 128),
                (&self.conversation_id, 256),
                (&self.owner_id, 1024),
            ]
            .into_iter()
            .all(|(s, limit)| s.len() <= limit && !s.chars().any(char::is_control))
    }
}

/// The approved counts-only sentence. Unknown or missing projections stay
/// generic; private paths and CLI reason strings never enter the notice.
pub fn retained_notice(row: Option<&Value>, launch_unowned: bool, unknown: bool) -> String {
    let mut reasons = Vec::new();
    if launch_unowned {
        reasons.push("not created by Zerus".into());
    }
    if let Some(row) = row.filter(|row| row.is_object()) {
        let count = |key: &str| row[key].as_array().map_or(0, Vec::len);
        let number = |key: &str| row[key].as_u64().unwrap_or(0);
        let plural = |n, singular: &str, plural: &str| {
            format!("{n} {}", if n == 1 { singular } else { plural })
        };
        if count("sessions") > 0 {
            reasons.push(plural(
                count("sessions") as u64,
                "other session",
                "other sessions",
            ));
        }
        let processes = number("process_count").max(count("processes") as u64);
        if processes > 0 {
            reasons.push(plural(processes, "running program", "running programs"));
        }
        if count("unreadable_processes") > 0
            || row["reasons"]
                .as_array()
                .is_some_and(|reasons| reasons.iter().any(|r| r["code"] == "processes_unavailable"))
        {
            reasons.push("programs that could not be checked".into());
        }
        for (key, one, many) in [
            ("changes", "changed file", "changed files"),
            ("untracked", "untracked file", "untracked files"),
        ] {
            if number(key) > 0 {
                reasons.push(plural(number(key), one, many));
            }
        }
        if worktree_review::local_data(row) {
            reasons.push("local files outside Git".into());
        }
        if unknown {
            reasons.push("an unknown result".into());
        }
    }
    if reasons.is_empty() {
        "Working folder kept for review.".into()
    } else {
        format!("Working folder kept for review: {}.", reasons.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn notice_contains_only_ordered_counts_and_never_private_reason_text() {
        let row = json!({"sessions":["private/session"],"processes":[{"path":"/private/program"},{"path":"/private/other"}],
            "unreadable_processes":[1],"changes":2,"untracked":1,"ignored_count":3,
            "reasons":[{"code":"processes_unavailable","message":"/private/secret"}]});
        assert_eq!(retained_notice(Some(&row),true,true), "Working folder kept for review: not created by Zerus, 1 other session, 2 running programs, programs that could not be checked, 2 changed files, 1 untracked file, local files outside Git, an unknown result.");
        assert_eq!(
            retained_notice(None, false, true),
            "Working folder kept for review."
        );
        assert_eq!(
            retained_notice(None, true, false),
            "Working folder kept for review: not created by Zerus."
        );
        assert_eq!(
            retained_notice(
                Some(&json!({"reasons":[{"code":"new_reason","message":"/private/path"}]})),
                false,
                false
            ),
            "Working folder kept for review."
        );
    }
    #[test]
    fn archive_evidence_requires_exact_known_identity_and_version() {
        let target = Target {
            computer: "example".into(),
            name: "claude/example/check".into(),
            run_id: "run-1".into(),
            conversation_id: "conversation-1".into(),
        };
        let identity = ArchiveIdentity {
            version: 1,
            archive_id: "11111111-1111-4111-8111-111111111111".into(),
            name: target.name.clone(),
            run_id: target.run_id.clone(),
            conversation_id: target.conversation_id.clone(),
            owner_id: "owner-1".into(),
            launch_owned: true,
        };
        assert!(identity.matches(&target));
        for changed in [
            ArchiveIdentity {
                version: 2,
                ..identity.clone()
            },
            ArchiveIdentity {
                archive_id: "invalid".into(),
                ..identity.clone()
            },
            ArchiveIdentity {
                run_id: "other".into(),
                ..identity.clone()
            },
            ArchiveIdentity {
                conversation_id: String::new(),
                ..identity.clone()
            },
        ] {
            assert!(!changed.matches(&target));
        }
    }

    #[test]
    fn an_observed_row_must_name_this_folder_and_fit_the_report_bound() {
        let mut report: crate::WorktreeCleanup = serde_json::from_value(json!({
            "status":"kept", "path":"/srv/example/work", "common_dir":"/srv/example/main/.git",
            "review":{"path":"/srv/example/work","kind":"linked","process_count":50,"processes":[{},{}]}
        })).unwrap();
        assert_eq!(
            retained_notice(report.review_row(), false, false),
            "Working folder kept for review: 50 running programs."
        );
        report.review.as_mut().unwrap()["path"] = json!("/srv/example/other");
        assert!(report.review_row().is_none());
        report.review.as_mut().unwrap()["path"] = json!(report.path);
        report.review.as_mut().unwrap()["extra"] = json!("x".repeat(60 * 1024));
        assert!(report.review_row().is_none());
    }
}
