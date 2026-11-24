//! WhiteWh fork: the team catalog — roles, templates and routing rules that New
//! team, Settings > Teams and the Team designer use. The catalog is versioned;
//! the designer only proposes changes into a draft, and a human keeps or
//! rejects each one and applies them as the next version.
use super::super::*;
use super::model::Task;
use anyhow::{bail, ensure, Context};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;

pub(crate) fn path() -> PathBuf {
    root().join("teams/catalog.json")
}
fn draft_path() -> PathBuf {
    root().join("teams/catalog-draft.json")
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Role {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub effort: String,
    #[serde(default)]
    pub permissions: String,
    /// Task 154: the rights profile a member of this role launches with;
    /// empty: the role's default (rights::role_profile).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub permissions_profile: String,
    #[serde(default)]
    pub computer: String,
    /// worktree, shared or read_only.
    #[serde(default)]
    pub folder: String,
    /// lead or team.
    #[serde(default)]
    pub can_message: String,
    #[serde(default)]
    pub dangerous: bool,
    /// Section 9.8: this role may lead a sub-team.
    #[serde(default)]
    pub can_lead_subteam: bool,
    // U25: the editable role from the owner's designer.
    #[serde(default)]
    pub about: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub machines: Vec<String>,
    /// lead, some or team.
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub message_to: Vec<String>,
    #[serde(default)]
    pub joins: String,
    #[serde(default)]
    pub danger: Value,
    #[serde(default)]
    pub protocol_extra: String,
    #[serde(default)]
    pub task_template: String,
    #[serde(default)]
    pub prompt_order: Vec<String>,
    #[serde(default)]
    pub prompt_off: Vec<String>,
    #[serde(default)]
    pub notes: BTreeMap<String, String>,
    /// U36: the pipeline this role's tasks use unless the plan names one; a
    /// docs role uses Docs. Empty falls back to the template's.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pipeline: String,
    /// U45: the models a member of this role may switch between before a
    /// task; copied into the member when it joins. Empty: its launch model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<super::model::ModelChoice>,
    /// U46: what Zerus does to this role's context before each hand-out:
    /// empty or "none", "compact" or "clear". Copied onto a member that joins.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub context_before_task: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Seat {
    pub role: String,
    #[serde(default = "one")]
    pub count: u32,
    /// The seat is filled only when the goal needs it (shown as 0–1).
    #[serde(default)]
    pub optional: bool,
    /// Section 9.8: the seat's member leads a sub-team from this template.
    #[serde(default)]
    pub subteam: String,
}
fn one() -> u32 {
    1
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct SkeletonTask {
    pub title: String,
    pub role: String,
    /// Indexes of earlier skeleton tasks.
    #[serde(default)]
    pub deps: Vec<usize>,
    /// U25: the phase this skeleton task belongs to.
    #[serde(default = "one_phase")]
    pub phase: u32,
}
fn one_phase() -> u32 {
    1
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Template {
    pub id: String,
    pub name: String,
    #[serde(default, alias = "desc")]
    pub description: String,
    #[serde(default)]
    pub lead_protocol: String,
    #[serde(default)]
    pub roster: Vec<Seat>,
    #[serde(default)]
    pub tasks: Vec<SkeletonTask>,
    /// lead (the lead plans) or template (start from the skeleton).
    #[serde(default)]
    pub planning: String,
    /// review or auto.
    #[serde(default)]
    pub approval: String,
    /// "" (the plan's approval), review or auto: how new phases are approved.
    #[serde(default)]
    pub phase_approval: String,
    #[serde(default)]
    pub max_members: usize,
    /// U25: worktree, clone, same or cloud.
    #[serde(default)]
    pub work: String,
    /// U25: same, owner or lead (new phases).
    #[serde(default)]
    pub phases: String,
    /// U36: the pipeline a team from this template gives its tasks by default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pipeline: String,
}

/// U36: one stage of a pipeline preset: what it checks and who does it, a
/// role (its first free member) or a pinned member.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Stage {
    /// review, test, command (U47) or land (U48).
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub member: String,
    /// U47/U48: the command Zerus runs for a command or land stage, its
    /// expected exit code and time limit. Only the owner's catalog has it: a
    /// plan chooses the preset, never the command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<super::model::Check>,
    /// U48: a land stage's exit codes and outcomes, `{"0":"accepted",
    /// "2":"returned","3":"held"}`; empty is 0 accepted and any other held.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub exits: BTreeMap<String, String>,
}

/// U36: a named pipeline preset ("Code: review, test, lead"): its stages after
/// the implementer's report, the review rounds before the lead decides, the
/// folder rule of a test stage and who gives the final acceptance.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Pipeline {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub stages: Vec<Stage>,
    /// Returned rounds before the task goes to the lead; 0 reads as 3.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub max_rounds: u32,
    /// hold (the test holds the task's folder) or checkout (a detached
    /// checkout of the attempt's commits); empty reads as hold.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// lead, sub_lead or auto; empty reads as lead.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub accept: String,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// U36: the rounds a preset allows when it names none.
pub(crate) const DEFAULT_ROUNDS: u32 = 3;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Rule {
    pub id: String,
    /// Words that, all present in the goal, pick this rule.
    pub when: String,
    pub template: String,
    #[serde(default)]
    pub approval: String,
    /// U25: a template id (stops the search) or "+role" (adds a member and
    /// keeps looking). Empty uses `template`.
    #[serde(default, rename = "use")]
    pub use_: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Catalog {
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub roles: Vec<Role>,
    #[serde(default)]
    pub templates: Vec<Template>,
    #[serde(default)]
    pub routing: Vec<Rule>,
    /// U28: the lead's planning rules, versioned with the catalog.
    #[serde(default = "default_planning_rules")]
    pub planning_rules: String,
    /// U36: the pipeline presets; an old catalog has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pipelines: Vec<Pipeline>,
}

pub(crate) fn default_planning_rules() -> String {
    [
        "(1) At most 20-25 tasks per phase, each ending with an install-ready build and NOTES.md; split more into phases.",
        "(2) Checkpoints (merge, then review and QA in their own lanes) on demand.",
        "(3) Choose checkpoints by efficiency: batch cheaper, check risky work in smaller pieces.",
        "(4) Checkpoint findings go back to the same member before integration; final review findings go to the next phase.",
        "(5) An approved plan is not reshaped; new work goes to the next phase's draft.",
        "(6) A hard dependency only where a task really cannot start earlier; one inside a lane moves that task to the next lane.",
        "(7) No dependencies on tasks of an earlier phase: the barrier waits for it.",
        "(8) Shared foundations first, at the start of the first lane, for a strong member.",
        "(9) Balance every lane by volume and cost (a Qt task weighs about three server tasks), never by task count.",
        "(10) GUI, hard logic and debugging go to strong models; well-specified server work and small fixes to DeepSeek.",
        "(11) Split a heavy task into its server and desktop parts.",
        "(12) QA never implements tasks; the reviewer never writes code it will review.",
        "(13) Expert and ProExpert stay in reserve; the Expert takes a task returned three times, the ProExpert only when the Expert fails.",
        "(14) Before planning, ask the owner how to spread members over accounts.",
        "(15) The lead plans only merges, integration and builds for itself; no long background work except critical end fixes.",
        "(16) Task text is self-contained: plan item, spec/prototype/screenshot paths, what to change, acceptance criteria.",
        "(17) Tests go through the real entry point and fail before the fix.",
        "(18) Name the checks and totals; each suite once, reruns only failures; ask the lead before a run over about ten minutes; plan test load and batch heavy suites.",
        "(19) Folders follow the agent and need; plan first and ask the owner, explaining the choice.",
        "(20) Builds and tests through the built-in templates or scripts in the member's folder.",
        "(21) Before sending: plan health has no errors, a sensible critical path, parallelism near the implementers, balanced lanes, every warning explained.",
        "(22) Offer a phase split above 25 tasks.",
        "(23) No starved notices before a phase is approved; idleness only when the lead can act.",
        "(24) The roster and member limit change only on the owner's explicit word (U26); otherwise the lead proposes.",
        "(25) Above about 8 members or 20 open tasks keep two reviewers and two mergers and a sub-lead whose members report to it, and give tasks a review pipeline (\"docs\": review, auto; \"code\": review, test, lead) so the lead decides only returned work.",
    ]
    .join("\n")
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Decision {
    #[default]
    Proposed,
    Kept,
    Rejected,
}

/// F3: the designer's role corrections: each names its role, the change and
/// why, at most 20 and 500 characters each.
fn check_notes(proposal: &Proposal) -> anyhow::Result<()> {
    ensure!(proposal.notes.len() <= 20, "A proposal has at most 20 role notes");
    for note in &proposal.notes {
        ensure!(!note["role"].as_str().unwrap_or_default().trim().is_empty(), "A role note names its role");
        for field in ["change", "why"] {
            ensure!(
                note[field].as_str().unwrap_or_default().trim().chars().count() <= 500,
                "A role note {field} has at most 500 characters"
            );
        }
    }
    Ok(())
}

/// One change the designer proposes: a whole role, template or rule to add or
/// replace (by id), or an id to remove.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Proposal {
    /// Given by hgs when the proposal is made.
    #[serde(default)]
    pub id: String,
    /// role, template, rule, pipeline or planning_rules.
    pub kind: String,
    /// add, change or remove.
    pub action: String,
    pub target: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub reason: String,
    /// F3: the designer's role corrections, [{role, change, why}], at most 20.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Value>,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub state: Decision,
    #[serde(default)]
    pub at: f64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Draft {
    /// The catalog version the proposals were made against.
    #[serde(default)]
    pub base: u64,
    #[serde(default)]
    pub proposals: Vec<Proposal>,
}

fn role(id: &str, name: &str, color: &str, agent: &str, model: &str, folder: &str, prompt: &str) -> Role {
    Role {
        id: id.into(),
        name: name.into(),
        color: color.into(),
        prompt: prompt.into(),
        agent: agent.into(),
        model: model.into(),
        folder: folder.into(),
        can_message: "lead".into(),
        ..Default::default()
    }
}

/// The catalog before anyone changed it.
pub(crate) fn builtin() -> Catalog {
    let finish = "Finish every task with `hgs team report` (hgs guide team).";
    let roles = vec![
        role("lead", "Lead", "#b79cff", "claude", "", "shared",
            "You lead this team: plan tasks with dependencies, hand nothing out yourself, and accept or reject each report against its criteria."),
        role("architect", "Architect", "#8e9bff", "claude", "", "read_only", &format!("You are the Architect: design interfaces and boundaries; change code only when a task asks. {finish}")),
        role("backend", "Backend", "#6fa8ff", "codex", "", "worktree", &format!("You are the Backend engineer of this team. Work only inside your folder. Keep APIs backward compatible unless the task says otherwise. {finish}")),
        role("frontend", "Frontend", "#f08bb4", "codex", "", "worktree", &format!("You are the Frontend engineer of this team. Work only inside your folder. {finish}")),
        role("tester", "Tester", "#56c8e0", "claude", "", "worktree", &format!("You are the Tester of this team: write and run tests and report what passed and what failed. In a task's test stage, run only the suites of the files its change touched, take the criteria's shots, edit none of the task's files, and answer with a verdict (pass, or return with the command and its failure). Never delete, skip or narrow a test to make it pass. {finish}")),
        role("reviewer", "Reviewer", "#d9b38c", "claude", "", "read_only", &format!("You are the Reviewer: read a change against the task's criteria and report findings by severity; change nothing. In a task's review stage, read the attempt's change in the task's folder, edit none of its files, and answer with a verdict: pass, or return with findings that name the file and the line. Judge each criterion with your own evidence, treat the implementer's report as claims, judge an off-by-default setting as if it were on, and return a change that weakens a test or a criterion the task did not ask to change or breaks a project rule. Never pass a change you could not read. {finish}")),
        role("security", "Security", "#e48de0", "claude", "", "read_only", &format!("You are the Security reviewer: look for secrets, injection, authorization and data exposure. {finish}")),
        role("integrator", "Integrator", "#a3b1c6", "codex", "", "shared", &format!("You are the Integrator: merge the members' branches, resolve conflicts, run the full checks. {finish}")),
        role("researcher", "Researcher", "#b9d36b", "claude", "", "read_only", &format!("You are the Researcher: find facts and options with sources; change no code. {finish}")),
    ];
    let seat = |role: &str, count: u32, optional: bool| Seat { role: role.into(), count, optional, subteam: String::new() };
    let task = |title: &str, role: &str, deps: &[usize]| SkeletonTask { title: title.into(), role: role.into(), deps: deps.to_vec(), phase: 1 };
    let templates = vec![
        Template {
            id: "feature".into(),
            name: "Feature delivery".into(),
            description: "Design, build, test and review a feature.".into(),
            roster: vec![seat("backend", 1, false), seat("frontend", 1, true), seat("tester", 1, false), seat("reviewer", 1, false)],
            tasks: vec![
                task("Requirements and acceptance", "lead", &[]),
                task("Implementation", "backend", &[0]),
                task("Tests", "tester", &[1]),
                task("Code review", "reviewer", &[1, 2]),
            ],
            planning: "lead".into(),
            approval: "review".into(),
            max_members: 6,
            ..Default::default()
        },
        Template {
            id: "bugfix".into(),
            name: "Bugfix".into(),
            description: "Reproduce, fix and test one bug.".into(),
            roster: vec![seat("backend", 1, false), seat("tester", 1, false)],
            tasks: vec![task("Reproduce", "tester", &[]), task("Fix", "backend", &[0]), task("Test the fix", "tester", &[1])],
            planning: "template".into(),
            approval: "review".into(),
            max_members: 3,
            ..Default::default()
        },
    ];
    let routing = vec![Rule { id: "bug".into(), when: "bug fix".into(), template: "bugfix".into(), approval: "review".into(), use_: String::new() }];
    Catalog { version: 1, roles, templates, routing, planning_rules: default_planning_rules(), pipelines: builtin_pipelines() }
}

/// U36: the built-in presets. No template names one yet, so a team keeps
/// today's flow until the owner picks a default.
pub(crate) fn builtin_pipelines() -> Vec<Pipeline> {
    let stage = |kind: &str, role: &str| Stage { kind: kind.into(), role: role.into(), ..Default::default() };
    let preset = |id: &str, name: &str, stages: Vec<Stage>, accept: &str| Pipeline {
        id: id.into(),
        name: name.into(),
        stages,
        max_rounds: DEFAULT_ROUNDS,
        folder: "hold".into(),
        accept: accept.into(),
    };
    vec![
        preset("code", "Code: review, test, lead", vec![stage("review", "reviewer"), stage("test", "tester")], "lead"),
        preset("docs", "Docs: review, auto", vec![stage("review", "reviewer")], "auto"),
        preset("check", "Check: lead", Vec::new(), "lead"),
    ]
}

pub(crate) fn load() -> anyhow::Result<Catalog> {
    match fs::read_to_string(path()) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(builtin()),
        Err(e) => Err(e.into()),
    }
}

/// Task #404: a catalog saved before the presets existed has no pipelines; the
/// built-in presets are its default, so a plan or extend naming 'docs' works.
fn parse(text: &str) -> anyhow::Result<Catalog> {
    let mut catalog: Catalog = serde_json::from_str(text).context("Invalid team catalog")?;
    if catalog.pipelines.is_empty() {
        catalog.pipelines = builtin_pipelines();
    }
    Ok(catalog)
}

fn load_draft() -> anyhow::Result<Draft> {
    match fs::read_to_string(draft_path()) {
        Ok(text) => serde_json::from_str(&text).context("Invalid catalog draft"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Draft::default()),
        Err(e) => Err(e.into()),
    }
}

fn save(catalog: &Catalog) -> anyhow::Result<()> {
    private_dir(&root().join("teams")).map_err(anyhow::Error::msg)?;
    // U25 Revert: keep the previous version before the new one lands.
    if path().exists() {
        let _ = fs::copy(path(), prev_path());
    }
    atomic(&path(), &serde_json::to_string_pretty(catalog)?).map_err(anyhow::Error::msg)
}

/// N3: the role's prompt blocks as they are now, copied onto a member when it
/// joins so a later catalog edit changes no running team mid-phase.
pub(crate) fn snapshot_for(role_name: &str) -> Option<super::model::PromptSnapshot> {
    let catalog = load().unwrap_or_else(|_| builtin());
    let role = catalog.roles.iter().find(|role| role.id == role_name || role.name == role_name)?;
    Some(super::model::PromptSnapshot {
        prompt_order: role.prompt_order.clone(),
        prompt_off: role.prompt_off.clone(),
        protocol_extra: role.protocol_extra.clone(),
        task_template: role.task_template.clone(),
        notes: role.notes.clone(),
        context_before_task: role.context_before_task.clone(),
    })
}

/// Task 154 (4): the rights profile a member of this role launches with when
/// it names none: the role's own, else what the role's job needs (QA
/// operates sessions and workers, a merger commits, a tester builds).
pub(crate) fn profile_for(role_name: &str) -> String {
    let catalog = load().unwrap_or_else(|_| builtin());
    let role = catalog.roles.iter().find(|role| role.id == role_name || role.name == role_name);
    if let Some(profile) = role.map(|role| role.permissions_profile.clone()).filter(|profile| !profile.is_empty()) {
        return profile;
    }
    let (id, name) = role.map(|role| (role.id.as_str(), role.name.as_str())).unwrap_or(("", role_name));
    super::rights::role_profile(id, name).unwrap_or_default().to_owned()
}

/// U45: the model list of a catalog role, copied into a member when it joins
/// so a later catalog edit changes no running team.
pub(crate) fn models_for(role_name: &str) -> Vec<super::model::ModelChoice> {
    let catalog = load().unwrap_or_else(|_| builtin());
    catalog.roles.iter().find(|role| role.id == role_name || role.name == role_name).map(|role| role.models.clone()).unwrap_or_default()
}

/// Renders the member's snapshot in the role's order.
pub(crate) fn prompt_order_text_snapshot(snapshot: &super::model::PromptSnapshot, member_prompt: &str, task: Option<&Task>,
    rules: &str,
) -> Option<String> {
    let role = Role {
        prompt_order: snapshot.prompt_order.clone(),
        prompt_off: snapshot.prompt_off.clone(),
        protocol_extra: snapshot.protocol_extra.clone(),
        task_template: snapshot.task_template.clone(),
        notes: snapshot.notes.clone(),
        ..Default::default()
    };
    prompt_order_text(&role, member_prompt, task, rules)
}

/// C1: the role's own task block replaces the standard "Task #N" block only
/// when it is in the order, switched on, and has a template to fill. An empty
/// template (never typed, or a U25 import without one) keeps the standard
/// block, so a hand-out never loses its title, description or criteria.
pub(crate) fn sends_task(order: &[String], off: &[String], template: &str) -> bool {
    order.iter().any(|key| key == "task") && !off.iter().any(|key| key == "task") && !template.trim().is_empty()
}

/// U25: the role's prompt blocks in the owner's order, skipping the ones turned
/// off. None when the role has no order (an old catalog keeps today's text).
pub(crate) fn prompt_order_text(role: &Role, member_prompt: &str, task: Option<&Task>, project_rules: &str) -> Option<String> {
    if role.prompt_order.is_empty() {
        return None;
    }
    let mut text = String::new();
    let mut order = role.prompt_order.clone();
    if !project_rules.trim().is_empty() && !order.iter().any(|key| key == "rules") {
        order.insert(0, "rules".into());
    }
    for key in &order {
        if role.prompt_off.contains(key) && key != "rules" {
            continue;
        }
        let block = match key.as_str() {
            "rules" => {
                (!project_rules.trim().is_empty()).then(|| format!("{}\n", project_rules.trim()))}
            "role" => (!member_prompt.trim().is_empty()).then(|| format!("Role: {}\n", member_prompt.trim())),
            "protocol" => (!role.protocol_extra.trim().is_empty()).then(|| format!("Team protocol extra:\n{}\n", role.protocol_extra.trim())),
            "task" => task.filter(|_| !role.task_template.trim().is_empty()).map(|task| {
                let criteria = task.criteria.join("; ");
                let rendered = role
                    .task_template
                    .replace("{id}", &task.id.to_string())
                    .replace("{title}", &task.title)
                    .replace("{description}", &task.description)
                    .replace("{criteria}", &criteria);
                format!("Task:\n{rendered}\n")
            }),
            note => role.notes.get(note).map(|value| format!("Note:\n{}\n", value.trim())),
        };
        if let Some(block) = block {
            text += &block;
        }
    }
    Some(text)
}

fn prev_path() -> PathBuf {
    root().join("teams/catalog.prev.json")
}

fn save_draft(draft: &Draft) -> anyhow::Result<()> {
    private_dir(&root().join("teams")).map_err(anyhow::Error::msg)?;
    if draft.proposals.is_empty() {
        let _ = fs::remove_file(draft_path());
        return Ok(());
    }
    atomic(&draft_path(), &serde_json::to_string_pretty(draft)?).map_err(anyhow::Error::msg)
}

fn clean_id(id: &str) -> anyhow::Result<String> {
    ensure!(
        !id.is_empty() && id.len() <= 40 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "Catalog ids use letters, digits, - and _"
    );
    Ok(id.to_owned())
}

/// A team member id as `hgs team` takes it: letters, digits, - and _.
fn member_id_shape(id: &str) -> bool {
    !id.is_empty() && id.len() <= 40 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// U47/U48: a stage Zerus runs itself: a command of 1 to 2000 characters, no
/// member or role; a land stage is the preset's last and maps exit codes to
/// accepted, returned or held.
fn validate_command_stage(pipeline: &Pipeline, i: usize, stage: &Stage) -> anyhow::Result<()> {
    let (id, n) = (&pipeline.id, i + 1);
    ensure!(stage.role.is_empty() && stage.member.is_empty(), "Pipeline {id} stage {n}: Zerus runs a {} stage, with no role or member", stage.kind);
    let command = stage.command.as_ref().with_context(|| format!("Pipeline {id} stage {n}: a {} stage needs its command", stage.kind))?;
    let length = command.command.trim().chars().count();
    ensure!((1..=2000).contains(&length), "Pipeline {id} stage {n}: the command has 1 to 2000 characters");
    ensure!(!command.command.chars().any(|ch| ch == char::from(0)), "Pipeline {id} stage {n}: the command has no NUL");
    ensure!((1..=240).contains(&command.timeout_minutes), "Pipeline {id} stage {n}: the time limit is 1 to 240 minutes");
    if stage.kind == "land" {
        ensure!(n == pipeline.stages.len(), "Pipeline {id}: a land stage is the last stage");
        for (code, outcome) in &stage.exits {
            ensure!(code.parse::<i32>().is_ok(), "Pipeline {id} stage {n}: exit {code} is not a number");
            ensure!(matches!(outcome.as_str(), "accepted" | "returned" | "held"), "Pipeline {id} stage {n}: exit {code} is accepted, returned or held");
        }
    } else {
        ensure!(stage.exits.is_empty(), "Pipeline {id} stage {n}: only a land stage maps exit codes");
    }
    Ok(())
}

/// Checks a whole catalog: unique ids, known roles and templates.
pub(crate) fn validate(catalog: &Catalog) -> anyhow::Result<()> {
    let mut roles = BTreeMap::new();
    for role in &catalog.roles {
        clean_id(&role.id)?;
        ensure!(!role.name.trim().is_empty(), "Role {} needs a name", role.id);
        ensure!(roles.insert(role.id.clone(), ()).is_none(), "Role {} is listed twice", role.id);
        ensure!(role.prompt.len() <= 8000, "The prompt of role {} is too long", role.id);
        // U46: opt-in; an old catalog keeps the default none.
        ensure!(
            matches!(role.context_before_task.as_str(), "" | "none" | "compact" | "clear"),
            "Role {}: context before a task is none, compact or clear",
            role.id
        );
        // U25: the role's editable prompt order and notes.
        ensure!(role.about.chars().count() <= 500, "The about of role {} is too long", role.id);
        ensure!(role.skills.len() <= 20, "Role {} has at most 20 skills", role.id);
        ensure!(matches!(role.message.as_str(), "" | "lead" | "some" | "team"), "Role {}: message is lead, some or team", role.id);
        super::models::check_choices(&role.models).with_context(|| format!("Role {}", role.id))?;
        for key in &role.prompt_off {
            ensure!(role.prompt_order.contains(key) || role.notes.contains_key(key), "Role {} turns off unknown block {key}", role.id);
        }
        for key in &role.prompt_order {
            ensure!(
                matches!(key.as_str(), "rules" | "role" | "protocol" | "task") || role.notes.contains_key(key),
                "Role {}: unknown prompt block {key}",
                role.id
            );
        }
    }
    let mut templates = BTreeMap::new();
    for template in &catalog.templates {
        clean_id(&template.id)?;
        ensure!(templates.insert(template.id.clone(), ()).is_none(), "Template {} is listed twice", template.id);
        for seat in &template.roster {
            ensure!(roles.contains_key(&seat.role), "Template {} names unknown role {}", template.name, seat.role);
        }
        for (i, task) in template.tasks.iter().enumerate() {
            ensure!(task.role == "lead" || roles.contains_key(&task.role), "Template {} task {} names unknown role {}", template.name, i + 1, task.role);
            ensure!(task.deps.iter().all(|d| *d < i), "Template {} task {} depends on a later task", template.name, i + 1);
        }
        ensure!(matches!(template.phase_approval.as_str(), "" | "review" | "auto"),
            "Template {}: phase approval is \"\", review or auto", template.id);
        ensure!(matches!(template.work.as_str(), "" | "worktree" | "clone" | "same" | "cloud"),
            "Template {}: work is worktree, clone, same or cloud", template.id);
        ensure!(matches!(template.phases.as_str(), "" | "same" | "owner" | "lead"),
            "Template {}: phases is same, owner or lead", template.id);
        for task in &template.tasks {
            ensure!(task.phase >= 1, "Template {}: a task phase starts at 1", template.id);
        }
    }
    // U36: presets with known stage kinds and roles; "none" is reserved for
    // a task without a pipeline.
    ensure!(catalog.pipelines.len() <= 20, "The catalog has at most 20 pipelines");
    let mut pipelines = BTreeMap::new();
    for pipeline in &catalog.pipelines {
        clean_id(&pipeline.id)?;
        ensure!(pipeline.id != "none", "Pipeline id none is reserved for no pipeline");
        ensure!(!pipeline.name.trim().is_empty() && pipeline.name.chars().count() <= 80, "Pipeline {} needs a name of at most 80 characters", pipeline.id);
        ensure!(pipelines.insert(pipeline.id.clone(), ()).is_none(), "Pipeline {} is listed twice", pipeline.id);
        ensure!(pipeline.stages.len() <= 8, "Pipeline {} has at most 8 stages", pipeline.id);
        ensure!(pipeline.max_rounds <= 10, "Pipeline {}: at most 10 rounds", pipeline.id);
        ensure!(matches!(pipeline.folder.as_str(), "" | "hold" | "checkout"), "Pipeline {}: folder is hold or checkout", pipeline.id);
        ensure!(matches!(pipeline.accept.as_str(), "" | "lead" | "sub_lead" | "auto"), "Pipeline {}: accept is lead, sub_lead or auto", pipeline.id);
        for (i, stage) in pipeline.stages.iter().enumerate() {
            ensure!(
                matches!(stage.kind.as_str(), "review" | "test" | "command" | "land"),
                "Pipeline {} stage {}: kind is review or test, or a command or land stage that Zerus runs",
                pipeline.id,
                i + 1
            );
            if matches!(stage.kind.as_str(), "command" | "land") {
                validate_command_stage(pipeline, i, stage)?;
                continue;
            }
            ensure!(
                stage.command.is_none() && stage.exits.is_empty(),
                "Pipeline {} stage {}: only a command or land stage runs a command",
                pipeline.id,
                i + 1
            );
            ensure!(stage.role.is_empty() != stage.member.is_empty(), "Pipeline {} stage {}: name a role or a member", pipeline.id, i + 1);
            ensure!(stage.role.is_empty() || roles.contains_key(&stage.role), "Pipeline {} stage {} names unknown role {}", pipeline.id, i + 1, stage.role);
            ensure!(stage.member.is_empty() || member_id_shape(&stage.member), "Pipeline {} stage {}: invalid member {}", pipeline.id, i + 1, stage.member);
        }
    }
    let known_pipeline = |id: &str| id.is_empty() || id == "none" || pipelines.contains_key(id);
    for role in &catalog.roles {
        ensure!(known_pipeline(&role.pipeline), "Role {} names unknown pipeline {}", role.id, role.pipeline);
    }
    for template in &catalog.templates {
        ensure!(known_pipeline(&template.pipeline), "Template {} names unknown pipeline {}", template.id, template.pipeline);
    }
    for rule in &catalog.routing {
        clean_id(&rule.id)?;
        let use_ = if rule.use_.is_empty() { &rule.template } else { &rule.use_ };
        if let Some(role) = use_.strip_prefix('+') {
            ensure!(roles.contains_key(role), "Rule {} names unknown role {}", rule.id, role);
        } else {
            ensure!(templates.contains_key(use_), "Rule {} names unknown template {}", rule.id, use_);
        }
    }
    // Section 9.8: a seat may delegate to a template that exists, and no
    // template may lead back to itself.
    for template in &catalog.templates {
        for seat in template.roster.iter().filter(|seat| !seat.subteam.is_empty()) {
            ensure!(
                templates.contains_key(&seat.subteam),
                "Template {} names unknown sub-team template {}",
                template.id,
                seat.subteam
            );
        }
    }
    fn visit(catalog: &Catalog, id: &str, visiting: &mut BTreeSet<String>, done: &mut BTreeSet<String>) -> anyhow::Result<()> {
        if done.contains(id) {
            return Ok(());
        }
        ensure!(visiting.insert(id.to_owned()), "Template {} has a sub-team cycle", id);
        if let Some(template) = catalog.templates.iter().find(|template| template.id == id) {
            for seat in template.roster.iter().filter(|seat| !seat.subteam.is_empty()) {
                visit(catalog, &seat.subteam, visiting, done)?;
            }
        }
        visiting.remove(id);
        done.insert(id.to_owned());
        Ok(())
    }
    let mut done = BTreeSet::new();
    for template in &catalog.templates {
        visit(catalog, &template.id, &mut BTreeSet::new(), &mut done)?;
    }
    Ok(())
}

fn apply_one(catalog: &mut Catalog, proposal: &Proposal) -> anyhow::Result<()> {
    fn upsert<T: Clone>(items: &mut Vec<T>, id: &str, value: Option<T>, get: impl Fn(&T) -> &str) {
        let at = items.iter().position(|item| get(item) == id);
        match (at, value) {
            (Some(i), Some(v)) => items[i] = v,
            (None, Some(v)) => items.push(v),
            (Some(i), None) => {
                items.remove(i);
            }
            (None, None) => {}
        }
    }
    let remove = proposal.action == "remove";
    match proposal.kind.as_str() {
        "role" => {
            let value: Option<Role> = if remove { None } else { Some(serde_json::from_value(proposal.value.clone()).context("Invalid role")?) };
            upsert(&mut catalog.roles, &proposal.target, value, |r| r.id.as_str());
        }
        "template" => {
            let value: Option<Template> = if remove { None } else { Some(serde_json::from_value(proposal.value.clone()).context("Invalid template")?) };
            upsert(&mut catalog.templates, &proposal.target, value, |t| t.id.as_str());
        }
        "rule" => {
            let value: Option<Rule> = if remove { None } else { Some(serde_json::from_value(proposal.value.clone()).context("Invalid rule")?) };
            upsert(&mut catalog.routing, &proposal.target, value, |r| r.id.as_str());
        }
        // U36: the Pipelines tab's presets, kept or rejected like the others.
        "pipeline" => {
            let value: Option<Pipeline> = if remove { None } else { Some(serde_json::from_value(proposal.value.clone()).context("Invalid pipeline")?) };
            upsert(&mut catalog.pipelines, &proposal.target, value, |p| p.id.as_str());
        }
        // U28: the planning rules are designer-editable text like roles.
        "planning_rules" => {
            ensure!(!remove, "The planning rules cannot be removed");
            let text = proposal.value.as_str().context("Planning rules value is text")?.trim();
            ensure!(!text.is_empty() && text.len() <= 20_000, "Planning rules are 1 to 20000 characters");
            catalog.planning_rules = text.to_owned();
        }
        kind => bail!("Unknown catalog kind {kind}"),
    }
    Ok(())
}

/// The template a goal would use: the first rule that matches.
/// N1: the prototype's routeMatch, as it is: "everything" anywhere in `when`,
/// or a `when` that is just "*", always matches. Otherwise each run of three or
/// more ASCII letters or digits in `when` (its stop words out) matches when the
/// goal contains it, or its singular when it is a plural of five or more
/// letters. An empty `when` never matches.
fn route_match(when: &str, goal: &str) -> bool {
    const STOP: [&str; 9] = ["label", "files", "file", "the", "and", "with", "when", "services", "src"];
    let when = when.to_lowercase();
    if when.contains("everything") || when.trim() == "*" {
        return true;
    }
    let goal = goal.to_lowercase();
    when.split(|ch: char| !(ch.is_ascii_lowercase() || ch.is_ascii_digit()))
        .filter(|word| word.len() >= 3 && !STOP.contains(word))
        .any(|word| goal.contains(word) || (word.len() > 4 && word.ends_with('s') && goal.contains(&word[..word.len() - 1])))
}

pub(crate) fn route(catalog: &Catalog, goal: &str) -> Value {
    let mut added: Vec<String> = Vec::new();
    for rule in &catalog.routing {
        if !route_match(&rule.when, goal) {
            continue;
        }
        let use_ = if rule.use_.is_empty() { &rule.template } else { &rule.use_ };
        if let Some(role) = use_.strip_prefix('+') {
            if !added.iter().any(|found| found == role) {
                added.push(role.to_owned());
            }
            continue;
        }
        if let Some(template) = catalog.templates.iter().find(|t| t.id == *use_) {
            return json!({"rule":rule.id,"template":template,"approval":rule.approval,"added":added});
        }
    }
    let fallback = catalog.templates.first();
    json!({"rule":null,"template":fallback,"approval":fallback.map(|t| t.approval.clone()),"added":added})
}

fn locked<T>(f: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
    private_dir(&root().join("teams")).map_err(anyhow::Error::msg)?;
    let file = lock_file(&root().join("teams/catalog.lock")).map_err(anyhow::Error::msg)?;
    fs2::FileExt::lock_exclusive(&file)?;
    let result = f();
    let _ = fs2::FileExt::unlock(&file);
    result
}

/// The Team designer's first prompt: what it may do and how it proposes.
pub(crate) fn designer_brief(catalog: &Catalog, goal: &str, project: &str, project_name: &str) -> String {
    let mut text = format!(
        "You are the Zerus Team designer, a conversational assistant for building agent teams. You configure the team catalog: \
         roles, templates (which roles a kind of work needs, with a task skeleton) and routing rules. Read `hgs guide designer` \
         first. Read the selected project and the existing catalog before designing. Discuss the owner's goals and ask focused \
         questions when the tasks, constraints or acceptance criteria are unclear. Reuse suitable existing roles and templates, \
         explain which ones fit, and propose new or changed ones only for needs they do not cover. You may read the owner's \
         projects to learn how they work, but change no file: your only output is proposals with `hgs team catalog propose --json`. \
         The owner keeps or rejects each proposal and applies them; running teams keep their roster. The catalog is v{}: \
         read it with `hgs team catalog get --json <<< '{{}}'`. Run `hgs` directly and send JSON on stdin in a heredoc.",
        catalog.version
    );
    // Section 9.8: the designer explains sub-leads: a role may lead one, and a
    // seat may delegate to another template.
    text += "\n\nA role with can_lead_subteam may lead a sub-team; a roster seat may name subteam: <template> so that member delegates that template. Unknown templates and template cycles are refused, so keep the catalog acyclic.";
    if !project_name.trim().is_empty() {
        text += &format!("\n\nThe selected project is {}.", project_name.trim());
    }
    if !project.trim().is_empty() {
        text += &format!("\n\nRead the selected project in the folder {}.", project.trim());
    }
    if !goal.trim().is_empty() {
        text += &format!("\n\nThe owner asks: {}", goal.trim());
    }
    text += &format!("\n\nThe catalog's planning rules (v{}):\n{}", catalog.version, catalog.planning_rules);
    text
}

/// U16: the Team designer's narrow Claude rules. It reads and proposes — the
/// catalog read, propose and route calls and the guides over Bash, plus
/// read-only file tools. Nothing here writes outside its folder.
const DESIGNER_CLAUDE_RULES: [&str; 7] = [
    "Bash(hgs team catalog get:*)",
    "Bash(hgs team catalog propose:*)",
    "Bash(hgs team catalog route:*)",
    "Bash(hgs guide:*)",
    "Read",
    "Glob",
    "Grep",
];

/// U16: Claude stops the designer at its start-up trust prompt. Zerus's own
/// designer folder is private to Zerus (docs/reference.md), so exactly that
/// folder is trusted in the account config the launch reads, before the
/// session starts; every other folder keeps the manual prompt. Other agents
/// have no such trust entry and get nothing.
fn trust_designer(agent: &str, folder: &str, config: &std::path::Path) -> anyhow::Result<bool> {
    if agent != "claude" {
        return Ok(false);
    }
    Ok(crate::accounts::trust_claude_folder(config, folder)?)
}

/// The `hgs` arguments that start the Team designer. A Claude designer also
/// takes the narrow rules on its own command line, after `--` (the way B5
/// writes the lead's `hgs team answer` rule); other agents get no new option.
fn designer_commands(agent: &str, folder: &str, model: &str, effort: &str, brief: &str, launch_id: &str) -> Vec<String> {
    let mut args: Vec<String> = [agent, folder, "--new", "-n", "team-designer", "--launch-id", launch_id].map(String::from).to_vec();
    // As a new session from the desktop: a terminal agent runs detached and
    // takes the model on its own command line; OpenCode takes it from hgs.
    let terminal = agent != "dsh" && agent != "opencode";
    if terminal {
        args.push("-d".into());
    }
    if agent == "opencode" && !model.is_empty() {
        args.extend(["--model".into(), model.to_owned()]);
        if !effort.is_empty() {
            args.extend(["--effort".into(), effort.to_owned()]);
        }
    }
    args.push("--".into());
    if terminal {
        args.extend(super::dispatcher::agent_args(agent, model, effort));
        if agent == "claude" {
            args.push("--allowedTools".into());
            args.extend(DESIGNER_CLAUDE_RULES.iter().map(|rule| (*rule).to_owned()));
            // N19: --allowedTools takes every word up to the next option, so
            // the brief comes after Claude's own `--`; launch flags (the
            // account's permission mode, model defaults) go in before it.
            args.push("--".into());
        }
    }
    args.push(brief.to_owned());
    args
}

fn designer(input: &Value) -> anyhow::Result<Value> {
    // The catalog belongs to no project: the designer runs in its own folder,
    // so the desktop lists it outside the projects. The project it may read is
    // named in its brief.
    let project = input["folder"].as_str().unwrap_or_default();
    let home = super::super::root().join("team-designer");
    std::fs::create_dir_all(&home).context("Could not create the Team designer's folder")?;
    let folder = home.to_string_lossy().into_owned();
    let agent = input["agent"].as_str().filter(|a| !a.is_empty()).unwrap_or("claude");
    ensure!(["claude", "codex", "kimi", "qwen", "dsh", "opencode"].contains(&agent), "Unknown agent {agent}");
    // A model or effort is one word, as a provider's command line takes it.
    let word = |key: &str| -> anyhow::Result<String> {
        let value = input[key].as_str().unwrap_or_default().trim().to_owned();
        ensure!(value.len() <= 120 && !value.contains(char::is_whitespace) && !value.starts_with('-'), "Invalid {key}");
        Ok(value)
    };
    let (model, effort) = (word("model")?, word("effort")?);
    let launch_id = uuid::Uuid::new_v4().to_string();
    // U16: before the launch, so Claude reads the answer instead of asking.
    if agent == "claude" {
        let config = crate::config::Config::load()?;
        trust_designer(agent, &folder, &crate::accounts::claude_launch_config(&config)?)?;
    }
    let brief = designer_brief(&load()?, input["goal"].as_str().unwrap_or_default(), project, input["project"].as_str().unwrap_or_default());
    let args = designer_commands(agent, &folder, &model, &effort, &brief, &launch_id);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = super::dispatcher::hgs("", &args, None, std::time::Duration::from_secs(120))?;
    ensure!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr).trim());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let name = stdout.lines().chain(String::from_utf8_lossy(&output.stderr).lines()).find_map(|line| line.trim().strip_prefix("hgs: started ").map(str::to_owned));
    Ok(json!({"status":"started","launch_id":launch_id,"name":name}))
}

/// `hgs team catalog get|set|propose|decide|apply|discard|route|designer --json`.
/// `human` is true when the caller said "by":"human" from no session.
pub(crate) fn run(op: &str, input: &Value, human: bool) -> anyhow::Result<Value> {
    let text = |key: &str| input[key].as_str().unwrap_or_default().to_owned();
    match op {
        "get" => {
            let catalog = load()?;
            let draft = load_draft()?;
            Ok(json!({"catalog":catalog,"draft":draft,"builtin":!path().exists()}))
        }
        "route" => Ok(route(&load()?, &text("goal"))),
        "set" => locked(|| {
            ensure!(human, "Only you change the catalog directly; a designer proposes");
            let current = load()?;
            let expected = input["version"].as_u64().context("The catalog version you edited is required")?;
            ensure!(expected == current.version, "The catalog changed (now v{}); refresh and try again", current.version);
            let mut catalog: Catalog = serde_json::from_value(input["catalog"].clone()).context("Invalid catalog")?;
            validate(&catalog)?;
            catalog.version = current.version + 1;
            save(&catalog)?;
            Ok(json!({"status":"saved","version":catalog.version}))
        }),
        "propose" => locked(|| {
            let current = load()?;
            let mut draft = load_draft()?;
            // N38 (#548): a draft on an older version can never be applied
            // ("apply" refuses it), and only the owner may discard; so the
            // designer's next proposals replace it with a draft on this one.
            if draft.proposals.is_empty() || draft.base != current.version {
                draft = Draft { base: current.version, proposals: Vec::new() };
            }
            let mut added = Vec::new();
            for value in input["proposals"].as_array().context("A list of proposals is required")? {
                let mut proposal: Proposal = serde_json::from_value(value.clone()).context("Invalid proposal")?;
                ensure!(matches!(proposal.kind.as_str(), "role" | "template" | "rule"), "A proposal changes a role, template or rule");
                ensure!(matches!(proposal.action.as_str(), "add" | "change" | "remove"), "A proposal adds, changes or removes");
                proposal.target = clean_id(&proposal.target)?;
                // F3: the designer names each role correction with its reason.
                check_notes(&proposal)?;
                // A proposal must apply on its own to the current catalog.
                let mut trial = current.clone();
                apply_one(&mut trial, &proposal)?;
                proposal.id = uuid::Uuid::new_v4().to_string();
                proposal.state = Decision::Proposed;
                proposal.at = now();
                added.push(proposal.id.clone());
                draft.proposals.push(proposal);
            }
            ensure!(draft.proposals.len() <= 200, "The draft holds at most 200 proposals");
            save_draft(&draft)?;
            Ok(json!({"status":"proposed","ids":added,"base":draft.base}))
        }),
        "decide" => locked(|| {
            ensure!(human, "Only you keep or reject a proposal");
            let mut draft = load_draft()?;
            let decision = match text("decision").as_str() {
                "keep" => Decision::Kept,
                "reject" => Decision::Rejected,
                "undo" => Decision::Proposed,
                _ => bail!("The decision is keep, reject or undo"),
            };
            let id = text("proposal");
            let proposal = draft.proposals.iter_mut().find(|p| p.id == id).context("Unknown proposal")?;
            proposal.state = decision;
            save_draft(&draft)?;
            Ok(json!({"status":"decided","proposal":id}))
        }),
        "apply" => locked(|| {
            ensure!(human, "Only you apply the draft");
            let current = load()?;
            let draft = load_draft()?;
            ensure!(draft.base == current.version, "The catalog changed since this draft (v{} → v{}); discard it and ask again", draft.base, current.version);
            let kept: Vec<&Proposal> = draft.proposals.iter().filter(|p| p.state == Decision::Kept).collect();
            ensure!(!kept.is_empty(), "Keep at least one change to apply");
            let mut catalog = current.clone();
            for proposal in &kept {
                apply_one(&mut catalog, proposal)?;
            }
            validate(&catalog)?;
            catalog.version = current.version + 1;
            save(&catalog)?;
            save_draft(&Draft::default())?;
            Ok(json!({"status":"applied","version":catalog.version,"changes":kept.len()}))
        }),
        "export" => Ok(json!({"catalog":load()?})),
        "revert" => locked(|| {
            ensure!(human, "Only you revert the catalog");
            let current = load()?;
            let text = fs::read_to_string(prev_path()).context("There is no previous catalog version to revert to")?;
            let mut previous: Catalog = serde_json::from_str(&text).context("Invalid previous catalog")?;
            validate(&previous)?;
            previous.version = current.version + 1;
            save(&previous)?;
            Ok(json!({"status":"reverted","version":previous.version}))
        }),
        "import" => locked(|| {
            ensure!(human, "Only you import a catalog");
            let current = load()?;
            let expected = input["version"].as_u64().context("The catalog version you edited is required")?;
            ensure!(expected == current.version, "The catalog changed (now v{}); refresh and try again", current.version);
            let mut catalog: Catalog = serde_json::from_value(input["catalog"].clone()).context("Invalid catalog")?;
            validate(&catalog)?;
            catalog.version = current.version + 1;
            save(&catalog)?;
            Ok(json!({"status":"imported","version":catalog.version}))
        }),
        "designer" => {
            ensure!(human, "Only you start the Team designer");
            designer(input)
        }
        "discard" => locked(|| {
            ensure!(human, "Only you discard the draft");
            save_draft(&Draft::default())?;
            Ok(json!({"status":"discarded"}))
        }),
        _ => bail!("Unknown catalog operation {op}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planning_rules_default_and_reach_the_briefs() {
        let builtin = builtin();
        assert!(builtin.planning_rules.contains("(24)") && builtin.planning_rules.contains("(18)"));
        // An old catalog without the field loads the built-in rules.
        let old: Catalog = serde_json::from_value(json!({"version":3,"roles":[],"templates":[],"routing":[]})).unwrap();
        assert_eq!(old.planning_rules, default_planning_rules());
        let brief = designer_brief(&builtin, "goal", "/work/orbit", "Orbit platform");
        assert!(brief.contains("Planning rules") || brief.contains("(24)"));
        assert!(brief.contains("Orbit platform") && brief.contains("/work/orbit"));
        assert!(brief.contains("ask focused questions") && brief.contains("Reuse suitable existing roles and templates"));
        // The rules are versioned with the catalog like any other text.
        let mut edited = builtin.clone();
        edited.version = 4;
        edited.planning_rules = "(1) a changed rule".into();
        let round_trip: Catalog = serde_json::from_value(serde_json::to_value(&edited).unwrap()).unwrap();
        assert_eq!(round_trip.planning_rules, "(1) a changed rule");
    }

    #[test]
    fn route_matches_like_the_prototype() {
        let mut catalog = builtin();
        catalog.routing = vec![
            Rule { id: "review".into(), when: "review · release".into(), template: "bugfix".into(), approval: String::new(), use_: String::new() },
            Rule { id: "all".into(), when: "everything else".into(), template: "feature".into(), approval: String::new(), use_: String::new() },
        ];
        validate(&catalog).unwrap();
        // Either word of "review · release" matches; the dot is not a token.
        assert_eq!(route(&catalog, "please review this")["template"]["id"], "bugfix");
        assert_eq!(route(&catalog, "make a release")["template"]["id"], "bugfix");
        // "everything else" always matches, so it is the catch-all, not the
        // first template fallback.
        assert_eq!(route(&catalog, "new checkout")["template"]["id"], "feature");
        // An empty when never matches.
        catalog.routing[0].when.clear();
        assert_eq!(route(&catalog, "new checkout")["template"]["id"], "feature");
    }

    #[test]
    fn route_ports_the_prototypes_match_with_its_stop_words_and_plurals() {
        let rule = |id: &str, when: &str, use_: &str| Rule { id: id.into(), when: when.into(), template: String::new(), approval: String::new(), use_: use_.into() };
        let mut catalog = builtin();
        // N1: the prototype's own rules r1 and r2, then a "*" catch-all.
        catalog.routing = vec![
            rule("r1", "label \u{201c}bug\u{201d} \u{b7} \u{2264} 3 files", "bugfix"),
            rule("r2", "services/payments/** \u{b7} **/auth/**", "+security"),
            rule("all", "*", "feature"),
        ];
        validate(&catalog).unwrap();
        // "label", "files" and "services" are stop words: they route nothing.
        let found = route(&catalog, "Tidy the label files of the services");
        assert_eq!((found["rule"].clone(), found["template"]["id"].clone(), found["added"].clone()), (json!("all"), json!("feature"), json!([])));
        // A substring and a plural's singular match, as in the prototype.
        assert_eq!(route(&catalog, "Debug the login")["rule"], "r1");
        let found = route(&catalog, "Round the payment total (OAuth)");
        assert_eq!((found["rule"].clone(), found["added"].clone()), (json!("all"), json!(["security"])));
        // "*" alone always matches, so the router never falls to templates.first().
        catalog.templates.rotate_left(1);
        assert_ne!(catalog.templates[0].id, "feature");
        assert_eq!(route(&catalog, "New checkout")["template"]["id"], "feature");
        assert!(!route_match("", "anything") && !route_match("  ", "anything"));
        assert!(route_match(" * ", "anything") && route_match("Everything else", "x"));
    }

    #[test]
    fn a_claude_designer_is_trusted_for_its_folder_and_gets_the_narrow_rules() {
        let folder = "C:\\Users\\example\\.local/state\\hgs/sessions\\team-designer";
        // N19: Claude files a Windows project under forward slashes.
        let key = if cfg!(windows) { "C:/Users/example/.local/state/hgs/sessions/team-designer" } else { folder };
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join(".claude.json");
        fs::write(&config, json!({
            "oauthAccount": {"emailAddress": "owner@example.com"},
            "projects": {
                "C:\\work\\project": {"hasTrustDialogAccepted": false, "allowedTools": ["Bash(cargo:*)"]},
                (key): {"allowedTools": ["Read"]}
            }
        }).to_string()).unwrap();
        // The launch arguments carry the narrow rules after the agent flags.
        let args = designer_commands("claude", folder, "Opus", "high", "brief", "launch");
        let at = args.iter().position(|arg| arg == "--allowedTools").expect("--allowedTools");
        for (index, rule) in DESIGNER_CLAUDE_RULES.iter().enumerate() {
            assert_eq!(args[at + 1 + index], *rule, "{args:?}");
        }
        assert_eq!(args.last().unwrap(), "brief");
        // N19: Claude reads the brief as its prompt, not as an eighth rule:
        // its own `--` comes right after the rules, and the launch's
        // permission flags go in before that `--`, never after the brief.
        assert_eq!(args[at + 1 + DESIGNER_CLAUDE_RULES.len()], "--", "{args:?}");
        let agent_at = args.iter().position(|arg| arg == "--").unwrap() + 1;
        let mut argv: Vec<String> = std::iter::once("claude".to_owned()).chain(args[agent_at..].iter().cloned()).collect();
        crate::accounts::apply_permissions("claude", "bypass", &mut argv).unwrap();
        let delimiter = argv.iter().position(|arg| arg == "--").unwrap();
        assert_eq!(&argv[delimiter..], ["--", "brief"], "{argv:?}");
        assert!(argv[..delimiter].iter().any(|arg| arg == "--permission-mode"), "{argv:?}");
        assert!(args.iter().any(|arg| arg == "Opus") && args.iter().any(|arg| arg == "high"));
        // Before the launch, exactly the designer's folder is trusted.
        assert!(trust_designer("claude", folder, &config).unwrap());
        let saved: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
        assert_eq!(saved["projects"][key]["hasTrustDialogAccepted"], true);
        assert_eq!(saved["projects"][key]["allowedTools"], json!(["Read"]), "its tools stay as Claude saved them");
        assert_eq!(saved["projects"]["C:\\work\\project"]["hasTrustDialogAccepted"], false, "another folder keeps its answer");
        assert_eq!(saved["projects"].as_object().unwrap().len(), 2, "no other project entry is added");
        assert_eq!(saved["oauthAccount"]["emailAddress"], "owner@example.com");
        assert!(!trust_designer("claude", folder, &config).unwrap(), "a second launch changes nothing");
    }

    #[test]
    fn other_designer_agents_get_no_trust_and_no_rules() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join(".claude.json");
        for agent in ["codex", "kimi", "dsh", "opencode"] {
            let args = designer_commands(agent, "C:\\designer", "", "", "brief", "launch");
            assert!(!args.iter().any(|arg| arg == "--allowedTools"), "{agent}: {args:?}");
            assert!(!trust_designer(agent, "C:\\designer", &config).unwrap(), "{agent}");
        }
        assert!(!config.exists(), "nothing writes a Claude account config for another agent");
    }

    /// Task #404: a catalog file saved before the presets existed (no
    /// "pipelines") still offers the built-in presets, so an extend naming
    /// 'docs' or 'code' is accepted; a catalog with its own keeps them.
    #[test]
    fn a_catalog_without_pipelines_gets_the_builtin_presets() {
        let catalog = parse(r#"{"version":1,"roles":[{"id":"lead","name":"Lead"}]}"#).unwrap();
        let ids: Vec<&str> = catalog.pipelines.iter().map(|pipeline| pipeline.id.as_str()).collect();
        assert_eq!(ids, ["code", "docs", "check"], "{ids:?}");
        // The extend path's check takes them: this is the 'Unknown pipeline
        // docs' refusal the task named.
        crate::state::teams::pipeline::check_choice("docs", &catalog).unwrap();
        crate::state::teams::pipeline::check_choice("code", &catalog).unwrap();
        let own = parse(r#"{"version":1,"pipelines":[{"id":"mine","name":"Mine"}]}"#).unwrap();
        assert_eq!(own.pipelines.len(), 1, "{:?}", own.pipelines);
        assert_eq!(own.pipelines[0].id, "mine");
    }

    #[test]
    fn the_builtin_catalog_is_valid_and_routes_bugs() {
        let catalog = builtin();
        validate(&catalog).unwrap();
        assert_eq!(route(&catalog, "Fix the login bug")["template"]["id"], "bugfix");
        assert_eq!(route(&catalog, "New checkout")["template"]["id"], "feature");
    }

    #[test]
    fn a_route_adds_a_role_and_the_next_template_rule_stops() {
        let mut catalog = builtin();
        catalog.routing.insert(0, Rule {
            id: "security".into(), when: "secure".into(), template: String::new(), approval: String::new(),
            use_: "+security".into(),
        });
        if !catalog.roles.iter().any(|role| role.id == "security") {
            catalog.roles.push(Role { id: "security".into(), name: "Security".into(), ..Default::default() });
        }
        validate(&catalog).unwrap();
        let found = route(&catalog, "secure login bug fix");
        assert_eq!(found["added"], json!(["security"]));
        assert_eq!(found["template"]["id"], "bugfix");
        // A rule naming an unknown role or template is refused.
        catalog.routing[0].use_ = "+nobody".into();
        assert!(validate(&catalog).unwrap_err().to_string().contains("unknown role"));
    }

    #[test]
    fn export_import_and_revert_move_versions() {
        let _guard = crate::state::ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let old = std::env::var("HGS_STATE_DIR").ok();
        std::env::set_var("HGS_STATE_DIR", dir.path());
        let result = (|| -> anyhow::Result<()> {
            let mut start = builtin();
            start.version = 1;
            save(&start)?;
            let exported = run("export", &json!({}), true)?;
            assert_eq!(exported["catalog"]["version"], 1);
            // A bad catalog is refused and changes nothing.
            let error = run("import", &json!({"version":1,"catalog":{"roles":[{"id":"x","name":"","prompt":""}]}}), true)
                .unwrap_err()
                .to_string();
            assert!(error.contains("needs a name"), "{error}");
            // A good one becomes v2.
            let mut imported = start.clone();
            imported.roles[0].name = "Imported Lead".into();
            let value = run("import", &json!({"version":1,"catalog":imported}), true)?;
            assert_eq!(value["version"], 2);
            assert_eq!(load()?.roles[0].name, "Imported Lead");
            // Revert brings the previous version back as v3.
            let value = run("revert", &json!({}), true)?;
            assert_eq!(value["version"], 3);
            assert_eq!(load()?.roles[0].name, "Lead");
            Ok(())
        })();
        match old {
            Some(value) => std::env::set_var("HGS_STATE_DIR", value),
            None => std::env::remove_var("HGS_STATE_DIR"),
        }
        result.unwrap();
    }

    #[test]
    fn the_prompt_order_sends_each_block_and_skips_the_off_ones() {
        let mut role = Role {
            id: "dev".into(), name: "Dev".into(), prompt: "Write the code".into(), protocol_extra: "Report each step".into(),
            prompt_order: vec!["rules".into(), "role".into(), "protocol".into(), "task".into(), "note1".into()],
            task_template: "Do #{id} {title}: {description}. Criteria: {criteria}".into(),
            ..Default::default()
        };
        role.notes.insert("note1".into(), "Read the docs first".into());
        let task = Task { id: 7, title: "API".into(), description: "The endpoint".into(), criteria: vec!["Fast".into(), "Safe".into()], ..Default::default() };
        let text = prompt_order_text(&role, &role.prompt, Some(&task), "Project rules:\nUse tabs").unwrap();
        assert!(text.starts_with("Project rules:\nUse tabs\nRole: Write the code\n"), "{text}");
        assert!(text.contains("Team protocol extra:\nReport each step"), "{text}");
        assert!(text.contains("Do #7 API: The endpoint. Criteria: Fast; Safe"), "{text}");
        assert!(text.contains("Note:\nRead the docs first"), "{text}");
        // Off blocks are skipped; an old role (no order) keeps today's text.
        role.prompt_off = vec!["protocol".into(), "task".into()];
        let text = prompt_order_text(&role, &role.prompt, Some(&task), "").unwrap();
        assert!(!text.contains("Team protocol extra") && !text.contains("Do #7"), "{text}");
        role.prompt_order.clear();
        assert!(prompt_order_text(&role, &role.prompt, Some(&task), "").is_none());
    }

    #[test]
    fn an_ordered_task_block_without_a_template_is_not_sent() {
        // C1: an order that names "task" with an empty template sends no empty
        // "Task:" block, and the standard block stays (sends_task is false).
        let role = Role {
            id: "dev".into(), name: "Dev".into(), prompt: "Write the code".into(),
            prompt_order: vec!["role".into(), "task".into()], task_template: " \n ".into(),
            ..Default::default()
        };
        let task = Task { id: 7, title: "API".into(), ..Default::default() };
        let text = prompt_order_text(&role, &role.prompt, Some(&task), "").unwrap();
        assert_eq!(text, "Role: Write the code\n");
        assert!(!sends_task(&role.prompt_order, &role.prompt_off, &role.task_template));
        assert!(sends_task(&role.prompt_order, &role.prompt_off, "#{id} {title}"));
        assert!(!sends_task(&role.prompt_order, &["task".into()], "#{id} {title}"));
        assert!(!sends_task(&["role".into()], &[], "#{id} {title}"));
    }

    #[test]
    fn an_old_catalog_still_loads_and_prompt_order_is_checked() {
        // A catalog from before U25: no new fields anywhere.
        let old: Catalog = serde_json::from_value(json!({
            "version": 3,
            "roles": [{"id":"dev","name":"Dev","prompt":"Work","agent":"codex"}],
            "templates": [{"id":"t","name":"T","description":"D","roster":[{"role":"dev"}],
                "tasks":[{"title":"A","role":"dev"}]}],
            "routing": [{"id":"r","when":"x","template":"t"}]
        })).unwrap();
        validate(&old).unwrap();
        assert!(old.roles[0].prompt_order.is_empty());
        assert!(old.templates[0].work.is_empty());
        // A prompt block must be a known block or a note, and prompt_off may not
        // turn off an unknown one.
        let mut bad = old.clone();
        bad.roles[0].prompt_order = vec!["unknown".into()];
        assert!(validate(&bad).unwrap_err().to_string().contains("unknown prompt block"));
        bad.roles[0].prompt_order = vec!["rules".into(), "note1".into()];
        bad.roles[0].notes.insert("note1".into(), "Read the docs first".into());
        bad.roles[0].prompt_off = vec!["rules".into()];
        validate(&bad).unwrap();
    }

    #[test]
    fn a_proposals_role_notes_are_bounded_and_explained() {
        let mut proposal = Proposal { kind: "role".into(), action: "add".into(), target: "security".into(), ..Default::default() };
        proposal.notes = vec![json!({"role":"backend","change":"merge the tests role","why":"the goal has no tests owner"})];
        check_notes(&proposal).unwrap();
        proposal.notes = (0..21).map(|_| json!({"role":"r","change":"c","why":"w"})).collect();
        assert_eq!(check_notes(&proposal).unwrap_err().to_string(), "A proposal has at most 20 role notes");
        proposal.notes = vec![json!({"role":"","change":"c","why":"w"})];
        assert!(check_notes(&proposal).unwrap_err().to_string().contains("names its role"));
        proposal.notes = vec![json!({"role":"r","change":"c","why":"x".repeat(501)})];
        assert!(check_notes(&proposal).unwrap_err().to_string().contains("A role note why"));
    }

    #[test]
    fn a_proposal_needs_no_id_of_its_own() {
        // hgs gives the id; the guide's example has none.
        let proposal: Proposal = serde_json::from_value(json!({"kind":"role","action":"remove","target":"tester"})).unwrap();
        assert!(proposal.id.is_empty());
    }

    #[test]
    fn a_template_carries_its_phase_approval() {
        let template: Template = serde_json::from_value(json!({"id":"feature","name":"Feature","phase_approval":"auto"})).unwrap();
        assert_eq!(template.phase_approval, "auto");
        let plain: Template = serde_json::from_value(json!({"id":"feature","name":"Feature"})).unwrap();
        assert_eq!(plain.phase_approval, "");
        let mut catalog = builtin();
        catalog.templates[0].phase_approval = "sometimes".into();
        assert!(validate(&catalog).is_err());
    }

    #[test]
    fn proposals_apply_in_order_and_keep_the_catalog_valid() {
        let mut catalog = builtin();
        let change = Proposal {
            kind: "role".into(),
            action: "change".into(),
            target: "tester".into(),
            value: serde_json::to_value(Role { id: "tester".into(), name: "Tester".into(), model: "Sonnet".into(), ..Default::default() }).unwrap(),
            ..Default::default()
        };
        apply_one(&mut catalog, &change).unwrap();
        assert_eq!(catalog.roles.iter().find(|r| r.id == "tester").unwrap().model, "Sonnet");
        let remove = Proposal { kind: "role".into(), action: "remove".into(), target: "tester".into(), ..Default::default() };
        apply_one(&mut catalog, &remove).unwrap();
        // Templates still name the tester: the result is refused.
        assert!(validate(&catalog).is_err());
    }

    /// N38 (#548): after the owner saves the catalog, a draft on the older
    /// version can never be applied and the designer cannot discard it; the
    /// designer's next proposals replace it with a draft on the new version,
    /// which the owner can apply.
    #[test]
    fn a_designer_redo_replaces_an_out_of_date_draft() {
        let temp = tempfile::tempdir().unwrap();
        crate::state::with_test_state_root(temp.path(), || {
            let tester = |model: &str| {
                json!({"proposals":[{"kind":"role","action":"change","target":"tester","title":"Tester model",
                    "value":serde_json::to_value(Role { id: "tester".into(), name: "Tester".into(), model: model.into(), ..Default::default() }).unwrap()}]})
            };
            let first = run("propose", &tester("Sonnet"), false).unwrap();
            let base = first["base"].as_u64().unwrap();
            // The owner saves the catalog meanwhile: the draft is out of date.
            let catalog = run("get", &json!({}), true).unwrap()["catalog"].clone();
            run("set", &json!({"catalog":catalog,"version":base}), true).unwrap();
            let id = first["ids"][0].as_str().unwrap().to_owned();
            run("decide", &json!({"proposal":id,"decision":"keep"}), true).unwrap();
            let refused = run("apply", &json!({}), true).unwrap_err().to_string();
            assert!(refused.contains("changed since this draft"), "{refused}");
            assert!(run("discard", &json!({}), false).is_err(), "only the owner discards");
            // The redo: new proposals on the new version replace the old draft.
            let redo = run("propose", &tester("Haiku"), false).unwrap();
            assert_eq!(redo["base"].as_u64().unwrap(), base + 1);
            let draft = run("get", &json!({}), true).unwrap()["draft"].clone();
            assert_eq!(draft["proposals"].as_array().unwrap().len(), 1, "{draft}");
            run("decide", &json!({"proposal":redo["ids"][0],"decision":"keep"}), true).unwrap();
            let applied = run("apply", &json!({}), true).unwrap();
            assert_eq!(applied["version"].as_u64().unwrap(), base + 2);
            assert_eq!(load().unwrap().roles.iter().find(|r| r.id == "tester").unwrap().model, "Haiku");
            // A current draft still gathers proposals.
            let one = run("propose", &tester("Opus"), false).unwrap();
            let two = run("propose", &tester("Sonnet"), false).unwrap();
            assert_eq!(one["base"], two["base"]);
            assert_eq!(run("get", &json!({}), true).unwrap()["draft"]["proposals"].as_array().unwrap().len(), 2);
        });
    }

    #[test]
    fn a_sub_team_template_must_exist_and_may_not_cycle() {
        let mut catalog = builtin();
        let id = catalog.templates[0].id.clone();
        let role = catalog.templates[0].roster[0].role.clone();
        // A seat may name a template that exists.
        catalog.templates[0].roster[0].subteam = id.clone();
        assert!(validate(&catalog).is_err(), "a template leading to itself is a cycle");
        catalog.templates.push(Template { id: "sub".into(), name: "Sub".into(), roster: vec![Seat { role: role.clone(), subteam: id.clone(), ..Default::default() }], ..Default::default() });
        // A two-template cycle is refused.
        assert!(validate(&catalog).is_err());
        // A missing template is refused before any cycle check.
        catalog.templates[0].roster[0].subteam = "missing".into();
        let error = validate(&catalog).unwrap_err().to_string();
        assert!(error.contains("unknown sub-team template"), "{error}");
        // A valid delegation loads, and the role may say it leads sub-teams.
        catalog.templates[0].roster[0].subteam = "sub".into();
        catalog.templates.iter_mut().find(|template| template.id == "sub").unwrap().roster[0].subteam =
            String::new();
        assert!(validate(&catalog).is_ok());
        catalog.roles[0].can_lead_subteam = true;
        let value = serde_json::to_value(&catalog.roles[0]).unwrap();
        assert_eq!(value["can_lead_subteam"], true);
    }
}
