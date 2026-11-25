//! Compact view models of the team reads. Each is projected once in the core
//! from the checked reply and serialized as it is to the webview, which never
//! parses an `hgs` reply. Texts and lists are bounded; a view that left
//! something out says so (`truncated`).
use serde::Serialize;
use serde_json::{json, Map, Value};

/// The longest name, title or short field kept.
pub const NAME_CHARS: usize = 200;
/// The longest free text kept (a goal, a reason, a role prompt).
pub const TEXT_CHARS: usize = 8000;
/// One report opened explicitly; full history never enters the webview.
pub const REPORT_CHARS: usize = 64 * 1024;
/// The longest line kept (a finding, a summary).
pub const LINE_CHARS: usize = 600;
/// Teams one list keeps.
pub const LIST_KEPT: usize = 1000;
/// Members one team keeps.
pub const MEMBERS_KEPT: usize = 256;
/// Tasks one team keeps.
pub const TASKS_KEPT: usize = 5000;
/// Open proposals one team keeps (hgs keeps at most 50).
pub const PROPOSALS_KEPT: usize = 50;
/// Items one inbox, Waiting page, attention list or finding list keeps.
pub const ITEMS_KEPT: usize = 200;
/// The largest proposal change or event detail kept as it is (bytes of JSON).
pub const DETAIL_BYTES: usize = 64 * 1024;

fn cut(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((at, _)) => text[..at].to_owned(),
        None => text.to_owned(),
    }
}

fn text(value: &Value, key: &str, chars: usize) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|text| cut(text, chars))
        .unwrap_or_default()
}

fn name(value: &Value, key: &str) -> String {
    text(value, key, NAME_CHARS)
}

fn number(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

fn count(value: &Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(|found| {
            found
                .as_u64()
                .or_else(|| found.as_f64().map(|n| n.max(0.0) as u64))
        })
        .unwrap_or(0)
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// A list of strings, or of objects with `text`, bounded.
fn lines(value: &Value, key: &str, chars: usize) -> Vec<String> {
    array(value, key)
        .iter()
        .take(ITEMS_KEPT)
        .filter_map(|item| {
            item.as_str()
                .map(str::to_owned)
                .or_else(|| item.get("text").and_then(Value::as_str).map(str::to_owned))
        })
        .map(|line| cut(&line, chars))
        .collect()
}

fn numbers(value: &Value, key: &str) -> Vec<u64> {
    array(value, key)
        .iter()
        .take(TASKS_KEPT)
        .filter_map(Value::as_u64)
        .collect()
}

/// A JSON value kept as it is when small, else null.
fn bounded(value: Option<&Value>) -> Value {
    match value {
        Some(found)
            if serde_json::to_string(found).is_ok_and(|json| json.len() <= DETAIL_BYTES) =>
        {
            found.clone()
        }
        _ => Value::Null,
    }
}

/// A member's session: the computer alias (empty: the team's computer) and
/// its name, with the run and conversation it was bound to.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SessionView {
    pub host: String,
    pub name: String,
    pub run_id: String,
    pub conversation_id: String,
}

fn session(value: &Value) -> Option<SessionView> {
    let found = value.get("session").filter(|session| session.is_object())?;
    let view = SessionView {
        host: name(found, "host"),
        name: name(found, "name"),
        run_id: name(found, "run_id"),
        conversation_id: name(found, "conversation_id"),
    };
    (!view.name.is_empty()).then_some(view)
}

// ---------------------------------------------------------------- the list

/// One member of a listed team, enough to find a session's team.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ListedMember {
    pub id: String,
    pub role: String,
    pub agent: String,
    pub state: String,
    pub lead: bool,
    pub session: Option<SessionView>,
}

/// One team of `team list`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TeamSummary {
    pub id: String,
    pub name: String,
    pub status: String,
    pub project: String,
    pub folder: String,
    pub plan_mode: String,
    /// The lead's member id.
    pub lead: String,
    pub members: Vec<ListedMember>,
    pub tasks: u64,
    pub accepted: u64,
    pub submitted: u64,
    pub in_review: u64,
    pub in_test: u64,
    pub attention: u64,
    pub open_proposals: u64,
    pub waiting_for_lead: bool,
    pub pending_phase: Option<u64>,
    pub updated: f64,
    pub revision: u64,
    /// Task 52: the team whose task this sub-team came from ("" for a team
    /// with no parent), so a tree action counts the whole tree from the list.
    pub parent: String,
    /// Task #58: the phases that plan in free mode, from the list's
    /// `phase_modes` (empty from an older hgs).
    pub free_phases: Vec<u64>,
    /// The phase in work and its two mini bars (Qt `Teams::phaseProgress`):
    /// the lowest unfinished phase's done/total, and all phases' done/total.
    pub phase: u64,
    pub phase_done: u64,
    pub phase_total: u64,
    pub phases_done: u64,
    pub phases_total: u64,
}

impl TeamSummary {
    pub fn draft(&self) -> bool {
        self.status == "draft"
    }
}

/// The teams of `team list`, newest first as the store lists them.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TeamList {
    pub teams: Vec<TeamSummary>,
    pub truncated: bool,
}

/// The phase in work and the two bars (Qt `Teams::phaseProgress`): the lowest
/// non-draft phase with open tasks, else the next draft, else the last phase.
/// A team whose list has no phase records still gets one bar over all tasks.
struct Progress {
    phase: u64,
    phase_done: u64,
    phase_total: u64,
    phases_done: u64,
    phases_total: u64,
}

fn phase_progress(item: &Value) -> Progress {
    use std::collections::BTreeMap;
    // Task #6: hgs gives the strip's numbers; a peer's older hgs gives the
    // tasks, and the numbers are counted from them as before.
    if let Some(given) = item.get("progress").filter(|progress| progress.is_object()) {
        return Progress {
            phase: count(given, "phase"),
            phase_done: count(given, "phase_done"),
            phase_total: count(given, "phase_total"),
            phases_done: count(given, "phases_done"),
            phases_total: count(given, "phases_total"),
        };
    }
    let mut groups: BTreeMap<u64, (u64, u64)> = BTreeMap::new();
    for task in array(item, "tasks") {
        let phase = task.get("phase").and_then(Value::as_u64).unwrap_or(1);
        let finished = matches!(task.get("status").and_then(Value::as_str), Some("accepted" | "cancelled"));
        let entry = groups.entry(phase).or_default();
        entry.1 += 1;
        if finished { entry.0 += 1; }
    }
    let phases_total = groups.len() as u64;
    let phases_done = groups.values().filter(|(done, total)| done == total).count() as u64;
    if groups.is_empty() {
        return Progress { phase: 0, phase_done: 0, phase_total: 0, phases_done, phases_total };
    }
    let (phase, (phase_done, phase_total)) = groups
        .iter()
        .find(|(_, (done, total))| done < total)
        .or_else(|| groups.iter().next_back())
        .map(|(phase, counts)| (*phase, *counts))
        .unwrap();
    Progress { phase, phase_done, phase_total, phases_done, phases_total }
}

