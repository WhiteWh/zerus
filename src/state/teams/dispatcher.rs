//! WhiteWh fork: the team dispatcher (`hgs team worker`). Each tick it reads
//! every member session once, starts members that have work, hands ready tasks
//! to free members and forwards reports to the lead. A hand-off is recorded
//! before it is sent and never sent again once it may have arrived.
use super::super::*;
use super::inbox;
use super::model::*;
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
/// How long a member may sit idle without ready work before the lead hears
/// once that the plan starves it (C7).
const STARVED_AFTER: f64 = 20.0 * 60.0;
/// The worker's session listing is bounded, so a stop request is never far away.
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
/// The worker's log keeps at most this much before it starts over.
const LOG_BYTES: u64 = 64 * 1024;

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
    /// The MCP servers and services it runs, which are not Background work
    /// (G7).
    pub services: Vec<SeenService>,
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
}

/// The newest Background job of a session (C12).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SeenWork {
    pub command: String,
    pub started_at: f64,
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

/// Every session of this computer and its peers from one listing.
pub(crate) fn seen(listing: &Value) -> Vec<Seen> {
    let mut sessions = Vec::new();
    for (index, machine) in listing.as_array().into_iter().flatten().enumerate() {
        let host = if index == 0 { String::new() } else { machine["host"].as_str().unwrap_or_default().to_owned() };
        for record in machine["sessions"].as_array().into_iter().flatten() {
            let field = |key: &str| record[key].as_str().unwrap_or_default();
            // Terminal sessions report their process, DeepSeek ones a state.
            let live = !matches!(field("state"), "paused" | "stopped" | "archived")
                && (field("state") == "running" || field("process_state") == "running" || field("runtime_state") == "live");
            let activity = field("activity").to_owned();
            let idle = live && activity == "idle";
            // An idle agent whose own jobs run in the background (Background:
            // a watcher, an MCP server, a long check) still takes messages.
            let free = idle
                && matches!(field("phase"), "" | "idle" | "ready")
                && record["optional_questions"]["count"].as_u64().unwrap_or(0) == 0
                && record["auth_required"] != true
                && record["send_available"] != false;
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
                services,
                asking: live && matches!(field("phase"), "question" | "input" | "approval"),
                last_event_at: record["last_event_at"].as_f64(),
                tool: field("current_tool").to_owned(),
                tool_started: record["tool_started"].as_f64().filter(|at| *at > 0.0),
                agent: field("agent").to_owned(),
                live,
                state: field("state").to_owned(),
                cwd: field("cwd").to_owned(),
                muted: record["notification_preferences"]["muted"] == true,
                notify_revision: record["notification_preferences"]["revision"].as_u64(),
                limited,
                model: field("model").to_owned(),
                effort: field("effort").to_owned(),
                account_id: field("account_id").to_owned(),
            });
        }
    }
    sessions
}

/// One thing the dispatcher does for a team in a tick.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    /// The member's started session appeared, or its identity changed.
    Adopt { member: String, session: SessionRef },
    Launch { member: String },
    HandOut { task: u32, member: String },
    /// U45: switch the member's model and effort before it gets the task.
    SwitchModel { task: u32, member: String, from: String, model: String, effort: String },
    /// U45: the switch was not confirmed in time; the task waits for the lead.
    ModelUncertain { task: u32 },
    /// U45: no member can run the task's models; it waits with the reason.
    TaskWaits { task: u32, reason: String },
    /// U46: a hand-out waits for its context action's confirmation: one look
    /// per tick, then the hand-out itself or the waiting reason.
    PrepareContext { task: u32, member: String },
    /// U31: an idle member takes an independent task of the next draft phase.
    WorkAhead { task: u32, member: String },
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
    /// The member got work again or nothing is open: the episode ends (C7).
    StarvedEnded { member: String },
    /// Idle without work for too long: tell the free lead once (C7).
    PlanStarved { member: String },
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
    /// H1: the member's session was stopped by a usage limit, or recovered
    /// from it (None).
    Limited { member: String, limited: Option<MemberLimit> },
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
    /// The capture gate was seen open: the report timeout counts from here (C2).
    GateOpen { task: u32 },
    /// A question or Background work holds a finished turn: the timeout
    /// restarts and a long block reaches the lead (C2, N5).
    GateClosed { task: u32 },
    /// Tells the free lead once that a finished turn has been blocked too long (N5).
    BlockedLate { task: u32 },
    /// The member started working again: its recorded turn end is cleared (A2).
    WorkResumed { task: u32 },
    /// The dispatcher saw a member's session live or stopped; status derives
    /// `stopped` from it without a listing of its own (L3).
    Liveness { member: String, live: bool },
    /// U56: a pending task pinned to a stuck, stopped or offline member says so
    /// in its waits.
    WaitOnMember { task: u32, member: String, reason: String },
    /// U56: the member is available again; the wait goes.
    ReleaseWait { task: u32 },
    /// U56/#728: the lead or the owner restarted a stuck member: interrupt its
    /// session and resume it in its conversation.
    RestartStuck { member: String },
    /// U61: an auto-accepted draft whose phases ahead are done and whose plan has
    /// no blocker is accepted by Auto; the ready path hands its tasks out next.
    AutoAcceptDraft { phase: u32 },
    /// U56: the member's phase and activity as the dispatcher saw them, so
    /// status shows them without an inspect.
    Watch { member: String, phase: String, activity: String, last_event_at: Option<f64> },
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
}

/// U18: the lead's turn has ended and it can take a message, also while its own
/// subagents or Background jobs run (an idle agent with Background work accepts
/// new messages); never during a turn, a question, an approval or a sign-in.
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

