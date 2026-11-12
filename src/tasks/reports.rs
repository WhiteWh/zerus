//! Reports carry the origin from their original dispatch, never a lookup of
//! the latest attempt. Older CLIs reject this action rather than ignoring a
//! new option on their permissive legacy `report` parser.
use super::*;

pub(super) const HISTORY_HINT: &str = "The report was kept as history and the task did not move. \
    Use the pinned report-attempt command in the original task prompt, or ask the person to accept \
    or resend the task.";

const LEGACY_TRANSITION_HINT: &str = "Start acknowledged. This task has managed history, but its \
    current execution has no managed report origin. No attempt UUID was created. Reports will be \
    kept as history and will not move the task. Ask the person to accept or resend it.";

pub(super) fn execute(config: &Config, args: &[String]) -> Result<Value> {
    validate(args)?;
    change(config, "report-attempt", args)
}

fn validate(args: &[String]) -> Result<()> {
    if args
        .first()
        .is_none_or(|reference| reference.starts_with('-'))
    {
        return Err(fail(
            "report-attempt requires a task reference and --attempt UUID",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut at = 1;
    while at < args.len() {
        let key = args[at].as_str();
        if !seen.insert(key) {
            return Err(fail("report-attempt refuses duplicate options"));
        }
        match key {
            "--attempt" | "--considered" | "--project" if at + 1 < args.len() => at += 2,
            "--progress" | "--ready" | "--json" => at += 1,
            _ => return Err(fail("report-attempt accepts --attempt UUID, --progress|--ready, --considered REV and --json")),
        }
    }
    let _attempt = option(args, "--attempt")?
        .filter(|id| zerus_replica::ids::is_uuid(id))
        .ok_or_else(|| fail("report-attempt requires --attempt with a canonical UUID"))?;
    Ok(())
}

/// One shell word for every agent shell, never the hostile name (zerus-sa9qn.38).
pub(super) fn command(project: &str, task: &str, attempt: Option<&str>) -> String {
    let reference = zerus_replica::command_reference(project, task);
    match attempt {
        Some(id) => format!("hgs task report-attempt {reference} --attempt {id}"),
        None => format!("hgs task report {reference}"),
    }
}

/// This is called only by an explicit start/acknowledgement, not by an
/// unscoped report or its refusal message.
pub(super) fn started(out: &mut Value, task: &TaskDoc, project: &str) -> Result<()> {
    let view = task.view().map_err(fail)?;
    let current = view.execution.as_ref().and_then(|e| e.current.as_ref());
    let origin = current.filter(|current| !current.legacy);
    if let Some(origin) = origin {
        out["reporting"] = json!("attempt");
        out["report_attempt"] = json!(origin.id);
        out["report_command"] = json!(command(project, &task.id(), Some(&origin.id)));
    } else if !task.execution_managed() {
        out["reporting"] = json!("legacy");
        out["report_command"] = json!(command(project, &task.id(), None));
    } else {
        out["reporting"] = json!("history_only");
        out["report_hint"] = json!(LEGACY_TRANSITION_HINT);
    }
    Ok(())
}

/// Captured before the native request is hashed. The legacy admission can
/// still return an explicitly legacy acknowledgement; it never fabricates an
/// attempt. Another attempt's command from a later lookup cannot retarget this
/// pinned origin.
pub(super) fn pin(prompt: &mut String, project: &str, task: &str, attempt: &str) {
    prompt.push_str(&format!(
        "\n\n[Zerus report origin: {attempt}]\nReport this execution with `{}` \
         instead of any unscoped report instruction above \
         (add --progress while work continues). Keep this original attempt ID; never replace it \
         with a newer ID from task get or start. If the start acknowledgement explicitly says \
         this is a legacy execution with no attempt UUID, use the ordinary report command it \
         prints instead. A history-only report succeeded as a history write; it did not move \
         the task and must not be retried as a duplicate.",
        command(project, task, Some(attempt))
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_options_are_explicit_and_strict() {
        let id = "00000000-0000-4000-8000-000000000001";
        let args = |tail: &[&str]| tail.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(validate(&args(&[
            "orchard/T-234567",
            "--attempt",
            id,
            "--progress",
            "--json"
        ]))
        .is_ok());
        for bad in [
            args(&["orchard/T-234567"]),
            args(&["orchard/T-234567", "--attempt"]),
            args(&["orchard/T-234567", "--attempt", "latest"]),
            args(&["orchard/T-234567", "--attempt", id, "--attempt", id]),
            args(&["orchard/T-234567", "--attempt", id, "--extra"]),
        ] {
            assert!(validate(&bad).is_err(), "{bad:?}");
        }
    }

    /// What `/bin/sh` passes to `hgs` for each `hgs task` command written in
    /// backticks in `text`, each run in an empty folder: a payload that ran
    /// leaves `pwned` there.
    #[cfg(unix)]
    fn shell_calls(text: &str) -> Vec<Vec<String>> {
        let mut calls = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find("`hgs task ") {
            let command = &rest[start + 1..];
            let end = command.find('`').expect("a closed command");
            let folder = tempfile::tempdir().unwrap();
            let script = format!("hgs() {{ printf '%s\\0' \"$@\"; }}\n{}", &command[..end]);
            let out = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(script)
                .current_dir(folder.path())
                .env("PATH", "/usr/bin:/bin")
                .output()
                .unwrap();
            assert!(
                !folder.path().join("pwned").exists(),
                "{:?} ran a payload",
                &command[..end]
            );
            calls.push(
                String::from_utf8(out.stdout)
                    .unwrap()
                    .split_terminator('\0')
                    .map(str::to_owned)
                    .collect(),
            );
            rest = &command[end + 1..];
        }
        calls
    }

    /// zerus-sa9qn.38: every `hgs task` command an agent is handed names the
    /// task as one word its shell reads back exactly, or leaves the hostile
    /// project name out (the bare ID resolves in the agent's own project).
    #[cfg(unix)]
    #[test]
    fn agent_task_commands_never_carry_a_hostile_project_name() {
        const ID: &str = "00000000-0000-4000-8000-000000000001";
        let view = zerus_replica::TaskDoc::create(zerus_replica::NewTask {
            project: "p1",
            id: "T-234567",
            text: "Fix it",
            stage: zerus_replica::Stage::Todo,
            target: zerus_replica::Target::Any,
            author: zerus_replica::Actor::Person,
            rank: "V",
            now: 1,
        })
        .unwrap()
        .view()
        .unwrap();
        let settings = zerus_replica::Settings {
            task_template: "[Zerus task {ref}]".into(),
            ..Default::default()
        };
        for project in [
            "orchard",
            "my app && touch pwned",
            "x;touch pwned;#",
            "$(touch pwned)",
            "`touch pwned`",
            "a\" ; touch pwned ; \"b",
            "a|touch pwned",
            "x'; touch pwned; '",
            "x\u{2019}; touch pwned; \u{2019}",
            "x\\'; touch pwned; #",
            "x\ntouch pwned;#",
            "x\u{ff1b}touch pwned",
            "x\u{202e}; touch pwned",
        ] {
            let label = format!("{project}/T-234567");
            let named = if zerus_replica::command_word(&label).is_some() {
                label.as_str()
            } else {
                "T-234567"
            };
            let attempt = |verb: &str| {
                vec![
                    "task".to_owned(),
                    verb.into(),
                    named.into(),
                    "--attempt".into(),
                    ID.into(),
                ]
            };
            let plain = |verb: &str| vec!["task".to_owned(), verb.into(), named.into()];
            let report = format!("`{}`", command(project, "T-234567", Some(ID)));
            assert_eq!(
                shell_calls(&report),
                [attempt("report-attempt")],
                "{project:?}"
            );
            assert_eq!(
                shell_calls(&format!("`{}`", command(project, "T-234567", None))),
                [plain("report")],
                "{project:?}"
            );
            let mut pinned = String::new();
            super::super::controls::pin_begin(&mut pinned, project, "T-234567", ID);
            pin(&mut pinned, project, "T-234567", ID);
            assert_eq!(
                shell_calls(&pinned),
                [attempt("begin-attempt"), attempt("report-attempt")],
                "{project:?}"
            );
            let feedback =
                super::super::messages::prompt(project, "T-234567", "m1", "reopen", "Again", None);
            assert!(
                feedback.starts_with(&format!("[Zerus task {label} message m1]")),
                "{feedback}"
            );
            assert_eq!(
                shell_calls(&feedback),
                [plain("get"), plain("report")],
                "{project:?}"
            );
            let (dispatched, _) =
                super::super::dispatch_content(&settings, &view, project, true).unwrap();
            assert!(
                dispatched.starts_with(&format!("[Zerus task {label}]")),
                "{dispatched}"
            );
            let mut worker = plain("worker");
            worker.extend(
                [
                    "--subagent",
                    "CHILD_ID",
                    "--request-id",
                    "CURRENT_SENT_REQUEST_ID",
                ]
                .map(String::from),
            );
            assert_eq!(
                shell_calls(&dispatched),
                [plain("get"), worker],
                "{project:?}"
            );
        }
    }
}
