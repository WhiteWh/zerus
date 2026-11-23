//! A read-only check against the installed `hgs` (or `ZERUS_CLIENT_HGS`):
//! `cargo test -- --ignored live_smoke`. It follows this computer for 5 s,
//! subscribes one session's Activity and processes and prints counts only,
//! never names.
use std::collections::BTreeMap;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};
use zerus_client_core::{default_hgs, Change, Coalescer, ComputerId, Fleet, SessionRef, Timing};

#[test]
#[ignore = "runs the installed hgs watch --json for 5 s"]
fn live_smoke() {
    let (tx, rx) = channel();
    let fleet = Fleet::start_local(default_hgs(), tx, Timing::default());
    let local = ComputerId::local();
    let mut coalescer = Coalescer::new();
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    let mut statuses: BTreeMap<&str, usize> = BTreeMap::new();
    let (mut rows, mut followed, mut batches, mut largest) = (0, false, 0, 0);
    let mut next_frame = Instant::now() + Duration::from_millis(16);
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if let Ok(change) = rx.recv_timeout(Duration::from_millis(4)) {
            let kind = match &change {
                Change::Upsert { .. } => "upsert",
                Change::Remove { .. } => "remove",
                Change::Replace { rows: all, .. } => {
                    rows = all.len();
                    if !followed {
                        let session = all.iter().find_map(|(_, row)| {
                            row.run_id.as_ref().map(|run| SessionRef {
                                name: row.name.clone(),
                                run_id: Some(run.clone()),
                                conversation_id: row.conversation_id.clone(),
                            })
                        });
                        if let Some(session) = session {
                            let key = session.key();
                            let _ = fleet.follow_processes(&local, session.clone());
                            let _ = fleet.follow_organization(&local);
                            followed = fleet.follow(&local, &key, session, None, 100).is_ok();
                        }
                    }
                    "replace"
                }
                Change::Computer { status, .. } => {
                    *statuses.entry(status.name()).or_default() += 1;
                    "computer"
                }
                Change::Retry { .. } => "retry",
                Change::Activity { .. } => "activity",
                Change::Processes { .. } => "processes",
                Change::Tasks { .. } => "tasks",
                Change::Git { .. } => "git",
                Change::Machine { .. } => "machine",
                Change::Organization { .. } => "organization",
                Change::Teams { .. } => "teams",
                Change::TeamHeads { .. } => "team_heads",
                Change::TeamViews { .. } => "team_views",
            };
            *kinds.entry(kind).or_default() += 1;
            coalescer.push(change);
        }
        if Instant::now() >= next_frame {
            let batch = coalescer.take();
            if !batch.is_empty() {
                batches += 1;
                largest = largest.max(serde_json::to_vec(&batch).unwrap().len());
            }
            next_frame += Duration::from_millis(16);
        }
    }
    fleet.stop();
    let changes: usize = kinds.values().sum();
    println!("live_smoke: rows {rows}, changes {changes}, by kind {kinds:?}");
    println!("live_smoke: statuses {statuses:?}, followed one session: {followed}");
    println!("live_smoke: non-empty batches {batches}, largest batch {largest} bytes");
    assert!(
        statuses.contains_key("live"),
        "this computer's stream went live"
    );
    assert!(kinds.get("replace").is_some_and(|count| *count >= 1));
}
