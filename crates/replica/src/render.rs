//! The prompts Zerus pastes: the task prompt and the cancel message.
use crate::schema::{check_controls, Actor, TaskView, MAX_TEXT};
use crate::{Error, Result};

/// How the prompt names the task tools: the MCP server, or `hgs task`
/// commands for an agent without it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tools {
    Mcp,
    Cli,
}

/// One word that POSIX shells (sh, bash, zsh, fish, Git Bash) and PowerShell
/// all read as exactly `text`: bare when it is plain, else in single quotes,
/// which none of them expands. None when one of them reads a character of it
/// inside single quotes (`'`, the quotes U+2018 to U+201B that PowerShell
/// also closes on, a backslash for fish), a backtick would end the Markdown
/// code span the command is written in, or it would hide how the command
/// reads (control and invisible formatting characters) (zerus-sa9qn.38).
pub fn command_word(text: &str) -> Option<String> {
    let hidden = |c: char| {
        c.is_control()
            || matches!(c, '\u{ad}' | '\u{61c}' | '\u{180e}' | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}')
    };
    if text.is_empty()
        || text
            .chars()
            .any(|c| matches!(c, '\'' | '\\' | '`' | '\u{2018}'..='\u{201b}') || hidden(c))
    {
        return None;
    }
    let plain = !text.starts_with('-')
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:-".contains(c));
    Some(if plain {
        text.to_owned()
    } else {
        format!("'{text}'")
    })
}

/// The task an `hgs task` command names for an agent: `PROJECT/T-xxxxxx` as
/// one `command_word`. A project name, which is free text, with no such word
/// leaves the bare task ID, which `hgs task` resolves in the agent's own
/// project and otherwise refuses; `<ref>`, never run as written, when the ID
/// is not plain either. The hostile name never reaches the command.
pub fn command_reference(project: &str, task: &str) -> String {
    command_word(&format!("{project}/{task}"))
        .or_else(|| command_word(task).filter(|word| word == task))
        .unwrap_or_else(|| "<ref>".into())
}

fn tool(tools: Tools, name: &str, reference: &str) -> String {
    match tools {
        Tools::Mcp => format!("zerus.{name}"),
        Tools::Cli => format!(
            "`hgs task {} {reference}`",
            name.trim_start_matches("task_")
        ),
    }
}

