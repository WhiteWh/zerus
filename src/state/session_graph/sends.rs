//! Durable metadata for an observed Zerus send. The caller is proved against
//! its saved native process ancestry, never against an environment label alone.
//! This ledger cannot retry input and stores no prompt or attachment content.
use super::{model::Endpoint, *};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use std::io::{Read, Seek, Write};
use std::process::{Command, Stdio};

const MAX_INPUT: u64 = 29 * 1024 * 1024;
const MAX_RECEIPT: usize = 1024 * 1024;

fn machine() -> Option<String> {
    let config = crate::config::Config::load().ok()?;
    crate::swarm::local_node_id(&config).ok()
}

/// A verified native target is useful even when an older caller has no source
/// attribution. Do not add it to an old receipt on replay.
pub(crate) fn target_receipt(record: &Value) -> Option<Value> {
    target_receipt_for_machine(record, &machine()?)
}

/// The same target from an already verified, existing machine identity.
/// This factory never loads, initializes or saves a machine catalog.
pub(crate) fn target_receipt_for_machine(record: &Value, machine: &str) -> Option<Value> {
    let endpoint = Endpoint::from_record(machine, record)?;
    Some(json!({"schema":1,"node":endpoint.key(),"endpoint":endpoint,
        "name":record["name"],"run_id":record["run_id"]}))
}

pub(crate) fn send_origin() -> Option<Value> {
    target_receipt(&owned_record()?)
}

/// The live binding whose native agent is an actual ancestor of this CLI, or
/// whose agent window holds the Claude background job this CLI runs under.
/// Its name may be a rename shadow's: hooks and agents keep the environment
/// they started with.
fn owned_record() -> Option<Value> {
    token_record().or_else(super::super::continuations::background_session)
}

fn token_record() -> Option<Value> {
    let name = nonempty_env("HGS_SESSION")?;
    let run = nonempty_env("HGS_RUN_ID")?;
    let record = read_run(&name, &run).ok()??;
    if !process_alive(&record) || record["state"] == "archived" {
        return None;
    }
    // Ancestry through a recorded DeepSeek host proves no other agent's
    // session (zerus-ppd26.15).
    if crate::state::dsh::host_check() != crate::state::dsh::HostCheck::Outside {
        return None;
    }
    proven(record)
}

/// The record when this process belongs to its session: a descendant of its
/// owner, or (task #33) in that very session by the host's word.
fn proven(record: Value) -> Option<Value> {
    let owner = u32::try_from(record["pid"].as_u64()?).ok()?;
    if descends_from(owner) {
        return Some(record);
    }
    // Task #33: a Git Bash wrapper (`bash -c`, `env`, `timeout`, `xargs`, ...)
    // ends a Windows process between the session and this one, so the walk
    // stops at a dead PID. The session host then names the session whose job
    // holds this process, by the PID the kernel gives the host; the record the
    // environment names must be that very session and run. The environment
    // alone never proves it (H7): a leaked HGS_SESSION names another session.
    host_vouches(&record).then_some(record)
}

/// Whether this process is `owner` or descends from it through live
/// processes of this user.
fn descends_from(owner: u32) -> bool {
    // Task #6 (team ede724c4): on Windows `process_identity` snapshots every
    // process to read one parent, so a walk of eight ancestors took eight
    // snapshots on each call of a member's hgs. One table serves the whole
    // walk; each process is still verified (alive, this user, created before
    // the table, its parent older than it), as the process listing does.
    #[cfg(windows)]
    let listed = {
        let at = now();
        let Ok(table) = crate::platform::process_table() else { return false };
        (table, at)
    };
    let mut pid = std::process::id();
    for _ in 0..64 {
        #[cfg(windows)]
        let Some(process) = listed.0.get(&pid).and_then(|row| crate::platform::listed_identity(pid, row.parent, listed.1)) else { return false };
        #[cfg(not(windows))]
        let Some(process) = crate::platform::process_identity(pid) else { return false };
        if pid == owner {
            return true;
        }
        if process.parent == 0 || process.parent == pid {
            return false;
        }
        pid = process.parent;
    }
    false
}

/// Task #33: the host's answer for this process matches `record`: its name,
/// its pane and its run marker, which the launch set to the record's run.
fn host_vouches(record: &Value) -> bool {
    host_session().is_some_and(|found| vouched(record, &found))
}

fn vouched(record: &Value, found: &crate::host::protocol::CallerSession) -> bool {
    let run = string(record, "run_id");
    !run.is_empty()
        && found.run_marker == run
        && found.name == string(record, "name")
        && !found.pane.is_empty()
        && found.pane == string(record, "pane")
}

