//! Fleet rows as the product writes them (zerus-63rru.51): a row made by the
//! listing's own steps for an idle synthetic session, and the shared fixture
//! row, each stored as the catalog stores it and read by the actor's
//! availability and presence.
use serde_json::{json, Value};
use zerus_team_plan::model::{Member, MemberState, SessionKey};

const CHILD: &str = "desktop::teams::actor::tests::real_rows::";
const MACHINE: &str = "00000000-0000-4000-8000-0000000000aa";
const EPOCH: &str = "fleet-epoch";

/// Runs one test (its full path) alone in a child test process with a private state root,
/// so that the listing's own reads see nothing but this fixture's files.
pub(super) fn isolated(fixture: &str) -> String {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let config = root.path().join("config");
    for dir in [&state, &config] {
        crate::platform::private_dir(dir).unwrap();
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            fixture,
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("HGS_STATE_DIR", &state)
        .env("HGS_CONFIG_DIR", &config)
        .env("HOME", root.path())
        .env_remove("HGS_SESSION")
        .env_remove("HGS_SESSION_ID")
        .env_remove("HGS_RUN_ID")
        .env_remove("HGS_LAUNCH_ID")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success() && text.contains("1 passed"),
        "{text}"
    );
    text
}

pub(super) fn native(
    state: &std::path::Path,
    config: &std::path::Path,
) -> super::super::native::Native {
    super::super::native::Native {
        config: crate::config::Config {
            home: String::new(),
            dir: config.to_owned(),
            state: state.to_owned(),
            selves: vec![],
            peers: vec![],
            base_peers: vec![],
            peers_from_environment: false,
            machines: Default::default(),
            tab: false,
            tab_colors: String::new(),
        },
        machine: MACHINE.into(),
        epoch: EPOCH.into(),
        stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
}

/// Publish a producer's listing row through the catalog's normal watch
/// writer. Later updates keep the source and advance its frame sequence.
pub(super) fn store(state: &std::path::Path, key: &str, row: &Value) {
    use zerus_desktop_catalog as catalog;
    use zerus_watch_protocol as watch;
    let desktop = state.join("desktop");
    crate::platform::private_dir(&desktop).unwrap();
    let path = desktop.join("state.sqlite3");
    let new = !path.exists();
    if new {
        crate::sqlite_file::create(&path).unwrap();
    }
    let mut db = rusqlite::Connection::open(&path).unwrap();
    let source = catalog::Source {
        computer: String::new(),
        service_epoch: EPOCH.into(),
        incarnation: 1,
        generation: 1,
    };
    if new {
        catalog::migrate(&db).unwrap();
        catalog::begin_service(&mut db, EPOCH).unwrap();
        catalog::connect(
            &mut db,
            &source,
            &watch::Frame {
                v: watch::VERSION,
                seq: 1,
                body: watch::Body::Hello(watch::Hello {
                    machine_id: Some(MACHINE.into()),
                    host: "example-host".into(),
                    hgs_version: env!("CARGO_PKG_VERSION").into(),
                    stream: "fixture-stream".into(),
                    heartbeat_ms: watch::HEARTBEAT_MS,
                    max_frame_bytes: watch::MAX_FRAME_BYTES,
                    topics: vec![watch::topic::CATALOG.into()],
                    limits: watch::Limits::default(),
                }),
            },
        )
        .unwrap();
    }
    let previous: u64 = db
        .query_row(
            "SELECT seq FROM desktop_catalog_sources WHERE computer=''",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let item = watch::CatalogItem {
        key: key.into(),
        row: row.clone(),
    };
    let body = if new {
        watch::Body::Snapshot(watch::Snapshot {
            topic: watch::topic::CATALOG.into(),
            key: None,
            items: Some(vec![serde_json::to_value(item).unwrap()]),
            value: None,
            page: 0,
            last: true,
            cursor: 1,
        })
    } else {
        watch::Body::Delta(watch::Delta {
            topic: watch::topic::CATALOG.into(),
            upsert: vec![item],
            remove: vec![],
            cursor: previous as i64,
        })
    };
    assert_eq!(
        catalog::apply(
            &mut db,
            &source,
            &watch::Frame {
                v: watch::VERSION,
                seq: previous + 1,
                body
            },
        )
        .unwrap(),
        catalog::Applied::Visible,
    );
}

fn member(run: &str) -> Member {
    Member {
        launch: None,
        id: "m1".into(),
        role: "developer".into(),
        agent: "claude".into(),
        account: String::new(),
        state: MemberState::Active,
        session: Some(SessionKey {
            machine: MACHINE.into(),
            session_id: format!("run_id:{run}"),
        }),
        worktree: String::new(),
        ownership_nonce: String::new(),
    }
}

/// Each condition the actor's availability reads, by its own name.
fn conditions(row: &Value) -> Value {
    json!({
        "view.status": zerus_client_core::view::row(row).status,
        "state": row.get("state"),
        "activity": row.get("activity"),
        "phase": row.get("phase"),
        "process_work.count": row.get("process_work").map(|w| &w["count"]),
        "subagents_active": row.get("subagents_active"),
        "subagent_active_count": row.get("subagent_active_count"),
        "optional_questions.count": row.get("optional_questions").map(|q| &q["count"]),
        "runtime_state": row.get("runtime_state"),
        "process_state": row.get("process_state"),
    })
}

/// One row through availability and presence: what each answers.
fn read(
    state: &std::path::Path,
    config: &std::path::Path,
    key: &str,
    run: &str,
    row: &Value,
) -> String {
    store(state, key, row);
    let native = native(state, config);
    let member = member(run);
    let mut team = crate::teams::ops::tests::fixture().team;
    team.home = MACHINE.into();
    let free = super::super::availability::free(&native.config, EPOCH, &team.id, &member, false);
    let presence = super::super::auxiliary::presence(&native, &team, &member).map(|p| {
        json!({"live":p.live,"working":p.working,"background":p.background,"pending":p.pending,"idle":p.idle()})
    });
    format!(
        "free={free:?} presence={presence:?} conditions={}",
        conditions(row)
    )
}

fn idle_claude() -> Value {
    json!({"version":1,"run_identity_version":1,"name":"claude/example/member","agent":"claude",
        "run_id":"run-real","conversation_id":"conversation-real","session_id":"00000000-0000-4000-8000-000000000051",
        "launch_id":"","cwd":"/","launch_dir":"/","created":1793000000,
        "activity":"idle","phase":"idle","conversation_state":"active","last_event_at":1793000001.0})
}

#[test]
#[ignore = "run by its isolated wrapper"]
fn producer_row_child() {
    let state = std::path::PathBuf::from(std::env::var("HGS_STATE_DIR").unwrap());
    let config = std::path::PathBuf::from(std::env::var("HGS_CONFIG_DIR").unwrap());
    let row = crate::state::listing_fixture::listed_live_row(idle_claude()).unwrap();
    println!("REAL-ROWS producer row: {row}");
    println!(
        "REAL-ROWS producer: {}",
        read(&state, &config, "run:run-real", "run-real", &row)
    );
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../../client/core/tests/fixtures/action-rows.json"
    ))
    .unwrap();
    let shared = &fixture["rows"][0];
    let row = &shared["hgs"];
    let run = row["run_id"].as_str().unwrap_or("run-1");
    println!(
        "REAL-ROWS shared fixture ({}, {}): {}",
        shared["case"],
        shared["key"],
        read(&state, &config, shared["key"].as_str().unwrap(), run, row)
    );
}

