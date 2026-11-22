use super::ledger::Ledger;
use serde_json::json;

#[test]
fn actor_ledger_reopen_preserves_original_and_compares_versions() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = Ledger::open(temp.path()).unwrap();
    let mut row = ledger
        .reserve("team", "delivery:one", json!({"state":"intent"}))
        .unwrap();
    assert_eq!(row.version, 0);
    assert!(ledger
        .reserve("team", "delivery:one", json!({"state":"new"}))
        .is_err());
    let mut stale = row.clone();
    row.data = json!({"state":"outcome"});
    ledger.save(&mut row).unwrap();
    assert!(ledger.save(&mut stale).is_err());
    drop(ledger);
    let ledger = Ledger::open(temp.path()).unwrap();
    assert_eq!(ledger.get("team", "delivery:one").unwrap().unwrap(), row);
}

#[test]
fn actor_ledger_rejects_large_rows_and_unprivate_files() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = Ledger::open(temp.path()).unwrap();
    assert!(ledger
        .reserve("team", "huge", json!("x".repeat(16385)))
        .is_err());
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::PermissionsExt;
        drop(ledger);
        std::fs::set_permissions(
            temp.path().join("teams/actor-v1.sqlite3"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(Ledger::open(temp.path()).is_err());
    }
}

use super::ports::Result;

/// A command sentence with control characters, whitespace runs and more than
/// any cut: what a broken or hostile command could answer.
pub(super) fn hostile_sentence() -> String {
    format!(
        "{}{}",
        "busy\u{0007} \t\n\u{001B}\u{0000}\u{009B}".repeat(200),
        "x".repeat(600)
    )
}
/// The texts of the actor's retry cards, open or closed.
pub(super) fn blocked_cards(h: &Harness) -> Vec<String> {
    let cards = h.store.cards(TEAM, 200, None).unwrap().rows;
    cards
        .into_iter()
        .filter(|c| c.kind == "team_actor_blocked")
        .map(|c| c.text)
        .collect()
}
/// A command's sentence reached the card cleaned: no control character,
/// whitespace normalized.
pub(super) fn assert_clean(texts: &[String], case: &str) {
    assert!(!texts.is_empty(), "{case}: no card");
    for text in texts {
        assert!(!text.chars().any(char::is_control), "{case}: {text:?}");
        assert!(text.contains("busy busy busy"), "{case}: {text}");
    }
}
use crate::teams::{ops::tests::*, store::Store};
use zerus_team_plan::model::*;
mod cancel;
mod core;
#[cfg(unix)]
mod fresh_members;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod native;
mod publication;
#[test]
fn disabled_actor_does_not_create_any_store() {
    let temp = tempfile::tempdir().unwrap();
    assert!(super::Runtime::open(temp.path(), "machine").is_err());
    assert!(!temp.path().join("teams").exists());
}
#[test]
fn newer_actor_ledger_is_refused_without_relabeling_it() {
    let temp = tempfile::tempdir().unwrap();
    let _ = Ledger::open(temp.path()).unwrap();
    let db = rusqlite::Connection::open(temp.path().join("teams/actor-v1.sqlite3")).unwrap();
    db.pragma_update(None, "user_version", 2).unwrap();
    assert!(Ledger::open(temp.path()).is_err());
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        2
    );
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn actor_ledger_refuses_a_shared_or_linked_directory() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("teams");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Ledger::open(temp.path()).is_err());
    assert!(!dir.join("actor-v1.sqlite3").exists());
    std::fs::remove_dir(&dir).unwrap();
    let other = tempfile::tempdir().unwrap();
    symlink(other.path(), &dir).unwrap();
    assert!(Ledger::open(temp.path()).is_err());
    assert!(!other.path().join("actor-v1.sqlite3").exists());
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod adapters;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod commands;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod native_tasks;
mod tasks;
mod work;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod production;

#[test]
fn the_terminal_retry_is_a_single_bounded_row_even_when_the_actor_budget_is_full() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = Ledger::open(temp.path()).unwrap();
    let db = rusqlite::Connection::open(temp.path().join("teams/actor-v1.sqlite3")).unwrap();
    db.execute("UPDATE actor_budget SET rows=8192 WHERE id=1", [])
        .unwrap();
    let mut now = 100;
    for expected in [60_000, 120_000, 240_000, 480_000, 900_000, 900_000] {
        ledger.terminal_failed(now, &"界".repeat(2000)).unwrap();
        let row = ledger.terminal_retry().unwrap().unwrap();
        assert_eq!(row.next_ms - now, expected);
        assert!(row.reason.len() <= 2048);
        now = row.next_ms;
    }
    let count: u32 = db
        .query_row("SELECT count(*) FROM actor_terminal_retry", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(ledger.terminal_retry().unwrap().unwrap().failures, 6);
    ledger.terminal_succeeded().unwrap();
    assert!(ledger.terminal_retry().unwrap().is_none());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod native_cancels;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod native_cancel_tick;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod cancel_observation;

mod deferred;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod cancel_refusal;

mod terminal_pruning;

mod service_items;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) mod producers;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod cancel_recovery;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod handles;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod real_rows;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod removed_handout;
mod retry_cards;
mod project_limits;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod provider_stop;
mod unstaffed;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod retired_stage;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod reordered_children;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::desktop::teams) mod rows;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod subagent_rows;
// A member whose terminal holds input (zerus-63rru.79).
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod terminal_busy;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod cleanup_bound;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod handed;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod left_member;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod paused_started;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod uncertain_holders;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unreadable;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unreadable_handles;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unreadable_stop;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod cancel_progress;
mod closed_keys;
// WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
#[cfg(not(windows))]
mod seats102;

#[cfg(unix)]
mod legacy_cancel;
