//! History-free hints for previously used, unmarked native conversations.
//! The shared service receives only plugin plumbing. Every model request reads
//! one decided binding under nonblocking writer locks; environment is no grant.
use super::*;

const PLUGIN: &str = "zerus.agent-hint.context";
const VERSION: (u64, u64, u64) = (2, 2, 7);
const REVISION: u64 = 1;

fn directory() -> PathBuf {
    root().join("opencode/context-hint")
}
fn service_path() -> PathBuf {
    directory().join("service.json")
}
fn index_path(conversation: &str) -> PathBuf {
    directory().join("bindings").join(format!(
        "{:x}.json",
        Sha256::digest(conversation.as_bytes())
    ))
}
fn read(path: &Path, bytes: u64) -> Option<Value> {
    let text = labels::read_file(path, bytes).ok()??;
    serde_json::from_slice(&text).ok()
}
fn birth(pid: u32) -> Option<String> {
    crate::platform::process_identity(pid).map(|identity| identity.birth)
}
fn existing_lock(path: &Path) -> Option<fs::File> {
    // A callback must not create state or run writer recovery. All launch
    // locks already exist; absent/inaccessible/contended means no context.
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .ok()?;
    fs2::FileExt::try_lock_exclusive(&file).ok()?;
    Some(file)
}

