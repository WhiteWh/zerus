//! Optional exact-CLI-body control; the author owns/captures private live state.
use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    time::Instant,
};

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires ZERUS_LISTING_CAPTURE from the genuine private live CLI control"]
async fn genuine_full_listing_survives_native_snapshot_encryption_and_phone_open() {
    use std::os::unix::fs::PermissionsExt;
    let path = std::path::PathBuf::from(
        std::env::var_os("ZERUS_LISTING_CAPTURE").expect("private CLI capture"),
    );
    let raw = std::fs::read(&path).unwrap();
    assert!(raw.len() <= crate::native::MAX_BYTES);
    let expected: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(expected["host"], "test");
    let count = expected["sessions"].as_array().unwrap().len();
    assert!(count >= 20);
    let expected_ids = |rows: &Value| -> BTreeMap<String, Value> {
        rows.as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["name"].as_str().unwrap().into(),
                    json!([row["session_id"], row["run_id"], row["conversation_id"]]),
                )
            })
            .collect()
    };
    let ids = expected_ids(&expected["sessions"]);
    assert_eq!(ids.len(), count);

    // The real production native pipe/JSON cap, without a live gateway/state.
    let mut f = Fixture::new();
    let source = f._temporary.path().join("listing.json");
    std::fs::write(&source, &raw).unwrap();
    let executable = f._temporary.path().join("native-fixture");
    std::fs::write(
        &executable,
        format!("#!/bin/sh\nexec cat '{}'\n", source.display()),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let native = crate::native::Process::new(executable.to_str().unwrap().into(), 5.0);
    let parsed = native
        .call(Call::new(["ls", "--json", "--local", "--no-archives"]))
        .await
        .unwrap()
        .json();
    assert_eq!(parsed, expected);
    *f.connector.native.listing.borrow_mut() = Some(parsed);
    *f.connector.native.observation.borrow_mut() = Some(Observation {
        machine: MACHINE.into(),
        change_after_listing: false,
        inventory_reads: 0,
    });
    // Exercise the resident's real cached-extra path with nonempty public
    // projects and an OpenCode catalog. No native service/account is started.
    let choices = json!([{"value":"fixture/local","label":"Fixture A","provider":"fixture"}]);
    let accounts = json!({"profiles":[{"id":"opencode-fixture","provider":"opencode",
        "label":"Fixture account","installed":false,"catalog_state":"ready",
        "model_options":choices,"model_options_truncated":false}]});
    assert_eq!(
        crate::accounts::launch_catalog(&accounts).unwrap()["accounts"][0]["model_options"],
        choices
    );
    {
        let mut state = f.connector.state.borrow_mut();
        state.project_cache = Some(
            crate::projects::normalize(
                &json!({"schema":1,"initialized":true,"node_id":MACHINE,
                    "swarm_id":"fixture-swarm","organization":{"projects":[{
                        "id":"fixture-project","name":"Fixture project","folders":[],"sessions":[]}]}}),
                &expected,
                &f.connector.settings.binding,
            )
            .unwrap(),
        );
        state.project_next_poll = f64::MAX;
        state
            .accounts
            .apply_catalog(crate::accounts::catalog(&accounts), crate::now());
        state.accounts.next_catalog = f64::MAX;
    }
    let receiver = Receiver::new();
    let profile = receiver.bind(&mut f);
    let engine = f.engine();
    let snapshot = engine.observe_local(MACHINE).await.unwrap();
    assert_eq!(expected_ids(&snapshot["sessions"]), ids);
    assert!(snapshot["mobile_projects"].is_object());
    assert!(snapshot["mobile_accounts"].is_object());
    assert_eq!(
        snapshot["mobile_projects"]["projects"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        snapshot["mobile_accounts"]["accounts"][0]["model_options"],
        choices
    );
    // Same receiver budget used for a forwarded peer snapshot, with extras.
    let mobile_bytes = encoded(&snapshot).unwrap().len();
    assert!(mobile_bytes <= crate::fleet::MAX_PEER_SNAPSHOT);
    engine
        .snapshot(
            &profile,
            MACHINE,
            &profile.identity.device.device_id,
            "Private computer",
            snapshot,
            Value::Null,
        )
        .await
        .unwrap();
    let requests = receiver.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].0.starts_with("POST /v2/node/snapshot "));
    assert!(requests[1].0.starts_with("POST /v2/node/signals "));
    let envelope: EncryptedEnvelope =
        serde_json::from_value(requests[0].1["envelope"].clone()).unwrap();
    let plain = crypto::open(
        &f.phone,
        &profile.identity.device,
        &envelope.header,
        &envelope,
    )
    .unwrap();
    assert!(plain.len() <= 1024 * 1024);
    let decoded: Value = serde_json::from_slice(&plain).unwrap();
    assert_eq!(decoded["machine_id"], MACHINE);
    assert_eq!(expected_ids(&decoded["snapshot"]["sessions"]), ids);
    assert!(decoded["snapshot"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["relay_scope_id"].is_string()));
    println!("{{\"sessions\":{count},\"cli_bytes\":{},\"mobile_snapshot_bytes\":{mobile_bytes},\"encrypted_plaintext_bytes\":{},\"posted_ciphertext_bytes\":{}}}",raw.len(),plain.len(),encoded(&requests[0].1).unwrap().len());

    // Same canonical body limit, no truncate-to-success or enlarged transport.
    let mut oversized = decoded["snapshot"].clone();
    oversized["fixture_padding"] = json!("x".repeat(1024 * 1024));
    assert!(!receiver.task.as_ref().unwrap().is_finished());
    assert!(matches!(
        engine
            .snapshot(
                &profile,
                MACHINE,
                &profile.identity.device.device_id,
                "Private computer",
                oversized,
                Value::Null
            )
            .await,
        Err(Failure::Retry)
    ));
    assert_eq!(
        receiver.requests().len(),
        2,
        "oversize reached the live receiver"
    );
    receiver.finish().await;
}

