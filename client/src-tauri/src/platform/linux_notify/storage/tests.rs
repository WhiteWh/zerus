use super::*;
use serde_json::{json, Value};
use std::{
    io::Write,
    num::NonZeroU32,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering},
};
use zerus_client_core::linux_notify::{
    owned::data::{
        transitions::{Post, PostBatch, PostOutcome},
        Action, Original, OriginalInput,
    },
    Namespace,
};

const HOST: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const BUS: &str = "00112233445566778899aabbccddeeff";
const OWNERS: [Owner; 2] = [Owner::AgentAttention, Owner::SoftwareUpdate];

// A parallel test's fork can inherit our CLOEXEC root until that child execs.
// Dropping our File then does NOT release the shared OFD lock. Run immediate
// close/reopen assertions after exec in a single-test child, with no concurrent
// spawns, rather than weakening the adapter's lock or retrying its refusals.
fn private_lock_case(case: &'static str) -> bool {
    const MARKER: &str = "ZERUS_STORAGE_LOCK_CASE";
    if std::env::var(MARKER).as_deref() == Ok(case) {
        return true;
    }
    let _tree = crate::PROCESS_TREE
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let scratch = Scratch::new();
    let root = scratch.path.clone();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", case, "--nocapture", "--test-threads=1"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env(MARKER, case)
        .stdin(std::process::Stdio::null());
    for key in [
        "HOME",
        "HGS_CONFIG_DIR",
        "HGS_STATE_DIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
        "TMPDIR",
        "TMUX_TMPDIR",
    ] {
        child.env(key, &root);
    }
    let mut child = child.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "private lock case {case}: {status}");
            return false;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("private lock case {case} timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

struct Scratch {
    path: PathBuf,
    keep: bool,
}
impl Scratch {
    fn new() -> Self {
        Self::at(
            std::env::temp_dir().join(format!("g4-store-{}", uuid::Uuid::new_v4())),
            false,
        )
    }
    fn at(path: PathBuf, keep: bool) -> Self {
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let scratch = Self { path, keep };
        scratch.put(0, &envelope(Owner::AgentAttention));
        scratch.put(1, &envelope(Owner::SoftwareUpdate));
        scratch.put(
            2,
            &serde_json::to_vec(
                &json!({"version":1,"bus_id":BUS,"owner":":1.0","tokens":{},"uncertain":[]}),
            )
            .unwrap(),
        );
        scratch
    }
    fn put(&self, index: usize, bytes: &[u8]) {
        let path = self.path.join(NAMES[index]);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
    }
    fn raw(&self, index: usize) -> Vec<u8> {
        fs::read(self.path.join(NAMES[index])).unwrap()
    }
    fn open(&self) -> File {
        File::open(&self.path).unwrap()
    }
    fn load(&self) -> Result<Storage> {
        Storage::load(self.open(), HOST.into())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if !self.keep {
            fs::remove_dir_all(&self.path).unwrap();
        }
    }
}
fn namespace() -> Namespace {
    Namespace {
        bus_id: BUS.into(),
        owner: ":1.0".into(),
    }
}
fn token(owner: Owner, n: u64) -> String {
    match owner {
        Owner::AgentAttention => zerus_client_core::attention::encode_token(
            json!({"host":"","name":"codex/example/one","notice":format!("edge-{n}")})
                .as_object()
                .unwrap(),
        ),
        Owner::SoftwareUpdate => format!("zerus-update:v2:dev:{n}"),
    }
}
fn envelope(owner: Owner) -> Vec<u8> {
    let mut value = json!({"version":2,"bus_id":BUS,"owner":":1.0","tokens":{},"uncertain":[],"legacy_uncertain":[]});
    if owner == Owner::SoftwareUpdate {
        value["invalidations"] = json!([]);
    }
    serde_json::to_vec(&value).unwrap()
}
fn row(owner: Owner, n: u64, state: &str, id: Option<u32>) -> Value {
    let mut value = json!({
        "owner": if owner == Owner::AgentAttention { "agent_attention" } else { "software_update" },
        "token": token(owner,n), "state":state,
        "original":{"request_id":format!("request-{n}"),"poster_instance":HOST,"post_nonce":n,
            "bus_id":BUS,"daemon_owner":":1.0","namespace_generation":1,"actions":["default","open-session"]}
    });
    if let Some(id) = id {
        value["native_id"] = json!(id);
    }
    value
}
fn retained(owner: Owner, n: u64, state: &str, id: Option<u32>) -> Vec<u8> {
    let mut value: Value = serde_json::from_slice(&envelope(owner)).unwrap();
    let row = row(owner, n, state, id);
    if state == "delivered" {
        value["tokens"][id.unwrap().to_string()] = row;
    } else {
        value["uncertain"] = json!([row]);
    }
    serde_json::to_vec(&value).unwrap()
}
fn queue(store: &mut Storage, owner: Owner, n: u64) {
    store
        .model
        .prepare_batch(
            owner,
            PostBatch {
                raised: vec![Post {
                    original: Original::new(
                        owner,
                        OriginalInput {
                            token: token(owner, n),
                            request_id: format!("request-{n}"),
                            poster_instance: HOST.into(),
                            post_nonce: n,
                            namespace: namespace(),
                            namespace_generation: 1,
                            actions: vec![Action::Default, Action::OpenSession],
                        },
                    )
                    .unwrap(),
                    title: "Title".into(),
                    body: "Private body".into(),
                }],
            },
        )
        .unwrap();
}
fn report(store: &mut Storage, id: u32, unknown: bool) {
    let mut proposal = store.model.take_next_native().unwrap().unwrap();
    let id = NonZeroU32::new(id).unwrap();
    let outcome = if unknown {
        PostOutcome::Unknown {
            native_id: Some(id),
        }
    } else {
        PostOutcome::Delivered(id)
    };
    let result = proposal.report(namespace(), outcome).unwrap();
    store.model.finish_post(&mut proposal, result).unwrap();
}
fn restart_refuses(scratch: &Scratch) {
    assert!(
        scratch.load().is_err(),
        "a new load must not resume an original live chain"
    );
}

#[test]
fn selected_private_load_keeps_raw_hashes_and_single_absent_or_present_legacy() {
    for legacy in [false, true] {
        let scratch = Scratch::new();
        if !legacy {
            fs::remove_file(scratch.path.join(NAMES[2])).unwrap();
        }
        let store = scratch.load().unwrap();
        for owner in OWNERS {
            let raw = scratch.raw(selected(owner));
            let image = store.model.image_report(owner);
            assert_eq!(image.last_acknowledged.sha256(), Some(hash(&raw).as_str()));
            assert_eq!(image.last_acknowledged.bytes(), Some(raw.len()));
        }
        assert_eq!(store.images[2].is_some(), legacy);
        assert!(!store.model.snapshot().write_pending);
    }
}
#[test]
fn independent_descriptions_contend_and_duplicates_explicitly_do_not() {
    if !private_lock_case("platform::linux_notify::storage::tests::independent_descriptions_contend_and_duplicates_explicitly_do_not") {
        return;
    }
    let scratch = Scratch::new();
    let file = scratch.open();
    let alias = file.try_clone().unwrap();
    let store = Storage::load(file, HOST.into()).unwrap();
    assert!(scratch.load().is_err());
    // SAFETY: this duplicate remains open, sharing the same OFD as store.root.
    assert_eq!(
        unsafe { libc::flock(alias.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    drop(store);
    assert!(
        scratch.load().is_err(),
        "Drop must not explicitly unlock an alias"
    );
    drop(alias);
    scratch.load().unwrap();
}
#[test]
fn unsafe_roots_and_unavailable_anchor_refuse_without_a_fallback() {
    let scratch = Scratch::new();
    for mode in [0o750, 0o770, 0o1700] {
        fs::set_permissions(&scratch.path, fs::Permissions::from_mode(mode)).unwrap();
        assert!(scratch.load().is_err());
    }
    fs::set_permissions(&scratch.path, fs::Permissions::from_mode(0o700)).unwrap();
    let mut store = scratch.load().unwrap();
    store.root.anchor = scratch.path.join("missing-anchor");
    assert_eq!(store.write_next(), Err(Refused::Io));
    assert!(store.refused);
}
#[test]
fn unsafe_leaf_types_links_and_permissions_refuse_before_any_write() {
    for (index, name) in NAMES.iter().enumerate() {
        for kind in 0..6 {
            let scratch = Scratch::new();
            let path = scratch.path.join(name);
            let original = scratch.raw(index);
            fs::remove_file(&path).unwrap();
            match kind {
                0 => std::os::unix::fs::symlink("missing", &path).unwrap(),
                1 => fs::create_dir(&path).unwrap(),
                2 => {
                    use std::ffi::CString;
                    let name = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
                    // SAFETY: a NUL-terminated path inside this owned fixture.
                    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                }
                3 => {
                    scratch.put(index, &original);
                    fs::hard_link(&path, scratch.path.join("alias")).unwrap();
                }
                4 => {
                    scratch.put(index, &original);
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
                }
                _ => {
                    scratch.put(index, &original);
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o1600)).unwrap();
                }
            }
            assert!(scratch.load().is_err(), "index {index} kind {kind}");
        }
    }
}
#[test]
fn missing_pair_malformed_owner_unknown_and_incomplete_provenance_fail_closed() {
    for index in 0..2 {
        for kind in 0..5 {
            let scratch = Scratch::new();
            let mut value: Value = serde_json::from_slice(&scratch.raw(index)).unwrap();
            match kind {
                0 => {
                    fs::remove_file(scratch.path.join(NAMES[index])).unwrap();
                }
                1 => scratch.put(index, b"{broken"),
                2 => {
                    value["foreign"] = json!(true);
                    scratch.put(index, &serde_json::to_vec(&value).unwrap());
                }
                3 => {
                    value["uncertain"] = json!([{"owner":if index==0 {"software_update"}else{"agent_attention"},"token":token(OWNERS[1-index],1)}]);
                    scratch.put(index, &serde_json::to_vec(&value).unwrap());
                }
                _ => {
                    value["uncertain"] = json!([{"owner":if index==0 {"agent_attention"}else{"software_update"},"token":token(OWNERS[index],1)}]);
                    scratch.put(index, &serde_json::to_vec(&value).unwrap());
                }
            }
            assert!(scratch.load().is_err(), "index {index} kind {kind}");
        }
    }
}
#[test]
fn one_shared_raw_byte_budget_refuses_oversize_and_accepts_exact_boundary() {
    for extra in [0, 1] {
        let scratch = Scratch::new();
        let mut a = scratch.raw(0);
        let other = scratch.raw(1).len() + scratch.raw(2).len();
        a.resize(FILE_LIMIT as usize - other + extra, b' ');
        scratch.put(0, &a);
        assert_eq!(scratch.load().is_ok(), extra == 0);
    }
    let scratch = Scratch::new();
    let file = OpenOptions::new()
        .write(true)
        .open(scratch.path.join(NAMES[0]))
        .unwrap();
    file.set_len(FILE_LIMIT + 1).unwrap();
    assert!(scratch.load().is_err());
}
#[test]
fn untouched_raw_padding_counts_before_the_next_physical_replacement() {
    for owner in OWNERS {
        let mut scratch = Scratch::new();
        let other = 1 - selected(owner);
        let mut padded = scratch.raw(other);
        padded.resize(
            FILE_LIMIT as usize - scratch.raw(selected(owner)).len() - scratch.raw(2).len() - 64,
            b' ',
        );
        scratch.put(other, &padded);
        drop(padded);
        let before: [String; 3] = std::array::from_fn(|i| hash(&scratch.raw(i)));
        let mut store = scratch.load().unwrap();
        queue(&mut store, owner, 1);
        // Keep failed-case bytes for the private supervisor; successful fixtures
        // still clean up. Raw output pressure is distinct from canonical Model
        // capacity, and must refuse BEFORE changing even one target file.
        scratch.keep = true;
        eprintln!("RAW_OUTPUT_PRESSURE fixture {}", scratch.path.display());
        assert_eq!(store.write_next(), Err(Refused::Limit));
        assert_eq!(before, std::array::from_fn(|i| hash(&scratch.raw(i))));
        let state = store.model.snapshot();
        assert!(state.write_pending && state.write_unknown && state.proof_invalidated);
        assert_eq!(store.write_next(), Err(Refused::Stopped));
        scratch.keep = false;
    }
}
#[test]
fn raw_entries_are_counted_before_duplicate_projection_and_legacy_is_bounded() {
    let scratch = Scratch::new();
    let entry =
        serde_json::to_string(&row(Owner::AgentAttention, 1, "possible_post", None)).unwrap();
    let raw=format!("{{\"version\":2,\"bus_id\":\"{BUS}\",\"owner\":\":1.0\",\"tokens\":{{}},\"uncertain\":[{}],\"legacy_uncertain\":[]}}",vec![entry;8193].join(","));
    scratch.put(0, raw.as_bytes());
    assert!(scratch.load().is_err());
    scratch.put(0, &envelope(Owner::AgentAttention));
    scratch.put(2,&serde_json::to_vec(&json!({"version":1,"owner":":1.0","tokens":{},"uncertain":(0..4097).map(|n|format!("legacy-{n}")).collect::<Vec<_>>()})).unwrap());
    assert!(scratch.load().is_err());
}
#[test]
fn all_retained_positive_states_recompute_collisions_without_an_invalidation_file() {
    for state in [
        "delivered",
        "unknown_post",
        "withdrawal_pending",
        "unknown_withdrawal",
    ] {
        let scratch = Scratch::new();
        scratch.put(0, &retained(Owner::AgentAttention, 1, state, Some(42)));
        scratch.put(1, &retained(Owner::SoftwareUpdate, 2, state, Some(42)));
        assert!(scratch.load().is_err(), "cross-owner {state}");
        scratch.put(1, &envelope(Owner::SoftwareUpdate));
        let mut a: Value =
            serde_json::from_slice(&retained(Owner::AgentAttention, 1, "delivered", Some(42)))
                .unwrap();
        a["uncertain"] = json!([row(Owner::AgentAttention, 2, state, Some(42))]);
        scratch.put(0, &serde_json::to_vec(&a).unwrap());
        assert!(scratch.load().is_err(), "same-owner {state}");
    }
    for owner in OWNERS {
        for state in ["possible_post", "unknown_post"] {
            let scratch = Scratch::new();
            scratch.put(selected(owner), &retained(owner, 1, state, None));
            assert!(scratch.load().is_err());
        }
    }
}
#[test]
fn actual_barrier_and_result_are_private_exact_and_only_the_live_chain_continues() {
    if !private_lock_case("platform::linux_notify::storage::tests::actual_barrier_and_result_are_private_exact_and_only_the_live_chain_continues") {
        return;
    }
    for owner in OWNERS {
        let scratch = Scratch::new();
        let other = scratch.raw(1 - selected(owner));
        let legacy = scratch.raw(2);
        let mut store = scratch.load().unwrap();
        queue(&mut store, owner, 1);
        assert_eq!(store.write_next(), Ok(Some((owner, WritePhase::PreIssue))));
        assert!(matches(
            &store.images[selected(owner)],
            store.model.image_report(owner).last_acknowledged
        ));
        // This independent load refuses the held lock too. Decode the actual
        // raw pair explicitly to establish the restart policy, separately.
        assert!(inspect_selected(Inputs {
            agent: Some(&scratch.raw(0)),
            update: Some(&scratch.raw(1)),
            legacy: Some(&legacy)
        })
        .is_err());
        report(&mut store, 17, false);
        assert_eq!(
            store.write_next(),
            Ok(Some((owner, WritePhase::PostResult)))
        );
        assert_eq!(store.write_next(), Ok(None));
        assert_eq!(scratch.raw(1 - selected(owner)), other);
        assert_eq!(scratch.raw(2), legacy);
        assert_eq!(
            fs::metadata(scratch.path.join(NAMES[selected(owner)]))
                .unwrap()
                .mode()
                & 0o7777,
            0o600
        );
        drop(store);
        scratch.load().unwrap();
    }
}
#[test]
fn update_first_collision_durability_precedes_agent_and_restart_always_refuses() {
    for incoming in OWNERS {
        for unknown in [false, true] {
            let scratch = Scratch::new();
            scratch.put(
                0,
                &retained(Owner::AgentAttention, 1, "delivered", Some(42)),
            );
            let legacy = scratch.raw(2);
            let mut store = scratch.load().unwrap();
            queue(&mut store, incoming, 9);
            store.write_next().unwrap();
            report(&mut store, 42, unknown);
            let agent_before = scratch.raw(0);
            assert!(store.model.snapshot().proof_invalidated);
            assert_eq!(
                store.write_next(),
                Ok(Some((
                    Owner::SoftwareUpdate,
                    WritePhase::CollisionInvalidations
                )))
            );
            assert_eq!(scratch.raw(0), agent_before);
            assert!(
                !serde_json::from_slice::<Value>(&scratch.raw(1)).unwrap()["invalidations"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            if incoming == Owner::AgentAttention {
                assert_eq!(
                    store.write_next(),
                    Ok(Some((Owner::AgentAttention, WritePhase::CollisionResult)))
                );
            }
            assert_eq!(scratch.raw(2), legacy);
            assert!(store.model.snapshot().proof_invalidated);
            drop(store);
            restart_refuses(&scratch);
        }
    }
}
#[test]
fn every_post_plan_phase_failure_retains_original_unknown_and_forbids_replay() {
    for owner in OWNERS {
        for stage in [
            Stage::BeforeHelper,
            Stage::AfterHelper,
            Stage::BeforeDirSync,
            Stage::AfterDirSync,
            Stage::BeforeReadback,
            Stage::BeforeAck,
        ] {
            let scratch = Scratch::new();
            let mut store = scratch.load().unwrap();
            queue(&mut store, owner, 1);
            let original = store
                .model
                .image_report(owner)
                .last_acknowledged
                .sha256()
                .unwrap()
                .to_owned();
            store.hooks.fail = Some(stage);
            assert_eq!(store.write_next(), Err(Refused::Io));
            let state = store.model.snapshot();
            assert!(state.proof_invalidated && state.write_pending && state.write_unknown);
            assert_eq!(
                store.model.image_report(owner).last_acknowledged.sha256(),
                Some(original.as_str())
            );
            assert!(store
                .model
                .image_report(owner)
                .attempted_unconfirmed
                .is_some());
            assert_eq!(store.write_next(), Err(Refused::Stopped));
            drop(store); // A new barrier image refuses restart; an untouched old
                         // image is still only old data, never continuation/replay permission.
        }
    }
}
#[test]
fn model_ack_refusal_keeps_disk_and_original_custody_unknown_without_snapshot_advance() {
    let scratch = Scratch::new();
    let mut store = scratch.load().unwrap();
    queue(&mut store, Owner::AgentAttention, 1);
    let old = store.images[0].as_ref().unwrap().sha256.clone();
    store.hooks.unknown_before_saved = true;
    assert_eq!(store.write_next(), Err(Refused::Policy));
    assert_eq!(store.images[0].as_ref().unwrap().sha256, old);
    assert_ne!(hash(&scratch.raw(0)), old);
    assert!(store.model.snapshot().write_pending && store.model.snapshot().write_unknown);
    assert_eq!(store.write_next(), Err(Refused::Stopped));
}
#[test]
fn observed_legacy_other_root_permission_or_same_bytes_inode_drift_is_sticky() {
    for kind in 0..4 {
        let scratch = Scratch::new();
        let mut store = scratch.load().unwrap();
        queue(&mut store, Owner::AgentAttention, 1);
        match kind {
            0 => {
                let mut raw = scratch.raw(2);
                raw.push(b' ');
                scratch.put(2, &raw);
            }
            1 => {
                let mut raw = scratch.raw(1);
                raw.push(b' ');
                scratch.put(1, &raw);
            }
            2 => fs::set_permissions(&scratch.path, fs::Permissions::from_mode(0o750)).unwrap(),
            _ => {
                let raw = scratch.raw(0);
                fs::remove_file(scratch.path.join(NAMES[0])).unwrap();
                scratch.put(0, &raw);
            }
        }
        assert!(store.write_next().is_err());
        assert!(store.refused);
        assert!(!store.model.snapshot().write_pending);
        assert_eq!(store.write_next(), Err(Refused::Stopped));
    }
}
#[test]
fn renamed_root_stays_on_the_original_inode_and_never_touches_named_replacement() {
    let mut scratch = Scratch::new();
    let original = scratch.path.clone();
    let moved = original.with_extension("moved");
    let mut store = scratch.load().unwrap();
    queue(&mut store, Owner::AgentAttention, 1);
    fs::rename(&original, &moved).unwrap();
    scratch.path = moved;
    fs::DirBuilder::new().mode(0o700).create(&original).unwrap();
    fs::write(original.join("sentinel"), b"unchanged").unwrap();
    assert_eq!(
        store.write_next(),
        Ok(Some((Owner::AgentAttention, WritePhase::PreIssue)))
    );
    assert_eq!(fs::read(original.join("sentinel")).unwrap(), b"unchanged");
    assert!(!original.join(NAMES[0]).exists());
    fs::remove_dir_all(original).unwrap();
}
#[test]
fn frozen_agent_bytes_stay_exact_while_disjoint_update_writes_continue() {
    let scratch = Scratch::new();
    let agent = scratch.raw(0);
    let legacy = scratch.raw(2);
    let mut store = scratch.load().unwrap();
    store.model.close_agent_inputs();
    store.model.freeze_agent_writes().unwrap();
    queue(&mut store, Owner::SoftwareUpdate, 1);
    assert_eq!(
        store.write_next(),
        Ok(Some((Owner::SoftwareUpdate, WritePhase::PreIssue)))
    );
    report(&mut store, 51, false);
    store.write_next().unwrap();
    assert_eq!(scratch.raw(0), agent);
    assert_eq!(scratch.raw(2), legacy);
}

static CHILD_TRACE: AtomicBool = AtomicBool::new(false);
static CHILD_PAUSE: AtomicU8 = AtomicU8::new(0);
static CHILD_MODE: AtomicU8 = AtomicU8::new(0);
static CHILD_ROOT: AtomicI32 = AtomicI32::new(-1);
fn child_event(stage: Stage) {
    let (id, line): (u8, &[u8]) = match stage {
        Stage::BeforePlan => (1, b"G4STORE BEFORE_PLAN\n"),
        Stage::BeforeHelper => (2, b"G4STORE BEFORE_HELPER\n"),
        Stage::AfterHelper => (3, b"G4STORE AFTER_HELPER\n"),
        Stage::BeforeDirSync => (4, b"G4STORE BEFORE_DIR_SYNC\n"),
        Stage::AfterDirSync => (5, b"G4STORE AFTER_DIR_SYNC\n"),
        Stage::BeforeReadback => (6, b"G4STORE BEFORE_READBACK\n"),
        Stage::BeforeAck => (7, b"G4STORE BEFORE_ACK\n"),
    };
    if CHILD_TRACE.load(Ordering::SeqCst) {
        // SAFETY: fixed static bytes to this private child's stderr; no pointer,
        // body, allocation or caller-supplied text enters the scalar phase hook.
        unsafe {
            libc::write(2, line.as_ptr().cast(), line.len());
        }
    }
    if CHILD_MODE.load(Ordering::SeqCst) == id {
        // SAFETY: configured only for this exact child's still-owned root FD.
        assert_eq!(
            unsafe { libc::fchmod(CHILD_ROOT.load(Ordering::SeqCst), 0o750) },
            0
        );
    }
    if CHILD_PAUSE.load(Ordering::SeqCst) == id {
        // SAFETY: stop only this original supervised private fixture process.
        assert_eq!(unsafe { libc::raise(libc::SIGSTOP) }, 0);
    }
}
#[test]
fn controlled_private_child_entry() {
    let private = std::env::var_os("TEST_G4_STORAGE_ROOT");
    let scratch = if let Some(path) = private.as_ref() {
        let path = PathBuf::from(path);
        assert_eq!(path.parent(), Some(std::env::temp_dir().as_path()));
        assert!(path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("g4-storage-child-"));
        if let Ok(expected) = std::env::var("TEST_G4_STORAGE_RESTART") {
            assert!(matches!(expected.as_str(), "accept-old" | "refuse"));
            // A fresh independent read after the original child died. This
            // constructs supplied-data policy only, never resumes its Attempt
            // or manufactures an Original/native permission.
            let result = Storage::load(File::open(path).unwrap(), HOST.into());
            assert_eq!(result.is_ok(), expected == "accept-old");
            println!("G4STORE RESTART {expected}");
            return;
        }
        Scratch::at(path, true)
    } else {
        Scratch::new()
    };
    let before: [String; 3] = std::array::from_fn(|i| hash(&scratch.raw(i)));
    if private.is_some() && std::env::var("TEST_G4_STORAGE_NON_CLOEXEC").as_deref() == Ok("1") {
        // This negative control runs ONLY in the exact supervised child with
        // no other tests or spawning threads. Never expose an inheritable FD
        // during the normal parallel campaign.
        let file = scratch.open();
        // SAFETY: this exact child's owned fixture descriptor, closed on refusal.
        assert_eq!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0) },
            0
        );
        assert!(Storage::load(file, HOST.into()).is_err());
        assert_eq!(before, std::array::from_fn(|i| hash(&scratch.raw(i))));
        println!("G4STORE RESULT refused-non-cloexec");
        return;
    }
    let mut store = scratch.load().unwrap();
    queue(&mut store, Owner::AgentAttention, 1);
    if private.is_some() {
        CHILD_TRACE.store(true, Ordering::SeqCst);
        CHILD_ROOT.store(store.root.file.as_raw_fd(), Ordering::SeqCst);
        for (name, dest) in [
            ("TEST_G4_STORAGE_PAUSE", &CHILD_PAUSE),
            ("TEST_G4_STORAGE_MODE", &CHILD_MODE),
        ] {
            dest.store(
                std::env::var(name).ok().map_or(0, |v| v.parse().unwrap()),
                Ordering::SeqCst,
            );
        }
        store.hooks.event = Some(child_event);
        println!("G4STORE BEFORE {}", serde_json::to_string(&before).unwrap());
    }
    let result = store.write_next();
    if std::env::var("TEST_G4_STORAGE_EXPECT").as_deref() == Ok("unknown") {
        assert!(result.is_err());
        assert!(store.refused);
        let d = store.model.snapshot();
        assert!(d.proof_invalidated && d.write_pending && d.write_unknown);
        assert_eq!(store.write_next(), Err(Refused::Stopped));
    } else {
        assert_eq!(
            result,
            Ok(Some((Owner::AgentAttention, WritePhase::PreIssue)))
        );
    }
    if private.is_some() {
        println!(
            "G4STORE AFTER {}",
            serde_json::to_string(&std::array::from_fn::<_, 3, _>(|i| hash(&scratch.raw(i))))
                .unwrap()
        );
        println!(
            "G4STORE RESULT {}",
            if result.is_ok() { "saved" } else { "unknown" }
        );
    }
}