/// Prepared only on the existing cold-start path. A unique source directory
/// keeps an upgrade from hot-reloading a plugin inside an already live host.
pub(super) struct Overlay {
    nonce: String,
    pub content: String,
}
pub(super) fn overlay() -> Option<Overlay> {
    if !service::version_at_least(&catalog::installed_version()?, VERSION) {
        return None;
    }
    let nonce = uuid::Uuid::new_v4().to_string();
    let state = absolute_root().ok()?;
    let folder = state.join("opencode/context-hint/plugins").join(&nonce);
    atomic(&folder.join("index.js"), include_str!("context_plugin.js")).ok()?;
    atomic(&folder.join("package.json"), r#"{"type":"module","name":"zerus-agent-hint-context","exports":{"./server":"./index.js"}}"#).ok()?;
    let content = json!({"plugins":[{"package":folder,"options":{
        "hgs":std::env::current_exe().ok()?,"state":state,"nonce":nonce
    }}]})
    .to_string();
    Some(Overlay { nonce, content })
}
pub(super) fn started(service: &service::Service, overlay: &Overlay) {
    if let Some(birth) = birth(service.reg.pid) {
        let _ = atomic(
            &service_path(),
            &json!({"nonce":overlay.nonce,"pid":service.reg.pid,
            "birth":birth,"id":service.reg.id,"home":config_home(),
            "registration":service::registration_path()})
            .to_string(),
        );
    }
}
fn owned(service: &service::Registration, marker: &Value) -> bool {
    marker["id"] == service.id
        && marker["pid"] == service.pid
        && marker["home"] == config_home().to_string_lossy().as_ref()
        && marker["registration"] == service::registration_path().to_string_lossy().as_ref()
        && birth(service.pid).is_some_and(|b| marker["birth"] == b)
        && marker["nonce"]
            .as_str()
            .is_some_and(|n| uuid::Uuid::parse_str(n).is_ok())
}
fn ready(service: &service::Service, marker: &Value, cwd: &str) -> bool {
    let query = format!(
        "/api/plugin?location%5Bdirectory%5D={}",
        catalog::encode(cwd)
    );
    let expected = directory().join("plugins").join(string(marker, "nonce"));
    // A cold service announces its HTTP endpoint before a location finishes
    // activating plugins. Observe the exact active source within one bounded
    // readiness window; never restart a host or treat a partial list as ready.
    let deadline = Instant::now() + Duration::from_secs(2);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        if let Ok(v) = service.call_within(
            "GET",
            &query,
            None,
            remaining.min(Duration::from_millis(250)),
        ) {
            if let Some(plugin) = v["data"].as_array().and_then(|plugins| {
                plugins.iter().find(|p| {
                    p["id"] == PLUGIN
                        && p["source"]["type"] == "local"
                        && p["source"]["path"] == expected.to_string_lossy().as_ref()
                })
            }) {
                if plugin["state"]["status"] == "active" && plugin["features"]["server"] == true {
                    return true;
                }
                if plugin["state"]["status"] == "failed" {
                    return false;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(25).min(remaining));
    }
    false
}

/// This index is a fixed route to one authoritative record, never an authority
/// of its own. A failed/missing update leaves a stale digest and omits the hint.
pub(super) fn note(record: &Value) {
    if !native_id(string(record, "conversation_id"), "ses_")
        || !record["agent_hint_context"].is_object()
    {
        return;
    }
    let path = index_path(string(record, "conversation_id"));
    let previous = read(&path, 16384);
    // Two decided bindings must not silently replace each other's policy for
    // one native conversation. Retain ambiguity rather than choose a winner.
    let ambiguous = previous.as_ref().map_or_else(
        || {
            fs::symlink_metadata(&path)
                .map_or_else(|e| e.kind() != std::io::ErrorKind::NotFound, |_| true)
        },
        |old| old["ambiguous"] == true || old["session_id"] != record["session_id"],
    );
    let route = json!({"name":record["name"],"session_id":record["session_id"],"ambiguous":ambiguous,
        "id_layout":storage::in_id_layout(record),
        "digest":format!("{:x}",Sha256::digest(record.to_string().as_bytes()))});
    let _ = atomic(&path, &route.to_string());
}

/// Called only after the normal launch/resume verified the exact native
/// session under the writer and provider locks. Never migrate retained entries.
pub(super) fn configure(service: &service::Service, record: &mut Value) {
    if record.get("agent_hint").is_some() {
        return;
    }
    let enabled = agent_hint::for_account("native-opencode").is_some();
    let marker = read(&service_path(), 16384);
    let mut mark = json!({"revision":REVISION,"enabled":false,"state":"off"});
    if enabled {
        let ready = marker
            .as_ref()
            .filter(|m| owned(&service.reg, m))
            .filter(|m| ready(service, m, string(record, "cwd")));
        if let Some(marker) = ready {
            mark = json!({"revision":REVISION,"enabled":true,"state":"ready",
                "run_id":record["run_id"],"conversation_id":record["conversation_id"],"cwd":record["cwd"],
                "service_id":service.reg.id,"nonce":marker["nonce"]});
        } else {
            mark["state"] = json!("unavailable");
            mark["reason"] = json!("This OpenCode service has no ready Zerus context plugin; the optional coordination hint is omitted");
            eprintln!("hgs: {}", mark["reason"].as_str().unwrap());
        }
    }
    record["agent_hint_context"] = mark;
    let _ = save(record);
}

fn admitted(record: &Value, marker: &Value, nonce: &str, cwd: &str, conversation: &str) -> bool {
    let m = &record["agent_hint_context"];
    record["backend"] == "opencode"
        && record["agent"] == "opencode"
        && record["service"] == "opencode2"
        && record.get("agent_hint").is_none()
        && (record["creation_pending"].is_null() || record["creation_pending"] == false)
        && (record["archive_id"].is_null() || record["archive_id"] == "")
        && (record["rename_shadow"].is_null() || record["rename_shadow"] == false)
        && !string(record, "run_id").is_empty()
        && record["conversation_id"] == conversation
        && record["cwd"] == cwd
        && record["agent_home"] == config_home().to_string_lossy().as_ref()
        && m["revision"] == REVISION
        && m["enabled"] == true
        && m["state"] == "ready"
        && m["run_id"] == record["run_id"]
        && m["conversation_id"] == conversation
        && m["cwd"] == cwd
        && m["service_id"] == marker["id"]
        && m["nonce"] == nonce
}

/// Internal bounded read-only helper. Called before ordinary CLI/config/job
/// dispatch. Any failure, stale record or contended lock emits no context.
pub(crate) fn lookup(args: &[String]) -> Option<&'static str> {
    let [nonce, pid, cwd, conversation] = args else {
        return None;
    };
    if pid.len() > 10
        || nonce.len() > 64
        || uuid::Uuid::parse_str(nonce).is_err()
        || cwd.len() > 16384
        || !Path::new(cwd).is_absolute()
        || !native_id(conversation, "ses_")
    {
        return None;
    }
    let pid = pid.parse::<u32>().ok()?;
    let _writer = existing_lock(&root().join(".lock"))?;
    let _bindings = existing_lock(&bindings_lock())?;
    let marker = read(&service_path(), 16384)?;
    let service::RegistrationRead::Present(reg) =
        service::read_registration_at(&service::registration_path())
    else {
        return None;
    };
    if reg.pid != pid || marker["nonce"] != *nonce || !owned(&reg, &marker) {
        return None;
    }
    let route = read(&index_path(conversation), 16384)?;
    if route["ambiguous"] != false {
        return None;
    }
    let record = if route["id_layout"] == true {
        PROVIDER.read_id(string(&route, "session_id")).ok()?
    } else if route["id_layout"] == false {
        PROVIDER.read_label(string(&route, "name")).ok()?
    } else {
        return None;
    };
    if route["digest"] != format!("{:x}", Sha256::digest(record.to_string().as_bytes()))
        || !admitted(&record, &marker, nonce, cwd, conversation)
    {
        return None;
    }
    // Exclude native children even if somebody adopted one as a main binding.
    let service = service::Service { reg };
    let native = service
        .call_within(
            "GET",
            &session_path(&record, ""),
            None,
            Duration::from_millis(250),
        )
        .ok()?;
    let native = exact_session(&record, native).ok()?;
    if !native["parentID"].is_null() {
        return None;
    }
    // A provider move or an old run's retained native metadata cannot grant a
    // different current run. Migrated sessions may have no Zerus metadata.
    if !native["metadata"]["zerus"]["run_id"].is_null()
        && native["metadata"]["zerus"]["run_id"] != record["run_id"]
    {
        return None;
    }
    birth(pid).filter(|b| marker["birth"] == *b)?;
    Some(agent_hint::HINT)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_decided_context_binding_excludes_stale_off_and_entry_backed_records() {
        let marker = json!({"id":"service"});
        let record = json!({"backend":"opencode","agent":"opencode","service":"opencode2",
            "run_id":"run","conversation_id":"ses_A","cwd":"/example","agent_home":config_home(),
            "agent_hint_context":{"revision":1,"enabled":true,"state":"ready","run_id":"run",
                "conversation_id":"ses_A","cwd":"/example","service_id":"service","nonce":"nonce"}});
        assert!(admitted(&record, &marker, "nonce", "/example", "ses_A"));
        for (field, value) in [
            ("agent_hint", json!({"revision":1})),
            ("creation_pending", json!(true)),
            ("creation_pending", json!("invalid")),
            ("run_id", json!("other")),
            ("cwd", json!("/other")),
            ("conversation_id", json!("ses_other")),
            ("service", json!("other")),
            ("archive_id", json!("archived")),
            ("archive_id", json!(1)),
            ("rename_shadow", json!(true)),
            ("rename_shadow", json!("invalid")),
        ] {
            let mut bad = record.clone();
            bad[field] = value;
            assert!(
                !admitted(&bad, &marker, "nonce", "/example", "ses_A"),
                "{field}"
            );
        }
        for (field, value) in [
            ("enabled", json!(false)),
            ("revision", json!(2)),
            ("state", json!("unavailable")),
            ("run_id", json!("old")),
            ("service_id", json!("other")),
            ("nonce", json!("other")),
        ] {
            let mut bad = record.clone();
            bad["agent_hint_context"][field] = value;
            assert!(
                !admitted(&bad, &marker, "nonce", "/example", "ses_A"),
                "{field}"
            );
        }
    }
}