pub fn list(value: &Value) -> TeamList {
    let items = array(value, "items");
    let teams = items
        .iter()
        .take(LIST_KEPT)
        .map(|item| {
            let lead = name(item, "lead");
            let progress = phase_progress(item);
            TeamSummary {
                id: name(item, "id"),
                name: name(item, "name"),
                status: name(item, "status"),
                project: name(item, "project"),
                folder: text(item, "folder", LINE_CHARS),
                plan_mode: name(item, "plan_mode"),
                members: array(item, "members")
                    .iter()
                    .take(MEMBERS_KEPT)
                    .map(|member| ListedMember {
                        id: name(member, "id"),
                        role: name(member, "role"),
                        agent: name(member, "agent"),
                        state: name(member, "state"),
                        lead: member.get("id").and_then(Value::as_str) == Some(lead.as_str()),
                        session: session(member),
                    })
                    .collect(),
                lead,
                // Task #6: the counts hgs gives, else (a peer's older hgs)
                // counted from the arrays it sent.
                tasks: if item.get("task_count").is_some() {
                    count(item, "task_count")
                } else {
                    array(item, "tasks").len() as u64
                },
                accepted: count(item, "accepted"),
                submitted: count(item, "submitted"),
                in_review: count(item, "in_review"),
                in_test: count(item, "in_test"),
                attention: if item.get("attention_count").is_some() {
                    count(item, "attention_count")
                } else {
                    array(item, "attention").len() as u64
                },
                open_proposals: match item.get("open_proposals") {
                    Some(given) if !given.is_array() => count(item, "open_proposals"),
                    _ => array(item, "proposals")
                        .iter()
                        .filter(|proposal| proposal["state"] == "open")
                        .count() as u64,
                },
                waiting_for_lead: flag(item, "waiting_for_lead"),
                pending_phase: item.get("pending_phase").and_then(Value::as_u64),
                updated: number(item, "updated"),
                revision: count(item, "revision"),
                parent: item.get("parent").map(|parent| name(parent, "team")).unwrap_or_default(),
                phase: progress.phase,
                phase_done: progress.phase_done,
                phase_total: progress.phase_total,
                phases_done: progress.phases_done,
                phases_total: progress.phases_total,
                free_phases: item
                    .get("phase_modes")
                    .and_then(Value::as_object)
                    .map(|modes| {
                        modes
                            .iter()
                            .filter(|(_, mode)| mode.as_str() == Some("free"))
                            .filter_map(|(n, _)| n.parse::<u64>().ok())
                            .take(ITEMS_KEPT)
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        })
        .collect();
    TeamList {
        teams,
        truncated: items.len() > LIST_KEPT,
    }
}

// --------------------------------------------------------------- one team

/// One member of a team, as the Team tab's members column shows it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MemberView {
    pub id: String,
    pub role: String,
    pub agent: String,
    pub model: String,
    pub effort: String,
    pub account: String,
    pub computer: String,
    pub folder: String,
    /// `not_created`, `active`, `paused`, `removed`...
    pub state: String,
    /// What its session does now (`busy`, `idle`...), and the hand-off phase.
    pub activity: String,
    pub phase: String,
    pub error: String,
    pub lead: bool,
    pub session: Option<SessionView>,
    /// The session was seen live at the last listing; None: never seen.
    pub live: Option<bool>,
    pub stuck: bool,
    /// The member waits on a question of its own.
    pub question: bool,
    /// Its open task first, then the tasks returned to it.
    pub queue: Vec<u64>,
    /// The owner's mandatory additions to the member's rules, as lines.
    pub iron_rules: Vec<String>,
    pub minutes_without_progress: u64,
    pub host_shared: bool,
    /// Task 98: hand-outs its Terminal refused in a row (a draft, a dialog or
    /// an unknown prompt), hgs's reason, and whether the lead and the owner
    /// were told.
    pub input_refusals: u64,
    pub input_reason: String,
    pub input_raised: bool,
    /// Task 134: the member's unread team messages (status's
    /// `members[].unread`), and how many of them are urgent.
    pub unread: u64,
    pub unread_urgent: u64,
    /// Task 11: the member's pending question or permission request as hgs
    /// sends it (`members[].question`, at most [`DETAIL_BYTES`]): its id,
    /// hash, tool, command, reason and the card's own allow and deny ids.
    pub permission: Value,
    /// Task 133: the member's latest pulse (`members[].pulse`): what it did,
    /// does next and is blocked on, in at most three short lines; None
    /// without one.
    pub pulse: Option<PulseView>,
    /// Task 209: the member's native context reading (`members[].context`):
    /// tokens used, the window and their percentage, so the card shows it
    /// without an inspect of its own. None when its agent keeps no meter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextView>,
    /// Task 209: the member's context is being compacted (status reads its
    /// state `compacting`); the card shows Compacting instead of Working.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub compacting: bool,
    /// Task 199 (Qt member card): the member it reports to (`parent`, the
    /// lead when empty) and whether it is a sub-lead with reports of its own.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub parent: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub sub: bool,
    /// U49: it finishes its open work, takes nothing new, then leaves.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub draining: bool,
    /// H1: stopped at a usage limit (its reset time, when known), and whether
    /// Zerus continues it after the reset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limited: Option<LimitView>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub limit_resume: bool,
    /// U54: the account its session really runs on; None from an hgs that
    /// does not say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_account: Option<String>,
    /// G7: the services its session runs (MCP servers and the like), bounded.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceView>,
    /// C12: its background job's declared time budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<BudgetView>,
    /// U45: the models Zerus may switch it to before a task ({model, effort}).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<Value>,
    /// Task 315 (Qt U17/C4, `members[].permission_answers`): its answered
    /// permission requests, oldest first, the last [`PERMISSION_ANSWERS_KEPT`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub permission_answers: Vec<PermissionAnswerView>,
}

/// Task 315: one answered permission request of a member (Qt's
/// `PermissionAnswer`): what was decided (`allow` or `deny`), who decided it
/// (`owner`, `lead`, `rule`, `timeout`), when, the tool and command it asked
/// about and the deny text.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PermissionAnswerView {
    pub question_id: String,
    pub decision: String,
    pub by: String,
    pub reason: String,
    pub tool: String,
    pub command: String,
    pub at: f64,
}

/// hgs keeps a member's last 20 answers; so does the card.
pub const PERMISSION_ANSWERS_KEPT: usize = 20;

fn permission_answers(value: &Value) -> Vec<PermissionAnswerView> {
    let answers: Vec<PermissionAnswerView> = array(value, "permission_answers")
        .iter()
        .map(|answer| PermissionAnswerView {
            question_id: name(answer, "question_id"),
            decision: name(answer, "decision"),
            by: name(answer, "by"),
            reason: text(answer, "reason", LINE_CHARS),
            tool: name(answer, "tool"),
            command: text(answer, "command", LINE_CHARS),
            at: number(answer, "at"),
        })
        .filter(|answer| !answer.decision.is_empty())
        .collect();
    let skip = answers.len().saturating_sub(PERMISSION_ANSWERS_KEPT);
    answers.into_iter().skip(skip).collect()
}

/// Task 209: a member's native context reading as the member card shows it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ContextView {
    pub used: u64,
    pub limit: u64,
    /// Whole percent of the window, rounded down by hgs.
    pub percent: u64,
    /// When hgs read it.
    pub at: f64,
}

fn context(value: &Value) -> Option<ContextView> {
    let context = value.get("context").filter(|context| context.is_object())?;
    Some(ContextView {
        used: count(context, "used"),
        limit: count(context, "limit"),
        percent: count(context, "percent").min(100),
        at: number(context, "at"),
    })
}

/// A member's pulse as the Team tab shows it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PulseView {
    pub task: Option<u64>,
    pub done: String,
    pub next: String,
    pub blocked: String,
    pub at: f64,
    /// `note` (the member's own) or `auto` (Zerus's summary of the turn).
    pub source: String,
}

fn pulse(value: &Value) -> Option<PulseView> {
    let pulse = value.get("pulse").filter(|pulse| pulse.is_object())?;
    Some(PulseView {
        task: pulse.get("task").and_then(Value::as_u64),
        done: text(pulse, "done", LINE_CHARS),
        next: text(pulse, "next", LINE_CHARS),
        blocked: text(pulse, "blocked", LINE_CHARS),
        at: pulse.get("at").and_then(Value::as_f64).unwrap_or_default(),
        source: name(pulse, "source"),
    })
}

/// Task 199: an intake key of the team.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct IntakeView {
    pub id: String,
    pub label: String,
    pub created: f64,
}

/// Task 199: a member stopped at a usage limit.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LimitView {
    pub at: f64,
    pub resets_at: f64,
    pub detail: String,
}

/// Task 199: one service a member's session runs.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ServiceView {
    pub name: String,
    pub since: f64,
    pub command: String,
}

/// Task 199: a member's background job budget.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct BudgetView {
    pub label: String,
    pub state: String,
    pub minutes: u64,
    pub red_minutes: u64,
    pub elapsed_minutes: u64,
}

/// Task 199: the services kept per member.
const SERVICES_KEPT: usize = 12;

/// One dependency: the task it waits for, hard or soft.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DepView {
    pub task: u64,
    pub hard: bool,
}

/// One task, as the plan, its cards and the graph show it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TaskView {
    pub archived: bool,
    pub id: u64,
    pub title: String,
    pub status: String,
    pub assignee: String,
    pub phase: u64,
    pub deps: Vec<DepView>,
    pub estimate_minutes: f64,
    pub attempts: u64,
    /// The newest attempt's state (`delivered`, `reported`...), empty without one.
    pub attempt_state: String,
    /// Task 11: the newest attempt's id, which Use last reply names.
    pub attempt_id: String,
    /// Task 11: the newest attempt's member got no report in (`no_report`).
    pub no_report: bool,
    pub decided_by: String,
    pub pinned: bool,
    pub output: Vec<String>,
    /// What the task waits for, as hgs words it (`waits.unblock`).
    pub waits: String,
    /// Task 40: the owner holds it (`held`).
    pub held: bool,
    /// Task 160: "parked until <condition>, ETA HH:MM (in N min)", as hgs
    /// status words it; empty when the task is not parked.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub parked: String,
    /// Task 40: the pipeline chosen in the plan, and its run (stages and the
    /// open stage) when one started.
    pub pipeline: String,
    pub pipeline_run: Option<PipelineRunView>,
    /// Task 40: the task delegates to a sub-team (`subteam`).
    pub subteam: bool,
    /// Task 52: that sub-team's id, for Cancel's and Reassign's question.
    pub subteam_team: String,
    /// Task #60: the task this follow-up was added for ("spawned from"); it
    /// never blocks, unlike `deps`.
    pub origin: Option<u64>,
    /// Task #229: `crit` or `minor` for a follow-up; empty for ordinary work.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub severity: String,
    /// Task 55: the newest attempt's open questions to the lead (A3): the
    /// owner answers them with `hgs team reply` too.
    pub questions: Vec<QuestionView>,
    /// Task 196: what the plan's task editor shows of a task not yet handed
    /// out (draft or pending): its description (bounded) and criteria.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub criteria: Vec<String>,
    /// Task 197: where the estimate comes from (`lead`, `role`, `team`);
    /// empty from an older hgs, which names no estimate (Qt estimateKnown).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub estimate_source: String,
    /// Task 197: the minutes hgs counted for the open attempt
    /// (`estimate_spent_minutes`), when it names them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimate_spent: Option<f64>,
    /// Task 197 (U37): the running attempt's start, 0 when none runs
    /// (`timing.running_since`).
    #[serde(skip_serializing_if = "is_zero")]
    pub running_since: f64,
    /// Task 197 (U37): the seconds the task's attempts worked (`timing.worked_s`).
    #[serde(skip_serializing_if = "is_zero")]
    pub worked_s: f64,
    /// Task 197 (C3): what an open task waits for (`waits.for`: deps, owner,
    /// member...) and on whom (`waits.on`), for "after #3" on its card.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub wait_for: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub wait_on: Vec<String>,
    /// Task 197: the newest attempt began (it was started or its turn ended),
    /// which tells "starting" from "idle" on its card.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub attempt_begun: bool,
    /// Task #226: the plan lane (U27) the task is in; absent for lane 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane: Option<LaneView>,
}

/// Task #226: a plan lane (U27): its number (above 0) and the label every task
/// in it carries.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LaneView {
    pub n: u64,
    pub label: String,
}

fn lane(value: &Value) -> Option<LaneView> {
    let found = value.get("lane").filter(|lane| lane.is_object())?;
    let n = count(found, "n");
    (n > 0).then(|| LaneView {
        n,
        label: name(found, "label"),
    })
}

fn is_zero(value: &f64) -> bool {
    *value == 0.0
}

/// Task 55: one open question of a member to the lead.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct QuestionView {
    pub id: String,
    pub text: String,
    pub at: f64,
}

fn open_questions(value: &Value) -> Vec<QuestionView> {
    let Some(attempt) = array(value, "attempts").last() else {
        return Vec::new();
    };
    array(attempt, "questions")
        .iter()
        .filter(|question| {
            text(question, "answer", 1).is_empty()
                && question["answered_at"].as_f64().unwrap_or(0.0) <= 0.0
        })
        .take(ITEMS_KEPT)
        .map(|question| QuestionView {
            id: name(question, "id"),
            text: text(question, "text", TEXT_CHARS),
            at: number(question, "at"),
        })
        .filter(|question| !question.id.is_empty())
        .collect()
}

