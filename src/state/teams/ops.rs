//! WhiteWh fork: the team state machine. Every change is a pure function of
//! the team, the request and who asks, so the CLI and the dispatcher share it
//! and tests need no processes.
use super::super::now;
use super::model::*;
use super::plan;
use super::store::Event;
use anyhow::{bail, ensure, Context};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Who asks for a change.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Actor {
    /// The desktop or a person at the CLI; their decision is final.
    Human,
    /// A team member's session, by member id (the lead included).
    Member(String),
    /// C6: the parent team's lead, proven by its session in the parent team
    /// (section 9.3), acting on this sub-team.
    ParentLead,
    /// U51: a process presenting the owner's intake key (its label). It may add
    /// one urgent phase and nothing else.
    Intake(String),
}

impl Actor {
    pub(crate) fn label(&self, team: &Team) -> String {
        match self {
            Actor::Human => "You".into(),
            Actor::Member(id) => team.role_of(id),
            Actor::ParentLead => "Parent lead".into(),
            Actor::Intake(label) => format!("Intake {label}"),
        }
    }
    pub(crate) fn is_lead(&self, team: &Team) -> bool {
        matches!(self, Actor::Member(id) if *id == team.lead)
    }
    fn is_lead_of(&self, lead: &str) -> bool {
        matches!(self, Actor::Member(id) if id == lead)
    }
    /// You, the lead, or the sub-lead `member` reports to (spec 5.6): who may
    /// answer, hold, retry and decide that member's work.
    pub(crate) fn reviews(&self, team: &Team, member: &str) -> bool {
        match self {
            Actor::Human => true,
            Actor::Member(id) => *id == team.lead || (!member.is_empty() && team.reviewer_of(member) == *id),
            Actor::ParentLead | Actor::Intake(_) => false,
        }
    }
}

/// Spec 5.6: every parent is the lead or a sub-lead of the team, never the
/// member itself or one of the members below it; the lead reports to nobody.
fn check_hierarchy(team: &Team) -> anyhow::Result<()> {
    for member in &team.members {
        if member.id == team.lead {
            ensure!(member.parent.is_empty() && !member.sub, "The lead reports to nobody and is no sub-lead");
            continue;
        }
        if member.parent.is_empty() || member.parent == team.lead {
            continue;
        }
        let parent = team
            .member(&member.parent)
            .filter(|parent| parent.state != MemberState::Removed)
            .with_context(|| format!("{} reports to unknown member {}", member.id, member.parent))?;
        ensure!(parent.sub, "{} reports to {}, which is not a sub-lead", member.id, parent.id);
        // Walk up: a cycle would never reach the lead.
        let mut seen = BTreeSet::new();
        let mut at = member.parent.clone();
        while !at.is_empty() && at != team.lead {
            ensure!(at != member.id && seen.insert(at.clone()), "{} would report to itself", member.id);
            at = team.member(&at).map(|found| found.parent.clone()).unwrap_or_default();
        }
    }
    Ok(())
}

/// Spec 5.6: "Reports to..." and "Make sub-lead" / "Stop being a sub-lead".
fn hierarchy(team: &mut Team, op: &str, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    ensure!(matches!(actor, Actor::Human) || actor.is_lead(team), "Only the lead or you change who reports to whom");
    ensure!(
        matches!(team.status, TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused),
        "Only a planning, running or paused team changes its hierarchy"
    );
    let id = text(input, "member").to_owned();
    ensure!(id != team.lead, "The lead reports to nobody");
    ensure!(team.member(&id).is_some_and(|member| member.state != MemberState::Removed), "Unknown member {id}");
    let by = actor.label(team);
    let role = team.role_of(&id);
    if op == "reports-to" {
        let parent = text(input, "parent").to_owned();
        let parent = if parent == team.lead { String::new() } else { parent };
        let before = team.member(&id).map(|member| member.parent.clone()).unwrap_or_default();
        team.members.iter_mut().find(|member| member.id == id).context("Unknown member")?.parent = parent.clone();
        if let Err(error) = check_hierarchy(team) {
            team.members.iter_mut().find(|member| member.id == id).context("Unknown member")?.parent = before;
            return Err(error);
        }
        let to = if parent.is_empty() { "Lead".to_owned() } else { team.role_of(&parent) };
        events.push(Event::new("reports_to", json!({"by":by,"member":role,"to":to})));
        return Ok(json!({"status":"reports_to","member":id,"parent":if parent.is_empty() { team.lead.clone() } else { parent }}));
    }
    let sub = input["sub"].as_bool().context("sub is true or false")?;
    if !sub {
        let reports: Vec<String> = team.reports_of(&id).iter().map(|member| team.name_of(&member.id)).collect();
        ensure!(reports.is_empty(), "Move its reports to another lead first: {}", reports.join(", "));
    }
    let member = team.members.iter_mut().find(|member| member.id == id).context("Unknown member")?;
    member.sub = sub;
    if sub {
        // N10: a new sub-lead gets a short brief once.
        member.sub_brief = true;
    }
    if !sub {
        member.inbox = Inbox::default();
    }
    events.push(Event::new("sub_lead", json!({"by":by,"member":role,"sub":sub})));
    Ok(json!({"status":if sub { "sub_lead" } else { "member" },"member":id}))
}

fn text<'a>(input: &'a Value, key: &str) -> &'a str {
    input[key].as_str().unwrap_or_default()
}

fn task_id(input: &Value) -> anyhow::Result<u32> {
    input["task"].as_u64().and_then(|id| u32::try_from(id).ok()).filter(|id| *id > 0).context("A task number is required")
}

fn clean(value: &str, max: usize, what: &str) -> anyhow::Result<String> {
    ensure!(value.chars().count() <= max, "{what} is too long");
    ensure!(!value.chars().any(|c| c.is_control() && c != '\n' && c != '\t'), "{what} contains control characters");
    Ok(value.to_owned())
}

/// U58: role and task text keeps LF newlines - CRLF and a lone CR become LF -
/// and a refused control character names the field and its position.
fn clean_text(value: &str, max: usize, what: &str) -> anyhow::Result<String> {
    let value = value.replace("\r\n", "\n").replace('\r', "\n");
    ensure!(value.chars().count() <= max, "{what} is too long");
    if let Some((at, _)) = value.char_indices().find(|(_, ch)| ch.is_control() && *ch != '\n' && *ch != '\t') {
        bail!("{what} has a control character at position {}", value[..at].chars().count() + 1);
    }
    Ok(value)
}

/// U58: whether a task has started: any attempt that may have reached the
/// member (a refused one never did).
fn task_started(task: &Task) -> bool {
    task.attempts.iter().any(|attempt| attempt.state != AttemptState::Refused)
}

/// U58: whether no task of a phase has started, so a queued or approved phase
/// may still be edited like a draft.
fn phase_untouched(team: &Team, phase: &Phase) -> bool {
    team.tasks.iter().filter(|task| task.phase == phase.n).all(|task| !task_started(task))
}

fn member_id(value: &str) -> anyhow::Result<String> {
    ensure!(
        !value.is_empty() && value.len() <= 40 && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "Member ids use letters, digits, - and _"
    );
    Ok(value.to_owned())
}

/// The bounds on a team's settings, for the settings a request sets.
/// U48: a task's branch from the team's template: `{id}`, `{member}` (as a
/// branch-safe tag) and `{team8}`.
pub(crate) fn branch_name(template: &str, id: u32, member: &str, team8: &str) -> String {
    template.replace("{id}", &id.to_string()).replace("{member}", &super::dispatcher::tag(member)).replace("{team8}", team8)
}

fn validate_settings(settings: &Settings) -> anyhow::Result<()> {
    ensure!(
        settings.tests.as_ref().is_none_or(|tests| tests.is_object()),
        "The tests override is an object shaped like .zerus/tests.toml"
    );
    ensure!((1..=32).contains(&settings.max_members), "A team has 1 to 32 members");
    ensure!(
        (30..=3600).contains(&settings.start_timeout_seconds),
        "The start warning waits 30 to 3600 seconds after delivery"
    );
    ensure!(
        (60..=86_400).contains(&settings.blocked_timeout_seconds),
        "The blocked warning waits 60 to 86400 seconds"
    );
    // Section 9.2: the tree bounds are set on the master.
    ensure!((1..=8).contains(&settings.tree_depth), "A tree goes 1 to 8 levels below its master");
    ensure!((1..=200).contains(&settings.tree_sessions), "A tree runs 1 to 200 sessions");
    // U48: the task branch's name template gives a valid branch for any task.
    if let Some(branch) = &settings.branch_per_task {
        let sample = branch_name(&branch.template, 12, "dev", "1234abcd");
        ensure!(branch.template.contains("{id}"), "A task branch template names {{id}}, such as task/{{id}}");
        super::templates::check_branch(&sample).context("The task branch template does not give a valid branch name")?;
    }
    ensure!(settings.base_branch.is_empty() || super::templates::check_branch(&settings.base_branch).is_ok(), "The base branch is not a valid branch name");
    // N19: a short list of folder paths; Zerus does not read them.
    ensure!(settings.evidence_folders.len() <= 20, "A team lists at most 20 evidence folders");
    for folder in &settings.evidence_folders {
        ensure!(!folder.trim().is_empty() && folder.chars().count() <= 1000, "An evidence folder is a path of 1 to 1000 characters");
        ensure!(!folder.chars().any(char::is_control), "An evidence folder has no control characters");
    }
    Ok(())
}

fn parse_member(value: &Value) -> anyhow::Result<Member> {
    let mut member: Member = serde_json::from_value(value.clone()).context("Invalid member")?;
    member.id = member_id(&member.id)?;
    member.role = clean_text(&member.role, 60, "A role")?;
    ensure!(!member.agent.is_empty(), "Member {} needs an agent", member.id);
    // C3: `permissions` is the launch mode, the profile an agent's mode.
    ensure!(
        member.permissions.is_empty() || ["auto", "bypass"].contains(&member.permissions.as_str()),
        "A permissions mode is auto or bypass"
    );
    ensure!(
        member.permissions_profile.is_empty() || ["read", "write", "build", "commit", "network"].contains(&member.permissions_profile.as_str()),
        "A permissions profile is read, write, build, commit or network"
    );
    // U46: opt-in; empty inherits the role, "none" overrides it with none.
    ensure!(
        matches!(member.context_before_task.as_str(), "" | "none" | "compact" | "clear"),
        "A context before a task is none, compact or clear"
    );
    member.launch_id.clear();
    member.error.clear();
    member.inbox = Inbox::default();
    if !member.parent.is_empty() {
        member.parent = member_id(&member.parent)?;
    }
    member.prompt = clean_text(&member.prompt, 8000, "A role prompt")?;
    // N3: copy the role's prompt blocks now; a later catalog edit changes no
    // running team mid-phase. An old member loaded from the store keeps None.
    if member.prompt_snapshot.is_none() {
        member.prompt_snapshot = super::catalog::snapshot_for(&member.role);
    }
    // U45: the role's model list, copied as the prompt is.
    if member.models.is_empty() {
        member.models = super::catalog::models_for(&member.role);
    }
    super::models::check_choices(&member.models)?;
    member.state = match member.session {
        Some(_) => MemberState::Active,
        None => MemberState::NotCreated,
    };
    Ok(member)
}

/// F1.2/U49: an added member without a folder of its own takes the agent's
/// default one, shared when the project forbids worktrees.
fn fill_folder(team: &Team, member: &mut Member, no_folder_given: bool) {
    if team.settings.folder_policy == FolderPolicy::Auto && no_folder_given && member.directory.is_empty() {
        member.folder = if team.settings.worktrees_forbidden {
            MemberFolder::Shared
        } else {
            super::dispatcher::default_folder(&member.agent)
        };
    }
}

/// U49: a role's own words as a member id: "Backend" gives "backend", "QA
/// lead" gives "qa-lead"; a role without letters or digits keeps "member".
fn scale_base(role: &str) -> String {
    let mut base = String::new();
    for ch in role.chars() {
        if ch.is_ascii_alphanumeric() {
            base.push(ch.to_ascii_lowercase());
        } else if !base.is_empty() && !base.ends_with('-') {
            base.push('-');
        }
    }
    let base: String = base.trim_matches('-').chars().take(36).collect();
    let base = base.trim_end_matches('-');
    if base.is_empty() { "member".into() } else { base.to_owned() }
}

/// U49: the id scale gives a new member, numbered past the ones the team and
/// this scale already use ("backend", "backend2", ...).
fn scale_id(team: &Team, chosen: &[String], role: &str) -> String {
    let base = scale_base(role);
    (1..)
        .map(|n| if n == 1 { base.clone() } else { format!("{base}{n}") })
        .find(|id| team.members.iter().all(|member| member.id != *id) && !chosen.iter().any(|chosen| chosen == id))
        .unwrap_or_else(|| format!("{base}{}", chosen.len() + 1))
}

/// U27: a lane is visual only, bounded in number and label; it never touches
/// dependencies or hand-out.
fn clean_lane(lane: &mut super::model::Lane, task: u32) -> anyhow::Result<()> {
    ensure!((1..=99).contains(&lane.n), "Task #{task}: a lane number is 1 to 99");
    lane.label = clean(lane.label.trim(), 40, "A lane label")?;
    Ok(())
}

fn parse_task(value: &Value) -> anyhow::Result<Task> {
    let mut task: Task = serde_json::from_value(value.clone()).context("Invalid task")?;
    ensure!(task.id > 0, "Task numbers start at 1");
    task.title = clean_text(&task.title, 200, "A task title")?;
    ensure!(!task.title.trim().is_empty(), "Task #{} needs a title", task.id);
    task.description = clean_text(&task.description, 20_000, "A task description")?;
    // U58: a task for a role keeps the role's text clean too.
    task.role = clean_text(&task.role, 60, "A task role")?;
    // C5: a check carries its exact command and expected exit code; a plan
    // task may not hide one.
    match task.kind {
        TaskKind::Check => {
            let check = task
                .check
                .as_mut()
                .with_context(|| format!("Task #{} is a check: add \"check\":{{\"command\":\"...\"}}", task.id))?;
            check.command = clean(check.command.trim(), 2000, "A check command")?;
            ensure!(!check.command.is_empty(), "Task #{} is a check: its command is empty", task.id);
            ensure!(
                (1..=1440).contains(&check.timeout_minutes),
                "Task #{}: a check waits 1 to 1440 minutes",
                task.id
            );
        }
        TaskKind::Task => {
            ensure!(task.check.is_none(), "Task #{} has a check but no \"kind\":\"check\"", task.id);
        }
    }
    for criterion in &mut task.criteria {
        *criterion = clean_text(criterion, 500, "An acceptance criterion")?;
    }
    let mut seen = BTreeSet::new();
    task.deps.retain(|dep| seen.insert(dep.task));
    ensure!(task.deps.iter().all(|dep| dep.task != task.id), "Task #{} depends on itself", task.id);
    // U27: the lane the task is drawn in; its number and label are bounded and
    // it changes nothing about hand-out.
    if let Some(lane) = task.lane.as_mut() {
        clean_lane(lane, task.id)?;
    }
    // Owner 2026-10-31: the lead's estimate is in minutes; a task without one
    // gets Zerus' own from the per-computer history when it is shown.
    if let Some(minutes) = task.estimate_minutes {
        ensure!((1.0..=1440.0).contains(&minutes), "Task #{}: an estimate is 1 to 1440 minutes", task.id);
        task.estimate_minutes = Some(minutes.round());
        task.estimate_source = "lead".into();
    } else {
        task.estimate_source = "none".into();
    }
    task.status = TaskStatus::Draft;
    // `plan` (and `create`'s planning tasks) always start phase 1; `extend`
    // sets the computed phase afterwards.
    task.phase = 1;
    task.attempts.clear();
    task.hold = None;
    task.decided_by = None;
    // U36: the plan names a preset; Zerus snapshots it when the task is
    // assigned, so a request can never carry its own run.
    task.pipeline_run = None;
    check_pipeline(&task.pipeline)?;
    // U45: the models allowed for the task and a role instead of an assignee.
    super::models::check_task(&mut task)?;
    Ok(task)
}

/// U48: a task the plan, extend or edit-draft gives a preset with a land stage
/// needs the owner's `land_allowed`, because landing may push: the request is
/// refused before it changes anything.
fn check_land_choices(team: &Team, input: &Value) -> anyhow::Result<()> {
    let chosen: Vec<&str> = ["tasks", "tasks_update"]
        .into_iter()
        .flat_map(|key| input[key].as_array().into_iter().flatten())
        .filter_map(|task| task["pipeline"].as_str())
        .map(str::trim)
        .filter(|choice| !choice.is_empty() && *choice != "none")
        .collect();
    if team.settings.land_allowed || chosen.is_empty() {
        return Ok(());
    }
    let catalog = super::catalog::load().unwrap_or_else(|_| super::catalog::builtin());
    for choice in chosen {
        let lands = catalog.pipelines.iter().find(|pipeline| pipeline.id == choice).is_some_and(|pipeline| pipeline.stages.iter().any(|stage| stage.kind == "land"));
        ensure!(!lands, "Pipeline {choice} lands the task, and the owner has not allowed landing in this team (land_allowed)");
    }
    Ok(())
}

/// U36: a task's pipeline choice names a preset of the team catalog.
fn check_pipeline(choice: &str) -> anyhow::Result<()> {
    if choice.is_empty() || choice == "none" {
        return Ok(());
    }
    let catalog = super::catalog::load().unwrap_or_else(|_| super::catalog::builtin());
    super::pipeline::check_choice(choice, &catalog)
}

/// A new team in Planning with its lead.
pub(crate) fn create(id: &str, input: &Value) -> anyhow::Result<(Team, Vec<Event>)> {
    let name = clean(text(input, "name").trim(), 120, "A team name")?;
    ensure!(!name.is_empty(), "A team name is required");
    let mut lead = parse_member(&input["lead"]).context("Invalid lead")?;
    if lead.role.is_empty() {
        lead.role = "Lead".into();
    }
    let settings: Settings = if input["settings"].is_null() {
        Settings::default()
    } else {
        serde_json::from_value(input["settings"].clone()).context("Invalid team settings")?
    };
    validate_settings(&settings)?;
    // Section 9.1: a sub-team names the parent team and its open task; the
    // caller fills the parent lead and the assignee's session as the lead.
    let parent: Option<Parent> = if input["parent"].is_object() {
        let value = &input["parent"];
        let team = clean(text(value, "team").trim(), 80, "The parent team")?;
        ensure!(!team.is_empty(), "The parent team is required");
        let task = value["task"]
            .as_u64()
            .and_then(|id| u32::try_from(id).ok())
            .filter(|id| *id > 0)
            .context("The parent task number is required")?;
        Some(Parent {
            team,
            task,
            lead: clean(text(value, "lead").trim(), 40, "The parent lead")?,
            member: clean(text(value, "member").trim(), 40, "The parent member")?,
        })
    } else {
        None
    };
    // From the desktop: the members and template tasks proposed for the
    // lead's plan; the lead starts and gets a planning brief.
    let mut members = vec![lead.clone()];
    for value in input["members"].as_array().into_iter().flatten() {
        let member = parse_member(value)?;
        ensure!(members.iter().all(|m| m.id != member.id), "Member id {} is used twice", member.id);
        members.push(member);
    }
    let mut tasks = Vec::new();
    for value in input["tasks"].as_array().into_iter().flatten() {
        let task = parse_task(value)?;
        ensure!(tasks.iter().all(|t: &Task| t.id != task.id), "Task #{} is used twice", task.id);
        tasks.push(task);
    }
    let start = input["start"].as_bool() == Some(true);
    // The request's lead protocol, even an empty one; without one, the
    // template's as the catalog has it now. The lead reads it with the
    // planning brief.
    let template = text(input, "template");
    let lead_protocol = match input["lead_protocol"].as_str() {
        Some(given) => given.to_owned(),
        None if !template.is_empty() => super::catalog::load()
            .ok()
            .and_then(|catalog| catalog.templates.into_iter().find(|t| t.id == template))
            .map(|t| t.lead_protocol)
            .unwrap_or_default(),
        None => String::new(),
    };
    // A lead without a session is started first; an existing lead is briefed.
    let team = Team {
        id: id.to_owned(),
        name: name.clone(),
        goal: clean(text(input, "goal"), 20_000, "The goal")?,
        project: clean(text(input, "project"), 200, "The project")?,
        folder: clean(text(input, "folder"), 4096, "The folder")?,
        template: clean(text(input, "template"), 120, "The template")?,
        status: TeamStatus::Planning,
        settings,
        lead: lead.id.clone(),
        members,
        tasks,
        brief: if start { Notice::Pending } else { Notice::None },
        lead_protocol: clean(&lead_protocol, 8000, "The lead protocol")?,
        parent,
        ..Default::default()
    };
    let events = vec![Event::new("created", json!({"by":"You","name":name}))];
    Ok((team, events))
}

/// Replaces the plan: members besides the lead, tasks and dependencies, the
/// roadmap of later phases, and optionally settings. Only while Planning.
fn plan(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    ensure!(team.status == TeamStatus::Planning, "Only a team in Planning can change its plan");
    ensure!(matches!(actor, Actor::Human) || actor.is_lead(team), "Only the lead or you can change the plan");
    // U59: at the start of planning the lead proposes standard or free with a
    // one-line reason; the owner's approval chooses one of the two.
    if let Some(value) = input.get("plan_mode").filter(|value| !value.is_null()) {
        let mode: PlanMode = serde_json::from_value(value.clone()).context("The plan mode is standard or free")?;
        let reason = clean(text(input, "plan_mode_reason").trim(), 500, "A plan mode reason")?;
        ensure!(!reason.is_empty(), "A plan mode proposal needs its one-line reason");
        team.plan_mode_offer = Some(mode);
        team.plan_mode_reason = reason.clone();
        events.push(Event::new("plan_mode_proposed", json!({"mode":mode,"reason":reason})));
    }
    let lead = team.member(&team.lead).cloned().context("The team has no lead")?;
    let mut members = vec![lead];
    for value in input["members"].as_array().into_iter().flatten() {
        let member = parse_member(value)?;
        ensure!(members.iter().all(|m| m.id != member.id), "Member id {} is used twice", member.id);
        members.push(member);
    }
    let mut tasks = Vec::new();
    for value in input["tasks"].as_array().into_iter().flatten() {
        let mut task = parse_task(value)?;
        // The plan may queue tasks of later phases; phase 1 stays the default.
        task.phase = match value["phase"].as_u64() {
            None => 1,
            Some(phase) => u32::try_from(phase).ok().filter(|phase| *phase > 0).context("A task phase is a number from 1")?,
        };
        ensure!(tasks.iter().all(|t: &Task| t.id != task.id), "Task #{} is used twice", task.id);
        tasks.push(task);
    }
    ensure!(tasks.len() <= 200, "A plan has at most 200 tasks");
    let settings: Option<Settings> = if input["settings"].is_null() {
        None
    } else {
        Some(serde_json::from_value(input["settings"].clone()).context("Invalid team settings")?)
    };
    if let Some(settings) = &settings {
        validate_settings(settings)?;
        ensure_lead_settings(&team.settings, settings, actor, false)?;
    }
    // B6: under the auto folder policy a member without its own choice gets
    // its agent's default: a clone for a sandboxed agent, else a worktree.
    let policy = settings.as_ref().map(|settings| settings.folder_policy).unwrap_or(team.settings.folder_policy);
    if policy == FolderPolicy::Auto {
        for (member, value) in members.iter_mut().skip(1).zip(input["members"].as_array().into_iter().flatten()) {
            if value.get("folder").is_none() && member.directory.is_empty() {
                // F1.2: auto keeps the agent's default folder (a clone for a
                // sandboxed agent, a worktree otherwise) unless the plan names
                // one; a project that forbids worktrees leaves the member shared.
                member.folder = if team.settings.worktrees_forbidden {
                    MemberFolder::Shared
                } else {
                    super::dispatcher::default_folder(&member.agent)
                };
            }
        }
    }
    // The roadmap (spec section 10): phases 2..N in order, all Draft. The owner
    // approves each one strictly after the phase before it ended.
    let approval = settings.as_ref().map(|settings| settings.phase_approval.unwrap_or(settings.approval)).unwrap_or_else(|| phase_approval(team));
    let by = actor.label(team);
    let mut phases: Vec<Phase> = Vec::new();
    for value in input["phases"].as_array().into_iter().flatten() {
        let n = value["n"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n >= 2)
            .context("A planned phase is numbered from 2")?;
        ensure!(phases.iter().all(|phase| phase.n != n), "Phase {n} is used twice");
        let title = clean(value["title"].as_str().unwrap_or_default().trim(), 120, "A phase title")?;
        phases.push(Phase {
            n,
            title,
            barrier: value["barrier"].as_bool() == Some(true),
            approval,
            state: PhaseState::Draft,
            added_by: by.clone(),
            added_at: now(),
            ..Default::default()
        });
    }
    // U42 (#587): a phase the plan already had keeps its proposer, its time
    // and its place in the order (a move stays); a new one goes after them by
    // number.
    for phase in &mut phases {
        if let Some(old) = team.phases.iter().find(|old| old.n == phase.n) {
            phase.added_by = old.added_by.clone();
            phase.added_at = old.added_at;
            phase.order = old.order();
        }
    }
    phases.sort_by_key(|phase| (if phase.order == 0 { u32::MAX } else { phase.order }, phase.n));
    // The plan lists the phases in order; the owner may move a draft later (F10).
    // Phase 1 is the approved plan and has no record, so the orders start at 2.
    for (index, phase) in phases.iter_mut().enumerate() {
        phase.order = index as u32 + 2;
    }
    let listed: BTreeSet<u32> = phases.iter().map(|phase| phase.n).collect();
    for task in &tasks {
        ensure!(task.phase == 1 || listed.contains(&task.phase), "Task #{} is in phase {}, which the plan does not list", task.id, task.phase);
    }
    for phase in &mut phases {
        phase.tasks = tasks.iter().filter(|task| task.phase == phase.n).map(|task| task.id).collect();
        ensure!(!phase.tasks.is_empty(), "Phase {} has no tasks", phase.n);
    }
    if let Some(settings) = settings {
        team.settings = settings;
    }
    // F8: a member with an existing session is adopted, not launched; record
    // it once, when the session first appears or changes (L2).
    let adopted: Vec<(String, String)> = members
        .iter()
        .filter(|member| member.id != team.lead)
        .filter_map(|member| {
            let session = member.session.as_ref()?;
            let old = team.member(&member.id).and_then(|old| old.session.as_ref());
            (old != Some(session)).then(|| (member.id.clone(), session.name.clone()))
        })
        .collect();
    // B5: a plan that widens a role's permissions waits for the owner's
    // explicit confirmation.
    let widening = widening(team, &members);
    let old_members = std::mem::replace(&mut team.members, members);
    if let Err(error) = check_hierarchy(team) {
        team.members = old_members;
        return Err(error);
    }
    team.tasks = tasks;
    team.phases = phases;
    team.profile_widening = widening;
    let validation = plan::validate(team, &BTreeSet::new());
    for (id, session) in adopted {
        events.push(Event::new("member_adopted", json!({"member":team.role_of(&id),"session":session})));
    }
    events.push(Event::new(
        "planned",
        json!({"by":by,"members":team.members.len(),"tasks":team.tasks.len(),"phases":team.phases.len(),"errors":validation.errors.len()}),
    ));
    Ok(json!({"status":"planned","validation":validation}))
}

fn set_status(team: &mut Team, status: TeamStatus, kind: &'static str, actor: &Actor, events: &mut Vec<Event>) -> Value {
    team.status = status;
    events.push(Event::new(kind, json!({"by":actor.label(team)})));
    json!({"status":kind})
}

/// The owner's sentence the CLI checked for this request (F13): present only
/// when a `UserPromptSubmit` event of the lead's own session carried it.
fn verified_word(input: &Value) -> anyhow::Result<Option<OwnerWord>> {
    match input.get("owner_word_checked") {
        Some(value) if value.is_object() => {
            Ok(Some(serde_json::from_value(value.clone()).context("Invalid checked owner word")?))
        }
        _ => Ok(None),
    }
}

/// Section 9.3: a sub-team's plan and phases are approved by its parent lead
/// (default) or by the owner; the sub-lead cannot approve its own plan.
fn ensure_subteam_approver(team: &Team, actor: &Actor) -> anyhow::Result<()> {
    if team.parent.is_none() || matches!(actor, Actor::Human) {
        return Ok(());
    }
    if team.settings.subteam_approval == SubteamApproval::Owner {
        bail!("The owner approves this sub-team's plan and phases");
    }
    ensure!(matches!(actor, Actor::ParentLead), "The parent lead approves this sub-team's plan and phases");
    Ok(())
}

/// Whether this actor is the parent lead (section 9.3, default setting).
fn parent_approves(team: &Team, actor: &Actor) -> bool {
    team.parent.is_some() && team.settings.subteam_approval == SubteamApproval::Parent && matches!(actor, Actor::ParentLead)
}

