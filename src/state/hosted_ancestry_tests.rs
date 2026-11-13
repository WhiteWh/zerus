//! zerus-ppd26.15: a command under a recorded resident DeepSeek host is never
//! proven as another agent's session by process ancestry, even when that
//! agent's recorded process is a live ancestor of the host.
//!
//! The tree is the probe's: the fixture process is the claimed Claude agent;
//! it starts a stand-in host (another isolated test process) whose PID and
//! birth it records in `dsh/hosts/<key>/host.identity.json`; the host then
//! starts the command, which carries `HGS_AGENT=dsh` and the Claude
//! session's own markers, or Claude's background-job variables. The person
//! case carries neither and no `HGS_AGENT`: the process tree and the host
//! records decide, never a marker. Private roots only; every inherited
//! variable is cleared.
use super::*;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

const LEAD: &str = "claude/example/lead";
const LEAD_RUN: &str = "run-lead";
const WINDOW: &str = "claude/example/window";
const JOB: &str = "job-probe";
const MODULE: &str = "state::hosted_ancestry_tests::";

fn isolated(test: &str, root: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("HGS_STATE_DIR", root.join("state"))
        .env("HGS_CONFIG_DIR", root.join("config"))
        .env("TMUX_TMPDIR", root.join("tmux"))
        .env("CLAUDE_CONFIG_DIR", root.join("claude"))
        .env("PROBE_ROOT", root)
        .args([
            "--exact",
            &format!("{MODULE}{test}"),
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .stdin(Stdio::null());
    command
}

fn probe_line(output: &[u8]) -> Option<Value> {
    String::from_utf8_lossy(output)
        .lines()
        .find_map(|line| line.split_once("PROBE ").map(|(_, found)| found))
        .and_then(|line| serde_json::from_str(line).ok())
}

/// One case: the fixture's own result line.
fn run_case(case: &str, mode: &str) -> Value {
    let temp = tempfile::tempdir().unwrap();
    for folder in ["home", "state", "config", "tmux", "claude/sessions"] {
        fs::create_dir_all(temp.path().join(folder)).unwrap();
    }
    let output = isolated("claim_fixture", temp.path())
        .env("PROBE_CASE", case)
        .env("PROBE_MODE", mode)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fixture {case}/{mode} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    probe_line(&output.stdout).unwrap_or_else(|| {
        panic!(
            "no probe line for {case}/{mode}:\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn a_claim_under_a_recorded_host_is_never_another_agents_session() {
    // The recorded host refuses every ancestry claim: the markers' route and
    // Claude's background-job route, for the send origin (sends, wait begin),
    // the caller record (tasks, question routing, self, parent adopt) and the
    // parent links of a launch, local and forwarded.
    for mode in ["markers", "background"] {
        let found = run_case("recorded", mode);
        assert_eq!(
            found["caller_record"],
            Value::Null,
            "{mode}: a command under a recorded DeepSeek host was proven as {found}"
        );
        assert_eq!(found["send_origin"], Value::Null, "{mode}: {found}");
        assert_eq!(found["forwarded_parent"], false, "{mode}: {found}");
        assert_eq!(found["launcher_parent"], false, "{mode}: {found}");
        assert_eq!(
            found["host_refusal"],
            json!(crate::state::dsh::UNDER_HOST_FOR_TESTS),
            "{mode}: {found}"
        );
        assert_eq!(
            found["agent_refusal"],
            json!({"ok":false,"error":"agent_refused"}),
            "{mode}: {found}"
        );
    }
}

#[test]
fn an_unmarked_command_under_a_recorded_host_is_never_the_person() {
    let found = run_case("recorded", "person");
    assert_eq!(
        found["host_refusal"],
        json!(crate::state::dsh::UNDER_HOST_FOR_TESTS),
        "{found}"
    );
    assert_eq!(
        found["agent_refusal"],
        json!({"ok":false,"error":"agent_refused"}),
        "{found}"
    );
    for case in ["unrecorded", "other_birth", "other_boot"] {
        let found = run_case(case, "person");
        assert_eq!(found["host_refusal"], Value::Null, "{case}: {found}");
        assert_eq!(found["agent_refusal"], Value::Null, "{case}: {found}");
    }
}

#[test]
fn the_same_tree_without_a_recorded_host_keeps_its_proof() {
    // The control: the very same tree proves the agent when no host is
    // recorded, when the record names an exited host (another birth), and
    // when the record is from another boot.
    for case in ["unrecorded", "other_birth", "other_boot"] {
        for mode in ["markers", "background"] {
            let found = run_case(case, mode);
            let expected = if mode == "markers" { LEAD } else { WINDOW };
            assert_eq!(
                found["caller_record"],
                json!(expected),
                "{case}/{mode}: {found}"
            );
            assert!(found["send_origin"].is_object(), "{case}/{mode}: {found}");
        }
    }
}

#[test]
fn an_unreadable_host_record_fails_closed() {
    // In both directions, and the refusal names the record and how to clear it.
    for mode in ["markers", "background", "person"] {
        let found = run_case("corrupt", mode);
        assert_eq!(found["caller_record"], Value::Null, "{mode}: {found}");
        assert_eq!(found["send_origin"], Value::Null, "{mode}: {found}");
        let record = found["record"].as_str().unwrap().to_owned();
        let sentence = found["host_refusal"].as_str().unwrap_or_default();
        assert!(
            sentence.contains(&format!("remove {record}.")),
            "{mode}: {found}"
        );
        if mode == "person" {
            assert_eq!(
                found["agent_refusal"]["error"], "host_unverified",
                "{found}"
            );
            assert_eq!(
                found["agent_refusal"]["message"],
                json!(sentence),
                "{found}"
            );
        } else {
            // The HGS_AGENT=dsh marker refuses first, as an agent's call.
            assert_eq!(
                found["agent_refusal"],
                json!({"ok":false,"error":"agent_refused"}),
                "{mode}: {found}"
            );
        }
    }
}

fn birth(pid: u32) -> String {
    crate::platform::process_identity(pid).unwrap().birth
}

fn agent(name: &str, run: &str, pid: u32, conversation: &str) {
    let mut record = json!({"version":1,"name":name,"agent":"claude","run_id":run,
        "conversation_id":conversation,"pid":pid,
        "process_start":super::storage::process_start(pid),"activity":"idle","phase":"idle",
        "active_tools":{},"subagents":{},"created":now()});
    super::storage::write(&mut record).unwrap();
}

fn registry(pid: u32, entry: Value) {
    let root = PathBuf::from(std::env::var_os("PROBE_ROOT").unwrap());
    let mut entry = entry;
    entry["pid"] = json!(pid);
    fs::write(
        root.join("claude/sessions").join(format!("{pid}.json")),
        entry.to_string(),
    )
    .unwrap();
}

fn host_record(case: &str, host: u32) -> PathBuf {
    let dir = root()
        .join("dsh/hosts")
        .join(&format!("{:x}", Sha256::digest(b"/agent/home"))[..16]);
    fs::create_dir_all(&dir).unwrap();
    let text = match case {
        "recorded" => json!({"state":"running","version":1,"pid":host,"birth":birth(host),
            "pid_file":[0,0,0,0]})
        .to_string(),
        "other_birth" => {
            let other = (birth(host).parse::<u64>().unwrap_or(2) - 1).max(1);
            json!({"state":"running","version":1,"pid":host,"birth":other.to_string(),
                "pid_file":[0,0,0,0]})
            .to_string()
        }
        "other_boot" => json!({"state":"running","version":2,"pid":host,"birth":birth(host),
            "pid_file":[0,0,0,0],"boot":uuid::Uuid::new_v4().to_string()})
        .to_string(),
        "corrupt" => "{".to_owned(),
        _ => return dir,
    };
    fs::write(dir.join("host.identity.json"), text).unwrap();
    fs::canonicalize(dir.join("host.identity.json")).unwrap()
}

#[test]
#[ignore = "isolated ppd26.15 probe fixture: the claimed agent"]
fn claim_fixture() {
    let me = std::process::id();
    let above = crate::platform::process_identity(me).unwrap().parent;
    let window_conversation = uuid::Uuid::new_v4().to_string();
    // The markers' route: this process is the lead's agent.
    agent(LEAD, LEAD_RUN, me, &uuid::Uuid::new_v4().to_string());
    // Claude's background route: the window (the test harness above) parks
    // the job that this process runs as Claude's background service.
    agent(WINDOW, "run-window", above, &window_conversation);
    registry(above, json!({"parkedJobId":JOB}));
    registry(me, json!({"kind":"bg","jobId":JOB}));
    let root = PathBuf::from(std::env::var_os("PROBE_ROOT").unwrap());
    let mode = std::env::var("PROBE_MODE").unwrap();
    let mut host = isolated("host_fixture", &root);
    host.env("PROBE_MODE", &mode)
        .env("PROBE_WINDOW_CONVERSATION", &window_conversation)
        .env("PROBE_CLAUDE_PID", me.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = host.spawn().unwrap();
    let record = host_record(&std::env::var("PROBE_CASE").unwrap(), child.id());
    child.stdin.take().unwrap().write_all(b"go\n").unwrap();
    let mut output = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut output)
        .unwrap();
    assert!(child.wait().unwrap().success());
    let mut found = probe_line(&output).expect("no command result");
    found["record"] = json!(record);
    println!("PROBE {found}");
}

#[test]
#[ignore = "isolated ppd26.15 probe fixture: the stand-in DeepSeek host"]
fn host_fixture() {
    let mut line = String::new();
    BufReader::new(std::io::stdin())
        .read_line(&mut line)
        .unwrap();
    let root = PathBuf::from(std::env::var_os("PROBE_ROOT").unwrap());
    let mut command = isolated("command_fixture", &root);
    let mode = std::env::var("PROBE_MODE").unwrap();
    if mode != "person" {
        command.env("HGS_AGENT", "dsh");
    }
    if mode == "markers" {
        command.env("HGS_SESSION", LEAD).env("HGS_RUN_ID", LEAD_RUN);
    } else if mode == "background" {
        command
            .env(
                "CLAUDE_CODE_SESSION_ID",
                std::env::var("PROBE_WINDOW_CONVERSATION").unwrap(),
            )
            .env("CLAUDE_PID", std::env::var("PROBE_CLAUDE_PID").unwrap());
    }
    let output = command.output().unwrap();
    std::io::stdout().write_all(&output.stdout).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "isolated ppd26.15 probe fixture: the hosted command"]
fn command_fixture() {
    let caller = session_graph::caller_record().map(|record| record["name"].clone());
    let origin = session_graph::send_origin();
    let forwarded = parent_link::forwarded_argument().is_some();
    let launcher = !parent_link::launcher_value().unwrap_or_default().is_empty();
    let found = json!({"caller_record":caller,"send_origin":origin,
        "forwarded_parent":forwarded,"launcher_parent":launcher,
        "host_refusal":dsh::host_refusal(),"agent_refusal":question_routing::agent_refusal()});
    println!("PROBE {found}");
}