/// Task 40: a task's pipeline run (`pipeline_run`): its stage kinds in order,
/// how it is accepted, and the stage that is open now, if any.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PipelineRunView {
    pub name: String,
    pub accept: String,
    pub round: u64,
    pub max_rounds: u64,
    pub stages: Vec<String>,
    /// The open run's stage index, kind and member (None: no stage runs).
    pub open: Option<StageRunView>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StageRunView {
    pub stage: u64,
    pub kind: String,
    pub member: String,
    pub state: String,
}

fn pipeline_run(value: &Value) -> Option<PipelineRunView> {
    let run = value.get("pipeline_run").filter(|run| run.is_object())?;
    let last = array(run, "runs").last();
    // A run is open while its state is one hgs keeps open (Qt StageRun::open).
    let open = last
        .filter(|last| matches!(last["state"].as_str(), Some("waiting" | "delivered")))
        .map(|last| StageRunView {
            stage: count(last, "stage"),
            kind: name(last, "kind"),
            member: name(last, "member"),
            state: name(last, "state"),
        });
    Some(PipelineRunView {
        name: name(run, "name"),
        accept: name(run, "accept"),
        round: count(run, "round"),
        max_rounds: count(run, "max_rounds"),
        stages: array(run, "stages")
            .iter()
            .take(ITEMS_KEPT)
            .map(|stage| name(stage, "kind"))
            .collect(),
        open,
    })
}

/// One phase of the plan (`phase_summary`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PhaseView {
    pub archived: bool,
    pub n: u64,
    pub title: String,
    pub state: String,
    pub status: String,
    pub done: u64,
    pub total: u64,
    pub barrier: bool,
    pub auto: bool,
    pub acceptance: String,
    pub paused: bool,
    pub health: String,
    /// Task #58: the mode the phase plans in, "standard" or "free" (its own,
    /// else the team's); "" from an hgs that sends no phase modes.
    pub mode: String,
    /// Task #229: "plan" or "minor"; empty from an older hgs.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// Task #229: the open critical tasks holding the phase.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub crit: Vec<u64>,
    /// Task #229: why the phase's critical tasks hold it, as hgs words it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub crit_gate: String,
    /// Task 196: the phase header's details from the phase's record (Qt's
    /// TeamWorkspace phase header): the lead's note, the owner's send-back
    /// comment and whether the lead reworked it, who proposed it and when,
    /// whether Auto accepted it, why it waits ("Starts after ..."), its
    /// pause's author and reason, the lead's edits since, and whether it
    /// waits for the owner's acceptance.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub rejection: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub reworked: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub added_by: String,
    #[serde(skip_serializing_if = "is_zero_f64")]
    pub added_at: f64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub accepted_by_auto: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub start_ahead: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub waiting: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub pause_by: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub pause_reason: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pause_owner: bool,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub changes: u64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub owner_accepts: bool,
}

fn is_zero_f64(value: &f64) -> bool {
    *value == 0.0
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

/// One open proposal, as its card shows it: hgs's own summary when the change
/// carries one (an owner-gated operation), else the kind.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ProposalView {
    pub id: String,
    pub kind: String,
    pub by: String,
    pub at: f64,
    pub reason: String,
    /// The content hash an answer names (`expected_hash`).
    pub hash: String,
    pub summary: String,
    pub lines: Vec<String>,
    /// The exact change (at most [`DETAIL_BYTES`]), for the kinds that list it.
    pub change: Value,
    pub member: Value,
    /// Task 11: a next goal's text and a plan mode switch's mode, which the
    /// proposal carries beside its change.
    pub goal: String,
    pub mode: String,
}

/// One finding on the Findings page (D2), with the owner's state and a replan
/// proposal when it has one.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct OwnerFindingView {
    pub id: String,
    pub from_task: String,
    pub severity: String,
    pub title: String,
    pub file: String,
    pub line: u64,
    pub scenario: String,
    pub task: String,
    pub state: String,
    pub by: String,
    pub at: f64,
    pub note: String,
    pub proposal: String,
    pub phase: u64,
}

/// The first page of the team's findings (`hgs team status`'s `findings`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct FindingsView {
    pub items: Vec<OwnerFindingView>,
    pub next_cursor: String,
}

fn owner_findings(value: Option<&Value>) -> Option<FindingsView> {
    let _page = value?.as_object()?;
    Some(FindingsView {
        items: array(value?, "items")
            .iter()
            .take(ITEMS_KEPT)
            .map(|item| OwnerFindingView {
                id: name(item, "id"),
                from_task: name(item, "from_task"),
                severity: name(item, "severity"),
                title: text(item, "title", LINE_CHARS),
                file: text(item, "file", LINE_CHARS),
                line: count(item, "line"),
                scenario: text(item, "scenario", TEXT_CHARS),
                task: item.get("task").map(|task| match task {
                    Value::String(line) => line.clone(),
                    _ => task.to_string(),
                }).unwrap_or_default(),
                state: name(item, "state"),
                by: name(item, "by"),
                at: number(item, "at"),
                note: text(item, "note", TEXT_CHARS),
                proposal: text(item, "proposal", TEXT_CHARS),
                phase: count(item, "phase"),
            })
            .collect(),
        next_cursor: name(value?, "next_cursor"),
    })
}

/// One preflight finding.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct FindingView {
    pub kind: String,
    pub text: String,
    pub task: Option<u64>,
    pub member: String,
}

fn findings(value: Option<&Value>, key: &str) -> Vec<FindingView> {
    value
        .map(|found| array(found, key))
        .unwrap_or(&[])
        .iter()
        .take(ITEMS_KEPT)
        .map(|finding| match finding {
            Value::String(line) => FindingView {
                text: cut(line, LINE_CHARS),
                ..Default::default()
            },
            _ => FindingView {
                kind: name(finding, "kind"),
                text: text(finding, "text", LINE_CHARS),
                task: finding.get("task").and_then(Value::as_u64),
                member: name(finding, "member"),
            },
        })
        .collect()
}

/// The preflight: what blocks Create team or Approve, and what only warns.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Validation {
    pub errors: Vec<FindingView>,
    pub warnings: Vec<FindingView>,
}

/// The plan's health line: its critical path, parallelism and warnings.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct HealthView {
    pub critical_path: u64,
    pub parallelism: f64,
    pub warnings: Vec<String>,
}

/// A draft's own state: its revision, the fields the lead changed that the
/// owner has not seen, and the ones the owner edited.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DraftView {
    pub revision: u64,
    pub unseen: Vec<String>,
    pub owner_edited: Vec<String>,
    /// Task #13 (team ede724c4): the lead's values for the owner's settings,
    /// by field (an object; empty when none), applied with one click.
    pub setting_offers: Value,
}

/// `team status` for the Team tab.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TeamView {
    pub archived_phases_not_shown: u64,
    pub id: String,
    pub name: String,
    pub status: String,
    pub project: String,
    /// Task #308: the team's working mode and the project proposal.
    #[serde(skip_serializing_if = "Value::is_null")]
    pub working_mode: Value,
    pub folder: String,
    pub goal: String,
    pub plan_mode: String,
    /// Task #58: the phase that takes tasks as they come (`free_phase`), if any.
    pub free_phase: Option<u64>,
    /// Task #58: whether hgs sends each phase's mode (`phase_modes`), so the
    /// owner can switch one phase (`phase-mode`).
    pub phase_modes_known: bool,
    pub lead: String,
    pub revision: u64,
    pub created: f64,
    pub updated: f64,
    pub waiting_for_lead: bool,
    pub pending_phase: Option<u64>,
    /// F7: the team a next goal came from, and the one it led to.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub previous: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub next: String,
    /// Task 112: a paused team's pause ({at, by, reason}), or null.
    pub paused: Value,
    /// The team's settings object as hgs stores it (a few dozen fields).
    pub settings: Value,
    pub members: Vec<MemberView>,
    pub tasks: Vec<TaskView>,
    pub phases: Vec<PhaseView>,
    pub proposals: Vec<ProposalView>,
    /// The attention items as hgs words them (bounded).
    pub attention: Vec<Value>,
    /// Task 11: additions to a running phase that wait for the owner's
    /// confirmation (`pending_additions`, each bounded): id, phase, tasks.
    pub additions: Vec<Value>,
    /// Task 40: the plan's permission widenings ({member, role, from, to}),
    /// which Approve & Run confirms separately (Qt Team::widenings).
    pub widenings: Vec<Value>,
    /// Task 199 (Qt TeamSettingsDialog, U51): the team's intake keys, never
    /// the secret, which hgs shows once at creation.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub intakes: Vec<IntakeView>,
    /// Task 40: the team's direct sub-teams ({team, name, status}), for the
    /// tree confirmations of Pause and Stop.
    pub children: Vec<Value>,
    pub validation: Validation,
    /// D2: the first page of the findings, when hgs sends it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub findings: Option<FindingsView>,
    pub health: HealthView,
    pub ready: Vec<u64>,
    pub cycle: Vec<u64>,
    pub draft: Option<DraftView>,
    /// Members, tasks or proposals over their caps were left out.
    pub truncated: bool,
}