#[test]
fn a_real_idle_row_is_read_by_availability_and_presence() {
    let text = isolated(&format!("{CHILD}producer_row_child"));
    let lines: Vec<&str> = text
        .lines()
        .filter_map(|l| l.find("REAL-ROWS").map(|at| &l[at..]))
        .collect();
    for line in &lines {
        eprintln!("{line}");
    }
    let producer = lines
        .iter()
        .find(|l| l.starts_with("REAL-ROWS producer: "))
        .unwrap();
    assert!(
        producer.contains("free=Ok(true)"),
        "an idle real row is not free: {producer}"
    );
}

/// Production availability and delivery against producer-made fleet rows:
/// the child's reply is the fixture exit, so the held admission still runs.
pub(super) struct Production {
    pub(super) native: super::super::native::Native,
    pub(super) deliveries: Vec<String>,
}
impl super::super::ports::Ports for Production {
    fn changes(
        &mut self,
        _: &str,
        _: &str,
        _: u64,
    ) -> super::super::ports::Result<zerus_replica::Changes> {
        Ok(zerus_replica::Changes {
            generation: "journal".into(),
            offset: 0,
            documents: Some(Default::default()),
        })
    }
    fn observe(
        &mut self,
        _: &mut crate::teams::store::Store,
        _: &zerus_team_plan::model::TaskRef,
    ) -> super::super::ports::Result<()> {
        Ok(())
    }
    fn free(
        &mut self,
        t: &zerus_team_plan::model::Team,
        m: &Member,
    ) -> super::super::ports::Result<bool> {
        super::super::ports::Ports::free(&mut self.native, t, m)
    }
    fn availability(
        &mut self,
        t: &zerus_team_plan::model::Team,
        m: &Member,
    ) -> super::super::ports::Result<super::super::ports::Availability> {
        super::super::ports::Ports::availability(&mut self.native, t, m)
    }
    fn held_availability(
        &mut self,
        store: &mut crate::teams::store::Store,
        team: &zerus_team_plan::model::Team,
        member: &Member,
        effect: &crate::teams::store::EffectIntent,
        confirmation: Option<&crate::teams::ops::ProviderConfirmation>,
        now: i64,
    ) -> super::super::ports::Result<(super::super::ports::Availability, Option<String>)> {
        super::super::ports::Ports::held_availability(
            &mut self.native,
            store,
            team,
            member,
            effect,
            confirmation,
            now,
        )
    }
    fn deliver(
        &mut self,
        t: &zerus_team_plan::model::Team,
        n: &zerus_team_plan::model::Node,
        m: &Member,
        id: &str,
    ) -> super::super::ports::DeliveryResult {
        crate::desktop::teams::child::FIXTURE_EXIT.with(|s| {
            *s.borrow_mut() = Some(
                json!({"schema":1,"id":id,"state":"submitted","error":""})
                    .to_string()
                    .into_bytes(),
            )
        });
        let r = super::super::ports::Ports::deliver(&mut self.native, t, n, m, id);
        crate::desktop::teams::child::FIXTURE_EXIT.with(|s| s.borrow_mut().take());
        self.deliveries.push(format!("{} -> {r:?}", m.id));
        r
    }
}