fn session_of<'a>(member: &Member, seen: &'a [Seen]) -> Option<&'a Seen> {
    let session = member.session.as_ref()?;
    seen.iter().find(|s| s.host == session.host && s.name == session.name)
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
            // A2/C2: the capture gate. A turn end is recorded only for a quiet
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
                            && attempt.started_at.is_some()
                            && now - delivered >= team.settings.report_timeout_seconds as f64
                        {
                            // A2: the session is quiet but the listing carries no
                            // reply mark for the turn (its replies were never read),
                            // so no turn end could be recorded. After the timeout the
                            // attempt is decided all the same: No report captures the
                            // quiet session's last reply instead of leaving the task
                            // in progress for hours (U20, #343 attempt 2).
                            steps.push(Step::NoReport { task: task.id });
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
                        // L6: the session is gone; the old fallback captures what
                        // it can, else marks No report.
                        if !attempt.no_report && now - ready >= team.settings.report_timeout_seconds as f64 {
                            steps.push(Step::NoReport { task: task.id });
                        }
                    } else if quiet {
                        if attempt.report_ready_at.is_none() {
                            // The gate was already open when this record was
                            // written; the timeout starts now (C2).
                            steps.push(Step::GateOpen { task: task.id });
                        } else if !attempt.no_report && now - ready >= team.settings.report_timeout_seconds as f64 {
                            steps.push(Step::NoReport { task: task.id });
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
                    // away: its last reply is captured from the session's record,
                    // or it shows No report (U20). N7: only while its computer is
                    // listed - an absent listing is not a stopped session.
                    if !attempt.no_report
                        && attempt.started_at.is_some()
                        && now - delivered >= team.settings.report_timeout_seconds as f64
                    {
                        steps.push(Step::NoReport { task: task.id });
                    }
                }
            }
        }
    }
    // Each tick lists the sessions: a member's liveness is recorded when it
    // changes, so status derives `stopped` without a listing of its own (L3).
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
        // U56: the member's phase and activity feed status; a busy member with
        // no session event for stuck_minutes is marked stuck, once per episode.
        // A question, a Zerus build or Background work is never stuck; the lead
        // is never stuck either (L1).
        if found.live {
            // L3: phase and activity on change, the event time at most once a
            // minute (status rounds to minutes anyway).
            let moved = match (member.seen_event_at, found.last_event_at) {
                (None, None) => false,
                (old, new) => new.is_none_or(|at| old.is_none_or(|old| (at - old).abs() >= 60.0)),
            };
            if member.seen_phase != found.phase || member.seen_activity != found.activity || moved {
                steps.push(Step::Watch {
                    member: member.id.clone(),
                    phase: found.phase.clone(),
                    activity: found.activity.clone(),
                    last_event_at: found.last_event_at,
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
            let stuck = busy && !found.asking && !found.background && !found.building && quiet >= limit;
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
        } else if member.stuck.is_some() {
            steps.push(Step::Unstuck { member: member.id.clone() });
        }
    }
    // U56: a pending task whose member is kept for it (pinned, or every planned
    // assignee with keep_assignees) and who is stuck, stopped or offline says so
    // in its waits, so the lead sees why it never starts (N4).
    for task in &team.tasks {
        if task.status != TaskStatus::Pending || task.assignee.is_empty() || !team.keeps_assignee(task) {
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
    // limited or swapped session is never restarted, and neither is one that
    // has not been seen live since (a failed relaunch does not loop).
    if team.settings.restart_crashed && team.status == TeamStatus::Running {
        for member in &team.members {
            if member.id == team.lead
                || member.agent == "opencode"
                || member.state != MemberState::Active
                || member.session.is_none()
                || member.limited.is_some()
            {
                continue;
            }
            let Some(found) = session_of(member, seen) else { continue };
            if found.live || found.state != "stopped" || found.limited.is_some() || !member.seen.is_some_and(|seen| seen.live) {
                continue;
            }
            let restarted = member.restarts.iter().filter(|at| now - **at <= RESTART_WINDOW).count();
            if restarted < RESTARTS_PER_HOUR {
                steps.push(Step::Restart { member: member.id.clone() });
            } else if !member.crash_told {
                steps.push(Step::CrashStopped { member: member.id.clone() });
            }
        }
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
        if !(found.live && found.activity == "busy" && !found.asking && !found.background && quiet >= SILENT_MINUTES * 60.0) {
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
        if !found.asking && member.pending.is_some() {
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
    // C2: one message lists the new items, at most once per interval. The
    // owner (2026-10-28): it reaches a lead in its own turn too, which takes
    // queued input between its tool calls (a permission waited 8 minutes
    // otherwise); a question or dialog on the lead's screen still holds it.
    let lead_reachable = lead_ready || lead.is_some_and(|found| found.live && !found.asking && found.activity == "busy");
    if lead_reachable {
        let new = inbox::items(team).into_iter().filter(|item| !team.inbox.told.contains(&item.key)).count();
        let interval = team.settings.inbox_interval_seconds as f64;
        if new > 0 && (team.inbox.told_at == 0.0 || now - team.inbox.told_at >= interval) {
            steps.push(Step::Inbox);
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
        let new = inbox::items_for(team, &sub.id).into_iter().filter(|item| !sub.inbox.told.contains(&item.key)).count();
        let interval = team.settings.inbox_interval_seconds as f64;
        if new > 0 && (sub.inbox.told_at == 0.0 || now - sub.inbox.told_at >= interval) {
            steps.push(Step::SubInbox { member: sub.id.clone() });
        }
    }
    // #511 N3: a member whose worked-ahead attempt was revoked hears it once,
    // as soon as its session runs; the message waits behind its turn.
    for stop in team.stops.iter().filter(|stop| stop.notice == Notice::Pending) {
        if team.member(&stop.member).and_then(|member| session_of(member, seen)).is_some_and(|found| found.live) {
            steps.push(Step::TellStop { stop: stop.id.clone() });
        }
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
    // C7: a member idle without a ready task while tasks are open starves the
    // plan. The lead hears once after 20 minutes; the episode ends with work.
    // U56/#728: a restart the lead or the owner asked for runs on its own.
    if team.status == TeamStatus::Running {
        for member in team.members.iter().filter(|member| member.restart_pending) {
            steps.push(Step::RestartStuck { member: member.id.clone() });
        }
    }
    // U61: an auto-accepted draft whose phases ahead are done and whose plan has
    // no blocker is accepted by Auto here, in its own change; the ordinary ready
    // path then hands its tasks out (launch, rebalance, role routing).
    for phase in super::ops::drafts_ready_to_auto_accept(team, &BTreeSet::new()) {
        steps.push(Step::AutoAcceptDraft { phase });
    }
    let ready_ids = plan::ready(team);
    let staging_open = team.tasks.iter().any(|task| !task.status.finished());
    // U9: while every open task waits for the owner's phase approval, a
    // member with nothing ready is expected; no episode starts or continues.
    let owner_wait = plan::waiting_for_owner(team);
    for member in &team.members {
        if member.id == team.lead || matches!(member.state, MemberState::Removed | MemberState::Paused) {
            continue;
        }
        let Some(session) = member.session.as_ref() else { continue };
        let Some(found) = seen.iter().find(|seen| seen.host == session.host && seen.name == session.name) else { continue };
        let has_work = busy.contains(member.id.as_str())
            || ready_ids.iter().any(|id| team.task(*id).is_some_and(|task| task.assignee == member.id));
        // U49: a draining member is idle on purpose; its episode ends quietly.
        if !staging_open || owner_wait || has_work || member.draining || !(found.free && found.live) {
            if member.starved_since.is_some() {
                steps.push(Step::StarvedEnded { member: member.id.clone() });
            }
        } else if member.starved_since.is_none() {
            steps.push(Step::Starving { member: member.id.clone() });
        } else if !member.starved_told
            && now - member.starved_since.unwrap_or(now) >= STARVED_AFTER
        {
            steps.push(Step::PlanStarved { member: member.id.clone() });
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
        team.tasks.iter().flat_map(|t| &t.attempts).any(|a| a.member == member.id && a.state == AttemptState::Refused && now - a.sent_at < REFUSAL_PAUSE)
    };
    let available = |member: &Member, busy: &BTreeSet<&str>| {
        !matches!(member.state, MemberState::Removed | MemberState::Paused)
            && !member.draining
            && !busy.contains(member.id.as_str())
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
            if let Some(member) = candidates.iter().filter_map(|id| team.member(id)).find(|member| available(member, &busy)) {
                busy.insert(member.id.as_str());
                steps.push(Step::StageOut { task, member: member.id.clone() });
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
        if found.free && found.reply_at.unwrap_or(0.0) > delivered {
            steps.push(Step::StageSilent { task: task.id, run: run.id.clone() });
        }
    }
    // C6: among a member's ready tasks the one with the most transitive
    // dependants goes first, then the lowest number.
    let ready = plan::dispatch_order(team, ready_ids);
    for id in ready {
        let Some(task) = team.task(id) else { continue };
        // Spec 0: a refused or unconfirmed step waits for the lead or you.
        if task.waits.is_some() {
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
                    if switching.contains(member.id.as_str()) && available(member, &BTreeSet::new()) && super::stages::held(team, &member.id).is_none() {
                        switching.remove(member.id.as_str());
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
                !task.pinned
                    && task
                        .attempts
                        .iter()
                        .any(|a| a.member == m.id && matches!(a.state, AttemptState::Rejected | AttemptState::Refused) && !super::pipeline::pipeline_returned(a))
            };
            let candidates = super::models::candidates(team, task);
            let fits: Vec<(&Member, super::models::Fit)> = candidates
                .iter()
                .filter(|m| available(m, &busy) && !failed(m) && super::stages::held(team, &m.id).is_none())
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
                    steps.push(deliver(task, member));
                }
                Some((member, super::models::Fit::Switch(choice))) => {
                    busy.insert(member.id.as_str());
                    let found = session_of(member, seen);
                    let from = found.filter(|found| !found.model.is_empty()).map(|found| super::models::label(&found.model, &found.effort)).unwrap_or_default();
                    steps.push(Step::SwitchModel { task: id, member: member.id.clone(), from, model: choice.model.clone(), effort: choice.effort.clone() });
                }
                None => {
                    // Start the first member that could run it once it is up.
                    if let Some(member) = candidates.iter().find(|m| {
                        m.session.is_none() && m.state != MemberState::Paused && m.launch_id.is_empty() && m.error.is_empty()
                            && super::models::fit(m, task, super::models::launch_model(m)).is_some()
                    }) {
                        if launching.insert(member.id.clone()) {
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
            if matches!(member.state, MemberState::Removed | MemberState::Paused) || busy.contains(member.id.as_str()) {
                continue;
            }
            if member.session.is_none() {
                if member.launch_id.is_empty() && member.error.is_empty() && launching.insert(member.id.clone()) {
                    steps.push(Step::Launch { member: member.id.clone() });
                }
                continue;
            }
            busy.insert(member.id.as_str());
            steps.push(Step::HandOut { task: id, member: member.id.clone() });
            continue;
        }
        // Members with one role do the same work: a ready task goes to a free
        // one when its assignee is busy, and a retry to one that did not fail
        // it (a fresh context). The lead's or your choice stays.
        // U36: a test holding the assignee's folder keeps its next task back;
        // a free member with the same role may take it.
        let held = super::stages::held(team, &member.id).is_some();
        if !team.keeps_assignee(task) {
            let failed = |m: &Member| {
            task.attempts
                .iter()
                .any(|a| a.member == m.id && matches!(a.state, AttemptState::Rejected | AttemptState::Refused) && !super::pipeline::pipeline_returned(a))
        };
            let assignee_fits = available(member, &busy) && !failed(member) && !held;
            if !assignee_fits {
                let role = team.role_of(&member.id);
                if let Some(peer) = team.members.iter().find(|m| {
                    m.id != member.id && m.id != team.lead && team.role_of(&m.id) == role && available(m, &busy) && !failed(m)
                        && super::stages::held(team, &m.id).is_none()
                }) {
                    busy.insert(peer.id.as_str());
                    steps.push(Step::Rebalance { task: id, from: member.id.clone(), to: peer.id.clone() });
                    continue;
                }
            }
        }
        if matches!(member.state, MemberState::Removed | MemberState::Paused) || member.draining || busy.contains(member.id.as_str()) || held {
            continue;
        }
        if member.session.is_none() {
            if member.launch_id.is_empty() && member.error.is_empty() && launching.insert(member.id.clone()) {
                steps.push(Step::Launch { member: member.id.clone() });
            }
            continue;
        }
        if refused_recently(member) || !session_of(member, seen).is_some_and(|s| s.free && !s.run_id.is_empty()) {
            continue;
        }
        busy.insert(member.id.as_str());
        steps.push(if member.needs_settings() { Step::Settings { member: member.id.clone() } } else { Step::HandOut { task: id, member: member.id.clone() } });
    }
    // U31: work ahead. An idle member may take an independent task of the next
    // draft phase (#511 N2: every dependency accepted). U61: the same path takes
    // the tasks of an auto-accepted draft whose phases ahead are done, or that
    // the lead started ahead; that hand-out accepts the phase by Auto.
    let ahead: Vec<u32> = {
        let mut ids = work_ahead_tasks(team);
        for id in plan::auto_draft_tasks(team) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    };
    if !ahead.is_empty() {
        for member in &team.members {
            if !available(member, &busy) || busy.contains(member.id.as_str()) {
                continue;
            }
            if let Some(task) = ahead.iter().filter_map(|id| team.task(*id)).find(|task| task.assignee == member.id) {
                busy.insert(member.id.as_str());
                steps.push(Step::WorkAhead { task: task.id, member: member.id.clone() });
            }
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
    coalesce_lead(steps)
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
    let work = team.tasks.iter().any(|task| task.assignee == member && task_owes(task));
    !open_attempt && !open_stage && !work && team.reports_of(member).is_empty()
}

/// U49: a task still to start or work on, as opposed to one at the lead or in
/// a stage of its pipeline (which no implementer waits for).
fn task_owes(task: &Task) -> bool {
    matches!(task.status, TaskStatus::Draft | TaskStatus::Pending | TaskStatus::Assigned | TaskStatus::InProgress)
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

fn clear_restarted(store: &mut Store, team_id: &str, member_id: &str, role: &str) -> anyhow::Result<()> {
    change(store, team_id, |team, events| {
        let Some(found) = team.members.iter_mut().find(|found| found.id == member_id) else { return Ok(()) };
        found.restart_pending = false;
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
    }
    change(store, team_id, |team, events| super::merge_queue::step(team, &database, now, events).map(|_| ()))
}

fn test_queue_tick(store: &mut Store, team_id: &str, now: f64) -> anyhow::Result<()> {
    use chrono::Timelike;
    let team = store.load(team_id)?.with_context(|| format!("Unknown team {team_id}"))?;
    if team.status != TeamStatus::Running {
        return Ok(());
    }
    let project = std::path::Path::new(&team.folder);
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

/// U31: the tasks of the next draft phase an idle member may work ahead on: a
/// task (not a check) still waiting for its phase, whose dependencies are all
/// accepted (#511 N2: a dependency inside a draft or queued phase counts too)
/// and that names no file an open running task also names.
pub(crate) fn work_ahead_tasks(team: &Team) -> Vec<u32> {
    if !team.settings.work_ahead {
        return Vec::new();
    }
    // U61: work ahead keeps its meaning for review drafts; an auto-accepted
    // draft starts early only on the lead's start-ahead (or when the phases
    // ahead are done, which the Auto path above handles).
    let Some(draft) = team
        .phases
        .iter()
        .filter(|phase| phase.state == PhaseState::Draft && phase.approval != Approval::Auto)
        .map(|phase| phase.n)
        .min()
    else {
        return Vec::new();
    };
    let running: Vec<&Task> = team
        .tasks
        .iter()
        .filter(|task| {
            !task.status.finished() && team.phases.iter().find(|phase| phase.n == task.phase).is_none_or(|phase| phase.state == PhaseState::Approved)
        })
        .collect();
    let files = |text: &str| -> BTreeSet<String> {
        text.split_whitespace()
            .map(|word| word.trim_matches(|c: char| "`'\",;()[]{}".contains(c)))
            .filter(|word| word.contains('/') || word.contains('\\') || word.rsplit_once('.').is_some_and(|(_, ext)| !ext.is_empty() && ext.len() <= 4))
            .map(str::to_lowercase)
            .collect()
    };
    let running_files: BTreeSet<String> = running.iter().flat_map(|task| files(&format!("{} {}", task.title, task.description))).collect();
    team.tasks
        .iter()
        .filter(|task| {
            task.phase == draft
                && matches!(task.status, TaskStatus::Pending | TaskStatus::Draft)
                && task.kind == TaskKind::Task
                && task.hold.is_none()
                && task.waits.is_none()
                // U63: nothing works ahead behind a phase awaiting the owner's acceptance.
                && plan::acceptance_block(team, task.phase).is_none()
                && task.deps.iter().all(|dep| plan::satisfied(team, dep.task, DependencyKind::Hard))
                && files(&format!("{} {}", task.title, task.description)).is_disjoint(&running_files)
        })
        .map(|task| task.id)
        .collect()
}

/// Whether Zerus may hand `task_id` out now: a ready task of the running plan,
/// or (U31) a task of the next draft phase an idle member works ahead on. A
/// draft phase's tasks are Draft, which `plan::ready` never lists.
fn can_hand_out(team: &Team, task_id: u32) -> bool {
    if team.status != TeamStatus::Running {
        return false;
    }
    if plan::ready(team).contains(&task_id) || work_ahead_tasks(team).contains(&task_id) {
        return true;
    }
    // U61: an auto-accepted draft whose phases ahead are done, or that the lead
    // started ahead, hands out its independent draft tasks; the first hand-out
    // accepts the phase.
    plan::auto_draft_tasks(team).contains(&task_id)
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
    let listed: Vec<(&Task, &Attempt)> = task
        .deps
        .iter()
        .filter_map(|dep| team.task(dep.task))
        .filter_map(|found| dependency_attempt(found).map(|attempt| (found, attempt)))
        .collect();
    let mut text = String::new();
    let mut used = 0usize;
    for (index, (found, attempt)) in listed.iter().enumerate() {
        if index == DEPENDENCY_LIMIT {
            text += &format!("[{} more dependencies omitted]\n", listed.len() - index);
            break;
        }
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
                "\n[cut: shown {shown} of {total} characters; full report: hgs team status --json, task #{}]\n",
                found.id
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
pub(crate) fn hand_off_text_with(
    team: &Team,
    task: &Task,
    attempt: &Attempt,
    report_host: &str,
    member_host: &str,
    extra: &str,
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
    if let Some(prompt) = team.member(&attempt.member).map(|m| m.prompt.trim()).filter(|p| !p.is_empty()) {
        // U25: the role's prompt blocks in its order, when the catalog has one.
        // N3: a member's own snapshot wins; an old member keeps today's live text.
        let snapshot = team.member(&attempt.member).and_then(|member| member.prompt_snapshot.clone());
        let (ordered, has_task) = if let Some(snapshot) = snapshot {
            let has_task = super::catalog::sends_task(&snapshot.prompt_order, &snapshot.prompt_off, &snapshot.task_template);
            (super::catalog::prompt_order_text_snapshot(&snapshot, prompt, Some(task)), has_task)
        } else {
            super::catalog::load().ok().and_then(|catalog| {
                let role_name = team.role_of(&attempt.member);
                let role = catalog.roles.iter().find(|role| role.id == role_name || role.name == role_name)?;
                let has_task = super::catalog::sends_task(&role.prompt_order, &role.prompt_off, &role.task_template);
                Some((super::catalog::prompt_order_text(role, prompt, Some(task), ""), has_task))
            }).unwrap_or((None, false))
        };
        task_in_prompt = has_task;
        if let Some(ordered) = ordered.filter(|text| !text.trim().is_empty()) {
            text += &ordered;
        } else {
            text += &format!("Role: {prompt}\n");
        }
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
    text += &dependencies_text(team, task, report_host);
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
         Summarize what changed, where, and how you checked it. If you cannot run that command, end your turn with \
         the report as your last message: Zerus gives it to {boss}. A blocker goes to {boss} too: ask it, not the owner.",
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
    // U30: a build Zerus runs is work; a job left in the member's own shell is a service.
    text += "\nStart builds and long tests through Zerus (hgs team run, or hgs run: hgs guide run) so your session shows \
             as Building and your reply waits for them; a job you leave in your own shell after your turn counts as a \
             service, and your session looks idle.";
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
    let deadline = Instant::now() + Duration::from_secs(20);
    let run = |args: &[&str]| -> Option<String> {
        let mut hardened = vec!["-c", "core.hooksPath="];
        hardened.extend_from_slice(args);
        super::super::worktrees::git(Path::new(folder), &hardened, deadline)
            .ok()
            .map(|out| String::from_utf8_lossy(&out).into_owned())
    };
    // A filter, include, hook path or fsmonitor of the repository's own config
    // could still run code: such a folder is not read (as `commit_in`).
    if let Some(listing) = run(&["config", "--list", "--show-scope", "--name-only"]) {
        if let Some(reason) = refused_repository_config(&listing) {
            return format!("\nCommits and working tree: not read here, this repository {reason}.\n");
        }
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
pub(crate) fn brief_text(team: &Team) -> String {
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
    if let Some(prompt) = team.member(&team.lead).map(|m| m.prompt.trim()).filter(|p| !p.is_empty()) {
        let ordered = if let Some(snapshot) = team.member(&team.lead).and_then(|member| member.prompt_snapshot.clone()) {
            super::catalog::prompt_order_text_snapshot(&snapshot, prompt, None)
        } else {
            super::catalog::load().ok().and_then(|catalog| {
                let role_name = team.role_of(&team.lead);
                let role = catalog.roles.iter().find(|role| role.id == role_name || role.name == role_name)?;
                super::catalog::prompt_order_text(role, prompt, None, "")
            })
        };
        if let Some(ordered) = ordered.filter(|text| !text.trim().is_empty()) {
            text += &ordered;
        } else {
            text += &format!("Your role: {prompt}\n");
        }
    }
    if !team.lead_protocol.trim().is_empty() {
        text += &format!("\nTeam protocol:\n{}\n", team.lead_protocol.trim());
    }
    if let Ok(catalog) = super::catalog::load() {
        if !catalog.planning_rules.trim().is_empty() {
            text += &format!("\nPlanning rules (v{}):\n{}\n", catalog.version, catalog.planning_rules.trim());
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
                 send its report is never needed: deny it, Zerus takes the report from its folder or its last reply.\n")
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
    match (agent, profile) {
        ("claude", "read") => Some("plan"),
        ("claude", "write") => Some("default"),
        ("claude", "build" | "commit") => Some("acceptEdits"),
        _ => None,
    }
}

/// B4/F10: a sandboxed agent's folder is a clone (its `.git` is inside, so
/// merge and fetch work there); other agents get a worktree. Used when a plan
/// leaves the folder to the team (folder policy auto).
pub(crate) fn default_folder(agent: &str) -> MemberFolder {
    if agent == "dsh" {
        MemberFolder::Clone
    } else {
        MemberFolder::Worktree
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

pub(crate) fn git_at(folder: &str, args: &[&str]) -> Option<String> {
    let output = git_command(folder).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
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
    if let Some(origin) = git_at(folder, &["config", "--get", "remote.origin.url"]) {
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

#[cfg(test)]
thread_local! {
    static FOLDER_FACTS_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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
    member_folder_with(team, member, &|host, args| hgs(host, args, None, Duration::from_secs(300)))
}

/// `member_folder` with the `hgs` call it makes (`run(computer, args)`), so the
/// whole decision is tested without a second hgs.
fn member_folder_with(team: &Team, member: &Member, run: &dyn Fn(&str, &[&str]) -> anyhow::Result<Output>) -> anyhow::Result<String> {
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
    if remote {
        if let Some(found) = remote_checkout(run, &member.computer, &destination)? {
            anyhow::ensure!(
                found == branch,
                "{destination} already exists on {} and is not this member's folder ({})",
                member.computer,
                if found.is_empty() { "no checkout of its own".to_owned() } else { format!("on {found}, not {branch}") }
            );
            return Ok(destination);
        }
    } else if Path::new(&destination).exists() {
        // A failed create may leave its empty reservation: it goes.
        let empty = std::fs::read_dir(&destination).map(|mut entries| entries.next().is_none()).unwrap_or(false);
        if empty {
            std::fs::remove_dir(&destination)?;
        } else {
            if let Some(problem) = folder_problem(&destination, &folder, Some(&branch)) {
                anyhow::bail!("{destination} already exists and is not this member's folder: {problem}");
            }
            return Ok(destination);
        }
    }
    let mut args: Vec<&str> = vec!["worktrees", "create", "--path", &folder, "--branch", &branch, "--destination", &destination, "--base", "HEAD", "--json"];
    if member.folder == MemberFolder::Clone {
        args.insert(2, "--clone");
    }
    // N9: a DeepSeek member works in a sandbox that needs full control of its
    // folder; every other member's worktree is created without touching ACLs.
    if member.agent == "dsh" {
        args.insert(2, "--sandbox");
    }
    let output = run(&member.computer, &args)?;
    anyhow::ensure!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr).trim());
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
        args.push(intro);
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

/// DeepSeek's model is provider/model; a bare name keeps the session's own.
fn apply_settings(member: &Member) -> anyhow::Result<()> {
    let session = member.session.as_ref().context("no session")?;
    let mut input = json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_run_id":session.run_id,
        "expected_conversation_id":session.conversation_id});
    if member.model.contains('/') {
        input["model"] = json!(member.model);
    }
    if !member.effort.is_empty() {
        input["effort"] = json!(member.effort);
    }
    let output = hgs(&session.host, &["settings", &session.name, "--json"], Some(&input), Duration::from_secs(120))?;
    anyhow::ensure!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr).trim());
    Ok(())
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
    let mut input = json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_run_id":session.run_id,
        "expected_conversation_id":session.conversation_id,"model":model});
    if !effort.is_empty() {
        input["effort"] = json!(effort);
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
    change(store, &team.id, |team, events| {
        let (role, name) = (team.role_of(member_id), team.name_of(member_id));
        let task = team.task_mut(task_id).context("unknown task")?;
        if let Some(step) = task.prepare.iter_mut().rev().find(|step| step.step == "model" && step.result == "pending" && step.member == member_id) {
            step.result = outcome.0.into();
            step.reason = outcome.1.clone();
        }
        super::models::wait(task, "model", &format!("{name} could not switch to {to}: {}", outcome.1), now());
        events.push(Event::new(
            if outcome.0 == "refused" { "model_switch_refused" } else { "model_switch_uncertain" },
            json!({"task":task_id,"member":role,"to":to,"reason":outcome.1}),
        ));
        Ok(())
    })
}

fn launch(team: &Team, member: &Member) -> anyhow::Result<()> {
    let folder = member_folder(team, member)?;
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

/// U46: whether this agent has the native context action. DeepSeek compacts but
/// has no native clear; OpenCode has neither yet.
fn native_context(agent: &str, action: &str) -> bool {
    match action {
        "compact" => matches!(agent, "claude" | "codex" | "kimi" | "dsh"),
        "clear" => matches!(agent, "claude" | "codex" | "kimi"),
        _ => false,
    }
}

fn agent_label(agent: &str) -> &str {
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

/// U46: what a hand-out leaves for itself: its prepare steps, the conversation
/// a clear produced, and whether it resumes a wait already recorded on the task.
#[derive(Clone, Default)]
struct Preparation {
    steps: Vec<PrepareStep>,
    conversation: Option<(String, String)>,
    resume: bool,
}


/// U48: one git command in a member's folder, the repository's hooks and
/// fsmonitor off, bounded in time: its output, or why it failed.
fn git_bounded(folder: &str, args: &[&str]) -> std::result::Result<String, (bool, String)> {
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
        let base = if team.settings.base_branch.is_empty() {
            match lead_branch(team) {
                Ok(base) => base,
                Err(error) => return Some(step(&current, "refused", format!("{error:#}"))),
            }
        } else {
            team.settings.base_branch.clone()
        };
        (git_bounded(&folder, &["checkout", "-b", &branch, &base]), format!("created from {base}"))
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
    let action = context_before_task(&team, &member);
    if action.is_empty() {
        return hand_out_now(store, team_id, task_id, member_id, from, host, Preparation::default());
    }
    let Some(session) = member.session.clone() else {
        let step = context_failure(context_step(&action, now()), "refused", "The member's session is not started".into());
        return record_waits(store, team_id, task_id, member_id, from, step);
    };
    match run_context(&member, &session, &action) {
        // The action is submitted: record it and look again on the next tick.
        ContextStart::Started(request) => record_context_start(store, team_id, task_id, member_id, from, request),
        ContextStart::Done { step, conversation } => {
            let conversation = conversation.map(|new| (session.run_id.clone(), new));
            hand_out_now(store, team_id, task_id, member_id, from, host, Preparation { steps: vec![step], conversation, resume: false })
        }
        ContextStart::Failed { step } => record_waits(store, team_id, task_id, member_id, from, step),
    }
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
    // Record the attempt before sending; a crash leaves it Unconfirmed.
    let attempt_id = uuid::Uuid::new_v4().to_string();
    let mut message = None;
    // N16: the finding this hand-out carries comes back if the send is refused.
    let mut note = String::new();
    let mut extra = String::new();
    let mut git_job: Option<(String, f64)> = None;
    change(store, team_id, |team, events| {
        // U46: the confirmed wait is done; this hand-out replaces it.
        if resume {
            if let Some(task) = team.task_mut(task_id) {
                task.waits = None;
            }
        }
        anyhow::ensure!(can_hand_out(team, task_id), "no longer ready");
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
        let keeps = team.task(task_id).is_some_and(|task| team.keeps_assignee(task));
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
        task.attempts.push(Attempt {
            n,
            id: attempt_id.clone(),
            member: member_id.to_owned(),
            sent_at: now(),
            rebalanced_from: rebalanced.clone(),

            ..Default::default()
        });
        // Spec 1: the steps taken for this hand-out go with its attempt, in
        // their order: branch (U48), model (U45), context (U46).
        let mut prepared = super::models::take_for_attempt(task, member_id);
        prepared.extend(prepare.iter().cloned());
        prepared.sort_by_key(|step| match step.step.as_str() {
            "branch" => 0,
            "model" => 1,
            _ => 2,
        });
        for step in &mut prepared {
            step.member.clear();
        }
        task.attempts.last_mut().unwrap().prepare = prepared;
        if let (false, Some((from_role, to_role))) = (rebalanced.is_empty(), roles) {
            events.push(Event::new("rebalanced", json!({"task":task_id,"from":from_role,"to":to_role,"from_member":rebalanced,"to_member":member_id})));
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
        message = Some((session, hand_off_text_with(team, task, task.attempts.last().unwrap(), host, &member_host, &extra)));
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
    let outcome = send(&session, &attempt_id, &text);
    change(store, team_id, |team, events| {
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
        Ok(())
    })
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
        message = Some((run_id.clone(), super::stages::hand_off_text(team, task, run, &prepared, &report_host)));
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
    let outcome = send(&session, &run_id, &text);
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
        let keeps = team.task(task_id).is_some_and(|task| team.keeps_assignee(task));
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
            events.push(Event::new("rebalanced", json!({"task":task_id,"from":from_role,"to":to_role,"from_member":rebalanced,"to_member":member_id})));
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
        let keeps = team.task(task_id).is_some_and(|task| team.keeps_assignee(task));
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
            events.push(Event::new("rebalanced", json!({"task":task_id,"from":from_role,"to":to_role,"from_member":rebalanced,"to_member":member_id})));
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

/// One message for the lead: the open tasks Zerus gave to another member
/// with the same role, and to whom.
pub(crate) fn rebalanced_text(team: &Team, tasks: &[u32]) -> String {
    let mut text = String::from("[Zerus team \u{b7} from: Zerus \u{b7} tasks moved]\n");
    for task in tasks.iter().filter_map(|id| team.task(*id)) {
        if let Some(attempt) = task.open_attempt() {
            text += &format!(
                "#{} {}: given to {} instead of {}, which was busy or failed its last attempt.\n",
                task.id,
                task.title,
                team.name_of(&attempt.member),
                team.name_of(&attempt.rebalanced_from)
            );
        }
    }
    text + "Members with one role share their work; nothing to do unless you want another member (hgs team reassign keeps your choice)."
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
                card["can_answer"] != false
                    && !member.asked.contains(&format!(
                        "{}:{}",
                        card["question_id"].as_str().unwrap_or_default(),
                        card["question_hash"].as_str().unwrap_or_default()
                    ))
            })
            .map(|card| PendingQuestion {
                question_id: card["question_id"].as_str().unwrap_or_default().to_owned(),
                question_hash: card["question_hash"].as_str().unwrap_or_default().to_owned(),
                approval: card["approval"] == true,
                at: now(),
                card: card.clone(),
            })
    };
    let answered = ask.is_none() && same.is_none();
    if ask.is_none() && !answered {
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
    let command = card["tool_input"]["command"].as_str().filter(|command| !command.trim().is_empty())?;
    let answer = permission_answer(card).filter(|answer| answer["allow"].is_string())?;
    let tool = card["tool"].as_str().or_else(|| card["questions"][0]["header"].as_str()).unwrap_or_default();
    let folder = member_folder_path(team, member).ok()?;
    let action = safe_actions::find(team, member, tool, command, Path::new(&folder))?;
    Some((action, answer))
}

/// Sends the reserved one-time allow and records it by the safe action; when
/// it fails, the question stays in the lead's inbox as any other.
fn allow_safe(store: &mut Store, team: &Team, member_id: &str, pending: &PendingQuestion, action: &Value, answer: &Value) -> anyhow::Result<()> {
    let session = team.member(member_id).and_then(|member| member.session.clone()).context("the member has no session")?;
    let request = json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_run_id":session.run_id,
        "expected_conversation_id":session.conversation_id,"question_id":pending.question_id,
        "expected_question_hash":pending.question_hash,"answers":[{"question_id":answer["question_id"],"selected_option_ids":[answer["allow"]]}]});
    let delivered = match hgs(&session.host, &["answer", &session.name, "--json"], Some(&request), Duration::from_secs(120)) {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
        Err(error) => Err(error.to_string()),
    };
    let role = team.role_of(member_id);
    let by = format!("safe action {}", action["id"].as_str().unwrap_or_default());
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

/// Hands the lead its own task without a message (C1): the attempt exists, so
/// `hgs team current` names it and the inbox lists it to report and accept.
fn lead_hand_out(store: &mut Store, team_id: &str, task_id: u32) -> anyhow::Result<()> {
    // U46: the lead's own hand-out is a hand-out too; a team that sets no
    // context action keeps today's path.
    let team = store.load(team_id)?.with_context(|| format!("Unknown team {team_id}"))?;
    let lead = team.member(&team.lead).cloned().context("the team has no lead")?;
    let action = context_before_task(&team, &lead);
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

/// Sends the one coalesced inbox message and remembers which items it named
/// (C2). A refusal leaves everything pending for the next interval.
fn tell_lead_inbox(store: &mut Store, team: &Team) -> anyhow::Result<()> {
    let lead = team.member(&team.lead).and_then(|m| m.session.clone()).context("the lead has no session")?;
    let items = inbox::items(team);
    let new: Vec<inbox::Item> = items.iter().filter(|item| !team.inbox.told.contains(&item.key)).cloned().collect();
    anyhow::ensure!(!new.is_empty(), "no new inbox items");
    let outcome = send(&lead, &uuid::Uuid::new_v4().to_string(), &inbox::text(team, &new, now()));
    if matches!(outcome, Sent::Refused(_)) {
        return Ok(());
    }
    change(store, &team.id, |team, events| {
        let named = new.len();
        // L2: keep the keys of items that are still open.
        let open: BTreeSet<String> = inbox::items(team).into_iter().map(|item| item.key).collect();
        inbox::mark_told(&mut team.inbox.told, &new, &open);
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
        inbox::items_for(team, member).into_iter().filter(|item| !sub.inbox.told.contains(&item.key)).collect();
    anyhow::ensure!(!new.is_empty(), "no new inbox items");
    let outcome = send(&session, &uuid::Uuid::new_v4().to_string(), &inbox::text(team, &new, now()));
    if matches!(outcome, Sent::Refused(_)) {
        return Ok(());
    }
    let member = member.to_owned();
    change(store, &team.id, |team, events| {
        let open: BTreeSet<String> = inbox::items_for(team, &member).into_iter().map(|item| item.key).collect();
        let role = team.role_of(&member);
        let Some(sub) = team.members.iter_mut().find(|found| found.id == member) else { return Ok(()) };
        inbox::mark_told(&mut sub.inbox.told, &new, &open);
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
    let newest = inspected
        .ok()
        .filter(|record| record["run_id"].as_str().unwrap_or_default() == session.run_id)
        .and_then(|record| newest_event(&record));
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
    if quiet >= SILENT_MINUTES * 60.0 {
        tell_lead_silent(store, team, task_id, (quiet / 60.0).round().max(1.0) as u64, unreadable)?;
    }
    Ok(())
}

/// #761: the member's last report or turn end, the progress a DeepSeek listing
/// does not show once the turn is over.
fn last_progress(team: &Team, member: &str) -> Option<f64> {
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

/// The message that tells the lead a member starves on the plan (C7), with the
/// blocking chain of the open tasks.
pub(crate) fn starved_text(team: &Team, member: &Member, now: f64) -> String {
    let minutes = (((now - member.starved_since.unwrap_or(now)).max(0.0)) / 60.0).round() as u64;
    let role = team.name_of(&member.id);
    let ready: BTreeSet<u32> = plan::ready(team).into_iter().collect();
    let open: Vec<&Task> = team.tasks.iter().filter(|task| !task.status.finished()).collect();
    let mut chain = String::new();
    for task in open.iter().take(5) {
        if let Some(wait) = inbox::waits(team, task, ready.contains(&task.id)) {
            let on: Vec<String> = wait["on"].as_array().into_iter().flatten().map(|value| {
                value.as_str().map(str::to_owned).or_else(|| value.as_u64().map(|id| format!("#{id}"))).unwrap_or_default()
            }).collect();
            chain += &format!("  #{} {}: {} {}\n", task.id, task.title, wait["for"].as_str().unwrap_or_default(), on.join(", "));
        }
    }
    format!(
        "[Zerus team \u{b7} from: Zerus \u{b7} starved]\n{role} has had no ready task for {minutes} min while {} task(s) are open.\n\
         Blocking:\n{chain}\
         Give it work, release a held task or change the plan (hgs guide team-lead).",
        open.len()
    )
}

/// Tells the free lead once that a member starves on the plan (C7).
fn tell_lead_starved(store: &mut Store, team: &Team, member_id: &str) -> anyhow::Result<()> {
    let member = team.member(member_id).context("unknown member")?.clone();
    let lead = team.member(&team.lead).and_then(|m| m.session.clone()).context("the lead has no session")?;
    let outcome = send(&lead, &uuid::Uuid::new_v4().to_string(), &starved_text(team, &member, now()));
    if matches!(outcome, Sent::Refused(_)) {
        return Ok(());
    }
    let role = team.role_of(member_id);
    change(store, &team.id, |team, events| {
        let Some(found) = team.members.iter_mut().find(|found| found.id == member_id) else {
            return Ok(());
        };
        found.starved_told = true;
        let minutes = (((now() - found.starved_since.unwrap_or(now())).max(0.0)) / 60.0).round() as u64;
        events.push(Event::new("plan_starved", json!({"member":role,"minutes":minutes})));
        Ok(())
    })
}

/// U52: the crashed member's session is launched again into the same
/// conversation, folder and name, and its open attempt is returned as
/// unconfirmed for the lead. The restart is counted before it is attempted, so
/// an agent that keeps failing cannot loop.
fn restart_crashed(store: &mut Store, team: &Team, member_id: &str) -> anyhow::Result<()> {
    let session = team.member(member_id).and_then(|member| member.session.clone()).context("the member has no session")?;
    let at = now();
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
        for task in &mut team.tasks {
            let task_id = task.id;
            if let Some(attempt) = task.open_attempt_mut().filter(|attempt| attempt.member == member_id) {
                attempt.state = AttemptState::Unconfirmed;
                attempt.error = format!("the session crashed at {at:.0}; restarted");
                events.push(Event::new("unconfirmed", json!({"task":task_id,"attempt":attempt.n,"error":attempt.error})));
            }
        }
        events.push(Event::new("member_restarted", json!({"member":role,"at":at})));
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

/// U52: too many crashes this hour leave the session stopped; the lead hears
/// once, through the inbox item `crash_told` shows.
fn crash_stopped(store: &mut Store, team: &Team, member_id: &str) -> anyhow::Result<()> {
    let at = now();
    change(store, &team.id, |team, events| {
        let role = team.role_of(member_id);
        let found = team.members.iter_mut().find(|found| found.id == member_id).context("unknown member")?;
        found.crash = Some(at);
        found.crash_told = true;
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

pub(crate) fn send_denial(session: &SessionRef, tool: &str, reason: &str) -> anyhow::Result<()> {
    match send(session, &uuid::Uuid::new_v4().to_string(), &denial_text(tool, reason)) {
        Sent::Refused(error) => anyhow::bail!("the denial did not reach the member: {error}"),
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
        let key = key.trim().to_lowercase();
        let reason = if key.starts_with("include.") || key.starts_with("includeif.") {
            "includes another Git config".to_owned()
        } else if key.starts_with("filter.") {
            "configures a Git filter".to_owned()
        } else if key.starts_with("merge.") && key.ends_with(".driver") {
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
    let listed = git(&["config", "--list", "--show-scope", "--name-only"])?;
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
    let listing = match super::super::worktrees::git(Path::new(folder), &["config", "--list", "--show-scope", "--name-only"], deadline) {
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
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(folder)
        .args(["-c", hooks.as_str(), "-c", "core.fsmonitor=false", "-c", "commit.gpgsign=false", "-c", "gpg.program="])
        .arg("--no-optional-locks")
        .args(["merge", "--no-edit", merge]);
    let remaining = deadline.saturating_duration_since(Instant::now()).max(Duration::from_secs(1));
    let mut outcome = super::templates::run_tree(&mut command, remaining, None);
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
    let brief_id = uuid::Uuid::new_v4().to_string();
    let mut message = None;
    change(store, team_id, |team, _| {
        anyhow::ensure!(team.brief == Notice::Pending, "already briefed");
        let lead = team.member(&team.lead).and_then(|m| m.session.clone()).context("the lead has no session")?;
        message = Some((lead, brief_text(team)));
        team.brief = Notice::Sending;
        team.brief_id = brief_id.clone();
        Ok(())
    })?;
    let (lead, text) = message.unwrap();
    let outcome = send(&lead, &brief_id, &text);
    change(store, team_id, |team, events| {
        if team.brief != Notice::Sending || team.brief_id != brief_id {
            return Ok(());
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

/// U58: a reply that only says it waits (a build lock, a queue) is the member's
/// status, never its report: the attempt shows No report instead of storing the
/// wait as the task's outcome.
fn waits_only(reply: &str) -> bool {
    let text = reply.trim().to_lowercase();
    if text.is_empty() || text.chars().count() > 400 {
        return false;
    }
    let waits = [
        "waiting", "wait for", "wait until", "waiting on", "blocked", "queued", "in the queue", "build lock",
        "lock is held", "жду", "ожида", "в очереди", "заблокир", "подож",
    ];
    if !waits.iter().any(|phrase| text.contains(phrase)) {
        return false;
    }
    let reports = [
        "report", "done", "finished", "complete", "passed", "failed", "commit", "pushed", "merged", "tests", "fixed",
        "implemented", "acceptance", "отчёт", "готово", "сделал", "прошл", "провал", "коммит", "исправ", "тест", "принят",
    ];
    !reports.iter().any(|marker| text.contains(marker))
}

/// U56: a reply whose last line says the member still waits ("waiting for the
/// editor to close", "I'll report once the build ends") is not its report,
/// whatever work the lines before name; a wait for the lead's review is.
fn still_waits(reply: &str) -> bool {
    let Some(last) = reply.lines().map(str::trim).rfind(|line| !line.is_empty()) else { return false };
    let last = last.to_lowercase();
    // A report ends waiting for its decision: that wait is the lead's, not one.
    let decision = ["review", "accept", "decision", "approv", "feedback", "ревью", "решени", "одобр", "приём", "прием"];
    if decision.iter().any(|word| last.contains(word)) {
        return false;
    }
    let still = [
        "still waiting", "still wait", "i'll report once", "i'll report when", "i will report once", "i will report when",
        "report as soon as", "ещё жду", "еще жду", "всё ещё жду", "все еще жду", "отчитаюсь, когда", "отчитаюсь когда",
        "отчитаюсь после", "дождусь",
    ];
    if still.iter().any(|phrase| last.contains(phrase)) {
        return true;
    }
    // "waiting for the editor to close": a wait for something to end.
    let ends = [" to close", " to finish", " to complete", " to end", " to exit", " to be released", " to unlock", " to free"];
    ["waiting for", "wait for", "waiting on", "waiting until"].iter().any(|wait| {
        last.match_indices(wait).any(|(at, _)| {
            let rest: String = last[at + wait.len()..].chars().take(60).collect();
            ends.iter().any(|end| rest.contains(end))
        })
    }) || ["жду, пока", "жду пока", "жду заверш", "жду оконч"].iter().any(|phrase| last.contains(phrase))
}

/// What capture refuses to make a report: a waiting-only reply (U58) or one
/// that ends saying it still waits (U56). The attempt then shows No report.
fn not_a_report(reply: &str) -> bool {
    waits_only(reply) || still_waits(reply)
}

/// A2: records the member's last reply as the attempt's report and marks it
/// captured, so the member may replace it while the task is undecided (A6).
fn capture_report(store: &mut Store, team_id: &str, task_id: u32, reply: &str, expected_reply: &str) -> anyhow::Result<()> {
    // U58/U56: a reply that only waits, or still waits, is a status; the attempt then shows No report.
    anyhow::ensure!(!not_a_report(reply), "the reply only says it waits; it is a status, not a report");
    change(store, team_id, |team, events| {
        let task = team.task(task_id).context("unknown task")?;
        anyhow::ensure!(!task.is_check(), "a check task reports its exit code, not a reply");
        let attempt = task.attempts.last().context("the task has no attempt")?;
        // The gate was checked before the reply was read, which can take up to
        // 45 s: re-check it here, including the reply identity (N1).
        anyhow::ensure!(
            attempt.state == AttemptState::Delivered
                && attempt.turn_ended_at.is_some()
                && attempt.open_question().is_none()
                && (expected_reply.is_empty() || attempt.turn_reply == expected_reply),
            "the attempt changed before its report was captured"
        );
        let name = team.name_of(&attempt.member);
        let (attempt_id, member) = (attempt.id.clone(), attempt.member.clone());
        let input = json!({"task":task_id,"attempt":attempt_id,
            "text":format!("(From the last reply of {name}; it ran no hgs team report.)\n\n{reply}")});
        ops::apply(team, "capture-report", &input, &ops::Actor::Member(member), &BTreeSet::new(), events)?;
        Ok(())
    })
}

/// A2/U20: an attempt whose turn end never reached the listing (a quiet session
/// the listing carried no reply mark for): records the turn end and the member's
/// last reply as its report in one change, so the next tick cannot read it as a
/// resumed turn and clear it.
fn capture_quiet(store: &mut Store, team_id: &str, task_id: u32, reply: &str) -> anyhow::Result<()> {
    // U58/U56: a reply that only waits, or still waits, is a status; the attempt then shows No report.
    anyhow::ensure!(!not_a_report(reply), "the reply only says it waits; it is a status, not a report");
    change(store, team_id, |team, events| {
        let task = team.task(task_id).context("unknown task")?;
        anyhow::ensure!(!task.is_check(), "a check task reports its exit code, not a reply");
        let attempt = task.attempts.last().context("the task has no attempt")?;
        anyhow::ensure!(
            attempt.state == AttemptState::Delivered && attempt.turn_ended_at.is_none() && attempt.open_question().is_none(),
            "the attempt changed before its report was captured"
        );
        let at = now();
        let name = team.name_of(&attempt.member);
        let (attempt_id, member) = (attempt.id.clone(), attempt.member.clone());
        let input = json!({"task":task_id,"attempt":attempt_id,
            "text":format!("(From the last reply of {name}; it ran no hgs team report.)\n\n{reply}")});
        ops::apply(team, "capture-report", &input, &ops::Actor::Member(member), &BTreeSet::new(), events)?;
        let found = team.task_mut(task_id).and_then(|task| task.attempts.last_mut()).context("the captured attempt")?;
        found.turn_ended_at = Some(at);
        found.turn_reply.clear();
        Ok(())
    })
}

fn run_step(store: &mut Store, team: &Team, step: &Step, host: &str) -> anyhow::Result<()> {
    match step {
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
            }
            Ok(())
        }),
        Step::Inbox => tell_lead_inbox(store, team),
        Step::SubInbox { member } => tell_sub_inbox(store, team, member),
        Step::SubBrief { member } => send_sub_brief(store, team, member),
        Step::RemindReviewer { task, member } => remind_reviewer(store, team, *task, member),
        Step::RemindReport { task } => remind_report(store, team, *task),
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
        Step::PlanStarved { member } => tell_lead_starved(store, team, member),
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
        Step::Adopt { member, session } => {
            let mut filing = None;
            change(store, &team.id, |team, events| {
                let found = team.members.iter_mut().find(|m| m.id == *member).context("unknown member")?;
                let started = found.session.is_none();
                let computer = found.computer.clone();
                found.session = Some(session.clone());
                found.state = MemberState::Active;
                if started {
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
            let launch_id = uuid::Uuid::new_v4().to_string();
            let mut planned = None;
            change(store, &team.id, |team, _| {
                let found = team.members.iter_mut().find(|m| m.id == *member).context("unknown member")?;
                anyhow::ensure!(found.session.is_none() && found.launch_id.is_empty(), "already started");
                found.launch_id = launch_id.clone();
                let found = found.clone();
                planned = Some((team.clone(), found));
                Ok(())
            })?;
            let (team, found) = planned.unwrap();
            let swapped = found.launch_after_swap;
            let prepared = prepare_member_branch(&team, &found);
            let launched = match prepared {
                Ok(note) => {
                    if let Some(note) = note {
                        change(store, &team.id, |team, events| {
                            events.push(Event::new(
                                "folder_checked_out",
                                json!({"member":team.role_of(&found.id),"note":note}),
                            ));
                            Ok(())
                        })?;
                    }
                    launch(&team, &found)
                }
                Err(error) => Err(error),
            };
            if let Err(error) = launched {
                // U54: a failed post-swap launch keeps `launch_after_swap`, so a
                // relaunch starts it at once; the error names its fix.
                let error = error.to_string();
                let error = if swapped {
                    format!("{error}; fix the folder or the old session, then hgs team relaunch starts it at once")
                } else {
                    error
                };
                change(store, &team.id, |team, events| {
                    if let Some(found) = team.members.iter_mut().find(|m| m.id == *member && m.session.is_none()) {
                        found.error = error.clone();
                        found.launch_after_swap = swapped;
                    }
                    events.push(Event::new("launch_failed", json!({"member":team.role_of(member),"error":error})));
                    Ok(())
                })?;
            } else {
                change(store, &team.id, |team, _| {
                    if let Some(found) = team.members.iter_mut().find(|m| m.id == *member && m.session.is_none()) {
                        found.launch_after_swap = false;
                    }
                    Ok(())
                })?;
            }
            Ok(())
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
        Step::AutoAcceptDraft { phase } => change(store, &team.id, |team, events| {
            super::ops::accept_auto_draft(team, *phase, &BTreeSet::new(), events);
            Ok(())
        }),
        Step::WorkAhead { task, member } => {
            hand_out(store, &team.id, *task, member, None, host)?;            change(store, &team.id, |team, _| {
                if let Some(attempt) = team.task_mut(*task).and_then(|task| task.attempts.last_mut()) {
                    attempt.worked_ahead = true;
                }
                Ok(())
            })
        }
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
        Step::Rebalanced { tasks } => tell_lead_rebalanced(store, team, tasks),
        Step::TurnEnded { task, at, reply } => change(store, &team.id, |team, _| {
            if let Some(attempt) = team.task_mut(*task).and_then(|t| t.attempts.last_mut()) {
                if attempt.state == AttemptState::Delivered && attempt.turn_ended_at.is_none() {
                    attempt.turn_ended_at = Some(*at);
                    attempt.turn_reply = reply.clone();
                    // The gate is open at this tick: the timeout starts here (C2).
                    attempt.report_ready_at = Some(now());
                    attempt.blocked_since = None;
                }
            }
            Ok(())
        }),
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
        Step::Watch { member, phase, activity, last_event_at } => change(store, &team.id, |team, _| {
            let found = team.members.iter_mut().find(|found| found.id == *member).context("unknown member")?;
            found.seen_phase = phase.clone();
            found.seen_activity = activity.clone();
            found.seen_event_at = *last_event_at;
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
            }
            Ok(())
        }),
        Step::Restart { member } => restart_crashed(store, team, member),
        Step::CrashStopped { member } => crash_stopped(store, team, member),
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
            // The report timeout passed with the capture gate holding (A2): the
            // member's last reply becomes its report, marked captured so the
            // member may replace it (A6); only without a reply does the task
            // show No report.
            let attempt = team.task(*task).and_then(|t| t.attempts.last()).cloned();
            let session = attempt.as_ref().and_then(|a| team.member(&a.member)).and_then(|m| m.session.clone());
            if let (Some(attempt), Some(session)) = (attempt, session) {
                if attempt.turn_ended_at.is_none() {
                    // A2/U20: no turn end reached the listing (a quiet session whose
                    // replies were never marked), so there is no recorded reply to
                    // compare with. N6: only a reply written after this attempt was
                    // delivered is its outcome; the session's older last message (an
                    // earlier attempt's report, another task's) is not.
                    if let Ok(Some(reply)) = last_reply_after(&session, attempt.delivered_at.unwrap_or(attempt.sent_at)) {
                        if capture_quiet(store, &team.id, *task, &reply).is_ok() {
                            return Ok(());
                        }
                    }
                } else {
                    match last_reply_of(&session, &attempt.turn_reply) {
                        Ok(Some(reply)) => {
                            if capture_report(store, &team.id, *task, &reply, &attempt.turn_reply).is_ok() {
                                return Ok(());
                            }
                        }
                        Ok(None) => {
                            // A newer reply: the turn ended again, so record that one
                            // (A2). The next tick captures it once its timeout passed.
                            change(store, &team.id, |team, _| {
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
                            })?;
                            return Ok(());
                        }
                        Err(_) => {}
                    }
                }
            }
            change(store, &team.id, |team, events| {
            let role = team.task(*task).and_then(|t| t.attempts.last()).map(|a| team.role_of(&a.member)).unwrap_or_default();
            if let Some(attempt) = team.task_mut(*task).and_then(|t| t.attempts.last_mut()) {
                if attempt.state == AttemptState::Delivered && !attempt.no_report {
                    attempt.no_report = true;
                    attempt.report_ready_at = None;
                    events.push(Event::new("no_report", json!({"member":role,"task":task,"attempt":attempt.n})));
                }
            }
            Ok(())
            })
        }
    }
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
pub(crate) fn tick(store: &mut Store, host: &str) -> anyhow::Result<bool> {
    let teams: Vec<Team> = store
        .teams(false, 10_000)?
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
    if teams.is_empty() {
        return Ok(false);
    }
    let output = match listing(|| hgs("", &["ls", "--json", "--no-archives"], None, LIST_TIMEOUT)) {
        Some(result) => result?,
        // The previous session listing may still run: skip this tick, never
        // overlap two `hgs ls` children (U2).
        None => return Ok(true),
    };
    anyhow::ensure!(output.status.success(), "hgs ls failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    let listing: Value = serde_json::from_slice(&output.stdout).context("invalid hgs ls output")?;
    let sessions = seen(&listing);
    let listed = listed_hosts(&listing);
    for team in &teams {
        if let Err(error) = take_outbox(store, team, &sessions) {
            eprintln!("hgs team worker: {}: outbox: {error}", team.name);
        }
    }
    let teams: Vec<Team> = teams.iter().filter_map(|t| store.load(&t.id).ok().flatten()).collect();
    for team in &teams {
        for step in steps_with(team, &sessions, now(), Some(&listed)) {
            if let Err(error) = run_step(store, team, &step, host) {
                eprintln!("hgs team worker: {}: {step:?}: {error}", team.name);
            }
        }
        // U62 (#717): the team's test queue. An empty queue is a no-op (no load
        // read); a read is at most once per 30 s, and results become notices.
        // U62 (#760): the merge queue first, so its tests run in the same tick.
        if let Err(error) = merge_queue_tick(store, &team.id, now()) {
            eprintln!("hgs team worker: {}: merge queue: {error}", team.name);
        }
        if let Err(error) = test_queue_tick(store, &team.id, now()) {
            eprintln!("hgs team worker: {}: test queue: {error}", team.name);
        }
    }
    Ok(true)
}

/// N13: the folder the worker reads a member's outbox from is the one the
/// member's sandbox writes to: the folder the store records for it, once that
/// agrees with its live session. An adopted session's folder is its own, never
/// a worktree name computed from it; a shared member may work in a subfolder of
/// the team's checkout, where its sandbox writes.
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
            if request["op"] == "run" {
                match start_run(store, team, member, folder, &request, spawn) {
                    // A refusal is answered now; a claim needs no answer - the
                    // runner writes it, and a claim that could not start stays
                    // queued for the next tick.
                    Ok(Some(outcome)) => {
                        if let Err(error) = super::outbox::settle(folder, &request, &outcome) {
                            eprintln!("hgs team worker: {}: outbox {}: {error}", team.name, path.display());
                        }
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
        }
    }
    Ok(())
}

/// N1: the branch the lead's own folder is on: what a member merges. It is read
/// from the team's folder, never built into Zerus, so another project gets its
/// own branch.
fn lead_branch(team: &Team) -> anyhow::Result<String> {
    anyhow::ensure!(!team.folder.is_empty(), "The team has no folder to merge from");
    let deadline = Instant::now() + Duration::from_secs(20);
    let out = super::super::worktrees::git(Path::new(&team.folder), &["rev-parse", "--abbrev-ref", "HEAD"], deadline)
        .map_err(|error| anyhow::anyhow!("the lead's branch was not read: {}", error.detail))?;
    let branch = String::from_utf8_lossy(&out).trim().to_owned();
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
    let timeout = Duration::from_secs(super::templates::timeout_for(template));
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
        return Ok(merge_lead(&mine, &team.folder, &branch, timeout));
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
    let mut env: Vec<(&str, String)> = Vec::new();
    if template == "qt-test" && args["shots"].as_str() == Some("true") {
        // U21: the shots go into the member's own folder, never the team's.
        let shots = folder.join(".ci-build").join("shots");
        let _ = fs::create_dir_all(&shots);
        env.push(("ZERUS_TEAM_SHOTS", shots.display().to_string()));
    }
    Ok(super::templates::execute(&plan, folder, timeout, &env))
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
        "timed_out":outcome.timed_out,"error":outcome.error})
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
    // U30: the label and command show the member's session as Building.
    let command = std::iter::once(plan.program.as_str()).chain(plan.argv.iter().map(String::as_str)).collect::<Vec<_>>().join(" ");
    let marker = json!({"request_id":id,"started":now(),"timeout_secs":super::templates::timeout_for(template),
        "label":template,"command":command});
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

/// C1: a claim whose runner never settled (it crashed, the computer restarted)
/// is answered once its own bound has long passed, so a member never waits on a
/// result that cannot come.
fn settle_stale_runs(folder: &Path, team: &Team) {
    const GRACE_SECONDS: f64 = 120.0;
    for (_, marker) in super::outbox::running_all(folder) {
        let id = marker["request_id"].as_str().unwrap_or_default();
        let started = marker["started"].as_f64().unwrap_or(0.0);
        let bound = marker["timeout_secs"].as_u64().unwrap_or(1800) as f64;
        if !id.is_empty() && now() > started + bound + GRACE_SECONDS {
            let outcome = json!({"status":"refused","request_id":id,
                "error":"the run stopped without a result (the runner ended, this computer restarted, or the command outlived its bound); ask again with a new request_id"});
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
    run_request(&mut store, &folder, &request);
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

/// One queued request, applied as the member's own write (U20). The member's
/// input keys the store receipt, so a request applied twice (a worker that died
/// before it removed the file) is a replay, not a second report. U42: a budget
/// (`expect`) has no attempt; the folder itself proves the member.
fn apply_outbox(store: &mut Store, team: &Team, member: &Member, request: &Value) -> anyhow::Result<Value> {
    let op = request["op"].as_str().unwrap_or_default();
    anyhow::ensure!(matches!(op, "report" | "ask" | "commit" | "reply" | "expect"), "Unknown outbox request {op}");
    anyhow::ensure!(request["team"] == team.id.as_str(), "a request for another team");
    anyhow::ensure!(request["member"] == member.id.as_str(), "the folder belongs to {}", team.name_of(&member.id));
    // The member's own request, unchanged: a replay keys the same store receipt.
    let input = request["input"].clone();
    let request_id = request["request_id"].as_str().unwrap_or_default().to_owned();
    // A request applied before the worker removed its file replays its receipt,
    // even after the attempt closed (the store's own idempotence, U20).
    if let Some(receipt) = store.receipt(&request_id, &input)? {
        return Ok(receipt);
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
    let member_id = member.id.clone();
    let value = store.mutate(Some((&request_id, &input)), &team.id, None, |team, events| {
        ops::apply(team, op, &input, &ops::Actor::Member(member_id.clone()), &BTreeSet::new(), events)
    })?;
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
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let started = fs::metadata(&executable).and_then(|m| m.modified()).ok();
    let config = crate::config::Config::load().map_err(|e| e.to_string())?;
    let host = config.host();
    let mut store = Store::open(true).map_err(|e| e.to_string())?;
    if let Err(error) = recover(&mut store) {
        files.log(&format!("recover failed: {error}"));
    }
    let mut last_work = Instant::now();
    let mut last_reason = String::new();
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
    // N10: Zerus' own children are no session's own - the worker, its runner
    // and the hgs calls it makes must carry no agent marker at all.
    for name in ["TMUX", "HGS_HOST_PANE", "HGS_SESSION", "HGS_RUN_ID", "DSH_SESSION_ID"] {
        command.env_remove(name);
    }
    crate::platform::spawn_detached(&mut command)?;
    Ok(())
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod models_tests;


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
#[path = "tests/dispatcher_lead.rs"]
mod dispatcher_lead;

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
#[path = "tests/dispatcher_tests.rs"]
mod tests;