fn member(value: &Value, lead: &str) -> MemberView {
    MemberView {
        id: name(value, "id"),
        role: name(value, "role"),
        agent: name(value, "agent"),
        model: name(value, "model"),
        effort: name(value, "effort"),
        account: name(value, "account"),
        computer: name(value, "computer"),
        folder: name(value, "folder"),
        state: name(value, "state"),
        activity: name(value, "activity"),
        phase: name(value, "phase"),
        error: text(value, "error", LINE_CHARS),
        lead: value.get("id").and_then(Value::as_str) == Some(lead),
        session: session(value),
        live: value
            .get("seen")
            .and_then(|seen| seen.get("live"))
            .and_then(Value::as_bool),
        stuck: value
            .get("stuck")
            .is_some_and(|stuck| !stuck.is_null() && stuck != &Value::Bool(false)),
        question: value
            .get("question")
            .is_some_and(|question| !question.is_null()),
        queue: array(value, "queue")
            .iter()
            .take(ITEMS_KEPT)
            .filter_map(|entry| {
                entry
                    .as_u64()
                    .or_else(|| entry.get("task").and_then(Value::as_u64))
            })
            .collect(),
        iron_rules: lines(value, "iron_rules", TEXT_CHARS),
        minutes_without_progress: count(value, "minutes_without_progress"),
        host_shared: flag(value, "host_shared"),
        input_refusals: value.get("input_block").map_or(0, |block| count(block, "count")),
        input_reason: value.get("input_block").map_or_else(String::new, |block| text(block, "reason", LINE_CHARS)),
        input_raised: value.get("input_block").is_some_and(|block| flag(block, "raised")),
        unread: value.get("unread").map_or(0, |unread| count(unread, "count")),
        unread_urgent: value.get("unread").map_or(0, |unread| count(unread, "urgent")),
        parent: name(value, "parent"),
        sub: flag(value, "sub"),
        draining: flag(value, "draining"),
        limited: value.get("limited").filter(|limit| limit.is_object()).map(|limit| LimitView {
            at: number(limit, "at"),
            resets_at: number(limit, "resets_at"),
            detail: text(limit, "detail", LINE_CHARS),
        }),
        limit_resume: flag(value, "limit_resume"),
        session_account: value.get("session_account").and_then(Value::as_str).map(|account| account.chars().take(200).collect()),
        services: array(value, "services")
            .iter()
            .take(SERVICES_KEPT)
            .map(|service| ServiceView {
                name: text(service, "name", 120),
                since: number(service, "since"),
                command: text(service, "command", LINE_CHARS),
            })
            .filter(|service| !service.name.is_empty())
            .collect(),
        models: array(value, "models")
            .iter()
            .take(16)
            .filter(|choice| choice.get("model").and_then(Value::as_str).is_some_and(|model| !model.is_empty()))
            .map(|choice| json!({"model": name(choice, "model"), "effort": name(choice, "effort")}))
            .collect(),
        permission_answers: permission_answers(value),
        budget: value.get("budget").filter(|budget| budget.is_object()).map(|budget| BudgetView {
            label: text(budget, "label", LINE_CHARS),
            state: name(budget, "state"),
            minutes: count(budget, "minutes"),
            red_minutes: count(budget, "red_minutes"),
            elapsed_minutes: count(budget, "elapsed_minutes"),
        }),
        permission: bounded(
            value
                .get("question")
                .filter(|question| question.is_object()),
        ),
        pulse: pulse(value),
        context: context(value),
        compacting: value.get("state").and_then(Value::as_str) == Some("compacting")
            || value.get("compacting").is_some_and(Value::is_object),
    }
}

fn task(value: &Value) -> TaskView {
    let attempts = array(value, "attempts");
    let archive = value.get("archived").filter(|archive| archive.is_object());
    let timing = archive.and_then(|archive| archive.get("timing")).or_else(|| value.get("timing"));
    TaskView {
        archived: archive.is_some(),
        id: count(value, "id"),
        title: name(value, "title"),
        status: name(value, "status"),
        assignee: name(value, "assignee"),
        phase: count(value, "phase"),
        deps: array(value, "deps")
            .iter()
            .take(ITEMS_KEPT)
            .filter_map(|dep| {
                let task = dep
                    .as_u64()
                    .or_else(|| dep.get("task").and_then(Value::as_u64))?;
                Some(DepView {
                    task,
                    hard: dep.get("kind").and_then(Value::as_str) != Some("soft"),
                })
            })
            .collect(),
        estimate_minutes: number(value, "estimate_minutes"),
        attempts: attempts.len() as u64,
        attempt_state: attempts
            .last()
            .map(|attempt| name(attempt, "state"))
            .unwrap_or_default(),
        attempt_id: attempts
            .last()
            .map(|attempt| name(attempt, "id"))
            .unwrap_or_default(),
        no_report: attempts
            .last()
            .is_some_and(|attempt| flag(attempt, "no_report")),
        decided_by: name(value, "decided_by"),
        pinned: flag(value, "pinned"),
        output: lines(value, "output", LINE_CHARS),
        waits: value
            .get("waits")
            .map(|waits| text(waits, "unblock", LINE_CHARS))
            .unwrap_or_default(),
        held: flag(value, "held"),
        parked: text(value, "parked", LINE_CHARS),
        pipeline: name(value, "pipeline"),
        pipeline_run: pipeline_run(value),
        subteam: value.get("subteam").is_some_and(Value::is_object),
        subteam_team: value.get("subteam").map(|subteam| name(subteam, "team")).unwrap_or_default(),
        origin: value.get("origin").and_then(Value::as_u64).filter(|origin| *origin > 0),
        severity: text(value, "severity", LINE_CHARS),
        questions: open_questions(value),
        description: if editable_task(value) { text(value, "description", EDITOR_CHARS) } else { String::new() },
        criteria: if editable_task(value) { lines(value, "criteria", LINE_CHARS) } else { Vec::new() },
        estimate_source: name(value, "estimate_source"),
        estimate_spent: value.get("estimate_spent_minutes").and_then(Value::as_f64).filter(|minutes| minutes.is_finite() && *minutes >= 0.0),
        running_since: value.get("timing").map(|timing| number(timing, "running_since")).unwrap_or_default().max(0.0),
        worked_s: timing.map(|timing| number(timing, "worked_s")).unwrap_or_default().max(0.0),
        wait_for: value.get("waits").map(|waits| name(waits, "for")).unwrap_or_default(),
        wait_on: value
            .get("waits")
            .and_then(|waits| waits.get("on"))
            .and_then(Value::as_array)
            .map(|on| {
                on.iter()
                    .take(ITEMS_KEPT)
                    .filter_map(|item| item.as_u64().map(|id| id.to_string()).or_else(|| item.as_str().map(|text| text.chars().take(80).collect())))
                    .collect()
            })
            .unwrap_or_default(),
        attempt_begun: attempts
            .last()
            .is_some_and(|attempt| number(attempt, "started_at") > 0.0 || number(attempt, "turn_ended_at") > 0.0),
        lane: lane(value),
    }
}

/// Task 196: the longest description the plan's task editor is given.
const EDITOR_CHARS: usize = 4000;

/// Task 196: a task the plan's editor may change: not yet handed out.
fn editable_task(value: &Value) -> bool {
    matches!(name(value, "status").as_str(), "draft" | "pending") && array(value, "attempts").is_empty()
}

/// Task #58: phase `n`'s mode from status's `phase_modes`, else the team's
/// `plan_mode` for a phase it does not name; "" from an older hgs.
fn phase_mode(team: &Value, n: u64) -> String {
    let Some(modes) = team.get("phase_modes").and_then(Value::as_object) else {
        return String::new();
    };
    match modes.get(&n.to_string()).and_then(Value::as_str) {
        Some(mode @ ("free" | "standard")) => mode.to_owned(),
        _ if name(team, "plan_mode") == "free" => "free".into(),
        _ => "standard".into(),
    }
}

fn phase(value: &Value) -> PhaseView {
    PhaseView {
        archived: value.get("archived").is_some_and(Value::is_object),
        n: count(value, "n"),
        title: name(value, "title"),
        state: name(value, "state"),
        status: name(value, "status"),
        done: count(value, "done"),
        total: count(value, "total"),
        barrier: flag(value, "barrier"),
        auto: flag(value, "auto"),
        acceptance: name(value, "acceptance"),
        paused: value.get("pause").is_some_and(|pause| !pause.is_null()),
        health: text(value, "health_line", LINE_CHARS),
        mode: String::new(),
        kind: name(value, "kind"),
        crit: array(value, "crit").iter().filter_map(Value::as_u64).take(ITEMS_KEPT).collect(),
        crit_gate: text(value, "crit_gate", LINE_CHARS),
        start_ahead: flag(value, "start_ahead"),
        pause_by: value.get("pause").map(|pause| name(pause, "by")).unwrap_or_default(),
        pause_reason: value.get("pause").map(|pause| text(pause, "reason", LINE_CHARS)).unwrap_or_default(),
        pause_owner: value.get("pause").is_some_and(|pause| flag(pause, "owner")),
        owner_accepts: name(value, "acceptance") == "owner",
        ..PhaseView::default()
    }
}

/// Task 196: the details only the phase's record carries (`phases`).
fn phase_record(view: &mut PhaseView, record: &Value) {
    view.note = text(record, "note", TEXT_CHARS);
    if let Some(rejection) = record.get("rejection").filter(|rejection| rejection.is_object()) {
        view.rejection = text(rejection, "comment", TEXT_CHARS);
        view.reworked = number(rejection, "reworked_at") > 0.0;
    }
    view.added_by = name(record, "added_by");
    view.added_at = number(record, "added_at");
    view.accepted_by_auto = matches!(name(record, "state").as_str(), "approved" | "queued") && name(record, "decided_by") == "Auto";
    if let Some(waiting) = record.get("waiting").filter(|waiting| waiting.is_object()) {
        view.waiting = text(waiting, "text", LINE_CHARS);
    }
    view.changes = record.get("changes").and_then(Value::as_array).map_or(0, |changes| changes.len() as u64);
    if view.pause_by.is_empty() {
        if let Some(pause) = record.get("pause").filter(|pause| pause.is_object()) {
            view.pause_by = name(pause, "by");
            view.pause_reason = text(pause, "reason", LINE_CHARS);
            view.pause_owner = flag(pause, "owner");
        }
    }
}

/// One explicitly opened task; only the newest available report is retained.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TaskDetailView {
    pub task: TaskView,
    pub report: String,
    pub report_truncated: bool,
}

pub fn task_detail(value: &Value) -> TaskDetailView {
    let report = array(value, "attempts").iter().rev()
        .find_map(|attempt| attempt.get("report").and_then(|report| report.get("text")).and_then(Value::as_str))
        .unwrap_or_default();
    TaskDetailView {
        task: task(value),
        report: cut(report, REPORT_CHARS),
        report_truncated: report.chars().nth(REPORT_CHARS).is_some(),
    }
}

fn proposal(value: &Value) -> ProposalView {
    let change = value.get("change").unwrap_or(&Value::Null);
    let summary = text(change, "summary", LINE_CHARS);
    ProposalView {
        id: name(value, "id"),
        kind: name(value, "kind"),
        by: name(value, "by"),
        at: number(value, "at"),
        reason: text(value, "reason", TEXT_CHARS),
        hash: name(value, "hash"),
        summary: if summary.is_empty() {
            name(value, "kind")
        } else {
            summary
        },
        lines: lines(change, "lines", LINE_CHARS),
        change: bounded(value.get("change")),
        member: bounded(value.get("member")),
        goal: text(value, "goal", TEXT_CHARS),
        mode: name(value, "mode"),
    }
}

