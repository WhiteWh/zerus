//! In-memory registry only. No real HKCU or native activation is used.
use super::*;
#[derive(Default)]
struct Fake {
    keys: BTreeMap<String, Contents>,
    effects: usize,
    fail_at: Option<usize>,
}
impl Registry for Fake {
    fn read(&mut self, path: &str) -> Result<Option<Contents>, &'static str> {
        Ok(self.keys.get(path).cloned())
    }
    fn create_new(
        &mut self,
        path: &str,
        values: &BTreeMap<String, String>,
    ) -> Result<(), &'static str> {
        self.effects += 1;
        if self.fail_at == Some(self.effects) {
            return Err("injected_create_failure");
        }
        if self.keys.contains_key(path) {
            return Err("create_race_kept");
        }
        self.keys.insert(
            path.into(),
            Contents {
                values: values.clone(),
                children: BTreeSet::new(),
            },
        );
        if let Some((parent, name)) = path.rsplit_once('\\') {
            if let Some(key) = self.keys.get_mut(parent) {
                key.children.insert(name.into());
            }
        }
        Ok(())
    }
    fn remove_exact(&mut self, path: &str, contents: &Contents) -> Result<(), &'static str> {
        self.effects += 1;
        if self.fail_at == Some(self.effects) {
            return Err("injected_remove_failure");
        }
        if self.keys.get(path) != Some(contents) {
            return Err("changed_key_kept");
        }
        self.keys.remove(path);
        if let Some((parent, name)) = path.rsplit_once('\\') {
            if let Some(key) = self.keys.get_mut(parent) {
                key.children.remove(name);
            }
        }
        Ok(())
    }
}
fn plan() -> Plan {
    Plan::new(&"a".repeat(64),"S-1-5-21-1000",r#""C:\Example\hgs.exe" --desktop-notification-activation --config-dir C:\Example\config --state-dir C:\Example\state"#).unwrap()
}
#[test]
fn install_and_remove_are_explicit_idempotent_and_exactly_owned() {
    let p = plan();
    let mut r = Fake::default();
    assert_eq!(
        p.apply(&mut r, "status"),
        Ok(Reply {
            registered: false,
            changed: false
        })
    );
    assert_eq!(r.effects, 0);
    assert_eq!(
        p.apply(&mut r, "install"),
        Ok(Reply {
            registered: true,
            changed: true
        })
    );
    assert_eq!(r.effects, 3);
    assert_eq!(
        p.apply(&mut r, "install"),
        Ok(Reply {
            registered: true,
            changed: false
        })
    );
    assert_eq!(r.effects, 3);
    assert_eq!(
        p.apply(&mut r, "remove"),
        Ok(Reply {
            registered: false,
            changed: true
        })
    );
    assert!(r.keys.is_empty());
    assert_eq!(r.effects, 6);
    assert_eq!(
        p.apply(&mut r, "remove"),
        Ok(Reply {
            registered: false,
            changed: false
        })
    );
    assert_eq!(r.effects, 6);
}
#[test]
fn foreign_partial_and_extra_values_refuse_before_any_mutation() {
    let p = plan();
    let mut full = Fake::default();
    p.apply(&mut full, "install").unwrap();
    for edit in 0..4 {
        let mut r = Fake {
            keys: full.keys.clone(),
            ..Default::default()
        };
        match edit {
            0 => {
                r.keys.remove(&p.keys[1].0);
            }
            1 => {
                r.keys
                    .get_mut(&p.keys[2].0)
                    .unwrap()
                    .values
                    .insert("Other".into(), "kept".into());
            }
            2 => {
                r.keys
                    .get_mut(&p.keys[0].0)
                    .unwrap()
                    .values
                    .insert(CONFIGURATION_VALUE.into(), "b".repeat(64));
            }
            _ => {
                r.keys
                    .get_mut(&p.keys[0].0)
                    .unwrap()
                    .children
                    .insert("Other".into());
            }
        }
        let before = r.keys.clone();
        for operation in ["status", "install", "remove"] {
            assert!(p.apply(&mut r, operation).is_err());
            assert_eq!(r.effects, 0);
            assert_eq!(r.keys, before);
        }
    }
}
#[test]
fn creation_or_removal_failure_keeps_partial_state_and_never_auto_replays() {
    let p = plan();
    for fail_at in 1..=3 {
        let mut r = Fake {
            fail_at: Some(fail_at),
            ..Default::default()
        };
        assert!(p.apply(&mut r, "install").unwrap_err().uncertain);
        let before = r.keys.clone();
        if !before.is_empty() {
            assert!(p.apply(&mut r, "status").is_err());
            assert_eq!(r.keys, before);
        }
    }
    for fail_at in 4..=6 {
        let mut r = Fake::default();
        p.apply(&mut r, "install").unwrap();
        r.fail_at = Some(fail_at);
        assert!(p.apply(&mut r, "remove").unwrap_err().uncertain);
        let before = r.keys.clone();
        if fail_at != 4 {
            assert!(p.apply(&mut r, "install").is_err());
            assert_eq!(r.keys, before);
        }
    }
}
#[test]
fn service_accounts_and_unbounded_or_malformed_identity_are_refused() {
    for sid in ["S-1-5-18", "S-1-5-19", "S-1-5-20", "S-1-5-80-99", "unknown"] {
        assert!(Plan::new(&"a".repeat(64), sid, "command").is_err());
    }
    assert!(Plan::new(&"A".repeat(64), "S-1-5-21-1000", "command").is_err());
    assert!(Plan::new(&"a".repeat(64), "S-1-5-21-1000", &"x".repeat(32767)).is_err());
    let p = plan();
    let mut r = Fake::default();
    assert!(p.apply(&mut r, "run").is_err());
    assert!(r.keys.is_empty());
}

#[test]
fn removal_receipt_needs_original_configuration_request_epoch_and_all_drain_fields() {
    let id = uuid::Uuid::new_v4().to_string();
    let epoch = uuid::Uuid::new_v4().to_string();
    let configuration = "a".repeat(64);
    let receipt = serde_json::json!({"request_id":id,"configuration":configuration,"writer_epoch":epoch,"accepted":true,"durable":true,"revoked":true,"checkpointed":true});
    assert!(settled_revoke(&receipt, &id, &configuration, &epoch));
    for field in ["accepted", "durable", "revoked", "checkpointed"] {
        let mut changed = receipt.clone();
        changed[field] = false.into();
        assert!(!settled_revoke(&changed, &id, &configuration, &epoch));
    }
    for field in ["request_id", "configuration", "writer_epoch"] {
        let mut changed = receipt.clone();
        changed[field] = "changed".into();
        assert!(!settled_revoke(&changed, &id, &configuration, &epoch));
    }
    assert!(!settled_revoke(
        &serde_json::json!({"accepted":true}),
        &id,
        &configuration,
        &epoch
    ));
    assert!(!settled_revoke(
        &serde_json::json!({"applicable":false,"handoff":"none","revoked":true,"nothing_written":true}),
        &id,
        &configuration,
        &epoch
    ));
}

#[test]
fn a_key_appearing_after_preflight_is_preserved_instead_of_overwritten() {
    struct Race(Fake);
    impl Registry for Race {
        fn read(&mut self, path: &str) -> Result<Option<Contents>, &'static str> {
            self.0.read(path)
        }
        fn create_new(
            &mut self,
            path: &str,
            values: &BTreeMap<String, String>,
        ) -> Result<(), &'static str> {
            self.0.keys.insert(
                path.into(),
                Contents {
                    values: [("Other".into(), "kept".into())].into_iter().collect(),
                    children: BTreeSet::new(),
                },
            );
            self.0.create_new(path, values)
        }
        fn remove_exact(&mut self, _: &str, _: &Contents) -> Result<(), &'static str> {
            panic!("create race must not clean up")
        }
    }
    let p = plan();
    let mut r = Race(Fake::default());
    assert!(p.apply(&mut r, "install").unwrap_err().uncertain);
    assert_eq!(r.0.keys[&p.keys[0].0].values["Other"], "kept");
    assert_eq!(r.0.effects, 1);
}
