//! `hgs task`: the project task tracker on this computer. Tasks are Loro
//! documents kept by `zerus-replica` below `<state dir>/replica`; every
//! command prints JSON. See docs/superpowers/specs/2026-10-13-project-task-queue-design.md
//! and docs/superpowers/specs/2026-10-18-task-replication-loro-design.md.
//!
//! Lock order: catalog lookups (project, machine identity) happen before the
//! project store is opened, and stdin is read before it too; the hgs state lock
//! (taken when pasting) is only ever taken while holding the project lock.
use crate::cli::{Error, Result};
use crate::config::Config;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};
use zerus_replica::{
    cancel_message, task_dispatch_content, Actor, Position, ProjectStore, SessionKey, Settings,
    Stage, Target, TaskAttachment, TaskDoc, Tools, MAX_TEXT,
};

mod answers;
mod cancellation;
mod dispatch;
mod mobile;
mod phone;
mod sessions;
mod messages;
mod history;

/// A send that pasted the prompt but could not record it: its outcome is
/// uncertain, and the phone's receipt says so.
const SENT_UNRECORDED: &str = "the prompt was sent, but the task could not record it";

/// Whether a failed send may have pasted its prompt: it was sent but not
/// recorded, or its delivery is uncertain (the paste began, Enter or the
/// receipt did not follow). Never "nothing was sent" then.
fn paste_uncertain(message: &str) -> bool {
    message.starts_with(SENT_UNRECORDED) || message.starts_with("delivery uncertain")
}

const USAGE: &str = "usage: hgs task list --project P [--stage S] | get REF [--history-limit 1..80 [--history-before CURSOR]] | add --project P [--backlog] [--start] [--target SESSION|any [--host ALIAS]] [--title TITLE [--request-id UUID]] [--position N] | edit REF | rename REF | move REF --top|--before REF|--after REF|--position N | stage REF backlog|todo|done | target REF SESSION|any|new-session|subagent [--parent SESSION] [--host ALIAS] | send REF [--session SESSION [--host ALIAS]] | start-plan REF --json | start-deliver REF --json [--recover-only] | cancel REF [--reason TEXT] | cancel-execution REF --json (guard JSON on stdin) | handoff REF --session SESSION [--host ALIAS] | start REF | report REF | comment REF [--content-json] | message REF --request-id UUID --mode queue|send|reopen|note [--expect-dispatch ID] [--content-json] | worker REF --subagent ID --request-id REQUEST | settings --project P [--set] | digests --json | exchange --json | mobile-send|mobile-cancel|mobile-done --json
  REF is T-xxxxxx with --project P, or PROJECT/T-xxxxxx; texts and --set settings are read from stdin;
  a change an agent makes prints one line, and --json prints the task";

/// What Claude Code and Codex sessions read about the project's tasks each time
/// their SessionStart hooks run (spec "Instructions at session start").
const AGENT_INSTRUCTIONS: &str = concat!(
    "Zerus tasks are this project's shared task list. Run `hgs task` in your shell: it acts as your session, on your session's project; `hgs guide tasks` lists the commands. Texts go on stdin, for example hgs task report <ref> <<'EOF' ... EOF.\n\n",
    "Filing. When the person says \"поставь таск\", \"добавь задачу\", \"запиши задачу\", \"в очередь\", \"на потом\", \"file a task\", \"add to the queue\" or \"for later\", file it with `hgs task add`. Never put these into TodoWrite or your own plan. For work you are already doing, use `hgs task add --start` or `hgs task start <ref>`; this records In progress without sending a duplicate message. Plain add only queues later work. New tasks go to the end of To do unless the person names a place (--backlog, --position N).\n\n",
    "Working. A message starting \"[Zerus task <ref>]\" is a task for you; do it like any request from the person. When the prompt says the task was sent before, read its history with `hgs task get <ref>` before you start. Acknowledge a dispatched task with `hgs task start <ref>` before working; this does not send it again. For a subagent plan, register the native worker first. If you need the person, ask in this conversation as usual. When the work is done, or you find that it cannot be done, report with `hgs task report <ref>`: what you changed, how you checked it, and what is left undone or why it cannot be done.\n\n",
    "Cancelled. \"[Zerus task <ref> cancelled]\" means stop that task now and follow the message.\n\n",
    "Managing. `hgs task list` and `hgs task get` show tasks and their history; move, stage, target, edit, send and cancel change them as the person can. Accept a task (`hgs task stage <ref> done`) only when the person asks you to.\n\n",
    "Comments. `hgs task comment <ref>` adds a note for people and agents; it is never pasted into a session."
);

/// The SessionStart hook output that gives a session of `agent` the task
/// instructions; other agents get them once their hooks are reviewed.
pub(crate) fn session_context(agent: &str) -> Option<Value> {
    matches!(agent, "claude" | "codex").then(|| {
        json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": AGENT_INSTRUCTIONS}})
    })
}

fn fail(message: impl std::fmt::Display) -> Error {
    Error::new(1, message.to_string())
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn stdin_text(what: &str) -> Result<String> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_TEXT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(fail)?;
    if bytes.len() > MAX_TEXT {
        return Err(fail(format!("the {what} is longer than 64 KiB")));
    }
    String::from_utf8(bytes).map_err(|_| fail(format!("the {what} is not UTF-8")))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Content {
    text: String,
    #[serde(default)]
    attachments: Option<Vec<TaskAttachment>>,
}

fn stdin_content(args: &[String]) -> Result<Content> {
    if !flag(args, "--content-json") { return Ok(Content { text: stdin_text("prompt")?, attachments: None }); }
    let mut bytes = Vec::new();
    std::io::stdin().take(29 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(fail)?;
    if bytes.len() > 29 * 1024 * 1024 { return Err(fail("task content exceeds 29 MiB")); }
    // WhiteWh fork (H2): PowerShell 5.1 pipes JSON with a UTF-8 BOM.
    serde_json::from_slice(crate::state::without_bom(&bytes)).map_err(fail)
}

/// Refuses an agent's empty text with where it goes: agents tend to pass texts
/// as arguments. The person's tools validate first and read the task's own
/// refusal.
fn agent_text(text: &str, action: &str, who: &Actor) -> Result<()> {
    if !matches!(who, Actor::Agent(_)) || !text.trim().is_empty() {
        return Ok(());
    }
    let (what, example) = match action {
        "add" => ("prompt", "add"),
        "edit" => ("prompt", "edit REF"),
        "rename" => ("title", "rename REF"),
        "comment" => ("comment", "comment REF"),
        _ => ("report", "report REF"),
    };
    Err(fail(format!(
        "the {what} is empty; give it on stdin, for example: hgs task {example} <<'EOF'"
    )))
}

fn option<'a>(args: &'a [String], name: &str) -> Result<Option<&'a str>> {
    match args.iter().position(|a| a == name) {
        Some(i) => args
            .get(i + 1)
            .map(|v| Some(v.as_str()))
            .ok_or_else(|| fail(format!("{name} needs a value"))),
        None => Ok(None),
    }
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

/// The project's catalog ID and current name, by ID or name.
fn project(config: &Config, wanted: &str) -> Result<(String, String)> {
    let projects = crate::swarm::projects(config).map_err(fail)?;
    if let Some(found) = projects.iter().find(|(id, _)| id == wanted) {
        return Ok(found.clone());
    }
    let named: Vec<_> = projects.iter().filter(|(_, name)| name == wanted).collect();
    match named.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(fail(format!("no project {wanted:?} in the catalog"))),
        _ => Err(fail(format!(
            "{wanted:?} names more than one project; use its ID"
        ))),
    }
}