/// Task 40: the plan's permission widenings, as Qt reads them: each member's
/// `profile_widening`, then the team's own (an object or a list).
fn widenings(value: &Value) -> Vec<Value> {
    let one = |widening: &Value, member: &str, role: &str| {
        let from = name(widening, "from");
        let to = name(widening, "to");
        (!from.is_empty() || !to.is_empty()).then(|| {
            let named = name(widening, "member");
            json!({"member": if named.is_empty() { member.to_owned() } else { named },
                "role": if role.is_empty() { name(widening, "role") } else { role.to_owned() }, "from": from, "to": to})
        })
    };
    let mut out: Vec<Value> = array(value, "members")
        .iter()
        .filter_map(|member| {
            one(
                &member["profile_widening"],
                &name(member, "id"),
                &name(member, "role"),
            )
        })
        .collect();
    match value.get("profile_widening") {
        Some(Value::Array(list)) => {
            out.extend(list.iter().filter_map(|widening| one(widening, "", "")))
        }
        Some(widening) if widening.is_object() => out.extend(one(widening, "", "")),
        _ => {}
    }
    out.truncate(ITEMS_KEPT);
    out
}

/// Projects `team status` for the Team tab.
pub fn team(value: &Value) -> TeamView {
    let lead = name(value, "lead");
    let members = array(value, "members");
    let tasks = array(value, "tasks");
    let open: Vec<&Value> = array(value, "proposals")
        .iter()
        .filter(|proposal| proposal["state"] == "open")
        .collect();
    let validation = value.get("validation");
    let health = value
        .get("health")
        .or_else(|| validation.and_then(|found| found.get("health")));
    TeamView {
        archived_phases_not_shown: value["archive_totals"]["phases"].as_u64().unwrap_or(0)
            .saturating_sub(array(value, "phases").iter().filter(|phase| phase["archived"].is_object()).count() as u64),
        id: name(value, "id"),
        name: name(value, "name"),
        status: name(value, "status"),
        project: name(value, "project"),
        working_mode: value.get("working_mode").filter(|mode| mode.is_object()).map_or(Value::Null, |mode| bounded(Some(mode))),
        folder: text(value, "folder", LINE_CHARS),
        goal: text(value, "goal", TEXT_CHARS),
        plan_mode: name(value, "plan_mode"),
        free_phase: value.get("free_phase").and_then(|free| free.get("phase")).and_then(Value::as_u64),
        phase_modes_known: value.get("phase_modes").is_some_and(Value::is_object),
        revision: count(value, "revision"),
        created: number(value, "created"),
        updated: number(value, "updated"),
        waiting_for_lead: flag(value, "waiting_for_lead"),
        pending_phase: value.get("pending_phase").and_then(Value::as_u64),
        previous: name(value, "previous"),
        next: name(value, "next"),
        paused: value.get("paused").filter(|paused| paused.is_object()).map_or(Value::Null, |paused| bounded(Some(paused))),
        settings: match value.get("settings") {
            Some(Value::Object(settings)) => Value::Object(settings.clone()),
            _ => Value::Object(Map::new()),
        },
        members: members
            .iter()
            .take(MEMBERS_KEPT)
            .map(|found| member(found, &lead))
            .collect(),
        tasks: tasks.iter().take(TASKS_KEPT).map(task).collect(),
        phases: array(value, "phase_summary")
            .iter()
            .take(ITEMS_KEPT)
            .map(|found| {
                let mut view = phase(found);
                view.mode = phase_mode(value, view.n);
                view.archived = array(value, "phases").iter()
                    .any(|record| record["n"] == found["n"] && record["archived"].is_object());
                if let Some(record) = array(value, "phases").iter().find(|record| count(record, "n") == view.n) {
                    phase_record(&mut view, record);
                }
                view
            })
            .collect(),
        proposals: open
            .iter()
            .take(PROPOSALS_KEPT)
            .map(|found| proposal(found))
            .collect(),
        attention: array(value, "attention")
            .iter()
            .take(ITEMS_KEPT)
            .map(|item| bounded(Some(item)))
            .collect(),
        additions: array(value, "pending_additions")
            .iter()
            .take(ITEMS_KEPT)
            .map(|item| bounded(Some(item)))
            .collect(),
        widenings: widenings(value),
        intakes: array(value, "intakes")
            .iter()
            .take(8)
            .map(|intake| IntakeView { id: name(intake, "id"), label: text(intake, "label", 120), created: number(intake, "created") })
            .filter(|intake| !intake.id.is_empty())
            .collect(),
        children: array(value, "children")
            .iter()
            .take(ITEMS_KEPT)
            .map(|child| json!({"team": name(child, "team"), "name": name(child, "name"), "status": name(child, "status")}))
            .collect(),
        validation: Validation {
            errors: findings(validation, "errors"),
            warnings: findings(validation, "warnings"),
        },
        findings: owner_findings(value.get("findings")),
        health: HealthView {
            critical_path: health
                .map(|found| count(found, "critical_path"))
                .unwrap_or(0),
            parallelism: health
                .map(|found| number(found, "parallelism"))
                .unwrap_or(0.0),
            warnings: health
                .map(|found| lines(found, "warnings", LINE_CHARS))
                .unwrap_or_default(),
        },
        ready: numbers(value, "ready"),
        cycle: numbers(value, "cycle"),
        draft: value
            .get("draft")
            .filter(|draft| draft.is_object())
            .map(|draft| DraftView {
                revision: count(draft, "revision"),
                unseen: lines(draft, "unseen", NAME_CHARS),
                owner_edited: lines(draft, "owner_edited", NAME_CHARS),
                setting_offers: match bounded(draft.get("setting_offers")) {
                    offers @ Value::Object(_) => offers,
                    _ => Value::Object(Map::new()),
                },
            }),
        truncated: members.len() > MEMBERS_KEPT
            || tasks.len() > TASKS_KEPT
            || open.len() > PROPOSALS_KEPT,
        lead,
    }
}

// -------------------------------------------------------------- the graph

/// One task of the plan graph, in its layer.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct GraphNode {
    pub id: u64,
    pub title: String,
    pub status: String,
    pub assignee: String,
    /// The assignee's role, the lead's "Lead".
    pub role: String,
    pub phase: u64,
    /// The column the graph draws it in (hgs's `layers`).
    pub layer: u64,
    pub ready: bool,
    pub in_cycle: bool,
    /// Task #60: the task it was spawned from, when the graph has it; drawn
    /// as a link, never as a dependency edge.
    pub origin: Option<u64>,
}

/// One edge: `to` depends on `from`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct GraphEdge {
    pub from: u64,
    pub to: u64,
    pub hard: bool,
}

/// The plan graph of one team: its tasks in layers and their dependencies.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PlanGraph {
    pub team: String,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// How many layers the nodes take.
    pub layers: u64,
}

/// Projects the plan graph from `team status` (its `layers`) and its view.
pub fn graph(value: &Value, view: &TeamView) -> PlanGraph {
    let layers = value.get("layers").and_then(Value::as_object);
    let role = |id: &str| -> String {
        if id == view.lead {
            return "Lead".to_owned();
        }
        view.members
            .iter()
            .find(|member| member.id == id)
            .map(|member| member.role.clone())
            .unwrap_or_default()
    };
    let known: std::collections::BTreeSet<u64> = view.tasks.iter().map(|task| task.id).collect();
    let nodes: Vec<GraphNode> = view
        .tasks
        .iter()
        .map(|task| GraphNode {
            id: task.id,
            title: task.title.clone(),
            status: task.status.clone(),
            assignee: task.assignee.clone(),
            role: role(&task.assignee),
            phase: task.phase,
            layer: layers
                .and_then(|layers| layers.get(&task.id.to_string()))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            ready: view.ready.contains(&task.id),
            in_cycle: view.cycle.contains(&task.id),
            origin: task.origin.filter(|origin| known.contains(origin)),
        })
        .collect();
    let edges = view
        .tasks
        .iter()
        .flat_map(|task| {
            task.deps
                .iter()
                .filter(|dep| known.contains(&dep.task))
                .map(move |dep| GraphEdge {
                    from: dep.task,
                    to: task.id,
                    hard: dep.hard,
                })
        })
        .collect();
    PlanGraph {
        team: view.id.clone(),
        layers: nodes.iter().map(|node| node.layer + 1).max().unwrap_or(0),
        nodes,
        edges,
    }
}

// ---------------------------------------------------------------- New team

/// One member of a draft, with what New team edits.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DraftMember {
    pub id: String,
    pub role: String,
    pub agent: String,
    pub model: String,
    pub effort: String,
    pub account: String,
    pub permissions: String,
    pub computer: String,
    pub folder: String,
    pub prompt: String,
    pub lead: bool,
    pub session: Option<SessionView>,
}

/// A draft team, as the New team form shows it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct NewTeamView {
    pub team: String,
    pub name: String,
    pub goal: String,
    /// The lead protocol addendum.
    pub addendum: String,
    pub folder: String,
    pub settings: Value,
    /// Task #308: the proposed working mode, its reasons and the accepted preset.
    #[serde(skip_serializing_if = "Value::is_null")]
    pub working_mode: Value,
    /// Task #8 (team ede724c4): the plan mode the owner chose ("standard" or
    /// "free"), the lead's offer ("" for none) and the lead's one-line reason.
    pub plan_mode: String,
    pub plan_mode_offer: String,
    pub plan_mode_reason: String,
    pub members: Vec<DraftMember>,
    pub tasks: Vec<TaskView>,
    /// Owner, 2026-11-09: the draft's phases for New team's Phases page.
    pub phases: Vec<PhaseView>,
    pub draft: DraftView,
    pub validation: Validation,
    pub updated: f64,
}

