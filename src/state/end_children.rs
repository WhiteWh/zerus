//! `hgs terminate|archive SESSION --with-children` (zerus-sa9qn.17): the
//! explicit choice to end a parent's live child sessions after it. Nothing
//! propagates without the flag. The parent runs first with its own command,
//! unchanged; only when it succeeds does each live descendant on this
//! computer get the same command, one at a time, through its own existing
//! per-session rules: Terminate keeps every working folder (worktree removal
//! is never implied), Archive takes only stopped sessions, and a provider
//! without the action refuses it. The parent's own command and each child's
//! are bound to the session ID, run and conversation the listing showed, so a
//! session renamed, restarted or replaced under its label since then refuses
//! (zerus-sa9qn.29 for the parent: then no child is touched). A child's outcome
//! is reported and never retried. Descendants come from one listing: a link names a parent by its
//! session ID, and only a link without a session ID names a parent listed
//! without one, by conversation. That is stricter than the listing's nesting
//! (`parent_link::decorate`), which also falls back to the conversation for an
//! identified link. A link another computer forwarded never matches. The walk
//! stops after eight levels and never visits a session twice.
//! The configured worktree maintenance treats each archived child as it treats
//! any archived session; no Remove working folder choice reaches a child.
use super::*;

pub(super) const FLAG: &str = "--with-children";
/// The deepest descendant level ended, as the lists nest (spec 1.4).
const DEPTH: usize = 8;

/// One live descendant, as the listing names it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Child {
    pub(super) name: String,
    pub(super) session_id: String,
    pub(super) run_id: String,
    pub(super) conversation_id: String,
    pub(super) depth: usize,
}

/// The parent's live descendants in `rows` (a listing), breadth first:
/// every child before any grandchild. Archived rows are never children.
pub(super) fn descendants(rows: &[Value], parent: &Value) -> Vec<Child> {
    let key = |row: &Value| {
        let id = string(row, "session_id");
        if id.is_empty() {
            format!(
                "c:{}\n{}",
                string(row, "name"),
                string(row, "conversation_id")
            )
        } else {
            format!("s:{id}")
        }
    };
    let mut seen = std::collections::HashSet::from([key(parent)]);
    let mut found = Vec::new();
    let mut level = vec![parent];
    for depth in 1..=DEPTH {
        let mut next = Vec::new();
        for row in rows {
            if string(row, "state") == "archived" || seen.contains(&key(row)) {
                continue;
            }
            let Some(link) =
                parent_link::listed(row).filter(|link| string(link, "source") != "forwarded")
            else {
                continue;
            };
            let named = level.iter().any(|up| {
                let id = string(up, "session_id");
                if !id.is_empty() {
                    string(&link, "session_id") == id
                } else {
                    let conversation = string(up, "conversation_id");
                    string(&link, "session_id").is_empty()
                        && !conversation.is_empty()
                        && string(&link, "conversation_id") == conversation
                }
            });
            if !named {
                continue;
            }
            seen.insert(key(row));
            found.push(Child {
                name: string(row, "name").into(),
                session_id: string(row, "session_id").into(),
                run_id: string(row, "run_id").into(),
                conversation_id: string(row, "conversation_id").into(),
                depth,
            });
            next.push(row);
        }
        if next.is_empty() {
            break;
        }
        level = next;
    }
    found
}

/// This computer's listing, as `hgs ls --json` lists it, without archives.
fn listing() -> Result<Vec<Value>> {
    let listing = backend::current().listing()?;
    let raw = json!({"sessions": listed_rows(&listing)});
    let merged = merge_listing_archives(raw, &listed_ids(&listing), false)?;
    Ok(merged["sessions"].as_array().cloned().unwrap_or_default())
}

/// What `--with-children` decided from one listing, before anything ran: the
/// parent's own command and the parent and its live descendants as listed.
struct Plan {
    command: String,
    dry: bool,
    call: Vec<String>,
    parent: Child,
    children: Vec<Child>,
}

fn verb(command: &str) -> &'static str {
    if command == "terminate" {
        "terminated"
    } else {
        "archived"
    }
}

/// `command` (terminate or archive) with `args` that hold [`FLAG`].
pub(super) fn dispatch(command: &str, args: &[String]) -> Result<i32> {
    run(&plan(command, args)?)
}

