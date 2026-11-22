use super::*;
use zerus_team_plan::{
    catalog::{Stage, StageKind},
    model::Acceptance,
};

fn land_pipeline(allowed: bool, accept: Acceptance, kinds: &[StageKind]) -> (Harness, u32) {
    let mut h = Harness::empty();
    let mut op = create();
    let Operation::Create { bundle, .. } = &mut op else {
        unreachable!()
    };
    bundle.template.settings.land_allowed = allowed;
    bundle.template.settings.integration = Integration::None;
    let pipeline = bundle
        .pipelines
        .iter_mut()
        .find(|pipeline| pipeline.id == bundle.template.default_pipeline)
        .unwrap();
    pipeline.accept = accept;
    pipeline.stages = kinds
        .iter()
        .map(|&kind| Stage {
            kind,
            role: "reviewer".into(),
        })
        .collect();
    h.run(&person(), op).unwrap();
    h.bind("lead");
    h.bind("m1");
    h.bind("m3");
    let parent = h.add(&person(), 1);
    h.submit(parent, "m1");
    h.report(parent, "m1");
    (h, parent)
}

fn skips(h: &Harness, parent: u32) -> Vec<store::JournalEntry> {
    h.store
        .journal(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .filter(|entry| {
            entry.text.contains("Land stages skipped")
                && entry.text.contains(&format!("task #{parent} "))
                && entry.text.contains("landing is disabled in Team settings")
        })
        .collect()
}

fn check_drawer_fixture(
    name: &str,
    kind: zerus_client_core::upstream_teams::drawers::DrawerKind,
    item: &impl serde::Serialize,
) {
    use zerus_client_core::{
        actions::Exit,
        upstream_teams::drawers::{self, DrawerRead},
    };
    let page = drawers::decode(
        &DrawerRead {
            team: TEAM.into(),
            kind,
            after: None,
            recorded_timings: false,
            timing_seqs: None,
            timing_next: None,
        },
        Exit::Code {
            code: 0,
            stdout: serde_json::to_vec(&serde_json::json!({
                "ok":true, "actor":{"kind":"person"}, "team":TEAM, "rows":[item], "next":null
            }))
            .unwrap(),
            stderr: String::new(),
        },
    )
    .unwrap();
    let row = serde_json::to_value(&page.rows[0]).unwrap();
    let fixtures: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/client/core/src/upstream_teams/fixtures/landing-drawers.json"
    )))
    .unwrap();
    assert_eq!(row, fixtures[name]);
}

#[test]
fn landing_disabled_skips_consecutive_and_trailing_land_without_renumbering() {
    use StageKind::{Land, Review, Test};
    let (mut h, parent) = land_pipeline(
        false,
        Acceptance::Pipeline,
        &[Land, Land, Review, Land, Test, Land],
    );
    let first = stages(&h, parent);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].stage_index, 2, "Land must never be materialized");
    assert!(first[0].title.starts_with("Review: "));
    assert_eq!(skips(&h, parent)[0].kind, "record_report");

    let reviewer = bound(&h, "m3");
    let review = stage_attempt(&mut h, parent, "m3");
    let req = request(
        h.store.team(TEAM).unwrap().unwrap().revision,
        verdict(review, 1, true),
    );
    let receipt = apply(&mut h.store, &reviewer, &req, 1_234).unwrap();
    assert_eq!(
        apply(&mut h.store, &reviewer, &req, 1_234).unwrap(),
        receipt
    );
    h.observe(review, TaskStage::Done);

    let second = stages(&h, parent);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].stage_index, 4, "the Test keeps its catalog index");
    assert!(second[0].title.starts_with("Test: "));
    let test = stage_attempt(&mut h, parent, "m3");
    h.run(&reviewer, verdict(test, 1, true)).unwrap();
    h.observe(test, TaskStage::Done);
    assert_eq!(h.node(parent).decided_by, Some(Decider::Pipeline));
    assert!(stages(&h, parent).is_empty());
    let audit = skips(&h, parent);
    use zerus_client_core::upstream_teams::drawers::DrawerKind::Feed;
    check_drawer_fixture("report", Feed, &audit[0]);
    check_drawer_fixture("verdict", Feed, &audit[1]);
    assert_eq!(
        audit
            .iter()
            .map(|entry| entry.kind.as_str())
            .collect::<Vec<_>>(),
        ["record_report", "verdict", "verdict"]
    );
    assert!(h
        .store
        .stage_tasks(TEAM, 200, None)
        .unwrap()
        .rows
        .is_empty());
    let created: u32 = h
        .store
        .db
        .as_ref()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM stage_tasks WHERE team=?1 AND target=?2",
            rusqlite::params![TEAM, parent],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(created, 2);
}

#[test]
fn landing_disabled_keeps_normal_acceptance_without_a_synthetic_pass() {
    for acceptance in [Acceptance::Lead, Acceptance::Person, Acceptance::Pipeline] {
        let (mut h, parent) = land_pipeline(false, acceptance, &[StageKind::Land]);
        assert!(stages(&h, parent).is_empty());
        assert!(h
            .store
            .stage_tasks(TEAM, 200, None)
            .unwrap()
            .rows
            .is_empty());
        assert_eq!(h.node(parent).decided_by, None);
        assert_eq!(skips(&h, parent).len(), 1);
        if acceptance == Acceptance::Person {
            let cards = h.store.cards(TEAM, 200, None).unwrap().rows;
            assert_eq!(cards.len(), 1);
            assert_eq!(cards[0].node, Some(parent));
            assert_eq!(cards[0].kind, "pipeline_acceptance");
        } else {
            let inbox = h.store.inbox(TEAM, 200, None).unwrap().rows;
            assert_eq!(inbox.len(), 1);
            assert_eq!(inbox[0].node, parent);
            assert!(inbox[0].text.contains("landing is disabled"));
            if acceptance == Acceptance::Lead {
                use zerus_client_core::upstream_teams::drawers::DrawerKind::Inbox;
                check_drawer_fixture("lead", Inbox, &inbox[0]);
            }
            let lead = bound(&h, "lead");
            h.run(&lead, Operation::Accept { node: parent }).unwrap();
        }
    }
}

#[test]
fn turning_landing_off_keeps_the_handed_out_stage_and_skips_its_successor() {
    use StageKind::Land;
    let (mut h, parent) = land_pipeline(true, Acceptance::Pipeline, &[Land, Land]);
    assert!(stages(&h, parent)[0].title.starts_with("Land: "));
    let stage = stage_attempt(&mut h, parent, "m3");
    assert!(skips(&h, parent).is_empty());
    let previous = h.node(stage);
    let effects = h.store.effects(TEAM, 200, None).unwrap().rows;
    let tasks = h.store.stage_tasks(TEAM, 200, None).unwrap().rows;
    h.settings(|settings| settings.land_allowed = false);
    assert_eq!(h.node(stage), previous);
    assert_eq!(h.store.effects(TEAM, 200, None).unwrap().rows, effects);
    assert_eq!(h.store.stage_tasks(TEAM, 200, None).unwrap().rows, tasks);
    assert!(h.store.pending_cancels(TEAM, 200, 0).unwrap().is_empty());
    let reviewer = bound(&h, "m3");
    h.run(&reviewer, verdict(stage, 1, true)).unwrap();
    h.observe(stage, TaskStage::Done);
    assert_eq!(h.node(parent).decided_by, Some(Decider::Pipeline));
    assert!(stages(&h, parent).is_empty());
    assert_eq!(skips(&h, parent)[0].kind, "verdict");
}