/// Projects a draft's `team status` for New team; None for a team that is
/// not a draft.
pub fn new_team(value: &Value, view: &TeamView) -> Option<NewTeamView> {
    if view.status != "draft" {
        return None;
    }
    Some(NewTeamView {
        team: view.id.clone(),
        name: view.name.clone(),
        goal: view.goal.clone(),
        addendum: text(value, "lead_protocol", TEXT_CHARS),
        folder: view.folder.clone(),
        settings: view.settings.clone(),
        working_mode: bounded(value.get("working_mode")),
        // hgs leaves out the standard mode; an unknown one reads as standard.
        plan_mode: if view.plan_mode == "free" {
            "free".into()
        } else {
            "standard".into()
        },
        plan_mode_offer: match name(value, "plan_mode_offer").as_str() {
            offer @ ("standard" | "free") => offer.into(),
            _ => String::new(),
        },
        plan_mode_reason: text(value, "plan_mode_reason", LINE_CHARS),
        members: array(value, "members")
            .iter()
            .take(MEMBERS_KEPT)
            .map(|member| DraftMember {
                id: name(member, "id"),
                role: name(member, "role"),
                agent: name(member, "agent"),
                model: name(member, "model"),
                effort: name(member, "effort"),
                account: name(member, "account"),
                permissions: name(member, "permissions"),
                computer: name(member, "computer"),
                folder: name(member, "folder"),
                prompt: text(member, "prompt", TEXT_CHARS),
                lead: member.get("id").and_then(Value::as_str) == Some(view.lead.as_str()),
                session: session(member),
            })
            .collect(),
        tasks: view.tasks.clone(),
        phases: view.phases.clone(),
        draft: view.draft.clone().unwrap_or_default(),
        validation: view.validation.clone(),
        updated: view.updated,
    })
}

// ----------------------------------------------------------- feed and more

/// One journal event of the team's feed.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct FeedEvent {
    pub seq: i64,
    pub at: f64,
    pub kind: String,
    pub detail: Value,
}

/// One page of `team events`: its events, newest first, and the cursor of the
/// page before them (None: the journal's start).
pub fn feed_page(value: &Value) -> (Vec<FeedEvent>, Option<i64>) {
    let events = array(value, "items")
        .iter()
        .filter_map(|item| {
            Some(FeedEvent {
                seq: item.get("seq").and_then(Value::as_i64)?,
                at: number(item, "at"),
                kind: name(item, "kind"),
                detail: bounded(item.get("detail")),
            })
        })
        .collect();
    (
        events,
        value
            .get("next_cursor")
            .and_then(Value::as_i64)
            .filter(|cursor| *cursor > 0),
    )
}

/// The feed the view holds: the newest events first, at most
/// [`crate::teams::FEED_KEPT`].
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct FeedView {
    pub team: String,
    pub events: Vec<FeedEvent>,
    /// More events before the oldest held: Load older can ask for them.
    pub more: bool,
    /// Older events were left out to keep the feed bounded.
    pub capped: bool,
}

/// One item of the lead's inbox.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct InboxItem {
    pub kind: String,
    pub member: String,
    pub task: Option<u64>,
    pub summary: String,
    pub since: f64,
    pub command: String,
    pub detail: Value,
}

/// `team inbox`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct InboxView {
    pub team: String,
    pub items: Vec<InboxItem>,
    pub truncated: bool,
}

pub fn inbox(team: &str, value: &Value) -> InboxView {
    let items = array(value, "items");
    InboxView {
        team: team.to_owned(),
        items: items
            .iter()
            .take(ITEMS_KEPT)
            .map(|item| InboxItem {
                kind: name(item, "kind"),
                member: name(item, "member"),
                task: item.get("task").and_then(Value::as_u64),
                summary: text(item, "summary", LINE_CHARS),
                since: number(item, "since"),
                command: text(item, "command", LINE_CHARS),
                detail: bounded(item.get("detail")),
            })
            .collect(),
        truncated: items.len() > ITEMS_KEPT,
    }
}

/// One thing that waits for the owner.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct WaitingItem {
    pub id: String,
    pub kind: String,
    pub team: String,
    pub team_name: String,
    pub title: String,
    pub action: String,
    pub since: f64,
    pub proposal: Value,
    /// Task 11: the whole item as hgs lists it (at most [`DETAIL_BYTES`]):
    /// its phase, task, member, member id or tool offer, which an answer in
    /// place names.
    pub detail: Value,
    /// Task 304 (hgs #246): the situation the row stands for; Dismiss names
    /// it, and a changed state brings a dismissed row back.
    pub state: String,
}

/// The first page of `team waiting`, with how many wait in all.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct WaitingView {
    pub items: Vec<WaitingItem>,
    pub total: u64,
    /// More items wait after this page.
    pub more: bool,
    /// The cursor the next older page sends, as hgs words it.
    pub next: Option<Value>,
    /// Task 304 (hgs #246): the rows the owner dismissed, apart from the
    /// count; a decision that still blocks work says so (`blocking`, `blocks`).
    pub dismissed: Vec<WaitingItem>,
    pub dismissed_total: u64,
}

fn waiting_item(item: &Value) -> WaitingItem {
    WaitingItem {
        id: name(item, "id"),
        kind: name(item, "kind"),
        team: name(item, "team"),
        team_name: name(item, "team_name"),
        title: text(item, "title", LINE_CHARS),
        action: name(item, "action"),
        since: number(item, "since"),
        proposal: bounded(item.get("proposal")),
        detail: bounded(Some(item)),
        state: text(item, "state", LINE_CHARS),
    }
}

pub fn waiting(value: &Value) -> WaitingView {
    WaitingView {
        items: array(value, "items").iter().take(ITEMS_KEPT).map(waiting_item).collect(),
        dismissed: array(value, "dismissed").iter().take(ITEMS_KEPT).map(waiting_item).collect(),
        dismissed_total: count(value, "dismissed_total"),
        total: count(value, "total"),
        more: value.get("next").is_some_and(|next| !next.is_null()),
        next: value
            .get("next")
            .filter(|next| next.is_object())
            .cloned(),
    }
}

/// One page of `team spans`: each task's spans as hgs gives them, and the
/// task after which the next page starts.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SpansView {
    pub team: String,
    pub tasks: Vec<Value>,
    pub next: Option<u64>,
    pub truncated: bool,
}

pub fn spans(team: &str, value: &Value) -> SpansView {
    SpansView {
        team: team.to_owned(),
        tasks: array(value, "tasks")
            .iter()
            .take(ITEMS_KEPT)
            .map(|task| bounded(Some(task)))
            .collect(),
        next: value.get("next_cursor").and_then(Value::as_u64),
        truncated: value
            .get("events")
            .is_some_and(|events| flag(events, "truncated")),
    }
}

/// One loss the profile names (a rework, a wait for a member, the lead or a
/// dependency); `tokens` is null when hgs has no count, never zero.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ProfileLossView {
    pub kind: String,
    pub task: u64,
    pub seconds: f64,
    pub tokens: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ProfileRoleView {
    pub key: String,
    pub kind: String,
    pub accepted: u64,
    pub attempts: u64,
    pub returned: u64,
    pub tokens_per_accepted: Option<u64>,
}

/// `team profile`, projected: the summary, the top time losses and the roles
/// and models. Unknown token counts stay null.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ProfileView {
    pub team: String,
    pub elapsed_s: Option<u64>,
    pub worked_s: Option<u64>,
    pub tasks: u64,
    pub accepted: u64,
    pub tokens: Option<u64>,
    pub tokens_partial: bool,
    pub losses: Vec<ProfileLossView>,
    pub roles: Vec<ProfileRoleView>,
    /// Task 82: each task's lateness against the plan and overrun against
    /// its estimate, from the profile's first page of task rows.
    pub task_rows: Vec<ProfileTaskView>,
    /// More task rows follow the first page; their tasks show no heat.
    pub task_rows_more: bool,
}

/// Task 82: a task's lateness against the plan baseline. `known` is false
/// when the plan lacks a moment it needs (an estimate is missing); `from` is
/// the predecessor the inherited part comes from.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ProfileLatenessView {
    pub known: bool,
    pub unknown: String,
    pub planned_start: Option<f64>,
    pub planned_end: Option<f64>,
    pub actual_start: Option<f64>,
    pub actual_end: Option<f64>,
    pub start_late_s: i64,
    pub inherited_s: i64,
    pub own_s: i64,
    pub from: Option<u64>,
}

/// Task 82: a task's own work against its estimate; `known` is false
/// without an estimate, `started` false before its first attempt.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ProfileOverrunView {
    pub started: bool,
    pub known: bool,
    pub estimate_s: i64,
    pub work_s: i64,
    pub overrun_s: i64,
    pub ratio: f64,
    pub running: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ProfileTaskView {
    pub task: u64,
    /// Null when the task has no plan baseline (planned before it existed).
    pub lateness: Option<ProfileLatenessView>,
    pub overrun: Option<ProfileOverrunView>,
}

fn profile_task(row: &Value) -> Option<ProfileTaskView> {
    let task = row
        .get("task")
        .and_then(Value::as_u64)
        .filter(|task| *task > 0)?;
    let int = |value: &Value, key: &str| value.get(key).and_then(Value::as_i64);
    let moment = |value: &Value, key: &str| value.get(key).and_then(Value::as_f64);
    let lateness = row
        .get("lateness")
        .filter(|late| late.is_object())
        .map(|late| {
            let unknown = late
                .get("unknown")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            ProfileLatenessView {
                known: unknown.is_empty() && int(late, "start_late_s").is_some(),
                unknown,
                planned_start: moment(late, "planned_start"),
                planned_end: moment(late, "planned_end"),
                actual_start: moment(late, "actual_start"),
                actual_end: moment(late, "actual_end"),
                start_late_s: int(late, "start_late_s").unwrap_or(0),
                inherited_s: int(late, "inherited_s").unwrap_or(0),
                own_s: int(late, "own_s").unwrap_or(0),
                from: late.get("from").and_then(Value::as_u64),
            }
        });
    let overrun = row
        .get("overrun")
        .filter(|over| over.is_object())
        .map(|over| ProfileOverrunView {
            started: !flag(over, "not_started"),
            known: int(over, "overrun_s").is_some() && int(over, "estimate_s").is_some(),
            estimate_s: int(over, "estimate_s").unwrap_or(0),
            work_s: int(over, "work_s").unwrap_or(0),
            overrun_s: int(over, "overrun_s").unwrap_or(0),
            ratio: over.get("ratio").and_then(Value::as_f64).unwrap_or(0.0),
            running: flag(over, "running"),
        });
    Some(ProfileTaskView {
        task,
        lateness,
        overrun,
    })
}

