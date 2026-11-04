//! The native caller's fixed source-family selection, using existing decoders.
//! Separate reader groups let it attach the original Qt and Settings rechecks;
//! a family name or digest never chooses a path, writer or permission.
use super::{sources, SourceResult, SourceStream};
use crate::settings::QtSnapshot;

pub struct RetainedQt {
    pub qt: Vec<SourceStream>,
    pub settings: Vec<SourceStream>,
}

/// One actual Qt snapshot and the current Next Settings bytes. An absent Qt
/// source must be reported by the native caller, never invented as empty Qt.
/// Every result still needs its original reader's recheck before publication.
/// Construction performs no store load/seed, recovery, policy or delivery call.
pub fn retained_qt(
    snapshot: &QtSnapshot,
    next_settings: &[u8],
    linux: bool,
) -> SourceResult<RetainedQt> {
    let mut qt = vec![
        sources::qt_pending_reads(snapshot)?,
        sources::qt_shared_read_migrated(snapshot)?,
        sources::qt_task_forms(snapshot)?,
        sources::qt_pending_launches(snapshot)?,
        sources::qt_executor_launches(snapshot)?,
        sources::qt_repository_clones(snapshot)?,
        sources::qt_worktree_snapshots(snapshot)?,
        sources::qt_organization_checkpoints(snapshot)?,
        sources::qt_attention_observed(snapshot)?,
        sources::qt_local_read_marks(snapshot)?,
    ];
    for key in [
        "workspace/projectFolderSplit",
        "tasks/inspectorSplit",
        "workspace/removeWorktreeAfterTermination",
    ] {
        qt.push(sources::qt_retained_view_preference(snapshot, key)?);
    }
    if linux {
        qt.push(sources::qt_linux_notifications(snapshot)?);
    }
    let mut settings = Vec::new();
    for [original, current] in [
        sources::approval_policies(snapshot, next_settings)?,
        sources::provider_operations(snapshot, next_settings)?,
        sources::recovery_policy_caches(snapshot, next_settings)?,
    ] {
        qt.push(original);
        settings.push(current);
    }
    Ok(RetainedQt { qt, settings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{migration::BundleItem, settings::qt::QtValue};
    use serde_json::json;
    use std::collections::BTreeSet;
    use zerus_desktop_migration::sha256;

    const SETTINGS: &[u8] = br#"{"version":1,"values":{}}"#;

    fn snapshot() -> QtSnapshot {
        QtSnapshot {
            values: Default::default(),
            source: "qt-ini",
            sha256: sha256(b"[workspace]\n"),
            modified_at: None,
        }
    }

    #[test]
    fn fixed_families_keep_the_two_original_reader_groups_separate() {
        let snapshot = snapshot();
        let sources = retained_qt(&snapshot, SETTINGS, true).unwrap();
        assert_eq!(sources.qt.len(), 17);
        assert_eq!(sources.settings.len(), 3);
        let mut names = BTreeSet::new();
        for (group, expected) in [
            (sources.qt, snapshot.sha256),
            (sources.settings, sha256(SETTINGS)),
        ] {
            for source in group {
                assert_eq!(source.identity().sha256, expected);
                assert!(names.insert(source.identity().family.clone()));
            }
        }
        assert!(names.contains("qt-pending-reads"));
        assert!(names.contains("next-approval-policy"));
    }

    #[test]
    fn unsupported_current_policy_never_returns_a_partial_family_set() {
        assert!(retained_qt(&snapshot(), br#"{"version":2,"values":{}}"#, false).is_err());
    }

    #[test]
    fn a_bad_lazy_entry_prevents_a_complete_bundle_record() {
        let mut snapshot = snapshot();
        snapshot.values.insert(
            "attention/pendingReads".into(),
            QtValue::Text(json!({"not-an-exact-read-key":{}}).to_string()),
        );
        let families = retained_qt(&snapshot, SETTINGS, false).unwrap();
        let streams = families.qt.into_iter().chain(families.settings).collect();
        let mut bundle = super::super::Bundle::new(
            zerus_desktop_migration::SOURCE,
            false,
            "a".repeat(64),
            "11111111-1111-4111-8111-111111111111".into(),
            1,
            streams,
            64 << 20,
        )
        .unwrap();
        let mut refused = false;
        for item in &mut bundle {
            match item {
                Ok(BundleItem::Record(_)) => panic!("invalid source completed a manifest"),
                Ok(_) => (),
                Err(_) => refused = true,
            }
        }
        assert!(refused);
    }
}