fn approve(team: &mut Team, input: &Value, actor: &Actor, offline: &BTreeSet<String>, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    ensure!(team.status == TeamStatus::Planning, "Only a plan in Planning can be approved");
    ensure_subteam_approver(team, actor)?;
    let owner_word = verified_word(input)?;
    // C1: the owner's word is the lead's route; a member can never approve on
    // a word the CLI did not verify in the lead's own session.
    ensure!(owner_word.is_none() || actor.is_lead(team), "Only the lead approves on the owner's word");
    ensure!(
        matches!(actor, Actor::Human)
            || parent_approves(team, actor)
            || owner_word.is_some()
            || (team.settings.approval == Approval::Auto && actor.is_lead(team)),
        "The lead approves only on the owner's word: pass \"owner_word\" with the owner's sentence"
    );
    // U59: choosing at the start is the owner's own act, no proposal needed. A
    // lead's approval or an older client that sends no mode keeps standard.
    if let Some(value) = input.get("plan_mode").filter(|value| !value.is_null()) {
        ensure!(matches!(actor, Actor::Human), "Only you choose the plan mode");
        team.plan_mode = serde_json::from_value::<PlanMode>(value.clone()).context("The plan mode is standard or free")?;
    }
    team.plan_mode_offer = None;
    team.plan_mode_reason.clear();
    ensure!(!team.tasks.is_empty(), "The plan has no tasks");
    let validation = plan::validate(team, offline);
    ensure!(validation.errors.is_empty(), "Fix {} plan error(s) to run: {}", validation.errors.len(),
        validation.errors.iter().map(|f| f.text.as_str()).collect::<Vec<_>>().join("; "));
    // Phase 1 opens with the plan; the phases above it stay Draft until the
    // owner approves each one in order (spec section 10).
    for task in &mut team.tasks {
        if task.status == TaskStatus::Draft && task.phase == 1 {
            task.status = TaskStatus::Pending;
        }
    }
    // The owner has seen and confirmed the wider permissions.
    team.profile_widening.clear();
    let by = actor.label(team);
    team.status = TeamStatus::Running;
    // F13: the plan records who approved it and, for the lead, the owner's
    // exact sentence; the GUI says "Approved by Lead on your word".
    team.approved_by = by.clone();
    team.approved_at = Some(now());
    team.owner_word = owner_word.clone();
    events.push(Event::new(
        "approved",
        json!({"by":by,"owner_word":owner_word,"plan":true}),
    ));
    Ok(json!({"status":"approved","by":by,"owner_word":owner_word}))
}

/// U37: the decision's time on the attempt it decides: the last report, else
/// the last attempt (an owner's decision without a report). A later decision
/// over the same attempt keeps the first time.
fn mark_decided(task: &mut Task) {
    let at = task.attempts.iter().rposition(|attempt| attempt.state == AttemptState::Reported).or(task.attempts.len().checked_sub(1));
    if let Some(attempt) = at.map(|at| &mut task.attempts[at]) {
        attempt.decided_at.get_or_insert_with(now);
    }
}

/// The approval new phases wait for: their own setting, else the plan's.
fn phase_approval(team: &Team) -> Approval {
    team.settings.phase_approval.unwrap_or(team.settings.approval)
}

/// B5: the order of permission profiles; a wider one needs the owner.
fn profile_rank(profile: &str) -> u8 {
    match profile {
        "read" => 1,
        "build" => 3,
        "commit" => 4,
        "network" => 5,
        _ => 2, // write is the default
    }
}

/// B5: the members whose profile widens the role's default, for the owner.
fn widening(team: &Team, members: &[Member]) -> Vec<Value> {
    let mut found = Vec::new();
    for member in members.iter().filter(|member| member.id != team.lead) {
        let default = team.member(&member.id).map(|old| old.permissions_profile.as_str()).unwrap_or("write");
        if profile_rank(&member.permissions_profile) > profile_rank(default) {
            found.push(json!({"member":member.id,"from":default,"to":member.permissions_profile}));
        }
    }
    found
}

/// The validation findings that concern a new phase: those naming one of its
/// tasks, dependency cycles (a cycle finding names its tasks in `tasks`), and a
/// folder or policy mismatch (N39): that one names no task but the team cannot
/// run the phase in it, so it is not dropped.
fn phase_errors(validation: &plan::Validation, ids: &BTreeSet<u32>, any_cycle: bool) -> Vec<plan::Finding> {
    validation
        .errors
        .iter()
        .filter(|finding| {
            finding.task.is_some_and(|task| ids.contains(&task))
                || (finding.kind == "cycle" && (any_cycle || finding.tasks.iter().any(|task| ids.contains(task))))
                || finding.kind == "folder"
        })
        .cloned()
        .collect()
}

fn errors_text(errors: &[plan::Finding]) -> String {
    errors.iter().map(|finding| finding.text.as_str()).collect::<Vec<_>>().join("; ")
}

/// Appends a phase to a planning, running or paused team: the lead's or your
/// new work after the plan. A phase is a Draft when the plan is still being
/// drafted, when an earlier phase is a Draft, or when approval is `review`.
/// F11: refuses a draft change made against numbers that have since shifted.
fn ensure_draft_revision(team: &Team, input: &Value) -> anyhow::Result<()> {
    if let Some(revision) = input["revision"].as_u64() {
        ensure!(
            team.phase_numbers_revision <= revision,
            "Draft numbers changed after revision {revision}; read status again"
        );
    }
    Ok(())
}

/// F1.5: the lead or owner changes a team's settings and its members in place.
/// While Planning any member changes; a running team only members without a
/// session. A wider permissions profile keeps the B5 confirmation.
fn settings(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    ensure!(matches!(actor, Actor::Human) || actor.is_lead(team), "Only the lead or you change the team's settings");
    ensure!(
        matches!(team.status, TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused),
        "Only a planning, running or paused team changes its settings"
    );
    let planning = team.status == TeamStatus::Planning;
    let mut changed = Vec::new();
    let mut widen = Vec::new();
    for update in input["members_update"].as_array().into_iter().flatten() {
        let id = text(update, "id").to_owned();
        ensure!(!id.is_empty(), "A member update names its member");
        let member = team.members.iter_mut().find(|member| member.id == id).with_context(|| format!("Unknown member {id}"))?;
        // U46: the context before a task only changes the next hand-out, so the
        // owner may set it on a running member that has a session too; every
        // other field still changes only on a member without one. #581 N6: so
        // does its model list (U45), which only the next switch reads.
        let context_only = update.as_object().is_some_and(|fields| {
            fields.keys().any(|key| key == "context_before_task" || key == "models")
                && fields.keys().all(|key| key == "id" || key == "context_before_task" || key == "models")
        });
        ensure!(
            planning || member.session.is_none() || context_only,
            "A running team changes only members without a session"
        );
        if let Some(folder) = update.get("folder") {
            member.folder = serde_json::from_value(folder.clone()).context("A folder is shared, worktree or clone")?;
        }
        if let Some(directory) = update.get("directory") {
            member.directory = clean(directory.as_str().unwrap_or_default(), 500, "A directory")?;
        }
        if let Some(role) = update.get("role") {
            let role = clean(role.as_str().unwrap_or_default().trim(), 60, "A role")?;
            ensure!(!role.is_empty(), "A role is not empty");
            member.role = role;
        }
        if let Some(prompt) = update.get("prompt") {
            member.prompt = clean(prompt.as_str().unwrap_or_default(), 8000, "A role prompt")?;
        }
        if let Some(profile) = update.get("permissions_profile") {
            let profile = profile.as_str().unwrap_or_default().to_owned();
            ensure!(
                profile.is_empty() || ["read", "write", "build", "commit", "network"].contains(&profile.as_str()),
                "A permissions profile is read, write, build, commit or network"
            );
            if profile_rank(&profile) > profile_rank(&member.permissions_profile) {
                widen.push(json!({"member":id,"from":member.permissions_profile,"to":profile}));
            }
            member.permissions_profile = profile;
        }
        // U46: the owner sets what Zerus does to a member's context; the lead
        // never does (spec 0). Empty inherits the role, "none" overrides it.
        if let Some(context) = update.get("context_before_task") {
            ensure!(matches!(actor, Actor::Human), "Only you set a member's context before a task");
            let context = context.as_str().unwrap_or_default().to_owned();
            ensure!(matches!(context.as_str(), "" | "none" | "compact" | "clear"), "A context before a task is none, compact or clear");
            member.context_before_task = context;
        }
        // #581 N6, spec 2: the owner edits the models a member may switch
        // between; an empty list leaves only its launch model.
        if let Some(models) = update.get("models") {
            ensure!(matches!(actor, Actor::Human), "Only you set a member's models");
            let models: Vec<ModelChoice> = serde_json::from_value(models.clone()).context("Models are a list of {\"model\",\"effort\"}")?;
            let models: Vec<ModelChoice> =
                models.into_iter().map(|choice| ModelChoice { model: choice.model.trim().to_owned(), effort: choice.effort.trim().to_owned() }).collect();
            super::models::check_choices(&models)?;
            member.models = models;
        }
        changed.push(json!(id));
    }
    if let Some(value) = input.get("settings") {
        // C4: a partial object changes only the fields it names.
        let given = value.as_object().context("Settings are an object")?;
        let mut merged = serde_json::to_value(&team.settings)?;
        for (field, value) in given {
            merged[field] = value.clone();
        }
        let updated: Settings = serde_json::from_value(merged).context("Invalid settings")?;
        validate_settings(&updated)?;
        ensure_lead_settings(&team.settings, &updated, actor, input["roster_word"].as_bool() == Some(true))?;
        // N13: only a sub-lead (and its parent lead) is refused auto; the owner
        // may choose it, and a sub-team already carrying auto can still change
        // other settings.
        if team.parent.is_some() && !matches!(actor, Actor::Human) {
            ensure!(
                (updated.approval != Approval::Auto || team.settings.approval == Approval::Auto)
                    && (updated.phase_approval != Some(Approval::Auto)
                        || team.settings.phase_approval == Some(Approval::Auto)),
                "A sub-team's plan and phases are approved by its parent lead or you, never auto"
            );
        }
        team.settings = updated;
    }
    if changed.is_empty() && input.get("settings").is_none() {
        anyhow::bail!("A settings change names settings or members_update");
    }
    // C4: a member of a running team launches with its profile as it is, so
    // a wider one needs the owner's confirmation now, not at an approval.
    if !planning && !widen.is_empty() {
        ensure!(
            matches!(actor, Actor::Human) && input["confirm_widening"].as_bool() == Some(true),
            "The owner confirms the wider permissions: settings with \"confirm_widening\": true"
        );
    }
    team.profile_widening.extend(widen);
    events.push(Event::new("settings_changed", json!({"by":actor.label(team),"members":changed})));
    Ok(json!({"status":"updated","members":changed,"profile_widening":team.profile_widening}))
}

/// C4: the settings that decide who approves, how far a team may grow and
/// what runs without the owner stay the owner's; the lead changes the rest.
/// Spec 6.1 lets the lead set resume_after_limits too.
pub(crate) const OWNER_SETTINGS: [&str; 19] = [
    "approval",
    "phase_approval",
    "max_members",
    "worktrees_forbidden",
    "tree_depth",
    "tree_sessions",
    "subteam_approval",
    "auto_accept",
    // N6 (U18): the lead cannot switch off the owner's escalations.
    "remind_owner_minutes",
    "remind_owner_permission_minutes",
    "work_ahead",
    // U48: a land stage may push, and a branch per task moves members' folders.
    "land_allowed",
    "branch_per_task",
    "base_branch",
    // U52: only the owner lets Zerus launch a crashed member again.
    "restart_crashed",
    // U56: only the owner lets the lead restart a stuck member.
    "lead_restarts_stuck",
    // #752: only the owner turns the build broker on.
    "build_broker",
    // U62: the owner's override of the project's test configuration.
    "tests",
    // U62: only the owner says who may start a full test run.
    "full_tests",
];

fn ensure_lead_settings(old: &Settings, new: &Settings, actor: &Actor, on_word: bool) -> anyhow::Result<()> {
    if matches!(actor, Actor::Human) {
        return Ok(());
    }
    let (old, new) = (serde_json::to_value(old)?, serde_json::to_value(new)?);
    for field in OWNER_SETTINGS {
        // U26: max_members may move on the owner's verified word.
        if field == "max_members" && on_word {
            continue;
        }
        ensure!(old.get(field) == new.get(field), "Only you change the team's {field}");
    }
    Ok(())
}

fn extend(team: &mut Team, input: &Value, actor: &Actor, offline: &BTreeSet<String>, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    ensure!(
        matches!(actor, Actor::Human | Actor::Intake(_)) || actor.is_lead(team),
        "Only the lead or you extend the team"
    );
    ensure!(
        matches!(team.status, TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused),
        "Only a planning, running or paused team takes a new phase"
    );
    ensure_draft_revision(team, input)?;
    // U59: a new goal's plan may carry the lead's mode proposal as `plan` does.
    if team.status == TeamStatus::Planning {
        if let Some(value) = input.get("plan_mode").filter(|value| !value.is_null()) {
            let mode: PlanMode = serde_json::from_value(value.clone()).context("The plan mode is standard or free")?;
            let reason = clean(text(input, "plan_mode_reason").trim(), 500, "A plan mode reason")?;
            ensure!(!reason.is_empty(), "A plan mode proposal needs its one-line reason");
            team.plan_mode_offer = Some(mode);
            team.plan_mode_reason = reason.clone();
            events.push(Event::new("plan_mode_proposed", json!({"mode":mode,"reason":reason})));
        }
    }
    let values = input["tasks"].as_array().filter(|tasks| !tasks.is_empty()).context("A phase needs at least one task")?;
    ensure!(values.len() <= 50, "A phase has at most 50 tasks");
    ensure!(team.tasks.len() + values.len() <= 500, "A team has at most 500 tasks (it has {})", team.tasks.len());
    ensure!(team.phases.len() < 100, "A team has at most 100 phases");
    // F10: with "phase": N the tasks join that draft phase instead of a new one.
    // U58: a queued or approved phase with no delivered attempt takes them too.
    let target = match input.get("phase") {
        None => None,
        Some(value) => {
            let n = value
                .as_u64()
                .and_then(|phase| u32::try_from(phase).ok())
                .filter(|phase| *phase > 0)
                .context("A phase number is required")?;
            Some(editable_phase(team, n)?)
        }
    };
    // D2: an urgent phase runs at once: no approval under review, no barrier,
    // and its tasks are handed out first. U51: it is approved automatically.
    let urgent = input["urgent"].as_bool() == Some(true);
    let approval = if urgent { Approval::Auto } else { phase_approval(team) };
    // U59: a free team's open phase takes the tasks as they come, with no new
    // phase and no approval; they run as added.
    let free_open = (team.plan_mode == PlanMode::Free && target.is_none()).then(|| open_phase(team));
    // Several drafts may queue, in order; a phase added while an earlier one is
    // still a Draft is a Draft too, whatever the approval says (F9). U58: tasks
    // added to an accepted phase are Pending, not Draft.
    let target_draft = target.is_some_and(|at| team.phases[at].state.is_draft());
    let drafting = if target.is_some() {
        target_draft
    } else {
        free_open.is_none()
            && !urgent
            && (team.status == TeamStatus::Planning
                || team.phases.iter().any(|phase| phase.state.is_draft())
                || approval == Approval::Review)
    };
    let title = clean(text(input, "phase_title").trim(), 120, "A phase title")?;
    let mut tasks = values.iter().map(parse_task).collect::<anyhow::Result<Vec<Task>>>()?;
    // Ids continue after the highest live number: a discarded draft frees its
    // numbers, and drafts close the gaps (F285). #754: tasks numbered otherwise
    // take those numbers, their dependencies with them.
    let renumbered = renumber_new_tasks(team, &mut tasks)?;
    let last = team.tasks.iter().map(|task| task.id).max().unwrap_or(0);
    let next = last.checked_add(1).context("Task numbers are exhausted")?;
    let end = next.checked_add(tasks.len() as u32).context("Task numbers are exhausted")?;
    let wanted: BTreeSet<u32> = (next..end).collect();
    let n = match target {
        Some(at) => team.phases[at].n,
        None => free_open.unwrap_or_else(|| {
            team.tasks
                .iter()
                .map(|task| task.phase)
                .chain(team.phases.iter().map(|phase| phase.n))
                .max()
                .unwrap_or(1)
                .checked_add(1)
                .unwrap_or(1)
        }),
    };
    for task in &mut tasks {
        task.phase = n;
        if !drafting {
            task.status = TaskStatus::Pending;
        }
    }
    // A dependency already cancelled or finally rejected can never let its new
    // dependent start, so extend refuses it instead of leaving it waiting.
    for task in &tasks {
        for dep in &task.deps {
            if let Some(found) = team.task(dep.task) {
                if matches!(found.status, TaskStatus::Cancelled | TaskStatus::Rejected) {
                    bail!(
                        "#{} depends on #{}, which is {}",
                        task.id,
                        found.id,
                        if found.status == TaskStatus::Cancelled { "cancelled" } else { "rejected" }
                    );
                }
            }
        }
    }
    let barrier = !urgent && input["barrier"].as_bool() == Some(true);
    let ids: Vec<u32> = tasks.iter().map(|task| task.id).collect();
    let by = actor.label(team);
    let state = if drafting { PhaseState::Draft } else { PhaseState::Approved };
    // The new phase goes after every phase that exists: phase 1, the planned
    // ones and any phase still named by a task (F10). Read before the new tasks
    // join, whose phase number is this phase's own.
    let order = team
        .tasks
        .iter()
        .map(|task| task.phase)
        .chain(team.phases.iter().map(|phase| phase.order()))
        .max()
        .unwrap_or(1)
        + 1;
    // Validate the team as the phase would make it, in place; a refusal puts
    // the tasks back (nothing changed) without cloning every stored report.
    let first_new = team.tasks.len();
    team.tasks.extend(tasks);
    let validation = plan::validate(team, offline);
    let errors = phase_errors(&validation, &wanted, true);
    if !errors.is_empty() {
        team.tasks.truncate(first_new);
        bail!("Fix {} phase error(s) to extend: {}", errors.len(), errors_text(&errors));
    }
    if let Some(at) = target {
        // F10: the tasks joined the named draft phase, which did not change
        // anything else; the owner sees the added ids as the diff.
        let added: Vec<(u32, String)> = team
            .tasks
            .iter()
            .filter(|task| ids.contains(&task.id))
            .map(|task| (task.id, task.title.clone()))
            .collect();
        for (id, title) in added {
            record_change(&mut team.phases[at], json!({"kind":"added","task":id,"title":title}));
        }
        team.phases[at].tasks.extend(ids.iter().copied());
        events.push(Event::new("phase_edited", json!({"by":by,"phase":n,"diff":{"added":ids}})));
        return Ok(json!({"status":"edited","phase":n,"tasks":ids,"validation":validation,"renumbered":renumbered_json(&renumbered)}));
    }
    if let Some(n) = free_open {
        // U59: the open phase's record takes the new tasks; no phase is added.
        if let Some(at) = team.phases.iter().position(|phase| phase.n == n) {
            team.phases[at].tasks.extend(ids.iter().copied());
        }
        events.push(Event::new("phase_edited", json!({"by":by,"phase":n,"diff":{"added":ids},"free":true})));
        return Ok(json!({"status":"added","phase":n,"tasks":ids,"validation":validation,"renumbered":renumbered_json(&renumbered)}));
    }
    team.phases.push(Phase {
        n,
        order,
        title: title.clone(),
        barrier,
        approval,
        state,
        tasks: ids.clone(),
        added_by: by.clone(),
        added_at: now(),
        decided_by: String::new(),
        decided_at: None,
        owner_word: None,
        changes: Vec::new(),
        urgent,
        start_ahead: false,
        reason: String::new(),
        note: String::new(),
        rejection: None,
    });
    events.push(Event::new(
        "phase_added",
        json!({"by":by,"phase":n,"title":title,"tasks":ids.len(),"barrier":barrier,"approval":approval}),
    ));
    Ok(json!({"status":"added","phase":n,"tasks":ids,"approval":approval,"validation":validation,"renumbered":renumbered_json(&renumbered)}))
}

/// U51: the hash of an intake key; the team keeps only this.
pub(crate) fn intake_hash(key: &str) -> String {
    format!("{:x}", Sha256::digest(key.as_bytes()))
}

/// U51: a fresh intake key, shown once to the owner.
pub(crate) fn intake_secret() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// U51: the label of the key a process presents, when this team has it.
pub(crate) fn intake_label(team: &Team, key: &str) -> Option<String> {
    if key.is_empty() {
        return None;
    }
    let hash = intake_hash(key);
    team.intakes
        .iter()
        .find(|intake| !intake.hash.is_empty() && intake.hash == hash)
        .map(|intake| intake.label.clone())
}

/// U51: the owner creates a key, or revokes one by id. The key is returned
/// once; the team keeps only its hash and names its label in the feed.
fn intake_key(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    ensure!(matches!(actor, Actor::Human), "Only you create or revoke an intake key");
    let revoke = text(input, "revoke").trim().to_owned();
    if !revoke.is_empty() {
        let position = team
            .intakes
            .iter()
            .position(|intake| intake.id == revoke)
            .with_context(|| format!("Unknown intake key {revoke}"))?;
        let removed = team.intakes.remove(position);
        events.push(Event::new("intake_revoked", json!({"id":removed.id,"label":removed.label})));
        return Ok(json!({"status":"revoked","id":revoke}));
    }
    let label = clean(text(input, "label").trim(), 80, "An intake label")?;
    ensure!(!label.is_empty(), "An intake key needs a label");
    ensure!(team.intakes.len() < 8, "A team keeps at most 8 intake keys");
    let key = intake_secret();
    team.intakes_seq += 1;
    let id = format!("i{}", team.intakes_seq);
    team.intakes.push(Intake { id: id.clone(), label: label.clone(), hash: intake_hash(&key), created: now() });
    events.push(Event::new("intake_key", json!({"id":id,"label":label})));
    Ok(json!({"status":"created","id":id,"label":label,"key":key}))
}

/// U51: one bounded urgent phase from the owner's intake key. The key adds
/// tasks; it gets none of the owner's other rights, and the feed names its
/// label.
fn intake_phase(team: &mut Team, input: &Value, actor: &Actor, offline: &BTreeSet<String>, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    let label = match actor {
        Actor::Intake(label) => label.clone(),
        _ => bail!("An intake key adds one urgent phase"),
    };
    ensure!(input.get("phase").is_none(), "An intake adds its own phase; it cannot name one");
    let values = input["tasks"].as_array().context("An intake carries \"tasks\"")?;
    ensure!((1..=5).contains(&values.len()), "An intake adds 1 to 5 tasks");
    // L3: at most one open urgent phase per key, so a watcher in a loop cannot
    // starve the plan with phase after phase.
    let by = format!("Intake {label}");
    if let Some(open) = team.phases.iter().find(|phase| {
        phase.urgent
            && phase.added_by == by
            && phase.tasks.iter().any(|id| team.task(*id).is_some_and(|task| !task.status.finished()))
    }) {
        bail!("The key's urgent phase {} is still open; finish it before adding more", open.n);
    }
    let last = team.tasks.iter().map(|task| task.id).max().unwrap_or(0);
    let next = last.checked_add(1).context("Task numbers are exhausted")?;
    let mut tasks = Vec::new();
    for (index, value) in values.iter().enumerate() {
        // N4: the key keeps only the plan fields U51 gives it - title,
        // description, criteria, files and an assignee or a role. The owner's
        // other plan fields (pipeline, models, pinned, lane, deps, check, kind)
        // are not the key's: a "pipeline":"none" would skip the owner's gates.
        let source = value.as_object().context("An intake task is an object")?;
        let mut kept = serde_json::Map::new();
        for key in ["title", "description", "criteria", "files", "assignee", "role"] {
            if let Some(field) = source.get(key) {
                kept.insert(key.to_owned(), field.clone());
            }
        }
        if kept.contains_key("assignee") {
            kept.remove("role");
        }
        kept.insert("id".into(), json!(next + index as u32));
        tasks.push(Value::Object(kept));
    }
    let filled = json!({"urgent":true,"phase_title":format!("Urgent: {label}"),"tasks":tasks});
    let added = extend(team, &filled, actor, offline, events)?;
    events.push(Event::new("intake", json!({"key":label,"phase":added["phase"],"tasks":added["tasks"]})));
    Ok(json!({"status":"intake","phase":added["phase"],"tasks":added["tasks"],"validation":added["validation"]}))
}

/// U29: how long a warned addition waits for the owner's confirmation.
const PENDING_ADDITION_SECONDS: f64 = super::owner_word::FRESH_SECONDS;

/// U29: a warned addition still waits for the owner's confirmation; an
/// expired one is neither listed nor confirmed, and leaves on the team's next
/// change.
pub(crate) fn addition_fresh(pending: &PendingAddition, now: f64) -> bool {
    now - pending.warned_at <= PENDING_ADDITION_SECONDS
}
/// U29: at most this many additions wait at once (one per phase).
const PENDING_ADDITIONS: usize = 8;
/// U29: at most this many tasks join a running phase in one addition.
const ADDED_TASKS: usize = 20;

/// U29: what the lead shows the owner (and the desktop asks) before a task
/// joins a running phase. #511 C1: it names the queued task titles, so the
/// owner confirms what joins, not a blind "add"; titles are cut and the text
/// stays bounded.
pub(crate) fn addition_warning(phase: u32, titles: &[String]) -> String {
    let listed: Vec<String> = titles
        .iter()
        .map(|title| {
            let mut short: String = title.chars().take(100).collect();
            if short.chars().count() < title.chars().count() {
                short.push('…');
            }
            format!("\"{short}\"")
        })
        .collect();
    let what = if listed.len() == 1 {
        format!("the task {}", listed.first().map(String::as_str).unwrap_or_default())
    } else {
        format!("{} tasks {}", listed.len(), listed.join(", "))
    };
    format!(
        "Adding {what} to running phase {phase} changes its scope after you approved it. The load balance of the lanes \
         shifts and members may sit idle: members do not work ahead and later phases are not approved. \
         Confirm in a new message to add them (type \"add\" or \"{phase}\" on the desktop)."
    )
}

/// U29: tasks join a running phase only on the owner's word twice. The first
/// call (a request word from the lead's session, or the owner in person)
/// changes nothing: it keeps the addition pending and returns the warning.
/// The second call names that pending addition and carries a confirmation
/// newer than the warning; only then the tasks join as Pending with
/// `added_late`. A member never adds; a draft phase keeps edit-draft.
/// Owner 2026-10-31 (#754): new tasks take the team's next free numbers when
/// they join, whatever numbers they were proposed with (the lead's p11 named
/// #747 and #748, already taken, and Allow failed), in the order given; their
/// dependencies on each other follow them. A number given twice and a
/// dependency on a task neither in the plan nor among the new tasks are
/// refused (parse_task already keeps a dependency named twice once). Returns
/// the numbers that changed, old to new.
fn renumber_new_tasks(team: &Team, tasks: &mut [Task]) -> anyhow::Result<BTreeMap<u32, u32>> {
    let mut given = BTreeSet::new();
    for task in tasks.iter() {
        ensure!(given.insert(task.id), "Task #{} is given twice among the new tasks", task.id);
    }
    let next = team.tasks.iter().map(|task| task.id).max().unwrap_or(0).checked_add(1).context("Task numbers are exhausted")?;
    let mut numbers = BTreeMap::new();
    for (index, task) in tasks.iter().enumerate() {
        let number = u32::try_from(index).ok().and_then(|index| next.checked_add(index)).context("Task numbers are exhausted")?;
        numbers.insert(task.id, number);
    }
    for task in tasks.iter_mut() {
        for dep in &mut task.deps {
            match numbers.get(&dep.task) {
                Some(number) => dep.task = *number,
                None => ensure!(
                    team.task(dep.task).is_some(),
                    "Task #{} depends on #{}, which is neither in the plan nor among the new tasks",
                    task.id,
                    dep.task
                ),
            }
        }
        task.id = numbers[&task.id];
    }
    Ok(numbers.into_iter().filter(|(old, new)| old != new).collect())
}

/// The changed numbers as the receipts name them: {"747": 760}.
fn renumbered_json(numbers: &BTreeMap<u32, u32>) -> Value {
    Value::Object(numbers.iter().map(|(old, new)| (old.to_string(), json!(new))).collect())
}

/// The owner declines a warned addition: it leaves and nothing joins. The
/// owner's Cancel in Waiting for you sends it, and so does an Allow whose
/// confirmation failed after its warning was stored. A decline of an addition
/// that is already gone (confirmed, replaced, expired or declined) changes
/// nothing and still succeeds, so a retried Cancel never strands the item;
/// the phase need not be running any more.
fn decline_addition(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    ensure!(matches!(actor, Actor::Human), "Only you decline an addition to a running phase");
    let id = input["confirm"].as_str().filter(|id| !id.is_empty()).context("Name the addition to decline in \"confirm\"")?;
    let Some(found) = team.pending_additions.iter().position(|pending| pending.id == id) else {
        return Ok(json!({"status":"declined","pending":id,"found":false}));
    };
    let pending = team.pending_additions.remove(found);
    let ids: Vec<u32> = pending.tasks.iter().map(|task| task.id).collect();
    events.push(Event::new(
        "phase_addition_declined",
        json!({"by":actor.label(team),"phase":pending.phase,"tasks":ids,"request":pending.request}),
    ));
    Ok(json!({"status":"declined","pending":id,"found":true,"phase":pending.phase,"tasks":ids}))
}