pub fn profile(team: &str, value: &Value) -> ProfileView {
    let summary = value.get("summary").unwrap_or(&Value::Null);
    let tokens = summary.get("tokens").unwrap_or(&Value::Null);
    let total = tokens.get("total").and_then(Value::as_u64);
    let roles = [
        ("role", array(value, "roles")),
        ("model", array(value, "models")),
    ]
    .into_iter()
    .flat_map(|(kind, rows)| {
        rows.iter().take(PROPOSALS_KEPT).map(move |row| {
            let role = name(row, "role");
            let model = name(row, "model");
            ProfileRoleView {
                key: if !role.is_empty() { role } else if !model.is_empty() { model } else { name(row, "key") },
                kind: kind.to_owned(),
                accepted: row.get("accepted").and_then(Value::as_u64).unwrap_or(0),
                attempts: row.get("attempts").and_then(Value::as_u64).unwrap_or(0),
                returned: row.get("returned").and_then(Value::as_u64).unwrap_or(0),
                tokens_per_accepted: row.get("tokens_per_accepted").and_then(Value::as_u64),
            }
        })
    })
    .collect();
    ProfileView {
        team: team.to_owned(),
        elapsed_s: summary
            .get("totals")
            .and_then(|totals| totals.get("elapsed_s"))
            .and_then(Value::as_u64),
        worked_s: summary
            .get("totals")
            .and_then(|totals| totals.get("worked_s"))
            .and_then(Value::as_u64),
        tasks: summary.get("tasks").and_then(Value::as_u64).unwrap_or(0),
        accepted: summary.get("accepted").and_then(Value::as_u64).unwrap_or(0),
        tokens: total,
        tokens_partial: tokens
            .get("partial")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        losses: array(
            value
                .get("losses")
                .and_then(|losses| losses.get("by_time"))
                .unwrap_or(&Value::Null),
            "items",
        )
        .iter()
        .take(ITEMS_KEPT)
        .map(|loss| ProfileLossView {
            kind: name(loss, "kind"),
            task: loss.get("task").and_then(Value::as_u64).unwrap_or(0),
            seconds: number(loss, "s"),
            tokens: loss.get("tokens").and_then(Value::as_u64),
        })
        .collect(),
        roles,
        task_rows: array(value.get("tasks").unwrap_or(&Value::Null), "items")
            .iter()
            .take(ITEMS_KEPT)
            .filter_map(profile_task)
            .collect(),
        task_rows_more: value
            .get("tasks")
            .and_then(|tasks| tasks.get("next_cursor"))
            .is_some_and(|next| !next.is_null()),
    }
}

/// One test request still in the queue.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TestPendingView {
    pub id: String,
    pub suite: String,
    pub asked_by: String,
    pub state: String,
    pub task: u64,
    pub forced: bool,
    pub started: f64,
    pub seconds: f64,
    pub reason: String,
}

/// One recorded test run.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TestRunView {
    pub id: String,
    pub suite: String,
    pub status: String,
    pub asked_by: String,
    pub task: u64,
    pub at: f64,
    pub duration_ms: u64,
    pub recorded_ms: u64,
    pub runner: String,
    pub excerpt: String,
    pub flags: Vec<String>,
    pub reused_from: String,
    pub due: bool,
    pub over_budget: bool,
    /// Task 88: one result per test from the provider's report (counts,
    /// note and at most `REPORT_RESULTS` results, failures first), or null.
    pub report: Value,
    pub executor_load: Option<ExecutorLoadView>,
}

#[derive(Clone,Debug,Default,PartialEq,Serialize)]
pub struct ExecutorLoadView{pub valid:bool,pub loaded:bool,pub sample_kind:String,pub cpu_percent:Option<f64>,pub disk_percent:Option<f64>,pub disk_busy_percent:Option<f64>,pub waited_ms:u64}
fn executor_load(run:&Value)->Option<ExecutorLoadView>{
 let load=run.get("load")?;if load["origin"]!="executor-host"{return None}
 let during=load.get("during").filter(|sample|sample.is_object());let sample=during.or_else(||load.get("before")).unwrap_or(&Value::Null);
 let percent=|key:&str|sample.get(key).and_then(Value::as_f64).filter(|n|n.is_finite()&&(0.0..=100.0).contains(n));
 let cpu=percent("cpu_percent");let disk=percent("disk_percent");let busy=percent("disk_busy_percent");
 Some(ExecutorLoadView{valid:flag(load,"valid")&&cpu.is_some()&&disk.is_some()&&busy.is_some(),loaded:flag(load,"loaded"),sample_kind:if during.is_some(){"during_peak"}else{"before"}.into(),cpu_percent:cpu,disk_percent:disk,disk_busy_percent:busy,waited_ms:load["waited_ms"].as_u64().unwrap_or(0)})
}

/// Task 88: the results of one run the Tests mode shows; the counts cover the rest.
const REPORT_RESULTS: usize = 200;

/// Task 88: a run's report as the view carries it, every text bounded.
fn test_report(value: Option<&Value>) -> Value {
    let Some(report) = value.filter(|report| report.is_object()) else { return Value::Null };
    let results: Vec<Value> = report["results"]
        .as_array()
        .into_iter()
        .flatten()
        .take(REPORT_RESULTS)
        .map(|result| {
            json!({"suite":text(result, "suite", LINE_CHARS),"name":text(result, "name", LINE_CHARS),"status":name(result, "status"),
                "ms":result.get("ms").and_then(Value::as_u64).unwrap_or(0),"message":text(result, "message", LINE_CHARS)})
        })
        .collect();
    let count = |key: &str| report.get(key).and_then(Value::as_u64).unwrap_or(0);
    json!({"passed":count("passed"),"failed":count("failed"),"skipped":count("skipped"),"errors":count("errors"),
        "note":text(report, "note", LINE_CHARS),"results":results})
}

/// `team test` for the Tests mode: the queue and one page of results.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TestsView {
    pub team: String,
    pub queue: Vec<TestPendingView>,
    pub runs: Vec<TestRunView>,
    pub next: Option<f64>,
}