#[cfg(test)]
thread_local! {
    /// A test's session host answer; Some(None) is "outside every session".
    static HOST_SESSION: std::cell::RefCell<Option<Option<crate::host::protocol::CallerSession>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_test_host_session(found: Option<Option<crate::host::protocol::CallerSession>>) {
    HOST_SESSION.with(|value| *value.borrow_mut() = found);
}

fn host_session() -> Option<crate::host::protocol::CallerSession> {
    #[cfg(test)]
    if let Some(found) = HOST_SESSION.with(|value| value.borrow().clone()) {
        return found;
    }
    #[cfg(windows)]
    {
        super::super::backend::host::HOST.caller_session()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// The `HGS_AGENT` value a resident DeepSeek host's bridge sets on the
/// processes of every tool call, whether or not it also has that session's
/// own markers (zerus-ppd26).
pub(crate) const HOSTED_AGENT: &str = "dsh";

/// Whether this command runs in a resident host's tool call. Such a command
/// is an agent's even when no session can be proven for it (no identity file
/// yet, an agent the native UI opened, or a host this hgs cannot prove), so a
/// guard that would otherwise take an unmarked caller for the person refuses
/// it. Like every marker this guards against accidental use, not against an
/// agent that clears its own environment.
pub(crate) fn hosted_agent_call() -> bool {
    std::env::var_os("HGS_AGENT").is_some_and(|agent| agent == HOSTED_AGENT)
}

/// The calling agent's own binding under its current name, proven like a
/// send's origin.
pub(crate) fn caller_record() -> Option<Value> {
    let record = owned_record()?;
    if record["rename_shadow"] != true {
        return Some(record);
    }
    read_run(string(&record, "rename_target"), string(&record, "run_id"))
        .ok()?
        .filter(|current| current["rename_shadow"] != true)
}

fn path() -> PathBuf {
    root().join("graph/sends.sqlite3")
}

pub(super) fn open(read_only: bool) -> anyhow::Result<Connection> {
    let path = path();
    if let Ok(meta) = path.symlink_metadata() {
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "Nonregular graph send ledger"
        );
    }
    if !read_only {
        private_dir(path.parent().unwrap()).map_err(anyhow::Error::msg)?;
        if !path.exists() {
            crate::platform::private_open(fs::OpenOptions::new().write(true).create_new(true))
                .open(&path)?;
        }
    }
    let db = Connection::open_with_flags(
        &path,
        if read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        },
    )?;
    db.busy_timeout(Duration::from_millis(100))?;
    if !read_only {
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA max_page_count=8192;
            CREATE TABLE IF NOT EXISTS sends(id TEXT PRIMARY KEY,at REAL NOT NULL,payload TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS sends_at ON sends(at,id);")?;
    }
    Ok(db)
}

fn reserve(
    origin: &Value,
    route: &str,
    name: &str,
    input: &[u8],
) -> anyhow::Result<(String, Value)> {
    let request: Value = serde_json::from_slice(input)?;
    let request_id = string(&request, "request_id");
    ensure!(
        uuid::Uuid::parse_str(request_id)
            .is_ok_and(|id| !id.is_nil() && id.to_string() == request_id),
        "Invalid send request ID"
    );
    ensure!(
        name.len() <= 512 && !name.chars().any(char::is_control) && route.len() <= 256,
        "Invalid graph send scope"
    );
    let digest = format!("{:x}", Sha256::digest(input));
    let id = model::id("event", &json!([origin["node"], request_id]));
    let event = json!({"schema":1,"id":id,"source":origin,"route":route,"name":name,
        "request_id":request_id,"expected_run_id":request["expected_run_id"],
        "expected_conversation_id":request["expected_conversation_id"],
        "digest":digest,"at":now(),"status":"unknown"});
    let db = open(false)?;
    db.execute(
        "DELETE FROM sends WHERE at<?1",
        [now() - 30.0 * 24.0 * 3600.0],
    )?;
    if let Some(prior) = db
        .query_row("SELECT payload FROM sends WHERE id=?1", [&id], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
    {
        let prior: Value = serde_json::from_str(&prior)?;
        ensure!(
            prior["digest"] == digest && prior["source"] == *origin,
            "Graph send identity changed"
        );
        return Ok((id, prior));
    }
    db.execute(
        "INSERT INTO sends VALUES(?1,?2,?3)",
        params![id, now(), event.to_string()],
    )?;
    Ok((id, event))
}

fn confirm(id: &str, mut event: Value, bytes: &[u8]) -> anyhow::Result<()> {
    let receipt: Value = serde_json::from_slice(bytes)?;
    let target = &receipt["graph_target"];
    let endpoint: Endpoint = serde_json::from_value(target["endpoint"].clone())?;
    ensure!(
        endpoint.valid() && target["schema"] == 1 && target["node"] == endpoint.key(),
        "Unverified graph target"
    );
    ensure!(
        receipt["status"] == "submitted"
            && receipt["request_id"] == event["request_id"]
            && receipt["name"] == event["name"]
            && receipt["run_id"] == event["expected_run_id"]
            && receipt["conversation_id"] == event["expected_conversation_id"]
            && target["name"] == receipt["name"]
            && target["run_id"] == receipt["run_id"]
            && endpoint.conversation == string(&receipt, "conversation_id")
            && (endpoint.provisional_run.is_empty()
                || endpoint.provisional_run == string(&receipt, "run_id"))
            && string(&receipt, "agent_id").is_empty(),
        "Graph delivery receipt did not match"
    );
    event["target"] = target.clone();
    event["status"] = json!("submitted");
    // submitted_at is supplied by terminal/native receipt, not guessed from
    // inter-host clock ordering. The local observation time remains separate.
    event["submitted_at"] = receipt["submitted_at"].clone();
    open(false)?.execute(
        "UPDATE sends SET payload=?2,at=max(at+0.001,?3) WHERE id=?1",
        params![id, event.to_string(), now()],
    )?;
    Ok(())
}

/// Use the ordinary CLI in a child with attribution context cleared, preserving
/// every delivery guard and exact exit status. Only metadata capture is bounded;
/// stdout is streamed byte-for-byte, and errors retain the normal stderr route.
pub(crate) fn forward_send(route: &str, args: &[String], origin: Value) -> crate::cli::Result<i32> {
    let executable = crate::self_exec::current()?;
    let mut input = Vec::new();
    std::io::stdin()
        .take(MAX_INPUT + 1)
        .read_to_end(&mut input)?;
    if input.len() as u64 > MAX_INPUT {
        return Err(crate::cli::Error::new(1, "message payload exceeds 29 MiB"));
    }
    // The ledger digest and the receiving send's request identity both cover
    // the exact text delivered.
    // N34: a BOM is dropped before the envelope and the ledger digest.
    let input = enveloped(crate::state::without_bom(&input).to_vec());
    let reservation = match reserve(&origin, route, &args[0], &input) {
        Ok(value) => Some(value),
        Err(_) => {
            let marker = root().join("graph/recording-gap.json");
            if !marker.exists() {
                let _ = atomic(
                    &marker,
                    &json!({"since":now(),"reason":"metadata_recording_unavailable"}).to_string(),
                );
            }
            eprintln!(
                "hgs: Graph attribution could not be recorded; ordinary send guards remain active"
            );
            None
        }
    };
    let mut file = tempfile::tempfile()?;
    file.write_all(&input)?;
    file.rewind()?;
    let mut command = Command::new(executable);
    if !route.is_empty() {
        command.arg(format!("@{route}"));
    }
    command
        .arg("send")
        .args(args)
        .env_remove("HGS_SESSION")
        .env_remove("HGS_SESSION_ID")
        .env_remove("HGS_RUN_ID")
        .stdin(file)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = crate::platform::spawn(&mut command)?;
    let mut pipe = child.stdout.take().unwrap();
    let mut output = std::io::stdout().lock();
    let mut receipt = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let n = pipe.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        output.write_all(&chunk[..n])?;
        if receipt.len() <= MAX_RECEIPT {
            receipt.extend_from_slice(&chunk[..n]);
        }
    }
    output.flush()?;
    let status = child.wait()?;
    if status.success() && receipt.len() <= MAX_RECEIPT {
        if let Some((id, event)) = reservation {
            let _ = confirm(&id, event, &receipt);
        }
    }
    Ok(status.code().unwrap_or(1))
}

/// `input` with its text inside the calling agent's sender envelope. A payload
/// without text, or a caller whose reference cannot be proven, stays as it is.
fn enveloped(input: Vec<u8>) -> Vec<u8> {
    let Ok(mut request) = serde_json::from_slice::<Value>(&input) else {
        return input;
    };
    let Some(text) = request.get("text").and_then(Value::as_str) else {
        return input;
    };
    let Some(text) = crate::session_reference::sender_envelope(text) else {
        return input;
    };
    request["text"] = json!(text);
    serde_json::to_vec(&request).unwrap_or(input)
}

pub(super) fn source_ids(root: &Path) -> anyhow::Result<Vec<(String, String)>> {
    if !root.join("graph/sends.sqlite3").exists() {
        return Ok(Vec::new());
    }
    let db = open(true)?;
    let mut q = db.prepare("SELECT id,at FROM sends WHERE at>=?1 ORDER BY id LIMIT 10001")?;
    let rows = q
        .query_map([now() - 30.0 * 24.0 * 3600.0], |r| {
            Ok((r.get(0)?, format!("send-v1:{:.9}", r.get::<_, f64>(1)?)))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub(super) fn load(id: &str) -> anyhow::Result<Value> {
    Ok(serde_json::from_str(&open(true)?.query_row(
        "SELECT payload FROM sends WHERE id=?1",
        [id],
        |r| r.get::<_, String>(0),
    )?)?)
}

pub(super) fn coverage() -> anyhow::Result<Value> {
    let mut value = json!({"unknown_deliveries":0,"recording_gap":root().join("graph/recording-gap.json").exists()});
    if path().exists() {
        value["unknown_deliveries"]=json!(open(true)?.query_row("SELECT count(*) FROM sends WHERE at>=?1 AND json_extract(payload,'$.status')<>'submitted'",[now()-30.0*24.0*3600.0],|r|r.get::<_,i64>(0))?);
    }
    Ok(value)
}

#[cfg(test)]
mod caller_proof_tests {
    use super::*;
    use crate::host::protocol::CallerSession;

    fn record(owner: u32) -> Value {
        json!({"name":"claude/p/core","run_id":"830f440e-c015-40cf-9779-a7afa1e46bba","pane":"%7","pid":owner})
    }

    fn answer(name: &str, pane: &str, run: &str) -> CallerSession {
        CallerSession { session: "$7".into(), name: name.into(), pane: pane.into(), run_marker: run.into() }
    }

    fn with_host<T>(found: Option<CallerSession>, body: impl FnOnce() -> T) -> T {
        set_test_host_session(Some(found));
        let result = body();
        set_test_host_session(None);
        result
    }

    /// Task #6 (team ede724c4): the walk reads one process table and still
    /// verifies each ancestor: this process and its parent are reached, a PID
    /// no ancestor has is not.
    #[test]
    fn the_ancestry_walk_reaches_this_process_and_its_parent_only() {
        assert!(descends_from(std::process::id()));
        let parent = crate::platform::process_identity(std::process::id()).map(|process| process.parent).unwrap_or(0);
        if parent != 0 {
            assert!(descends_from(parent), "the parent {parent} is an ancestor");
        }
        assert!(!descends_from(u32::MAX - 7));
    }

    /// Task #33: a broken chain is proven only by the host naming this exact
    /// session, pane and run. A leaked HGS_SESSION (H7: the host names another
    /// session, or the same name under another run) and a process outside
    /// every session stay refused.
    #[test]
    fn only_the_hosts_word_for_this_very_session_proves_a_broken_chain() {
        // A PID that is no ancestor of this test: the chain does not reach it.
        let stranger = u32::MAX - 7;
        let run = "830f440e-c015-40cf-9779-a7afa1e46bba";
        assert!(with_host(Some(answer("claude/p/core", "%7", run)), || proven(record(stranger))).is_some());
        for found in [
            None,
            Some(answer("claude/p/other", "%9", "another-run")),
            Some(answer("claude/p/core", "%7", "an-older-run")),
            Some(answer("claude/p/core", "%8", run)),
            Some(answer("claude/p/core", "", run)),
        ] {
            assert!(with_host(found.clone(), || proven(record(stranger))).is_none(), "{found:?}");
        }
        // A record without a run never matches an empty marker.
        let mut bare = record(stranger);
        bare["run_id"] = json!("");
        assert!(with_host(Some(answer("claude/p/core", "%7", "")), || proven(bare)).is_none());
        // An intact chain needs no host at all.
        assert!(with_host(None, || proven(record(std::process::id()))).is_some());
    }

    /// The helper side of the test below: run through Git Bash's `env`, as the
    /// audit ran `env hgs run --list --json`.
    #[test]
    fn wrapped_chain_helper() {
        let Some(owner) = std::env::var("HGS_TEST_CHAIN_OWNER").ok().and_then(|owner| owner.parse::<u32>().ok()) else { return };
        let descends = descends_from(owner);
        let run = "830f440e-c015-40cf-9779-a7afa1e46bba";
        let proven = with_host(Some(answer("claude/p/core", "%7", run)), || proven(record(owner))).is_some();
        println!("descends={descends} proven={proven}");
    }

    /// Task #29 P1 / task #33: in a Claude session `env hgs run --list --json`
    /// (or bash -c, nohup, timeout, xargs, sh -c, npx -c) was refused as
    /// "run it from inside a Zerus session". Bash forks for `env`, and the
    /// fork's exec of a Cygwin program ends that Windows process, so the chain
    /// from hgs never reaches the session. Here the test process is the
    /// session's owner and Git Bash runs `env <helper>; true`: the chain breaks
    /// exactly so, and the host's answer proves the helper.
    #[cfg(windows)]
    #[test]
    fn a_process_started_through_git_bash_env_is_proven_by_the_host() {
        let Some(bash) = git_bash() else { return };
        let exe = std::env::current_exe().unwrap();
        let helper = "state::session_graph::sends::caller_proof_tests::wrapped_chain_helper";
        let run = |wrapped: bool| {
            let mut command = if wrapped {
                let exe = exe.display().to_string().replace('\\', "/");
                let mut command = Command::new(&bash);
                command.arg("-c").arg(format!("env '{exe}' {helper} --exact --nocapture --test-threads=1; true"));
                command
            } else {
                let mut command = Command::new(&exe);
                command.args([helper, "--exact", "--nocapture", "--test-threads=1"]);
                command
            };
            let output = command
                .env("HGS_TEST_CHAIN_OWNER", std::process::id().to_string())
                .stdin(Stdio::null())
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout).into_owned()
        };
        let direct = run(false);
        assert!(direct.contains("descends=true proven=true"), "{direct}");
        let wrapped = run(true);
        assert!(wrapped.contains("descends=false"), "the wrapper broke the chain as in the audit: {wrapped}");
        assert!(wrapped.contains("proven=true"), "{wrapped}");
    }

    /// Git for Windows' bash.exe, found next to git.
    #[cfg(windows)]
    fn git_bash() -> Option<std::path::PathBuf> {
        let output = Command::new("git").arg("--exec-path").output().ok()?;
        let exec = std::path::PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        // <git>/mingw64/libexec/git-core -> <git>/bin/bash.exe
        let bash = exec.parent()?.parent()?.parent()?.join("bin").join("bash.exe");
        bash.is_file().then_some(bash)
    }
}

#[cfg(test)]
mod receipt_tests {
    use super::*;
    #[test]
    fn existing_machine_factory_and_initializing_wrapper_keep_identical_shapes() {
        let home=tempfile::tempdir().unwrap();
        let output=std::process::Command::new(std::env::current_exe().unwrap())
            .env_clear().env("PATH",std::env::var_os("PATH").unwrap_or_default())
            .env("HOME",home.path()).env("HGS_CONFIG_DIR",home.path().join("config"))
            .env("HGS_STATE_DIR",home.path().join("state"))
            .args(["--exact","state::session_graph::sends::receipt_tests::private_factory_fixture","--ignored","--test-threads=1"])
            .stdin(std::process::Stdio::null()).output().unwrap();
        assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stdout));
    }
    #[test]
    #[ignore="private machine-initialization compatibility fixture"]
    fn private_factory_fixture() {
        let config=crate::config::Config::load().unwrap();
        let catalog=config.dir.join("swarm/catalog.json");
        let machine=uuid::Uuid::new_v4().to_string();
        let record=json!({"agent":"codex","run_id":"run","conversation_id":"cid","name":"codex/example/receipt"});
        let value=target_receipt_for_machine(&record,&machine).unwrap();
        let endpoint=Endpoint::from_record(&machine,&record).unwrap();
        assert_eq!(value,json!({"schema":1,"node":endpoint.key(),"endpoint":endpoint,"name":record["name"],"run_id":record["run_id"]}));
        assert!(!catalog.exists() && !root().join("graph").exists(),"read-only factory created state");
        let old=target_receipt(&record).unwrap();
        assert!(catalog.exists(),"ordinary wrapper stopped initializing machine identity");
        let bytes=fs::read(&catalog).unwrap();
        let persisted=crate::swarm::existing_node_id(&bytes).unwrap();
        assert_eq!(old,target_receipt_for_machine(&record,&persisted).unwrap());
        for agent in ["claude","codex","kimi","dsh","opencode"] {
            for cid in [Value::Null,json!(""),json!("cid")] {
                let mut record=record.clone();record["agent"]=json!(agent);record["conversation_id"]=cid;
                assert_eq!(target_receipt(&record),target_receipt_for_machine(&record,&persisted));
            }
        }
        assert_eq!(fs::read(catalog).unwrap(),bytes,"factory or stable wrapper changed the catalog");
        assert!(!root().join("graph").exists());
    }
}