/// The member's committed fleet row, changed in place as a test needs.
fn set_row(native: &super::super::native::Native, path: &str, value: Value) {
    let db = rusqlite::Connection::open(native.config.state.join("desktop/state.sqlite3")).unwrap();
    let changed = db
        .execute(
            "UPDATE desktop_catalog_rows SET json=json_set(json,?1,json(?2))",
            rusqlite::params![path, value.to_string()],
        )
        .unwrap();
    assert!(changed > 0);
}

fn handed_out(change: Option<(&str, Value)>) -> (Vec<String>, Option<String>) {
    use crate::teams::ops::tests::*;
    let mut h = Harness::new();
    h.bind("m1");
    let node = h.add(&person(), 1);
    let machine = h.store.team(TEAM).unwrap().unwrap().home;
    let native = super::production::native(&h);
    if let Some((path, value)) = change {
        set_row(&native, path, value);
    }
    let mut a = super::super::Runtime::open(h.temp.path(), &machine).unwrap();
    let mut p = Production {
        native,
        deliveries: vec![],
    };
    a.tick(&mut h.store, &mut p, TEAM, 100, true).unwrap();
    let effect = h
        .store
        .effects(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .find(|e| e.node == node)
        .map(|e| e.state);
    (p.deliveries, effect)
}

#[test]
fn a_real_idle_member_is_free_and_handed_a_task() {
    let (deliveries, effect) = handed_out(None);
    assert_eq!(deliveries.len(), 1, "{deliveries:?}");
    assert!(deliveries[0].contains("Submitted"), "{deliveries:?}");
    assert_eq!(effect.as_deref(), Some("submitted"));
}

#[test]
fn a_real_row_with_an_active_child_or_an_unknown_count_is_not_free() {
    for (path, value) in [
        ("$.subagent_active_count", json!(1)),
        ("$.subagent_active_count", Value::Null),
        ("$.optional_questions", Value::Null),
        ("$.process_work.count", Value::Null),
        ("$.process_state", json!("exited")),
        ("$.runtime_state", json!("stopped")),
    ] {
        let (deliveries, effect) = handed_out(Some((path, value.clone())));
        assert!(deliveries.is_empty(), "{path} {value}: {deliveries:?}");
        assert_eq!(effect, None, "{path} {value}");
    }
}

/// One rule for every kind: process running, runtime live, and state absent
/// (a live terminal row) or running (a DeepSeek row).
#[test]
fn liveness_is_one_rule_for_every_agent_kind() {
    use super::super::fleet::Facts;
    use super::rows::{seed_row, KINDS};
    for kind in KINDS {
        assert!(Facts::of(&seed_row(kind, &[])).live, "{kind}");
        for (path, value) in [
            ("process_state", json!("exited")),
            ("process_state", json!("unknown")),
            ("process_state", Value::Null),
            ("runtime_state", json!("stopped")),
            ("runtime_state", Value::Null),
        ] {
            assert!(
                !Facts::of(&seed_row(kind, &[(path, value.clone())])).live,
                "{kind} {path} {value}"
            );
        }
    }
    for state in [
        json!("paused"),
        json!("stopped"),
        json!("archived"),
        Value::Null,
        json!(1),
    ] {
        assert!(
            !Facts::of(&seed_row("dsh", &[("state", state.clone())])).live,
            "{state}"
        );
    }
    // A row whose process keys are absent altogether is not live either.
    let mut row = seed_row("claude", &[]);
    row.as_object_mut().unwrap().remove("process_state");
    assert!(!Facts::of(&row).live);
}

/// Presence takes the same reader: the same liveness, and no presence while a
/// count it needs is unknown.
#[test]
fn presence_reads_the_same_facts_and_unknown_counts_give_none() {
    use super::super::fleet::Facts;
    use super::rows::seed_row;
    let idle = Facts::of(&seed_row("claude", &[])).seen().unwrap();
    assert!(idle.live && !idle.working && !idle.background && !idle.pending);
    let child = Facts::of(&seed_row("claude", &[("subagent_active_count", json!(2))]));
    assert!(child.seen().unwrap().working && !child.free());
    for (path, value) in [
        ("subagent_active_count", Value::Null),
        ("process_work.count", Value::Null),
        ("optional_questions", Value::Null),
    ] {
        assert_eq!(
            Facts::of(&seed_row("codex", &[(path, value.clone())])).seen(),
            None,
            "{path}"
        );
    }
    // The captured idle row has a complete job summary. Older/incomplete
    // summaries still cannot establish presence.
    let idle = Facts::of(&seed_row("dsh", &[])).seen().unwrap();
    assert!(idle.live && !idle.working && !idle.background && !idle.pending);
    assert_eq!(
        Facts::of(&seed_row("dsh", &[("process_work.count", Value::Null)])).seen(),
        None
    );
    let stopped = Facts::of(&seed_row("dsh", &[("state", json!("stopped"))]));
    assert!(!stopped.live);
    assert!(!stopped.seen().unwrap().live);
}
