//! WhiteWh fork: the team dispatcher (`hgs team worker`). Each tick it reads
//! every member session once, starts members that have work, hands ready tasks
//! to free members and forwards reports to the lead. A hand-off is recorded
//! before it is sent and never sent again once it may have arrived.
use super::super::*;
use super::handoff;
use super::inbox;
use super::model::*;
use super::working::Urgency;
use super::ops;
use super::plan;
use super::safe_actions;
use super::store::{Event, Store};
use anyhow::Context;
use std::collections::BTreeSet;
use std::fs;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_secs(3);
/// A worker with nothing to serve exits after this long.
const IDLE_EXIT: Duration = Duration::from_secs(300);
/// After a refused hand-off the member gets no new one for this long.
const REFUSAL_PAUSE: f64 = 30.0;
const REFUSAL_PAUSE_MAX: f64 = 15.0 * 60.0;

/// Task 165: a member that keeps refusing waits 30 s, then double, capped.
pub(crate) fn refusal_pause(count: u32) -> f64 {
    (REFUSAL_PAUSE * 2f64.powi(count.saturating_sub(1).min(5) as i32)).min(REFUSAL_PAUSE_MAX)
}
/// How long a member may sit idle without ready work before the lead hears
/// once that the plan starves it (C7).
const STARVED_AFTER: f64 = 20.0 * 60.0;
/// Task 164 (owner, 2026-11-07): the lead gets one team-level starved digest
/// at most once an hour.
const STARVED_DIGEST_EVERY: f64 = 60.0 * 60.0;
/// The worker's session listing is bounded, so a stop request is never far away.
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
/// The worker's log keeps at most this much before it starts over.
const LOG_BYTES: u64 = 64 * 1024;
/// Task #451 (owner, 2026-11-09): how long a cached project profile is trusted.
/// The tick re-reads the project after this, so the working-mode advice follows
/// the repository (and the current classifier) and a stale block corrects itself.
pub(crate) const MODE_CHECK_SECONDS: f64 = 10.0 * 60.0;

/// The worker's directory: its lock, pid, heartbeat, stop request and log.
pub(crate) fn worker_dir() -> PathBuf {
    root().join("teams")
}

pub(crate) fn lock_path() -> PathBuf {
    worker_dir().join("worker.lock")
}

/// One member session as `hgs ls --json` showed it in this tick.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Seen {
    /// SSH alias of the session's computer; empty for this computer.
    pub host: String,
    pub name: String,
    pub run_id: String,
    pub conversation_id: String,
    pub launch_id: String,
    /// Idle, with nothing waiting on the owner and no Background work.
    pub free: bool,
    pub idle: bool,
    pub reply_at: Option<f64>,
    /// The last reply's identity, empty when the record has none (A2).
    pub reply_id: String,
    /// When its current turn started (A1).
    pub turn_started: Option<f64>,
    /// Busy, idle, background or unknown (A1, A2).
    pub activity: String,
    /// Idle, working, tool, input, question, approval, ready or unknown (A2).
    pub phase: String,
    /// Its own jobs run in the background: work in progress, not a finished
    /// turn (`process_work.count > 0`, or activity `background`) (A2).
    pub background: bool,
    /// The newest Background job: its command and when it started (C12).
    pub work: Option<SeenWork>,
    /// #511 N1: a build Zerus runs for the session (`hgs run`, or a claimed
    /// `hgs team run` in its folder) is work, as the listing's `build` says.
    pub building: bool,
    /// Task 36: that build's request id and label, empty when not building.
    pub build_run: String,
    pub build_label: String,
    /// The MCP servers and services it runs, which are not Background work
    /// (G7).
    pub services: Vec<SeenService>,
    /// Task 278: what it waits on: its Zerus build, the background jobs its
    /// hooks registered, Background work under a declared budget.
    pub jobs: Vec<SeenJob>,
    /// Waiting on an answer: a question or a permission request.
    pub asking: bool,
    /// Its process or native session runs.
    pub live: bool,
    /// The listing's own state: running, stopped, paused or archived (U52
    /// restarts only a session that is `stopped`).
    pub state: String,
    /// Its working folder, where a member that may not write the team's
    /// files leaves its report.
    pub cwd: String,
    /// Its notifications are muted, and the revision of that preference
    /// (None when the host does not know it).
    pub muted: bool,
    pub notify_revision: Option<u64>,
    /// H1: its session stopped by a usage limit.
    pub limited: Option<SeenLimit>,
    /// DCB-svn report (2026-11-12): its turn ended on another provider error
    /// while the session stays live.
    pub failed: Option<SeenFailure>,
    /// When its session last reported any event.
    pub last_event_at: Option<f64>,
    /// #728: the tool call its turn is in, and when that call started.
    pub tool: String,
    pub tool_started: Option<f64>,
    /// U45: the model and effort its session record names now.
    pub model: String,
    pub effort: String,
    /// Its agent (claude, codex, dsh, ...): a DeepSeek session's listing
    /// `last_event_at` does not advance with its native tool events (#628).
    pub agent: String,
    /// U54: the account its session really runs on, from the listing.
    pub account_id: String,
    /// Task 254: its prompt cache is cold: the listing's `session_state` says
    /// so, or (from a listing without it) the cache TTL passed since it was
    /// last active.
    pub cold: bool,
}

/// Task 254: whether a listed session's prompt cache is cold.
pub(crate) fn cache_cold(record: &Value, now: f64) -> bool {
    match record["session_state"].as_str() {
        Some(state) => state == "cold",
        None => {
            let ttl = record["cache_ttl"]["seconds"].as_f64().unwrap_or(0.0);
            let active = record["last_active"].as_f64().unwrap_or(0.0);
            ttl > 0.0 && active > 0.0 && now - active >= ttl
        }
    }
}

/// The newest Background job of a session (C12).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SeenWork {
    pub command: String,
    pub started_at: f64,
}

/// Task 278: a job a session waits on (`MemberJob` once recorded).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SeenJob {
    pub kind: String,
    pub what: String,
    pub command: String,
    pub since: f64,
}

/// One line of at most `chars` characters of `text`.
fn short(text: &str, chars: usize) -> String {
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= chars {
        line
    } else {
        format!("{}...", line.chars().take(chars).collect::<String>())
    }
}

/// Task 278: the jobs a listed session waits on: the build Zerus runs for it,
/// the background jobs its hooks registered (a Claude background shell, a
/// Codex exec session, a DeepSeek job) and Background work it waits for under
/// a declared budget.
pub(crate) fn listed_jobs(record: &Value) -> Vec<SeenJob> {
    let mut jobs = Vec::new();
    let build = &record["build"];
    if build.is_object() {
        let label = build["label"].as_str().unwrap_or_default().trim();
        jobs.push(SeenJob {
            kind: "build".into(),
            what: if label.is_empty() { "its own Zerus build".into() } else { format!("its own Zerus build ({})", short(label, 80)) },
            command: build["command"].as_str().unwrap_or_default().to_owned(),
            since: build["since"].as_f64().unwrap_or(0.0),
        });
    }
    for job in record["background_jobs"].as_array().into_iter().flatten().take(8) {
        let command = job["command"].as_str().unwrap_or_default();
        if command.trim().is_empty() {
            continue;
        }
        let description = job["description"].as_str().unwrap_or_default().trim();
        jobs.push(SeenJob {
            kind: "shell".into(),
            what: format!("its background job `{}`", short(if description.is_empty() { command } else { description }, 80)),
            command: command.to_owned(),
            since: job["started_at"].as_f64().unwrap_or(0.0),
        });
    }
    let work = &record["process_work"];
    if work["count"].as_u64().unwrap_or(0) > 0 {
        for job in work["jobs"].as_array().into_iter().flatten().take(8) {
            let command = job["command"].as_str().unwrap_or_default();
            if command.trim().is_empty() || jobs.iter().any(|known| known.command == command) {
                continue;
            }
            jobs.push(SeenJob {
                kind: "job".into(),
                what: format!("its job `{}`", short(command, 80)),
                command: command.to_owned(),
                since: job["started_at"].as_f64().unwrap_or(0.0),
            });
        }
    }
    jobs
}

/// An MCP server or service of a session (G7).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SeenService {
    pub name: String,
    pub command: String,
    pub since: f64,
}

/// H1: a session stopped by a usage limit, with the reset time when the
/// session reports one.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SeenLimit {
    pub at: f64,
    pub resets_at: Option<f64>,
    pub detail: String,
}

/// DCB-svn report (2026-11-12): a turn that ended on a provider error other
/// than a usage limit: when, its words, the recovery class the listing gives
/// it, and whether the shared recovery service already handles it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SeenFailure {
    pub at: f64,
    pub detail: String,
    pub class: String,
    pub recovering: bool,
}

/// The failure `record` (an `hgs ls` row) shows, if its turn ended on a
/// provider error that is not a usage limit while the session lives.
fn seen_failure(record: &Value, live: bool, limited: bool) -> Option<SeenFailure> {
    let error = &record["provider_error"];
    if !live || limited || record["phase"] != "error" || !error.is_object() {
        return None;
    }
    let detail = [&error["detail"], &record["last_error"], &record["activity_summary"]]
        .into_iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .find(|text| !text.is_empty())
        .unwrap_or("Provider error");
    Some(SeenFailure {
        at: error["at"].as_f64().or(record["last_event_at"].as_f64()).unwrap_or(0.0),
        detail: detail.chars().take(300).collect(),
        class: record["recovery_error_class"].as_str().unwrap_or_default().to_owned(),
        recovering: matches!(record["recovery"]["state"].as_str(), Some("waiting" | "dispatching" | "retrying")),
    })
}

/// Task #7: whether a session record is idle and may take a message, the same
/// test the listing's rows go through: live, activity idle, phase empty, idle
/// or ready, no open optional questions, no auth wall and a send that works.
fn record_free(record: &Value) -> bool {
    let field = |key: &str| record[key].as_str().unwrap_or_default();
    let live = field("process_state") != "exited"
        && !matches!(field("state"), "paused" | "stopped" | "archived")
        && (field("state") == "running" || field("process_state") == "running" || field("runtime_state") == "live");
    live && field("activity") == "idle"
        && matches!(field("phase"), "" | "idle" | "ready")
        && record["optional_questions"]["count"].as_u64().unwrap_or(0) == 0
        && record["auth_required"] != true
        && record["send_available"] != false
}

/// Every session of this computer and its peers from one listing.
pub(crate) fn seen(listing: &Value) -> Vec<Seen> {
    let mut sessions = Vec::new();
    for (index, machine) in listing.as_array().into_iter().flatten().enumerate() {
        let host = if index == 0 { String::new() } else { machine["host"].as_str().unwrap_or_default().to_owned() };
        for record in machine["sessions"].as_array().into_iter().flatten() {
            let field = |key: &str| record[key].as_str().unwrap_or_default();
            // Terminal sessions report their process, DeepSeek ones a state.
            // The owner, 2026-11-05: an agent that exited while its terminal
            // stays open is stopped, as the session state says (Agent exited),
            // never live: the team waited on three exited Kimi sessions.
            let exited = field("process_state") == "exited";
            let live = !exited
                && !matches!(field("state"), "paused" | "stopped" | "archived")
                && (field("state") == "running" || field("process_state") == "running" || field("runtime_state") == "live");
            let activity = field("activity").to_owned();
            let idle = live && activity == "idle";
            // Task #7: an idle agent whose own jobs run in the background
            // (Background: a watcher, an MCP server, a long check) still takes
            // messages; the lead's policy asks the same question of its record.
            let free = record_free(record);
            // Background work is not a finished turn, whatever the activity
            // says: a long command, a watcher or a native job still runs (A2).
            // C12: its command and start let a declared budget tell the lead
            // when the job is overdue.
            let work = record["process_work"]
                .as_object()
                .filter(|work| work.get("count").and_then(|count| count.as_u64()).is_some_and(|count| count > 0))
                .map(|work| SeenWork {
                    command: work.get("command").and_then(|command| command.as_str()).unwrap_or_default().to_owned(),
                    started_at: work.get("started_at").and_then(|started| started.as_f64()).unwrap_or(0.0),
                });
            let background = work.is_some() || activity == "background";
            // G7: MCP servers and services ride along with the session and are
            // not Background work.
            let services: Vec<SeenService> = record["process_work"]["services"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|service| {
                    let name = service["name"].as_str().unwrap_or_default();
                    (!name.is_empty()).then(|| SeenService {
                        name: name.to_owned(),
                        command: service["command"].as_str().unwrap_or_default().to_owned(),
                        since: service["since"].as_f64().unwrap_or(0.0),
                    })
                })
                .collect();
            // H1: a turn stopped by a usage limit; the reset time when the
            // session reports one.
            let limited = (field("phase") == "error")
                .then(|| {
                    let error = &record["provider_error"];
                    let text = format!(
                        "{} {}",
                        error["error_kind"].as_str().unwrap_or_default(),
                        error["detail"].as_str().unwrap_or_default()
                    )
                    .to_lowercase();
                    let session_limit = super::super::provider_errors::native_kind(error) == Some("session_limit")
                        || super::super::session_limits::recognized(&text);
                    session_limit.then(|| SeenLimit {
                        at: error["at"].as_f64().unwrap_or(0.0),
                        resets_at: super::super::session_limits::reset_at(error),
                        detail: text.chars().take(300).collect(),
                    })
                })
                .flatten();
            let failed = seen_failure(record, live, limited.is_some());
            sessions.push(Seen {
                host: host.clone(),
                name: field("name").to_owned(),
                run_id: field("run_id").to_owned(),
                conversation_id: field("conversation_id").to_owned(),
                launch_id: field("launch_id").to_owned(),
                free,
                idle,
                reply_at: record["reply_at"].as_f64(),
                reply_id: field("reply_id").to_owned(),
                turn_started: record["turn_started"].as_f64(),
                activity,
                phase: field("phase").to_owned(),
                background,
                work,
                building: record["build"].is_object(),
                build_run: record["build"]["request_id"].as_str().unwrap_or_default().to_owned(),
                build_label: record["build"]["label"].as_str().unwrap_or_default().to_owned(),
                services,
                jobs: if live { listed_jobs(record) } else { Vec::new() },
                asking: live && matches!(field("phase"), "question" | "input" | "approval"),
                last_event_at: record["last_event_at"].as_f64(),
                tool: field("current_tool").to_owned(),
                tool_started: record["tool_started"].as_f64().filter(|at| *at > 0.0),
                agent: field("agent").to_owned(),
                live,
                state: if exited && !matches!(field("state"), "paused" | "archived") { "stopped".to_owned() } else { field("state").to_owned() },
                cwd: field("cwd").to_owned(),
                muted: record["notification_preferences"]["muted"] == true,
                notify_revision: record["notification_preferences"]["revision"].as_u64(),
                limited,
                failed,
                model: field("model").to_owned(),
                effort: field("effort").to_owned(),
                account_id: field("account_id").to_owned(),
                cold: cache_cold(record, now()),
            });
        }
    }
    sessions
}

/// One thing the dispatcher does for a team in a tick.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    /// Task 134: the member's unread team messages ring their doorbell (at an
    /// idle moment only; the second ring after their bound).
    RingMessages { member: String },
    /// Task 277 (owner, 2026-11-08): an urgent message interrupts the
    /// member's busy turn natively; `turn` pins the displayed turn.
    InterruptMessages { member: String, turn: f64, run: String, conversation: String },
    /// Task 277 (owner, 2026-11-08): the stopped turn opens a new one with the
    /// urgent message on top and "continue your task from where you stopped";
    /// the attempt goes on.
    ContinueUrgent { member: String },
    /// Task 270 (owner, 2026-11-08): the running team's settings disagree with
    /// the project's working mode; the lead proposes the switch once, at a
    /// safe point.
    WorkingMode { preset: String, title: String, reasons: Vec<String> },
    /// Task 270: profile the project once and cache the proposal on the team.
    ModeCheck,
    /// Task 271 (owner, 2026-11-08): the lead's brief with the playbook, the
    /// breaches, the owner's standing decisions and the integration line, at
    /// team start, a phase start, or after a compaction or a clear.
    LeadBrief { reason: String },
    /// The member's started session appeared, or its identity changed.
    Adopt { member: String, session: SessionRef },
    Launch { member: String },
    HandOut { task: u32, member: String },
    /// Task 160: a parked task whose condition held goes back to its member.
    Resume { task: u32, member: String },
    /// Task 161: look for a wait loop in a task's work (its runs, repeated
    /// checks); one found becomes a park.
    DetectWait { task: u32 },
    /// Task 156: a new decision binds a task in flight: one message to its member.
    TellDecision { task: u32, decision: String },
    /// Task 156: a report's changes are checked against the decisions.
    ScanReport { task: u32 },
    /// Task 156: the specs' generated Decisions sections are out of date.
    SpecDecisions,
    /// U45: switch the member's model and effort before it gets the task.
    SwitchModel { task: u32, member: String, from: String, model: String, effort: String },
    /// U45: the switch was not confirmed in time; the task waits for the lead.
    ModelUncertain { task: u32 },
    /// U45: no member can run the task's models; it waits with the reason.
    TaskWaits { task: u32, reason: String },
    /// U46: a hand-out waits for its context action's confirmation: one look
    /// per tick, then the hand-out itself or the waiting reason.
    PrepareContext { task: u32, member: String },
    /// U49: a draining member owes nothing: pause its session, then remove it.
    /// `pause` is false when there is no live session to keep (an agent with no
    /// separate process, or a session already stopped).
    Drain { member: String, pause: bool },
    /// The member's first turn after the hand-off began (A1).
    Started { task: u32 },
    /// The hand-out did not start in `start_timeout_seconds` (A1).
    StartLate { task: u32 },
    /// Tells the free lead once that a hand-out has not started (A1).
    TellStartLate { task: u32 },
    /// Reads a member's native question or permission request into the inbox;
    /// clears it once the member no longer asks (C2).
    Permission { member: String },
    /// The member no longer asks: its stored permission request leaves the
    /// inbox (C2).
    ClearPermission { member: String },
    /// The one coalesced message that lists the lead's new inbox items (C2).
    Inbox,
    /// Task 186: the same list for a lead in its own turn, stored without
    /// typing anything; it waits for the lead's next safe point (hard rule 3).
    InboxWaits,
    /// Reminds the free lead once that a report still waits for a decision (C4).
    /// Reminds a sub-lead once that its report waits for a decision (N9).
    RemindReviewer { task: u32, member: String },
    RemindReport { task: u32 },
    /// #628: a DeepSeek member's listing `last_event_at` is older than its
    /// native tool events; the worker reads the session once before it calls
    /// the member silent.
    CheckSilent { task: u32, last: f64 },
    /// The member is idle without a ready task while tasks are open (C7).
    Starving { member: String },
    /// Task #464 (owner, 2026-11-09): the queue is off and accepted work waits
    /// for a landing; the merger that holds the landing task hears it once for
    /// each newly accepted task, so it lands them instead of going cold.
    WakeLanding { member: String, tasks: Vec<u32> },
    /// Task 173: the member's session works with nothing handed to it.
    Unassigned { member: String, what: String, task: Option<u32> },
    UnassignedEnded { member: String },
    /// Task #227: an attempt a restart left unconfirmed goes back to the
    /// restarted session by itself; `busy` says the new run already works on
    /// it, so the attempt is bound instead of sent again.
    RecoverCrash { task: u32, member: String, busy: bool },
    /// Task #227: the queued "continue" notices of a retry reach their members.
    Continues,
    /// The member got work again or nothing is open: the episode ends (C7).
    StarvedEnded { member: String },
    /// Idle without work for too long: tell the free lead once (C7).
    PlanStarved { members: Vec<String> },
    /// The member's Background work appeared; its default red time starts now
    /// (C12).
    BackgroundStart { member: String },
    /// Its Background work ended: the budget ends with the job (C12).
    BackgroundEnd { member: String },
    /// Tie a declared budget to the live Background job (C12).
    BudgetTie { member: String, command: String, since: f64 },
    /// The job passed its estimate (C12).
    BudgetAmber { member: String },
    /// The job passed its red time: tell the lead once (C12).
    BudgetRed { member: String },
    /// Background work without a budget passed the default red time (C12).
    BackgroundRed { member: String, command: String, since: f64 },
    /// The owner, 2026-10-28: a member in its turn with no event for a long
    /// time and no question Zerus can see (the reviewer's hidden subagent
    /// prompts held it 74 minutes): the lead hears once per attempt.
    Silent { task: u32, minutes: u64 },
    /// The MCP servers and services the member's session runs changed (G7).
    Services { member: String, services: Vec<SeenService> },
    /// Task 278: what the member waits on changed.
    Jobs { member: String, jobs: Vec<SeenJob> },
    /// H1: the member's session was stopped by a usage limit, or recovered
    /// from it (None).
    Limited { member: String, limited: Option<MemberLimit> },
    /// DCB-svn report (2026-11-12): the member's turn ended on a provider
    /// error (or that error is over); stored, and the lead hears it once.
    TurnFailed { member: String, failed: Option<TurnFailure> },
    /// DCB-svn report: after a temporary provider error the member holding
    /// work is asked once to continue, at most FAILURE_CONTINUES_PER_HOUR.
    ContinueFailed { member: String },
    /// H1: the team setting asks the recovery service to wait for this
    /// member's reset and continue it.
    StartLimitResume { member: String },
    /// H1: keep the recovery request fresh while the wait lasts.
    RefreshLimitResume { member: String },
    /// H1: the limit episode ended or the setting is off: stop waiting.
    StopLimitResume { member: String },
    /// Gives the member the lead's answer as a message (A3).
    AnswerQuestion { task: u32, question: String },
    TurnEnded { task: u32, at: f64, reply: String },
    /// Task 36: the member's session is Building on its own run during the
    /// attempt: remember the run, so its end is noticed.
    RunAwaited { task: u32, run: String, label: String },
    /// Task 36: the awaited run settled. `wake`: the member's turn is over
    /// (the session is quiet), so it is told once to read the result and go
    /// on, and its finished turn is not taken as its report; otherwise the member already
    /// works again and the run is only forgotten.
    RunSettled { task: u32, wake: bool },
    /// The report gate was seen open: the report timeout counts from here (C2).
    GateOpen { task: u32 },
    /// A question or Background work holds a finished turn: the timeout
    /// restarts and a long block reaches the lead (C2, N5).
    GateClosed { task: u32 },
    /// Tells the free lead once that a finished turn has been blocked too long (N5).
    BlockedLate { task: u32 },
    /// The member started working again: its recorded turn end is cleared (A2).
    WorkResumed { task: u32 },
    /// Task #49: a No report attempt whose member waits for its own job is no
    /// No report any more.
    OwnJobFound { task: u32 },
    /// The dispatcher saw a member's session live or stopped; status derives
    /// `stopped` from it without a listing of its own (L3).
    Liveness { member: String, live: bool },
    /// Task 95: the lead's stall as this tick sees it: raised, repeated (a new
    /// round), updated or cleared (None).
    LeadStall { stall: Option<LeadStall> },
    /// Task 97: the member's (or the lead's) native request waited past the
    /// team's `defer_minutes`: answer it safely and queue it for the owner.
    /// `question` is its `id:hash`.
    Defer { member: String, question: String },
    /// Task 113: a request only the member's Terminal takes waited for the
    /// defer time: raised once to the owner and the phone with its screen.
    EscalateTerminal { member: String },
    /// Task 139: a suspect member's sources compared, with a screen read;
    /// `hooks` is what its session record said when the pass planned it.
    Check { member: String, hooks: super::look::Hooks },
    /// Task 139: a member no longer suspect drops its last check.
    ClearCheck { member: String },
    /// Task 139: the lead idles while decisions wait (task 95): it is told
    /// once per round, with what waits.
    TellLeadIdle,
    /// U56: a pending task pinned to a stuck, stopped or offline member says so
    /// in its waits.
    WaitOnMember { task: u32, member: String, reason: String },
    /// U56: the member is available again; the wait goes.
    ReleaseWait { task: u32 },
    /// Task 154: no member the task may go to can do it with its launch
    /// rights; it waits for the lead (reassign) or a rights card.
    RightsWait { task: u32, reason: String },
    /// Task 154: a member's rights changed while its session ran: pause it
    /// once it is idle and launch it again in its conversation with them.
    RelaunchRights { member: String },
    /// U56/#728: the lead or the owner restarted a stuck member: interrupt its
    /// session and resume it in its conversation.
    RestartStuck { member: String },
    /// Task 209: the member's context reached the team's threshold while it was
    /// idle mid-task: compact it (or give it a fresh conversation) and continue
    /// the same attempt.
    CompactIdle { member: String },
    /// Task 254: a crashed attempt waits for its member's cold, large context
    /// to be compacted before it is given back.
    CompactCold { member: String },
    /// Task 209: the started compaction is confirmed (or failed): the member is
    /// told to go on and the attempt stays open.
    CheckCompact { member: String },
    /// Task 135/209: a fresh session (a relaunch, or a fresh conversation at the
    /// threshold) with an open attempt gets the handoff brief as its first
    /// message and continues the attempt.
    SendHandoff { member: String },
    /// Task 209: a reply that claims there is no context left while the meter
    /// says otherwise is answered with the meter, never taken as No report.
    AnswerContextClaim { task: u32 },
    /// U61: an auto-accepted draft whose phases ahead are done and whose plan has
    /// no blocker is accepted by Auto; the ready path hands its tasks out next.
    AutoAcceptDraft { phase: u32 },
    /// U56: the member's phase and activity as the dispatcher saw them, so
    /// status shows them without an inspect.
    Watch { member: String, phase: String, activity: String, last_event_at: Option<f64>, cold: bool },
    /// U56: a busy member with no session event for stuck_minutes is stuck.
    /// #728: `since` is the last progress Zerus saw, so the item says the
    /// real minutes.
    MarkStuck { member: String, reason: String, phase: String, since: f64 },
    /// U56: the member reported again or is idle: no longer stuck.
    Unstuck { member: String },
    /// #761: a DeepSeek member with no open attempt whose listing looks stuck
    /// is read once (at most once a minute) before it is called stuck; only a
    /// read that sees work without progress past `limit` seconds marks it.
    CheckStuck { member: String, last: f64, limit: f64, reason: String },
    /// U52: the member's session exited on its own and the team setting lets
    /// Zerus launch it again into the same conversation.
    Restart { member: String },
    /// U52: the member crashed too often this hour: it stays stopped and the
    /// lead hears once.
    CrashStopped { member: String },
    NoReport { task: u32 },
    /// The planning brief of a team created from the desktop.
    Brief,
    /// Tells the lead once that every task is decided: extend or finish.
    AllDecided,
    /// A DeepSeek member's model and effort, before its first message.
    Settings { member: String },
    /// Task #6: a running member's model or effort changed: switch its session
    /// before its next task.
    ApplyModel { member: String },
    /// Task #6: a running member's start prompt changed: send it as the message
    /// that replaces its earlier instructions, after the clear it asked for.
    ReplacePrompt { member: String },
    /// Gives a ready task to a free member with the assignee's role: the
    /// assignee is busy, or failed the task's last attempt.
    Rebalance { task: u32, from: String, to: String },
    /// U36: gives a task's waiting stage to a free member with its role (or
    /// its pinned member), in the task's own folder.
    StageOut { task: u32, member: String },
    /// U36: an ended stage's detached checkout and temporary folder go.
    StageCleanup { task: u32, run: String },
    /// #538 N4: a closed stage's own run ended; its folder and checkout are
    /// released.
    StageRunEnded { task: u32, run: String },
    /// Tells the lead, in one message, which open tasks Zerus gave to
    /// another member with the same role.
    Rebalanced { tasks: Vec<u32> },
    /// Spec 5.6: one coalesced message to a free sub-lead about its reports'
    /// reports, questions and stalls.
    SubInbox { member: String },
    /// N10: the once-only brief that tells a member it is now a sub-lead.
    SubBrief { member: String },
    /// Mutes a member's notifications (unless they already are): its
    /// replies are for the lead.
    Quiet { member: String, session: SessionRef, muted: bool, revision: u64 },
    /// U47/U48: start a command or land stage that waits for Zerus.
    StageCommand { task: u32, run: String },
    /// #630 N7: an open stage whose member left the roster or whose hand-off
    /// was never confirmed goes back to Waiting.
    StageStalled { task: u32, run: String, reason: String },
    /// #635 N1: a stage member's turn ended without a verdict; the lead hears
    /// it once and the run stays Delivered.
    StageSilent { task: u32, run: String },
    /// U47/U48: read a started command or land stage's run record.
    StageCommandEnded { task: u32, run: String },
    /// #511 N3: tell a member its worked-ahead attempt was revoked with its
    /// phase, so it stops.
    TellStop { stop: String },
    /// Task 24: interrupts a member's busy turn once before its stop notice
    /// (the owner's interrupt_revoked).
    InterruptStop { stop: String },
}

/// Task 277 (owner, 2026-11-08): the urgent-message step for `member` this
/// tick. A stopped turn asks for the new turn that carries the message; a
/// busy one asks for the native interrupt. None when there is nothing urgent
/// to deliver, the member is asking at a safe point, or the same message
/// already interrupted (the alarm covers it, and a newer urgent message may
/// try once more).
fn urgent_step(team: &Team, member: &Member, seen: &[Seen], given: bool) -> Option<Step> {
    if member.id == team.lead || member.state == MemberState::Removed || member.pending.is_some() || member.terminal_question.is_some() {
        return None;
    }
    let found = session_of(member, seen);
    if let Some(urgent) = member.urgent.as_ref() {
        if urgent.state == "requested" {
            let stopped = found.is_none_or(|found| !found.live || (found.free && can_take_a_message(found)));
            return stopped.then(|| Step::ContinueUrgent { member: member.id.clone() });
        }
        let newer = member.unread.as_ref().is_some_and(|unread| {
            unread.urgent > 0 && !unread.urgent_oldest.is_empty() && unread.urgent_oldest != urgent.oldest
        });
        if !newer {
            return None;
        }
    }
    member.unread.as_ref().filter(|unread| unread.urgent > 0)?;
    if given {
        // The delivery this tick already rings for everything unread.
        return None;
    }
    // Task #42 (team ede724c4): an interrupt would abort the compaction or
    // clear Zerus runs for the member's hand-out; it waits for that step.
    if context_pending(team, member) {
        return None;
    }
    let found = found.filter(|found| found.live && !found.free && !found.asking)?;
    let turn = found.turn_started?;
    Some(Step::InterruptMessages {
        member: member.id.clone(),
        turn,
        run: found.run_id.clone(),
        conversation: found.conversation_id.clone(),
    })
}

/// U18: the lead's turn has ended and it can take a message, also while its own
/// subagents or Background jobs run (an idle agent with Background work accepts
/// a new message); never during a turn, a question, an approval or a sign-in.
fn can_take_a_message(found: &Seen) -> bool {
    if found.free {
        return true;
    }
    found.live
        && !found.asking
        && matches!(found.phase.as_str(), "" | "idle" | "ready")
        // A turn (`busy`) or a question/approval (`attention`) is not a turn
        // that ended; an idle session with Background work is (U18).
        && matches!(found.activity.as_str(), "idle" | "background")
}

/// U45: the hand-out of a task to its assignee, or to another member (a
/// member of its role, or a peer of the assignee) as a rebalance.
fn deliver(task: &Task, member: &Member) -> Step {
    if member.id == task.assignee {
        Step::HandOut { task: task.id, member: member.id.clone() }
    } else {
        Step::Rebalance { task: task.id, from: task.assignee.clone(), to: member.id.clone() }
    }
}

/// Task 263 (owner, 2026-11-08): a task claims its files for this tick only
/// when the tick actually hands it out, or starts its member for it. A task
/// that merely waits for its busy member used to claim them too, and every
/// later ready task writing the same files was skipped in silence (218-226
/// VS 198): the idle member got nothing. Now the later task goes ahead, and
/// once the waiting task can start, the later task's in-progress work holds
/// it back instead (plan::output_holders), so the two still never run apart.
fn claim_outputs<'a>(claimed: &mut Vec<&'a Task>, task: &'a Task) {
    if task.narrow_outputs().next().is_some() {
        claimed.push(task);
    }
}

fn session_of<'a>(member: &Member, seen: &'a [Seen]) -> Option<&'a Seen> {
    let session = member.session.as_ref()?;
    seen.iter().find(|s| s.host == session.host && s.name == session.name)
}

/// Task #26: whether Zerus may start this member although no task is handed out
/// to it: no session, no start in flight, and neither removed, paused, draining
/// nor failed (a failed start waits for its relaunch).
fn may_start(member: &Member) -> bool {
    member.session.is_none()
        && !matches!(member.state, MemberState::Removed | MemberState::Paused)
        && !member.draining
        && member.launch_id.is_empty()
        && member.error.is_empty()
}

/// N7: whether this tick may decide anything from the member's session being
/// absent - only when its computer answered the listing. A member with no
/// session yet, or a tick with no listing information, is not held back.
fn host_listed(team: &Team, attempt: &Attempt, listed: Option<&BTreeSet<String>>) -> bool {
    let Some(listed) = listed else { return true };
    team.member(&attempt.member)
        .and_then(|member| member.session.as_ref())
        .is_none_or(|session| listed.contains(&session.host))
}

/// N7: the computers whose listing this tick answered. A peer that failed
/// appears with `ok: false` and no sessions; only the first machine is local
/// (`seen` names it with an empty host).
fn listed_hosts(listing: &Value) -> BTreeSet<String> {
    let mut hosts = BTreeSet::new();
    for (index, machine) in listing.as_array().into_iter().flatten().enumerate() {
        if machine["ok"] == false {
            continue;
        }
        hosts.insert(if index == 0 { String::new() } else { machine["host"].as_str().unwrap_or_default().to_owned() });
    }
    hosts
}

/// U52: how many times one member's session is launched again in an hour, and
/// the window those restarts are counted over.
const RESTARTS_PER_HOUR: usize = 3;
const RESTART_WINDOW: f64 = 3600.0;
/// DCB-svn report (2026-11-12): how many times an hour Zerus asks a member to
/// continue after a temporary provider error, and how long after the error.
pub(crate) const FAILURE_CONTINUES_PER_HOUR: usize = 3;
pub(crate) const FAILURE_CONTINUE_AFTER: f64 = 60.0;
/// A rate limit waits longer before the new turn.
pub(crate) const RATE_LIMIT_CONTINUE_AFTER: f64 = 300.0;

/// Task #42 (team ede724c4): whether Zerus runs a context action on `member`'s
/// session for a hand-out now: the cold-context compaction before its next task
/// (`context_compact` with a native request), a task hand-out waiting for its
/// compact or clear (U46, the task's assignee), a stage hand-out's context
/// action (#581) or the clear before a changed start prompt. A message sent
/// meanwhile would end that action.
pub(crate) fn context_pending(team: &Team, member: &Member) -> bool {
    if member.context_compact.as_ref().is_some_and(|pending| !pending.request_id.is_empty()) {
        return true;
    }
    if member.prompt_change.as_ref().is_some_and(|change| change.clear.is_some()) {
        return true;
    }
    team.tasks.iter().any(|task| {
        let handout = task.assignee == member.id
            && task.waits.as_ref().is_some_and(|wait| wait.state == "waiting" && !wait.action.is_empty());
        let stage = task
            .pipeline_run
            .as_ref()
            .and_then(|pipeline| pipeline.open_run())
            .is_some_and(|run| run.member == member.id && run.context.is_some());
        handout || stage
    })
}

/// A session quiet enough for its ended turn to become the attempt's report
/// (A2): idle, no question, input or approval, and no Background work.
fn capture_open(found: &Seen) -> bool {
    // #511 N1: a Zerus build keeps the turn open: the session is Building.
    found.live && found.idle && matches!(found.phase.as_str(), "" | "idle" | "ready") && !found.background && !found.building
}

/// The reply identity recorded with the turn end is still the session's last
/// reply (A2): its reply id, or its reply time when no id is reported.
fn same_turn(attempt: &Attempt, ended: f64, found: &Seen) -> bool {
    if !attempt.turn_reply.is_empty() && !found.reply_id.is_empty() {
        return attempt.turn_reply == found.reply_id;
    }
    found.reply_at.is_some_and(|at| at == ended)
}

/// What to do for one team now. Pure, so the rules are tested without
/// processes; the tick passes the computers whose listing answered (N7).
#[cfg(test)]
pub(crate) fn steps(team: &Team, seen: &[Seen], now: f64) -> Vec<Step> {
    steps_with(team, seen, now, None)
}

/// The dispatcher's decision for one team. `listed` names the computers whose
/// session listing answered this tick; a member whose computer did not answer is
/// not decided on absence (N7). None means every member is taken from `seen`.
pub(crate) fn steps_with(team: &Team, seen: &[Seen], now: f64, listed: Option<&BTreeSet<String>>) -> Vec<Step> {
    let mut steps = Vec::new();
    let planning = team.status == TeamStatus::Planning && matches!(team.brief, Notice::Pending | Notice::Sending);
    // N6: a draft team starts its lead and gives it the draft brief, then it
    // waits for the owner to create it.
    let drafting = team.status == TeamStatus::Draft && matches!(team.brief, Notice::Pending | Notice::Sending);
    let active = matches!(team.status, TeamStatus::Running | TeamStatus::Paused);
    if !active && !planning && !drafting {
        // N29: a team that ended withdraws its members' reset waits.
        for member in team.members.iter().filter(|member| member.limit_resume) {
            steps.push(Step::StopLimitResume { member: member.id.clone() });
        }
        return steps;
    }
    let mut launching = BTreeSet::new();
    for member in &team.members {
        if member.state == MemberState::Removed {
            continue;
        }
        if member.session.is_none() && !member.launch_id.is_empty() {
            if let Some(found) = seen.iter().find(|s| s.host == member.computer && s.launch_id == member.launch_id) {
                steps.push(Step::Adopt {
                    member: member.id.clone(),
                    session: SessionRef {
                        host: found.host.clone(),
                        name: found.name.clone(),
                        run_id: found.run_id.clone(),
                        conversation_id: found.conversation_id.clone(),
                    },
                });
            }
        } else if let (Some(session), Some(found)) = (&member.session, session_of(member, seen)) {
            if !found.run_id.is_empty()
                && (session.run_id != found.run_id || session.conversation_id != found.conversation_id)
            {
                steps.push(Step::Adopt {
                    member: member.id.clone(),
                    session: SessionRef { run_id: found.run_id.clone(), conversation_id: found.conversation_id.clone(), ..session.clone() },
                });
            }
        }
        // A member works for the lead: its replies do not call the owner.
        if let Some(found) = session_of(member, seen).filter(|_| member.id != team.lead) {
            if let Some(revision) = found.notify_revision.filter(|_| found.live && !found.conversation_id.is_empty() && member.quieted != found.conversation_id) {
                steps.push(Step::Quiet {
                    member: member.id.clone(),
                    session: SessionRef { host: found.host.clone(), name: found.name.clone(), run_id: found.run_id.clone(), conversation_id: found.conversation_id.clone() },
                    muted: found.muted,
                    revision,
                });
            }
        }
    }
    // H1: a member stopped by a usage limit is stored, named to the lead once,
    // and, when the team setting allows it, handed to the recovery service for
    // the wait until its reset.
    for member in &team.members {
        if member.state == MemberState::Removed {
            // A removed member no longer asks for a reset wait (H1).
            if member.limit_resume {
                steps.push(Step::StopLimitResume { member: member.id.clone() });
            }
            continue;
        }
        let found = session_of(member, seen);
        let limited = found.and_then(|found| found.limited.as_ref()).map(|limit| MemberLimit {
            at: limit.at,
            resets_at: limit.resets_at,
            detail: limit.detail.clone(),
        });
        if member.limited != limited {
            steps.push(Step::Limited { member: member.id.clone(), limited: limited.clone() });
        }
        // N29: the limit seen now decides, so the pass that sees the episode
        // end also withdraws its recovery request.
        let desired = limited.as_ref().is_some_and(|limit| limit.resets_at.is_some())
            && team.settings.resume_after_limits
            && member.agent != "dsh"
            && member.session.is_some()
            && found.is_some_and(|found| found.live);
        if desired && !member.limit_resume {
            steps.push(Step::StartLimitResume { member: member.id.clone() });
        } else if desired && member.limit_resume {
            // N29: a swap may have given the member another session; the request
            // follows the session it is bound to now, withdrawing the old one.
            if member.limit_session.as_ref() != member.session.as_ref() {
                steps.push(Step::StartLimitResume { member: member.id.clone() });
            } else {
                // The request expires if the dispatcher stops refreshing it; a
                // reset can be hours away.
                steps.push(Step::RefreshLimitResume { member: member.id.clone() });
            }
        } else if !desired && member.limit_resume {
            steps.push(Step::StopLimitResume { member: member.id.clone() });
        }
        // H6: a swapped member's new session starts even before work is ready.
        if member.launch_after_swap
            && !member.draining
            && member.session.is_none()
            && member.launch_id.is_empty()
            && member.error.is_empty()
            && launching.insert(member.id.clone())
        {
            steps.push(Step::Launch { member: member.id.clone() });
        }
    }
    // Task 209: the native context meter. A started compaction is checked each
    // tick; an idle member whose context reached the team's threshold and that
    // holds an open attempt is compacted and continues it; a fresh session
    // (a relaunch, or an agent without native compaction) gets the handoff
    // brief as its first message and continues the same attempt.
    for member in &team.members {
        if member.state == MemberState::Removed || member.id == team.lead {
            continue;
        }
        if member.context_compact.is_some() {
            steps.push(Step::CheckCompact { member: member.id.clone() });
            continue;
        }
        let Some(session) = member.session.as_ref() else { continue };
        let open = open_attempt_task(team, &member.id).is_some();
        let free = session_of(member, seen).is_some_and(|found| found.live && found.free);
        if open && free && member.handoff.as_ref().is_some_and(|need| now - need.at >= HANDOFF_RETRY_SECS) {
            steps.push(Step::SendHandoff { member: member.id.clone() });
            continue;
        }
        if !open || !free || member.context_retry_at.is_some_and(|retry| retry > now) {
            continue;
        }
        if member.context.is_some_and(|context| context.fresh(now) && context.reached(team.settings.context_compact_percent)) {
            // A session on another computer is its own host's to read: the
            // reading comes from this computer's inspect; without one nothing
            // is started.
            if seen.iter().any(|found| found.host == session.host && found.name == session.name) {
                steps.push(Step::CompactIdle { member: member.id.clone() });
            }
        }
    }
    // Task 97: a native request nobody answered for `defer_minutes` is
    // answered safely and queued for the owner, once per request.
    if team.settings.defer_minutes > 0 {
        let after = team.settings.defer_minutes as f64 * 60.0;
        for member in team.members.iter().filter(|member| member.state != MemberState::Removed && member.session.is_some()) {
            let Some(pending) = &member.pending else { continue };
            let key = format!("{}:{}", pending.question_id, pending.question_hash);
            if pending.card["can_answer"] != false && now - pending.at >= after && !member.deferred.contains(&key) {
                steps.push(Step::Defer { member: member.id.clone(), question: key });
            }
        }
    }
    // Task 113: a request only the member's Terminal takes never stays a
    // quiet wait; after the defer time the owner and the phone hear of it.
    for member in &team.members {
        if super::terminal_prompt::due(team, member, now) {
            steps.push(Step::EscalateTerminal { member: member.id.clone() });
        }
    }
    // H6: a swapped lead is briefed once, also while the team runs.
    let briefing = (active || planning || drafting) && matches!(team.brief, Notice::Pending | Notice::Sending);
    if briefing {
        // A desktop team starts only its lead, then briefs it once.
        if let Some(lead) = team.member(&team.lead) {
            if lead.session.is_none() {
                if (planning || drafting)
                    && lead.launch_id.is_empty()
                    && lead.error.is_empty()
                    && launching.insert(lead.id.clone())
                {
                    steps.push(Step::Launch { member: lead.id.clone() });
                }
            } else if team.brief == Notice::Pending && session_of(lead, seen).is_some_and(|s| s.free && !s.run_id.is_empty()) {
                steps.push(if lead.needs_settings() { Step::Settings { member: lead.id.clone() } } else { Step::Brief });
            }
        }
        if planning || drafting {
            return steps;
        }
    }
    // Turn ends, No report and reports for the lead, for open work.
    for task in &team.tasks {
        let Some(attempt) = task.attempts.last() else { continue };
        // C1: the lead's own task is never a delivered turn and its replies
        // are no report; it appears in the inbox and the lead reports and
        // accepts it in one step.
        if attempt.member == team.lead {
            continue;
        }
        if attempt.state == AttemptState::Delivered {
            // N7: a member whose computer did not answer this tick is not
            // evidence that it stopped; decide nothing about its attempt until
            // its listing answers again.
            if !host_listed(team, attempt, listed) {
                continue;
            }
            let found = team.member(&attempt.member).and_then(|m| session_of(m, seen));
            let delivered = attempt.delivered_at.unwrap_or(attempt.sent_at);
            // Task #49 (team ede724c4): a member that waits for a job of its
            // own is not done, so its finished turn is no No report (task 39's
            // rule for the inbox item, now for the attempt the plan's card
            // reads), and a No report set before the wait was seen is cleared.
            // The report clock is cleared once with it, so the timeout starts
            // when nothing of its own runs any more (GateOpen), and nothing is
            // written while the wait lasts.
            let waits = waits_for_own_job(attempt, found);
            if waits && (attempt.no_report || attempt.report_ready_at.is_some()) {
                steps.push(Step::OwnJobFound { task: task.id });
            }
            // A1: an attempt that shows the member working really started.
            if attempt.started_at.is_none() {
                let working = found.is_some_and(|found| {
                    found.live
                        && (!found.idle
                            || found.turn_started.is_some_and(|at| at > delivered)
                            || found.reply_at.is_some_and(|at| at > delivered))
                });
                if working {
                    steps.push(Step::Started { task: task.id });
                } else if !attempt.start_warned
                    && attempt.open_question().is_none()
                    && now - delivered >= team.settings.start_timeout_seconds as f64
                {
                    // A1: delivered and never started; Zerus re-sends nothing.
                    steps.push(Step::StartLate { task: task.id });
                }
            }
            // Task 36 (audit P8): a member that ends its turn while its own
            // Zerus build runs is not done: nothing would tell it when the run
            // settles, and the quiet session would turn into a captured
            // "waiting for my build" or No report. The run it waits on is
            // remembered; once it settles with the turn over, the member is
            // woken once and this tick captures nothing.
            if let Some(found) = found.filter(|found| found.live) {
                if found.building {
                    let run = if found.build_run.is_empty() { "build".to_owned() } else { found.build_run.clone() };
                    if attempt.awaited_run != run {
                        steps.push(Step::RunAwaited { task: task.id, run, label: found.build_label.clone() });
                    }
                } else if !attempt.awaited_run.is_empty() {
                    let wake = capture_open(found) && attempt.open_question().is_none();
                    steps.push(Step::RunSettled { task: task.id, wake });
                    if wake {
                        continue;
                    }
                }
            }
            // Task 209: a member whose meter reached the threshold is compacted
            // and continues the attempt: its finished turn is not decided (No
            // report would end an attempt that is not stuck).
            if context_due(team, &attempt.member, now) {
                continue;
            }
            // A2/C2: the report gate. A turn end is recorded only for a quiet
            // session: idle, no question, input or approval, no Background work
            // and no open team question. The report timeout counts from the
            // tick that first saw the gate open, not from the reply.
            match (attempt.turn_ended_at, found) {
                (None, Some(found)) => {
                    let replied = found.reply_at.filter(|at| *at > delivered);
                    if capture_open(found) && attempt.open_question().is_none() {
                        if let Some(at) = replied {
                            steps.push(Step::TurnEnded { task: task.id, at, reply: found.reply_id.clone() });
                        } else if !attempt.no_report
                            && !waits
                            && attempt.started_at.is_some()
                            && now - delivered >= team.settings.report_timeout_seconds as f64
                        {
                            // A2: the session is quiet but the listing carries no
                            // reply mark for the turn (its replies were never read),
                            // so no turn end could be recorded. After the timeout the
                            // attempt is decided all the same: No report keeps the
                            // quiet session's last reply for the lead instead of
                            // leaving the task in progress for hours (U20, #343 attempt 2).
                            steps.push(no_report_or_claim(team, task, now));
                        }
                    } else if found.live && found.idle && replied.is_some() && attempt.blocked_since.is_none() {
                        // The usual order: the member asks (or leaves Background
                        // work running) and ends its turn. No turn end is
                        // recorded, but the block starts now, so a question
                        // answered in prose or a job that never ends still
                        // reaches the lead (N5).
                        steps.push(Step::GateClosed { task: task.id });
                    } else if attempt.blocked_since.is_some() && found.live && !found.idle {
                        steps.push(Step::WorkResumed { task: task.id });
                    }
                }
                (Some(ended), found) => {
                    let same = found.is_some_and(|found| same_turn(attempt, ended, found));
                    let quiet = found.is_some_and(capture_open) && attempt.open_question().is_none() && same;
                    let ready = attempt.report_ready_at.unwrap_or(ended);
                    if found.is_none_or(|found| !found.live) {
                        // L6: the session is gone; the old fallback keeps what
                        // it can, else marks No report.
                        if !attempt.no_report && now - ready >= team.settings.report_timeout_seconds as f64 {
                            steps.push(no_report_or_claim(team, task, now));
                        }
                    } else if quiet {
                        if waits {
                            // Task #49: its own job still runs (OwnJobFound
                            // above cleared the clock); nothing is decided.
                        } else if attempt.report_ready_at.is_none() {
                            // The gate was already open when this record was
                            // written; the timeout starts now (C2).
                            steps.push(Step::GateOpen { task: task.id });
                        } else if !attempt.no_report && now - ready >= team.settings.report_timeout_seconds as f64 {
                            steps.push(no_report_or_claim(team, task, now));
                        }
                    } else if found.is_some_and(|found| !found.idle || !same_turn(attempt, ended, found)) {
                        steps.push(Step::WorkResumed { task: task.id });
                    } else if attempt.report_ready_at.is_some() || attempt.blocked_since.is_none() {
                        // A question or Background work holds the finished turn:
                        // the timeout restarts and a long block is reported (C2, N5).
                        steps.push(Step::GateClosed { task: task.id });
                    }
                }
                (None, None) => {
                    // A2: the member's session is not in the listing at all (it
                    // stopped or was renamed) and no turn end was recorded. After
                    // the timeout the attempt is decided as for a session that went
                    // away: its last reply is kept from the session's record,
                    // or it shows No report (U20). N7: only while its computer is
                    // listed - an absent listing is not a stopped session.
                    if !attempt.no_report
                        && attempt.started_at.is_some()
                        && now - delivered >= team.settings.report_timeout_seconds as f64
                    {
                        steps.push(no_report_or_claim(team, task, now));
                    }
                }
            }
        }
    }
    // Each tick lists the sessions: a member's liveness is recorded when it
    // changes, so status derives `stopped` without a listing of its own (L3).
    // Task 139: screens read this pass.
    let mut checks = 0;
    for member in &team.members {
        let Some(session) = member.session.as_ref() else { continue };
        // L4: a member its computer does not list is offline, not stuck: clear a
        // mark instead of letting it grow.
        let Some(found) = seen.iter().find(|seen| seen.host == session.host && seen.name == session.name) else {
            if member.stuck.is_some() {
                steps.push(Step::Unstuck { member: member.id.clone() });
            }
            continue;
        };
        if member.seen.is_none_or(|recorded| recorded.live != found.live) {
            steps.push(Step::Liveness { member: member.id.clone(), live: found.live });
        }
        // G7: services ride with the session; status shows them on the member.
        let changed = member.services.len() != found.services.len()
            || member
                .services
                .iter()
                .zip(&found.services)
                .any(|(old, new)| old.name != new.name || old.command != new.command || old.since != new.since);
        if changed {
            steps.push(Step::Services { member: member.id.clone(), services: found.services.clone() });
        }
        // Task 278: what the member waits on; status, look and the brief show it.
        let jobs_changed = member.jobs.len() != found.jobs.len()
            || member.jobs.iter().zip(&found.jobs).any(|(old, new)| old.kind != new.kind || old.command != new.command || old.since != new.since);
        if jobs_changed {
            steps.push(Step::Jobs { member: member.id.clone(), jobs: found.jobs.clone() });
        }
        // U56: the member's phase and activity feed status; a busy member with
        // no session event for stuck_minutes is marked stuck, once per episode.
        // A question, a Zerus build or Background work is never stuck; the lead
        // is never stuck either (L1).
        if found.live {
            // L3: phase and activity on change, the event time alone at most
            // every WATCH_EVENT_EVERY. Task #2 (team ede724c4): it was once a
            // minute, one team revision per busy member per minute; stuck
            // detection reads the listing's own time, so only the minutes
            // status shows for a busy member use the stored one.
            let moved = match (member.seen_event_at, found.last_event_at) {
                (None, None) => false,
                (old, new) => new.is_none_or(|at| old.is_none_or(|old| (at - old).abs() >= WATCH_EVENT_EVERY)),
            };
            if member.seen_phase != found.phase || member.seen_activity != found.activity || moved || member.seen_cold != found.cold {
                steps.push(Step::Watch {
                    member: member.id.clone(),
                    phase: found.phase.clone(),
                    activity: found.activity.clone(),
                    last_event_at: found.last_event_at,
                    cold: found.cold,
                });
            }
            if member.id == team.lead {
                continue;
            }
            // N1: a dsh listing's last_event_at does not advance with its native
            // tool events; the open attempt's activity_at counts too (as the
            // silent check does), and a dsh member is read once before it is
            // called stuck.
            let open = team
                .tasks
                .iter()
                .find_map(|task| task.open_attempt().filter(|attempt| attempt.member == member.id).map(|attempt| (task.id, attempt)));
            let listed = found.last_event_at;
            let read = open.as_ref().and_then(|(_, attempt)| attempt.activity_at);
            // #761: a report, a turn's end or a native event a read saw is
            // progress too: a member that just reported is not stuck because
            // its DeepSeek listing still says working.
            let read = [read, member.native_at, last_progress(team, &member.id)].into_iter().flatten().reduce(f64::max);
            let last = match (listed, read) {
                (Some(listed), Some(read)) => Some(listed.max(read)),
                (listed, read) => listed.or(read),
            };
            let busy = found.activity == "busy" || matches!(found.phase.as_str(), "working" | "tool" | "compacting");
            // #728: a turn inside a running tool call (one long Bash call that
            // polls a test run) is the agent waiting for its own tool: working,
            // stuck only past the much longer tool bound, counted from the
            // call's start. A tool phase with no running call has the usual bound.
            let in_tool = found.phase == "tool"
                && !found.tool.is_empty()
                && found.tool_started.is_some_and(|started| found.turn_started.is_none_or(|turn| started >= turn));
            let last = match (in_tool, last, found.tool_started) {
                (true, Some(last), Some(started)) => Some(last.max(started)),
                (true, None, started) => started,
                (_, last, _) => last,
            };
            let quiet = last.map(|at| now - at).unwrap_or(0.0);
            let limit = if in_tool { tool_stuck_minutes(team) * 60.0 } else { super::ops::stuck_minutes(team) * 60.0 };
            // Task 115: a member with an open native question or permission is
            // Waiting for you, never stuck in a silent turn: the stored card
            // outlives a tick whose phase has moved on.
            // Task 278: nor is one waiting on a job it registered.
            let stuck = busy
                && !found.asking
                && !found.background
                && !found.building
                && found.jobs.is_empty()
                && member.pending.is_none()
                && member.terminal_question.is_none()
                && quiet >= limit;
            if stuck && member.stuck.is_none() {
                let reason = if found.phase == "compacting" { "compaction_timeout" } else { "no_progress" };
                let checked = open.as_ref().is_some_and(|(_, attempt)| attempt.told_lead.iter().any(|kind| kind == "silent"));
                if member.agent == "dsh" && !checked {
                    if let Some((task_id, attempt)) = open.as_ref() {
                        // #728 L2: between the stuck and the silent bound the
                        // session is read at most once a minute, not every tick.
                        if attempt.silent_read_at.is_none_or(|at| now - at >= 60.0) {
                            steps.push(Step::CheckSilent { task: *task_id, last: listed.unwrap_or(0.0) });
                        }
                    } else if member.stuck_read_at.is_none_or(|at| now - at >= 60.0) {
                        // #761: no open attempt: read the session before the
                        // listing alone calls it stuck, at most once a minute.
                        steps.push(Step::CheckStuck { member: member.id.clone(), last: last.unwrap_or(0.0), limit, reason: reason.into() });
                    }
                } else {
                    steps.push(Step::MarkStuck { member: member.id.clone(), reason: reason.into(), phase: found.phase.clone(), since: last.unwrap_or(now) });
                }
            } else if !stuck && member.stuck.is_some() {
                steps.push(Step::Unstuck { member: member.id.clone() });
            }
            // Task 139: a suspect member's sources are compared, with its
            // screen read off the tick, a few members at a time.
            let hooks = super::look::Hooks {
                live: found.live,
                activity: found.activity.clone(),
                phase: found.phase.clone(),
                asking: found.asking,
                background: found.background,
                building: found.building,
                build_label: found.build_label.clone(),
                tool: found.tool.clone(),
                tool_since: if in_tool { found.tool_started } else { None },
                jobs: found.jobs.iter().map(|job| super::look::WaitJob { what: job.what.clone(), since: job.since }).collect(),
                last_event_at: found.last_event_at,
                quiet,
                limit,
            };
            match super::look::due(team, member, &hooks, now) {
                Some(true) if checks < super::look::MAX_CHECKS => {
                    checks += 1;
                    steps.push(Step::Check { member: member.id.clone(), hooks });
                }
                None if member.check.is_some() => steps.push(Step::ClearCheck { member: member.id.clone() }),
                _ => {}
            }
        } else if member.stuck.is_some() {
            steps.push(Step::Unstuck { member: member.id.clone() });
        }
    }
    // U56: a pending task whose member is kept for it (pinned, or every planned
    // assignee with keep_assignees) and who is stuck, stopped or offline says so
    // in its waits, so the lead sees why it never starts (N4).
    for task in &team.tasks {
        if task.status != TaskStatus::Pending || task.assignee.is_empty() || !team.requires_assignee(task) {
            continue;
        }
        if task.waits.as_ref().is_some_and(|wait| wait.waits_for != "member") {
            continue;
        }
        let Some(member) = team.member(&task.assignee) else { continue };
        let stuck = member.stuck.as_ref();
        let stopped = session_of(member, seen)
            .map(|found| !found.live)
            .or_else(|| member.seen.map(|seen| !seen.live))
            .unwrap_or(false);
        let reason = match (stuck, stopped) {
            // #761: the minutes since the last progress Zerus saw, never the
            // time since the mark ("stuck for 0 min"); an old mark without that
            // time names no minutes.
            (Some(stuck), _) => Some(match stuck.since {
                Some(since) => format!(
                    "pinned to {} ({}), no progress for {} min (phase {})",
                    member.id,
                    team.role_of(&member.id),
                    ((now - since) / 60.0).max(1.0).round() as u64,
                    if stuck.phase.is_empty() { "unknown" } else { stuck.phase.as_str() }
                ),
                None => format!(
                    "pinned to {} ({}), no progress (phase {})",
                    member.id,
                    team.role_of(&member.id),
                    if stuck.phase.is_empty() { "unknown" } else { stuck.phase.as_str() }
                ),
            }),
            (None, true) => Some(format!(
                "pinned to {} ({}), whose session is stopped or offline",
                member.id,
                team.role_of(&member.id)
            )),
            (None, false) => None,
        };
        let waiting = task.waits.as_ref().is_some_and(|wait| wait.waits_for == "member");
        match (reason, waiting) {
            (Some(reason), false) => steps.push(Step::WaitOnMember { task: task.id, member: member.id.clone(), reason }),
            (None, true) => steps.push(Step::ReleaseWait { task: task.id }),
            _ => {}
        }
    }
    // U52: a member session that exited on its own is launched again into its
    // own conversation, at most three times an hour. A paused, archived,
    // limited or swapped session is never restarted, and a member with nothing
    // to do needs a live sighting since its last restart (a failed relaunch
    // does not loop). Task #334 (owner, 2026-11-08): an exit that leaves a
    // member holding or handed a task relaunches it even when no tick saw the
    // session live - an install restarted the agents while their members were
    // paused - and a member the owner or the lead resumed is launched again
    // the same way, so neither waits for a relaunch card. The lead too
    // (owner, 2026-11-05): a team whose lead exited has nobody to read its
    // reports.
    if team.status == TeamStatus::Running {
        for member in &team.members {
            let resumed = member.relaunch_exited;
            if !team.settings.restart_crashed && !resumed {
                continue;
            }
            if member.agent == "opencode"
                || member.state != MemberState::Active
                || member.session.is_none()
                || member.limited.is_some()
            {
                continue;
            }
            let Some(found) = session_of(member, seen) else { continue };
            if found.live || found.state != "stopped" || found.limited.is_some() {
                continue;
            }
            let works = holds_work(team, &member.id);
            if !works && !resumed && !member.seen.is_some_and(|seen| seen.live) {
                continue;
            }
            let restarted = member.restarts.iter().filter(|at| now - **at <= RESTART_WINDOW).count();
            if restarted < RESTARTS_PER_HOUR {
                steps.push(Step::Restart { member: member.id.clone() });
            } else if resumed || !member.crash_told {
                // The resume's request is spent either way: the lead hears and
                // the member stays stopped until the next hour or a relaunch.
                steps.push(Step::CrashStopped { member: member.id.clone() });
            }
        }
    }
    // DCB-svn report (2026-11-12): a member whose live session ended its turn
    // on a provider error sat in "error" with nobody told, and restart_crashed
    // did nothing (the process had not crashed). The failure is stored and
    // named to the lead at once; after a temporary one (a transport or network
    // error, an overloaded service, a rate limit) a member holding work is
    // asked once to continue, unless the shared recovery service already does.
    for member in team.members.iter().filter(|member| member.state != MemberState::Removed && member.id != team.lead) {
        let found = session_of(member, seen).filter(|found| found.live);
        let failed = found.and_then(|found| found.failed.as_ref()).map(|failure| TurnFailure {
            at: failure.at,
            detail: failure.detail.clone(),
            class: failure.class.clone(),
            continued_at: None,
        });
        let same = match (&member.turn_failed, &failed) {
            (None, None) => true,
            (Some(old), Some(new)) => old.same(new),
            _ => false,
        };
        if !same {
            steps.push(Step::TurnFailed { member: member.id.clone(), failed });
            continue;
        }
        let Some(failure) = member.turn_failed.as_ref() else { continue };
        let recovering = found.and_then(|found| found.failed.as_ref()).is_some_and(|failure| failure.recovering);
        let wait = if failure.class == "rate_limit" { RATE_LIMIT_CONTINUE_AFTER } else { FAILURE_CONTINUE_AFTER };
        let continues = member.failure_continues.iter().filter(|at| now - **at <= RESTART_WINDOW).count();
        if team.status == TeamStatus::Running
            && member.state == MemberState::Active
            && failure.temporary()
            && failure.continued_at.is_none()
            && !recovering
            && now - failure.at >= wait
            && continues < FAILURE_CONTINUES_PER_HOUR
            && holds_work(team, &member.id)
        {
            steps.push(Step::ContinueFailed { member: member.id.clone() });
        }
    }
    // Task #227 (owner, 2026-11-07): an attempt a restart left unconfirmed goes
    // back to its restarted session by itself, never to the lead: sent again
    // when the new run idles, bound to it when the run already works on the
    // task (a resumed conversation continues on its own).
    for task in &team.tasks {
        let Some(attempt) = task
            .attempts
            .iter()
            .rev()
            .find(|attempt| attempt.state == AttemptState::Unconfirmed && attempt.error.contains("the session crashed"))
        else {
            continue;
        };
        let Some(member) = team.member(&attempt.member) else { continue };
        // Task 254: nothing goes back to a paused team, phase or member, a
        // draining member or a held task; the tick after the pause does it.
        if crash_held(team, task, member).is_some() {
            continue;
        }
        let Some(found) = session_of(member, seen) else { continue };
        if !found.live {
            continue;
        }
        let busy = !found.free;
        // Only a run that works on this very task keeps it: its branch or screen
        // names it, or it holds no other attempt. A run busy elsewhere is left
        // alone rather than told to start this one over.
        if busy && !crash_same_task(team, member, task.id) {
            continue;
        }
        // Task 254: a cold, large context is compacted before the attempt goes
        // back; the give-back follows on the tick after it is confirmed.
        if !busy && member.context_compact.is_some() {
            continue;
        }
        if !busy && cold_large(team, member) {
            steps.push(Step::CompactCold { member: attempt.member.clone() });
            continue;
        }
        steps.push(Step::RecoverCrash { task: task.id, member: attempt.member.clone(), busy });
    }
    // Task #227: the "continue" a retry queued reaches its member at a safe point.
    if team.continues.iter().any(|notice| notice.notice == Notice::Pending) {
        steps.push(Step::Continues);
    }
    // A hand-out that never started: the lead hears once, when free (A1).
    if team.member(&team.lead).and_then(|m| session_of(m, seen)).is_some_and(|s| s.free) {
        for task in &team.tasks {
            let Some(attempt) = task.open_attempt() else { continue };
            if attempt.started_at.is_none()
                && attempt.start_warned
                && attempt.open_question().is_none()
                && !attempt.told_lead.iter().any(|kind| kind == "start_late")
            {
                steps.push(Step::TellStartLate { task: task.id });
                break;
            }
        }
    }
    // A member in its turn that has said nothing for SILENT_MINUTES, with no
    // question or Background work Zerus sees: something on its screen may
    // hold it, and only an interrupt frees it.
    for task in &team.tasks {
        let Some(attempt) = task.open_attempt() else { continue };
        if attempt.member == team.lead || attempt.told_lead.iter().any(|kind| kind == "silent") {
            continue;
        }
        let Some(found) = team.member(&attempt.member).and_then(|member| session_of(member, seen)) else { continue };
        // #628: a DeepSeek session's listing `last_event_at` does not advance
        // with its dsh_native tool events. The newest event the worker has read
        // for this attempt counts too, so a working member is not silent.
        let last = match (found.last_event_at, attempt.activity_at) {
            (Some(listed), Some(read)) => Some(listed.max(read)),
            (listed, read) => listed.or(read),
        };
        let Some(last) = last else { continue };
        let quiet = now - last;
        if !(found.live && found.activity == "busy" && !found.asking && !found.background && found.jobs.is_empty() && quiet >= SILENT_MINUTES * 60.0) {
            continue;
        }
        if found.agent == "dsh" {
            // Read the native events once; the next tick counts what it found.
            steps.push(Step::CheckSilent { task: task.id, last });
        } else {
            steps.push(Step::Silent { task: task.id, minutes: (quiet / 60.0).round() as u64 });
        }
    }
    // A member's native question or permission request goes to the mailbox,
    // not to a message of its own (C2); one already answered is cleared.
    // #740: the lead's own question (its AskUserQuestion) is read the same
    // way; it is the owner's to answer, from Waiting for you.
    for member in &team.members {
        if member.state == MemberState::Removed {
            continue;
        }
        let Some(session) = member.session.as_ref() else { continue };
        let Some(found) = seen.iter().find(|seen| seen.host == session.host && seen.name == session.name) else { continue };
        let stale = member.pending.as_ref().is_some_and(|pending| now - pending.at > 60.0);
        if !found.asking && (member.pending.is_some() || member.terminal_question.is_some()) {
            steps.push(Step::ClearPermission { member: member.id.clone() });
        } else if found.asking && found.live && (member.pending.is_none() || stale) {
            steps.push(Step::Permission { member: member.id.clone() });
        }
    }
    // The lead's answer reaches the member as a message, once its session is
    // live (A3). #630 N6: a stage member's answer goes to its run.
    'answers: for task in &team.tasks {
        for attempt in &task.attempts {
            for question in &attempt.questions {
                if !question.open() && question.notice == Notice::Pending {
                    let live = team.member(&attempt.member).and_then(|m| session_of(m, seen)).is_some_and(|s| s.live);
                    if live {
                        steps.push(Step::AnswerQuestion { task: task.id, question: question.id.clone() });
                        break 'answers;
                    }
                }
            }
        }
        if let Some(pipeline) = task.pipeline_run.as_ref() {
            for run in &pipeline.runs {
                for question in &run.questions {
                    if !question.open() && question.notice == Notice::Pending {
                        let live = team.member(&run.member).and_then(|m| session_of(m, seen)).is_some_and(|s| s.live);
                        if live {
                            steps.push(Step::AnswerQuestion { task: task.id, question: question.id.clone() });
                            break 'answers;
                        }
                    }
                }
            }
        }
    }
    // A finished turn blocked by a question or Background work for too long:
    // the lead hears once (N5).
    if team.member(&team.lead).and_then(|m| session_of(m, seen)).is_some_and(|s| s.free) {
        for task in &team.tasks {
            let Some(attempt) = task.attempts.last() else { continue };
            if attempt.state == AttemptState::Delivered
                && attempt.blocked_since.is_some_and(|since| now - since >= team.settings.blocked_timeout_seconds as f64)
                && !attempt.told_lead.iter().any(|kind| kind == "blocked")
            {
                steps.push(Step::BlockedLate { task: task.id });
                break;
            }
        }
    }
    // C4: a report undecided for `remind_lead_minutes` reminds the lead once;
    // the owner sees it in status after `remind_owner_minutes`.
    let lead = team.member(&team.lead).and_then(|m| session_of(m, seen));
    let lead_free = lead.is_some_and(|s| s.free);
    // U18: the inbox and the report reminder also reach a lead whose own
    // Background work or subagents keep it from being `free`, as long as its
    // turn has ended and it can take a message.
    let lead_ready = lead.is_some_and(can_take_a_message);
    // Task 95: decisions wait for a lead that does not work on them: the
    // owner hears it once a round, at most hourly, until the lead moves.
    if let Some(stall) = super::lead_stall::desired(team, seen, now, listed) {
        if stall != team.lead_stall {
            steps.push(Step::LeadStall { stall });
        }
    }
    // Task 139: an idle lead with decisions waiting is told itself, once a
    // round, so nobody has to wake it by hand.
    if team.lead_stall.as_ref().is_some_and(|stall| stall.reason == "idle" && !stall.lead_told) && lead_ready {
        steps.push(Step::TellLeadIdle);
    }
    // N4: the sub-lead's reminder does not wait for the lead's turn to end -
    // only the lead's own reminder needs a lead that can take a message.
    if team.settings.remind_lead_minutes > 0 {
        let after = team.settings.remind_lead_minutes as f64 * 60.0;
        for task in &team.tasks {
            // Spec 5.6: a sub-lead's reports are its own to decide; it is
            // reminded first, the lead only after a second interval (N9).
            let sub = task.assignee != team.lead && team.reviewer_of(&task.assignee) != team.lead;
            if task.status != TaskStatus::Submitted {
                continue;
            }
            // Task #443: a report the lead accepted whose work waits for its
            // landing (the queue's, or a hand landing) is decided: it never
            // reminds anyone; the lead's status names it in one "accepted,
            // waiting to land" line.
            if super::merge_queue::waits_for_landing(task) {
                continue;
            }
            let Some(attempt) = task.attempts.iter().rev().find(|attempt| attempt.state == AttemptState::Reported) else { continue };
            let age = attempt.report.as_ref().map(|report| now - report.at).unwrap_or(0.0);
            if sub {
                let reviewer = team.reviewer_of(&task.assignee);
                if let Some(found) = team.member(&reviewer).filter(|member| member.sub) {
                    if session_of(found, seen).is_some_and(can_take_a_message)
                        && attempt.reminded_at.is_none()
                        && age >= after
                    {
                        steps.push(Step::RemindReviewer { task: task.id, member: reviewer });
                    }
                }
                // The lead hears after the second interval, once.
                if lead_ready && age >= after * 2.0 && !attempt.told_lead.iter().any(|kind| kind == "sub-remind") {
                    steps.push(Step::RemindReport { task: task.id });
                    break;
                }
                continue;
            }
            if !lead_ready || attempt.reminded_at.is_some() {
                continue;
            }
            if attempt.report.as_ref().is_some_and(|report| now - report.at >= after) {
                steps.push(Step::RemindReport { task: task.id });
                break;
            }
        }
    }
    // C2: one message lists the new items, at most once per interval.
    // Task 186 (docs/lore.md hard rule 3, the lead's decision): nothing is
    // typed into a lead's working turn. The owner's 2026-10-28 intent (a
    // permission waited 8 minutes for the turn end) is kept by the hooks: for
    // a lead whose agent has them (Claude, Codex) the list is stored at once
    // (`InboxWaits`); a Claude lead takes an urgent one at its next tool
    // boundary, both take every one at their next turn start, and the
    // doorbell rings at the turn end. A question or dialog on the lead's
    // screen still holds it.
    // Task 112: another agent (Kimi) would read a list composed long before,
    // with reports decided meanwhile, so it hears when it is free, from a
    // fresh list.
    let mid_turn = team.member(&team.lead).is_some_and(|found| takes_input_mid_turn(&found.agent));
    let lead_working = !lead_ready && mid_turn && lead.is_some_and(|found| found.live && !found.asking && found.activity == "busy");
    if lead_ready || lead_working {
        let items = inbox::items(team);
        let new = items.iter().filter(|item| !team.inbox.knows(&item.key, now)).count();
        // A permission or question the free lead left unanswered is told once
        // more (owner, 2026-11-05).
        let again = if lead_ready { inbox::overdue(&items, &team.inbox.told, now).len() } else { 0 };
        let interval = team.settings.inbox_interval_seconds as f64;
        if new + again > 0 && (team.inbox.told_at == 0.0 || now - team.inbox.told_at >= interval) {
            steps.push(if lead_ready { Step::Inbox } else { Step::InboxWaits });
        }
    }
    // Task #26: a sub-lead with reports to read and no session is started, so
    // its brief and inbox reach it; the message steps below follow once it can
    // take one. Only a running or paused team starts its members: a planning or
    // draft team starts its lead alone.
    if active {
        for sub in team.members.iter().filter(|member| member.sub && may_start(member)) {
            let untold = inbox::items_for(team, &sub.id).iter().any(|item| !sub.inbox.knows(&item.key, now));
            if untold && launching.insert(sub.id.clone()) {
                steps.push(Step::Launch { member: sub.id.clone() });
            }
        }
    }
    // N10: a member that just became a sub-lead hears what that means once.
    for sub in team.members.iter().filter(|member| member.sub && member.sub_brief && member.state != MemberState::Removed) {
        if session_of(sub, seen).is_some_and(can_take_a_message) {
            steps.push(Step::SubBrief { member: sub.id.clone() });
        }
    }
    // Spec 5.6: each free sub-lead hears about its own reports the same way.
    for sub in team.members.iter().filter(|member| member.sub && member.state != MemberState::Removed) {
        if !session_of(sub, seen).is_some_and(can_take_a_message) {
            continue;
        }
        let new = inbox::items_for(team, &sub.id).into_iter().filter(|item| !sub.inbox.knows(&item.key, now)).count();
        let interval = team.settings.inbox_interval_seconds as f64;
        if new > 0 && (sub.inbox.told_at == 0.0 || now - sub.inbox.told_at >= interval) {
            steps.push(Step::SubInbox { member: sub.id.clone() });
        }
    }
    // #511 N3: a member whose worked-ahead attempt was revoked hears it once,
    // as soon as its session runs; the message waits behind its turn.
    // Task 24: so does a member whose attempt in progress was reassigned,
    // taken over or cancelled; with the owner's interrupt_revoked its busy turn
    // is interrupted once first, so it stops at once and takes its next task.
    for stop in team.stops.iter().filter(|stop| stop.notice == Notice::Pending) {
        let Some(found) = team.member(&stop.member).and_then(|member| session_of(member, seen)).filter(|found| found.live) else {
            continue;
        };
        if stop.interrupt && !stop.interrupted && !found.free {
            steps.push(Step::InterruptStop { stop: stop.id.clone() });
        }
        steps.push(Step::TellStop { stop: stop.id.clone() });
    }
    // U47/U48: a started stage command ends while the team is paused too; a
    // new one starts only while it runs.
    for (task, run) in super::pipeline::commands_running(team) {
        steps.push(Step::StageCommandEnded { task, run });
    }
    if team.status == TeamStatus::Running {
        for (task, run) in super::pipeline::commands_waiting(team) {
            steps.push(Step::StageCommand { task, run });
        }
    }
    if team.status != TeamStatus::Running {
        // N35 (spec 2.5): a paused team's lead also hears one message a tick.
        return coalesce_lead(steps);
    }
    // The lead hears once that every task is decided; the next work re-arms it.
    if team.idle == Notice::Pending && lead_free {
        steps.push(Step::AllDecided);
    }
    // Task #464 (owner, 2026-11-09): the queue is off and accepted work waits
    // for a landing. Once for each accepted task, the merger that holds the
    // landing task hears it, so a finished turn never leaves the leading edge
    // cold; a landing it already heard of is never told again.
    if let Some((member, _)) = super::merge_queue::landing_task(team) {
        if let Some(waiting) = super::merge_queue::waiting_to_land(team, now) {
            let fresh: Vec<u32> = waiting["tasks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|task| task["state"] == "not_queued")
                .filter_map(|task| task["task"].as_u64().map(|id| id as u32))
                .filter(|id| !team.landing_told.contains(id))
                .collect();
            if !fresh.is_empty() {
                steps.push(Step::WakeLanding { member, tasks: fresh });
            }
        }
    }
    // Open tasks Zerus gave to another member with the same role, in one
    // message to the free lead.
    if lead_free {
        let tasks: Vec<u32> = team
            .tasks
            .iter()
            .filter(|t| t.open_attempt().is_some_and(|a| !a.rebalanced_from.is_empty() && !a.told_lead.iter().any(|k| k == "rebalanced")))
            .map(|t| t.id)
            .collect();
        if !tasks.is_empty() {
            steps.push(Step::Rebalanced { tasks });
        }
    }
    // Members busy with an open attempt, or handed one in this tick, or with
    // a stage of another member's task (U36). A7: a member whose undecided
    // report was captured from its last reply counts as busy too - no
    // hand-out, rebalance or work ahead, and it has work, so it does not
    // starve (C7) - until that report is decided or replaced (A6).
    let mut busy: BTreeSet<&str> = team
        .tasks
        .iter()
        .filter_map(|task| task.open_attempt())
        .map(|attempt| attempt.member.as_str())
        .chain(team.members.iter().filter(|member| team.captured_hold(&member.id).is_some()).map(|member| member.id.as_str()))
        .chain(super::stages::busy(team))
        .collect();
    // U46: a hand-out whose context action waits for its confirmation owns its
    // member: one look per tick, and no other task goes to that member until
    // the hand-out itself or its waiting reason is recorded.
    let mut preparing: BTreeSet<&str> = BTreeSet::new();
    for task in team.tasks.iter().filter(|task| task.waits.as_ref().is_some_and(TaskWait::running)) {
        busy.insert(task.assignee.as_str());
        if preparing.insert(task.assignee.as_str()) {
            steps.push(Step::PrepareContext { task: task.id, member: task.assignee.clone() });
        }
    }
    // Task #6: the owner changed a member's model, effort or start prompt while
    // it ran. The switch and the replacing message go out before the member's
    // next task, so it takes no task in this tick; a member Zerus is already
    // switching for a ready task (U45) keeps that claim, and a removed member is
    // done. A model step left on a finished or waiting task claims nothing.
    let ready_ids = plan::ready(team);
    let claimed: BTreeSet<&str> = ready_ids
        .iter()
        .filter_map(|id| team.task(*id))
        .filter(|task| task.waits.is_none() && super::models::applies(team, task))
        .filter_map(super::models::pending)
        .map(|step| step.member.as_str())
        .collect();
    for member in team.members.iter().filter(|member| member.model_switch.is_some() || member.prompt_change.is_some()) {
        if member.state == MemberState::Removed
            || claimed.contains(member.id.as_str())
            || busy.contains(member.id.as_str())
            || !session_of(member, seen).is_some_and(|found| found.live && found.free)
        {
            continue;
        }
        busy.insert(member.id.as_str());
        if member.model_switch.is_some() {
            steps.push(Step::ApplyModel { member: member.id.clone() });
        }
        if member.prompt_change.is_some() {
            steps.push(Step::ReplacePrompt { member: member.id.clone() });
        }
    }
    // C7: a member idle without a ready task while tasks are open starves the
    // plan. The lead hears once after 20 minutes; the episode ends with work.
    // U56/#728: a restart the lead or the owner asked for runs on its own.
    if team.status == TeamStatus::Running {
        for member in team.members.iter().filter(|member| member.restart_pending) {
            steps.push(Step::RestartStuck { member: member.id.clone() });
        }
    }
    // Task 154: new rights reach a running session when it is idle; until it
    // is relaunched it takes no new task.
    for member in team.members.iter().filter(|member| member.rights_relaunch && member.state != MemberState::Removed) {
        busy.insert(member.id.as_str());
        let idle = session_of(member, seen).is_none_or(|found| !found.live || (found.free && !found.asking));
        // The same conversation goes on, an open attempt with it.
        if idle {
            steps.push(Step::RelaunchRights { member: member.id.clone() });
        }
    }
    // U61: an auto-accepted draft whose phases ahead are done and whose plan has
    // no blocker is accepted by Auto here, in its own change; the ordinary ready
    // path then hands its tasks out (launch, rebalance, role routing).
    for phase in super::ops::drafts_ready_to_auto_accept(team, &BTreeSet::new()) {
        steps.push(Step::AutoAcceptDraft { phase });
    }
    let ready_ids = plan::ready(team);
    // Task 164 (owner, 2026-11-07): only work of a phase that can start makes
    // a member starve; a draft, a paused phase or one behind a barrier or the
    // owner's acceptance is the owner's or the plan's wait, not the lead's.
    let staging_open = team.tasks.iter().any(|task| plan::startable(team, task));
    let mut starved_due = Vec::new();
    // U9: while every open task waits for the owner's phase approval, a
    // member with nothing ready is expected; no episode starts or continues.
    let owner_wait = plan::waiting_for_owner(team);
    for member in &team.members {
        if member.id == team.lead || matches!(member.state, MemberState::Removed | MemberState::Paused) {
            continue;
        }
        let Some(session) = member.session.as_ref() else { continue };
        let Some(found) = seen.iter().find(|seen| seen.host == session.host && seen.name == session.name) else { continue };
        let has_work = busy.contains(member.id.as_str()) || team.reserved_for_rework(&member.id, None)
            || ready_ids.iter().any(|id| team.task(*id).is_some_and(|task| task.assignee == member.id));
        // Task 164: a review, QA, acceptance or merge role waits by design; it
        // starves only while a ready task of its kind sits with someone else.
        let waits_by_role = plan::waiting_role(&member.role) && plan::ready_for_role(team, member, &ready_ids).is_empty();
        // U49: a draining member is idle on purpose; its episode ends quietly.
        if !staging_open || owner_wait || has_work || waits_by_role || member.draining || !(found.free && found.live) {
            if member.starved_since.is_some() {
                steps.push(Step::StarvedEnded { member: member.id.clone() });
            }
        } else if member.starved_since.is_none() {
            steps.push(Step::Starving { member: member.id.clone() });
        } else if !member.starved_told
            && now - member.starved_since.unwrap_or(now) >= STARVED_AFTER
        {
            starved_due.push(member.id.clone());
        }
    }
    // Task 164: one team-level digest for every member due, at most once an
    // hour; a member that becomes due meanwhile waits for the next one.
    if !starved_due.is_empty() && team.starved_digest_at.is_none_or(|at| now - at >= STARVED_DIGEST_EVERY) {
        steps.push(Step::PlanStarved { members: starved_due });
    }
    // Task 173: a member building, or working past the grace, with nothing
    // handed to it is unassigned work for the lead at once, never idle.
    if team.status == TeamStatus::Running {
        for member in team.members.iter().filter(|member| member.id != team.lead) {
            let Some(found) = session_of(member, seen) else { continue };
            match (super::shadow::unassigned(team, member, found, now), member.unassigned.is_some()) {
                (Some(what), false) => steps.push(Step::Unassigned {
                    member: member.id.clone(),
                    what,
                    task: task_evidence(team, member),
                }),
                (None, true) if !found.building && (found.free || !super::shadow::handed(team, &member.id).is_empty()) => {
                    steps.push(Step::UnassignedEnded { member: member.id.clone() });
                }
                _ => {}
            }
        }
    }
    // C12: Background work and the budgets declared for it. A job without a
    // budget gets the default red time; a budget ends with its job and never
    // stops anything.
    for member in &team.members {
        if member.state == MemberState::Removed {
            continue;
        }
        let found = session_of(member, seen);
        let background = found.is_some_and(|found| found.background);
        let work = found.and_then(|found| found.work.clone());
        if !background {
            let stale_budget = member.budget.as_ref().is_some_and(|budget| budget.job.is_some() || now - budget.declared_at > 60.0);
            if member.background_since.is_some() || stale_budget {
                steps.push(Step::BackgroundEnd { member: member.id.clone() });
            }
            continue;
        }
        if member.background_since.is_none() {
            steps.push(Step::BackgroundStart { member: member.id.clone() });
        }
        match &member.budget {
            None => {
                let since = member.background_since.unwrap_or(now);
                if !member.background_told
                    && team.settings.background_red_minutes > 0
                    && now - since >= team.settings.background_red_minutes as f64 * 60.0
                {
                    steps.push(Step::BackgroundRed {
                        member: member.id.clone(),
                        command: work.map(|work| work.command).unwrap_or_default(),
                        since,
                    });
                }
            }
            Some(budget) => {
                if budget.job.is_none() {
                    if now - budget.declared_at <= 60.0 {
                        if let Some(work) = work {
                            steps.push(Step::BudgetTie {
                                member: member.id.clone(),
                                command: work.command,
                                since: if work.started_at > 0.0 { work.started_at } else { now },
                            });
                        }
                    } else {
                        steps.push(Step::BackgroundEnd { member: member.id.clone() });
                    }
                } else {
                    let state = budget.state(now);
                    if state == "red" && !budget.red_told {
                        steps.push(Step::BudgetRed { member: member.id.clone() });
                    } else if state != "ok" && !budget.amber_told {
                        steps.push(Step::BudgetAmber { member: member.id.clone() });
                    }
                }
            }
        }
    }
    let refused_recently = |member: &Member| {
        team.tasks.iter().flat_map(|t| &t.attempts)
            .filter(|a| a.member == member.id && a.state == AttemptState::Refused)
            .max_by(|a, b| a.sent_at.partial_cmp(&b.sent_at).unwrap_or(std::cmp::Ordering::Equal))
            .is_some_and(|last| last.refusal_state == member.seen_phase && now - last.sent_at < refusal_pause(last.refusal_count.max(1)))
    };
    let outputs = InProgressOutputs::new(team);
    let own_rework = |member: &Member, task: Option<u32>| {
        task.and_then(|id| team.task(id)).is_some_and(|task| task.rework() && task.assignee == member.id)
    };
    let available = |member: &Member, busy: &BTreeSet<&str>, task: Option<u32>| {
        !matches!(member.state, MemberState::Removed | MemberState::Paused)
            && (!member.draining || own_rework(member, task))
            && !busy.contains(member.id.as_str())
            && !team.reserved_for_rework(&member.id, task)
            && member.session.is_some()
            && !member.needs_settings()
            && !refused_recently(member)
            && session_of(member, seen).is_some_and(|s| s.free && !s.run_id.is_empty())
    };
    // U45: a member switching models for a ready task is busy with that
    // switch before any stage is given out. Only a member busy with nothing
    // else may take the task once the switch shows.
    let mut switching: BTreeSet<&str> = BTreeSet::new();
    for task in ready_ids.iter().filter_map(|id| team.task(*id)) {
        if task.waits.is_some() || !super::models::applies(team, task) {
            continue;
        }
        if let Some(pending) = super::models::pending(task) {
            if busy.insert(pending.member.as_str()) {
                switching.insert(pending.member.as_str());
            }
        }
    }
    // U36: a waiting stage goes to its first free candidate (the pinned
    // member, or a member with its role who did not implement the task),
    // before new tasks: it finishes work already done.
    if team.status == TeamStatus::Running {
        for (task, candidates) in super::stages::waiting(team) {
            if let Some(member) = candidates.iter().filter_map(|id| team.member(id)).find(|member| available(member, &busy, Some(task))) {
                busy.insert(member.id.as_str());
                steps.push(Step::StageOut { task, member: member.id.clone() });
                continue;
            }
            // Task #26: no candidate is available and the member this stage
            // would go to has no session: start it, so the stage is not left
            // waiting for a member that never starts.
            if let Some(member) = candidates.iter().filter_map(|id| team.member(id)).find(|member| may_start(member)) {
                if launching.insert(member.id.clone()) {
                    steps.push(Step::Launch { member: member.id.clone() });
                }
            }
        }
    }
    for (task, run) in super::stages::to_clean(team) {
        steps.push(Step::StageCleanup { task, run });
    }
    // #538 N4: a closed stage's own run ended; its folder and checkout are let go.
    for (task, run) in super::stages::runs_ended(team) {
        steps.push(Step::StageRunEnded { task, run });
    }
    // #630 N7: a stage given to a member that was removed or paused, or whose
    // hand-off was never confirmed, goes back to Waiting for a free member.
    for (task, run, reason) in super::stages::stalled(team, now) {
        steps.push(Step::StageStalled { task, run, reason });
    }
    // #635 N3: an unconfirmed hand-off is stalled only with evidence it did not
    // arrive: its member is idle and free since delivery, no stage question is
    // open and no run claim stands under the run id.
    for task in &team.tasks {
        let Some(run) = task.pipeline_run.as_ref().and_then(|pipeline| pipeline.open_run()) else { continue };
        if run.state != StageState::Delivered || !run.unconfirmed || run.member.is_empty() {
            continue;
        }
        let delivered = run.delivered_at.unwrap_or(run.at);
        if now - delivered < super::stages::STAGE_UNCONFIRMED_GRACE {
            continue;
        }
        let Some(member) = team.member(&run.member) else { continue };
        let Some(found) = session_of(member, seen) else { continue };
        if hand_off_missing(team, run, found, delivered) {
            steps.push(Step::StageStalled { task: task.id, run: run.id.clone(), reason: "the stage hand-off was not confirmed".into() });
        }
    }
    // #635 N1: a stage member's turn that ended after the hand-off without a
    // verdict, and with no stage question open, is a stalled item for the lead.
    // The owner (2026-11-01): waiting for Background work (the reviewer's
    // tests) or for a Zerus build is not a stall, so that turn is not silent
    // while either runs; the budget or the block message reaches the lead.
    for task in &team.tasks {
        let Some(run) = task.pipeline_run.as_ref().and_then(|pipeline| pipeline.open_run()) else { continue };
        if run.state != StageState::Delivered || run.member.is_empty() || !run.silent.is_empty() {
            continue;
        }
        if run.questions.iter().any(|question| question.open()) {
            continue;
        }
        let Some(member) = team.member(&run.member) else { continue };
        let Some(found) = session_of(member, seen) else { continue };
        let delivered = run.delivered_at.unwrap_or(run.at);
        if found.free && !found.background && !found.building && found.reply_at.unwrap_or(0.0) > delivered {
            steps.push(Step::StageSilent { task: task.id, run: run.id.clone() });
        }
    }
    // Task 160: a parked task whose condition held goes back to its member at
    // its next free turn, before any new task.
    if team.status == TeamStatus::Running {
        for task in team.tasks.iter().filter(|task| task.park.as_ref().is_some_and(|park| park.state == super::park::MET)) {
            let Some(member) = task.park.as_ref().and_then(|park| team.member(&park.member)) else { continue };
            if available(member, &busy, Some(task.id)) {
                busy.insert(member.id.as_str());
                steps.push(Step::Resume { task: task.id, member: member.id.clone() });
            }
        }
    }
    // C6: among a member's ready tasks the one with the most transitive
    // dependants goes first, then the lowest number.
    let ready = plan::dispatch_order(team, ready_ids);
    // Task 155: the outputs of the tasks taken in this tick's order; a later
    // task writing the same files waits for them. Task 263: a task claims them
    // only when the tick actually hands it out (or starts its member for it) -
    // a task that merely waits for a busy member does not hold a later
    // member's ready task, and once it can start the later task's work holds
    // it back instead (plan::output_holders).
    let mut claimed: Vec<&Task> = Vec::new();
    // Owner, 2026-11-09 ("приоритет выдачи поправь"): a member first takes the
    // ready tasks the plan gave it; it takes another member's task only when
    // none of its own is ready. Without this, an earlier task in the order
    // pulled a free member away and its own task went to yet another member,
    // across roles (records' #5 to export, export's #10 to core).
    let own_ready: BTreeSet<String> = ready
        .iter()
        .filter_map(|id| team.task(*id))
        .filter(|task| task.waits.is_none() && !task.assignee.is_empty())
        .filter_map(|task| team.member(&task.assignee))
        .filter(|member| member.session.is_some() && available(member, &busy, None))
        .map(|member| member.id.clone())
        .collect();
    for id in ready {
        let Some(task) = team.task(id) else { continue };
        // Spec 0: a refused or unconfirmed step waits for the lead or you.
        if task.waits.is_some() {
            continue;
        }
        // Rework waits for this exact member, including a native approval,
        // usage limit or stopped session; none of these selects a peer.
        if task.rework() && !team.member(&task.assignee).is_some_and(|member| {
            member.limited.is_none() && session_of(member, seen).is_some_and(|found| {
                found.live && found.free && !found.asking && found.limited.is_none()
            })
        }) {
            continue;
        }
        // Task 155 (owner, 2026-11-07): tasks writing the same files never run
        // apart on separate branches. One waits while an open task with an
        // overlapping output runs, or while an earlier one in this order waits
        // for its member; it never goes to another member to run beside it.
        // Task 259: only files and module paths hold; a folder-level output is
        // advisory and never holds a whole area.
        if !plan::output_holders(team, task).is_empty() || claimed.iter().any(|other| other.narrow_output_overlaps(task)) {
            continue;
        }
        // U45: a task with models or for a role goes to a member that can run
        // one of its models, switched first when it must be.
        if super::models::applies(team, task) {
            if let Some(pending) = super::models::pending(task) {
                // The member is busy with this task's switch until it shows
                // (marked before the stages); it gets the task only when it is
                // available and was busy with nothing else, once per tick.
                let member = team.member(&pending.member);
                let found = member.and_then(|member| session_of(member, seen));
                if let (Some(member), Some(_)) = (member, found.filter(|found| super::models::confirmed(pending, &found.model, &found.effort))) {
                    if switching.contains(member.id.as_str()) && available(member, &BTreeSet::new(), Some(id)) && super::stages::held(team, &member.id).is_none() {
                        switching.remove(member.id.as_str());
                        claim_outputs(&mut claimed, task);
                        steps.push(deliver(task, member));
                    }
                } else if now - pending.at >= super::models::CONFIRM_SECONDS {
                    steps.push(Step::ModelUncertain { task: id });
                }
                continue;
            }
            if !super::models::can_run(team, task) {
                steps.push(Step::TaskWaits { task: id, reason: super::models::no_fit_reason(team, task) });
                continue;
            }
            let failed = |m: &Member| {
                !team.requires_assignee(task) && task.failed_by(&m.id)
            };
            let output_blocks = !task.requests_fresh_retry() && outputs.blocks(task);
            let candidates = super::models::candidates(team, task);
            // Task 154: only a member whose launch rights do the task.
            let capable: Vec<&Member> = candidates.iter().copied().filter(|m| super::rights::missing(task, m).is_empty()).collect();
            if capable.is_empty() {
                if let Some(reason) = candidates.first().and_then(|first| super::rights::refusal(team, task, first)) {
                    steps.push(Step::RightsWait { task: id, reason });
                }
                continue;
            }
            let candidates = capable;
            let fits: Vec<(&Member, super::models::Fit)> = candidates
                .iter()
                .filter(|m| available(m, &busy, Some(id)) && !failed(m) && super::stages::held(team, &m.id).is_none())
                .filter(|m| m.id == task.assignee || !output_blocks)
                .filter_map(|m| {
                    let found = session_of(m, seen)?;
                    let current = if found.model.is_empty() { super::models::launch_model(m) } else { (found.model.as_str(), found.effort.as_str()) };
                    super::models::fit(m, task, current).map(|fit| (*m, fit))
                })
                .collect();
            // A member already on an allowed model goes before one that switches.
            let chosen = fits.iter().find(|(_, fit)| *fit == super::models::Fit::Ready).or_else(|| fits.first());
            match chosen {
                Some((member, super::models::Fit::Ready)) => {
                    busy.insert(member.id.as_str());
                    claim_outputs(&mut claimed, task);
                    steps.push(deliver(task, member));
                }
                Some((member, super::models::Fit::Switch(choice))) => {
                    busy.insert(member.id.as_str());
                    let found = session_of(member, seen);
                    let from = found.filter(|found| !found.model.is_empty()).map(|found| super::models::label(&found.model, &found.effort)).unwrap_or_default();
                    claim_outputs(&mut claimed, task);
                    steps.push(Step::SwitchModel { task: id, member: member.id.clone(), from, model: choice.model.clone(), effort: choice.effort.clone() });
                }
                None => {
                    // RogueDOD (2026-11-12): a started DeepSeek member whose
                    // role chose a model or effort is not `available` until its
                    // settings are applied, and nothing else applies them for a
                    // task for a role, so three idle secretaries never got a
                    // task. The free one takes its settings first; a later tick
                    // hands it the task, as an assignee's task does.
                    if let Some(member) = candidates.iter().find(|m| {
                        m.needs_settings()
                            && !failed(m)
                            && (m.id == task.assignee || !output_blocks)
                            && super::stages::held(team, &m.id).is_none()
                            && !matches!(m.state, MemberState::Removed | MemberState::Paused)
                            && (!m.draining || own_rework(m, Some(id)))
                            && !busy.contains(m.id.as_str())
                            && !team.reserved_for_rework(&m.id, Some(id))
                            && m.session.is_some()
                            && !refused_recently(m)
                            && session_of(m, seen).is_some_and(|s| s.free && !s.run_id.is_empty())
                    }) {
                        busy.insert(member.id.as_str());
                        claim_outputs(&mut claimed, task);
                        steps.push(Step::Settings { member: member.id.clone() });
                        continue;
                    }
                    // Start the first member that could run it once it is up.
                    if let Some(member) = candidates.iter().find(|m| {
                        !team.reserved_for_rework(&m.id, Some(id)) && !failed(m) && (m.id == task.assignee || !output_blocks)
                            && m.session.is_none() && m.state != MemberState::Paused && m.launch_id.is_empty() && m.error.is_empty()
                            && super::models::fit(m, task, super::models::launch_model(m)).is_some()
                    }) {
                        if launching.insert(member.id.clone()) {
                            claim_outputs(&mut claimed, task);
                            steps.push(Step::Launch { member: member.id.clone() });
                        }
                    }
                }
            }
            continue;
        }
        let Some(member) = team.member(&task.assignee) else { continue };
        // C1: the lead's own task is handed out without a message; it goes to
        // the inbox and never waits for the lead to be idle.
        if member.id == team.lead {
            if matches!(member.state, MemberState::Removed | MemberState::Paused) || busy.contains(member.id.as_str()) || team.reserved_for_rework(&member.id, Some(id)) {
                continue;
            }
            if member.session.is_none() {
                if member.launch_id.is_empty() && member.error.is_empty() && launching.insert(member.id.clone()) {
                    steps.push(Step::Launch { member: member.id.clone() });
                }
                continue;
            }
            busy.insert(member.id.as_str());
            claim_outputs(&mut claimed, task);
            steps.push(Step::HandOut { task: id, member: member.id.clone() });
            continue;
        }
        // Task 154: a task its member's launch cannot do waits for the lead,
        // with who can do it and which rights card fixes it.
        if let Some(reason) = super::rights::refusal(team, task, member) {
            steps.push(Step::RightsWait { task: id, reason });
            continue;
        }
        // Members with one role do the same work: a ready task goes to a free
        // one when its assignee is busy. Only an explicit fresh rejection
        // prefers a member that did not fail it; rework keeps its member.
        // Task 259 (owner, 2026-11-08): no idle member while a ready task
        // waits. A task with free outputs goes to an idle member of a
        // compatible role when its assignee cannot take it: the same role, or
        // one that does the same kind of work (Frontend SolidJS and Frontend
        // DeepSeek share "Frontend"). Rights decide; the exact role and the
        // plan's assignee only order the choice. A member Zerus has not
        // started yet keeps its task - the plan's assignee is launched for it;
        // only one that has a session and still cannot work now (busy, held by
        // a stage, failed, or already given another task) is replaced.
        // U36: a test holding the assignee's folder keeps its next task back;
        // a free member of a compatible role may take it.
        let held = super::stages::held(team, &member.id).is_some();
        if !team.requires_assignee(task) && (task.requests_fresh_retry() || !outputs.blocks(task)) {
            let assignee_fits = available(member, &busy, Some(id)) && !task.failed_by(&member.id) && !held;
            if !assignee_fits {
                // Task 259: the idle member of a compatible role, with the
                // rights, whose own work in progress does not write the same
                // files; the plan's assignee is only replaced once started.
                let peer = super::dispatcher_rebalance::peer(
                    team,
                    task,
                    member,
                    member.session.is_some(),
                    &|other| available(other, &busy, Some(id)) && !own_ready.contains(&other.id),
                    &outputs,
                );
                if let Some(peer) = peer {
                    busy.insert(peer.id.as_str());
                    claim_outputs(&mut claimed, task);
                    steps.push(Step::Rebalance { task: id, from: member.id.clone(), to: peer.id.clone() });
                    continue;
                }
                // Owner, 2026-11-09 (B1 waited 27 minutes for a busy assignee
                // while a member of its role was never started): when the
                // started assignee is busy and no started member can take the
                // task, the first unstarted member of the same role (then of a
                // related one) is started; a later tick hands the task over.
                if member.session.is_some() {
                    let role = team.role_of(&member.id);
                    let unstarted = |other: &&Member| other.id != member.id && other.id != team.lead
                        && other.session.is_none() && other.launch_id.is_empty() && other.error.is_empty()
                        && !matches!(other.state, MemberState::Removed | MemberState::Paused)
                        && !launching.contains(&other.id)
                        && super::rights::missing(task, other).is_empty();
                    let spare = team.members.iter().filter(unstarted).find(|other| team.role_of(&other.id) == role)
                        .or_else(|| team.members.iter().filter(unstarted).find(|other| super::model::related_roles(&role, &team.role_of(&other.id))));
                    if let Some(spare) = spare {
                        launching.insert(spare.id.clone());
                        steps.push(Step::Launch { member: spare.id.clone() });
                    }
                }
            }
        }
        if matches!(member.state, MemberState::Removed | MemberState::Paused) || (member.draining && !own_rework(member, Some(id))) || busy.contains(member.id.as_str()) || held
            || team.reserved_for_rework(&member.id, Some(id)) {
            continue;
        }
        if member.session.is_none() {
            if member.launch_id.is_empty() && member.error.is_empty() && launching.insert(member.id.clone()) {
                claim_outputs(&mut claimed, task);
                steps.push(Step::Launch { member: member.id.clone() });
            }
            continue;
        }
        if refused_recently(member) || !session_of(member, seen).is_some_and(|s| s.free && !s.run_id.is_empty()) {
            continue;
        }
        busy.insert(member.id.as_str());
        claim_outputs(&mut claimed, task);
        steps.push(if member.needs_settings() { Step::Settings { member: member.id.clone() } } else { Step::HandOut { task: id, member: member.id.clone() } });
    }
    // Task 158 (owner, 2026-11-07): there is no work ahead or start ahead any
    // more; a draft's tasks wait for its approval, and an approved phase's
    // tasks are on the ready path above, alongside the phases before it.
    // Task 161: a member's attempt that may have become a wait loop (its own
    // waiting job, the same read-only check repeated) is looked at, at most
    // every five minutes, ten minutes after its hand-out.
    if team.status == TeamStatus::Running {
        for task in team.tasks.iter().filter(|task| task.park.is_none()) {
            let Some(attempt) = task.open_attempt() else { continue };
            let Some(member) = team.member(&attempt.member).filter(|member| member.id != team.lead && member.session.is_some()) else { continue };
            // Only a busy session (a turn or a run going on) can be looping.
            let busy = member.session.as_ref().is_some_and(|session| seen.iter().any(|found| found.host == session.host && found.name == session.name && !found.free));
            let start = attempt.delivered_at.unwrap_or(attempt.sent_at);
            if attempt.state == AttemptState::Delivered
                && attempt.report.is_none()
                && attempt.open_question().is_none()
                && now - start >= super::wait_loops::DETECT_AFTER
                && now - attempt.wait_checked_at >= super::wait_loops::DETECT_EVERY
                && busy
            {
                steps.push(Step::DetectWait { task: task.id });
            }
        }
    }
    // Task 156: decisions reach the work: a task in flight a new decision
    // binds is told once; a report is checked against them before accept;
    // the specs they amend get their generated section.
    for task in &team.tasks {
        let Some(attempt) = task.open_attempt().filter(|attempt| attempt.member != team.lead) else { continue };
        if let Some(decision) = super::decisions::current(team).find(|decision| {
            decision.at > attempt.sent_at && !attempt.decisions_told.contains(&decision.id) && super::decisions::binds(team, decision, task).is_some()
        }) {
            steps.push(Step::TellDecision { task: task.id, decision: decision.id.clone() });
        }
    }
    if super::decisions::current(team).any(|decision| !decision.forbids.is_empty()) {
        for task in &team.tasks {
            if task.attempts.iter().rev().find(|attempt| attempt.state == AttemptState::Reported).is_some_and(|attempt| !attempt.decisions_scanned) {
                steps.push(Step::ScanReport { task: task.id });
            }
        }
    }
    if team.status == TeamStatus::Running && !super::decisions::specs(team).is_empty() && super::decisions::specs_hash(team) != team.decision_specs {
        steps.push(Step::SpecDecisions);
    }
    // Task 134: unread team messages ring at the member's next idle moment,
    // never into a busy composer, and once more past their bound; a member
    // given a task, a stage or a message this tick is not rung again (that
    // delivery rings for everything unread).
    for member in team.members.iter().filter(|member| member.state != MemberState::Removed) {
        let given = steps.iter().any(|step| match step {
            Step::HandOut { member: to, .. } | Step::StageOut { member: to, .. } | Step::Resume { member: to, .. } => *to == member.id,
            Step::Rebalance { to, .. } => *to == member.id,
            Step::Inbox => member.id == team.lead,
            _ => false,
        });
        if given || !super::doorbell::due(member, now) {
            continue;
        }
        // Task #42 (team ede724c4): never while Zerus compacts or clears the
        // member's context for a hand-out. A DeepSeek host treats any message
        // as consuming the compaction ("cancelled"): on 2026-11-10 a lead's
        // message rang dsh2 20 s into the compaction before task 26, which then
        // went out on the uncompacted cold context. The message rings once the
        // context step is settled (the hand-out itself rings for it).
        if context_pending(team, member) {
            continue;
        }
        if session_of(member, seen).is_some_and(|found| found.free && can_take_a_message(found)) {
            steps.push(Step::RingMessages { member: member.id.clone() });
        }
    }
    // Task 277 (owner, 2026-11-08): an urgent message interrupts a busy
    // member's turn natively and then opens a new turn that carries it and
    // says to continue the task (the attempt goes on); ordinary messages still
    // wait for the turn end. A member handed a task, a stage or a resume this
    // tick gets that delivery first, and a question or approval is a safe
    // point of its own.
    for member in team.members.iter().filter(|member| member.state != MemberState::Removed) {
        let given = steps.iter().any(|step| match step {
            Step::HandOut { member: to, .. } | Step::StageOut { member: to, .. } | Step::Resume { member: to, .. } => *to == member.id,
            Step::Rebalance { to, .. } => *to == member.id,
            _ => false,
        });
        if let Some(step) = urgent_step(team, member, seen, given) {
            steps.push(step);
        }
    }
    // U49: a draining member finishes its open attempt and stage run, takes
    // nothing new, then its session is paused and the member leaves the roster.
    // Its queued work went to a peer above; while it still owes anything the
    // drain waits.
    for member in &team.members {
        if !member.draining || member.id == team.lead || member.state == MemberState::Removed {
            continue;
        }
        if !drained(team, &member.id) {
            continue;
        }
        // A computer that did not answer leaves the drain to the next tick, and
        // a turn still in flight is its last work: pause only an idle session.
        if listed.is_some_and(|hosts| member.session.as_ref().is_some_and(|session| !hosts.contains(&session.host))) {
            continue;
        }
        let found = session_of(member, seen);
        if found.is_some_and(|found| found.live && !found.free) {
            continue;
        }
        // U49: an agent with no separate process to pause (OpenCode) has
        // nothing to keep; every other live session is paused, never killed.
        let pause = found.is_some_and(|found| found.live) && member.agent != "opencode";
        steps.push(Step::Drain { member: member.id.clone(), pause });
    }
    // Task 270 (owner, 2026-11-08): a running team whose settings disagree with
    // its project's profile gets one owner card from the lead proposing the
    // switch, at a safe point: no open attempt holds a folder the switch moves.
    // The project is profiled once and cached on the team, so a tick only
    // compares settings and never runs Git for it.
    if matches!(team.status, TeamStatus::Running | TeamStatus::Paused) {
        let open = team.proposals.iter().any(|proposal| proposal["kind"] == "working-mode" && proposal["state"] == "open");
        // Task #451 (owner, 2026-11-09): the cached profile is re-read when it
        // is older than MODE_CHECK_SECONDS, so a block left over from an older
        // profile (or an older hgs) stops proposing the wrong mode. A profile
        // written before this field reads as stale once and is then fresh.
        let stale = team.mode_proposal.as_ref().is_some_and(|proposal| {
            now - proposal["checked_at"].as_f64().unwrap_or(0.0) >= MODE_CHECK_SECONDS
        });
        if team.mode_proposal.is_none() || stale {
            // One re-read now; the card for the fresh profile comes on a later
            // tick, so a stale cache never proposes a mode the project lost.
            if !open && team.tasks.iter().all(|task| task.open_attempt().is_none()) {
                steps.push(Step::ModeCheck);
            }
        } else if !open {
            if let Some((preset, wrong)) = super::working_mode::cached_disagreement(team) {
                let offered = team.proposals.iter().any(|found| {
                    found["kind"] == "working-mode" && found["change"]["preset"].as_str() == Some(preset.as_str())
                });
                if !offered && team.tasks.iter().all(|task| task.open_attempt().is_none()) {
                    let title = team
                        .mode_proposal
                        .as_ref()
                        .and_then(|proposal| proposal["title"].as_str())
                        .unwrap_or_default()
                        .to_owned();
                    steps.push(Step::WorkingMode { preset, title, reasons: wrong });
                }
            }
        }
    }
    // Task 271 (owner, 2026-11-08): the lead's brief comes again at a phase
    // start and after every compaction or clear of its session: the method and
    // the rules, not only the plan. One fingerprint (the session's run and
    // conversation, the open phase, and the context reading) keeps it to once
    // per change; a compaction or a clear drops the reading.
    if matches!(team.status, TeamStatus::Running | TeamStatus::Paused)
        && team.brief == Notice::None
        && team.playbook.is_some()
    {
        if let Some(session) = team.member(&team.lead).and_then(|member| member.session.as_ref()) {
            if let Some(found) = seen.iter().find(|found| found.host == session.host && found.name == session.name && found.free) {
                let phase = current_phase(team);
                let used = team.member(&team.lead).and_then(|member| member.context.as_ref()).map_or(0, |context| context.used);
                let briefed = team.lead_briefed.as_ref();
                let same = briefed.is_some_and(|briefed| {
                    briefed["run"].as_str() == Some(found.run_id.as_str())
                        && briefed["conversation"].as_str() == Some(found.conversation_id.as_str())
                        && briefed["phase"].as_u64() == Some(u64::from(phase))
                        && !compacted(briefed, used)
                });
                // A team that never had its planning brief is not briefed
                // here: the first real brief starts this coaching.
                if !same && !(briefed.is_none() && team.brief_id.is_empty()) {
                    let reason = match briefed {
                        None => "the team started",
                        Some(briefed) if briefed["phase"].as_u64() != Some(u64::from(phase)) => "the phase started",
                        Some(briefed) if compacted(briefed, used) => "the lead's context was compacted or cleared",
                        _ => "the lead's session was resumed",
                    };
                    steps.push(Step::LeadBrief { reason: reason.to_owned() });
                }
            }
        }
    }
    coalesce_lead(steps)
}

/// Task 271: whether the lead's context reading fell well below the one the last
/// brief saw: a compaction or a clear.
fn compacted(briefed: &Value, used: u64) -> bool {
    let before = briefed["used"].as_u64().unwrap_or(0);
    before > 0 && used > 0 && used.saturating_mul(10) < before.saturating_mul(6)
}

/// U49: whether a draining member owes nothing any more: no open attempt, no
/// stage run it still holds, no task it still has to start or work on, and no
/// report of its own. A task at the lead or in a stage is the lead's now.
fn drained(team: &Team, member: &str) -> bool {
    let open_attempt = team
        .tasks
        .iter()
        .any(|task| task.open_attempt().is_some_and(|attempt| attempt.member == member));
    let open_stage = team.tasks.iter().any(|task| {
        task.pipeline_run
            .as_ref()
            .and_then(PipelineRun::open_run)
            .is_some_and(|run| run.state == StageState::Delivered && run.member == member)
    });
    let work = team.tasks.iter().any(|task| task.assignee == member && (task_owes(task) || task.rework()));
    !open_attempt && !open_stage && !work && team.reports_of(member).is_empty()
}

/// U49: a task still to start or work on, as opposed to one at the lead or in
/// a stage of its pipeline (which no implementer waits for).
fn task_owes(task: &Task) -> bool {
    matches!(task.status, TaskStatus::Draft | TaskStatus::Pending | TaskStatus::Assigned | TaskStatus::InProgress)
}

/// Task #334: the member holds or was handed a task - one of its own attempts
/// is open, or a pipeline stage was delivered to it - so its exit loses work
/// that must go on. An attempt a crash already gave back is not work here: its
/// relaunch did not come up, so the ordinary live-sighting guard applies and a
/// failing agent does not restart for ever.
fn holds_work(team: &Team, member_id: &str) -> bool {
    team.tasks
        .iter()
        .filter_map(|task| task.open_attempt())
        .any(|attempt| attempt.member == member_id && !(attempt.state == AttemptState::Unconfirmed && attempt.error.contains("the session crashed")))
        || super::stages::busy(team).contains(&member_id)
}

/// DCB-svn report (2026-11-12): asks a member whose turn ended on a temporary
/// provider error to continue its task, once for that error: a guarded send
/// that is never retried; the lead's inbox item says what happened.
fn continue_after_failure(store: &mut Store, team: &Team, member_id: &str) -> anyhow::Result<()> {
    let Some(member) = team.member(member_id) else { return Ok(()) };
    let Some(failure) = member.turn_failed.clone().filter(|failure| failure.continued_at.is_none()) else { return Ok(()) };
    let Some(session) = member.session.clone() else { return Ok(()) };
    let task = team.tasks.iter().find_map(|task| task.open_attempt().filter(|attempt| attempt.member == member_id).map(|_| task.id));
    let header = match task {
        Some(id) => format!("[Zerus team \u{b7} from: Zerus \u{b7} task: #{id}]"),
        None => "[Zerus team \u{b7} from: Zerus]".to_owned(),
    };
    let text = format!(
        "{header}\nYour last turn ended on a provider error: {}. {}",
        failure.detail.trim_end_matches('.'),
        super::doorbell::CONTINUE
    );
    let request = uuid::Uuid::new_v4().to_string();
    let outcome = send(&session, &request, &text);
    let at = now();
    let error = match &outcome {
        Sent::Refused(error) | Sent::Uncertain(error) => error.clone(),
        Sent::Delivered => String::new(),
    };
    change(store, &team.id, |team, events| {
        let role = team.role_of(member_id);
        let found = team.members.iter_mut().find(|found| found.id == member_id).context("unknown member")?;
        // The send is never repeated for this failure, whatever its outcome.
        if let Some(stored) = found.turn_failed.as_mut().filter(|stored| stored.same(&failure)) {
            stored.continued_at = Some(at);
        }
        found.failure_continues.retain(|then| at - *then <= RESTART_WINDOW);
        found.failure_continues.push(at);
        events.push(Event::new(
            "failure_continued",
            json!({"member":role,"task":task,"detail":failure.detail,"certain":matches!(outcome, Sent::Delivered),"error":error}),
        ));
        Ok(())
    })
}

/// U56/#728: interrupts a stuck member's session and resumes it in its own
/// conversation; the lead (with lead_restarts_stuck) and the owner have this
/// route. A member without a session keeps today's failed-launch relaunch.
fn restart_stuck(store: &mut Store, team_id: &str, member_id: &str) -> anyhow::Result<()> {
    let team = store.load(team_id)?.with_context(|| format!("Unknown team {team_id}"))?;
    let Some(member) = team.member(member_id) else { return Ok(()) };
    let role = team.role_of(member_id);
    let Some(session) = member.session.clone() else {
        return clear_restarted(store, team_id, member_id, &role);
    };
    let input = json!({"run_id":session.run_id,"conversation_id":session.conversation_id});
    // An interrupt of an already stopped session fails; the resume is what
    // brings it back, so only the resume's outcome decides.
    let _ = hgs(&session.host, &["interrupt", &session.name, "--json"], Some(&input), Duration::from_secs(60));
    let resumed = hgs(&session.host, &["resume", &session.name, "--json"], Some(&input), Duration::from_secs(60))?;
    anyhow::ensure!(
        resumed.status.success(),
        "The session of {role} was not resumed: {}",
        String::from_utf8_lossy(&resumed.stderr).trim()
    );
    clear_restarted(store, team_id, member_id, &role)
}

/// Task 154 (2): the member's session takes its new rights: Zerus pauses
/// it and launches it again under its name without `--new`, which resumes
/// the same conversation with the new `--permissions` (a DeepSeek session
/// is created again in it with the preset).
fn relaunch_rights(store: &mut Store, team_id: &str, member_id: &str) -> anyhow::Result<()> {
    let team = store.load(team_id)?.with_context(|| format!("Unknown team {team_id}"))?;
    let Some(member) = team.member(member_id).cloned() else { return Ok(()) };
    let role = team.role_of(member_id);
    let Some(session) = member.session.clone() else {
        // Not launched yet: the launch takes the rights.
        return finish_rights(store, team_id, member_id, &role, None);
    };
    let result = (|| -> anyhow::Result<()> {
        let args = relaunch_args(&team, &member, &session)?;
        let input = json!({"run_id":session.run_id,"conversation_id":session.conversation_id});
        // Pausing a stopped session fails; the launch decides.
        let _ = hgs(&session.host, &["pause", &session.name, "--json"], Some(&input), Duration::from_secs(60));
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let launched = hgs(&session.host, &refs, None, Duration::from_secs(120))?;
        anyhow::ensure!(launched.status.success(), "{}", String::from_utf8_lossy(&launched.stderr).trim());
        Ok(())
    })();
    finish_rights(store, team_id, member_id, &role, result.err().map(|error| format!("{error:#}")))
}

/// The launch that resumes the member's session with its rights: the same
/// agent, folder and name, no `--new`.
pub(crate) fn relaunch_args(team: &Team, member: &Member, session: &SessionRef) -> anyhow::Result<Vec<String>> {
    let folder = member_folder_path(team, member)?;
    // A session name is agent/project/tag; the tag is what -n names.
    let parts: Vec<&str> = session.name.split('/').collect();
    anyhow::ensure!(
        parts.len() == 3 && parts[0] == member.agent && !parts[2].is_empty(),
        "The session {} has no name this launch can resume; relaunch it from Zerus",
        session.name
    );
    let mut args: Vec<String> = vec![member.agent.clone(), folder, "-n".into(), parts[2].into()];
    if member.agent != "dsh" && member.agent != "opencode" {
        args.push("-d".into());
    }
    if !member.account.is_empty() {
        args.extend(["--account".into(), member.account.clone()]);
    }
    args.extend(["--permissions".into(), super::rights::launch_selection(member)]);
    Ok(args)
}

fn finish_rights(store: &mut Store, team_id: &str, member_id: &str, role: &str, error: Option<String>) -> anyhow::Result<()> {
    change(store, team_id, |team, events| {
        let Some(found) = team.members.iter_mut().find(|found| found.id == member_id) else { return Ok(()) };
        found.rights_relaunch = false;
        let rights = super::rights::view(found);
        match &error {
            None => events.push(Event::new("member_relaunched", json!({"member":role,"id":member_id,"rights":rights}))),
            // The lead hears it and relaunches the member itself.
            Some(error) => events.push(Event::new(
                "member_rights_failed",
                json!({"member":role,"id":member_id,"rights":rights,"error":error.chars().take(2000).collect::<String>()}),
            )),
        }
        Ok(())
    })
}

/// Task 133: at a member's turn end, its pulse: the member's own note when it
/// gave one during the turn, else a summary of the turn's tool events (the
/// task, the files changed, the last tool or error line). No chat text is
/// read; a turn with nothing concrete writes nothing.
///
/// Task 185: the summary needs the session's events, an `hgs inspect` with a
/// 15 s bound; a hung ssh member would hold the tick for that long at each of
/// its turn ends. The tick only queues the build to the pulse worker and goes
/// on; a test, whose fake hgs lives on its own thread, builds inline.
fn turn_pulse(store: &mut Store, team_id: &str, task_id: u32, ended: f64) {
    let Ok(Some(team)) = store.load(team_id) else { return };
    let Some(task) = team.task(task_id) else { return };
    let Some(attempt) = task.attempts.last() else { return };
    let Some(member) = team.member(&attempt.member).cloned() else { return };
    let start = attempt.delivered_at.unwrap_or(attempt.sent_at);
    let since = attempt.pulse_at.unwrap_or(start).max(start);
    let at = now();
    let noted = store
        .pulses(team_id, Some(&member.id), at, 5)
        .iter()
        .any(|pulse| pulse.source == "note" && pulse.task == task_id && pulse.at > since);
    if !noted {
        if let Some(session) = &member.session {
            let job = super::pulse_worker::Job {
                team: team_id.to_owned(),
                member: member.id.clone(),
                task: task_id,
                title: task.title.clone(),
                folder: member_folder_path(&team, &member).unwrap_or_default(),
                host: session.host.clone(),
                session: session.name.clone(),
                since,
                at,
            };
            queue_pulse(store, job);
        }
    }
    let _ = change(store, team_id, |team, _| {
        if let Some(attempt) = team.task_mut(task_id).and_then(|task| task.attempts.last_mut()) {
            attempt.pulse_at = Some(ended.max(since));
        }
        Ok(())
    });
}

/// Task 185: hands a turn end's pulse build to the worker, which reads the
/// session on its own thread. A test's fake hgs lives on the test's thread, so
/// a test only queues the job there and builds it itself.
#[cfg(not(test))]
fn queue_pulse(_store: &Store, job: super::pulse_worker::Job) {
    super::pulse_worker::SAMPLER.queue(job);
}

// Task 185: the turn ends a test queued, oldest first; the test drains them
// with `take_queued_pulses` and builds them on its own thread.
#[cfg(test)]
thread_local! {
    static QUEUED_PULSES: std::cell::RefCell<Vec<super::pulse_worker::Job>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn queue_pulse(_store: &Store, job: super::pulse_worker::Job) {
    QUEUED_PULSES.with(|queued| queued.borrow_mut().push(job));
}

/// Task 185: the jobs a turn end queued since the last call (a test only).
#[cfg(test)]
fn take_queued_pulses() -> Vec<super::pulse_worker::Job> {
    QUEUED_PULSES.with(|queued| std::mem::take(&mut *queued.borrow_mut()))
}

/// Task 185: stores the turn-end pulses the worker built since the last tick.
fn take_pulses(store: &Store) {
    for ready in super::pulse_worker::SAMPLER.take() {
        super::pulse_worker::apply(store, &ready.job, (ready.done, ready.next, ready.blocked));
    }
}

/// Task 185: how far the pulse prune got through the loaded teams.
static PRUNE_NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Task 185: pulses expire on their own schedule, never at a member's turn end:
/// one team a tick, round-robin, so a finished team's rows go without every
/// turn end loading the team again. Bounded by the store's 500 rows a call.
fn prune_pulses_step(store: &Store, teams: &[Team], at: f64, offset: usize) {
    let Some(team) = teams.get(offset % teams.len().max(1)) else { return };
    if let Err(error) = store.prune_pulses(team, at) {
        eprintln!("hgs team worker: {}: pulses: {error}", team.name);
    }
}

fn clear_restarted(store: &mut Store, team_id: &str, member_id: &str, role: &str) -> anyhow::Result<()> {
    change(store, team_id, |team, events| {
        let Some(found) = team.members.iter_mut().find(|found| found.id == member_id) else { return Ok(()) };
        found.restart_pending = false;
        found.relaunch_exited = false;
        found.stuck = None;
        found.seen = None;
        found.seen_phase.clear();
        found.seen_activity.clear();
        found.seen_event_at = None;
        events.push(Event::new("member_interrupted", json!({"member":role,"id":member_id})));
        events.push(Event::new("member_relaunched", json!({"member":role,"id":member_id})));
        Ok(())
    })
}

/// U62 (#717): one test-queue tick. An empty queue costs no load read; a read
/// is at least 30 s after the previous one; the night window comes from the
/// project's `[schedule]`; each result becomes one notice for the lead's card.
/// U62 (#760): one step of the merge queue: a merge under test decides once
/// all its results are in; else the oldest accepted report merges. Nothing is
/// written while a merge only waits for its tests.
fn merge_queue_tick(store: &mut Store, team_id: &str, now: f64) -> anyhow::Result<()> {
    let team = store.load(team_id)?.with_context(|| format!("Unknown team {team_id}"))?;
    // Task #443 (owner, 2026-11-09): accepted work whose commit is already on
    // the line is landed by itself - no second accept with "merged":true. One
    // Git read, and none when nothing waits.
    let mut changed = false;
    if team.status == TeamStatus::Running {
        let landed = super::merge_queue::landed_by_ancestry(&team, now);
        if !landed.is_empty() {
            // Task #41: the accept's phase checks read Git before the lock.
            super::plan::with_prerequisites_read(&team, "accept", &json!({}), || {
                change(store, team_id, |team, events| super::merge_queue::record_landed(team, &landed, now, events).map(|_| ()))
            })?;
            changed = true;
        }
    }
    let team = match changed {
        true => store.load(team_id)?.with_context(|| format!("Unknown team {team_id}"))?,
        false => team,
    };
    if team.status != TeamStatus::Running || !team.tasks.iter().any(|task| task.merge.is_some()) {
        return Ok(());
    }
    let database = super::test_queue::TestStore::open(false)?;
    if let Some(entry) = team.tasks.iter().filter_map(|task| task.merge.as_ref()).find(|entry| entry.state == "testing") {
        for request in &entry.requests {
            if database.result_of(request)?.is_none() {
                return Ok(());
            }
        }
        // Every result is in; the merge decides without reading the host's
        // classes or the machine config.
        return merge_step(store, team_id, &team, &database, |team, events, queue| {
            super::merge_queue::step_into(team, &database, now, events, || Ok((Vec::new(), Vec::new())), queue)
        });
    }
    // A queued merge (including one rolled back for a retry) merges now. Host
    // classes and the machine config are read after a merge completes, never on
    // a tick that only waits for test results.
    let has_queued = team
        .tasks
        .iter()
        .any(|task| task.merge.as_ref().is_some_and(|entry| matches!(entry.state.as_str(), "queued" | "waiting" | "reverting")));
    if !has_queued {
        return Ok(());
    }
    merge_step(store, team_id, &team, &database, |team, events, queue| {
        super::merge_queue::step_into(
            team,
            &database,
            now,
            events,
            || {
                let config = crate::config::Config::load().map_err(|error| anyhow::anyhow!("{error}"))?;
                Ok((super::test_runners::host_classes(), super::test_runners::configured(&config)))
            },
            queue,
        )
    })
}

/// Task #1 (team ede724c4): a merge-queue outcome that no longer applies to
/// the team as stored; the transaction that found it rolls back.
#[derive(Debug)]
struct MergeSuperseded(String);

impl std::fmt::Display for MergeSuperseded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "the merge queue's outcome no longer applies: {}", self.0)
    }
}

impl std::error::Error for MergeSuperseded {}

#[cfg(test)]
type MergeBetween = Box<dyn FnMut(&mut Store, &str)>;

#[cfg(test)]
thread_local! {
    /// Tests: a change another writer makes between the merge queue's Git
    /// step and its write.
    static MERGE_BETWEEN: std::cell::RefCell<Option<MergeBetween>> = std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(crate) fn set_merge_between(between: Option<MergeBetween>) {
    MERGE_BETWEEN.with(|slot| *slot.borrow_mut() = between);
}

/// Task #1 (team ede724c4): one merge-queue step with no Git under the team
/// store's write lock. The step (git fetch, merge or cherry-pick, merge-base,
/// reset, the generators and the test selection) runs on a copy of `base`, the
/// team read without the lock; a short transaction then applies its changes
/// over the team as stored, enqueues its tests and appends its events. When
/// another writer changed what the step decided on (`merge_queue::superseded`)
/// nothing is written: a merge commit the step made is rolled back and the next
/// tick decides again from the stored team, so a landing is recorded once. A
/// step that changed nothing writes nothing.
fn merge_step(
    store: &mut Store,
    team_id: &str,
    base: &Team,
    database: &super::test_queue::TestStore,
    step: impl FnOnce(&mut Team, &mut Vec<Event>, &mut Vec<super::test_queue::Request>) -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    let mut ours = base.clone();
    let mut events = Vec::new();
    let mut queue = Vec::new();
    step(&mut ours, &mut events, &mut queue)?;
    if ours == *base && events.is_empty() && queue.is_empty() {
        return Ok(());
    }
    #[cfg(test)]
    if let Some(mut between) = MERGE_BETWEEN.with(|slot| slot.borrow_mut().take()) {
        between(store, team_id);
        MERGE_BETWEEN.with(|slot| *slot.borrow_mut() = Some(between));
    }
    let (base_value, our_value) = (serde_json::to_value(base)?, serde_json::to_value(&ours)?);
    let written = change(store, team_id, |team, out| {
        let their_value = serde_json::to_value(&*team)?;
        if let Some(reason) = super::merge_queue::superseded(&base_value, &our_value, &their_value) {
            return Err(MergeSuperseded(reason).into());
        }
        *team = serde_json::from_value(super::store::merge3(&base_value, &our_value, &their_value))
            .context("The merge queue's changes did not merge into the team record")?;
        out.append(&mut events);
        // The test database is its own file: no Git and no wait on this one.
        for request in &queue {
            database.enqueue(request)?;
        }
        Ok(())
    });
    let Err(error) = written else { return Ok(()) };
    let Some(superseded) = error.downcast_ref::<MergeSuperseded>() else { return Err(error) };
    let stored = store.load(team_id)?;
    let undone = super::merge_queue::undo_superseded(base, &ours, stored.as_ref());
    eprintln!("hgs team worker: {}: {superseded}; retried on the next tick{}", base.name, undone.map(|note| format!(" ({note})")).unwrap_or_default());
    Ok(())
}

/// Task 174: the teams whose test queue runs on its thread now.
static TEST_QUEUES: std::sync::Mutex<BTreeSet<String>> = std::sync::Mutex::new(BTreeSet::new());

/// Task 174: runs the team's test queue beside the tick: a thread of its own
/// per team (never two), with its own store; an empty queue starts nothing.
/// Tests drive the queue in the tick itself, with their own state folder.
fn start_test_queue(store: &mut Store, team_id: &str, name: &str) {
    if cfg!(test) {
        if let Err(error) = test_queue_tick(store, team_id, now()) {
            eprintln!("hgs team worker: {name}: test queue: {error}");
        }
        return;
    }
    let queued = super::test_queue::TestStore::open(true).and_then(|database| database.queued(team_id)).map(|list| !list.is_empty());
    if !queued.unwrap_or(false) {
        return;
    }
    {
        let Ok(mut running) = TEST_QUEUES.lock() else { return };
        if !running.insert(team_id.to_owned()) {
            return;
        }
    }
    let (team_id, name) = (team_id.to_owned(), name.to_owned());
    let started = std::thread::Builder::new().name("hgs-test-queue".into()).spawn({
        let team_id = team_id.clone();
        move || {
            let result = Store::open(true).and_then(|mut store| test_queue_tick(&mut store, &team_id, now()));
            if let Err(error) = result {
                eprintln!("hgs team worker: {name}: test queue: {error}");
            }
            if let Ok(mut running) = TEST_QUEUES.lock() {
                running.remove(&team_id);
            }
        }
    });
    if started.is_err() {
        if let Ok(mut running) = TEST_QUEUES.lock() {
            running.remove(&team_id);
        }
    }
}

fn test_queue_tick(store: &mut Store, team_id: &str, now: f64) -> anyhow::Result<()> {
    use chrono::Timelike;
    let team = store.load(team_id)?.with_context(|| format!("Unknown team {team_id}"))?;
    if team.status != TeamStatus::Running {
        return Ok(());
    }
    // Task 174: queued suites run where the lead branch is checked out.
    let project = std::path::Path::new(team.integration_folder());
    let loaded = crate::test_discipline::load(project, Some(&team.id)).map_err(|error| anyhow::anyhow!("{error}"))?;
    let classes = loaded.config.resources.clone();
    let night = loaded.config.schedule.clone();
    let policy = super::test_results::Policy::of(&loaded.config);
    let database = super::test_queue::TestStore::open(false)?;
    // #732: the owner's test runners among this computer's Machines.
    let runners = crate::config::Config::load().map(|config| super::test_runners::configured(&config)).unwrap_or_default();
    let places = super::test_queue::Places { host: super::test_runners::host_classes(), runners, remote: &super::test_runners::Ssh };
    let hour = chrono::Local::now().hour();
    let (records, read) = super::test_queue::tick(
        &database,
        project,
        &team.id,
        &classes,
        &night,
        hour,
        team.test_load_at,
        now,
        super::test_queue::load_now,
        &policy,
        &places,
        team.settings.tests.as_ref(),
    )?;
    if !read {
        return Ok(());
    }
    let estimate = super::test_results::full_estimate(project, &loaded.config);
    change(store, team_id, |team, events| {
        // #718: notices, flaky quarantine and items, budgets, a full run's proposal.
        super::test_results::apply(team, &records, &database, &policy, now, estimate)?;
        for record in &records {
            events.push(Event::new(
                "test_result",
                json!({"suite":record.suite,"status":record.status,"record":record.id,"asked_by":record.asked_by}),
            ));
        }
        Ok(())
    })
}

/// Whether Zerus may hand `task_id` out now: a ready task of an approved phase
/// (task 158: alongside the phases before it). A draft phase's tasks are
/// Draft, which `plan::ready` never lists.
fn can_hand_out(team: &Team, task_id: u32) -> bool {
    if team.status != TeamStatus::Running {
        return false;
    }
    // Task 155: rechecked at the hand-out itself: an open task writing the
    // same files that started meanwhile holds this one back.
    if team.task(task_id).is_some_and(|task| !plan::output_holders(team, task).is_empty()) {
        return false;
    }
    plan::ready(team).contains(&task_id)
}

/// At most one message reaches the free lead per tick; the others wait for the
/// next tick in the order they were found, so one tick never floods it (L9).
fn coalesce_lead(steps: Vec<Step>) -> Vec<Step> {
    let lead_message = |step: &Step| {
        matches!(
            step,
            Step::Inbox
                | Step::RemindReport { .. }
                | Step::PlanStarved { .. }
                | Step::BudgetRed { .. }
                | Step::BackgroundRed { .. }
                | Step::Silent { .. }
                | Step::TellStartLate { .. }
                | Step::BlockedLate { .. }
                | Step::Rebalanced { .. }
                | Step::AllDecided
        )
    };
    let mut told = false;
    steps
        .into_iter()
        .filter(|step| {
            if !lead_message(step) {
                return true;
            }
            if told {
                return false;
            }
            told = true;
            true
        })
        .collect()
}

/// Bounds for the dependency reports a hand-off carries.
const DEPENDENCY_REPORT_CHARS: usize = 2_000;
const DEPENDENCY_REPORTS_CHARS: usize = 6_000;
/// Header lines a hand-off lists before it summarizes the rest.
const DEPENDENCY_LIMIT: usize = 20;

/// The stored report a hand-off shows for one dependency: the reported attempt
/// that accepted it, else the latest attempt with a report. Only report text
/// stored with the attempt is used; journals are never read.
fn dependency_attempt(task: &Task) -> Option<&Attempt> {
    if task.status == TaskStatus::Accepted {
        task.attempts.iter().rev().find(|attempt| attempt.state == AttemptState::Reported && attempt.report.is_some())
    } else {
        task.attempts.iter().rev().find(|attempt| attempt.report.is_some())
    }
}

/// Where a dependency's member works, prefixed with its computer when that is
/// not the lead's; a member without one works on this computer. The lead's own
/// tasks show the team's folder.
fn dependency_folder(team: &Team, member_id: &str, local_host: &str) -> String {
    if member_id == team.lead {
        return team.folder.clone();
    }
    let Some(member) = team.member(member_id) else { return String::new() };
    let folder = member_folder_path(team, member).unwrap_or_default();
    let lead_host = team.member(&team.lead).map(|lead| lead.computer.as_str()).unwrap_or_default();
    if member.computer == lead_host {
        return folder;
    }
    let host = if member.computer.is_empty() { local_host } else { member.computer.as_str() };
    format!("{host}:{folder}")
}

/// The "From dependencies" section: every dependency with a stored report, in
/// `deps` order, each report bounded to 2,000 characters and 6,000 in total,
/// and at most 20 header lines.
fn dependencies_text(team: &Team, task: &Task, local_host: &str) -> String {
    let listed: Vec<(&Task, Option<&Attempt>)> = task
        .deps.iter().filter_map(|dep| team.task(dep.task))
        .filter_map(|found| {
            if found.archived.is_some() { Some((found, None)) }
            else { dependency_attempt(found).map(|attempt| (found, Some(attempt))) }
        }).collect();
    let mut text = String::new();
    let mut used = 0usize;
    for (index, (found, attempt)) in listed.iter().enumerate() {
        if index == DEPENDENCY_LIMIT {
            text += &format!("[{} more dependencies omitted]\n", listed.len() - index);
            break;
        }
        if found.archived.is_some() {
            text += &format!("#{} {}: archived report: {} | hgs team status --json\n",
                found.id, found.title, json!({"team":team.id,"task":found.id}));
            continue;
        }
        let Some(attempt) = attempt else { continue };
        let Some(report) = attempt.report.as_ref() else { continue };
        let status = serde_json::to_value(found.status).ok().and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_default();
        text += &format!(
            "#{} {} ({}) by {} ({}), folder: {}\n",
            found.id,
            found.title,
            status.replace('_', " "),
            attempt.member,
            team.role_of(&attempt.member),
            dependency_folder(team, &attempt.member, local_host)
        );
        let total = report.text.chars().count();
        let room = DEPENDENCY_REPORTS_CHARS.saturating_sub(used);
        if room == 0 {
            text += "[report omitted: limit reached]\n";
            continue;
        }
        let shown = total.min(DEPENDENCY_REPORT_CHARS).min(room);
        text += &report.text.chars().take(shown).collect::<String>();
        if shown < total {
            text += &format!(
                "\n[cut: shown {shown} of {total} characters; full report: {} | hgs team status --json]\n",
                json!({"team":team.id,"task":found.id})
            );
        } else {
            text.push('\n');
        }
        used += shown;
    }
    if text.is_empty() {
        text
    } else {
        format!("\nFrom dependencies:\n{text}")
    }
}

/// The message that gives a member its task.
#[cfg(test)]
pub(crate) fn hand_off_text(team: &Team, task: &Task, attempt: &Attempt, report_host: &str, member_host: &str) -> String {
    hand_off_text_with(team, task, attempt, report_host, member_host, "")
}

/// H6: the hand-off of a swapped member carries the previous attempt's partial
/// report and open questions in `extra`; the git part is read at send time.
#[cfg(test)]
pub(crate) fn hand_off_text_with(
    team: &Team,
    task: &Task,
    attempt: &Attempt,
    report_host: &str,
    member_host: &str,
    extra: &str,
) -> String {
    let rules = team
        .member(&attempt.member)
        .map(|member| super::super::iron_rules::for_team(team, member))
        .transpose()
        .map(|rules| rules.unwrap_or_default())
        .unwrap_or_else(|error| {
            format!("Iron rules unavailable: {error}. Ask the lead before working.\n")
        });
    hand_off_text_rules(team, task, attempt, report_host, member_host, extra, &rules)
}

/// Task #25: whether `folder` has CodeLore's code map: a lore.toml whose
/// `[atlas]` table turns it on. A folder on another computer reads as none.
pub(crate) fn code_map_in(folder: &str) -> bool {
    if folder.is_empty() {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(Path::new(folder).join("lore.toml")) else { return false };
    let mut in_atlas = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_atlas = line == "[atlas]";
        } else if in_atlas && line.replace(' ', "").starts_with("enabled=true") {
            return true;
        }
    }
    false
}

fn hand_off_text_rules(
    team: &Team,
    task: &Task,
    attempt: &Attempt,
    report_host: &str,
    member_host: &str,
    extra: &str,
    rules: &str,
) -> String {
    // Spec 5.6: a member that reports to a sub-lead gets its tasks from it,
    // and its questions, report and blockers go there.
    let reviewer = team.reviewer_of(&attempt.member);
    let (from, boss) = if reviewer.is_empty() || reviewer == team.lead {
        ("Lead".to_owned(), "the lead".to_owned())
    } else {
        let role = team.role_of(&reviewer);
        (role.clone(), role)
    };
    let mut text = format!(
        "[Zerus team \u{b7} from: {from} \u{b7} task: #{} \u{b7} attempt: {}]\nTeam: {}. Your role: {}.\n",
        task.id,
        attempt.n,
        team.name,
        team.role_of(&attempt.member)
    );
    // D2: a reopened task starts with its finding and the lead's note.
    if !task.reopen_note.trim().is_empty() {
        text = format!("[Zerus team \u{b7} finding]\n{}\n\n{text}", task.reopen_note.trim());
    }
    if !team.goal.is_empty() {
        text += &format!("Team goal: {}\n", team.goal);
    }
    let mut task_in_prompt = false;
    let mut rules_in_prompt = false;
    if let Some(prompt) = team.member(&attempt.member).map(|m| m.prompt.trim()).filter(|p| !p.is_empty()) {
        // U25: the role's prompt blocks in its order, when the catalog has one.
        // N3: a member's own snapshot wins; an old member keeps today's live text.
        let snapshot = team.member(&attempt.member).and_then(|member| member.prompt_snapshot.clone());
        let (ordered, has_task) = if let Some(snapshot) = snapshot {
            let has_task = super::catalog::sends_task(&snapshot.prompt_order, &snapshot.prompt_off, &snapshot.task_template);
            (super::catalog::prompt_order_text_snapshot(&snapshot, prompt, Some(task), rules), has_task)
        } else {
            super::catalog::load().ok().and_then(|catalog| {
                let role_name = team.role_of(&attempt.member);
                let role = catalog.roles.iter().find(|role| role.id == role_name || role.name == role_name)?;
                let has_task = super::catalog::sends_task(&role.prompt_order, &role.prompt_off, &role.task_template);
                Some((super::catalog::prompt_order_text(role, prompt, Some(task), rules), has_task))
            }).unwrap_or((None, false))
        };
        task_in_prompt = has_task;
        if let Some(ordered) = ordered.filter(|text| !text.trim().is_empty()) {
            rules_in_prompt = !rules.is_empty();
            text += &ordered;
        } else {
            text += &format!("Role: {prompt}\n");
        }
    }
    if !rules_in_prompt && !rules.is_empty() {
        text += &format!("\n{rules}\n");
    }
    if !task_in_prompt {
        text += &format!("\nTask #{}: {}\n", task.id, task.title);
        if !task.description.is_empty() {
            text += &format!("{}\n", task.description);
        }
        if !task.criteria.is_empty() {
            text += "\nAcceptance criteria:\n";
            for criterion in &task.criteria {
                text += &format!("- {criterion}\n");
            }
        }
    }
    let deps: Vec<String> = task
        .deps
        .iter()
        .filter_map(|dep| team.task(dep.task))
        .map(|dep| {
            let status = serde_json::to_value(dep.status).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
            format!("#{} {} ({})", dep.id, dep.title, status.replace('_', " "))
        })
        .collect();
    if !deps.is_empty() {
        text += &format!("\nDepends on: {}\n", deps.join(", "));
    }
    // Task 155: the accepted work Zerus merged into the member's branch.
    if let Some(step) = attempt.prepare.iter().find(|step| step.step == "dependencies" && step.result == "done") {
        text += &format!("Zerus {} into your branch before this hand-out; build on it.\n", step.reason);
    }
    text += &dependencies_text(team, task, report_host);
    // Task 156: the owner's decisions that bind this task, by relevance.
    text += &super::decisions::block(team, task);
    // Owner 2026-10-31: the hand-out says the task's time estimate.
    text += &super::estimates::prompt_line(team, task);
    if let Some(previous) = task.attempts.iter().rev().find(|a| a.n < attempt.n && a.state == AttemptState::Rejected) {
        // U36: a stage returned it (no lead step): its findings, not a decision.
        let returned = task.pipeline_run.as_ref().and_then(|run| {
            let stage = run.runs.iter().rev().find(|stage| stage.attempt == previous.n && stage.state != super::model::StageState::Revoked)?;
            (stage.state == super::model::StageState::Returned).then(|| (stage.clone(), run.max_rounds))
        });
        // #630 N2: once the lead or you decided the task (a round-limit return,
        // then a rejection, or a send-back), that decision is the latest word,
        // not the stage run that preceded it.
        if let Some((stage, max_rounds)) = returned.filter(|_| task.decided_by.is_none()) {
            text += &format!("\nAttempt {} was returned by the {} of {} (round {} of {max_rounds}). Fix these findings:\n", previous.n, stage.kind, stage.by, stage.round);
            if !stage.text.trim().is_empty() {
                text += &format!("{}\n", stage.text.trim());
            }
            for finding in &stage.findings {
                text += &format!("- {finding}\n");
            }
        } else {
            text += &format!("\nAttempt {} was rejected: {}\n", previous.n, task.decision_reason);
        }
    }
    if let Some(check) = task.check.as_ref().filter(|_| task.is_check()) {
        // C5: the member runs the exact command; Zerus decides by its exit code.
        text += &format!(
            "\nThis is a check task: run exactly this command in your folder, change nothing else, and report its exit code:\n  {}\n\
             Expected exit code: {} (timeout {} min).\n",
            check.command, check.expect_exit, check.timeout_minutes
        );
    }
    // H6: a swapped member continues from what its old session had.
    if !extra.is_empty() {
        text += extra;
    }
    let at = if member_host.is_empty() { String::new() } else { format!("@{report_host} ") };
    let mut report = json!({"request_id":uuid::Uuid::new_v4().to_string(),"team":team.id,"task":task.id,"attempt":attempt.id,"text":"..."});
    if task.is_check() {
        report["text"] = json!("");
        report["check"] = json!({"exit":0,"tail":"the last 40 lines"});
    }
    // H5: every hand-out names the question route, the current-attempt route
    // and the report command.
    text += &format!(
        "\nQuestions for {boss}: {ask} | hgs {at}team ask --json\n\
         Your task and attempt again: {current} | hgs {at}team current --json\n\
         Report once at the end: hgs {at}team report --json, with JSON on stdin:\n  {report}\n\
         A progress line is not a report; a question is not a report.\n\
         Never repair file permissions or ACLs yourself; report the exact error to {boss} with the folder and the command.\n\
         Never install a tool yourself: a missing test or build tool is offered to the owner in Waiting for you \
         (hgs test tools); ask {boss} about it and work without it.\n\
         Summarize what changed, where, and how you checked it. A report is an explicit `hgs {at}team report`: \
         ending your turn without one leaves the task as No report, and {boss} reads your last reply and decides. \
         A blocker goes to {boss} too: ask it, not the owner.",
        ask = json!({"request_id":uuid::Uuid::new_v4().to_string(),"team":team.id,"task":task.id,"attempt":attempt.id,"text":"..."}),
        current = json!({"attempt":attempt.id}),
        report = report
    );
    // U62 (#760): with the merge queue, a task starts from the lead's branch
    // and its accepted report goes back through the queue.
    if super::merge_queue::enabled(team) && !task.is_check() {
        text += "\nThe merge queue is on: merge the lead branch into your folder before you start (the git-merge-lead template). \
                 When the lead accepts your report, Zerus merges your reported commit into the lead branch and runs its affected tests there; \
                 a conflict or a failure comes back to you with the files or the failure.";
    }
    // U21: the routine commands Zerus runs outside the member's sandbox; the
    // owner turned the set off in Settings > Teams when it is off.
    if super::templates::enabled() {
        let ids: Vec<&str> = super::templates::BUILTIN.iter().map(|template| template.id).collect();
        text += &format!(
            "\nZerus runs these commands for you, outside your sandbox, without an approval: {}.\n\
             Ask for one with {run} | hgs team run --json: a new request_id for every run; --wait returns the exit code as soon as it is ready.\n\
             Ask again with the same request_id to read the result - it never runs the command twice.\n\
             Anything else still asks the lead.",
            ids.join(", "),
            run = json!({"request_id":uuid::Uuid::new_v4().to_string(),"team":team.id,"attempt":attempt.id,"template":"cargo-test","args":{}})
        );
    }
    // Task #25: a member whose folder has CodeLore's code map is told to search
    // the code and the docs through it, and the team's memory through Zerus.
    let folder = team
        .member(&attempt.member)
        .map(|member| if member.directory.is_empty() { team.folder.clone() } else { member.directory.clone() })
        .unwrap_or_default();
    if code_map_in(&folder) {
        text += "\nSearch the code and the docs with lore atlas search \"what it does\" in your folder, and the team's \
                 memory with hgs team lore find, before opening files by guesswork (hgs guide code).\n";
    }
    // RogueDOD (2026-11-11): the project's records live in its repository.
    if let Some(top) = super::super::project_memory::repo_store(std::path::Path::new(&folder)) {
        text += &format!(
            "\nThe project's memory lives in its repository: lore -C {} find \"words\" and lore for FILE before you edit, \
             lore add for what you learned (hgs guide code).\n",
            top.display()
        );
    }
    // RogueDOD (2026-11-11): the lore areas this task holds as its output.
    let areas = task.lore_areas();
    if !areas.is_empty() {
        text += &format!(
            "\nYour lore records go only into the area{} {}: this task holds {}, and no other open task writes there now.\n",
            if areas.len() == 1 { "" } else { "s" },
            areas.join(", "),
            if areas.len() == 1 { "it" } else { "them" }
        );
    }
    // U30: a build Zerus runs is work; a job left in the member's own shell is a service.
    text += "\nStart builds and long tests through Zerus (hgs team run, or hgs run: hgs guide run) so your session shows \
             as Building and your reply waits for them; a job you leave in your own shell after your turn counts as a \
             service, and your session looks idle. A background shell you start and wait on is shown to the lead as what \
             you wait on, never as an uncertain status; builds and tests still go through hgs run or hgs team run.";
    text
}

/// U36/#536: the hand-off to a stage member (review or test), sent when the
/// stage delivery (#532) gives the open stage run `run` to its member. It says
/// what to read (the attempt's change), where (the task's own folder), what a
/// verdict must contain (`pass` or `return` with findings) and that a stage
/// never edits the task's files.
#[cfg_attr(not(test), allow(dead_code))] // the stage delivery (#532) sends it
pub(crate) fn stage_hand_off_text(team: &Team, task: &Task, run: &StageRun) -> String {
    let implementer = team.member(&task.assignee);
    let checked = task.attempts.iter().find(|attempt| attempt.n == run.attempt);
    // The change is in the folder of the member that made the attempt, which
    // may differ from the task's assignee after a reassignment.
    let author = checked.and_then(|attempt| team.member(&attempt.member)).or(implementer);
    let implementer_role = author.map(|member| member.role.clone()).unwrap_or_default();
    let commits: Vec<&str> = checked.map(|attempt| attempt.commits.iter().map(|commit| commit.sha.as_str()).collect()).unwrap_or_default();
    let reported: String = checked
        .and_then(|attempt| attempt.report.as_ref())
        .map(|report| report.text.trim().chars().take(700).collect())
        .unwrap_or_default();
    let max_rounds = task.pipeline_run.as_ref().map(|pipeline| pipeline.max_rounds).filter(|rounds| *rounds > 0).unwrap_or(3);
    let folder = author
        .map(|member| if member.directory.is_empty() { team.folder.clone() } else { member.directory.clone() })
        .filter(|folder| !folder.is_empty())
        .unwrap_or_else(|| "(the implementer's folder)".to_owned());
    let shared = author.is_some_and(|member| member.folder == MemberFolder::Shared);
    let mut text = format!(
        "[Zerus team \u{b7} from: Lead \u{b7} task: #{} \u{b7} stage: {} \u{b7} round {} of {}]\nTeam: {}. Your role: {}.\n",
        task.id, run.kind, run.round, max_rounds, team.name, team.role_of(&run.member)
    );
    if !team.goal.is_empty() {
        text += &format!("Team goal: {}\n", team.goal);
    }
    text += &format!("\nTask #{}: {}\n", task.id, task.title);
    if !task.description.is_empty() {
        text += &format!("{}\n", task.description);
    }
    if !task.criteria.is_empty() {
        text += "\nAcceptance criteria:\n";
        for criterion in &task.criteria {
            text += &format!("- {criterion}\n");
        }
    }
    if reported.is_empty() {
        text += &format!("\nThe implementer ({implementer_role}) reported attempt {} without a text.\n", run.attempt);
    } else {
        text += &format!("\nThe implementer ({implementer_role}) reported attempt {}:\n{reported}\n", run.attempt);
    }
    // U36: what changed comes from the attempt - its commits in a Git checkout,
    // otherwise the report's changed files; both are read in the task's folder.
    text += &format!("\nRead attempt {}'s change in the task's own folder: {folder}\n", run.attempt);
    if commits.is_empty() {
        text += "- No commits were recorded: the report names the changed files; read them in that folder as they are now.\n";
    } else {
        text += &format!(
            "- Its commits: {}. Read what each changed with `git show <sha>` (and `git log -p`) in that folder.\n",
            commits.join(", ")
        );
    }
    if shared {
        text += "- This is the team's shared folder: only the files the report names are this task's; the other members keep working in it.\n";
    }
    if run.kind == "test" {
        text += "\nThis is a test stage: run only the suites of the files this change touched and take the criteria's shots; \
                 never the full suites - the lead runs them once at integration. While the task is in its test stage its folder \
                 is held: the implementer's next task in that folder waits.\n";
    } else {
        text += "\nThis is a review stage: read the change against the criteria and name what is wrong by file and line.\n";
    }
    text += "Change nothing in the task's files: a stage reads, builds and tests only, writes only build output and its own \
             temporary folder, and never edits, commits or reverts the task's files.\n";
    let verdict = json!({"request_id":uuid::Uuid::new_v4().to_string(),"team":team.id,"task":task.id,"attempt":run.id,
        "verdict":"pass|return","findings":["file:line - what is wrong"],"text":"..."});
    text += &format!(
        "\nReport a verdict, not a progress line, with hgs team report --json, JSON on stdin:\n  {}\n\
         pass moves the task to its next stage; return sends your findings to the implementer as a new attempt, without the lead. \
         Round {} of {}: after {} returned rounds the lead decides with every round's findings.\n\
         Questions for the lead: {} | hgs team ask --json\n\
         Your stage again: {} | hgs team current --json\n",
        verdict,
        run.round,
        max_rounds,
        max_rounds,
        json!({"request_id":uuid::Uuid::new_v4().to_string(),"team":team.id,"task":task.id,"attempt":run.id,"text":"..."}),
        json!({"attempt":run.id})
    );
    text
}

/// H6: what a swapped member's new session needs from the old attempt.
fn continuation_extra(task: &Task) -> String {
    let Some(continuation) = task.continuation.as_ref() else { return String::new() };
    let mut text = format!(
        "\nYou continue task #{} after Zerus swapped your session (previous attempt {}).\n",
        task.id, continuation.n
    );
    if !continuation.report.is_empty() {
        text += &format!("The previous session's partial report:\n{}\n", continuation.report);
    }
    if !continuation.questions.is_empty() {
        text += "Open questions from the previous session:\n";
        for question in &continuation.questions {
            text += &format!("- {question}\n");
        }
    }
    text += "\nBefore a test run expected to take more than about ten minutes (the full test_sessionswindow, the full Rust suite, repeated GUI runs), ask the lead with `hgs team ask --json`, naming the suite and why.\n";
    text += "The previous session worked in this folder; check the working tree before you continue.\n";
    text
}

/// H6: the commits and working-tree changes since the swapped attempt began.
fn git_continuation(folder: &str, since: f64) -> String {
    if folder.is_empty() {
        return String::new();
    }
    // C13: the bounded Git path - a deadline, bounded output, no optional
    // locks, no fsmonitor and no hooks - so a member's repository can neither
    // run code here nor hold the dispatcher.
    // Task #32 (team ede724c4): each read has its own window (they shared one
    // 20 s deadline, which a busy computer ran out of).
    let run = |args: &[&str]| -> Option<String> {
        let mut hardened = vec!["-c", "core.hooksPath="];
        hardened.extend_from_slice(args);
        let deadline = Instant::now() + super::super::worktrees::read_window(20);
        super::super::worktrees::git(Path::new(folder), &hardened, deadline)
            .ok()
            .map(|out| String::from_utf8_lossy(&out).into_owned())
    };
    // A filter, include, hook path or fsmonitor of the repository's own config
    // could still run code: such a folder is not read (as `commit_in`). Task
    // #32: a config that could not be read is not read past either.
    match run(&["config", "--list", "--show-scope"]) {
        Some(listing) => {
            if let Some(reason) = refused_repository_config(&listing) {
                return format!("\nCommits and working tree: not read here, this repository {reason}.\n");
            }
        }
        None => return "\nCommits and working tree: not read here, Git could not read its config (not a repository, or Git did not answer in time).\n".to_owned(),
    }
    // A missing or zero start time is no date filter: git 2.55 reads
    // `--since @0` as "now" and would hide every commit.
    let since_arg = format!("@{since:.0}");
    let args: Vec<&str> = if since > 0.0 {
        vec!["log", "--format=%h %at %s", "--since", &since_arg, "-n", "20"]
    } else {
        vec!["log", "--format=%h %at %s", "-n", "20"]
    };
    let mut text = String::from("\nCommits since the attempt began (at most 20):\n");
    match run(&args) {
        Some(read) => {
            let lines: Vec<&str> = read.lines().take(20).collect();
            if lines.is_empty() {
                text += "(none)\n";
            } else {
                for line in lines {
                    text += &format!("{line}\n");
                }
            }
        }
        None => text += "(not read)\n",
    }
    text += "Working tree (git status --short, at most 50 lines):\n";
    match run(&["status", "--short"]) {
        Some(read) => {
            let lines: Vec<&str> = read.lines().take(50).collect();
            if lines.is_empty() {
                text += "(clean)\n";
            } else {
                for line in lines {
                    text += &format!("{line}\n");
                }
            }
        }
        _ => text += "(not read)\n",
    }
    text
}

/// The planning brief for the lead of a team created from the desktop: the
/// goal, the members and tasks proposed so far, and how to plan. A lead
/// swapped into a running team gets the short brief (H6).
#[cfg(test)]
pub(crate) fn brief_text(team: &Team) -> String {
    let rules = team
        .member(&team.lead)
        .map(|member| super::super::iron_rules::for_team(team, member))
        .transpose()
        .map(|rules| rules.unwrap_or_default())
        .unwrap_or_else(|error| {
            format!("Iron rules unavailable: {error}. Ask the owner before working.\n")
        });
    brief_text_rules(team, &rules)
}

fn brief_text_rules(team: &Team, rules: &str) -> String {
    // N6: a draft's lead gets the draft brief once: the protocol, the designer
    // guide and the draft id; nothing runs before the owner creates the team.
    if team.status == TeamStatus::Draft {
        return format!(
            "[Zerus team \u{b7} from: You \u{b7} draft]\nYou lead the draft team {} ({}). Read the built-in lead protocol \
             (hgs guide team-lead) and hgs guide designer. The owner and you edit the draft with hgs team draft --json \
             (get or update); nothing runs until the owner creates the team.\n",
            team.name, team.id
        );
    }
    if matches!(team.status, TeamStatus::Running | TeamStatus::Paused) {
        let mut text = format!(
            "[Zerus team \u{b7} from: Zerus \u{b7} brief]\nYou are now the lead of the team {} ({}). \
             Its reports, questions, permission requests and your own tasks wait in one inbox: \
             {{\"team\":\"{}\"}} | hgs team inbox --json. The team state is in hgs team status --json; \
             hgs guide team-lead has the rest. Before you report or finish a phase, \
             {{\"team\":\"{}\",\"phase\":N}} | hgs team profile --json ranks its losses: name the phase's top three \
             by time in the phase report.\n",
            team.name,
            if team.status == TeamStatus::Running { "running" } else { "paused" },
            team.id,
            team.id
        );
        // U54: a lead swapped to another conversation gets the previous lead's
        // context, bounded: its session, conversation and transcript path.
        let lead = team.role_of(&team.lead);
        if let Some(previous) = team.swapped.iter().rev().find(|swap| {
            swap["member"] == lead && !swap["conversation"].as_str().unwrap_or_default().is_empty()
        }) {
            let bounded = |value: &Value| value.as_str().unwrap_or_default().chars().take(500).collect::<String>();
            text += &format!(
                "Previous lead: {} (conversation {}), transcript {}. Its latest compact summary is the last summary in that transcript; read it before you plan anew.\n",
                bounded(&previous["old"]),
                bounded(&previous["conversation"]),
                bounded(&previous["transcript"]),
            );
        }
        // Task #279: the owner's standing rules, cited from the owner's own
        // messages, are recalled in every lead brief.
        text += &super::decisions::rules_block(team);
        return text;
    }
    let members: Vec<Value> = team
        .members
        .iter()
        .filter(|m| m.id != team.lead && m.state != MemberState::Removed)
        .map(|m| {
            json!({"id":m.id,"role":m.role,"agent":m.agent,"account":m.account,"model":m.model,"effort":m.effort,
                "permissions":m.permissions,"computer":m.computer,"folder":m.folder,"prompt":m.prompt})
        })
        .collect();
    let tasks: Vec<Value> = team
        .tasks
        .iter()
        .map(|t| json!({"id":t.id,"title":t.title,"description":t.description,"criteria":t.criteria,"assignee":t.assignee,"deps":t.deps}))
        .collect();
    let mut text = format!(
        "[Zerus team \u{b7} from: You \u{b7} planning]\nYou lead the team {}. Goal:\n{}\n\n",
        team.name,
        if team.goal.is_empty() { "(none given; ask the owner)" } else { team.goal.as_str() }
    );
    if !team.folder.is_empty() {
        text += &format!("Folder: {}\n", team.folder);
    }
    let mut rules_in_prompt = false;
    if let Some(prompt) = team.member(&team.lead).map(|m| m.prompt.trim()).filter(|p| !p.is_empty()) {
        let ordered = if let Some(snapshot) = team.member(&team.lead).and_then(|member| member.prompt_snapshot.clone()) {
            super::catalog::prompt_order_text_snapshot(&snapshot, prompt, None, rules)
        } else {
            super::catalog::load().ok().and_then(|catalog| {
                let role_name = team.role_of(&team.lead);
                let role = catalog.roles.iter().find(|role| role.id == role_name || role.name == role_name)?;
                super::catalog::prompt_order_text(role, prompt, None, rules)
            })
        };
        if let Some(ordered) = ordered.filter(|text| !text.trim().is_empty()) {
            rules_in_prompt = true;
            text += &ordered;
        } else {
            text += &format!("Your role: {prompt}\n");
        }
    }
    if !rules_in_prompt && !rules.is_empty() {
        text += &format!("\n{rules}\n");
    }
    if !team.lead_protocol.trim().is_empty() {
            text += &format!("\nTeam protocol:\n{}\n", team.lead_protocol.trim());
    }
    if let Ok(catalog) = super::catalog::load() {
        if !catalog.planning_rules.trim().is_empty() {
            text += &format!(
        "\nPlanning rules (v{}):\n{}\n",
                catalog.version,
                catalog.planning_rules.trim()
            );
        }
    }
    text += &format!(
        "\nPlan the work as tasks with acceptance criteria and dependencies (hard: waits for Accepted; soft: may start \
         once the dependency is In progress), then send the whole plan once with hgs team plan --json (hgs guide team). \
         Keep these members unless the goal needs others; nothing runs until the owner approves. \
         Once it runs, answer a member's question with hgs team reply --json, keep a returned task back with \
         hgs team hold --json and give it out again with hgs team release --json; new work goes in a new phase and \
         you finish the team (hgs guide team-lead). Weigh lanes with hgs team profile's roles[] (utilization and \
         tokens per accepted task) from the last finished phase. \
         Reports, questions, permission requests and your own tasks wait in one inbox \
         ({{\"team\":\"{}\"}} | hgs team inbox --json): decide a report with hgs team accept --json, and report \
         and accept your own task in one step with \"accept\":true in hgs team report --json.\n\
         {{\"request_id\":\"{}\",\"team\":\"{}\",\"members\":{},\"tasks\":{}}}",
        team.id,
        uuid::Uuid::new_v4(),
        team.id,
        serde_json::to_string(&members).unwrap_or_default(),
        serde_json::to_string(&tasks).unwrap_or_default()
    );
    text
}

/// The one message that tells the lead every task is decided.
pub(crate) fn all_decided_text(team: &Team) -> String {
    let count = |status: TaskStatus| team.tasks.iter().filter(|task| task.status == status).count();
    format!(
        "[Zerus team \u{b7} from: Zerus \u{b7} all tasks decided]\nTeam: {}. All {} tasks are decided ({} accepted, {} rejected, {} cancelled).\n\
         Add the next phase with hgs team extend --json, or finish the team with a summary (hgs guide team-lead):\n  \
         {{\"request_id\":\"{}\",\"team\":\"{}\",\"summary\":\"...\"}} | hgs team finish --json",
        team.name,
        team.tasks.len(),
        count(TaskStatus::Accepted),
        count(TaskStatus::Rejected),
        count(TaskStatus::Cancelled),
        uuid::Uuid::new_v4(),
        team.id
    )
}

/// A member's question as the lead reads it, with the answer to send back.
/// B1: the exact command a permission request names, and where it came from:
/// the tool input for Claude and Codex, the process for DeepSeek, unknown
/// when the agent did not show it.
pub(crate) fn permission_command(card: &Value) -> (Option<String>, &'static str) {
    let input = &card["tool_input"];
    if let Some(command) = input["command"].as_str().filter(|command| !command.trim().is_empty()) {
        return (Some(command.to_owned()), "tool_input");
    }
    // A patch names its files when there is no single command.
    if let Some(patch) = input["patch"].as_str().or_else(|| input["input"].as_str()) {
        let paths: Vec<&str> = patch
            .lines()
            .filter_map(|line| ["*** Update File: ", "*** Add File: ", "*** Delete File: "].iter().find_map(|prefix| line.strip_prefix(prefix)))
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .collect();
        if !paths.is_empty() {
            return (Some(paths.join(", ")), "tool_input");
        }
    }
    if let Some(command) = card["command"].as_str().filter(|command| !command.trim().is_empty()) {
        return (Some(command.to_owned()), "process");
    }
    (None, "unknown")
}

/// At most this much of a permission's command the lead reads.
const PERMISSION_COMMAND_CHARS: usize = 4000;

/// B1: the request status shows: its tool, the command and the source of that
/// command, and the question hash `hgs team answer` needs to answer it.
/// C4: `answer` carries the card's own question id and the allow/deny option
/// ids, so the desktop sends the native ids back.
pub(crate) fn permission_record(pending: &PendingQuestion) -> Value {
    let tool = pending.card["questions"][0]["header"].as_str().or_else(|| pending.card["tool"].as_str()).unwrap_or_default();
    let (command, source) = permission_command(&pending.card);
    let reason: String = pending.card["questions"][0]["body"].as_str().unwrap_or_default().chars().take(2000).collect();
    let answer = permission_answer(&pending.card);
    let mut record = json!({"question_id":pending.question_id,"question_hash":pending.question_hash,"tool":tool,"command":command,
        "command_source":source,"reason":reason,"asked_at":pending.at});
    // #740: a question (not a permission) carries its questions, options and
    // Other, so the owner answers it from Waiting for you as on the session's
    // card. Bounded: a record over 32 KiB keeps only its summary.
    if !pending.approval && pending.card["questions"].is_array() && pending.card["questions"].to_string().len() <= 32 * 1024 {
        record["questions"] = pending.card["questions"].clone();
    }
    if let Some(answer) = answer {
        record["answer"] = answer;
    }
    // U19 (#520): a subagent card says so, with its rows for the desktop.
    if pending.card["subagent_confirmation"] == true {
        record["subagent_confirmation"] = json!(true);
        record["subagent_calls"] = pending.card["subagent_calls"].clone();
        if pending.card["subagents"].is_array() {
            record["subagents"] = pending.card["subagents"].clone();
        }
    }
    record
}

/// C4: the native answer shape of a permission card: the card question's own
/// id and the option ids that allow and deny, picked by their labels the way
/// `hgs team answer` classifies a denial. Claude's approval card answers
/// `{"question_id":"approval","allow":"1","deny":"2"}`, DeepSeek's
/// `{"question_id":"decision","allow":"allow","deny":"deny"}`.
pub(crate) fn permission_answer(card: &Value) -> Option<Value> {
    let question = card["questions"].as_array()?.first()?;
    let question_id = question["id"].as_str().filter(|id| !id.is_empty())?;
    let options = question["options"].as_array()?;
    let mut answer = json!({"question_id":question_id});
    // Only a one-time option answers a request: a broader one would change a
    // persistent permission, which stays manual. An option without a scope
    // (DeepSeek's allow once / reject) is one-time.
    let by_label = |deny: bool| {
        options.iter().find_map(|option| {
            if option["scope"].as_str().is_some_and(|scope| scope != "once") {
                return None;
            }
            let label = option["label"].as_str()?;
            if super::commands::denies(label) != deny {
                return None;
            }
            option["id"].as_str().filter(|id| !id.is_empty()).map(str::to_owned)
        })
    };
    if let Some(allow) = by_label(false) {
        answer["allow"] = json!(allow);
    }
    if let Some(deny) = by_label(true) {
        answer["deny"] = json!(deny);
    }
    (answer.get("allow").is_some() || answer.get("deny").is_some()).then_some(answer)
}

/// N5: what Zerus records when a pending question vanished without an hgs team
/// answer (the desktop answered it, or it is gone): by unknown, newest 20 kept.
pub(crate) fn permission_lost(answers: &mut Vec<Value>, question_id: &str, at: f64) -> Value {
    answers.push(json!({"question_id":question_id,"decision":"unknown","by":"unknown","at":at,"reason":""}));
    let excess = answers.len().saturating_sub(20);
    answers.drain(..excess);
    json!({"question_id":question_id,"decision":"unknown","by":"unknown","at":at})
}

/// B1: the line that names the exact command a permission request asks about.
pub(crate) fn permission_command_line(card: &Value) -> String {
    match permission_command(card) {
        (Some(command), source) => {
            let shown: String = command.chars().take(PERMISSION_COMMAND_CHARS).collect();
            let cut = if command.chars().count() > PERMISSION_COMMAND_CHARS { "\n[cut: shown 4000 characters; read it with hgs inspect]" } else { "" };
            format!("Command (from the {}):\n{shown}{cut}\n", if source == "process" { "process" } else { "tool input" })
        }
        (None, _) => "command not shown by the agent - read it with hgs inspect\n".to_owned(),
    }
}

/// The message a per-item permission used to be (B1's tests read this): the
/// coalesced inbox message carries the same command line now.
#[cfg(test)]
pub(crate) fn question_text(team: &Team, member: &Member, card: &Value) -> String {
    let role = team.role_of(&member.id);
    let approval = card["approval"] == true;
    let mut text = if approval {
        format!("[Zerus team \u{b7} from: {role} \u{b7} permission]\n{role} asks you, the lead, for a permission instead of the owner. \
                 You decide: allow what its task needs inside the team's folder; deny what would change other folders, the \
                 system or permissions, or delete what it cannot restore, and ask the owner only for those. Access only to \
                 send its report is never needed: deny it; if it ends its turn without one, you can use its last reply as the report.\n")
    } else {
        format!("[Zerus team \u{b7} from: {role} \u{b7} question]\n{role} asks you, the lead, instead of the owner:\n")
    };
    if approval {
        text += &permission_command_line(card);
    }
    let mut answers = Vec::new();
    for item in card["questions"].as_array().into_iter().flatten() {
        let id = item["id"].as_str().unwrap_or_default();
        text += "\n";
        if let Some(header) = item["header"].as_str().filter(|h| !h.is_empty()) {
            text += &format!("{header}: ");
        }
        text += &format!("{}\n", item["question"].as_str().unwrap_or_default());
        if let Some(body) = item["body"].as_str().filter(|b| !b.is_empty()) {
            text += &format!("{body}\n");
        }
        for option in item["options"].as_array().into_iter().flatten() {
            let description = option["description"].as_str().unwrap_or_default();
            text += &format!("  {}) {}{}\n", option["id"].as_str().unwrap_or_default(), option["label"].as_str().unwrap_or_default(),
                if description.is_empty() { String::new() } else { format!(" \u{2014} {description}") });
        }
        if item["allow_other"] != false {
            text += "  or your own words as text\n";
        }
        answers.push(json!({"question_id":id,"selected_option_ids":["<option id>"],"text":""}));
    }
    text += &format!(
        "\nAnswer for them once with hgs team answer --json (hgs guide team):\n  {}\n\
         If the owner should decide, say so in your reply instead.",
        json!({"team":team.id,"member":member.id,"question_id":card["question_id"],
            "expected_question_hash":card["question_hash"],"answers":answers})
    );
    text
}

/// Runs this hgs with JSON on stdin; the child is no session's own.
pub(crate) fn hgs(host: &str, args: &[&str], input: Option<&Value>, timeout: Duration) -> anyhow::Result<Output> {
    #[cfg(test)]
    if let Some(result) = FAKE_HGS.with(|fake| fake.borrow_mut().as_mut().map(|fake| fake(host, args, input))) {
        return result;
    }
    let mut command = Command::new(std::env::current_exe()?);
    if !host.is_empty() {
        command.arg(format!("@{host}"));
    }
    command.args(args);
    // N10: Zerus' own children are no session's own - the worker, its runner
    // and the hgs calls it makes must carry no agent marker at all.
    for name in ["TMUX", "HGS_HOST_PANE", "HGS_SESSION", "HGS_RUN_ID", "DSH_SESSION_ID"] {
        command.env_remove(name);
    }
    match input {
        Some(input) => {
            let mut file = tempfile::tempfile()?;
            std::io::Write::write_all(&mut file, input.to_string().as_bytes())?;
            std::io::Seek::rewind(&mut file)?;
            command.stdin(Stdio::from(file));
        }
        None => {
            command.stdin(Stdio::null());
        }
    }
    // Not platform::output_until: on Windows its job ends every descendant
    // with the call, which would end a session host or DeepSeek host that a
    // launch starts, and it closes stdin. Output goes to files, so a chatty
    // child never blocks on a pipe.
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    command.stdout(Stdio::from(stdout.try_clone()?)).stderr(Stdio::from(stderr.try_clone()?));
    let mut child = command.spawn()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("timed out");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut output = Output { status, stdout: Vec::new(), stderr: Vec::new() };
    std::io::Seek::rewind(&mut stdout)?;
    std::io::Seek::rewind(&mut stderr)?;
    std::io::Read::read_to_end(&mut stdout, &mut output.stdout)?;
    std::io::Read::read_to_end(&mut stderr, &mut output.stderr)?;
    Ok(output)
}

/// The last reply of a member's session, for Use last reply as report and the
/// capture of a finished turn (A2).
pub(crate) fn last_reply(session: &SessionRef) -> anyhow::Result<String> {
    Ok(last_reply_of(session, "")?.unwrap_or_default())
}

/// One `hgs inspect` record of a member's session, at most 45 s. Tests replace
/// it with a record of their own, so the capture rules run without a CLI.
fn inspect_reply(session: &SessionRef) -> anyhow::Result<Value> {
    #[cfg(test)]
    if let Some(record) = INSPECTED.with(|record| record.borrow().clone()) {
        return Ok(record);
    }
    let output = hgs(&session.host, &["inspect", &session.name, "--skip-processes"], None, Duration::from_secs(45))?;
    anyhow::ensure!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr).trim());
    serde_json::from_slice(&output.stdout).context("invalid inspect output")
}

#[cfg(test)]
thread_local! {
    /// A test's `hgs inspect` record for this thread, instead of a real call.
    static INSPECTED: std::cell::RefCell<Option<Value>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn inspect_override(record: Option<Value>) {
    INSPECTED.with(|slot| *slot.borrow_mut() = record);
}

/// The last reply, but only when it is still the reply whose id is `expected`:
/// a newer turn started, so this is not that turn's report (A2). `None` means
/// the session moved on; an error means it has no reply or cannot be read.
pub(crate) fn last_reply_of(session: &SessionRef, expected: &str) -> anyhow::Result<Option<String>> {
    let record = inspect_reply(session)?;
    if !expected.is_empty() {
        let found = record["reply_id"].as_str().unwrap_or_default();
        if !found.is_empty() && found != expected {
            return Ok(None);
        }
    }
    let text = reply_text(&record);
    anyhow::ensure!(!text.is_empty(), "The member's session has no reply yet");
    Ok(Some(text))
}

/// N6: the session's last reply, but only when it was written after `since` -
/// the moment this attempt was delivered. Without a recorded turn identity the
/// time is the only proof the reply belongs to this attempt: an earlier
/// attempt's, or an earlier task's, last message is not this attempt's report.
/// None means there is no newer reply (or no time to prove one).
pub(crate) fn last_reply_after(session: &SessionRef, since: f64) -> anyhow::Result<Option<String>> {
    let record = inspect_reply(session)?;
    let at = record["reply_at"].as_f64().unwrap_or(0.0);
    let written_after_delivery = at.is_finite() && at > since;
    if !written_after_delivery {
        return Ok(None);
    }
    let text = reply_text(&record);
    Ok((!text.is_empty()).then_some(text))
}

/// The last reply's text in an `hgs inspect` record.
fn reply_text(record: &Value) -> String {
    let mut text = record["last_message"].as_str().unwrap_or_default().trim().to_owned();
    if text.is_empty() {
        // DeepSeek reports its replies as the main agent's messages.
        text = record["events"].as_array().into_iter().flatten().rev()
            .find(|e| e["type"] == "AgentMessage" && e["agent_id"].as_str().unwrap_or_default().is_empty()
                && !e["detail"].as_str().unwrap_or_default().trim().is_empty())
            .and_then(|e| e["detail"].as_str()).unwrap_or_default().trim().to_owned();
    }
    text
}

/// The result of one `hgs send`.
#[derive(Debug, PartialEq)]
pub(crate) enum Sent {
    Delivered,
    /// Refused before anything reached the agent.
    Refused(String),
    /// It may have arrived.
    Uncertain(String),
}

pub(crate) fn classify(output: &anyhow::Result<Output>) -> Sent {
    let output = match output {
        Ok(output) => output,
        Err(error) => return Sent::Uncertain(error.to_string()),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if output.status.success() {
        let receipt: Value = serde_json::from_str(stdout.trim()).unwrap_or_default();
        return if receipt["status"] == "submitted" { Sent::Delivered } else { Sent::Uncertain(format!("unexpected receipt: {}", stdout.trim())) };
    }
    // hgs reports every possibly delivered input as uncertain; ssh exits 255
    // when the connection, not hgs, failed.
    if stderr.to_lowercase().contains("uncertain") || output.status.code() != Some(1) {
        Sent::Uncertain(stderr)
    } else {
        Sent::Refused(stderr)
    }
}

/// Task 98: hgs's refusal of input while the composer holds a draft, a
/// dialog or an unrecognized prompt.
pub(crate) const DRAFT_REFUSAL: &str = "terminal has a draft, dialog, or unrecognized prompt";

/// Task 98: the last non-empty lines of a Terminal screen, its colors and
/// cursor codes removed, each at most 200 characters.
pub(crate) fn last_lines(screen: &str, count: usize) -> Vec<String> {
    let mut plain = String::with_capacity(screen.len());
    let mut chars = screen.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    for next in chars.by_ref() {
                        if next == '\u{7}' || next == '\\' {
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            }
            continue;
        }
        if ch == '\n' || !ch.is_control() {
            plain.push(ch);
        }
    }
    let lines: Vec<String> = plain
        .lines()
        .map(|line| line.trim_end())
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.chars().take(200).collect())
        .collect();
    lines[lines.len().saturating_sub(count)..].to_vec()
}

/// Task 98: a hand-out the member's composer refused. Zerus's own leftover
/// paste (the draft `hgs send` recorded right after it pasted) is finished
/// once with its Enter; anything else is counted and, at
/// `InputBlock::RAISE_AFTER` refusals, raised once to the lead and the owner
/// with the screen's last lines. A delivery ends the episode.
fn input_blocked(store: &mut Store, team_id: &str, member_id: &str, session: &SessionRef, outcome: &Sent) -> anyhow::Result<()> {
    let team = store.load(team_id)?.with_context(|| format!("Unknown team {team_id}"))?;
    let member = team.member(member_id).context("unknown member")?;
    let refused = match outcome {
        Sent::Refused(error) if error.contains(DRAFT_REFUSAL) => error.clone(),
        Sent::Delivered if member.input_block.is_some() => {
            return change(store, team_id, |team, events| {
                let role = team.role_of(member_id);
                if let Some(found) = team.members.iter_mut().find(|found| found.id == member_id) {
                    if found.input_block.take().is_some_and(|block| block.raised) {
                        events.push(Event::new("input_unblocked", json!({"member":role})));
                    }
                }
                Ok(())
            });
        }
        _ => return Ok(()),
    };
    let request = json!({"request_id":uuid::Uuid::new_v4().to_string(),"action":"submit_own_paste",
        "expected_run_id":session.run_id,"expected_conversation_id":session.conversation_id});
    let finished = hgs(&session.host, &["terminal", &session.name, "--json"], Some(&request), Duration::from_secs(60));
    let receipt: Value = finished
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| serde_json::from_slice(&output.stdout).ok())
        .unwrap_or_default();
    let own = receipt["status"].as_str().filter(|status| matches!(*status, "submitted" | "uncertain")).map(str::to_owned);
    let count = member.input_block.as_ref().map_or(0, |block| block.count) + u32::from(own.is_none());
    let raise = own.is_none() && count >= InputBlock::RAISE_AFTER && !member.input_block.as_ref().is_some_and(|block| block.raised);
    let screen = if raise {
        let snapshot = json!({"request_id":uuid::Uuid::new_v4().to_string(),"action":"snapshot",
            "expected_run_id":session.run_id,"expected_conversation_id":session.conversation_id});
        hgs(&session.host, &["terminal", &session.name, "--json"], Some(&snapshot), Duration::from_secs(30))
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok())
            .map(|answer| last_lines(answer["screen"].as_str().unwrap_or_default(), 12))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let reason = refused.lines().next().unwrap_or_default().split(" [trace:").next().unwrap_or_default().trim().to_owned();
    change(store, team_id, |team, events| {
        let role = team.role_of(member_id);
        let found = team.members.iter_mut().find(|found| found.id == member_id).context("unknown member")?;
        let at = now();
        match own.as_deref() {
            Some("submitted") => {
                found.input_block = None;
                events.push(Event::new("own_paste_finished", json!({"member":role,"delivery":receipt["delivery"]})));
            }
            Some(_) => {
                // The Enter may have gone: never sent again; counted as before.
                let block = found.input_block.get_or_insert_with(|| InputBlock { since: at, ..Default::default() });
                block.own_paste = receipt["delivery"].as_str().unwrap_or("unknown").to_owned();
                block.at = at;
                events.push(Event::new("own_paste_uncertain", json!({"member":role,"delivery":receipt["delivery"],"error":receipt["error"]})));
            }
            None => {
                let block = found.input_block.get_or_insert_with(|| InputBlock { since: at, ..Default::default() });
                block.count = count;
                block.at = at;
                block.reason = reason.clone();
                if raise {
                    block.raised = true;
                    block.screen = screen.clone();
                    events.push(Event::new("input_blocked", json!({"member":role,"count":count,"reason":reason})));
                }
            }
        }
        Ok(())
    })
}

/// Task 113: raises the member's Terminal-only request to the owner once,
/// with the screen's last lines; a snapshot that fails raises it without them.
fn escalate_terminal(store: &mut Store, team: &Team, member_id: &str) -> anyhow::Result<()> {
    let member = team.member(member_id).context("unknown member")?;
    let Some((asked, None)) = super::terminal_prompt::of(member) else { return Ok(()) };
    let question = asked.question_id.clone();
    let screen = member
        .session
        .as_ref()
        .and_then(|session| {
            let snapshot = json!({"request_id":uuid::Uuid::new_v4().to_string(),"action":"snapshot",
                "expected_run_id":session.run_id,"expected_conversation_id":session.conversation_id});
            hgs(&session.host, &["terminal", &session.name, "--json"], Some(&snapshot), Duration::from_secs(30)).ok()
        })
        .filter(|output| output.status.success())
        .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok())
        .map(|answer| last_lines(answer["screen"].as_str().unwrap_or_default(), TerminalEscalation::SCREEN_LINES))
        .unwrap_or_default();
    change(store, &team.id, |team, events| {
        let role = team.role_of(member_id);
        let found = team.members.iter_mut().find(|found| found.id == member_id).context("unknown member")?;
        // The request may have gone while the screen was read.
        let Some(asked) = found.terminal_question.as_ref().filter(|asked| asked.question_id == question) else {
            return Ok(());
        };
        let at = now();
        events.push(Event::new("terminal_prompt_raised", json!({"member":role,"question_id":question,
            "tool":super::terminal_prompt::title(asked),"minutes":super::terminal_prompt::minutes(asked, at),
            "screen":!screen.is_empty()})));
        found.terminal_escalation = Some(TerminalEscalation { question_id: question.clone(), at, screen: screen.clone() });
        Ok(())
    })
}

/// Task 278: a member's jobs changed. The team record keeps the current ones
/// (status, look and the brief read them); the working tier keeps each job's
/// start, command and end, journaled as `job_started` and `job_ended` so the
/// lead's brief sees what each member waits on.
pub(crate) fn record_jobs(store: &mut Store, team: &Team, member_id: &str, jobs: &[SeenJob], at: f64) -> anyhow::Result<()> {
    let before = team.member(member_id).map(|member| member.jobs.clone()).unwrap_or_default();
    let key = |kind: &str, command: &str, since: f64| format!("{kind}:{since:.3}:{}", command.chars().take(160).collect::<String>());
    let started: Vec<&SeenJob> =
        jobs.iter().filter(|job| !before.iter().any(|old| key(&old.kind, &old.command, old.since) == key(&job.kind, &job.command, job.since))).collect();
    let ended: Vec<&MemberJob> =
        before.iter().filter(|old| !jobs.iter().any(|job| key(&old.kind, &old.command, old.since) == key(&job.kind, &job.command, job.since))).collect();
    if !started.is_empty() || !ended.is_empty() {
        let role = team.role_of(member_id);
        let team_id = team.id.clone();
        store.write_rows(&team.id, |tx, events| {
            for job in &started {
                let since = if job.since > 0.0 { job.since } else { at };
                super::working::job_started(tx, &team_id, member_id, &key(&job.kind, &job.command, job.since), &job.kind, &job.what, &job.command, since)?;
                events.push(Event::new("job_started", json!({"member":role,"kind":job.kind,"what":job.what,"since":since})));
            }
            for job in &ended {
                super::working::job_ended(tx, &team_id, member_id, &key(&job.kind, &job.command, job.since), at)?;
                let minutes = if job.since > 0.0 { ((at - job.since).max(0.0) / 60.0).round() as u64 } else { 0 };
                events.push(Event::new("job_ended", json!({"member":role,"kind":job.kind,"what":job.what,"since":job.since,"minutes":minutes})));
            }
            super::working::prune_jobs(tx, &team_id)
        })?;
    }
    change(store, &team.id, |team, _| {
        if let Some(found) = team.members.iter_mut().find(|found| found.id == member_id) {
            found.jobs = jobs
                .iter()
                .map(|job| MemberJob { kind: job.kind.clone(), what: job.what.clone(), command: job.command.clone(), since: job.since })
                .collect();
        }
        Ok(())
    })
}

/// Task 139: the members whose screens are being read, `team\nmember`.
static CHECKING: std::sync::Mutex<BTreeSet<String>> = std::sync::Mutex::new(BTreeSet::new());

/// Task 139: reads the member's screen and compares its sources. Off the tick:
/// the read runs on its own thread (inline in tests, where hgs is faked per
/// thread), at most `look::MAX_CHECKS` at a time, never twice for one member.
fn check_member(store: &mut Store, team: &Team, member_id: &str, hooks: &super::look::Hooks) {
    let Some(session) = team.member(member_id).and_then(|member| member.session.clone()) else { return };
    let key = format!("{}\n{member_id}", team.id);
    {
        let mut checking = CHECKING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if checking.len() >= super::look::MAX_CHECKS || !checking.insert(key.clone()) {
            return;
        }
    }
    let (team_id, member_id, hooks) = (team.id.clone(), member_id.to_owned(), hooks.clone());
    let dsh = team.member(&member_id).is_some_and(|member| member.agent == "dsh");
    let tool_bound = tool_stuck_minutes(team) * 60.0;
    let done = key.clone();
    let run = move |store: &mut Store| {
        let mut hooks = hooks;
        // Task 181: a DeepSeek listing shows no tool call; the session's events do.
        if dsh && hooks.tool.is_empty() {
            fold_open_tool(&session, &mut hooks, tool_bound);
        }
        let screen = read_screen(&session);
        let _ = apply_check(store, &team_id, &member_id, &hooks, screen.as_deref(), now());
        CHECKING.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&done);
    };
    if cfg!(test) {
        run(store);
    } else {
        let _ = std::thread::Builder::new().name("team-check".into()).spawn(move || {
            if let Ok(mut store) = Store::open(true) {
                run(&mut store);
            } else {
                CHECKING.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&key);
            }
        });
    }
}

/// Task 181: a DeepSeek member inside one long tool call (a poll around its own
/// `hgs run`, a cargo build) is working: the call's start is progress and the
/// tool bound holds it, as for Claude. The session's events are read once.
pub(crate) fn fold_open_tool(session: &SessionRef, hooks: &mut super::look::Hooks, bound: f64) {
    let Ok(record) = inspect_reply(session) else { return };
    if record["run_id"].as_str().unwrap_or_default() != session.run_id {
        return;
    }
    super::look::note_open_tool(hooks, super::look::open_tool(&record), bound, now());
}

/// Task 181: asks the session to settle a finished turn its hooks still call
/// working (`hgs subagent settle`): true when its record now says idle.
fn settle_session(session: &SessionRef, quiet: f64) -> bool {
    let input = json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_run_id":session.run_id,
        "expected_conversation_id":session.conversation_id,"quiet_seconds":quiet});
    hgs(&session.host, &["subagent", "settle", &session.name, "--json"], Some(&input), Duration::from_secs(30))
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok())
        .is_some_and(|answer| answer["settled"] == true)
}

/// The member's Terminal as `hgs terminal NAME --json` snapshots it.
pub(crate) fn read_screen(session: &SessionRef) -> Option<String> {
    let snapshot = json!({"request_id":uuid::Uuid::new_v4().to_string(),"action":"snapshot",
        "expected_run_id":session.run_id,"expected_conversation_id":session.conversation_id});
    hgs(&session.host, &["terminal", &session.name, "--json"], Some(&snapshot), Duration::from_secs(20))
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok())
        .and_then(|answer| answer["screen"].as_str().map(str::to_owned))
}

/// Compares the member's sources against `screen` and keeps the result; an
/// uncertain member is told to the lead once per episode.
pub(crate) fn apply_check(
    store: &mut Store,
    team_id: &str,
    member_id: &str,
    hooks: &super::look::Hooks,
    screen: Option<&str>,
    at: f64,
) -> anyhow::Result<()> {
    use super::look;
    let team = store.load(team_id)?.context("unknown team")?;
    let member = team.member(member_id).context("unknown member")?;
    let lines = screen.map(|screen| last_lines(screen, 40)).unwrap_or_default();
    let shown = screen.map(|_| look::classify(&member.agent, &lines));
    let record = look::recorded_request(member);
    let compared = look::compare(hooks, record.as_deref(), shown.as_ref().map(|(kind, detail)| (*kind, detail.as_str())));
    // Task 181: a finished turn (an idle prompt, no agent row or shell) the hooks
    // still call working is idle for hand-out purposes: its record is corrected
    // and nobody is told, instead of an `uncertain` notice and a stuck member.
    let finished = look::settle_due(&member.agent, hooks, record.as_deref(), shown.as_ref().map(|(kind, detail)| (*kind, detail.as_str())), member.check.as_ref(), at);
    if finished {
        if let Some(session) = member.session.clone() {
            if settle_session(&session, hooks.quiet) {
                change(store, team_id, |team, events| {
                    let role = team.role_of(member_id);
                    if let Some(found) = team.members.iter_mut().find(|found| found.id == member_id) {
                        found.check = None;
                        found.seen_activity = "idle".into();
                        found.seen_phase = "idle".into();
                        found.stuck = None;
                    }
                    events.push(Event::new("member_settled", json!({"member":role,"reason":"screen_idle_prompt","quiet_minutes":(hooks.quiet / 60.0).round() as u64})));
                    Ok(())
                })?;
                return Ok(());
            }
        }
    }
    let kept = lines[lines.len().saturating_sub(look::SCREEN_LINES)..].to_vec();
    let check = look::next_check(member.check.as_ref(), &compared, kept, at);
    let tell = check.state == "uncertain" && !check.told;
    change(store, team_id, |team, events| {
        let role = team.role_of(member_id);
        let found = team.members.iter_mut().find(|found| found.id == member_id).context("unknown member")?;
        let before = found.check.as_ref().map(|check| check.state.clone()).unwrap_or_default();
        if check.state == "uncertain" && before != "uncertain" {
            events.push(Event::new("member_uncertain", json!({"member":role,"disagree":check.disagree})));
        } else if check.state == "agree" && before == "uncertain" {
            events.push(Event::new("member_check_settled", json!({"member":role})));
        }
        found.check = Some(check.clone());
        Ok(())
    })?;
    if tell {
        let team = store.load(team_id)?.context("unknown team")?;
        let member = team.member(member_id).context("unknown member")?.clone();
        let Some(lead) = team.member(&team.lead).and_then(|lead| lead.session.clone()) else { return Ok(()) };
        if matches!(send(&lead, &uuid::Uuid::new_v4().to_string(), &look::lead_text(&team, &member, &check, at)), Sent::Refused(_)) {
            return Ok(());
        }
        change(store, team_id, |team, events| {
            let role = team.role_of(member_id);
            if let Some(found) = team.members.iter_mut().find(|found| found.id == member_id) {
                if let Some(kept) = found.check.as_mut().filter(|kept| kept.since == check.since) {
                    kept.told = true;
                    events.push(Event::new("member_uncertain_told", json!({"member":role})));
                }
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// Task 139: tells the idle lead, once a round, what decisions wait for it.
fn tell_lead_idle(store: &mut Store, team: &Team) -> anyhow::Result<()> {
    let Some(stall) = team.lead_stall.clone().filter(|stall| stall.reason == "idle" && !stall.lead_told) else { return Ok(()) };
    let lead = team.member(&team.lead).and_then(|lead| lead.session.clone()).context("the lead has no session")?;
    if matches!(send(&lead, &uuid::Uuid::new_v4().to_string(), &super::look::lead_idle_text(team, &stall, now())), Sent::Refused(_)) {
        return Ok(());
    }
    change(store, &team.id, |team, events| {
        if let Some(found) = team.lead_stall.as_mut().filter(|found| found.round == stall.round) {
            found.lead_told = true;
            events.push(Event::new("lead_idle_told", json!({"round":stall.round,"items":stall.items})));
        }
        Ok(())
    })
}

/// An escalation outlives no request: it goes with the request it raised.
fn drop_stale_escalation(found: &mut Member, role: &str, events: &mut Vec<Event>) {
    let current = found.terminal_question.as_ref().map(|asked| asked.question_id.clone());
    if let Some(raised) = found.terminal_escalation.take_if(|raised| Some(&raised.question_id) != current.as_ref()) {
        events.push(Event::new("terminal_prompt_cleared", json!({"member":role,"question_id":raised.question_id})));
    }
}

/// Task 97: an `hgs answer` outcome, as conservatively as a send: success is
/// an answer that reached the session, exit 1 without "uncertain" one refused
/// before anything was typed, anything else may have arrived.
pub(crate) fn classify_answer(output: &anyhow::Result<Output>) -> Sent {
    let output = match output {
        Ok(output) => output,
        Err(error) => return Sent::Uncertain(error.to_string()),
    };
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if output.status.success() {
        Sent::Delivered
    } else if stderr.to_lowercase().contains("uncertain") || output.status.code() != Some(1) {
        Sent::Uncertain(stderr)
    } else {
        Sent::Refused(stderr)
    }
}

/// Task 160: gives a parked task back to its member in the same attempt, with
/// the notes it parked with. A refused message leaves it to the next tick; an
/// uncertain one is unconfirmed, as a hand-out is.
fn resume_parked(store: &mut Store, team: &Team, task_id: u32, member_id: &str) -> anyhow::Result<()> {
    let task = team.task(task_id).context("unknown task")?;
    let park = task.park.clone().filter(|park| park.state == super::park::MET).context("the task is no longer parked")?;
    let member = team.member(member_id).context("unknown member")?;
    let session = member.session.clone().context("the member has no session")?;
    let text = super::park::resume_text(task_id, &task.title, &park);
    let sent = send(&session, &uuid::Uuid::new_v4().to_string(), &text);
    if let Sent::Refused(reason) = &sent {
        anyhow::bail!("the resume of #{task_id} was refused: {reason}");
    }
    let unconfirmed = matches!(sent, Sent::Uncertain(_));
    change(store, &team.id, |team, events| {
        let task = team.task_mut(task_id).context("unknown task")?;
        if task.park.as_ref().is_none_or(|found| found.attempt != park.attempt) {
            return Ok(());
        }
        task.park = None;
        task.status = TaskStatus::InProgress;
        if let Some(attempt) = task.attempts.iter_mut().find(|attempt| attempt.id == park.attempt) {
            attempt.state = if unconfirmed { AttemptState::Unconfirmed } else { AttemptState::Delivered };
            attempt.sent_at = now();
        }
        events.push(Event::new("resumed", json!({"task":task_id,"member":park.member,"condition":super::park::label(&park.condition),
            "detail":park.detail,"unconfirmed":unconfirmed})));
        Ok(())
    })?;
    // Task 161: the lead gets every park's outcome; a failed or overdue one
    // is in its inbox, a met one is told here.
    tell_lead(store, team, "park", &format!(
        "[Zerus team] Task #{task_id} ({}) is back with {}: its park ended, {} ({}).",
        task.title,
        team.name_of(&park.member),
        super::park::label(&park.condition),
        if park.detail.is_empty() { "condition met" } else { park.detail.as_str() }
    ));
    Ok(())
}

/// Task 261: the lead's line when a run restarted the shared sccache server.
fn cache_restarted_text(team: &Team, member: &str, task: u32, what: &str, exit: Option<i32>) -> String {
    format!(
        "[Zerus team] Build cache: {}'s {what}{} failed writing its outputs through the shared sccache server (os error 5: the server was running in a restricted token). \
         Zerus restarted the server from its worker and ran it again: {}.",
        team.role_of(member),
        if task > 0 { format!(" for #{task}") } else { String::new() },
        match exit {
            Some(0) => "it passed".to_owned(),
            Some(code) => format!("exit {code}"),
            None => "no exit code".to_owned(),
        }
    )
}

/// Task 261: from a run that is not the dispatcher's (`hgs run` of a member):
/// the lead of `team_id` hears that the shared sccache server was restarted.
pub(crate) fn tell_lead_cache_restarted(team_id: &str, task: u64, label: &str, exit: Option<i32>) {
    let Ok(mut store) = Store::open(true) else { return };
    let Ok(Some(team)) = store.load(team_id) else { return };
    let member = team
        .tasks
        .iter()
        .find(|found| u64::from(found.id) == task)
        .and_then(|found| found.attempts.last())
        .map(|attempt| attempt.member.clone())
        .unwrap_or_default();
    let text = cache_restarted_text(&team, &member, u32::try_from(task).unwrap_or(0), label, exit);
    tell_lead(&mut store, &team, "build_cache", &text);
}

/// Task 161: one line for the lead, stored and rung; a refusal is left (the
/// event journal has it).
fn tell_lead(store: &mut Store, team: &Team, kind: &str, text: &str) {
    let Some(lead) = team.member(&team.lead).and_then(|lead| lead.session.clone()) else { return };
    let _ = super::doorbell::post_and_ring(store, &team.id, &team.lead, &lead, &uuid::Uuid::new_v4().to_string(), kind, "zerus", super::working::Urgency::Normal, text);
}

/// Task 270 (owner, 2026-11-08): profiles the project once and caches the
/// working-mode proposal on the team, so later ticks only compare settings.
fn check_working_mode(store: &mut Store, team: &Team) -> anyhow::Result<()> {
    let folder = std::path::PathBuf::from(team.folder.trim());
    // Task #32 (team ede724c4): Git too slow to read the project is cached as
    // such and read again in two minutes, never as "not a repository".
    let proposal = match super::working_mode::propose(&folder) {
        Ok(proposal) => {
            let mut value = proposal.value();
            // Task #451: when the profile was read, so a stale block is refreshed.
            value["checked_at"] = json!(crate::state::now());
            value
        }
        Err(error @ super::working_mode::ProfileError::Slow { .. }) => super::working_mode::slow_cache(&error, crate::state::now()),
        Err(_) => json!({"preset":"","checked_at":crate::state::now()}),
    };
    change(store, &team.id, |team, _events| {
        team.mode_proposal = Some(proposal);
        Ok(())
    })
}

/// Task 270 (owner, 2026-11-08): the lead proposes the project's working mode
/// once, when the running team's settings disagree with it at a safe point; the
/// owner's Allow writes the preset's settings (hgs team propose / decide).
fn propose_working_mode(store: &mut Store, team: &Team, preset: &str, title: &str, reasons: &[String]) -> anyhow::Result<()> {
    // The cached proposal still names this preset, and the settings still
    // disagree with it: a stale step creates no card.
    if !super::working_mode::cached_disagreement(team).is_some_and(|(found, _)| found == preset) {
        return Ok(());
    }
    // Task #451: the card follows the project as it is now. When the profile
    // moved since the step was computed, nothing is proposed here: the tick
    // re-reads the cache (ModeCheck) and offers the new mode on a later tick.
    // Task #32: a project Git was too slow to read proposes nothing now either.
    match super::working_mode::propose(&std::path::PathBuf::from(team.folder.trim())) {
        Ok(proposal) if proposal.preset != preset => return Ok(()),
        Err(super::working_mode::ProfileError::Slow { .. }) => return Ok(()),
        _ => {}
    }
    let reason = format!(
        "The project's profile selects {title}: {}. Switch the team to it at this safe point (no attempt is open).",
        reasons.join("; ")
    );
    let input = json!({"kind":"working-mode","preset":preset,"reason":reason});
    change(store, &team.id, |team, events| {
        let lead = ops::Actor::Member(team.lead.clone());
        ops::apply(team, "propose", &input, &lead, &BTreeSet::new(), events)?;
        Ok(())
    })
}

/// Task 271: the phase the team works on now: the first with a task not
/// finished, else the last phase recorded.
fn current_phase(team: &Team) -> u32 {
    team.tasks
        .iter()
        .filter(|task| !task.status.finished())
        .map(|task| task.phase)
        .min()
        .or_else(|| team.phases.iter().map(|phase| phase.n).max())
        .unwrap_or(0)
}

/// Task 271 (owner, 2026-11-08): the lead's brief: the playbook (the preset and
/// the rules a-(i)), the open breaches with their fix, the owner's standing
/// decisions from the lore, the integration line's last landing and who idles.
/// It is sent at team start, at a phase start and after a compaction or a clear;
/// the fingerprint records the session and phase it answered.
fn send_lead_brief(store: &mut Store, team: &Team, reason: &str) -> anyhow::Result<()> {
    let Some(lead) = team.member(&team.lead).and_then(|member| member.session.clone()) else {
        anyhow::bail!("the lead has no session to brief");
    };
    let phase = current_phase(team);
    let used = team.member(&team.lead).and_then(|member| member.context.as_ref()).map_or(0, |context| context.used);
    let text = format!("[Zerus team · from: Zerus · brief · {}]\n{}", reason, super::playbook::brief(team));
    let id = uuid::Uuid::new_v4().to_string();
    let outcome = send(&lead, &id, &text);
    let sent = match &outcome {
        Sent::Delivered => "delivered",
        Sent::Refused(_) => "refused",
        Sent::Uncertain(_) => "uncertain",
    };
    change(store, &team.id, |team, events| {
        team.lead_briefed = Some(json!({
            "reason": reason,
            "run": lead.run_id,
            "conversation": lead.conversation_id,
            "phase": phase,
            "used": used,
            "at": now(),
        }));
        team.playbook = Some(super::playbook::playbook(team));
        events.push(Event::new("lead_briefed", json!({"reason":reason,"phase":phase,"sent":sent})));
        Ok(())
    })
}

/// Task 161: a member's attempt that waits instead of working: a running job
/// of its session whose purpose is waiting, or the same read-only check its
/// turn repeated at intervals. Found, the task is parked on the condition
/// Zerus guesses, the member is told to stop waiting, and the lead is told
/// once with the guessed condition to correct.
fn detect_wait(store: &mut Store, team: &Team, task_id: u32) -> anyhow::Result<()> {
    let task = team.task(task_id).context("unknown task")?;
    let attempt = task.open_attempt().context("no open attempt")?.clone();
    let member = team.member(&attempt.member).context("unknown member")?.clone();
    let session = member.session.clone().context("the member has no session")?;
    let at = now();
    let mut found: Option<(String, String)> = None;
    let mut background = !member.jobs.is_empty();
    if member.computer.is_empty() {
        for claim in super::super::builds::running_for(&session.name) {
            if claim["session"]["run_id"] != session.run_id.as_str() {
                continue;
            }
            background = true;
            let label = claim["label"].as_str().unwrap_or_default();
            let command = claim["command"].as_str().unwrap_or_default();
            let cwd = std::path::PathBuf::from(claim["cwd"].as_str().unwrap_or("."));
            let script = super::wait_loops::script_text(command, &cwd);
            if let Some(why) = super::wait_loops::waiting_job(label, &format!("{command}\n{script}")) {
                found = Some((format!("its run \"{label}\" ({why})"), format!("{label}\n{command}\n{script}")));
                break;
            }
        }
    }
    // Task #413 (owner, 2026-11-09): a member with a background monitor or job
    // of its own is waiting on that work, so a repeated command beside it is
    // not a loop Zerus parks.
    if found.is_none() && !background {
        let events: Vec<Value> = hgs(&session.host, &["inspect", &session.name, "--after", "0"], None, Duration::from_secs(15))
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok())
            .and_then(|value| value.get("events").and_then(Value::as_array).cloned())
            .unwrap_or_default();
        if let Some((command, times)) = super::wait_loops::repeated_check(&events, attempt.delivered_at.unwrap_or(attempt.sent_at)) {
            // Task #413: a repeated check is a loop only while the state it
            // reads stands. The member's folder HEAD is that state: a
            // fast-forward moves it on every run, so the run is work, not a
            // wait; a first observation is never enough to park on.
            let folder = if member.computer.is_empty() {
                member_folder_path(team, &member).ok().filter(|folder| !folder.is_empty())
            } else {
                None
            };
            let head = folder.as_deref().and_then(|folder| git_at(folder, &["rev-parse", "HEAD"])).unwrap_or_default();
            if !head.is_empty() && !super::wait_loops::unchanged_state(&head, &attempt.wait_head) {
                return change(store, &team.id, |team, _| {
                    if let Some(open) = team.task_mut(task_id).and_then(|task| task.attempts.iter_mut().find(|found| found.id == attempt.id)) {
                        open.wait_checked_at = at;
                        open.wait_head = head.clone();
                    }
                    Ok(())
                });
            }
            let state = if head.is_empty() { String::new() } else { format!(", with HEAD {} unchanged", head.chars().take(9).collect::<String>()) };
            found = Some((format!("it ran `{command}` {times} times at intervals{state}"), command));
        }
    }
    let Some((why, text)) = found else {
        return change(store, &team.id, |team, _| {
            if let Some(open) = team.task_mut(task_id).and_then(|task| task.attempts.iter_mut().find(|found| found.id == attempt.id)) {
                open.wait_checked_at = at;
            }
            Ok(())
        });
    };
    let (condition, what) = super::wait_loops::guess_condition(&text, at);
    let mut parked = false;
    change(store, &team.id, |team, events| {
        if let Some(open) = team.task_mut(task_id).and_then(|task| task.attempts.iter_mut().find(|found| found.id == attempt.id)) {
            open.wait_checked_at = at;
        }
        let request = json!({"task":task_id,"attempt":attempt.id,"condition":condition,"eta_minutes":super::wait_loops::GUESSED_ETA_MINUTES,
            "note":format!("Parked by Zerus: the attempt was waiting ({why}).")});
        let lead = ops::Actor::Member(team.lead.clone());
        let mut inner = Vec::new();
        if ops::apply(team, "park", &request, &lead, &BTreeSet::new(), &mut inner).is_ok() {
            if let Some(park) = team.task_mut(task_id).and_then(|task| task.park.as_mut()) {
                park.by = "Zerus (a wait loop)".into();
            }
            events.extend(inner);
            events.push(Event::new("wait_parked", json!({"task":task_id,"member":member.id,"attempt":attempt.n,"why":why,"condition":what})));
            parked = true;
        }
        Ok(())
    })?;
    if !parked {
        return Ok(());
    }
    let _ = send(&session, &uuid::Uuid::new_v4().to_string(), &format!(
        "[Zerus team] Your task #{task_id} was waiting ({why}), so Zerus parked it until {what}. Stop the wait now (end that job or loop) and end your turn: \
Zerus watches the condition and gives the task back to you; take your next task meanwhile. A wait is always a park: hgs guide team-park."
    ));
    tell_lead(store, team, "park", &format!(
        "[Zerus team] Task #{task_id} ({}) of {} was a wait loop ({why}); Zerus parked it until {what} (a guess, ETA {} min). \
Set the real condition with hgs team unpark and hgs team park, or leave it.",
        task.title,
        team.name_of(&member.id),
        super::wait_loops::GUESSED_ETA_MINUTES
    ));
    Ok(())
}

/// Task 156: one message to the member of a task in flight that a new
/// decision binds; told once per attempt.
fn tell_decision(store: &mut Store, team: &Team, task_id: u32, decision_id: &str) -> anyhow::Result<()> {
    let task = team.task(task_id).context("unknown task")?;
    let attempt = task.open_attempt().context("no open attempt")?.clone();
    let decision = team.decisions.iter().find(|found| found.id == decision_id).context("unknown decision")?;
    let member = team.member(&attempt.member).context("unknown member")?;
    let session = member.session.clone().context("the member has no session")?;
    let text = format!(
        "[Zerus team] A decision of the owner binds your task #{task_id} ({}):\n{}\nFollow it in this attempt (attempt {}); hgs team ask if it conflicts with your work.",
        task.title,
        super::decisions::line(decision),
        attempt.id
    );
    if let Sent::Refused(reason) = send(&session, &uuid::Uuid::new_v4().to_string(), &text) {
        anyhow::bail!("the decision {decision_id} did not reach {}: {reason}", member.id);
    }
    change(store, &team.id, |team, events| {
        if let Some(found) = team.task_mut(task_id).and_then(|task| task.attempts.iter_mut().find(|found| found.id == attempt.id)) {
            if !found.decisions_told.iter().any(|id| id == decision_id) {
                found.decisions_told.push(decision_id.to_owned());
                events.push(Event::new("decision_told", json!({"task":task_id,"attempt":attempt.n,"decision":decision_id})));
            }
        }
        Ok(())
    })
}

/// Task 156: what a report adds: its text and the lines its commits add
/// against the lead branch (bounded), to check against the decisions.
fn added_text(team: &Team, task: &Task, attempt: &Attempt) -> String {
    let mut text = attempt.report.as_ref().map(|report| report.text.clone()).unwrap_or_default();
    let Some(member) = team.member(&attempt.member) else { return text };
    let Ok(folder) = member_folder_path(team, member) else { return text };
    let Ok(branch) = lead_branch(team) else { return text };
    let head = if attempt.folder_head.is_empty() { "HEAD".to_owned() } else { attempt.folder_head.clone() };
    if folder.is_empty() || !member.computer.is_empty() {
        return text;
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let range = format!("{branch}...{head}");
    if let Ok(out) = super::super::worktrees::git(Path::new(&folder), &["diff", "--no-color", "--no-ext-diff", "-U0", &range], deadline) {
        let diff = String::from_utf8_lossy(&out);
        for line in diff.lines().filter(|line| line.starts_with('+') && !line.starts_with("+++")).take(20_000) {
            text.push('\n');
            text.push_str(&line[1..]);
            if text.len() > 2 * 1024 * 1024 {
                break;
            }
        }
    }
    let _ = task;
    text
}

/// Task 156: a report's changes against the decisions that bind its task;
/// a contradiction is flagged before accept (the inbox item, and accept asks
/// for "against_decisions").
fn scan_report(store: &mut Store, team: &Team, task_id: u32) -> anyhow::Result<()> {
    let task = team.task(task_id).context("unknown task")?;
    let attempt = task.attempts.iter().rev().find(|attempt| attempt.state == AttemptState::Reported).context("no report")?.clone();
    let added = added_text(team, task, &attempt);
    let flags = super::decisions::conflicts(team, task, &added);
    change(store, &team.id, |team, events| {
        if let Some(found) = team.task_mut(task_id).and_then(|task| task.attempts.iter_mut().find(|found| found.id == attempt.id)) {
            found.decisions_scanned = true;
            found.decision_flags = flags.clone();
        }
        if !flags.is_empty() {
            events.push(Event::new("decision_conflict", json!({"task":task_id,"attempt":attempt.n,"flags":flags})));
        }
        Ok(())
    })
}

/// Task 156: the Decisions section of each spec the decisions amend, written
/// into the lead branch's checkout and committed there (only when it is
/// clean); the spec is a view of the records.
fn spec_decisions(store: &mut Store, team: &Team) -> anyhow::Result<()> {
    let folder = team.integration_folder().to_owned();
    anyhow::ensure!(!folder.is_empty(), "the team has no folder for its specs");
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = super::super::worktrees::git(Path::new(&folder), &["status", "--porcelain", "--untracked-files=no"], deadline)
        .map_err(|error| anyhow::anyhow!("the lead branch's status was not read: {}", error.detail))?;
    if !String::from_utf8_lossy(&status).trim().is_empty() {
        // The next tick tries again once the checkout is clean.
        return Ok(());
    }
    let branch = lead_branch(team)?;
    let mut written = Vec::new();
    for spec in super::decisions::specs(team) {
        let path = Path::new(&folder).join(&spec);
        let Ok(current) = std::fs::read_to_string(&path) else { continue };
        let updated = super::decisions::render_into(&current, &super::decisions::spec_section(team, &spec));
        if updated != current {
            std::fs::write(&path, &updated).with_context(|| format!("{}", path.display()))?;
            written.push(spec);
        }
    }
    if !written.is_empty() {
        commit_in(&folder, &branch, &format!("Decisions: {} from the team's decision records", written.join(", ")), &written)?;
    }
    let hash = super::decisions::specs_hash(team);
    change(store, &team.id, |team, events| {
        team.decision_specs = hash.clone();
        if !written.is_empty() {
            events.push(Event::new("decision_specs_written", json!({"specs":written})));
        }
        Ok(())
    })
}

pub(crate) fn send(session: &SessionRef, request_id: &str, text: &str) -> Sent {
    let input = json!({"request_id":request_id,"expected_run_id":session.run_id,
        "expected_conversation_id":session.conversation_id,"text":text});
    classify(&hgs(&session.host, &["send", &session.name, "--json"], Some(&input), Duration::from_secs(120)))
}

pub(crate) fn tag(text: &str) -> String {
    let tag: String = text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    tag.chars().take(24).collect()
}

/// C3/B5: the agent's own permission mode for a role profile, where the agent
/// has one; None means the profile is advisory.
pub(crate) fn profile_mode(agent: &str, profile: &str) -> Option<&'static str> {
    // Task 154: the other agents take the profile as their native selection
    // (rights::profile_selection), passed with --permissions.
    if agent == "claude" { super::rights::claude_mode(profile) } else { None }
}

/// B4/F10: a sandboxed agent's folder is a clone (its `.git` is inside, so
/// merge and fetch work there); other agents get a worktree. Used when a plan
/// leaves the folder to the team (folder policy auto).
/// Task #35 (team ede724c4, 2026-11-10): a DeepSeek (dsh) session's sandbox
/// gives the folder it works in Windows' inheritable Low integrity label
/// (Zerus grants it WRITE_OWNER there, which setting a label needs). Every
/// program built there afterwards starts at Low and is refused outside it:
/// a qa member of team 33fbafeb shared D:\Tools\Zerus, the owner's checkout
/// and the team's integration folder, and 2047 tests, the lead's runs and the
/// Tauri build script failed with Access denied. So a DeepSeek member works
/// only in its own Zerus-made folder (its clone or worktree): never a shared
/// folder, and never as the lead, who works in the team folder. A member that
/// has a session (adopted, or started before) is not launched; the check is
/// for the start Zerus makes. None when the member may start; else the
/// refusal with the one action.
pub(crate) fn deepseek_folder_problem(team: &Team, member: &Member) -> Option<String> {
    if member.agent != "dsh" || member.state == MemberState::Removed || member.session.is_some() || dsh_unsandboxed(member) {
        return None;
    }
    let folder = if member.directory.is_empty() { team.folder.as_str() } else { member.directory.as_str() };
    const WHY: &str = "a DeepSeek (dsh) session's sandbox gives the folder it works in Windows' Low integrity label, and every program built there afterwards is refused outside it";
    if member.id == team.lead {
        return Some(format!("{}: the lead works in the team folder {folder}, and {WHY}. Choose another agent for the lead.", member.id));
    }
    if member.folder != MemberFolder::Shared {
        return None;
    }
    Some(format!(
        "{id}: {WHY}, so it may not share {folder}. Run it without the sandbox (permissions profile network: DeepSeek's danger-full-access preset runs at Medium integrity and labels nothing): {{\"team\":\"{team}\",\"members_update\":[{{\"id\":\"{id}\",\"permissions_profile\":\"network\"}}],\"request_id\":\"UUID\"}} | hgs team settings --json; or give it its own clone: the same with \"folder\":\"clone\" (or choose another agent for it).",
        id = member.id,
        team = team.id
    ))
}

/// RogueDOD (owner, 2026-11-12): a DeepSeek member that runs without the
/// sandbox, DeepSeek's danger-full-access preset (the network and operate
/// profiles, or a permission that names it). Checked live: its commands run
/// at Medium integrity, the folder and the repository's .git keep no label,
/// and git and lore commit there. Such a member may share a folder and
/// needs no clone of its own.
pub(crate) fn dsh_unsandboxed(member: &Member) -> bool {
    member.agent == "dsh"
        && if member.permissions.trim().is_empty() {
            matches!(member.permissions_profile.as_str(), "network" | "operate")
        } else {
            member.permissions.contains("danger-full-access")
        }
}

pub(crate) fn default_folder(agent: &str) -> MemberFolder {
    if agent == "dsh" {
        MemberFolder::Clone
    } else {
        MemberFolder::Worktree
    }
}

/// Task #28 (5): a sandboxed dsh member cannot write a worktree's git
/// metadata (it lives in the main checkout's `.git`, outside the sandbox), so
/// its own folder is a clone whatever asked for a worktree: a role default, a
/// template, the New team form or the worktree policy, which a clone also
/// meets. Before, the preflight only warned and the member could not commit.
/// An adopted session keeps the folder it works in.
pub(crate) fn sandbox_folder(member: &mut Member) {
    if member.agent == "dsh" && member.folder == MemberFolder::Worktree && member.session.is_none() && !dsh_unsandboxed(member) {
        member.folder = MemberFolder::Clone;
    }
}

/// The folder the member works in, computed without creating anything: the
/// team's (or its own) folder, or the worktree/clone path a writing member
/// gets.
pub(crate) fn member_folder_path(team: &Team, member: &Member) -> anyhow::Result<String> {
    // F8/N13: an adopted session works in the folder it was adopted in. That
    // folder is the member's own; a worktree name computed from it is a folder
    // nobody writes to, so a report queued there never reaches the worker.
    if member.session.is_some() && !member.directory.is_empty() {
        return Ok(member.directory.clone());
    }
    let folder = if member.directory.is_empty() { team.folder.clone() } else { member.directory.clone() };
    anyhow::ensure!(!folder.is_empty(), "The team has no folder");
    if !matches!(member.folder, MemberFolder::Worktree | MemberFolder::Clone) {
        return Ok(folder);
    }
    let short = &team.id[..8.min(team.id.len())];
    let path = Path::new(&folder);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("project");
    Ok(path
        .parent()
        .unwrap_or(path)
        .join(format!("{name}-team-{short}-{}", tag(&member.id)))
        .display()
        .to_string())
}

/// F1.4/F1.6: one git read in a folder, trimmed; None when git says nothing.
/// F1.4/F1.6: one git command in a folder. Tests can set a per-thread ceiling
/// (a temp dir) so a TEMP inside another repository cannot leak into discovery
/// without touching the process environment other tests share.
pub(crate) fn git_command(folder: &str) -> std::process::Command {
    // Task 287: a git started while this thread holds the store's write lock
    // is a warning in the lock log.
    super::store::note_process("git", &[folder.to_owned()]);
    let mut command = std::process::Command::new("git");
    // A member's folder may carry its own config: no fsmonitor, hooks or
    // optional locks run for these reads.
    command.args(["--no-optional-locks", "-c", "core.fsmonitor=false", "-c", "core.hooksPath="]);
    command.arg("-C").arg(folder);
    #[cfg(test)]
    if let Some(ceiling) = GIT_CEILING.with(|ceiling| ceiling.borrow().clone()) {
        command.env("GIT_CEILING_DIRECTORIES", ceiling);
    }
    command
}

#[cfg(test)]
thread_local! {
    static GIT_CEILING: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// A test's stand-in for the `hgs` child this thread would run: a send, a
/// listing or a request on another computer.
#[cfg(test)]
pub(crate) type FakeHgs = Box<dyn FnMut(&str, &[&str], Option<&Value>) -> anyhow::Result<Output>>;

#[cfg(test)]
thread_local! {
    static FAKE_HGS: std::cell::RefCell<Option<FakeHgs>> = std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(crate) fn set_fake_hgs(fake: Option<FakeHgs>) {
    FAKE_HGS.with(|value| *value.borrow_mut() = fake);
}

/// A finished child's output with this exit code.
#[cfg(test)]
pub(crate) fn fake_output(code: i32, stdout: &str, stderr: &str) -> Output {
    #[cfg(unix)]
    let status = std::os::unix::process::ExitStatusExt::from_raw(code << 8);
    #[cfg(windows)]
    let status = std::os::windows::process::ExitStatusExt::from_raw(code as u32);
    Output { status, stdout: stdout.as_bytes().to_vec(), stderr: stderr.as_bytes().to_vec() }
}

#[cfg(test)]
/// A member's subagent card as the approval channel builds it (U19).
pub(crate) fn subagent_card() -> Value {
    json!({"question_id":"claude-approval:subagents","question_hash":"h".repeat(64),"tool_call_id":"toolu_one",
        "source":"claude_tool_approval","approval":true,"can_answer":true,"approval_choices":true,"answer_transport":"claude_hook",
        "panel_verified":false,"subagent_confirmation":true,"subagent_calls":["toolu_one","toolu_two"],"run_id":"r1","conversation_id":"c",
        "questions":[{"id":"approval","header":"Subagents",
            "question":"Start 2 subagents: Explore (model haiku): Find the callers; general (model inherits claude-opus-4-6): Write the fix",
            "body":"Start 2 subagents: ...","allow_other":true,"other_label":"Tell Claude what to do instead",
            "options":[{"id":"1","label":"Yes","scope":"once"},{"id":"2","label":"No","scope":"once"}]}]})
}

/// The ceiling this test thread's git reads use; call it at the start of a test
/// that creates a repository under a TEMP which may itself sit in one.
#[cfg(test)]
pub(crate) fn set_git_ceiling(ceiling: &str) {
    GIT_CEILING.with(|value| *value.borrow_mut() = Some(ceiling.to_owned()));
}

/// Git answers read before the write lock, by folder and arguments.
type GitMemo = std::collections::HashMap<(String, Vec<String>), Option<String>>;

thread_local! {
    /// Task 287: git answers gathered before a store transaction and served
    /// inside it (`with_git_answers`), so no git runs under the write lock.
    static GIT_MEMO: std::cell::RefCell<Option<GitMemo>> =
        const { std::cell::RefCell::new(None) };
}

/// Task 287: runs `prefetch` (the git reads a change will make, outside any
/// transaction), then `then` (the change) with those answers served from
/// memory. A read `prefetch` did not make still runs, and the store logs it.
pub(crate) fn with_git_answers<T>(prefetch: impl FnOnce(), then: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            GIT_MEMO.with(|memo| *memo.borrow_mut() = None);
            super::plan::prerequisite_memo(false);
        }
    }
    GIT_MEMO.with(|memo| *memo.borrow_mut() = Some(std::collections::HashMap::new()));
    // Task #41: the phase checks' prerequisite reads are served the same way.
    super::plan::prerequisite_memo(true);
    let _reset = Reset;
    prefetch();
    then()
}

#[cfg(test)]
thread_local! {
    /// Task #401: the git processes this thread started (their first argument).
    static GIT_CALLS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Task #401: the git processes this thread started since the last call.
#[cfg(test)]
pub(crate) fn take_git_calls() -> Vec<String> {
    GIT_CALLS.with(|calls| std::mem::take(&mut *calls.borrow_mut()))
}

fn note_git(_first: &str) {
    #[cfg(test)]
    GIT_CALLS.with(|calls| calls.borrow_mut().push(_first.to_owned()));
}

/// Task #401: the contents of `specs` (`<rev>:<path>`), read by one `git
/// cat-file --batch` instead of a `git show` per file; None for a spec git has
/// no blob for (a file added since `rev`). Memoized like `git_at`, so a
/// report's prefetch answers the read inside the store's transaction.
pub(crate) fn git_cat_files(folder: &str, specs: &[String]) -> Option<Vec<Option<String>>> {
    if specs.is_empty() {
        return Some(Vec::new());
    }
    let mut key_args = vec!["cat-file".to_owned(), "--batch".to_owned()];
    key_args.extend(specs.iter().cloned());
    let key = (folder.to_owned(), key_args);
    if let Some(answer) = GIT_MEMO.with(|memo| memo.borrow().as_ref().and_then(|memo| memo.get(&key).cloned())) {
        return answer.and_then(|text| serde_json::from_str(&text).ok());
    }
    note_git("cat-file");
    let answer = (|| {
        use std::io::{Read, Write};
        let mut child = git_command(folder)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut stdin = child.stdin.take()?;
        let input: String = specs.iter().map(|spec| format!("{spec}\n")).collect();
        // Written beside the read, so a large answer never fills the pipe first.
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
        let mut out = Vec::new();
        child.stdout.take()?.read_to_end(&mut out).ok()?;
        let _ = writer.join();
        if !child.wait().ok()?.success() {
            return None;
        }
        parse_cat_batch(&out, specs.len())
    })();
    GIT_MEMO.with(|memo| {
        if let Some(memo) = memo.borrow_mut().as_mut() {
            memo.insert(key, answer.as_ref().and_then(|answer| serde_json::to_string(answer).ok()));
        }
    });
    answer
}

/// `git cat-file --batch` output: per object `<oid> <type> <size>\n<bytes>\n`,
/// or `<spec> missing\n`.
fn parse_cat_batch(out: &[u8], count: usize) -> Option<Vec<Option<String>>> {
    let mut answers = Vec::with_capacity(count);
    let mut at = 0;
    while answers.len() < count {
        let end = at + out.get(at..)?.iter().position(|byte| *byte == b'\n')?;
        let header = String::from_utf8_lossy(&out[at..end]).into_owned();
        at = end + 1;
        let parts: Vec<&str> = header.split(' ').collect();
        match parts.as_slice() {
            [_, kind, size] if size.parse::<usize>().is_ok() => {
                let size: usize = size.parse().ok()?;
                let body = out.get(at..at + size)?;
                answers.push((*kind == "blob").then(|| String::from_utf8_lossy(body).into_owned()));
                at += size + 1;
            }
            _ => answers.push(None),
        }
    }
    Some(answers)
}

pub(crate) fn git_at(folder: &str, args: &[&str]) -> Option<String> {
    // Task #393: a blank folder is no repository. A `git -C "" rev-parse`
    // reads the process's own folder and was seen in the status and inbox
    // reads; it is never started.
    if folder.trim().is_empty() {
        return None;
    }
    let key = (folder.to_owned(), args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<String>>());
    if let Some(answer) = GIT_MEMO.with(|memo| memo.borrow().as_ref().and_then(|memo| memo.get(&key).cloned())) {
        return answer;
    }
    // Task #393: the answer stays for this process's next tick while the
    // repository's HEAD (its ref, or its commit) is the same, so a worker
    // tick reads one project's folder once instead of every body reading it.
    let head = head_key(folder);
    if !head.is_empty() {
        if let Some(answer) = git_cache_get(&key, &head) {
            return answer;
        }
    }
    note_git(args.first().copied().unwrap_or_default());
    let answer = (|| {
        let output = git_command(folder).args(args).output().ok()?;
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    })();
    GIT_MEMO.with(|memo| {
        if let Some(memo) = memo.borrow_mut().as_mut() {
            memo.insert(key.clone(), answer.clone());
        }
    });
    if !head.is_empty() {
        git_cache_put(key, head, answer.clone());
    }
    answer
}

/// Task #393: the repository's HEAD as a cache key, read from its files, never
/// by starting Git: the HEAD file, the ref it names (a worktree's refs live in
/// its common folder), or the commit of a detached HEAD.
pub(crate) fn head_key(folder: &str) -> String {
    let dot = std::path::Path::new(folder).join(".git");
    let gitdir = if dot.is_dir() {
        dot
    } else {
        let Ok(text) = std::fs::read_to_string(&dot) else { return String::new() };
        let Some(path) = text.trim().strip_prefix("gitdir:") else { return String::new() };
        let path = std::path::PathBuf::from(path.trim());
        if path.is_absolute() { path } else { dot.parent().unwrap_or(std::path::Path::new(folder)).join(path) }
    };
    let Ok(head) = std::fs::read_to_string(gitdir.join("HEAD")) else { return String::new() };
    let mut key = head.trim().to_owned();
    if let Some(reference) = head.trim().strip_prefix("ref: ") {
        let common = std::fs::read_to_string(gitdir.join("commondir"))
            .ok()
            .map(|common| {
                let path = std::path::PathBuf::from(common.trim());
                if path.is_absolute() { path } else { gitdir.join(path) }
            })
            .unwrap_or_else(|| gitdir.clone());
        let mut found = false;
        for dir in [&gitdir, &common] {
            if let Ok(value) = std::fs::read_to_string(dir.join(reference)) {
                key.push('=');
                key.push_str(value.trim());
                found = true;
                break;
            }
        }
        if !found {
            if let Ok(packed) = std::fs::read_to_string(common.join("packed-refs")) {
                for line in packed.lines() {
                    if let Some((sha, name)) = line.split_once(' ') {
                        if name == reference {
                            key.push('=');
                            key.push_str(sha);
                            break;
                        }
                    }
                }
            }
        }
    }
    key
}

type GitCacheKey = (String, Vec<String>);
type GitCacheEntry = (String, Option<String>);
static GIT_CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<GitCacheKey, GitCacheEntry>>> =
    std::sync::OnceLock::new();

fn git_cache() -> &'static std::sync::Mutex<std::collections::HashMap<GitCacheKey, GitCacheEntry>> {
    GIT_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn git_cache_get(key: &GitCacheKey, head: &str) -> Option<Option<String>> {
    let cache = git_cache().lock().ok()?;
    let (entry_head, answer) = cache.get(key)?;
    (entry_head == head).then(|| answer.clone())
}

fn git_cache_put(key: GitCacheKey, head: String, answer: Option<String>) {
    let Ok(mut cache) = git_cache().lock() else { return };
    // A worker's ticks see many folders; the bound keeps the map small.
    if cache.len() > 4096 {
        cache.clear();
    }
    cache.insert(key, (head, answer));
}

/// F1.4: the reason a member's folder cannot be started in, or None when it
/// can: it exists, it is the team folder or the same repository (a worktree,
/// clone or sibling of the team folder), and a worktree/clone sits on the
/// member's branch. A folder of another repository is refused, never filed as
/// General and never silently used.
pub(crate) fn folder_problem(folder: &str, source: &str, branch: Option<&str>) -> Option<String> {
    let path = Path::new(folder);
    if !path.exists() {
        return Some(format!("{folder} does not exist"));
    }
    let repo_of = |at: &str| git_at(at, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).or_else(|| git_at(at, &["rev-parse", "--git-common-dir"]));
    if let Some(source_repo) = repo_of(source) {
        let Some(folder_repo) = repo_of(folder) else {
            return Some(format!("{folder} is not a repository like {source}"));
        };
        if Path::new(&folder_repo) != Path::new(&source_repo) && !clone_of(folder, source) {
            return Some(format!("{folder} belongs to another repository"));
        }
    }
    if let Some(branch) = branch {
        if let Some(current) = git_at(folder, &["rev-parse", "--abbrev-ref", "HEAD"]) {
            if current != branch {
                return Some(format!("{folder} is on {current}, not {branch}"));
            }
        }
    }
    None
}

/// C10: a clone of the team's repository has its own `.git`, so its common
/// dir differs: it is the team's when its origin is the team folder (as
/// `worktrees create --clone` makes it) or it shares the repository's root
/// commit.
fn clone_of(folder: &str, source: &str) -> bool {
    let canonical = |path: &str| crate::platform::canonical(Path::new(path)).ok();
    // Task #5 (team ede724c4): the origin from the clone's config file when it
    // names it plainly, without a git process.
    #[cfg(test)]
    let from_files = if old_status() { None } else { origin_url_from_files(folder) };
    #[cfg(not(test))]
    let from_files = origin_url_from_files(folder);
    if let Some(origin) = from_files.or_else(|| git_at(folder, &["config", "--get", "remote.origin.url"])) {
        let origin = origin.strip_prefix("file://").unwrap_or(&origin);
        if canonical(origin).is_some_and(|origin| Some(origin) == canonical(source)) {
            return true;
        }
    }
    let roots = |at: &str| -> std::collections::BTreeSet<String> {
        git_at(at, &["rev-list", "--max-parents=0", "HEAD"])
            .map(|out| out.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    };
    let mine = roots(folder);
    !mine.is_empty() && !mine.is_disjoint(&roots(source))
}

/// The same folder written two ways (separators, case on Windows drives, the
/// `\\?\` prefix, a trailing separator).
pub(crate) fn same_path(left: &str, right: &str) -> bool {
    let key = |path: &str| {
        let path = path.strip_prefix(r"\\?\").unwrap_or(path).replace('\\', "/");
        let path = path.trim_end_matches('/').to_owned();
        if path.as_bytes().get(1) == Some(&b':') { path.to_lowercase() } else { path }
    };
    !left.is_empty() && key(left) == key(right)
}

/// C11/C12: what is at `path` on a member's other computer, read there with
/// `hgs worktrees --path`: None when nothing is, else the branch of the
/// checkout rooted at `path` (empty when it is no checkout of its own).
fn remote_checkout(run: &dyn Fn(&str, &[&str]) -> anyhow::Result<Output>, computer: &str, path: &str) -> anyhow::Result<Option<String>> {
    let output = run(computer, &["worktrees", "--path", path, "--refresh", "--json"])?;
    anyhow::ensure!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr).trim());
    let value: Value = serde_json::from_slice(&output.stdout).context("unreadable worktree catalog")?;
    if value["state"] == "folder_unavailable" {
        return Ok(None);
    }
    let root = value["selected_root"].as_str().unwrap_or_default();
    if value["state"] != "ok" || !same_path(root, path) {
        return Ok(Some(String::new()));
    }
    let branch = value["worktrees"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|tree| tree["path"].as_str().is_some_and(|tree| same_path(tree, root)))
        .and_then(|tree| tree["branch"].as_str())
        .unwrap_or_default();
    Ok(Some(branch.to_owned()))
}

/// F1.6/N37: the folder's repository and branch, read from git in one command
/// (a status call reads each distinct folder once); empty when unknown.
pub(crate) fn folder_facts(folder: &str) -> (String, String) {
    #[cfg(test)]
    FOLDER_FACTS_READS.with(|reads| reads.set(reads.get() + 1));
    if folder.is_empty() {
        return (String::new(), String::new());
    }
    // Task #5 (team ede724c4): the repository's own files answer without a
    // git process (one cost 100-400 ms on Windows, once per member folder of
    // every status poll); git answers what they do not.
    #[cfg(test)]
    let fast = !old_status();
    #[cfg(not(test))]
    let fast = true;
    if let Some(facts) = fast.then(|| folder_facts_from_files(folder)).flatten() {
        return facts;
    }
    // N37: one `rev-parse` prints the common dir and the branch together, so a
    // status call with several members in one folder spawns one git, not 2-4
    // per member.
    if let Some(read) = git_at(folder, &["rev-parse", "--path-format=absolute", "--git-common-dir", "--abbrev-ref", "HEAD"]) {
        let mut lines = read.lines();
        let dir = lines.next().unwrap_or_default().trim().to_owned();
        let branch = lines.next().unwrap_or_default().trim().to_owned();
        let project = Path::new(&dir).parent().map(|parent| parent.display().to_string()).unwrap_or(dir);
        return (project, branch);
    }
    // An older git, or a repository with no commit yet (an unborn HEAD has no
    // branch to abbreviate): the same facts in separate reads.
    let project = git_at(folder, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .or_else(|| git_at(folder, &["rev-parse", "--git-common-dir"]))
        .map(|dir| {
            let path = Path::new(&dir);
            path.parent().map(|parent| parent.display().to_string()).unwrap_or(dir)
        })
        .unwrap_or_default();
    let branch = git_at(folder, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    (project, branch)
}

/// Task #5 (team ede724c4): what `git rev-parse --path-format=absolute
/// --git-common-dir --abbrev-ref HEAD` prints for `folder`, read from the
/// repository's files: the folder of the common git dir (forward slashes, as
/// git prints it) and the branch, or `HEAD` when detached. None for anything
/// this reader does not know for certain (a git environment override, a
/// reftable store, an unborn or ambiguous branch, a ref outside refs/heads,
/// an unusual `.git`), which git then answers as before.
pub(crate) fn folder_facts_from_files(folder: &str) -> Option<(String, String)> {
    let (git_dir, common) = git_dirs_from_files(folder)?;
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    let branch = if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        let name = reference.strip_prefix("refs/heads/")?;
        let packed = std::fs::read_to_string(common.join("packed-refs")).unwrap_or_default();
        let exists = |full: &str| common.join(full).is_file() || packed.lines().any(|line| line.split_once(' ').is_some_and(|(_, at)| at == full));
        // An unborn branch has no commit to abbreviate; a tag or remote of
        // the same name makes git print a longer name.
        if name.is_empty() || !exists(reference) || exists(&format!("refs/tags/{name}")) || exists(&format!("refs/remotes/{name}")) || exists(&format!("refs/{name}")) || exists(name) {
            return None;
        }
        name.to_owned()
    } else if head.len() >= 40 && head.chars().all(|ch| ch.is_ascii_hexdigit()) {
        "HEAD".to_owned()
    } else {
        return None;
    };
    let project = common.parent()?.to_str()?;
    #[cfg(windows)]
    let project = {
        // A verbatim path (\\?\D:\x) as git prints it (D:/x); a share is
        // left to git.
        let plain = project.strip_prefix(r"\\?\").unwrap_or(project);
        if plain.starts_with(r"\\") || plain.starts_with("UNC") {
            return None;
        }
        plain.replace('\\', "/")
    };
    #[cfg(not(windows))]
    let project = project.to_owned();
    Some((project, branch))
}

/// Task #5 (team ede724c4): `git config --get remote.origin.url` in `folder`,
/// read from the repository's own config file when it names the origin
/// plainly; None for anything else (an include, a worktree config, quoting or
/// escapes beyond a doubled backslash, no origin there), which git answers.
pub(crate) fn origin_url_from_files(folder: &str) -> Option<String> {
    let (git_dir, common) = git_dirs_from_files(folder)?;
    if git_dir.join("config.worktree").exists() || common.join("config.worktree").exists() {
        return None;
    }
    let text = std::fs::read_to_string(common.join("config")).ok()?;
    let mut in_origin = false;
    let mut url = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            let header = line.to_ascii_lowercase();
            if header.starts_with("[include") {
                return None;
            }
            in_origin = header.starts_with(r#"[remote "origin""#);
            if in_origin && line != r#"[remote "origin"]"# {
                return None;
            }
            continue;
        }
        if !in_origin {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        if !key.trim().eq_ignore_ascii_case("url") {
            continue;
        }
        let value = value.trim();
        if value.contains(['"', ';', '#']) || value.replace(r"\\", "").contains('\\') {
            return None;
        }
        url = Some(value.replace(r"\\", "\\"));
    }
    url.filter(|url| !url.is_empty())
}

/// The git dir and the common git dir of the repository `folder` is in, as
/// git would find them, from the files alone; None for anything unusual.
fn git_dirs_from_files(folder: &str) -> Option<(PathBuf, PathBuf)> {
    if ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_CEILING_DIRECTORIES", "GIT_DISCOVERY_ACROSS_FILESYSTEM"]
        .iter()
        .any(|name| std::env::var_os(name).is_some())
    {
        return None;
    }
    let start = std::fs::canonicalize(folder).ok()?;
    // A test's git ceiling, as its git reads get it: never looked at or above.
    #[cfg(test)]
    let ceiling = GIT_CEILING.with(|ceiling| ceiling.borrow().clone()).and_then(|ceiling| std::fs::canonicalize(ceiling).ok());
    #[cfg(not(test))]
    let ceiling: Option<PathBuf> = None;
    let mut dir = start.as_path();
    let dot_git = loop {
        if ceiling.as_deref() == Some(dir) {
            return None;
        }
        let candidate = dir.join(".git");
        if candidate.symlink_metadata().is_ok() {
            break candidate;
        }
        dir = dir.parent()?;
    };
    let meta = dot_git.symlink_metadata().ok()?;
    let (git_dir, common) = if meta.is_dir() {
        if dot_git.join("commondir").exists() {
            return None;
        }
        (dot_git.clone(), dot_git.clone())
    } else if meta.is_file() {
        // A linked worktree: `gitdir: <its git dir>`, whose `commondir` names
        // the repository's own git dir.
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
        let git_dir = std::fs::canonicalize(dir.join(target)).ok()?;
        let common = match std::fs::read_to_string(git_dir.join("commondir")) {
            Ok(text) => std::fs::canonicalize(git_dir.join(text.lines().next()?.trim())).ok()?,
            Err(_) => git_dir.clone(),
        };
        (git_dir, common)
    } else {
        return None;
    };
    if !common.join("objects").is_dir() || !common.join("refs").is_dir() || common.join("reftable").exists() {
        return None;
    }
    Some((git_dir, common))
}

#[cfg(test)]
thread_local! {
    static FOLDER_FACTS_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Task #5 (team ede724c4): this test thread's status reads as before
    /// (git for every folder fact, an estimate per task over the whole
    /// history), to compare the old and the new view.
    static OLD_STATUS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn set_old_status(on: bool) {
    OLD_STATUS.with(|old| old.set(on));
}

#[cfg(test)]
pub(crate) fn old_status() -> bool {
    OLD_STATUS.with(std::cell::Cell::get)
}

/// N37: how many folder-fact reads this test thread made, so a status call's
/// git work can be counted.
#[cfg(test)]
pub(crate) fn folder_facts_reads() -> usize {
    FOLDER_FACTS_READS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_folder_facts_reads() {
    FOLDER_FACTS_READS.with(|reads| reads.set(0));
}

/// F1.6/N36: whether a member's folder is outside the team's repository. A
/// team's project label is the GUI's group name, never a path or an id, so the
/// folders themselves decide: the same repository (shared, nested or a
/// worktree), a clone of it (C10) or an unknown folder is not outside.
pub(crate) fn outside_project(team_folder: &str, folder: &str, project: &str, team_project: &str) -> bool {
    if team_folder.is_empty() || folder.is_empty() || project.is_empty() || team_project.is_empty() {
        return false;
    }
    if same_path(folder, team_folder) || same_path(project, team_project) {
        return false;
    }
    !clone_of(folder, team_folder)
}

/// F1.1/N36: the project an adopted session is filed into, from the catalog's
/// entry for the team's folder. `team.project` is the GUI's group name (a
/// label), not a catalog project id or a path, so it never decides this.
fn filed_project(known: &Value) -> Option<String> {
    let project = known["project_id"].as_str().unwrap_or_default();
    (!project.is_empty()).then(|| project.to_owned())
}

/// The request `hgs swarm assign-launch` takes for an adopted member (F1.1).
fn assign_launch_payload(swarm_id: &str, project_id: &str, session: &SessionRef, directory: &str) -> Value {
    json!({"request_id":uuid::Uuid::new_v4().to_string(),"name":session.name,"expected_run_id":session.run_id,
        "expected_conversation_id":session.conversation_id,"swarm_id":swarm_id,"project_id":project_id,"directory":directory})
}

/// F1.1/N36: the `swarm assign-launch` payload for an adopted member, or None
/// when the catalog does not know the folder or its project. The project is the
/// folder's catalog entry: a team's own `project` is the GUI's group name, not
/// a catalog project id, and never decides the filing.
fn filing_payload(team: &Team, known: &Value, session: &SessionRef) -> Option<Value> {
    if team.folder.is_empty() {
        return None;
    }
    let swarm_id = known["swarm_id"].as_str().unwrap_or_default();
    if swarm_id.is_empty() {
        return None;
    }
    let project_id = filed_project(known)?;
    Some(assign_launch_payload(swarm_id, &project_id, session, &team.folder))
}

/// F1.1: files an adopted member's session into the team's project with
/// `hgs swarm assign-launch`, as the dispatcher's other child hgs calls do. No
/// catalog, or a folder the catalog does not know, means nothing to do; a
/// failure never blocks the member.
fn file_in_project(team: &Team, computer: &str, session: &SessionRef) {
    let Ok(config) = crate::config::Config::load() else { return };
    let Some(known) = crate::swarm::directory_project(&config, Path::new(&team.folder)) else { return };
    let Some(payload) = filing_payload(team, &known, session) else { return };
    let _ = hgs(computer, &["swarm", "assign-launch", "--json"], Some(&payload), Duration::from_secs(15));
}

/// The folder the member works in: the team's (or its own) folder, or a new
/// worktree or clone of it, created now.
fn member_folder(team: &Team, member: &Member) -> anyhow::Result<String> {
    member_folder_with(team, member, &|host, args| {
        // Task 20: `worktrees create --clone` of a large repository takes
        // minutes on Windows (its clone step alone may take 25); the
        // catalog reads stay short.
        let minutes = if args.get(1) == Some(&"create") { 40 } else { 5 };
        hgs(host, args, None, Duration::from_secs(minutes * 60))
    })
}

/// Task #13 (team ede724c4): how long a member's worktree creation waits for
/// another creation in the same repository before it fails.
const WORKTREE_BUSY_WAIT: Duration = Duration::from_secs(300);

/// Task #13: `worktrees create` refused because another worktree operation
/// holds the repository's lock (nothing was created).
fn worktree_busy(stderr: &str) -> bool {
    stderr.contains("Another worktree operation is running")
}

/// DCB-svn report (2026-11-12): `branch` is the task branch (U48) of a task
/// this member holds or worked on, so a folder on it is still the member's.
fn own_task_branch(team: &Team, member: &Member, branch: &str) -> bool {
    !branch.is_empty()
        && team.tasks.iter().any(|task| {
            (task.assignee == member.id || task.attempts.iter().any(|attempt| attempt.member == member.id))
                && task_branch(team, task.id, &member.id).is_some_and(|own| own == branch)
        })
}

/// `member_folder` with the `hgs` call it makes (`run(computer, args)`), so the
/// whole decision is tested without a second hgs.
fn member_folder_with(team: &Team, member: &Member, run: &dyn Fn(&str, &[&str]) -> anyhow::Result<Output>) -> anyhow::Result<String> {
    // Task #35: a DeepSeek member starts only in its own Zerus-made folder;
    // nothing is created, granted or launched otherwise.
    if let Some(problem) = deepseek_folder_problem(team, member) {
        anyhow::bail!("{problem}");
    }
    let destination = member_folder_path(team, member)?;
    // C11: a member on another computer works in a folder there; this
    // computer can neither see nor check it, so the checks run there.
    let remote = !member.computer.is_empty();
    if member.folder == MemberFolder::Shared {
        // F1.4: a shared folder must exist and belong to the team's repository.
        // On another computer its session starts there or fails there.
        if !remote {
            if let Some(problem) = folder_problem(&destination, &team.folder, None) {
                anyhow::bail!("{problem}");
            }
        }
        return Ok(destination);
    }
    let folder = if member.directory.is_empty() { team.folder.clone() } else { member.directory.clone() };
    let short = &team.id[..8.min(team.id.len())];
    let branch = format!("team/{short}/{}", tag(&member.id));
    // N38/F1.4: a worktree or clone is cut from the team's repository, never
    // from another repository the member's own folder field happens to name.
    // The source is checked here on this computer; on another computer
    // `worktrees create` makes and checks the folder there (C11).
    if !remote {
        if let Some(problem) = folder_problem(&folder, &team.folder, None) {
            anyhow::bail!("The worktree source {folder} is not the team's repository: {problem}");
        }
    }
    // C12: the member's own worktree or clone from an earlier start (a
    // relaunch, a swap) is used again; any other folder there is refused.
    // Task 20: a clone whose create ran out of time after `git clone` has no
    // branch of the member's yet; `worktrees create` finishes it (and checks
    // there that it is such a clone) instead of the member failing again.
    let clone = member.folder == MemberFolder::Clone;
    if remote {
        if let Some(found) = remote_checkout(run, &member.computer, &destination)? {
            let own = found == branch || own_task_branch(team, member, &found);
            let unfinished = clone && !found.is_empty() && !own;
            anyhow::ensure!(
                own || unfinished,
                "{destination} already exists on {} and is not this member's folder ({})",
                member.computer,
                if found.is_empty() { "no checkout of its own".to_owned() } else { format!("on {found}, not {branch}") }
            );
            if !unfinished {
                return Ok(destination);
            }
        }
    } else if Path::new(&destination).exists() {
        // A failed create may leave its empty reservation: it goes.
        let empty = std::fs::read_dir(&destination).map(|mut entries| entries.next().is_none()).unwrap_or(false);
        if empty {
            std::fs::remove_dir(&destination)?;
        } else {
            let Some(problem) = folder_problem(&destination, &folder, Some(&branch)) else {
                return Ok(destination);
            };
            // DCB-svn report (2026-11-12): with branch_per_task the prepare
            // step moved the folder to one of the member's own tasks; it is
            // still the member's folder, and the next hand-out picks the
            // task's branch itself.
            if folder_problem(&destination, &folder, None).is_none()
                && git_at(&destination, &["rev-parse", "--abbrev-ref", "HEAD"]).is_some_and(|current| own_task_branch(team, member, &current))
            {
                return Ok(destination);
            }
            let unfinished = clone
                && folder_problem(&destination, &folder, None).is_none()
                && git_at(&destination, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_none();
            if !unfinished {
                anyhow::bail!("{destination} already exists and is not this member's folder: {problem}");
            }
        }
    }
    // Task 20: the member's branch starts from the team's base branch when
    // one is set (U48), else from the HEAD of the team's folder (the lead's
    // branch), never from a clone's default branch.
    let base = if team.settings.base_branch.is_empty() { "HEAD".to_owned() } else { team.settings.base_branch.clone() };
    // Task 374 (owner, 2026-11-08): the integration line may live only in the
    // team's integration folder; fetch its branch into the team's repository
    // before the worktree or clone is cut from it (a clone copies its refs).
    if !remote && !base.is_empty() && base != "HEAD" && !team.settings.integration_folder.trim().is_empty() {
        let source = Path::new(&folder);
        let deadline = Instant::now() + Duration::from_secs(30);
        let known = super::super::worktrees::git(source, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{base}")], deadline).is_ok()
            || super::super::worktrees::git(source, &["rev-parse", "--verify", "--quiet", &format!("{base}^{{commit}}")], deadline).is_ok();
        if !known {
            let integration = team.integration_folder();
            super::super::worktrees::git(source, &["fetch", "--no-tags", "--quiet", integration, &base], deadline)
                .map_err(|error| anyhow::anyhow!("The line's branch {base} is not in {folder} and was not fetched from {integration}: {}", error.detail))?;
        }
    }
    let mut args: Vec<&str> = vec!["worktrees", "create", "--path", &folder, "--branch", &branch, "--destination", &destination, "--base", &base, "--json"];
    if member.folder == MemberFolder::Clone {
        args.insert(2, "--clone");
    }
    // N9: a DeepSeek member works in a sandbox that needs full control of its
    // folder; every other member's worktree is created without touching ACLs.
    if member.agent == "dsh" {
        args.insert(2, "--sandbox");
    }
    // Task #13 (team ede724c4): members created at once share the
    // repository's worktree lock, which `worktrees create` waits for only a
    // few seconds. A creation refused because another one holds it is tried
    // again until WORKTREE_BUSY_WAIT, never a failed member; the refusal
    // reserved nothing, so trying again is safe.
    let started = Instant::now();
    let mut pause = Duration::from_secs(1);
    let output = loop {
        let output = run(&member.computer, &args)?;
        let busy = !output.status.success() && worktree_busy(&String::from_utf8_lossy(&output.stderr));
        if !busy || started.elapsed() >= WORKTREE_BUSY_WAIT {
            break output;
        }
        std::thread::sleep(if cfg!(test) { Duration::from_millis(1) } else { pause });
        pause = (pause * 2).min(Duration::from_secs(10));
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::ensure!(
        output.status.success(),
        "{}{}",
        stderr.trim(),
        if worktree_busy(&stderr) { "; Zerus starts the member again on its next tick (hgs team relaunch starts it now)" } else { "" }
    );
    // F1.4: the new folder is the repository's (a clone by its origin), on the
    // member's branch. `worktrees create` confirmed this where it ran; a
    // member on this computer is checked here again.
    if !remote {
        if let Some(problem) = folder_problem(&destination, &folder, Some(&branch)) {
            anyhow::bail!("{problem}");
        }
    }
    Ok(destination)
}

/// The `hgs` arguments that start a member's session in `folder`.
pub(crate) fn launch_args(team: &Team, member: &Member, folder: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![
        member.agent.clone(),
        folder.into(),
        "--new".into(),
        "-n".into(),
        format!("{}-{}", tag(&team.name), tag(&member.id)),
        "--launch-id".into(),
        member.launch_id.clone(),
    ];
    // The same options the desktop passes for a new session.
    let terminal = member.agent != "dsh" && member.agent != "opencode";
    if terminal {
        args.push("-d".into());
    }
    if !member.account.is_empty() {
        args.extend(["--account".into(), member.account.clone()]);
    }
    if !member.permissions.is_empty() {
        args.extend(["--permissions".into(), member.permissions.clone()]);
    } else if let Some(selection) = super::rights::profile_selection(&member.agent, &member.permissions_profile) {
        // Task 154: Codex's sandbox, Kimi's plan mode and DeepSeek's preset,
        // as the solo launch takes them.
        args.extend(["--permissions".into(), selection]);
    }
    // C3/B5: a profile maps to the agent's own mode where it has one; it is an
    // agent flag, so it goes after `--`, never among hgs's own options.
    let profile_args: Vec<String> = if member.permissions.is_empty() {
        profile_mode(&member.agent, &member.permissions_profile)
            .map(|mode| vec!["--permission-mode".to_owned(), mode.to_owned()])
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    if member.agent == "opencode" && !member.model.is_empty() {
        args.extend(["--model".into(), member.model.clone()]);
        if !member.effort.is_empty() {
            args.extend(["--effort".into(), member.effort.clone()]);
        }
    }
    if terminal {
        args.push("--".into());
        args.extend(profile_args);
        args.extend(agent_args(&member.agent, &member.model, &member.effort));
        // A terminal agent reports its status only once a conversation
        // starts; a short first turn makes the member Ready for its task.
        let intro = if member.id == team.lead {
            format!(
                "You lead the Zerus agent team {}. A planning brief arrives as a message that begins with [Zerus team. \
                 Until it arrives do nothing and reply only: Ready.",
                team.name
            )
        } else {
            format!(
                "You are {} in the Zerus agent team {}. Tasks arrive as messages that begin with [Zerus team. \
                 Until one arrives do nothing and reply only: Ready.",
                member.role, team.name
            )
        };
        let rules = super::super::iron_rules::for_team(team, member).unwrap_or_else(|error| {
            format!("Iron rules unavailable: {error}. Ask the lead before working.\n")
        });
        args.push(if rules.is_empty() {
            intro} else {
            format!("[Zerus team startup]\n{rules}\n{intro}")
        });
    }
    args
}

/// A terminal agent's own model and effort options, after `--`.
pub(crate) fn agent_args(agent: &str, model: &str, effort: &str) -> Vec<String> {
    let mut args = Vec::new();
    if !model.is_empty() {
        args.extend(["--model".into(), model.to_owned()]);
    }
    if !effort.is_empty() {
        match agent {
            "claude" => args.extend(["--effort".into(), effort.to_owned()]),
            "codex" => args.extend(["-c".into(), format!("model_reasoning_effort=\"{effort}\"")]),
            _ => {}
        }
    }
    args
}

/// A DeepSeek member's model and effort, after its session starts: the model as
/// `provider/model` or a bare id (`deepseek-flash`) hgs resolves (task #19).
fn apply_settings(member: &Member) -> anyhow::Result<()> {
    let session = member.session.as_ref().context("no session")?;
    let mut input = json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_run_id":session.run_id,
        "expected_conversation_id":session.conversation_id});
    // Task #19: only a DeepSeek member gets its settings this way; hgs takes
    // its model as `provider/model` or as the bare id one provider offers
    // (`deepseek-flash`), which was dropped before.
    if !member.model.is_empty() {
        input["model"] = json!(member.model);
    }
    if !member.effort.is_empty() {
        input["effort"] = json!(member.effort);
    }
    let output = hgs(&session.host, &["settings", &session.name, "--json"], Some(&input), Duration::from_secs(120))?;
    anyhow::ensure!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr).trim());
    Ok(())
}

/// U45/Task #6: the `hgs settings` call that switches a running session's model
/// and effort, for a task's hand-out and for a member's own change. `Ok` when
/// the agent took it; `Err((refused, reason))`, where `refused` is false for a
/// call that may have reached it.
fn set_session_model(session: &SessionRef, model: &str, effort: &str) -> std::result::Result<(), (bool, String)> {
    let mut input = json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_run_id":session.run_id,
        "expected_conversation_id":session.conversation_id,"model":model});
    if !effort.is_empty() {
        input["effort"] = json!(effort);
    }
    match hgs(&session.host, &["settings", &session.name, "--json"], Some(&input), Duration::from_secs(120)) {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let line = stderr.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("the settings change was refused");
            Err((true, line.chars().take(300).collect::<String>()))
        }
        Err(error) => Err((false, error.to_string().chars().take(300).collect())),
    }
}

/// U45: switches a member's model and effort before it gets `task_id`. The
/// step is recorded before the call, so a crash leaves it pending and then
/// uncertain, never sent twice; the hand-out waits until the member's session
/// record shows the new model. A refusal leaves the task waiting with its
/// reason, as does a call whose outcome is unknown.
fn switch_model(store: &mut Store, team: &Team, task_id: u32, member_id: &str, from: &str, model: &str, effort: &str) -> anyhow::Result<()> {
    let session = team.member(member_id).and_then(|member| member.session.clone()).context("no session")?;
    let to = super::models::label(model, effort);
    change(store, &team.id, |team, events| {
        let role = team.role_of(member_id);
        let task = team.task_mut(task_id).context("unknown task")?;
        anyhow::ensure!(super::models::pending(task).is_none() && task.waits.is_none(), "already switching");
        super::models::record(task, PrepareStep { step: "model".into(), member: member_id.to_owned(), from: from.to_owned(), to: to.clone(), at: now(), result: "pending".into(), reason: String::new() });
        events.push(Event::new("model_switch", json!({"task":task_id,"member":role,"from":from,"to":to})));
        Ok(())
    })?;
    let (state, reason) = match set_session_model(&session, model, effort) {
        Ok(()) => return Ok(()),
        Err((refused, reason)) => (if refused { "refused" } else { "uncertain" }, reason),
    };
    change(store, &team.id, |team, events| {
        let (role, name) = (team.role_of(member_id), team.name_of(member_id));
        let task = team.task_mut(task_id).context("unknown task")?;
        if let Some(step) = task.prepare.iter_mut().rev().find(|step| step.step == "model" && step.result == "pending" && step.member == member_id) {
            step.result = state.into();
            step.reason = reason.clone();
        }
        super::models::wait(task, "model", &format!("{name} could not switch to {to}: {reason}"), now());
        events.push(Event::new(
            if state == "refused" { "model_switch_refused" } else { "model_switch_uncertain" },
            json!({"task":task_id,"member":role,"to":to,"reason":reason}),
        ));
        Ok(())
    })
}

/// Task #6: switches a running member's session to the model and effort its
/// settings change named, before its next task. The change is taken once: a
/// refusal puts the member's model back where its session is, and a newer
/// change that arrived while the call ran stays for the next tick.
fn apply_model(store: &mut Store, team: &Team, member_id: &str) -> anyhow::Result<()> {
    let Some(wanted) = team.member(member_id).and_then(|member| member.model_switch.clone()) else { return Ok(()) };
    let to = super::models::label(&wanted.model, &wanted.effort);
    let outcome = match team.member(member_id).and_then(|member| member.session.as_ref()) {
        Some(session) => set_session_model(session, &wanted.model, &wanted.effort),
        None => Err((true, "the member has no session".to_owned())),
    };
    change(store, &team.id, |team, events| {
        let role = team.role_of(member_id);
        let Some(found) = team.members.iter_mut().find(|member| member.id == member_id) else { return Ok(()) };
        // The hgs call may have taken up to two minutes; a change the owner made
        // meanwhile is newer than this one and stays for the next tick.
        if found.model_switch.as_ref() != Some(&wanted) {
            return Ok(());
        }
        found.model_switch = None;
        match &outcome {
            Ok(()) => events.push(Event::new("model_switched", json!({"member":role,"to":to}))),
            // A definite refusal leaves the session on the model it ran, so the
            // member's model follows it back. An uncertain call may have reached
            // the session: the new value stays and the feed says so.
            Err((true, reason)) => {
                found.model = wanted.from_model.clone();
                found.effort = wanted.from_effort.clone();
                events.push(Event::new("model_switch_failed", json!({"member":role,"to":to,"reason":reason})));
            }
            Err((false, reason)) => events.push(Event::new("model_switch_uncertain", json!({"member":role,"to":to,"reason":reason}))),
        }
        Ok(())
    })
}

/// Owner feedback 2026-11-04 (14, 15): a swapped member's old session kept
/// the member's name, so every new session failed with "This session already
/// has an account", and an old session waiting on a startup dialog refused
/// pause and archive alike. It is paused when it can be (its conversation stays
/// under Saved; nothing is killed) and renamed to `<name>-replaced`, so the new
/// session starts under the member's name. A remote one is left to its own
/// computer. The note says what happened; an error names the old session.
/// Task #17 (team ede724c4): what setting a replaced session aside gave.
#[derive(Debug, PartialEq)]
pub(crate) enum SetAside {
    /// Nothing runs in the member's folder any more (none was replaced, it
    /// had stopped, or it was paused and renamed): the new session starts.
    Done(Option<String>),
    /// The replaced session still works or waits on a dialog: it is neither
    /// renamed nor left beside a new one. It keeps the member's name and its
    /// open attempt; the start waits for a later tick.
    Busy(String),
}

/// Feedback 14: a swap's (or relaunch's) old session is set aside first, so
/// the new one can take the member's name. Task #17 (team ede724c4): a
/// replaced session that still works is never renamed and left running beside
/// the new one in the same folder (dev1-replaced worked on task #1 for twenty
/// minutes next to the new dev1): it is paused once its turn ends, and only
/// then does the new session start.
fn set_aside_replaced(member: &Member) -> anyhow::Result<SetAside> {
    let Some(old) = member.replaced.as_ref().filter(|old| old.host.is_empty() && !old.name.is_empty()) else {
        return Ok(SetAside::Done(None));
    };
    #[cfg(test)]
    if let Some(stand_in) = test_set_aside() {
        return stand_in(&old.name);
    }
    // RogueDOD (2026-11-12, team 75549318): a DeepSeek session is a binding of
    // its own, not a terminal record; read as one, it looked gone, and every
    // relaunch and swap of a DeepSeek member failed on its still-taken name.
    if crate::state::dsh::exists(&old.name) {
        use crate::state::dsh::Aside;
        return match crate::state::dsh::set_aside(&old.name, &old.conversation_id) {
            Ok(Aside::Gone) => Ok(SetAside::Done(None)),
            Ok(Aside::Busy) => Ok(SetAside::Busy(old.name.clone())),
            Ok(Aside::Renamed { target, paused: true }) => Ok(SetAside::Done(Some(format!("the replaced session was paused and saved as {target}")))),
            Ok(Aside::Renamed { target, paused: false }) => Ok(SetAside::Done(Some(format!("the replaced session had stopped and was saved as {target}")))),
            Ok(Aside::StillRuns { target, error }) => Ok(SetAside::Done(Some(format!(
                "the replaced session was saved as {target} but still runs ({error}); stop it in Sessions"
            )))),
            Err(error) => Err(anyhow::anyhow!(
                "the old session {} still holds the member's name and could not be set aside ({error}); pause and rename it (hgs pause, hgs rename), then hgs team relaunch",
                old.name
            )),
        };
    }
    let Ok(record) = crate::state::storage::read(&old.name) else {
        return Ok(SetAside::Done(None));
    };
    // A different run under the old name is not the session the swap replaced.
    if !old.run_id.is_empty() && record["run_id"].as_str().is_some_and(|run| !run.is_empty() && run != old.run_id) {
        return Ok(SetAside::Done(None));
    }
    let alive = crate::state::storage::process_alive(&record);
    let paused = alive && crate::state::lifecycle::pause_one_quiet(&record, None, true).is_ok();
    if alive && !paused {
        return Ok(SetAside::Busy(old.name.clone()));
    }
    let target = (1..=9)
        .map(|n| if n == 1 { format!("{}-replaced", old.name) } else { format!("{}-replaced-{n}", old.name) })
        .find(|name| crate::state::storage::read(name).is_err())
        .context("the old session's -replaced names are all taken; rename or close it in Sessions")?;
    // Owner, 2026-11-11 (team 86821caa): the rename right after the pause can
    // meet the paused session's terminal still closing ("live pane does not
    // match its saved binding"); the member was then left with its old session
    // stopped and no new one. It is tried again for a few seconds first.
    let mut renamed = crate::state::rename::dispatch(&[old.name.clone(), target.clone()], false);
    for _ in 0..6 {
        if renamed.is_ok() || !paused {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
        renamed = crate::state::rename::dispatch(&[old.name.clone(), target.clone()], false);
    }
    renamed.map_err(|error| {
        anyhow::anyhow!("the old session {} still holds the member's name and could not be renamed ({error}); close or rename it in Sessions, then hgs team relaunch", old.name)
    })?;
    Ok(SetAside::Done(Some(if paused {
        format!("the replaced session was paused and saved as {target}")
    } else {
        format!("the replaced session had stopped and was saved as {target}")
    })))
}

#[cfg(test)]
type SetAsideStandIn = fn(&str) -> anyhow::Result<SetAside>;
#[cfg(test)]
thread_local! {
    static TEST_SET_ASIDE: std::cell::Cell<Option<SetAsideStandIn>> = const { std::cell::Cell::new(None) };
}
#[cfg(test)]
fn test_set_aside() -> Option<SetAsideStandIn> {
    TEST_SET_ASIDE.with(std::cell::Cell::get)
}
#[cfg(test)]
pub(crate) fn set_test_set_aside(stand_in: Option<SetAsideStandIn>) {
    TEST_SET_ASIDE.with(|cell| cell.set(stand_in));
}

#[cfg(test)]
mod set_aside_tests {
    use super::*;

    #[test]
    fn only_a_local_replaced_session_is_set_aside() {
        set_test_set_aside(Some(|name| Ok(SetAside::Done(Some(format!("set aside {name}"))))));
        let mut member = Member { id: "dev".into(), ..Default::default() };
        assert_eq!(set_aside_replaced(&member).unwrap(), SetAside::Done(None), "nothing was replaced");
        member.replaced = Some(SessionRef { host: "mac".into(), name: "codex/x/dev".into(), ..Default::default() });
        assert_eq!(set_aside_replaced(&member).unwrap(), SetAside::Done(None), "a remote session is its computer's");
        member.replaced = Some(SessionRef { name: "codex/x/dev".into(), ..Default::default() });
        assert_eq!(set_aside_replaced(&member).unwrap(), SetAside::Done(Some("set aside codex/x/dev".into())));
        set_test_set_aside(None);
    }
}

/// Task 22: how many times a member is started again by itself when its
/// DeepSeek host was still starting. Each start waits for the host (dsh's own
/// bound), so a host that never answers fails once after about four of them.
pub(crate) const HOST_WAITS: u32 = 3;

/// Task 22: the start failed only because the DeepSeek host it started (or
/// another start of it) did not answer yet. The host keeps starting on its
/// own, so the next start finds it up.
pub(crate) fn host_still_starting(error: &str) -> bool {
    error.contains("DeepSeek host is still starting")
}

fn launch(team: &Team, member: &Member) -> anyhow::Result<()> {
    super::super::iron_rules::for_team(team, member)?;
    // Task 115: a Kimi member's read-only rules are the owner's explicit
    // opt-in (`hgs account kimi-read-only`), never written by a launch: Kimi
    // has no per-launch rule override, and editing the config a launch reads
    // would silently change every Kimi session on this computer.
    let folder = member_folder(team, member)?;
    // Task 159: under the owner's scratch rule (still granted), the folder is
    // trusted for the rule's agents before the launch reads its config.
    if !team.scratch_rule.is_empty() && member.computer.is_empty() {
        let rules = super::owner_steps::rules();
        if let Some(rule) = rules.iter().find(|rule| rule.id == team.scratch_rule && super::owner_steps::inside(&folder, &rule.folder)) {
            if super::owner_steps::trusted(rule).contains(member.agent.as_str()) {
                if let Ok(config) = crate::config::Config::load() {
                    if let Err(error) = crate::accounts::trust_folder_for(&config, &member.agent, &member.account, &folder) {
                        eprintln!("hgs team worker: {}: trusting {folder} for {}: {error}", team.name, member.id);
                    }
                }
            }
        }
    }
    // Owner, 2026-11-09 ("вот это должно быть в режиме auto apply"): a team
    // member's own checkout (a worktree or clone Zerus made of the team's
    // folder) is trusted for its agent before the launch, so Claude's folder
    // trust question never stops a team. Only Zerus-made checkouts on this
    // computer: the shared folder and other people's folders keep asking.
    if member.computer.is_empty()
        && matches!(member.folder, super::model::MemberFolder::Worktree | super::model::MemberFolder::Clone)
        && folder.trim() != team.folder.trim()
    {
        if let Ok(config) = crate::config::Config::load() {
            if let Err(error) = crate::accounts::trust_folder_for(&config, &member.agent, &member.account, &folder) {
                eprintln!("hgs team worker: {}: trusting {folder} for {}: {error}", team.name, member.id);
            }
        }
    }
    let args = launch_args(team, member, &folder);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = hgs(&member.computer, &args, None, Duration::from_secs(120))?;
    anyhow::ensure!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr).trim());
    Ok(())
}

/// U54: a member's folder may be left on one of its task branches (a worktree
/// the member tested on). When the folder is clean and that branch is already
/// merged into the member's branch or the lead's, it is switched back to the
/// member's branch; the task branch is kept and the caller records the note.
/// Otherwise the error offers that checkout as the review action - never a
/// reset. A shared folder, a remote member and a detached HEAD are left to the
/// launch.
fn prepare_member_branch(team: &Team, member: &Member) -> anyhow::Result<Option<String>> {
    if !member.computer.is_empty() || member.folder == MemberFolder::Shared || team.folder.is_empty() {
        return Ok(None);
    }
    let folder = member_folder(team, member)?;
    check_folder_branch(team, member, &folder)
}

fn check_folder_branch(team: &Team, member: &Member, folder: &str) -> anyhow::Result<Option<String>> {
    let (_, current) = folder_facts(folder);
    if current.is_empty() || current == "HEAD" {
        return Ok(None);
    }
    let own = format!("team/{}/{}", &team.id[..8.min(team.id.len())], tag(&member.id));
    if current == own {
        return Ok(None);
    }
    let lead = lead_branch(team).unwrap_or_default();
    if !lead.is_empty() && current == lead {
        return Ok(None);
    }
    // Only a task branch this member has worked on is a leftover to undo.
    let task_branch = team.tasks.iter().rev().find_map(|task| {
        let branch = task_branch(team, task.id, &member.id)?;
        (branch == current && task.attempts.iter().any(|attempt| attempt.member == member.id)).then_some(branch)
    });
    if task_branch.is_none() {
        return Ok(None);
    }
    let git = |args: &[&str]| {
        super::super::worktrees::git(Path::new(folder), args, Instant::now() + Duration::from_secs(20))
    };
    let merged_into = |branch: &str| {
        !branch.is_empty() && git(&["merge-base", "--is-ancestor", &current, branch]).is_ok()
    };
    let clean = git(&["status", "--porcelain"]).is_ok_and(|output| output.iter().all(u8::is_ascii_whitespace));
    if !clean {
        anyhow::bail!(
            "{folder} is on {current}, not {own}; it has uncommitted changes, so Zerus did not switch it. \
             Check out {own} in the folder when its work is committed."
        );
    }
    if !merged_into(&own) && !merged_into(&lead) {
        anyhow::bail!(
            "{folder} is on {current}, not {own}; {current} is not merged into {own}. Merge it, then check out {own} in the folder."
        );
    }
    git(&["switch", &own]).map_err(|error| {
        anyhow::anyhow!("{folder} is on {current}, not {own}, and Zerus could not switch it: {}", error.detail)
    })?;
    Ok(Some(format!("the folder was on {current} (merged) and is back on {own}; {current} is kept")))
}

/// One change to a team outside any request: the dispatcher's own record.
#[track_caller]
fn change(store: &mut Store, team: &str, change: impl FnOnce(&mut Team, &mut Vec<Event>) -> anyhow::Result<()>) -> anyhow::Result<()> {
    store.mutate(None, team, None, |team, events| {
        change(team, events)?;
        // U63 (owner, 2026-10-31): a stage that decided a phase's last task
        // here (not through ops::apply) lets the phase wait for the owner's
        // acceptance at once.
        super::ops::settle_acceptance(team, events);
        Ok(Value::Null)
    })?;
    Ok(())
}

/// H1: asks the recovery service to wait for a member's usage-limit reset (or
/// to stop). The service keeps the timing, cancellation and text; Zerus only
/// names the session and its run. N29: the request always names the session the
/// member is bound to now; the one a swap replaced is withdrawn first.
fn set_limit_resume(store: &mut Store, team: &str, member: &str, on: bool) -> anyhow::Result<()> {
    let mut calls = Vec::new();
    change(store, team, |team, _| {
        let found = team.members.iter_mut().find(|found| found.id == member).context("unknown member")?;
        let current = found.session.clone();
        calls = limit_resume_calls(found.limit_session.as_ref(), current.as_ref(), on && current.is_some());
        found.limit_resume = on && current.is_some();
        found.limit_session = if found.limit_resume { current } else { None };
        found.limit_refreshed_at = None;
        Ok(())
    })?;
    for (session, on) in calls {
        if let Err(error) = team_override(&session, on) {
            eprintln!(
                "hgs: the team could not {} automatic recovery for {}: {error}",
                if on { "start" } else { "stop" },
                session.name
            );
        }
    }
    Ok(())
}

/// N29: the recovery calls one limit-resume change makes: the recorded session
/// is withdrawn when it is not the current one, and a new request names the
/// current one.
fn limit_resume_calls(recorded: Option<&SessionRef>, current: Option<&SessionRef>, on: bool) -> Vec<(SessionRef, bool)> {
    let mut calls = Vec::new();
    if let Some(recorded) = recorded {
        // Turning the wait off always withdraws the recorded session; turning it
        // on withdraws it only when a swap replaced it.
        if !on || current != Some(recorded) {
            calls.push((recorded.clone(), false));
        }
    }
    if on {
        if let Some(current) = current {
            calls.push((current.clone(), true));
        }
    }
    calls
}

/// N30: the recovery service ignores a refresh younger than a minute, and a
/// refresh on another computer is an `hgs` call with a 60 s bound; a slow peer
/// would hold the dispatcher for every team. One refresh a minute per member is
/// enough and stays well inside the request's 900 s expiry. The time is
/// recorded before the call, so a peer that hangs is not asked again next tick.
fn refresh_limit_resume(store: &mut Store, team: &Team, member: &str) -> anyhow::Result<()> {
    const REFRESH_SECONDS: f64 = 60.0;
    let Some(found) = team.member(member) else { return Ok(()) };
    let named = found.limit_session.clone().or_else(|| found.session.clone());
    let Some(session) = named else { return Ok(()) };
    if now() - found.limit_refreshed_at.unwrap_or(0.0) < REFRESH_SECONDS {
        return Ok(());
    }
    change(store, &team.id, |team, _| {
        if let Some(found) = team.members.iter_mut().find(|found| found.id == member) {
            found.limit_refreshed_at = Some(now());
        }
        Ok(())
    })?;
    if let Err(error) = team_override(&session, true) {
        eprintln!("hgs: the team could not refresh automatic recovery for {}: {error}", session.name);
    }
    Ok(())
}

/// H1: the recovery service of the session's computer waits for its reset;
/// a member on another computer is asked there (N30).
fn team_override(session: &SessionRef, on: bool) -> anyhow::Result<()> {
    if session.host.is_empty() {
        return super::super::recovery::set_team_override(&session.name, &session.run_id, on).map_err(|error| anyhow::anyhow!("{error}"));
    }
    let input = json!({"name":session.name,"run_id":session.run_id,"on":on});
    let output = hgs(&session.host, &["recovery", "team-override"], Some(&input), Duration::from_secs(60))?;
    anyhow::ensure!(output.status.success(), "{}: {}", session.host, String::from_utf8_lossy(&output.stderr).trim());
    Ok(())
}

/// U49: the end of a drain. The session is paused, never killed: its
/// conversation stays under Saved; a session that is already gone needs no
/// pause. Then the member leaves the roster, as remove-member takes it out.
fn drain_member(store: &mut Store, team: &Team, member: &str, pause: bool) -> anyhow::Result<()> {
    // Read the member fresh: a drain may have been cancelled since the step.
    let current = store.load(&team.id)?.context("Unknown team")?;
    let Some(found) = current.member(member) else { return Ok(()) };
    if !found.draining || found.state == MemberState::Removed {
        return Ok(());
    }
    if pause {
        if let Some(session) = found.session.clone() {
            let output = hgs(&session.host, &["pause", &session.name], None, Duration::from_secs(60))?;
            anyhow::ensure!(
                output.status.success(),
                "The session of {} was not paused: {}",
                current.name_of(member),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
    }
    change(store, &team.id, |team, events| {
        let (role, name) = (team.role_of(member), team.name_of(member));
        anyhow::ensure!(
            team.tasks.iter().all(|task| task.assignee != member || !task_owes(task)),
            "{} still owes a task",
            name
        );
        anyhow::ensure!(team.reports_of(member).is_empty(), "{} still has reports", name);
        let found = team.members.iter_mut().find(|found| found.id == member).context("Unknown member")?;
        anyhow::ensure!(found.draining, "the drain was cancelled");
        found.draining = false;
        found.state = MemberState::Removed;
        events.push(Event::new("member_removed", json!({"by":"Zerus","member":role,"draining":true})));
        Ok(())
    })
}

/// U46: the context action a member asks for before a hand-out. A member's own
/// setting overrides the role snapshot copied when it joined; an old member
/// without a snapshot follows the role's live setting, as it follows the role's
/// prompt text. Empty means none; "none" overrides a role's action with none.
pub(crate) fn context_before_task(team: &Team, member: &Member) -> String {
    let from_role = member
        .prompt_snapshot
        .as_ref()
        .map(|snapshot| snapshot.context_before_task.clone())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            super::catalog::load().ok().and_then(|catalog| {
                let role_name = team.role_of(&member.id);
                catalog
                    .roles
                    .iter()
                    .find(|role| role.id == role_name || role.name == role_name)
                    .map(|role| role.context_before_task.clone())
            })
        })
        .unwrap_or_default();
    let wanted = if member.context_before_task.is_empty() { from_role } else { member.context_before_task.clone() };
    match wanted.as_str() {
        "compact" | "clear" => wanted,
        _ => String::new(),
    }
}

/// Task #7: whether the member's measured context meets its After-a-task
/// criterion: at least the threshold of tokens used, or of the context window.
fn after_task_due(member: &Member, used: Option<f64>, limit: Option<f64>) -> bool {
    let Some(used) = used.filter(|used| *used > 0.0) else { return false };
    if member.after_task_at == 0 {
        return false;
    }
    let at = f64::from(member.after_task_at);
    match member.after_task_on.as_str() {
        "tokens" => used >= at,
        "percent" => limit.is_some_and(|limit| limit > 0.0 && used * 100.0 >= at * limit),
        _ => false,
    }
}

/// Task #7: the context action a member's After-a-task policy asks for now,
/// empty when it keeps its context or the record's native usage is not past the
/// threshold.
fn after_task_action(member: &Member, record: &Value) -> String {
    if !matches!(member.after_task.as_str(), "compact" | "clear") {
        return String::new();
    }
    if after_task_due(member, record["context"]["used"].as_f64(), record["context"]["limit"].as_f64()) {
        member.after_task.clone()
    } else {
        String::new()
    }
}

/// Task #7: the context action before a member's next task. A configured
/// After-a-task policy replaces U46's every-task action and runs only when its
/// threshold says the context is spent; the empty default keeps today's path.
/// The member is free (the planner's gate), so only the usage is measured; a
/// remote session's usage lives on its own computer, so it keeps its context.
pub(crate) fn context_action(team: &Team, member: &Member) -> String {
    if member.after_task.is_empty() {
        return context_before_task(team, member);
    }
    let Some(session) = member.session.as_ref().filter(|session| session.host.is_empty()) else { return String::new() };
    let Ok(record) = crate::state::storage::read(&session.name) else { return String::new() };
    after_task_action(member, &record)
}

/// Task #7: the lead's hand-out action. C1 still hands the lead its own task
/// without a message while it is busy, but its After-a-task policy runs only
/// while the lead's session is free.
pub(crate) fn lead_context_action(team: &Team, lead: &Member) -> String {
    if lead.after_task.is_empty() {
        return context_before_task(team, lead);
    }
    let Some(session) = lead.session.as_ref().filter(|session| session.host.is_empty()) else { return String::new() };
    let Ok(record) = crate::state::storage::read(&session.name) else { return String::new() };
    if !record_free(&record) {
        return String::new();
    }
    after_task_action(lead, &record)
}

/// U46: whether this agent has the native context action. DeepSeek compacts but
/// has no native clear; OpenCode has neither yet.
pub(crate) fn native_context(agent: &str, action: &str) -> bool {
    match action {
        "compact" => matches!(agent, "claude" | "codex" | "kimi" | "dsh"),
        "clear" => matches!(agent, "claude" | "codex" | "kimi"),
        _ => false,
    }
}

pub(crate) fn agent_label(agent: &str) -> &str {
    match agent {
        "claude" => "Claude",
        "codex" => "Codex",
        "kimi" => "Kimi",
        "dsh" => "DeepSeek",
        "opencode" => "OpenCode",
        other => other,
    }
}

/// U46: a native context action that a hand-out started, recorded on the task
/// while its confirmation is pending. The request identity lets a later tick -
/// or a restarted worker - read the same action instead of running it again.
pub(crate) struct ContextRequest {
    pub action: String,
    pub request_id: String,
    pub run_id: String,
    pub conversation: String,
    pub started_at: f64,
}

/// What starting the context action leaves to the hand-out.
pub(crate) enum ContextStart {
    /// The action is submitted; the caller records it and looks again each tick.
    Started(ContextRequest),
    /// It finished at once: the agent has no native action (skipped), or a
    /// clear the command itself confirmed.
    Done { step: PrepareStep, conversation: Option<String> },
    /// Nothing is left to wait for.
    Failed { step: PrepareStep },
}

/// What one look at the session record shows for a started action.
pub(crate) enum ContextCheck {
    Done { conversation: Option<String> },
    Waiting,
    Refused(String),
    Uncertain(String),
}

/// The bound on the native action's confirmation (spec 3).
const CONTEXT_TIMEOUT_SECS: f64 = 60.0;

fn context_step(action: &str, at: f64) -> PrepareStep {
    PrepareStep {
        step: "context".into(),
        member: String::new(),
        from: "none".into(),
        to: action.to_owned(),
        at,
        result: "done".into(),
        reason: String::new(),
    }
}

fn skipped_context(member: &Member, action: &str, at: f64) -> PrepareStep {
    PrepareStep {
        step: "context".into(),
        member: String::new(),
        from: "none".into(),
        to: "none".into(),
        at,
        result: "skipped".into(),
        reason: format!("{} has no {action}; the task continues the conversation", agent_label(&member.agent)),
    }
}

fn context_failure(mut step: PrepareStep, result: &str, reason: String) -> PrepareStep {
    step.result = result.into();
    step.reason = reason;
    step
}

/// U46: starts the context step for a hand-out: `hgs compact-context` or
/// `hgs clear-context` on the idle member session. It returns at once - the
/// caller records the request on the task and checks it each tick - except when
/// there is nothing to wait for (a skipped agent, or a clear the command itself
/// confirmed). A U36 stage hand-out calls this and `check_context` too; the lead
/// wires it when #537 and this land together.
pub(crate) fn run_context(member: &Member, session: &SessionRef, action: &str) -> ContextStart {
    let at = now();
    let step = context_step(action, at);
    if !native_context(&member.agent, action) {
        return ContextStart::Done { step: skipped_context(member, action, at), conversation: None };
    }
    let request_id = uuid::Uuid::new_v4().to_string();
    let input = json!({"request_id":request_id,"expected_run_id":session.run_id,
        "expected_conversation_id":session.conversation_id});
    let args = [format!("{action}-context"), session.name.clone(), "--json".to_owned()];
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = match hgs(&session.host, &args, Some(&input), Duration::from_secs(120)) {
        Ok(output) => output,
        Err(error) => return ContextStart::Failed { step: context_failure(step, "uncertain", error.to_string()) },
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let reason = stderr.lines().next().unwrap_or_default().trim().to_owned();
        let reason = if reason.is_empty() { format!("The {action} was refused") } else { reason };
        return ContextStart::Failed { step: context_failure(step, "refused", reason) };
    }
    let receipt: Value = match serde_json::from_slice(&output.stdout) {
        Ok(receipt) => receipt,
        Err(error) => {
            return ContextStart::Failed { step: context_failure(step, "uncertain", format!("Unexpected {action} receipt: {error}")) }
        }
    };
    let status = receipt["status"].as_str().unwrap_or_default();
    if action == "clear" && status == "confirmed" {
        // The command proved the clear; the record may already name the new
        // conversation, which the member's session follows (spec 3).
        let conversation = inspect_reply(session).ok().and_then(|record| {
            let new = record["conversation_id"].as_str().unwrap_or_default();
            (!new.is_empty() && new != session.conversation_id).then(|| new.to_owned())
        });
        return ContextStart::Done { step, conversation };
    }
    if !matches!(status, "submitted" | "in_progress" | "compacting") {
        let reason = format!("Unexpected {action} receipt: {}", String::from_utf8_lossy(&output.stdout).trim());
        return ContextStart::Failed { step: context_failure(step, "uncertain", reason) };
    }
    ContextStart::Started(ContextRequest {
        action: action.to_owned(),
        request_id,
        run_id: session.run_id.clone(),
        conversation: session.conversation_id.clone(),
        started_at: at,
    })
}

/// U46: one look at the session record for a started action - the compact
/// request by its id, or the cleared conversation - and the 60 s bound. Never
/// sleeps: the tick that called it goes on.
pub(crate) fn check_context(session: &SessionRef, request: &ContextRequest) -> ContextCheck {
    if now() - request.started_at > CONTEXT_TIMEOUT_SECS {
        return ContextCheck::Uncertain(format!(
            "The {} was not confirmed within {} s",
            request.action, CONTEXT_TIMEOUT_SECS as u64
        ));
    }
    let record = match inspect_reply(session) {
        Ok(record) => record,
        // A transient read failure is not a refusal; the bound ends the wait.
        Err(_) => return ContextCheck::Waiting,
    };
    if record["run_id"].as_str().unwrap_or_default() != request.run_id {
        return ContextCheck::Uncertain("The member's session changed while preparing the hand-out".into());
    }
    match request.action.as_str() {
        "compact" => {
            let found = &record["compact_context_request"];
            if found["request_id"].as_str() != Some(request.request_id.as_str()) {
                return ContextCheck::Waiting;
            }
            match found["status"].as_str().unwrap_or_default() {
                "completed" | "unchanged" => {
                    // A compaction can follow a new transcript; the hand-out's
                    // session must name it before the send.
                    let new = record["conversation_id"].as_str().unwrap_or_default();
                    let conversation = (!new.is_empty() && new != session.conversation_id).then(|| new.to_owned());
                    ContextCheck::Done { conversation }
                }
                "cancelled" => ContextCheck::Refused("The compaction was cancelled before it finished".into()),
                "failed" => ContextCheck::Refused("The compaction failed".into()),
                _ => ContextCheck::Waiting,
            }
        }
        "clear" => {
            let new = record["conversation_id"].as_str().unwrap_or_default();
            if !new.is_empty() && new != request.conversation {
                ContextCheck::Done { conversation: Some(new.to_owned()) }
            } else {
                ContextCheck::Waiting
            }
        }
        _ => ContextCheck::Refused("Unknown context action".into()),
    }
}

/// Task #2 (team ede724c4): how often a live member's last event time alone is
/// written (seconds); a phase or activity change is written at once.
const WATCH_EVENT_EVERY: f64 = 300.0;

/// Task 209: how often an active member's native context is read (seconds),
/// how often one that holds no open attempt is, and how many inspects one tick
/// may run so a slow agent never holds the tick.
const CONTEXT_EVERY: f64 = 30.0;
const CONTEXT_IDLE_EVERY: f64 = 120.0;
const CONTEXT_READS_PER_TICK: usize = 4;
/// Task 209: a compaction that refused or stayed unconfirmed is tried again
/// after this long, never on every tick.
const CONTEXT_RETRY_SECS: f64 = 600.0;
/// Task #2 (team ede724c4): a reading is written to the team only when it
/// moves the figure status shows by this many points (or crosses the team's
/// compaction threshold, or names another window). Each write raises the
/// team revision, and every desktop that follows the team re-reads it.
const CONTEXT_STEP: u8 = 2;
/// Task #2 (team ede724c4): an unchanged reading is still written after this
/// long, so the stored one stays fresh for a claim (`CLAIM_FRESH`, 600 s) and
/// the shown value is never older than this plus one read interval.
const CONTEXT_REFRESH: f64 = super::context::CLAIM_FRESH / 2.0;
/// Task 209: how long a handoff brief waits for its first refused send.
const HANDOFF_RETRY_SECS: f64 = 5.0;

/// Task 209: a member's native context reading, from its session's own
/// `hgs inspect` (the listing carries no meter). None when the agent reports
/// none or the read failed; the last reading stays on the member then. `at`
/// is the reading's time: the tick's own (task #2), or now.
fn read_context(member: &Member, at: f64) -> Option<MemberContext> {
    let session = member.session.as_ref()?;
    let output = hgs(&session.host, &["inspect", &session.name, "--json"], None, Duration::from_secs(10)).ok()?;
    if !output.status.success() {
        return None;
    }
    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
    let (used, limit) = super::context::reading(&value)?;
    Some(MemberContext::of(used, limit, at))
}

/// Task #2 (team ede724c4): whether a new reading is worth a write over the
/// stored one: the shown figure moved by `CONTEXT_STEP` points, the reading
/// crossed the compaction `threshold`, the window changed, or the stored
/// reading is `CONTEXT_REFRESH` old. A reading that only carries a new time
/// is not.
pub(crate) fn context_moved(stored: Option<&MemberContext>, new: &MemberContext, threshold: u8) -> bool {
    let Some(stored) = stored else { return true };
    stored.limit != new.limit
        || stored.percent.abs_diff(new.percent) >= CONTEXT_STEP
        || stored.reached(threshold) != new.reached(threshold)
        || new.at - stored.at >= CONTEXT_REFRESH
}

/// Task 209: keeps the reading with the team, so status and the member card
/// show the percentage without a read of their own. Task #2 (team ede724c4):
/// only a reading that moved (`context_moved`, against `team` as read) is
/// written; true when it was.
fn record_context(store: &mut Store, team: &Team, member_id: &str, context: MemberContext) -> anyhow::Result<bool> {
    let stored = team.member(member_id).and_then(|member| member.context);
    if !context_moved(stored.as_ref(), &context, team.settings.context_compact_percent) {
        return Ok(false);
    }
    change(store, &team.id, |team, _| {
        if let Some(found) = team.members.iter_mut().find(|member| member.id == member_id) {
            found.context = Some(context);
        }
        Ok(())
    })?;
    Ok(true)
}

/// Task #2 (team ede724c4): when the worker last read each member's context
/// (team, member), written or not: the next read waits for the interval from
/// whichever is newer, this or the stored reading's time, so a reading that
/// was not written is not read again on every tick.
static CONTEXT_READ_AT: std::sync::Mutex<std::collections::BTreeMap<(String, String), f64>> = std::sync::Mutex::new(std::collections::BTreeMap::new());

fn context_read_at(team: &str, member: &str) -> Option<f64> {
    CONTEXT_READ_AT.lock().ok()?.get(&(team.to_owned(), member.to_owned())).copied()
}

fn note_context_read(team: &str, member: &str, at: f64) {
    if let Ok(mut reads) = CONTEXT_READ_AT.lock() {
        // Bounded by the members the worker reads; a removed one is dropped
        // when the map grows past what a busy machine has.
        if reads.len() > 4096 {
            reads.clear();
        }
        reads.insert((team.to_owned(), member.to_owned()), at);
    }
}

/// Task 209: the task whose open attempt belongs to `member`, if any.
fn open_attempt_task<'a>(team: &'a Team, member: &str) -> Option<&'a Task> {
    team.tasks.iter().find(|task| task.open_attempt().is_some_and(|attempt| attempt.member == member))
}

/// Task 209: whether the member's context is at the team's threshold (or a
/// compaction of it is being confirmed): the finished turn's report gate waits,
/// because the compaction is what continues the attempt.
fn context_due(team: &Team, member: &str, at: f64) -> bool {
    let Some(found) = team.member(member) else { return false };
    if found.context_compact.is_some() {
        return true;
    }
    if found.context_retry_at.is_some_and(|retry| retry > at) {
        return false;
    }
    found.context.is_some_and(|context| context.fresh(at) && context.reached(team.settings.context_compact_percent))
}

/// Task 209: the step a finished turn becomes. A turn that says it has no
/// context left while a fresh meter says otherwise is answered with the figure
/// once; the attempt stays open. Without a reading the claim is not refused.
fn no_report_or_claim(team: &Team, task: &Task, at: f64) -> Step {
    let Some(attempt) = task.attempts.last() else { return Step::NoReport { task: task.id } };
    if attempt.context_answered_at.is_some() || !super::context::claims_exhausted(&attempt.last_reply) {
        return Step::NoReport { task: task.id };
    }
    let fresh = team
        .member(&attempt.member)
        .and_then(|member| member.context)
        .is_some_and(|context| context.fresh(at));
    if fresh {
        Step::AnswerContextClaim { task: task.id }
    } else {
        Step::NoReport { task: task.id }
    }
}


/// Task 209 and 254: the context action due before a member works, and
/// whether only its cold cache asks for it: at the team's threshold (95%), or
/// with a cold prompt cache from `cold_compact_percent` (30%) up, since a cold
/// session re-reads its whole context at full price before anything else.
fn due_context(team: &Team, member: &Member) -> (String, bool) {
    let Some(context) = member.context.filter(|context| context.fresh(now())) else { return (String::new(), false) };
    if member.context_retry_at.is_some_and(|retry| retry > now()) {
        return (String::new(), false);
    }
    let threshold = context.reached(team.settings.context_compact_percent);
    let cold = !threshold && cold_large(team, member);
    if !threshold && !cold {
        return (String::new(), false);
    }
    let action = if native_context(&member.agent, "compact") { "compact" } else { "fresh" };
    (action.into(), cold)
}

/// Task 254: a cold prompt cache under a context of at least the team's
/// `cold_compact_percent` (0: never) of the window, by a fresh reading.
fn cold_large(team: &Team, member: &Member) -> bool {
    let percent = team.settings.cold_compact_percent;
    member.seen_cold
        && percent > 0
        && member.context.is_some_and(|context| context.fresh(now()) && context.reached(percent))
        && !member.context_retry_at.is_some_and(|retry| retry > now())
}

/// Task 254 (owner, 2026-11-08): why a crashed attempt is not given back now:
/// its team or its phase is paused, its member is paused or draining, or the
/// task is held. The next tick after the pause gives it back.
pub(crate) fn crash_held(team: &Team, task: &Task, member: &Member) -> Option<&'static str> {
    if team.status != TeamStatus::Running {
        return Some("the team is not running");
    }
    if super::plan::phase_pause(team, task.phase).is_some() {
        return Some("its phase is paused");
    }
    if member.state == MemberState::Paused {
        return Some("the member is paused");
    }
    if member.draining {
        return Some("the member is draining");
    }
    if task.hold.is_some() {
        return Some("the task is held");
    }
    None
}

/// Task #3 (team ede724c4): the context reads in flight, (team, member), and
/// the finished ones waiting for the next sampling to record them. Each read
/// is one `hgs inspect` (up to 10 s), so the tick starts them on threads of
/// their own and never waits for one.
static CONTEXT_IN_FLIGHT: std::sync::Mutex<std::collections::BTreeSet<(String, String)>> = std::sync::Mutex::new(std::collections::BTreeSet::new());
type FinishedRead = (String, String, f64, Option<MemberContext>, Instant);
static CONTEXT_DONE: std::sync::Mutex<Vec<FinishedRead>> = std::sync::Mutex::new(Vec::new());

/// A test's thread-safe stand-in for a member's context read, by team id: the
/// read then runs on a thread as in the worker. Without one a test reads
/// inline through its thread's fake `hgs`.
#[cfg(test)]
pub(crate) type ContextReader = std::sync::Arc<dyn Fn(&Member, f64) -> Option<MemberContext> + Send + Sync>;
#[cfg(test)]
static TEST_CONTEXT_READERS: std::sync::Mutex<std::collections::BTreeMap<String, ContextReader>> = std::sync::Mutex::new(std::collections::BTreeMap::new());
#[cfg(test)]
pub(crate) fn set_context_reader(team: &str, reader: Option<ContextReader>) {
    let mut readers = TEST_CONTEXT_READERS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match reader {
        Some(reader) => readers.insert(team.to_owned(), reader),
        None => readers.remove(team),
    };
}

fn finish_context_read(team: String, member: String, at: f64, context: Option<MemberContext>) {
    CONTEXT_DONE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((team.clone(), member.clone(), at, context, Instant::now()));
    CONTEXT_IN_FLIGHT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&(team, member));
}

/// Starts one member's context read; false when none could start (one is in
/// flight already, or no thread could be made).
fn start_context_read(team: &str, member: &Member, at: f64) -> bool {
    let key = (team.to_owned(), member.id.clone());
    if !CONTEXT_IN_FLIGHT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key.clone()) {
        return false;
    }
    let (team_id, member_id, found) = (key.0.clone(), key.1.clone(), member.clone());
    #[cfg(test)]
    {
        let reader = TEST_CONTEXT_READERS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(team).cloned();
        let Some(reader) = reader else {
            // Inline, through this thread's fake hgs.
            finish_context_read(team_id, member_id, at, read_context(&found, at));
            return true;
        };
        let spawned = std::thread::Builder::new().name("hgs-team-context".into()).spawn(move || {
            let context = reader(&found, at);
            finish_context_read(team_id, member_id, at, context);
        });
        if spawned.is_err() {
            CONTEXT_IN_FLIGHT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&key);
            return false;
        }
        true
    }
    #[cfg(not(test))]
    {
        let spawned = std::thread::Builder::new().name("hgs-team-context".into()).spawn(move || {
            let context = read_context(&found, at);
            finish_context_read(team_id, member_id, at, context);
        });
        if spawned.is_err() {
            CONTEXT_IN_FLIGHT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&key);
            return false;
        }
        true
    }
}

fn context_in_flight(team: &str, member: &str) -> bool {
    CONTEXT_IN_FLIGHT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(&(team.to_owned(), member.to_owned()))
}

/// The reads in flight for these teams' members (the worker's tick samples
/// every team it runs, so this is the worker's whole count).
fn contexts_in_flight(teams: &[Team]) -> usize {
    let ids: std::collections::BTreeSet<&str> = teams.iter().map(|team| team.id.as_str()).collect();
    CONTEXT_IN_FLIGHT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().filter(|(team, _)| ids.contains(team.as_str())).count()
}

/// Records the finished reads of `teams`' members against each team as
/// stored now; a failed read records nothing and is tried again next tick. A
/// reading for a team no longer ticked is dropped ten minutes after it
/// finished, so the list stays bounded.
fn apply_context_reads(store: &mut Store, teams: &[Team]) {
    let ids: std::collections::BTreeSet<&str> = teams.iter().map(|team| team.id.as_str()).collect();
    let finished: Vec<FinishedRead> = {
        let mut done = CONTEXT_DONE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mine, mut others): (Vec<_>, Vec<_>) = done.drain(..).partition(|(team, ..)| ids.contains(team.as_str()));
        others.retain(|(.., finished)| finished.elapsed() < Duration::from_secs(600));
        *done = others;
        mine
    };
    for (team_id, member_id, at, context, _) in finished {
        let Some(context) = context else { continue };
        note_context_read(&team_id, &member_id, at);
        let Ok(Some(team)) = store.load(&team_id) else { continue };
        if let Err(error) = record_context(store, &team, &member_id, context) {
            eprintln!("hgs team worker: {}: context: {error}", team.name);
        }
    }
}

/// Task 209: reads how many tokens a member's session holds and updates the
/// member's reading. Called by the tick only; a hand-out or a report uses the
/// reading already collected, never a second inspect. Task #3 (team
/// ede724c4): the reads run on threads of their own, at most
/// CONTEXT_READS_PER_TICK at a time; the readings finished since the last
/// sampling are recorded first, so the tick never waits for an inspect.
fn sample_contexts(store: &mut Store, teams: &[Team], seen: &[Seen], at: f64) {
    apply_context_reads(store, teams);
    'teams: for team in teams {
        for member in team.members.iter().filter(|member| member.state != MemberState::Removed) {
            if contexts_in_flight(teams) >= CONTEXT_READS_PER_TICK {
                break 'teams;
            }
            let Some(session) = member.session.as_ref() else { continue };
            if member.context_compact.is_some() || context_in_flight(&team.id, &member.id) {
                continue;
            }
            let open = open_attempt_task(team, &member.id).is_some();
            let every = if open { CONTEXT_EVERY } else { CONTEXT_IDLE_EVERY };
            let last = member.context.map(|context| context.at).into_iter().chain(context_read_at(&team.id, &member.id)).reduce(f64::max);
            if last.is_some_and(|last| at - last < every) {
                continue;
            }
            // A session the listing did not answer is not an idle one: its
            // reading waits for the computer to answer.
            if !seen.iter().any(|found| found.host == session.host && found.name == session.name) {
                continue;
            }
            start_context_read(&team.id, member, at);
        }
    }
    // A read that finished at once (a test's inline read) is recorded now.
    apply_context_reads(store, teams);
}

/// Task 209: starts the context action for an idle member whose meter reached
/// the threshold. A native compaction is submitted and confirmed on later
/// ticks; an agent without one gets a fresh conversation with the handoff
/// brief. Either way the member's open attempt continues.
fn start_member_compact(store: &mut Store, team_id: &str, member_id: &str, reason: &str) -> anyhow::Result<()> {
    let team = store.load(team_id)?.context("Unknown team")?;
    let Some(member) = team.member(member_id).cloned() else { return Ok(()) };
    if member.context_compact.is_some() || member.context_retry_at.is_some_and(|retry| retry > now()) {
        return Ok(());
    }
    let Some(session) = member.session.clone() else { return Ok(()) };
    let task = open_attempt_task(&team, member_id);
    let (task_id, attempt_id) = task
        .map(|task| (task.id, task.open_attempt().map(|attempt| attempt.id.clone()).unwrap_or_default()))
        .unwrap_or((0, String::new()));
    if !native_context(&member.agent, "compact") {
        change(store, team_id, |team, events| {
            let role = team.role_of(member_id);
            super::ops::fresh_session(team, member_id, "context", events)?;
            events.push(Event::new("context_compact_fresh", json!({"member":role,"task":task_id,"reason":reason})));
            Ok(())
        })?;
        return Ok(());
    }
    match run_context(&member, &session, "compact") {
        ContextStart::Started(request) => change(store, team_id, |team, events| {
            let role = team.role_of(member_id);
            let found = team.members.iter_mut().find(|found| found.id == member_id).context("Unknown member")?;
            found.context_compact = Some(MemberCompact {
                action: "compact".into(),
                request_id: request.request_id,
                run_id: request.run_id,
                conversation: request.conversation,
                started_at: request.started_at,
                task: task_id,
                attempt: attempt_id.clone(),
                reason: reason.to_owned(),
            });
            events.push(Event::new(
                "context_compact_started",
                json!({"member":role,"task":task_id,"attempt":attempt_id,"reason":reason}),
            ));
            Ok(())
        }),
        // A native action that needed nothing (an agent without one, or a clear
        // the command itself confirmed): the member is told to go on.
        ContextStart::Done { step: _, conversation } => {
            if let Some(conversation) = conversation {
                change(store, team_id, |team, _| {
                    if let Some(found) = team.members.iter_mut().find(|member| member.id == member_id).and_then(|member| member.session.as_mut()) {
                        if found.run_id == session.run_id {
                            found.conversation_id = conversation;
                        }
                    }
                    Ok(())
                })?;
            }
            finish_member_compact(store, team_id, member_id, reason)
        }
        ContextStart::Failed { step } => change(store, team_id, |team, events| {
            let role = team.role_of(member_id);
            if let Some(found) = team.members.iter_mut().find(|found| found.id == member_id) {
                found.context_retry_at = Some(now() + CONTEXT_RETRY_SECS);
            }
            events.push(Event::new(
                "context_compact_failed",
                json!({"member":role,"task":task_id,"result":step.result,"reason":step.reason}),
            ));
            Ok(())
        }),
    }
}

/// Task 209: one tick's look at a started compaction. Confirmed: the member is
/// told to continue the same attempt with the recall brief. Refused or past its
/// bound: the member keeps working, the reading stays, and a retry waits.
fn check_member_compact(store: &mut Store, team_id: &str, member_id: &str) -> anyhow::Result<()> {
    let team = store.load(team_id)?.context("Unknown team")?;
    let Some(member) = team.member(member_id).cloned() else { return Ok(()) };
    let Some(pending) = member.context_compact.clone() else { return Ok(()) };
    let reason = pending.reason.clone();
    let Some(session) = member.session.clone() else {
        return change(store, team_id, |team, events| {
            let role = team.role_of(member_id);
            if let Some(found) = team.members.iter_mut().find(|found| found.id == member_id) {
                found.context_compact = None;
            }
            events.push(Event::new("context_compact_failed", json!({"member":role,"reason":"the member's session is gone"})));
            Ok(())
        });
    };
    if pending.request_id.is_empty() {
        // A fresh conversation, not a native compaction: its brief is sent when
        // the new session is up.
        return Ok(());
    }
    let request = ContextRequest {
        action: "compact".into(),
        request_id: pending.request_id,
        run_id: pending.run_id,
        conversation: pending.conversation,
        started_at: pending.started_at,
    };
    match check_context(&session, &request) {
        ContextCheck::Waiting => Ok(()),
        ContextCheck::Done { conversation } => {
            if let Some(conversation) = conversation {
                change(store, team_id, |team, _| {
                    if let Some(found) = team.members.iter_mut().find(|member| member.id == member_id).and_then(|member| member.session.as_mut()) {
                        found.conversation_id = conversation;
                    }
                    Ok(())
                })?;
            }
            finish_member_compact(store, team_id, member_id, &reason)
        }
        ContextCheck::Refused(reason) | ContextCheck::Uncertain(reason) => change(store, team_id, |team, events| {
            let role = team.role_of(member_id);
            if let Some(found) = team.members.iter_mut().find(|found| found.id == member_id) {
                found.context_compact = None;
                found.context_retry_at = Some(now() + CONTEXT_RETRY_SECS);
            }
            events.push(Event::new("context_compact_failed", json!({"member":role,"reason":reason})));
            Ok(())
        }),
    }
}

/// Task 209: the confirmed compaction's recall brief and the instruction to go
/// on with the same attempt, as one working-tier message.
fn finish_member_compact(store: &mut Store, team_id: &str, member_id: &str, reason: &str) -> anyhow::Result<()> {
    let team = store.load(team_id)?.context("Unknown team")?;
    let Some(member) = team.member(member_id).cloned() else { return Ok(()) };
    let Some(session) = member.session.clone() else { return Ok(()) };
    let task = open_attempt_task(&team, member_id).cloned();
    let recent = recent_events(store, team_id);
    let pulses = handoff::pulse_lines(&team, member_id, &recent, now());
    let mut built = handoff::build(&team, member_id, handoff::Why::Compaction, &pulses, handoff::RECALL_UNITS);
    // Task 254: an attempt a crash left unconfirmed goes back by its own
    // hand-out on the next tick, not by "continue" here.
    let task = task.filter(|task| task.open_attempt().is_some_and(|attempt| attempt.state != AttemptState::Unconfirmed));
    if let Some(task) = &task {
        built.text.push_str(&format!(
            "\nContinue task #{} (attempt {}): the compaction changed nothing else; work from the files and report with hgs team report.\n",
            task.id,
            task.attempts.last().map(|attempt| attempt.n).unwrap_or(0)
        ));
    }
    let request_id = uuid::Uuid::new_v4().to_string();
    let outcome = super::doorbell::post_and_ring(
        store, team_id, member_id, &session, &request_id, "context", "Zerus", Urgency::Normal, &built.text,
    );
    change(store, team_id, |team, events| {
        let role = team.role_of(member_id);
        if let Some(found) = team.members.iter_mut().find(|found| found.id == member_id) {
            found.context_compact = None;
            // Read the window again after the compaction.
            found.context = None;
            found.context_retry_at = None;
        }
        match &outcome {
            Sent::Delivered | Sent::Uncertain(_) => events.push(Event::new(
                "context_compacted",
                json!({"member":role,"task":task.as_ref().map(|task| task.id).unwrap_or(0),
                    "attempt":task.as_ref().and_then(|task| task.attempts.last()).map(|attempt| attempt.n).unwrap_or(0),
                    "reason":reason,"units":built.text.encode_utf16().count()}),
            )),
            Sent::Refused(error) => events.push(Event::new("context_compacted_unsent", json!({"member":role,"error":error}))),
        }
        Ok(())
    })
}

/// Task 209/135: sends a fresh session's handoff brief as its first message and
/// continues the member's open attempt under the same number.
fn send_handoff_now(store: &mut Store, team_id: &str, member_id: &str) -> anyhow::Result<()> {
    let team = store.load(team_id)?.context("Unknown team")?;
    let Some(member) = team.member(member_id).cloned() else { return Ok(()) };
    let Some(need) = member.handoff.clone() else { return Ok(()) };
    let Some(session) = member.session.clone() else { return Ok(()) };
    let task = open_attempt_task(&team, member_id).cloned();
    let recent = recent_events(store, team_id);
    let pulses = handoff::pulse_lines(&team, member_id, &recent, now());
    let why = if need.why == "relaunch" { handoff::Why::Relaunch(&need) } else { handoff::Why::Swap(&need) };
    let mut built = handoff::build(&team, member_id, why, &pulses, handoff::BRIEF_UNITS);
    if let Some(task) = &task {
        built.text.push_str(&format!(
            "\nContinue task #{} (attempt {}): your previous session stopped mid-task. Work from the files; do not restart it; report with hgs team report.\n",
            task.id,
            task.attempts.last().map(|attempt| attempt.n).unwrap_or(0)
        ));
    }
    let request_id = uuid::Uuid::new_v4().to_string();
    let outcome = super::doorbell::post_and_ring(
        store, team_id, member_id, &session, &request_id, "handoff", "Zerus", Urgency::Normal, &built.text,
    );
    change(store, team_id, |team, events| {
        match &outcome {
            Sent::Delivered | Sent::Uncertain(_) => handoff_sent(team, events, member_id, &built, &need.why),
            Sent::Refused(error) => {
                // Keep the brief for a later attempt, but not on the next tick.
                if let Some(found) = team.members.iter_mut().find(|found| found.id == member_id) {
                    if let Some(need) = found.handoff.as_mut() {
                        need.at = now();
                    }
                }
                events.push(Event::new("handoff_unsent", json!({"member":team.role_of(member_id),"why":need.why,"error":error})));
            }
        }
        Ok(())
    })
}

/// Task 209: answers a reply that claims there is no context left while the
/// meter says otherwise. Below the threshold the member hears the actual
/// figure and the attempt stays open; at or above it the compaction starts.
/// Without a reading the claim cannot be disproved and No report stands.
fn answer_context_claim(store: &mut Store, team_id: &str, task_id: u32) -> anyhow::Result<()> {
    let team = store.load(team_id)?.context("Unknown team")?;
    let Some(task) = team.task(task_id) else { return Ok(()) };
    let Some(attempt) = task.attempts.last() else { return Ok(()) };
    if attempt.state != AttemptState::Delivered || attempt.context_answered_at.is_some() {
        return Ok(());
    }
    let Some(member) = team.member(&attempt.member) else { return Ok(()) };
    let Some(context) = member.context.filter(|context| context.fresh(now())) else { return Ok(()) };
    let threshold = team.settings.context_compact_percent;
    if context.reached(threshold) {
        return start_member_compact(store, team_id, &member.id, "claim");
    }
    let Some(session) = member.session.clone() else { return Ok(()) };
    let text = format!(
        "[Zerus team \u{b7} from: Zerus \u{b7} context]\nYour context is at {}; the team's threshold is {threshold}%, so there is no need to stop. \
         Continue task #{} (attempt {}) and report it with hgs team report. If you see something else, say exactly what you see: hgs team ask.",
        super::context::meter(context.percent, context.used, context.limit),
        task_id,
        attempt.n
    );
    let request_id = uuid::Uuid::new_v4().to_string();
    let outcome = super::doorbell::post_and_ring(
        store, team_id, &member.id, &session, &request_id, "context", "Zerus", Urgency::Normal, &text,
    );
    change(store, team_id, |team, events| {
        let role = team.role_of(&member.id);
        let n = attempt.n;
        let answered = matches!(&outcome, Sent::Delivered | Sent::Uncertain(_));
        if let Some(found) = team.task_mut(task_id).and_then(|task| task.attempts.iter_mut().find(|found| found.id == attempt.id)) {
            found.context_answered_at = Some(now());
        }
        events.push(Event::new(
            "context_claim_refused",
            json!({"member":role,"task":task_id,"attempt":n,"percent":context.percent,
                "used":context.used,"limit":context.limit,"answered":answered}),
        ));
        Ok(())
    })
}

/// Task 209: a report that claims there is no context left while the member's
/// meter says otherwise is not a report. Returns the answer for the member: the
/// attempt stays open and Zerus compacts when the meter reached the threshold.
/// None when the text is no claim or no reading can be taken (an unverifiable
/// claim is not refused).
pub(crate) fn report_context_answer(store: &mut Store, team_id: &str, input: &Value) -> anyhow::Result<Option<String>> {
    let text = input["text"].as_str().unwrap_or_default();
    if !super::context::claims_exhausted(text) {
        return Ok(None);
    }
    let Some(team) = store.load(team_id)? else { return Ok(None) };
    let Some(task) = team.task(input["task"].as_u64().unwrap_or_default() as u32) else { return Ok(None) };
    let attempt_id = input["attempt"].as_str().unwrap_or_default();
    let Some(attempt) = task
        .attempts
        .iter()
        .find(|attempt| attempt.state.open() && (attempt_id.is_empty() || attempt.id == attempt_id))
    else {
        return Ok(None);
    };
    let Some(member) = team.member(&attempt.member) else { return Ok(None) };
    let Some(context) = read_context(member, now()).or_else(|| member.context.filter(|context| context.fresh(now()))) else { return Ok(None) };
    record_context(store, &team, &member.id, context)?;
    let threshold = team.settings.context_compact_percent;
    if context.reached(threshold) {
        change(store, team_id, |team, events| {
            let role = team.role_of(&member.id);
            events.push(Event::new(
                "context_compact_requested",
                json!({"member":role,"task":task.id,"attempt":attempt.n,"reason":"claim",
                    "percent":context.percent,"used":context.used,"limit":context.limit}),
            ));
            Ok(())
        })?;
        return Ok(Some(format!(
            "Your report was not accepted: your context is at {} and Zerus is compacting it. Continue task #{} (attempt {}) and report with hgs team report.",
            super::context::meter(context.percent, context.used, context.limit),
            task.id,
            attempt.n
        )));
    }
    change(store, team_id, |team, events| {
        let role = team.role_of(&member.id);
        events.push(Event::new(
            "context_claim_refused",
            json!({"member":role,"task":task.id,"attempt":attempt.n,"answered":false,
                "percent":context.percent,"used":context.used,"limit":context.limit}),
        ));
        Ok(())
    })?;
    Ok(Some(format!(
        "Your report was not accepted: your context is at {}, below the team's {threshold}% threshold, so there is no need to stop. \
         Continue task #{} (attempt {}) and report it with hgs team report.",
        super::context::meter(context.percent, context.used, context.limit),
        task.id,
        attempt.n
    )))
}

/// U46: what a hand-out leaves for itself: its prepare steps, the conversation
/// a clear produced, and whether it resumes a wait already recorded on the task.
#[derive(Clone, Default)]
struct Preparation {
    steps: Vec<PrepareStep>,
    conversation: Option<(String, String)>,
    resume: bool,
}

/// Task #6: what the clear before a changed start prompt leaves to its send.
enum ClearAhead {
    Done,
    Waiting,
    Failed(String),
}

/// Task #6: runs, or reads, the clear a changed start prompt asked for. A new
/// conversation the clear names is kept on `session`, so the message that
/// follows keeps the member.
fn clear_ahead(store: &mut Store, team: &Team, member: &Member, session: &mut SessionRef, wanted: &PromptChange) -> anyhow::Result<ClearAhead> {
    if !wanted.clear_context {
        return Ok(ClearAhead::Done);
    }
    let Some(clear) = wanted.clear.clone() else {
        return Ok(match run_context(member, session, "clear") {
            ContextStart::Started(request) => {
                change(store, &team.id, |team, _| {
                    if let Some(found) = team.members.iter_mut().find(|found| found.id == member.id) {
                        if let Some(prompt_change) = found.prompt_change.as_mut() {
                            prompt_change.clear = Some(PromptClear {
                                request_id: request.request_id,
                                run_id: request.run_id,
                                conversation_id: request.conversation,
                                started_at: request.started_at,
                            });
                        }
                    }
                    Ok(())
                })?;
                ClearAhead::Waiting
            }
            ContextStart::Done { conversation, .. } => {
                if let Some(new) = conversation {
                    session.conversation_id = new;
                }
                ClearAhead::Done
            }
            ContextStart::Failed { step } => ClearAhead::Failed(step.reason),
        });
    };
    let request = ContextRequest {
        action: "clear".into(),
        request_id: clear.request_id,
        run_id: clear.run_id,
        conversation: clear.conversation_id,
        started_at: clear.started_at,
    };
    match check_context(session, &request) {
        ContextCheck::Waiting => Ok(ClearAhead::Waiting),
        ContextCheck::Done { conversation } => {
            if let Some(new) = conversation {
                session.conversation_id = new;
            }
            Ok(ClearAhead::Done)
        }
        ContextCheck::Refused(reason) | ContextCheck::Uncertain(reason) => Ok(ClearAhead::Failed(reason)),
    }
}

/// Task #6: what the send of a changed start prompt did.
enum PromptDone {
    Replaced,
    Failed(String),
    Uncertain(String),
}

/// Task #6: whether `member` still holds the prompt change `wanted` describes:
/// the same prompt and the same clear. A newer edit makes this send its own.
fn same_prompt_change(member: &Member, wanted: &PromptChange, prompt: &str) -> bool {
    member.prompt.trim() == prompt.trim()
        && member.prompt_change.as_ref().is_some_and(|stored| stored.clear == wanted.clear && stored.clear_context == wanted.clear_context)
}

/// Task #6: sends a member's changed start prompt as the message that replaces
/// its earlier instructions, after the clear the change asked for.
fn replace_prompt(store: &mut Store, team: &Team, member_id: &str) -> anyhow::Result<()> {
    let Some(member) = team.member(member_id) else { return Ok(()) };
    let Some(wanted) = member.prompt_change.clone() else { return Ok(()) };
    let prompt = member.prompt.clone();
    let text = replace_prompt_text(team, member);
    // A send attempted by an earlier tick (a crash during it) is never replayed
    // under a new id: the message may have arrived (U45's rule).
    if wanted.sent_at.is_some() {
        return finish_prompt_change(store, team, member_id, None, &wanted, &prompt, PromptDone::Uncertain("the send was interrupted; it was not replayed".into()));
    }
    let Some(mut session) = member.session.clone() else {
        return finish_prompt_change(store, team, member_id, None, &wanted, &prompt, PromptDone::Failed("the member has no session".into()));
    };
    match clear_ahead(store, team, member, &mut session, &wanted)? {
        ClearAhead::Waiting => return Ok(()),
        ClearAhead::Failed(reason) => return finish_prompt_change(store, team, member_id, None, &wanted, &prompt, PromptDone::Failed(reason)),
        ClearAhead::Done => {}
    }
    // The send is recorded before the message leaves, so a crash during it
    // leaves it uncertain instead of sending it again. A newer edit that
    // replaced the change meanwhile makes this send stale: nothing goes out and
    // the next tick sends the current prompt.
    let ours = std::cell::Cell::new(false);
    change(store, &team.id, |team, _| {
        if let Some(found) = team.members.iter_mut().find(|member| member.id == member_id) {
            let same = same_prompt_change(found, &wanted, &prompt);
            ours.set(same);
            if let Some(stored) = found.session.as_mut().filter(|stored| stored.run_id == session.run_id) {
                stored.conversation_id = session.conversation_id.clone();
            }
            if same {
                if let Some(prompt_change) = found.prompt_change.as_mut() {
                    prompt_change.sent_at = Some(now());
                }
            }
        }
        Ok(())
    })?;
    if !ours.get() {
        return Ok(());
    }
    let done = match send(&session, &uuid::Uuid::new_v4().to_string(), &text) {
        Sent::Delivered => PromptDone::Replaced,
        // A refusal or an uncertain send is not repeated; the next hand-out
        // carries the new prompt anyway, and the feed records what happened.
        Sent::Refused(reason) => PromptDone::Failed(reason),
        Sent::Uncertain(reason) => PromptDone::Uncertain(reason),
    };
    finish_prompt_change(store, team, member_id, Some(&session), &wanted, &prompt, done)
}

/// Task #6: the changed start prompt is done: the clear's new conversation is
/// kept, this change leaves the member and its outcome reaches the feed. A newer
/// prompt or clear that arrived while the send ran is not this change and stays
/// for a later tick.
fn finish_prompt_change(
    store: &mut Store,
    team: &Team,
    member_id: &str,
    session: Option<&SessionRef>,
    wanted: &PromptChange,
    prompt: &str,
    done: PromptDone,
) -> anyhow::Result<()> {
    change(store, &team.id, |team, events| {
        let role = team.role_of(member_id);
        let Some(found) = team.members.iter_mut().find(|member| member.id == member_id) else { return Ok(()) };
        if same_prompt_change(found, wanted, prompt) {
            if let Some(session) = session {
                if let Some(stored) = found.session.as_mut().filter(|stored| stored.run_id == session.run_id) {
                    stored.conversation_id = session.conversation_id.clone();
                }
            }
            found.prompt_change = None;
        }
        match done {
            PromptDone::Replaced => events.push(Event::new("prompt_replaced", json!({"member":role}))),
            PromptDone::Failed(reason) => events.push(Event::new("prompt_change_failed", json!({"member":role,"reason":reason}))),
            PromptDone::Uncertain(reason) => events.push(Event::new("prompt_change_uncertain", json!({"member":role,"reason":reason}))),
        }
        Ok(())
    })
}

/// Task #6: the message that gives a running member its new start prompt. It
/// says the words replace the earlier instructions, so the member does not keep
/// both.
pub(crate) fn replace_prompt_text(team: &Team, member: &Member) -> String {
    let prompt = member.prompt.trim();
    let instructions = if prompt.is_empty() {
        "Your start prompt is now empty: the instructions you were given before no longer apply.".to_owned()
    } else {
        format!("Your start prompt changed. These instructions replace the ones you were given before:\n\n{prompt}")
    };
    format!(
        "[Zerus team \u{b7} from: Zerus \u{b7} replaces previous instructions]\n\
         Team: {}. Your role: {}.\n\
         {instructions}\n\n\
         They apply from your next task on; do not follow the earlier instructions, and go on with the work you have.",
        team.name,
        team.role_of(&member.id)
    )
}


/// U48: one git command in a member's folder, the repository's hooks and
/// fsmonitor off, bounded in time: its output, or why it failed.
fn git_bounded(folder: &str, args: &[&str]) -> std::result::Result<String, (bool, String)> {
    if folder.trim().is_empty() {
        return Err((false, "the member's folder is empty: no Git read".into()));
    }
    let mut command = git_command(folder);
    command.args(args).env("LC_ALL", "C");
    let outcome = super::templates::run_tree(&mut command, Duration::from_secs(60), None);
    match outcome.exit {
        Some(0) => Ok(outcome.tail.trim().to_owned()),
        Some(_) => Err((false, outcome.tail.trim().lines().last().unwrap_or("git failed").to_owned())),
        None if outcome.timed_out => Err((true, format!("git {} did not finish in 60 s", args.first().copied().unwrap_or_default()))),
        None => Err((false, outcome.error)),
    }
}

/// U48: the task's branch for this member when the team sets
/// `branch_per_task`; None keeps the member's own branch.
pub(crate) fn task_branch(team: &Team, task_id: u32, member_id: &str) -> Option<String> {
    let setting = team.settings.branch_per_task.as_ref()?;
    Some(ops::branch_name(&setting.template, task_id, member_id, &team.id[..8.min(team.id.len())]))
}

/// U48 (spec 1, step 2): with `branch_per_task`, the task's branch in the
/// member's folder before the hand-out, outside any sandbox: a folder with
/// uncommitted changes waits; a missing branch starts from the team's base (the
/// lead's branch unless the owner names one), an existing one (a returned or
/// reopened task) is checked out. Branches are never deleted. None when the
/// team keeps one branch per member, or for the lead's own task.
pub(crate) fn prepare_branch(team: &Team, task_id: u32, member_id: &str) -> Option<PrepareStep> {
    let branch = task_branch(team, task_id, member_id)?;
    if member_id == team.lead {
        return None;
    }
    let member = team.member(member_id)?;
    let step = |from: &str, result: &str, reason: String| PrepareStep {
        step: "branch".into(),
        member: String::new(),
        from: from.to_owned(),
        to: branch.clone(),
        at: now(),
        result: result.into(),
        reason,
    };
    if super::templates::check_branch(&branch).is_err() {
        return Some(step("", "refused", format!("{branch} is not a valid branch name")));
    }
    if member.folder == MemberFolder::Shared {
        return Some(step("", "skipped", "the member works in the team's shared folder, whose branch stays".into()));
    }
    if !member.computer.is_empty() {
        return Some(step("", "refused", format!("a task branch is made only in a folder on this computer, not on {}", member.computer)));
    }
    let folder = match member_folder_path(team, member) {
        Ok(folder) if !folder.is_empty() => folder,
        Ok(_) => return Some(step("", "refused", "the member has no folder".into())),
        Err(error) => return Some(step("", "refused", format!("{error:#}"))),
    };
    if same_path(&folder, &team.folder) {
        return Some(step("", "skipped", "the member works in the team's own folder, whose branch stays".into()));
    }
    let current = match git_bounded(&folder, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(current) => current,
        Err((_, error)) => return Some(step("", "refused", format!("the folder's branch was not read: {error}"))),
    };
    match git_bounded(&folder, &["status", "--porcelain", "--untracked-files=no"]) {
        Ok(changes) if changes.is_empty() => {}
        Ok(_) => return Some(step(&current, "refused", "folder has changes".into())),
        Err((_, error)) => return Some(step(&current, "refused", format!("the folder's changes were not read: {error}"))),
    }
    if current == branch {
        return Some(step(&current, "skipped", format!("the folder is on {branch} already")));
    }
    let exists = git_bounded(&folder, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok();
    let (moved, how) = if exists {
        (git_bounded(&folder, &["checkout", &branch]), "checked out".to_owned())
    } else {
        // Task #267: with an integration folder the team's base is named once:
        // a new task's branch starts from the integration line's head, never
        // from the member's previous branch or another member's commit.
        let base = if !team.settings.integration_folder.trim().is_empty() {
            match super::merge_queue::integration_branch(team) {
                Some(base) => base,
                None => return Some(step(&current, "refused", "The integration line's branch was not read".into())),
            }
        } else if team.settings.base_branch.is_empty() {
            match lead_branch(team) {
                Ok(base) => base,
                Err(error) => return Some(step(&current, "refused", format!("{error:#}"))),
            }
        } else {
            team.settings.base_branch.clone()
        };
        // Task 374 (owner, 2026-11-08): a member's separate clone has the
        // line's branch only if it fetched it, and the line lives in the team's
        // integration folder. Fetch it there before branching from it.
        let known = |rev: &str| git_bounded(&folder, &["rev-parse", "--verify", "--quiet", rev]).is_ok();
        let start = if known(&format!("refs/heads/{base}")) || known(&format!("{base}^{{commit}}")) {
            base.clone()
        } else {
            let source = team.integration_folder();
            let fetched = git_bounded(&folder, &["fetch", "--no-tags", "--quiet", source, &base]);
            match fetched {
                Ok(_) => "FETCH_HEAD".to_owned(),
                Err((_, error)) => {
                    return Some(step(
                        &current,
                        "refused",
                        format!("The line's branch {base} is not in this folder and was not fetched from {source}: {error}"),
                    ))
                }
            }
        };
        (git_bounded(&folder, &["checkout", "-b", &branch, &start]), format!("created from {base}"))
    };
    match moved {
        Ok(_) => {}
        Err((true, error)) => return Some(step(&current, "uncertain", error)),
        Err((false, error)) => return Some(step(&current, "refused", error)),
    }
    // Confirmed before the hand-out: the folder is on the task's branch now.
    match git_bounded(&folder, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(now_on) if now_on == branch => Some(step(&current, "done", how)),
        Ok(now_on) => Some(step(&current, "uncertain", format!("the folder is on {now_on}, not {branch}"))),
        Err((_, error)) => Some(step(&current, "uncertain", format!("the folder's branch was not read again: {error}"))),
    }
}

/// Task 155 (owner, 2026-11-07): an accepted task a hand-out builds on: the
/// commit its accepted report named, who made it and on which branch.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AcceptedWork {
    pub task: u32,
    pub commit: String,
    pub author: String,
    pub branch: String,
}

/// Task 155: the accepted work `task_id` starts from: every accepted task it
/// depends on, directly or through another dependency, whose accepted report
/// named a commit; in number order, so a predecessor merges before the tasks
/// built on it.
pub(crate) fn accepted_predecessors(team: &Team, task_id: u32) -> Vec<AcceptedWork> {
    let mut seen = BTreeSet::new();
    let mut pending = vec![task_id];
    while let Some(id) = pending.pop() {
        for dep in team.task(id).map(|task| task.deps.as_slice()).unwrap_or_default() {
            if dep.task != task_id && seen.insert(dep.task) {
                pending.push(dep.task);
            }
        }
    }
    seen.into_iter()
        .filter_map(|id| team.task(id))
        .filter(|task| task.status == TaskStatus::Accepted)
        .filter_map(|task| {
            let attempt = task.attempts.iter().rev().find(|attempt| attempt.state == AttemptState::Reported && !attempt.folder_head.is_empty())?;
            Some(AcceptedWork { task: task.id, commit: attempt.folder_head.clone(), author: attempt.member.clone(), branch: attempt.folder_branch.clone() })
        })
        .collect()
}

/// Task #267: with an integration folder, a new task's folder starts from the
/// current integration head, never from the member's previous branch; work it
/// has uncommitted is committed on that branch first, so nothing is lost.
/// None without an integration folder, for the lead, for a shared folder and
/// for a task continuing its own branch (a retry or a task branch).
pub(crate) fn prepare_integration_base(team: &Team, task_id: u32, member_id: &str) -> Option<PrepareStep> {
    if team.settings.integration_folder.trim().is_empty() || member_id == team.lead {
        return None;
    }
    let member = team.member(member_id)?;
    if member.folder == MemberFolder::Shared || !member.computer.is_empty() {
        return None;
    }
    let task = team.task(task_id)?;
    if !task.attempts.is_empty() {
        return None;
    }
    let folder = match member_folder_path(team, member) {
        Ok(folder) if !folder.is_empty() => folder,
        Ok(_) => return None,
        Err(_) => return None,
    };
    if same_path(&folder, team.integration_folder()) || same_path(&folder, &team.folder) {
        return None;
    }
    // Task #382 (owner, 2026-11-08): the base step never moves the branch the
    // member's committed work sits on. The task gets its own branch from the
    // integration head, `team/<team8>/<member>-<task>`; an existing one is
    // checked out (a returned hand-out continues it) and the old branch stays
    // where it is, with every commit it holds still reachable from it.
    let branch = format!("team/{}/{}-{}", &team.id[..8.min(team.id.len())], tag(member_id), task_id);
    let step = |from: &str, result: &str, reason: String| PrepareStep {
        step: "base".into(),
        member: String::new(),
        from: from.to_owned(),
        to: branch.clone(),
        at: now(),
        result: result.into(),
        reason,
    };
    // A task branch is created from the integration head by prepare_branch.
    if task_branch(team, task_id, member_id).is_some() {
        return None;
    }
    let Some(base) = super::merge_queue::integration_branch(team) else {
        return Some(step("", "refused", "The integration line's branch was not read".into()));
    };
    let current = match git_bounded(&folder, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(current) => current,
        Err((_, error)) => return Some(step("", "refused", format!("the folder's branch was not read: {error}"))),
    };
    if current == base {
        return Some(step(&current, "skipped", "the folder is on the integration line already".into()));
    }
    let head = super::merge_queue::integration_head(team).unwrap_or_default();
    let here = git_bounded(&folder, &["rev-parse", "HEAD"]).unwrap_or_default();
    if !head.is_empty() && here == head {
        return Some(step(&current, "skipped", "the folder is at the integration head already".into()));
    }
    // Dirty work is committed on its old branch first, as the lead did by hand.
    let changes = match git_bounded(&folder, &["status", "--porcelain", "--untracked-files=no"]) {
        Ok(changes) => changes,
        Err((_, error)) => return Some(step(&current, "refused", format!("the folder's changes were not read: {error}"))),
    };
    if !changes.is_empty() {
        if let Err((_, error)) = git_bounded(&folder, &["add", "-u"]) {
            return Some(step(&current, "refused", format!("the folder's changes were not staged: {error}")));
        }
        let message = format!("Zerus: {member_id}'s unfinished work before task #{task_id}");
        if let Err((_, error)) = git_bounded(&folder, &["commit", "-q", "-m", &message]) {
            return Some(step(&current, "refused", format!("the folder's changes were not committed: {error}")));
        }
    }
    let head = super::merge_queue::integration_head(team).unwrap_or_default();
    // Task #382 (owner, 2026-11-08): never `checkout -B` the branch the folder
    // is on - that reset the member's branch to the line and orphaned the
    // commits it had just reported. The task's own branch starts from the
    // integration head (or is checked out when it exists); the branch the
    // folder leaves keeps every commit it holds.
    if branch == current {
        return Some(step(
            &current,
            "skipped",
            format!("the folder is on this task's branch {branch} already; it is never reset, and the commits it holds stay on it"),
        ));
    }
    if super::templates::check_branch(&branch).is_err() {
        return Some(step(&current, "refused", format!("{branch} is not a valid branch name")));
    }
    let exists = git_bounded(&folder, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok();
    let (moved, how) = if exists {
        // A branch this task left behind (an interrupted hand-out): continue
        // it. It is never moved, and the folder keeps its old branch too.
        (
            git_bounded(&folder, &["checkout", &branch]),
            format!("task branch {branch} is checked out; the folder's branch {current} keeps its commits"),
        )
    } else {
        (
            git_bounded(&folder, &["checkout", "-b", &branch, &base]),
            format!(
                "{branch} starts from the integration head {base} ({}), and {current} keeps its commits",
                head.chars().take(9).collect::<String>()
            ),
        )
    };
    match moved {
        Ok(_) => {}
        Err((true, error)) => return Some(step(&current, "uncertain", error)),
        Err((false, error)) => return Some(step(&current, "refused", error)),
    }
    // Confirmed before the hand-out: the folder is on the new task branch now.
    match git_bounded(&folder, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(now_on) if now_on == branch => Some(step(&current, "done", how)),
        Ok(now_on) => Some(step(&current, "uncertain", format!("the folder is on {now_on}, not {branch}"))),
        Err((_, error)) => Some(step(&current, "uncertain", format!("the folder's branch was not read again: {error}"))),
    }
}

/// Task 155 (owner, 2026-11-07): before the hand-out, the accepted work of
/// the task's dependencies is merged into the member's branch, outside any
/// sandbox, so a dependent task starts from its predecessors' accepted
/// commits. A commit the folder lacks is fetched from its author's folder; one
/// the branch holds already is skipped. A conflict is aborted, the folder put
/// back and the task waits for the lead with the files: it is never left to
/// the member. None when nothing is accepted yet, for the lead's own task and
/// for a member working in the team's own folder (the lead merges there).
pub(crate) fn prepare_dependencies(team: &Team, task_id: u32, member_id: &str) -> Option<PrepareStep> {
    // Task #267: with an integration folder the accepted work is already on
    // the line every hand-out starts from, so no member branch ever merges
    // another member's commit; the step says so for the lead's status.
    if !team.settings.integration_folder.trim().is_empty() && member_id != team.lead {
        let wanted = accepted_predecessors(team, task_id);
        if wanted.is_empty() {
            return None;
        }
        let names = wanted.iter().map(|work| format!("#{}", work.task)).collect::<Vec<_>>().join(", ");
        // Task #464 (owner, 2026-11-09): its own accepted work never waits for
        // a landing - its folder holds the commits it made - and a landing
        // task is the landing: it starts without waiting for what it lands.
        let own = wanted.iter().all(|work| work.author == member_id);
        let landing = team.task(task_id).is_some_and(super::merge_queue::is_landing);
        if !own && !landing {
            // A dependent starts only from what is on the line: while a landing
            // waits (the queue, its tests or a conflict's resolve task), it waits
            // with that reason instead of starting without its predecessor.
            if let Some(missing) = wanted.iter().find(|work| !super::merge_queue::landed(team, &work.commit)) {
                let state = team.task(missing.task).and_then(|task| task.merge.as_ref()).map_or_else(|| "no landing".to_owned(), |entry| {
                    if entry.state == "conflict" { format!("its landing conflicts; resolve task #{} waits", entry.resolve_task.unwrap_or_default()) } else { format!("its landing is {}", entry.state) }
                });
                return Some(PrepareStep {
                    step: "dependencies".into(),
                    member: String::new(),
                    from: String::new(),
                    to: names,
                    at: now(),
                    result: "refused".into(),
                    reason: format!("#{}'s accepted work ({}) is not on the integration line yet: {state}", missing.task, missing.commit.chars().take(9).collect::<String>()),
                });
            }
            return Some(PrepareStep {
                step: "dependencies".into(),
                member: String::new(),
                from: String::new(),
                to: names,
                at: now(),
                result: "skipped".into(),
                reason: "the accepted work lands on the integration line; the folder starts from its head".into(),
            });
        }
    }
    let wanted = accepted_predecessors(team, task_id);
    if wanted.is_empty() || member_id == team.lead {
        return None;
    }
    let member = team.member(member_id)?;
    if member.folder == MemberFolder::Shared {
        return None;
    }
    let names = wanted.iter().map(|work| format!("#{}", work.task)).collect::<Vec<_>>().join(", ");
    let step = |from: &str, result: &str, reason: String| PrepareStep {
        step: "dependencies".into(),
        member: String::new(),
        from: from.to_owned(),
        to: names.clone(),
        at: now(),
        result: result.into(),
        reason,
    };
    if !member.computer.is_empty() {
        return Some(step("", "skipped", format!("the accepted work of {names} is merged only in a folder on this computer, not on {}", member.computer)));
    }
    let folder = match member_folder_path(team, member) {
        Ok(folder) if !folder.is_empty() => folder,
        Ok(_) => return Some(step("", "refused", "the member has no folder".into())),
        Err(error) => return Some(step("", "refused", format!("{error:#}"))),
    };
    if same_path(&folder, &team.folder) {
        return None;
    }
    let head = match git_bounded(&folder, &["rev-parse", "--short", "HEAD"]) {
        Ok(head) => head,
        Err((_, error)) => return Some(step("", "refused", format!("the folder's HEAD was not read: {error}"))),
    };
    match git_bounded(&folder, &["status", "--porcelain", "--untracked-files=no"]) {
        Ok(changes) if changes.is_empty() => {}
        Ok(_) => return Some(step(&head, "refused", format!("folder has changes, so the accepted work of {names} was not merged"))),
        Err((_, error)) => return Some(step(&head, "refused", format!("the folder's changes were not read: {error}"))),
    }
    let short = |commit: &str| commit.chars().take(9).collect::<String>();
    // A merge into the member's branch: done, or the step that refuses (or is
    // uncertain) with the conflicting files and the folder put back.
    #[allow(clippy::result_large_err)] // the refusal is the prepare step itself
    let merge = |rev: &str, message: &str, what: &str, merged: &[String]| -> std::result::Result<(), PrepareStep> {
        // The catalogs merge as data (scripts/i18n-merge.py); like the merge
        // queue, this path passes the driver rather than trusting repo config.
        let driver = super::merge_queue::i18n_driver_config();
        let Err((timed_out, error)) = git_bounded(&folder, &["-c", driver.as_str(), "merge", "--no-ff", "--no-edit", "-m", message, rev]) else {
            return Ok(());
        };
        let files = git_bounded(&folder, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
        let files: Vec<&str> = files.lines().map(str::trim).filter(|line| !line.is_empty()).take(20).collect();
        let in_merge = git_bounded(&folder, &["rev-parse", "--verify", "--quiet", "MERGE_HEAD"]).is_ok();
        let restored = !in_merge || git_bounded(&folder, &["merge", "--abort"]).is_ok();
        let why = if files.is_empty() { format!("was not merged: {error}") } else { format!("conflicts in {}", files.join(", ")) };
        let earlier = if merged.is_empty() { String::new() } else { format!(" ({} merged before it stay)", merged.join(", ")) };
        Err(if timed_out || !restored {
            step(&head, "uncertain", format!("{what} {why}; the merge into {folder} did not finish cleanly and the folder may still be mid-merge"))
        } else {
            step(&head, "refused", format!(
                "{what} {why}. Zerus put the folder back{earlier}; merge it into the member's branch ({folder}) and resolve it, or reassign the task, then retry"
            ))
        })
    };
    let mut merged = Vec::new();
    // Task #15 (team ede724c4, lead of team 2b863c49, 2026-11-10): when the
    // team has a line (its base branch, or the lead's branch), a branch that
    // lacks accepted work is brought up to the line's head first. The lead
    // resolves the predecessors' conflicts there once; merging them commit by
    // commit met the same conflict again in every folder that lacked one.
    let missing = wanted.iter().any(|work| git_bounded(&folder, &["merge-base", "--is-ancestor", &work.commit, "HEAD"]).is_err());
    let line = if missing { dependency_line(team, &folder) } else { None };
    if let Some((base, line_head)) = &line {
        match git_bounded(&folder, &["merge-base", "--is-ancestor", line_head, "HEAD"]) {
            Ok(_) => {}
            Err((true, error)) => return Some(step(&head, "uncertain", error)),
            Err((false, _)) => {
                let message = format!("Merge the line {base} ({}) before task #{task_id}", short(line_head));
                if let Err(refused) = merge(line_head, &message, &format!("The line {base} ({})", short(line_head)), &merged) {
                    return Some(refused);
                }
                merged.push(format!("the line {base} ({})", short(line_head)));
            }
        }
    }
    for work in &wanted {
        let object = format!("{}^{{commit}}", work.commit);
        if git_bounded(&folder, &["cat-file", "-e", &object]).is_err() {
            let source = team
                .member(&work.author)
                .filter(|author| author.computer.is_empty())
                .and_then(|author| member_folder_path(team, author).ok())
                .filter(|source| !source.is_empty());
            let Some(source) = source else {
                return Some(step(&head, "refused", format!(
                    "#{}'s accepted commit {} is not in this folder and its author's folder is not on this computer",
                    work.task, short(&work.commit)
                )));
            };
            let refspec = if work.branch.is_empty() || work.branch == "HEAD" { "HEAD".to_owned() } else { work.branch.clone() };
            match git_bounded(&folder, &["fetch", "--no-tags", "--quiet", &source, &refspec]) {
                Err((true, error)) => return Some(step(&head, "uncertain", error)),
                _ if git_bounded(&folder, &["cat-file", "-e", &object]).is_ok() => {}
                Err((false, error)) => {
                    return Some(step(&head, "refused", format!("#{}'s accepted commit {} was not fetched from {source}: {error}", work.task, short(&work.commit))))
                }
                Ok(_) => return Some(step(&head, "refused", format!("#{}'s accepted commit {} is not on {refspec} in {source}", work.task, short(&work.commit)))),
            }
        }
        match git_bounded(&folder, &["merge-base", "--is-ancestor", &work.commit, "HEAD"]) {
            Ok(_) => continue,
            Err((true, error)) => return Some(step(&head, "uncertain", error)),
            Err((false, _)) => {}
        }
        // Task #15: a predecessor the line holds as an equivalent commit (a
        // cherry-pick or a rebase of it) is on the line already.
        if let Some((_, line_head)) = &line {
            let cherry = git_bounded(&folder, &["cherry", line_head, &work.commit, &format!("{}^", work.commit)]).unwrap_or_default();
            if !cherry.trim().is_empty() && cherry.lines().all(|line| line.starts_with('-')) {
                continue;
            }
        }
        let message = format!("Merge the accepted work of #{} ({}) before task #{task_id}", work.task, short(&work.commit));
        if let Err(refused) = merge(&work.commit, &message, &format!("#{}'s accepted commit {}", work.task, short(&work.commit)), &merged) {
            return Some(refused);
        }
        merged.push(format!("#{} ({})", work.task, short(&work.commit)));
    }
    Some(if merged.is_empty() {
        step(&head, "skipped", format!("the branch holds the accepted work of {names} already"))
    } else {
        step(&head, "done", format!("merged {}", merged.join(", ")))
    })
}

/// Task #15 (team ede724c4): the line a hand-out brings a member's branch up
/// to, as (branch, head commit) in the member's `folder`: the team's base
/// branch, else the lead's branch. Its head is fetched from the team's folder
/// (a separate clone has it only if it fetched it, as `prepare_branch` does);
/// without that the folder's own branch of that name. None when the team
/// names no line or it is not read.
fn dependency_line(team: &Team, folder: &str) -> Option<(String, String)> {
    let base = match team.settings.base_branch.trim() {
        // Task #19: a manual line is the team's line.
        _ if super::merge_queue::manual(team) => team.settings.manual_line.trim().to_owned(),
        "" => lead_branch(team).ok()?,
        named => named.to_owned(),
    };
    if base.is_empty() || base == "HEAD" || base.starts_with('-') {
        return None;
    }
    let source = team.integration_folder();
    if !source.trim().is_empty() && git_bounded(folder, &["fetch", "--no-tags", "--quiet", source, &base]).is_ok() {
        if let Ok(head) = git_bounded(folder, &["rev-parse", "--verify", "--quiet", "FETCH_HEAD^{commit}"]) {
            return Some((base, head));
        }
    }
    git_bounded(folder, &["rev-parse", "--verify", "--quiet", &format!("{base}^{{commit}}")]).ok().map(|head| (base, head))
}

/// Spec 0: a refused or unconfirmed step before a hand-out leaves the task
/// waiting with its reason and one inbox item for the lead; it is never
/// retried on its own.
fn task_waits(team: &mut Team, task_id: u32, member_id: &str, step: &PrepareStep, events: &mut Vec<Event>) -> anyhow::Result<()> {
    let role = team.role_of(member_id);
    let task = team.task_mut(task_id).context("unknown task")?;
    task.waits = Some(TaskWait::zerus(&step.step, &step.result, &step.reason, step.at));
    events.push(Event::new(
        "task_waits",
        json!({"task":task_id,"member":role,"step":step.step,"to":step.to,"result":step.result,"reason":step.reason}),
    ));
    Ok(())
}

/// #511 N3: the message that tells a member to stop a worked-ahead attempt
/// Zerus revoked with its phase.
pub(crate) fn stop_text(team: &Team, stop: &StopNotice) -> String {
    // Task 24: an attempt in progress that a reassign, a take-over or a
    // cancel revoked (no phase).
    if stop.phase == 0 {
        return format!(
            "[Zerus team \u{b7} from: Zerus \u{b7} task: #{} \u{b7} stop]\n\
             Stop working on task #{} {}: {}, so Zerus revoked your attempt {}. \
             Stop the builds, tests and commands you started for it, do not report it and commit nothing more for it. \
             Your next task comes in its own message (team {}).",
            stop.task, stop.task, stop.title, stop.reason, stop.attempt, team.name
        );
    }
    format!(
        "[Zerus team \u{b7} from: Zerus \u{b7} task: #{} \u{b7} stop]\n\
         Stop working on task #{} {}: {}, so Zerus revoked your attempt {} that worked ahead of it. \
         Do not report it and commit nothing more for it. The lead gives the task out again once phase {} is approved; \
         until then wait for your next task (team {}).",
        stop.task, stop.task, stop.title, stop.reason, stop.attempt, stop.phase, team.name
    )
}

/// #511 N3: sends one stop notice under its own request id. A refusal is tried
/// again on a later tick, ten times at most; an uncertain send is never
/// repeated (it may have arrived) and stays unconfirmed in status.
fn tell_stop(store: &mut Store, team: &Team, id: &str) -> anyhow::Result<()> {
    let Some(stop) = team.stops.iter().find(|stop| stop.id == id && stop.notice == Notice::Pending).cloned() else { return Ok(()) };
    let session = team.member(&stop.member).and_then(|member| member.session.clone()).context("the member has no session")?;
    let outcome = send(&session, &stop.id, &stop_text(team, &stop));
    change(store, &team.id, |team, events| {
        let role = team.role_of(&stop.member);
        let Some(found) = team.stops.iter_mut().find(|found| found.id == stop.id) else { return Ok(()) };
        match &outcome {
            Sent::Delivered => {
                found.notice = Notice::Sent;
                events.push(Event::new("stop_told", json!({"task":stop.task,"member":role,"attempt":stop.attempt})));
            }
            Sent::Refused(error) => {
                found.refused += 1;
                if found.refused >= 10 {
                    found.notice = Notice::Unconfirmed;
                    events.push(Event::new("stop_unconfirmed", json!({"task":stop.task,"member":role,"attempt":stop.attempt,"error":error})));
                }
            }
            Sent::Uncertain(error) => {
                found.notice = Notice::Unconfirmed;
                events.push(Event::new("stop_unconfirmed", json!({"task":stop.task,"member":role,"attempt":stop.attempt,"error":error})));
            }
        }
        Ok(())
    })
}

/// Task 24: interrupts the busy turn of a member whose attempt was revoked,
/// once (the owner's interrupt_revoked); the stop notice follows. A failed
/// interrupt is recorded and not repeated: the notice still reaches the member.
fn interrupt_stop(store: &mut Store, team: &Team, id: &str) -> anyhow::Result<()> {
    let Some(stop) = team.stops.iter().find(|stop| stop.id == id && stop.notice == Notice::Pending && stop.interrupt && !stop.interrupted).cloned() else {
        return Ok(());
    };
    let Some(session) = team.member(&stop.member).and_then(|member| member.session.clone()) else { return Ok(()) };
    let input = json!({"run_id":session.run_id,"conversation_id":session.conversation_id});
    let outcome = hgs(&session.host, &["interrupt", &session.name, "--json"], Some(&input), Duration::from_secs(60));
    let error = match &outcome {
        Ok(output) if output.status.success() => String::new(),
        Ok(output) => String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        Err(error) => error.to_string(),
    };
    change(store, &team.id, |team, events| {
        let role = team.role_of(&stop.member);
        if let Some(found) = team.stops.iter_mut().find(|found| found.id == stop.id) {
            found.interrupted = true;
        }
        events.push(Event::new(
            "revoked_interrupted",
            json!({"task":stop.task,"member":role,"attempt":stop.attempt,"ok":error.is_empty(),"error":error}),
        ));
        Ok(())
    })
}

/// #680: a commit in a message: its first 12 characters.
fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// U47/U48: starts a command or land stage as a detached `hgs run` in the
/// stage's folder, with no model. C1 (#680): the folder follows the preset's
/// rule like a test's: a detached checkout of the checked attempt's last commit
/// for "checkout", the task's held folder otherwise; the command gets the task,
/// its branch and that commit, and a land stage refuses a folder on another
/// commit. The command is the task's snapshot of the owner's catalog; a land
/// stage runs only when the owner allowed landing in this team.
fn start_stage_command(store: &mut Store, team: &Team, task_id: u32, run_id: &str) -> anyhow::Result<()> {
    let Some((open, stage)) = super::pipeline::command_of(team, task_id, run_id) else { return Ok(()) };
    let not_run = |store: &mut Store, reason: String| {
        change(store, &team.id, |team, events| super::pipeline::command_not_run(team, task_id, run_id, "refused", &reason, events).map(drop))
    };
    if stage.kind == "land" && !team.settings.land_allowed {
        return not_run(store, "the owner has not allowed landing in this team (land_allowed)".into());
    }
    let task = team.task(task_id).context("unknown task")?;
    let author = task.attempts.iter().find(|attempt| attempt.n == open.attempt).map_or_else(|| task.assignee.clone(), |attempt| attempt.member.clone());
    let Some(member) = team.member(&author) else { return not_run(store, "the attempt's member left the team".into()) };
    if !member.computer.is_empty() {
        return not_run(store, format!("the stage's folder is on {}; Zerus runs a command stage on this computer only", member.computer));
    }
    // #635 L1: no command or land stage of this task starts while an earlier
    // run of the task is awaiting_run and live; a replaced report restarts the
    // stage on the next tick after that run ends instead of beside it.
    if team.task(task_id).and_then(|task| task.pipeline_run.as_ref()).is_some_and(|pipeline| {
        pipeline.runs.iter().any(|run| run.awaiting_run && super::stages::run_live(team, run))
    }) {
        return Ok(());
    }
    // #680 C1: the stage's folder by the preset's rule, exactly as a test's:
    // a detached checkout of the checked commit, or the task's held folder.
    let prepared = match super::stages::prepare(team, task_id) {
        Ok(prepared) => prepared,
        Err(error) => return not_run(store, format!("{error:#}")),
    };
    let clean = |prepared: &super::stages::Prepared| {
        let _ = super::stages::clean_up(&StageRun {
            checkout: prepared.checkout.clone(),
            checkout_repo: prepared.checkout_repo.clone(),
            temp: prepared.temp.clone(),
            ..Default::default()
        });
    };
    if !Path::new(&prepared.folder).is_dir() {
        clean(&prepared);
        return not_run(store, format!("the stage's folder {} does not exist", prepared.folder));
    }
    // N11: one run per folder at a time; the stage starts on a later tick.
    if !super::outbox::running_all(Path::new(&prepared.folder)).is_empty()
        || !super::super::builds::running_in(Path::new(&prepared.folder), now()).is_empty()
    {
        clean(&prepared);
        return Ok(());
    }
    // #680/#684: a land stage lands the checked commit and nothing else. In a
    // Git folder a missing recorded commit is refused rather than reading HEAD
    // at the stage's start; in a folder without Git there is no HEAD to check,
    // and the hold rule keeps the folder as the implementer left it (N2).
    if stage.kind == "land" && prepared.git {
        if prepared.commit.is_empty() {
            clean(&prepared);
            return not_run(store, "The attempt recorded no commit for the land stage to check; commit with hgs team commit (or git) before reporting".into());
        }
        let head = git_at(&prepared.folder, &["rev-parse", "HEAD"]).unwrap_or_default();
        if head != prepared.commit {
            clean(&prepared);
            return not_run(
                store,
                format!(
                    "The stage's folder is on {} but the checked commit is {}; the next task may have moved it",
                    short_sha(&head),
                    short_sha(&prepared.commit)
                ),
            );
        }
    }
    // The task's own branch, else the branch the attempt reported, else the
    // implementer's folder's: what the command sees.
    let reported_branch = task.attempts.iter().find(|attempt| attempt.n == open.attempt).map(|attempt| attempt.folder_branch.clone()).unwrap_or_default();
    let live = super::stages::task_folder(team, task, &open).unwrap_or_default();
    let branch = task_branch(team, task_id, &author)
        .or_else(|| (!reported_branch.is_empty()).then_some(reported_branch))
        .or_else(|| git_at(&live, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|branch| branch != "HEAD"))
        .unwrap_or_default();
    let commit = prepared.commit.clone();
    // #684 L1: a checkout prepare() made goes if the start is not recorded
    // (a store error here, or a crash between prepare and this change).
    if let Err(error) = change(store, &team.id, |team, events| {
        super::pipeline::command_started(team, task_id, run_id, &commit, events)?;
        super::stages::record(team, task_id, run_id, &prepared);
        Ok(())
    }) {
        clean(&prepared);
        return Err(error);
    }
    let command = stage.command.clone().unwrap_or_default();
    let session = member.session.as_ref().map_or_else(
        || json!({}),
        |session| json!({"name":session.name,"run_id":session.run_id,"conversation_id":session.conversation_id}),
    );
    let label = format!("{} stage of #{task_id}", stage.kind);
    let task_text = task_id.to_string();
    let env = [
        ("HGS_TASK_ID", task_text.as_str()),
        ("HGS_TASK_BRANCH", branch.as_str()),
        ("HGS_TASK_COMMIT", commit.as_str()),
    ];
    if let Err(error) = super::super::builds::start_for(run_id, &label, &command.command, Path::new(&prepared.folder), session, command.timeout_minutes, &env) {
        let reason = format!("the stage's command did not start: {error}");
        change(store, &team.id, |team, events| super::pipeline::command_not_run(team, task_id, run_id, "refused", &reason, events).map(drop))?;
    }
    Ok(())
}

/// How long a started stage may leave no run record at all before the lead
/// hears that its run was lost (the worker stopped between the claim and the
/// start).
const STAGE_RECORD_GRACE: f64 = 120.0;

/// U47/U48: reads a started stage's run record; once it ended, its exit code
/// decides the stage. A run that stopped without a result is uncertain and
/// goes to the lead; it never runs again on its own.
fn settle_stage_command(store: &mut Store, team: &Team, task_id: u32, run_id: &str) -> anyhow::Result<()> {
    let Some((open, _)) = super::pipeline::command_of(team, task_id, run_id) else { return Ok(()) };
    let at = now();
    let record = match super::super::builds::record(run_id, at) {
        Some(record) if record["status"] == "running" => return Ok(()),
        Some(record) => record,
        None if at - open.delivered_at.unwrap_or(at) < STAGE_RECORD_GRACE => return Ok(()),
        None => {
            return change(store, &team.id, |team, events| {
                super::pipeline::command_not_run(team, task_id, run_id, "uncertain", "the stage's run left no record", events).map(drop)
            });
        }
    };
    if record["status"] != "ran" {
        let reason = record["error"].as_str().filter(|error| !error.is_empty()).unwrap_or("the stage's run stopped without a result").to_owned();
        return change(store, &team.id, |team, events| super::pipeline::command_not_run(team, task_id, run_id, "uncertain", &reason, events).map(drop));
    }
    let outcome = super::pipeline::CommandOutcome {
        exit: record["exit"].as_i64().and_then(|exit| i32::try_from(exit).ok()),
        tail: record["tail"].as_str().unwrap_or_default().to_owned(),
        timed_out: record["timed_out"] == true,
        error: record["error"].as_str().unwrap_or_default().to_owned(),
    };
    change(store, &team.id, |team, events| super::pipeline::command_ended(team, task_id, run_id, &outcome, events).map(drop))
}

fn hand_out(store: &mut Store, team_id: &str, task_id: u32, member_id: &str, from: Option<&str>, host: &str) -> anyhow::Result<()> {
    // Spec 1: prepare the member before the hand-out. A team that sets no
    // branch per task and no context action takes today's path with no extra
    // call and no extra event.
    let team = store.load(team_id)?.with_context(|| format!("Unknown team {team_id}"))?;
    let member = team.member(member_id).cloned().with_context(|| format!("Unknown member {member_id}"))?;
    anyhow::ensure!(!team.reserved_for_rework(member_id, Some(task_id)), "member reserved for rework");
    if let Some(task) = team.task(task_id).filter(|task| task.assignee != member_id && !task.assignee.is_empty()) {
        anyhow::ensure!(!team.rebalance_blocked(task), "task must stay with its member");
        // Task 259: a task given across roles never lands beside work in
        // progress in the same files in the new member's folder.
        anyhow::ensure!(!InProgressOutputs::new(&team).blocks_for(member_id, task), "the member has work in progress in the same files");
    }
    // U48 (spec 1, step 2): the task's branch, confirmed before the hand-out;
    // a refused or unconfirmed one leaves the task waiting. A confirmed one
    // waits on the task for the attempt that hands it out.
    if let Some(step) = if can_hand_out(&team, task_id) { prepare_branch(&team, task_id, member_id) } else { None } {
        if matches!(step.result.as_str(), "refused" | "uncertain") {
            return change(store, team_id, |team, events| task_waits(team, task_id, member_id, &step, events));
        }
        change(store, team_id, |team, _| {
            let task = team.task_mut(task_id).context("unknown task")?;
            super::models::record(task, PrepareStep { member: member_id.to_owned(), ..step });
            Ok(())
        })?;
    }
    // Task #267: with an integration folder the folder starts from the
    // integration head (its dirty work committed on the old branch first); the
    // predecessors' work is there because it landed.
    if let Some(step) = if can_hand_out(&team, task_id) { prepare_integration_base(&team, task_id, member_id) } else { None } {
        if matches!(step.result.as_str(), "refused" | "uncertain") {
            return change(store, team_id, |team, events| task_waits(team, task_id, member_id, &step, events));
        }
        change(store, team_id, |team, _| {
            let task = team.task_mut(task_id).context("unknown task")?;
            super::models::record(task, PrepareStep { member: member_id.to_owned(), ..step });
            Ok(())
        })?;
    }
    // Task 155: the accepted work of the task's dependencies, merged into the
    // member's branch (after its task branch is checked out); a conflict or an
    // unconfirmed merge leaves the task waiting for the lead.
    if let Some(step) = if can_hand_out(&team, task_id) { prepare_dependencies(&team, task_id, member_id) } else { None } {
        if matches!(step.result.as_str(), "refused" | "uncertain") {
            return change(store, team_id, |team, events| task_waits(team, task_id, member_id, &step, events));
        }
        change(store, team_id, |team, _| {
            let task = team.task_mut(task_id).context("unknown task")?;
            super::models::record(task, PrepareStep { member: member_id.to_owned(), ..step });
            Ok(())
        })?;
    }
    // Task 209: a member whose context is being compacted takes no new task
    // until the compaction confirmed and it was told to go on.
    if member.context_compact.is_some() {
        return Ok(());
    }
    // U46: the role's or the member's own action; Task 209: else the collected
    // meter: at the team's threshold the task waits for a compact (or a fresh
    // conversation for an agent without one) before it is sent.
    // Task #7: the owner's After a task policy replaces the role's every-task action.
    let own = context_action(&team, &member);
    // Task 254: `cold`: only the member's cold prompt cache asks for the
    // action; its failure still hands the task out and tells the lead.
    let (action, cold) = if own.is_empty() { due_context(&team, &member) } else { (own, false) };
    if action == "fresh" {
        change(store, team_id, |team, events| {
            let role = team.role_of(member_id);
            super::ops::fresh_session(team, member_id, "context", events)?;
            events.push(Event::new("context_compact_fresh", json!({"member":role,"task":task_id,"reason":"threshold"})));
            Ok(())
        })?;
        return Ok(());
    }
    if action.is_empty() {
        return hand_out_now(store, team_id, task_id, member_id, from, host, Preparation::default());
    }
    let Some(session) = member.session.clone() else {
        let step = context_failure(context_step(&action, now()), "refused", "The member's session is not started".into());
        return record_waits(store, team_id, task_id, member_id, from, step);
    };
    match run_context(&member, &session, &action) {
        // The action is submitted: record it and look again on the next tick.
        ContextStart::Started(request) => {
            record_context_start(store, team_id, task_id, member_id, from, request)?;
            if cold {
                change(store, team_id, |team, _| {
                    if let Some(wait) = team.task_mut(task_id).and_then(|task| task.waits.as_mut()) {
                        wait.reason = COLD_REASON.into();
                    }
                    Ok(())
                })?;
            }
            Ok(())
        }
        ContextStart::Done { step, conversation } => {
            let conversation = conversation.map(|new| (session.run_id.clone(), new));
            hand_out_now(store, team_id, task_id, member_id, from, host, Preparation { steps: vec![step], conversation, resume: false })
        }
        ContextStart::Failed { step } if cold => {
            cold_compact_failed(store, &team, task_id, member_id, &step.reason);
            hand_out_now(store, team_id, task_id, member_id, from, host, Preparation::default())
        }
        ContextStart::Failed { step } => record_waits(store, team_id, task_id, member_id, from, step),
    }
}

/// Task 254: the waiting reason of a hand-out whose compaction only its
/// member's cold cache asked for.
const COLD_REASON: &str = "cold cache";

/// Task 254: a cold session's compaction failed: the task goes out anyway and
/// the lead's inbox says so once, with the reason.
fn cold_compact_failed(store: &mut Store, team: &Team, task_id: u32, member_id: &str, reason: &str) {
    let role = team.role_of(member_id);
    let _ = change(store, &team.id, |team, events| {
        events.push(Event::new("context_compact_failed", json!({"member":role,"task":task_id,"reason":"cold","result":reason})));
        if let Some(task) = team.task_mut(task_id) {
            task.waits = None;
        }
        Ok(())
    });
    tell_lead(
        store,
        team,
        "context",
        &format!(
            "[Zerus team \u{b7} from: Zerus \u{b7} task: #{task_id} \u{b7} compaction failed]\n\
             {role}'s prompt cache was cold and its context large, so Zerus compacted it before task #{task_id}; the compaction failed ({}). \
             The task was handed out anyway.",
            if reason.is_empty() { "no reason given" } else { reason }
        ),
    );
}

fn hand_out_now(
    store: &mut Store,
    team_id: &str,
    task_id: u32,
    member_id: &str,
    from: Option<&str>,
    host: &str,
    preparation: Preparation,
) -> anyhow::Result<()> {
    let Preparation { steps: prepare, conversation, resume } = preparation;
    let mut rules_team = store.load(team_id)?.context("Unknown team")?;
    if let Some((run, id)) = &conversation {
        if let Some(session) = rules_team
            .members
            .iter_mut()
            .find(|member| member.id == member_id)
            .and_then(|member| member.session.as_mut())
        {
            if session.run_id == *run {
                session.conversation_id = id.clone();
            }
        }
    }
    let rules = super::super::iron_rules::for_team(
        &rules_team,
        rules_team.member(member_id).context("Unknown member")?,
    )?;
    sample_member_usage(&rules_team, member_id);
    // Record the attempt before sending; a crash leaves it Unconfirmed.
    let attempt_id = uuid::Uuid::new_v4().to_string();
    let mut message = None;
    // N16: the finding this hand-out carries comes back if the send is refused.
    let mut note = String::new();
    let mut extra = String::new();
    let mut git_job: Option<(String, f64)> = None;
    // Task 135: a fresh session's first message carries the handoff brief.
    let recent = recent_events(store, team_id);
    let mut handoff: Option<(handoff::Built, String)> = None;
    change(store, team_id, |team, events| {
        // U46: the confirmed wait is done; this hand-out replaces it.
        if resume {
            if let Some(task) = team.task_mut(task_id) {
                task.waits = None;
            }
        }
        anyhow::ensure!(can_hand_out(team, task_id), "no longer ready");
        anyhow::ensure!(!team.reserved_for_rework(member_id, Some(task_id)), "member reserved for rework");
        // U61: the first hand-out of an auto-accepted draft fixes it by Auto in
        // this same change, so its reports need no owner approval.
        super::ops::accept_phase_on_hand_out(team, task_id, events);
        // U45: a task for a role becomes its chosen member's before its
        // pipeline is taken from that member's role.
        if from == Some("") {
            if let Some(task) = team.task_mut(task_id).filter(|task| super::models::for_role(task)) {
                task.assignee = member_id.to_owned();
            }
        }
        // U36: the task's pipeline as the catalog has it at its first hand-out.
        super::pipeline::snapshot(team, task_id);
        let role = team.role_of(member_id);
        // U46: a clear may name a new conversation; the member follows it before
        // the hand-out so its identity checks keep the member (spec 3).
        if let Some((run, new)) = &conversation {
            if let Some(found) = team.members.iter_mut().find(|member| member.id == member_id) {
                if let Some(session) = found.session.as_mut().filter(|session| &session.run_id == run) {
                    session.conversation_id = new.clone();
                }
            }
        }
        // U46: the first hand-out that finds no native action tells the lead
        // once; every later one stays silent.
        if let Some(step) = prepare.iter().find(|step| step.result == "skipped") {
            if let Some(found) = team.members.iter_mut().find(|member| member.id == member_id) {
                if !found.context_noted {
                    found.context_noted = true;
                    events.push(Event::new("context_skipped", json!({"member":role,"task":task_id,"reason":step.reason})));
                }
            }
        }
        if let Some(step) = prepare.iter().find(|step| step.result == "done") {
            events.push(Event::new("context_prepared", json!({"member":role,"task":task_id,"action":step.to})));
        }
        let session = team.member(member_id).and_then(|m| m.session.clone()).context("no session")?;
        let member_host = team.member(member_id).map(|m| m.computer.clone()).unwrap_or_default();
        let roles = from.map(|from| (team.role_of(from), team.role_of(member_id)));
        let keeps = team.task(task_id).is_some_and(|task| team.rebalance_blocked(task));
        let task = team.task_mut(task_id).context("unknown task")?;
        // Given to a free member with the same role, unless the lead or you
        // chose the assignee in the meantime.
        let rebalanced = match from {
            Some(from) if task.assignee == from && !keeps => {
                task.assignee = member_id.to_owned();
                from.to_owned()
            }
            _ => String::new(),
        };
        anyhow::ensure!(task.assignee == member_id, "reassigned");
        let n = task.attempts.len() as u32 + 1;
        task.fresh_retry = false;
        task.attempts.push(Attempt {
            n,
            id: attempt_id.clone(),
            member: member_id.to_owned(),
            sent_at: now(),
            rebalanced_from: rebalanced.clone(),

            ..Default::default()
        });
        // Spec 1: the steps taken for this hand-out go with its attempt, in
        // their order: branch (U48), dependencies (task 155), model (U45),
        // context (U46).
        let mut prepared = super::models::take_for_attempt(task, member_id);
        prepared.extend(prepare.iter().cloned());
        prepared.sort_by_key(|step| match step.step.as_str() {
            "branch" => 0,
            "dependencies" => 1,
            "model" => 2,
            _ => 3,
        });
        for step in &mut prepared {
            step.member.clear();
        }
        task.attempts.last_mut().unwrap().prepare = prepared;
        if let (false, Some((from_role, to_role))) = (rebalanced.is_empty(), roles) {
            let cross_role = from_role != to_role;
            events.push(Event::new("rebalanced", json!({"task":task_id,"from":from_role,"to":to_role,"from_member":rebalanced,"to_member":member_id,"cross_role":cross_role})));
        }
        task.status = TaskStatus::Assigned;
        let task = team.task(task_id).unwrap();
        extra = continuation_extra(task);
        if member_host.is_empty() {
            if let (Some(continuation), Some(member)) = (task.continuation.as_ref(), team.member(member_id)) {
                if let Ok(folder) = member_folder_path(team, member) {
                    git_job = Some((folder, continuation.since));
                }
            }
        }
        message = Some((session, hand_off_text_rules(team, task, task.attempts.last().unwrap(), host, &member_host, &extra,
                &rules,
            )));
        if let Some(need) = team.member(member_id).and_then(|member| member.handoff.clone()) {
            let pulses = handoff::pulse_lines(team, member_id, &recent, now());
            let why = if need.why == "relaunch" { handoff::Why::Relaunch(&need) } else { handoff::Why::Swap(&need) };
            let built = handoff::build(team, member_id, why, &pulses, handoff::BRIEF_UNITS);
            if let Some((_, text)) = message.as_mut() {
                // Under the hand-out's own header line: the doorbell is made from it (task 134).
                *text = handoff::within(text, &built.text);
            }
            handoff = Some((built, need.why));
        }
        // D2: the finding goes with this one hand-out only.
        if !task.reopen_note.is_empty() {
            if let Some(task) = team.task_mut(task_id) {
                note = std::mem::take(&mut task.reopen_note);
            }
        }
        // Spec 5.6: "QA lead: Task to Tester" when a sub-lead hands it out.
        let reviewer = team.reviewer_of(member_id);
        let by = if reviewer.is_empty() { "Lead".to_owned() } else { team.role_of(&reviewer) };
        events.push(Event::new("handed_out", json!({"by":by,"task":task_id,"to":team.role_of(member_id),"attempt":n})));
        Ok(())
    })?;
    let (session, mut text) = message.unwrap();
    if let Some((folder, since)) = git_job {
        text += &git_continuation(&folder, since);
    }
    // Task 134: the hand-out's text goes into the working tier; the terminal
    // gets the doorbell under the attempt's own request id.
    let outcome = super::doorbell::post_and_ring(store, team_id, member_id, &session, &attempt_id, "hand_out", "lead", Urgency::Normal, &text);
    if let Err(error) = input_blocked(store, team_id, member_id, &session, &outcome) {
        eprintln!("hgs team worker: {member_id}: blocked input: {error}");
    }
    change(store, team_id, |team, events| {
        let member_state = team.member(member_id).map(|member| member.seen_phase.clone()).unwrap_or_default();
        let task = team.task_mut(task_id).context("unknown task")?;
        let Some(attempt) = task.attempts.iter_mut().find(|a| a.id == attempt_id && a.state == AttemptState::Delivering) else {
            return Ok(());
        };
        let n = attempt.n;
        match &outcome {
            Sent::Delivered => {
                attempt.state = AttemptState::Delivered;
                attempt.delivered_at = Some(now());
                task.status = TaskStatus::InProgress;
                task.continuation = None;
                events.push(Event::new("delivered", json!({"task":task_id,"attempt":n})));
            }
            Sent::Refused(error) => {
                attempt.state = AttemptState::Refused;
                attempt.error = error.clone();
                attempt.refusal_count = 1;
                attempt.refusal_reason = error.clone();
                attempt.refusal_state = member_state.clone();
                task.status = TaskStatus::Pending;
                if task.reopen_note.is_empty() {
                    task.reopen_note = note.clone();
                }
                events.push(Event::new("refused", json!({"task":task_id,"attempt":n,"error":error})));
            }
            Sent::Uncertain(error) => {
                attempt.state = AttemptState::Unconfirmed;
                attempt.error = error.clone();
                task.continuation = None;
                events.push(Event::new("unconfirmed", json!({"task":task_id,"attempt":n,"error":error})));
            }
        }
        // Task 165: one attempt carries the refusal count. A repeat refusal
        // with the same reason replaces the new record, so the task never
        // grows an attempt per 30-second tick.
        if let Sent::Refused(error) = &outcome {
            if let Some(current) = task.attempts.iter().position(|found| found.id == attempt_id) {
                let previous = (0..current).rev().find(|index| {
                    task.attempts[*index].state == AttemptState::Refused
                        && task.attempts[*index].member == member_id
                        && task.attempts[*index].refusal_reason == *error
                });
                if let Some(previous) = previous {
                    let count = task.attempts[current].refusal_count.max(1);
                    task.attempts[previous].refusal_count += count;
                    task.attempts[previous].sent_at = task.attempts[current].sent_at;
                    task.attempts[previous].error = error.clone();
                    let state = task.attempts[current].refusal_state.clone();
                    task.attempts[previous].refusal_state = state;
                    task.attempts[previous].id = attempt_id.clone();
                    task.attempts.remove(current);
                }
            }
        }
        if !matches!(outcome, Sent::Refused(_)) {
            if let Some((built, why)) = &handoff {
                handoff_sent(team, events, member_id, built, why);
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// Task 135: the brief went out with the session's first message (a refused
/// send leaves it pending): keep it with the team as the task's next version,
/// clear what the seat still had to be told, and say so in the journal; a brief
/// that cannot answer a control question of its task is a `handoff_gap` the lead
/// can see.
fn handoff_sent(team: &mut Team, events: &mut Vec<Event>, member_id: &str, built: &handoff::Built, why: &str) {
    let role = team.role_of(member_id);
    handoff::record(team, built, member_id, why, now());
    if let Some(member) = team.members.iter_mut().find(|member| member.id == member_id) {
        member.handoff = None;
    }
    events.push(Event::new("handoff_sent", json!({"member":role,"task":built.task,"why":why,"units":built.text.encode_utf16().count()})));
    if !built.misses.is_empty() {
        events.push(Event::new("handoff_gap", json!({"member":role,"task":built.task,"questions":built.misses})));
    }
}

/// Task 135: the team's last journal items, newest first, for the pulse lines
/// of a brief; a journal that cannot be read gives none.
fn recent_events(store: &Store, team_id: &str) -> Vec<Value> {
    store.events(team_id, None, 60).ok().and_then(|page| page["items"].as_array().cloned()).unwrap_or_default()
}

/// U36: gives the task's waiting stage to `member` and sends the stage
/// hand-off. The run is recorded before the send, so a crash leaves it with its
/// member; a refused send puts it back to wait, and its checkout goes.
/// U36 stage delivery. #581 N1: like a task hand-out it runs the model step
/// (when the stage's role lists models) and the context step, waiting for each
/// as a task does; it never runs the branch step.
fn stage_hand_out(store: &mut Store, team_id: &str, task_id: u32, member_id: &str, host: &str) -> anyhow::Result<()> {
    let team = store.load(team_id)?.with_context(|| format!("Unknown team {team_id}"))?;
    anyhow::ensure!(team.status == TeamStatus::Running, "the team is not running");
    let member = team.member(member_id).cloned().with_context(|| format!("Unknown member {member_id}"))?;
    let session = member.session.clone().context("no session")?;
    sample_member_usage(&team, member_id);
    let member_host = member.computer.clone();
    let (run_id, stage_role) = {
        let task = team.task(task_id).context("unknown task")?;
        let pipeline = task.pipeline_run.as_ref().context("The task has no pipeline")?;
        let run = pipeline.open_run().filter(|run| run.state == StageState::Waiting).context("no waiting stage")?;
        let stage = pipeline.stages.get(run.stage).context("no such stage")?;
        (run.id.clone(), stage.role.clone())
    };
    let mut steps: Vec<PrepareStep> = Vec::new();
    // #581 N1: a context action this stage started earlier: one look, then the
    // hand-out or the failure.
    let mut conversation: Option<(String, String)> = None;
    // #581 N1: a context action this stage already ran and saw confirm is this
    // hand-out's step; it is not started again.
    let mut context_done = false;
    if let Some(wait) = stage_run(&team, task_id, &run_id).and_then(|run| run.context.clone()) {
        let request = ContextRequest {
            action: wait.action.clone(),
            request_id: wait.request_id.clone(),
            run_id: wait.run_id.clone(),
            conversation: wait.conversation.clone(),
            started_at: wait.started_at,
        };
        match check_context(&session, &request) {
            ContextCheck::Waiting => return Ok(()),
            ContextCheck::Done { conversation: new } => {
                steps.push(context_step(&request.action, now()));
                stage_clear_context(store, &team, task_id, &run_id)?;
                conversation = new.map(|new| (request.run_id.clone(), new));
                context_done = true;
            }
            ContextCheck::Refused(reason) => {
                let step = context_failure(context_step(&request.action, now()), "refused", reason.clone());
                stage_record_step(store, &team, task_id, &run_id, &step, "stage_prepare_failed")?;
                stage_clear_context(store, &team, task_id, &run_id)?;
                steps.push(step);
            }
            ContextCheck::Uncertain(reason) => {
                let step = context_failure(context_step(&request.action, now()), "uncertain", reason.clone());
                stage_record_step(store, &team, task_id, &run_id, &step, "stage_prepare_failed")?;
                stage_clear_context(store, &team, task_id, &run_id)?;
                steps.push(step);
            }
        }
    }
    // #581 N1: a model switch this stage started earlier waits for the session
    // record to show the new model, as a task hand-out does.
    if let Some(pending) = stage_pending_model(&team, task_id) {
        let (model, effort) = session_model(&member, &session);
        if super::models::confirmed(&pending, &model, &effort) {
            let step = stage_mark_model(store, &team, task_id, &run_id, "done", "")?;
            steps.push(step);
        } else if now() - pending.at >= super::models::CONFIRM_SECONDS {
            let reason = format!("The switch to {} was not confirmed within {} s", pending.to, super::models::CONFIRM_SECONDS as u64);
            let step = stage_mark_model(store, &team, task_id, &run_id, "uncertain", &reason)?;
            stage_feed(store, &team, task_id, &run_id, &step, "stage_prepare_failed")?;
            steps.push(step);
        } else {
            return Ok(());
        }
    }
    // #581 N1: step 3 for the stage member - the models the stage's role lists.
    let role_models = super::catalog::models_for(&stage_role);
    if !role_models.is_empty() {
        let (model, effort) = session_model(&member, &session);
        let wanted = Task {
            models: role_models.iter().map(|choice| super::models::label(&choice.model, &choice.effort)).collect(),
            ..Default::default()
        };
        match super::models::fit(&member, &wanted, (&model, &effort)) {
            Some(super::models::Fit::Switch(choice)) => {
                let from = super::models::label(&model, &effort);
                stage_switch_model(store, &team, task_id, &run_id, &member, &session, &from, &choice)?;
                return Ok(());
            }
            Some(super::models::Fit::Ready) => {}
            None => {
                // The member cannot switch: the step is skipped, and the stage
                // runs on the model it has (the lead sees it in the feed).
                let step = PrepareStep {
                    step: "model".into(),
                    member: member.id.clone(),
                    from: super::models::label(&model, &effort),
                    to: "none".into(),
                    at: now(),
                    result: "skipped".into(),
                    reason: format!(
                        "{} cannot switch to the stage role's models {}",
                        agent_label(&member.agent),
                        role_models.iter().map(|choice| super::models::label(&choice.model, &choice.effort)).collect::<Vec<_>>().join(", ")
                    ),
                };
                stage_record_step(store, &team, task_id, &run_id, &step, "stage_prepare_skipped")?;
                steps.push(step);
            }
        }
    }
    // #581 N1: step 4 for the stage member - the context action its role or it
    // asks for.
    let action = context_before_task(&team, &member);
    if !action.is_empty() && !context_done {
        match run_context(&member, &session, &action) {
            ContextStart::Done { step, conversation: new } => {
                steps.push(step);
                if let Some((at, new)) = new.map(|new| (session.run_id.clone(), new)) {
                    conversation = Some((at, new));
                }
            }
            ContextStart::Started(request) => {
                stage_context_wait(store, &team, task_id, &run_id, member_id, &request)?;
                return Ok(());
            }
            ContextStart::Failed { step } => {
                steps.push(step.clone());
                stage_record_step(store, &team, task_id, &run_id, &step, "stage_prepare_failed")?;
            }
        }
    }
    if let Some((run, new)) = &conversation {
        stage_conversation(store, &team, member_id, run, new)?;
    }
    // The folder, the checkout and the file states, outside the store's lock.
    let prepared = super::stages::prepare(&team, task_id)?;
    let fresh = store.load(team_id)?.context("Unknown team")?;
    let rules = super::super::iron_rules::for_team(
        &fresh,
        fresh.member(member_id).context("Unknown member")?,
    )?;
    let mut message = None;
    let recorded = change(store, team_id, |team, events| {
        let run_id = super::pipeline::give_stage(team, task_id, member_id)?;
        super::stages::record(team, task_id, &run_id, &prepared);
        if let Some(run) = stage_run_mut(team, task_id, &run_id) {
            // #581 N1: the steps this hand-out took go with its stage run.
            run.prepare = steps.clone();
            run.context = None;
        }
        let task = team.task(task_id).context("unknown task")?;
        let run = task.pipeline_run.as_ref().and_then(|run| run.open_run()).context("no open stage")?;
        let report_host = if member_host.is_empty() { String::new() } else { host.to_owned() };
        let mut text = super::stages::hand_off_text(team, task, run, &prepared, &report_host);
        if !rules.is_empty() {
            text.push('\n');
            text.push_str(&rules);
        }
        message = Some((run_id.clone(), text));
        events.push(Event::new(
            "stage_handed_out",
            json!({"task":task_id,"stage":run.kind,"round":run.round,"to":team.role_of(member_id),"folder":prepared.folder,
                "checkout":!prepared.checkout.is_empty(),"holds":prepared.holds}),
        ));
        Ok(())
    });
    if let Err(error) = recorded {
        let _ = super::stages::clean_up(&StageRun { checkout: prepared.checkout.clone(), checkout_repo: prepared.checkout_repo.clone(), temp: prepared.temp.clone(), ..Default::default() });
        return Err(error);
    }
    let (run_id, text) = message.context("no stage message")?;
    let outcome = super::doorbell::post_and_ring(store, team_id, member_id, &session, &run_id, "stage", "lead", Urgency::Normal, &text);
    if let Err(error) = input_blocked(store, team_id, member_id, &session, &outcome) {
        eprintln!("hgs team worker: {member_id}: blocked input: {error}");
    }
    change(store, team_id, |team, events| {
        let Some(task) = team.task_mut(task_id) else { return Ok(()) };
        let pinned = task.pipeline_run.as_ref().and_then(|run| run.open_run()).and_then(|run| task.pipeline_run.as_ref()?.stages.get(run.stage)).map(|stage| stage.member.clone()).unwrap_or_default();
        let Some(run) = task.pipeline_run.as_mut().and_then(|run| run.open_run_mut()).filter(|run| run.id == run_id) else { return Ok(()) };
        match &outcome {
            Sent::Delivered => {
                run.delivered_at = Some(now());
                events.push(Event::new("stage_delivered", json!({"task":task_id,"stage":run.kind,"round":run.round})));
            }
            Sent::Refused(error) => {
                // Back to wait for a member; nothing reached this one.
                let _ = super::stages::clean_up(run);
                run.state = StageState::Waiting;
                run.member = if pinned == run.member { pinned } else { String::new() };
                run.delivered_at = None;
                run.folder.clear();
                run.checkout.clear();
                run.temp.clear();
                run.holds = false;
                events.push(Event::new("stage_refused", json!({"task":task_id,"stage":run.kind,"error":error})));
            }
            Sent::Uncertain(error) => {
                // #630 N7: a hand-off that may not have arrived is not
                // confirmed; after a grace it goes back to Waiting.
                run.unconfirmed = true;
                events.push(Event::new("stage_unconfirmed", json!({"task":task_id,"stage":run.kind,"error":error})));
            }
        }
        Ok(())
    })
}

/// #630 N7: takes a stalled stage back to Waiting and frees its checkout.
/// #635 N2: the pipeline keeps it while the stage's own run is live.
fn stall_stage_command(store: &mut Store, team: &Team, task_id: u32, run_id: &str, reason: &str) -> anyhow::Result<()> {
    change(store, &team.id, |team, events| {
        super::pipeline::stall_stage(team, task_id, run_id, reason, events);
        Ok(())
    })
}

/// #635 N3: whether an unconfirmed hand-off shows evidence it did not arrive:
/// its member is idle and free since delivery, no stage question is open, and
/// no run claim stands under the run id in its folder or the stage's.
fn hand_off_missing(team: &Team, run: &StageRun, found: &Seen, delivered: f64) -> bool {
    if !found.free || found.reply_at.unwrap_or(0.0) > delivered || found.turn_started.is_some_and(|at| at > delivered) {
        return false;
    }
    if run.questions.iter().any(|question| question.open()) {
        return false;
    }
    let claim = |folder: &str| super::outbox::running(Path::new(folder), &run.id).is_some();
    if let Some(member) = team.member(&run.member) {
        if let Ok(folder) = member_folder_path(team, member) {
            if claim(&folder) {
                return false;
            }
        }
    }
    if !run.folder.is_empty() && claim(&run.folder) {
        return false;
    }
    if super::super::builds::record(&run.id, now()).is_some_and(|record| record["status"] == "running") {
        return false;
    }
    true
}

/// #635 N1: records once that a stage member's turn ended without a verdict,
/// so the lead's inbox has the stalled item; the run stays Delivered.
fn stage_silent_command(store: &mut Store, team: &Team, task_id: u32, run_id: &str) -> anyhow::Result<()> {
    let role = team
        .task(task_id)
        .and_then(|task| task.pipeline_run.as_ref())
        .and_then(|pipeline| pipeline.runs.iter().find(|run| run.id == run_id))
        .map(|run| team.name_of(&run.member))
        .unwrap_or_default();
    change(store, &team.id, |team, events| {
        let Some(run) = stage_run_mut(team, task_id, run_id) else { return Ok(()) };
        if !run.silent.is_empty() || run.state != StageState::Delivered {
            return Ok(());
        }
        run.silent = "its member ended its turn without a verdict".to_owned();
        let (stage, round) = (run.kind.clone(), run.round);
        events.push(Event::new("stage_silent", json!({"task":task_id,"stage":stage,"round":round,"member":role})));
        Ok(())
    })
}

/// #581 N1: the open stage run with this id, mutably.
fn stage_run_mut<'a>(team: &'a mut Team, task_id: u32, run_id: &str) -> Option<&'a mut StageRun> {
    team.task_mut(task_id)?.pipeline_run.as_mut()?.runs.iter_mut().find(|run| run.id == run_id)
}

/// #581 N1: the open stage run with this id.
fn stage_run<'a>(team: &'a Team, task_id: u32, run_id: &str) -> Option<&'a StageRun> {
    team.task(task_id)?.pipeline_run.as_ref()?.runs.iter().find(|run| run.id == run_id)
}

/// #581 N1: a stage's model switch still waiting for its session record.
fn stage_pending_model(team: &Team, task_id: u32) -> Option<PrepareStep> {
    let run = team.task(task_id)?.pipeline_run.as_ref()?.open_run()?;
    run.prepare.iter().rev().find(|step| step.step == "model" && step.result == "pending").cloned()
}

/// #581 N1: the session's model and effort, or the member's launch ones.
fn session_model(member: &Member, session: &SessionRef) -> (String, String) {
    let record = inspect_reply(session).ok();
    let model = record.as_ref().and_then(|record| record["model"].as_str()).unwrap_or_default().to_owned();
    let effort = record.as_ref().and_then(|record| record["effort"].as_str()).unwrap_or_default().to_owned();
    if model.is_empty() {
        let (model, effort) = super::models::launch_model(member);
        (model.to_owned(), effort.to_owned())
    } else {
        (model, effort)
    }
}

/// #581 N1: records a step on a stage run and its feed event.
fn stage_record_step(store: &mut Store, team: &Team, task_id: u32, run_id: &str, step: &PrepareStep, event: &'static str) -> anyhow::Result<()> {
    change(store, &team.id, |team, events| {
        let Some(run) = stage_run_mut(team, task_id, run_id) else { return Ok(()) };
        run.prepare.push(step.clone());
        let extra = run.prepare.len().saturating_sub(20);
        run.prepare.drain(..extra);
        events.push(Event::new(
            event,
            json!({"task":task_id,"stage":run.kind,"round":run.round,"step":step.step,"result":step.result,"reason":step.reason}),
        ));
        Ok(())
    })
}

/// #581 N1: one feed line for a stage step already on its run.
fn stage_feed(store: &mut Store, team: &Team, task_id: u32, run_id: &str, step: &PrepareStep, event: &'static str) -> anyhow::Result<()> {
    change(store, &team.id, |team, events| {
        let Some(run) = stage_run_mut(team, task_id, run_id) else { return Ok(()) };
        events.push(Event::new(
            event,
            json!({"task":task_id,"stage":run.kind,"round":run.round,"step":step.step,"result":step.result,"reason":step.reason}),
        ));
        Ok(())
    })
}

/// #581 N1: records the model step of a stage as pending and runs `hgs settings`
/// for it; the next tick waits for the session record to show the model.
#[allow(clippy::too_many_arguments)]
fn stage_switch_model(
    store: &mut Store,
    team: &Team,
    task_id: u32,
    run_id: &str,
    member: &Member,
    session: &SessionRef,
    from: &str,
    choice: &super::model::ModelChoice,
) -> anyhow::Result<()> {
    let to = super::models::label(&choice.model, &choice.effort);
    let role = team.role_of(&member.id);
    change(store, &team.id, |team, events| {
        let Some(run) = stage_run_mut(team, task_id, run_id) else { return Ok(()) };
        run.prepare.push(PrepareStep {
            step: "model".into(),
            member: member.id.clone(),
            from: from.to_owned(),
            to: to.clone(),
            at: now(),
            result: "pending".into(),
            reason: String::new(),
        });
        run.member = member.id.clone();
        events.push(Event::new("stage_model_switch", json!({"task":task_id,"stage":run.kind,"round":run.round,"member":role,"from":from,"to":to})));
        Ok(())
    })?;
    let mut input = json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_run_id":session.run_id,
        "expected_conversation_id":session.conversation_id,"model":choice.model});
    if !choice.effort.is_empty() {
        input["effort"] = json!(choice.effort);
    }
    let outcome = match hgs(&session.host, &["settings", &session.name, "--json"], Some(&input), Duration::from_secs(120)) {
        Ok(output) if output.status.success() => return Ok(()),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let line = stderr.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("the settings change was refused");
            ("refused", line.chars().take(300).collect::<String>())
        }
        Err(error) => ("uncertain", error.to_string().chars().take(300).collect()),
    };
    let step = stage_mark_model(store, team, task_id, run_id, outcome.0, &outcome.1)?;
    stage_feed(store, team, task_id, run_id, &step, "stage_prepare_failed")
}

/// #581 N1: ends the stage's pending model step with a result; returns it.
fn stage_mark_model(store: &mut Store, team: &Team, task_id: u32, run_id: &str, result: &str, reason: &str) -> anyhow::Result<PrepareStep> {
    let mut marked = None;
    change(store, &team.id, |team, _| {
        let Some(run) = stage_run_mut(team, task_id, run_id) else { return Ok(()) };
        if let Some(step) = run.prepare.iter_mut().rev().find(|step| step.step == "model" && step.result == "pending") {
            step.result = result.into();
            step.reason = reason.to_owned();
            marked = Some(step.clone());
        }
        Ok(())
    })?;
    Ok(marked.unwrap_or_default())
}

/// #581 N1: reserves the stage for the member whose context action it started.
fn stage_context_wait(store: &mut Store, team: &Team, task_id: u32, run_id: &str, member_id: &str, request: &ContextRequest) -> anyhow::Result<()> {
    change(store, &team.id, |team, _| {
        let Some(run) = stage_run_mut(team, task_id, run_id) else { return Ok(()) };
        run.member = member_id.to_owned();
        run.context = Some(TaskWait {
            waits_for: "context".into(),
            on: vec!["context".into()],
            state: "waiting".into(),
            action: request.action.clone(),
            request_id: request.request_id.clone(),
            run_id: request.run_id.clone(),
            conversation: request.conversation.clone(),
            started_at: request.started_at,
            ..Default::default()
        });
        Ok(())
    })
}

/// #581 N1: the stage's context action ended or failed; its wait is over.
fn stage_clear_context(store: &mut Store, team: &Team, task_id: u32, run_id: &str) -> anyhow::Result<()> {
    change(store, &team.id, |team, _| {
        if let Some(run) = stage_run_mut(team, task_id, run_id) {
            run.context = None;
        }
        Ok(())
    })
}

/// U46: a clear may name a new conversation; the member follows it before the
/// stage's hand-out, so its identity checks keep the member.
fn stage_conversation(store: &mut Store, team: &Team, member_id: &str, run_id: &str, conversation: &str) -> anyhow::Result<()> {
    change(store, &team.id, |team, _| {
        if let Some(member) = team.members.iter_mut().find(|member| member.id == member_id) {
            if let Some(session) = member.session.as_mut().filter(|session| session.run_id == run_id) {
                session.conversation_id = conversation.to_owned();
            }
        }
        Ok(())
    })
}

/// U36: removes an ended stage's checkout and temporary folder, then forgets
/// them; a failure is kept for the next tick and shown in the feed once.
fn stage_clean_up(store: &mut Store, team: &Team, task_id: u32, run_id: &str) -> anyhow::Result<()> {
    let Some(run) = team.task(task_id).and_then(|task| task.pipeline_run.as_ref()).and_then(|run| run.runs.iter().find(|run| run.id == run_id)).cloned() else {
        return Ok(());
    };
    let removed = super::stages::clean_up(&run);
    change(store, &team.id, |team, events| {
        let Some(found) = team.task_mut(task_id).and_then(|task| task.pipeline_run.as_mut()).and_then(|run| run.runs.iter_mut().find(|run| run.id == run_id)) else {
            return Ok(());
        };
        match &removed {
            Ok(()) => {
                if !found.checkout.is_empty() {
                    events.push(Event::new("stage_checkout_removed", json!({"task":task_id,"stage":found.kind,"round":found.round})));
                }
                found.checkout.clear();
                found.temp.clear();
            }
            Err(error) => anyhow::bail!("{error:#}"),
        }
        Ok(())
    })
}

/// #538 N4: a closed stage's own run ended: release its folder and let its
/// checkout and temporary folder go (the next tick's StageCleanup removes them).
fn stage_run_ended(store: &mut Store, team: &Team, task_id: u32, run_id: &str) -> anyhow::Result<()> {
    change(store, &team.id, |team, events| {
        if !super::stages::release(team, task_id, run_id) {
            return Ok(());
        }
        let (kind, round) = team
            .task(task_id)
            .and_then(|task| task.pipeline_run.as_ref())
            .and_then(|pipeline| pipeline.runs.iter().find(|run| run.id == run_id))
            .map(|run| (run.kind.clone(), run.round))
            .unwrap_or_default();
        events.push(Event::new("stage_run_ended", json!({"task":task_id,"stage":kind,"round":round,"run":run_id})));
        Ok(())
    })
}

/// U46: records a prepared hand-out that waits for its context action. The
/// task's `waits` names the request, so the next tick - or a restarted
/// worker - only reads the session record; the action is not run again.
fn record_context_start(
    store: &mut Store,
    team_id: &str,
    task_id: u32,
    member_id: &str,
    from: Option<&str>,
    request: ContextRequest,
) -> anyhow::Result<()> {
    change(store, team_id, |team, events| {
        anyhow::ensure!(can_hand_out(team, task_id), "no longer ready");
        let roles = from.map(|from| (team.role_of(from), team.role_of(member_id)));
        let keeps = team.task(task_id).is_some_and(|task| team.rebalance_blocked(task));
        let task = team.task_mut(task_id).context("unknown task")?;
        let rebalanced = match from {
            Some(from) if task.assignee == from && !keeps => {
                task.assignee = member_id.to_owned();
                from.to_owned()
            }
            _ => String::new(),
        };
        anyhow::ensure!(task.assignee == member_id, "reassigned");
        task.waits = Some(TaskWait {
            action: request.action,
            request_id: request.request_id,
            run_id: request.run_id,
            conversation: request.conversation,
            started_at: request.started_at,
            ..TaskWait::zerus("context", "waiting", "", now())
        });
        if let (false, Some((from_role, to_role))) = (rebalanced.is_empty(), roles) {
            let cross_role = from_role != to_role;
            events.push(Event::new("rebalanced", json!({"task":task_id,"from":from_role,"to":to_role,"from_member":rebalanced,"to_member":member_id,"cross_role":cross_role})));
        }
        Ok(())
    })
}

/// U46: records a context action that refused or stayed unconfirmed before the
/// hand-out. The attempt is kept for the feed and the task waits for Zerus:
/// `plan::ready` skips it and the lead's inbox names the retry.
fn record_waits(
    store: &mut Store,
    team_id: &str,
    task_id: u32,
    member_id: &str,
    from: Option<&str>,
    step: PrepareStep,
) -> anyhow::Result<()> {
    change(store, team_id, |team, events| {
        anyhow::ensure!(can_hand_out(team, task_id), "no longer ready");
        let roles = from.map(|from| (team.role_of(from), team.role_of(member_id)));
        let keeps = team.task(task_id).is_some_and(|task| team.rebalance_blocked(task));
        let task = team.task_mut(task_id).context("unknown task")?;
        let rebalanced = match from {
            Some(from) if task.assignee == from && !keeps => {
                task.assignee = member_id.to_owned();
                from.to_owned()
            }
            _ => String::new(),
        };
        anyhow::ensure!(task.assignee == member_id, "reassigned");
        let n = task.attempts.len() as u32 + 1;
        task.fresh_retry = false;
        task.attempts.push(Attempt {
            n,
            id: uuid::Uuid::new_v4().to_string(),
            member: member_id.to_owned(),
            state: AttemptState::Refused,
            sent_at: now(),
            rebalanced_from: rebalanced.clone(),
            error: step.reason.clone(),
            prepare: vec![step.clone()],
            ..Default::default()
        });
        task.waits = Some(TaskWait {
            attempt: n,
            ..TaskWait::zerus("context", if step.result == "uncertain" { "uncertain" } else { "refused" }, &step.reason, step.at)
        });
        if let (false, Some((from_role, to_role))) = (rebalanced.is_empty(), roles) {
            let cross_role = from_role != to_role;
            events.push(Event::new("rebalanced", json!({"task":task_id,"from":from_role,"to":to_role,"from_member":rebalanced,"to_member":member_id,"cross_role":cross_role})));
        }
        events.push(Event::new(
            "prepare_failed",
            json!({"member":team.role_of(member_id),"task":task_id,"step":step.step,"result":step.result,"reason":step.reason}),
        ));
        Ok(())
    })
}

/// U46: a started context action that refused or stayed unconfirmed. The wait
/// already recorded on the task becomes the reason it waits, with its attempt.
fn fail_context(store: &mut Store, team_id: &str, task_id: u32, member_id: &str, step: PrepareStep) -> anyhow::Result<()> {
    change(store, team_id, |team, events| {
        let Some(task) = team.task_mut(task_id) else { return Ok(()) };
        let Some(wait) = task.waits.as_mut().filter(|wait| wait.state == "waiting") else { return Ok(()) };
        // model.rs: only an explicit fresh rejection may bypass shared Output:
        // an automatic preparation refusal never keeps the exemption.
        task.fresh_retry = false;
        let n = task.attempts.len() as u32 + 1;
        wait.state = if step.result == "uncertain" { "uncertain".into() } else { "refused".into() };
        wait.reason = step.reason.clone();
        wait.at = now();
        wait.attempt = n;
        task.attempts.push(Attempt {
            n,
            id: uuid::Uuid::new_v4().to_string(),
            member: member_id.to_owned(),
            state: AttemptState::Refused,
            sent_at: now(),
            error: step.reason.clone(),
            prepare: vec![step.clone()],
            ..Default::default()
        });
        events.push(Event::new(
            "prepare_failed",
            json!({"member":team.role_of(member_id),"task":task_id,"step":step.step,"result":step.result,"reason":step.reason}),
        ));
        Ok(())
    })
}

/// U46: one tick's look at a hand-out that waits for its context action. It
/// reads the session record once: the hand-out itself when the action is
/// confirmed, or the waiting reason when it failed or passed its bound.
fn check_context_step(store: &mut Store, team_id: &str, task_id: u32, member_id: &str, host: &str) -> anyhow::Result<()> {
    let team = store.load(team_id)?.with_context(|| format!("Unknown team {team_id}"))?;
    let Some(wait) = team.task(task_id).and_then(|task| task.waits.clone()).filter(|wait| wait.state == "waiting") else {
        return Ok(());
    };
    let Some(session) = team.member(member_id).and_then(|member| member.session.clone()) else { return Ok(()) };
    let request = ContextRequest {
        action: wait.action,
        request_id: wait.request_id,
        run_id: wait.run_id,
        conversation: wait.conversation,
        started_at: wait.started_at,
    };
    match check_context(&session, &request) {
        ContextCheck::Waiting => Ok(()),
        ContextCheck::Done { conversation } => {
            let preparation = Preparation {
                steps: vec![context_step(&request.action, now())],
                conversation: conversation.map(|new| (request.run_id.clone(), new)),
                resume: true,
            };
            if member_id == team.lead {
                lead_hand_out_now(store, team_id, task_id, preparation)
            } else {
                hand_out_now(store, team_id, task_id, member_id, None, host, preparation)
            }
        }
        // Task 254: only a cold cache asked for it: the task goes out anyway.
        ContextCheck::Refused(reason) | ContextCheck::Uncertain(reason) if wait.reason == COLD_REASON => {
            cold_compact_failed(store, &team, task_id, member_id, &reason);
            if member_id == team.lead {
                lead_hand_out_now(store, team_id, task_id, Preparation::default())
            } else {
                hand_out_now(store, team_id, task_id, member_id, None, host, Preparation::default())
            }
        }
        ContextCheck::Refused(reason) => {
            let step = context_failure(context_step(&request.action, now()), "refused", reason);
            fail_context(store, team_id, task_id, member_id, step)
        }
        ContextCheck::Uncertain(reason) => {
            let step = context_failure(context_step(&request.action, now()), "uncertain", reason);
            fail_context(store, team_id, task_id, member_id, step)
        }
    }
}

/// The message that tells the lead a hand-out never started (A1). Zerus never
/// re-sends the hand-out and never presses keys: the input may still be waiting
/// in the member's terminal.
pub(crate) fn start_late_text(team: &Team, task: &Task, attempt: &Attempt, now: f64) -> String {
    let role = team.name_of(&attempt.member);
    let minutes = (((now - attempt.delivered_at.unwrap_or(attempt.sent_at)) / 60.0).round() as u64).max(1);
    format!(
        "[Zerus team \u{b7} from: Zerus \u{b7} task: #{} \u{b7} not started]\n\
         #{} was delivered to {role} {minutes} min ago and has not started; its input may be waiting in the terminal.\n\
         Zerus does not re-send a hand-off: check its session, then message it or retry the task (hgs guide team).",
        task.id, task.id
    )
}

/// The message that gives the member the lead's answer (A3).
pub(crate) fn answer_to_member_text(task: &Task, question: &Question) -> String {
    format!(
        "[Zerus team \u{b7} from: {} \u{b7} task: #{} \u{b7} answer {}]\n\
         Answer to your question on task #{} {}:\n\n{}\n\n\
         Continue with the task; report with hgs team report --json (your task and attempt: hgs team current --json).",
        question.by, task.id, question.id, task.id, task.title, question.answer
    )
}

/// The message that tells the lead a finished turn has waited too long for an
/// answer or for Background work (N5).
pub(crate) fn blocked_text(team: &Team, task: &Task, attempt: &Attempt, now: f64) -> String {
    let role = team.name_of(&attempt.member);
    let minutes = (((now - attempt.blocked_since.unwrap_or(now)) / 60.0).round() as u64).max(1);
    if let Some(question) = attempt.open_question() {
        format!(
            "[Zerus team \u{b7} from: Zerus \u{b7} task: #{} \u{b7} waiting {minutes} min]\n\
             {role}'s turn on task #{} {} has waited {minutes} min for an answer to question {}:\n\n{}\n\n\
             Answer with hgs team reply --json; the attempt stays open until then.",
            task.id, task.id, task.title, question.id, question.text
        )
    } else {
        format!(
            "[Zerus team \u{b7} from: Zerus \u{b7} task: #{} \u{b7} waiting {minutes} min]\n\
             {role}'s turn on task #{} {} has not ended for {minutes} min: Background work or a Zerus build still runs. \
             Zerus takes no report while it runs; message the member or check its session.",
            task.id, task.id, task.title
        )
    }
}

/// Tells the free lead once that a finished turn has been blocked too long (N5).
fn tell_lead_blocked(store: &mut Store, team: &Team, task_id: u32) -> anyhow::Result<()> {
    let task = team.task(task_id).context("unknown task")?;
    let attempt = task.attempts.last().context("the task has no attempt")?.clone();
    let lead = team.member(&team.lead).and_then(|m| m.session.clone()).context("the lead has no session")?;
    let outcome = send(&lead, &uuid::Uuid::new_v4().to_string(), &blocked_text(team, task, &attempt, now()));
    if matches!(outcome, Sent::Refused(_)) {
        return Ok(());
    }
    change(store, &team.id, |team, events| {
        let role = team.role_of(&attempt.member);
        let found = team.task_mut(task_id).and_then(|t| t.attempts.last_mut()).context("unknown attempt")?;
        found.told_lead.push("blocked".to_owned());
        events.push(Event::new("lead_told", json!({"member":role,"task":task_id,"kind":"blocked"})));
        Ok(())
    })
}

/// One message for the lead: the open tasks Zerus gave to another member,
/// with the same role or (task 259) across roles, and to whom.
pub(crate) fn rebalanced_text(team: &Team, tasks: &[u32]) -> String {
    let mut text = String::from("[Zerus team \u{b7} from: Zerus \u{b7} tasks moved]\n");
    for task in tasks.iter().filter_map(|id| team.task(*id)) {
        if let Some(attempt) = task.open_attempt() {
            text += &format!(
                "#{} {}: given to {} instead of {}, which could not take it now.\n",
                task.id,
                task.title,
                team.name_of(&attempt.member),
                team.name_of(&attempt.rebalanced_from)
            );
        }
    }
    text + "Zerus hands a ready task with free outputs to an idle member of a compatible role; the plan's member \
            and the exact role stay the preference. Nothing to do unless you want another member \
            (hgs team reassign keeps your choice)."
}

fn tell_lead_rebalanced(store: &mut Store, team: &Team, tasks: &[u32]) -> anyhow::Result<()> {
    let lead = team.member(&team.lead).and_then(|m| m.session.clone()).context("the lead has no session")?;
    let outcome = send(&lead, &uuid::Uuid::new_v4().to_string(), &rebalanced_text(team, tasks));
    if matches!(outcome, Sent::Refused(_)) {
        return Ok(());
    }
    change(store, &team.id, |team, _| {
        for id in tasks {
            if let Some(attempt) = team.task_mut(*id).and_then(|t| t.attempts.iter_mut().rev().find(|a| a.state.open())) {
                attempt.told_lead.push("rebalanced".to_owned());
            }
        }
        Ok(())
    })
}

/// Tells the free lead once that a hand-out has not started (A1).
fn tell_lead_start_late(store: &mut Store, team: &Team, task_id: u32) -> anyhow::Result<()> {
    let task = team.task(task_id).context("unknown task")?;
    let attempt = task.open_attempt().context("no open attempt")?.clone();
    let lead = team.member(&team.lead).and_then(|m| m.session.clone()).context("the lead has no session")?;
    let outcome = send(&lead, &uuid::Uuid::new_v4().to_string(), &start_late_text(team, task, &attempt, now()));
    if matches!(outcome, Sent::Refused(_)) {
        return Ok(());
    }
    change(store, &team.id, |team, events| {
        let role = team.role_of(&attempt.member);
        let found = team.task_mut(task_id).and_then(|t| t.attempts.iter_mut().find(|a| a.id == attempt.id)).context("unknown attempt")?;
        found.told_lead.push("start_late".to_owned());
        events.push(Event::new("lead_told", json!({"member":role,"task":task_id,"kind":"start_late"})));
        Ok(())
    })
}

/// Task 36: the message that wakes a member whose own run settled after its
/// turn ended.
pub(crate) fn run_settled_text(task: &Task, attempt: &Attempt) -> String {
    let what = if attempt.awaited_label.is_empty() { "build".to_owned() } else { format!("build ({})", attempt.awaited_label) };
    let read = if attempt.awaited_run == "build" {
        "Read its result".to_owned()
    } else {
        format!("Read its result (ask again with request_id {})", attempt.awaited_run)
    };
    format!(
        "[Zerus team] Your {what} for task #{} {} has finished. {read}, then go on with the task and finish it with hgs team report (attempt {}). Zerus does not take a reply that only says you are waiting as your report.",
        task.id, task.title, attempt.id
    )
}

/// Task 36: the awaited run settled. With the member's turn over it is woken
/// once and its finished turn starts again: the report timeout counts anew
/// from the wake, so a member that still does nothing gets No report then.
fn run_settled(store: &mut Store, team: &Team, task_id: u32, wake: bool) -> anyhow::Result<()> {
    let task = team.task(task_id).context("unknown task")?;
    let attempt = task.attempts.last().context("no attempt")?;
    if attempt.state != AttemptState::Delivered || attempt.awaited_run.is_empty() {
        return Ok(());
    }
    let run = attempt.awaited_run.clone();
    let mut sent = None;
    if wake {
        let session = team.member(&attempt.member).and_then(|m| m.session.clone()).context("the member has no session")?;
        let notice = uuid::Uuid::new_v4().to_string();
        sent = Some(send(&session, &notice, &run_settled_text(task, attempt)));
    }
    let role = team.role_of(&attempt.member);
    change(store, &team.id, |team, events| {
        let Some(attempt) = team.task_mut(task_id).and_then(|t| t.attempts.last_mut()) else { return Ok(()) };
        if attempt.state != AttemptState::Delivered || attempt.awaited_run != run {
            return Ok(());
        }
        attempt.awaited_run.clear();
        attempt.awaited_label.clear();
        if let Some(sent) = sent {
            // Woken once, whatever the delivery: a refused wake leaves the
            // finished turn to the usual No report.
            if !matches!(sent, Sent::Refused(_)) {
                attempt.turn_ended_at = None;
                attempt.turn_reply.clear();
                attempt.report_ready_at = None;
                attempt.blocked_since = None;
            }
            events.push(Event::new(
                "run_settled_woken",
                json!({"member":role,"task":task_id,"attempt":attempt.n,"run":run,"delivered":!matches!(sent, Sent::Refused(_))}),
            ));
        }
        Ok(())
    })
}

/// Gives the member the lead's answer as a message, once (A3).
fn send_answer(store: &mut Store, team: &Team, task_id: u32, question_id: &str) -> anyhow::Result<()> {
    let task = team.task(task_id).context("unknown task")?;
    // #630 N6: the answer may belong to an attempt or to a stage member's run.
    let (asked_by, question) = task
        .attempts
        .iter()
        .rev()
        .find_map(|attempt| {
            attempt
                .questions
                .iter()
                .find(|question| question.id == question_id && !question.open() && question.notice == Notice::Pending)
                .map(|question| (attempt.member.clone(), question.clone()))
        })
        .or_else(|| {
            task.pipeline_run.as_ref().and_then(|pipeline| {
                pipeline.runs.iter().rev().find_map(|run| {
                    run.questions
                        .iter()
                        .find(|question| question.id == question_id && !question.open() && question.notice == Notice::Pending)
                        .map(|question| (run.member.clone(), question.clone()))
                })
            })
        })
        .context("the answer is no longer waiting")?;
    let session = team.member(&asked_by).and_then(|m| m.session.clone()).context("the member has no session")?;
    let notice_id = uuid::Uuid::new_v4().to_string();
    let outcome = send(&session, &notice_id, &answer_to_member_text(task, &question));
    if matches!(outcome, Sent::Refused(_)) {
        return Ok(());
    }
    change(store, &team.id, |team, _| {
        let task = team.task_mut(task_id);
        let question = task.and_then(|task| {
            task.attempts
                .iter_mut()
                .flat_map(|attempt| attempt.questions.iter_mut())
                .find(|question| question.id == question_id)
                .or_else(|| task.pipeline_run.as_mut().and_then(|pipeline| pipeline.runs.iter_mut().flat_map(|run| run.questions.iter_mut()).find(|question| question.id == question_id)))
        });
        if let Some(question) = question {
            if question.notice == Notice::Pending {
                question.notice = match outcome {
                    Sent::Delivered => Notice::Sent,
                    Sent::Uncertain(_) => Notice::Unconfirmed,
                    Sent::Refused(_) => Notice::Pending,
                };
                question.notice_id = notice_id.clone();
            }
        }
        Ok(())
    })
}

/// Reads a member's native question or permission request into the inbox
/// (C2). Nothing is sent: the coalesced inbox message carries it. Inspection
/// runs at most every 20 seconds per member; a session that no longer asks
/// clears the stored request.
/// When each member's questions were last read (one read per 20 seconds).
static PERMISSION_CHECKED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, f64>>> = std::sync::OnceLock::new();

#[cfg(test)]
fn sync_permission_forget(team: &str, member: &str) {
    PERMISSION_CHECKED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).remove(&format!("{team}\n{member}"));
}

fn sync_permission(store: &mut Store, team: &Team, member_id: &str) -> anyhow::Result<()> {
    let member = team.member(member_id).context("unknown member")?;
    let key = format!("{}\n{member_id}", team.id);
    {
        let mut checked = PERMISSION_CHECKED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        if checked.get(&key).is_some_and(|at| now() - at < 20.0) {
            return Ok(());
        }
        checked.insert(key, now());
    }
    let session = member.session.as_ref().context("the member has no session")?;
    let output = hgs(&session.host, &["inspect", &session.name, "--skip-processes"], None, Duration::from_secs(45))?;
    anyhow::ensure!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr).trim());
    let record: Value = serde_json::from_slice(&output.stdout).context("invalid inspect output")?;
    let cards = record["pending_questions"].as_array().cloned().unwrap_or_default();
    let same = member.pending.as_ref().and_then(|pending| {
        cards
            .iter()
            .find(|card| {
                card["question_id"].as_str() == Some(pending.question_id.as_str())
                    && card["question_hash"].as_str() == Some(pending.question_hash.as_str())
            })
            .cloned()
    });
    let ask = if same.is_some() {
        None
    } else {
        cards
            .iter()
            .find(|card| {
                // One that waited for the Terminal and can be answered again
                // goes back to the lead.
                card["can_answer"] != false
                    && (!member.asked.contains(&format!(
                        "{}:{}",
                        card["question_id"].as_str().unwrap_or_default(),
                        card["question_hash"].as_str().unwrap_or_default()
                    )) || member.terminal_question.as_ref().is_some_and(|seen| card["question_id"].as_str() == Some(seen.question_id.as_str())))
            })
            .map(|card| PendingQuestion {
                question_id: card["question_id"].as_str().unwrap_or_default().to_owned(),
                question_hash: card["question_hash"].as_str().unwrap_or_default().to_owned(),
                approval: card["approval"] == true,
                at: now(),
                card: card.clone(),
            })
    };
    // A request Zerus cannot answer now (Kimi's Edit and Write approvals, or
    // one behind another request) is never the lead's to answer, but it is
    // never silent either. Kimi's cards say so on every inspection, so one
    // that can be answered again leaves it; the first record of the same
    // request stays, with its time and reason, so the lead hears of it once.
    let asked_now = |id: &str| {
        ask.as_ref().is_some_and(|asked| asked.question_id == id) || same.as_ref().is_some_and(|card| card["question_id"] == id)
    };
    let terminal = cards
        .iter()
        .find(|card| card["can_answer"] == false && !asked_now(card["question_id"].as_str().unwrap_or_default()))
        .map(|card| {
            let question_id = card["question_id"].as_str().unwrap_or_default().to_owned();
            member.terminal_question.as_ref().filter(|seen| seen.question_id == question_id).cloned().unwrap_or_else(|| PendingQuestion {
                question_id,
                question_hash: card["question_hash"].as_str().unwrap_or_default().to_owned(),
                approval: card["approval"] == true,
                at: now(),
                card: card.clone(),
            })
        });
    let answered = ask.is_none() && same.is_none();
    if ask.is_none() && !answered && terminal == member.terminal_question {
        return Ok(());
    }
    let role = team.role_of(member_id);
    // The owner, 2026-10-28: a request of a shape the lead recorded as safe is
    // allowed by Zerus, once, for this question only. The reservation is in
    // the journal before the answer goes out; the question is asked only once,
    // so an answer whose delivery is unknown is never sent again.
    let safe = ask.as_ref().and_then(|pending| safe_match(team, member, pending));
    change(store, &team.id, |team, events| {
        let found = team.members.iter_mut().find(|m| m.id == member_id).context("unknown member")?;
        if found.terminal_question != terminal {
            if let Some(asked) = terminal.as_ref().filter(|asked| {
                found.terminal_question.as_ref().is_none_or(|seen| seen.question_id != asked.question_id)
            }) {
                events.push(Event::new("question_in_terminal", json!({"member":role,"question_id":asked.question_id,
                    "tool":asked.card["questions"][0]["header"],"reason":asked.card["answer_unavailable_reason"]})));
            }
            found.terminal_question = terminal.clone();
            drop_stale_escalation(found, &role, events);
        }
        match &ask {
            Some(pending) => {
                found.asked.push(format!("{}:{}", pending.question_id, pending.question_hash));
                let excess = found.asked.len().saturating_sub(50);
                found.asked.drain(..excess);
                found.pending = Some(pending.clone());
                let mut record = permission_record(pending);
                if let Some((action, _)) = &safe {
                    record["safe_action"] = action["id"].clone();
                    events.push(Event::new("safe_action_reserved", json!({"member":role,"question_id":pending.question_id,
                        "action":action["id"],"command":record["command"]})));
                }
                found.permission = Some(record);
                events.push(Event::new("question_forwarded", json!({"member":role,"approval":pending.approval})));
            }
            None if answered && found.pending.is_some() => {
                // N5: a question that vanished without an hgs team answer (the
                // desktop answered it, or it is gone) is still recorded, by
                // unknown, never dropped.
                if let Some(pending) = found.pending.take() {
                    let answer = permission_lost(&mut found.permission_answers, &pending.question_id, now());
                    events.push(Event::new("permission_answered", json!({"member":role,
                        "question_id":answer["question_id"],"decision":answer["decision"],"by":answer["by"]})));
                }
                found.permission = None;
            }
            None => {}
        }
        Ok(())
    })?;
    if let (Some(pending), Some((action, answer))) = (&ask, &safe) {
        allow_safe(store, team, member_id, pending, action, answer)?;
    }
    Ok(())
}

/// The safe action that allows this approval and the card's one-time allow
/// answer: only for an exact shell command the card carries, never for the
/// lead's own request or a subagent card.
fn safe_match(team: &Team, member: &Member, pending: &PendingQuestion) -> Option<(Value, Value)> {
    let card = &pending.card;
    if !pending.approval || member.id == team.lead || card["subagent_confirmation"] == true || card["can_answer"] == false {
        return None;
    }
    // Task #346: the command may come from the tool input (Claude, Codex) or the
    // process a DeepSeek approval names; a patch's file list is no command.
    let command = card["tool_input"]["command"]
        .as_str()
        .or_else(|| card["command"].as_str())
        .filter(|command| !command.trim().is_empty())?
        .to_owned();
    let answer = permission_answer(card).filter(|answer| answer["allow"].is_string())?;
    let tool = card["tool"].as_str().or_else(|| card["questions"][0]["header"].as_str()).unwrap_or_default();
    let folder = member_folder_path(team, member).ok()?;
    if let Some(action) = safe_actions::find(team, member, tool, &command, Path::new(&folder)) {
        return Some((action, answer));
    }
    // Task #346 (owner, 2026-11-08): ONE `hgs team <op> --json-file PATH` call in
    // the member's own scope - its own work, or its scratch team under the
    // owner's standing rule - is Zerus's to allow once, recorded in the lead's
    // feed; a compound script, a wrapper or another scope still asks.
    if let Some(action) = super::sandbox_scope::match_call(team, member, &command) {
        return Some((action, answer));
    }
    // Task #232: the owner's scratch rule trusts its folder at launch, and the
    // team's own CLI is what a member runs there: Codex's approval of an hgs
    // command is the rule's to allow once, without the lead.
    if card["source"] == crate::state::codex_approval::SOURCE && command_is_hgs(&command) {
        if let Some(rule) = scratch_rule_trusting(team, &folder, &member.agent) {
            return Some((
                json!({"id":"scratch-hgs","reason":format!(
                    "the owner's scratch rule {} trusts the team's own CLI in {folder}", rule.id)}),
                answer,
            ));
        }
    }
    None
}

/// The still-granted scratch rule that covers `folder` for `agent`, or None.
fn scratch_rule_trusting(team: &Team, folder: &str, agent: &str) -> Option<super::owner_steps::ScratchRule> {
    if team.scratch_rule.is_empty() {
        return None;
    }
    super::owner_steps::rules().into_iter().find(|rule| {
        rule.id == team.scratch_rule
            && super::owner_steps::inside(folder, &rule.folder)
            && super::owner_steps::trusted(rule).contains(agent)
    })
}

/// Whether a shell command runs the team's own CLI.
fn command_is_hgs(command: &str) -> bool {
    let first = command.split_whitespace().next().unwrap_or_default();
    let first = first.trim_matches(|ch| ch == '"' || ch == '\'');
    let name = Path::new(first).file_name().and_then(|name| name.to_str()).unwrap_or(first);
    name.eq_ignore_ascii_case("hgs") || name.eq_ignore_ascii_case("hgs.exe")
}

/// Sends the reserved one-time allow and records it by the safe action; when
/// it fails, the question stays in the lead's inbox as any other.
fn allow_safe(store: &mut Store, team: &Team, member_id: &str, pending: &PendingQuestion, action: &Value, answer: &Value) -> anyhow::Result<()> {
    let session = team.member(member_id).and_then(|member| member.session.clone()).context("the member has no session")?;
    let role = team.role_of(member_id);
    // Task #346: an action Zerus answers by itself names how (e.g. its own-scope
    // team call); a recorded safe action keeps its `safe action <id>` line.
    let by = action["answer_by"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("safe action {}", action["id"].as_str().unwrap_or_default()));
    // Task #349 (owner, 2026-11-08): the approval journal is the one place for
    // an approval not given by the owner in person. Zerus's own safe answer is
    // written there, with its source, before it is delivered; a failed
    // delivery settles the same entry.
    let journal_root = crate::approval_log::root();
    let journal = {
        let label = pending.card["questions"].as_array().into_iter().flatten()
            .flat_map(|item| item["options"].as_array().into_iter().flatten())
            .find(|option| option["id"] == answer["allow"])
            .and_then(|option| option["label"].as_str())
            .unwrap_or("allow");
        let source = if action["answer_by"].as_str().is_some() { "Zerus safe-call rule" } else { "safe-action shape" };
        let entry = json!({"host": if session.host.is_empty() { "local" } else { session.host.as_str() },
            "session": session.name,"source":source,"team":team.id,"member":member_id,
            "question":pending.card["questions"][0]["question"],"command":pending.card["tool_input"]["command"].as_str().or_else(|| pending.card["command"].as_str()).unwrap_or(""),
            "answer":format!("allow: {label}"),"status":"answered"});
        match crate::approval_log::note(&journal_root, &entry) {
            Ok(id) => Some(id),
            Err(error) => anyhow::bail!("The approval journal could not record this safe answer; nothing was sent: {error}"),
        }
    };
    let request = json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_run_id":session.run_id,
        "expected_conversation_id":session.conversation_id,"question_id":pending.question_id,
        "expected_question_hash":pending.question_hash,"answers":[{"question_id":answer["question_id"],"selected_option_ids":[answer["allow"]]}]});
    let delivered = match hgs(&session.host, &["answer", &session.name, "--json"], Some(&request), Duration::from_secs(120)) {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
        Err(error) => Err(error.to_string()),
    };
    if let Err(error) = &delivered {
        if let Some(id) = journal {
            let _ = crate::approval_log::settle(&journal_root, id, "failed", error);
        }
    } else if let Some(id) = journal {
        let _ = crate::approval_log::settle(&journal_root, id, "approved", "");
    }
    change(store, &team.id, |team, events| {
        let found = team.members.iter_mut().find(|m| m.id == member_id).context("unknown member")?;
        let question = pending.question_id.as_str();
        match &delivered {
            Ok(()) => {
                let (tool, command) = found.permission.as_ref().map(|asked| (asked["tool"].clone(), asked["command"].clone())).unwrap_or_default();
                if found.pending.as_ref().is_some_and(|asked| asked.question_id == question) {
                    found.pending = None;
                }
                if found.permission.as_ref().is_some_and(|asked| asked["question_id"] == question) {
                    found.permission = None;
                }
                found.permission_answers.push(json!({"question_id":question,"decision":"allow","by":by,"at":now(),
                    "reason":action["reason"],"tool":tool,"command":command,"safe_action":action["id"]}));
                let excess = found.permission_answers.len().saturating_sub(20);
                found.permission_answers.drain(..excess);
                events.push(Event::new("permission_answered", json!({"member":role,"question_id":question,"decision":"allow","by":by})));
            }
            Err(error) => {
                events.push(Event::new("safe_action_failed", json!({"member":role,"question_id":question,"action":action["id"],"error":error})));
            }
        }
        Ok(())
    })
}

/// Task 97: answers the member's request with its safe choice and queues it
/// for the owner. The team record reserves it first (`member.deferred`), then
/// the queue does; neither is undone, so an unknown delivery is never sent
/// again. An answer that reached the session closes the request in the team.
fn defer_request(store: &mut Store, team: &Team, member_id: &str, question: &str) -> anyhow::Result<()> {
    let member = team.member(member_id).context("unknown member")?;
    let Some(pending) = member.pending.clone().filter(|pending| format!("{}:{}", pending.question_id, pending.question_hash) == question) else {
        return Ok(());
    };
    let session = member.session.clone().context("the member has no session")?;
    change(store, &team.id, |team, _| {
        let found = team.members.iter_mut().find(|found| found.id == member_id).context("unknown member")?;
        anyhow::ensure!(!found.deferred.iter().any(|known| known == question), "already deferred");
        found.deferred.push(question.to_owned());
        let excess = found.deferred.len().saturating_sub(50);
        found.deferred.drain(..excess);
        Ok(())
    })?;
    let origin = super::owner_queue::Origin { session: &session, team: Some(team), member: member_id };
    let Some(entry) = super::owner_queue::defer(&origin, &pending, now())? else { return Ok(()) };
    let role = team.role_of(member_id);
    change(store, &team.id, |team, events| {
        let found = team.members.iter_mut().find(|found| found.id == member_id).context("unknown member")?;
        if entry.delivery == "answered" {
            let (tool, command) = found.permission.as_ref().map(|asked| (asked["tool"].clone(), asked["command"].clone())).unwrap_or_default();
            if found.pending.as_ref().is_some_and(|asked| asked.question_id == pending.question_id) {
                found.pending = None;
            }
            if found.permission.as_ref().is_some_and(|asked| asked["question_id"] == pending.question_id.as_str()) {
                found.permission = None;
            }
            let decision = if entry.safe == "deny" { "deny" } else { "answer" };
            found.permission_answers.push(json!({"question_id":pending.question_id,"decision":decision,"by":"owner queue","at":now(),
                "reason":super::owner_queue::AWAY_MESSAGE,"tool":tool,"command":command,"deferred":entry.id}));
            let excess = found.permission_answers.len().saturating_sub(20);
            found.permission_answers.drain(..excess);
        }
        events.push(Event::new("request_deferred", json!({"member":role,"question_id":pending.question_id,"kind":entry.kind,
            "safe":entry.safe,"delivery":entry.delivery,"notice":entry.notice,"id":entry.id})));
        Ok(())
    })
}

/// Task 97: the ordinary sessions the owner turned the queue on for. A
/// session that asks is read at most every 20 seconds; a team's session is
/// its team's to defer.
pub(crate) fn defer_watched(teams: &[Team], sessions: &[Seen]) {
    let watched = super::owner_queue::watched();
    if watched.is_empty() {
        return;
    }
    for found in sessions.iter().filter(|found| found.live && found.asking) {
        let Some(minutes) = super::owner_queue::minutes_for(&watched, &found.host, &found.name) else { continue };
        let in_team = teams.iter().any(|team| {
            team.members.iter().any(|member| member.session.as_ref().is_some_and(|session| session.host == found.host && session.name == found.name))
        });
        if in_team {
            continue;
        }
        let key = format!("watched|{}|{}", found.host, found.name);
        {
            let mut checked = PERMISSION_CHECKED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
            if checked.get(&key).is_some_and(|at| now() - at < 20.0) {
                continue;
            }
            checked.insert(key, now());
        }
        let output = match hgs(&found.host, &["inspect", &found.name, "--skip-processes"], None, Duration::from_secs(45)) {
            Ok(output) if output.status.success() => output,
            Ok(_) => continue,
            Err(error) => {
                eprintln!("hgs team worker: {}: {error}", found.name);
                continue;
            }
        };
        let Ok(record) = serde_json::from_slice::<Value>(&output.stdout) else { continue };
        let cards = record["pending_questions"].as_array().cloned().unwrap_or_default();
        let session = SessionRef { host: found.host.clone(), name: found.name.clone(), run_id: found.run_id.clone(),
            conversation_id: found.conversation_id.clone() };
        if let Err(error) = super::owner_queue::tick_session(&session, minutes, &cards, now()) {
            eprintln!("hgs team worker: {}: owner queue: {error}", found.name);
        }
    }
}

/// Hands the lead its own task without a message (C1): the attempt exists, so
/// `hgs team current` names it and the inbox lists it to report and accept.
fn lead_hand_out(store: &mut Store, team_id: &str, task_id: u32) -> anyhow::Result<()> {
    // U46: the lead's own hand-out is a hand-out too; a team that sets no
    // context action keeps today's path.
    let team = store.load(team_id)?.with_context(|| format!("Unknown team {team_id}"))?;
    let lead = team.member(&team.lead).cloned().context("the team has no lead")?;
    // Task #7: a configured lead policy runs only while the lead is free; the
    // default path keeps U46's every-hand-out action as today.
    let action = lead_context_action(&team, &lead);
    if action.is_empty() {
        return lead_hand_out_now(store, team_id, task_id, Preparation::default());
    }
    let Some(session) = lead.session.clone() else {
        let step = context_failure(context_step(&action, now()), "refused", "The lead's session is not started".into());
        return record_waits(store, team_id, task_id, &lead.id, None, step);
    };
    match run_context(&lead, &session, &action) {
        ContextStart::Started(request) => record_context_start(store, team_id, task_id, &lead.id, None, request),
        ContextStart::Done { step, conversation } => {
            let conversation = conversation.map(|new| (session.run_id.clone(), new));
            lead_hand_out_now(store, team_id, task_id, Preparation { steps: vec![step], conversation, resume: false })
        }
        ContextStart::Failed { step } => record_waits(store, team_id, task_id, &lead.id, None, step),
    }
}

fn lead_hand_out_now(store: &mut Store, team_id: &str, task_id: u32, preparation: Preparation) -> anyhow::Result<()> {
    let Preparation { steps: prepare, conversation, resume } = preparation;
    change(store, team_id, |team, events| {
        if resume {
            if let Some(task) = team.task_mut(task_id) {
                task.waits = None;
            }
        }
        anyhow::ensure!(can_hand_out(team, task_id), "no longer ready");
        let lead = team.lead.clone();
        if let Some((run, new)) = &conversation {
            if let Some(found) = team.members.iter_mut().find(|member| member.id == lead) {
                if let Some(session) = found.session.as_mut().filter(|session| &session.run_id == run) {
                    session.conversation_id = new.clone();
                }
            }
        }
        if let Some(step) = prepare.iter().find(|step| step.result == "skipped") {
            if let Some(found) = team.members.iter_mut().find(|member| member.id == lead) {
                if !found.context_noted {
                    found.context_noted = true;
                    events.push(Event::new("context_skipped", json!({"member":"Lead","task":task_id,"reason":step.reason})));
                }
            }
        }
        if let Some(step) = prepare.iter().find(|step| step.result == "done") {
            events.push(Event::new("context_prepared", json!({"member":"Lead","task":task_id,"action":step.to})));
        }
        let attempt_id = uuid::Uuid::new_v4().to_string();
        let task = team.task_mut(task_id).context("unknown task")?;
        anyhow::ensure!(task.assignee == lead, "not the lead's task");
        let n = task.attempts.len() as u32 + 1;
        task.attempts.push(Attempt {
            n,
            id: attempt_id,
            member: lead,
            state: AttemptState::Delivered,
            sent_at: now(),
            delivered_at: Some(now()),
            // The lead works when it suits: no start warning for its own task.
            started_at: Some(now()),
            prepare,
            ..Default::default()
        });
        task.status = TaskStatus::InProgress;
        events.push(Event::new("handed_out", json!({"by":"Zerus","task":task_id,"to":"Lead","attempt":n,"no_message":true})));
        Ok(())
    })
}

/// Task 112: agents whose own input queue hands a pasted message to the agent
/// between its tool calls (Claude, Codex); others send it after the turn.
/// Task 186: nothing is pasted into a working turn any more; these are the
/// agents whose hooks take a stored message at a safe point instead.
pub(crate) fn takes_input_mid_turn(agent: &str) -> bool {
    super::doorbell::turn_hook(agent)
}

/// Sends the one coalesced inbox message and remembers which items it named
/// (C2). A refusal leaves everything pending for the next interval.
/// Task 186: `ring` false stores it for a lead in its own turn, typing
/// nothing; it waits for the next safe point.
fn tell_lead_inbox(store: &mut Store, team: &Team, ring: bool) -> anyhow::Result<()> {
    // Task 112: the list is composed from the team as it is now, never from
    // this tick's earlier read: a report decided meanwhile is never told.
    let fresh = store.load(&team.id)?.context("Unknown team")?;
    let team = &fresh;
    let lead = team.member(&team.lead).and_then(|m| m.session.clone()).context("the lead has no session")?;
    let items = inbox::items(team);
    // Task #24 (team ede724c4): an item told before that closed and opened
    // again unchanged is not new.
    let new: Vec<inbox::Item> = items.iter().filter(|item| !team.inbox.knows(&item.key, now())).cloned().collect();
    let again = inbox::overdue(&items, &team.inbox.told, now());
    // Task 112: what the tick saw may have been decided since (a report
    // accepted, a member back from its limit): nothing new now, nothing told.
    if new.is_empty() && again.is_empty() {
        return Ok(());
    }
    // Task 133: the digest ends with what each member is doing (its latest
    // pulse of the last day), so the lead need not read the chats.
    let mut text = inbox::text_with(team, &new, &again, now());
    let recent: Vec<super::pulses::Pulse> = store.latest_pulses(&team.id, now()).into_iter().filter(|pulse| now() - pulse.at <= 86_400.0).collect();
    let block = super::pulses::digest(team, &recent, now());
    if !block.is_empty() {
        text = format!("{text}\n\n{block}");
    }
    // Task 128: the team memory's open items and newest decisions.
    let memory = super::lore_sync::digest(team);
    if !memory.is_empty() {
        text = format!("{text}\n\n{memory}");
    }
    // Task 134: the text is stored and one line rings.
    let urgency = inbox::urgency(new.iter().chain(&again));
    let request_id = uuid::Uuid::new_v4().to_string();
    if ring {
        let outcome = super::doorbell::post_and_ring(store, &team.id, &team.lead, &lead, &request_id, "inbox", "zerus", urgency, &text);
        if matches!(outcome, Sent::Refused(_)) {
            return Ok(());
        }
    } else {
        super::doorbell::post_waiting(store, &team.id, &team.lead, &request_id, "inbox", "zerus", urgency, &text)?;
    }
    change(store, &team.id, |team, events| {
        let named = new.len() + again.len();
        // L2: keep the keys of items that are still open.
        let open: BTreeSet<String> = inbox::items(team).into_iter().map(|item| item.key).collect();
        for item in &again {
            team.inbox.told.push(format!("remind:{}", item.key));
        }
        inbox::mark_told_inbox(&mut team.inbox, &new, &open, now());
        team.inbox.told_at = now();
        events.push(Event::new("inbox_told", json!({"items":named})));
        Ok(())
    })
}

/// Spec 5.6: the sub-lead's coalesced inbox message, as `tell_lead_inbox`
/// sends the lead's.
fn tell_sub_inbox(store: &mut Store, team: &Team, member: &str) -> anyhow::Result<()> {
    let sub = team.member(member).filter(|found| found.sub).context("not a sub-lead")?;
    let session = sub.session.clone().context("the sub-lead has no session")?;
    let new: Vec<inbox::Item> =
        inbox::items_for(team, member).into_iter().filter(|item| !sub.inbox.knows(&item.key, now())).collect();
    anyhow::ensure!(!new.is_empty(), "no new inbox items");
    let outcome = super::doorbell::post_and_ring(store, &team.id, member, &session, &uuid::Uuid::new_v4().to_string(), "inbox", "zerus",
        inbox::urgency(new.iter()), &inbox::text(team, &new, now()));
    if matches!(outcome, Sent::Refused(_)) {
        return Ok(());
    }
    let member = member.to_owned();
    change(store, &team.id, |team, events| {
        let open: BTreeSet<String> = inbox::items_for(team, &member).into_iter().map(|item| item.key).collect();
        let role = team.role_of(&member);
        let Some(sub) = team.members.iter_mut().find(|found| found.id == member) else { return Ok(()) };
        inbox::mark_told_inbox(&mut sub.inbox, &new, &open, now());
        sub.inbox.told_at = now();
        events.push(Event::new("inbox_told", json!({"items":new.len(),"member":role})));
        Ok(())
    })
}

/// Reminds the free lead once that a report still waits for a decision (C4).
/// How long a member in its turn may say nothing before the lead hears.
const SILENT_MINUTES: f64 = 20.0;
/// #728: a member waiting in one running tool call is stuck only after six
/// times the stuck bound, and never under an hour.
pub(crate) fn tool_stuck_minutes(team: &Team) -> f64 {
    (super::ops::stuck_minutes(team) * 6.0).max(60.0)
}

/// #628: a DeepSeek member's listing `last_event_at` does not advance with its
/// dsh_native PreToolUse/PostToolUse events. Read the session's newest event
/// once, record it on the attempt so later ticks count it, and call the member
/// silent only when that is stale too.
fn check_dsh_silent(store: &mut Store, team: &Team, task_id: u32, listing_last: f64) -> anyhow::Result<()> {
    let Some(task) = team.task(task_id) else { return Ok(()) };
    let Some(attempt) = task.open_attempt() else { return Ok(()) };
    let Some(member) = team.member(&attempt.member) else { return Ok(()) };
    let Some(session) = member.session.clone() else { return Ok(()) };
    // A record from another run is not this member's activity.
    let inspected = inspect_reply(&session);
    // #595 L3: when the events cannot be read (an offline computer, another
    // run's record), the lead is told so instead of only "silent".
    let unreadable = match &inspected {
        Ok(record) => record["run_id"].as_str().unwrap_or_default() != session.run_id,
        Err(_) => true,
    };
    let own = inspected.ok().filter(|record| record["run_id"].as_str().unwrap_or_default() == session.run_id);
    let newest = own.as_ref().and_then(newest_event);
    // Task 181: a member inside one open tool call is held to the tool bound.
    let bound = if own.as_ref().is_some_and(|record| super::look::open_tool(record).is_some()) {
        (SILENT_MINUTES * 60.0).max(tool_stuck_minutes(team) * 60.0)
    } else {
        SILENT_MINUTES * 60.0
    };
    let last = listing_last.max(newest.unwrap_or(0.0));
    let quiet = now() - last;
    let attempt_id = attempt.id.clone();
    change(store, &team.id, |team, _| {
        if let Some(found) = team.task_mut(task_id).and_then(|task| task.attempts.iter_mut().find(|found| found.id == attempt_id)) {
            // #728 L2: the read is recorded, so the next one is a minute away.
            found.silent_read_at = Some(now());
            if let Some(newest) = newest {
                if found.activity_at.is_none_or(|at| newest > at) {
                    found.activity_at = Some(newest);
                }
            }
        }
        Ok(())
    })?;
    if quiet >= bound {
        tell_lead_silent(store, team, task_id, (quiet / 60.0).round().max(1.0) as u64, unreadable)?;
    }
    Ok(())
}

/// #761: the member's last report or turn end, the progress a DeepSeek listing
/// does not show once the turn is over.
pub(crate) fn last_progress(team: &Team, member: &str) -> Option<f64> {
    team.tasks
        .iter()
        .flat_map(|task| task.attempts.iter())
        .filter(|attempt| attempt.member == member)
        .flat_map(|attempt| [attempt.report.as_ref().map(|report| report.at), attempt.turn_ended_at])
        .flatten()
        .reduce(f64::max)
}

/// #761 (owner, 2026-10-31, team 6d0aabee): a DeepSeek member with no open
/// attempt whose listing still says working is read before it is called
/// stuck. The read is recorded, so the next is a minute away. A read that
/// fails, another run's record, an idle session or a native event newer than
/// the bound marks nothing; only a busy session without progress past the
/// bound is marked, since its last progress.
fn check_dsh_stuck(store: &mut Store, team: &Team, member_id: &str, listing_last: f64, limit: f64, reason: &str) -> anyhow::Result<()> {
    let Some(member) = team.member(member_id) else { return Ok(()) };
    let Some(session) = member.session.clone() else { return Ok(()) };
    let record = inspect_reply(&session).ok().filter(|record| record["run_id"].as_str().unwrap_or_default() == session.run_id);
    let newest = record.as_ref().and_then(newest_event);
    let at = now();
    let busy = record.as_ref().is_some_and(|record| {
        record["activity"] == "busy" || matches!(record["phase"].as_str().unwrap_or_default(), "working" | "tool" | "compacting")
    });
    let phase = record.as_ref().and_then(|record| record["phase"].as_str()).unwrap_or_default().to_owned();
    let last = listing_last.max(newest.unwrap_or(0.0));
    // Task 181: an open tool call has the tool bound, not the stuck bound.
    let limit = if record.as_ref().is_some_and(|record| super::look::open_tool(record).is_some()) { limit.max(tool_stuck_minutes(team) * 60.0) } else { limit };
    let stuck = busy && at - last >= limit;
    change(store, &team.id, |team, events| {
        let role = team.role_of(member_id);
        let Some(found) = team.members.iter_mut().find(|found| found.id == member_id) else { return Ok(()) };
        found.stuck_read_at = Some(at);
        if let Some(newest) = newest {
            if found.native_at.is_none_or(|old| newest > old) {
                found.native_at = Some(newest);
            }
        }
        if stuck && found.stuck.is_none() {
            found.stuck = Some(MemberStuck { at, reason: reason.to_owned(), phase: phase.clone(), since: Some(last) });
            let minutes = ((at - last) / 60.0).max(1.0).round() as u64;
            events.push(Event::new("member_stuck", json!({"member":role,"id":member_id,"reason":reason,"phase":phase,"minutes":minutes})));
        }
        Ok(())
    })
}

/// The newest `at` of a session record's events, in seconds; a native DeepSeek
/// session's dsh_native tool events are such entries (#628).
fn newest_event(record: &Value) -> Option<f64> {
    record["events"]
        .as_array()?
        .iter()
        .filter_map(|event| event["at"].as_f64())
        .filter(|at| *at > 0.0)
        .max_by(f64::total_cmp)
}

fn tell_lead_silent(store: &mut Store, team: &Team, task_id: u32, minutes: u64, unreadable: bool) -> anyhow::Result<()> {
    let task = team.task(task_id).context("unknown task")?;
    let attempt = task.open_attempt().context("no open attempt")?.clone();
    let member = team.member(&attempt.member).context("unknown member")?;
    let session = member.session.as_ref().map(|session| session.name.clone()).unwrap_or_default();
    let lead = team.member(&team.lead).and_then(|m| m.session.clone()).context("the lead has no session")?;
    let seen = if unreadable {
        "and Zerus could not read its events (its computer did not answer, or the record is from another run), \
         so a question or Background work may be open"
    } else {
        "and Zerus sees no question or Background work"
    };
    let text = format!(
        "[Zerus team \u{b7} from: Zerus \u{b7} task: #{} \u{b7} silent {minutes} min]\n\
         {} is in its turn on #{} {} but has reported nothing for {minutes} min, {seen}. A prompt Zerus cannot read \
         (a permission of a subagent call, a dialog) may hold it: look at its terminal, or interrupt the turn with the \
         owner's word (hgs interrupt {session} --json) and tell it how to go on.",
        task.id,
        team.name_of(&attempt.member),
        task.id,
        task.title,
    );
    let outcome = send(&lead, &uuid::Uuid::new_v4().to_string(), &text);
    if matches!(outcome, Sent::Refused(_)) {
        return Ok(());
    }
    let attempt_id = attempt.id.clone();
    change(store, &team.id, |team, events| {
        if let Some(found) = team.task_mut(task_id).and_then(|task| task.attempts.iter_mut().find(|found| found.id == attempt_id)) {
            if !found.told_lead.iter().any(|kind| kind == "silent") {
                found.told_lead.push("silent".into());
            }
        }
        events.push(Event::new("member_silent", json!({"task":task_id,"minutes":minutes})));
        Ok(())
    })
}

fn remind_report(store: &mut Store, team: &Team, task_id: u32) -> anyhow::Result<()> {
    let task = team.task(task_id).context("unknown task")?;
    let attempt = task
        .attempts
        .iter()
        .rev()
        .find(|attempt| attempt.state == AttemptState::Reported)
        .context("the task has no report")?
        .clone();
    let report = attempt.report.as_ref().context("the attempt has no report")?;
    let minutes = (((now() - report.at) / 60.0).round() as u64).max(1);
    let lead = team.member(&team.lead).and_then(|m| m.session.clone()).context("the lead has no session")?;
    // A7: a captured report holds its member back from new work.
    let held = if report.captured && attempt.member != team.lead {
        format!("{}\n", inbox::captured_hold_note(team, &attempt.member))
    } else {
        String::new()
    };
    let text = format!(
        "[Zerus team \u{b7} from: Zerus \u{b7} task: #{} \u{b7} report waiting {minutes} min]\n\
         {}'s report on #{} {} has waited {minutes} min for your decision.\n\
         {held}Decide with hgs team accept --json or reject --json, or look at the whole inbox:\n  \
         {} | hgs team inbox --json",
        task.id,
        team.name_of(&attempt.member),
        task.id,
        task.title,
        json!({"team":team.id})
    );
    let outcome = send(&lead, &uuid::Uuid::new_v4().to_string(), &text);
    if matches!(outcome, Sent::Refused(_)) {
        return Ok(());
    }
    let attempt_id = attempt.id.clone();
    change(store, &team.id, |team, events| {
        let is_sub = team.task(task_id).is_some_and(|task| team.reviewer_of(&task.assignee) != team.lead);
        let Some(found) = team.task_mut(task_id).and_then(|task| task.attempts.iter_mut().find(|found| found.id == attempt_id)) else {
            return Ok(());
        };
        found.reminded_at = Some(now());
        if is_sub && !found.told_lead.iter().any(|kind| kind == "sub-remind") {
            found.told_lead.push("sub-remind".into());
        }
        events.push(Event::new("reminded", json!({"task":task_id,"attempt":found.n,"to":"Lead","minutes":minutes})));
        Ok(())
    })
}

/// Reminds a sub-lead once that a report still waits for its decision (N9).
fn remind_reviewer(store: &mut Store, team: &Team, task_id: u32, member: &str) -> anyhow::Result<()> {
    let task = team.task(task_id).context("unknown task")?;
    let attempt = task
        .attempts
        .iter()
        .rev()
        .find(|attempt| attempt.state == AttemptState::Reported)
        .context("the task has no report")?
        .clone();
    let report = attempt.report.as_ref().context("the attempt has no report")?;
    let minutes = (((now() - report.at) / 60.0).round() as u64).max(1);
    let sub = team.member(member).filter(|found| found.sub).context("not a sub-lead")?;
    let session = sub.session.clone().context("the sub-lead has no session")?;
    let text = format!(
        "[Zerus team \u{b7} from: Zerus \u{b7} task: #{} \u{b7} report waiting {minutes} min]\n\
         {}'s report on #{} {} has waited {minutes} min for your decision as its sub-lead.\n\
         Decide with hgs team accept --json or reject --json, or look at your inbox:\n  \
         {} | hgs team inbox --json",
        task.id,
        team.name_of(&attempt.member),
        task.id,
        task.title,
        json!({"team":team.id})
    );
    let outcome = send(&session, &uuid::Uuid::new_v4().to_string(), &text);
    if matches!(outcome, Sent::Refused(_)) {
        return Ok(());
    }
    let attempt_id = attempt.id.clone();
    change(store, &team.id, |team, events| {
        let Some(found) = team.task_mut(task_id).and_then(|task| task.attempts.iter_mut().find(|found| found.id == attempt_id)) else {
            return Ok(());
        };
        found.reminded_at = Some(now());
        events.push(Event::new("reminded", json!({"task":task_id,"attempt":found.n,"to":member,"minutes":minutes})));
        Ok(())
    })
}

/// N10: the once-only brief for a member that has just become a sub-lead.
fn send_sub_brief(store: &mut Store, team: &Team, member: &str) -> anyhow::Result<()> {
    let sub = team.member(member).filter(|found| found.sub).context("not a sub-lead")?;
    let session = sub.session.clone().context("the sub-lead has no session")?;
    let reports: Vec<String> = team.reports_of(member).iter().map(|found| found.role.clone()).collect();
    let text = format!(
        "[Zerus team \u{b7} from: Zerus \u{b7} sub-lead]\nYou are now a sub-lead of the team {}: the members {} report to you.\n\
         Their reports and questions wait in your inbox: {} | hgs team inbox --json.\n\
         Decide their reports with hgs team accept --json or reject --json; a rejection reopens the task with your reason.\n\
         You may hand out their tasks, hold one back and release it; your own reports still go to the lead (hgs guide team-lead).",
        team.name,
        if reports.is_empty() { "that report to you".to_owned() } else { reports.join(", ") },
        json!({"team":team.id})
    );
    let outcome = send(&session, &uuid::Uuid::new_v4().to_string(), &text);
    if matches!(outcome, Sent::Refused(_)) {
        return Ok(());
    }
    let member = member.to_owned();
    change(store, &team.id, |team, events| {
        if let Some(found) = team.members.iter_mut().find(|found| found.id == member) {
            found.sub_brief = false;
        }
        events.push(Event::new("sub_brief", json!({"member":member})));
        Ok(())
    })
}

/// Task 164 (owner, 2026-11-07): what the lead can do about members idle
/// without ready work, one line per concrete action: a ready task of a
/// member's role sitting with another member, a draft that would run
/// alongside once approved, and what each startable open task waits for
/// (never a member at work, never "give it work"). Empty when the lead can do
/// nothing; then nobody is told.
pub(crate) fn starved_actions(team: &Team, members: &[String]) -> Vec<String> {
    let ready = plan::ready(team);
    let mut lines = Vec::new();
    for id in members {
        let Some(member) = team.member(id) else { continue };
        for task_id in plan::ready_for_role(team, member, &ready).into_iter().take(2) {
            if let Some(task) = team.task(task_id) {
                let holder = if task.assignee.is_empty() { "nobody".to_owned() } else { team.name_of(&task.assignee) };
                lines.push(format!(
                    "#{} {} is ready and sits with {holder}: reassign it to {} (hgs team reassign)",
                    task.id, task.title, team.name_of(&member.id)
                ));
            }
        }
    }
    // Owner, 2026-11-11: an approval starts nothing behind a barrier, so the
    // digest never offers one for a barrier phase (the owner approved phase 2
    // on this advice and nothing changed); it names the barrier and how it
    // goes. With Auto-accept drafts on, Auto takes a draft and no card is due.
    let mut phases: Vec<&Phase> = team.phases.iter().filter(|phase| matches!(phase.state, PhaseState::Draft | PhaseState::Approved)).collect();
    phases.sort_by_key(|phase| (phase.order(), phase.n));
    for phase in phases {
        let n = phase.n;
        let open = team.tasks.iter().filter(|task| task.phase == n && !task.status.finished()).count();
        if open == 0 {
            continue;
        }
        let held = plan::phase_barrier_blockers(team, n).len();
        if phase.state == PhaseState::Approved {
            if held > 0 {
                lines.push(format!(
                    "Phase {n} ({open} task(s)) is approved behind its barrier: it waits for {held} open task(s) of the phases before it. \
                     If it may run alongside them, propose removing the barrier: \
                     {{\"kind\":\"edit-draft\",\"phase\":{n},\"barrier\":false,\"reason\":\"...\"}} | hgs team propose --json"
                ));
            }
        } else if held > 0 {
            lines.push(format!(
                "Phase {n} ({open} task(s)) is a draft behind its barrier: approved or not, it waits for {held} open task(s) of the phases before it. \
                 If it may run alongside them, remove the barrier: {{\"phase\":{n},\"barrier\":false}} | hgs team edit-draft --json"
            ));
        } else if team.settings.auto_accept {
            if let Some(blocker) = super::ops::auto_accept_blocker(team, n) {
                lines.push(format!("Phase {n} ({open} task(s)) is a draft Auto cannot accept yet: {blocker}"));
            }
        } else {
            lines.push(format!(
                "Phase {n} ({open} task(s)) can run alongside once it is approved: ask the owner with a card (hgs team propose)"
            ));
        }
    }
    let ready_set: BTreeSet<u32> = ready.iter().copied().collect();
    let mut waits = 0;
    for task in team.tasks.iter().filter(|task| plan::startable(team, task) && task.status == TaskStatus::Pending && !ready_set.contains(&task.id)) {
        let Some(wait) = inbox::waits(team, task, false) else { continue };
        let kind = wait["for"].as_str().unwrap_or_default();
        let unblock = wait["unblock"].as_str().unwrap_or_default();
        if kind == "member" || unblock.is_empty() || unblock == "wait" {
            continue;
        }
        let owner = if kind == "owner" { " (an owner card)" } else { "" };
        lines.push(format!("#{} {} waits for {kind}: {unblock}{owner}", task.id, task.title));
        waits += 1;
        if waits == 5 {
            break;
        }
    }
    // Task #464 (owner, 2026-11-09): accepted work waits for a hand landing:
    // name the state plainly (how many, the merger that holds the landing task
    // and whether it is cold), never "waiting for the lead to review".
    if let Some(line) = super::merge_queue::landing_line(team, crate::state::now()) {
        lines.push(line);
    }
    lines
}

/// The team-level starved digest (task 164): who is idle and for how long,
/// and the concrete actions; None when there is no action to name.
pub(crate) fn starved_text(team: &Team, members: &[String], now: f64) -> Option<String> {
    let actions = starved_actions(team, members);
    let terminal = super::terminal_prompt::lead_lines(team, now);
    if actions.is_empty() && terminal.is_empty() {
        return None;
    }
    let idle: Vec<String> = members
        .iter()
        .filter_map(|id| team.member(id))
        .map(|member| {
            let minutes = (((now - member.starved_since.unwrap_or(now)).max(0.0)) / 60.0).round() as u64;
            format!("{} {minutes} min", team.name_of(&member.id))
        })
        .collect();
    let mut text = format!(
        "[Zerus team \u{b7} from: Zerus \u{b7} starved]\nIdle with no ready task: {}.\n",
        idle.join(", ")
    );
    if !actions.is_empty() {
        text += "What you can do:\n";
        for line in &actions {
            text += &format!("- {line}\n");
        }
    }
    // Task 113: a member held by a prompt only its Terminal takes.
    text += &terminal;
    text += "At most one starved digest an hour (hgs guide team-lead).";
    Some(text)
}

/// Task #464 (owner, 2026-11-09): a lead accept with the merge queue off wakes
/// the merger that holds the landing task, once for each newly accepted task:
/// what waits, its commit and the line it goes to. The landing is the
/// merger's business; the author is free and never waits for it.
fn wake_landing(store: &mut Store, team: &Team, member: &str, tasks: &[u32]) -> anyhow::Result<()> {
    let Some(session) = team.member(member).and_then(|found| found.session.clone()) else { return Ok(()) };
    let mut lines = Vec::new();
    for id in tasks {
        let Some(task) = team.task(*id) else { continue };
        let commit = super::merge_queue::accepted_commit(task).unwrap_or_default();
        let short: String = commit.chars().take(9).collect();
        let named = if short.is_empty() { String::new() } else { format!(" ({short})") };
        lines.push(format!("#{id} {}{named}", task.title));
    }
    if lines.is_empty() {
        return Ok(());
    }
    let line = super::merge_queue::integration_branch(team).unwrap_or_default();
    let text = format!(
        "[Zerus team \u{b7} from: Zerus \u{b7} landing]\n{} accepted task(s) wait for a landing on {line}:\n- {}\n\
         The lead accepted them and the merge queue is off: land each commit into the line by hand and report the line's head. \
         An accepted task whose commit is on the line is marked landed by itself (or hgs team landed --json names it); \
         its author is free already and must not be asked to wait for the landing.",
        tasks.len(),
        lines.join("\n- ")
    );
    let request_id = uuid::Uuid::new_v4().to_string();
    let outcome = super::doorbell::post_and_ring(store, &team.id, member, &session, &request_id, "landing", "zerus", Urgency::Normal, &text);
    if matches!(outcome, Sent::Refused(_)) {
        return Ok(());
    }
    let tasks = tasks.to_vec();
    change(store, &team.id, |team, events| {
        for id in &tasks {
            if !team.landing_told.contains(id) {
                team.landing_told.push(*id);
            }
        }
        let excess = team.landing_told.len().saturating_sub(500);
        team.landing_told.drain(..excess);
        events.push(Event::new("landing_woken", json!({"member":team.role_of(member),"tasks":tasks})));
        Ok(())
    })
}

/// Tells the lead once an hour at most which members starve on the plan and
/// what it can do (C7, task 164). Nothing is sent when there is no action;
/// the members are still marked told for this episode.
fn tell_lead_starved(store: &mut Store, team: &Team, members: &[String]) -> anyhow::Result<()> {
    let at = now();
    let text = starved_text(team, members, at);
    if let Some(text) = &text {
        let lead = team.member(&team.lead).and_then(|m| m.session.clone()).context("the lead has no session")?;
        let outcome = super::doorbell::post_and_ring(store, &team.id, &team.lead, &lead, &uuid::Uuid::new_v4().to_string(), "starved", "zerus",
            Urgency::Normal, text);
        if matches!(outcome, Sent::Refused(_)) {
            return Ok(());
        }
    }
    let actions = starved_actions(team, members).len();
    change(store, &team.id, |team, events| {
        let mut named = Vec::new();
        for id in members {
            if let Some(found) = team.members.iter_mut().find(|found| found.id == *id) {
                found.starved_told = true;
                named.push(json!({"member":id,"minutes":(((at - found.starved_since.unwrap_or(at)).max(0.0)) / 60.0).round() as u64}));
            }
        }
        if text.is_some() {
            team.starved_digest_at = Some(at);
            let roles: Vec<String> = members.iter().map(|id| team.role_of(id)).collect();
            events.push(Event::new("plan_starved", json!({"members":roles,"idle":named,"actions":actions})));
        }
        Ok(())
    })
}

/// U52: the crashed member's session is launched again into the same
/// conversation, folder and name, and its open attempt is returned as
/// unconfirmed for the lead. The restart is counted before it is attempted, so
/// an agent that keeps failing cannot loop. Task #334: the same launch covers
/// an exit that interrupted a held task and a member the owner or the lead
/// resumed, and the feed says which.
fn restart_crashed(store: &mut Store, team: &Team, member_id: &str) -> anyhow::Result<()> {
    let session = team.member(member_id).and_then(|member| member.session.clone()).context("the member has no session")?;
    let at = now();
    let resumed = team.member(member_id).is_some_and(|member| member.relaunch_exited);
    let why = if resumed { "resumed" } else if holds_work(team, member_id) { "exited with a task" } else { "crashed" };
    change(store, &team.id, |team, events| {
        let role = team.role_of(member_id);
        let found = team.members.iter_mut().find(|found| found.id == member_id).context("unknown member")?;
        found.restarts.push(at);
        if found.restarts.len() > 20 {
            let extra = found.restarts.len() - 20;
            found.restarts.drain(..extra);
        }
        found.crash = Some(at);
        found.crash_told = false;
        found.relaunch_exited = false;
        for task in &mut team.tasks {
            let task_id = task.id;
            if let Some(attempt) = task.open_attempt_mut().filter(|attempt| attempt.member == member_id) {
                attempt.state = AttemptState::Unconfirmed;
                attempt.error = format!("the session crashed at {at:.0}; restarted");
                events.push(Event::new("unconfirmed", json!({"task":task_id,"attempt":attempt.n,"error":attempt.error})));
            }
        }
        events.push(Event::new("member_restarted", json!({"member":role,"at":at,"why":why})));
        Ok(())
    })?;
    let outcome = hgs(&session.host, &["resume", session.name.as_str(), "-d"], None, Duration::from_secs(120));
    let error = match outcome {
        Ok(output) if output.status.success() => return Ok(()),
        Ok(output) => String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        Err(error) => error.to_string(),
    };
    change(store, &team.id, |team, events| {
        events.push(Event::new("restart_failed", json!({"member":team.role_of(member_id),"error":error})));
        Ok(())
    })
}

/// Task #227: the task a member's folder branch names: a per-task branch, or
/// the fork's task branch ending in `-<id>`. None when the folder shows none.
fn task_evidence(team: &Team, member: &Member) -> Option<u32> {
    let folder = member_folder_path(team, member).ok().filter(|folder| !folder.is_empty())?;
    let branch = git_bounded(&folder, &["rev-parse", "--abbrev-ref", "HEAD"]).ok()?;
    team.tasks
        .iter()
        .filter(|task| task.assignee == member.id && !task.status.finished())
        .find(|task| {
            task_branch(team, task.id, &member.id).is_some_and(|expected| expected == branch)
                || branch.ends_with(&format!("-{}", task.id))
        })
        .map(|task| task.id)
}

/// Task #227: whether a busy member's run works on `task`: its branch names it,
/// or it holds no attempt for another task.
fn crash_same_task(team: &Team, member: &Member, task: u32) -> bool {
    match task_evidence(team, member) {
        Some(shown) => shown == task,
        None => !team
            .tasks
            .iter()
            .filter(|found| found.id != task)
            .any(|found| found.attempts.iter().any(|attempt| attempt.member == member.id && attempt.state.open())),
    }
}

/// Task #227: what a restarted session reads before its old hand-out: the fixed
/// preamble the owner asked for, then the task as it was handed out.
fn crash_preamble(team: &Team, task: &Task, attempt: &Attempt) -> String {
    format!(
        "[Zerus team \u{b7} from: Zerus \u{b7} task: #{} \u{b7} attempt: {} \u{b7} restarted]\n\
         Your session crashed at {} and was restarted. Continue this same task from your folder and branch: run git status and git log first, \
         then go on where you stopped. Do not start over, and keep attempt {} (team {}).\n\n",
        task.id,
        attempt.n,
        super::inbox::local_time(attempt.sent_at),
        attempt.n,
        team.name
    )
}

/// Task #227: an attempt a restart left unconfirmed goes back to the member by
/// itself. `busy`: the new run already works on the task, so the attempt is
/// bound to it instead of sent again. Returns what it did, for the sweep.
fn recover_crash(store: &mut Store, team: &Team, task_id: u32, member_id: &str, busy: bool) -> anyhow::Result<Option<Value>> {
    let snapshot = store.load(&team.id)?.context("Unknown team")?;
    let Some(member) = snapshot.member(member_id).cloned() else { return Ok(None) };
    let Some(task) = snapshot.task(task_id) else { return Ok(None) };
    let Some(attempt) = task
        .attempts
        .iter()
        .rev()
        .find(|attempt| attempt.member == member_id && attempt.state == AttemptState::Unconfirmed && attempt.error.contains("the session crashed"))
    else {
        return Ok(None);
    };
    let (n, attempt_id) = (attempt.n, attempt.id.clone());
    if busy {
        // The resumed conversation is already on it: no second hand-out, the
        // attempt runs under the new run and keeps its number.
        let mut done = false;
        change(store, &team.id, |team, events| {
            let at = now();
            let role = team.role_of(member_id);
            {
                let task = team.task_mut(task_id).context("unknown task")?;
                if let Some(attempt) = task.attempts.iter_mut().find(|attempt| attempt.id == attempt_id) {
                    attempt.state = AttemptState::Delivered;
                    attempt.delivered_at = Some(at);
                    attempt.started_at = Some(at);
                    attempt.error = "the session crashed and was restarted; its new run continued on its own".into();
                    task.status = TaskStatus::InProgress;
                    done = true;
                }
            }
            if done {
                events.push(Event::new("attempt_continued", json!({"task":task_id,"attempt":n,"member":role})));
                team.crash_recovered.push(json!({"task":task_id,"attempt":n,"member":role,"at":at,"how":"continued"}));
                if team.crash_recovered.len() > 20 {
                    let excess = team.crash_recovered.len() - 20;
                    team.crash_recovered.drain(..excess);
                }
            }
            Ok(())
        })?;
        return Ok(done.then(|| json!({"task":task_id,"attempt":n,"how":"continued"})));
    }
    let Some(session) = member.session.clone() else { return Ok(None) };
    let rules = super::super::iron_rules::for_team(&snapshot, &member)?;
    let extra = continuation_extra(task);
    let text = format!("{}{}", crash_preamble(&snapshot, task, attempt), hand_off_text_rules(&snapshot, task, attempt, "", &member.computer, &extra, &rules));
    let request = uuid::Uuid::new_v4().to_string();
    let outcome = super::doorbell::post_and_ring(store, &team.id, member_id, &session, &request, "hand_out", "zerus", Urgency::Normal, &text);
    let mut how = "";
    change(store, &team.id, |team, events| {
        let at = now();
        let role = team.role_of(member_id);
        {
            let task = team.task_mut(task_id).context("unknown task")?;
            let Some(attempt) = task.attempts.iter_mut().find(|attempt| attempt.id == attempt_id) else { return Ok(()) };
            match &outcome {
                Sent::Delivered => {
                    attempt.state = AttemptState::Delivered;
                    attempt.delivered_at = Some(at);
                    attempt.error.clear();
                    task.status = TaskStatus::InProgress;
                    how = "resent";
                }
                Sent::Uncertain(error) => {
                    attempt.error = format!("the session crashed and was restarted; the task may have been sent again ({error})");
                    how = "uncertain";
                }
                Sent::Refused(error) => {
                    attempt.error = format!("the session crashed and was restarted, but the task was not sent again: {error}");
                    how = "refused";
                }
            }
        }
        match how {
            "resent" => events.push(Event::new("attempt_resent", json!({"task":task_id,"attempt":n,"member":role}))),
            "refused" => events.push(Event::new("attempt_resend_refused", json!({"task":task_id,"attempt":n,"member":role}))),
            _ => events.push(Event::new("attempt_resend_uncertain", json!({"task":task_id,"attempt":n,"member":role}))),
        }
        if how == "resent" {
            team.crash_recovered.push(json!({"task":task_id,"attempt":n,"member":role,"at":at,"how":"resent"}));
            if team.crash_recovered.len() > 20 {
                let excess = team.crash_recovered.len() - 20;
                team.crash_recovered.drain(..excess);
            }
        }
        Ok(())
    })?;
    Ok(match how {
        "resent" => Some(json!({"task":task_id,"attempt":n,"how":"resent"})),
        _ => None,
    })
}

/// Task #227: one sweep over the running teams after an update (the sessions
/// were restarted) or at a worker's start: every attempt a crash left
/// unconfirmed goes back to its member. The lead reads the batch as one item.
pub(crate) fn recover_crashed(store: &mut Store, seen: &[Seen], _now: f64) -> anyhow::Result<Vec<Value>> {
    let mut done = Vec::new();
    for team in store.active_teams(1000)? {
        if team.status != TeamStatus::Running {
            continue;
        }
        for task in team.tasks.clone() {
            let Some(attempt) = task
                .attempts
                .iter()
                .rev()
                .find(|attempt| attempt.state == AttemptState::Unconfirmed && attempt.error.contains("the session crashed"))
            else {
                continue;
            };
            let Some(member) = team.member(&attempt.member) else { continue };
            // Task 254: a paused team, phase or member, a draining member or a
            // held task keeps its attempt until the pause ends.
            if crash_held(&team, &task, member).is_some() {
                continue;
            }
            let Some(found) = session_of(member, seen) else { continue };
            if !found.live {
                continue;
            }
            let busy = !found.free;
            if !busy && member.context_compact.is_some() {
                continue;
            }
            // Task 254: a cold, large context is compacted first; the tick
            // gives the attempt back once that is confirmed.
            if !busy && cold_large(&team, member) {
                start_member_compact(store, &team.id, &attempt.member, "cold")?;
                done.push(json!({"task":task.id,"attempt":attempt.n,"how":"compacting"}));
                continue;
            }
            if let Some(result) = recover_crash(store, &team, task.id, &attempt.member, busy)? {
                done.push(result);
            }
        }
    }
    Ok(done)
}

/// Task #227: the sweep a GUI install or update runs: the update restarted the
/// sessions, so their attempts are given back now instead of waiting for the
/// lead's retry.
pub(crate) fn recover_after_update() -> anyhow::Result<Value> {
    if !super::store::path().exists() {
        return Ok(json!({"recovered":[]}));
    }
    let mut store = Store::open(true)?;
    Ok(json!({"recovered": sweep_crashed(&mut store)?}))
}

/// Task #227: one sweep with a fresh session listing. Only a team with an
/// attempt a crash left unconfirmed pays for the listing.
fn sweep_crashed(store: &mut Store) -> anyhow::Result<Vec<Value>> {
    let crashed = store.active_teams(1000)?.iter().any(|team| {
        team.status == TeamStatus::Running
            && team
                .tasks
                .iter()
                .flat_map(|task| &task.attempts)
                .any(|attempt| attempt.state == AttemptState::Unconfirmed && attempt.error.contains("the session crashed"))
    });
    if !crashed {
        return Ok(Vec::new());
    }
    let output = hgs("", &["ls", "--json", "--no-archives"], None, LIST_TIMEOUT)?;
    anyhow::ensure!(output.status.success(), "hgs ls failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    let listing: Value = serde_json::from_slice(&output.stdout).context("invalid hgs ls output")?;
    let sessions = seen(&listing);
    recover_crashed(store, &sessions, now())
}

/// Task #227: the words of a retry that continues the work: nothing was
/// revoked, so the member goes on and reports under the attempt it has.
fn continue_text(team: &Team, notice: &ContinueNotice) -> String {
    let by = if notice.by.is_empty() { "Lead".to_owned() } else { notice.by.clone() };
    format!(
        "[Zerus team \u{b7} from: {by} \u{b7} task: #{} \u{b7} continue]\n\
         Continue task #{} (attempt {}) from where you are: nothing was revoked and you do not have to start over. \
         Finish it as usual, then hgs team report with attempt {} (team {}).",
        notice.task, notice.task, notice.attempt, notice.attempt, team.name
    )
}

/// Task #227: a retry's "continue" reaches its member at a safe point; a refused
/// send is tried on later ticks, ten times at most.
fn tell_continues(store: &mut Store, team: &Team) -> anyhow::Result<()> {
    for notice in team.continues.iter().filter(|notice| notice.notice == Notice::Pending) {
        let Some(member) = team.member(&notice.member) else { continue };
        let Some(session) = member.session.clone() else { continue };
        let text = continue_text(team, notice);
        let outcome = super::doorbell::post_and_ring(store, &team.id, &notice.member, &session, &notice.id, "continue", "lead", Urgency::Normal, &text);
        change(store, &team.id, |team, events| {
            let Some(found) = team.continues.iter_mut().find(|found| found.id == notice.id) else { return Ok(()) };
            match &outcome {
                Sent::Delivered => {
                    found.notice = Notice::Sent;
                    events.push(Event::new("continue_sent", json!({"task":found.task,"attempt":found.attempt})));
                }
                Sent::Refused(_) => {
                    found.tries += 1;
                    if found.tries >= 10 {
                        found.notice = Notice::Unconfirmed;
                        events.push(Event::new("continue_undelivered", json!({"task":found.task,"attempt":found.attempt})));
                    }
                }
                Sent::Uncertain(_) => {
                    found.notice = Notice::Unconfirmed;
                    events.push(Event::new("continue_undelivered", json!({"task":found.task,"attempt":found.attempt})));
                }
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// Whether a refused native answer can only be given in the session's
/// Terminal: Kimi's adapters say so whenever Zerus cannot prove which
/// request its panel shows.
pub(crate) fn terminal_only(error: &str) -> bool {
    error.contains("in Terminal") || error.contains("enlarge Terminal")
}

/// U52: too many crashes this hour leave the session stopped; the lead hears
/// once, through the inbox item `crash_told` shows. Task #334: a resume that
/// arrives at the bound is spent too - the lead hears once, and the member
/// waits for the next hour or a relaunch instead of looping.
fn crash_stopped(store: &mut Store, team: &Team, member_id: &str) -> anyhow::Result<()> {
    let at = now();
    change(store, &team.id, |team, events| {
        let role = team.role_of(member_id);
        let found = team.members.iter_mut().find(|found| found.id == member_id).context("unknown member")?;
        found.crash = Some(at);
        found.crash_told = true;
        found.relaunch_exited = false;
        let restarts = found.restarts.iter().filter(|restart| at - **restart <= RESTART_WINDOW).count();
        events.push(Event::new("member_crashed", json!({"member":role,"restarts":restarts})));
        Ok(())
    })
}

/// The last lines of a job's output, bounded (C12).
fn output_tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(12)..].join("\n");
    if tail.is_empty() {
        "(no output captured; check its session with hgs inspect)".into()
    } else if tail.chars().count() > 1000 {
        tail.chars().take(1000).collect::<String>() + "\n[cut]"
    } else {
        tail
    }
}

/// The message that tells the lead a Background job passed its red time (C12).
pub(crate) fn budget_text(team: &Team, member: &Member, command: &str, since: f64, now: f64, last_output: &str) -> String {
    let role = team.name_of(&member.id);
    let minutes = (((now - since).max(0.0)) / 60.0).round() as u64;
    let command = if command.is_empty() { "(command not reported)" } else { command };
    match member.budget.as_ref() {
        Some(budget) => format!(
            "[Zerus team \u{b7} from: Zerus \u{b7} budget red]\n{role}: \"{}\" expected {} min, red after {} min; {minutes} min now.\n\
             Command: {command}\nLast output:\n{last_output}\n\
             Check whether the job hangs or died; a budget never stops it (hgs guide team-lead).",
            budget.label, budget.minutes, budget.red_minutes
        ),
        None => format!(
            "[Zerus team \u{b7} from: Zerus \u{b7} budget red]\n{role}'s Background work has run {minutes} min without a declared budget \
             (red after {} min).\nCommand: {command}\nLast output:\n{last_output}\n\
             Check whether the job hangs or died (hgs guide team-lead).",
            team.settings.background_red_minutes
        ),
    }
}

/// Tells the free lead once that a job is red, with its command and last
/// output lines (C12).
fn tell_lead_budget_red(
    store: &mut Store,
    team: &Team,
    member_id: &str,
    default: bool,
    command: String,
    since: f64,
) -> anyhow::Result<()> {
    let member = team.member(member_id).context("unknown member")?.clone();
    let session = member.session.clone().context("the member has no session")?;
    let lead = team.member(&team.lead).and_then(|m| m.session.clone()).context("the lead has no session")?;
    let last_output = last_reply(&session).unwrap_or_default();
    let start = if since > 0.0 { since } else { member.budget.as_ref().map(|budget| budget.elapsed(now())).map(|elapsed| now() - elapsed).unwrap_or(now()) };
    let command = if command.is_empty() { member.budget.as_ref().and_then(|budget| budget.job.as_ref()).map(|job| job.command.clone()).unwrap_or_default() } else { command };
    let text = budget_text(team, &member, &command, start, now(), &output_tail(&last_output));
    let outcome = send(&lead, &uuid::Uuid::new_v4().to_string(), &text);
    if matches!(outcome, Sent::Refused(_)) {
        return Ok(());
    }
    let role = team.role_of(member_id);
    change(store, &team.id, |team, events| {
        let Some(found) = team.members.iter_mut().find(|found| found.id == member_id) else { return Ok(()) };
        let mut label = String::new();
        if default {
            if found.background_told {
                return Ok(());
            }
            found.background_told = true;
        } else if let Some(budget) = found.budget.as_mut() {
            if budget.red_told {
                return Ok(());
            }
            budget.red_told = true;
            label = budget.label.clone();
        } else {
            return Ok(());
        }
        let minutes = (((now() - start).max(0.0)) / 60.0).round() as u64;
        events.push(Event::new("budget_red", json!({"member":role,"label":label,"default":default,"minutes":minutes})));
        Ok(())
    })
}

/// B2: the member hears the lead's reason right after a denied permission.
pub(crate) fn denial_text(tool: &str, reason: &str) -> String {
    format!("[Zerus team \u{b7} from: Zerus \u{b7} permission denied]\nLead: denied {tool}: {reason}")
}

/// Task #37 (team ede724c4): the note an answered permission sends with its
/// reason: a denial as before, an Allow as well, naming who decided.
pub(crate) fn decision_text(tool: &str, deny: bool, speaker: &str, reason: &str) -> String {
    if deny && speaker == "Lead" {
        return denial_text(tool, reason);
    }
    let (title, verb) = if deny { ("permission denied", "denied") } else { ("permission allowed", "allowed") };
    format!("[Zerus team \u{b7} from: Zerus \u{b7} {title}]\n{speaker}: {verb} {tool}: {reason}")
}

pub(crate) fn send_decision(session: &SessionRef, tool: &str, deny: bool, speaker: &str, reason: &str) -> anyhow::Result<()> {
    match send(session, &uuid::Uuid::new_v4().to_string(), &decision_text(tool, deny, speaker, reason)) {
        Sent::Refused(error) => anyhow::bail!("the reason did not reach the member: {error}"),
        _ => Ok(()),
    }
}

/// B4: commits the member's work in its own folder, on its own team branch.
/// Never pushes, never amends, never changes branches.
/// C2: why a repository's own configuration (the `local` and `worktree`
/// scopes of `git config --list --show-scope --name-only`) cannot be committed
/// with, or None.
pub(crate) fn refused_repository_config(listing: &str) -> Option<String> {
    for line in listing.lines() {
        let Some((scope, key)) = line.split_once(char::is_whitespace) else { continue };
        if !matches!(scope, "local" | "worktree") {
            continue;
        }
        // Task 377 (owner, 2026-11-08): Zerus writes its own union driver for
        // the i18n catalogs; a repository carrying exactly that command as
        // merge.i18n.driver is not a foreign merge driver (every merge Zerus
        // runs overrides it with the same -c value).
        let (key, value) = key.split_once('=').unwrap_or((key, ""));
        let key = key.trim().to_lowercase();
        let reason = if key.starts_with("include.") || key.starts_with("includeif.") {
            "includes another Git config".to_owned()
        } else if key.starts_with("filter.") {
            "configures a Git filter".to_owned()
        } else if key.starts_with("merge.") && key.ends_with(".driver") {
            if key == "merge.i18n.driver" && value.trim() == super::merge_queue::I18N_MERGE_DRIVER {
                continue;
            }
            "configures a Git merge driver".to_owned()
        } else if matches!(key.as_str(), "core.hookspath" | "core.fsmonitor" | "gpg.program") || key.starts_with("gpg.") {
            format!("configures {key}")
        } else {
            continue;
        };
        return Some(reason);
    }
    None
}

pub(crate) fn commit_in(folder: &str, branch: &str, message: &str, paths: &[String]) -> anyhow::Result<Value> {
    let git = |args: &[&str]| -> anyhow::Result<std::process::Output> {
        // C2: every git call carries the hardened config and a timeout; the
        // repository's own hooks, fsmonitor and filters never run.
        let mut child = std::process::Command::new("git")
            .arg("-C")
            .arg(folder)
            .args([
                "-c",
                "core.hooksPath=",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "gpg.program=",
            ])
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| anyhow::anyhow!("git did not run: {error}"))?;
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        // C2: drain both pipes in threads started right after the spawn, while
        // git runs; otherwise a large output fills the pipe and git blocks
        // until the timeout. The repository's own hooks, fsmonitor, filters and
        // signing program never run.
        let out_pipe = child.stdout.take().map(|mut pipe| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
                buf
            })
        });
        let err_pipe = child.stderr.take().map(|mut pipe| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
                buf
            })
        });
        loop {
            if child.try_wait()?.is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("git {args:?} timed out");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let status = child.wait()?;
        let stdout = out_pipe.and_then(|handle| handle.join().ok()).unwrap_or_default();
        let stderr = err_pipe.and_then(|handle| handle.join().ok()).unwrap_or_default();
        Ok(std::process::Output { status, stdout, stderr })
    };
    // C2: configuration the repository itself controls - its .git/config and,
    // with extensions.worktreeConfig, its config.worktree - must not run code:
    // a hook, fsmonitor, signing program, filter or included file is refused
    // rather than silently ignored. Git lowercases section and key names, so
    // the keys are compared in lowercase.
    let listed = git(&["config", "--list", "--show-scope"])?;
    anyhow::ensure!(listed.status.success(), "{}", String::from_utf8_lossy(&listed.stderr).trim());
    if let Some(reason) = refused_repository_config(&String::from_utf8_lossy(&listed.stdout)) {
        anyhow::bail!("This repository {reason}; Zerus commits without it");
    }
    let current = git(&["rev-parse", "--abbrev-ref", "HEAD"])?;
    anyhow::ensure!(current.status.success(), "{}", String::from_utf8_lossy(&current.stderr).trim());
    let current = String::from_utf8_lossy(&current.stdout).trim().to_owned();
    anyhow::ensure!(current == branch, "This folder is on {current}, not {branch}");
    if paths.is_empty() {
        let added = git(&["add", "-A"])?;
        anyhow::ensure!(added.status.success(), "{}", String::from_utf8_lossy(&added.stderr).trim());
    } else {
        let mut args: Vec<&str> = vec!["add", "--"];
        args.extend(paths.iter().map(String::as_str));
        let added = git(&args)?;
        anyhow::ensure!(added.status.success(), "{}", String::from_utf8_lossy(&added.stderr).trim());
    }
    let staged = git(&["diff", "--cached", "--quiet"])?;
    if staged.status.success() {
        return Ok(json!({"status":"clean","files":0}));
    }
    let committed = git(&["commit", "--no-verify", "--no-gpg-sign", "-m", message])?;
    anyhow::ensure!(committed.status.success(), "{}", String::from_utf8_lossy(&committed.stderr).trim());
    let sha = git(&["rev-parse", "HEAD"])?;
    anyhow::ensure!(sha.status.success(), "{}", String::from_utf8_lossy(&sha.stderr).trim());
    let sha = String::from_utf8_lossy(&sha.stdout).trim().to_owned();
    let listed = git(&["show", "--name-only", "--pretty=format:"])?;
    let files = String::from_utf8_lossy(&listed.stdout).lines().filter(|line| !line.trim().is_empty()).count();
    Ok(json!({"status":"committed","sha":sha,"files":files}))
}

/// U21: the git route of the built-in template, run the way commit_in runs git
/// (C2): the repository's own config may name neither a hook path, fsmonitor,
/// an include, a filter nor a signing program, and hooks are disabled outright;
/// a clone member owns its .git, so its config must never run code here. N1: the
/// branch is the lead's own, passed in, never a name built into Zerus. N12: the
/// branch is fetched from the team's folder first, so a clone merges the lead's
/// current work, not the copy frozen at clone time.
pub(crate) fn merge_lead(folder: &str, source: &str, branch: &str, timeout: Duration) -> super::templates::Outcome {
    let started = Instant::now();
    let deadline = started + timeout;
    let elapsed = || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    // A read, not a run: `worktrees::git` bounds it in time and output.
    let listing = match super::super::worktrees::git(Path::new(folder), &["config", "--list", "--show-scope"], deadline) {
        Ok(out) => String::from_utf8_lossy(&out).into_owned(),
        Err(error) => {
            return super::templates::Outcome {
                exit: Some(1),
                tail: format!("This repository's config was not read: {}", error.detail),
                ms: elapsed(),
                ..Default::default()
            }
        }
    };
    if let Some(reason) = refused_repository_config(&listing) {
        return super::templates::Outcome {
            exit: Some(1),
            tail: format!("This repository {reason}; Zerus merges without it"),
            ms: elapsed(),
            ..Default::default()
        };
    }
    let hooks = format!("core.hooksPath={}", if cfg!(windows) { "NUL" } else { "/dev/null" });
    // N12: a clone keeps its own copy of the lead's branch from the day it was
    // made, so merging that name says "Already up to date" without the lead's
    // work. Fetch the branch from the team's folder and merge what came. A
    // worktree shares the team's refs, where the branch is current already, so
    // there is nothing to fetch and no transport to run.
    let fetch_from_source = !same_repository(folder, source, deadline);
    if fetch_from_source {
        let mut fetch = std::process::Command::new("git");
        fetch
            .arg("-C")
            .arg(folder)
            .args(["-c", hooks.as_str(), "-c", "core.fsmonitor=false"])
            .arg("--no-optional-locks")
            .args(["fetch", "--no-tags", "--quiet", source, branch]);
        let remaining = deadline.saturating_duration_since(Instant::now()).max(Duration::from_secs(1));
        let fetched = super::templates::run_tree(&mut fetch, remaining, None);
        if fetched.exit != Some(0) || fetched.timed_out {
            return super::templates::Outcome {
                exit: Some(1),
                tail: format!("The lead's branch was not fetched from {source}:\n{}", fetched.tail),
                ms: elapsed(),
                ..Default::default()
            };
        }
    }
    let merge = if fetch_from_source { "FETCH_HEAD" } else { branch };
    let driver = super::merge_queue::i18n_driver_config();
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(folder)
        .args(["-c", hooks.as_str(), "-c", "core.fsmonitor=false", "-c", "commit.gpgsign=false", "-c", "gpg.program="])
        .args(["-c", driver.as_str()])
        .arg("--no-optional-locks")
        .args(["merge", "--no-edit", merge]);
    let remaining = deadline.saturating_duration_since(Instant::now()).max(Duration::from_secs(1));
    let mut outcome = super::templates::run_tree(&mut command, remaining, None);
    // Task 377 (owner, 2026-11-08): the project's generated files (its catalogs)
    // refresh in the merge commit, so both sides' strings stay.
    if outcome.exit == Some(0) {
        if let Some(head) = super::merge_queue::regenerate_and_amend(Path::new(folder)) {
            outcome.tail = format!("{} {head}", outcome.tail.trim()).trim().to_owned();
        }
    }
    outcome.ms = elapsed();
    outcome
}

/// N12: whether two folders are the same repository (a worktree and its main
/// checkout share refs); a clone is not.
pub(crate) fn same_repository(folder: &str, source: &str, deadline: Instant) -> bool {
    let common = |path: &str| {
        super::super::worktrees::git(Path::new(path), &["rev-parse", "--path-format=absolute", "--git-common-dir"], deadline)
            .map(|out| String::from_utf8_lossy(&out).trim().to_owned())
            .ok()
    };
    match (common(folder), common(source)) {
        (Some(folder), Some(source)) => same_path(&folder, &source),
        _ => false,
    }
}

fn send_brief(store: &mut Store, team_id: &str) -> anyhow::Result<()> {
    let rules_team = store.load(team_id)?.context("Unknown team")?;
    let rules = super::super::iron_rules::for_team(
        &rules_team,
        rules_team
            .member(&rules_team.lead)
            .context("Unknown lead")?,
    )?;
    let brief_id = uuid::Uuid::new_v4().to_string();
    let mut message = None;
    // Task 135: a swapped lead's first message starts with the handoff brief.
    let recent = recent_events(store, team_id);
    let mut handoff: Option<(handoff::Built, String)> = None;
    change(store, team_id, |team, _| {
        anyhow::ensure!(team.brief == Notice::Pending, "already briefed");
        let lead = team.member(&team.lead).and_then(|m| m.session.clone()).context("the lead has no session")?;
        let mut text = brief_text_rules(team, &rules);
        if let Some(need) = team.member(&team.lead).and_then(|member| member.handoff.clone()) {
            let pulses = handoff::pulse_lines(team, &team.lead, &recent, now());
            let built = handoff::build(team, &team.lead, handoff::Why::Swap(&need), &pulses, handoff::BRIEF_UNITS);
            text = format!("{}\n{}", built.text, text);
            handoff = Some((built, need.why));
        }
        message = Some((lead, text));
        team.brief = Notice::Sending;
        team.brief_id = brief_id.clone();
        Ok(())
    })?;
    let (lead, text) = message.unwrap();
    let outcome = send(&lead, &brief_id, &text);
    let lead_id = rules_team.lead.clone();
    change(store, team_id, |team, events| {
        if team.brief != Notice::Sending || team.brief_id != brief_id {
            return Ok(());
        }
        if !matches!(outcome, Sent::Refused(_)) {
            if let Some((built, why)) = &handoff {
                handoff_sent(team, events, &lead_id, built, why);
            }
        }
        team.brief = match &outcome {
            Sent::Delivered => Notice::Sent,
            Sent::Refused(_) => Notice::Pending,
            Sent::Uncertain(_) => Notice::Unconfirmed,
        };
        match &outcome {
            Sent::Delivered => events.push(Event::new("briefed", json!({"by":"Zerus"}))),
            Sent::Uncertain(error) => events.push(Event::new("brief_unconfirmed", json!({"error":error}))),
            Sent::Refused(_) => {}
        }
        Ok(())
    })
}

/// The lead hears once that every task is decided: a refusal leaves the
/// message pending, anything else marks it sent (an uncertain one is never
/// re-sent). New work clears `idle` and re-arms the message.
fn send_all_decided(store: &mut Store, team_id: &str) -> anyhow::Result<()> {
    let message_id = uuid::Uuid::new_v4().to_string();
    let mut message = None;
    change(store, team_id, |team, _| {
        anyhow::ensure!(team.status == TeamStatus::Running && team.idle == Notice::Pending, "nothing to tell");
        let lead = team.member(&team.lead).and_then(|m| m.session.clone()).context("the lead has no session")?;
        message = Some((lead, all_decided_text(team)));
        team.idle = Notice::Sending;
        Ok(())
    })?;
    let (lead, text) = message.unwrap();
    let outcome = send(&lead, &message_id, &text);
    change(store, team_id, |team, events| {
        if team.idle != Notice::Sending {
            return Ok(());
        }
        team.idle = match &outcome {
            Sent::Delivered => Notice::Sent,
            Sent::Refused(_) => Notice::Pending,
            Sent::Uncertain(_) => Notice::Unconfirmed,
        };
        if !matches!(outcome, Sent::Refused(_)) {
            events.push(Event::new("lead_told", json!({"member":"Lead","kind":"all_decided"})));
        }
        Ok(())
    })
}

/// Tests outside the dispatcher (task #13's walk from New team to running
/// members): one step as the worker runs it.
#[cfg(test)]
pub(crate) fn test_run_step(store: &mut Store, team: &Team, step: &Step) -> anyhow::Result<()> {
    run_step(store, team, step, "")
}

fn run_step(store: &mut Store, team: &Team, step: &Step, host: &str) -> anyhow::Result<()> {
    if matches!(step, Step::Brief | Step::SubBrief{..} | Step::Launch{..} | Step::HandOut{..} | Step::StageOut{..}) {
        match super::super::iron_rules::project_identity(team) {
            Err(error) => {
                let detail = error.to_string();
                if team.iron_rules_error != detail || team.iron_rules_error_at.is_none() {
                    change(store, &team.id, |team, events| {
                        team.iron_rules_error = detail.clone();
                        team.iron_rules_error_at = Some(now());
                        events.push(Event::new("iron_rules_blocked", json!({"reason":detail})));
                        Ok(())
                    })?;
                }
                return Err(error);
            }
            Ok(_) if !team.iron_rules_error.is_empty() => {
                change(store, &team.id, |team, _| { team.iron_rules_error.clear(); team.iron_rules_error_at = None; Ok(()) })?;
            }
            Ok(_) => {},
        }
    }
    match step {
        Step::Resume { task, member } => resume_parked(store, team, *task, member),
        Step::DetectWait { task } => detect_wait(store, team, *task),
        Step::TellDecision { task, decision } => tell_decision(store, team, *task, decision),
        Step::ScanReport { task } => scan_report(store, team, *task),
        Step::SpecDecisions => spec_decisions(store, team),
        Step::WorkingMode { preset, title, reasons } => propose_working_mode(store, team, preset, title, reasons),
        Step::ModeCheck => check_working_mode(store, team),
        Step::LeadBrief { reason } => send_lead_brief(store, team, reason),
        Step::Brief => send_brief(store, &team.id),
        Step::AllDecided => send_all_decided(store, &team.id),
        Step::Permission { member } => sync_permission(store, team, member),
        // The owner (2026-10-28): a question the session no longer asks (you
        // answered it on the desktop, or it went) closes in the team too: its
        // record leaves the member's open question and the answer is kept as
        // unknown, as sync_permission does (N5); it showed open for an hour.
        Step::ClearPermission { member } => change(store, &team.id, |team, events| {
            let role = team.role_of(member);
            if let Some(found) = team.members.iter_mut().find(|found| found.id == *member) {
                if let Some(pending) = found.pending.take() {
                    let answer = permission_lost(&mut found.permission_answers, &pending.question_id, now());
                    events.push(Event::new("permission_answered", json!({"member":role,
                        "question_id":answer["question_id"],"decision":answer["decision"],"by":answer["by"]})));
                }
                found.permission = None;
                found.terminal_question = None;
                drop_stale_escalation(found, &role, events);
            }
            Ok(())
        }),
        Step::Inbox => tell_lead_inbox(store, team, true),
        Step::InboxWaits => tell_lead_inbox(store, team, false),
        Step::SubInbox { member } => tell_sub_inbox(store, team, member),
        Step::SubBrief { member } => send_sub_brief(store, team, member),
        Step::RingMessages { member } => super::doorbell::ring(store, team, member).map(drop),
        Step::InterruptMessages { member, turn, run, conversation } => super::doorbell::interrupt(store, team, member, *turn, run, conversation),
        Step::ContinueUrgent { member } => super::doorbell::continue_after_interrupt(store, team, member),
        Step::RemindReviewer { task, member } => remind_reviewer(store, team, *task, member),
        Step::RemindReport { task } => remind_report(store, team, *task),
        Step::Unassigned { member, what, task } => change(store, &team.id, |team, events| {
            let at = now();
            let Some(found) = team.members.iter_mut().find(|found| found.id == *member) else { return Ok(()) };
            if found.unassigned.is_none() {
                found.unassigned = Some(Unassigned { since: at, what: what.clone(), task: *task });
                events.push(Event::new("unassigned_work", json!({"member":member,"what":what,"task":task})));
            }
            Ok(())
        }),
        Step::UnassignedEnded { member } => change(store, &team.id, |team, events| {
            let Some(found) = team.members.iter_mut().find(|found| found.id == *member) else { return Ok(()) };
            if found.unassigned.take().is_some() {
                events.push(Event::new("unassigned_work_ended", json!({"member":member})));
            }
            Ok(())
        }),
        Step::RecoverCrash { task, member, busy } => {
            recover_crash(store, team, *task, member, *busy).map(drop)
        }
        Step::Continues => tell_continues(store, team),
        Step::Starving { member } => change(store, &team.id, |team, _| {
            if let Some(found) = team.members.iter_mut().find(|found| found.id == *member) {
                if found.starved_since.is_none() {
                    found.starved_since = Some(now());
                    found.starved_told = false;
                }
            }
            Ok(())
        }),
        Step::StarvedEnded { member } => change(store, &team.id, |team, _| {
            if let Some(found) = team.members.iter_mut().find(|found| found.id == *member) {
                found.starved_since = None;
                found.starved_told = false;
            }
            Ok(())
        }),
        Step::WakeLanding { member, tasks } => wake_landing(store, team, member, tasks),
        Step::PlanStarved { members } => tell_lead_starved(store, team, members),
        Step::BackgroundStart { member } => change(store, &team.id, |team, _| {
            if let Some(found) = team.members.iter_mut().find(|found| found.id == *member) {
                if found.background_since.is_none() {
                    found.background_since = Some(now());
                    found.background_told = false;
                }
            }
            Ok(())
        }),
        Step::BackgroundEnd { member } => change(store, &team.id, |team, _| {
            if let Some(found) = team.members.iter_mut().find(|found| found.id == *member) {
                // C12: a budget ends with the job, and one that never met a job
                // within its minute is dropped.
                found.background_since = None;
                found.background_told = false;
                found.budget = None;
            }
            Ok(())
        }),
        Step::BudgetTie { member, command, since } => change(store, &team.id, |team, _| {
            if let Some(found) = team.members.iter_mut().find(|found| found.id == *member) {
                if let Some(budget) = found.budget.as_mut() {
                    if budget.job.is_none() {
                        budget.job = Some(JobRef { command: command.clone(), since: *since });
                    }
                }
            }
            Ok(())
        }),
        Step::BudgetAmber { member } => change(store, &team.id, |team, events| {
            let role = team.role_of(member);
            let mut told = None;
            if let Some(found) = team.members.iter_mut().find(|found| found.id == *member) {
                if let Some(budget) = found.budget.as_mut() {
                    if !budget.amber_told {
                        budget.amber_told = true;
                        told = Some((budget.label.clone(), budget.minutes, budget.red_minutes));
                    }
                }
            }
            if let Some((label, minutes, red)) = told {
                events.push(Event::new(
                    "budget_amber",
                    json!({"member":role,"label":label,"minutes":minutes,"red_minutes":red}),
                ));
            }
            Ok(())
        }),
        Step::BudgetRed { member } => tell_lead_budget_red(store, team, member, false, String::new(), 0.0),
        Step::BackgroundRed { member, command, since } => tell_lead_budget_red(store, team, member, true, command.clone(), *since),
        Step::CheckSilent { task, last } => check_dsh_silent(store, team, *task, *last),
        Step::CheckStuck { member, last, limit, reason } => check_dsh_stuck(store, team, member, *last, *limit, reason),
        Step::Silent { task, minutes } => tell_lead_silent(store, team, *task, *minutes, false),
        Step::Quiet { member, session, muted, revision } => {
            if !*muted {
                let input = json!({"run_id":session.run_id,"conversation_id":session.conversation_id,"muted":true,"expected_revision":revision});
                let output = hgs(&session.host, &["notifications", &session.name, "--json"], Some(&input), Duration::from_secs(60))?;
                // A changed preference is read again on the next pass.
                anyhow::ensure!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr).trim());
            }
            change(store, &team.id, |team, _| {
                let quiet = team.members.iter_mut().find(|m| m.id == *member).context("unknown member")?;
                quiet.quieted = session.conversation_id.clone();
                Ok(())
            })
        }
        Step::SwitchModel { task, member, from, model, effort } => switch_model(store, team, *task, member, from, model, effort),
        Step::ModelUncertain { task } => change(store, &team.id, |team, events| {
            let task_id = *task;
            let task = team.task_mut(task_id).context("unknown task")?;
            let Some(step) = task.prepare.iter_mut().rev().find(|step| step.step == "model" && step.result == "pending") else { return Ok(()) };
            step.result = "uncertain".into();
            step.reason = format!("the session did not show {} within {} s", step.to, super::models::CONFIRM_SECONDS as u64);
            let (member, to, reason) = (step.member.clone(), step.to.clone(), step.reason.clone());
            super::models::wait(task, "model", &format!("The switch to {to} is unconfirmed: {reason}; retry or reassign"), now());
            let role = team.role_of(&member);
            events.push(Event::new("model_switch_uncertain", json!({"task":task_id,"member":role,"to":to,"reason":reason})));
            Ok(())
        }),
        Step::TaskWaits { task, reason } => change(store, &team.id, |team, events| {
            let task_id = *task;
            let task = team.task_mut(task_id).context("unknown task")?;
            if task.waits.is_none() {
                super::models::wait(task, "model", reason, now());
                events.push(Event::new("task_waits", json!({"task":task_id,"on":"model","reason":reason})));
            }
            Ok(())
        }),
        Step::Settings { member } => {
            let found = team.member(member).cloned().context("unknown member")?;
            let result = apply_settings(&found);
            change(store, &team.id, |team, events| {
                let role = team.role_of(member);
                let found = team.members.iter_mut().find(|m| m.id == *member).context("unknown member")?;
                found.settings_applied = true;
                if let Err(error) = &result {
                    events.push(Event::new("member_settings_failed", json!({"member":role,"error":error.to_string()})));
                }
                Ok(())
            })
        }
        Step::ApplyModel { member } => apply_model(store, team, member),
        Step::ReplacePrompt { member } => replace_prompt(store, team, member),
        Step::Adopt { member, session } => {
            let mut filing = None;
            change(store, &team.id, |team, events| {
                let found = team.members.iter_mut().find(|m| m.id == *member).context("unknown member")?;
                let started = found.session.is_none();
                let computer = found.computer.clone();
                found.session = Some(session.clone());
                found.state = MemberState::Active;
                if started {
                    // Task #6: a new session takes the prompt and model it
                    // starts with; a change recorded for the session it
                    // replaced is done.
                    found.prompt_change = None;
                    found.model_switch = None;
                    let role = team.role_of(member);
                    events.push(Event::new("member_started", json!({"member":role,"session":session.name})));
                    // F1.1: the adopted session is filed into the team's project,
                    // which is its folder's (the team's project label is only a
                    // GUI group name, N36).
                    filing = Some(computer);
                }
                Ok(())
            })?;
            if let Some(computer) = filing {
                file_in_project(team, &computer, session);
            }
            Ok(())
        }
        Step::Launch { member } => {
            let (team, found) = reserve_launch(store, team, member)?;
            let outcome = start_member(&team, &found);
            finish_launch(store, &team, member, &found, outcome)
        }
        Step::HandOut { task, member } => {
            if *member == team.lead {
                lead_hand_out(store, &team.id, *task)
            } else {
                hand_out(store, &team.id, *task, member, None, host)
            }
        }
        Step::PrepareContext { task, member } => check_context_step(store, &team.id, *task, member, host),
        Step::RestartStuck { member } => restart_stuck(store, &team.id, member),
        Step::RelaunchRights { member } => relaunch_rights(store, &team.id, member),
        // Task 209: the context meter's three steps.
        Step::CompactIdle { member } => start_member_compact(store, &team.id, member, "threshold"),
        Step::CompactCold { member } => start_member_compact(store, &team.id, member, "cold"),
        Step::CheckCompact { member } => check_member_compact(store, &team.id, member),
        Step::SendHandoff { member } => send_handoff_now(store, &team.id, member),
        Step::AnswerContextClaim { task } => answer_context_claim(store, &team.id, *task),
        Step::RightsWait { task, reason } => change(store, &team.id, |team, events| {
            let task_id = *task;
            let task = team.task_mut(task_id).context("unknown task")?;
            if task.waits.is_none() {
                super::models::wait(task, "rights", reason, now());
                events.push(Event::new("task_waits", json!({"task":task_id,"on":"rights","reason":reason})));
            }
            Ok(())
        }),
        Step::AutoAcceptDraft { phase } => change(store, &team.id, |team, events| {
            super::ops::accept_auto_draft(team, *phase, &BTreeSet::new(), events);
            Ok(())
        }),
        Step::Drain { member, pause } => drain_member(store, team, member, *pause),
        Step::Rebalance { task, from, to } => hand_out(store, &team.id, *task, to, Some(from), host),
        Step::StageOut { task, member } => stage_hand_out(store, &team.id, *task, member, host),
        Step::StageCleanup { task, run } => stage_clean_up(store, team, *task, run),
        Step::StageRunEnded { task, run } => stage_run_ended(store, team, *task, run),
        Step::StageCommand { task, run } => start_stage_command(store, team, *task, run),
        Step::StageCommandEnded { task, run } => settle_stage_command(store, team, *task, run),
        Step::StageStalled { task, run, reason } => stall_stage_command(store, team, *task, run, reason),
        Step::StageSilent { task, run } => stage_silent_command(store, team, *task, run),
        Step::TellStop { stop } => tell_stop(store, team, stop),
        Step::InterruptStop { stop } => interrupt_stop(store, team, stop),
        Step::Rebalanced { tasks } => tell_lead_rebalanced(store, team, tasks),
        Step::TurnEnded { task, at, reply } => {
            // Task 163: the reply that ended the turn is kept for the lead to
            // read and to use as the report explicitly; it is never the report
            // by itself.
            let session = team
                .task(*task)
                .and_then(|t| t.attempts.last())
                .and_then(|a| team.member(&a.member))
                .and_then(|m| m.session.clone());
            let last_reply = session.and_then(|session| last_reply_of(&session, reply).ok().flatten());
            change(store, &team.id, |team, _| {
                if let Some(attempt) = team.task_mut(*task).and_then(|t| t.attempts.last_mut()) {
                    if attempt.state == AttemptState::Delivered && attempt.turn_ended_at.is_none() {
                        attempt.turn_ended_at = Some(*at);
                        attempt.turn_reply = reply.clone();
                        if let Some(last_reply) = last_reply.clone() {
                            attempt.last_reply = last_reply;
                        }
                        // The gate is open at this tick: the timeout starts here (C2).
                        attempt.report_ready_at = Some(now());
                        attempt.blocked_since = None;
                    }
                }
                Ok(())
            })?;
            // Task 133: the member's pulse for this turn; a failure to read
            // the session's events only means no automatic pulse.
            turn_pulse(store, &team.id, *task, *at);
            Ok(())
        }
        Step::RunAwaited { task, run, label } => change(store, &team.id, |team, _| {
            if let Some(attempt) = team.task_mut(*task).and_then(|t| t.attempts.last_mut()) {
                if attempt.state == AttemptState::Delivered {
                    attempt.awaited_run = run.clone();
                    attempt.awaited_label = label.clone();
                }
            }
            Ok(())
        }),
        Step::RunSettled { task, wake } => run_settled(store, team, *task, *wake),
        Step::GateOpen { task } => change(store, &team.id, |team, _| {
            // An old record (or a reopened gate): the timeout starts now (C2).
            if let Some(attempt) = team.task_mut(*task).and_then(|t| t.attempts.last_mut()) {
                if attempt.state == AttemptState::Delivered && attempt.turn_ended_at.is_some() && attempt.report_ready_at.is_none() {
                    attempt.report_ready_at = Some(now());
                    attempt.blocked_since = None;
                }
            }
            Ok(())
        }),
        Step::GateClosed { task } => change(store, &team.id, |team, _| {
            // A question or Background work holds the turn: restart the timeout
            // and remember since when it waits (C2, N5).
            if let Some(attempt) = team.task_mut(*task).and_then(|t| t.attempts.last_mut()) {
                if attempt.state == AttemptState::Delivered {
                    attempt.report_ready_at = None;
                    if attempt.blocked_since.is_none() {
                        attempt.blocked_since = Some(now());
                    }
                }
            }
            Ok(())
        }),
        Step::BlockedLate { task } => tell_lead_blocked(store, team, *task),
        Step::OwnJobFound { task } => change(store, &team.id, |team, events| {
            // Task #49 (team ede724c4): the member waits for its own job, so the
            // turn it ended is no No report; the report timeout starts again
            // once nothing of its own runs.
            let role = team.task(*task).and_then(|t| t.attempts.last()).map(|a| team.role_of(&a.member)).unwrap_or_default();
            if let Some(attempt) = team.task_mut(*task).and_then(|t| t.attempts.last_mut()) {
                if attempt.state == AttemptState::Delivered && attempt.no_report {
                    attempt.no_report = false;
                    attempt.report_ready_at = None;
                    events.push(Event::new("no_report_cleared", json!({"member":role,"task":task,"attempt":attempt.n,"reason":"waits for its own job"})));
                }
            }
            Ok(())
        }),
        Step::WorkResumed { task } => change(store, &team.id, |team, _| {
            // A member that works again is not a finished turn any more (A2).
            if let Some(attempt) = team.task_mut(*task).and_then(|t| t.attempts.last_mut()) {
                if attempt.state == AttemptState::Delivered {
                    attempt.turn_ended_at = None;
                    attempt.turn_reply.clear();
                    attempt.no_report = false;
                    attempt.report_ready_at = None;
                    attempt.blocked_since = None;
                }
            }
            Ok(())
        }),
        Step::WaitOnMember { task, member, reason } => change(store, &team.id, |team, events| {
            let found = team.task_mut(*task).context("unknown task")?;
            found.waits = Some(TaskWait {
                waits_for: "member".into(),
                on: vec![member.clone()],
                reason: reason.clone(),
                at: now(),
                state: "member".into(),
                ..Default::default()
            });
            events.push(Event::new(
                "task_waiting_member",
                json!({"task":task,"member":member,"reason":reason}),
            ));
            Ok(())
        }),
        Step::ReleaseWait { task } => change(store, &team.id, |team, _| {
            if let Some(found) = team.task_mut(*task) {
                if found.waits.as_ref().is_some_and(|wait| wait.waits_for == "member") {
                    found.waits = None;
                }
            }
            Ok(())
        }),
        Step::Watch { member, phase, activity, last_event_at, cold } => change(store, &team.id, |team, _| {
            let found = team.members.iter_mut().find(|found| found.id == *member).context("unknown member")?;
            found.seen_phase = phase.clone();
            found.seen_activity = activity.clone();
            found.seen_event_at = *last_event_at;
            found.seen_cold = *cold;
            Ok(())
        }),
        Step::MarkStuck { member, reason, phase, since } => {
            let role = team.role_of(member);
            change(store, &team.id, |team, events| {
                let found = team.members.iter_mut().find(|found| found.id == *member).context("unknown member")?;
                found.stuck = Some(MemberStuck { at: now(), reason: reason.clone(), phase: phase.clone(), since: Some(*since) });
                let minutes = ((now() - since) / 60.0).max(0.0).round() as u64;
                events.push(Event::new(
                    "member_stuck",
                    json!({"member":role,"id":member,"reason":reason,"phase":phase,"minutes":minutes}),
                ));
                Ok(())
            })
        }
        Step::Unstuck { member } => change(store, &team.id, |team, _| {
            let found = team.members.iter_mut().find(|found| found.id == *member).context("unknown member")?;
            found.stuck = None;
            Ok(())
        }),
        Step::Liveness { member, live } => change(store, &team.id, |team, _| {
            // The dispatcher sees the live sessions: one recorded liveness per
            // change lets status derive `stopped` by itself (L3).
            let found = team.members.iter_mut().find(|found| found.id == *member).context("unknown member")?;
            found.seen = Some(MemberSeen { at: now(), live: *live });
            if *live {
                // U52: a session that runs again can crash again later.
                found.crash_told = false;
                // #334: a launched resume needs nothing while the session runs.
                found.relaunch_exited = false;
            }
            Ok(())
        }),
        Step::Defer { member, question } => defer_request(store, team, member, question),
        Step::EscalateTerminal { member } => escalate_terminal(store, team, member),
        Step::Check { member, hooks } => {
            check_member(store, team, member, hooks);
            Ok(())
        }
        Step::ClearCheck { member } => change(store, &team.id, |team, events| {
            let role = team.role_of(member);
            if let Some(found) = team.members.iter_mut().find(|found| found.id == *member) {
                if found.check.take().is_some_and(|check| check.state == "uncertain") {
                    events.push(Event::new("member_check_settled", json!({"member":role})));
                }
            }
            Ok(())
        }),
        Step::TellLeadIdle => tell_lead_idle(store, team),
        Step::LeadStall { stall } => change(store, &team.id, |team, events| {
            if let Some((kind, data)) = super::lead_stall::event(team.lead_stall.as_ref(), stall.as_ref(), now()) {
                events.push(Event::new(kind, data));
            }
            team.lead_stall = stall.clone();
            Ok(())
        }),
        Step::Restart { member } => restart_crashed(store, team, member),
        Step::CrashStopped { member } => crash_stopped(store, team, member),
        Step::Jobs { member, jobs } => record_jobs(store, team, member, jobs, now()),
        Step::Services { member, services } => change(store, &team.id, |team, _| {
            if let Some(found) = team.members.iter_mut().find(|found| found.id == *member) {
                found.services = services
                    .iter()
                    .map(|service| MemberService {
                        name: service.name.clone(),
                        command: service.command.clone(),
                        since: service.since,
                    })
                    .collect();
            }
            Ok(())
        }),
        Step::Limited { member, limited } => change(store, &team.id, |team, events| {
            let role = team.role_of(member);
            let found = team.members.iter_mut().find(|found| found.id == *member).context("unknown member")?;
            let new_episode = limited.is_some() && found.limited.is_none();
            // N29: the end of the episode is withdrawn by StopLimitResume in
            // the same pass, with the session the request names.
            if let (true, Some(limit)) = (new_episode, limited.as_ref()) {
                events.push(Event::new(
                    "member_limited",
                    json!({"member":role,"resets_at":limit.resets_at,"detail":limit.detail}),
                ));
            }
            found.limited = limited.clone();
            Ok(())
        }),
        Step::TurnFailed { member, failed } => change(store, &team.id, |team, events| {
            let role = team.role_of(member);
            let found = team.members.iter_mut().find(|found| found.id == *member).context("unknown member")?;
            match failed {
                Some(failure) => events.push(Event::new(
                    "member_turn_failed",
                    json!({"member":role,"detail":failure.detail,"class":failure.class,"at":failure.at}),
                )),
                None if found.turn_failed.is_some() => events.push(Event::new("member_turn_recovered", json!({"member":role}))),
                None => {}
            }
            found.turn_failed = failed.clone();
            Ok(())
        }),
        Step::ContinueFailed { member } => continue_after_failure(store, team, member),
        Step::StartLimitResume { member } => set_limit_resume(store, &team.id, member, true),
        // N30: a refresh on another computer is a 60 s `hgs` call; once a
        // minute is enough inside the request's expiry.
        Step::RefreshLimitResume { member } => refresh_limit_resume(store, team, member),
        Step::StopLimitResume { member } => set_limit_resume(store, &team.id, member, false),
        Step::Started { task } => change(store, &team.id, |team, events| {
            let Some(member) = team.task(*task).and_then(|t| t.attempts.last()).map(|a| a.member.clone()) else {
                return Ok(());
            };
            let role = team.role_of(&member);
            let Some(attempt) = team.task_mut(*task).and_then(|t| t.attempts.last_mut()) else { return Ok(()) };
            // A1: recorded once, the first time the member is seen working.
            if attempt.state == AttemptState::Delivered && attempt.started_at.is_none() {
                attempt.started_at = Some(now());
                events.push(Event::new("started", json!({"task":task,"attempt":attempt.n,"member":role})));
            }
            Ok(())
        }),
        Step::StartLate { task } => change(store, &team.id, |team, events| {
            let Some(member) = team.task(*task).and_then(|t| t.attempts.last()).map(|a| a.member.clone()) else {
                return Ok(());
            };
            let role = team.role_of(&member);
            let Some(attempt) = team.task_mut(*task).and_then(|t| t.attempts.last_mut()) else { return Ok(()) };
            if attempt.state == AttemptState::Delivered && attempt.started_at.is_none() && !attempt.start_warned {
                attempt.start_warned = true;
                events.push(Event::new("start_late", json!({"task":task,"attempt":attempt.n,"member":role})));
            }
            Ok(())
        }),
        Step::TellStartLate { task } => tell_lead_start_late(store, team, *task),
        Step::AnswerQuestion { task, question } => send_answer(store, team, *task, question),
        Step::NoReport { task } => {
            // Owner, 2026-11-07: Zerus never makes the member's last reply its
            // report. The turn ended without `hgs team report`; the task shows
            // No report, and the lead reads the reply and decides (one action:
            // use it as the report). The reply is read now only to show it.
            let attempt = team.task(*task).and_then(|t| t.attempts.last()).cloned();
            let session = attempt.as_ref().and_then(|a| team.member(&a.member)).and_then(|m| m.session.clone());
            let mut last_reply = attempt.as_ref().map(|a| a.last_reply.clone()).unwrap_or_default();
            if last_reply.is_empty() {
                if let (Some(attempt), Some(session)) = (&attempt, &session) {
                    last_reply = if attempt.turn_ended_at.is_some() {
                        // A2/U20: without a recorded reply mark there is no id to
                        // compare with; N6 takes only a reply written after the
                        // attempt was delivered.
                        last_reply_of(session, &attempt.turn_reply).ok().flatten().unwrap_or_default()
                    } else {
                        last_reply_after(session, attempt.delivered_at.unwrap_or(attempt.sent_at))
                            .ok()
                            .flatten()
                            .unwrap_or_default()
                    };
                }
            }
            // Task 209: a reply that claims there is no context left while the
            // member's meter says otherwise is answered with the meter; it is
            // not a No report, and the attempt stays open.
            if let Some(attempt) = &attempt {
                let claimed = super::context::claims_exhausted(&last_reply);
                let fresh = team.member(&attempt.member).and_then(|member| member.context).is_some_and(|context| context.fresh(now()));
                if claimed && fresh && attempt.context_answered_at.is_none() {
                    return answer_context_claim(store, &team.id, *task);
                }
            }
            change(store, &team.id, |team, events| {
                let role = team.task(*task).and_then(|t| t.attempts.last()).map(|a| team.role_of(&a.member)).unwrap_or_default();
                if let Some(attempt) = team.task_mut(*task).and_then(|t| t.attempts.last_mut()) {
                    if attempt.state == AttemptState::Delivered && !attempt.no_report {
                        attempt.no_report = true;
                        attempt.report_ready_at = None;
                        if !last_reply.is_empty() {
                            attempt.last_reply = last_reply;
                        }
                        events.push(Event::new("no_report", json!({"member":role,"task":task,"attempt":attempt.n})));
                    }
                }
                Ok(())
            })?;
            // Task 161: a turn that ended waiting or blocked on an outside
            // event is offered the park, never kept as a wait.
            let reply = team_reply(store, &team.id, *task);
            if let (Some(attempt), Some(session)) = (attempt, session) {
                if super::wait_loops::waiting_reply(&reply) {
                    let (condition, what) = super::wait_loops::guess_condition(&reply, now());
                    let request = json!({"team":team.id,"task":task,"attempt":attempt.id,"condition":condition,"eta_minutes":60,"note":"..."});
                    let _ = send(&session, &uuid::Uuid::new_v4().to_string(), &format!(
                        "[Zerus team] Your turn on task #{task} ended waiting. A wait is a park, never a turn or a loop: {request} | hgs team park --json \
(a guess: until {what}; conditions ci, build, task, card, time; hgs guide team-park). Zerus watches it, gives the task back to you and tells the lead. \
Otherwise finish with hgs team report."
                    ));
                    let _ = change(store, &team.id, |_, events| {
                        events.push(Event::new("park_offered", json!({"task":task,"attempt":attempt.n,"until":what})));
                        Ok(())
                    });
                }
            }
            Ok(())
        }
    }
}

/// Task 161: the last reply kept on a task's newest attempt.
fn team_reply(store: &Store, team_id: &str, task_id: u32) -> String {
    store
        .load(team_id)
        .ok()
        .flatten()
        .and_then(|team| team.task(task_id).and_then(|task| task.attempts.last()).map(|attempt| attempt.last_reply.clone()))
        .unwrap_or_default()
}

/// After a restart nothing is re-sent: an attempt or report notice that was
/// being sent may have arrived.
fn recover(store: &mut Store) -> anyhow::Result<()> {
    for team in store.teams(false, 10_000)? {
        let stuck = team.brief == Notice::Sending
            || team.idle == Notice::Sending
            || team.tasks.iter().flat_map(|t| &t.attempts).any(|a| a.state == AttemptState::Delivering || a.notice == Notice::Sending);
        if !stuck {
            continue;
        }
        change(store, &team.id, |team, events| {
            if team.brief == Notice::Sending {
                team.brief = Notice::Unconfirmed;
            }
            if team.idle == Notice::Sending {
                team.idle = Notice::Unconfirmed;
            }
            for task in &mut team.tasks {
                for attempt in &mut task.attempts {
                    if attempt.state == AttemptState::Delivering {
                        attempt.state = AttemptState::Unconfirmed;
                        attempt.error = "the dispatcher stopped while sending".into();
                        events.push(Event::new("unconfirmed", json!({"task":task.id,"attempt":attempt.n,"error":attempt.error})));
                    }
                    if attempt.notice == Notice::Sending {
                        attempt.notice = Notice::Unconfirmed;
                    }
                }
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// One pass over every running team. Returns whether any team is active.
/// Task #406: the tick is measured (`tick_clock`); a slow one names its
/// slowest phase in the worker's log.
pub(crate) fn tick(store: &mut Store, host: &str) -> anyhow::Result<bool> {
    super::tick_clock::begin();
    let result = tick_teams(store, host);
    if let Some(record) = super::tick_clock::finish() {
        if record.ms >= super::tick_clock::SLOW_TICK.as_millis() {
            eprintln!("hgs team worker: tick took {} ms; slowest: {} ({} ms)", record.ms, record.slowest, record.slowest_ms);
        }
    }
    result
}

fn tick_teams(store: &mut Store, host: &str) -> anyhow::Result<bool> {
    let loaded: Vec<Team> = store.teams(false, 10_000)?;
    // Task 185: the pulses built on the worker since the last tick, and one
    // loaded team's expired pulses, retired here rather than at a turn end. A
    // finished team's rows are still reached: the worker keeps ticking for a
    // while after the last team ends.
    take_pulses(store);
    let offset = PRUNE_NEXT.load(std::sync::atomic::Ordering::Relaxed);
    prune_pulses_step(store, &loaded, now(), offset);
    PRUNE_NEXT.store(offset.wrapping_add(1), std::sync::atomic::Ordering::Relaxed);
    let teams: Vec<Team> = loaded
        .into_iter()
        .filter(|team| {
            matches!(team.status, TeamStatus::Running | TeamStatus::Paused)
                // N6: a draft still starting (or briefing) its lead is ticked too.
                || (matches!(team.status, TeamStatus::Planning | TeamStatus::Draft)
                    && matches!(team.brief, Notice::Pending | Notice::Sending))
                // N29: an ended team still withdraws its members' reset waits.
                || team.members.iter().any(|member| member.limit_resume)
        })
        .collect();
    // Task 97: an ordinary session the owner turned the queue on for keeps the
    // worker reading it.
    if teams.is_empty() && super::owner_queue::watched().is_empty() {
        return Ok(false);
    }
    let output = match super::tick_clock::phase("listing", tick_listing) {
        Some(result) => result?,
        // The previous session listing may still run: skip this tick, never
        // overlap two `hgs ls` children (U2).
        None => return Ok(true),
    };
    anyhow::ensure!(output.status.success(), "hgs ls failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    let listing: Value = serde_json::from_slice(&output.stdout).context("invalid hgs ls output")?;
    let sessions = seen(&listing);
    let listed = listed_hosts(&listing);
    // Task #406 (owner, 2026-11-09): hand-outs and answers first, for every
    // team, before any step that reads git, walks files or waits on a child.
    // A night of idle members came from hand-outs queued behind reports'
    // test deltas, merge checks and slow listings.
    for (index, team) in teams.iter().enumerate() {
        if index > 0 {
            std::thread::sleep(TEAM_YIELD);
        }
        // Task 277 (owner, 2026-11-08): a hand-out or stage message that
        // reached the member's turn is taken, not unread.
        if let Err(error) = super::doorbell::mark_taken(store, team) {
            eprintln!("hgs team worker: {}: taken hand-outs: {error}", team.name);
        }
        super::tick_clock::phase(&format!("hand-outs: {}", team.name), || run_team_steps(store, team, &sessions, &listed, host, Pass::Urgent));
    }
    sample_usage(&teams, &sessions);
    for team in &teams {
        // Task #406: a member's report reads git (its test delta); the outbox
        // waits for the next tick once this one has used its budget.
        if !within_budget("outbox", &team.id, OUTBOX_BUDGET) {
            continue;
        }
        if let Err(error) = super::tick_clock::phase(&format!("outbox: {}", team.name), || take_outbox(store, team, &sessions)) {
            eprintln!("hgs team worker: {}: outbox: {error}", team.name);
        }
    }
    // Task 209: the native context meter, a bounded number of inspects a tick;
    // the reload below carries the readings into this tick's steps.
    super::tick_clock::phase("contexts", || sample_contexts(store, &teams, &sessions, now()));
    let teams: Vec<Team> = teams.iter().filter_map(|t| store.load(&t.id).ok().flatten()).collect();
    defer_watched(&teams, &sessions);
    for (index, team) in teams.iter().enumerate() {
        // Task 287: a member's call waiting for the store gets it between two
        // teams (SQLite's busy wait queues nobody).
        if index > 0 {
            std::thread::sleep(TEAM_YIELD);
        }
        // A report the outbox took this tick may free a member: its next
        // hand-out goes now, before the team's other steps.
        super::tick_clock::phase(&format!("hand-outs: {}", team.name), || run_team_steps(store, team, &sessions, &listed, host, Pass::Urgent));
        super::tick_clock::phase(&format!("steps: {}", team.name), || run_team_steps(store, team, &sessions, &listed, host, Pass::Rest));
        // U62 (#717): the team's test queue. An empty queue is a no-op (no load
        // read); a read is at most once per 30 s, and results become notices.
        // U62 (#760): the merge queue first, so its tests run in the same tick.
        // Task #406: its git merges wait for a tick with time left.
        if within_budget("merge", &team.id, MERGE_BUDGET) {
            if let Err(error) = super::tick_clock::phase(&format!("merge queue: {}", team.name), || merge_queue_tick(store, &team.id, now())) {
                eprintln!("hgs team worker: {}: merge queue: {error}", team.name);
                merge_queue_failed(store, team, &format!("{error:#}"));
            }
        }
        // Task 174: queued suites run minutes (a merge's Qt suites tens of
        // minutes): never in the tick. One thread per team runs them.
        start_test_queue(store, &team.id, &team.name);
    }
    // Task 108: the build folder cleanup, at most every ten minutes, on a
    // thread of its own so removing gigabytes never holds up a tick.
    crate::state::build_dirs::maintain_in_background(now());
    Ok(true)
}

/// Task #406: the time a tick may have used before a team's outbox (reports
/// with their git reads) waits for the next tick.
const OUTBOX_BUDGET: Duration = Duration::from_secs(4);
/// Task #406: the same for a team's merge queue (its git merges).
const MERGE_BUDGET: Duration = Duration::from_secs(4);
/// Task #406: a team's other steps of one tick, after its hand-outs.
const STEPS_BUDGET: Duration = Duration::from_secs(2);
/// Task #406: a heavy phase skipped this many ticks in a row runs anyway, so
/// a busy worker never starves it.
const MAX_SKIPS: u32 = 5;

/// DCB-svn report (2026-11-12): a merge-queue step that fails writes nothing,
/// and its error went only to the worker's stderr, which nobody reads: the
/// queue stood for an hour with no word. The error goes to the worker log and
/// the team's events, once per distinct error in MERGE_FAILURE_REPEAT.
fn merge_queue_failed(store: &mut Store, team: &Team, error: &str) {
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    static TOLD: Mutex<BTreeMap<String, (String, f64)>> = Mutex::new(BTreeMap::new());
    let at = now();
    let error: String = error.chars().take(600).collect();
    {
        let Ok(mut told) = TOLD.lock() else { return };
        if told.get(&team.id).is_some_and(|(last, then)| *last == error && at - then < MERGE_FAILURE_REPEAT) {
            return;
        }
        told.insert(team.id.clone(), (error.clone(), at));
    }
    WorkerFiles::new(worker_dir()).log(&format!("{}: merge queue: {error}", team.name));
    let _ = change(store, &team.id, |team, events| {
        // The landing the step took (the one under test, else the oldest
        // queued) carries the error as its reason, so the lead's inbox names it.
        let testing = team.tasks.iter().position(|task| task.merge.as_ref().is_some_and(|entry| entry.state == "testing"));
        let oldest = || {
            team.tasks
                .iter()
                .enumerate()
                .filter_map(|(at, task)| task.merge.as_ref().filter(|entry| entry.state == "queued").map(|entry| (entry.at, at)))
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, at)| at)
        };
        let task = testing.or_else(oldest);
        if let Some(entry) = task.and_then(|at| team.tasks[at].merge.as_mut()).filter(|entry| entry.state == "queued") {
            entry.note = format!("The merge queue's step failed: {error}");
            entry.wait_reason = MergeWaitReason::Other;
        }
        events.push(Event::new("merge_queue_failed", json!({"error":error,"task":task.map(|at| team.tasks[at].id)})));
        Ok(())
    });
}

/// How often the same merge-queue error is told again.
const MERGE_FAILURE_REPEAT: f64 = 30.0 * 60.0;

/// Task #406: whether a heavy phase of `team` runs in this tick: while the
/// tick is within `budget`, or once it was skipped `MAX_SKIPS` ticks in a row.
fn within_budget(kind: &str, team: &str, budget: Duration) -> bool {
    use std::sync::Mutex;
    use std::collections::BTreeMap;
    static SKIPS: Mutex<Option<BTreeMap<String, u32>>> = Mutex::new(None);
    let key = format!("{kind}:{team}");
    let mut skips = SKIPS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let skips = skips.get_or_insert_with(BTreeMap::new);
    let skipped = skips.get(&key).copied().unwrap_or(0);
    if super::tick_clock::elapsed() <= budget || skipped >= MAX_SKIPS {
        skips.remove(&key);
        true
    } else {
        skips.insert(key, skipped + 1);
        false
    }
}

/// Task #406: which steps of a tick run first: hand-outs, resumes, adopting a
/// member's session and the messages and decisions a member waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pass {
    Urgent,
    Rest,
}

pub(crate) fn urgent(step: &Step) -> bool {
    matches!(
        step,
        Step::HandOut { .. }
            | Step::Resume { .. }
            | Step::Adopt { .. }
            | Step::Started { .. }
            | Step::TellDecision { .. }
            | Step::RingMessages { .. }
            | Step::InterruptMessages { .. }
            | Step::ContinueUrgent { .. }
    )
}

/// Task #406: the steps of one pass, in their order; the rest pass stops at
/// its budget and leaves the others to the next tick (they are derived from
/// the team again then).
pub(crate) fn pass_steps(steps: Vec<Step>, pass: Pass) -> Vec<Step> {
    steps.into_iter().filter(|step| urgent(step) == (pass == Pass::Urgent)).collect()
}

/// Owner, 2026-11-09 ("скоряй первый старт"): a member's start in three parts,
/// so a tick starts all its members at once: the launch id is reserved in the
/// store, the folder and the session are made without it (in parallel), and
/// the outcome is recorded in the store again.
fn reserve_launch(store: &mut Store, team: &Team, member: &str) -> anyhow::Result<(Team, Member)> {
        let launch_id = uuid::Uuid::new_v4().to_string();
        let mut planned = None;
        change(store, &team.id, |team, _| {
            let found = team.members.iter_mut().find(|m| m.id == member).context("unknown member")?;
            anyhow::ensure!(found.session.is_none() && found.launch_id.is_empty(), "already started");
            found.launch_id = launch_id.clone();
            let found = found.clone();
            planned = Some((team.clone(), found));
            Ok(())
        })?;
        Ok(planned.unwrap())
}

struct LaunchOutcome {
    checked_out: Option<String>,
    retired: Option<Option<String>>,
    /// Task #17: the replaced session still works in the member's folder:
    /// nothing was launched, the start is tried again on a later tick.
    busy: Option<String>,
    result: anyhow::Result<()>,
}

fn start_member(team: &Team, found: &Member) -> LaunchOutcome {
    let mut checked_out = None;
    let mut retired = None;
    let mut busy = None;
    let result = (|| -> anyhow::Result<()> {
        checked_out = prepare_member_branch(team, found)?;
        // Feedback 14: a swap's old session is set aside first, so the new
        // one can take the member's name. Task #17: one that still works is
        // waited for, never left beside the new one.
        match set_aside_replaced(found)? {
            SetAside::Busy(old) => {
                busy = Some(old);
                Ok(())
            }
            SetAside::Done(note) => {
                retired = Some(note);
                launch(team, found)
            }
        }
    })();
    LaunchOutcome { checked_out, retired, busy, result }
}

fn finish_launch(store: &mut Store, team: &Team, member: &str, found: &Member, outcome: LaunchOutcome) -> anyhow::Result<()> {
    let swapped = found.launch_after_swap;
    if let Some(note) = outcome.checked_out {
        change(store, &team.id, |team, events| {
            events.push(Event::new("folder_checked_out", json!({"member":team.role_of(&found.id),"note":note})));
            Ok(())
        })?;
    }
    if let Some(old) = outcome.busy {
        // Task #17: no launch; the start is tried again each tick, said once.
        change(store, &team.id, |team, events| {
            let role = team.role_of(member);
            if let Some(record) = team.members.iter_mut().find(|m| m.id == member && m.session.is_none()) {
                record.launch_id.clear();
                if !record.replaced_busy {
                    record.replaced_busy = true;
                    record.replaced_since = Some(now());
                    events.push(Event::new(
                        "launch_waiting",
                        json!({"member":role,"reason":format!(
                            "the replaced session {old} still works in the member's folder; it keeps its task and is paused when its turn ends, then the new session starts (stop or answer it in Sessions to start sooner)"
                        )}),
                    ));
                }
            }
            Ok(())
        })?;
        return Ok(());
    }
    if let Some(note) = outcome.retired {
        if note.is_some() || found.replaced.is_some() || found.replaced_busy {
            change(store, &team.id, |team, events| {
                if let Some(note) = &note {
                    events.push(Event::new("member_retired", json!({"member":team.role_of(member),"note":note})));
                }
                if let Some(record) = team.members.iter_mut().find(|m| m.id == member) {
                    record.replaced = None;
                    record.replaced_busy = false;
                    record.replaced_since = None;
                }
                Ok(())
            })?;
        }
    }
    let launched = outcome.result;
        if let Err(error) = launched {
            let error = error.to_string();
            // Task 22: a host that is still starting is waited for: the
            // member is started again on a later tick (a new launch id;
            // nothing was saved for this one), with no error and no inbox
            // item, until HOST_WAITS such starts have failed.
            let waits = found.host_waits;
            if host_still_starting(&error) && waits < HOST_WAITS {
                change(store, &team.id, |team, events| {
                    let role = team.role_of(member);
                    if let Some(record) = team.members.iter_mut().find(|m| m.id == member && m.session.is_none()) {
                        record.launch_id.clear();
                        record.host_waits = waits + 1;
                    }
                    events.push(Event::new(
                        "launch_waiting",
                        json!({"member":role,"reason":error,"wait":waits + 1,"of":HOST_WAITS}),
                    ));
                    Ok(())
                })?;
                return Ok(());
            }
            let error = if host_still_starting(&error) {
                format!("{error} (started {} times while the DeepSeek host was starting; it did not answer)", waits + 1)
            } else {
                error
            };
            // U54: a failed post-swap launch keeps `launch_after_swap`, so a
            // relaunch starts it at once; the error names its fix.
            let error = if swapped {
                format!("{error}; fix the folder or the old session, then hgs team relaunch starts it at once")
            } else {
                error
            };
            change(store, &team.id, |team, events| {
                if let Some(found) = team.members.iter_mut().find(|m| m.id == member && m.session.is_none()) {
                    found.error = error.clone();
                    found.launch_after_swap = swapped;
                    found.host_waits = 0;
                }
                events.push(Event::new("launch_failed", json!({"member":team.role_of(member),"error":error})));
                Ok(())
            })?;
        } else {
            change(store, &team.id, |team, _| {
                if let Some(found) = team.members.iter_mut().find(|m| m.id == member && m.session.is_none()) {
                    found.launch_after_swap = false;
                    found.host_waits = 0;
                }
                Ok(())
            })?;
        }
        Ok(())
}

/// Owner, 2026-11-09: the tick's member starts run side by side (each clone
/// and launch took 15-35 s, one after another).
fn run_launches(store: &mut Store, team: &Team, members: &[String]) {
    let mut reserved = Vec::new();
    for member in members {
        match reserve_launch(store, team, member) {
            Ok(pair) => reserved.push((member.clone(), pair)),
            Err(error) => eprintln!("hgs team worker: {}: Launch {{ member: {member:?} }}: {error}", team.name),
        }
    }
    let outcomes: Vec<LaunchOutcome> = std::thread::scope(|scope| {
        let handles: Vec<_> = reserved.iter().map(|(_, (team, found))| scope.spawn(move || start_member(team, found))).collect();
        handles.into_iter().map(|handle| handle.join().unwrap_or_else(|_| LaunchOutcome {
            checked_out: None, retired: None, busy: None, result: Err(anyhow::anyhow!("the member's start panicked")),
        })).collect()
    });
    for ((member, (fresh, found)), outcome) in reserved.iter().zip(outcomes) {
        if let Err(error) = finish_launch(store, fresh, member, found, outcome) {
            eprintln!("hgs team worker: {}: Launch {{ member: {member:?} }}: {error}", team.name);
        }
    }
}

/// The step's kind, for the tick's measurement.
fn step_kind(step: &Step) -> String {
    let text = format!("{step:?}");
    text.split([' ', '{', '(']).next().unwrap_or_default().to_owned()
}

/// Task 287: the pause between two teams of a tick, longer than SQLite's
/// longest busy-wait sleep, so a waiting member call takes the lock.
const TEAM_YIELD: Duration = Duration::from_millis(100);

/// Task 287 (owner, 2026-11-08): a team's steps of one tick are one
/// transaction. Every change the steps make goes to the store's working copy
/// of the team (`Store::begin_batch`), which is written once at the end; a
/// change with a request id or of the working tier writes it first.
fn run_team_steps(store: &mut Store, team: &Team, sessions: &[Seen], listed: &BTreeSet<String>, host: &str, pass: Pass) {
    if let Err(error) = store.begin_batch(&team.id) {
        eprintln!("hgs team worker: {}: {error:#}", team.name);
    }
    if let Err(error) = refresh_unread(store, team) {
        eprintln!("hgs team worker: {}: unread: {error:#}", team.name);
    }
    let fresh = store.load(&team.id).ok().flatten();
    let team = fresh.as_ref().unwrap_or(team);
    let started = Instant::now();
    let sessions = &with_outbox_runs(team, sessions);
    let steps = pass_steps(steps_with(team, sessions, now(), Some(listed)), pass);
    // Owner, 2026-11-09: two or more member starts of this tick run together.
    let launches: Vec<String> = steps.iter().filter_map(|step| match step { Step::Launch { member } => Some(member.clone()), _ => None }).collect();
    let steps = if launches.len() > 1 && super::super::iron_rules::project_identity(team).is_ok() {
        let at = Instant::now();
        run_launches(store, team, &launches);
        super::tick_clock::note(&format!("{}: Launch x{}", team.name, launches.len()), at.elapsed());
        steps.into_iter().filter(|step| !matches!(step, Step::Launch { .. })).collect()
    } else {
        steps
    };
    let total = steps.len();
    for (index, step) in steps.into_iter().enumerate() {
        // Task #406: the rest of a team's steps wait for the next tick once
        // this pass used its budget; hand-outs never wait.
        if pass == Pass::Rest && started.elapsed() > STEPS_BUDGET {
            eprintln!("hgs team worker: {}: {} step(s) left to the next tick (budget)", team.name, total - index);
            break;
        }
        let at = Instant::now();
        if let Err(error) = run_step(store, team, &step, host) {
            eprintln!("hgs team worker: {}: {step:?}: {error}", team.name);
        }
        super::tick_clock::note(&format!("{}: {}", team.name, step_kind(&step)), at.elapsed());
    }
    if let Err(error) = store.end_batch() {
        eprintln!("hgs team worker: {}: the tick's changes were not written: {error:#}", team.name);
    }
}

/// Task 287: marking messages read changes only their rows; the tick brings
/// each member's unread summary in the team record up to date from them, in
/// its own transaction, before its steps ring anyone.
pub(crate) fn refresh_unread(store: &mut Store, team: &Team) -> anyhow::Result<()> {
    let at = now();
    let mut fresh = Vec::new();
    for member in team.members.iter().filter(|member| member.unread.is_some()) {
        let summary = super::working::summary(store.connection(), &team.id, &member.id, member.unread.as_ref(), super::working::urgent_bound(team), at)?;
        if summary != member.unread {
            fresh.push((member.id.clone(), summary));
        }
    }
    if fresh.is_empty() {
        return Ok(());
    }
    change(store, &team.id, |team, _| {
        for (id, summary) in fresh {
            if let Some(member) = team.members.iter_mut().find(|member| member.id == id) {
                member.unread = summary;
            }
        }
        Ok(())
    })
}

/// DeepSeek reports only the running total of its tokens and keeps no history,
/// so the team profile takes an attempt's tokens from counter samples taken at
/// its ends (`usage_samples`). Reading a counter is an rpc to the session host
/// (35 s timeout), so the tick only tells the sampler thread which local
/// DeepSeek members run and which of them work; it never waits for a reading.
fn sample_usage(teams: &[Team], seen: &[Seen]) {
    let mut sessions: Vec<(String, bool)> = Vec::new();
    for team in teams {
        for member in team.members.iter().filter(|member| member.state != MemberState::Removed) {
            let Some(session) = member.session.as_ref().filter(|session| session.host.is_empty()) else { continue };
            let Some(found) = session_of(member, seen).filter(|found| found.agent == "dsh" && found.live) else { continue };
            if !sessions.iter().any(|(name, _)| *name == session.name) {
                sessions.push((session.name.clone(), !found.idle));
            }
        }
    }
    super::usage_sampler::SAMPLER.tick(&sessions, now());
}

/// The reading an attempt's or stage's start is measured from: the session's
/// counters at the hand-out. The sampler thread takes it and records it at this
/// moment (an idle member's counters do not move); the hand-out never waits.
fn sample_member_usage(team: &Team, member_id: &str) {
    let Some(session) = team.member(member_id).and_then(|member| member.session.as_ref()).filter(|session| session.host.is_empty()) else {
        return;
    };
    if super::super::dsh::exists(&session.name) {
        super::usage_sampler::SAMPLER.hand_out(&session.name, now());
    }
}

/// N13: the folder the worker reads a member's outbox from is the one the
/// member's sandbox writes to: the folder the store records for it, once that
/// agrees with its live session. An adopted session's folder is its own, never
/// a worktree name computed from it; a shared member may work in a subfolder of
/// the team's checkout, where its sandbox writes.
/// Task #49 (team ede724c4): whether the attempt's member waits for a job of
/// its own, by task 39's rule: its listed jobs (its Zerus build, the background
/// jobs its hooks registered - a shell job it reports as still running -, and
/// Background work under a budget), its own Background work or build, or the
/// run the dispatcher remembers it waits on. A session that is not live waits
/// for nothing.
pub(crate) fn waits_for_own_job(attempt: &Attempt, found: Option<&Seen>) -> bool {
    found.is_some_and(|found| {
        found.live && (found.building || found.background || !found.jobs.is_empty() || !attempt.awaited_run.is_empty())
    })
}

/// Task #49 (team ede724c4): the member's own template runs that its listing
/// cannot show: a request still queued in its outbox (the worker claims it on
/// a later tick, or after the folder's current run) and a claim made after the
/// listing was taken. They are its jobs as its Zerus build is. A claim is the
/// job of the member it names (`claim_of`); one that names nobody counts only
/// in a folder of its own.
fn with_outbox_runs(team: &Team, seen: &[Seen]) -> Vec<Seen> {
    let mut sessions = seen.to_vec();
    for member in team.members.iter().filter(|member| member.id != team.lead && member.state != MemberState::Removed) {
        let Some(found) = session_of(member, seen).filter(|found| found.live && found.host.is_empty() && !found.cwd.is_empty()) else { continue };
        let Some(index) = seen.iter().position(|other| std::ptr::eq(other, found)) else { continue };
        let folder = outbox_folder(team, member, &found.cwd);
        let mut jobs: Vec<SeenJob> = super::outbox::queued_runs(&folder, &member.id)
            .into_iter()
            .map(|(request, queued)| {
                let template = request["template"].as_str().unwrap_or_default();
                SeenJob {
                    kind: "build".into(),
                    what: format!("its own Zerus run ({}), queued", short(template, 80)),
                    command: format!("{template} {}", request["request_id"].as_str().unwrap_or_default()),
                    since: queued,
                }
            })
            .collect();
        let shared = team.members.iter().any(|other| {
            other.id != member.id && session_of(other, seen).is_some_and(|other| other.host.is_empty() && same_path(&other.cwd, &found.cwd))
        });
        if !found.building {
            for (_, marker) in super::outbox::running_all(&folder).into_iter().filter(|(_, marker)| claim_of(marker, &member.id, shared)) {
                let label = marker["label"].as_str().unwrap_or_default();
                jobs.push(SeenJob {
                    kind: "build".into(),
                    what: format!("its own Zerus run ({})", short(label, 80)),
                    command: marker["command"].as_str().unwrap_or_default().to_owned(),
                    since: marker["started"].as_f64().unwrap_or(0.0),
                });
            }
        }
        let known = &mut sessions[index].jobs;
        for job in jobs {
            if !known.iter().any(|old| old.command == job.command) {
                known.push(job);
            }
        }
    }
    sessions
}

/// Whether a run's claim is this member's. Members that may not have worktrees
/// work in one folder and its claims lie side by side, so the claim names the
/// member that asked for the run (`start_run`) and is nobody else's: a run one
/// member started showed every member of the folder as Building, wrote a job
/// for each of them, flagged the ones with no task as unassigned work and woke
/// another member's attempt with a result that was not its own. A claim that
/// names nobody (one written before claims named their member) is the
/// member's only in a folder no other member works in.
pub(crate) fn claim_of(claim: &Value, member: &str, shared: bool) -> bool {
    match claim["member"].as_str().filter(|owner| !owner.is_empty()) {
        Some(owner) => owner == member,
        None => !shared,
    }
}

fn outbox_folder(team: &Team, member: &Member, cwd: &str) -> PathBuf {
    match member_folder_path(team, member) {
        Ok(folder) if !folder.is_empty() && same_path(&folder, cwd) => PathBuf::from(folder),
        _ => PathBuf::from(cwd),
    }
}

/// N16: whether the request's member works in this same folder, so its request
/// is theirs to apply on their own pass.
fn shares_folder(team: &Team, request: &Value, seen: &[Seen], cwd: &str) -> bool {
    let Some(other) = team.member(request["member"].as_str().unwrap_or_default()) else { return false };
    other.id != team.lead
        && other.state != MemberState::Removed
        && session_of(other, seen).is_some_and(|found| found.host.is_empty() && !found.cwd.is_empty() && same_path(&found.cwd, cwd))
}

/// Applies the requests members left in their folders: a sandbox may not write
/// the team store, but it can write its own folder (U20). The folder proves the
/// member; the request must name that member and one of its running attempts.
/// C1: a run is only claimed and started here; a detached runner executes it and
/// settles the request, so no template can hold the tick.
fn take_outbox(store: &mut Store, team: &Team, seen: &[Seen]) -> anyhow::Result<()> {
    take_outbox_with(store, team, seen, &spawn_runner)
}

fn take_outbox_with(
    store: &mut Store,
    team: &Team,
    seen: &[Seen],
    spawn: &dyn Fn(&Path, &Value) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    if team.status != TeamStatus::Running && team.status != TeamStatus::Paused {
        return Ok(());
    }
    for member in team.members.iter().filter(|member| member.id != team.lead && member.state != MemberState::Removed) {
        let Some(found) = session_of(member, seen).filter(|found| !found.cwd.is_empty() && found.host.is_empty()) else { continue };
        let folder = outbox_folder(team, member, &found.cwd);
        let folder = folder.as_path();
        // C1: a runner that died leaves its claim behind; the member then hears
        // that the run stopped instead of waiting for a result that never comes.
        settle_stale_runs(folder, team);
        let Ok(requests) = super::outbox::pending(folder, super::outbox::PER_TICK) else { continue };
        for (path, request) in requests {
            // N16: members that may not have worktrees share one folder and one
            // outbox. A request another member of this folder queued there is
            // theirs to apply, never this member's to refuse and settle; only a
            // name nobody here answers to is refused below.
            if request["member"].as_str() != Some(member.id.as_str()) && shares_folder(team, &request, seen, &found.cwd) {
                continue;
            }
            // Task #28: a sandboxed member's own `hgs run` and its host reads.
            if matches!(request["op"].as_str(), Some("session-run" | "read")) {
                // Task 173: a sandboxed member's run belongs to its handed work too.
                let outcome = match (request["op"].as_str(), member.id != team.lead) {
                    (Some("session-run"), true) => match super::shadow::job_of(team, &member.id, request["input"]["attempt"].as_str().unwrap_or_default()) {
                        Ok(_) => sandbox_request(member, folder, &request),
                        Err(error) => json!({"status":"refused","request_id":request["request_id"],"error":error.to_string()}),
                    },
                    _ => sandbox_request(member, folder, &request),
                };
                if let Err(error) = super::outbox::settle(folder, &request, &outcome) {
                    eprintln!("hgs team worker: {}: outbox {}: {error}", team.name, path.display());
                }
                announce_outbox(store, team, member, folder, &request, &outcome);
                continue;
            }
            if request["op"] == "run" {
                match start_run(store, team, member, folder, &request, spawn) {
                    // A refusal is answered now; a claim needs no answer - the
                    // runner writes it, and a claim that could not start stays
                    // queued for the next tick.
                    Ok(Some(outcome)) => {
                        if let Err(error) = super::outbox::settle(folder, &request, &outcome) {
                            eprintln!("hgs team worker: {}: outbox {}: {error}", team.name, path.display());
                        }
                        announce_outbox(store, team, member, folder, &request, &outcome);
                    }
                    Ok(None) => {}
                    Err(error) => eprintln!("hgs team worker: {}: run {}: {error:#}", team.name, request["request_id"]),
                }
                continue;
            }
            let outcome = match apply_outbox(store, team, member, &request) {
                Ok(value) => value,
                // A refusal is final: the request leaves the outbox as well, or
                // the worker would try it again every tick.
                Err(error) => json!({"status":"refused","request_id":request["request_id"],"error":format!("{error:#}")}),
            };
            if let Err(error) = super::outbox::settle(folder, &request, &outcome) {
                eprintln!("hgs team worker: {}: outbox {}: {error}", team.name, path.display());
            }
            announce_outbox(store, team, member, folder, &request, &outcome);
        }
    }
    Ok(())
}

/// Task #28: what the worker does for a sandboxed member that its sandbox
/// cannot. `session-run` records and starts the member's `hgs run` as its
/// session's work, at the sandbox's Low integrity (`builds::start_for_sandbox`);
/// `read` answers one host read the sandbox cannot reach
/// (`outbox::sandbox_read_allowed`). The folder proves the member (U20); a
/// refusal is the answer, never a retry.
fn sandbox_request(member: &Member, folder: &Path, request: &Value) -> Value {
    let id = request["request_id"].as_str().unwrap_or_default();
    let refused = |error: String| json!({"status":"refused","request_id":id,"error":error});
    let Some(session) = member.session.as_ref().filter(|session| session.host.is_empty()) else {
        return refused("the member has no session on this computer".into());
    };
    match request["op"].as_str() {
        Some("session-run") => {
            let session = json!({"name":session.name,"run_id":session.run_id,"conversation_id":session.conversation_id});
            // Task #48: a Codex member's run keeps Codex's sandbox, any
            // other sandboxed member's Low integrity.
            let sandbox = if member.agent == "codex" { super::super::builds::Sandbox::Codex } else { super::super::builds::Sandbox::Low };
            super::super::builds::start_for_sandbox(id, &request["input"], session, folder, sandbox).unwrap_or_else(refused)
        }
        _ => {
            let argv: Vec<String> =
                request["input"]["argv"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
            if !super::outbox::sandbox_read_allowed(&argv) {
                return refused(format!("a sandbox asks only for hgs ls, inspect and task reads, not: hgs {}", argv.join(" ")));
            }
            let words: Vec<&str> = argv.iter().map(String::as_str).collect();
            match hgs("", &words, None, Duration::from_secs(45)) {
                Ok(output) => {
                    let mut stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                    let truncated = stdout.len() > super::outbox::READ_STDOUT;
                    if truncated {
                        let mut end = super::outbox::READ_STDOUT;
                        while !stdout.is_char_boundary(end) {
                            end -= 1;
                        }
                        stdout.truncate(end);
                    }
                    let stderr: String = String::from_utf8_lossy(&output.stderr).chars().take(64 * 1024).collect();
                    json!({"status":"ran","request_id":id,"exit":output.status.code().unwrap_or(1),"stdout":stdout,
                        "stderr":stderr,"truncated":truncated})
                }
                Err(error) => refused(format!("{error:#}")),
            }
        }
    }
}

/// N1: the branch the lead's own folder is on: what a member merges. It is read
/// from the team's folder, never built into Zerus, so another project gets its
/// own branch.
fn lead_branch(team: &Team) -> anyhow::Result<String> {
    // Task #19 (owner, 2026-11-10): a lead who lands by hand names its line.
    if super::merge_queue::manual(team) {
        return Ok(team.settings.manual_line.trim().to_owned());
    }
    // Task 377 (owner, 2026-11-08): with an integration folder the line is a
    // named branch, never whatever the folder has checked out.
    if !team.settings.integration_folder.trim().is_empty() {
        return super::merge_queue::integration_branch(team)
            .context("The team's line branch is not named: set base_branch (hgs team settings) or record the line");
    }
    // Task 174: the lead branch is the integration folder's branch.
    let folder = team.integration_folder();
    anyhow::ensure!(!folder.is_empty(), "The team has no folder to merge from");
    let deadline = Instant::now() + Duration::from_secs(20);
    let out = super::super::worktrees::git(Path::new(folder), &["rev-parse", "--abbrev-ref", "HEAD"], deadline)
        .map_err(|error| anyhow::anyhow!("the lead's branch was not read: {}", error.detail))?;
    let branch = String::from_utf8_lossy(&out).trim().to_owned();
    // Task #19: a team folder on no branch (another team's detached line)
    // merges the line the team names instead of refusing.
    if branch == "HEAD" {
        let named = [team.settings.base_branch.trim(), team.line_branch.trim()].into_iter().find(|name| !name.is_empty());
        return match named {
            Some(name) => {
                super::templates::check_branch(name)?;
                Ok(name.to_owned())
            }
            None => anyhow::bail!(
                "The team folder {folder} is on no branch: the lead names the team's line, {{\"manual_line\":\"BRANCH\"}} | hgs team settings --json"
            ),
        };
    }
    super::templates::check_branch(&branch)?;
    Ok(branch)
}

/// Whether `folder` is inside `root` (a shared member in a subfolder of the
/// team's checkout, not an own worktree or clone beside it).
fn inside(folder: &str, root: &str) -> bool {
    let (Ok(folder), Ok(root)) = (crate::platform::canonical(Path::new(folder)), crate::platform::canonical(Path::new(root))) else {
        return false;
    };
    folder != root && folder.starts_with(&root)
}

/// U36/#538: whether `attempt` is this member's: its own attempt, or a stage
/// run given to it while the stage is open. A stage that ended is neither, so
/// a run that started too late is refused rather than run elsewhere.
fn attempt_of(team: &Team, attempt: &str, member: &str) -> bool {
    team.tasks.iter().any(|task| task.attempts.iter().any(|found| found.id == attempt && found.member == member))
        || super::pipeline::open_run_task(team, attempt, member).is_some()
}

/// U21/C1: one queued run, executed outside the member's sandbox. N1: the merge
/// runs in the member's own folder and only when that is not the team's.
fn run_outcome(team: &Team, member: &Member, folder: &Path, request: &Value) -> anyhow::Result<super::templates::Outcome> {
    let template = request["template"].as_str().unwrap_or_default();
    let args = request["args"].clone();
    // Task 108: the build folder this run builds in is recorded for the
    // cleanup; it is Zerus's when Zerus created the member's folder or the run
    // creates it.
    let zerus_folder = matches!(member.folder, MemberFolder::Worktree | MemberFolder::Clone)
        && member_folder_path(team, member).is_ok_and(|mine| !mine.is_empty() && same_path(&mine, &folder.display().to_string()) && !same_path(&mine, &team.folder));
    for (path, kind) in crate::state::build_dirs::template_folders(template, &args, folder) {
        crate::state::build_dirs::note(crate::state::build_dirs::Use {
            path,
            kind,
            origin: "team_run",
            team: team.id.clone(),
            team_name: team.name.clone(),
            member: member.id.clone(),
            task: request["task"].as_u64().unwrap_or(0) as u32,
            session: member.session.as_ref().map(|session| session.name.clone()).unwrap_or_default(),
            zerus_folder,
        });
    }
    // Task 85: a project template's own timeout comes from the folder's tests file.
    let timeout = Duration::from_secs(super::templates::timeout_in(template, folder));
    // U36: a test stage builds in the task's folder (or its checkout), with
    // its own temporary folder, only while the stage is open.
    if let Some((stage_folder, temp)) = super::stages::run_folder(team, &member.id, request["attempt"].as_str().unwrap_or_default(), template)? {
        let plan = super::templates::plan(template, &args, &stage_folder)?;
        let env: Vec<(&str, String)> = if temp.is_empty() { Vec::new() } else { ["TEMP", "TMP", "TMPDIR"].map(|key| (key, temp.clone())).to_vec() };
        return Ok(super::templates::execute(&plan, &stage_folder, timeout, &env));
    }
    // #752: with the owner's build_broker a build runs in the member's own
    // snapshot of the shared folder, one at a time per build folder.
    if let Some(config) = super::broker::applies(team, template, folder) {
        let shared = member_folder_path(team, member)?;
        return super::broker::run(team, member, Path::new(&shared), template, &args, &config, timeout);
    }
    let plan = super::templates::plan(template, &args, folder)?;
    if template == "git-merge-lead" {
        let mine = member_folder_path(team, member)?;
        anyhow::ensure!(
            !mine.is_empty() && !same_path(&mine, &team.folder) && !inside(&mine, &team.folder),
            "This team works in one shared folder; Zerus does not merge the lead's branch into it"
        );
        let branch = lead_branch(team)?;
        return Ok(merge_lead(&mine, team.integration_folder(), &branch, timeout));
    }
    if let Some(payload) = &plan.commit {
        // B4/U21: the commit template is Zerus' own code, not a subprocess: the
        // identity is the one the outbox proved.
        let input = json!({"request_id":request["request_id"],"team":team.id,"attempt":request["attempt"],
            "message":payload["message"],"paths":payload["paths"]});
        return Ok(match super::commands::commit_in_team(team, &member.id, &input) {
            Ok(result) => super::templates::Outcome {
                exit: Some(0),
                tail: format!("{} {}", result["status"].as_str().unwrap_or_default(), result["sha"].as_str().unwrap_or_default()).trim().to_owned(),
                ..Default::default()
            },
            Err(error) => super::templates::Outcome { exit: Some(1), tail: format!("{error:#}"), ..Default::default() },
        });
    }
    Ok(super::templates::execute(&plan, folder, timeout, &[]))
}

/// U21/C1: records one run on its attempt - even when the attempt ended while
/// the command ran, so its exit code and tail are never lost - and answers the
/// member with them.
fn record_run(store: &mut Store, team: &Team, member: &Member, request: &Value, outcome: &super::templates::Outcome) -> Value {
    let template = request["template"].as_str().unwrap_or_default();
    let args = request["args"].clone();
    let attempt_id = request["attempt"].as_str().unwrap_or_default().to_owned();
    let role = team.role_of(&member.id);
    let recorded = change(store, &team.id, |team, events| {
        let record = RunRecord { template: template.to_owned(), args: args.clone(), exit: outcome.exit, at: now(), ms: outcome.ms, timed_out: outcome.timed_out };
        // U36: a stage's run is kept on its stage run.
        if let Some(task) = team.tasks.iter_mut().find(|task| task.pipeline_run.as_ref().is_some_and(|run| run.runs.iter().any(|run| run.id == attempt_id))) {
            let task_id = task.id;
            let run = task.pipeline_run.as_mut().and_then(|run| run.runs.iter_mut().find(|run| run.id == attempt_id)).context("unknown stage")?;
            run.runs.push(record);
            let excess = run.runs.len().saturating_sub(20);
            run.runs.drain(..excess);
            events.push(Event::new(
                "ran",
                json!({"member":role,"task":task_id,"stage":run.kind,"round":run.round,"template":template,"exit":outcome.exit,"timed_out":outcome.timed_out}),
            ));
            return Ok(());
        }
        let task = team
            .tasks
            .iter_mut()
            .find(|task| task.attempts.iter().any(|attempt| attempt.id == attempt_id))
            .context("unknown attempt")?;
        let task_id = task.id;
        let attempt = task.attempts.iter_mut().find(|attempt| attempt.id == attempt_id).context("unknown attempt")?;
        let n = attempt.n;
        attempt.runs.push(RunRecord {
            template: template.to_owned(),
            args,
            exit: outcome.exit,
            at: now(),
            ms: outcome.ms,
            timed_out: outcome.timed_out,
        });
        let excess = attempt.runs.len().saturating_sub(20);
        attempt.runs.drain(..excess);
        events.push(Event::new(
            "ran",
            json!({"member":role,"task":task_id,"attempt":n,"template":template,"exit":outcome.exit,"timed_out":outcome.timed_out}),
        ));
        Ok(())
    });
    if let Err(error) = recorded {
        eprintln!("hgs team worker: {}: ran: {error:#}", team.name);
    }
    json!({"status":"ran","template":template,"exit":outcome.exit,"tail":outcome.tail,
        "failures":outcome.failures,"excerpt":outcome.excerpt,"ms":outcome.ms,
        "timed_out":outcome.timed_out,"error":outcome.error,"report":outcome.report})
}

/// C1: claims a queued run and starts the detached runner. None means claimed
/// (the runner settles it); Some is a refusal to answer now.
fn start_run(
    store: &mut Store,
    team: &Team,
    member: &Member,
    folder: &Path,
    request: &Value,
    spawn: &dyn Fn(&Path, &Value) -> anyhow::Result<()>,
) -> anyhow::Result<Option<Value>> {
    let _ = store;
    let id = request["request_id"].as_str().unwrap_or_default().to_owned();
    let refuse = |error: String| Ok(Some(json!({"status":"refused","request_id":id,"error":error})));
    if !super::store::canonical_uuid(&id) {
        return refuse("An outbox request needs a canonical request_id".into());
    }
    // N4: an answer that is already there is the answer; it never runs twice.
    if super::outbox::result(folder, &id).is_some() {
        let _ = std::fs::remove_file(super::outbox::request_path(folder, &id));
        return Ok(None);
    }
    if super::outbox::running(folder, &id).is_some() {
        // A push of the same id while the runner works: keep the claim, drop the copy.
        let _ = std::fs::remove_file(super::outbox::request_path(folder, &id));
        return Ok(None);
    }
    // N11: one run per member folder at a time. A second claim while the first
    // runner works would run beside it in the same folder (a build and a test
    // on a half-linked binary); the request stays queued for a later tick.
    // #680: a command or land stage's own `team_stage` run in the folder counts
    // too, so a member's run never races the stage it is being checked by.
    // #538 N4: a stage's runs are serialized by the stage's folder as well - the
    // tester's claim sits in its own folder while the run goes in the
    // implementer's.
    let target_attempt = request["attempt"].as_str().unwrap_or_default();
    let target_template = request["template"].as_str().unwrap_or_default();
    let mut racing: Vec<PathBuf> = vec![folder.to_path_buf()];
    if let Ok(Some((stage_folder, _))) = super::stages::run_folder(team, &member.id, target_attempt, target_template) {
        let known = racing.iter().any(|known| same_path(&known.display().to_string(), &stage_folder.display().to_string()));
        if !known {
            racing.push(stage_folder);
        }
    }
    for racing_folder in &racing {
        if !super::outbox::running_all(racing_folder).is_empty()
            || super::super::builds::running_in(racing_folder, now()).iter().any(|claim| claim["source"] == "team_stage")
        {
            return Ok(None);
        }
    }
    if request["team"].as_str() != Some(team.id.as_str()) {
        return refuse("a request for another team".into());
    }
    if request["member"].as_str() != Some(member.id.as_str()) {
        return refuse(format!("the folder belongs to {}", team.name_of(&member.id)));
    }
    // Task #229: a Crit holds the release: no release candidate starts while one
    // is open in the team.
    if target_template == "release-candidate" {
        let crits = super::plan::open_crits_all(team);
        if !crits.is_empty() {
            return refuse(format!(
                "A critical task is open: {}; the release candidate starts once it is decided",
                crits.iter().map(|(phase, id)| format!("#{id} in phase {phase}")).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    let attempt = request["attempt"].as_str().unwrap_or_default();
    // U36: a test stage builds under its stage run, as an attempt.
    if !attempt_of(team, attempt, &member.id) {
        return refuse(format!("attempt {attempt} does not run for this member"));
    }
    let template = request["template"].as_str().unwrap_or_default();
    if let Err(error) = super::stages::run_folder(team, &member.id, attempt, template) {
        return refuse(format!("{error:#}"));
    }
    let plan = match super::templates::plan(template, &request["args"], folder) {
        Ok(plan) => plan,
        Err(error) => return refuse(format!("{error:#}")),
    };
    // U30: the label and command show the member's session as Building. The
    // claim names the member and attempt that asked: members that share one
    // folder share its claims too, and the run is the build of its asker only.
    let command = std::iter::once(plan.program.as_str()).chain(plan.argv.iter().map(String::as_str)).collect::<Vec<_>>().join(" ");
    let marker = json!({"request_id":id,"started":now(),"timeout_secs":super::templates::timeout_for(template),
        "label":template,"command":command,"member":member.id,"attempt":attempt});
    super::outbox::write_running(folder, &id, &marker)?;
    let _ = std::fs::remove_file(super::outbox::request_path(folder, &id));
    if let Err(error) = spawn(folder, request) {
        // Nobody took the claim: put the request back for the next tick.
        let _ = std::fs::remove_file(super::outbox::running_path(folder, &id));
        super::outbox::push(folder, request)?;
        return Err(error.context("the runner did not start"));
    }
    Ok(None)
}

/// C1: starts one detached runner for a claimed request; the worker never waits
/// for a template. The request goes to the runner on its standard input, never
/// through the member's folder, so the folder cannot change what runs.
fn spawn_runner(folder: &Path, request: &Value) -> anyhow::Result<()> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["team", "worker", "--run"])
        .current_dir(folder)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // N10: Zerus' own children are no session's own - the worker, its runner
    // and the hgs calls it makes must carry no agent marker at all.
    for name in ["TMUX", "HGS_HOST_PANE", "HGS_SESSION", "HGS_RUN_ID", "DSH_SESSION_ID"] {
        command.env_remove(name);
    }
    let mut child = crate::platform::spawn_detached(&mut command)?;
    if let Some(mut pipe) = child.stdin.take() {
        use std::io::Write;
        let _ = pipe.write_all(&serde_json::to_vec(request)?);
    }
    Ok(())
}

/// Task #239: the last lines of the team worker's log, for a run whose runner
/// stopped without a result — the crash is an outcome with its log, never
/// silence.
pub(crate) fn worker_log_tail() -> String {
    let Ok(bytes) = fs::read(worker_dir().join("worker.log")) else { return String::new() };
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(12)..].join("\n");
    if tail.chars().count() > 1500 {
        tail.chars().skip(tail.chars().count() - 1500).collect()
    } else {
        tail
    }
}

/// C1: a claim whose runner never settled (it crashed, the computer restarted)
/// is answered once its runner is gone or its own bound has long passed, so a
/// member never waits on a result that cannot come. The answer carries the
/// worker log's tail: a crash is an outcome, never silence.
fn settle_stale_runs(folder: &Path, team: &Team) {
    const GRACE_SECONDS: f64 = 120.0;
    for (_, marker) in super::outbox::running_all(folder) {
        let id = marker["request_id"].as_str().unwrap_or_default();
        let started = marker["started"].as_f64().unwrap_or(0.0);
        let bound = marker["timeout_secs"].as_u64().unwrap_or(1800) as f64;
        // Task #239: the runner records its own pid; a pid that is gone is a
        // crash now, not after the bound.
        let runner_gone = marker["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .is_some_and(|pid| crate::platform::process_identity(pid).is_none());
        if !id.is_empty() && (runner_gone || now() > started + bound + GRACE_SECONDS) {
            let log = worker_log_tail();
            let outcome = json!({"status":"refused","request_id":id,
                "error":format!("the run stopped without a result (its runner ended, this computer restarted, or the command outlived its bound); Zerus's worker log{}", if log.is_empty() { " is empty".to_owned() } else { format!(" ends with:\n{log}") })});
            let _ = super::outbox::settle_running(folder, id, &outcome);
            eprintln!("hgs team worker: {}: run {id} stopped without a result", team.name);
        }
    }
}

/// N10: the detached runner is the worker's own child. An agent marker (a dsh
/// conversation, an `HGS_SESSION` or an `HGS_RUN_ID`) or a caller record means
/// a session started this CLI itself, and the claim the worker writes before
/// the spawn must name this exact request. Without both, an unsandboxed member
/// could cd into another member's folder and pipe a request in, then commit as
/// that member and record a forged run on its attempt.
fn ensure_the_workers_run(folder: &Path, request_id: &str, caller: bool, markers: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        !caller && !markers,
        "hgs team worker --run starts only from the team worker, not from an agent session"
    );
    let claimed = super::outbox::running(folder, request_id)
        .is_some_and(|marker| marker["request_id"].as_str() == Some(request_id));
    anyhow::ensure!(claimed, "This run has no claim: the team worker starts it, never a session");
    Ok(())
}

/// C1: the detached runner for one claimed request, read from its standard
/// input: execute it, record it and leave the outcome where the member polls.
pub(crate) fn worker_run() -> Result<i32> {
    use std::io::Read;
    let folder = std::env::current_dir().map_err(|error| error.to_string())?;
    let caller = super::super::session_graph::caller_record().is_some();
    let markers = ["DSH_SESSION_ID", "HGS_SESSION", "HGS_RUN_ID"].iter().any(|name| nonempty_env(name).is_some());
    let mut bytes = Vec::new();
    std::io::stdin().take(super::outbox::REQUEST_BYTES + 1).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    let request: Value = match serde_json::from_slice(crate::state::without_bom(&bytes)) {
        Ok(value) => value,
        Err(error) => return Err(format!("The run request is not JSON: {error}")),
    };
    // N10: checked before the store is opened, so a piped request leaves nothing.
    ensure_the_workers_run(&folder, request["request_id"].as_str().unwrap_or_default(), caller, markers)
        .map_err(|error| format!("{error:#}"))?;
    let mut store = match Store::open(true) {
        Ok(store) => store,
        Err(error) => {
            if let Some(id) = request["request_id"].as_str() {
                let outcome = json!({"status":"refused","request_id":id,"error":format!("the team store was not opened: {error:#}")});
                let _ = super::outbox::settle_running(&folder, id, &outcome);
            }
            return Ok(1);
        }
    };
    // Task #239: the runner records itself in the claim, so a dead runner is
    // recognized as one by the worker's own poll, and a panic below is still an
    // outcome with the worker log's tail.
    let id = request["request_id"].as_str().unwrap_or_default().to_owned();
    if super::outbox::result(&folder, &id).is_none() {
        if let Some(mut marker) = super::outbox::running(&folder, &id) {
            marker["pid"] = json!(std::process::id());
            let _ = super::outbox::write_running(&folder, &id, &marker);
        }
    }
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_request(&mut store, &folder, &request)));
    if ran.is_err() {
        let log = worker_log_tail();
        let outcome = json!({"status":"refused","request_id":id,
            "error":format!("the run stopped without a result: its worker panicked. Zerus's worker log{}", if log.is_empty() { " is empty".to_owned() } else { format!(" ends with:\n{log}") })});
        let _ = super::outbox::settle_running(&folder, &id, &outcome);
    }
    Ok(0)
}
/// C1: one claimed request from the runner's folder. Its outcome, good or bad,
/// is left in the outbox; the runner never holds the worker or another team.
fn run_request(store: &mut Store, folder: &Path, request: &Value) {
    let id = request["request_id"].as_str().unwrap_or_default().to_owned();
    let outcome = (|| -> anyhow::Result<Value> {
        anyhow::ensure!(super::store::canonical_uuid(&id), "An outbox request needs a canonical request_id");
        let team = store.load(request["team"].as_str().unwrap_or_default())?.context("Unknown team")?;
        anyhow::ensure!(matches!(team.status, TeamStatus::Running | TeamStatus::Paused), "The team is not running");
        let member = team.member(request["member"].as_str().unwrap_or_default()).context("Unknown member")?.clone();
        // The folder still proves the member: it is the one Zerus gives it.
        let mine = member_folder_path(&team, &member)?;
        let here = folder.display().to_string();
        anyhow::ensure!(
            !mine.is_empty() && (same_path(&here, &mine) || inside(&here, &mine)),
            "This folder is not {}'s own folder",
            team.name_of(&member.id)
        );
        let attempt = request["attempt"].as_str().unwrap_or_default();
        // C1: the attempt may have ended while the command ran; the result is
        // still its own, so the claim is not refused for that. U36/#538: a
        // stage run given to the member is its attempt too.
        anyhow::ensure!(attempt_of(&team, attempt, &member.id), "attempt {attempt} is not this member's");
        let outcome = run_outcome(&team, &member, folder, request)?;
        // Task 261: the run restarted the shared sccache server: the lead hears.
        if outcome.tail.contains(super::super::build_cache_server::RESTART_NOTE) {
            let task = team.tasks.iter().find(|task| task.attempts.iter().any(|found| found.id == attempt)).map_or(0, |task| task.id);
            tell_lead(store, &team, "build_cache", &cache_restarted_text(&team, &member.id, task, request["template"].as_str().unwrap_or_default(), outcome.exit));
        }
        Ok(record_run(store, &team, &member, request, &outcome))
    })();
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => json!({"status":"refused","request_id":id,"error":format!("{error:#}")}),
    };
    if let Err(error) = super::outbox::settle_running(folder, &id, &outcome) {
        eprintln!("hgs team worker: run {id}: {error}");
    }
}

/// Task #275: a sandboxed member's queued call answers in a file it never
/// reads. A refused or failed outcome is also a team message at its next safe
/// point (urgent for a question or a report), and the lead's inbox names the
/// reason. The result file stays as before, and a `messaged` outcome (task
/// 209's context claim, told already) is not told twice.
fn announce_outbox(store: &mut Store, team: &Team, member: &Member, folder: &Path, request: &Value, outcome: &Value) {
    let status = outcome["status"].as_str().unwrap_or_default();
    if !matches!(status, "refused" | "failed" | "error") || outcome["messaged"] == true {
        return;
    }
    let error = outcome["error"].as_str().unwrap_or("the worker could not apply it");
    let op = request["op"].as_str().unwrap_or("call");
    let request_id = request["request_id"].as_str().unwrap_or_default();
    let urgent = matches!(op, "ask" | "report");
    let mut text = format!(
        "[Zerus team \u{b7} from: Zerus \u{b7} outbox]\nYour queued {op} (request {request_id}) was {}; it was NOT applied.\n{error}\n",
        if status == "failed" { "failed" } else { "refused" },
    );
    // Task #275: a "too long" refusal names the limit and how to send it.
    if error.to_lowercase().contains("too long") {
        let limit = match op {
            "ask" => "4000 characters",
            "report" => "64 KiB",
            _ => "the limit named above",
        };
        text += &format!(
            "The limit is {limit}; put the long text in a file and name its path in the text (for example \"see D:/work/question.md\").\n"
        );
    }
    text += &format!(
        "Fix it and send it again. Its result stays at {}.",
        super::outbox::result_path(folder, request_id).display()
    );
    if let Some(session) = member.session.as_ref() {
        let message = uuid::Uuid::new_v4().to_string();
        let _ = super::doorbell::post_and_ring(
            store,
            &team.id,
            &member.id,
            session,
            &message,
            "outbox",
            "Zerus",
            if urgent { Urgency::Urgent } else { Urgency::Normal },
            &text,
        );
    }
    let record = json!({"member":member.id,"op":op,"request_id":request_id,"error":error,"urgent":urgent,"at":now()});
    let team_id = team.id.clone();
    let _ = store.mutate(None, &team_id, None, |team, events| {
        team.outbox_failures.push(record.clone());
        if team.outbox_failures.len() > 20 {
            let extra = team.outbox_failures.len() - 20;
            team.outbox_failures.drain(..extra);
        }
        events.push(Event::new("outbox_refused", record.clone()));
        Ok(Value::Null)
    });
}

/// One queued request, applied as the member's own write (U20). The member's
/// input keys the store receipt, so a request applied twice (a worker that died
/// before it removed the file) is a replay, not a second report. U42: a budget
/// (`expect`) has no attempt; the folder itself proves the member.
fn apply_outbox(store: &mut Store, team: &Team, member: &Member, request: &Value) -> anyhow::Result<Value> {
    let op = request["op"].as_str().unwrap_or_default();
    anyhow::ensure!(
        matches!(op, "report" | "ask" | "commit" | "reply" | "expect" | "messages-read" | "team"),
        "Unknown outbox request {op}"
    );
    anyhow::ensure!(request["team"] == team.id.as_str(), "a request for another team");
    anyhow::ensure!(request["member"] == member.id.as_str(), "the folder belongs to {}", team.name_of(&member.id));
    // The member's own request, unchanged: a replay keys the same store receipt.
    let input = request["input"].clone();
    let request_id = request["request_id"].as_str().unwrap_or_default().to_owned();
    // Task #346: a sandboxed member's own-scope team call (a scratch team's
    // create, or an operation for the scratch team it leads) is applied with the
    // member's identity and checked again against its profile and the rule.
    if op == "team" {
        return super::commands::apply_team_outbox(store, team, member, request);
    }
    // A request applied before the worker removed its file replays its receipt,
    // even after the attempt closed (the store's own idempotence, U20).
    if let Some(receipt) = store.receipt(&request_id, &input)? {
        return Ok(receipt);
    }
    // Task 134: a sandboxed member's read receipt, recorded as its own
    // `hgs team inbox` would have: its own messages only.
    if op == "messages-read" {
        let ids: Vec<String> = input["ids"].as_array().into_iter().flatten().filter_map(|id| id.as_str().map(str::to_owned)).take(200).collect();
        let via = if input["via"] == "current" { "current" } else { "inbox" };
        let member_id = member.id.clone();
        // Task 287: the rows and the receipt only, never the team record.
        let marked = store.write_rows(&team.id, |tx, events| super::commands::read_messages(tx, team, &member_id, &ids, via, &request_id, events))?;
        return Ok(json!({"status":"read","read":marked,"request_id":request_id}));
    }
    if op != "expect" {
        let attempt_id = request["attempt"].as_str().unwrap_or_default();
        anyhow::ensure!(attempt_id == input["attempt"].as_str().unwrap_or_default(), "the request names another attempt");
        let task = team
            .tasks
            .iter()
            .find(|task| task.open_attempt().is_some_and(|attempt| attempt.id == attempt_id && attempt.member == member.id))
            .map(|task| task.id)
            // U42: a queued report also reaches an attempt whose report already
            // landed (the placeholder repair), while the task is undecided.
            .or_else(|| {
                if op != "report" {
                    return None;
                }
                team.tasks
                    .iter()
                    .find(|task| {
                        task.status.reported()
                            && task.attempts.iter().any(|attempt| {
                                attempt.id == attempt_id && attempt.member == member.id && attempt.state == AttemptState::Reported
                            })
                    })
                    .map(|task| task.id)
            })
            // U36: a stage member in a sandbox queues its verdict the same way.
            .or_else(|| super::pipeline::open_run_task(team, attempt_id, &member.id));
        let Some(task) = task else { anyhow::bail!("attempt {attempt_id} does not run for this member") };
        if input["task"].as_u64().is_some() {
            anyhow::ensure!(input["task"] == json!(task), "the request names another task");
        }
    }
    if op == "commit" {
        return super::commands::commit_in_team(team, &member.id, &input);
    }
    // Task 209: a report that claims there is no context left while the meter
    // says otherwise is not a report. A sandboxed member never saw the
    // command's own refusal, so the answer goes to it as a message.
    if op == "report" && input["interim"].as_bool() != Some(true) {
        if let Some(answer) = report_context_answer(store, &team.id, &input)? {
            if let Some(session) = member.session.clone() {
                let text = format!("[Zerus team \u{b7} from: Zerus \u{b7} context]\n{answer}");
                let _ = super::doorbell::post_and_ring(
                    store, &team.id, &member.id, &session, &uuid::Uuid::new_v4().to_string(),
                    "context", "Zerus", Urgency::Normal, &text,
                );
            }
            return Ok(json!({"status":"refused","request_id":request_id,"task":input["task"].clone(),"error":answer,"messaged":true}));
        }
    }
    let member_id = member.id.clone();
    let actor = ops::Actor::Member(member_id.clone());
    // Task 287: the report's git reads run before the store's lock.
    let value = with_git_answers(
        || ops::prefetch_git(team, op, &input, &actor),
        || store.mutate(Some((&request_id, &input)), &team.id, None, |team, events| ops::apply(team, op, &input, &actor, &BTreeSet::new(), events)),
    )?;
    // 9.6: a queued reply is the same answer the direct path routes down to the
    // sub-team's question; the answer is already recorded, so a routing error
    // is logged and never refused as if nothing had been applied.
    if op == "reply" {
        if let Err(error) = super::commands::route_reply_down(store, &team.id, &input) {
            eprintln!("hgs team worker: {}: reply: {error:#}", team.name);
        }
    }
    Ok(value)
}

/// The worker's own files: a pid, a heartbeat, a stop request and a log (U2).
struct WorkerFiles {
    dir: PathBuf,
}

/// Why the worker ended, for its log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkerExit {
    StopRequested,
    Idle,
    Updated,
}

impl WorkerExit {
    fn reason(self) -> &'static str {
        match self {
            WorkerExit::StopRequested => "stop requested",
            WorkerExit::Idle => "no active team",
            WorkerExit::Updated => "hgs was updated",
        }
    }
}

/// One pass: a stop request ends the worker before the tick (U2).
#[derive(Debug)]
enum WorkerPass {
    Stop,
    Ticked(anyhow::Result<bool>),
}

impl WorkerFiles {
    fn new(dir: PathBuf) -> Self {
        Self { dir }
    }
    fn pid_file(&self) -> PathBuf {
        self.dir.join("worker.pid")
    }
    fn heartbeat_file(&self) -> PathBuf {
        self.dir.join("worker.heartbeat")
    }
    fn stop_file(&self) -> PathBuf {
        self.dir.join("worker.stop")
    }
    fn log_file(&self) -> PathBuf {
        self.dir.join("worker.log")
    }

    /// Records this process as the worker with its first heartbeat.
    fn start(&self) {
        let pid = std::process::id();
        let _ = fs::write(self.pid_file(), pid.to_string());
        self.beat(0, "starting");
        self.log(&format!("start pid={pid}"));
    }

    fn pid(&self) -> Option<u32> {
        fs::read_to_string(self.pid_file()).ok()?.trim().parse().ok()
    }

    /// A stop request that names this process; a request for another pid is
    /// stale and ignored (U2).
    fn stop_requested(&self) -> bool {
        let Ok(bytes) = fs::read(self.stop_file()) else { return false };
        serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| value["pid"].as_u64())
            .is_some_and(|pid| pid == u64::from(std::process::id()))
    }

    fn request_stop(&self, pid: u32) {
        let _ = fs::write(self.stop_file(), json!({"pid":pid,"at":now()}).to_string());
    }

    /// A clean stop: no pid or heartbeat is left behind, and the reason stays
    /// in the bounded log (U2).
    fn finish(&self, reason: &str) {
        let _ = fs::remove_file(self.pid_file());
        let _ = fs::remove_file(self.heartbeat_file());
        let _ = fs::remove_file(self.stop_file());
        self.log(&format!("stop: {reason}"));
    }

    /// The files a dead worker leaves: removed when nothing holds its lock.
    fn cleanup(&self) {
        let _ = fs::remove_file(self.pid_file());
        let _ = fs::remove_file(self.heartbeat_file());
        let _ = fs::remove_file(self.stop_file());
    }

    fn beat(&self, teams: usize, reason: &str) {
        let beat = json!({"pid":std::process::id(),"at":now(),"teams":teams,"reason":reason});
        let _ = fs::write(self.heartbeat_file(), beat.to_string());
    }

    /// Appends one line, starting the log over once it is too large so it never
    /// grows without a bound (U2).
    fn log(&self, line: &str) {
        if fs::metadata(self.log_file()).map(|meta| meta.len() > LOG_BYTES).unwrap_or(false) {
            let _ = fs::remove_file(self.log_file());
        }
        let text = format!("{:.0} {line}\n", now());
        let _ = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_file())
            .and_then(|mut file| std::io::Write::write_all(&mut file, text.as_bytes()));
    }
}

/// Runs the worker's session listing unless one may still run: a tick is
/// skipped rather than overlap two `hgs ls` children (U2).
/// Task #3 (team ede724c4): how old a session listing the tick may use. A
/// listing takes 1.4 to 3 s; the tick takes the newest one up to this old and
/// starts the next on a thread of its own, so hand-outs never wait for `hgs
/// ls`. Without a fresh one the tick lists itself, as before.
#[cfg_attr(test, allow(dead_code))]
const LISTING_MAX_AGE: Duration = Duration::from_secs(5);

/// Task #3: the newest listing the worker read, and when it finished.
#[cfg_attr(test, allow(dead_code))]
static LAST_LISTING: std::sync::Mutex<Option<(Instant, Output)>> = std::sync::Mutex::new(None);

#[cfg_attr(test, allow(dead_code))]
fn run_listing() -> anyhow::Result<Output> {
    let output = hgs("", &["ls", "--json", "--no-archives"], None, LIST_TIMEOUT)?;
    if output.status.success() {
        *LAST_LISTING.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((Instant::now(), output.clone()));
    }
    Ok(output)
}

/// Task #3: the session listing of this tick: a fresh cached one (and the
/// next started in the background), else one read now. None while a listing
/// still runs and none is fresh: the tick is skipped, never two `hgs ls`
/// children at once (U2). A test lists on its own thread, through its fake.
fn tick_listing() -> Option<anyhow::Result<Output>> {
    #[cfg(test)]
    {
        listing(|| hgs("", &["ls", "--json", "--no-archives"], None, LIST_TIMEOUT))
    }
    #[cfg(not(test))]
    {
        let cached = LAST_LISTING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|(at, _)| at.elapsed() <= LISTING_MAX_AGE)
            .map(|(_, output)| output.clone());
        match cached {
            Some(output) => {
                // The next listing runs while this tick works; it is skipped
                // when one still runs.
                let _ = std::thread::Builder::new().name("hgs-team-listing".into()).spawn(|| {
                    if let Some(Err(error)) = listing(run_listing) {
                        eprintln!("hgs team worker: listing: {error}");
                    }
                });
                Some(Ok(output))
            }
            None => listing(run_listing),
        }
    }
}

fn listing<T>(run: impl FnOnce() -> T) -> Option<T> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static RUNNING: AtomicBool = AtomicBool::new(false);
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            RUNNING.store(false, Ordering::SeqCst);
        }
    }
    if RUNNING.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return None;
    }
    let clear = Clear;
    let out = run();
    drop(clear);
    Some(out)
}

/// The one worker's lock, or None while another worker holds it (U2).
fn worker_lock(path: &Path) -> anyhow::Result<Option<crate::platform::FileLock>> {
    let file = lock_file(path).map_err(anyhow::Error::msg)?;
    if fs2::FileExt::try_lock_exclusive(&file).is_err() {
        return Ok(None);
    }
    Ok(Some(crate::platform::FileLock::held(file)))
}

/// One worker pass: a stop request ends it before the tick (U2).
fn worker_pass(files: &WorkerFiles, tick: impl FnOnce() -> anyhow::Result<bool>) -> WorkerPass {
    if files.stop_requested() {
        return WorkerPass::Stop;
    }
    WorkerPass::Ticked(tick())
}

/// Task #406: how long a stop request waits for the tick it arrived in.
const STOP_GRACE: Duration = Duration::from_secs(4);

/// Task #406 (owner, 2026-11-09): `hgs team worker --stop` kept waiting 30+
/// minutes behind a tick until a taskkill. A watchdog beside the ticks ends the
/// worker once a stop request has waited `STOP_GRACE` for the tick: whatever
/// the tick was sending stays unconfirmed, and the next start recovers it
/// (`recover`), so nothing is sent twice.
fn stop_watchdog(dir: PathBuf, ended: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    std::thread::spawn(move || {
        let files = WorkerFiles::new(dir);
        let mut requested: Option<Instant> = None;
        loop {
            if ended.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            requested = if files.stop_requested() { requested.or_else(|| Some(Instant::now())) } else { None };
            if stop_overdue(requested, Instant::now()) {
                let step = super::tick_clock::status()["running"]["step"].as_str().unwrap_or("a tick").to_owned();
                files.log(&format!("stop: the tick ({step}) did not end within {} s; ending in it", STOP_GRACE.as_secs()));
                files.finish("stop requested");
                std::process::exit(0);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });
}

/// Task #406: a stop request that has waited the grace for the tick.
fn stop_overdue(requested: Option<Instant>, now: Instant) -> bool {
    requested.is_some_and(|at| now.duration_since(at) >= STOP_GRACE)
}

/// Sleeps a tick in small slices, so a stop request ends the worker promptly.
fn sleep_or_stop(files: &WorkerFiles, duration: Duration) {
    let started = Instant::now();
    while started.elapsed() < duration {
        if files.stop_requested() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// What keeps the worker alive, for its heartbeat and log (U2).
fn worker_reason(store: &Store) -> (usize, String) {
    let active = store.active_teams(200).unwrap_or_default();
    let running = active.iter().filter(|team| team.status == TeamStatus::Running).count();
    let paused = active.iter().filter(|team| team.status == TeamStatus::Paused).count();
    let planning = active.len().saturating_sub(running + paused);
    (active.len(), format!("{running} running, {paused} paused, {planning} planning"))
}

pub(crate) fn worker() -> Result<i32> {
    private_dir(&worker_dir())?;
    let files = WorkerFiles::new(worker_dir());
    let Some(lock) = worker_lock(&lock_path()).map_err(|error| error.to_string())? else {
        return Ok(0);
    };
    files.start();
    // Task #406: a stop request ends the worker within 5 s, even in a tick.
    let ended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    stop_watchdog(worker_dir(), ended.clone());
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let started = fs::metadata(&executable).and_then(|m| m.modified()).ok();
    let config = crate::config::Config::load().map_err(|e| e.to_string())?;
    let host = config.host();
    let mut store = Store::open(true).map_err(|e| e.to_string())?;
    if let Err(error) = recover(&mut store) {
        files.log(&format!("recover failed: {error}"));
    }
    // Task #227: at a worker's start (an update restarted the sessions), one
    // sweep gives the attempts a crash left unconfirmed back to their members.
    if let Err(error) = sweep_crashed(&mut store) {
        files.log(&format!("recovery sweep failed: {error}"));
    }
    let mut last_work = Instant::now();
    let mut last_reason = String::new();
    // Task 128: the lore writer runs beside the ticks, never inside one.
    super::lore_sync::start_background(|line| eprintln!("hgs team worker: {line}"));
    // Task 160: the park watcher, beside the ticks too.
    super::park::start_background(|line| eprintln!("hgs team worker: {line}"));
    let exit = loop {
        if fs::metadata(&executable).and_then(|m| m.modified()).ok() != started {
            // An update replaced hgs: the new one carries on with the teams.
            break WorkerExit::Updated;
        }
        let result = match worker_pass(&files, || tick(&mut store, host)) {
            WorkerPass::Stop => break WorkerExit::StopRequested,
            WorkerPass::Ticked(result) => result,
        };
        match result {
            Ok(true) => {
                last_work = Instant::now();
                let (teams, reason) = worker_reason(&store);
                files.beat(teams, &reason);
                if reason != last_reason {
                    files.log(&format!("staying: {reason}"));
                    last_reason = reason;
                }
            }
            Ok(false) => {
                files.beat(0, "no active team");
                if last_work.elapsed() >= IDLE_EXIT {
                    break WorkerExit::Idle;
                }
            }
            Err(error) => files.log(&format!("error: {error}")),
        }
        sleep_or_stop(&files, TICK);
    };
    ended.store(true, std::sync::atomic::Ordering::SeqCst);
    files.finish(exit.reason());
    drop(lock);
    if exit == WorkerExit::Updated {
        if let Err(error) = ensure_worker() {
            files.log(&format!("the updated dispatcher did not start: {error}"));
        }
    }
    Ok(0)
}

/// `hgs team worker --stop`: asks the running worker to end and waits briefly
/// (U2). Agents and the session host are never touched.
pub(crate) fn stop_worker() -> Result<i32> {
    let files = WorkerFiles::new(worker_dir());
    if !crate::status::locked(&lock_path()) {
        files.cleanup();
        println!("hgs: no team worker runs");
        return Ok(0);
    }
    let Some(pid) = files.pid() else {
        println!("hgs: the team worker runs an older build and does not take stop requests");
        return Ok(0);
    };
    files.request_stop(pid);
    let deadline = Instant::now() + Duration::from_secs(20);
    while crate::status::locked(&lock_path()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    if crate::status::locked(&lock_path()) {
        println!("hgs: the team worker was asked to stop; it ends after its current listing");
    } else {
        println!("hgs: the team worker stopped");
    }
    Ok(0)
}

/// Starts a detached worker unless one runs. It exits by itself once no team
/// runs.
pub(crate) fn ensure_worker() -> anyhow::Result<()> {
    // A command test runs the real entry point; its executable is the test
    // binary, which must not start as a worker.
    #[cfg(test)]
    return Ok(());
    #[cfg(not(test))]
    ensure_worker_now()
}

#[cfg(not(test))]
fn ensure_worker_now() -> anyhow::Result<()> {
    if crate::status::locked(&lock_path()) {
        return Ok(());
    }
    let mut command = Command::new(std::env::current_exe()?);
    command.args(["team", "worker"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // The worker outlives its caller: started from the caller's folder it would
    // hold that folder (on Windows a fork update could not move the
    // installation), so it runs from the home folder.
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).filter(|home| !home.is_empty()) {
        command.current_dir(home);
    }
    // N10: Zerus' own children are no session's own - the worker, its runner
    // and the hgs calls it makes must carry no agent marker at all.
    for name in ["TMUX", "HGS_HOST_PANE", "HGS_SESSION", "HGS_RUN_ID", "DSH_SESSION_ID"] {
        command.env_remove(name);
    }
    // Task #23 (team ede724c4): nor the agent's own Git configuration. An agent
    // session that started the worker passed `GIT_CONFIG_PARAMETERS`
    // ('safe.bareRepository=explicit' 'core.fsmonitor=') to every build, test
    // and merge-queue suite the worker ran, and the test runner's bare mirror
    // was refused in each of them.
    for (name, _) in std::env::vars_os() {
        let text = name.to_string_lossy();
        if text == "GIT_CONFIG_PARAMETERS" || text == "GIT_CONFIG_COUNT" || text.starts_with("GIT_CONFIG_KEY_") || text.starts_with("GIT_CONFIG_VALUE_")
            || text == "CLAUDECODE" || text.starts_with("CLAUDE_CODE_")
        {
            command.env_remove(&name);
        }
    }
    crate::platform::spawn_detached(&mut command)?;
    Ok(())
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod models_tests;

#[cfg(test)]
#[path = "handoff_tests.rs"]
mod handoff_tests;


#[cfg(test)]
#[path = "tests/support_dispatcher.rs"]
mod support;

#[cfg(test)]
#[path = "tests/dispatcher_stages.rs"]
mod dispatcher_stages;

#[cfg(test)]
#[path = "tests/dispatcher_restart.rs"]
mod dispatcher_restart;

#[cfg(test)]
#[path = "tests/dispatcher_git_cache.rs"]
mod dispatcher_git_cache;

#[cfg(test)]
#[path = "tests/dispatcher_base.rs"]
mod dispatcher_base;

#[cfg(test)]
#[path = "tests/dispatcher_approval.rs"]
mod dispatcher_approval;

#[cfg(test)]
#[path = "tests/dispatcher_crash.rs"]
mod dispatcher_crash;

#[cfg(test)]
#[path = "tests/dispatcher_pause_cold.rs"]
mod dispatcher_pause_cold;
#[cfg(test)]
#[path = "tests/dispatcher_idle_handout.rs"]
mod dispatcher_idle_handout;
#[cfg(test)]
#[path = "tests/dispatcher_store_locks.rs"]
mod dispatcher_store_locks;
#[cfg(test)]
#[path = "tests/dispatcher_jobs.rs"]
mod dispatcher_jobs;

#[cfg(test)]
#[path = "tests/dispatcher_host_start.rs"]
mod dispatcher_host_start;

#[cfg(test)]
#[path = "tests/dispatcher_delivery.rs"]
mod dispatcher_delivery;

#[cfg(test)]
#[path = "tests/dispatcher_capture.rs"]
mod dispatcher_capture;

#[cfg(test)]
#[path = "tests/dispatcher_inbox.rs"]
mod dispatcher_inbox;

#[cfg(test)]
#[path = "tests/dispatcher_drain.rs"]
mod dispatcher_drain;

#[cfg(test)]
#[path = "tests/dispatcher_context.rs"]
mod dispatcher_context;

#[cfg(test)]
#[path = "tests/dispatcher_launch.rs"]
mod dispatcher_launch;

#[cfg(test)]
#[path = "tests/dispatcher_live_edit.rs"]
mod dispatcher_live_edit;

#[cfg(test)]
#[path = "tests/dispatcher_lead.rs"]
mod dispatcher_lead;

#[cfg(test)]
#[path = "tests/dispatcher_tick_budget.rs"]
mod dispatcher_tick_budget;

#[cfg(test)]
#[path = "tests/dispatcher_outbox.rs"]
mod dispatcher_outbox;

#[cfg(test)]
#[path = "tests/dispatcher_merge.rs"]
mod dispatcher_merge;

#[cfg(test)]
#[path = "tests/dispatcher_reports.rs"]
mod dispatcher_reports;

#[cfg(test)]
#[path = "tests/dispatcher_drafts.rs"]
mod dispatcher_drafts;

#[cfg(test)]
#[path = "tests/dispatcher_stall.rs"]
mod dispatcher_stall;
#[cfg(test)]
#[path = "tests/dispatcher_lead_stall.rs"]
mod dispatcher_lead_stall;
#[cfg(test)]
#[path = "tests/dispatcher_owner_queue.rs"]
mod dispatcher_owner_queue;
#[cfg(test)]
#[path = "tests/dispatcher_input_block.rs"]
mod dispatcher_input_block;
#[cfg(test)]
#[path = "tests/dispatcher_terminal_prompt.rs"]
mod dispatcher_terminal_prompt;
#[cfg(test)]
#[path = "tests/dispatcher_look.rs"]
mod dispatcher_look;
#[cfg(test)]
#[path = "tests/dispatcher_rights.rs"]
mod dispatcher_rights;
#[cfg(test)]
#[path = "tests/dispatcher_pulses.rs"]
mod dispatcher_pulses;
#[cfg(test)]
#[path = "tests/dispatcher_shadow.rs"]
mod dispatcher_shadow;
#[cfg(test)]
#[path = "tests/dispatcher_park.rs"]
mod dispatcher_park;
#[cfg(test)]
#[path = "tests/dispatcher_wait_loops.rs"]
mod dispatcher_wait_loops;
#[cfg(test)]
#[path = "tests/dispatcher_decisions.rs"]
mod dispatcher_decisions;

#[cfg(test)]
#[path = "tests/dispatcher_areas.rs"]
mod tests;

