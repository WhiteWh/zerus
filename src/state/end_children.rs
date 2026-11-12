//! `hgs terminate|archive SESSION --with-children` (zerus-sa9qn.17): the
//! explicit choice to end a parent's live child sessions after it. Nothing
//! propagates without the flag. The parent runs first with its own command,
//! unchanged; only when it succeeds does each live descendant on this
//! computer get the same command, one at a time, through its own existing
//! per-session rules: Terminate keeps every working folder (worktree removal
//! is never implied), Archive takes only stopped sessions, and a provider
//! without the action refuses it. Each child is bound to its exact session ID
//! and, for Terminate, its listed run. A child's outcome is reported and never
//! retried. Descendants come from one listing, matched as listings match a
//! parent (`parent_link::decorate`): by session ID, or by conversation for a
//! parent listed without one; a link another computer forwarded never
//! matches. The walk stops after eight levels and never visits a session twice.
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

/// `command` (terminate or archive) with `args` that hold [`FLAG`].
pub(super) fn dispatch(command: &str, args: &[String]) -> Result<i32> {
    let verb = if command == "terminate" {
        "terminated"
    } else {
        "archived"
    };
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
    if dry {
        let code = super::dispatch(&call)?;
        for child in &children {
            println!(
                "hgs: would also {command} child session {} (level {})",
                child.name, child.depth
            );
        }
        return Ok(code);
    }
    // The parent first, with its own command and rules. Its failure ends here:
    // no child is touched.
    let code = super::dispatch(&call)?;
    if code != 0 {
        eprintln!("hgs: {name} was not {verb}; no child session was {verb}");
        return Ok(code);
    }
    let mut failed = 0;
    for child in &children {
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
    if failed > 0 {
        return Err(format!(
            "{failed} of {} child sessions were not {verb}; nothing was retried",
            children.len()
        ));
    }
    Ok(0)
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
}