/// The project a command works on when it names none: the calling agent's
/// session's catalog project. The person is told `missing`.
fn own_project(config: &Config, who: &Actor, missing: &str) -> Result<(String, String)> {
    let Actor::Agent(own) = who else {
        return Err(fail(missing));
    };
    crate::swarm::session_project(config, &own.session)
        .map_err(fail)?
        .ok_or_else(|| {
            fail(format!(
                "{} belongs to no project; name one: PROJECT/T-xxxxxx or --project P",
                own.session
            ))
        })
}

/// `--project P`, or the calling agent's own project.
fn project_arg(config: &Config, args: &[String], who: &Actor) -> Result<(String, String)> {
    match option(args, "--project")? {
        Some(wanted) => project(config, wanted),
        None => own_project(config, who, USAGE),
    }
}

/// A task reference: `PROJECT/T-xxxxxx`, or `T-xxxxxx` (or a UUID) with
/// `--project`, or of the calling agent's own project.
struct Reference {
    project: (String, String),
    task: String,
}

fn reference(config: &Config, args: &[String], who: &Actor) -> Result<Reference> {
    let value = args.first().ok_or_else(|| fail(USAGE))?;
    let (project_name, task) = match value.rsplit_once('/') {
        Some((project, task)) => (Some(project), task),
        None => (option(args, "--project")?, value.as_str()),
    };
    let found = match project_name {
        Some(name) => project(config, name)?,
        None => own_project(config, who, "name the project: PROJECT/T-xxxxxx or --project P")?,
    };
    Ok(Reference {
        project: found,
        task: task.to_owned(),
    })
}

fn store(config: &Config, project_id: &str) -> Result<ProjectStore> {
    ProjectStore::open(&config.state.join("replica"), project_id).map_err(fail)
}

/// This computer and the swarm members it verified itself, by SSH alias.
struct Machines {
    here: String,
    /// This computer's own names (`HGS_SELF`, else its hostname), which
    /// `hgs machine ls` reports as local and never lists as a peer.
    selves: Vec<String>,
    peers: BTreeMap<String, String>,
    bindings: Vec<Value>,
}

fn machines(config: &Config) -> Result<Machines> {
    let here = crate::swarm::local_node_id(config).map_err(fail)?;
    #[cfg(unix)]
    let peers = crate::swarm::verified_bindings(config, &here);
    #[cfg(not(unix))]
    let peers = BTreeMap::new();
    Ok(Machines { here, selves: config.selves.clone(), peers, bindings: crate::state::task_bindings().map_err(fail)? })
}

impl Machines {
    /// The computer `--host ALIAS` names, or this one without it or by one
    /// of its own names.
    fn node(&self, host: Option<&str>) -> Result<String> {
        match host.filter(|h| !h.is_empty()) {
            None => Ok(self.here.clone()),
            Some(alias) if self.selves.iter().any(|name| name == alias) => Ok(self.here.clone()),
            Some(alias) => self.peers.get(alias).cloned().ok_or_else(|| {
                fail(format!("{alias} is not a verified computer of this swarm; omit --host for this computer"))
            }),
        }
    }

    /// The alias of a verified member by its node.
    fn alias(&self, node: &str) -> Option<String> {
        self.peers.iter().find(|(_, id)| id.as_str() == node).map(|(alias, _)| alias.clone())
    }

    /// How callers name a computer: "" for this one, a verified member's
    /// alias, or null when this computer does not know it.
    fn host(&self, node: &str) -> Value {
        if node == self.here {
            return json!("");
        }
        self.peers
            .iter()
            .find(|(_, id)| id.as_str() == node)
            .map_or(Value::Null, |(alias, _)| json!(alias))
    }
}

/// The agent of the session that runs this command, or the person. A command
/// whose environment names a session (`HGS_SESSION`, `HGS_RUN_ID`) acts as its
/// agent only when it proves it as `hgs self` does: the run is live on this
/// computer and the command descends from its agent; the proof follows a rename
/// to the session's current name. A send or cancel another computer forwards
/// (`forwardable`) names its agent with `--agent NODE/SESSION`.
fn actor(here: &str, args: &[String], forwardable: bool) -> Result<Actor> {
    let var = |name| std::env::var(name).ok().filter(|v| !v.is_empty());
    if let Some(agent) = option(args, "--agent")? {
        if !forwardable {
            return Err(fail("only send and cancel take --agent"));
        }
        if var("HGS_SESSION").is_some() && var("HGS_RUN_ID").is_some() {
            return Err(fail("--agent is for commands another computer forwards; an agent acts as its own session"));
        }
        let (machine, session) = agent
            .split_once('/')
            .filter(|(machine, session)| uuid::Uuid::parse_str(machine).is_ok() && !session.is_empty())
            .ok_or_else(|| fail("--agent is NODE/SESSION, NODE being a computer's identity"))?;
        return Ok(Actor::Agent(SessionKey {
            machine: machine.to_owned(),
            session: session.to_owned(),
            session_id: option(args, "--agent-session-id")?.unwrap_or_default().into(),
        }));
    }
    let (Some(claimed), Some(_)) = (var("HGS_SESSION"), var("HGS_RUN_ID")) else {
        return Ok(Actor::Person);
    };
    let record = crate::state::session_graph::caller_record().ok_or_else(|| {
        fail(format!(
            "this command names the agent session {claimed}, but that run is not live on this computer or does not run it; run hgs task from the agent's own shell, or unset HGS_SESSION and HGS_RUN_ID to act as yourself"
        ))
    })?;
    Ok(Actor::Agent(SessionKey {
        machine: here.to_owned(),
        session: record["name"].as_str().unwrap_or(&claimed).to_owned(),
        session_id: sessions::identity(&record),
    }))
}