/// Remains available through the negative; a cap mutant cannot hide behind a
/// receiver that already returned after the positive request.
struct Receiver {
    url: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    rows: std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Receiver {
    fn new() -> Self {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let rows = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::clone(&stop);
        let captured = Arc::clone(&rows);
        let task = tokio::task::spawn_blocking(move || {
            let until = Instant::now() + Duration::from_secs(15);
            while !stopped.load(Ordering::SeqCst) && Instant::now() < until {
                let (mut stream, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(e) => panic!("private receiver failed: {e}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let start = loop {
                    let mut block = [0; 4096];
                    let n = stream.read(&mut block).unwrap();
                    assert!(n > 0 && bytes.len() + n < MAX_SMALL);
                    bytes.extend_from_slice(&block[..n]);
                    if let Some(i) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let header = String::from_utf8(bytes[..start].to_vec()).unwrap();
                let length = header
                    .lines()
                    .find_map(|line| {
                        let (n, v) = line.split_once(':')?;
                        n.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while bytes.len() < start + length {
                    let mut block = [0; 4096];
                    let n = stream.read(&mut block).unwrap();
                    assert!(n > 0 && bytes.len() + n < MAX_SMALL);
                    bytes.extend_from_slice(&block[..n]);
                }
                let row = (
                    header.lines().next().unwrap().to_owned(),
                    serde_json::from_slice(&bytes[start..start + length]).unwrap(),
                );
                captured.lock().unwrap().push(row);
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    )
                    .unwrap();
            }
        });
        Self {
            url,
            stop,
            rows,
            task: Some(task),
        }
    }
    fn bind(&self, f: &mut Fixture) -> Profile {
        *f.connector.native.observation.borrow_mut() = Some(Observation {
            machine: MACHINE.into(),
            change_after_listing: false,
            inventory_reads: 0,
        });
        let mut profile = workspace::load_active(&f.private_dir).unwrap();
        profile.server_url = self.url.clone();
        let store = Store::lock(&f.private_dir, false).unwrap();
        let old = store.read("active.json").unwrap().unwrap();
        store
            .replace(
                "active.json",
                &Store::revision(Some(&old)),
                &serde_json::to_vec(&profile).unwrap(),
            )
            .unwrap();
        drop(store);
        f.relay = workspace::relay(&profile, true).unwrap();
        profile
    }
    fn requests(&self) -> Vec<(String, Value)> {
        self.rows.lock().unwrap().clone()
    }
    async fn finish(mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.task.take().unwrap().await.unwrap();
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tokio::test]
async fn exact_plaintext_ceiling_accepts_one_mib_and_refuses_next_byte_before_live_receiver() {
    let mut f = Fixture::new();
    let receiver = Receiver::new();
    let profile = receiver.bind(&mut f);
    let engine = f.engine();
    let base = json!({"host":"test","sessions":[],"fixture_padding":""});
    engine
        .snapshot(
            &profile,
            MACHINE,
            &profile.identity.device.device_id,
            "Private computer",
            base.clone(),
            Value::Null,
        )
        .await
        .unwrap();
    let open = |body: &Value| {
        let envelope: EncryptedEnvelope = serde_json::from_value(body["envelope"].clone()).unwrap();
        crypto::open(
            &f.phone,
            &profile.identity.device,
            &envelope.header,
            &envelope,
        )
        .unwrap()
    };
    let baseline = receiver.requests();
    assert_eq!(baseline.len(), 2);
    let baseline_bytes = open(&baseline[0].1).len();
    let mut exact = base;
    exact["fixture_padding"] = json!("x".repeat(1024 * 1024 - baseline_bytes));
    engine
        .snapshot(
            &profile,
            MACHINE,
            &profile.identity.device.device_id,
            "Private computer",
            exact.clone(),
            Value::Null,
        )
        .await
        .unwrap();
    let accepted = receiver.requests();
    assert_eq!(accepted.len(), 4);
    assert_eq!(open(&accepted[2].1).len(), 1024 * 1024);
    assert!(!receiver.task.as_ref().unwrap().is_finished());
    exact["fixture_padding"] = json!(format!("{}x", exact["fixture_padding"].as_str().unwrap()));
    assert!(matches!(
        engine
            .snapshot(
                &profile,
                MACHINE,
                &profile.identity.device.device_id,
                "Private computer",
                exact,
                Value::Null
            )
            .await,
        Err(Failure::Retry)
    ));
    assert_eq!(
        receiver.requests().len(),
        4,
        "oversized plaintext reached a healthy receiver"
    );
    assert!(!receiver.task.as_ref().unwrap().is_finished());
    receiver.finish().await;
}

#[tokio::test]
async fn optional_archive_preview_fits_the_packet_while_current_rows_stay_exact() {
    let mut f = Fixture::new();
    let receiver = Receiver::new();
    let profile = receiver.bind(&mut f);
    let engine = f.engine();
    let current = json!({"name":"codex/fixture/main","session_id":MACHINE,
        "run_id":REQUEST,"conversation_id":"fixture-conversation","state":"running",
        "activity":"working","phase":"working","reply_id":"original-reply",
        "unread_reply":false,"mobile_unread_reply":false,
        "allowed_actions":["inspect"],"pending_question_count":0,
        "subagents":[{"id":"child","state":"working","detail":"Original child detail"}]});
    let mut snapshot = json!({"host":"test","sessions":[current],"fixture_padding":"",
        "mobile_archive":{"schema":1,"available":true,"loaded":0,"total":80,"partial":true},
        "mobile_projects":{"available":true,"projects":[{"id":"fixture-project"}]},
        "mobile_accounts":{"available":true,"accounts":[{"id":"fixture-account"}]}});
    engine
        .snapshot(
            &profile,
            MACHINE,
            &profile.identity.device.device_id,
            "Private computer",
            snapshot.clone(),
            Value::Null,
        )
        .await
        .unwrap();
    let open = |body: &Value| {
        let envelope: EncryptedEnvelope = serde_json::from_value(body["envelope"].clone()).unwrap();
        let plain = crypto::open(
            &f.phone,
            &profile.identity.device,
            &envelope.header,
            &envelope,
        )
        .unwrap();
        assert!(plain.len() <= 1024 * 1024);
        (
            plain.len(),
            serde_json::from_slice::<Value>(&plain).unwrap(),
        )
    };
    let baseline = receiver.requests();
    assert_eq!(baseline.len(), 2);
    let (base_bytes, expected) = open(&baseline[0].1);
    snapshot["fixture_padding"] = json!("x".repeat(1024 * 1024 - base_bytes - 3500));
    let archived: Vec<Value> = (1..=3).map(|index| json!({
        "name":"codex/fixture/main", "state":"archived",
        "archive_id":format!("00000000-0000-4000-8000-{index:012}"),
        "run_id":format!("archived-run-{index}"),"conversation_id":format!("archived-conversation-{index}"),
        "last_message":"y".repeat(2000),"archived_at":100-index,
        "allowed_actions":["inspect","restore"]})).collect();
    snapshot["sessions"]
        .as_array_mut()
        .unwrap()
        .extend(archived.clone());
    snapshot["mobile_archive"]["loaded"] = json!(3);
    let before = snapshot.clone();
    let result = engine
        .snapshot(
            &profile,
            MACHINE,
            &profile.identity.device.device_id,
            "Private computer",
            snapshot,
            Value::Null,
        )
        .await;
    assert!(
        result.is_ok(),
        "optional preview blocked the entire current catalog: {result:?}"
    );
    let posted = receiver.requests();
    assert_eq!(posted.len(), 4);
    let (_, decoded) = open(&posted[2].1);
    let actual = &decoded["snapshot"];
    assert_eq!(actual["sessions"][0], expected["snapshot"]["sessions"][0]);
    assert_eq!(actual["sessions"].as_array().unwrap().len(), 2);
    assert_eq!(actual["sessions"][1], archived[0]);
    assert_eq!(
        actual["mobile_archive"],
        json!({"schema":1,"available":true,"loaded":1,"total":80,"partial":true})
    );
    for key in ["fixture_padding", "mobile_projects", "mobile_accounts"] {
        assert_eq!(actual[key], before[key]);
    }
    let mut mandatory_overflow = before;
    mandatory_overflow["fixture_padding"] = json!("z".repeat(1024 * 1024));
    assert!(matches!(
        engine
            .snapshot(
                &profile,
                MACHINE,
                &profile.identity.device.device_id,
                "Private computer",
                mandatory_overflow,
                Value::Null
            )
            .await,
        Err(Failure::Retry)
    ));
    assert_eq!(
        receiver.requests().len(),
        4,
        "mandatory overflow reached the healthy receiver"
    );
    assert!(!receiver.task.as_ref().unwrap().is_finished());
    receiver.finish().await;
}