/// Substitutes `{name}` placeholders once; substituted text is not scanned
/// again, so a prompt that contains `{ref}` stays as written. `command` is
/// the `command_reference` the tool commands name.
fn fill(
    template: &str,
    tools: Tools,
    command: &str,
    value: impl Fn(&str) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let key = &after[..end];
        let tail = &after[end + 1..];
        if key == "tools" {
            if let Some(next) = ["task_report", "task_get"]
                .iter()
                .find(|name| tail.starts_with(&format!(".{name}")))
            {
                out.push_str(&tool(tools, next, command));
                rest = &tail[next.len() + 1..];
                continue;
            }
            out.push_str(match tools {
                Tools::Mcp => "zerus",
                Tools::Cli => "`hgs task`",
            });
            rest = tail;
            continue;
        }
        match value(key) {
            Some(text) => out.push_str(&text),
            None => out.push_str(&rest[start..start + end + 2]),
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

fn checked(text: String) -> Result<String> {
    if text.len() > MAX_TEXT {
        return Err(Error::new(
            "the rendered prompt is longer than 64 KiB; shorten the task text",
        ));
    }
    if text.starts_with('/') || text.starts_with('!') {
        return Err(Error::new("a prompt cannot start with / or !"));
    }
    check_controls(&text, "rendered prompt")?;
    Ok(text)
}

fn author(task: &TaskView) -> String {
    match &task.author {
        Actor::Person => "the person".into(),
        Actor::Agent(key) => format!("the agent of {}", key.session),
    }
}

/// `project` is the project's current name, shown in the reference.
fn render_task_prompt(
    template: &str,
    task: &TaskView,
    project: &str,
    tools: Tools,
) -> Result<String> {
    let reference = format!("{project}/{}", task.id);
    // `{ref}` is a label; the commands name the task as one safe word.
    let command = command_reference(project, &task.id);
    let sent_before = task.log.iter().any(|entry| entry.kind == "sent");
    let history = if sent_before {
        format!(
            "This task was sent before; read its history with {} before you start.",
            tool(tools, "task_get", &command)
        )
    } else {
        String::new()
    };
    let mut prompt = fill(template, tools, &command, |key| match key {
        "ref" => Some(reference.clone()),
        "author" => Some(author(task)),
        "prompt" => Some(if task.title.is_empty() {
            task.text.clone()
        } else if task.text.is_empty() {
            task.title.clone()
        } else {
            format!("{}\n\n{}", task.title, task.text)
        }),
        "history" => Some(history.clone()),
        _ => None,
    });
    for message in task
        .log
        .iter()
        .filter(|e| e.kind == "message" && e.delivery.as_ref().is_some_and(|d| d.state == "queued"))
    {
        prompt.push_str(&format!(
            "\n\n[Task clarification {}]\n{}",
            message.request_id.as_deref().unwrap_or_default(),
            message.text
        ));
    }
    checked(prompt)
}

/// Scope attachment labels to their original message, then flatten the explicit
/// dispatch snapshot. Two queued images both named [Image #1] remain distinct.
pub fn task_dispatch_content(
    template: &str,
    task: &TaskView,
    project: &str,
    tools: Tools,
) -> Result<(String, Vec<crate::TaskAttachment>)> {
    let mut task = task.clone();
    let queued = |e: &&mut crate::LogEntry| {
        e.kind == "message" && e.delivery.as_ref().is_some_and(|d| d.state == "queued")
    };
    let renumber = task.log.iter().any(|e| {
        e.kind == "message"
            && e.delivery.as_ref().is_some_and(|d| d.state == "queued")
            && !e.attachments.is_empty()
    });
    let mut next = 1;
    let rewrite = |text: &mut String, files: &mut Vec<crate::TaskAttachment>, next: &mut usize| {
        let mut labels = std::collections::BTreeMap::new();
        for file in files {
            let label = format!(
                "[{} #{}]",
                if file.mime.starts_with("image/") {
                    "Image"
                } else {
                    "File"
                },
                next
            );
            *next += 1;
            if !file.reference.is_empty() {
                labels.insert(file.reference.clone(), label.clone());
            }
            file.reference = label;
        }
        let original = std::mem::take(text);
        let mut rest = original.as_str();
        while let Some(start) = rest.find('[') {
            text.push_str(&rest[..start]);
            rest = &rest[start..];
            if let Some(end) = rest.find(']') {
                let token = &rest[..=end];
                text.push_str(labels.get(token).map(String::as_str).unwrap_or(token));
                rest = &rest[end + 1..];
            } else {
                break;
            }
        }
        text.push_str(rest);
    };
    if renumber {
        rewrite(&mut task.text, &mut task.attachments, &mut next);
    }
    let mut files = task.attachments.clone();
    for message in task.log.iter_mut().filter(queued) {
        if renumber {
            rewrite(&mut message.text, &mut message.attachments, &mut next);
        }
        files.extend(message.attachments.clone());
    }
    crate::attachment::check_attachments(&files)?;
    Ok((render_task_prompt(template, &task, project, tools)?, files))
}

pub fn task_prompt(template: &str, task: &TaskView, project: &str, tools: Tools) -> Result<String> {
    task_dispatch_content(template, task, project, tools).map(|(prompt, _)| prompt)
}

pub fn cancel_message(
    template: &str,
    task: &TaskView,
    project: &str,
    tools: Tools,
) -> Result<String> {
    let reference = format!("{project}/{}", task.id);
    let command = command_reference(project, &task.id);
    checked(fill(template, tools, &command, |key| match key {
        "ref" => Some(reference.clone()),
        "author" => Some(author(task)),
        _ => None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{DEFAULT_CANCEL_TEMPLATE, DEFAULT_TASK_TEMPLATE};
    use crate::schema::{SessionKey, Stage, Target};
    use crate::task::{NewTask, TaskDoc};

    fn view() -> TaskView {
        TaskDoc::create(NewTask {
            project: "p1",
            id: "T-a1b2c3",
            text: "Fix {ref} handling",
            stage: Stage::Todo,
            target: Target::Any,
            author: Actor::Person,
            rank: "V",
            now: 1,
        })
        .unwrap()
        .view()
        .unwrap()
    }

    #[test]
    fn named_worker_prompts_include_both_title_and_description() {
        let mut task = view();
        task.title = "Independent {ref} title".into();
        task.text = "Detailed content".into();
        let text = task_prompt(DEFAULT_TASK_TEMPLATE, &task, "orchard", Tools::Cli).unwrap();
        assert!(text.contains("Independent {ref} title\n\nDetailed content"));
        task.text.clear();
        assert!(
            task_prompt(DEFAULT_TASK_TEMPLATE, &task, "orchard", Tools::Cli)
                .unwrap()
                .contains("Independent {ref} title")
        );
    }

    #[test]
    fn a_rendered_prompt_refuses_terminal_control_characters() {
        let template = format!("{DEFAULT_TASK_TEMPLATE}\x07");
        let error = task_prompt(&template, &view(), "orchard", Tools::Cli).unwrap_err();
        assert!(error.to_string().contains("control characters"), "{error}");
    }

    #[test]
    fn first_send_prompt() {
        let text = task_prompt(DEFAULT_TASK_TEMPLATE, &view(), "orchard", Tools::Mcp).unwrap();
        assert!(text.starts_with("[Zerus task orchard/T-a1b2c3]\nWritten by the person.\nFix {ref} handling\n\nWhen you finish"), "{text}");
        assert!(text.contains("report with\nzerus.task_report: one to three"));
    }

    #[test]
    fn later_send_points_to_the_history_and_cli_names_commands() {
        let mut doc = TaskDoc::create(NewTask {
            project: "p1",
            id: "T-a1b2c3",
            text: "Fix it",
            stage: Stage::Todo,
            target: Target::Any,
            author: Actor::Agent(SessionKey {
                machine: "m".into(),
                session: "codex/api".into(),
                ..Default::default()
            }),
            rank: "V",
            now: 1,
        })
        .unwrap();
        doc.record_send(
            &SessionKey {
                machine: "m".into(),
                session: "codex/api".into(),
                ..Default::default()
            },
            "r",
            &Actor::Person,
            2,
        )
        .unwrap();
        let text = task_prompt(
            DEFAULT_TASK_TEMPLATE,
            &doc.view().unwrap(),
            "orchard",
            Tools::Cli,
        )
        .unwrap();
        assert!(text.contains("Written by the agent of codex/api."));
        assert!(text.contains("This task was sent before; read its history with `hgs task get orchard/T-a1b2c3` before you start."));
        assert!(text.contains("report with\n`hgs task report orchard/T-a1b2c3`: one to three"));
        let cancel = cancel_message(
            DEFAULT_CANCEL_TEMPLATE,
            &doc.view().unwrap(),
            "orchard",
            Tools::Cli,
        )
        .unwrap();
        assert!(cancel.starts_with("[Zerus task orchard/T-a1b2c3 cancelled]\nThe person cancelled"));
    }

    #[test]
    fn command_words_read_the_same_in_every_agent_shell() {
        for (text, word) in [
            ("orchard/T-a1b2c3", Some("orchard/T-a1b2c3")),
            ("my app/T-a1b2c3", Some("'my app/T-a1b2c3'")),
            (
                "x;touch pwned;#/T-a1b2c3",
                Some("'x;touch pwned;#/T-a1b2c3'"),
            ),
            ("$(touch pwned)/T-a1b2c3", Some("'$(touch pwned)/T-a1b2c3'")),
            ("a\" ; b/T-a1b2c3", Some("'a\" ; b/T-a1b2c3'")),
            ("x\u{ff1b}touch/T-a1b2c3", Some("'x\u{ff1b}touch/T-a1b2c3'")),
            ("~root/T-a1b2c3", Some("'~root/T-a1b2c3'")),
            ("=ls/T-a1b2c3", Some("'=ls/T-a1b2c3'")),
            ("-x/T-a1b2c3", Some("'-x/T-a1b2c3'")),
            ("a,b@c/T-a1b2c3", Some("'a,b@c/T-a1b2c3'")),
            ("", None),
            ("Bob's app", None),
            ("x\u{2019};calc;\u{2019}", None),
            ("x\u{2018}", None),
            ("x\u{201a}", None),
            ("x\u{201b}", None),
            ("a\\b", None),
            ("`touch pwned`", None),
            ("x\ntouch pwned", None),
            ("x\rb", None),
            ("ab\u{202e}c", None),
            ("a\u{200b}b", None),
            ("a\u{2066}b", None),
            ("a\u{2028}b", None),
            ("a\u{feff}", None),
        ] {
            assert_eq!(command_word(text).as_deref(), word, "{text:?}");
        }
        assert_eq!(command_reference("orchard", "T-a1b2c3"), "orchard/T-a1b2c3");
        assert_eq!(command_reference("my app", "T-a1b2c3"), "'my app/T-a1b2c3'");
        for name in [
            "Bob's app",
            "x\u{2019};calc",
            "a\\b",
            "`x`",
            "x\ny",
            "a\u{202e}b",
        ] {
            assert_eq!(command_reference(name, "T-a1b2c3"), "T-a1b2c3", "{name:?}");
        }
        assert_eq!(command_reference("Bob's", "T a"), "<ref>");
    }

    /// What `/bin/sh` passes to `hgs` for each `hgs task` command written in
    /// backticks in `text`, each run in an empty folder; a payload that ran
    /// leaves `pwned` there.
    #[cfg(unix)]
    fn shell_calls(text: &str) -> Vec<Vec<String>> {
        let mut calls = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find("`hgs task ") {
            let command = &rest[start + 1..];
            let end = command.find('`').expect("a closed command");
            let folder = tempfile::tempdir().unwrap();
            let out = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(format!(
                    "hgs() {{ printf '%s\\0' \"$@\"; }}\n{}",
                    &command[..end]
                ))
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

    #[cfg(unix)]
    #[test]
    fn task_commands_name_a_hostile_project_as_one_shell_word() {
        let mut doc = TaskDoc::create(NewTask {
            project: "p1",
            id: "T-a1b2c3",
            text: "Fix it",
            stage: Stage::Todo,
            target: Target::Any,
            author: Actor::Person,
            rank: "V",
            now: 1,
        })
        .unwrap();
        let key = SessionKey {
            machine: "m".into(),
            session: "codex/api".into(),
            ..Default::default()
        };
        doc.record_send(&key, "r", &Actor::Person, 2).unwrap();
        let view = doc.view().unwrap();
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
            let label = format!("{project}/T-a1b2c3");
            let named = if command_word(&label).is_some() {
                label.clone()
            } else {
                "T-a1b2c3".to_owned()
            };
            let text = task_prompt(DEFAULT_TASK_TEMPLATE, &view, project, Tools::Cli).unwrap();
            assert!(
                text.starts_with(&format!("[Zerus task {label}]\n")),
                "{text}"
            );
            let calls = shell_calls(&text);
            assert_eq!(
                calls,
                [["task", "get", &named], ["task", "report", &named]],
                "{project:?}: {text}"
            );
            let cancel = cancel_message(
                "[Zerus task {ref} cancelled] Report with {tools}.task_report.",
                &view,
                project,
                Tools::Cli,
            )
            .unwrap();
            assert_eq!(
                shell_calls(&cancel),
                [["task", "report", &named]],
                "{project:?}"
            );
        }
    }

    #[test]
    fn limits() {
        let mut big = view();
        big.text = "x".repeat(64 * 1024);
        assert!(task_prompt(DEFAULT_TASK_TEMPLATE, &big, "orchard", Tools::Mcp).is_err());
        assert!(task_prompt("/{prompt}", &view(), "orchard", Tools::Mcp).is_err());
        assert_eq!(
            task_prompt("[Zerus task {unknown} {ref", &view(), "o", Tools::Mcp).unwrap(),
            "[Zerus task {unknown} {ref"
        );
    }
}
