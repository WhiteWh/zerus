//! Task #18 of team c536f0e5 (P2.6b): the lead's worker follows a member
//! computer's catalog stream and falls back to listing it.
use super::*;
use std::sync::atomic::AtomicUsize;
use zerus_watch_protocol::{CatalogItem, Delta, Frame, Gap, Hello, Limits, Removal, Snapshot};

fn row(name: &str, activity: &str, last_active: f64) -> Value {
    json!({"name":name,"run_id":format!("run-{name}"),"session_state":if activity == "idle" { "user" } else { "working" },
        "activity":activity,"state":"running","process_state":"running","last_active":last_active})
}

fn item(row: &Value) -> Value {
    json!({"key":zerus_watch_protocol::catalog_key(row).unwrap(),"row":row})
}

fn hello() -> Body {
    Body::Hello(Hello {
        machine_id: None,
        host: "msi-old".into(),
        hgs_version: "1.58.0".into(),
        stream: "s".into(),
        heartbeat_ms: 10_000,
        max_frame_bytes: 1 << 20,
        topics: vec![topic::CATALOG.into()],
        limits: Limits::default(),
    })
}

fn snapshot(items: Vec<Value>, page: u32, last: bool) -> Body {
    Body::Snapshot(Snapshot { topic: topic::CATALOG.into(), key: None, items: Some(items), value: None, page, last, cursor: 1 })
}

fn meta() -> Body {
    Body::Snapshot(Snapshot {
        topic: topic::CATALOG_META.into(),
        key: None,
        items: None,
        value: Some(json!({"host":"MSI-OLD","ok":true,"hgs_version":"1.58.0","projects":{}})),
        page: 0,
        last: true,
        cursor: 1,
    })
}

fn upsert(row: &Value) -> Body {
    let key = zerus_watch_protocol::catalog_key(row).unwrap();
    Body::Delta(Delta { topic: topic::CATALOG.into(), upsert: vec![CatalogItem { key, row: row.clone() }], remove: Vec::new(), cursor: 2 })
}