fn view(task: &TaskDoc, project_name: &str, with_log: bool, machines: &Machines) -> Result<Value> {
    let mut task_view = task.view().map_err(fail)?;
    let files = if with_log { Vec::new() } else {
        std::mem::take(&mut task_view.attachments).iter().map(TaskAttachment::metadata)
            .collect::<std::result::Result<Vec<_>, _>>().map_err(fail)?
    };
    let dispatch_id = task_view.execution_id();
    let mut value = serde_json::to_value(task_view).map_err(fail)?;
    value["dispatch_id"] = json!(dispatch_id);
    // A note or receipt changes no other list field. Its clock must change
    // the tasks-topic digest so the selected inspector can refresh.
    if !with_log { value["history_revision"] = json!(task.revision()); }
    label_view(value, project_name, with_log, &files, machines)
}

fn label_view(mut value: Value, project_name: &str, with_log: bool, files: &[Value], machines: &Machines) -> Result<Value> {
    value["ref"] = json!(format!("{project_name}/{}", value["id"].as_str().unwrap_or_default()));
    // Every session reference names its computer: the task's target, session and
    // author, and each history entry's session and actor.
    let name = |reference: &mut Value| {
        if let Some(machine) = reference["machine"].as_str().map(str::to_owned) {
            reference["host"] = machines.host(&machine);
            if let Ok(key) = serde_json::from_value::<SessionKey>(reference.clone()) {
                match machines.resolve(&key) {
                    Some(current) => {
                        reference["session"] = json!(current.session);
                        if !current.session_id.is_empty() { reference["session_id"] = json!(current.session_id); }
                    }
                    None => reference["resolved"] = json!(false),
                }
            }
        }
    };
    for key in ["target", "session", "author"] { name(&mut value[key]); }
    if value["target"]["parent"].is_object() { name(&mut value["target"]["parent"]); }
    if value["worker"]["parent"].is_object() { name(&mut value["worker"]["parent"]); }
    if let Some(log) = value["log"].as_array_mut() {
        for entry in log {
            name(&mut entry["session"]);
            name(&mut entry["actor"]);
        }
    }
    if !with_log {
        value.as_object_mut().expect("object").remove("log");
        if !files.is_empty() { value["attachments"] = json!(files); }
    }
    Ok(value)
}

/// The place of `task` in its list, counting from 1.
fn place(store: &ProjectStore, task: &TaskDoc) -> Result<usize> {
    let stage = task.stage().map_err(fail)?;
    let (tasks, _) = store.listing().map_err(fail)?;
    Ok(tasks
        .iter()
        .filter(|t| t.stage().ok() == Some(stage))
        .position(|t| t.uuid() == task.uuid())
        .map_or(0, |i| i + 1))
}

/// `out` with the line an agent reads (`answer`), for a change an agent made.
fn with_answer(who: &Actor, action: &str, mut out: Value, place: usize, destination: &str) -> Value {
    if let Actor::Agent(own) = who {
        out["answer"] = json!(answers::answer(action, &out, place, &own.session, destination));
    }
    out
}

pub fn dispatch(config: &Config, args: &[String], dry: bool) -> Result<i32> {
    if dry {
        return Err(fail("task commands do not support --dry-run"));
    }
    // The phone's commands refuse in JSON, which the connector passes on.
    let phone = ["exchange", "mobile-send", "mobile-cancel", "mobile-done"];
    if args.first().is_some_and(|action| phone.contains(&action.as_str())) && flag(args, "--json") {
        return Ok(match execute(config, args) {
            Ok(out) => {
                println!("{out}");
                0
            }
            Err(error) => {
                println!("{}", refusal(&error.message));
                1
            }
        });
    }
    let out = execute(config, args)?;
    // An agent reads one line saying what happened; --json prints the task.
    match out.get("answer").and_then(Value::as_str) {
        Some(line) if !flag(args, "--json") => println!("{line}"),
        _ => println!("{out}"),
    }
    Ok(0)
}

/// A phone command's refusal, which the connector passes on: `uncertain`
/// when the prompt may be in the session already.
fn refusal(message: &str) -> Value {
    let mut refusal = json!({"schema": 1, "ok": false, "error": message});
    if paste_uncertain(message) {
        refusal["uncertain"] = json!(true);
    }
    refusal
}

/// `hgs task list --project PROJECT --json`, for `hgs watch`.
pub(crate) fn listing(config: &Config, project: &str) -> Result<Value> {
    list(config, &["--project".to_owned(), project.to_owned()])
}

/// The folder whose files hold a project's tasks: a change below it changes
/// the project's listing.
pub(crate) fn store_dir(config: &Config, project_id: &str) -> std::path::PathBuf {
    use sha2::Digest;
    config.state.join("replica").join(format!("{:x}", sha2::Sha256::digest(project_id.as_bytes())))
}

/// Runs one task command and returns its JSON.
fn execute(config: &Config, args: &[String]) -> Result<Value> {
    let Some((action, args)) = args.split_first() else {
        return Err(fail(USAGE));
    };
    match action.as_str() {
        "list" => list(config, args),
        "get" => {
            let machines = machines(config)?;
            let who = actor(&machines.here, args, false)?;
            let r = reference(config, args, &who)?;
            let limit = history::limit(args)?;
            let store = store(config, &r.project.0)?;
            let value = if let Some(limit) = limit {
                let uuid = if zerus_replica::ids::is_uuid(&r.task) { r.task.clone() }
                    else { store.find(&r.task).map_err(fail)?.uuid().to_owned() };
                let task = history::page(&store, &store_dir(config, &r.project.0), &uuid,
                    limit, option(args, "--history-before")?)?;
                label_view(task, &r.project.1, true, &[], &machines)?
            } else {
                view(&store.find(&r.task).map_err(fail)?, &r.project.1, true, &machines)?
            };
            Ok(json!({"project": {"id": r.project.0, "name": r.project.1}, "task": value}))
        }
        "add" => add(config, args),
        "message" => messages::execute(config, args),
        "cancel-execution" => {
            let guard = cancellation::read()?;
            change_guarded(config, "cancel", args, None, Some(&guard))
        }
        "start-plan" | "start-deliver" => dispatch::execute(config, action, args),
        "edit" | "rename" | "move" | "stage" | "target" | "comment" | "report" | "start" | "send" | "handoff" | "cancel" | "worker" => {
            change(config, action, args)
        }
        "settings" => settings(config, args),
        // The phone's exchange through the relay; both take exactly --json.
        "digests" | "exchange" if args.len() == 1 && args[0] == "--json" => {
            if action == "digests" {
                phone::digests(config)
            } else {
                phone::exchange(config)
            }
        }
        // The phone's Send now, Cancel and Done list; each takes exactly --json.
        "mobile-send" | "mobile-cancel" | "mobile-done" if args.len() == 1 && args[0] == "--json" => {
            match action.as_str() {
                "mobile-send" => mobile::send(config),
                "mobile-cancel" => mobile::cancel(config),
                _ => mobile::done(config),
            }
        }
        _ => Err(fail(USAGE)),
    }
}

