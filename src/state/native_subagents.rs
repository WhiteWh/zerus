//! Provider-specific child identity and turn evidence. Native catalogs establish
//! membership; only exact typed turn boundaries reconcile missing hooks.
use super::*;
use rusqlite::OptionalExtension;
#[cfg(test)]
use rusqlite::Connection;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};

const TAIL_BYTES: u64 = 128 * 1024;

fn tail(path: &Path) -> Option<Vec<Value>> {
    let mut file = File::open(path).ok()?;
    let offset = file.metadata().ok()?.len().saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut bytes = Vec::new();
    file.take(TAIL_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > TAIL_BYTES || !bytes.ends_with(b"\n") { return None; }
    let mut rows = Vec::new();
    for (index, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        if offset > 0 && index == 0 { continue; }
        if line.iter().all(|b| b.is_ascii_whitespace()) { continue; }
        rows.push(serde_json::from_slice(line).ok()?);
    }
    Some(rows)
}

fn apply(record: &mut Value, id: &str, evidence: &Value, source: &str) -> bool {
    let child = &mut record["subagents"][id];
    let Some(updated) = child["updated"].as_f64() else { return false; };
    let Some(at) = evidence["at"].as_f64().filter(|at| *at > updated) else { return false; };
    if at <= child["started"].as_f64().unwrap_or(0.0) { return false; }
    let working = evidence["state"] == "working";
    if !working || child["state"] != "working" {
        child["current_tool"] = json!("");
        child["detail"] = evidence["detail"].clone();
    }
    child["state"] = evidence["state"].clone();
    child["updated"] = json!(at);
    if working { child.as_object_mut().unwrap().remove("completion_source"); }
    else { child["completion_source"] = json!(source); }
    child["tracking_evidence"] = json!({"source":source,"kind":evidence["kind"],"at":at,"native_id":evidence["native_id"]});
    for key in ["display_state", "attention_id"] { child.as_object_mut().unwrap().remove(key); }
    if evidence["error"] == true { child["display_state"] = json!("error"); }
    // Preserve existing reply/read identities, including on interrupted turns.
    true
}

fn settle(record: &mut Value) {
    journal::settle_children(record);
    if record["main_done"] == true && ["idle", "working"].contains(&string(record,"phase"))
        && record["subagents"].as_object().is_some_and(|children| children.values().any(|child| telemetry::child_working(record,child))) {
        record["activity"] = json!("busy");
        record["phase"] = json!("working");
    }
}

fn codex_turn(path: &Path, home: &Path, id: &str, parent: &str, external: bool) -> Option<Value> {
    let path = crate::platform::canonical(path).ok()?;
    if !path.starts_with(crate::platform::canonical(home).ok()?) { return None; }
    let key = format!("{parent}/{id}/{external}");
    scan_cache::cached("codex-child-turn-v2", &path, &key, || {
        let file = File::open(&path).ok()?;
        let mut first = String::new();
        BufReader::new(file.take(256 * 1024)).read_line(&mut first).ok()?;
        let header: Value = serde_json::from_str(&first).ok()?;
        let meta = &header["payload"];
        if header["type"] != "session_meta" || meta["id"] != id { return None; }
        if !external {
            let parents: Vec<_> = [meta.get("parent_thread_id"), meta.get("forked_from_id"),
                meta["source"]["subagent"]["thread_spawn"].get("parent_thread_id")]
                .into_iter().flatten().filter_map(Value::as_str).filter(|p| !p.is_empty()).collect();
            if parents.is_empty() || parents.iter().any(|p| *p != parent) { return None; }
        }
        let mut latest = Value::Null;
        for row in tail(&path)? {
            if row["type"] != "event_msg" { continue; }
            let event = &row["payload"];
            let kind = string(event,"type");
            if !["task_started", "turn_started", "task_complete", "turn_complete", "turn_aborted"].contains(&kind) { continue; }
            let turn = if kind == "turn_aborted" && string(event,"turn_id").is_empty() && latest["state"] == "working" {
                string(&latest,"native_id")
            } else { string(event,"turn_id") }.to_owned();
            let at = search::timestamp(&row["timestamp"]);
            if turn.is_empty() || turn.len() > 160 || turn.chars().any(char::is_control) || at <= 0.0 { return None; }
            // Conflicting ordering never lets an older completion end later work.
            if latest["at"].as_f64().is_some_and(|prior| at < prior) { return None; }
            if ["task_started","turn_started"].contains(&kind) {
                latest = json!({"state":"working","kind":kind,"at":at,"native_id":turn,"detail":"Native child turn started"});
            } else if latest["state"] == "working" && latest["native_id"] != turn {
                // A late terminal event from a different turn cannot end this one.
                continue;
            } else if kind == "turn_aborted" && event["reason"] == "interrupted" {
                latest = json!({"state":"finished","kind":kind,"at":at,"native_id":turn,"detail":"Native child turn interrupted"});
            } else if kind == "turn_aborted" {
                return Some(Value::Null);
            } else if event["error"].is_object() {
                latest = json!({"state":"finished","error":true,"kind":kind,"at":at,"native_id":turn,
                    "detail":journal::clipped(&event["error"]["message"],240)});
            } else if event["error"].is_null() && !string(event,"last_agent_message").trim().is_empty() {
                latest = json!({"state":"finished","kind":kind,"at":at,"native_id":turn,
                    "detail":journal::clipped(&event["last_agent_message"],240)});
            } else { return Some(Value::Null); }
        }
        Some(latest)
    }).filter(Value::is_object)
}

fn codex(record: &mut Value) -> bool {
    let mut changed = false;
    let parent = string(record, "conversation_id").to_owned();
    let own = string(record, "agent") == "codex";
    let children: Vec<_> = record["subagents"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, c)| {
            ["working", "finished"].contains(&string(c, "state"))
                && ((own && c["external"] != true)
                    || (c["external"] == true && c["provider"] == "codex"))
        })
        .map(|(id, c)| (id.clone(), c.clone()))
        .collect();
    if children.is_empty() {
        return false;
    }
    let home = if string(record, "agent_home").is_empty() {
        home().join(".codex")
    } else {
        PathBuf::from(string(record, "agent_home"))
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let picker = own && effort::picker_entered();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let picker = false;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let snapshot = if picker {
        let until = match effort::picker_deadline(
            std::time::Instant::now() + std::time::Duration::from_secs(2),
        ) {
            Ok(until) => until,
            Err(_) => {
                let _ = effort::picker_refuse::<()>(
                    "ID Effort cannot verify this Codex child catalog; use Terminal".into(),
                );
                return false;
            }
        };
        let ids: Vec<String> = children
            .iter()
            .filter(|(_, child)| child["external"] != true)
            .map(|(id, _)| id.clone())
            .collect();
        match codex_sqlite::rollout_paths(&home, &ids, until) {
            Ok(paths) => Some(paths),
            Err(_) => {
                let _ = effort::picker_refuse::<()>(
                    "ID Effort cannot verify this Codex child catalog; use Terminal".into(),
                );
                return false;
            }
        }
    } else {
        None
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let snapshot: Option<std::collections::BTreeMap<String, String>> = None;
    let database = if own && !picker {
        codex_sqlite::NativeConnection::open(home.join("state_5.sqlite")).ok()
    } else {
        None
    };
    if let Some(db) = &database {
        let _ = db.busy_timeout(std::time::Duration::from_millis(20));
    }
    for (id, child) in children {
        let external = child["external"] == true;
        let native_id = if external {
            string(&child, "conversation_id")
        } else {
            &id
        };
        if uuid::Uuid::parse_str(native_id).is_err() {
            continue;
        }
        let (path, native_home) = if external {
            (
                PathBuf::from(string(&child, "transcript")),
                PathBuf::from(string(&child, "agent_home")),
            )
        } else if let Some(paths) = &snapshot {
            let Some(path) = paths.get(native_id) else {
                continue;
            };
            (PathBuf::from(path), home.clone())
        } else {
            let Some(db) = &database else {
                continue;
            };
            let Ok(Some(path)) = db.query_row(
                "SELECT rollout_path FROM threads WHERE id=?1",
                [native_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            else {
                continue;
            };
            (PathBuf::from(path), home.clone())
        };
        if let Some(evidence) = codex_turn(&path, &native_home, native_id, &parent, external) {
            changed |= apply(record, &id, &evidence, "codex_native_rollout");
        }
    }
    changed
}

fn kimi_turn(path: &Path, id: &str) -> Option<Value> {
    scan_cache::cached("kimi-child-turn-v2", path, id, || {
        let mut latest = Value::Null;
        for row in tail(path)? {
            if !["", id].contains(&string(&row,"agentId")) { return None; }
            let kind = string(&row,"type");
            if !["agent.turn.started", "agent.turn.ended", "turn.ended"].contains(&kind) { continue; }
            let at = row["time"].as_f64()? / 1000.0;
            if at <= 0.0 || latest["at"].as_f64().is_some_and(|prior| at < prior) { return None; }
            let turn = row.get("turnId").filter(|id| id.is_string() || id.is_u64())?;
            let reason = if kind == "agent.turn.ended" { string(&row,"outcome") } else { string(&row,"reason") };
            if kind == "agent.turn.started" {
                latest = json!({"state":"working","kind":kind,"at":at,"native_id":turn,"detail":"Native child turn started"});
            } else if latest["state"] == "working" && latest["native_id"] != *turn {
                continue;
            } else if ["done", "completed", "aborted", "interrupted", "cancelled"].contains(&reason) {
                latest = json!({"state":"finished","kind":kind,"at":at,"native_id":turn,
                    "detail":if ["aborted","interrupted","cancelled"].contains(&reason) {"Native child turn interrupted"} else {"Native child turn completed"}});
            } else { return Some(Value::Null); }
        }
        Some(latest)
    }).filter(Value::is_object)
}

fn kimi(record: &mut Value) -> bool {
    if string(record,"agent") != "kimi" { return false; }
    let previous_native = record["kimi_native_roster"] == true;
    record.as_object_mut().unwrap().remove("kimi_native_roster");
    if previous_native { record["subagent_groups_complete"] = json!(false); }
    let Ok(paths) = questions::wire_paths(record) else { return false; };
    let [main] = paths.as_slice() else { return false; };
    let Some(session) = main.parent().and_then(Path::parent).and_then(Path::parent) else { return false; };
    let path = session.join("state.json");
    if !crate::platform::canonical(&path).is_ok_and(|path| path.starts_with(session)) { return false; }
    if !fs::metadata(&path).is_ok_and(|m| m.len() <= 1024 * 1024) { return false; }
    let Ok(text) = fs::read_to_string(&path) else { return false; };
    let Ok(state) = serde_json::from_str::<Value>(&text) else { return false; };
    if state["id"] != record["conversation_id"] || state["version"] != 2 { return false; }
    let catalog_at = state["updatedAt"].as_f64().unwrap_or(0.0) / 1000.0;
    // A delayed catalog must not negate a newer positive profile-hook count.
    if record["subagent_groups"].as_object().into_iter().flatten().any(|(_, group)|
        group["active_count"].as_u64().unwrap_or(0) > 0 && group["updated"].as_f64().unwrap_or(0.0) > catalog_at) {
        return false;
    }
    let Some(agents) = state["agents"].as_object() else { return false; };
    if agents.len() > 65 || !agents.contains_key("main") { return false; }
    let mut complete = true;
    let mut changed = false;
    for (id, native) in agents {
        if id == "main" || native["parentAgentId"] != "main" { continue; }
        if !id.starts_with("agent-") || id.len() > 160 || id.contains(['/', '\\']) { complete = false; continue; }
        let expected = session.join("agents").join(id);
        let (Ok(expected),Ok(actual)) = (crate::platform::canonical(&expected),crate::platform::canonical(string(native,"homedir"))) else { complete = false; continue; };
        if expected != actual || !actual.starts_with(session) { complete = false; continue; }
        let wire = actual.join("wire.jsonl");
        // Rows may omit agentId because this is the child's own wire. A file
        // symlink therefore cannot borrow another child's identity-free rows.
        let evidence = crate::platform::canonical(&wire).ok().filter(|path| *path == wire)
            .and_then(|path| kimi_turn(&path,id));
        if !record["subagents"].is_object() { record["subagents"] = json!({}); }
        if record["subagents"].get(id).is_none() {
            let name = string(&native["labels"],"profileName");
            record["subagents"][id] = json!({"name":if name.is_empty(){id.as_str()}else{name},"state":"unknown", "updated":0.0,"started":0.0,
                "native_roster":true,"tracking_evidence":{"source":"kimi_session_state","kind":"catalog_membership"}});
        }
        record["subagents"][id]["native_roster"] = json!(true);
        if let Some(evidence) = evidence { changed |= apply(record,id,&evidence,"kimi_native_wire"); }
    }
    if complete {
        record["kimi_native_roster"] = json!(true);
        // A removed native identity loses current-work authority; its result is
        // still unknown and its recorded history remains inspectable.
        for (id, child) in record["subagents"].as_object_mut().into_iter().flatten() {
            if child["native_roster"] == true && !agents.contains_key(id)
                && child["state"] == "working" && catalog_at > child["updated"].as_f64().unwrap_or(f64::MAX) {
                child["state"] = json!("unknown");
                child["updated"] = json!(catalog_at);
                child["tracking_evidence"] = json!({"source":"kimi_session_state","kind":"catalog_removed","at":catalog_at});
                changed = true;
            }
        }
    }
    changed || (complete && !previous_native)
}

pub(super) fn reconcile(record: &mut Value) {
    let changed = codex(record) | kimi(record);
    if changed { settle(record); }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PARENT: &str = "11111111-1111-4111-8111-111111111111";
    const CHILD: &str = "22222222-2222-4222-8222-222222222222";
    const TURN: &str = "33333333-3333-4333-8333-333333333333";

    fn codex_fixture() -> (tempfile::TempDir,Value,PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");fs::create_dir_all(&home).unwrap();
        let rollout = home.join("rollout-child.jsonl");
        let db = Connection::open(home.join("state_5.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE threads(id TEXT PRIMARY KEY, rollout_path TEXT)").unwrap();
        db.execute("INSERT INTO threads VALUES(?1,?2)",rusqlite::params![CHILD,rollout.to_str()]).unwrap();
        let record = json!({"agent":"codex","agent_home":home,"conversation_id":PARENT,"main_done":true,"activity":"busy","phase":"working",
            "active_tools":{},"reply_id":"parent-reply","subagents":{CHILD:{"name":"Reviewer","state":"working","started":1.0,"updated":2.0,"reply_id":"child-reply"}}});
        (temp,record,rollout)
    }

    fn codex_rows(path: &Path, parent: &str, events: &[Value]) {
        use std::fmt::Write;
        let header = json!({"type":"session_meta","payload":{"id":CHILD,"parent_thread_id":parent,"forked_from_id":parent}});
        let mut text=format!("{header}\n");
        for event in events { writeln!(&mut text,"{event}").unwrap(); }
        fs::write(path,text).unwrap();
    }

    fn codex_event(kind: &str, at: &str) -> Value {
        json!({"type":"event_msg","timestamp":at,"payload":{"type":kind,"turn_id":TURN,"last_agent_message":"Complete"}})
    }

    #[test]
    fn codex_missing_stop_and_start_reconcile_exact_child_and_preserve_replies() {
        let (_temp,mut record,path) = codex_fixture();
        codex_rows(&path,PARENT,&[codex_event("task_complete","1970-01-01T00:00:10Z")]);
        reconcile(&mut record);
        assert_eq!(record["subagents"][CHILD]["state"],"finished");assert_eq!(record["phase"],"idle");
        assert_eq!(record["reply_id"],"parent-reply");assert_eq!(record["subagents"][CHILD]["reply_id"],"child-reply");
        codex_rows(&path,PARENT,&[codex_event("task_started","1970-01-01T00:00:11Z")]);
        reconcile(&mut record);
        assert_eq!(record["subagents"][CHILD]["state"],"working");assert_eq!(record["phase"],"working");
        record["subagents"][CHILD]["updated"] = json!(12.0);record["subagents"][CHILD]["state"] = json!("finished");
        reconcile(&mut record);assert_eq!(record["subagents"][CHILD]["state"],"finished");
    }

    #[test]
    fn codex_foreign_partial_empty_or_reordered_evidence_cannot_end_work() {
        for scenario in ["foreign-parent","foreign-child","partial","empty-reply","reordered","prose"] {
            let (_temp,mut record,path) = codex_fixture();
            let mut complete = codex_event("task_complete","1970-01-01T00:00:10Z");
            if scenario == "empty-reply" { complete["payload"]["last_agent_message"] = json!(""); }
            if scenario == "prose" { complete["type"] = json!("response_item"); }
            let parent = if scenario == "foreign-parent" {CHILD} else {PARENT};
            let mut events = vec![complete];
            if scenario == "reordered" { events.push(codex_event("task_started","1970-01-01T00:00:09Z")); }
            codex_rows(&path,parent,&events);
            if scenario == "foreign-child" { let s=fs::read_to_string(&path).unwrap().replace(CHILD,TURN);fs::write(&path,s).unwrap(); }
            if scenario == "partial" { let s=fs::read_to_string(&path).unwrap();fs::write(&path,s.trim_end()).unwrap(); }
            let before=record.clone();reconcile(&mut record);assert_eq!(record,before,"{scenario}");
        }
    }

    #[test]
    fn codex_explicit_failure_finishes_the_turn_and_retains_visible_error() {
        let (_temp,mut record,path)=codex_fixture();
        let mut event=codex_event("task_complete","1970-01-01T00:00:10Z");
        event["payload"]["error"]=json!({"message":"Provider request failed"});
        codex_rows(&path,PARENT,&[event]);reconcile(&mut record);
        assert_eq!(record["subagents"][CHILD]["state"],"finished");
        assert_eq!(record["subagents"][CHILD]["display_state"],"error");
        assert_eq!(record["subagents"][CHILD]["reply_id"],"child-reply");
    }

    #[test]
    fn codex_interrupts_require_a_turn_binding_and_cannot_end_another_turn() {
        for scenario in ["explicit","bound-legacy","unbound-legacy","other-turn"] {
            let (_temp,mut record,path)=codex_fixture();
            let start=codex_event("task_started","1970-01-01T00:00:09Z");
            let mut ended=codex_event("turn_aborted","1970-01-01T00:00:10Z");
            ended["payload"]["reason"]=json!("interrupted");
            if scenario.contains("legacy") { ended["payload"].as_object_mut().unwrap().remove("turn_id"); }
            if scenario == "other-turn" { ended["payload"]["turn_id"]=json!("different-turn"); }
            let rows=if scenario == "unbound-legacy" {vec![ended]} else {vec![start,ended]};
            codex_rows(&path,PARENT,&rows);reconcile(&mut record);
            assert_eq!(record["subagents"][CHILD]["state"],if ["explicit","bound-legacy"].contains(&scenario) {"finished"} else {"working"},"{scenario}");
        }
    }

    fn kimi_fixture() -> (tempfile::TempDir,Value,PathBuf,Value) {
        let temp=tempfile::tempdir().unwrap();let home=temp.path().join("kimi");
        let session=home.join("sessions/workspace").join(format!("session_{PARENT}"));
        for id in ["main","agent-0","agent-1"] { fs::create_dir_all(session.join("agents").join(id)).unwrap(); }
        fs::write(session.join("agents/main/wire.jsonl"),"{}\n").unwrap();
        let agents=json!({"main":{"homedir":session.join("agents/main")},
            "agent-0":{"homedir":session.join("agents/agent-0"),"parentAgentId":"main","labels":{"profileName":"code"}},
            "agent-1":{"homedir":session.join("agents/agent-1"),"parentAgentId":"main","labels":{"profileName":"code"}}});
        let state=json!({"id":format!("session_{PARENT}"),"version":2,"updatedAt":20000,"agents":agents});
        fs::write(session.join("state.json"),state.to_string()).unwrap();
        let record=json!({"agent":"kimi","agent_home":home,"conversation_id":format!("session_{PARENT}"),"main_done":true,"activity":"busy","phase":"working",
            "active_tools":{},"last_event_at":2.0,"subagent_groups_complete":true,
            "subagent_groups":{"code":{"active_count":3,"total_count":3}},
            "subagents":{"code":{"name":"code","state":"working"}},"reply_id":"parent-reply"});
        (temp,record,session,state)
    }

    fn kimi_write(session: &Path,id: &str,kind: &str,at: u64) {
        let row=json!({"type":kind,"agentId":id,"turnId":0,"reason":"completed","outcome":"done","time":at});
        fs::write(session.join("agents").join(id).join("wire.jsonl"),format!("{row}\n")).unwrap();
    }

    #[test]
    fn kimi_exact_roster_replaces_profile_ambiguity_and_recovers_missing_hooks() {
        let (_temp,mut record,session,_state)=kimi_fixture();
        kimi_write(&session,"agent-0","turn.ended",10000);kimi_write(&session,"agent-1","agent.turn.started",11000);
        reconcile(&mut record);assert_eq!(record["kimi_native_roster"],true);
        let summary=telemetry::summary(&record,&json!({"runtime_state":"live","activity":"busy","phase":"working"}),"","");
        assert_eq!(summary["subagent_active_count"],1);assert_eq!(summary["subagent_completed_count"],1);
        assert_eq!(summary["subagent_total_count"],2);assert_eq!(telemetry::grouped_active(&record),0);
        kimi_write(&session,"agent-1","turn.ended",12000);reconcile(&mut record);
        assert_eq!(record["phase"],"idle");assert!(lifecycle::safe_to_pause(&record));
        assert_eq!(record["reply_id"],"parent-reply");
        kimi_write(&session,"agent-0","agent.turn.started",13000);reconcile(&mut record);
        assert_eq!(record["phase"],"working");assert!(!lifecycle::safe_to_pause(&record));
    }

    #[test]
    fn kimi_partial_foreign_or_legacy_catalogs_keep_profile_counts() {
        for scenario in ["legacy","foreign-session","foreign-home","partial-wire","newer-hook"] {
            let (_temp,mut record,session,mut state)=kimi_fixture();
            kimi_write(&session,"agent-0","turn.ended",10000);kimi_write(&session,"agent-1","turn.ended",10000);
            match scenario {
                "legacy"=>state["version"]=json!(1),
                "foreign-session"=>state["id"]=json!("foreign"),
                "foreign-home"=>state["agents"]["agent-0"]["homedir"]=json!(session.join("agents/main")),
                "partial-wire"=>fs::write(session.join("agents/agent-0/wire.jsonl"),"{").unwrap(),
                "newer-hook"=>record["subagent_groups"]["code"]["updated"]=json!(30.0),
                _=>unreachable!(),
            }
            fs::write(session.join("state.json"),state.to_string()).unwrap();reconcile(&mut record);
            if scenario == "partial-wire" {
                assert_eq!(record["subagents"]["agent-0"]["state"],"unknown");
                let summary=telemetry::summary(&record,&json!({"runtime_state":"live","activity":"idle","phase":"idle"}),"","");
                assert_eq!(summary["subagent_counts_complete"],false);assert_eq!(summary["subagent_unknown_count"],1);
            } else { assert_ne!(record["kimi_native_roster"],true); }
        }
    }

    #[test]
    #[cfg(unix)]
    fn kimi_foreign_wire_symlink_cannot_borrow_identity_free_events() {
        let (_temp,mut record,session,_state)=kimi_fixture();
        let other=session.join("foreign-wire.jsonl");
        fs::write(&other,"{\"type\":\"agent.turn.ended\",\"turnId\":0,\"outcome\":\"done\",\"time\":10000}\n").unwrap();
        std::os::unix::fs::symlink(other,session.join("agents/agent-0/wire.jsonl")).unwrap();
        kimi_write(&session,"agent-1","turn.ended",11000);
        reconcile(&mut record);
        assert_eq!(record["subagents"]["agent-0"]["state"],"unknown");
        assert_eq!(record["subagents"]["agent-1"]["state"],"finished");
    }

    #[test]
    fn kimi_abort_and_late_foreign_turn_have_exact_boundaries() {
        let temp=tempfile::tempdir().unwrap();let path=temp.path().join("wire.jsonl");
        let start=json!({"type":"agent.turn.started","turnId":0,"time":10000});
        let abort=json!({"type":"agent.turn.ended","turnId":0,"outcome":"aborted","time":11000});
        fs::write(&path,format!("{start}\n{abort}\n")).unwrap();
        assert_eq!(kimi_turn(&path,"agent-0").unwrap()["state"],"finished");
        let late=json!({"type":"turn.ended","turnId":1,"reason":"completed","time":12000});
        fs::write(&path,format!("{start}\n{late}\n")).unwrap();
        assert_eq!(kimi_turn(&path,"agent-0").unwrap()["state"],"working");
    }

    #[test]
    fn kimi_catalog_removal_does_not_invent_a_finished_result() {
        let (_temp,mut record,session,mut state)=kimi_fixture();
        kimi_write(&session,"agent-0","agent.turn.started",10000);kimi_write(&session,"agent-1","turn.ended",11000);
        reconcile(&mut record);state["agents"].as_object_mut().unwrap().remove("agent-0");
        fs::write(session.join("state.json"),state.to_string()).unwrap();reconcile(&mut record);
        assert_eq!(record["subagents"]["agent-0"]["state"],"unknown");assert_eq!(record["phase"],"idle");
    }
}
