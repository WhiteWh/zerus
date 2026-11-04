//! Native Files intents only. Slow cache preparation precedes the FRESH
//! original-child Execute registration. Lost replies query original IDs; no
//! retry creates another effect or renews an old native permit.
use super::*;
use std::time::Instant;
use zerus_client_core::desktop::cache_maintenance::{self as wire, Command, Phase, Status, Timing};

const UNAVAILABLE: &str = "Downloaded files maintenance is unavailable from the desktop service.";
const UNCONFIRMED: &str = "The previous downloaded files maintenance is unconfirmed. Check it again before making another change.";

#[derive(Clone)]
pub(super) struct Pending {
    id: String,
    command: Command,
}
fn identity() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn terminal(status: &Status) -> bool {
    status.drained
        && matches!(
            status.phase,
            Phase::Settled | Phase::Refused | Phase::Unknown
        )
}
fn no_effect(value: &Value, pending: &Pending) -> bool {
    // Only consumed from the native Frontend controller, whose original-child
    // exit + absence rules settle lost submissions before emitting this shape.
    value["request_id"] == pending.id
        && value["op"] == "cache_maintenance"
        && value["accepted"] == false
        && value["durable"] == false
        && value["pending"] == false
}
fn status(value: &Value, pending: &Pending) -> Option<Status> {
    let mut value = value.clone();
    value["request_id"] = json!(pending.id);
    Status::response(&value, &pending.id, &pending.command)
}
fn selected_root(root: PathBuf) -> Result<String, String> {
    if let Ok(meta) = std::fs::symlink_metadata(&root) {
        if meta.file_type().is_symlink() {
            return Err(UNAVAILABLE.into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err(UNAVAILABLE.into());
            }
        }
    }
    // No creation and no unbounded parent walk. Preserve a missing final
    // folder so an explicit preference can be saved before the first download.
    let mut parent = root.as_path();
    let mut tail = Vec::new();
    let canonical = loop {
        if let Ok(value) = std::fs::canonicalize(parent) {
            break value;
        }
        if tail.len() == 64 {
            return Err(UNAVAILABLE.into());
        }
        tail.push(parent.file_name().ok_or(UNAVAILABLE)?.to_owned());
        parent = parent.parent().ok_or(UNAVAILABLE)?;
    };
    let mut path = canonical;
    for part in tail.into_iter().rev() {
        path.push(part);
    }
    let path = path
        .to_str()
        .filter(|v| v.len() <= 8192 && !v.chars().any(char::is_control))
        .ok_or(UNAVAILABLE)?;
    Ok(path.into())
}
impl Service {
    fn cache_pending(&self) -> Option<Pending> {
        self.shared
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    fn cache_record(&self, pending: Option<Pending>) {
        *self
            .shared
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = pending;
    }
    async fn cache_lookup(&self, pending: &Pending) -> Option<Status> {
        let query = identity();
        let value = request(
            &self.shared,
            json!({"op":"cache_maintenance_result","request_id":query,
            "original_request_id":pending.id}),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .ok()?;
        if value["op"] != "cache_maintenance_result"
            || value["request_id"] != query
            || value["original_request_id"] != pending.id
            || value["accepted"] != true
            || value["durable"] != false
            || value["found"] != true
        {
            return None;
        }
        status(&value["result"], pending)
    }
    /// `change` is Some ONLY for Files > Apply. Normal refresh/downloads use
    /// actor-owned policy, never imported UI preferences as deletion consent.
    pub(crate) async fn cache_pass(
        self: &Arc<Self>,
        root: PathBuf,
        change: Option<u64>,
        timing: Timing,
    ) -> Result<Option<Status>, String> {
        let _charge = self
            .shared
            .cache_rpc
            .admit(Role::Downloads)
            .map_err(|_| UNCONFIRMED)?;
        if self.shared.stopped.load(Ordering::Acquire) {
            return Err(UNAVAILABLE.into());
        }
        if let Some(old) = self.cache_pending() {
            // Query only: never silently replay the previous phase. Ordinary
            // refresh can display the still-pending original's bounded data.
            let last = self
                .frontend_result(frontend::Operation::CacheMaintenance)
                .await;
            if no_effect(&last, &old) {
                self.cache_record(None);
            } else if let Some(observed) = self.cache_lookup(&old).await {
                if terminal(&observed) {
                    self.cache_record(None);
                }
                if change.is_none() {
                    return Ok(Some(observed));
                }
                if !terminal(&observed) {
                    return Err(UNCONFIRMED.into());
                }
            } else {
                return Err(UNCONFIRMED.into());
            }
        }
        let filesystem_hold = _charge.clone();
        let root = tauri::async_runtime::spawn_blocking(move || {
            let _hold = filesystem_hold;
            selected_root(root)
        })
        .await
        .map_err(|_| UNAVAILABLE)??;
        let configuration = self
            .shared
            .configuration
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or(UNAVAILABLE)?;
        let query = identity();
        let policy = request(
            &self.shared,
            json!({"op":"cache_maintenance_policy","request_id":query,"root":root}),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .map_err(|_| UNAVAILABLE)?;
        if policy["request_id"] != query
            || policy["abi"] != wire::ABI
            || policy["configuration"] != configuration
            || policy["root"] != root
        {
            return Err(UNAVAILABLE.into());
        }
        let revision = policy["revision"]
            .as_u64()
            .filter(|v| *v < i64::MAX as u64)
            .ok_or(UNAVAILABLE)?;
        if policy["configured"] != json!(revision > 0) {
            return Err(UNAVAILABLE.into());
        }
        let limit = match change {
            Some(value) => value,
            None if revision == 0 => return Ok(None),
            None => policy["limit"].as_u64().ok_or(UNAVAILABLE)?,
        };
        if limit > i64::MAX as u64 {
            return Err(UNAVAILABLE.into());
        }
        let operation_id = identity();
        let preparation_id = identity();
        let pending = Pending {
            id: preparation_id.clone(),
            command: Command::Prepare {
                operation_id: operation_id.clone(),
                configuration: configuration.clone(),
                root,
                limit,
                revision,
                timing,
                policy_change: change.is_some(),
            },
        };
        self.cache_record(Some(pending.clone()));
        let until = Instant::now() + Duration::from_secs(20);
        let value = self
            .cache_intent(pending.id.clone(), pending.command.clone())
            .await?;
        if value["pending"] == false && value["durable"] == false {
            self.cache_record(None);
            return Err(UNAVAILABLE.into());
        }
        let mut observed = status(&value, &pending);
        loop {
            if let Some(current) = &observed {
                if terminal(current) {
                    self.cache_record(None);
                    return Ok(observed);
                }
                if current.phase == Phase::Prepared {
                    break;
                }
            }
            if Instant::now() >= until || self.shared.stopped.load(Ordering::Acquire) {
                return Err(UNCONFIRMED.into());
            }
            // This also settles the frontend packet's retained original-child
            // bookkeeping before a NEW original Execute child is registered.
            let receipt = self
                .frontend_result(frontend::Operation::CacheMaintenance)
                .await;
            if status(&receipt, &pending).is_none() {
                if no_effect(&receipt, &pending) {
                    self.cache_record(None);
                }
                return Err(UNCONFIRMED.into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            observed = self.cache_lookup(&pending).await;
        }
        let hash = observed.and_then(|v| v.binding_sha256).ok_or(UNCONFIRMED)?;
        let receipt = self
            .frontend_result(frontend::Operation::CacheMaintenance)
            .await;
        if status(&receipt, &pending).is_none() {
            return Err(UNCONFIRMED.into());
        }
        let execute = Pending {
            id: operation_id,
            command: Command::Execute {
                configuration,
                preparation_id,
                binding_sha256: hash,
            },
        };
        // Replace before spawn. Any cancellation/lost reply keeps this exact
        // original ID for LOOKUP ONLY; a later call cannot dispatch it again.
        self.cache_record(Some(execute.clone()));
        let value = self
            .cache_intent(execute.id.clone(), execute.command.clone())
            .await?;
        if value["pending"] == false && value["durable"] == false {
            self.cache_record(None);
            return Err(UNAVAILABLE.into());
        }
        let mut observed = status(&value, &execute);
        loop {
            if let Some(current) = &observed {
                if terminal(current) {
                    self.cache_record(None);
                    return Ok(observed);
                }
                if timing == Timing::AfterOpeningGrace && current.phase == Phase::Reserved {
                    return Ok(observed);
                }
            }
            if Instant::now() >= until || self.shared.stopped.load(Ordering::Acquire) {
                return Err(UNCONFIRMED.into());
            }
            let _ = self
                .frontend_result(frontend::Operation::CacheMaintenance)
                .await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            observed = self.cache_lookup(&execute).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_folder_uses_its_existing_canonical_parent_without_creation() {
        let base = std::env::temp_dir().canonicalize().unwrap();
        let path = base.join(format!("zerus-missing-cache-{}", uuid::Uuid::new_v4()));
        assert_eq!(selected_root(path.clone()).unwrap(), path.to_str().unwrap());
        assert!(!path.exists());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod native_tests {
    use super::super::{
        frontend::tests::native::{responder, CacheServer},
        tests::{eventually, Fixture},
    };
    use super::*;
    #[test]
    fn original_prepare_and_fresh_execute_survive_lost_replies_without_resubmission() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        tauri::async_runtime::block_on(async {
            // Real original CLI children/native cached evidence; fake actor
            // receipts. Root's separate real SQLite/files tests prove effects.
            let fixture =
                Fixture::new(json!({"ready":true,"cache":true,"frontend_uncertain":true}));
            let responder = responder();
            assert!(responder.configure_ownership_mode_once(
                zerus_client_core::desktop::ownership::Mode::Service
            ));
            let _cache = CacheServer::start(&fixture.0, responder.clone());
            let (service, _) = fixture.start_with_responder(true, Some(responder));
            eventually("cache component", || {
                service.snapshot().component_ready("cache_maintenance")
            })
            .await;
            let receipt = service
                .cache_pass(fixture.0.join("cache-root"), Some(1), Timing::Immediate)
                .await
                .unwrap_or_else(|error| {
                    panic!(
                        "{error}; bounded calls: {:?}",
                        fixture
                            .calls()
                            .into_iter()
                            .rev()
                            .take(12)
                            .collect::<Vec<_>>()
                    )
                })
                .unwrap();
            assert_eq!(receipt.phase, Phase::Settled);
            assert!(receipt.policy_saved);
            assert_eq!(receipt.removed, 1);
            assert!(service.cache_pending().is_none());
            service.join().await.unwrap();
            let calls = fixture.calls();
            let writes: Vec<_> = calls
                .iter()
                .filter(|v| v["args"] == json!(["desktop", "frontend-request", "--json"]))
                .collect();
            assert_eq!(
                writes.len(),
                2,
                "each original phase is submitted once despite both lost replies"
            );
            assert_eq!(writes[0]["value"]["request"]["command"]["phase"], "prepare");
            assert_eq!(writes[1]["value"]["request"]["command"]["phase"], "execute");
            assert_ne!(writes[0]["value"]["attempt"], writes[1]["value"]["attempt"]);
            assert!(fixture.0.join("original-child-bound").exists());
            assert!(calls
                .iter()
                .any(|v| v["value"]["op"] == "cache_maintenance_result"));
            assert!(!calls
                .iter()
                .any(|v| v["value"]["op"] == "cache_maintenance"));
        });
    }
}