#[test]
fn a_followed_catalog_lists_the_peer_as_ls_would() {
    let at = Instant::now();
    let mut catalog = Catalog::default();
    assert!(!catalog.apply(hello(), at));
    assert!(!catalog.apply(meta(), at), "the listing's other fields change no session");
    assert!(!catalog.apply(snapshot(vec![item(&row("claude/x/dev", "busy", 10.0))], 0, false), at));
    assert!(!catalog.following(at), "a snapshot still arriving is not followed");
    assert!(catalog.apply(snapshot(vec![item(&row("claude/x/qa", "idle", 5.0))], 1, true), at), "now ready");
    assert!(catalog.following(at));
    let listing = catalog.listing("msi-old");
    assert_eq!(listing["host"], "MSI-OLD", "the host the peer names itself, as `ls` shows it");
    assert_eq!(listing["ok"], true);
    assert_eq!(listing["hgs_version"], "1.58.0");
    let names: Vec<&str> = listing["sessions"].as_array().unwrap().iter().map(|row| row["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["claude/x/dev", "claude/x/qa"]);
    // Silent past its heartbeats: no longer followed.
    assert!(!catalog.following(at + SILENT));
    // Without catalog.meta the stream's hello names the host.
    let mut bare = Catalog::default();
    bare.apply(hello(), at);
    bare.apply(snapshot(Vec::new(), 0, true), at);
    assert_eq!(bare.listing("alias")["host"], "msi-old");
}

#[test]
fn only_a_change_of_a_sessions_state_wakes_the_worker() {
    let at = Instant::now();
    let mut catalog = Catalog::default();
    let busy = row("claude/x/dev", "busy", 10.0);
    assert!(!catalog.apply(upsert(&busy), at), "a delta before the snapshot wakes nobody");
    catalog.apply(snapshot(vec![item(&busy)], 0, true), at);
    // A busy session's hook events move its times: the row follows, silently.
    assert!(!catalog.apply(upsert(&row("claude/x/dev", "busy", 11.0)), at));
    assert_eq!(catalog.listing("p")["sessions"][0]["last_active"], 11.0);
    // Its turn ends.
    assert!(catalog.apply(upsert(&row("claude/x/dev", "idle", 12.0)), at));
    // A new session, and one that goes away.
    assert!(catalog.apply(upsert(&row("claude/x/qa", "busy", 12.0)), at));
    let removal = Body::Delta(Delta {
        topic: topic::CATALOG.into(),
        upsert: Vec::new(),
        remove: vec![Removal { key: "run:run-claude/x/qa".into(), reason: Some("ended".into()) }],
        cursor: 3,
    });
    assert!(catalog.apply(removal, at));
    assert_eq!(catalog.listing("p")["sessions"].as_array().unwrap().len(), 1);
    // A gap: not followed until the next snapshot.
    assert!(catalog.apply(Body::Gap(Gap { topic: topic::CATALOG.into(), key: None, reason: "overflow".into() }), at));
    assert!(!catalog.following(at));
    assert!(!catalog.apply(snapshot(vec![item(&busy)], 0, false), at));
    assert!(catalog.apply(snapshot(Vec::new(), 1, true), at));
    assert!(catalog.following(at));
}

#[test]
fn a_peer_whose_stream_does_not_follow_it_is_listed_the_old_way() {
    let at = Instant::now();
    let followed = Arc::new(Mutex::new(Catalog::default()));
    followed.lock().unwrap().apply(snapshot(vec![item(&row("claude/x/dev", "idle", 1.0))], 0, true), at);
    let connecting = Arc::new(Mutex::new(Catalog::default()));
    let catalogs = BTreeMap::from([("a".to_owned(), followed), ("b".to_owned(), connecting)]);
    let peers = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
    let mut asked = Vec::new();
    let (listings, listed) = compose(&peers, &catalogs, at, |peer| {
        asked.push(peer.to_owned());
        json!({"host":peer,"ok":true,"sessions":[]})
    });
    assert_eq!(asked, ["b", "c"], "the followed peer is not listed");
    assert_eq!(listed, ["b", "c"]);
    assert_eq!(listings.len(), 3);
    assert_eq!(listings[0]["sessions"][0]["name"], "claude/x/dev");
    assert_eq!((listings[1]["host"].as_str(), listings[2]["host"].as_str()), (Some("b"), Some("c")));
    // A stream silent past its heartbeats is listed again.
    let (_, listed) = compose(&peers, &catalogs, at + SILENT, |peer| json!({"host":peer}));
    assert_eq!(listed, ["a", "b", "c"]);
}

/// The frames a fake `hgs @peer watch --json` prints: hello, a snapshot
/// with a busy session, catalog.meta, then that session's turn end.
fn frames(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("frames.ndjson");
    let bodies = [
        hello(),
        snapshot(vec![item(&row("claude/x/dev", "busy", 10.0))], 0, true),
        meta(),
        upsert(&row("claude/x/dev", "idle", 11.0)),
    ];
    let text: String = bodies
        .into_iter()
        .enumerate()
        .map(|(seq, body)| zerus_watch_protocol::encode(&Frame { v: 1, seq: seq as u64 + 1, body }).unwrap() + "\n")
        .collect();
    std::fs::write(&path, text).unwrap();
    path
}

/// Prints `path` and keeps stdout open for `linger` seconds.
fn fake_watch(path: &std::path::Path, linger: u32) -> std::io::Result<std::process::Child> {
    use std::process::{Command, Stdio};
    // cmd reads its command line as written: no argument quoting.
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("cmd");
        command.raw_arg(format!("/D /C type \"{}\" & ping -n {} 127.0.0.1 >nul", path.display(), linger + 1));
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new("sh");
        command.args(["-c", &format!("cat '{}'; sleep {linger}", path.display())]);
        command
    };
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_followed_stream_wakes_the_worker_at_a_turn_end() {
    let dir = tempfile::tempdir().unwrap();
    let path = frames(dir.path());
    let woken = Arc::new(AtomicUsize::new(0));
    let counter = woken.clone();
    let spawn: Spawn = Arc::new(move |_| fake_watch(&path, 5));
    let follower = Follower::start("msi-old".into(), spawn, Arc::new(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    // The snapshot made it ready, the turn end changed a session.
    wait_until("the turn end", || woken.load(Ordering::SeqCst) >= 2);
    let catalog = follower.catalog.lock().unwrap();
    assert!(catalog.following(Instant::now()));
    let listing = catalog.listing("msi-old");
    assert_eq!(listing["sessions"][0]["activity"], "idle");
    assert_eq!(listing["host"], "MSI-OLD");
}

#[test]
fn a_stream_that_ends_falls_back_to_the_listing_and_starts_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = frames(dir.path());
    let woken = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(AtomicUsize::new(0));
    let (counter, starts) = (woken.clone(), started.clone());
    // The first watch prints its frames and ends; later ones cannot start.
    let spawn: Spawn = Arc::new(move |_| {
        if starts.fetch_add(1, Ordering::SeqCst) == 0 {
            fake_watch(&path, 0)
        } else {
            Err(std::io::Error::other("the peer cannot be reached"))
        }
    });
    let follower = Follower::start("msi-old".into(), spawn, Arc::new(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    // Ready, the turn end, then the end of the stream.
    wait_until("the end of the stream", || woken.load(Ordering::SeqCst) >= 3);
    assert!(!follower.catalog.lock().unwrap().following(Instant::now()));
    let catalogs = BTreeMap::from([("msi-old".to_owned(), follower.catalog.clone())]);
    let (listings, listed) = compose(&["msi-old".to_owned()], &catalogs, Instant::now(), |peer| json!({"host":peer,"ok":true,"sessions":[],"listed":true}));
    assert_eq!(listed, ["msi-old"]);
    assert_eq!(listings[0]["listed"], true);
    // It tries again after the first delay.
    wait_until("a new start", || started.load(Ordering::SeqCst) >= 2);
}
