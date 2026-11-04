use super::*;
use crate::swarm::{edit::Intent, sync::Due};
use serde_json::json;

fn snapshot(digest: &str, swarm: &str) -> Value {
    json!({"schema":1,"node_id":"local-node","swarm_id":swarm,"digest":digest,"initialized":true,
        "machines":[],"peers":{},"conflicts":[],"versions":{"name":[digest]},
        "organization":{"version":2,"projects":[{"id":"ungrouped","name":"Inbox","sessions":[],"folders":[]}]}})
}
fn observation(digest: &str, swarm: &str) -> Observation {
    Observation::from_reply(snapshot(digest, swarm)).unwrap()
}
fn ready() -> Controller {
    let mut controller = Controller::default();
    assert!(controller.observe(observation("old", "first"), 0));
    controller
}
fn rename() -> Intent {
    Intent::Rename {
        project: "ungrouped".into(),
        name: "Renamed".into(),
    }
}

#[test]
fn reservation_excludes_edits_due_and_second_reservation_without_changing_the_snapshot() {
    let mut controller = ready();
    let ticket = controller.begin_external(controller.revision()).unwrap();
    assert_eq!(ticket.snapshot()["digest"], "old");
    assert!(!controller.ready());
    assert!(controller.busy());
    assert_eq!(controller.edit(&rename(), 1).unwrap_err(), EXTERNAL_BUSY);
    assert_eq!(controller.view().reason.as_deref(), Some(EXTERNAL_BUSY));
    assert!(controller.begin_external(controller.revision()).is_err());
    assert!(controller.next_due().is_none());
    assert!(matches!(controller.due(u64::MAX), Due::Nothing));
    assert!(controller.take_draft().is_none());
}

#[test]
fn pending_edits_apply_and_stale_revision_refuse_before_a_reservation() {
    let mut controller = ready();
    assert_eq!(controller.begin_external(0).unwrap_err(), EXTERNAL_CHANGED);
    let ticket = controller.edit(&rename(), 0).unwrap();
    assert!(controller.begin_external(controller.revision()).is_err());
    assert!(controller.pending());
    assert!(matches!(controller.due(120), Due::Apply(_)));
    assert!(controller.begin_external(controller.revision()).is_err());
    assert!(controller.busy());
    assert!(ticket > 0);
}

#[test]
fn tickets_are_bound_to_the_controller_and_are_one_use() {
    let mut first = ready();
    let mut second = ready();
    let a = first.begin_external(first.revision()).unwrap();
    let b = second.begin_external(second.revision()).unwrap();
    assert!(first
        .finish_external(&b, observation("new", "second"), 10)
        .is_err());
    assert!(first.busy());
    first
        .finish_external(&a, observation("new", "second"), 10)
        .unwrap();
    assert!(first.ready());
    assert!(!first.busy());
    assert_eq!(first.digest(), "new");
    assert!(first
        .finish_external(&a, observation("replay", "third"), 11)
        .is_err());
    assert_eq!(first.digest(), "new");
    assert!(second.busy());
}

#[test]
fn unknown_or_failed_call_and_lost_ticket_never_resume_the_controller() {
    let mut controller = ready();
    let ticket = controller.begin_external(controller.revision()).unwrap();
    controller.source_failed("Unconfirmed external result");
    assert!(!controller.observe(observation("topic", "first"), 3));
    controller.source_ended();
    drop(ticket);
    assert!(controller.busy());
    assert!(!controller.ready());
    assert!(controller.next_due().is_none());
    assert!(matches!(controller.due(u64::MAX), Due::Nothing));
    assert_eq!(controller.digest(), "old");
}

#[test]
fn invalid_completion_keeps_the_fence_for_a_fresh_snapshot() {
    let mut controller = ready();
    let ticket = controller.begin_external(controller.revision()).unwrap();
    let mut wrong = snapshot("new", "second");
    wrong["node_id"] = json!("different-node");
    assert!(controller
        .finish_external(&ticket, Observation::from_reply(wrong).unwrap(), 2)
        .is_err());
    let mut uninitialized = snapshot("new", "second");
    uninitialized["initialized"] = json!(false);
    assert!(controller
        .finish_external(&ticket, Observation::from_reply(uninitialized).unwrap(), 2)
        .is_err());
    assert!(controller.busy());
    controller
        .finish_external(&ticket, observation("new", "second"), 3)
        .unwrap();
    assert!(controller.ready());
}

#[test]
fn held_pre_result_observation_is_discarded_and_late_old_heads_are_guarded() {
    let mut controller = ready();
    let ticket = controller.begin_external(controller.revision()).unwrap();
    assert!(!controller.observe(observation("stale", "first"), 1));
    controller
        .finish_external(&ticket, observation("new", "second"), 2)
        .unwrap();
    assert_eq!(controller.digest(), "new");
    assert!(!controller.observe(observation("old", "first"), 3));
    assert_eq!(controller.digest(), "new");
    assert!(controller.observe(observation("new", "second"), 4));
    assert!(controller.next_due().is_none());
}

#[test]
fn catalog_identity_is_guarded_even_with_empty_versions_and_past_the_timeout() {
    let mut controller = ready();
    let ticket = controller.begin_external(controller.revision()).unwrap();
    let mut result = observation("new", "second");
    result.versions = json!({});
    controller.finish_external(&ticket, result, 100).unwrap();
    assert!(!controller.observe(observation("old", "first"), 101));
    assert!(matches!(controller.due(100 + GUARD_MS), Due::Nothing));
    assert!(!controller.observe(observation("old", "first"), 200 + GUARD_MS));
    assert_eq!(
        controller.digest(),
        "new",
        "time does not qualify an old catalog"
    );
    assert!(controller.observe(observation("current", "second"), 201 + GUARD_MS));
    assert_eq!(controller.digest(), "current");
    assert!(controller.ready());
}