/// The listing's parent and descendants for `command` with `args`.
fn plan(command: &str, args: &[String]) -> Result<Plan> {
    let verb = verb(command);
    let base: Vec<String> = args.iter().filter(|arg| *arg != FLAG).cloned().collect();
    let dry = base.iter().any(|arg| arg == "--dry-run");
    // The session comes first, as each command reads it; `--dry-run` may
    // stand anywhere, as dispatch drops it from anywhere.
    let name = base
        .iter()
        .find(|arg| *arg != "--dry-run")
        .filter(|arg| !arg.starts_with('-'))
        .cloned()
        .ok_or_else(|| format!("usage: hgs {command} <session> {FLAG}"))?;
    let expected = base
        .windows(2)
        .find(|pair| pair[0] == "--expected-run-id")
        .map(|pair| pair[1].clone());
    // The session the command's label resolution or `--session-id` selected.
    let selected = session_id::selected().filter(|selection| selection.name == name);
    let rows = listing()?;
    let parents: Vec<&Value> = rows
        .iter()
        .filter(|row| string(row, "name") == name && string(row, "state") != "archived")
        .filter(|row| {
            expected
                .as_deref()
                .is_none_or(|run| string(row, "run_id") == run)
        })
        .filter(|row| {
            selected.as_ref().is_none_or(|selection| {
                selection.session_id.is_empty() || string(row, "session_id") == selection.session_id
            })
        })
        .collect();
    let [parent] = parents.as_slice() else {
        return Err(format!(
            "{name} is not one listed live session here; no session was {verb}"
        ));
    };
    let children = descendants(&rows, parent);
    let mut call = vec![command.to_owned()];
    call.extend(base.iter().cloned());
    Ok(Plan {
        command: command.to_owned(),
        dry,
        call,
        parent: Child {
            name: string(parent, "name").into(),
            session_id: string(parent, "session_id").into(),
            run_id: string(parent, "run_id").into(),
            conversation_id: string(parent, "conversation_id").into(),
            depth: 0,
        },
        children,
    })
}

/// Runs `plan`: the parent, then each child.
fn run(plan: &Plan) -> Result<i32> {
    let (command, verb, parent) = (plan.command.as_str(), verb(&plan.command), &plan.parent);
    // zerus-sa9qn.29: the parent's own command reads its label only as the
    // session the listing named, with the session ID, run and conversation the
    // descendants were found under. A rename plus a reused label, or a restart,
    // since the listing refuses here, and no child is touched.
    session_id::select_guarded(
        &parent.name,
        &parent.session_id,
        &parent.run_id,
        &parent.conversation_id,
    );
    if plan.dry {
        let code = super::dispatch(&plan.call)?;
        for child in &plan.children {
            println!(
                "hgs: would also {command} child session {} (level {})",
                child.name, child.depth
            );
        }
        return Ok(code);
    }
    // The parent first, with its own command and rules. Its failure ends here:
    // no child is touched.
    let code = super::dispatch(&plan.call)?;
    if code != 0 {
        eprintln!(
            "hgs: {} was not {verb}; no child session was {verb}",
            parent.name
        );
        return Ok(code);
    }
    let failed = end_each(command, verb, &plan.children);
    if failed > 0 {
        return Err(format!(
            "{failed} of {} child sessions were not {verb}; nothing was retried",
            plan.children.len()
        ));
    }
    Ok(0)
}