fn add_to_phase(team: &mut Team, input: &Value, actor: &Actor, offline: &BTreeSet<String>, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    if input["decline"].as_bool() == Some(true) {
        return decline_addition(team, input, actor, events);
    }
    ensure!(matches!(actor, Actor::Human) || actor.is_lead(team), "Only the lead, on your word, or you add a task to a running phase");
    ensure!(matches!(team.status, TeamStatus::Running | TeamStatus::Paused), "Only a running or paused team takes a task in a running phase");
    let n = phase_number(input)?;
    if let Some(record) = team.phases.iter().find(|phase| phase.n == n) {
        ensure!(!record.state.is_draft(), "Phase {n} is a draft: change it with edit-draft");
    }
    let running = plan::phase_summary(team).iter().any(|phase| phase["n"].as_u64() == Some(u64::from(n)) && phase["state"] == "running");
    ensure!(running, "Phase {n} is not running; add the task to a draft phase or extend the plan");
    let word = verified_word(input)?;
    ensure!(!actor.is_lead(team) || word.is_some(), "The lead adds a task to a running phase only on your word: pass \"owner_word\"");
    let at = now();
    team.pending_additions.retain(|pending| addition_fresh(pending, at));
    let by = actor.label(team);
    let quote = word.as_ref().map(|word| word.text.clone()).unwrap_or_default();
    // The tasks as they would join: numbered on from the highest live id,
    // Pending in this phase; validated in place and put back.
    let check = |team: &mut Team, tasks: &[Task]| -> anyhow::Result<plan::Validation> {
        let next = team.tasks.iter().map(|task| task.id).max().unwrap_or(0) + 1;
        let wanted: BTreeSet<u32> = (next..next + tasks.len() as u32).collect();
        ensure!(tasks.iter().map(|task| task.id).collect::<BTreeSet<u32>>() == wanted, "New tasks are numbered from #{next}");
        let first_new = team.tasks.len();
        team.tasks.extend(tasks.iter().cloned());
        let validation = plan::validate(team, offline);
        let errors = phase_errors(&validation, &wanted, true);
        team.tasks.truncate(first_new);
        ensure!(errors.is_empty(), "Fix {} task error(s) to add: {}", errors.len(), errors_text(&errors));
        Ok(validation)
    };
    let Some(confirm) = input["confirm"].as_str().filter(|id| !id.is_empty()) else {
        let values = input["tasks"].as_array().filter(|tasks| !tasks.is_empty()).context("Name at least one task to add")?;
        ensure!(values.len() <= ADDED_TASKS, "At most {ADDED_TASKS} tasks join a running phase at once");
        ensure!(team.tasks.len() + values.len() <= 500, "A team has at most 500 tasks (it has {})", team.tasks.len());
        let mut tasks = values.iter().map(parse_task).collect::<anyhow::Result<Vec<Task>>>()?;
        for task in &mut tasks {
            task.phase = n;
            task.status = TaskStatus::Pending;
        }
        // #754: the next free numbers, whatever the proposal named.
        let renumbered = renumber_new_tasks(team, &mut tasks)?;
        check(team, &tasks)?;
        // One waiting addition per phase: a new request replaces the old one.
        team.pending_additions.retain(|pending| pending.phase != n);
        ensure!(team.pending_additions.len() < PENDING_ADDITIONS, "{PENDING_ADDITIONS} additions already wait for your confirmation");
        let ids: Vec<u32> = tasks.iter().map(|task| task.id).collect();
        // #511 C1: the warning names what will join, so the owner confirms the
        // titles, not a blind "add".
        let titles: Vec<String> = tasks.iter().map(|task| task.title.clone()).collect();
        let id = uuid::Uuid::new_v4().to_string();
        team.pending_additions.push(PendingAddition { id: id.clone(), phase: n, tasks, by: by.clone(), request: quote.clone(), warned_at: at });
        events.push(Event::new("phase_addition_warned", json!({"by":by,"phase":n,"tasks":ids,"request":quote})));
        return Ok(json!({"status":"warning","phase":n,"pending":id,"tasks":ids,"warning":addition_warning(n, &titles),
            "expires_at":at + PENDING_ADDITION_SECONDS,"renumbered":renumbered_json(&renumbered)}));
    };
    let found = team.pending_additions.iter().position(|pending| pending.id == confirm).context("That addition is unknown or expired; ask again")?;
    let pending = &team.pending_additions[found];
    ensure!(pending.phase == n, "That addition is for phase {}, not phase {n}", pending.phase);
    let confirmation = match (&word, actor) {
        (Some(word), _) => {
            ensure!(word.at > pending.warned_at, "The confirmation must come after the warning");
            word.text.clone()
        }
        (None, Actor::Human) => {
            // The owner on the desktop types the word "add" or the phase number.
            let typed = text(input, "confirmation").trim().to_lowercase();
            ensure!(typed == "add" || typed == n.to_string(), "Type \"add\" or \"{n}\" to confirm the addition");
            typed
        }
        (None, _) => bail!("The lead adds a task to a running phase only on your word: pass \"owner_word\""),
    };
    let pending = team.pending_additions.remove(found);
    // #754: the plan may have grown since the warning: the tasks move on to the
    // next free numbers again, their dependencies with them.
    let mut tasks = pending.tasks;
    let renumbered = renumber_new_tasks(team, &mut tasks)?;
    for task in &mut tasks {
        task.added_late = Some(AddedLate { by: by.clone(), at, request: pending.request.clone(), confirm: confirmation.clone() });
    }
    let validation = check(team, &tasks).context("The plan changed since the warning; these tasks no longer fit it")?;
    let ids: Vec<u32> = tasks.iter().map(|task| task.id).collect();
    team.tasks.extend(tasks);
    if let Some(record) = team.phases.iter_mut().find(|phase| phase.n == n) {
        record.tasks.extend(ids.iter().copied());
    }
    let health = plan::phase_health(team, n);
    events.push(Event::new(
        "phase_tasks_added",
        json!({"by":by,"phase":n,"tasks":ids,"request":pending.request,"confirm":confirmation,
            "critical_path":health.critical_path,"parallelism":health.parallelism}),
    ));
    Ok(json!({"status":"added","phase":n,"tasks":ids,"validation":validation,"renumbered":renumbered_json(&renumbered)}))
}

/// Approves a draft phase once every phase before it ended (owner), or discards
/// it with its tasks (the lead or the owner, F10).
fn decide_phase(team: &mut Team, input: &Value, actor: &Actor, approve: bool, offline: &BTreeSet<String>, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    if approve {
        ensure_subteam_approver(team, actor)?;
    }
    let owner_word = verified_word(input)?;
    // C1: the owner's word is the lead's route; a member can never approve a
    // phase on a word the CLI did not verify in the lead's own session.
    ensure!(owner_word.is_none() || actor.is_lead(team), "Only the lead approves on the owner's word");
    if approve {
        // F13: only the owner approves in person; the lead does it on the
        // owner's exact sentence, checked by the CLI against its session.
        // Section 9.3: a parent lead approves its sub-team's phases.
        ensure!(
            matches!(actor, Actor::Human) || parent_approves(team, actor) || owner_word.is_some(),
            "The lead approves a phase only on the owner's word: pass \"owner_word\""
        );
    }
    ensure!(matches!(team.status, TeamStatus::Running | TeamStatus::Paused), "The team is not running");
    let n = input["phase"]
        .as_u64()
        .and_then(|phase| u32::try_from(phase).ok())
        .filter(|phase| *phase > 0)
        .context("A phase number is required")?;
    // Owner, 2026-10-28: a phase can be discarded in every state but done
    // (U39: an approval the owner did not mean could not be taken back). A
    // draft, also one sent back, is the lead's or yours to drop (F10, U25);
    // an accepted phase (approved or queued) only yours, or the lead's on your
    // word, and only while none of its work has started, so nothing done is
    // erased. Only a plain draft waits for approval.
    let draft = team.phases.iter().any(|phase| phase.n == n && phase.state.is_draft());
    let accepted = !approve && team.phases.iter().any(|phase| phase.n == n && phase.state.is_accepted());
    // U56: an accepted phase left with no tasks (they were moved out, U58) is
    // not done: it stays yours to discard, or the lead's on your word. An empty
    // draft is the lead's to discard like any draft (#645 N5).
    let empty = accepted && team.phases.iter().any(|phase| phase.n == n && phase.tasks.is_empty());
    if !approve {
        if draft {
            ensure!(
                matches!(actor, Actor::Human) || actor.is_lead(team) || parent_approves(team, actor),
                "Only the lead or you discard a phase"
            );
            ensure_draft_revision(team, input)?;
        } else if accepted {
            ensure!(
                matches!(actor, Actor::Human) || parent_approves(team, actor) || owner_word.is_some(),
                "Only you discard an accepted phase; the lead does it on your word: pass \"owner_word\""
            );
        }
    }
    let at = team
        .phases
        .iter()
        .position(|phase| phase.n == n && if approve { phase.state == PhaseState::Draft } else { phase.state.is_draft() || phase.state.is_accepted() })
        .with_context(|| match team.phases.iter().find(|phase| phase.n == n) {
            Some(phase) if approve && phase.state == PhaseState::Rejected => {
                format!("Phase {n} was sent back to the lead; undo the send-back or wait for the rework")
            }
            Some(_) if !approve => format!("Phase {n} cannot be discarded"),
            _ => format!("Phase {n} is not waiting for approval"),
        })?;
    if accepted && !empty {
        let tasks = team.phases[at].tasks.clone();
        // U57: a phase with no work done may be discarded even while it runs:
        // none of its tasks accepted and none delivered by a task that was not
        // cancelled. Its cancelled tasks do not keep it.
        let accepted_any = tasks
            .iter()
            .any(|id| team.task(*id).is_some_and(|task| task.status == TaskStatus::Accepted));
        ensure!(!accepted_any, "Phase {n} has accepted work; a phase with accepted work stays");
        let delivered: Vec<String> = tasks
            .iter()
            .filter_map(|id| team.task(*id))
            .filter(|task| {
                task.status != TaskStatus::Cancelled
                    && task
                        .attempts
                        .iter()
                        .any(|attempt| !matches!(attempt.state, AttemptState::Revoked | AttemptState::Refused))
            })
            .map(|task| format!("#{}", task.id))
            .collect();
        ensure!(
            delivered.is_empty(),
            "Phase {n} has delivered work ({}): cancel those tasks first, then discard the phase",
            delivered.join(", ")
        );
    }
    let ids = team.phases[at].tasks.clone();
    let wanted: BTreeSet<u32> = ids.iter().copied().collect();
    if approve {
        accept_blockers(team, at, offline)?;
    }
    let label = actor.label(team);
    let count = ids.len();
    if approve {
        // N6/F12: the owner's confirmed "approve alongside" hands the phase's
        // tasks out now; without it a phase behind open work queues (U25).
        let alongside = input["alongside"].as_bool() == Some(true);
        let state = accept_phase(team, at, &label, alongside);
        // F13: the phase keeps the owner's sentence when the lead opened it.
        team.phases[at].owner_word = owner_word.clone();
        events.push(Event::new(
            "phase_approved",
            json!({"by":label,"phase":n,"tasks":count,"owner_word":owner_word,"state":state,"alongside":alongside}),
        ));
        Ok(json!({"status":"phase_approved","phase":n,"tasks":ids,"owner_word":owner_word,"phase_state":state,"alongside":alongside}))
    } else {
        // F285: a discarded draft leaves no trace in the numbering. The record
        // goes, its Draft tasks go, and the later drafts close the gaps; the
        // discarded event keeps their titles.
        let record = team.phases.remove(at);
        let titles: Vec<Value> = record
            .tasks
            .iter()
            .filter_map(|id| team.task(*id).map(|task| json!({"id":id,"title":task.title})))
            .collect();
        team.tasks.retain(|task| !wanted.contains(&task.id));
        // A surviving task cannot depend on a discarded one (F285).
        for task in &mut team.tasks {
            task.deps.retain(|dep| !wanted.contains(&dep.task));
        }
        events.push(Event::new(
            "phase_discarded",
            json!({"by":label,"phase":n,"title":record.title,"tasks":titles,"state":record.state,"owner_word":owner_word}),
        ));
        // F11: a discard invalidates the draft numbers a caller may hold.
        team.phase_numbers_revision = team.revision + 1;
        let renumbering = renumbered(team, &label, events);
        Ok(json!({"status":"phase_discarded","phase":n,"tasks":ids,"renumbered":renumbering}))
    }
}

/// Why the draft at `at` cannot be accepted yet: an earlier phase is still a
/// draft (F12, in the display and approval order of F10) or one of its tasks
/// has a plan error (an assignee may have been removed since it was added).
/// Phase 1 has no record and counts as approved.
fn accept_blockers(team: &Team, at: usize, offline: &BTreeSet<String>) -> anyhow::Result<()> {
    let n = team.phases[at].n;
    let target = team.phases[at].order();
    let mut numbers: BTreeSet<u32> = team.tasks.iter().map(|task| task.phase).collect();
    numbers.extend(team.phases.iter().map(|phase| phase.n));
    for phase_n in numbers {
        let listed = team.phases.iter().find(|phase| phase.n == phase_n);
        let order = listed.map_or(phase_n, |phase| phase.order());
        if order >= target {
            continue;
        }
        // F12: the earlier phase must be approved, not finished: an accepted
        // phase behind it queues until it ends (U25).
        ensure!(
            listed.is_none_or(|phase| !phase.state.is_draft()),
            "Phase {n} opens after the previous phase is approved"
        );
    }
    let wanted: BTreeSet<u32> = team.phases[at].tasks.iter().copied().collect();
    let validation = plan::validate(team, offline);
    let errors = phase_errors(&validation, &wanted, false);
    ensure!(errors.is_empty(), "Fix {} phase error(s) to approve: {}", errors.len(), errors_text(&errors));
    Ok(())
}

/// U25: whether a phase still waits for earlier work: some task of a phase
/// before it in the display order is not finished.
fn earlier_open(team: &Team, at: usize) -> bool {
    let record = &team.phases[at];
    let target = record.order();
    let order_of = |n: u32| team.phases.iter().find(|phase| phase.n == n).map_or(n, |phase| phase.order());
    team.tasks.iter().any(|task| task.phase != record.n && order_of(task.phase) < target && !task.status.finished())
}

/// Accepts the draft at `at`: its Draft tasks become Pending, and the phase
/// runs at once when every earlier phase is finished, else it queues behind
/// them (U25) - unless the owner confirmed approving it to run alongside them
/// (F12/N6), when it is approved and its barrier, if any, still holds it.
/// Returns the state the receipt and the feed name.
fn accept_phase(team: &mut Team, at: usize, label: &str, alongside: bool) -> &'static str {
    let wanted: BTreeSet<u32> = team.phases[at].tasks.iter().copied().collect();
    for task in &mut team.tasks {
        if task.status == TaskStatus::Draft && wanted.contains(&task.id) {
            task.status = TaskStatus::Pending;
        }
    }
    let queued = !alongside && earlier_open(team, at);
    let record = &mut team.phases[at];
    record.state = if queued { PhaseState::Queued } else { PhaseState::Approved };
    record.decided_by = label.to_owned();
    record.decided_at = Some(now());
    if queued {
        "queued"
    } else {
        "running"
    }
}

/// U39: the tasks of an accepted phase whose work started or was decided.
fn started_tasks(team: &Team, at: usize) -> Vec<String> {
    team.phases[at]
        .tasks
        .iter()
        .filter_map(|id| team.task(*id))
        .filter(|task| {
            task.status.finished()
                || !matches!(task.status, TaskStatus::Pending | TaskStatus::Draft)
                || task.attempts.iter().any(|attempt| !matches!(attempt.state, AttemptState::Revoked | AttemptState::Refused))
        })
        .map(|task| format!("#{}", task.id))
        .collect()
}

/// U42 (#587): an approved or queued phase whose work has not started goes
/// back to a draft that waits for the owner's approval again: you do it, or
/// the lead on your word naming the phase. Its tasks return to Draft and the
/// drafts are numbered after the accepted phases again.
fn return_to_draft(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    let owner_word = verified_word(input)?;
    ensure!(owner_word.is_none() || actor.is_lead(team), "Only the lead acts on the owner's word");
    ensure!(
        matches!(actor, Actor::Human) || parent_approves(team, actor) || owner_word.is_some(),
        "Only you return a phase to a draft; the lead does it on your word: pass \"owner_word\""
    );
    ensure!(matches!(team.status, TeamStatus::Running | TeamStatus::Paused), "The team is not running");
    let n = phase_number(input)?;
    let at = team.phases.iter().position(|phase| phase.n == n).with_context(|| format!("Unknown phase {n}"))?;
    ensure!(team.phases[at].state.is_accepted(), "Phase {n} is not approved or queued; only an accepted phase returns to a draft");
    ensure!(
        !team.settings.auto_accept,
        "Auto-accept is on and would accept the draft again at once: turn it off first"
    );
    let tasks = team.phases[at].tasks.clone();
    ensure!(
        !tasks.is_empty() && !tasks.iter().all(|id| team.task(*id).is_none_or(|task| task.status.finished())),
        "Phase {n} is done; a finished phase stays"
    );
    let started = started_tasks(team, at);
    ensure!(
        started.is_empty(),
        "Phase {n} has started work ({}): cancel those tasks first, then return the phase to a draft",
        started.join(", ")
    );
    let wanted: BTreeSet<u32> = tasks.iter().copied().collect();
    for task in &mut team.tasks {
        if wanted.contains(&task.id) && task.status == TaskStatus::Pending {
            task.status = TaskStatus::Draft;
        }
    }
    let label = actor.label(team);
    let record = &mut team.phases[at];
    let from = record.state;
    record.state = PhaseState::Draft;
    record.decided_by.clear();
    record.decided_at = None;
    record.owner_word = None;
    let title = record.title.clone();
    events.push(Event::new(
        "phase_returned_to_draft",
        json!({"by":label,"phase":n,"title":title,"tasks":tasks,"from":from,"owner_word":owner_word}),
    ));
    // A draft is numbered after every accepted phase (F285).
    let renumbering = renumbered(team, &label, events);
    let now_n = renumbering["phases"].as_array().into_iter().flatten().find(|moved| moved["from"] == n).and_then(|moved| moved["to"].as_u64()).map_or(n, |to| to as u32);
    let ids = team.phases.iter().find(|phase| phase.n == now_n).map(|phase| phase.tasks.clone()).unwrap_or_default();
    Ok(json!({"status":"phase_returned_to_draft","phase":now_n,"from_phase":n,"tasks":ids,"renumbered":renumbering}))
}

/// The phase number of a request.
fn phase_number(input: &Value) -> anyhow::Result<u32> {
    input["phase"]
        .as_u64()
        .and_then(|phase| u32::try_from(phase).ok())
        .filter(|phase| *phase > 0)
        .context("A phase number is required")
}

/// The longest send-back comment and rework note (U25).
const REJECT_COMMENT: usize = 2000;

/// U25: the owner sends a draft back to the lead with a comment and may undo
/// it until the lead reworked it; the lead reworks it into a draft again,
/// with a note quoting the comment.
fn send_back(team: &mut Team, op: &str, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    ensure!(matches!(team.status, TeamStatus::Running | TeamStatus::Paused), "The team is not running");
    let n = phase_number(input)?;
    let at = team.phases.iter().position(|phase| phase.n == n).with_context(|| format!("Unknown phase {n}"))?;
    let label = actor.label(team);
    let title = team.phases[at].title.clone();
    match op {
        "reject-phase" => {
            ensure_subteam_approver(team, actor)?;
            ensure!(matches!(actor, Actor::Human) || parent_approves(team, actor), "Only you send a phase back");
            ensure!(team.phases[at].state == PhaseState::Draft, "Phase {n} is not waiting for approval");
            // N14 (#383): words written for a phase before the drafts were
            // renumbered never reach the phase that now has its number.
            ensure_draft_revision(team, input)?;
            let comment = clean(text(input, "comment").trim(), REJECT_COMMENT, "A comment")?;
            ensure!(!comment.is_empty(), "Say what the lead should change in this phase");
            let record = &mut team.phases[at];
            record.state = PhaseState::Rejected;
            record.rejection = Some(PhaseRejection { comment: comment.clone(), by: label.clone(), at: now(), reworked_at: None });
            // U31: sending the phase back revokes work done ahead of it. #511 N3:
            // a member still on it (or whose report waits) is told to stop.
            let at_now = now();
            let mut stops = Vec::new();
            for task in team.tasks.iter_mut().filter(|task| task.phase == n) {
                if task.attempts.iter().any(|attempt| attempt.worked_ahead) {
                    for attempt in task.attempts.iter().filter(|attempt| {
                        attempt.worked_ahead
                            && matches!(attempt.state, AttemptState::Delivering | AttemptState::Delivered | AttemptState::Unconfirmed | AttemptState::Reported)
                    }) {
                        stops.push(StopNotice {
                            id: uuid::Uuid::new_v4().to_string(),
                            task: task.id,
                            title: task.title.clone(),
                            member: attempt.member.clone(),
                            attempt: attempt.n,
                            phase: n,
                            reason: format!("phase {n} was sent back"),
                            at: at_now,
                            notice: Notice::Pending,
                            refused: 0,
                        });
                    }
                    task.status = TaskStatus::Draft;
                    task.attempts.clear();
                }
            }
            for stop in &stops {
                events.push(Event::new("work_ahead_revoked", json!({"by":label,"phase":n,"task":stop.task,"member":team.role_of(&stop.member),"attempt":stop.attempt})));
            }
            team.stops.extend(stops);
            let excess = team.stops.len().saturating_sub(20);
            team.stops.drain(..excess);
            events.push(Event::new("phase_rejected", json!({"by":label,"phase":n,"title":title,"comment":comment})));
            Ok(json!({"status":"phase_rejected","phase":n}))
        }
        "undo-reject-phase" => {
            ensure_subteam_approver(team, actor)?;
            ensure!(matches!(actor, Actor::Human) || parent_approves(team, actor), "Only you undo a send-back");
            ensure!(team.phases[at].state == PhaseState::Rejected, "Phase {n} was not sent back");
            let record = &mut team.phases[at];
            record.state = PhaseState::Draft;
            let comment = record.rejection.take().map(|rejection| rejection.comment).unwrap_or_default();
            events.push(Event::new("phase_reject_undone", json!({"by":label,"phase":n,"title":title,"comment":comment})));
            Ok(json!({"status":"phase_reject_undone","phase":n}))
        }
        _ => {
            ensure!(matches!(actor, Actor::Human) || actor.is_lead(team), "Only the lead or you rework a phase");
            ensure!(team.phases[at].state == PhaseState::Rejected, "Phase {n} was not sent back");
            ensure_draft_revision(team, input)?;
            let extra = clean(text(input, "note").trim(), REJECT_COMMENT, "A note")?;
            let record = &mut team.phases[at];
            let rejection = record.rejection.get_or_insert_with(Default::default);
            rejection.reworked_at = Some(now());
            let mut note = format!("Reworked after your comment: \u{201c}{}\u{201d}", rejection.comment);
            if !extra.is_empty() {
                note += &format!(" \u{b7} {extra}");
            }
            record.state = PhaseState::Draft;
            record.note = note.clone();
            events.push(Event::new("phase_reworked", json!({"by":label,"phase":n,"title":title,"note":note})));
            Ok(json!({"status":"phase_reworked","phase":n,"note":note}))
        }
    }
}

/// U61: with auto_accept on, a draft stays a draft until its first task is
/// handed out; that hand-out accepts it, in its own change
/// (accept_phase_on_hand_out). A draft whose work already started is accepted
/// here so an older record keeps today's state.
fn auto_accept(team: &mut Team, offline: &BTreeSet<String>, events: &mut Vec<Event>) {
    if !team.settings.auto_accept || !matches!(team.status, TeamStatus::Running | TeamStatus::Paused) {
        return;
    }
    let mut order: Vec<usize> = (0..team.phases.len()).filter(|at| team.phases[*at].state.is_draft()).collect();
    order.sort_by_key(|at| (team.phases[*at].order(), team.phases[*at].n));
    for at in order {
        if team.phases[at].state != PhaseState::Draft || accept_blockers(team, at, offline).is_err() {
            continue;
        }
        let started = team.phases[at]
            .tasks
            .iter()
            .any(|id| team.task(*id).is_some_and(|task| !task.attempts.is_empty() || task.status == TaskStatus::InProgress));
        if !started {
            continue;
        }
        let state = accept_phase(team, at, "Auto", false);
        let record = &team.phases[at];
        events.push(Event::new(
            "phase_auto_accepted",
            json!({"phase":record.n,"title":record.title,"tasks":record.tasks.len(),"state":state}),
        ));
        team.phases[at].start_ahead = false;
    }
}

/// U61: the auto-accepted drafts whose phases ahead are done and whose plan has
/// no blocker: the dispatcher accepts them by Auto, and the ordinary ready path
/// then hands out (launch, rebalance, role routing).
pub(crate) fn drafts_ready_to_auto_accept(team: &Team, offline: &BTreeSet<String>) -> Vec<u32> {
    if !team.settings.auto_accept || !matches!(team.status, TeamStatus::Running | TeamStatus::Paused) {
        return Vec::new();
    }
    team.phases
        .iter()
        .filter(|phase| phase.state.is_draft() && phase.approval == Approval::Auto)
        .filter(|phase| {
            let ahead_done = team
                .tasks
                .iter()
                .filter(|task| task.phase == phase.n)
                .all(|task| super::plan::barrier_blockers(team, task).is_empty());
            ahead_done
        })
        .filter(|phase| {
            let at = team.phases.iter().position(|found| found.n == phase.n).unwrap_or(0);
            accept_blockers(team, at, offline).is_ok()
        })
        .map(|phase| phase.n)
        .collect()
}

/// U61: accepts one auto draft by Auto in its own change; false when it is no
/// longer a draft or its plan has a blocker.
pub(crate) fn accept_auto_draft(team: &mut Team, phase: u32, offline: &BTreeSet<String>, events: &mut Vec<Event>) -> bool {
    if !team.settings.auto_accept {
        return false;
    }
    let Some(at) = team
        .phases
        .iter()
        .position(|found| found.n == phase && found.state.is_draft() && found.approval == Approval::Auto)
    else {
        return false;
    };
    if accept_blockers(team, at, offline).is_err() {
        return false;
    }
    let state = accept_phase(team, at, "Auto", false);
    let record = &team.phases[at];
    events.push(Event::new(
        "phase_auto_accepted",
        json!({"phase":record.n,"title":record.title,"tasks":record.tasks.len(),"state":state}),
    ));
    team.phases[at].start_ahead = false;
    true
}

/// U61: the first hand-out of a task in an auto-accepted draft fixes the phase
/// by Auto, in the same change that hands the task out; a plan blocker keeps it
/// a draft, as auto_accept does.
pub(crate) fn accept_phase_on_hand_out(team: &mut Team, task_id: u32, events: &mut Vec<Event>) {
    if !team.settings.auto_accept {
        return;
    }
    let Some(task) = team.task(task_id) else { return };
    let Some(at) = team
        .phases
        .iter()
        .position(|phase| phase.n == task.phase && phase.state.is_draft() && phase.approval == Approval::Auto)
    else {
        return;
    };
    if accept_blockers(team, at, &BTreeSet::new()).is_err() {
        return;
    }
    let state = accept_phase(team, at, "Auto", false);
    let record = &team.phases[at];
    events.push(Event::new(
        "phase_auto_accepted",
        json!({"phase":record.n,"title":record.title,"tasks":record.tasks.len(),"state":state}),
    ));
    team.phases[at].start_ahead = false;
}

/// U59: adds the free phase's ordinary build task (assigned to the lead), one
/// open per phase, and records it in the phase's list when it has a record.
fn add_build_task(team: &mut Team, phase: u32, by: &str, events: &mut Vec<Event>) -> anyhow::Result<u32> {
    ensure!(
        !team.tasks.iter().any(|task| task.phase == phase && task.phase_build && !task.status.finished()),
        "Phase {phase} already has a build task"
    );
    let id = team.tasks.iter().map(|task| task.id).max().unwrap_or(0) + 1;
    let assignee = if team.member(&team.lead).is_some() { team.lead.clone() } else { String::new() };
    // U62 (#760): the merge queue tested its merged range already; the build
    // checks only what came after.
    let description = match super::merge_queue::untested_since(team) {
        Some(tested) => format!(
            "Integrate the accepted work and run the build. The merge queue tested the lead branch up to {tested}: \
             check only what came after it (hgs test affected {tested}..HEAD)."
        ),
        None => "Integrate the accepted work and run the build.".into(),
    };
    team.tasks.push(Task {
        id,
        title: format!("Build phase {phase}"),
        description,
        assignee,
        phase,
        status: TaskStatus::Pending,
        phase_build: true,
        ..Default::default()
    });
    if let Some(at) = team.phases.iter().position(|record| record.n == phase) {
        team.phases[at].tasks.push(id);
    }
    events.push(Event::new("phase_build_added", json!({"phase":phase,"task":id,"by":by})));
    Ok(id)
}

