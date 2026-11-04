//! Explicit stop-and-archive. `kill`/`forget` retain their removal semantics.
use super::*;
use std::time::{Duration, Instant};

pub(super) fn dispatch(args: &[String], dry: bool) -> Result<i32> { dispatch_scoped(args,dry,None,false) }

pub(super) fn dispatch_scoped(args: &[String], dry: bool, scope: Option<&session_action::Scope>, forget: bool) -> Result<i32> {
    let (name, expected_run) = match args {
        [name] => (name.as_str(), None),
        [name, flag, run] if flag == "--expected-run-id" && !run.is_empty() => {
            (name.as_str(), Some(run.as_str()))
        }
        _ => return Err("usage: hgs terminate <session> [--expected-run-id ID]".into()),
    };
    let original = {
        let _guard = lock(None)?;
        let record = read(name)?;
        if let Some(scope)=scope {scope.check(&record)?;}
        if expected_run.is_some_and(|run| run != string(&record, "run_id")) {
            return Err("session changed before termination; refresh and try again".into());
        }
        let snapshot = live()?;
        if let Some(panes) = snapshot.get(name) {
            if panes.len() != 1
                || panes[0].0 != string(&record, "pane")
                || panes[0].2 != string(&record, "run_id")
                || panes[0].2.is_empty()
            {
                return Err("session terminal identity changed; refresh before terminating".into());
            }
            let target = backend::current()
                .pane_info(
                    backend::Target::Pane(string(&record, "pane")),
                    &[backend::PaneField::SessionId],
                )?
                .value()
                .to_owned();
            if !target
                .strip_prefix('$')
                .is_some_and(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
            {
                return Err("could not confirm the session terminal".into());
            }
            if !dry {
                // Pin the tmux instance, never resolve a reusable name a second time.
                // The writer lock prevents a concurrent HGS rename/relaunch here.
                if let Some(scope)=scope {scope.changing();}
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                let owned = processes::Termination::prepare(&record)
                    .map_err(|error| format!("Could not verify session processes; terminal kept: {error}"))?;
                backend::current().kill_session(backend::Target::Session(&target))?;
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                if let Some(owned) = owned { owned.finish()
                    .map_err(|error| format!("Terminal closed, but process shutdown was not confirmed; worktree kept: {error}"))?; }
            }
        } else if process_alive(&record) {
            return Err(
                "tracked agent is running without its expected terminal; no process was stopped"
                    .into(),
            );
        }
        record
    };
    if dry {
        println!("hgs: would terminate and archive {name}");
        return Ok(0);
    }
    // Supervisors and final hooks can now record the exit. On timeout the binding
    // stays in place: stopping a terminal must never silently forget its history.
    // A Windows session host first pauses the agent for up to 10 s, then closes
    // its console, which leaves another 5 s (spec D11).
    let deadline = Instant::now() + Duration::from_secs(if cfg!(windows) { 20 } else { 10 });
    while process_alive(&original) {
        if Instant::now() >= deadline {
            return Err(
                "terminal closed, but the agent has not exited; its saved binding was kept".into(),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // A Windows session host lists the session until its supervisor has recorded
    // the exit, which takes the state lock, and ended (spec D11): wait for that
    // without the lock. tmux dropped it with kill-session already.
    #[cfg(windows)]
    while listed(&live()?, name, string(&original, "run_id")) {
        if Instant::now() >= deadline {
            return Err(
                "terminal closed, but its supervisor has not ended; its saved binding was kept"
                    .into(),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _guard = lock(None)?;
    if let Some(archived) = archive::records()?.into_iter().find(|entry| {
        entry["name"] == original["name"]
            && entry["conversation_id"] == original["conversation_id"]
            && (entry["run_id"] == original["run_id"]
                || (!string(&original, "restore_archive_id").is_empty()
                    && entry["archive_id"] == original["restore_archive_id"]))
    }) {
        if forget {
            // Check every retained binding and recovery reference before the
            // first removal. A newly launched/awaiting restore owns its source.
            let current = if record_path(name).exists() {Some(read(name)?)} else {None};
            forget_archive_preflight(&archived,current.as_ref(),scope)?;
            if live()?.contains_key(name) || current.as_ref().is_some_and(process_alive) {
                return Err("Session is still running; no archive or binding was forgotten".into());
            }
            if let Some(scope)=scope {scope.changing();}
            archive::remove_synced(&archive::path(string(&archived,"archive_id"))?)?;
            if let Some(record)=current {
                rename::remove_legacy(&record)?;
                archive::remove_synced(&record_path(name))?;
            }
        } else {
            drop(_guard);
            super::worktree_lifecycle::after_archive(&archived, string(&archived, "archive_id"));
            if let Some(scope)=scope {scope.result_record(&archived);}
            else {println!("hgs: archived {name} ({})", string(&archived, "archive_id"));}
        }
        return Ok(0);
    }
    let mut record = read(name)?;
    if let Some(scope)=scope {scope.check(&record)?;}
    if record["run_id"] != original["run_id"] || record["created"] != original["created"] {
        return Err(
            "session binding changed while terminating; the replacement was left intact".into(),
        );
    }
    // No live instance with the same name may be removed from the catalog.
    if live()?.contains_key(name) || process_alive(&record) {
        return Err("session is still running; its saved binding was kept".into());
    }
    if let Some(scope)=scope {scope.changing();}
    if forget {
        rename::remove_legacy(&record)?;
        archive::remove_synced(&record_path(name))?;
        return Ok(0);
    }
    record["completion_source"] = json!("manual_terminate");
    record["completion_reason"] = json!("user_terminated");
    let id = archive::save(&record)?;
    let archived = archive::read_archive(name,&id)?;
    drop(_guard);
    super::worktree_lifecycle::after_archive(&record, &id);
    if let Some(scope)=scope {scope.result_record(&archived);} else {println!("hgs: terminated and archived {name} ({id})");}
    Ok(0)
}

/// Whether `snapshot` still lists `name` with a pane of run `run_id`.
#[cfg_attr(not(windows), allow(dead_code))]
fn listed(snapshot: &Snapshot, name: &str, run_id: &str) -> bool {
    snapshot
        .get(name)
        .is_some_and(|panes| panes.iter().any(|(_, _, run)| run == run_id))
}

fn forget_archive_preflight(archived: &Value, current: Option<&Value>, scope: Option<&session_action::Scope>) -> Result<()> {
    if let Some(current)=current {
        if !string(current,"restore_archive_id").is_empty() && current["restore_archive_id"] == archived["archive_id"] {
            return Err("Archive restore is awaiting confirmation; its recovery source was kept".into());
        }
        if let Some(scope)=scope {scope.check(current)?;}
    }
    Ok(())
}

#[cfg(test)] mod scoped_tests {
    use super::*;
    #[test] fn replacement_restore_protects_archive_before_any_removal() {
        let archived=json!({"archive_id":"archive","run_id":"old"});
        let replacement=json!({"run_id":"new","restore_archive_id":"archive"});
        assert!(forget_archive_preflight(&archived,Some(&replacement),None).is_err());
        assert!(forget_archive_preflight(&archived,None,None).is_ok());
    }
    #[test] fn only_the_terminated_run_keeps_the_wait() {
        let pane=|run: &str| ("%1".to_owned(),"0".to_owned(),run.to_owned());
        let snapshot=Snapshot::from([("a".to_owned(),vec![pane("run-1")]),("b".to_owned(),vec![pane("run-2")])]);
        assert!(listed(&snapshot,"a","run-1"));
        // Another run under the name, or none, ends the wait; the binding checks
        // after it refuse a replacement.
        assert!(!listed(&snapshot,"a","run-2"));
        assert!(!listed(&snapshot,"c","run-1"));
        assert!(!listed(&Snapshot::new(),"a","run-1"));
    }
}