fn list(config: &Config, args: &[String]) -> Result<Value> {
    let machines = machines(config)?;
    let who = actor(&machines.here, args, false)?;
    let (id, name) = project_arg(config, args, &who)?;
    let only = option(args, "--stage")?
        .map(Stage::parse)
        .transpose()
        .map_err(fail)?;
    // One load of the project; each stage is then a filter of the sorted list.
    let (tasks, damaged) = store(config, &id)?.listing().map_err(fail)?;
    let mut out = Vec::new();
    for stage in [
        Stage::Review,
        Stage::Active,
        Stage::Todo,
        Stage::Backlog,
        Stage::Done,
    ] {
        if only.is_some_and(|s| s != stage) {
            continue;
        }
        for task in tasks.iter().filter(|t| t.stage().ok() == Some(stage)) {
            out.push(view(task, &name, false, &machines)?);
        }
    }
    Ok(json!({"project": {"id": id, "name": name}, "tasks": out, "damaged": damaged}))
}

/// `--position N`, counting from 1, as a list index.
fn position_index(args: &[String]) -> Result<Option<usize>> {
    option(args, "--position")?
        .map(|p| {
            p.parse::<usize>()
                .ok()
                .filter(|p| *p >= 1)
                .map(|p| p - 1)
                .ok_or_else(|| fail("--position is a number from 1"))
        })
        .transpose()
}

fn add(config: &Config, args: &[String]) -> Result<Value> {
    let content = stdin_content(args)?;
    let text = content.text;
    let machines = machines(config)?;
    let author = actor(&machines.here, args, false)?;
    let title = option(args, "--title")?;
    if title.is_none() { agent_text(&text, "add", &author)?; }
    let (id, name) = project_arg(config, args, &author)?;
    let stage = if flag(args, "--backlog") {
        Stage::Backlog
    } else {
        Stage::Todo
    };
    // A task an agent files is always pinned to that agent's session.
    if option(args, "--host")?.is_some() && matches!(option(args, "--target")?, None | Some("any")) {
        return Err(fail("--host needs a --target session"));
    }
    let target = match (&author, option(args, "--target")?) {
        (Actor::Agent(_), Some(_)) => {
            return Err(fail(
                "a task an agent files is pinned to its own session; --target is for people",
            ))
        }
        (Actor::Agent(own), None) => Target::Session(own.clone()),
        (Actor::Person, None | Some("any")) => Target::Any,
        (Actor::Person, Some(session)) => Target::Session(machines.target(machines.node(option(args, "--host")?)?, session, args)?),
    };
    let position = position_index(args)?.map_or(Position::End, Position::Index);
    let store = store(config, &id)?;
    let (mut task, created) = if let Some(title) = title {
        store.new_named_task(title, &text, content.attachments.as_deref().unwrap_or_default(), stage, target, author.clone(), &position, &name, now(), option(args, "--request-id")?, flag(args, "--start")).map_err(fail)?
    } else {
        if option(args, "--request-id")?.is_some() { return Err(fail("--request-id requires --title")); }
        let mut task = store.new_task(&text, stage, target, author.clone(), &position, &name, now()).map_err(fail)?;
        if let Some(files) = content.attachments { task.set_attachments(&files, &author, now()).map_err(fail)?; }
        (task, true)
    };
    if created {
        if title.is_none() && flag(args, "--start") { task.record_start(&author, now()).map_err(fail)?; }
        store.save(&mut task).map_err(fail)?;
    }
    let at = place(&store, &task)?;
    Ok(with_answer(&author, "add", json!({"task": view(&task, &name, true, &machines)?}), at, ""))
}

fn change(config: &Config, action: &str, args: &[String]) -> Result<Value> {
    change_within(config, action, args, None)
}

/// `change`, waiting at most `wait` for a project another command holds:
/// the phone's Send now and Cancel are refused as busy before anything happens.
fn change_within(config: &Config, action: &str, args: &[String], wait: Option<std::time::Duration>) -> Result<Value> {
    change_guarded(config, action, args, wait, None)
}