/// U56: how long a busy member may go without a session event; unset reads 10.
pub(crate) fn stuck_minutes(team: &Team) -> f64 {
    let minutes = team.settings.stuck_minutes;
    if minutes == 0 { 10.0 } else { f64::from(minutes) }
}

/// U59: the open phase of a free team: the latest approved phase, or phase 1
/// (the approved initial plan) when no phase was extended yet.
pub(crate) fn open_phase(team: &Team) -> u32 {
    team.phases
        .iter()
        .rev()
        .find(|phase| phase.state == PhaseState::Approved)
        .map(|phase| phase.n)
        .unwrap_or(1)
}

/// The accepted phases (phase 1 included) whose tasks are all finished.
fn phases_done(team: &Team) -> BTreeSet<u32> {
    // U59: a free team's open phase is never done by itself.
    if team.plan_mode == PlanMode::Free {
        return BTreeSet::new();
    }
    let mut numbers: BTreeSet<u32> = team.tasks.iter().map(|task| task.phase).collect();
    numbers.retain(|n| *n == 1 || team.phases.iter().any(|phase| phase.n == *n && phase.state.is_accepted()));
    numbers.retain(|n| team.tasks.iter().filter(|task| task.phase == *n).all(|task| task.status.finished()));
    numbers
}

/// U25: a phase whose tasks all ended says so once, and a queued phase starts
/// once every earlier phase is finished.
fn advance_phases(team: &mut Team, done_before: &BTreeSet<u32>, events: &mut Vec<Event>) {
    if !matches!(team.status, TeamStatus::Running | TeamStatus::Paused) || team.plan_mode == PlanMode::Free {
        // U59: a free phase never closes by itself and no phase starts behind it.
        return;
    }
    for n in phases_done(team).difference(done_before) {
        let title = team.phases.iter().find(|phase| phase.n == *n).map(|phase| phase.title.clone()).unwrap_or_default();
        events.push(Event::new("phase_done", json!({"phase":n,"title":title})));
    }
    let mut order: Vec<usize> = (0..team.phases.len()).filter(|at| team.phases[*at].state == PhaseState::Queued).collect();
    order.sort_by_key(|at| (team.phases[*at].order(), team.phases[*at].n));
    for at in order {
        if earlier_open(team, at) {
            return;
        }
        let record = &mut team.phases[at];
        record.state = PhaseState::Approved;
        events.push(Event::new("phase_started", json!({"phase":record.n,"title":record.title,"tasks":record.tasks.len()})));
    }
}

/// Numbers the listed phases by their position in the array, which is the
/// display and approval order (F10). Phase 1 is the approved plan and has no
/// record, so the listed phases keep orders from 2.
fn normalize_orders(team: &mut Team) {
    for (index, phase) in team.phases.iter_mut().enumerate() {
        phase.order = index as u32 + 2;
    }
}

/// F285: drafts stay contiguous after the last approved phase. Their phase
/// numbers and the ids of their tasks follow the highest live numbers outside
/// the drafts; dependencies inside the drafts are rewritten, dependencies on
/// approved work stay. Returns the old -> new mappings.
fn renumber_drafts(team: &mut Team) -> Value {
    let drafts: Vec<u32> = team.phases.iter().filter(|phase| phase.state.is_draft()).map(|phase| phase.n).collect();
    if drafts.is_empty() {
        return json!({"phases":[],"tasks":[]});
    }
    let last_approved = team.phases.iter().filter(|phase| phase.state.is_accepted()).map(|phase| phase.n).max().unwrap_or(1);
    let base = team.tasks.iter().filter(|task| !drafts.contains(&task.phase)).map(|task| task.id).max().unwrap_or(0);
    let phase_map: Vec<(u32, u32)> =
        drafts.iter().enumerate().map(|(index, old)| (*old, last_approved + 1 + index as u32)).collect();
    let mut task_map: Vec<(u32, u32)> = Vec::new();
    let mut next = base + 1;
    for old in &drafts {
        let Some(phase) = team.phases.iter().find(|phase| phase.n == *old) else { continue };
        let mut ids = phase.tasks.clone();
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            task_map.push((id, next));
            next += 1;
        }
    }
    for phase in &mut team.phases {
        if !phase.state.is_draft() {
            continue;
        }
        if let Some((_, new)) = phase_map.iter().find(|(from, _)| *from == phase.n) {
            phase.n = *new;
        }
        phase.tasks = phase.tasks.iter().map(|id| task_map.iter().find(|(from, _)| from == id).map_or(*id, |(_, to)| *to)).collect();
    }
    for task in &mut team.tasks {
        if let Some((_, new)) = task_map.iter().find(|(from, _)| *from == task.id) {
            task.id = *new;
            if let Some((_, new_phase)) = phase_map.iter().find(|(from, _)| *from == task.phase) {
                task.phase = *new_phase;
            }
        }
    }
    let map: std::collections::BTreeMap<u32, u32> = task_map.iter().copied().collect();
    for task in &mut team.tasks {
        for dep in &mut task.deps {
            if let Some(new) = map.get(&dep.task) {
                dep.task = *new;
            }
        }
    }
    // The display order is the phase number order now.
    team.phases.sort_by_key(|phase| phase.n);
    normalize_orders(team);
    // F11: the numbers a caller may still hold are stale from this revision.
    if phase_map.iter().any(|(from, to)| from != to) || task_map.iter().any(|(from, to)| from != to) {
        team.phase_numbers_revision = team.revision + 1;
    }
    let phases: Vec<Value> = phase_map.iter().filter(|(from, to)| from != to).map(|(from, to)| json!({"from":from,"to":to})).collect();
    let tasks: Vec<Value> = task_map.iter().filter(|(from, to)| from != to).map(|(from, to)| json!({"from":from,"to":to})).collect();
    json!({"phases":phases,"tasks":tasks})
}

/// Renumbers the drafts and records old -> new when anything moved (F285).
fn renumbered(team: &mut Team, by: &str, events: &mut Vec<Event>) -> Value {
    let changes = renumber_drafts(team);
    let empty = |key: &str| changes[key].as_array().is_none_or(|list| list.is_empty());
    if !(empty("phases") && empty("tasks")) {
        events.push(Event::new(
            "phase_renumbered",
            json!({"by":by,"phases":changes["phases"],"tasks":changes["tasks"]}),
        ));
    }
    changes
}

/// How many edits a phase keeps for the owner's diff view (F10).
const PHASE_CHANGES: usize = 50;

/// Records one edit of a draft phase, keeping the newest changes only (F10).
fn record_change(phase: &mut Phase, change: Value) {
    phase.changes.push(change);
    if phase.changes.len() > PHASE_CHANGES {
        let drop = phase.changes.len() - PHASE_CHANGES;
        phase.changes.drain(..drop);
    }
}

/// A short form of a task field for the change list.
fn brief(value: &str) -> String {
    let short: String = value.chars().take(200).collect();
    if value.chars().count() > 200 {
        format!("{short}...")
    } else {
        short
    }
}

/// The draft phase numbered `n` and its index.
fn draft_phase(team: &Team, n: u32) -> anyhow::Result<usize> {
    let at = team.phases.iter().position(|phase| phase.n == n).with_context(|| format!("Unknown phase {n}"))?;
    // U25: the lead reworks a phase the owner sent back with the same edits.
    ensure!(
        team.phases[at].state.is_draft(),
        "Phase {n} is approved or discarded; only a draft phase can change"
    );
    Ok(at)
}

/// U58: the phase numbered `n` and its index while it can still change: a
/// draft, or a queued/approved phase whose work has not started (the scope the
/// owner approved is the work, not its numbering).
fn editable_phase(team: &Team, n: u32) -> anyhow::Result<usize> {
    let at = team.phases.iter().position(|phase| phase.n == n).with_context(|| format!("Unknown phase {n}"))?;
    let phase = &team.phases[at];
    ensure!(phase.state != PhaseState::Discarded, "Phase {n} is discarded");
    if !phase.state.is_draft() {
        ensure!(
            phase_untouched(team, phase),
            "Phase {n} has started; only a phase with no delivered attempt can change"
        );
    }
    Ok(at)
}

/// F10: edits a draft phase until the owner approves it - its title, its
/// barrier, fields of its draft tasks by id, and task removals. The event
/// carries the diff the owner sees.
fn edit_draft(team: &mut Team, input: &Value, actor: &Actor, offline: &BTreeSet<String>, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    ensure!(matches!(actor, Actor::Human) || actor.is_lead(team), "Only the lead or you edit a draft");
    ensure!(
        matches!(team.status, TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused),
        "Only a planning, running or paused team edits a draft"
    );
    ensure_draft_revision(team, input)?;
    let n = input["phase"]
        .as_u64()
        .and_then(|phase| u32::try_from(phase).ok())
        .filter(|phase| *phase > 0)
        .context("A phase number is required")?;
    let mut proposed = team.clone();
    let at = editable_phase(&proposed, n)?;
    let by = actor.label(team);
    let mut diff = json!({"updated":[],"removed":[]});
    let mut changes: Vec<Value> = Vec::new();
    if let Some(value) = input.get("title") {
        let title = clean(value.as_str().unwrap_or_default().trim(), 120, "A phase title")?;
        if proposed.phases[at].title != title {
            diff["title"] = json!({"from":proposed.phases[at].title,"to":title});
            proposed.phases[at].title = title;
        }
    }
    if let Some(value) = input.get("barrier") {
        let barrier = value.as_bool().context("barrier is true or false")?;
        if proposed.phases[at].barrier != barrier {
            diff["barrier"] = json!({"from":proposed.phases[at].barrier,"to":barrier});
            proposed.phases[at].barrier = barrier;
        }
    }
    for value in input["tasks_update"].as_array().into_iter().flatten() {
        let id = value["id"].as_u64().and_then(|id| u32::try_from(id).ok()).filter(|id| *id > 0).context("A task id is required")?;
        ensure!(proposed.phases[at].tasks.contains(&id), "Task #{id} is not in phase {n}");
        let assignee = match value.get("assignee") {
            None => None,
            Some(value) => Some(member_id(value.as_str().unwrap_or_default())?),
        };
        if let Some(assignee) = &assignee {
            ensure!(proposed.member(assignee).is_some_and(|member| member.state != MemberState::Removed), "Unknown member {assignee}");
        }
        let task = proposed.task_mut(id).context("Unknown task")?;
        // U58: a draft, or a task waiting in a queued/approved phase that has
        // not started, is edited like a draft.
        ensure!(
            matches!(task.status, TaskStatus::Draft | TaskStatus::Pending) && !task_started(task),
            "Task #{id} is not a draft or waiting in an unstarted phase"
        );
        let mut fields: Vec<&str> = Vec::new();
        if let Some(value) = value.get("title") {
            let old = task.title.clone();
            task.title = clean_text(value.as_str().unwrap_or_default().trim(), 200, "A task title")?;
            ensure!(!task.title.is_empty(), "Task #{id} needs a title");
            if old != task.title {
                changes.push(json!({"kind":"changed","task":id,"title":task.title,"field":"title","from":old,"to":task.title}));
            }
            fields.push("title");
        }
        if let Some(value) = value.get("description") {
            let old = task.description.clone();
            task.description = clean_text(value.as_str().unwrap_or_default(), 20_000, "A task description")?;
            if old != task.description {
                changes.push(json!({"kind":"changed","task":id,"title":task.title,"field":"description","from":brief(&old),"to":brief(&task.description)}));
            }
            fields.push("description");
        }
        if let Some(value) = value.get("criteria") {
            let list = value.as_array().context("criteria is a list")?;
            let old = task.criteria.join("; ");
            task.criteria = list
                .iter()
                .map(|item| clean_text(item.as_str().unwrap_or_default(), 500, "An acceptance criterion"))
                .collect::<anyhow::Result<Vec<_>>>()?;
            if old != task.criteria.join("; ") {
                changes.push(json!({"kind":"changed","task":id,"title":task.title,"field":"criteria","from":brief(&old),"to":brief(&task.criteria.join("; "))}));
            }
            fields.push("criteria");
        }
        if let Some(value) = value.get("deps") {
            let old: Vec<String> = task.deps.iter().map(|dep| format!("#{}", dep.task)).collect();
            task.deps = serde_json::from_value(value.clone()).context("Invalid dependencies")?;
            let mut seen = BTreeSet::new();
            task.deps.retain(|dep| seen.insert(dep.task));
            ensure!(task.deps.iter().all(|dep| dep.task != id), "Task #{id} depends on itself");
            let new: Vec<String> = task.deps.iter().map(|dep| format!("#{}", dep.task)).collect();
            if old != new {
                changes.push(json!({"kind":"changed","task":id,"title":task.title,"field":"deps","from":old.join(", "),"to":new.join(", ")}));
            }
            fields.push("deps");
        }
        if let Some(assignee) = assignee {
            if task.assignee != assignee {
                changes.push(json!({"kind":"changed","task":id,"title":task.title,"field":"assignee","from":task.assignee,"to":assignee}));
            }
            task.assignee = assignee;
            fields.push("assignee");
        }
        if let Some(value) = value.get("pinned") {
            let pinned = value.as_bool().context("pinned is true or false")?;
            if task.pinned != pinned {
                changes.push(json!({"kind":"changed","task":id,"title":task.title,"field":"pinned","from":if task.pinned {"on"} else {"off"},"to":if pinned {"on"} else {"off"}}));
            }
            task.pinned = pinned;
            fields.push("pinned");
        }
        // Owner 2026-10-31: the lead's estimate in minutes; null clears it and
        // Zerus computes it from the history again.
        if let Some(value) = value.get("estimate_minutes") {
            let old = task.estimate_minutes.map(|minutes| format!("{} min", minutes.round() as u64)).unwrap_or_else(|| "no estimate".into());
            task.estimate_minutes = match value {
                Value::Null => None,
                _ => {
                    let minutes = value.as_f64().context("estimate_minutes is a number of minutes or null")?;
                    ensure!((1.0..=1440.0).contains(&minutes), "Task #{id}: an estimate is 1 to 1440 minutes");
                    Some(minutes.round())
                }
            };
            task.estimate_source = if task.estimate_minutes.is_some() { "lead".into() } else { "none".into() };
            let new = task.estimate_minutes.map(|minutes| format!("{} min", minutes.round() as u64)).unwrap_or_else(|| "no estimate".into());
            if old != new {
                changes.push(json!({"kind":"changed","task":id,"title":task.title,"field":"estimate_minutes","from":old,"to":new}));
            }
            fields.push("estimate_minutes");
        }
        // U36: the pipeline the task takes when it is assigned.
        if let Some(value) = value.get("pipeline") {
            let pipeline = value.as_str().context("pipeline is a preset id, none or empty")?.trim().to_owned();
            check_pipeline(&pipeline)?;
            if task.pipeline != pipeline {
                changes.push(json!({"kind":"changed","task":id,"title":task.title,"field":"pipeline","from":task.pipeline,"to":pipeline}));
            }
            task.pipeline = pipeline;
            fields.push("pipeline");
        }
        // U45: the models allowed for the task, and a role instead of an
        // assignee (a role clears the assignee; an assignee clears the role).
        if value.get("models").is_some() || value.get("role").is_some() {
            let old = (task.models.join(", "), task.role.clone());
            if let Some(models) = value.get("models") {
                task.models = serde_json::from_value(models.clone()).context("models is a list of model ids")?;
                fields.push("models");
            }
            if let Some(role) = value.get("role") {
                task.role = clean_text(role.as_str().context("role is a role name or empty")?, 60, "A task role")?;
                if !task.role.trim().is_empty() {
                    task.assignee.clear();
                    task.pinned = false;
                }
                fields.push("role");
            } else if fields.contains(&"assignee") {
                task.role.clear();
            }
            super::models::check_task(task)?;
            if old.0 != task.models.join(", ") {
                changes.push(json!({"kind":"changed","task":id,"title":task.title,"field":"models","from":old.0,"to":task.models.join(", ")}));
            }
            if old.1 != task.role {
                changes.push(json!({"kind":"changed","task":id,"title":task.title,"field":"role","from":old.1,"to":task.role}));
            }
        } else if fields.contains(&"assignee") && !task.role.is_empty() {
            task.role.clear();
        }
        // U27: the lead or the owner moves a task to a lane; null takes it out.
        if let Some(value) = value.get("lane") {
            let lane = if value.is_null() {
                None
            } else {
                let mut lane: super::model::Lane = serde_json::from_value(value.clone()).context("Invalid lane")?;
                clean_lane(&mut lane, id)?;
                Some(lane)
            };
            if task.lane != lane {
                let name = |lane: &Option<super::model::Lane>| lane.as_ref().map(|lane| lane.label.clone()).unwrap_or_default();
                changes.push(json!({"kind":"changed","task":id,"title":task.title,"field":"lane",
                    "from":name(&task.lane),"to":name(&lane)}));
            }
            task.lane = lane;
            fields.push("lane");
        }
        if !fields.is_empty() {
            diff["updated"].as_array_mut().unwrap().push(json!({"id":id,"fields":fields}));
        }
    }
    let mut removed: Vec<u32> = Vec::new();
    for value in input["tasks_remove"].as_array().into_iter().flatten() {
        let id = value.as_u64().and_then(|id| u32::try_from(id).ok()).filter(|id| *id > 0).context("A task id is required")?;
        ensure!(proposed.phases[at].tasks.contains(&id), "Task #{id} is not in phase {n}");
        ensure!(
            proposed.task(id).is_some_and(|task| matches!(task.status, TaskStatus::Draft | TaskStatus::Pending) && !task_started(task)),
            "Task #{id} is not a draft or waiting in an unstarted phase"
        );
        ensure!(!removed.contains(&id), "Task #{id} is removed twice");
        removed.push(id);
    }
    for id in &removed {
        if let Some(dependent) = proposed.tasks.iter().find(|task| task.deps.iter().any(|dep| dep.task == *id)) {
            bail!("#{} depends on #{id}; change its deps first", dependent.id);
        }
    }
    let removed_changes: Vec<Value> = removed
        .iter()
        .filter_map(|id| proposed.task(*id).map(|task| json!({"kind":"removed","task":id,"title":task.title})))
        .collect();
    proposed.tasks.retain(|task| !removed.contains(&task.id));
    proposed.phases[at].tasks.retain(|id| !removed.contains(id));
    for change in changes.into_iter().chain(removed_changes) {
        record_change(&mut proposed.phases[at], change);
    }
    diff["removed"] = json!(removed);
    let validation = plan::validate(&proposed, offline);
    ensure!(validation.errors.is_empty(), "Fix {} plan error(s) to edit: {}", validation.errors.len(), errors_text(&validation.errors));
    *team = proposed;
    events.push(Event::new("phase_edited", json!({"by":by,"phase":n,"diff":diff})));
    Ok(json!({"status":"draft_edited","phase":n,"diff":diff,"validation":validation}))
}

/// F10/F285: moves a draft next to another draft - before or after it - and
/// renumbers the drafts (their phase numbers and their tasks' ids stay
/// contiguous after the approved work).
fn move_phase(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    ensure!(matches!(actor, Actor::Human) || actor.is_lead(team), "Only the lead or you move a phase");
    ensure!(
        matches!(team.status, TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused),
        "Only a planning, running or paused team moves phases"
    );
    ensure_draft_revision(team, input)?;
    let n = input["phase"]
        .as_u64()
        .and_then(|phase| u32::try_from(phase).ok())
        .filter(|phase| *phase > 0)
        .context("A phase number is required")?;
    // The desktop sends {to, position}; a plain "before" still works (F10).
    let to = match input.get("to").or_else(|| input.get("before")) {
        Some(value) => value
            .as_u64()
            .and_then(|to| u32::try_from(to).ok())
            .filter(|to| *to > 0)
            .context("A neighbouring phase is required")?,
        None => bail!("A neighbouring phase (\"to\") is required"),
    };
    let position = text(input, "position");
    ensure!(position.is_empty() || position == "before" || position == "after", "position is before or after");
    ensure!(n != to, "Phase {n} is already next to itself");
    let mut proposed = team.clone();
    let at = draft_phase(&proposed, n)?;
    draft_phase(&proposed, to)?;
    let before: Vec<u32> = proposed.phases.iter().map(|phase| phase.n).collect();
    let old_position = before.iter().position(|phase| *phase == n).map(|index| index as u32 + 1).unwrap_or(0);
    let phase = proposed.phases.remove(at);
    let index = proposed.phases.iter().position(|phase| phase.n == to).context("Unknown phase")?;
    let placement = if position == "after" { index + 1 } else { index };
    proposed.phases.insert(placement, phase);
    let moved_order: Vec<u32> = proposed.phases.iter().map(|phase| phase.n).collect();
    // The feed says when the other drafts were renumbered too (F10).
    let reordered = moved_order != before;
    let by = actor.label(team);
    record_change(
        &mut proposed.phases[placement],
        json!({"kind":"moved","from":old_position,"to":placement as u32 + 1}),
    );
    // F285: the drafts close the gaps after the move, and the old -> new
    // mapping is recorded.
    renumbered(&mut proposed, &by, events);
    let to_position = placement as u32 + 1;
    let order: Vec<u32> = proposed.phases.iter().map(|phase| phase.n).collect();
    *team = proposed;
    events.push(Event::new("phase_moved", json!({"by":by,"phase":n,"to":to_position,"renumbered":reordered,"order":order})));
    Ok(json!({"status":"phase_moved","phase":n,"to":to_position,"renumbered":reordered,"order":order}))
}

/// Closes the open questions of a task: its attempt ended or the task was
/// decided, so they are never answered and never sent (N5).
fn close_questions(task: &mut Task) {
    let at = now();
    for attempt in &mut task.attempts {
        for question in &mut attempt.questions {
            if question.open() {
                question.answered_at = Some(at);
                question.by = "Zerus".into();
            }
        }
    }
}

/// The task's open attempt must be revoked before another starts. The
/// dispatcher interrupts the old worker separately.
fn revoke(task: &mut Task, why: &str) -> Option<u32> {
    // Spec 0 (U45, U46, U48): a retry, a reassignment, a take-over or a
    // decision ends a wait for Zerus; the next hand-out prepares again.
    task.waits = None;
    // The attempt is over: its questions close with it (N5).
    close_questions(task);
    // U36: so does the stage that was reading its report.
    super::pipeline::revoke_runs(task, why);
    let attempt = task.open_attempt_mut()?;
    attempt.state = AttemptState::Revoked;
    attempt.error = why.to_owned();
    Some(attempt.n)
}

/// Every task is finished and there is at least one: the team waits for the
/// lead to extend it or finish it.
pub(crate) fn all_decided(team: &Team) -> bool {
    // U59: an empty queue in free mode is idle, not done: the team waits for
    // tasks to be added to its open phase.
    if team.plan_mode == PlanMode::Free {
        return false;
    }
    !team.tasks.is_empty() && team.tasks.iter().all(|task| task.status.finished())
}

/// Marks every draft phase record discarded: a team that is over keeps no
/// "pending approval" record.
fn discard_draft_records(team: &mut Team, by: &str) {
    let at = now();
    for record in &mut team.phases {
        if record.state.is_draft() {
            record.state = PhaseState::Discarded;
            record.decided_by = by.to_owned();
            record.decided_at = Some(at);
        }
    }
}

/// A draft phase with no Draft task left is over: it never blocks an extend
/// or stays "pending approval".
fn discard_empty_draft(team: &mut Team, phase: u32, by: &str) {
    if team.tasks.iter().any(|task| task.phase == phase && task.status == TaskStatus::Draft) {
        return;
    }
    let at = now();
    if let Some(record) = team.phases.iter_mut().find(|record| record.n == phase && record.state.is_draft()) {
        record.state = PhaseState::Discarded;
        record.decided_by = by.to_owned();
        record.decided_at = Some(at);
    }
}

/// Ends the team with the lead's or your summary; Done is final.
fn finish(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    ensure!(matches!(actor, Actor::Human) || actor.is_lead(team), "Only the lead or you finish the team");
    ensure!(matches!(team.status, TeamStatus::Running | TeamStatus::Paused), "The team is not running");
    let summary = clean(text(input, "summary").trim(), 4000, "A summary")?;
    ensure!(!summary.is_empty(), "A summary is required");
    let open = team.tasks.iter().filter(|task| !task.status.finished()).count();
    ensure!(open == 0, "{open} task(s) are not decided: decide or cancel them first");
    // D2: a critical finding that is still open blocks a finish; the owner may
    // force it.
    let critical = team.findings.iter().any(|finding| finding.severity == "critical" && finding.unresolved());
    // C16: only the owner forces a finish over an open critical finding.
    let force = input["force"].as_bool() == Some(true) && matches!(actor, Actor::Human);
    ensure!(
        force || !critical,
        "A critical finding is open: fix it, or the owner finishes with \"force\": true"
    );
    let by = actor.label(team);
    let accepted = team.tasks.iter().filter(|task| task.status == TaskStatus::Accepted).count();
    let summary_chars = summary.chars().count();
    discard_draft_records(team, &by);
    team.finish = Some(Finish { by: by.clone(), summary, at: now() });
    team.idle = Notice::None;
    team.status = TeamStatus::Done;
    events.push(Event::new(
        "finished",
        json!({"by":by,"accepted":accepted,"tasks":team.tasks.len(),"summary_chars":summary_chars}),
    ));
    // D2: the result lists every open and deferred finding, at most 100 with a
    // count of the rest, so the owner sees what remains.
    let open_findings: Vec<&Finding> = team
        .findings
        .iter()
        .filter(|finding| finding.unresolved() || finding.state == "deferred")
        .collect();
    let listed: Vec<Value> = open_findings
        .iter()
        .take(100)
        .map(|finding| json!({"id":finding.id,"severity":finding.severity,"title":finding.title,"state":finding.state}))
        .collect();
    let more = open_findings.len().saturating_sub(listed.len());
    Ok(json!({"status":"finished","findings":listed,"findings_more":more}))
}

/// 9.5: files a finished sub-team's summary (plus its id, 4000 characters) as
/// the sub-lead's report on the parent task.
pub(crate) fn file_subteam_report(child: &Team, parent: &mut Team) -> anyhow::Result<u32> {
    let link = child.parent.as_ref().context("The team is not a sub-team")?;
    ensure!(child.status == TeamStatus::Done, "The sub-team is not finished");
    let summary = child.finish.as_ref().map(|finish| finish.summary.clone()).unwrap_or_default();
    let task = parent.tasks.iter_mut().find(|task| task.id == link.task).context("The parent task is missing")?;
    ensure!(!task.status.finished(), "The parent task is already decided");
    let member = if link.member.is_empty() { task.assignee.clone() } else { link.member.clone() };
    let text: String = format!("[Sub-team {} {}] {}", child.id, child.name, summary).chars().take(4000).collect();
    // N23: a replayed finish (or a second call after the report landed) files
    // no second report. C5: only a report still waiting for the parent lead
    // counts - a rejected one is a decision the sub-team reworked, and its
    // new finish must reach the lead.
    if task.status == TaskStatus::Submitted
        && task.attempts.iter().any(|attempt| {
            attempt.state == AttemptState::Reported
                && attempt.report.as_ref().is_some_and(|report| report.text.starts_with(&format!("[Sub-team {} ", child.id)))
        })
    {
        return Ok(task.id);
    }
    let n = task.attempts.len() as u32 + 1;
    task.attempts.push(Attempt {
        n,
        id: uuid::Uuid::new_v4().to_string(),
        member,
        sent_at: now(),
        delivered_at: Some(now()),
        // N22: a filed sub-team report is a report waiting for the parent lead,
        // not an open delivery.
        state: AttemptState::Reported,
        report: Some(Report { text, at: now(), ..Default::default() }),
        ..Default::default()
    });
    task.status = TaskStatus::Submitted;
    Ok(task.id)
}

/// 9.5: a rejected sub-team report reopens it: Running with a "Rework" draft
/// that carries the reason for its lead.
pub(crate) fn reopen_subteam(child: &mut Team, reason: &str) -> anyhow::Result<u32> {
    ensure!(child.parent.is_some(), "The team is not a sub-team");
    // N24: only a finished sub-team reopens; a stopped one stays stopped.
    ensure!(child.status == TeamStatus::Done, "The sub-team is not finished");
    let n = child
        .phases
        .iter()
        .map(|phase| phase.n)
        .chain(child.tasks.iter().map(|task| task.phase))
        .max()
        .unwrap_or(1)
        + 1;
    child.status = TeamStatus::Running;
    child.finish = None;
    child.idle = Notice::None;
    child.phases.push(Phase {
        n,
        title: "Rework".into(),
        state: PhaseState::Draft,
        reason: reason.chars().take(500).collect(),
        added_by: "Lead".into(),
        added_at: now(),
        ..Default::default()
    });
    Ok(n)
}

/// 9.6: the parent question id a sub-team question becomes.
pub(crate) fn forward_question(child: &Team, question: &Question) -> String {
    format!("sub:{}:{}", child.id, question.id)
}