/// Each listed child in turn, after its parent ended: the same command, bound
/// to the identities the listing showed. Returns how many were not ended;
/// none is retried.
fn end_each(command: &str, verb: &str, children: &[Child]) -> usize {
    let mut failed = 0;
    for child in children {
        // Every read of this child's label must carry each identity it was
        // listed with, as a guarded command's selection does: a session that
        // took the label since, or a child resumed into another run, refuses.
        session_id::select_guarded(
            &child.name,
            &child.session_id,
            &child.run_id,
            &child.conversation_id,
        );
        let mut call = vec![command.to_owned(), child.name.clone()];
        if command == "terminate" && !child.run_id.is_empty() {
            call.extend(["--expected-run-id".to_owned(), child.run_id.clone()]);
        }
        // The child's own command reports its success, as it would alone.
        match super::dispatch(&call) {
            Ok(0) => {}
            Ok(code) => {
                failed += 1;
                eprintln!(
                    "hgs: child session {} was not {verb} (exit {code}); not retried",
                    child.name
                );
            }
            Err(error) => {
                failed += 1;
                eprintln!(
                    "hgs: child session {} was not {verb}: {error}; not retried",
                    child.name
                );
            }
        }
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;

    const MACHINE: &str = "11111111-1111-4111-8111-111111111111";
    fn id(n: u32) -> String {
        format!("00000000-0000-4000-8000-{n:012}")
    }
    fn link(n: u32, source: &str) -> Value {
        json!({"schema":1,"machine":MACHINE,"host":"fixture","session_id":id(n),"name":format!("kimi/p/s{n}"),
               "conversation_id":format!("c{n}"),"run_id":format!("r{n}"),"source":source,"at":1})
    }
    fn row(n: u32, parent: Option<Value>, state: &str) -> Value {
        let mut row = json!({"name":format!("kimi/p/s{n}"),"session_id":id(n),"run_id":format!("r{n}"),
                             "conversation_id":format!("c{n}"),"state":state});
        if let Some(parent) = parent {
            row["parent"] = parent;
        }
        row
    }

    #[test]
    fn descendants_are_live_breadth_first_and_never_forwarded_or_archived() {
        let rows = vec![
            row(1, None, "stopped"),
            row(2, Some(link(1, "launch")), "running"),
            row(3, Some(link(2, "adopt")), "running"),
            row(4, Some(link(1, "manual")), "stopped"),
            row(5, Some(link(1, "launch")), "archived"),
            row(6, Some(link(1, "forwarded")), "running"),
            row(7, None, "running"),
            row(8, Some(link(7, "launch")), "running"),
        ];
        let found = descendants(&rows, &rows[0]);
        let names: Vec<_> = found
            .iter()
            .map(|child| (child.name.as_str(), child.depth))
            .collect();
        assert_eq!(
            names,
            [("kimi/p/s2", 1), ("kimi/p/s4", 1), ("kimi/p/s3", 2)]
        );
        assert_eq!(found[0].run_id, "r2");
        assert_eq!(found[0].session_id, id(2));
    }

    #[test]
    fn a_parent_listed_without_a_session_id_matches_by_conversation_only() {
        let mut parent = row(1, None, "stopped");
        parent["session_id"] = json!("");
        let mut by_conversation = link(1, "launch");
        by_conversation["session_id"] = json!("");
        let rows = vec![
            parent.clone(),
            row(2, Some(by_conversation), "running"),
            row(3, Some(link(1, "launch")), "running"),
        ];
        let names: Vec<_> = descendants(&rows, &parent)
            .into_iter()
            .map(|child| child.name)
            .collect();
        assert_eq!(names, ["kimi/p/s2"]);
    }

    #[test]
    fn a_loop_or_a_deep_chain_ends_and_visits_nobody_twice() {
        // 1 <- 2 <- 1: the parent is never its own descendant.
        let rows = vec![
            row(1, Some(link(2, "launch")), "stopped"),
            row(2, Some(link(1, "launch")), "running"),
        ];
        assert_eq!(descendants(&rows, &rows[0]).len(), 1);
        let chain: Vec<Value> = (1..=20)
            .map(|n| row(n, (n > 1).then(|| link(n - 1, "launch")), "running"))
            .collect();
        let found = descendants(&chain, &chain[0]);
        assert_eq!(found.len(), DEPTH);
        assert_eq!(found.last().unwrap().depth, DEPTH);
    }

    /// Re-runs `fixture` alone in this test binary with private state, as
    /// `parent_link`'s fixtures do: a command reads the process's state root
    /// and its selection.
    #[cfg(unix)]
    fn isolated(fixture: &str) {
        let root = tempfile::tempdir().unwrap();
        for folder in ["state", "config", "home", "xdg", "tmux"] {
            std::fs::create_dir(root.path().join(folder)).unwrap();
        }
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            &format!("state::end_children::tests::{fixture}"),
            "--ignored",
            "--test-threads=1",
        ]);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("HGS_") || key == "TMUX" {
                command.env_remove(key);
            }
        }
        let output = command
            .env("HGS_STATE_DIR", root.path().join("state"))
            .env("HGS_CONFIG_DIR", root.path().join("config"))
            .env("HOME", root.path().join("home"))
            .env("XDG_STATE_HOME", root.path().join("xdg"))
            .env("TMUX_TMPDIR", root.path().join("tmux"))
            .env("HGS_SELF", "fixture")
            .env("HGS_PEERS", "")
            .env("ZERUS_END_CHILDREN_FIXTURE", "1")
            .output()
            .unwrap();
        let out = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "{out}");
        // An --exact filter that matched nothing would pass without running it.
        assert!(out.contains("test result: ok. 1 passed"), "{out}");
    }

    /// A stopped tracked session in the private state's legacy layout, as
    /// tests/test_end_children.py writes them.
    #[cfg(unix)]
    fn saved(n: u32, label: &str, parent: Option<&Value>) -> Value {
        let mut record = json!({"version":1,"name":format!("codex/example/{label}"),"agent":"codex",
            "run_id":format!("run-{n}"),"conversation_id":format!("conversation-{n}"),"session_id":id(n),
            "account_id":"fixture","subagents":{},"created":1_700_000_000 + n,"cwd":root().display().to_string(),
            "pid":1,"process_start":"not this process"});
        if let Some(parent) = parent {
            record["parent"] = json!({"schema":1,"machine":MACHINE,"host":"fixture",
                "session_id":string(parent, "session_id"),"name":string(parent, "name"),
                "conversation_id":string(parent, "conversation_id"),"run_id":string(parent, "run_id"),
                "source":"launch","at":1_700_000_000});
        }
        write_saved(&record);
        record
    }

    #[cfg(unix)]
    fn write_saved(record: &Value) {
        std::fs::write(
            legacy_record_path(string(record, "name")),
            record.to_string(),
        )
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_changed_after_the_listing_is_refused() {
        isolated("changed_child_fixture");
    }

    /// #2086 review (t1, t2): a child restarted into another run, or given
    /// another conversation, between the listing and its turn is refused by
    /// both commands and kept as it is; the same child unchanged is ended.
    #[cfg(unix)]
    #[test]
    #[ignore = "process-isolated private state fixture"]
    fn changed_child_fixture() {
        if std::env::var("ZERUS_END_CHILDREN_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        let mut n = 10;
        for command in ["archive", "terminate"] {
            let verb = if command == "terminate" {
                "terminated"
            } else {
                "archived"
            };
            for change in ["", "run_id", "conversation_id"] {
                n += 2;
                let label = format!(
                    "{command}-{}",
                    if change.is_empty() { "same" } else { change }
                );
                let parent = saved(n, &format!("{label}-lead"), None);
                let child = saved(n + 1, &format!("{label}-child"), Some(&parent));
                session_id::clear();
                let rows = listing().unwrap();
                let listed = rows
                    .iter()
                    .find(|row| string(row, "name") == string(&parent, "name"))
                    .unwrap();
                let children = descendants(&rows, listed);
                assert_eq!(children.len(), 1, "{label}: {rows:?}");
                assert_eq!(children[0].run_id, string(&child, "run_id"), "{label}");
                assert_eq!(
                    children[0].conversation_id,
                    string(&child, "conversation_id"),
                    "{label}"
                );
                let mut now = child.clone();
                if !change.is_empty() {
                    now[change] = json!(format!("{}-new", string(&child, change)));
                    write_saved(&now);
                }
                let failed = end_each(command, verb, &children);
                let archived: Vec<Value> = archive::records()
                    .unwrap()
                    .into_iter()
                    .filter(|entry| string(entry, "name") == string(&child, "name"))
                    .collect();
                let path = legacy_record_path(string(&child, "name"));
                if change.is_empty() {
                    assert_eq!(failed, 0, "{label}");
                    assert_eq!(archived.len(), 1, "{label}");
                    assert!(!path.exists(), "{label}");
                } else {
                    assert_eq!(
                        failed, 1,
                        "{label}: a child that changed after the listing must be refused"
                    );
                    assert!(archived.is_empty(), "{label}: {archived:?}");
                    let kept: Value =
                        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                    assert_eq!(kept[change], now[change], "{label}");
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_parent_relabeled_or_restarted_after_the_listing_ends_nobody() {
        isolated("relabeled_parent_fixture");
    }

    /// zerus-sa9qn.29: between the listing and the parent's own call, the
    /// parent is renamed away and another session takes its label, or the
    /// parent is restarted into another run. Both commands refuse, and the
    /// label's new holder, the renamed parent and the child all stay; the same
    /// parent unchanged ends with its child. A dry run is bound the same way
    /// (zerus-sa9qn.31): it refuses a changed parent, and none ends anything.
    #[cfg(unix)]
    #[test]
    #[ignore = "process-isolated private state fixture"]
    fn relabeled_parent_fixture() {
        if std::env::var("ZERUS_END_CHILDREN_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        let archived = |name: &str| {
            archive::records()
                .unwrap()
                .iter()
                .filter(|entry| string(entry, "name") == name)
                .count()
        };
        let mut n = 100;
        for (command, dry) in [
            ("archive", false),
            ("terminate", false),
            ("archive", true),
            ("terminate", true),
        ] {
            for change in ["", "relabel", "restart"] {
                n += 3;
                let label = format!(
                    "{command}{}-{}",
                    if dry { "-dry" } else { "" },
                    if change.is_empty() { "same" } else { change }
                );
                let parent = saved(n, &format!("{label}-lead"), None);
                let child = saved(n + 1, &format!("{label}-child"), Some(&parent));
                let name = string(&parent, "name").to_owned();
                session_id::clear();
                let mut args = vec![name.clone(), FLAG.into()];
                if dry {
                    args.push("--dry-run".into());
                }
                let planned = plan(command, &args).unwrap();
                assert_eq!(planned.dry, dry, "{label}");
                assert_eq!(
                    planned.parent.session_id,
                    string(&parent, "session_id"),
                    "{label}"
                );
                assert_eq!(planned.children.len(), 1, "{label}");
                let renamed = format!("codex/example/{label}-renamed");
                let mut holder = parent.clone();
                match change {
                    "relabel" => {
                        let mut moved = parent.clone();
                        moved["name"] = json!(renamed);
                        std::fs::remove_file(legacy_record_path(&name)).unwrap();
                        write_saved(&moved);
                        holder["session_id"] = json!(id(n + 2));
                        holder["run_id"] = json!(format!("run-{}", n + 2));
                        holder["conversation_id"] = json!(format!("conversation-{}", n + 2));
                        write_saved(&holder);
                    }
                    "restart" => {
                        holder["run_id"] = json!(format!("{}-new", string(&parent, "run_id")));
                        write_saved(&holder);
                    }
                    _ => {}
                }
                let result = run(&planned);
                if change.is_empty() {
                    let ended = usize::from(!dry);
                    assert_eq!(result, Ok(0), "{label}");
                    assert_eq!(archived(&name), ended, "{label}");
                    assert_eq!(archived(string(&child, "name")), ended, "{label}");
                    continue;
                }
                assert!(
                    result.is_err(),
                    "{label}: the parent changed after the listing: {result:?}"
                );
                assert_eq!(archived(&name), 0, "{label}: the label's holder stays");
                assert_eq!(archived(&renamed), 0, "{label}");
                assert_eq!(
                    archived(string(&child, "name")),
                    0,
                    "{label}: no child is touched"
                );
                let kept: Value = serde_json::from_str(
                    &std::fs::read_to_string(legacy_record_path(&name)).unwrap(),
                )
                .unwrap();
                assert_eq!(kept, holder, "{label}");
                assert!(
                    legacy_record_path(string(&child, "name")).exists(),
                    "{label}"
                );
                if change == "relabel" {
                    assert!(legacy_record_path(&renamed).exists(), "{label}");
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_parent_listed_without_a_session_id_still_ends_with_its_child() {
        isolated("legacy_parent_fixture");
    }

    /// The parent's binding checks only the identities the listing showed
    /// (zerus-sa9qn.29): a parent saved without a session ID, whose child links
    /// it by conversation, still ends with that child under both commands.
    #[cfg(unix)]
    #[test]
    #[ignore = "process-isolated private state fixture"]
    fn legacy_parent_fixture() {
        if std::env::var("ZERUS_END_CHILDREN_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        let archived = |name: &str| {
            archive::records()
                .unwrap()
                .iter()
                .filter(|entry| string(entry, "name") == name)
                .count()
        };
        for (n, command) in [(200, "archive"), (203, "terminate")] {
            let label = format!("legacy-{command}");
            let mut parent = saved(n, &format!("{label}-lead"), None);
            parent.as_object_mut().unwrap().remove("session_id");
            write_saved(&parent);
            let child = saved(n + 1, &format!("{label}-child"), Some(&parent));
            let name = string(&parent, "name").to_owned();
            session_id::clear();
            let planned = plan(command, &[name.clone(), FLAG.into()]).unwrap();
            assert_eq!(planned.parent.session_id, "", "{label}");
            assert_eq!(planned.children.len(), 1, "{label}");
            assert_eq!(run(&planned), Ok(0), "{label}");
            assert_eq!(archived(&name), 1, "{label}");
            assert_eq!(archived(string(&child, "name")), 1, "{label}");
        }
    }
}