fn change_guarded(config: &Config, action: &str, args: &[String], wait: Option<std::time::Duration>, guard: Option<&cancellation::Guard>) -> Result<Value> {
    let content = if action == "edit" || (action == "comment" && flag(args, "--content-json")) { Some(stdin_content(args)?) } else { None };
    let text = match action {
        "edit" => Some(content.as_ref().expect("edit content").text.clone()),
        "comment" => Some(match &content { Some(content) => content.text.clone(), None => stdin_text("comment")? }),
        "report" => Some(stdin_text("report")?),
        "rename" => Some(stdin_text("title")?.trim().to_owned()),
        _ => None,
    };
    let machines = machines(config)?;
    let here = machines.here.as_str();
    let who = actor(here, args, matches!(action, "send" | "handoff" | "cancel"))?;
    if let Some(text) = &text {
        if action != "edit" || !text.is_empty() { agent_text(text, action, &who)?; }
    }
    let r = reference(config, args, &who)?;
    let (project_id, project_name) = (&r.project.0, &r.project.1);
    // An agent reads a refused send as "SESSION cannot take it now: …"; a paste
    // that happened is never reworded. A command another computer forwards
    // (`--agent`) leaves the wording to that computer, where the agent reads it.
    let forwarded_here = option(args, "--agent")?.is_some();
    let refuse = |error: Error, to: &str| -> Error {
        if matches!(who, Actor::Agent(_))
            && !forwarded_here
            && action == "send"
            && !paste_uncertain(&error.message)
        {
            fail(answers::refused(to, &error.message))
        } else {
            error
        }
    };
    // Explicit worker registration inspects the native roster once, before the
    // project lock. Normal task listings never query providers for every row.
    let worker_proof = if action == "worker" {
        let Actor::Agent(own) = &who else { return Err(fail("worker is for the task's coordinator")); };
        let id = option(args, "--subagent")?.ok_or_else(|| fail("worker needs --subagent ID"))?;
        let request = option(args, "--request-id")?.ok_or_else(|| fail("worker needs the current --request-id"))?;
        let caller = crate::state::session_graph::caller_record().ok_or_else(|| fail("the coordinator run is no longer live"))?;
        let details = crate::state::reference_details(&own.session, "", "").map_err(fail)?;
        let worker = observed_worker(own, &caller, &details, id, request)?;
        Some(worker)
    } else { None };
    let store = match wait {
        None => store(config, project_id)?,
        Some(wait) => ProjectStore::open_within(&config.state.join("replica"), project_id, wait)
            .map_err(fail)?
            .ok_or_else(|| fail("the project's tasks are busy; try again"))?,
    };
    let mut task = store.find(&r.task).map_err(fail)?;
    if let Some(guard) = guard { guard.check(&task)?; }
    if action == "edit" && text.as_deref() == Some("") && task.title().is_empty() { agent_text("", action, &who)?; }
    if task.view().map_err(fail)?.writable {
        let view = task.view().map_err(fail)?;
        let target = match view.target { Target::Session(target) | Target::NewSession { parent: target } | Target::Subagent { parent: target } => Some(target), Target::Any => None };
        for key in target.into_iter().chain(view.session) {
            if let Some(current) = machines.resolve(&key).filter(|current| !current.session_id.is_empty()) {
                task.bind_session_identity(&key, &current).map_err(fail)?;
            }
        }
    }
    if let Some(expected) = option(args, "--expect-new-session-parent")? {
        if action != "send" || !matches!(task.view().map_err(fail)?.target, Target::NewSession { parent } if format!("{}/{}", parent.machine, parent.session_id) == expected) {
            return Err(fail("task assignment changed while the new executor was starting; nothing was sent"));
        }
    }
    // A send or cancel for a session another computer runs happens there.
    let session = match action {
        "handoff" => {
            task.check_handoff(&who).map_err(fail)?;
            let name = option(args, "--session")?.ok_or_else(|| fail("handoff needs --session SESSION [--host ALIAS]"))?;
            let to = machines.target(machines.node(option(args, "--host")?)?, name, args)?;
            if task.session().as_ref() == Some(&to) { return Err(fail("choose another session for the handoff")); }
            Some((to, false))
        }
        "send" => Some(send_session(&task, args, &who, &machines)?),
        // Only a task In progress is cancelled; this computer refuses anything else.
        "cancel" if task.stage().map_err(fail)? == Stage::Active => {
            task.view().map_err(fail)?.session.map(|session| (machines.resolve(&session).unwrap_or(session), false))
        }
        _ => None,
    };
    if let Some((session, _)) = session.as_ref().filter(|(session, _)| session.machine != here) {
        // An agent reads the session with its computer: "codex/api on mac".
        let to = match machines.alias(&session.machine) {
            Some(alias) => format!("{} on {alias}", session.session),
            None => session.session.clone(),
        };
        // This computer's copy refuses first: a task it shows In progress, or
        // in a stage that is not sent, never reaches the other computer.
        if action == "send" {
            task.check_send().map_err(|e| refuse(fail(e), &to))?;
        }
        let uuid = task.uuid().to_owned();
        drop(store);
        let out = match reach(config, &machines, project_id, session) {
            // That computer words its own refusals (see `forwarded`).
            Ok(alias) => forwarded(config, &machines, &r.project, &uuid, action, args, session, &alias, &who, guard)?,
            // A cancel goes through while that computer is out of reach: the task
            // moves to In review here and its session is not told.
            Err(reason) if action == "cancel" => cancel_here(config, &machines, &r.project, &uuid, args, &who, reason, guard)?,
            Err(reason) => return Err(refuse(fail(reason), &to)),
        };
        return Ok(with_answer(&who, action, out, 0, &to));
    }
    let text = text.unwrap_or_default();
    let mut extra = json!({});
    let mut destination = String::new();
    match action {
        "worker" => { task.record_worker(worker_proof.as_ref().expect("worker proof"), &who, now()).map_err(fail)?; task.record_start(&who, now()).map_err(fail)?; },
        "start" => task.record_start(&who, now()).map_err(fail)?,
        "rename" => { task.set_title(&text, &who, now()).map_err(fail)?; store.check_prompt(&task, project_name).map_err(fail)?; },
        "edit" => {
            store
                .edit_task(&mut task, &text, &who, project_name, now())
                .map_err(fail)?;
            if let Some(files) = content.and_then(|content| content.attachments) { task.set_attachments(&files, &who, now()).map_err(fail)?; }
        }
        "comment" => task.comment_with_attachments(&text, &content.and_then(|content| content.attachments).unwrap_or_default(), &who, now()).map_err(fail)?,
        "report" => {
            let moved = task.record_report(&text, &who, now()).map_err(fail)?;
            extra = json!({"moved_to_review": moved});
        }
        "move" => {
            let position = if flag(args, "--top") {
                Position::Top
            } else if let Some(anchor) = option(args, "--before")? {
                Position::Before(anchor.rsplit('/').next().unwrap_or(anchor).to_owned())
            } else if let Some(anchor) = option(args, "--after")? {
                Position::After(anchor.rsplit('/').next().unwrap_or(anchor).to_owned())
            } else if let Some(index) = position_index(args)? {
                Position::Index(index)
            } else {
                return Err(fail(USAGE));
            };
            store.reorder(&mut task, &position).map_err(fail)?;
        }
        "stage" => {
            let to = Stage::parse(args.get(1).ok_or_else(|| fail(USAGE))?).map_err(fail)?;
            store.change_stage(&mut task, to, &who, now()).map_err(fail)?;
        }
        "target" => {
            let value = args.get(1).ok_or_else(|| fail(USAGE))?;
            if value == "any" && option(args, "--host")?.is_some() {
                return Err(fail("--host needs a --target session"));
            }
            let target = if matches!(value.as_str(), "new-session" | "subagent") {
                let parent = option(args, "--parent")?.ok_or_else(|| fail("choose --parent SESSION for a new executor"))?;
                let parent = machines.target(machines.node(option(args, "--host")?)?, parent, args)?;
                if value == "new-session" { Target::NewSession { parent } } else { Target::Subagent { parent } }
            } else if value == "any" {
                Target::Any
            } else {
                Target::Session(machines.target(machines.node(option(args, "--host")?)?, value, args)? )
            };
            task.set_target(&target, &who, now()).map_err(fail)?;
        }
        "send" | "handoff" => {
            let (session, pin) = session.expect("a send has its session");
            destination = session.session.clone();
            extra = send(&store, &mut task, project_name, session, pin, &who, action == "handoff")
                .map_err(|e| refuse(e, &destination))?;
        }
        "cancel" => extra = cancel(&store, &machines, &mut task, project_name, cancellation::reason(args, guard)?, &who, None)?,
        _ => return Err(fail(USAGE)),
    }
    store.save(&mut task).map_err(|e| {
        // The prompt is in the session already: say so, never "refused".
        if matches!(action, "send" | "handoff") {
            fail(format!("{SENT_UNRECORDED}: {e}"))
        } else {
            fail(e)
        }
    })?;
    let at = if action == "move" { place(&store, &task)? } else { 0 };
    let mut out = json!({"task": view(&task, project_name, guard.is_none(), &machines)?});
    for (key, value) in extra.as_object().expect("object") {
        out[key] = value.clone();
    }
    Ok(with_answer(&who, action, out, at, &destination))
}