/// 9.6: copies the parent lead's answer back into the sub-team question.
/// N24: returns the answered question's id, so the caller can carry the answer
/// on down when that question itself came from a deeper sub-team.
pub(crate) fn answer_forwarded(child: &mut Team, parent_question: &str, answer: &str) -> Option<String> {
    for task in &mut child.tasks {
        for attempt in &mut task.attempts {
            for question in &mut attempt.questions {
                if question.open() && question.forwarded == parent_question {
                    question.answer = answer.to_owned();
                    question.answered_at = Some(now());
                    question.by = "Lead".into();
                    // N21: the dispatcher delivers a Pending answer to the member.
                    question.notice = Notice::Pending;
                    return Some(question.id.clone());
                }
            }
        }
    }
    None
}

/// 9.7: cancelling or reassigning a task with a sub-team asks first; "stop"
/// stops the sub-team, "keep" leaves it running.
pub(crate) fn subteam_stop_choice(input: &Value, task: &Task) -> anyhow::Result<bool> {
    if task.subteam.is_none() {
        return Ok(false);
    }
    match text(input, "subteam") {
        "stop" => Ok(true),
        "keep" => Ok(false),
        other => bail!(
            "Task #{} has a sub-team; pass \"subteam\": \"stop\" or \"keep\" (got \"{}\")",
            task.id,
            other
        ),
    }
}

/// The phase of a task's last attempt as `status`, `current` and the desktop
/// show it (spec section 2): `sent`, `delivered`, `late`, `working`, `asking`,
/// `reported`, `no_report`, `stopped`, `held`, `rejected` or `unconfirmed`.
/// The plan's "input accepted" is `delivered` here; a decided task has no
/// attempt phase. `session_stopped` is known only from the live session
/// listing, so the caller passes it and the stored status passes false.
pub(crate) fn attempt_phase(task: &Task, session_stopped: bool) -> Option<&'static str> {
    if task.status.finished() {
        return None;
    }
    if task.hold.is_some() {
        return Some("held");
    }
    let attempt = task.attempts.last()?;
    let phase = match attempt.state {
        AttemptState::Unconfirmed => "unconfirmed",
        AttemptState::Delivering => "sent",
        AttemptState::Rejected => "rejected",
        // Nothing reached the agent, or another attempt replaced this one:
        // the task itself is Pending, so it has no phase of its own.
        AttemptState::Refused | AttemptState::Revoked => return None,
        AttemptState::Delivered | AttemptState::Reported => {
            if attempt.open_question().is_some() {
                // A question is not a report, and the attempt stays open (A3).
                "asking"
            } else if attempt.state == AttemptState::Reported {
                "reported"
            } else if session_stopped {
                "stopped"
            } else if attempt.no_report {
                "no_report"
            } else if attempt.started_at.is_some() {
                "working"
            } else if attempt.start_warned {
                // Delivered and not started: the lead was told once (A1).
                "late"
            } else {
                "delivered"
            }
        }
    };
    Some(phase)
}

/// D1: validates a structured report against the task's criteria and bounds.
fn structured_report(team: &Team, id: u32, report: &Value) -> anyhow::Result<Value> {
    ensure!(serde_json::to_string(report)?.len() <= 64 * 1024, "A structured report stays within 64 KB");
    let criteria = report["criteria"].as_array().cloned().unwrap_or_default();
    ensure!(criteria.len() <= 50, "A report lists at most 50 criteria");
    let count = team.task(id).map(|task| task.criteria.len()).unwrap_or(0);
    let evidence = report["evidence"].as_array().cloned().unwrap_or_default();
    ensure!(evidence.len() <= 50, "A report lists at most 50 evidence items");
    for (index, item) in evidence.iter().enumerate() {
        let kind = item["kind"].as_str().context("An evidence item names its kind")?;
        ensure!(["command", "image", "html", "file", "log", "url", "text"].contains(&kind), "Unknown evidence kind {kind}");
        clean(item["caption"].as_str().unwrap_or_default(), 500, "An evidence caption")?;
        match kind {
            "command" => {
                ensure!(!item["command"].as_str().unwrap_or_default().trim().is_empty(), "Command evidence {} needs its command", index);
                ensure!(item["exit"].as_i64().is_some(), "Command evidence {} needs its exit code", index);
                clean(item["tail"].as_str().unwrap_or_default(), 4000, "An evidence tail")?;
            }
            "url" => ensure!(
                item["url"].as_str().is_some_and(|url| url.starts_with("http://") || url.starts_with("https://")),
                "Evidence {} needs an http(s) url",
                index
            ),
            "text" => ensure!(!item["text"].as_str().unwrap_or_default().trim().is_empty(), "Text evidence {} needs its text", index),
            _ => ensure!(!item["path"].as_str().unwrap_or_default().trim().is_empty(), "Evidence {} needs its path", index),
        }
    }
    for criterion in &criteria {
        let n = criterion["n"].as_u64().context("A criterion names its 1-based number")?;
        ensure!(n >= 1 && n as usize <= count, "Task #{id} has no criterion {n}");
        let result = criterion["result"].as_str().context("A criterion names its result")?;
        ensure!(["pass", "fail", "partial", "n/a"].contains(&result), "A criterion result is pass, fail, partial or n/a");
        for index in criterion["evidence"].as_array().into_iter().flatten() {
            ensure!(
                index.as_u64().is_some_and(|index| (index as usize) < evidence.len()),
                "A criterion cites evidence {index} that the report does not list"
            );
        }
    }
    // U36: the files the attempt changed, which a stage reads in a folder
    // without Git.
    let files = report["files"].as_array().cloned().unwrap_or_default();
    ensure!(report.get("files").is_none_or(Value::is_array) && files.len() <= 500, "A report lists at most 500 changed files");
    for file in &files {
        ensure!(file.as_str().is_some_and(|file| !file.trim().is_empty() && file.chars().count() <= 500), "A changed file is a path of at most 500 characters");
    }
    let limits = report["limits"].as_array().cloned().unwrap_or_default();
    ensure!(limits.len() <= 50, "A report lists at most 50 limits");
    for limit in &limits {
        clean(limit.as_str().unwrap_or_default(), 1000, "A limit")?;
    }
    let findings = report["findings"].as_array();
    ensure!(!findings.is_some_and(|findings| findings.len() > 100), "A report lists at most 100 findings");
    // N13: a finding is checked when the report arrives, so a bad one is
    // refused to the member, never at accept, where it would block the task.
    for finding in findings.into_iter().flatten() {
        checked_finding(team, finding)?;
    }
    Ok(report.clone())
}

/// One finding of a report: its severity, title, scenario, file and task.
struct CheckedFinding {
    severity: String,
    title: String,
    scenario: String,
    file: String,
    task: Option<u32>,
}

fn checked_finding(team: &Team, finding: &Value) -> anyhow::Result<CheckedFinding> {
    ensure!(finding.is_object(), "A finding is an object");
    let severity = finding["severity"].as_str().unwrap_or("normal").to_owned();
    ensure!(["critical", "normal", "later"].contains(&severity.as_str()), "A finding is critical, normal or later");
    let title = clean(finding["title"].as_str().unwrap_or_default().trim(), 200, "A finding title")?;
    ensure!(!title.is_empty(), "A finding needs a title");
    let scenario = clean(finding["scenario"].as_str().unwrap_or_default().trim(), 2000, "A finding scenario")?;
    ensure!(!scenario.is_empty(), "A finding needs its scenario");
    let file = clean(finding["file"].as_str().unwrap_or_default().trim(), 500, "A finding file")?;
    let task = finding["task"].as_u64().map(|n| n as u32);
    if let Some(task) = task {
        ensure!(team.task(task).is_some(), "A finding names unknown task #{task}");
    }
    Ok(CheckedFinding { severity, title, scenario, file, task })
}

fn report(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    report_with(team, input, actor, false, events)
}

/// `captured`: Zerus records the member's last reply (A2); it never enters the
/// pipeline's stages (#538 C1).
fn report_with(team: &mut Team, input: &Value, actor: &Actor, captured: bool, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    let id = task_id(input)?;
    // U36: a stage member's verdict names its stage run as the attempt.
    if let Some(result) = super::pipeline::stage_report(team, input, actor, events)? {
        return Ok(result);
    }
    let attempt_id = text(input, "attempt");
    let mut body = clean(text(input, "text").trim(), 64 * 1024, "A report")?;
    // C5: a check task reports its exit code and output tail; its text is
    // optional (Zerus writes the summary) and its decision is the exit code.
    let check = input.get("check").filter(|value| value.is_object());
    // D1: an optional structured report; text falls back to its summary.
    let structured = input.get("report").filter(|value| value.is_object());
    let structured = structured.map(|report| structured_report(team, id, report)).transpose()?;
    if let Some(report) = &structured {
        if body.is_empty() {
            body = clean(report["summary"].as_str().unwrap_or_default().trim(), 64 * 1024, "A report")?;
        }
    }
    ensure!(!body.is_empty() || check.is_some(), "A report needs text");
    let check_task = team.task(id).is_some_and(Task::is_check);
    ensure!(!check_task || check.is_some(), "A check task's report needs \"check\":{{\"exit\":N,\"tail\":\"...\"}}");
    ensure!(!check.is_some() || check_task, "Task #{id} is not a check: drop \"check\" from its report");
    let by_member = match actor {
        Actor::Member(member) => member.clone(),
        Actor::Human => String::new(),
        Actor::ParentLead => bail!("The parent lead does not report on a sub-team's task"),
        Actor::Intake(_) => bail!("An intake key adds urgent tasks; it cannot report"),
    };
    let label = actor.label(team);
    let team_lead = team.lead.clone();
    // #684 N1: the attempt records its folder's HEAD and branch at the report,
    // so a later stage checks that commit and never reads HEAD at its start.
    let (folder_head, folder_branch) = if by_member.is_empty() {
        (String::new(), String::new())
    } else {
        let folder = team.member(&by_member).and_then(|member| super::dispatcher::member_folder_path(team, member).ok()).unwrap_or_default();
        if folder.is_empty() || !std::path::Path::new(&folder).is_dir() || !super::stages::is_git(&folder) {
            (String::new(), String::new())
        } else {
            let head = super::dispatcher::git_at(&folder, &["rev-parse", "HEAD"]).unwrap_or_default();
            let branch = super::dispatcher::task_branch(team, id, &by_member)
                .or_else(|| super::dispatcher::git_at(&folder, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|branch| branch != "HEAD"))
                .unwrap_or_default();
            (head, branch)
        }
    };
    // #718: the test lines and functions the change adds and removes, against
    // the lead's commit; the project's test globs say what a test file is.
    let test_delta = if folder_head.is_empty() || team.folder.is_empty() {
        None
    } else {
        let folder = team.member(&by_member).and_then(|member| super::dispatcher::member_folder_path(team, member).ok()).unwrap_or_default();
        let base = super::dispatcher::git_at(&team.folder, &["rev-parse", "HEAD"]).unwrap_or_default();
        match crate::test_discipline::load(std::path::Path::new(&team.folder), None) {
            Ok(loaded) if !base.is_empty() && loaded.active() => super::test_results::report_delta(&folder, &base, &loaded.config),
            _ => None,
        }
    };
    // C1: the lead reports its own task and accepts it in one step; nobody
    // else accepts on a report.
    let accept = input["accept"].as_bool() == Some(true);
    if accept {
        ensure!(!by_member.is_empty() && by_member == team_lead, "Only the lead accepts its own task on its report");
    }
    let task = team.task_mut(id).context("Unknown task")?;
    ensure!(task.attempts.iter().any(|a| a.id == attempt_id), "Task #{id} has no attempt {attempt_id}");
    let current = task.open_attempt().map(|a| (a.id.clone(), a.member.clone(), a.n));
    let open = current.filter(|(open, member, _)| open == attempt_id && (by_member.is_empty() || *member == by_member));
    if let Some((open_id, member, n)) = open {
        if accept {
            ensure!(member == team_lead, "Task #{id} is not the lead's own task: accept it with hgs team accept");
        }
        let expected = task.check.clone();
        let check_result = match (expected.as_ref(), check) {
            (Some(expected), Some(result)) => {
                let exit = result["exit"]
                    .as_i64()
                    .and_then(|exit| i32::try_from(exit).ok())
                    .context("A check result names its exit code")?;
                let tail = clean(result["tail"].as_str().unwrap_or_default().trim(), 20_000, "A check output tail")?;
                if body.is_empty() {
                    body = format!("Check `{}`: exit {exit} (expected {})", expected.command, expected.expect_exit);
                }
                Some(CheckResult { exit, tail, at: now() })
            }
            _ => None,
        };
        let attempt = task.attempts.iter_mut().find(|a| a.id == open_id).unwrap();
        attempt.state = AttemptState::Reported;
        attempt.report = Some(Report { text: body, at: now(), structured: structured.clone(), unstructured: structured.is_none(), captured, tests: test_delta.clone(), ..Default::default() });
        attempt.folder_head = folder_head.clone();
        attempt.folder_branch = folder_branch.clone();
        attempt.no_report = false;
        attempt.check = check_result;
        // The lead hears of a report unless it wrote it.
        attempt.notice = if member == team_lead { Notice::None } else { Notice::Pending };
        task.status = TaskStatus::Submitted;
        task.decided_by = None;
        let title = task.title.clone();
        let role = team.role_of(&member);
        // Spec 5.6: "Report to QA lead" when a sub-lead reviews it.
        let reviewer = team.reviewer_of(&member);
        let to = if reviewer.is_empty() { String::new() } else { team.role_of(&reviewer) };
        events.push(Event::new("reported", json!({"by":role,"to":to,"task":id,"title":title,"attempt":n,"human":by_member.is_empty()})));
        if accept {
            let decided = decide(team, input, actor, true, events)?;
            let status = decided["status"].clone();
            return Ok(json!({"status":status,"task":id,"attempt":n}));
        }
        if check.is_some() {
            // C5: the check decides, without the lead.
            return decide_check(team, id, n, events);
        }
        // U36: a task with a pipeline goes to its first stage. #538 C1: a
        // captured reply stays Submitted for the member's real report (A6)
        // or the lead's decision; the real report enters the stages.
        if captured {
            return Ok(json!({"status":"submitted","task":id,"attempt":n}));
        }
        if let Some(result) = super::pipeline::on_report(team, id, events)? {
            return Ok(result);
        }
        return Ok(json!({"status":"submitted","task":id,"attempt":n}));
    }
    // A6/U42: while the task is undecided the member may send its report again
    // for the same attempt. A captured reply was always replaceable; U42 also
    // repairs one that reached the store as a placeholder (a short "poll"
    // closed #512's attempt and the whole text was refused afterwards). Only
    // the newest reported attempt: older reports are history.
    let newest = task
        .attempts
        .iter()
        .rev()
        .find(|attempt| attempt.state == AttemptState::Reported)
        .map(|attempt| attempt.id.clone());
    let Some(attempt) = task.attempts.iter_mut().find(|a| a.id == attempt_id) else {
        unreachable!("the attempt was checked above")
    };
    let replaceable = task.status.reported()
        && attempt.state == AttemptState::Reported
        && newest.as_deref() == Some(attempt.id.as_str())
        && (by_member.is_empty() || attempt.member == by_member);
    if replaceable {
        let n = attempt.n;
        let member = attempt.member.clone();
        let Some(report) = attempt.report.as_mut() else { bail!("This attempt has no report to replace") };
        let replaced = std::mem::take(&mut report.text);
        report.replaced.push(ReplacedReport { text: replaced, at: report.at });
        // At most the newest three kept; a longer record would grow without bound.
        if report.replaced.len() > 3 {
            report.replaced.remove(0);
        }
        report.text = body;
        report.structured = structured.clone();
        report.unstructured = structured.is_none();
        report.at = now();
        // #635 N5: a replacement (a captured reply's real report) records the
        // folder's HEAD and branch again, so the stage checks the commit the
        // member reports, not the capture's.
        if !by_member.is_empty() {
            attempt.folder_head = folder_head.clone();
            attempt.folder_branch = folder_branch.clone();
        }
        let was_captured = std::mem::take(&mut report.captured);
        attempt.no_report = false;
        // The lead hears the real report as a report, not as the captured one;
        // U36: a stage still reading it tells the lead after the last stage.
        if member != team_lead && task.status == TaskStatus::Submitted {
            attempt.notice = Notice::Pending;
        }
        let (in_stage, submitted) = (task.status.in_stage(), task.status == TaskStatus::Submitted);
        let title = task.title.clone();
        let role = team.role_of(&member);
        events.push(Event::new(
            "report_replaced",
            json!({"by":role,"task":id,"title":title,"attempt":n,"human":by_member.is_empty()}),
        ));
        // #538 C1: the real report that replaces a captured one enters the
        // stages; #595 N4: one replaced while a stage reads it starts that
        // stage again on the new text.
        let staged = if in_stage {
            super::pipeline::restart_open(team, id, events)?
        } else if submitted && was_captured {
            super::pipeline::on_report(team, id, events)?
        } else {
            None
        };
        if let Some(result) = staged {
            return Ok(result);
        }
        return Ok(json!({"status":"submitted","task":id,"attempt":n}));
    }
    // A late report of a revoked or replaced attempt is refused and shown.
    let n = task.attempts.iter().find(|a| a.id == attempt_id).map(|a| a.n);
    // The refusal is a result, not an error, so its feed entry is kept.
    events.push(Event::new("stale_report", json!({"by":label,"task":id,"attempt":n})));
    Ok(json!({"status":"stale","task":id,"attempt":n}))
}

/// C5: the exit code decides a check task's report. The lead is not asked; the
/// expected code accepts, anything else finally rejects, and the feed says
/// which happened.
fn decide_check(team: &mut Team, id: u32, n: u32, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    let task = team.task_mut(id).context("Unknown task")?;
    let expected = task.check.clone().context("Task is not a check")?;
    let exit = task
        .attempts
        .iter()
        .rev()
        .find(|attempt| attempt.state == AttemptState::Reported)
        .and_then(|attempt| attempt.check.as_ref())
        .map(|check| check.exit)
        .context("The check result is missing")?;
    let title = task.title.clone();
    mark_decided(task);
    if exit == expected.expect_exit {
        revoke(task, "check passed");
        task.status = TaskStatus::Accepted;
        task.decided_by = None;
        task.decision_reason = format!("check passed: exit {exit}");
        events.push(Event::new(
            "check_passed",
            json!({"task":id,"attempt":n,"exit":exit,"expect":expected.expect_exit,"command":expected.command,"title":title}),
        ));
        Ok(json!({"status":"accepted","task":id,"attempt":n,"check":"passed"}))
    } else {
        // N6: a failed check is a non-final reject: the task returns for
        // another attempt and only the lead's decision ends it. The attempt
        // keeps its check result.
        if let Some(attempt) = task.attempts.iter_mut().rev().find(|attempt| attempt.state == AttemptState::Reported) {
            attempt.state = AttemptState::Rejected;
        }
        revoke(task, "check failed");
        task.status = TaskStatus::Pending;
        // N6: the failed check waits for the lead; Zerus never reruns it on its
        // own. Release gives it another attempt, cancel ends it.
        task.hold = Some(Hold {
            by: "Zerus".into(),
            at: now(),
            reason: format!("check failed: exit {exit}, expected {}", expected.expect_exit),
        });
        task.decided_by = None;
        task.decision_reason = format!("check failed: exit {exit}, expected {}", expected.expect_exit);
        events.push(Event::new(
            "check_failed",
            json!({"task":id,"attempt":n,"exit":exit,"expect":expected.expect_exit,"command":expected.command,"title":title,"final":false}),
        ));
        Ok(json!({"status":"rejected","task":id,"attempt":n,"check":"failed","final":false}))
    }
}

/// C12: a member declares how long its newest Background job should take; the
/// dispatcher ties the budget to the live job and the lead hears when it is
/// red. A budget never stops anything.
fn expect(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    ensure!(matches!(team.status, TeamStatus::Running | TeamStatus::Paused), "The team is not running");
    let Actor::Member(member) = actor else {
        bail!("Only a team member declares a budget for its own work");
    };
    let label = clean(text(input, "label").trim(), 80, "A budget label")?;
    ensure!(!label.is_empty(), "A budget needs a label");
    let minutes = input["minutes"]
        .as_u64()
        .and_then(|minutes| u32::try_from(minutes).ok())
        .filter(|minutes| (1..=1440).contains(minutes))
        .context("A budget is 1 to 1440 minutes")?;
    let red_minutes = match input["red_minutes"].as_u64() {
        Some(red) => u32::try_from(red)
            .ok()
            .filter(|red| (1..=2880).contains(red))
            .context("A red time is 1 to 2880 minutes")?,
        None => minutes.saturating_mul(2),
    };
    ensure!(red_minutes >= minutes, "The red time cannot come before the estimate");
    let role = team.role_of(member);
    let found = team.members.iter_mut().find(|found| found.id == *member).context("Unknown member")?;
    found.budget = Some(Budget { label: label.clone(), minutes, red_minutes, declared_at: now(), ..Default::default() });
    events.push(Event::new(
        "budget_declared",
        json!({"member":role,"label":label,"minutes":minutes,"red_minutes":red_minutes}),
    ));
    Ok(json!({"status":"expected","label":label,"minutes":minutes,"red_minutes":red_minutes}))
}

/// H6: the owner swaps a member's session, agent, model, effort or account
/// (the lead included). The member keeps its id, role, tasks, folder and
/// branch; the old session keeps running and is listed for the owner. An open
/// attempt is revoked (`swapped`) and handed again to the new session with the
/// previous attempt's report and open questions as a continuation. U54: a bulk
/// request (`members` plus `bulk`) moves each named member with one account
/// change and keeps its session, conversation and attempt.
fn swap_member(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    // N9: the lead swaps on the owner's verified word; the owner directly.
    ensure!(
        matches!(actor, Actor::Human)
            || (actor.is_lead(team) && input["roster_word"].as_bool() == Some(true)),
        "Only you swap a member; the lead needs your word"
    );
    ensure!(
        matches!(team.status, TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused),
        "Only a planning, running or paused team swaps a member"
    );
    if let Some(ids) = input["members"].as_array().filter(|ids| !ids.is_empty()) {
        // U54: one owner action moves every selected member; each is reported.
        // A member whose account switch did not confirm keeps its binding and
        // carries the reason.
        for failure in input["failed"].as_array().into_iter().flatten() {
            if let Some(member) = team.members.iter_mut().find(|member| member.id == text(failure, "member")) {
                member.error = text(failure, "error").to_owned();
            }
        }
        // A member the switch could not move falls back to a fresh session; the
        // old one was paused and renamed, and the feed says so.
        let fresh: Vec<String> = input["fresh"]
            .as_array()
            .map(|ids| ids.iter().filter_map(|id| id.as_str()).map(str::to_owned).collect())
            .unwrap_or_default();
        for note in input["notes"].as_array().into_iter().flatten() {
            events.push(Event::new(
                "member_retired",
                json!({"member":team.role_of(text(note, "member")),"note":text(note, "note")}),
            ));
        }
        let mut swapped = Vec::new();
        for id in ids.iter().filter_map(|id| id.as_str()) {
            let keep = input["bulk"].as_bool() == Some(true) && !fresh.iter().any(|fresh| fresh == id);
            swapped.push(swap_one(team, input, id, keep, actor, events)?);
        }
        return Ok(json!({"status":"swapped","members":swapped,"fresh":fresh}));
    }
    let id = text(input, "member").to_owned();
    ensure!(!id.is_empty(), "A member id is required");
    swap_one(team, input, &id, false, actor, events)
}

fn swap_one(team: &mut Team, input: &Value, id: &str, keep_session: bool, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    let state = team.member(id).map(|member| member.state).with_context(|| format!("Unknown member {id}"))?;
    // N31: a removed member stays removed (and outside max_members); a
    // paused one is resumed first, so a swap never lifts its pause.
    ensure!(state != MemberState::Removed, "This member was removed");
    ensure!(state != MemberState::Paused, "Resume {id} before swapping it");
    let session: Option<SessionRef> = match input.get("session") {
        Some(value) if value.is_object() => Some(serde_json::from_value(value.clone()).context("Invalid session")?),
        _ => None,
    };
    let role = team.role_of(id);
    let lead = id == team.lead;
    let at = now();
    let mut handed = Vec::new();
    if !keep_session {
        for task in &mut team.tasks {
            let Some(attempt) = task.open_attempt() else { continue };
            if attempt.member != id {
                continue;
            }
            if !lead {
                let report = attempt.report.as_ref().map(|report| report.text.chars().take(2000).collect()).unwrap_or_default();
                let questions: Vec<String> = attempt
                .questions
                .iter()
                .filter(|question| question.open())
                .map(|question| question.text.chars().take(500).collect())
                .collect();
            task.continuation = Some(Continuation { attempt: attempt.id.clone(), n: attempt.n, since: attempt.sent_at, report, questions });
        }
        if let Some(open) = task.attempts.iter_mut().rev().find(|attempt| attempt.state.open()) {
            open.state = AttemptState::Revoked;
            open.error = "swapped".to_owned();
        }
        task.status = TaskStatus::Pending;
        task.decided_by = None;
        handed.push(task.id);
        }
    }
    let mut from = String::new();
    let mut changed = Vec::new();
    {
        let member = team.members.iter_mut().find(|member| member.id == id).unwrap();
        if keep_session {
            from = member.session.as_ref().map(|session| session.name.clone()).unwrap_or_default();
        } else if let Some(old) = member.session.take() {
            from = old.name;
        }
        let fields =
            [("agent", &mut member.agent), ("model", &mut member.model), ("effort", &mut member.effort), ("account", &mut member.account)];
        for (key, field) in fields {
            if let Some(value) = input[key].as_str().map(str::trim).filter(|value| !value.is_empty()) {
                *field = value.to_owned();
                changed.push(key);
            }
        }
        // N32: an adopted session works in its own folder; the continuation
        // and the folder checks follow it there.
        if let (Some(_), Some(directory)) = (&session, input["directory"].as_str().filter(|directory| !directory.is_empty())) {
            member.directory = directory.to_owned();
            if let Ok(folder) = serde_json::from_value::<MemberFolder>(input["folder"].clone()) {
                member.folder = folder;
            }
        }
        if !keep_session {
            member.session = session;
            member.launch_after_swap = member.session.is_none();
            member.state = if member.session.is_some() { MemberState::Active } else { MemberState::NotCreated };
        } else {
            member.launch_after_swap = false;
        }
        member.launch_id.clear();
        member.error.clear();
        member.seen = None;
        member.settings_applied = false;
        member.pending = None;
        member.permission = None;
        // N29: `limit_resume` and `limit_session` stay: the dispatcher's next
        // pass withdraws the recovery request for the old session by name.
        member.limited = None;
    }
    let agent = team.member(id).map(|member| member.agent.clone()).unwrap_or_default();
    let to = team.member(id).and_then(|member| member.session.as_ref()).map(|session| session.name.clone()).unwrap_or_default();
    // U54: the new lead's brief names the previous lead's conversation and
    // transcript; the old session's own record has them, when it can be read.
    let previous = if lead && !keep_session {
        crate::state::storage::read(&from).ok()
    } else {
        None
    };
    let conversation = previous.as_ref().and_then(|record| record["conversation_id"].as_str()).unwrap_or_default().to_owned();
    let transcript = previous.as_ref().and_then(|record| record["transcript"].as_str()).unwrap_or_default().to_owned();
    team.swapped.push(json!({"member":role,"old":from,"at":at,"by":"You",
        "conversation":conversation,"transcript":transcript}));
    let excess = team.swapped.len().saturating_sub(20);
    team.swapped.drain(..excess);
    // The lead's open task stays in its inbox and the new lead session gets
    // the one brief that names the team and `hgs team inbox` (H6, U8).
    if lead && !keep_session {
        team.brief = Notice::Pending;
        team.brief_id.clear();
    }
    let by = if input["roster_word"].as_bool() == Some(true) {
        format!("{} on your word", actor.label(team))
    } else {
        "You".to_owned()
    };
    let quote = input["owner_word_checked"]["text"].as_str().unwrap_or_default();
    events.push(Event::new(
        "member_swapped",
        json!({"member":role,"from":from,"to":to,"by":by,"agent":agent,"changed":changed,"lead":lead,"owner_word":quote}),
    ));
    Ok(json!({"status":"swapped","member":id,"from":from,"to":to,"agent":agent,"tasks":handed,"lead":lead,"by":by}))
}

/// Section 9.2: the root of a team's tree, by parent links among the loaded
/// teams (a missing parent stops the walk at the team itself).
pub(crate) fn root_of(team: &Team, all: &[Team]) -> Team {
    let mut current = team.clone();
    let mut seen = BTreeSet::new();
    while let Some(parent) = current.parent.clone() {
        if !seen.insert(current.id.clone()) {
            break;
        }
        match all.iter().find(|candidate| candidate.id == parent.team) {
            Some(next) => current = next.clone(),
            None => break,
        }
    }
    current
}

/// How many sub-team levels a team sits below its master.
pub(crate) fn depth_of(team: &Team, all: &[Team]) -> u32 {
    let mut depth = 0;
    let mut current = team.clone();
    let mut seen = BTreeSet::new();
    while let Some(parent) = current.parent.clone() {
        if depth >= 32 || !seen.insert(current.id.clone()) {
            break;
        }
        match all.iter().find(|candidate| candidate.id == parent.team) {
            Some(next) => {
                depth += 1;
                current = next.clone();
            }
            None => break,
        }
    }
    depth
}

