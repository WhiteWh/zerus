//! Task #16 of team c536f0e5 (P2.5): the `team.views` rows and their frames.
use super::*;
use std::cell::RefCell;
use std::time::Duration;

/// A read that answers each operation with its input, and records it.
fn echo(log: &RefCell<Vec<(String, Value)>>) -> impl Fn(&str, &Value) -> anyhow::Result<Value> + '_ {
    move |op, input| {
        log.borrow_mut().push((op.to_owned(), input.clone()));
        Ok(json!({"op": op, "input": input}))
    }
}

#[test]
fn the_rows_are_the_desktops_reads_with_its_inputs() {
    let log = RefCell::new(Vec::new());
    let rows = rows(None, echo(&log));
    assert_eq!(rows.keys().collect::<Vec<_>>(), ["list", "waiting"], "no team: the list and Waiting");
    assert_eq!(rows["list"], json!({"op":"list","input":{"include_cancelled":true,"limit":200}}));
    assert_eq!(rows["waiting"], json!({"op":"waiting","input":{"limit":100}}));
    let log = RefCell::new(Vec::new());
    let rows = super::rows(Some("t1"), echo(&log));
    assert_eq!(rows.keys().collect::<Vec<_>>(), ["feed:t1", "inbox:t1", "list", "status:t1", "waiting"]);
    // The inputs `zerus_client_core::teams::TeamRead::payload` sends.
    assert_eq!(rows["status:t1"]["input"], json!({"team":"t1","include_cancelled":true}));
    assert_eq!(rows["inbox:t1"]["input"], json!({"team":"t1"}));
    assert_eq!((rows["feed:t1"]["op"].as_str(), &rows["feed:t1"]["input"]), (Some("events"), &json!({"team":"t1","limit":50})));
    assert_eq!(super::rows(Some(""), echo(&RefCell::new(Vec::new()))).len(), 2, "an empty team names none");
}

#[test]
fn a_failed_or_too_large_view_is_an_error_row_read_with_its_command() {
    let rows = rows(Some("t1"), |op, _| match op {
        "status" => Ok(json!({"text": "x".repeat(ROW_BYTES + 1)})),
        "inbox" => anyhow::bail!("Unknown team"),
        "events" => panic!("boom"),
        _ => Ok(json!({"items": []})),
    });
    assert_eq!(rows["status:t1"]["error"]["code"], "too_large");
    assert!(rows["status:t1"]["error"]["bytes"].as_u64().unwrap() > ROW_BYTES as u64);
    assert_eq!(rows["inbox:t1"]["error"], json!({"code":"failed","message":"Unknown team"}));
    assert_eq!(rows["feed:t1"]["error"]["code"], "failed");
    assert_eq!(rows["list"], json!({"items": []}), "the other views still go out");
}

fn finish(views: &mut TeamViews, job: Job, team: Option<String>, status: &str) {
    let rows = super::rows(team.as_deref(), |op, input| Ok(json!({"op": op, "team": input["team"], "status": status})));
    assert_eq!(views.done(Done { token: job.token, value: Some(Ok(rows)) }), None);
}

fn keys(rows: &[(String, Value)]) -> Vec<&str> {
    rows.iter().map(|(key, _)| key.as_str()).collect()
}

#[test]
fn a_subscription_gets_a_snapshot_then_only_the_changed_views() {
    let start = Instant::now();
    let mut views = TeamViews::new();
    assert!(views.pulse(start, |_| 0).is_none(), "nothing before a subscription");
    let (job, team) = views.subscribe(Some("t1".into()), Vec::new(), 0, start);
    assert_eq!(team.as_deref(), Some("t1"));
    finish(&mut views, job, team, "running");
    let Some(Plan::Snapshot { gap: false, pages }) = views.plan(1 << 20) else { panic!("a snapshot first") };
    assert_eq!(keys(&pages[0]), ["feed:t1", "inbox:t1", "list", "status:t1", "waiting"]);
    views.sent();
    assert!(views.plan(1 << 20).is_none(), "the client is current");
    // The store changed: the read runs a second after the last one at the
    // earliest, so each key changes at most once a second.
    assert!(views.pulse(start + Duration::from_millis(500), |_| 1).is_none());
    let (job, team) = views.pulse(start + Duration::from_secs(1), |_| 1).expect("the store changed");
    finish(&mut views, job, team, "running");
    assert!(views.plan(1 << 20).is_none(), "the same views send nothing");
    let (job, team) = views.pulse(start + Duration::from_secs(2), |_| 2).expect("changed again");
    finish(&mut views, job, team, "paused");
    let Some(Plan::Delta { upsert, remove }) = views.plan(1 << 20) else { panic!("a delta") };
    assert_eq!(keys(&upsert), ["feed:t1", "inbox:t1", "list", "status:t1", "waiting"]);
    assert!(remove.is_empty());
    views.sent();
    // Another team: every row again, the first team's gone.
    let (job, team) = views.subscribe(Some("t2".into()), Vec::new(), 2, start + Duration::from_secs(3));
    finish(&mut views, job, team, "paused");
    let Some(Plan::Snapshot { pages, .. }) = views.plan(1 << 20) else { panic!("a snapshot") };
    assert_eq!(keys(&pages[0]), ["feed:t2", "inbox:t2", "list", "status:t2", "waiting"]);
    views.sent();
    // No team: the list and Waiting alone.
    let (job, team) = views.subscribe(None, Vec::new(), 2, start + Duration::from_secs(4));
    finish(&mut views, job, team, "paused");
    let Some(Plan::Snapshot { pages, .. }) = views.plan(1 << 20) else { panic!("a snapshot") };
    assert_eq!(keys(&pages[0]), ["list", "waiting"]);
    assert!(views.unsubscribe());
    assert!(views.plan(1 << 20).is_none());
}

#[test]
fn only_a_changed_view_is_sent_and_a_missed_delta_is_a_gap() {
    let start = Instant::now();
    let mut views = TeamViews::new();
    let (job, _) = views.subscribe(Some("t1".into()), Vec::new(), 0, start);
    let row = |inbox: u64| {
        let mut rows = BTreeMap::new();
        rows.insert("list".to_owned(), json!({"items": []}));
        rows.insert("inbox:t1".to_owned(), json!({"items": inbox}));
        rows
    };
    views.done(Done { token: job.token, value: Some(Ok(row(1))) });
    views.sent();
    let (job, _) = views.pulse(start + team::SAFETY, |_| 0).unwrap();
    views.done(Done { token: job.token, value: Some(Ok(row(2))) });
    let Some(Plan::Delta { upsert, .. }) = views.plan(1 << 20) else { panic!("a delta") };
    assert_eq!(keys(&upsert), ["inbox:t1"], "the list did not change");
    views.missed();
    assert!(matches!(views.plan(1 << 20), Some(Plan::Snapshot { gap: true, .. })));
    // A failed read: reported, and the next good read is a gap and a snapshot.
    views.sent();
    let (job, _) = views.pulse(start + team::SAFETY * 2, |_| 0).unwrap();
    assert_eq!(views.done(Done { token: job.token, value: Some(Err("database is locked".into())) }).as_deref(), Some("database is locked"));
}