/// The session a send goes to, and whether it pins the task there: --session
/// (with --host on another computer; an agent's choice also pins the task),
/// the pinned session, or for an Any task the agent's own session.
fn send_session(task: &TaskDoc, args: &[String], who: &Actor, machines: &Machines) -> Result<(SessionKey, bool)> {
    let mut view = task.view().map_err(fail)?;
    if let Target::Session(pinned) = &view.target {
        view.target = Target::Session(machines.resolve(pinned).ok_or_else(|| fail("the task's session no longer exists; choose another session"))?);
    }
    let host = option(args, "--host")?.filter(|host| !host.is_empty());
    let named = |session: &str| -> Result<SessionKey> {
        machines.target(machines.node(host)?, session, args)
    };
    match (option(args, "--session")?, &view.target, who) {
        (None, ..) if host.is_some() => Err(fail("--host needs a --session")),
        (Some(name), Target::Session(pinned), Actor::Person) if *pinned != named(name)? => {
            // The same name on another computer is another session: say whose.
            let whose = if pinned.session == name && pinned.machine != machines.here {
                format!(" on {}", machines.alias(&pinned.machine).unwrap_or_else(|| "another computer".into()))
            } else {
                String::new()
            };
            Err(fail(format!("{} is pinned to {}{whose}; change its session first", task.id(), pinned.session)))
        }
        (Some(name), _, Actor::Person) => Ok((named(name)?, false)),
        (Some(name), _, Actor::Agent(_)) => Ok((named(name)?, true)),
        (None, Target::Session(pinned), _) => Ok((pinned.clone(), false)),
        (None, Target::Any, Actor::Agent(own)) => Ok((own.clone(), false)),
        (None, Target::Any, Actor::Person) => Err(fail("choose the session with --session")),
        (None, Target::NewSession { .. }, _) => Err(fail("start this task in Zerus to configure its new session")),
        (None, Target::Subagent { parent }, _) => {
            Ok((machines.resolve(parent).ok_or_else(|| fail("the coordinator session no longer exists"))?, false))
        }
    }
}

/// The alias of the computer that runs `session`, with this project's tasks
/// exchanged with it, or why it cannot be reached now.
fn reach(config: &Config, machines: &Machines, project: &str, session: &SessionKey) -> std::result::Result<String, String> {
    let alias = machines
        .alias(&session.machine)
        .ok_or_else(|| format!("{} is on a computer this one has not verified", session.session))?;
    crate::swarm::replicate(config, &alias, project).map_err(|e| format!("{e:#}"))?;
    Ok(alias)
}

/// Runs a send or cancel on the computer behind `alias`, which runs `session`
/// and now holds this project's tasks: it checks and records the action in its
/// own copy and pastes in its session; its copy then comes back.
#[allow(clippy::too_many_arguments)]
fn forwarded(
    config: &Config,
    machines: &Machines,
    project: &(String, String),
    uuid: &str,
    action: &str,
    args: &[String],
    session: &SessionKey,
    alias: &str,
    who: &Actor,
    guard: Option<&cancellation::Guard>,
) -> Result<Value> {
    // That computer answers in JSON even for an agent; this one answers the agent.
    let wire_action = if guard.is_some() { "cancel-execution" } else { action };
    let mut remote: Vec<String> = vec!["task".into(), wire_action.into(), uuid.into(), "--project".into(), project.0.clone(), "--json".into()];
    if matches!(action, "send" | "handoff") && option(args, "--session")?.is_some() {
        remote.extend(["--session".into(), session.session.clone()]);
        if !session.session_id.is_empty() { remote.extend(["--session-id".into(), session.session_id.clone()]); }
    }
    if let Some(expected) = option(args, "--expect-new-session-parent")? { remote.extend(["--expect-new-session-parent".into(), expected.into()]); }
    if let Some(reason) = option(args, "--reason")?.filter(|_| action == "cancel") {
        remote.extend(["--reason".into(), reason.into()]);
    }
    if let Actor::Agent(agent) = who {
        remote.extend(["--agent".into(), format!("{}/{}", agent.machine, agent.session)]);
        if !agent.session_id.is_empty() { remote.extend(["--agent-session-id".into(), agent.session_id.clone()]); }
    }
    let reply = match guard {
        Some(guard) => crate::swarm::remote_hgs_input(config, alias, &remote, &serde_json::to_value(guard).map_err(fail)?),
        None => crate::swarm::remote_hgs(config, alias, &remote),
    }.map_err(|e| {
        let message = format!("{e:#}");
        match message.strip_prefix(&format!("{alias}: hgs: ")) {
            // That computer's own refusal: it pasted nothing, and an agent reads
            // it as a send that was not taken.
            Some(refusal)
                if action == "send"
                    && matches!(who, Actor::Agent(_))
                    && !paste_uncertain(refusal) =>
            {
                fail(answers::refused(&format!("{} on {alias}", session.session), refusal))
            }
            Some(refusal) => fail(format!("{alias}: {refusal}")),
            // A connection that failed on the way may have failed after the
            // paste: never say that nothing was sent.
            None => fail(message),
        }
    })?;
    // Its copy comes back now (its identity was checked a moment ago); when
    // that fails the next sync brings it, and `synced` says the copy here is behind.
    let synced = match crate::swarm::exchange_tasks(config, alias, &project.0) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("hgs: {error:#}");
            false
        }
    };
    let task = store(config, &project.0)?.find(uuid).map_err(fail)?;
    let mut out = json!({"task": view(&task, &project.1, guard.is_none(), machines)?, "synced": synced});
    for key in ["receipt", "cancel_message"] {
        if let Some(value) = reply.get(key) {
            out[key] = value.clone();
        }
    }
    Ok(out)
}

/// Cancels here a task whose session's computer cannot be reached: it moves to
/// In review, its session is not told, and that computer's copy follows with
/// the next sync.
#[allow(clippy::too_many_arguments)]
fn cancel_here(
    config: &Config,
    machines: &Machines,
    project: &(String, String),
    uuid: &str,
    args: &[String],
    who: &Actor,
    reason: String,
    guard: Option<&cancellation::Guard>,
) -> Result<Value> {
    let store = store(config, &project.0)?;
    let mut task = store.find(uuid).map_err(fail)?;
    if let Some(guard) = guard { guard.check(&task)?; }
    let extra = cancel(&store, machines, &mut task, &project.1, cancellation::reason(args, guard)?, who, Some(reason))?;
    store.save(&mut task).map_err(fail)?;
    let mut out = json!({"task": view(&task, &project.1, guard.is_none(), machines)?});
    out["cancel_message"] = extra["cancel_message"].clone();
    Ok(out)
}