/// The sessions a team binds (its lead included), without removed members.
pub(crate) fn session_count(team: &Team) -> usize {
    // N26: a planned member without a session still gets launched, so it binds
    // a session in the tree budget.
    team.members.iter().filter(|member| member.state != MemberState::Removed).count()
}

/// The sessions the whole tree rooted at `master` binds. A shared sub-lead
/// session counts once, and finished teams no longer bind sessions (N26).
pub(crate) fn tree_session_count(master: &Team, all: &[Team]) -> usize {
    let mut seen = BTreeSet::new();
    let mut sessions = BTreeSet::new();
    let mut queue = vec![master.id.clone()];
    while let Some(id) = queue.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(team) = all.iter().find(|team| team.id == id) {
            if !matches!(team.status, TeamStatus::Done | TeamStatus::Stopped | TeamStatus::Archived) {
                for member in team.members.iter().filter(|member| member.state != MemberState::Removed) {
                    let key = member.session.as_ref().map_or_else(
                        || format!("member:{}:{}", team.id, member.id),
                        |session| format!("session:{}:{}", session.host, session.name),
                    );
                    sessions.insert(key);
                }
            }
            queue.extend(team.children.iter().cloned());
        }
    }
    // Parent links are the truth even if a children list was not kept.
    for team in all.iter().filter(|team| !seen.contains(&team.id)) {
        if root_of(team, all).id == master.id && !matches!(team.status, TeamStatus::Done | TeamStatus::Stopped | TeamStatus::Archived) {
            for member in team.members.iter().filter(|member| member.state != MemberState::Removed) {
                let key = member.session.as_ref().map_or_else(
                    || format!("member:{}:{}", team.id, member.id),
                    |session| format!("session:{}:{}", session.host, session.name),
                );
                sessions.insert(key);
            }
        }
    }
    sessions.len()
}

/// Section 9.2: a sub-team may be created only within the master's depth, its
/// 32 children and the tree's session budget.
pub(crate) fn ensure_tree_create(parent: &Team, master: &Team, all: &[Team], child: &Team) -> anyhow::Result<()> {
    ensure!(parent.children.len() < 32, "A team has at most 32 sub-teams");
    let depth = depth_of(parent, all) + 1;
    ensure!(
        depth <= master.settings.tree_depth,
        "Sub-teams go at most {} levels below the master",
        master.settings.tree_depth
    );
    let sessions = tree_session_count(master, all) + session_count(child);
    ensure!(
        sessions <= master.settings.tree_sessions as usize,
        "The whole tree runs at most {} sessions",
        master.settings.tree_sessions
    );
    Ok(())
}

/// Section 9.2: a change that adds sessions (add-member, swap-member) keeps the
/// tree within the master's session budget.
pub(crate) fn ensure_tree_sessions(team: &Team, all: &[Team], added: usize) -> anyhow::Result<()> {
    let master = root_of(team, all);
    let sessions = tree_session_count(&master, all) + added;
    ensure!(
        sessions <= master.settings.tree_sessions as usize,
        "The whole tree runs at most {} sessions",
        master.settings.tree_sessions
    );
    Ok(())
}

/// The id of the next question of a team: `q1`, `q2`, ... Questions are never
/// removed, so an id names one question for the whole team (A3), which lets a
/// reply name it without a task.
fn next_question(team: &Team) -> String {
    let asked: usize = team.tasks.iter().flat_map(|task| &task.attempts).map(|attempt| attempt.questions.len()).sum();
    format!("q{}", asked + 1)
}

/// A member asks the lead about its open attempt (A3). The attempt stays open
/// and is not captured while the question is open.
/// #630 N6: what a member asks about: an open attempt or an open stage run.
enum AskTarget {
    Attempt(usize),
    Stage(usize),
}

/// #630 N6: where a question waits: on an attempt or on a stage run.
enum AskedWhere {
    Attempt(usize),
    Stage(usize),
}

fn ask(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    ensure!(matches!(team.status, TeamStatus::Running | TeamStatus::Paused), "The team is not running");
    let Actor::Member(member) = actor else {
        bail!("Only a team member asks the lead");
    };
    ensure!(*member != team.lead, "The lead has no task to ask about");
    let body = clean(text(input, "text").trim(), 4000, "A question")?;
    ensure!(!body.is_empty(), "A question needs text");
    let given = input["task"].as_u64().and_then(|id| u32::try_from(id).ok());
    // Only an attempt, or a stage run given to the caller, that is still open
    // belongs to the caller (#630 N6: a stage member asks about its stage).
    let open: Vec<(usize, usize)> = team
        .tasks
        .iter()
        .enumerate()
        .filter_map(|(task, value)| {
            value.attempts.iter().rposition(|attempt| attempt.state.open() && attempt.member == *member).map(|attempt| (task, attempt))
        })
        .collect();
    let stages: Vec<(usize, usize)> = team
        .tasks
        .iter()
        .enumerate()
        .filter_map(|(task, value)| {
            let pipeline = value.pipeline_run.as_ref()?;
            let run = pipeline.open_run().filter(|run| run.state == StageState::Delivered && !run.member.is_empty() && run.member == *member)?;
            pipeline.runs.iter().rposition(|found| found.id == run.id).map(|run| (task, run))
        })
        .collect();
    let target = |task: usize| team.tasks[task].id;
    let (task, asked) = match given {
        Some(id) => {
            let found = open
                .iter()
                .find(|(task, _)| target(*task) == id)
                .map(|(task, attempt)| (*task, AskTarget::Attempt(*attempt)))
                .or_else(|| stages.iter().find(|(task, _)| target(*task) == id).map(|(task, run)| (*task, AskTarget::Stage(*run))));
            found.with_context(|| format!("Task #{id} has no open attempt or stage of yours"))?
        }
        None => match (open.as_slice(), stages.as_slice()) {
            ([only], []) => (only.0, AskTarget::Attempt(only.1)),
            ([], [only]) => (only.0, AskTarget::Stage(only.1)),
            ([], []) => bail!("You have no open attempt or stage to ask about"),
            (open, stages) => {
                let tasks: Vec<String> = open.iter().chain(stages).map(|(task, _)| format!("#{}", target(*task))).collect();
                bail!("You have {} open pieces of work: {}; name one with \"task\"", tasks.len(), tasks.join(", "));
            }
        },
    };
    let question = next_question(team);
    let role = team.role_of(member);
    let task_id = team.tasks[task].id;
    match asked {
        AskTarget::Stage(run) => {
            let (round, kind, run_id) = {
                let found = team.tasks[task].pipeline_run.as_ref().and_then(|pipeline| pipeline.runs.get(run)).context("The stage ended")?;
                (found.round, found.kind.clone(), found.id.clone())
            };
            let found = team.tasks[task].pipeline_run.as_mut().and_then(|pipeline| pipeline.runs.get_mut(run)).context("The stage ended")?;
            ensure!(found.questions.len() < 20, "This stage has 20 questions already (the most allowed)");
            ensure!(
                found.questions.iter().filter(|question| question.open()).count() < 3,
                "This stage has 3 open questions; wait for the lead's answer"
            );
            found.questions.push(Question { id: question.clone(), text: body, at: now(), ..Default::default() });
            events.push(Event::new("question_asked", json!({"by":role,"task":task_id,"stage":kind,"round":round,"question":question})));
            Ok(json!({"status":"asked","question":question,"task":task_id,"attempt":run_id,"stage":kind,"round":round}))
        }
        AskTarget::Attempt(attempt) => {
            let n = team.tasks[task].attempts[attempt].n;
            let found = &mut team.tasks[task].attempts[attempt];
            ensure!(found.questions.len() < 20, "This attempt has 20 questions already (the most allowed)");
            ensure!(
                found.questions.iter().filter(|question| question.open()).count() < 3,
                "This attempt has 3 open questions; wait for the lead's answer"
            );
            found.questions.push(Question { id: question.clone(), text: body, at: now(), ..Default::default() });
            events.push(Event::new("question_asked", json!({"by":role,"task":task_id,"attempt":n,"question":question})));
            Ok(json!({"status":"asked","question":question,"task":task_id,"attempt":n}))
        }
    }
}

/// The lead (or you) answers one of a member's questions (A3); the member gets
/// the answer as a message.
fn reply(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    // Spec 5.6: a sub-lead answers the questions of its own reports.
    ensure!(matches!(actor, Actor::Human | Actor::Member(_)), "Only the lead or you answer a member's question");
    // C7: a forwarded sub-team question is "sub:<team id>:<question id>",
    // once per level (at most 8 levels).
    let question = clean(text(input, "question").trim(), 400, "A question id")?;
    ensure!(!question.is_empty(), "A question id is required");
    let body = clean(text(input, "text").trim(), 4000, "An answer")?;
    ensure!(!body.is_empty(), "An answer needs text");
    // A question id names one question of the team; it belongs to an attempt or
    // to a stage run (#630 N6).
    let mut found: Option<(usize, AskedWhere, usize)> = None;
    'tasks: for (task, value) in team.tasks.iter().enumerate() {
        for (attempt, value) in value.attempts.iter().enumerate() {
            if let Some(question_at) = value.questions.iter().position(|q| q.id == question) {
                found = Some((task, AskedWhere::Attempt(attempt), question_at));
                break 'tasks;
            }
        }
        if let Some(pipeline) = value.pipeline_run.as_ref() {
            for (run, value) in pipeline.runs.iter().enumerate() {
                if let Some(question_at) = value.questions.iter().position(|q| q.id == question) {
                    found = Some((task, AskedWhere::Stage(run), question_at));
                    break 'tasks;
                }
            }
        }
    }
    let Some((task, where_at, question_at)) = found else {
        bail!("Unknown question {question}")
    };
    let asked_by = match where_at {
        AskedWhere::Attempt(attempt) => team.tasks[task].attempts[attempt].member.clone(),
        AskedWhere::Stage(run) => team.tasks[task].pipeline_run.as_ref().and_then(|pipeline| pipeline.runs.get(run)).map(|run| run.member.clone()).unwrap_or_default(),
    };
    ensure!(actor.reviews(team, &asked_by), "Only the lead or you answer a member's question");
    let asked = match where_at {
        AskedWhere::Attempt(attempt) => &team.tasks[task].attempts[attempt].questions[question_at],
        AskedWhere::Stage(run) => &team.tasks[task].pipeline_run.as_ref().and_then(|pipeline| pipeline.runs.get(run)).context("The stage ended")?.questions[question_at],
    };
    ensure!(
        !team.tasks[task].status.finished(),
        "Task #{} is decided; its question {question} is closed",
        team.tasks[task].id
    );
    if !asked.open() {
        bail!(
            "Question {question} was {}",
            if asked.answer.is_empty() { "closed with its attempt" } else { "already answered" }
        );
    }
    let task_id = team.tasks[task].id;
    let label = actor.label(team);
    let asked = match where_at {
        AskedWhere::Attempt(attempt) => &mut team.tasks[task].attempts[attempt].questions[question_at],
        AskedWhere::Stage(run) => &mut team.tasks[task].pipeline_run.as_mut().and_then(|pipeline| pipeline.runs.get_mut(run)).context("The stage ended")?.questions[question_at],
    };
    asked.answer = body;
    asked.answered_at = Some(now());
    asked.by = label.clone();
    // The answer reaches the member as a message; the dispatcher sends it once
    // its session is live (A3).
    asked.notice = Notice::Pending;
    match where_at {
        AskedWhere::Attempt(attempt) => {
            let n = team.tasks[task].attempts[attempt].n;
            events.push(Event::new("question_answered", json!({"by":label,"task":task_id,"attempt":n,"question":question})));
        }
        AskedWhere::Stage(run) => {
            let (stage, round) = {
                let found = &team.tasks[task].pipeline_run.as_ref().and_then(|pipeline| pipeline.runs.get(run)).context("The stage ended")?;
                (found.kind.clone(), found.round)
            };
            events.push(Event::new("question_answered", json!({"by":label,"task":task_id,"stage":stage,"round":round,"question":question})));
        }
    }
    Ok(json!({"status":"answered","question":question}))
}

/// A5: keeps a pending task back from the dispatcher until `release`.
fn hold(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    let assignee = team.task(task_id(input)?).map(|task| task.assignee.clone()).unwrap_or_default();
    ensure!(actor.reviews(team, &assignee), "Only the lead or you hold a task");
    // A finished team keeps its tasks as they are (L1).
    ensure!(
        matches!(team.status, TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused),
        "Only a planning, running or paused team holds tasks"
    );
    let id = task_id(input)?;
    let reason = clean(text(input, "reason").trim(), 4000, "A reason")?;
    let label = actor.label(team);
    let task = team.task_mut(id).context("Unknown task")?;
    ensure!(!task.status.finished(), "Task #{id} is already decided");
    ensure!(task.status == TaskStatus::Pending, "Only a pending task can be held");
    ensure!(task.hold.is_none(), "Task #{id} is already held");
    task.hold = Some(Hold { by: label.clone(), at: now(), reason: reason.clone() });
    events.push(Event::new("held", json!({"by":label,"task":id,"reason":reason})));
    Ok(json!({"status":"held","task":id}))
}

/// A5: clears a hold; the task is handed out again when it is ready.
fn release(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    let assignee = team.task(task_id(input)?).map(|task| task.assignee.clone()).unwrap_or_default();
    ensure!(actor.reviews(team, &assignee), "Only the lead or you release a task");
    // A finished team keeps its tasks as they are (L1).
    ensure!(
        matches!(team.status, TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused),
        "Only a planning, running or paused team releases tasks"
    );
    let id = task_id(input)?;
    let label = actor.label(team);
    let task = team.task_mut(id).context("Unknown task")?;
    ensure!(task.hold.is_some(), "Task #{id} is not held");
    task.hold = None;
    events.push(Event::new("released", json!({"by":label,"task":id})));
    Ok(json!({"status":"released","task":id}))
}

