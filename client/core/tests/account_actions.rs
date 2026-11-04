#![cfg(unix)]
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};
use zerus_client_core::account_actions::Values;
use zerus_client_core::{
    account_actions::{Binding, Kind},
    ComputerId,
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
/// A symlink to the checked-in (executable) fake, never a copy: writing a new
/// executable while another test thread forks lets the child inherit the
/// write fd, and exec then fails with ETXTBSY. The fake reads its files beside
/// the link (`Path(__file__).parent`), not beside the target.
fn link_fake(dir: &Path, script: &str) {
    let hgs = dir.join("hgs");
    let _ = fs::remove_file(&hgs);
    std::os::unix::fs::symlink(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/support")
            .join(script),
        hgs,
    )
    .unwrap();
}
fn setup() -> (Scratch, Binding) {
    let dir = std::env::temp_dir().join(format!(
        "zerus-account-edit-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&dir).unwrap();
    link_fake(&dir, "fake_hgs_account_edits.py");
    let catalog = json!({"revision":"r1","setup_revision":true,"profiles":[{"id":"work","provider":"codex","home":"/accounts/example","label":"Example","auth_revision":"a1"}]});
    fs::write(dir.join("catalog.json"), catalog.to_string()).unwrap();
    let binding = Binding::capture(
        ComputerId("studio".into()),
        "11111111-1111-4111-8111-111111111111".into(),
        &catalog,
        "work",
    )
    .unwrap();
    (Scratch(dir), binding)
}
#[test]
fn each_catalog_edit_reads_fresh_then_dispatches_exactly_once() {
    for kind in [Kind::Rename, Kind::Default, Kind::Remove] {
        let (scratch, binding) = setup();
        let result = binding
            .execute(&scratch.0.join("hgs"), kind, "New account")
            .unwrap();
        assert_eq!(result["revision"], "r2");
        let calls: Vec<Value> = fs::read_to_string(scratch.0.join("calls.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0][4], "ls");
        match kind {
            Kind::Rename => assert_eq!(result["profiles"][0]["label"], "New account"),
            Kind::Default => assert_eq!(result["profiles"][0]["is_default"], true),
            Kind::Remove => assert_eq!(result["profiles"], json!([])),
            _ => unreachable!(),
        }
    }
}
#[test]
fn changed_catalog_refuses_before_the_effect() {
    let (scratch, binding) = setup();
    let mut catalog: Value =
        serde_json::from_slice(&fs::read(scratch.0.join("catalog.json")).unwrap()).unwrap();
    catalog["revision"] = json!("new");
    fs::write(scratch.0.join("catalog.json"), catalog.to_string()).unwrap();
    assert!(binding
        .execute(&scratch.0.join("hgs"), Kind::Remove, "")
        .is_err());
    assert_eq!(
        fs::read_to_string(scratch.0.join("calls.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}
#[test]
fn an_uncertain_exit_never_replays_the_mutation() {
    let (scratch, binding) = setup();
    fs::write(scratch.0.join("uncertain"), b"fixture").unwrap();
    assert!(binding
        .execute(&scratch.0.join("hgs"), Kind::Rename, "New account")
        .is_err());
    assert_eq!(
        fs::read_to_string(scratch.0.join("calls.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert!(binding
        .execute(&scratch.0.join("hgs"), Kind::Rename, "New account")
        .is_err());
    assert_eq!(
        fs::read_to_string(scratch.0.join("calls.jsonl"))
            .unwrap()
            .lines()
            .count(),
        3,
        "second explicit call performs only its read and refuses the stale revision"
    );
}

#[test]
fn model_catalog_is_read_only_and_settings_use_the_same_guarded_cas() {
    let (scratch, binding) = setup();
    let models = binding.model_settings(&scratch.0.join("hgs")).unwrap();
    assert_eq!(models.models[0].id, "model-a");
    assert_eq!(models.provider_defaults.effort, "low");
    assert_eq!(
        fs::read_to_string(scratch.0.join("calls.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    let values = Values {
        model: "model-a".into(),
        effort: "high".into(),
        ..Default::default()
    };
    models.validate(&values).unwrap();
    let saved = binding
        .execute_values(&scratch.0.join("hgs"), Kind::Model, &values)
        .unwrap();
    assert_eq!(
        saved["profiles"][0]["model_defaults"],
        json!({"model":"model-a","effort":"high"})
    );
    let (scratch, binding) = setup();
    let saved = binding
        .execute_values(
            &scratch.0.join("hgs"),
            Kind::Permissions,
            &Values {
                mode: "bypass".into(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(saved["profiles"][0]["permission_mode"], "bypass");
    assert_eq!(
        fs::read_to_string(scratch.0.join("calls.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[test]
fn setup_creates_once_restores_only_exact_saved_evidence_and_keeps_keys_off_arguments() {
    use zerus_client_core::account_actions::setup::Host;
    let (scratch, binding) = setup();
    let read = || {
        serde_json::from_slice::<Value>(&fs::read(scratch.0.join("catalog.json")).unwrap()).unwrap()
    };
    let host = Host::capture(
        binding.computer.clone(),
        binding.machine_id.clone(),
        &read(),
    )
    .unwrap();
    let created = host
        .create(&scratch.0.join("hgs"), "codex-new", "codex", "Work")
        .unwrap();
    assert_eq!(created["profiles"][1]["id"], "codex-new");
    assert!(host
        .create(&scratch.0.join("hgs"), "codex-other", "codex", "Work")
        .is_err());
    let mut catalog = read();
    let saved = catalog["profiles"].as_array_mut().unwrap().remove(1);
    catalog["removed_profiles"] = json!([saved.clone()]);
    fs::write(scratch.0.join("catalog.json"), catalog.to_string()).unwrap();
    let host = Host::capture(
        binding.computer.clone(),
        binding.machine_id.clone(),
        &catalog,
    )
    .unwrap();
    let mut forged = saved.clone();
    forged["home"] = json!("/accounts/other");
    assert!(host.restore(&scratch.0.join("hgs"), &forged).is_err());
    let restored = host.restore(&scratch.0.join("hgs"), &saved).unwrap();
    assert_eq!(restored["profiles"][1]["id"], "codex-new");
    let mut catalog = read();
    catalog["profiles"] =
        json!([{"id":"native-dsh","provider":"dsh","installed":true,"label":"Example"}]);
    fs::write(scratch.0.join("catalog.json"), catalog.to_string()).unwrap();
    let host = Host::capture(binding.computer, binding.machine_id, &catalog).unwrap();
    host.set_key(&scratch.0.join("hgs"), Some("Work"), "fixture-secret")
        .unwrap();
    assert!(!fs::read_to_string(scratch.0.join("calls.jsonl"))
        .unwrap()
        .contains("fixture-secret"));
}

#[test]
fn transfer_uses_existing_hgs_copy_with_both_computer_catalog_and_auth_guards() {
    use zerus_client_core::account_actions::{setup::Host, transfer};
    for scenario in ["success", "legacy", "uncertain", "changed"] {
        let (scratch, binding) = setup();
        link_fake(&scratch.0, "fake_hgs_account_transfer.py");
        let mut source: Value =
            serde_json::from_slice(&fs::read(scratch.0.join("catalog.json")).unwrap()).unwrap();
        source["copy_revision"] = json!(true);
        source["profiles"][0]["portable"] = json!(true);
        source["profiles"][0]["credential_file"] = json!(true);
        if scenario == "changed" {
            source["profiles"][0]["auth_revision"] = json!("different");
        }
        fs::write(scratch.0.join("studio.json"), source.to_string()).unwrap();
        let destination = json!({"revision":"r1","setup_revision":true,"copy_revision":true,"profiles":[],"removed_profiles":[]});
        fs::write(scratch.0.join("lab.json"), destination.to_string()).unwrap();
        let target = Host::capture(
            ComputerId("lab".into()),
            "22222222-2222-4222-8222-222222222222".into(),
            &destination,
        )
        .unwrap();
        if ["legacy", "uncertain"].contains(&scenario) {
            fs::write(scratch.0.join(scenario), b"fixture").unwrap();
        }
        let result = transfer::copy(
            &scratch.0.join("hgs"),
            &binding,
            &target,
            "codex-copy",
            "Example",
        );
        match scenario {
            "success" => assert_eq!(result.unwrap()["profiles"][0]["id"], "codex-copy"),
            "uncertain" => assert!(result.unwrap_err().issued),
            _ => assert!(!result.unwrap_err().issued),
        }
        let calls: Vec<Value> = fs::read_to_string(scratch.0.join("calls.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let effects = calls
            .iter()
            .filter(|args| args[0] == "account" && args[1] == "copy")
            .count();
        assert_eq!(
            effects,
            usize::from(["success", "uncertain"].contains(&scenario))
        );
    }
}