/// Pastes the task prompt into its session on this computer.
fn send(
    store: &ProjectStore,
    task: &mut TaskDoc,
    project_name: &str,
    session: SessionKey,
    pin: bool,
    who: &Actor,
    handoff: bool,
) -> Result<Value> {
    // Everything that could stop the send from being recorded is checked
    // before the paste; only I/O can fail after it.
    if handoff { task.check_handoff(who).map_err(fail)?; } else { task.check_send().map_err(fail)?; }
    let view = task.view().map_err(fail)?;
    let settings = store.project().map_err(fail)?.settings();
    let starts = store
        .agent_starts_since(now() - 3_600_000)
        .map_err(fail)?;
    settings
        .check_agent(who, &session, true, starts)
        .map_err(fail)?;
    if pin && view.target != Target::Session(session.clone()) {
        task.set_target(&Target::Session(session.clone()), who, now())
            .map_err(fail)?;
    }
    let (prompt, attachments) = dispatch_content(&settings, &task.view().map_err(fail)?, project_name, matches!(view.target, Target::Subagent { .. }))?;
    let receipt = crate::state::submit_task_message(&session, &prompt, &attachments).map_err(fail)?;
    let request_id = receipt["request_id"].as_str().unwrap_or_default().to_owned();
    (if handoff { task.record_handoff(&session, &request_id, who, now()) }
     else { task.record_send(&session, &request_id, who, now()) })
        .map_err(|e| fail(format!("{SENT_UNRECORDED}: {e}")))?;
    task.record_queued_messages(&request_id, who, now()).map_err(|e| fail(format!("{SENT_UNRECORDED}: {e}")))?;
    Ok(json!({"receipt": receipt}))
}

fn dispatch_content(settings: &Settings, view: &zerus_replica::TaskView, project_name: &str, subagent: bool) -> Result<(String, Vec<Value>)> {
    let (mut prompt, files) = task_dispatch_content(&settings.task_template, view, project_name, Tools::Cli).map_err(fail)?;
    if subagent {
        prompt.push_str(&format!("\n\n[Zerus executor: subagent]\nYou are the coordinator for this task. Create a native subagent only now, after this explicit start request, and give it the implementation work. Keep coordination and task reporting in this session. After the native child appears, read `hgs task get {}/{}` and register its exact live ID with `hgs task worker {}/{} --subagent CHILD_ID --request-id CURRENT_SENT_REQUEST_ID`. Do not claim that the child has started before this registration succeeds; if creation is unavailable, report that limitation. The child must not impersonate the coordinator's CLI identity.\n", project_name, view.id, project_name, view.id));
    }
    let attachments = files.iter().map(serde_json::to_value).collect::<std::result::Result<Vec<_>, _>>().map_err(fail)?;
    Ok((prompt, attachments))
}

/// Moves the task to In review and pastes the cancel message; the turn is not
/// stopped. `undelivered` says why the message cannot reach the session.
fn cancel(
    store: &ProjectStore,
    machines: &Machines,
    task: &mut TaskDoc,
    project_name: &str,
    reason: &str,
    who: &Actor,
    undelivered: Option<String>,
) -> Result<Value> {
    let stored_session = task
        .view()
        .map_err(fail)?
        .session
        .ok_or_else(|| fail("only a task In progress can be cancelled"))?;
    let resolved = machines.resolve(&stored_session);
    let session = resolved.as_ref().unwrap_or(&stored_session);
    if let Actor::Agent(own) = who {
        if own == session {
            return Err(fail(format!(
                "You cannot cancel {project_name}/{}, a task you were sent. If it cannot be done, report what is left undone.",
                task.id()
            )));
        }
    }
    let settings = store.project().map_err(fail)?.settings();
    settings
        .check_agent(who, session, false, 0)
        .map_err(fail)?;
    task.record_cancel(reason, who, now()).map_err(fail)?;
    store.save(task).map_err(fail)?;
    let message = cancel_message(
        &settings.cancel_template,
        &task.view().map_err(fail)?,
        project_name,
        Tools::Cli,
    )
    .map_err(fail)?;
    let pasted = match undelivered {
        Some(reason) => Err(reason),
        None if resolved.is_none() => Err("the task's session identity is no longer available".into()),
        None => crate::state::submit_task_message(session, &message, &[]).map_err(|e| e.to_string()),
    };
    Ok(match pasted {
        Ok(receipt) => json!({"cancel_message": {"sent": true, "receipt": receipt}}),
        Err(error) => {
            task.note(&format!("Cancel message not sent: {error}"), who, now())
                .map_err(fail)?;
            json!({"cancel_message": {"sent": false, "error": error}})
        }
    })
}

fn settings(config: &Config, args: &[String]) -> Result<Value> {
    let invalid = |e: &dyn std::fmt::Display| fail(format!("invalid settings: {e}"));
    let changes = if flag(args, "--set") {
        match serde_json::from_str(&stdin_text("settings")?).map_err(|e| invalid(&e))? {
            Value::Object(changes) => Some(changes),
            _ => return Err(invalid(&"expected a JSON object")),
        }
    } else {
        None
    };
    let who = actor(&machines(config)?.here, args, false)?;
    let (id, name) = project_arg(config, args, &who)?;
    let store = store(config, &id)?;
    let mut doc = store.project().map_err(fail)?;
    if let Some(changes) = changes {
        // Only the fields given change; the others keep their current values.
        let mut merged = serde_json::to_value(doc.settings()).map_err(fail)?;
        for (key, value) in changes {
            if merged.get(&key).is_none() {
                return Err(invalid(&format!("unknown field {key:?}")));
            }
            merged[key] = value;
        }
        let wanted: Settings = serde_json::from_value(merged).map_err(|e| invalid(&e))?;
        doc.set_settings(&wanted).map_err(fail)?;
        store.save_project(&doc).map_err(fail)?;
    }
    Ok(json!({"project": {"id": id, "name": name}, "settings": doc.settings()}))
}