/// D2: files a report's findings; later ones are deferred at once.
pub(crate) fn file_findings(team: &mut Team, from: u32, report: &Value, by: &str) -> anyhow::Result<(usize, usize)> {
    // N13: checked when the report came; one stored before that check is
    // filed without its bad findings rather than blocking the accept.
    let findings: Vec<(CheckedFinding, Option<u32>)> = report["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|finding| checked_finding(team, finding).ok().map(|checked| (checked, finding["line"].as_u64().map(|line| line as u32))))
        .collect();
    // N14: the cap counts the findings still in play; the oldest closed ones
    // (fixed, dropped) make room, so the store stays bounded.
    const KEPT: usize = 1000;
    let live = team.findings.iter().filter(|finding| !finding.closed()).count();
    ensure!(live + findings.len() <= KEPT, "A team keeps at most {KEPT} findings that are not fixed or dropped");
    while team.findings.len() + findings.len() > KEPT {
        let Some(oldest) = team.findings.iter().position(Finding::closed) else { break };
        team.findings.remove(oldest);
    }
    let mut deferred = 0usize;
    for (finding, line) in &findings {
        let CheckedFinding { severity, title, scenario, file, task } = finding;
        let (severity, title, scenario, file, task) = (severity.clone(), title.clone(), scenario.clone(), file.clone(), *task);
        let state = if severity == "later" {
            deferred += 1;
            "deferred"
        } else {
            "open"
        };
        team.findings_seq += 1;
        team.findings.push(Finding {
            id: format!("f{}", team.findings_seq),
            from_task: from,
            severity,
            title,
            file,
            line: *line,
            scenario,
            task,
            state: state.into(),
            by: by.into(),
            at: now(),
            note: String::new(),
            proposal: String::new(),
            phase: None,
        });
    }
    Ok((findings.len(), deferred))
}

/// D2: the lead or owner acts on a finding: reopen, urgent, replan, fix_phase,
/// defer, fixed or dropped.
fn finding(team: &mut Team, input: &Value, actor: &Actor, offline: &BTreeSet<String>, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    ensure!(matches!(actor, Actor::Human) || actor.is_lead(team), "Only the lead or you act on a finding");
    let id = text(input, "finding").to_owned();
    let at = team.findings.iter().position(|finding| finding.id == id).with_context(|| format!("Unknown finding {id}"))?;
    let action = text(input, "action").to_owned();
    let note = clean(text(input, "note").trim(), 2000, "A finding note")?;
    let severity = team.findings[at].severity.clone();
    // N20: an action that gives the team work again needs a team that still
    // runs; closing or deferring is bookkeeping and works on a finished team.
    if matches!(action.as_str(), "reopen" | "urgent" | "replan" | "fix_phase") {
        ensure!(
            matches!(team.status, TeamStatus::Running | TeamStatus::Paused),
            "The team is {}: reopen, urgent, replan and fix_phase need a running or paused team",
            serde_json::to_value(team.status)?.as_str().unwrap_or_default()
        );
    }
    let mut proposal = String::new();
    let mut phase = None;
    let state = match action.as_str() {
        "reopen" => {
            let task_id = input["task"].as_u64().map(|n| n as u32).or(team.findings[at].task).context("A reopen names its task")?;
            let location = if team.findings[at].file.is_empty() {
                String::new()
            } else {
                format!("\n{}:{}", team.findings[at].file, team.findings[at].line.unwrap_or(0))
            };
            let mut text = format!(
                "Finding {} ({}): {}{}\nScenario: {}",
                team.findings[at].id, severity, team.findings[at].title, location, team.findings[at].scenario
            );
            if !note.is_empty() {
                text.push_str(&format!("\nLead's note: {note}"));
            }
            let task = team.task_mut(task_id).context("Unknown task")?;
            ensure!(task.status == TaskStatus::Accepted, "A reopen needs an accepted task");
            task.history.push(json!({"decided_by":task.decided_by,"reason":task.decision_reason,"at":now()}));
            task.pinned = true;
            task.reopen_note = text;
            task.status = TaskStatus::Pending;
            task.decided_by = None;
            task.decision_reason = String::new();
            "reopened"
        }
        "urgent" => {
            let mut quick = input.clone();
            quick["urgent"] = json!(true);
            extend(team, &quick, actor, offline, events)?;
            "urgent"
        }
        "replan" => {
            ensure!(matches!(team.status, TeamStatus::Running | TeamStatus::Paused), "The team is not running");
            proposal = clean(text(input, "proposal").trim(), 4000, "A proposal")?;
            ensure!(!proposal.is_empty(), "A replan carries the owner's proposal");
            team.status = TeamStatus::Paused;
            events.push(Event::new("replan_proposed", json!({"by":actor.label(team),"finding":id,"proposal":proposal})));
            "replan"
        }
        "fix_phase" => {
            // N18 (spec 8): a normal finding goes to a draft phase; a critical
            // one is reopened, made urgent or replanned.
            ensure!(severity == "normal", "Only a normal finding is marked for a draft phase");
            let n = input["phase"].as_u64().context("fix_phase names its phase")? as u32;
            ensure!(team.phases.iter().any(|found| found.n == n && found.state.is_draft()), "Phase {n} is not a draft");
            phase = Some(n);
            "fix_phase"
        }
        "defer" => {
            ensure!(severity != "critical" || matches!(actor, Actor::Human), "Only you defer a critical finding");
            "deferred"
        }
        "fixed" => "fixed",
        "dropped" => "dropped",
        _ => bail!("A finding action is reopen, urgent, replan, fix_phase, defer, fixed or dropped"),
    };
    team.findings[at].state = state.into();
    team.findings[at].by = actor.label(team);
    team.findings[at].note = note;
    // N17, N18: the proposal and the phase stay on the finding for status,
    // the inbox and the desktop.
    team.findings[at].proposal = proposal;
    team.findings[at].phase = phase;
    events.push(Event::new("finding_action", json!({"finding":id,"action":action,"state":state,"phase":phase})));
    Ok(json!({"status":action,"finding":id,"state":state}))
}

/// F5: the owner and the lead edit a draft; the owner creates or discards it.
fn draft(team: &mut Team, input: &Value, actor: &Actor, offline: &BTreeSet<String>, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    ensure!(team.status == TeamStatus::Draft, "The team is not a draft");
    let human = matches!(actor, Actor::Human);
    let action = text(input, "action").to_owned();
    let mut state = team.draft.clone().unwrap_or_default();
    match action.as_str() {
        "update" => {
            ensure!(human || actor.is_lead(team), "Only the lead or you edit the draft");
            let revision = input["revision"].as_u64().context("A draft update carries the revision it read")?;
            ensure!(revision == state.revision, "Draft revision is {} now; read it again", state.revision);
            let set = input["set"].as_object().context("A draft update names the fields to set")?;
            let mut changed = Vec::new();
            for (field, value) in set {
                if !human && state.owner_edited.contains(field) {
                    bail!("Owner changed {field}");
                }
                match field.as_str() {
                    "name" => team.name = clean(value.as_str().unwrap_or_default().trim(), 80, "A name")?,
                    "goal" => team.goal = clean(value.as_str().unwrap_or_default(), 4000, "A goal")?,
                    // Spec 3: the protocol addendum is the team's lead protocol.
                    "addendum" => team.lead_protocol = clean(value.as_str().unwrap_or_default(), 8000, "The lead protocol")?,
                    "settings" => {
                        let settings: Settings = serde_json::from_value(value.clone()).context("Invalid settings")?;
                        validate_settings(&settings)?;
                        // C4: the lead drafts the rest; the owner's settings stay theirs.
                        ensure_lead_settings(&team.settings, &settings, actor, false)?;
                        team.settings = settings;
                    }
                    "members" => {
                        let lead = team.member(&team.lead).cloned();
                        let mut members: Vec<Member> = value
                            .as_array()
                            .context("Members are a list")?
                            .iter()
                            .map(parse_member)
                            .collect::<anyhow::Result<Vec<_>>>()?;
                        ensure!(!members.is_empty(), "A draft keeps at least its lead");
                        // C3: the same member checks as a plan: unique ids, the
                        // auto folder default and the B5 widening the owner
                        // confirms at create.
                        for (index, member) in members.iter().enumerate() {
                            ensure!(members[..index].iter().all(|m| m.id != member.id), "Member id {} is used twice", member.id);
                        }
                        let given: Vec<&Value> = value.as_array().into_iter().flatten().collect();
                        if team.settings.folder_policy == FolderPolicy::Auto {
                            for (member, value) in members.iter_mut().zip(given) {
                                if value.get("folder").is_none() && member.directory.is_empty() && member.session.is_none() {
                                    member.folder = if team.settings.worktrees_forbidden {
                                        MemberFolder::Shared
                                    } else {
                                        super::dispatcher::default_folder(&member.agent)
                                    };
                                }
                            }
                        }
                        if let Some(lead) = lead {
                            members.retain(|member| member.id != lead.id);
                            members.insert(0, lead);
                        }
                        // Nothing is approved yet, so every member above the
                        // role default widens it.
                        team.profile_widening = members
                            .iter()
                            .filter(|member| member.id != team.lead && profile_rank(&member.permissions_profile) > profile_rank("write"))
                            .map(|member| json!({"member":member.id,"from":"write","to":member.permissions_profile}))
                            .collect();
                        team.members = members;
                    }
                    "tasks" => {
                        let tasks: Vec<Task> = value
                            .as_array()
                            .context("Tasks are a list")?
                            .iter()
                            .map(parse_task)
                            .collect::<anyhow::Result<Vec<_>>>()?;
                        team.tasks = tasks;
                    }
                    other => bail!("A draft sets name, goal, addendum, settings, members or tasks, not {other}"),
                }
                changed.push(field.clone());
            }
            if human {
                for field in &changed {
                    if !state.owner_edited.contains(field) {
                        state.owner_edited.push(field.clone());
                    }
                }
                state.unseen.retain(|field| !changed.contains(field));
            } else {
                for field in &changed {
                    if !state.unseen.contains(field) {
                        state.unseen.push(field.clone());
                    }
                }
            }
            state.revision += 1;
            team.draft = Some(state.clone());
            events.push(Event::new("draft_updated", json!({"by":actor.label(team),"fields":changed,"revision":state.revision})));
            // C1: the store answers its own team revision as "revision".
            Ok(json!({"status":"updated","draft_revision":state.revision,"unseen":state.unseen}))
        }
        "seen" => {
            ensure!(human, "Only you mark the draft as seen");
            let fields: Vec<String> = input["fields"].as_array().into_iter().flatten().filter_map(|field| field.as_str().map(str::to_owned)).collect();
            state.unseen.retain(|field| !fields.contains(field));
            team.draft = Some(state.clone());
            Ok(json!({"status":"seen","unseen":state.unseen}))
        }
        "create" => {
            ensure!(human, "Only you create the team from the draft");
            let validation = plan::validate(team, offline);
            ensure!(
                validation.errors.is_empty(),
                "Fix {} plan error(s) to create: {}",
                validation.errors.len(),
                validation.errors.iter().map(|finding| finding.text.as_str()).collect::<Vec<_>>().join("; ")
            );
            let approve = input["approve"].as_bool() == Some(true);
            if approve {
                ensure!(
                    team.profile_widening.is_empty() || input["confirm_widening"].as_bool() == Some(true),
                    "The owner confirms the wider permissions first: create with \"confirm_widening\": true"
                );
            }
            team.status = if approve { TeamStatus::Running } else { TeamStatus::Planning };
            for task in &mut team.tasks {
                if task.status == TaskStatus::Draft && task.phase == 1 {
                    task.status = TaskStatus::Pending;
                }
            }
            team.draft = None;
            // N6: the same lead session now leads the team and gets its planning
            // brief once, created from a draft or not.
            team.brief = Notice::Pending;
            events.push(Event::new("draft_created", json!({"approve":approve,"team":team.id})));
            Ok(json!({"status":if approve {"running"} else {"planning"},"team":team.id}))
        }
        "discard" => {
            ensure!(human, "Only you discard a draft");
            let reason = clean(text(input, "reason").trim(), 2000, "A reason")?;
            team.status = TeamStatus::Stopped;
            // N7: the durable reason the owner discarded it for, and the
            // request to stop the lead's session (the command stops it).
            team.stop_reason = "draft_discarded".into();
            team.draft = None;
            team.brief = Notice::None;
            let stop_lead = input["stop_lead"].as_bool() == Some(true);
            events.push(Event::new(
                "draft_discarded",
                json!({"stop_lead":stop_lead,"reason":reason,"by":actor.label(team)}),
            ));
            Ok(json!({"status":"stopped","stop_lead":stop_lead,"reason":"draft_discarded"}))
        }
        other => bail!("A draft action is update, seen, get, create or discard, not {other}"),
    }
}

/// F7: the next team a Done team starts: the same folder, settings, lead and
/// members, whose live sessions are adopted; empty plan and goal. N8: a session
/// that is gone, or that now serves another active team, is not carried over -
/// that member starts fresh (NotCreated) - and no session-bound state of the
/// old team follows.
pub(crate) fn next_team(team: &Team, goal: &str, name: &str, live: &[super::dispatcher::Seen], others: &[Team]) -> Team {
    let mut next = Team {
        id: uuid::Uuid::new_v4().to_string(),
        name: if name.is_empty() { team.name.clone() } else { name.to_owned() },
        goal: goal.to_owned(),
        folder: team.folder.clone(),
        settings: team.settings.clone(),
        lead: team.lead.clone(),
        previous: team.id.clone(),
        status: TeamStatus::Planning,
        created: now(),
        members: Vec::new(),
        // N6/N8: the same lead session leads the new team and gets its brief.
        brief: Notice::Pending,
        ..Default::default()
    };
    for member in &team.members {
        // N21: a removed member stays removed; reviving it would count against
        // the tree budget and block its adoption elsewhere.
        if member.state == MemberState::Removed {
            continue;
        }
        let mut member = member.clone();
        let adopted = member.session.as_ref().filter(|session| !serves_another_team(session, others)).and_then(|session| {
            live.iter()
                .find(|seen| seen.host == session.host && seen.name == session.name && seen.live && !seen.run_id.is_empty())
                .map(|seen| SessionRef { run_id: seen.run_id.clone(), conversation_id: seen.conversation_id.clone(), ..session.clone() })
        });
        member.session = adopted;
        member.state = if member.session.is_some() { MemberState::Active } else { MemberState::NotCreated };
        // N8: none of the session-bound state of the old team follows.
        member.launch_id.clear();
        member.error.clear();
        member.settings_applied = false;
        member.asked.clear();
        member.pending = None;
        member.permission = None;
        member.permission_answers.clear();
        member.limited = None;
        member.limit_resume = false;
        member.budget = None;
        member.background_since = None;
        member.background_told = false;
        member.starved_since = None;
        member.starved_told = false;
        member.services.clear();
        member.quieted.clear();
        member.seen = None;
        member.launch_after_swap = false;
        member.inbox = Inbox::default();
        next.members.push(member);
    }
    next
}

/// N8: whether a session is the lead or a member of another active team; it
/// then stays with that team and the next team starts this member fresh.
fn serves_another_team(session: &SessionRef, others: &[Team]) -> bool {
    others.iter().any(|other| {
        other
            .member(&other.lead)
            .and_then(|lead| lead.session.as_ref())
            .is_some_and(|lead| lead.name == session.name && lead.host == session.host)
            || other
                .members
                .iter()
                .filter(|member| member.id != other.lead && member.state != MemberState::Removed)
                .any(|member| member.session.as_ref().is_some_and(|member| member.name == session.name && member.host == session.host))
    })
}

/// F8: the lead proposes a membership change; the owner decides.
fn propose(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    ensure!(actor.is_lead(team), "Only the lead proposes a change to your team");
    let kind = text(input, "kind").to_owned();
    ensure!(
        ["add-member", "remove-member", "swap-member", "next-goal", "switch-plan-mode",
         "add-to-phase", "cancel-tasks", "discard-phase", "question", "switch-setting"].contains(&kind.as_str()),
        "A proposal adds or removes members, adds tasks to a running phase, cancels tasks, discards a phase, starts the next goal, switches the plan mode or a setting, or asks you a question"
    );
    let reason = clean(text(input, "reason").trim(), 2000, "A proposal reason")?;
    // #740: a question carries its own words; any other proposal its reason.
    ensure!(!reason.is_empty() || kind == "question", "A proposal needs its reason");
    // U59: a plan-mode proposal names the mode.
    let mut mode = String::new();
    if kind == "switch-plan-mode" {
        let requested = text(input, "mode").to_owned();
        ensure!(matches!(requested.as_str(), "free" | "standard"), "A plan-mode proposal names free or standard");
        mode = requested;
    }
    // U57: the exact change the owner's card shows and Allow applies.
    let change = match kind.as_str() {
        "add-member" => {
            let members: Vec<Value> = match input.get("members").filter(|members| members.is_array()) {
                Some(members) => members.as_array().cloned().unwrap_or_default(),
                None => {
                    let member = input["member"].clone();
                    ensure!(member.is_object(), "The add-member proposal carries its member");
                    vec![member]
                }
            };
            ensure!(!members.is_empty() && members.len() <= 50, "An add-member proposal names 1 to 50 members");
            ensure!(members.iter().all(Value::is_object), "Every added member is an object");
            ensure!(Value::Array(members.clone()).to_string().len() <= 64 * 1024, "The added members have at most 64 KiB");
            json!({"members":members})
        }
        "add-to-phase" => {
            let phase = input["phase"].as_u64().context("The add-to-phase proposal names its phase")?;
            let tasks: Vec<Value> = input["tasks"].as_array().cloned().context("The add-to-phase proposal carries its tasks")?;
            ensure!(!tasks.is_empty() && tasks.len() <= 50, "An add-to-phase proposal carries 1 to 50 tasks");
            json!({"phase":phase,"tasks":tasks})
        }
        "cancel-tasks" => {
            let tasks: Vec<u64> = input["tasks"].as_array().into_iter().flatten().filter_map(Value::as_u64).collect();
            ensure!(!tasks.is_empty() && tasks.len() <= 50, "A cancel-tasks proposal names 1 to 50 tasks");
            json!({"tasks":tasks})
        }
        "discard-phase" => {
            let phase = input["phase"].as_u64().context("The discard-phase proposal names its phase")?;
            json!({"phase":phase})
        }
        "question" => question_change(input)?,
        "switch-setting" => {
            // #752: a switch card names one owner setting the lead may propose
            // and the value Allow sets.
            let setting = text(input, "setting");
            ensure!(SWITCHES.contains(&setting), "A switch proposal names one of: {}", SWITCHES.join(", "));
            let value = input["value"].as_bool().context("A switch proposal names its value, true or false")?;
            // #756: the card names the members who share a folder.
            let shared: Vec<Value> = super::broker::shared_folders(team).into_iter().map(|(folder, members)| json!({"folder":folder,"members":members})).collect();
            json!({"setting":setting,"value":value,"shared":shared})
        }
        _ => Value::Null,
    };
    ensure!(change.to_string().len() <= 64 * 1024, "A proposal's change has at most 64 KiB");
    ensure!(team.proposals.iter().filter(|proposal| proposal["state"] == "open").count() < 50, "A team keeps at most 50 open proposals");
    // C8: a swap keeps its section 5 fields with the member, the way the
    // owner's decision runs it and the desktop shows it.
    let mut member = input["member"].clone();
    if kind == "swap-member" {
        if let Some(id) = member.as_str().map(str::to_owned) {
            member = json!({"id":id});
        }
        ensure!(member["id"].as_str().is_some_and(|id| !id.is_empty()), "A swap proposal names its member");
        for key in ["agent", "model", "effort", "account", "session"] {
            if let Some(field) = input.get(key).filter(|field| !field.is_null()) {
                member[key] = field.clone();
            }
        }
    }
    ensure!(member.to_string().len() <= 8192, "A proposal's member has at most 8 KiB");
    // #740: the change is checked now, on a copy of the team, by the code
    // Allow runs: a proposal Allow could not apply (a task the plan cannot
    // parse, an unknown member or phase) is refused here, not left as a card
    // the owner cannot allow. A next goal and a question apply nothing; a
    // swap is checked by the swap itself when it runs.
    if matches!(kind.as_str(), "add-member" | "remove-member" | "add-to-phase" | "cancel-tasks" | "discard-phase" | "switch-plan-mode" | "switch-setting") {
        let mut trial = team.clone();
        let mut ignored = Vec::new();
        run_proposal(&mut trial, &kind, &member, &change, &mode, "", "", &BTreeSet::new(), &mut ignored)
            .map_err(|error| anyhow::anyhow!("Allow could not apply this proposal: {error:#}"))?;
    }
    let id = format!("p{}", team.proposals.len() + 1);
    // N3: accepting the proposal runs the change under this one request id, so
    // a retried or interrupted decision replays it instead of running twice.
    let operation_request = uuid::Uuid::new_v4().to_string();
    // U57: the owner's Allow is bound to this id and this content hash; a
    // changed proposal never applies.
    let content = json!({"kind":kind,"reason":reason,"change":change,"member":member,
        "goal":input["goal"].clone(),"mode":mode});
    let hash = format!("{:x}", Sha256::digest(content.to_string().as_bytes()));
    let proposal = json!({"id":id,"kind":kind,"by":actor.label(team),"at":now(),"state":"open","reason":reason,
        "member":member,"goal":input["goal"].clone(),"operation_request":operation_request,
        "mode":mode,"hash":hash,"change":change});
    team.proposals.push(proposal);
    events.push(Event::new("proposed", json!({"id":id,"kind":kind,"hash":hash})));
    Ok(json!({"status":"proposed","proposal":id,"hash":hash,"mode":mode,"change":change}))
}

/// F8: only the owner decides a proposal; accepting runs the change as the
/// owner would.
fn decide_proposal(team: &mut Team, input: &Value, actor: &Actor, offline: &BTreeSet<String>, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    ensure!(matches!(actor, Actor::Human), "Only you decide a proposal");
    let id = text(input, "proposal").to_owned();
    let at = team.proposals.iter().position(|proposal| proposal["id"] == id).context("Unknown proposal")?;
    ensure!(team.proposals[at]["state"] == "open", "The proposal is already decided");
    let accept = input["accept"].as_bool() == Some(true);
    let reason = clean(text(input, "reason").trim(), 2000, "A decision reason")?;
    let kind = team.proposals[at]["kind"].as_str().unwrap_or_default().to_owned();
    // U57: the owner's Allow is bound to this proposal's id and its content
    // hash; a proposal whose content changed never applies. Older proposals
    // stored without a hash stay decidable.
    if let Some(hash) = team.proposals[at]["hash"].as_str().filter(|hash| !hash.is_empty()) {
        let expected = text(input, "expected_hash");
        ensure!(
            !expected.is_empty() && expected == hash,
            "The proposal changed; reload the card and answer again with its hash"
        );
    }
    if !accept {
        team.proposals[at]["state"] = json!("rejected");
        team.proposals[at]["decided_at"] = json!(super::super::now());
        team.proposals[at]["note"] = json!(reason);
        events.push(Event::new("proposal_decided", json!({"id":id,"accept":false,"reason":reason})));
        if kind == "switch-plan-mode" {
            // U59: the reason reaches the lead with the team's feed.
            events.push(Event::new("plan_mode_denied", json!({"by":"You","proposal":id,"reason":reason})));
        }
        return Ok(json!({"status":"rejected","proposal":id}));
    }
    let member = team.proposals[at]["member"].clone();
    let change = team.proposals[at]["change"].clone();
    if kind == "question" {
        // #740: the owner's answer, an offered option or words of the owner's
        // own, reaches the lead's inbox with the question.
        let answer = clean(text(input, "answer").trim(), 4000, "An answer")?;
        ensure!(!answer.is_empty(), "Answer the question: pick an option or write your own");
        team.proposals[at]["state"] = json!("answered");
        team.proposals[at]["decided_at"] = json!(super::super::now());
        team.proposals[at]["answer"] = json!(answer);
        team.proposals[at]["note"] = json!(answer);
        events.push(Event::new("proposal_decided", json!({"id":id,"accept":true,"kind":kind,"answer":answer})));
        return Ok(json!({"status":"answered","proposal":id,"answer":answer}));
    }
    let mode = team.proposals[at]["mode"].as_str().unwrap_or_default().to_owned();
    let reason_of = team.proposals[at]["reason"].as_str().unwrap_or_default().to_owned();
    let result = run_proposal(team, &kind, &member, &change, &mode, &id, &reason_of, offline, events)?;
    team.proposals[at]["state"] = json!("accepted");
    team.proposals[at]["decided_at"] = json!(super::super::now());
    team.proposals[at]["note"] = json!(reason);
    events.push(Event::new("proposal_decided", json!({"id":id,"accept":true,"kind":kind})));
    Ok(json!({"status":"accepted","proposal":id,"result":result}))
}

/// #740: a question to the owner: its words, the options offered (at most
/// eight) and the one the lead recommends, which must be among them.
fn question_change(input: &Value) -> anyhow::Result<Value> {
    let question = clean(text(input, "question").trim(), 4000, "A question")?;
    ensure!(!question.is_empty(), "A question proposal carries its \"question\"");
    let mut options: Vec<String> = Vec::new();
    for value in input["options"].as_array().into_iter().flatten() {
        let label = clean(value.as_str().or_else(|| value["label"].as_str()).unwrap_or_default().trim(), 200, "An option")?;
        ensure!(!label.is_empty(), "Every option has words");
        ensure!(!options.contains(&label), "The option {label:?} is offered twice");
        options.push(label);
    }
    ensure!(options.len() <= 8, "A question offers at most eight options");
    let recommended = clean(text(input, "recommended").trim(), 200, "A recommended option")?;
    ensure!(recommended.is_empty() || options.contains(&recommended), "The recommended option is one of the options");
    Ok(json!({"question":question,"options":options,"recommended":recommended}))
}

/// The change Allow applies for a proposal, on `team`: run by the owner's
/// decision and, on a copy, when the lead proposes (#740).
#[allow(clippy::too_many_arguments)]
fn run_proposal(team: &mut Team, kind: &str, member: &Value, change: &Value, mode: &str, id: &str, reason: &str,
                offline: &BTreeSet<String>, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    let result = match kind {
        "add-member" => {
            let members: Vec<Value> = change["members"].as_array().cloned().unwrap_or_else(|| vec![member.clone()]);
            let mut applied = Vec::new();
            for (index, member) in members.into_iter().enumerate() {
                let mut value = member;
                value["request_id"] = json!(uuid::Uuid::new_v4().to_string());
                let _ = index;
                applied.push(apply(team, "add-member", &json!({"member":value}), &Actor::Human, offline, events)?);
            }
            json!({"added":applied.len()})
        }
        "remove-member" => {
            let id = member["id"].as_str().or(member.as_str()).unwrap_or_default().to_owned();
            apply(team, "remove-member", &json!({"member":id}), &Actor::Human, offline, events)?
        }
        "add-to-phase" => {
            let phase = change["phase"].as_u64().context("The add-to-phase proposal carries its phase")?;
            let tasks = change["tasks"].clone();
            let first = apply(team, "add-to-phase", &json!({"phase":phase,"tasks":tasks}), &Actor::Human, offline, events)?;
            // The warning's receipt names its pending id; the owner's Allow is the confirmation.
            if let Some(confirm) = first["confirm"].as_str().or_else(|| first["pending"].as_str()).filter(|id| !id.is_empty()) {
                apply(team, "add-to-phase", &json!({"phase":phase,"tasks":change["tasks"],"confirm":confirm,"confirmation":"add"}),
                      &Actor::Human, offline, events)?
            } else {
                first
            }
        }
        "cancel-tasks" => {
            let mut cancelled = Vec::new();
            for id in change["tasks"].as_array().into_iter().flatten().filter_map(Value::as_u64) {
                cancelled.push(apply(team, "cancel", &json!({"task":id}), &Actor::Human, offline, events)?);
            }
            json!({"cancelled":cancelled.len()})
        }
        "discard-phase" => {
            let phase = change["phase"].as_u64().context("The discard-phase proposal carries its phase")?;
            apply(team, "discard-phase", &json!({"phase":phase}), &Actor::Human, offline, events)?
        }
        "switch-plan-mode" => {
            team.plan_mode = if mode == "free" { PlanMode::Free } else { PlanMode::Standard };
            events.push(Event::new(
                "plan_mode_switched",
                json!({"by":"You","mode":mode,"proposal":id,"reason":reason}),
            ));
            json!({"status":"switched","plan_mode":mode})
        }
        "switch-setting" => switch_setting(team, change, id, reason, events)?,
        _ => bail!("A next-goal proposal is accepted with hgs team next-goal"),
    };
    Ok(result)
}

/// #752: the owner settings a lead may propose to switch, each a flag.
pub(crate) const SWITCHES: [&str; 1] = ["build_broker"];

/// #752: Allow of a switch card sets the one setting it names.
pub(crate) fn switch_setting(team: &mut Team, change: &Value, id: &str, reason: &str, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    let setting = change["setting"].as_str().unwrap_or_default();
    let value = change["value"].as_bool().context("The switch proposal carries its value")?;
    match setting {
        "build_broker" => team.settings.build_broker = value,
        other => bail!("{other} is not a setting a switch card sets"),
    }
    events.push(Event::new("setting_switched", json!({"by":"You","setting":setting,"value":value,"proposal":id,"reason":reason})));
    Ok(json!({"status":"switched","setting":setting,"value":value}))
}

/// #630 N7: the stage runs given to `member` go back to Waiting when its
/// member is removed or paused; the lead hears each as stalled.
fn stall_stages_of(team: &mut Team, member: &str, reason: &str, events: &mut Vec<Event>) {
    let mut stalled = Vec::new();
    for task in &team.tasks {
        let Some(run) = task.pipeline_run.as_ref().and_then(|pipeline| pipeline.open_run()) else { continue };
        // A command or land stage names Zerus, never a member.
        if run.member != member || run.member.is_empty() {
            continue;
        }
        stalled.push((task.id, run.id.clone()));
    }
    for (task, run) in stalled {
        super::pipeline::stall_stage(team, task, &run, reason, events);
    }
}

fn decide(team: &mut Team, input: &Value, actor: &Actor, accept: bool, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    let id = task_id(input)?;
    let human = matches!(actor, Actor::Human);
    // Spec 5.6: a sub-lead decides the reports of its own reports.
    let assignee = team.task(id).map(|task| task.assignee.clone()).unwrap_or_default();
    ensure!(actor.reviews(team, &assignee), "Only the lead or you decide on a report");
    // Deciding is part of the running state machine; a finished or stopped team
    // keeps its reports as they are.
    ensure!(matches!(team.status, TeamStatus::Running | TeamStatus::Paused), "The team is not running");
    let reason = clean(text(input, "reason").trim(), 4000, "A reason")?;
    ensure!(accept || !reason.is_empty(), "A rejection needs a reason");
    // A5: a rejection may keep the task for a member that is not ready for it.
    let hold = !accept && input["hold"].as_bool() == Some(true);
    ensure!(!(hold && input["final"].as_bool() == Some(true)), "A final rejection cannot be held");
    let label = actor.label(team);
    let worked_ahead = team.task(id).is_some_and(|task| task.attempts.iter().rev().find(|a| a.state == AttemptState::Reported).is_some_and(|a| a.worked_ahead));
    let ahead_phase = team.task(id).map(|task| task.phase).unwrap_or_default();
    if accept && worked_ahead {
        ensure!(
            team.phases.iter().find(|found| found.n == ahead_phase).is_none_or(|found| matches!(found.state, PhaseState::Approved | PhaseState::Queued)),
            "This result worked ahead of phase {ahead_phase}; wait for the owner's approval of that phase"
        );
    }
    let lead_id = team.lead.clone();
    // U62 (#760): with the merge queue on, an accept queues the author's commit
    // for the lead branch; the queue's own accept (merged) decides it.
    let merged = input["merged"].as_bool() == Some(true);
    let queue = if accept && !merged { super::merge_queue::entry(team, id, &label, human, &reason) } else { None };
    let task = team.task_mut(id).context("Unknown task")?;
    ensure!(!(task.decided_by == Some(Decider::Human) && task.status.finished()), "You already decided task #{id}");
    if task.merge.as_ref().is_some_and(|entry| entry.state == "testing") && !merged {
        bail!("Task #{id} is merged into the lead branch and its tests run; wait for the merge queue's result");
    }
    // #538 N1: accepting over an open stage names why, as skip-stage does.
    ensure!(!accept || !task.status.in_stage() || !reason.is_empty(), "Accepting task #{id} in a stage needs a reason");
    if human {
        ensure!(!matches!(task.status, TaskStatus::Draft | TaskStatus::Cancelled), "Task #{id} cannot be decided now");
    } else {
        // U36: the lead may accept or reject at any stage of the pipeline.
        ensure!(task.status.reported(), "Task #{id} has no report to decide");
        ensure!(
            !accept || actor.is_lead_of(&lead_id) || !super::pipeline::lead_accepts_only(task),
            "Task #{id}'s pipeline is accepted by the lead"
        );
    }
    if let Some(entry) = queue {
        if task.merge.is_none() {
            let commit = entry.commit.clone();
            task.merge = Some(entry);
            events.push(Event::new("merge_queued", json!({"by":label,"task":id,"commit":commit})));
        }
        return Ok(json!({"status":"merge_queued","task":id}));
    }
    // A reject or a decision that skips the queue takes the task out of it.
    task.merge = None;
    let n = task.attempts.iter().rev().find(|a| a.state == AttemptState::Reported).map(|a| a.n);
    // N15: the lead's accept filed this report's findings already; the
    // owner's accept over it does not file them again.
    let filed_before = task.status == TaskStatus::Accepted && task.decided_by == Some(Decider::Lead);
    mark_decided(task);
    if accept {
        revoke(task, "accepted without this attempt");
        task.status = TaskStatus::Accepted;
    } else {
        if let Some(attempt) = task.attempts.iter_mut().rev().find(|a| a.state == AttemptState::Reported) {
            attempt.state = AttemptState::Rejected;
        }
        revoke(task, "rejected");
        task.status = if input["final"].as_bool() == Some(true) {
            TaskStatus::Rejected
        } else if task.subteam.is_some() {
            // N28: a rejected sub-team report goes back to the sub-team, which
            // reworks it; until its next finish the parent task belongs to it
            // and is never handed to the sub-lead as a fresh attempt.
            TaskStatus::InProgress
        } else {
            TaskStatus::Pending
        };
        if hold {
            task.hold = Some(Hold { by: label.clone(), at: now(), reason: reason.clone() });
        }
    }
    task.decided_by = Some(if human { Decider::Human } else { Decider::Lead });
    task.decision_reason = reason.clone();
    let title = task.title.clone();
    // U59: a build task's decision moves the header: accepted it is the build's
    // proof and closes a pending close; rejected it fails the pending close and
    // the phase stays open.
    let phase_build = team.task(id).is_some_and(|task| task.phase_build);
    if accept && phase_build {
        team.last_build_at = Some(now());
        team.free_accepted_since_build = 0;
        if let Some(n) = team.free_close_pending.take() {
            team.plan_mode = PlanMode::Standard;
            events.push(Event::new("free_phase_closed", json!({"by":label,"phase":n,"build":true})));
        }
    } else if accept && team.plan_mode == PlanMode::Free {
        team.free_accepted_since_build += 1;
    } else if !accept && phase_build {
        if let Some(n) = team.free_close_pending.take() {
            events.push(Event::new("free_phase_build_failed", json!({"phase":n,"reason":reason})));
        }
    }
    // D2: accepting the report files its findings; later ones are deferred and
    // the owner hears about them once.
    if accept && !filed_before {
        let structured = team
            .task(id)
            .and_then(|task| task.attempts.iter().find(|attempt| Some(attempt.n) == n))
            .and_then(|attempt| attempt.report.as_ref())
            .and_then(|report| report.structured.clone());
        if let Some(report) = structured {
            let (filed, deferred) = file_findings(team, id, &report, &label)?;
            if filed > 0 {
                events.push(Event::new("findings_filed", json!({"task":id,"count":filed,"deferred":deferred})));
                if deferred > 0 {
                    events.push(Event::new("findings_deferred", json!({"count":deferred,
                        "notice":format!("{deferred} findings deferred")})));
                }
            }
        }
    }
    let manual = accept && human && input["manual"].as_bool() == Some(true);
    events.push(Event::new(
        if accept { "accepted" } else { "rejected" },
        json!({"by":label,"task":id,"title":title,"attempt":n,"reason":reason,"manual":manual}),
    ));
    if hold {
        events.push(Event::new("held", json!({"by":label,"task":id,"reason":reason})));
    }
    let status = serde_json::to_value(team.task(id).unwrap().status)?;
    Ok(json!({"status":status,"task":id}))
}

fn reassign(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    let id = task_id(input)?;
    ensure!(matches!(actor, Actor::Human) || actor.is_lead(team), "Only the lead or you reassign tasks");
    // 9.7: a task with a sub-team says what becomes of it (C5: checked here,
    // acted on after this change is stored).
    subteam_stop_choice(input, team.task(id).context("Unknown task")?)?;
    let target = text(input, "member");
    ensure!(team.member(target).is_some_and(|m| m.state != MemberState::Removed), "Unknown member {target}");
    let label = actor.label(team);
    let role = team.role_of(target);
    let task = team.task_mut(id).context("Unknown task")?;
    ensure!(!task.status.finished(), "Task #{id} is already decided");
    let revoked = revoke(task, "reassigned");
    task.assignee = target.to_owned();
    task.pinned = true;
    // U45: a task for a role now has its member; a waiting step starts over.
    task.role.clear();
    task.waits = None;
    task.prepare.clear();
    if task.status != TaskStatus::Draft {
        task.status = TaskStatus::Pending;
    }
    events.push(Event::new("reassigned", json!({"by":label,"task":id,"to":role,"revoked_attempt":revoked})));
    Ok(json!({"status":"reassigned","task":id,"revoked_attempt":revoked}))
}

/// U58: moves a task that has not started (no delivered attempt) to another
/// phase, keeping its id; its deps must not stay in a later phase than the
/// target. A task that has started, or is decided, never moves.
fn move_task(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    ensure!(matches!(actor, Actor::Human) || actor.is_lead(team), "Only the lead or you move a task");
    ensure!(
        matches!(team.status, TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused),
        "Only a planning, running or paused team moves a task"
    );
    ensure_draft_revision(team, input)?;
    let id = task_id(input)?;
    let n = phase_number(input)?;
    let task = team.task(id).context("Unknown task")?;
    ensure!(!task.status.finished(), "Task #{id} is already decided");
    ensure!(!task_started(task), "Task #{id} has started; only a task with no delivered attempt moves");
    let from = task.phase;
    ensure!(from != n, "Task #{id} is already in phase {n}");
    // Phase 1 is the plan's own phase and has no record until work is added to
    // it; every later phase needs its record.
    let at = team.phases.iter().position(|phase| phase.n == n);
    if n > 1 {
        ensure!(at.is_some(), "Unknown phase {n}");
        ensure!(team.phases[at.unwrap()].state != PhaseState::Discarded, "Phase {n} is discarded");
    }
    // The task's deps keep their phases: a dep in a later phase than the target
    // would make the task wait for work that starts after it.
    let order = |phase: u32| team.phases.iter().find(|found| found.n == phase).map_or(phase, |found| found.order());
    let target = order(n);
    for dep in &task.deps {
        let Some(found) = team.task(dep.task) else { continue };
        ensure!(
            order(found.phase) <= target,
            "Task #{id} depends on #{}, which stays in a later phase ({}); move it first",
            found.id,
            found.phase
        );
    }
    // U58: a task moved into a draft phase (or the plan) is a Draft again; one
    // moved into an approved phase is Pending.
    let draft = match at {
        Some(at) => team.phases[at].state.is_draft(),
        None => team.status == TeamStatus::Planning,
    };
    let mut proposed = team.clone();
    if let Some(old) = proposed.phases.iter_mut().find(|phase| phase.n == from) {
        old.tasks.retain(|task| *task != id);
    }
    let moved = proposed.task_mut(id).context("Unknown task")?;
    moved.phase = n;
    moved.status = if draft { TaskStatus::Draft } else { TaskStatus::Pending };
    let by = actor.label(team);
    if let Some(at) = at {
        proposed.phases[at].tasks.push(id);
        proposed.phases[at].tasks.sort_unstable();
        record_change(&mut proposed.phases[at], json!({"kind":"moved","task":id,"from":from,"to":n}));
    }
    let validation = plan::validate(&proposed, &BTreeSet::new());
    let errors: Vec<plan::Finding> = validation
        .errors
        .iter()
        .filter(|finding| finding.task == Some(id) || (finding.kind == "cycle" && finding.tasks.contains(&id)))
        .cloned()
        .collect();
    ensure!(
        errors.is_empty(),
        "Fix {} plan error(s) to move it: {}",
        errors.len(),
        errors_text(&errors)
    );
    *team = proposed;
    events.push(Event::new("task_moved", json!({"by":by,"task":id,"from":from,"to":n})));
    Ok(json!({"status":"moved","task":id,"from":from,"to":n}))
}

fn retry(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    let id = task_id(input)?;
    let assignee = team.task(id).map(|task| task.assignee.clone()).unwrap_or_default();
    ensure!(actor.reviews(team, &assignee), "Only the lead or you retry tasks");
    let label = actor.label(team);
    let task = team.task_mut(id).context("Unknown task")?;
    ensure!(!task.status.finished() && task.status != TaskStatus::Draft, "Task #{id} cannot be retried now");
    let revoked = revoke(task, "retried");
    task.status = TaskStatus::Pending;
    // Spec 0: the lead or the owner retries a refused or unconfirmed step.
    task.waits = None;
    task.prepare.clear();
    events.push(Event::new("retried", json!({"by":label,"task":id,"revoked_attempt":revoked})));
    Ok(json!({"status":"pending","task":id,"revoked_attempt":revoked}))
}

fn take_over(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    let id = task_id(input)?;
    ensure!(matches!(actor, Actor::Human), "Only you can take over a task");
    let task = team.task_mut(id).context("Unknown task")?;
    ensure!(!task.status.finished() && task.status != TaskStatus::Draft, "Task #{id} cannot be taken over now");
    let revoked = revoke(task, "taken over");
    task.assignee.clear();
    task.status = TaskStatus::InProgress;
    events.push(Event::new("taken_over", json!({"by":"You","task":id,"revoked_attempt":revoked})));
    Ok(json!({"status":"in_progress","task":id,"revoked_attempt":revoked}))
}

fn cancel(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    let id = task_id(input)?;
    ensure!(matches!(actor, Actor::Human) || actor.is_lead(team), "Only the lead or you cancel tasks");
    subteam_stop_choice(input, team.task(id).context("Unknown task")?)?;
    let label = actor.label(team);
    let task = team.task_mut(id).context("Unknown task")?;
    ensure!(!task.status.finished(), "Task #{id} is already decided");
    let revoked = revoke(task, "cancelled");
    let phase = task.phase;
    let phase_build = task.phase_build;
    task.status = TaskStatus::Cancelled;
    // #759: the tasks it held start once their other dependencies are done;
    // the event and the lead's inbox name them for a re-plan.
    let released = super::plan::waiting_on(team, id);
    events.push(Event::new("cancelled", json!({"by":label,"task":id,"revoked_attempt":revoked,"released":released})));
    // U59: a cancelled build fails a pending close; the phase stays open.
    if phase_build {
        if let Some(n) = team.free_close_pending.take() {
            events.push(Event::new("free_phase_build_failed", json!({"phase":n,"reason":"the build was cancelled"})));
        }
    }
    discard_empty_draft(team, phase, &label);
    Ok(json!({"status":"cancelled","task":id,"revoked_attempt":revoked,"released":released}))
}

/// A human settles an unconfirmed hand-off after checking the session: it
/// arrived, or it did not (the task returns to Pending; nothing is re-sent).
fn check(team: &mut Team, input: &Value, actor: &Actor, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    let id = task_id(input)?;
    ensure!(matches!(actor, Actor::Human), "Only you settle an unconfirmed hand-off");
    let arrived = match text(input, "outcome") {
        "delivered" => true,
        "not_delivered" => false,
        _ => bail!("The outcome is delivered or not_delivered"),
    };
    let task = team.task_mut(id).context("Unknown task")?;
    let attempt = task.open_attempt_mut().filter(|a| a.state == AttemptState::Unconfirmed).context("Task has no unconfirmed hand-off")?;
    let n = attempt.n;
    if arrived {
        attempt.state = AttemptState::Delivered;
        attempt.delivered_at = Some(now());
        task.status = TaskStatus::InProgress;
    } else {
        attempt.state = AttemptState::Refused;
        attempt.error = "not delivered (checked)".into();
        task.status = TaskStatus::Pending;
        // Nothing reached the member: its questions close with the attempt (N5).
        close_questions(task);
    }
    events.push(Event::new("checked", json!({"by":"You","task":id,"attempt":n,"delivered":arrived})));
    Ok(json!({"status":if arrived {"in_progress"} else {"pending"},"task":id}))
}

/// Applies one human, lead or member request. The dispatcher records its own
/// changes directly.
pub(crate) fn apply(team: &mut Team, op: &str, input: &Value, actor: &Actor, offline: &BTreeSet<String>, events: &mut Vec<Event>) -> anyhow::Result<Value> {
    // N14: the word is checked again on the stored team inside the change. Two
    // requests sent at once both pass the CLI's pre-check on their own read;
    // the store serializes them, and only the second sees the first's record.
    if let Some(event) = input["owner_word_event"].as_i64() {
        let allowed = input["owner_word_count"].as_u64().unwrap_or(1).max(1) as usize;
        let change = input["owner_word_change"].as_str().unwrap_or("this change");
        // U58: one sentence naming a member and its tasks pays for both, the
        // member first: the roster add, then its tasks' phase request.
        let its_tasks = op == "add-to-phase"
            && change == "the request to add to a running phase"
            && team.owner_word_allows_its_tasks(event);
        ensure!(
            its_tasks || team.owner_word_allowed_again(event, allowed, change),
            "That word was already used for {}; ask the owner again",
            team.owner_word_used(event).unwrap_or_default()
        );
    }
    // U29: an expired addition waits for nobody: it leaves on the team's next
    // change, so a stored one is never listed or confirmed again.
    team.pending_additions.retain(|pending| addition_fresh(pending, now()));
    // F285: a stored team with discarded drafts is compacted on its next
    // change - the dead records leave and the drafts close the gaps - so the
    // change that follows sees live numbers only.
    if team.phases.iter().any(|phase| phase.state == PhaseState::Discarded) {
        team.phases.retain(|phase| phase.state != PhaseState::Discarded);
        let by = actor.label(team);
        renumbered(team, &by, events);
    }
    let human_or_lead = matches!(actor, Actor::Human) || actor.is_lead(team);
    let was_decided = all_decided(team);
    let done_before = phases_done(team);
    let result = match op {
        "plan" => check_land_choices(team, input).and_then(|()| plan(team, input, actor, events)),
        "extend" => {
            // C15: an urgent phase comes only from the finding action or the
            // owner's intake key.
            let mut plain = input.clone();
            if let Some(object) = plain.as_object_mut() {
                object.remove("urgent");
            }
            check_land_choices(team, &plain).and_then(|()| extend(team, &plain, actor, offline, events))
        }
        "intake" => intake_phase(team, input, actor, offline, events),
        "intake-key" => intake_key(team, input, actor, events),
        "edit-draft" => check_land_choices(team, input).and_then(|()| edit_draft(team, input, actor, offline, events)),
        "reports-to" | "sub-lead" => hierarchy(team, op, input, actor, events),
        "move-phase" => move_phase(team, input, actor, events),
        "add-to-phase" => add_to_phase(team, input, actor, offline, events),
        "approve-phase" => decide_phase(team, input, actor, true, offline, events),
        "discard-phase" => decide_phase(team, input, actor, false, offline, events),
        "return-to-draft" => return_to_draft(team, input, actor, events),
        "reject-phase" | "undo-reject-phase" | "rework-phase" => send_back(team, op, input, actor, events),
        "settings" => settings(team, input, actor, events),
        "finding" => finding(team, input, actor, offline, events),
        "draft" => draft(team, input, actor, offline, events),
        "propose" => propose(team, input, actor, events),
        "decide-proposal" => decide_proposal(team, input, actor, offline, events),
        "finish" => finish(team, input, actor, events),
        "approve" => {
            // B5: a wider permissions profile needs the owner's word.
            ensure!(
                team.profile_widening.is_empty()
                    || (matches!(actor, Actor::Human) && input["confirm_widening"].as_bool() == Some(true)),
                "The owner confirms the wider permissions first: approve with \"confirm_widening\": true"
            );
            approve(team, input, actor, offline, events)
        }
        "discard" => {
            ensure!(human_or_lead, "Only the lead or you discard a plan");
            ensure!(team.status == TeamStatus::Planning, "Only a plan in Planning can be discarded");
            Ok(set_status(team, TeamStatus::Stopped, "discarded", actor, events))
        }
        "pause" => {
            ensure!(human_or_lead, "Only the lead or you pause the team");
            ensure!(team.status == TeamStatus::Running, "Only a running team can pause");
            Ok(set_status(team, TeamStatus::Paused, "paused", actor, events))
        }
        "resume" => {
            ensure!(human_or_lead, "Only the lead or you resume the team");
            ensure!(team.status == TeamStatus::Paused, "Only a paused team can resume");
            Ok(set_status(team, TeamStatus::Running, "resumed", actor, events))
        }
        "stop" => {
            ensure!(matches!(actor, Actor::Human), "Only you stop the team");
            ensure!(matches!(team.status, TeamStatus::Running | TeamStatus::Paused | TeamStatus::Planning), "The team is not running");
            for task in &mut team.tasks {
                if !task.status.finished() {
                    revoke(task, "team stopped");
                    task.status = TaskStatus::Cancelled;
                }
            }
            let by = actor.label(team);
            discard_draft_records(team, &by);
            Ok(set_status(team, TeamStatus::Stopped, "stopped", actor, events))
        }
        "archive" => {
            ensure!(matches!(actor, Actor::Human), "Only you archive the team");
            ensure!(matches!(team.status, TeamStatus::Done | TeamStatus::Stopped), "Stop the team before archiving it");
            Ok(set_status(team, TeamStatus::Archived, "archived", actor, events))
        }
        "report" | "use-last-reply" => {
            ensure!(op == "report" || matches!(actor, Actor::Human), "Only you use a last reply as the report");
            ensure!(matches!(team.status, TeamStatus::Running | TeamStatus::Paused), "The team is not running");
            report(team, input, actor, events)
        }
        // A2: the dispatcher's capture of a member's last reply; no command
        // reaches this operation.
        "capture-report" => {
            ensure!(matches!(actor, Actor::Member(_)), "Zerus captures a member's reply");
            ensure!(matches!(team.status, TeamStatus::Running | TeamStatus::Paused), "The team is not running");
            report_with(team, input, actor, true, events)
        }
        "ask" => ask(team, input, actor, events),
        "reply" => reply(team, input, actor, events),
        "hold" => hold(team, input, actor, events),
        "release" => release(team, input, actor, events),
        "accept" => decide(team, input, actor, true, events),
        "reject" => decide(team, input, actor, false, events),
        "reassign" => reassign(team, input, actor, events),
        // U58: move a task that has not started to another phase, keeping its id.
        "move-task" => move_task(team, input, actor, events),
        // U36: the lead's and the owner's pipeline overrides.
        "skip-stage" => super::pipeline::skip_stage(team, input, actor, events),
        "send-back" => super::pipeline::send_back(team, input, actor, events),
        "retry" => retry(team, input, actor, events),
        "take-over" => take_over(team, input, actor, events),
        "cancel" => cancel(team, input, actor, events),
        "check" => check(team, input, actor, events),
        "expect" => expect(team, input, actor, events),
        "swap-member" => swap_member(team, input, actor, events),
        // U59: the free phase's ordinary build task; accepting it is the
        // build's proof and moves the header's last build time.
        "integrate-build" => {
            ensure!(matches!(actor, Actor::Human) || actor.is_lead(team), "Only the lead or you start the phase build");
            ensure!(matches!(team.status, TeamStatus::Running | TeamStatus::Paused), "The team is not running");
            ensure!(team.plan_mode == PlanMode::Free, "The build task belongs to a free phase");
            let n = open_phase(team);
            let id = add_build_task(team, n, &actor.label(team), events)?;
            Ok(json!({"status":"build_added","phase":n,"task":id}))
        }
        // U59: the owner closes a free phase explicitly; the team continues in
        // standard mode. Integration and a build are ordinary lead tasks, so the
        // close creates none and is never a barrier.
        "full-tests" => {
            // U62: the owner's one-time permission for one full run, bound to
            // the release it is for; the run that uses it clears it.
            ensure!(matches!(actor, Actor::Human), "Only you grant a full test run");
            let release = text(input, "release").trim().to_owned();
            if input["revoke"].as_bool() == Some(true) || release.is_empty() {
                team.full_tests_grant = None;
                events.push(Event::new("full_tests_grant_revoked", json!({"by":"You"})));
                return Ok(json!({"status":"revoked"}));
            }
            ensure!(release.chars().count() <= 120, "A release is at most 120 characters");
            team.full_tests_grant = Some(super::model::FullTestsGrant { release: release.clone(), at: now(), by: "You".into() });
            events.push(Event::new("full_tests_granted", json!({"release":release})));
            Ok(json!({"status":"granted","release":release}))
        }
        "start-ahead" => {
            // U61: the lead lets idle members start an auto-accepted draft now;
            // its first hand-out accepts the phase, so it is undoable only
            // before any task is handed out.
            ensure!(human_or_lead, "Only the lead or you start a phase ahead");
            ensure!(
                team.settings.auto_accept,
                "Start-ahead needs Auto-accept: the phase is accepted at its first hand-out"
            );
            let n = phase_number(input)?;
            let undo = input["undo"].as_bool() == Some(true);
            let at = team
                .phases
                .iter()
                .position(|phase| phase.n == n)
                .with_context(|| format!("Unknown phase {n}"))?;
            let record = &team.phases[at];
            ensure!(record.state.is_draft(), "Phase {n} is not a draft");
            ensure!(record.approval == Approval::Auto, "Phase {n} is a review draft: the owner approves it");
            ensure!(matches!(team.status, TeamStatus::Running | TeamStatus::Paused), "Only a running or paused team starts a phase ahead");
            let handed = record.tasks.iter().any(|id| team.task(*id).is_some_and(|task| !task.attempts.is_empty()));
            ensure!(!handed, "Phase {n} has a handed-out task: it is accepted and stays");
            team.phases[at].start_ahead = !undo;
            events.push(Event::new(
                if undo { "phase_start_ahead_undone" } else { "phase_started_ahead" },
                json!({"phase":n,"by":actor.label(team)}),
            ));
            Ok(json!({"status":if undo {"undone"} else {"started"},"phase":n,"start_ahead":!undo}))
        }
        "close-phase" => {
            ensure!(matches!(actor, Actor::Human), "Only you close a free phase");
            ensure!(team.plan_mode == PlanMode::Free, "The team is not planning in free mode");
            ensure!(matches!(team.status, TeamStatus::Running | TeamStatus::Paused), "The team is not running");
            let reason = clean(text(input, "reason").trim(), 2000, "A close reason")?;
            let n = open_phase(team);
            // (b) a phase whose work still runs is not closed; the running ids
            // are named so the owner or the lead knows what waits.
            let running: Vec<u32> = team
                .tasks
                .iter()
                .filter(|task| task.phase == n && !task.status.finished())
                .map(|task| task.id)
                .collect();
            ensure!(
                running.is_empty(),
                "Phase {n} still has {} running; close it when they are decided",
                running.iter().map(|id| format!("#{id}")).collect::<Vec<_>>().join(", ")
            );
            // (a) the close may ask for the integration and build first: when
            // tasks were accepted since the last build, an ordinary lead build
            // task is added and the close waits for its acceptance. With nothing
            // accepted since the last build the close is immediate.
            if input["integrate"].as_bool() == Some(true) && team.free_accepted_since_build > 0 {
                let build = add_build_task(team, n, "You", events)?;
                team.free_close_pending = Some(n);
                events.push(Event::new("free_phase_close_pending", json!({"phase":n,"build":build})));
                return Ok(json!({"status":"close_pending","phase":n,"build":build}));
            }
            team.plan_mode = PlanMode::Standard;
            team.free_close_pending = None;
            events.push(Event::new("free_phase_closed", json!({"by":"You","phase":n,"reason":reason})));
            Ok(json!({"status":"closed","phase":n,"plan_mode":"standard","build_at":team.last_build_at}))
        }
        "relaunch" => {
            // U54: the owner relaunches directly; the lead on its verified word.
            // U56: on the owner's setting, the lead also relaunches a member
            // Zerus marks stuck.
            let member_id = text(input, "member").to_owned();
            let stuck = team.member(&member_id).is_some_and(|member| member.stuck.is_some());
            ensure!(
                matches!(actor, Actor::Human)
                    || (actor.is_lead(team)
                        && (input["roster_word"].as_bool() == Some(true) || (team.settings.lead_restarts_stuck && stuck))),
                "Only you relaunch a member; the lead needs your word, or the owner's setting lead_restarts_stuck for a stuck member (hgs team settings with {{\"lead_restarts_stuck\":true}})"
            );
            let id = member_id;
            let by = actor.label(team);
            let member = team.members.iter_mut().find(|m| m.id == id).context("Unknown member")?;
            if member.stuck.is_some() {
                // U56/#728: the real route for a stuck member: the dispatcher
                // interrupts its session and resumes it in its conversation;
                // the lead needs no word when the owner set lead_restarts_stuck,
                // and the owner has the same route.
                member.restart_pending = true;
                member.error.clear();
                member.launch_id.clear();
                events.push(Event::new("member_restart_requested", json!({"by":by,"member":id})));
                return Ok(json!({"status":"relaunching","member":id}));
            }
            ensure!(member.session.is_none() && !member.error.is_empty(), "This member's session did not fail to start");
            member.error.clear();
            member.launch_id.clear();
            events.push(Event::new("relaunch", json!({"by":"You","member":id})));
            Ok(json!({"status":"relaunch","member":id}))
        }
        "pause-member" | "resume-member" => {
            ensure!(human_or_lead, "Only the lead or you pause a member");
            let pause = op == "pause-member";
            let id = text(input, "member").to_owned();
            ensure!(id != team.lead, "Pause the team to pause its lead");
            let label = actor.label(team);
            let member = team.members.iter_mut().find(|m| m.id == id).context("Unknown member")?;
            ensure!(member.state != MemberState::Removed, "This member was removed");
            // A paused member gets no new task; its current turn finishes.
            member.state = match (pause, &member.session) {
                (true, _) => MemberState::Paused,
                (false, Some(_)) => MemberState::Active,
                (false, None) => MemberState::NotCreated,
            };
            let role = team.role_of(&id);
            if pause {
                // #630 N7: a paused member's stage goes back to the pool.
                stall_stages_of(team, &id, &format!("{} was paused", team.name_of(&id)), events);
            }
            events.push(Event::new(if pause { "member_paused" } else { "member_resumed" }, json!({"by":label,"member":role})));
            Ok(json!({"status":if pause {"paused"} else {"resumed"},"member":id}))
        }
        "add-member" => {
            // A running team grows: the new member starts with its first task,
            // or adopts the existing session the lead named (F8).
            ensure!(human_or_lead, "Only the lead or you add a member");
            ensure!(matches!(team.status, TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused),
                "Only a planning, running or paused team takes new members");
            let mut member = parse_member(&input["member"])?;
            // F1.2: the agent's default folder, shared when the project
            // forbids worktrees.
            fill_folder(team, &mut member, input["member"].get("folder").is_none());
            ensure!(team.members.iter().all(|m| m.id != member.id), "Member id {} is used", member.id);
            let active = team.members.iter().filter(|m| m.state != MemberState::Removed).count();
            let id = member.id.clone();
            let role = member.role.clone();
            // U32/N7: the lead adds on the owner's verified word even at a full
            // cap: the cap grows by one for each add, and a count word ("add two
            // backends") pays for that many adds, within the tree's session
            // budget, in this same change.
            let on_word = input["roster_word"].as_bool() == Some(true)
                && matches!(actor, Actor::Member(lead) if *lead == team.lead)
                && input["owner_word_event"].is_i64();
            let quote = input["owner_word_checked"]["text"].as_str().unwrap_or_default();
            if active >= team.settings.max_members {
                ensure!(on_word, "The team already has its {} members", team.settings.max_members);
                let old = team.settings.max_members;
                let new = old.saturating_add(1);
                let budget = input["tree_sessions"].as_u64().map(|budget| budget as usize).unwrap_or(team.settings.tree_sessions as usize);
                ensure!(new <= budget, "The whole tree runs at most {budget} sessions; the team keeps {old} members");
                team.settings.max_members = new;
                events.push(Event::new(
                    "max_members_raised",
                    json!({"old":old,"new":new,"member":role,"by":"Lead on your word","owner_word":quote}),
                ));
            }
            if on_word {
                // U32: the roster says how the member joined.
                member.added = Some(json!({"by":"Lead on your word","owner_word":quote,"at":now()}));
            }
            let profile = member.permissions_profile.clone();
            let adopted = member.session.as_ref().map(|session| session.name.clone());
            team.members.push(member);
            // Spec 5.6: "Reports to" names the lead or a sub-lead.
            if let Err(error) = check_hierarchy(team) {
                team.members.retain(|member| member.id != id);
                return Err(error);
            }
            // N39/F1.3: a new member's folder is checked against the team's
            // policy here, as at plan and approve: a mismatch is an error, and
            // the member is not filed to fail at its launch.
            let folder_errors: Vec<plan::Finding> = plan::validate(team, offline)
                .errors
                .into_iter()
                .filter(|finding| finding.kind == "folder" && finding.member.as_deref() == Some(id.as_str()))
                .collect();
            if !folder_errors.is_empty() {
                team.members.retain(|member| member.id != id);
                bail!("Fix {} folder error(s) to add the member: {}", folder_errors.len(), errors_text(&folder_errors));
            }
            // B5: a profile wider than the role's default waits for the owner.
            if profile_rank(&profile) > profile_rank("write") {
                team.profile_widening.push(json!({"member":id,"from":"write","to":profile}));
            }
            let by = if on_word { format!("{} on your word", actor.label(team)) } else { actor.label(team) };
            events.push(Event::new("member_added", json!({"by":by,"member":role,"owner_word":quote})));
            // F8: the existing session is adopted, not launched (L2).
            if let Some(session) = adopted {
                events.push(Event::new("member_adopted", json!({"member":role,"session":session})));
            }
            Ok(json!({"status":"added","member":id,"by":by}))
        }
        "scale" => {
            // U49: several members of one role at once, as add-member adds one;
            // all of them or none, within the roster the team already has.
            ensure!(human_or_lead, "Only the lead or you scale the team");
            ensure!(
                matches!(team.status, TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused),
                "Only a planning, running or paused team takes new members"
            );
            let role = clean(text(input, "role").trim(), 60, "A role")?;
            ensure!(!role.is_empty(), "scale names the role to add");
            let agent = clean(text(input, "agent").trim(), 40, "An agent")?;
            ensure!(!agent.is_empty(), "scale names the agent to add");
            let count = input["count"].as_u64().context("scale names how many members to add")?;
            ensure!((1..=32).contains(&count), "scale adds 1 to 32 members");
            let count = count as usize;
            let active = team.members.iter().filter(|member| member.state != MemberState::Removed).count();
            ensure!(
                active + count <= team.settings.max_members,
                "The team has {active} of {} members; adding {count} does not fit",
                team.settings.max_members
            );
            let on_word = input["roster_word"].as_bool() == Some(true) && matches!(actor, Actor::Member(lead) if *lead == team.lead);
            let quote = input["owner_word_checked"]["text"].as_str().unwrap_or_default();
            let by = if on_word { format!("{} on your word", actor.label(team)) } else { actor.label(team) };
            let mut ids: Vec<String> = Vec::new();
            let mut added: Vec<Member> = Vec::new();
            for _ in 0..count {
                let id = scale_id(team, &ids, &role);
                let mut value = json!({"id":id,"role":role,"agent":agent});
                for field in ["model", "effort", "account"] {
                    let given = text(input, field).trim();
                    if !given.is_empty() {
                        value[field] = json!(given);
                    }
                }
                let mut member = parse_member(&value)?;
                fill_folder(team, &mut member, true);
                if on_word {
                    // U32: the roster says how the member joined.
                    member.added = Some(json!({"by":"Lead on your word","owner_word":quote,"at":now()}));
                }
                ids.push(member.id.clone());
                added.push(member);
            }
            let roles: Vec<String> = added.iter().map(|member| member.role.clone()).collect();
            for member in added {
                team.members.push(member);
            }
            // N39/F1.3: the folders are checked as a whole; a mismatch refuses
            // the scale and no member joins.
            if let Err(error) = check_hierarchy(team) {
                team.members.retain(|member| !ids.contains(&member.id));
                return Err(error);
            }
            let folder_errors: Vec<plan::Finding> = plan::validate(team, offline)
                .errors
                .into_iter()
                .filter(|finding| finding.kind == "folder" && finding.member.as_deref().is_some_and(|id| ids.iter().any(|new| new == id)))
                .collect();
            if !folder_errors.is_empty() {
                team.members.retain(|member| !ids.contains(&member.id));
                bail!("Fix {} folder error(s) to scale the team: {}", folder_errors.len(), errors_text(&folder_errors));
            }
            for (id, role) in ids.iter().zip(roles) {
                events.push(Event::new("member_added", json!({"by":by,"member":role,"id":id,"owner_word":quote,"scaled":true})));
            }
            Ok(json!({"status":"scaled","members":ids,"by":by,"count":count}))
        }
        "remove-member" => {
            // U26: the lead removes only on the owner's verified word.
            ensure!(
                matches!(actor, Actor::Human)
                    || (actor.is_lead(team) && input["roster_word"].as_bool() == Some(true)),
                "Only you remove a member; the lead needs your word"
            );
            let id = text(input, "member").to_owned();
            ensure!(id != team.lead, "The lead cannot be removed");
            let reports: Vec<String> = team.reports_of(&id).iter().map(|member| team.name_of(&member.id)).collect();
            ensure!(reports.is_empty(), "Move its reports to another lead first: {}", reports.join(", "));
            // N10: a member whose only open tasks are drafts can still go when
            // the owner names the member that takes them (spec 4).
            let open: Vec<u32> = team.tasks.iter().filter(|t| t.assignee == id && !t.status.finished()).map(|t| t.id).collect();
            if !open.is_empty() {
                let target = text(input, "reassign_drafts_to").trim().to_owned();
                ensure!(
                    !target.is_empty(),
                    "Reassign or cancel its open tasks first: {} (or give \"reassign_drafts_to\" for its drafts)",
                    open.iter().map(|id| format!("#{id}")).collect::<Vec<_>>().join(", ")
                );
                ensure!(target != id, "A member cannot take over its own drafts");
                ensure!(
                    team.member(&target).is_some_and(|member| member.id != team.lead && member.state != MemberState::Removed),
                    "Unknown member {target}"
                );
                let drafts: Vec<u32> =
                    open.iter().copied().filter(|task| team.task(*task).is_some_and(|task| task.status == TaskStatus::Draft)).collect();
                let started: Vec<u32> = open.iter().copied().filter(|task| !drafts.contains(task)).collect();
                ensure!(
                    started.is_empty(),
                    "Reassign or cancel its started tasks first: {}",
                    started.iter().map(|id| format!("#{id}")).collect::<Vec<_>>().join(", ")
                );
                let role = team.role_of(&id);
                let to = team.role_of(&target);
                for task_id in &drafts {
                    if let Some(task) = team.task_mut(*task_id) {
                        task.assignee = target.clone();
                    }
                }
                events.push(Event::new(
                    "drafts_reassigned",
                    json!({"by":actor.label(team),"from":role,"to":to,"tasks":drafts}),
                ));
            }
            let role = team.role_of(&id);
            let member = team.members.iter_mut().find(|m| m.id == id).context("Unknown member")?;
            member.state = MemberState::Removed;
            // U49: a member removed by hand is no longer draining.
            member.draining = false;
            // #630 N7: its stage runs go back to Waiting for a free member.
            stall_stages_of(team, &id, &format!("{} was removed", team.name_of(&id)), events);
            let by = if input["roster_word"].as_bool() == Some(true) { "Lead on your word" } else { "You" };
            let quote = input["owner_word_checked"]["text"].as_str().unwrap_or_default();
            events.push(Event::new("member_removed", json!({"by":by,"member":role,"owner_word":quote})));
            Ok(json!({"status":"removed","member":id,"by":by}))
        }
        "drain" => {
            // U49: the member finishes its open attempt and stage run, takes
            // nothing new, then its session is paused and the member removed.
            ensure!(human_or_lead, "Only the lead or you drain a member");
            ensure!(
                matches!(team.status, TeamStatus::Running | TeamStatus::Paused),
                "Only a running or paused team drains a member"
            );
            let id = text(input, "member").to_owned();
            ensure!(!id.is_empty(), "drain names the member");
            ensure!(id != team.lead, "The lead cannot be drained");
            let reports: Vec<String> = team.reports_of(&id).iter().map(|member| team.name_of(&member.id)).collect();
            ensure!(reports.is_empty(), "Move its reports to another lead first: {}", reports.join(", "));
            // Undo before the drain ends: the same command with `draining: false`.
            let draining = input["draining"].as_bool().unwrap_or(true);
            let role = team.role_of(&id);
            let by = if input["roster_word"].as_bool() == Some(true) && matches!(actor, Actor::Member(lead) if *lead == team.lead) {
                "Lead on your word"
            } else {
                "You"
            };
            let member = team
                .members
                .iter_mut()
                .find(|member| member.id == id && member.state != MemberState::Removed)
                .context("Unknown member")?;
            if member.draining != draining {
                member.draining = draining;
                let quote = input["owner_word_checked"]["text"].as_str().unwrap_or_default();
                events.push(Event::new(
                    if draining { "member_draining" } else { "drain_cancelled" },
                    json!({"by":by,"member":role,"owner_word":quote}),
                ));
            }
            // N3: a drain moves the member's not-started tasks (a pinned one
            // included) to same-role peers, so the drain ends; what cannot move
            // is named in the answer and in the lead's inbox item.
            if draining {
                let peers: Vec<String> = team
                    .members
                    .iter()
                    .filter(|other| other.id != id && other.state != MemberState::Removed && !other.draining)
                    .filter(|other| team.role_of(&other.id) == role)
                    .map(|other| other.id.clone())
                    .collect();
                let movable: Vec<u32> = team
                    .tasks
                    .iter()
                    .filter(|task| task.assignee == id && matches!(task.status, TaskStatus::Draft | TaskStatus::Pending | TaskStatus::Assigned))
                    .map(|task| task.id)
                    .collect();
                for (index, task_id) in movable.iter().enumerate() {
                    let Some(to) = peers.get(index % peers.len().max(1)).cloned() else { break };
                    let to_role = team.role_of(&to);
                    if let Some(task) = team.task_mut(*task_id) {
                        task.assignee = to;
                        task.pinned = false;
                        task.waits = None;
                        task.prepare.clear();
                    }
                    events.push(Event::new("drain_moved", json!({"task":task_id,"from":role,"to":to_role})));
                }
            }
            let waiting: Vec<u32> = team
                .tasks
                .iter()
                .filter(|task| task.assignee == id && !task.status.finished())
                .map(|task| task.id)
                .collect();
            Ok(json!({"status":if draining {"draining"} else {"draining_cancelled"},"member":id,"by":by,"waiting":waiting}))
        }
        _ => bail!("Unknown team operation {op}"),
    }?;
    // U26/U32: a verified owner word is spent on the one change it authorized,
    // so a second add on the same sentence needs a new word.
    if let Some(event) = input["owner_word_event"].as_i64() {
        let change = input["owner_word_change"].as_str().unwrap_or("this change");
        team.record_owner_word(event, change);
    }
    // U25: auto-accept takes every error-free draft, then a queued phase whose
    // earlier work ended starts.
    auto_accept(team, offline, events);
    advance_phases(team, &done_before, events);
    // A running team with every task decided waits for the lead: it hears once
    // and hears again only after new work is decided.
    if matches!(team.status, TeamStatus::Running | TeamStatus::Paused) {
        let decided = all_decided(team);
        // A legacy record can be all-decided with no message armed (the old
        // code stopped deciding while Paused); it hears once like any other.
        if decided && (!was_decided || team.idle == Notice::None) {
            team.idle = Notice::Pending;
            let count = |status: TaskStatus| team.tasks.iter().filter(|task| task.status == status).count();
            events.push(Event::new(
                "all_decided",
                json!({"accepted":count(TaskStatus::Accepted),"rejected":count(TaskStatus::Rejected),
                    "cancelled":count(TaskStatus::Cancelled),"tasks":team.tasks.len()}),
            ));
        } else if !decided {
            team.idle = Notice::None;
        }
    }
    Ok(result)
}

#[cfg(test)]
#[path = "tests/support_ops.rs"]
mod support;

#[cfg(test)]
#[path = "tests/ops_members.rs"]
mod ops_members;

#[cfg(test)]
#[path = "tests/ops_phases.rs"]
mod ops_phases;

#[cfg(test)]
#[path = "tests/ops_tasks.rs"]
mod ops_tasks;

#[cfg(test)]
#[path = "tests/ops_settings.rs"]
mod ops_settings;

#[cfg(test)]
#[path = "tests/ops_other.rs"]
mod ops_other;