pub fn tests(team: &str, queue: &Value, results: &Value) -> TestsView {
    TestsView {
        team: team.to_owned(),
        queue: array(queue, "items")
            .iter()
            .take(ITEMS_KEPT)
            .map(|item| TestPendingView {
                id: name(item, "id"),
                suite: name(item, "suite"),
                asked_by: name(item, "asked_by"),
                state: name(item, "state"),
                task: item.get("task").and_then(Value::as_u64).unwrap_or(0),
                forced: flag(item, "forced"),
                started: number(item, "started"),
                seconds: number(item, "seconds"),
                reason: text(item, "reason", LINE_CHARS),
            })
            .collect(),
        runs: array(results, "items")
            .iter()
            .take(ITEMS_KEPT)
            .map(|run| TestRunView {
                id: name(run, "id"),
                suite: name(run, "suite"),
                status: name(run, "status"),
                asked_by: name(run, "asked_by"),
                task: run.get("task").and_then(Value::as_u64).unwrap_or(0),
                at: number(run, "at"),
                duration_ms: run.get("duration_ms").and_then(Value::as_u64).unwrap_or(0),
                recorded_ms: run.get("recorded_ms").and_then(Value::as_u64).unwrap_or(0),
                runner: name(run, "runner"),
                excerpt: text(run, "excerpt", TEXT_CHARS),
                flags: lines(run, "flags", NAME_CHARS),
                reused_from: name(run, "reused_from"),
                due: flag(run, "due"),
                over_budget: flag(run, "over_budget"),
                report: test_report(run.get("report")),
                executor_load:executor_load(run),
            })
            .collect(),
        next: results.get("next_cursor").and_then(Value::as_f64),
    }
}
/// One catalog role as the editor lists it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CatalogRoleView {
    pub id: String,
    pub name: String,
    pub agent: String,
    pub model: String,
    pub prompt: String,
    /// Task 42: what New team copies into a member of this role.
    pub effort: String,
    pub account: String,
    pub permissions: String,
    pub computer: String,
    /// `worktree`, `shared` or `read_only`: the role's folder.
    pub folder: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CatalogSeatView {
    pub role: String,
    pub count: u64,
    pub optional: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CatalogTemplateView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub planning: String,
    pub approval: String,
    pub max_members: u64,
    pub pipeline: String,
    pub roster: Vec<CatalogSeatView>,
    /// Task 42: the approval of later phases and the lead protocol New team
    /// takes from the template.
    pub phase_approval: String,
    pub lead_protocol: String,
    /// Task 42: the template's tasks (Use template tasks): title, the role
    /// that does it and the indexes of the tasks it waits for.
    pub tasks: Vec<CatalogTaskView>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CatalogTaskView {
    pub title: String,
    pub role: String,
    pub deps: Vec<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CatalogRuleView {
    pub id: String,
    pub when: String,
    pub template: String,
    pub r#use: String,
    pub approval: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CatalogStageView {
    pub kind: String,
    pub role: String,
    pub member: String,
    pub command: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CatalogPipelineView {
    pub id: String,
    pub name: String,
    pub stages: Vec<CatalogStageView>,
    pub max_rounds: u64,
    pub folder: String,
    pub accept: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CatalogProposalView {
    pub id: String,
    pub kind: String,
    pub action: String,
    pub target: String,
    pub title: String,
    pub reason: String,
    pub state: String,
    pub value: Value,
    pub text: String,
    pub at: f64,
}

/// `team catalog get`: the saved catalog and the designer's draft. Roles,
/// templates, rules, pipelines and proposals are bounded; texts and changes
/// are capped, and the webview never parses an hgs reply.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CatalogView {
    pub version: u64,
    pub builtin: bool,
    pub roles: Vec<CatalogRoleView>,
    pub templates: Vec<CatalogTemplateView>,
    pub routing: Vec<CatalogRuleView>,
    pub planning_rules: String,
    pub pipelines: Vec<CatalogPipelineView>,
    pub draft_base: u64,
    pub proposals: Vec<CatalogProposalView>,
}

pub fn catalog(value: &Value) -> CatalogView {
    let catalog = value.get("catalog").unwrap_or(&Value::Null);
    let draft = value.get("draft").unwrap_or(&Value::Null);
    CatalogView {
        version: catalog.get("version").and_then(Value::as_u64).unwrap_or(0),
        builtin: value
            .get("builtin")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        roles: array(catalog, "roles")
            .iter()
            .take(PROPOSALS_KEPT)
            .map(|role| CatalogRoleView {
                id: name(role, "id"),
                name: name(role, "name"),
                agent: name(role, "agent"),
                model: name(role, "model"),
                prompt: text(role, "prompt", TEXT_CHARS),
                effort: name(role, "effort"),
                account: name(role, "account"),
                permissions: name(role, "permissions"),
                computer: name(role, "computer"),
                folder: name(role, "folder"),
            })
            .collect(),
        templates: array(catalog, "templates")
            .iter()
            .take(PROPOSALS_KEPT)
            .map(|template| CatalogTemplateView {
                id: name(template, "id"),
                name: name(template, "name"),
                description: text(template, "description", TEXT_CHARS),
                planning: name(template, "planning"),
                approval: name(template, "approval"),
                max_members: template
                    .get("max_members")
                    .and_then(Value::as_u64)
                    .unwrap_or(6),
                pipeline: name(template, "pipeline"),
                phase_approval: name(template, "phase_approval"),
                lead_protocol: text(template, "lead_protocol", TEXT_CHARS),
                tasks: array(template, "tasks")
                    .iter()
                    .take(TASKS_KEPT)
                    .map(|task| CatalogTaskView {
                        title: text(task, "title", LINE_CHARS),
                        role: name(task, "role"),
                        deps: array(task, "deps")
                            .iter()
                            .filter_map(Value::as_u64)
                            .collect(),
                    })
                    .collect(),
                roster: array(template, "roster")
                    .iter()
                    .take(MEMBERS_KEPT)
                    .map(|seat| CatalogSeatView {
                        role: name(seat, "role"),
                        count: seat.get("count").and_then(Value::as_u64).unwrap_or(1),
                        optional: seat
                            .get("optional")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                    .collect(),
            })
            .collect(),
        routing: array(catalog, "routing")
            .iter()
            .take(PROPOSALS_KEPT)
            .map(|rule| CatalogRuleView {
                id: name(rule, "id"),
                when: text(rule, "when", TEXT_CHARS),
                template: name(rule, "template"),
                r#use: name(rule, "use"),
                approval: name(rule, "approval"),
            })
            .collect(),
        planning_rules: text(catalog, "planning_rules", TEXT_CHARS),
        pipelines: array(catalog, "pipelines")
            .iter()
            .take(PROPOSALS_KEPT)
            .map(|pipeline| CatalogPipelineView {
                id: name(pipeline, "id"),
                name: name(pipeline, "name"),
                stages: array(pipeline, "stages")
                    .iter()
                    .take(ITEMS_KEPT)
                    .map(|stage| CatalogStageView {
                        kind: name(stage, "kind"),
                        role: name(stage, "role"),
                        member: name(stage, "member"),
                        command: text(
                            stage.get("command").unwrap_or(&Value::Null),
                            "command",
                            TEXT_CHARS,
                        ),
                    })
                    .collect(),
                max_rounds: pipeline
                    .get("max_rounds")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                folder: name(pipeline, "folder"),
                accept: name(pipeline, "accept"),
            })
            .collect(),
        draft_base: draft.get("base").and_then(Value::as_u64).unwrap_or(0),
        proposals: array(draft, "proposals")
            .iter()
            .take(PROPOSALS_KEPT)
            .map(|proposal| {
                let change = proposal.get("value").cloned().unwrap_or(Value::Null);
                CatalogProposalView {
                    id: name(proposal, "id"),
                    kind: name(proposal, "kind"),
                    action: name(proposal, "action"),
                    target: name(proposal, "target"),
                    title: text(proposal, "title", TEXT_CHARS),
                    reason: text(proposal, "reason", TEXT_CHARS),
                    state: text(proposal, "state", NAME_CHARS),
                    text: change.as_str().unwrap_or_default().to_owned(),
                    value: bounded(Some(&change)),
                    at: number(proposal, "at"),
                }
            })
            .collect(),
    }
}


/// Task 199: the Tests tab's Builds (Qt TeamTests builds): the build
/// broker's shared folders, bounded.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct BuildsView {
    pub team: String,
    /// The build broker is on: members sharing a folder build in snapshots.
    pub enabled: bool,
    pub folders: Vec<BuildFolderView>,
    pub deliveries: Vec<ArtifactDeliveryView>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ArtifactDeliveryView {pub job:String,pub profile:String,pub executor:String,pub device:String,pub phase:String,pub instructions:String,pub updated:f64}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct BuildFolderView {
    pub folder: String,
    pub members: Vec<String>,
    pub build_folders: Vec<BuildGroupView>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct BuildGroupView {
    pub name: String,
    pub usual_ms: f64,
    /// The share of recent requests served from an earlier build; None
    /// before any finished.
    pub hit_rate: Option<f64>,
    pub running: Vec<BuildItemView>,
    pub queue: Vec<BuildItemView>,
    pub recent: Vec<BuildItemView>,
}

/// One build: running, waiting or finished.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct BuildItemView {
    pub member: String,
    pub template: String,
    /// When it started, began waiting or finished.
    pub at: f64,
    pub seconds: f64,
    pub usual_ms: f64,
    pub snapshot: bool,
    pub hit: bool,
    pub by: String,
    pub ms: f64,
    pub exit: Option<i64>,
    pub timed_out: bool,
    pub error: String,
    pub coalesced_with: String,
    pub waiting_for: Vec<String>,
}

/// Task 199: the bounds of the Builds view.
const BUILD_FOLDERS_KEPT: usize = 20;
const BUILD_ITEMS_KEPT: usize = 20;

pub fn builds(team: &str, value: &Value) -> BuildsView {
    let item = |entry: &Value| BuildItemView {
        member: name(entry, "member"),
        template: name(entry, "template"),
        at: [number(entry, "started"), number(entry, "since"), number(entry, "at")].into_iter().find(|at| *at > 0.0).unwrap_or(0.0),
        seconds: number(entry, "seconds"),
        usual_ms: number(entry, "usual_ms"),
        snapshot: entry.get("snapshot").and_then(Value::as_bool).unwrap_or(false),
        hit: flag(entry, "hit"),
        by: name(entry, "by"),
        ms: number(entry, "ms"),
        exit: entry.get("exit").and_then(Value::as_i64),
        timed_out: flag(entry, "timed_out"),
        error: text(entry, "error", LINE_CHARS),
        coalesced_with: name(entry, "coalesced_with"),
        waiting_for: array(entry, "waiting_for").iter().take(BUILD_ITEMS_KEPT).filter_map(Value::as_str).map(str::to_owned).collect(),
    };
    let items = |entry: &Value, key: &str| array(entry, key).iter().take(BUILD_ITEMS_KEPT).map(item).collect::<Vec<_>>();
    BuildsView {
        team: team.to_owned(),
        enabled: flag(value, "enabled"),
        deliveries:array(value,"deliveries").iter().take(BUILD_ITEMS_KEPT).map(|row|ArtifactDeliveryView{job:name(row,"job"),profile:name(row,"profile"),executor:name(row,"executor"),device:name(row,"device"),phase:name(row,"phase"),instructions:text(row,"instructions",16384),updated:number(row,"updated")}).collect(),
        folders: array(value, "folders")
            .iter()
            .take(BUILD_FOLDERS_KEPT)
            .map(|folder| BuildFolderView {
                folder: text(folder, "folder", LINE_CHARS),
                members: array(folder, "members").iter().take(ITEMS_KEPT).filter_map(Value::as_str).map(str::to_owned).collect(),
                build_folders: array(folder, "build_folders")
                    .iter()
                    .take(BUILD_FOLDERS_KEPT)
                    .map(|group| BuildGroupView {
                        name: text(group, "name", LINE_CHARS),
                        usual_ms: number(group, "usual_ms"),
                        hit_rate: group.get("hit_rate").and_then(Value::as_f64),
                        running: items(group, "running"),
                        queue: items(group, "queue"),
                        recent: items(group, "timeline"),
                    })
                    .collect(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executor_load_projection_distinguishes_capacity_activity_and_missing_samples(){
        let run=json!({"load":{"origin":"executor-host","valid":true,"loaded":true,"during":{"cpu_percent":70,"disk_percent":80,"disk_busy_percent":20}}});
        let view=executor_load(&run).unwrap();assert!(view.valid&&view.loaded);assert_eq!(view.sample_kind,"during_peak");assert_eq!(view.disk_percent,Some(80.0));assert_eq!(view.disk_busy_percent,Some(20.0));
        let missing=executor_load(&json!({"load":{"origin":"executor-host","valid":true}})).unwrap();assert!(!missing.valid);assert_eq!(missing.cpu_percent,None);
        assert!(executor_load(&json!({"load":{"origin":"builder","valid":true}})).is_none());
    }

    #[test]
    fn a_task_carries_its_lane_and_lane_zero_is_no_lane() {
        let lane = task(&json!({"id":1,"title":"T","lane":{"n":2,"label":"Checkpoint 2"}}));
        assert_eq!(
            lane.lane,
            Some(LaneView { n: 2, label: "Checkpoint 2".into() })
        );
        assert_eq!(task(&json!({"id":2,"title":"T","lane":{"n":0,"label":"x"}})).lane, None);
        assert_eq!(task(&json!({"id":3,"title":"T"})).lane, None);
    }

    #[test]
    fn a_member_carries_its_answered_permissions_as_qt_reads_them() {
        let mut answers: Vec<Value> = (0..23)
            .map(|n| json!({"question_id": format!("q{n}"), "decision": "allow", "by": "lead", "at": 100.0 + n as f64,
                "tool": "Bash", "command": "cargo build"}))
            .collect();
        answers.push(json!({"question_id": "q-no-decision", "by": "owner", "at": 200.0}));
        answers.push(json!({"question_id": "q-last", "decision": "deny", "by": "owner", "reason": "Not in this folder",
            "tool": "Write", "at": 300.0}));
        let view = member(&json!({"id": "dev", "permission_answers": answers}), "lead");
        assert_eq!(view.permission_answers.len(), PERMISSION_ANSWERS_KEPT, "the last 20 with a decision");
        assert_eq!(view.permission_answers[0].question_id, "q4");
        let last = view.permission_answers.last().unwrap();
        assert_eq!(
            (last.decision.as_str(), last.by.as_str(), last.reason.as_str(), last.tool.as_str(), last.at),
            ("deny", "owner", "Not in this folder", "Write", 300.0)
        );
        assert!(member(&json!({"id": "dev"}), "lead").permission_answers.is_empty());
        let shown = serde_json::to_value(member(&json!({"id": "dev"}), "lead")).unwrap();
        assert!(shown.get("permission_answers").is_none(), "an empty list is not sent");
    }
}