fn observed_worker(own: &SessionKey, caller: &Value, details: &Value, id: &str, request: &str) -> Result<zerus_replica::TaskWorker> {
    let child = &details["subagents"][id];
    if sessions::identity(caller) != own.session_id || caller["name"].as_str() != Some(own.session.as_str())
        || caller["run_id"].as_str().is_none_or(str::is_empty) || details["run_id"] != caller["run_id"]
        || caller["conversation_id"].as_str().is_none_or(str::is_empty) || details["conversation_id"] != caller["conversation_id"]
        || child.is_null() || child["external"] == true || child["historical"] == true
        || !matches!(child["state"].as_str(), Some("working" | "running" | "waiting")) {
        return Err(fail("choose a confirmed live native subagent of this coordinator's current run and conversation"));
    }
    Ok(zerus_replica::TaskWorker { parent: own.clone(), id: id.into(),
        conversation_id: details["conversation_id"].as_str().unwrap().into(), provider: own.session.split('/').next().unwrap_or("").into(),
        name: child["name"].as_str().filter(|name| !name.is_empty()).unwrap_or(id).into(), request_id: request.into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_registration_requires_the_current_native_roster() {
        let own = SessionKey { machine: "node".into(), session: "codex/coordinator".into(), session_id: "launch_id:parent".into() };
        let caller = json!({"name":own.session,"run_id":"run","launch_id":"parent","conversation_id":"conversation"});
        let details = json!({"run_id":"run","conversation_id":"conversation","subagents":{"child":{"name":"Builder","state":"working"}}});
        let worker = observed_worker(&own, &caller, &details, "child", "receipt").unwrap();
        assert_eq!((worker.id.as_str(), worker.conversation_id.as_str(), worker.name.as_str()), ("child", "conversation", "Builder"));
        for (field, value) in [("run_id", json!("old")), ("conversation_id", json!("old"))] {
            let mut changed = details.clone(); changed[field] = value;
            assert!(observed_worker(&own, &caller, &changed, "child", "receipt").is_err());
        }
        for (field, value) in [("external", json!(true)), ("historical", json!(true)), ("state", json!("completed"))] {
            let mut changed = details.clone(); changed["subagents"]["child"][field] = value;
            assert!(observed_worker(&own, &caller, &changed, "child", "receipt").is_err());
        }
        assert!(observed_worker(&own, &caller, &details, "missing", "receipt").is_err());
    }

    #[test]
    fn a_task_pinned_to_the_same_name_elsewhere_names_that_computer() {
        let task = TaskDoc::create(zerus_replica::NewTask {
            project: "project-1",
            id: "T-a1b2c3",
            text: "Add rate limiting",
            stage: Stage::Todo,
            target: Target::Session(SessionKey { machine: "node-mac".into(), session: "codex/api".into(), ..Default::default()}),
            author: Actor::Person,
            rank: "V",
            now: 1,
        })
        .unwrap();
        let machines = Machines { here: "node-here".into(), selves: vec![], peers: BTreeMap::from([("mac".to_owned(), "node-mac".to_owned())]), bindings: vec![] };
        let args = ["--session".to_owned(), "codex/api".to_owned()];
        let error = send_session(&task, &args, &Actor::Person, &machines).unwrap_err().message;
        assert!(error.contains("pinned to codex/api on mac"), "{error}");
        let unknown = Machines { here: "node-here".into(), selves: vec![], peers: BTreeMap::new(), bindings: vec![] };
        let error = send_session(&task, &args, &Actor::Person, &unknown).unwrap_err().message;
        assert!(error.contains("pinned to codex/api on another computer"), "{error}");
    }

    #[test]
    fn host_names_this_computer_by_its_own_name() {
        let machines = Machines {
            here: "node-here".into(),
            selves: vec!["arch".into(), "arch-lan".into()],
            peers: BTreeMap::from([("mac".to_owned(), "node-mac".to_owned())]),
            bindings: vec![],
        };
        for host in [None, Some(""), Some("arch"), Some("arch-lan")] {
            assert_eq!(machines.node(host).unwrap(), "node-here", "{host:?}");
        }
        assert_eq!(machines.node(Some("mac")).unwrap(), "node-mac");
        let error = machines.node(Some("orchard")).unwrap_err().message;
        assert!(error.contains("orchard is not a verified computer of this swarm; omit --host for this computer"), "{error}");
    }

    /// A saved record, as JSON, at `file` below the state directory `root`.
    fn saved(root: &std::path::Path, file: &str, record: Value) {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    }

    #[test]
    fn a_large_saved_record_still_routes_by_name_and_identity() {
        let state = tempfile::tempdir().unwrap();
        let root = state.path();
        let file = |n: u8| format!("{n:064x}.json");
        // A live run keeps up to 128 shell jobs with their full heredoc commands.
        let jobs: serde_json::Map<String, Value> = (0..128)
            .map(|i| (format!("main:call-{i}"), json!({"command": format!("python3 - <<'PY'\n{}\nPY", "x".repeat(12_000)), "status": "completed"})))
            .collect();
        saved(root, &file(1), json!({"version":1,"name":"codex/big","run_id":"run-big","launch_id":"launch-big","journal_name":"codex/first","shell_jobs":jobs}));
        assert!(std::fs::metadata(root.join(file(1))).unwrap().len() > 1024 * 1024);
        // A renamed run and the shadow its old name leaves, which never routes.
        saved(root, &format!("bindings/{}", file(2)), json!({"name":"codex/renamed","run_id":"run-renamed","legacy_shadow":{"name":"codex/older","run_id":"run-renamed"}}));
        saved(root, &file(3), json!({"name":"codex/older","run_id":"run-renamed","rename_shadow":true}));
        saved(root, &file(4), json!({"name":"codex/twice","run_id":"run-a"}));
        saved(root, &format!("bindings/{}", file(5)), json!({"name":"codex/twice","run_id":"run-b"}));
        saved(root, &file(6), json!({"name":"codex/stopped"}));
        let machines = Machines {
            here: "node-here".into(),
            selves: vec![],
            peers: BTreeMap::new(),
            bindings: crate::state::task_bindings_in(root).unwrap(),
        };
        let routed = |session: &str, session_id: &str| {
            let key = SessionKey { machine: "node-here".into(), session: session.into(), session_id: session_id.into() };
            machines.resolve(&key).map(|key| (key.session, key.session_id))
        };
        let big = Some(("codex/big".to_owned(), "launch_id:launch-big".to_owned()));
        assert_eq!(routed("codex/big", ""), big);
        assert_eq!(routed("codex/earlier-label", "launch_id:launch-big"), big);
        assert_eq!(routed("codex/first", ""), big);
        let target = machines.target("node-here".into(), "codex/big", &[]).unwrap();
        assert_eq!((target.session.as_str(), target.session_id.as_str()), ("codex/big", "launch_id:launch-big"));
        assert_eq!(routed("codex/big", "launch_id:gone"), None);
        // The shadow neither routes nor makes its old name ambiguous.
        assert_eq!(routed("codex/older", ""), Some(("codex/renamed".to_owned(), "run_id:run-renamed".to_owned())));
        assert_eq!(routed("codex/twice", ""), None);
        // Without a routable binding, a name stays a name-only legacy hint.
        assert_eq!(routed("codex/stopped", ""), Some(("codex/stopped".to_owned(), String::new())));
    }

    #[test]
    fn phone_refusals_say_when_the_prompt_may_be_in_the_session() {
        let uncertain = |message: &str| refusal(message).get("uncertain").cloned();
        assert_eq!(
            uncertain("delivery uncertain: agent changed after paste; inspect Terminal before retrying"),
            Some(json!(true))
        );
        assert_eq!(uncertain(&format!("{SENT_UNRECORDED}: disk full")), Some(json!(true)));
        assert_eq!(uncertain("no task T-a1b2c3 in this project"), None);
        assert_eq!(refusal("no task")["error"], "no task");
    }

    #[test]
    fn claude_and_codex_read_the_instructions_others_do_not() {
        for agent in ["claude", "codex"] {
            let context = session_context(agent).unwrap();
            assert_eq!(context["hookSpecificOutput"]["hookEventName"], "SessionStart");
            let text = context["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
            for phrase in [
                "Zerus tasks are this project's shared task list.",
                "\"поставь таск\"",
                "hgs task report <ref>",
                "hgs guide tasks",
            ] {
                assert!(text.contains(phrase), "{phrase}");
            }
        }
        assert!(session_context("kimi").is_none());
    }
}
