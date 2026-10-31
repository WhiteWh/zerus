//! WhiteWh fork: `hgs [@host] team <operation> --json`. Every operation reads
//! one JSON object on stdin; changes carry a canonical `request_id` and replay
//! their first receipt.
use super::super::session_graph::caller_record;
use super::super::*;
use super::model::*;
use super::ops::{self, Actor};
use super::plan;
use super::store::{self, canonical_uuid, Event, Store, PAGE};
use anyhow::{bail, ensure, Context};
use std::collections::BTreeSet;

pub(crate) const USAGE: &str = "usage: hgs [@host] team create|plan|extend|edit-draft|move-phase|move-task|add-to-phase|approve-phase|discard-phase|return-to-draft|reject-phase|undo-reject-phase|rework-phase|finish|approve|answer|commit|discard|pause|resume|stop|archive|status|inbox|list|events|spans|profile|current|report|ask|reply|hold|release|accept|reject|skip-stage|send-back|reassign|take-over|cancel|retry|check|expect|swap-member|escalate|use-last-reply|relaunch|add-member|scale|drain|pause-member|resume-member|remove-member|settings|finding|draft|propose|decide-proposal|next-goal|leftovers|intake-key|intake|waiting|close-phase|integrate-build|start-ahead|full-tests|test|run|templates|safe-action|reports-to|sub-lead --json (JSON stdin) | hgs [@host] team catalog get|set|propose|decide|apply|discard|route|designer --json | hgs team worker [--stop]";

const MUTATIONS: &[&str] = &[
    "plan", "extend", "edit-draft", "move-phase", "move-task", "add-to-phase", "approve-phase", "discard-phase", "return-to-draft", "reject-phase", "undo-reject-phase",
    "rework-phase", "reports-to", "sub-lead", "finish", "approve", "commit", "discard", "pause", "resume", "stop",
    "archive", "report", "ask", "reply", "hold", "release", "accept", "reject", "skip-stage", "send-back", "reassign", "take-over", "cancel",
    "retry", "check", "expect", "swap-member", "use-last-reply", "relaunch", "add-member", "scale", "drain", "pause-member", "resume-member", "remove-member", "settings", "finding",
    "draft", "propose", "close-phase", "integrate-build", "start-ahead", "full-tests",
];

fn text<'a>(input: &'a Value, key: &str) -> &'a str {
    input[key].as_str().unwrap_or_default()
}

/// How many active teams a status or an adoption looks at (C1): the indexed
/// Planning/Running/Paused query, bounded, never every stored team.
const ACTIVE_TEAMS: i64 = 1000;

/// C6: whether this CLI runs in the session of the sub-team's parent lead.
fn parent_lead_calls(store: &Store, team: &Team) -> anyhow::Result<bool> {
    let Some(link) = &team.parent else { return Ok(false) };
    let record = caller_record();
    let dsh = nonempty_env("DSH_SESSION_ID");
    if record.is_none() && dsh.is_none() {
        return Ok(false);
    }
    Ok(parent_lead_proven(team, store.load(&link.team)?.as_ref(), record.as_ref(), dsh.as_deref()))
}

/// The caller is the parent team's lead and no member of the sub-team itself:
/// proven by its process record, or for a dsh lead by its conversation.
fn parent_lead_proven(team: &Team, parent: Option<&Team>, record: Option<&Value>, dsh: Option<&str>) -> bool {
    let (Some(link), Some(parent)) = (&team.parent, parent) else { return false };
    let (here, there) = match record {
        Some(record) => (calling_member(team, record), calling_member(parent, record)),
        None => (dsh_member(team, dsh.unwrap_or_default()), dsh_member(parent, dsh.unwrap_or_default())),
    };
    link.team == parent.id && here.is_none() && there.as_deref() == Some(parent.lead.as_str())
}

/// The member whose session runs this CLI, proven by process ancestry. U54: a
/// renamed session stays the member: the run id and the conversation id match
/// as well as the stored name, so `hgs rename` never drops a member.
fn calling_member(team: &Team, record: &Value) -> Option<String> {
    let name = record["name"].as_str()?;
    let run = record["run_id"].as_str().unwrap_or_default();
    let conversation = record["conversation_id"].as_str().unwrap_or_default();
    team.members
        .iter()
        .find(|member| {
            member.session.as_ref().is_some_and(|session| {
                session.host.is_empty()
                    && (session.name == name
                        || (!run.is_empty() && session.run_id == run)
                        || (!conversation.is_empty() && session.conversation_id == conversation))
            })
        })
        .map(|member| member.id.clone())
}

/// U54: the team record follows a session's rename. A caller proven by its run
/// or conversation under a new name updates the member's stored session name
/// (and fills the identity it was missing), so every later command matches it
/// by name again.
fn follow_session_rename(store: &mut Store, team_id: &str, record: Option<&Value>) -> anyhow::Result<()> {
    let Some(record) = record else { return Ok(()) };
    let name = record["name"].as_str().unwrap_or_default();
    let run = record["run_id"].as_str().unwrap_or_default();
    let conversation = record["conversation_id"].as_str().unwrap_or_default();
    if name.is_empty() || (run.is_empty() && conversation.is_empty()) {
        return Ok(());
    }
    let Some(team) = store.load(team_id)? else { return Ok(()) };
    let matches = |session: &super::model::SessionRef| {
        session.host.is_empty()
            && session.name != name
            && ((!run.is_empty() && session.run_id == run)
                || (!conversation.is_empty() && session.conversation_id == conversation))
    };
    if !team.members.iter().any(|member| member.session.as_ref().is_some_and(matches)) {
        return Ok(());
    }
    store.mutate(None, team_id, None, |team, events| {
        let roles: Vec<(String, String)> = team.members.iter().map(|member| (member.id.clone(), team.role_of(&member.id))).collect();
        for member in &mut team.members {
            let Some(session) = member.session.as_mut() else { continue };
            if session.host.is_empty()
                && session.name != name
                && ((!run.is_empty() && session.run_id == run)
                    || (!conversation.is_empty() && session.conversation_id == conversation))
            {
                let old = session.name.clone();
                session.name = name.to_owned();
                if session.run_id.is_empty() {
                    session.run_id = run.to_owned();
                }
                if session.conversation_id.is_empty() {
                    session.conversation_id = conversation.to_owned();
                }
                let role = roles.iter().find(|(id, _)| *id == member.id).map(|(_, role)| role.clone()).unwrap_or_default();
                events.push(Event::new("member_renamed", json!({"member":role,"from":old,"to":name})));
            }
        }
        Ok(Value::Null)
    })?;
    Ok(())
}

/// U54: the members a bulk swap moves: those of `provider` (any when empty)
/// whose session really runs on `from` (the listing's account, not only the
/// configured one), the lead included.
fn bulk_swap_members(team: &Team, sessions: &[super::dispatcher::Seen], from: &str, provider: &str) -> Vec<String> {
    let native = default_account(from);
    team.members
        .iter()
        .filter(|member| member.state != MemberState::Removed)
        .filter(|member| provider.is_empty() || member.agent == provider)
        .filter(|member| {
            let Some(session) = member
                .session
                .as_ref()
                .filter(|session| session.host.is_empty() && !session.name.is_empty())
            else {
                return false;
            };
            sessions
                .iter()
                .find(|seen| seen.name == session.name && (session.run_id.is_empty() || session.run_id == seen.run_id))
                .is_some_and(|seen| if native { default_account(&seen.account_id) } else { seen.account_id == from })
        })
        .map(|member| member.id.clone())
        .collect()
}

/// The swap-member input of a bulk move: every member that leaves the old
/// account is swapped, the moved ones keeping their session and the fresh ones
/// getting a new one, so an all-fresh move never reads as a single swap.
fn fill_bulk_swap(filled: &mut Value, moved: &[String], fresh: &[String], notes: Vec<Value>, failed: Vec<Value>, to: &str) {
    filled["members"] = json!(moved.iter().chain(fresh).collect::<Vec<_>>());
    filled["fresh"] = json!(fresh);
    filled["notes"] = json!(notes);
    filled["bulk"] = json!(true);
    filled["account"] = json!(to);
    filled["failed"] = json!(failed);
}

/// A switch receipt's status as a word ("rolled_back"), not as JSON.
fn switch_status(receipt: &Value) -> &str {
    receipt["status"].as_str().unwrap_or("unknown")
}

/// U54: "", "native" and "native-<agent>" all name an agent's default
/// account; `hgs ls` names a default-account session "native-claude".
fn default_account(account: &str) -> bool {
    account.is_empty() || account == "native" || account.starts_with("native-")
}

/// U54: a same-agent account move goes through U53's switch, which keeps the
/// session, conversation and attempt; a test replaces the live switch.
#[cfg(test)]
pub(crate) type TestSwitch = fn(&str, &str, &str) -> anyhow::Result<Value>;

fn switch_member_account(name: &str, account: &str, request: &str) -> anyhow::Result<Value> {
    #[cfg(test)]
    if let Some(switch) = support::test_switch() {
        return switch(name, account, request);
    }
    let config = crate::config::Config::load().map_err(|error| anyhow::anyhow!(error.message))?;
    crate::state::switch_account::switch_member(&config, name, account, request).map_err(|error| anyhow::anyhow!(error))
}

/// U54: the fresh-session fallback. The old session is paused once it is idle
/// (never killed; its conversation stays under Saved) and renamed to
/// `<name>-<old account>` so the member's new session of the same name can
/// start. A busy one is left alone (`Err` names the wait).
fn retire_old_session(name: &str, from: &str) -> std::result::Result<String, String> {
    let record = crate::state::storage::read(name)
        .map_err(|error| format!("the old session was not paused: {error}"))?;
    crate::state::lifecycle::pause_one_quiet(&record, None, true)
        .map_err(|_| "waiting for the old session to finish its turn".to_owned())?;
    let label = from.trim();
    let label = if label.is_empty() || label == "native" {
        "native".to_owned()
    } else {
        label.trim_start_matches("native-").to_owned()
    };
    let renamed = format!("{name}-{label}");
    crate::state::rename::dispatch(&[name.to_owned(), renamed.clone()], false)
        .map_err(|error| format!("the old session was paused but not renamed: {error}"))?;
    Ok(format!("the old session was paused and saved as {renamed}"))
}

/// The member whose dsh session carries this conversation id. A dsh session
/// has no process environment of an hgs session; its harness exports the
/// conversation id as `DSH_SESSION_ID` instead (A3, A4).
fn dsh_member(team: &Team, conversation: &str) -> Option<String> {
    if conversation.is_empty() {
        return None;
    }
    team.members
        .iter()
        .find(|member| {
            member.session.as_ref().is_some_and(|session| session.host.is_empty() && session.conversation_id == conversation)
        })
        .map(|member| member.id.clone())
}

/// Who asks. A team session is proven by ancestry; a dsh session by the
/// conversation its harness exports; a report, question or `current` from
/// elsewhere (a member on another computer) is authorized by its attempt id;
/// anything else must say it is you.
fn actor(team: &Team, op: &str, input: &Value) -> anyhow::Result<Actor> {
    let (dsh, env, run) = caller_identities();
    resolve_actor(team, op, input, caller_record().as_ref(), dsh.as_deref(), env.as_deref(), run.as_deref())
}

/// The ambient session identities of this process (H7, U12): its dsh
/// conversation, its hgs session and its hgs run. A test driving the real
/// entry point replaces them, because the test process itself may run inside
/// a session.
pub(crate) type Identities = (Option<String>, Option<String>, Option<String>);

/// The ambient session identities this process carries, or the test's stand-in
/// for them. `crate::state::owner_request` reads the same three.
pub(crate) fn caller_identities() -> Identities {
    #[cfg(test)]
    if let Some(found) = TEST_CALLER_IDENTITIES.with(|value| value.borrow().clone()) {
        return found;
    }
    (nonempty_env("DSH_SESSION_ID"), nonempty_env("HGS_SESSION"), nonempty_env("HGS_RUN_ID"))
}

#[cfg(test)]
thread_local! {
    static TEST_CALLER_IDENTITIES: std::cell::RefCell<Option<Identities>> = const { std::cell::RefCell::new(None) };
}

/// Sets (or clears) this thread's ambient session identities; used with the
/// private state directory so a command test acts as the owner, not as the
/// session that happens to run the tests.
#[cfg(test)]
pub(crate) fn set_test_caller_identities(identities: Option<Identities>) {
    TEST_CALLER_IDENTITIES.with(|value| *value.borrow_mut() = identities);
}

/// The proven identities decide who asks. A stale `HGS_SESSION` (a resumed host
/// leaks another session's into its members) never names the caller: a member
/// is proven by its ancestry, its dsh conversation or its attempt id, in that
/// order (H7). An agent session marker (U12) never falls through to the owner.
fn resolve_actor(
    team: &Team,
    op: &str,
    input: &Value,
    record: Option<&Value>,
    dsh: Option<&str>,
    env: Option<&str>,
    run: Option<&str>,
) -> anyhow::Result<Actor> {
    if let Some(record) = record {
        let member = calling_member(team, record).with_context(|| format!("This session is not a member of team {}", team.name))?;
        return Ok(Actor::Member(member));
    }
    if let Some(member) = dsh_member(team, dsh.unwrap_or_default()) {
        return Ok(Actor::Member(member));
    }
    if matches!(op, "report" | "ask" | "commit") && text(input, "by") != "human" {
        let attempt = text(input, "attempt");
        if let Some(member) = attempt_member(team, attempt) {
            return Ok(Actor::Member(member));
        }
        // A report keeps the old empty-member path for a stale id; a question
        // names the attempt it belongs to.
        if op == "report" {
            return Ok(Actor::Member(String::new()));
        }
        bail!("The attempt is not in this team");
    }
    // U12: a dsh session or any other agent session carries a marker; it can
    // never claim to be the owner, even with "by":"human".
    ensure!(
        dsh.is_none() && run.is_none(),
        "An agent session cannot act as the owner"
    );
    // A process outside every session acts as the owner; a session env that
    // does not own the calling process could name another one (H7).
    ensure!(env.is_none(), "Zerus could not verify the calling session");
    ensure!(text(input, "by") == "human", "Run this from the lead's session, or add \"by\":\"human\" to act as yourself");
    Ok(Actor::Human)
}

/// The member whose attempt id the request names, for a session with no local
/// identity of its own (N2).
fn attempt_member(team: &Team, attempt: &str) -> Option<String> {
    if attempt.is_empty() {
        return None;
    }
    team.tasks
        .iter()
        .flat_map(|task| &task.attempts)
        .find(|found| found.id == attempt)
        .map(|found| found.member.clone())
        // U36: a stage member reports with its stage run's id.
        .or_else(|| super::pipeline::run_member(team, attempt))
}

/// A running team whose every task is decided waits for the lead to extend it
/// or finish it.
fn waiting_for_lead(team: &Team) -> bool {
    team.status == TeamStatus::Running && ops::all_decided(team)
}

/// The phase record still waiting for the owner, if any. A team that is over
/// shows none, even if an old record was left in draft.
fn pending_phase(team: &Team) -> Option<u32> {
    if !matches!(team.status, TeamStatus::Running | TeamStatus::Paused) {
        return None;
    }
    team.phases.iter().find(|phase| phase.state == PhaseState::Draft).map(|phase| phase.n)
}

/// Operations after which new work may be ready for the dispatcher, or a
/// question or answer waits to be delivered.
/// U42 (#587): the lead's owner word for a phase discard or a return to a
/// draft, verified against its own session and spent on that one change. A
/// lead without a word passes through: ops refuses what needs one.
fn check_phase_change_word(
    team: &Team,
    op: &str,
    input: &Value,
    filled: &mut Value,
    caller: &Actor,
    verify: impl Fn(&Team, &Value, u32, super::owner_word::PhaseChange, f64) -> anyhow::Result<super::model::OwnerWord>,
) -> anyhow::Result<()> {
    if !matches!(caller, Actor::Member(member) if *member == team.lead) {
        return Ok(());
    }
    let Some(word) = input.get("owner_word").filter(|word| word.is_object()) else { return Ok(()) };
    let phase = input["phase"].as_u64().and_then(|phase| u32::try_from(phase).ok()).filter(|phase| *phase > 0).context("A phase number is required")?;
    let (change, what) = if op == "discard-phase" {
        (super::owner_word::PhaseChange::Discard, format!("the discard of phase {phase}"))
    } else {
        (super::owner_word::PhaseChange::ToDraft, format!("the return of phase {phase} to a draft"))
    };
    let verified = verify(team, word, phase, change, super::super::now())?;
    if let Some(used) = team.owner_word_used(verified.event) {
        bail!("That word was already used for {used}; ask the owner again");
    }
    filled["owner_word_checked"] = serde_json::to_value(&verified)?;
    filled["owner_word_event"] = json!(verified.event);
    filled["owner_word_change"] = json!(what);
    Ok(())
}

fn starts_worker(op: &str) -> bool {
    matches!(
        op,
        "approve" | "resume" | "extend" | "approve-phase" | "ask" | "reply" | "release" | "expect" | "swap-member"
            // U36: a send-back or a skipped stage leaves work to hand out.
            | "send-back" | "skip-stage"
            // U25: auto-accept may take a draft on enabling or after a rework.
            | "settings" | "rework-phase"
            // U49: the drain finishes when the member owes nothing; the worker
            // pauses its session and removes it.
            | "drain"
            // N6: a created draft team starts its lead and sends it the brief.
            | "draft"
            // U58: a task moved into the running phase is ready to hand out.
            | "move-task"
    )
}

/// Whether the attempt's member was last seen stopped: the dispatcher records
/// it every time the live listing changes (L3).
fn stopped(team: &Team, task: &Task) -> bool {
    task.attempts
        .last()
        .and_then(|attempt| team.member(&attempt.member))
        .and_then(|member| member.seen)
        .is_some_and(|seen| !seen.live)
}

/// A member's queue (A5, spec section 6): its open task first, then the tasks
/// the lead returned to it (they have attempts, are not finished and are Pending,
/// held or not). Nothing else.
fn member_queue(team: &Team, member: &str) -> Vec<Value> {
    let stopped = team.member(member).and_then(|member| member.seen).is_some_and(|seen| !seen.live);
    let entry = |task: &Task| json!({"task": task.id, "phase": ops::attempt_phase(task, stopped)});
    let mut open: Vec<u32> = team
        .tasks
        .iter()
        .filter(|task| task.open_attempt().is_some_and(|attempt| attempt.member == member))
        .map(|task| task.id)
        .collect();
    open.sort_unstable();
    let mut queue: Vec<Value> = open.iter().filter_map(|id| team.task(*id)).map(entry).collect();
    let mut returned: Vec<&Task> = team
        .tasks
        .iter()
        .filter(|task| {
            task.assignee == member && !task.status.finished() && task.status == TaskStatus::Pending && !task.attempts.is_empty()
        })
        .collect();
    returned.sort_by_key(|task| task.id);
    queue.extend(returned.into_iter().map(entry));
    queue
}

/// A7: the task whose captured report holds the member back from new work,
/// shown on the member in status and list; absent when nothing holds it.
fn captured_hold(team: &Team, member: &str) -> Option<Value> {
    let task = team.captured_hold(member)?;
    Some(json!({"task":task,"text":format!("No new work: the captured report on #{task} awaits a decision")}))
}

/// The members of a finished team whose sessions still run, from the live
/// listing (F7). The team's lead and any session that is the lead or a member
/// of a team still active are left out, so the owner cannot stop a working
/// session by mistake. Zerus never stops them.
fn left_running(team: &Team, teams: &[Team], sessions: &[super::dispatcher::Seen]) -> Vec<Value> {
    if !matches!(team.status, TeamStatus::Done | TeamStatus::Stopped) {
        return Vec::new();
    }
    let busy = |session: &SessionRef| {
        teams.iter().filter(|other| matches!(other.status, TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused)).any(|other| {
            let same = |other: &SessionRef| other.host == session.host && other.name == session.name;
            other.member(&other.lead).and_then(|lead| lead.session.as_ref()).is_some_and(same)
                || other.members.iter().any(|member| {
                    member.state != MemberState::Removed && member.session.as_ref().is_some_and(same)
                })
        })
    };
    team.members
        .iter()
        .filter(|member| member.state != MemberState::Removed && member.id != team.lead)
        .filter_map(|member| {
            let session = member.session.as_ref()?;
            let found = sessions.iter().find(|seen| seen.host == session.host && seen.name == session.name)?;
            (found.live && !busy(session)).then(|| json!({"member":member.id,"role":team.role_of(&member.id),"session":session}))
        })
        .collect()
}

/// F1.7: sessions still running for teams that are finished, at most 200.
/// Nothing is stopped here.
fn leftovers(teams: &[Team], sessions: &[super::dispatcher::Seen]) -> Vec<Value> {
    let busy = |session: &SessionRef| {
        teams.iter().filter(|other| matches!(other.status, TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused)).any(|other| {
            let same = |other: &SessionRef| other.host == session.host && other.name == session.name;
            other.member(&other.lead).and_then(|lead| lead.session.as_ref()).is_some_and(same)
                || other.members.iter().any(|member| {
                    member.state != MemberState::Removed && member.session.as_ref().is_some_and(same)
                })
        })
    };
    let mut items = Vec::new();
    for team in teams
        .iter()
        .filter(|team| matches!(team.status, TeamStatus::Done | TeamStatus::Stopped | TeamStatus::Archived))
    {
        for member in team.members.iter().filter(|member| member.state != MemberState::Removed && member.id != team.lead) {
            let Some(session) = member.session.as_ref() else { continue };
            let Some(found) = sessions.iter().find(|seen| seen.host == session.host && seen.name == session.name) else { continue };
            if !found.live || busy(session) {
                continue;
            }
            items.push(json!({"team":team.id,"name":team.name,"member":member.id,"role":team.role_of(&member.id),
                "session":session,"since":member.seen.as_ref().map(|seen| seen.at)}));
        }
    }
    items.truncate(200);
    items
}

/// H3: group dsh members by the host process serving them. A member is
/// `host_shared` when its host serves more than one; status keeps the host's
/// pid, start and members.
fn hosts_value(members: &[(String, Option<Value>)]) -> (Value, BTreeSet<String>) {
    let mut hosts: std::collections::BTreeMap<u32, (f64, Vec<String>)> = std::collections::BTreeMap::new();
    let mut shared = BTreeSet::new();
    for (id, info) in members {
        let Some(info) = info else { continue };
        let Some(pid) = info["pid"].as_u64() else { continue };
        let entry = hosts
            .entry(pid as u32)
            .or_insert_with(|| (info["started"].as_f64().unwrap_or(0.0), Vec::new()));
        entry.1.push(id.clone());
    }
    for (_, ids) in hosts.values() {
        if ids.len() > 1 {
            shared.extend(ids.iter().cloned());
        }
    }
    let value = json!(hosts
        .iter()
        .map(|(pid, (started, ids))| json!({"agent":"dsh","pid":pid,"started":started,"members":ids}))
        .collect::<Vec<_>>());
    (value, shared)
}

/// N31: when an attempt stopped being the task's current one: the time its
/// report came, or the end of its turn when it ended without one. An open
/// attempt has no end yet, so the answers arriving now still belong to it.
fn attempt_end(attempt: &Attempt) -> f64 {
    if attempt.state.open() {
        return f64::INFINITY;
    }
    attempt.report.as_ref().map(|report| report.at).or(attempt.turn_ended_at).unwrap_or(f64::INFINITY)
}

/// #538: a team stores the worker's stage facts (the report's file state, the
/// hold from the report and a closed stage's run), but they never reach a
/// client: the status wire carries the runs' `holds`, `changed` and states.
pub(crate) fn strip_stage_facts(value: &mut Value) {
    let Some(tasks) = value["tasks"].as_array_mut() else { return };
    for task in tasks {
        let Some(task) = task.as_object_mut() else { continue };
        if let Some(attempts) = task.get_mut("attempts").and_then(|attempts| attempts.as_array_mut()) {
            for attempt in attempts {
                if let Some(attempt) = attempt.as_object_mut() {
                    attempt.remove("report_files");
                }
            }
        }
        if let Some(pipeline) = task.get_mut("pipeline_run").and_then(|pipeline| pipeline.as_object_mut()) {
            pipeline.remove("hold_from_report");
            if let Some(runs) = pipeline.get_mut("runs").and_then(|runs| runs.as_array_mut()) {
                for run in runs {
                    if let Some(run) = run.as_object_mut() {
                        run.remove("awaiting_run");
                        run.remove("stalled_member");
                        run.remove("prepare");
                        run.remove("context");
                    }
                }
            }
        }
    }
}

/// The team with its live members when the caller listed sessions (F7).
#[cfg(test)]
pub(super) fn view_with(team: &Team, teams: &[Team], sessions: Option<&[super::dispatcher::Seen]>) -> Value {
    view_with_cancelled(team, teams, sessions, true)
}

/// U58: `hgs team status` hides cancelled tasks and the phases whose tasks are
/// all cancelled unless the caller asks for cancelled work; every other view
/// keeps the whole team.
pub(super) fn view_with_cancelled(team: &Team, teams: &[Team], sessions: Option<&[super::dispatcher::Seen]>, include_cancelled: bool) -> Value {
    let mut value = serde_json::to_value(team).unwrap_or_default();
    // #538: the worker's own stage facts are stored with the team but are not
    // part of the status wire (the desktop and Android read the runs and the
    // hold through `holds`, `changed` and the stages' states).
    strip_stage_facts(&mut value);
    // C7: the plan-health block, with the members starving on the plan now.
    let now = super::super::now();
    // Owner 2026-10-31: every task carries its time estimate and, while it
    // runs, the time spent against it; accepted tasks refresh the history.
    super::estimates::fill_status(&mut value, team, now);
    let mut validation = plan::validate(team, &BTreeSet::new());
    validation.health.starved = plan::starved(team, now);
    for starving in &validation.health.starved {
        validation.health.warnings.push(format!(
            "starved: {} idle {} min without a ready task; Open: {}",
            team.name_of(&starving.member),
            starving.minutes,
            starving.blocking.iter().map(|task| format!("#{task}")).collect::<Vec<_>>().join(", ")
        ));
    }
    value["validation"] = json!(&validation);
    value["health"] = json!(&validation.health);
    // C7 refinement: each phase's own health, at the top and on the phase.
    value["phases_health"] = json!(&validation.phases_health);
    // Section 10.2: the workspace header rows (G9).
    value["phase_summary"] = json!(plan::phase_summary(team));
    if let Some(phases) = value["phases"].as_array_mut() {
        for (phase, value) in team.phases.iter().zip(phases) {
            if let Some(health) = validation.phases_health.get(&phase.n.to_string()) {
                value["health"] = json!(health);
            }
        }
    }
    value["layers"] = json!(plan::layers(&team.tasks));
    value["cycle"] = json!(plan::cycle_tasks(&team.tasks));
    value["ready"] = json!(plan::ready(team));
    value["waiting_for_lead"] = json!(waiting_for_lead(team));
    value["pending_phase"] = json!(pending_phase(team));
    // U59: the plan mode (standard or free) the team's phase runs in, and the
    // free phase's own state for the plan header.
    value["plan_mode"] = json!(team.plan_mode);
    // U62 (#715): the full-run policy and the owner's one-time grant.
    value["full_tests"] = json!(team.settings.full_tests);
    value["full_tests_grant"] = json!(team.full_tests_grant);
    // U59: the lead's mode proposal at the start of planning; the owner's
    // approval chooses one of the two.
    value["plan_mode_offer"] = json!(team.plan_mode_offer);
    value["plan_mode_reason"] = json!(team.plan_mode_reason);
    if team.plan_mode == super::model::PlanMode::Free {
        let n = ops::open_phase(team);
        let tasks: Vec<&super::model::Task> = team.tasks.iter().filter(|task| task.phase == n).collect();
        let running: Vec<u32> = tasks.iter().filter(|task| !task.status.finished()).map(|task| task.id).collect();
        let accepted = tasks.iter().filter(|task| task.status == TaskStatus::Accepted).count();
        value["free_phase"] = json!({
            "phase":n,
            "state":if running.is_empty() { "idle" } else { "open" },
            "running":running,
            "accepted":accepted,
            "accepted_since":team.free_accepted_since_build,
            "last_build_at":team.last_build_at,
            "close_pending":team.free_close_pending,
        });
    }
    // U25: only the running phases' open tasks can make the team Stuck.
    value["stuck_scope"] = json!(plan::stuck_scope(team));
    // U21: the command templates Zerus runs outside a member's sandbox: the
    // built-ins (with the global switch) and the project's additions.
    value["templates"] = super::templates::listed();
    // C5: the plan approval view lists every check command with its expected
    // exit code, so the owner sees what will run and where.
    value["checks"] = json!(team
        .tasks
        .iter()
        .filter(|task| task.is_check())
        .map(|task| {
            let check = task.check.as_ref();
            json!({"task":task.id,"title":task.title,
                "command":check.map(|check| check.command.clone()).unwrap_or_default(),
                "expect_exit":check.map(|check| check.expect_exit).unwrap_or(0),
                "timeout_minutes":check.map(|check| check.timeout_minutes).unwrap_or(0)})
        })
        .collect::<Vec<_>>());
    // Each task's last attempt carries its derived phase (spec section 2). The
    // dispatcher records each member's last seen liveness, so `stopped` is
    // derived here without a listing of status's own (L3).
    if let Some(tasks) = value["tasks"].as_array_mut() {
        let ready: BTreeSet<u32> = plan::ready(team).into_iter().collect();
        let now = super::super::now();
        for (task, value) in team.tasks.iter().zip(tasks) {
            // U37: the task's working time and its waits, one number for every
            // client.
            value["timing"] = super::timing::timing(team, task, now);
            if let Some(phase) = ops::attempt_phase(task, stopped(team, task)) {
                if let Some(attempt) = value["attempts"].as_array_mut().and_then(|attempts| attempts.last_mut()) {
                    attempt["phase"] = json!(phase);
                }
            }
            // C3: every open task says what blocks it and which action frees it.
            // U62 (#760): the task's place in the merge queue.
            if let Some(merge) = super::merge_queue::view(task) {
                value["merge"] = merge;
            }
            // #759: cancelled dependencies no longer hold the task.
            let released = super::plan::cancelled_deps(team, task);
            if !released.is_empty() {
                value["released_by_cancel"] = json!(released);
            }
            if let Some(waits) = super::inbox::waits(team, task, ready.contains(&task.id)) {
                value["waits"] = waits;
            }
            // U36/U37: each stage run's waiting and working span.
            let spans = super::pipeline::spans(task, now);
            if !spans.is_empty() {
                value["stage_spans"] = json!(spans);
            }
            // U36: who may take the open stage (never the implementer).
            if task.status.in_stage() {
                value["stage_candidates"] = json!(super::pipeline::candidates(team, task.id));
            }
        }
    }
    // U17/B1: the answers stored on the member while an attempt ran belong to
    // it; the task card lists them with that attempt (newest shown first).
    if let Some(tasks) = value["tasks"].as_array_mut() {
        for (task, value) in team.tasks.iter().zip(tasks) {
            let Some(attempts) = value["attempts"].as_array_mut() else { continue };
            for (index, attempt) in attempts.iter_mut().enumerate() {
                let Some(recorded) = task.attempts.get(index) else { continue };
                // N31: an answer belongs to the member the attempt ran for, not
                // to whoever the task is assigned to now (a reassigned task).
                let Some(member) = team.member(&recorded.member) else { continue };
                if member.permission_answers.is_empty() {
                    continue;
                }
                // N31: the window ends with the attempt, not at the next one
                // only: a closed attempt never collects later answers.
                let end = task.attempts.get(index + 1).map(|next| next.sent_at).unwrap_or_else(|| attempt_end(recorded));
                let answers: Vec<&Value> = member
                    .permission_answers
                    .iter()
                    .filter(|answer| answer["at"].as_f64().is_some_and(|at| at >= recorded.sent_at && at < end))
                    .collect();
                if !answers.is_empty() {
                    attempt["permissions"] = json!(answers);
                }
            }
        }
    }
    // C4: the reports the owner's attention list shows.
    value["attention"] = json!(super::inbox::attention(team, super::super::now()));
    // Section 9.4: a delegated task shows its sub-team, and the team lists its
    // children with rolled-up counts; each level keeps its own tasks and plan
    // health.
    if let Some(tasks) = value["tasks"].as_array_mut() {
        for (task, value) in team.tasks.iter().zip(tasks) {
            let child = teams.iter().find(|child| {
                child.parent.as_ref().is_some_and(|parent| parent.team == team.id && parent.task == task.id)
            });
            if let Some(child) = child {
                let done = child.tasks.iter().filter(|task| task.status.finished()).count();
                value["subteam"] = json!({"team":child.id,"status":child.status,"done":done,"total":child.tasks.len()});
            }
        }
    }
    value["children"] = json!(teams
        .iter()
        .filter(|child| child.parent.as_ref().is_some_and(|parent| parent.team == team.id))
        .map(|child| {
            let done = child.tasks.iter().filter(|task| task.status.finished()).count();
            json!({"team":child.id,"name":child.name,"status":child.status,"done":done,"total":child.tasks.len()})
        })
        .collect::<Vec<_>>());
    // F12: a draft that cannot open yet says so plainly; the tooltip data names
    // the phase before it and that phase's open tasks.
    if let Some(phases) = value["phases"].as_array_mut() {
        for (phase, value) in team.phases.iter().zip(phases) {
            if phase.state != PhaseState::Draft {
                continue;
            }
            let target = phase.order();
            let mut numbers: Vec<(u32, u32)> =
                team.phases.iter().map(|other| (other.n, other.order())).filter(|(n, _)| *n != phase.n).collect();
            numbers.push((1, 1));
            let Some((previous, _)) = numbers.into_iter().filter(|(_, order)| *order < target).max_by_key(|(_, order)| *order) else {
                continue;
            };
            let drafted = team.phases.iter().any(|other| other.n == previous && other.state.is_draft());
            let open: Vec<u32> =
                team.tasks.iter().filter(|task| task.phase == previous && !task.status.finished()).map(|task| task.id).collect();
            // N40 (spec 2.6): behind an approved phase a draft can be approved
            // now (F12), whatever that phase still runs; only a draft before
            // it holds it.
            if drafted {
                value["waiting"] = json!({
                    "text":"Waiting for the previous phase to be approved",
                    "phase":previous,
                    "open":open
                });
            } else if !open.is_empty() {
                // F12/N40: an approved phase that is still running does not
                // block the draft - the owner may approve it to run alongside
                // (the desktop warns before sending). N6: a barrier phase's
                // tasks still wait for the earlier phase, so its text says so.
                let barrier = team.phases.iter().find(|other| other.n == phase.n).is_some_and(|other| other.barrier);
                value["waiting"] = json!({
                    "text": if barrier {
                        format!("Starts when phase {previous} is finished")
                    } else {
                        format!("Can run alongside phase {previous}")
                    },
                    "phase":previous,
                    "open":open,
                    "alongside":true
                });
            }
        }
    }
    // F12: an approved barrier phase starts only when the earlier phases are
    // finished; it says which one still holds it.
    if let Some(phases) = value["phases"].as_array_mut() {
        for (phase, value) in team.phases.iter().zip(phases) {
            let queued = phase.state == PhaseState::Queued;
            if !(queued || phase.state == PhaseState::Approved && phase.barrier) {
                continue;
            }
            let target = phase.order();
            let order_of = |n: u32| team.phases.iter().find(|other| other.n == n).map_or(n, |other| other.order());
            let mut open: Vec<u32> = team
                .tasks
                .iter()
                .filter(|task| order_of(task.phase) < target && !task.status.finished())
                .map(|task| task.id)
                .collect();
            open.sort_unstable();
            // The owner (2026-10-28): name the phase right ahead of this one in
            // the order, the one it queues behind, not the oldest phase that
            // still has a task open ("Phase 43 starts after phase 40" read
            // wrong while 41 and 42 were also ahead of it).
            // A draft ahead (or one sent back) does not hold it.
            let drafted = |n: u32| team.phases.iter().any(|other| other.n == n && other.state.is_draft());
            let Some(previous) = open
                .iter()
                .map(|id| team.task(*id).map(|task| task.phase).unwrap_or(1))
                .filter(|phase| !drafted(*phase))
                .max_by_key(|phase| order_of(*phase))
            else {
                continue;
            };
            // U25: an accepted phase queued behind a running one.
            let text = if queued { format!("Starts after phase {previous}") } else { format!("Starts when phase {previous} is finished") };
            value["waiting"] = json!({
                "text":text,
                "phase":previous,
                "open":open
            });
        }
    }
    // H3: the DeepSeek host(s) serving members, read only from their host pid
    // files; every member of a host with others is marked shared.
    let host_members: Vec<(String, Option<Value>)> = team
        .members
        .iter()
        .filter(|member| member.agent == "dsh")
        .filter_map(|member| {
            let session = member.session.as_ref()?;
            if !session.host.is_empty() {
                return None;
            }
            Some((member.id.clone(), super::super::dsh::host_of(&session.name)))
        })
        .collect();
    let (hosts, shared) = hosts_value(&host_members);
    value["hosts"] = hosts;
    // Each member's queue: its open task first, then the tasks returned to it
    // (A5, spec section 6).
    if let Some(members) = value["members"].as_array_mut() {
        for (member, value) in team.members.iter().zip(members) {
            value["host_shared"] = json!(shared.contains(&member.id));
            value["queue"] = json!(member_queue(team, &member.id));
            if let Some(held) = captured_hold(team, &member.id) {
                value["captured_hold"] = held;
            }
            // #635 N4: the task and stage whose hold keeps this member's folder,
            // so the desktop and Android need not derive the hold from text.
            if let Some((task, stage)) = super::stages::held_by(team, &member.id) {
                value["held_by"] = json!({"task":task,"stage":stage});
            }
            // U36: the stage it has, then the waiting stages it may take.
            value["stage_queue"] = json!(super::stages::queue(team, &member.id));
            // U32: how the member joined on the owner's word, with the quote.
            if let Some(added) = &member.added {
                value["added"] = added.clone();
            }
            // C12: the declared budget, its state and the job it is tied to.
            if let Some(budget) = &member.budget {
                let since = budget.job.as_ref().map(|job| job.since).unwrap_or(budget.declared_at);
                value["budget"] = json!({
                    "label":budget.label,"minutes":budget.minutes,"red_minutes":budget.red_minutes,
                    "since":since,"elapsed_minutes":((now - since).max(0.0) / 60.0).round() as u64,
                    "state":budget.state(now),
                    "command":budget.job.as_ref().map(|job| job.command.clone()).unwrap_or_default()
                });
            }
            if let Some(since) = member.background_since {
                value["background_minutes"] = json!(((now - since).max(0.0) / 60.0).round() as u64);
            }
            // U22/N17: a DeepSeek member's session that still uploads, or whose
            // running host was started before the block, says so on the member
            // and its card.
            if member.agent == "dsh" {
                if let Some(session) = &member.session {
                    if session.host.is_empty() {
                        if let Some(upload) = super::super::dsh::upload_state_for(&session.name).as_object() {
                            for (key, mark) in upload {
                                value[key] = mark.clone();
                            }
                        }
                    }
                }
            }
            // B1: the pending permission request and the answers so far.
            value["question"] = member.permission.clone().unwrap_or(Value::Null);
            value["permission_answers"] = json!(member.permission_answers);
        }
    }
    // Done and Stopped teams say which of their members still run (F7).
    if let Some(sessions) = sessions {
        if matches!(team.status, TeamStatus::Done | TeamStatus::Stopped) {
            value["left_running"] = json!(left_running(team, teams, sessions));
        }
    }
    // F1.6/N37: every member's folder facts, read from the folder the member
    // actually works in (a worktree member's own worktree, not the team folder)
    // and once per distinct folder; a member whose folder is another repository
    // is flagged, not silently accepted. The team's `project` is the GUI's group
    // name (N36), so the folders themselves decide what is outside.
    if let Some(members) = value["members"].as_array_mut() {
        let team_facts = super::dispatcher::folder_facts(&team.folder);
        let mut cache: std::collections::BTreeMap<String, (String, String)> = std::collections::BTreeMap::new();
        cache.insert(team.folder.clone(), team_facts.clone());
        for (member, entry) in team.members.iter().zip(members.iter_mut()) {
            if member.state == MemberState::Removed {
                continue;
            }
            // U54: the real account of the member's session, beside its
            // configured one.
            entry["session_account"] = json!(session_account(member));
            // U56: each member's session phase, activity and minutes without an
            // event, and whether Zerus marks it stuck - status without inspect.
            entry["phase"] = json!(member.seen_phase);
            entry["activity"] = json!(member.seen_activity);
            entry["minutes_without_progress"] =
                json!(member.seen_event_at.map(|at| ((crate::state::now() - at) / 60.0).max(0.0).round() as u64));
            entry["stuck"] = serde_json::to_value(&member.stuck).unwrap_or(Value::Null);
            let folder = super::dispatcher::member_folder_path(team, member).unwrap_or_default();
            // C11: a member on another computer works in a folder there; this
            // computer cannot read it, so its facts stay empty rather than
            // showing a local folder that happens to share the path.
            if !member.computer.is_empty() {
                entry["folder_path"] = json!(folder);
                entry["project"] = json!("");
                entry["branch"] = json!("");
                entry["outside_project"] = json!(false);
                continue;
            }
            let (project, branch) = cache
                .entry(folder.clone())
                .or_insert_with(|| super::dispatcher::folder_facts(&folder))
                .clone();
            let outside = super::dispatcher::outside_project(&team.folder, &folder, &project, &team_facts.0);
            entry["folder_path"] = json!(folder);
            entry["project"] = json!(&project);
            entry["branch"] = json!(branch);
            entry["outside_project"] = json!(outside);
        }
    }
    // D2: the findings summary and the newest page of findings (50).
    let open = |severity: &str| {
        team.findings
            .iter()
            .filter(|finding| finding.severity == severity && finding.unresolved())
            .count()
    };
    let deferred = team.findings.iter().filter(|finding| finding.state == "deferred").count();
    value["findings_summary"] = json!({"open":{"critical":open("critical"),"normal":open("normal"),"later":open("later")},"deferred":deferred});
    value["findings"] = findings_page(team, None);
    // U58: a phase whose tasks are all cancelled reads cancelled; the status
    // hides cancelled tasks and those phases unless the caller asks for them.
    if let Some(phases) = value["phases"].as_array_mut() {
        for (phase, value) in team.phases.iter().zip(phases.iter_mut()) {
            if plan::phase_cancelled(team, phase.n) {
                value["state"] = json!("cancelled");
            }
        }
    }
    if !include_cancelled {
        let cancelled: BTreeSet<u32> = team
            .phases
            .iter()
            .filter(|phase| plan::phase_cancelled(team, phase.n))
            .map(|phase| phase.n)
            .collect();
        if let Some(tasks) = value["tasks"].as_array_mut() {
            let kept: Vec<Value> = team
                .tasks
                .iter()
                .zip(std::mem::take(tasks))
                .filter(|(task, _)| task.status != TaskStatus::Cancelled)
                .map(|(_, value)| value)
                .collect();
            *tasks = kept;
        }
        if let Some(phases) = value["phases"].as_array_mut() {
            let kept: Vec<Value> = team
                .phases
                .iter()
                .zip(std::mem::take(phases))
                .filter(|(phase, _)| !cancelled.contains(&phase.n))
                .map(|(_, value)| value)
                .collect();
            *phases = kept;
        }
        if let Some(summary) = value["phase_summary"].as_array_mut() {
            summary.retain(|phase| phase["n"].as_u64().is_none_or(|n| !cancelled.contains(&(n as u32))));
        }
    }
    value
}

/// D2: one page of findings, newest first, with a stable cursor: `before` is a
/// finding number and the page holds the 50 newest older than it.
fn findings_page(team: &Team, before: Option<u64>) -> Value {
    let number = |id: &str| id.trim_start_matches('f').parse::<u64>().unwrap_or(0);
    let items: Vec<&super::model::Finding> = team
        .findings
        .iter()
        .filter(|finding| before.is_none_or(|before| number(&finding.id) < before))
        .rev()
        .take(50)
        .collect();
    let next_cursor = (items.len() == 50).then(|| items.last().map(|finding| number(&finding.id))).flatten();
    json!({"items":items,"next_cursor":next_cursor})
}

/// U54: the account a member's session really runs on, read from its record -
/// the configured account only while the session is remote or unknown, so the
/// roster never hides where a member really runs.
fn session_account(member: &Member) -> String {
    let Some(session) = member
        .session
        .as_ref()
        .filter(|session| session.host.is_empty() && !session.name.is_empty())
    else {
        return member.account.clone();
    };
    crate::state::storage::read(&session.name)
        .ok()
        .and_then(|record| record["account_id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| member.account.clone())
}

/// A team as the session list shows it: members with their sessions and each
/// task's state, without descriptions, reports or the feed. The list never
/// reads the live session listing: the desktop polls it often (C1).
fn summary(team: &Team, include_cancelled: bool) -> Value {
    let count = |status: TaskStatus| team.tasks.iter().filter(|task| task.status == status).count();
    // U58: the dashboard hides cancelled tasks and the phases whose tasks are
    // all cancelled, unless it asks for them.
    let cancelled: BTreeSet<u32> = team
        .phases
        .iter()
        .filter(|phase| plan::phase_cancelled(team, phase.n))
        .map(|phase| phase.n)
        .collect();
    let members: Vec<Value> = team
        .members
        .iter()
        .filter(|m| m.state != MemberState::Removed)
        .map(|m| {
            let mut value = json!({"id":m.id,"role":team.role_of(&m.id),"agent":m.agent,"model":m.model,"computer":m.computer,
                "account":m.account,"session_account":session_account(m),
                "session":m.session,"state":m.state,"error":m.error,"queue":member_queue(team, &m.id),
                "stage_queue":super::stages::queue(team, &m.id),
                "services":m.services,"question":m.permission});
            if let Some(held) = captured_hold(team, &m.id) {
                value["captured_hold"] = held;
            }
            value
        })
        .collect();
    let tasks: Vec<Value> = team
        .tasks
        .iter()
        .filter(|task| include_cancelled || task.status != TaskStatus::Cancelled)
        .map(|t| {
            let attempt = t.attempts.last();
            json!({"id":t.id,"title":t.title,"status":t.status,"assignee":t.assignee,"phase":t.phase,
                "attempt":attempt.map(|a| json!({"n":a.n,"state":a.state,"no_report":a.no_report,
                    "phase":ops::attempt_phase(t, stopped(team, t))}))})
        })
        .collect();
    // G8: the session row counts the phase in work and every phase, so it needs
    // each task's phase and the phase records (without their change history).
    // U58: a phase whose tasks are all cancelled reads cancelled.
    let phases: Vec<Value> = team
        .phases
        .iter()
        .filter(|p| include_cancelled || !cancelled.contains(&p.n))
        .map(|p| json!({"n":p.n,"order":p.order,"title":p.title,"barrier":p.barrier,
            "state":if cancelled.contains(&p.n) { json!("cancelled") } else { json!(p.state) },"tasks":p.tasks,
            "note":p.note,"rejection":p.rejection}))
        .collect();
    json!({
        "id":team.id,"name":team.name,"status":team.status,"project":team.project,"folder":team.folder,
        "plan_mode":team.plan_mode,
        "lead":team.lead,"members":members,"tasks":tasks,"phases":phases,
        "accepted":count(TaskStatus::Accepted),"submitted":count(TaskStatus::Submitted),
        "in_review":count(TaskStatus::InReview),"in_test":count(TaskStatus::InTest),
        "revision":team.revision,"updated":team.updated,
        "waiting_for_lead":waiting_for_lead(team),"pending_phase":pending_phase(team),
        "stuck_scope":plan::stuck_scope(team),"auto_accept":team.settings.auto_accept,
        // C4: the owner's attention list, also from the polled team list.
        "attention":super::inbox::attention(team, super::super::now()),
        // U57: the lead's open proposals (at most 50), so the lead's session
        // window shows their cards from the polled list.
        "proposals":open_proposals(team),
    })
}

/// U57: the open proposals as the owner's card shows them, without the
/// stored request id their decision replays under.
fn open_proposals(team: &Team) -> Vec<Value> {
    team.proposals
        .iter()
        .filter(|proposal| proposal["state"] == "open")
        .map(|proposal| {
            let mut shown = proposal.clone();
            if let Some(object) = shown.as_object_mut() {
                object.remove("operation_request");
            }
            shown
        })
        .collect()
}

/// N7: stops the lead's session of a discarded draft, when the owner asked for
/// it (spec 3). The session is terminated, not forgotten: it stays in the
/// archive like any stopped session.
fn stop_lead_session(session: &SessionRef) -> anyhow::Result<()> {
    ensure!(!session.name.is_empty(), "The draft's lead has no session to stop");
    #[cfg(test)]
    if RECORD_LEAD_STOPS.with(std::cell::Cell::get) {
        LEAD_STOPS.with(|stops| stops.borrow_mut().push((session.host.clone(), session.name.clone())));
        return Ok(());
    }
    let output = super::dispatcher::hgs(&session.host, &["terminate", &session.name], None, std::time::Duration::from_secs(30))?;
    ensure!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr).trim());
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// Tests record the lead stop instead of terminating a real session.
    static RECORD_LEAD_STOPS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static LEAD_STOPS: std::cell::RefCell<Vec<(String, String)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// N7: from now on, stop_lead_session records its argument instead of running
/// hgs terminate; `lead_stops` reads the record, `record_lead_stops(false)`
/// returns to the real route.
#[cfg(test)]
pub(crate) fn record_lead_stops(record: bool) {
    if record {
        LEAD_STOPS.with(|stops| stops.borrow_mut().clear());
    }
    RECORD_LEAD_STOPS.with(|flag| flag.set(record));
}

#[cfg(test)]
pub(crate) fn lead_stops() -> Vec<(String, String)> {
    LEAD_STOPS.with(|stops| stops.borrow().clone())
}

/// F5: the draft with its revision, unseen fields and preflight. N1/F5: a read
/// by the draft's own lead clears the owner-edited marks - the owner's edit wins
/// only over a lead edit made before the lead read it (spec 3) - so the lead can
/// change those fields after reading.
fn draft_get(input: &Value) -> anyhow::Result<Value> {
    // A lead's read writes the marks away; the owner's read does not. A store
    // the sandbox leaves read-only still reads the draft (the marks stay).
    let mut store = match Store::open(true) {
        Ok(store) => store,
        Err(error) if read_only(&error) => Store::open(false)?,
        Err(error) => return Err(super::store::write_error(error)),
    };
    draft_get_in(&mut store, input)
}

fn draft_get_in(store: &mut Store, input: &Value) -> anyhow::Result<Value> {
    let mut team = store.load(text(input, "team"))?.context("Unknown team")?;
    ensure!(team.status == TeamStatus::Draft, "The team is not a draft");
    let lead_reads = matches!(actor(&team, "draft", input), Ok(Actor::Member(member)) if member == team.lead);
    // N22: the lead must see which fields the owner changed in the answer that
    // clears their marks, or it never learns what the owner edited.
    let mut cleared: Vec<String> = Vec::new();
    let marked = team.draft.as_ref().is_some_and(|draft| !draft.owner_edited.is_empty());
    if lead_reads && marked {
        let marks = team.draft.as_ref().map(|draft| draft.owner_edited.clone()).unwrap_or_default();
        let label = team.role_of(&team.lead);
        let cleared_write = store.mutate(None, &team.id, None, |team, events| {
            if let Some(draft) = team.draft.as_mut() {
                draft.owner_edited.clear();
            }
            events.push(Event::new("draft_read", json!({"by":label})));
            Ok(Value::Null)
        });
        // A read that cannot write (a read-only store) still answers the draft,
        // and then names no cleared field: the marks are still there.
        if cleared_write.is_ok() {
            team = store.load(&team.id)?.context("Unknown team")?;
            cleared = marks;
        }
    }
    let draft = team.draft.clone().unwrap_or_default();
    let validation = super::plan::validate(&team, &BTreeSet::new());
    Ok(json!({"draft":draft,"revision":draft.revision,"unseen":draft.unseen,"owner_edited":cleared,"preflight":validation}))
}

/// F5: the owner starts a draft team; the lead session is adopted when given,
/// otherwise the dispatcher starts it in the team's folder (N6). Nothing exists
/// before this call.
fn draft_start(input: &Value) -> anyhow::Result<Value> {
    let request = text(input, "request_id").to_owned();
    ensure!(super::store::canonical_uuid(&request), "A canonical request_id is required");
    // C2: there is no team yet to prove a member by, so only you start one.
    let (dsh, session, run) = caller_identities();
    owner_without_team(input, caller_record().is_some(), dsh.is_some(), session.is_some(), run.is_some())?;
    let mut store = Store::open(true)?;
    let mut lead = input["lead"].clone();
    if store.receipt(&request, input)?.is_none() && lead["session"].is_object() {
        // C2: the lead's session runs here and leads or serves no other
        // active team, as an adopted member's must (F8).
        if lead.get("id").is_none() {
            lead["id"] = json!("lead");
        }
        let member: Member = serde_json::from_value(lead.clone()).context("Invalid lead")?;
        let adopted = adopted_session(&Team::default(), &member, &store.active_teams(ACTIVE_TEAMS)?, &live_sessions()?)?;
        lead["session"] = serde_json::to_value(&adopted.session)?;
    }
    let value = draft_start_in(&mut store, &request, input, lead)?;
    // N6: the draft's lead is launched and briefed by the dispatcher.
    if let Err(error) = super::dispatcher::ensure_worker() {
        eprintln!("hgs: the team dispatcher did not start: {error}");
    }
    Ok(value)
}

/// C1: the team revision a change must find. The draft, edit-draft,
/// move-phase, discard-phase and extend operations read "revision" as their
/// own (the draft's, or the phase numbers they were made against).
fn expected_revision(op: &str, input: &Value) -> Option<u64> {
    if matches!(op, "draft" | "edit-draft" | "move-phase" | "discard-phase" | "extend") {
        None
    } else {
        input["revision"].as_u64()
    }
}

/// U12/N4: one owner check for every path that can act as the owner. An agent
/// session carries DSH_SESSION_ID, HGS_SESSION or HGS_RUN_ID and is refused.
fn owner_caller(input: &Value, record: bool, dsh: bool, env: bool, run: bool) -> anyhow::Result<()> {
    ensure!(!record && !dsh && !env && !run, "An agent session cannot act as the owner");
    ensure!(text(input, "by") == "human", "Add \"by\":\"human\" to act as yourself");
    Ok(())
}

/// C2: an operation before any team exists is yours: from a process outside
/// every session, saying so.
fn owner_without_team(input: &Value, record: bool, dsh: bool, env: bool, run: bool) -> anyhow::Result<()> {
    owner_caller(input, record, dsh, env, run)
        .context("Only you start a draft team, not an agent session")
}

/// N4/N3: the catalog's owner paths (set, decide, apply, revert, import,
/// designer) run only on your word from outside every agent session. The
/// returned flag is what `catalog::run` takes: false leaves every owner path
/// refused, true lets that operation check its own contract.
fn catalog_owner(input: &Value) -> anyhow::Result<bool> {
    if text(input, "by") != "human" {
        return Ok(false);
    }
    let (dsh, session, run) = caller_identities();
    owner_caller(input, caller_record().is_some(), dsh.is_some(), session.is_some(), run.is_some())?;
    Ok(true)
}

fn draft_start_in(store: &mut Store, request: &str, input: &Value, lead: Value) -> anyhow::Result<Value> {
    if let Some(receipt) = store.receipt(request, input)? {
        return Ok(receipt);
    }
    let folder = text(input, "folder").trim().to_owned();
    ensure!(!folder.is_empty(), "A draft names its folder");
    let name = text(input, "name").trim().to_owned();
    ensure!(name.chars().count() <= 80, "A name has at most 80 characters");
    let mut lead_value = lead;
    if lead_value.get("id").is_none() {
        lead_value["id"] = json!("lead");
    }
    let mut lead: super::model::Member = serde_json::from_value(lead_value).context("Invalid lead")?;
    ensure!(lead.session.is_some() || !lead.agent.is_empty(), "A draft lead needs an agent or an existing session");
    lead.id = "lead".into();
    // N6/spec 3: the draft's lead starts in the team's folder, never a worktree.
    lead.folder = MemberFolder::Shared;
    lead.directory.clear();
    if lead.role.is_empty() {
        lead.role = "Lead".into();
    }
    lead.state = if lead.session.is_some() { MemberState::Active } else { MemberState::NotCreated };
    let mut team = Team {
        id: uuid::Uuid::new_v4().to_string(),
        name: if name.is_empty() { "New team".into() } else { name },
        folder,
        status: TeamStatus::Draft,
        lead: "lead".into(),
        draft: Some(super::model::Draft { revision: 1, ..Default::default() }),
        revision: 1,
        // N6: the dispatcher starts (or adopts) the lead in the folder and gives
        // it the draft brief once (spec 3).
        brief: Notice::Pending,
        ..Default::default()
    };
    team.members.push(lead);
    let id = team.id.clone();
    store.create(request, input, team, vec![Event::new("team_drafted", json!({"team":id}))])?;
    Ok(json!({"team":id,"revision":1}))
}

/// F7: a done team starts the next goal: the same folder, settings, lead and
/// members, their live sessions adopted, the old team kept.
fn next_goal(input: &Value) -> anyhow::Result<Value> {
    let request = text(input, "request_id").to_owned();
    ensure!(super::store::canonical_uuid(&request), "A canonical request_id is required");
    let mut store = Store::open(true)?;
    let value = next_goal_in(&mut store, &request, input)?;
    // N6/N8: the next team's lead gets its planning brief from the dispatcher.
    if let Err(error) = super::dispatcher::ensure_worker() {
        eprintln!("hgs: the team dispatcher did not start: {error}");
    }
    Ok(value)
}

fn next_goal_in(store: &mut Store, request: &str, input: &Value) -> anyhow::Result<Value> {
    let old = store.load(text(input, "team"))?.context("Unknown team")?;
    let caller = actor(&old, "next-goal", input)?;
    next_goal_as(store, request, input, &caller)
}

fn next_goal_as(store: &mut Store, request: &str, input: &Value, caller: &Actor) -> anyhow::Result<Value> {
    if let Some(receipt) = store.receipt(request, input)? {
        return Ok(receipt);
    }
    let old = store.load(text(input, "team"))?.context("Unknown team")?;
    ensure!(old.status == TeamStatus::Done, "Only a done team starts the next goal");
    // N3/N12: one next goal per done team, and the lead takes the accepted
    // proposal's goal rather than a new one.
    ensure!(old.next.is_empty(), "The next goal was already started for this team");
    let accepted_goal = old.proposals.iter().find_map(|proposal| {
        (proposal["kind"] == "next-goal" && proposal["state"] == "accepted")
            .then(|| proposal["goal"].as_str().unwrap_or_default().trim().to_owned())
    });
    ensure!(
        matches!(caller, Actor::Human) || (matches!(caller, Actor::Member(member) if *member == old.lead) && accepted_goal.is_some()),
        "Only you, or the lead with an accepted next-goal proposal, start the next goal"
    );
    let goal = match caller {
        Actor::Human => text(input, "goal").trim().to_owned(),
        _ => accepted_goal.unwrap_or_default(),
    };
    ensure!(!goal.is_empty(), "A next goal needs its goal");
    ensure!(goal.chars().count() <= 4000, "A goal has at most 4000 characters");
    let name = text(input, "name").trim().to_owned();
    ensure!(name.chars().count() <= 80, "A name has at most 80 characters");
    // N8: the members' live sessions are adopted; one that is gone or serves
    // another active team is not. A listing that fails adopts none (the
    // members start fresh when work is ready) rather than refusing the goal.
    let live = live_sessions().unwrap_or_default();
    let next = super::ops::next_team(&old, &goal, &name, &live, &store.active_teams(ACTIVE_TEAMS)?);
    let next_id = next.id.clone();
    store.create(request, input, next, vec![Event::new("next_goal", json!({"team":next_id,"previous":old.id}))])?;
    store.mutate(None, &old.id, None, |team, events| {
        team.next = next_id.clone();
        events.push(Event::new("next_goal_started", json!({"team":next_id})));
        Ok(Value::Null)
    })?;
    Ok(json!({"status":"planned","team":next_id,"previous":old.id}))
}

/// F8: the owner decides a proposal. An accepted one runs as the owner: an
/// accepted next goal creates the next team, an accepted swap calls the swap
/// operation. A decided proposal is idempotent: a replay returns its state.
fn decide_proposal(input: &Value) -> anyhow::Result<Value> {
    let store_path = text(input, "request_id").to_owned();
    ensure!(super::store::canonical_uuid(&store_path), "A canonical request_id is required");
    let mut store = Store::open(true)?;
    let team = store.load(text(input, "team"))?.context("Unknown team")?;
    let caller = actor(&team, "decide-proposal", input)?;
    let receipt = decide_proposal_in(&mut store, input, &caller)?;
    if receipt["status"] == "accepted"
        && matches!(receipt["kind"].as_str(), Some("add-member") | Some("swap-member") | Some("next-goal"))
    {
        // N4: an accepted membership change may bind or launch a session, and a
        // next goal's lead gets its brief; the dispatcher must run.
        if let Err(error) = super::dispatcher::ensure_worker() {
            eprintln!("hgs: the team dispatcher did not start: {error}");
        }
    }
    Ok(receipt)
}

/// C8/N3: the input an accepted add-member or swap-member proposal runs with.
/// A swap's member is its id, or an object with the id and the section 5
/// fields; the operation carries the proposal's own request id, so a retried
/// decision replays it.
fn proposal_input(team: &str, kind: &str, member: &Value, request: &str) -> anyhow::Result<Value> {
    let mut value = json!({"team":team,"request_id":request,"by":"human"});
    if kind == "add-member" {
        ensure!(member.is_object(), "The add-member proposal carries its member");
        value["member"] = member.clone();
        return Ok(value);
    }
    let id = member.as_str().or_else(|| member["id"].as_str()).unwrap_or_default();
    ensure!(!id.is_empty(), "The swap-member proposal names its member");
    value["member"] = json!(id);
    for key in ["agent", "model", "effort", "account", "session"] {
        if let Some(field) = member.get(key).filter(|field| !field.is_null()) {
            value[key] = field.clone();
        }
    }
    Ok(value)
}

/// The tree-budget key of one session, as `tree_session_count` counts it: the
/// live session when there is one, else the member that still needs it.
fn session_key(team: &str, member: &str, session: Option<(&str, &str)>) -> String {
    match session {
        Some((host, name)) => format!("session:{host}:{name}"),
        None => format!("member:{team}:{member}"),
    }
}

/// How many new sessions a change binds, for the tree budget (section 9.2).
fn added_sessions(team: &Team, op: &str, filled: &Value) -> usize {
    match op {
        // N26: an added member without a session is launched once work is
        // ready, so it counts now: leaving it out let a 17th session start at
        // 16/16. A member with an adopted session adds that session.
        "add-member" => 1,
        // U49: scale binds one session per member it adds.
        "scale" => filled["count"].as_u64().unwrap_or(0) as usize,
        "swap-member" => usize::from(
            filled["session"].is_object() && team.member(text(filled, "member")).is_none_or(|member| member.session.is_none()),
        ),
        // N26: a plan replaces the stored roster; only the members whose
        // session is not already counted are added, so a same-size re-plan fits.
        "plan" => {
            let old: BTreeSet<String> = team
                .members
                .iter()
                .filter(|member| member.id != team.lead && member.state != MemberState::Removed)
                .map(|member| {
                    session_key(&team.id, &member.id, member.session.as_ref().map(|session| (session.host.as_str(), session.name.as_str())))
                })
                .collect();
            let new: BTreeSet<String> = filled["members"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|member| {
                    let id = member["id"].as_str().unwrap_or_default();
                    let host = member["session"]["host"].as_str().unwrap_or_default();
                    let name = member["session"]["name"].as_str().unwrap_or_default();
                    if name.is_empty() { session_key(&team.id, id, None) } else { session_key(&team.id, id, Some((host, name))) }
                })
                .collect();
            new.difference(&old).count()
        }
        _ => 0,
    }
}

/// C8: an accepted proposal runs as your own change would: the same session
/// adoption, tree budget and request receipt.
fn owner_change(store: &mut Store, op: &str, value: &Value) -> anyhow::Result<Value> {
    let request = text(value, "request_id").to_owned();
    let team_id = text(value, "team").to_owned();
    let mut filled = value.clone();
    adopt_existing_sessions(store, &request, value, op, &mut filled)?;
    let team = store.load(&team_id)?.context("Unknown team")?;
    let tree = tree_around(&team, &mut |id| store.load(id))?;
    ops::ensure_tree_sessions(&team, &tree, added_sessions(&team, op, &filled))?;
    store.mutate(Some((&request, value)), &team_id, None, |team, events| {
        ops::apply(team, op, &filled, &Actor::Human, &BTreeSet::new(), events)
    })
}

/// U29: the second write of an Allowed add-to-phase confirms the addition its
/// warning stored. When the confirmation fails, that warning is declined (best
/// effort), so no addition is left waiting that the Allow meant to settle; a
/// retried Allow asks for a new warning anyway. The confirmation's error is
/// what the owner sees.
fn confirm_allowed_addition(store: &mut Store, team_id: &str, value: &Value) -> anyhow::Result<Value> {
    let request = text(value, "request_id").to_owned();
    let confirmed = store.mutate(Some((&request, value)), team_id, None, |team, events| {
        ops::apply(team, "add-to-phase", value, &Actor::Human, &BTreeSet::new(), events)
    });
    if confirmed.is_err() {
        let decline = json!({"team":team_id,"by":"human","phase":value["phase"],"confirm":value["confirm"],"decline":true});
        let _ = store.mutate(None, team_id, None, |team, events| {
            ops::apply(team, "add-to-phase", &decline, &Actor::Human, &BTreeSet::new(), events)
        });
    }
    confirmed
}

/// N3: the request id an accepted proposal's change runs under. It is stored
/// with the proposal the first time it is needed (proposals made before this
/// field get one then), so a decision retried or interrupted between its writes
/// replays the change instead of running it twice.
fn proposal_operation_request(store: &mut Store, team_id: &str, proposal: &str, team: &Team) -> anyhow::Result<String> {
    if let Some(request) = team.proposals.iter().find(|found| found["id"] == proposal).and_then(|found| found["operation_request"].as_str()) {
        if super::store::canonical_uuid(request) {
            return Ok(request.to_owned());
        }
    }
    let request = uuid::Uuid::new_v4().to_string();
    let mut stored = request.clone();
    store.mutate(None, team_id, None, |team, _| {
        if let Some(found) = team.proposals.iter_mut().find(|found| found["id"] == proposal) {
            match found["operation_request"].as_str() {
                Some(existing) if super::store::canonical_uuid(existing) => stored = existing.to_owned(),
                _ => found["operation_request"] = json!(request),
            }
        }
        Ok(Value::Null)
    })?;
    Ok(stored)
}

fn decide_proposal_in(store: &mut Store, input: &Value, caller: &Actor) -> anyhow::Result<Value> {
    let team_id = text(input, "team").to_owned();
    let team = store.load(&team_id)?.context("Unknown team")?;
    ensure!(matches!(caller, Actor::Human), "Only you decide a proposal");
    let id = text(input, "proposal").to_owned();
    let at = team.proposals.iter().position(|proposal| proposal["id"] == id).context("Unknown proposal")?;
    let state = team.proposals[at]["state"].as_str().unwrap_or_default().to_owned();
    if state != "open" {
        return Ok(json!({"status":state,"proposal":id}));
    }
    // An answer is explicit: a missing or misspelled "accept" never becomes a
    // rejection (RogueDOD, 2026-10-30: "decision":"allow" rejected a proposal).
    let accept = input["accept"].as_bool().context(
        "decide-proposal needs \"accept\": true or false, with \"proposal\" and \"expected_hash\" from the proposal; nothing was decided",
    )?;
    let reason = text(input, "reason").trim().to_owned();
    ensure!(reason.chars().count() <= 2000, "A reason has at most 2000 characters");
    let kind = team.proposals[at]["kind"].as_str().unwrap_or_default().to_owned();
    // U57: the owner's Allow is bound to this id and the proposal's content
    // hash; a changed proposal never applies. An older proposal without a hash
    // stays decidable.
    if let Some(hash) = team.proposals[at]["hash"].as_str().filter(|hash| !hash.is_empty()) {
        // The proposal's receipt names it "hash"; both spellings are read.
        let expected = Some(text(input, "expected_hash")).filter(|value| !value.is_empty()).unwrap_or_else(|| text(input, "hash"));
        ensure!(
            !expected.is_empty() && expected == hash,
            "The proposal changed; reload the card and answer again with its hash"
        );
    }
    if !accept {
        store.mutate(None, &team_id, None, |team, events| {
            if let Some(proposal) = team.proposals.iter_mut().find(|proposal| proposal["id"] == id) {
                proposal["state"] = json!("rejected");
                proposal["decided_at"] = json!(super::super::now());
                proposal["note"] = json!(reason);
            }
            events.push(Event::new("proposal_decided", json!({"proposal":id,"accept":false,"reason":reason})));
            if team.proposals.iter().any(|proposal| proposal["id"] == id && proposal["kind"] == "switch-plan-mode") {
                // U59: a Deny's reason reaches the lead with the team's feed.
                events.push(Event::new("plan_mode_denied", json!({"by":"You","proposal":id,"reason":reason})));
            }
            Ok(Value::Null)
        })?;
        return Ok(json!({"status":"rejected","proposal":id}));
    }
    if kind == "question" {
        // #740: the owner's answer to the lead's question, an offered option or
        // words of the owner's own, reaches the lead's inbox with it.
        let answer = text(input, "answer").trim().to_owned();
        ensure!(!answer.is_empty(), "Answer the question: \"answer\" is an offered option or your own words");
        ensure!(answer.chars().count() <= 4000, "An answer has at most 4000 characters");
        store.mutate(None, &team_id, None, |team, events| {
            if let Some(proposal) = team.proposals.iter_mut().find(|proposal| proposal["id"] == id) {
                proposal["state"] = json!("answered");
                proposal["decided_at"] = json!(super::super::now());
                proposal["answer"] = json!(answer);
                proposal["note"] = json!(answer);
            }
            events.push(Event::new("proposal_decided", json!({"proposal":id,"accept":true,"kind":"question","answer":answer})));
            Ok(Value::Null)
        })?;
        return Ok(json!({"status":"answered","proposal":id,"answer":answer}));
    }
    let member = team.proposals[at]["member"].clone();
    let change = team.proposals[at]["change"].clone();
    let goal = team.proposals[at]["goal"].as_str().unwrap_or_default().to_owned();
    // N3: the change runs under one request id stored with the proposal, so an
    // interrupted or retried decision replays it instead of applying it twice.
    let operation_request = proposal_operation_request(store, &team_id, &id, &team)?;
    let result = match kind.as_str() {
        "add-member" | "swap-member" => {
            let members: Vec<Value> = change["members"].as_array().cloned().unwrap_or_else(|| vec![member.clone()]);
            let mut applied = Vec::new();
            for (index, member) in members.into_iter().enumerate() {
                let request = if index == 0 {
                    operation_request.clone()
                } else {
                    // One request id per member: each change is its own replay.
                    use sha2::{Digest, Sha256};
                    let digest = Sha256::digest(format!("{operation_request}:{index}").as_bytes());
                    let mut bytes = [0u8; 16];
                    bytes.copy_from_slice(&digest[..16]);
                    uuid::Uuid::from_bytes(bytes).to_string()
                };
                let value = proposal_input(&team_id, &kind, &member, &request)?;
                applied.push(owner_change(store, &kind, &value)?);
            }
            json!({"applied":applied.len()})
        }
        "remove-member" => {
            let member_id = member["id"].as_str().or(member.as_str()).unwrap_or_default().to_owned();
            let value = json!({"team":team_id,"request_id":operation_request,"member":member_id,"by":"human"});
            store.mutate(Some((&operation_request, &value)), &team_id, None, |team, events| {
                super::ops::apply(team, "remove-member", &value, &Actor::Human, &BTreeSet::new(), events)
            })?
        }
        "add-to-phase" => {
            let phase = change["phase"].as_u64().context("The add-to-phase proposal carries its phase")?;
            let tasks = change["tasks"].clone();
            // The confirmation's own request id, derived from the stored one: a
            // decision interrupted after it applied replays its receipt.
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(format!("{operation_request}:confirm").as_bytes());
            let mut bytes = [0u8; 16];
            bytes.copy_from_slice(&digest[..16]);
            let confirm_request = uuid::Builder::from_random_bytes(bytes).into_uuid().to_string();
            match store.stored_receipt(&confirm_request)? {
                Some(receipt) => receipt,
                None => {
                    // The first step answers a running phase with a warning and
                    // its pending id; the owner's Allow is the confirmation, so
                    // it is applied at once. #754: an earlier Allow's warning is
                    // asked again (it changed nothing), so a retry never meets an
                    // expired addition; a direct addition replays its receipt.
                    let value = json!({"team":team_id,"request_id":operation_request,"by":"human","phase":phase,"tasks":tasks});
                    let warned = store.stored_receipt(&operation_request)?.is_some_and(|receipt| receipt["status"] == "warning");
                    let first = store.mutate((!warned).then_some((operation_request.as_str(), &value)), &team_id, None, |team, events| {
                        super::ops::apply(team, "add-to-phase", &value, &Actor::Human, &BTreeSet::new(), events)
                    })?;
                    match first["confirm"].as_str().or_else(|| first["pending"].as_str()).filter(|confirm| !confirm.is_empty()) {
                        Some(confirm) => {
                            let value = json!({"team":team_id,"request_id":confirm_request,"by":"human","phase":phase,
                                "tasks":change["tasks"],"confirm":confirm,"confirmation":"add"});
                            confirm_allowed_addition(store, &team_id, &value)?
                        }
                        None => first,
                    }
                }
            }
        }
        "cancel-tasks" => {
            let mut cancelled = 0;
            for id in change["tasks"].as_array().into_iter().flatten().filter_map(Value::as_u64) {
                let request = uuid::Uuid::new_v4().to_string();
                let value = json!({"team":team_id,"request_id":request,"by":"human","task":id});
                store.mutate(Some((&request, &value)), &team_id, None, |team, events| {
                    super::ops::apply(team, "cancel", &value, &Actor::Human, &BTreeSet::new(), events)
                })?;
                cancelled += 1;
            }
            json!({"cancelled":cancelled})
        }
        "discard-phase" => {
            let phase = change["phase"].as_u64().context("The discard-phase proposal carries its phase")?;
            let value = json!({"team":team_id,"request_id":operation_request,"by":"human","phase":phase});
            store.mutate(Some((&operation_request, &value)), &team_id, None, |team, events| {
                super::ops::apply(team, "discard-phase", &value, &Actor::Human, &BTreeSet::new(), events)
            })?
        }
        "next-goal" => {
            ensure!(!goal.is_empty(), "The next-goal proposal carries its goal");
            // N2: it runs as your own next goal, not as the caller re-derived.
            let value = json!({"team":team_id,"goal":goal,"request_id":operation_request,"by":"human"});
            next_goal_as(store, &operation_request, &value, &Actor::Human)?
        }
        "switch-plan-mode" => {
            // U59: the desktop's dialog decides through this path; the switch
            // applies only the mode the proposal names.
            let mode = team.proposals[at]["mode"].as_str().unwrap_or_default().to_owned();
            let asked = team.proposals[at]["reason"].clone();
            store.mutate(None, &team_id, None, |team, events| {
                team.plan_mode = if mode == "free" { super::model::PlanMode::Free } else { super::model::PlanMode::Standard };
                events.push(Event::new("plan_mode_switched", json!({"by":"You","mode":mode,"proposal":id,"reason":asked})));
                Ok(Value::Null)
            })?;
            json!({"status":"switched","plan_mode":mode})
        }
        "switch-setting" => {
            // #752: the owner's Allow sets the one setting the card names.
            let change = team.proposals[at]["change"].clone();
            let asked = team.proposals[at]["reason"].as_str().unwrap_or_default().to_owned();
            store.mutate(None, &team_id, None, |team, events| super::ops::switch_setting(team, &change, &id, &asked, events))?
        }
        other => bail!("Unknown proposal kind {other}"),
    };
    store.mutate(None, &team_id, None, |team, events| {
        if let Some(proposal) = team.proposals.iter_mut().find(|proposal| proposal["id"] == id) {
            proposal["state"] = json!("accepted");
            proposal["decided_at"] = json!(super::super::now());
            proposal["note"] = json!(reason);
        }
        events.push(Event::new("proposal_decided", json!({"proposal":id,"accept":true,"kind":kind})));
        Ok(Value::Null)
    })?;
    Ok(json!({"status":"accepted","proposal":id,"kind":kind,"result":result}))
}

/// N8: a sub-team's create input cannot set an owner-only setting.
fn strip_subteam_owner_settings(settings: &mut Value) {
    if let Some(object) = settings.as_object_mut() {
        for key in ops::OWNER_SETTINGS {
            object.remove(key);
        }
    }
}

/// Who creates a sub-team for a parent task (section 9.1): the assignee's own
/// session, the parent lead on its behalf, or the owner.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ParentCaller {
    Assignee,
    ParentLead,
    Owner,
}

/// The proven caller of a sub-team create: a session record (name and run),
/// else the parent member's dsh conversation, else the owner only when no
/// session env can name another caller and the request says it is you.
fn parent_caller(
    team: &Team,
    assignee: &str,
    record: Option<&Value>,
    dsh: Option<&str>,
    env: Option<&str>,
    run: Option<&str>,
    by: &str,
) -> anyhow::Result<ParentCaller> {
    let resolved = if let Some(record) = record {
        Some(
            calling_member(team, record)
                .with_context(|| format!("This session is not a member of team {}", team.name))?,
        )
    } else {
        dsh_member(team, dsh.unwrap_or_default())
    };
    match resolved.as_deref() {
        Some(id) if id == assignee => Ok(ParentCaller::Assignee),
        Some(id) if id == team.lead => Ok(ParentCaller::ParentLead),
        Some(_) => bail!("A sub-team is created from the parent task's assignee, the parent lead or you"),
        None => {
            // U12: an agent session never falls through to the owner.
            ensure!(dsh.is_none() && run.is_none(), "An agent session cannot act as the owner");
            ensure!(env.is_none(), "Zerus could not verify the calling session");
            ensure!(
                by == "human",
                "A sub-team is created from the parent task's assignee, the parent lead or you"
            );
            Ok(ParentCaller::Owner)
        }
    }
}

/// Loads the tree around a team by id: its ancestors (at most 32 links, enough
/// to find the master) and, from the master, its children down to the master's
/// declared depth, at most 32 per team. A flat team reads nothing extra, and
/// no call ever lists every team (C1).
fn tree_around(team: &Team, load: &mut dyn FnMut(&str) -> anyhow::Result<Option<Team>>) -> anyhow::Result<Vec<Team>> {
    let mut loaded = vec![team.clone()];
    let mut ids: BTreeSet<String> = [team.id.clone()].into_iter().collect();
    let mut current = team.clone();
    let mut links = 0;
    while let Some(parent) = current.parent.clone() {
        if links >= 32 || !ids.insert(parent.team.clone()) {
            break;
        }
        match load(&parent.team)? {
            Some(next) => {
                loaded.push(next.clone());
                current = next;
                links += 1;
            }
            None => {
                ids.remove(&parent.team);
                break;
            }
        }
    }
    // The topmost loaded team is the master; the depth budget is its own.
    let master = loaded
        .iter()
        .rev()
        .find(|candidate| candidate.parent.is_none())
        .cloned()
        .unwrap_or_else(|| team.clone());
    let mut queue: Vec<(Team, u32)> = vec![(master.clone(), 0)];
    while let Some((current, depth)) = queue.pop() {
        if depth >= master.settings.tree_depth.max(1) {
            continue;
        }
        for child in current.children.iter().take(32) {
            if !ids.insert(child.clone()) {
                continue;
            }
            match load(child)? {
                Some(next) => {
                    loaded.push(next.clone());
                    queue.push((next, depth + 1));
                }
                None => {
                    ids.remove(child);
                }
            }
        }
    }
    Ok(loaded)
}

/// Section 9.1: creates a sub-team from its parent task. The assignee's
/// session becomes the sub-team's lead; the caller must be that session, the
/// parent lead, or the owner. Returns (parent team, task) for the link.
fn parent_create(input: &Value, filled: &mut Value) -> anyhow::Result<Option<(String, u32)>> {
    if !input["parent"].is_object() {
        return Ok(None);
    }
    let store = Store::open(false)?;
    let parent = store
        .load(text(&input["parent"], "team"))?
        .context("Unknown parent team")?;
    ensure!(
        matches!(parent.status, TeamStatus::Running | TeamStatus::Paused),
        "Only a running or paused team delegates a sub-team"
    );
    let number = input["parent"]["task"]
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .filter(|number| *number > 0)
        .context("The parent task number is required")?;
    let task = parent.task(number).context("Unknown parent task")?;
    ensure!(!task.status.finished(), "The parent task is already decided");
    // N25: one sub-team per parent task; a second create must not overwrite it.
    ensure!(task.subteam.is_none(), "The parent task already has a sub-team");
    let member = parent
        .member(&task.assignee)
        .with_context(|| format!("The parent task #{} has no assignee", task.id))?
        .clone();
    let session = member
        .session
        .clone()
        .with_context(|| format!("{} has no session to lead a sub-team", member.id))?;
    // N3: the same caller identities every other gate reads (a test replaces
    // them, so an agent marker in the test process never decides the caller).
    let (dsh, env, run) = caller_identities();
    parent_caller(
        &parent,
        &member.id,
        caller_record().as_ref(),
        dsh.as_deref(),
        env.as_deref(),
        run.as_deref(),
        text(input, "by"),
    )?;
    // The assignee's session becomes the sub-team's lead.
    filled["lead"] = json!({
        "id":member.id,"role":if member.role.is_empty(){"Lead"}else{member.role.as_str()},"agent":member.agent,
        "account":member.account,"model":member.model,"effort":member.effort,"permissions":member.permissions,
        "permissions_profile":member.permissions_profile,"computer":member.computer,"folder":member.folder,
        "directory":if member.directory.is_empty(){parent.folder.clone()}else{member.directory.clone()},
        "session":session,"prompt":member.prompt
    });
    filled["parent"] = json!({"team":parent.id,"task":task.id,"lead":parent.lead,"member":member.id});
    if text(filled, "folder").is_empty() {
        filled["folder"] = json!(if member.directory.is_empty() { parent.folder.clone() } else { member.directory.clone() });
    }
    if text(filled, "project").is_empty() {
        filled["project"] = json!(parent.project.clone());
    }
    // N8: a sub-team's create input cannot carry an owner-only setting; the
    // parent's sub-team approval is inherited and the plan stays review.
    if filled["settings"].is_object() {
        let mut settings = filled["settings"].clone();
        strip_subteam_owner_settings(&mut settings);
        filled["settings"] = settings;
    }
    // Section 9.3: a sub-team inherits who approves its plan unless the
    // request says otherwise.
    if filled["settings"].is_null() {
        filled["settings"] = json!({"subteam_approval": parent.settings.subteam_approval});
    } else if filled["settings"].get("subteam_approval").is_none() {
        filled["settings"]["subteam_approval"] = json!(parent.settings.subteam_approval);
    }
    // N27: a sub-team's plan and phases are approved by its parent lead (or
    // you), never auto; its create input cannot turn approval off.
    filled["settings"]["approval"] = json!("review");
    filled["settings"]["phase_approval"] = json!("review");
    Ok(Some((parent.id.clone(), task.id)))
}

/// N25: a create whose child landed but whose parent update failed is repaired
/// on the replay: the reserved child id is already the request id.
fn repair_parent_link(child_id: &str, input: &Value) -> anyhow::Result<()> {
    let Some(parent_id) = input["parent"]["team"].as_str() else { return Ok(()) };
    let Some(number) = input["parent"]["task"].as_u64().and_then(|n| u32::try_from(n).ok()) else { return Ok(()) };
    let mut store = Store::open(true)?;
    if store.load(child_id)?.is_none() {
        return Ok(());
    }
    store.mutate(None, parent_id, None, |parent, events| {
        if let Some(task) = parent.tasks.iter_mut().find(|task| task.id == number) {
            task.subteam = Some(child_id.to_owned());
        }
        if !parent.children.iter().any(|id| id == child_id) {
            parent.children.push(child_id.to_owned());
        }
        events.push(Event::new("subteam_repaired", json!({"team":child_id,"task":number})));
        Ok(Value::Null)
    })?;
    Ok(())
}

/// N25: takes back the parent task's sub-team reservation after a create that
/// failed before the child existed, so a later create can delegate it again.
fn release_reservation(parent_link: &Option<(String, u32)>, child_id: &str) {
    let Some((parent_id, task)) = parent_link else { return };
    let Ok(mut store) = Store::open(true) else { return };
    let number = *task;
    let ours = child_id.to_owned();
    let _ = store.mutate(None, parent_id, None, |parent, _events| {
        if let Some(parent_task) = parent.tasks.iter_mut().find(|candidate| candidate.id == number) {
            if parent_task.subteam.as_deref() == Some(ours.as_str()) {
                parent_task.subteam = None;
            }
        }
        Ok(Value::Null)
    });
}

fn create(input: &Value) -> anyhow::Result<Value> {
    let request = text(input, "request_id");
    ensure!(canonical_uuid(request), "A canonical request_id is required");
    // A replay returns the first receipt; a parent link is repaired first
    // (N25: the child may exist from a create whose parent mutate failed).
    if let Some(receipt) = Store::open(false)?.receipt(request, input)? {
        if input["parent"].is_object() {
            repair_parent_link(request, input)?;
        }
        return Ok(receipt);
    }
    let mut filled = input.clone();
    let parent_link = parent_create(input, &mut filled)?;
    if parent_link.is_none() {
        if let Some(record) = caller_record() {
            // A lead creates its own team: its session is the lead's.
            let lead = &mut filled["lead"];
            ensure!(lead.is_object(), "Describe the lead");
            let session = json!({"name":record["name"],"run_id":record["run_id"],"conversation_id":record["conversation_id"]});
            ensure!(lead["session"].is_null() || lead["session"]["name"] == record["name"], "A session creates only a team it leads");
            lead["session"] = session;
            if lead["agent"].is_null() {
                lead["agent"] = json!(record["agent"]);
            }
        } else {
            // C2/N3: only you start a team from outside the lead's session,
            // through the same gate every owner path uses.
            let (dsh, session, run) = caller_identities();
            owner_caller(input, false, dsh.is_some(), session.is_some(), run.is_some())
                .context("Run this from the lead's session, or add \"by\":\"human\" to act as yourself")?;
        }
    }
    let (team, events, child_id) = {
        let mut child_id = uuid::Uuid::new_v4().to_string();
        if let Some((parent_id, task)) = &parent_link {
            // N25: reserve the parent link before the child exists. The request
            // id is the child id, so a retry after a failed create reuses it;
            // a different concurrent create sees the reservation and is refused.
            child_id = request.to_owned();
            let mut store = Store::open(true)?;
            let pid = parent_id.clone();
            let number = *task;
            store.mutate(None, &pid, None, |parent, events| {
                let parent_task = parent.tasks.iter_mut().find(|candidate| candidate.id == number).context("The parent task is missing")?;
                match &parent_task.subteam {
                    Some(existing) if existing != &child_id => bail!("The parent task already has a sub-team"),
                    _ => parent_task.subteam = Some(child_id.clone()),
                }
                events.push(Event::new("subteam_reserved", json!({"team":child_id,"task":number})));
                Ok(Value::Null)
            })?;
        }
        match ops::create(&child_id, &filled) {
            Ok(created) => (created.0, created.1, child_id),
            Err(error) => {
                // N25: a create refused before the child exists releases the
                // reservation, or no later delegation of the task could happen.
                release_reservation(&parent_link, &child_id);
                return Err(error);
            }
        }
    };
    // Section 9.2: depth, children and tree sessions are checked at create.
    if let Some((parent_id, _)) = &parent_link {
        let check = (|| -> anyhow::Result<()> {
            let store = Store::open(false)?;
            let parent = store.load(parent_id)?.context("The parent team is no longer active")?;
            // Bounded: the parent's chain by id and its children down to the
            // master's depth; this never lists every team.
            let tree = tree_around(&parent, &mut |id| store.load(id))?;
            let master = ops::root_of(&parent, &tree);
            ops::ensure_tree_create(&parent, &master, &tree, &team)
        })();
        if let Err(error) = check {
            // N25: a refused depth, children or session check releases it too.
            release_reservation(&parent_link, &child_id);
            return Err(error);
        }
    }
    let start = team.brief == Notice::Pending;
    // The request's identity is what the caller sent, so a replay matches.
    let receipt = match Store::open(true)?.create(request, input, team.clone(), events) {
        Ok(receipt) => receipt,
        Err(error) => {
            release_reservation(&parent_link, &child_id);
            return Err(error);
        }
    };
    if let Some((parent_id, task)) = parent_link {
        let mut store = Store::open(true)?;
        let child = team.id.clone();
        store.mutate(None, &parent_id, None, |parent, events| {
            if !parent.children.contains(&child) {
                parent.children.push(child.clone());
            }
            if let Some(parent_task) = parent.tasks.iter_mut().find(|candidate| candidate.id == task) {
                parent_task.subteam = Some(child.clone());
            }
            events.push(Event::new("subteam_created", json!({"team":child,"task":task})));
            Ok(Value::Null)
        })?;
    }
    if start {
        if let Err(error) = super::dispatcher::ensure_worker() {
            eprintln!("hgs: the team dispatcher did not start: {error}");
        }
    }
    Ok(receipt)
}

/// The lead (or you) answers a member's question or permission request in the
/// member's session, by its exact id and hash.
/// Whether a native option label is the deny answer (B2). The permission
/// record's `answer` picks its deny option with the same rule (C4).
pub(crate) fn denies(label: &str) -> bool {
    let label = label.trim().to_lowercase();
    label.starts_with("deny") || label.starts_with("no") || label == "reject"
}

fn answer(input: &Value) -> anyhow::Result<Value> {
    let team = Store::open(false)?.load(text(input, "team"))?.context("Unknown team")?;
    // Section 9.6: a sub-team's permission request may be answered one level
    // up by the parent lead, who is proven in the parent team.
    let parent = team.parent.clone().and_then(|link| Store::open(false).ok()?.load(&link.team).ok().flatten());
    let parent_authority = parent.as_ref().is_some_and(|parent| {
        caller_record()
            .as_ref()
            .is_some_and(|record| calling_member(parent, record).as_deref() == Some(parent.lead.as_str()))
    });
    let by = if parent_authority {
        Actor::Member(parent.as_ref().map(|parent| parent.lead.clone()).unwrap_or_default())
    } else {
        actor(&team, "answer", input)?
    };
    ensure!(
        matches!(&by, Actor::Human) || matches!(&by, Actor::Member(id) if *id == team.lead) || parent_authority,
        "Only the lead or the parent lead answers a member's question"
    );
    let member_id = text(input, "member").to_owned();
    let member = team.member(&member_id).context("Unknown member")?;
    // N20: the lead's own permission request goes to the owner, never into the
    // mailbox the lead answers; a lead that answered itself would also decide
    // the whole subagent group it was asked to confirm.
    ensure!(member_id != team.lead || matches!(&by, Actor::Human), "The lead's own permission request is the owner's to answer");
    let session = member.session.clone().context("The member has no session")?;
    let (question, hash) = (text(input, "question_id").to_owned(), text(input, "expected_question_hash").to_owned());
    ensure!(!question.is_empty() && !hash.is_empty() && input["answers"].is_array(), "question_id, expected_question_hash and answers are required");
    let inspected = super::dispatcher::hgs(&session.host, &["inspect", &session.name, "--skip-processes"], None, std::time::Duration::from_secs(45))?;
    ensure!(inspected.status.success(), "{}", String::from_utf8_lossy(&inspected.stderr).trim());
    let record: Value = serde_json::from_slice(&inspected.stdout).context("invalid inspect output")?;
    let card = record["pending_questions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|card| card["question_id"] == question.as_str() && card["question_hash"] == hash.as_str())
        .cloned()
        .context("This question changed or is no longer pending")?;
    let request = json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_run_id":session.run_id,
        "expected_conversation_id":session.conversation_id,"question_id":question,"expected_question_hash":hash,"answers":input["answers"]});
    let output = super::dispatcher::hgs(&session.host, &["answer", &session.name, "--json"], Some(&request), std::time::Duration::from_secs(120))?;
    ensure!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr).trim());
    // B2: a denied permission's text reaches the member as one message, and
    // B1: the lead's or owner's answer is recorded with its reason.
    let selected: Vec<&str> = input["answers"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|answer| answer["selected_option_ids"].as_array().into_iter().flatten())
        .filter_map(|id| id.as_str())
        .collect();
    let deny = card["questions"].as_array().into_iter().flatten().any(|item| {
        item["options"].as_array().into_iter().flatten().any(|option| {
            option["id"].as_str().is_some_and(|id| selected.contains(&id))
                && option["label"].as_str().is_some_and(denies)
        })
    });
    let tool = card["questions"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item["header"].as_str())
        .or_else(|| card["tool"].as_str())
        .unwrap_or_default()
        .to_owned();
    let reason = text(input, "text").trim().to_owned();
    // N5: the answer is recorded even when the denial message cannot be sent.
    let denial_error = if deny && !reason.is_empty() {
        super::dispatcher::send_denial(&session, &tool, &reason).err()
    } else {
        None
    };
    let decision = if deny { "deny" } else { "allow" };
    let who = if matches!(&by, Actor::Human) { "owner" } else { "lead" };
    let mut store = Store::open(true)?;
    store.mutate(None, &team.id, None, |team, events| {
        let member = team.members.iter_mut().find(|found| found.id == member_id).context("unknown member")?;
        // U17: the stored answer keeps the tool and the command the agent asked
        // about, so the desktop can list them beside the decision.
        let (asked_tool, asked_command) = member
            .permission
            .as_ref()
            .map(|pending| {
                (
                    pending["tool"].as_str().unwrap_or_default().to_owned(),
                    pending["command"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .unwrap_or_default();
        if member.permission.as_ref().is_some_and(|pending| pending["question_id"] == question.as_str()) {
            member.permission = None;
        }
        // Answered here: the next question sync must not record it again as
        // answered by unknown.
        if member.pending.as_ref().is_some_and(|pending| pending.question_id == question) {
            member.pending = None;
        }
        member.permission_answers.push(json!({"question_id":question,"decision":decision,"by":who,"at":now(),"reason":reason,
            "tool":asked_tool,"command":asked_command}));
        let excess = member.permission_answers.len().saturating_sub(20);
        member.permission_answers.drain(..excess);
        events.push(Event::new(
            "permission_answered",
            json!({"member":team.role_of(&member_id),"question_id":question,"decision":decision,"by":who}),
        ));
        if let Some(error) = &denial_error {
            events.push(Event::new(
                "permission_denial_failed",
                json!({"member":team.role_of(&member_id),"question_id":question,"error":error.to_string()}),
            ));
        }
        Ok(Value::Null)
    })?;
    Ok(serde_json::from_slice(&output.stdout).unwrap_or_else(|_| json!({"status":"answered"})))
}

/// B4: commits the member's own work in its own folder, on its team branch.
/// Zerus runs git; it never pushes, amends or changes branches.
fn commit(input: &Value) -> anyhow::Result<Value> {
    // L1: a repeated request returns the stored receipt, never a second commit.
    let request = text(input, "request_id");
    ensure!(canonical_uuid(request), "A canonical request_id is required");
    let store = Store::open(false)?;
    if let Some(receipt) = store.receipt(request, input)? {
        return Ok(receipt);
    }
    let team = store.load(text(input, "team"))?.context("Unknown team")?;
    let by = actor(&team, "commit", input)?;
    let Actor::Member(member_id) = by else { bail!("Only a member commits its work") };
    ensure!(member_id != team.lead, "The lead does not commit a member's task");
    drop(store);
    // U20: prove the store is writable before git runs. A member whose sandbox
    // blocks it queues the whole commit instead (the worker runs it), so no
    // commit exists without its attempt recording it.
    Store::open(true)?;
    commit_in_team(&team, &member_id, input)
}

/// The commit itself, as the member's own (B4): the worker applies a queued
/// commit with the same code and checks.
pub(crate) fn commit_in_team(team: &Team, member_id: &str, input: &Value) -> anyhow::Result<Value> {
    let request = text(input, "request_id");
    ensure!(canonical_uuid(request), "A canonical request_id is required");
    if let Some(receipt) = Store::open(false)?.receipt(request, input)? {
        return Ok(receipt);
    }
    ensure!(member_id != team.lead, "The lead does not commit a member's task");
    let member = team.member(member_id).context("Unknown member")?;
    let message = text(input, "message").trim().to_owned();
    ensure!(!message.is_empty() && message.chars().count() <= 4000, "A commit message is required (at most 4000 characters)");
    let folder = super::dispatcher::member_folder_path(team, member)?;
    ensure!(!folder.is_empty() && folder != team.folder, "Zerus commits in the member's own folder, not the team's");
    // U48: with a branch per task, the member's open task's branch is its team
    // branch while that attempt is open.
    let attempt_id = text(input, "attempt");
    let open_task = team.tasks.iter().find(|task| {
        task.open_attempt().is_some_and(|attempt| attempt.member == member_id && (attempt_id.is_empty() || attempt.id == attempt_id))
    });
    let branch = open_task
        .and_then(|task| super::dispatcher::task_branch(team, task.id, member_id))
        .unwrap_or_else(|| format!("team/{}/{}", &team.id[..8.min(team.id.len())], super::dispatcher::tag(&member.id)));
    let paths: Vec<String> = input["paths"].as_array().into_iter().flatten().filter_map(|path| path.as_str()).map(str::to_owned).collect();
    for path in &paths {
        ensure!(!path.is_empty() && !path.starts_with('/') && !path.split(['/', '\\']).any(|part| part == ".."), "A commit path must stay inside the member's folder");
    }
    let result = super::dispatcher::commit_in(&folder, &branch, &message, &paths)?;
    if result["status"] != "committed" {
        // Nothing changed: the receipt still records this request as done.
        Store::open(true)?.mutate(Some((request, input)), &team.id, None, |_, _| Ok(result.clone()))?;
        return Ok(result);
    }
    let sha = result["sha"].as_str().unwrap_or_default().to_owned();
    let files = result["files"].as_u64().unwrap_or(0);
    let attempt_id = text(input, "attempt").to_owned();
    let mut store = Store::open(true)?;
    store.mutate(Some((request, input)), &team.id, None, |team, events| {
        let role = team.role_of(member_id);
        let task = team.tasks.iter_mut().find(|task| {
            task.open_attempt().is_some_and(|attempt| {
                (!attempt_id.is_empty() && attempt.id == attempt_id) || (attempt_id.is_empty() && attempt.member == member_id)
            })
        });
        let Some(task) = task else { bail!("The member has no open attempt to record the commit on") };
        let task_id = task.id;
        let attempt = task.open_attempt_mut().context("the attempt ended")?;
        let n = attempt.n;
        attempt.commits.push(CommitRef { sha: sha.clone(), at: now(), by: "Zerus".into(), files: files as usize });
        let excess = attempt.commits.len().saturating_sub(20);
        attempt.commits.drain(..excess);
        events.push(Event::new("committed", json!({"member":role,"task":task_id,"attempt":n,"sha":sha,"files":files})));
        Ok(Value::Null)
    })?;
    Ok(result)
}

/// What `hgs team current` answers for one member of a team (spec section 5):
/// the live attempt, its phase, its open questions and the report command.
/// Read only.
pub(crate) fn current_view(team: &Team, member: &str) -> anyhow::Result<Value> {
    let Some((task, attempt)) = team
        .tasks
        .iter()
        .rev()
        .filter_map(|task| task.open_attempt().map(|attempt| (task, attempt)))
        .find(|(_, attempt)| attempt.member == member)
    else {
        // U36: a stage member's current attempt is the open stage run given to
        // it; its answer is a verdict, not a report. A waiting stage is not
        // given out yet.
        if let Some((task, run)) = team.tasks.iter().rev().find_map(|task| {
            task.pipeline_run
                .as_ref()
                .and_then(PipelineRun::open_run)
                .filter(|run| run.state == StageState::Delivered && run.member == member)
                .map(|run| (task, run))
        }) {
            return Ok(json!({
                "team": team.id,
                "task": task.id,
                "title": task.title,
                "attempt": run.id,
                "n": run.attempt,
                "stage": run.kind,
                "round": run.round,
                "folder": run.folder,
                "phase": if run.kind == "test" { "in_test" } else { "in_review" },
                "open_questions": [],
                "report": {
                    "request_id": uuid::Uuid::new_v4().to_string(),
                    "team": team.id,
                    "task": task.id,
                    "attempt": run.id,
                    "verdict": "pass|return",
                    "findings": [],
                    "text": "..."
                }
            }));
        }
        bail!("{}", no_attempt_text(team, member));
    };
    let phase = ops::attempt_phase(task, false).unwrap_or("unconfirmed");
    let open_questions: Vec<&str> =
        attempt.questions.iter().filter(|question| question.open()).map(|question| question.id.as_str()).collect();
    Ok(json!({
        "team": team.id,
        "task": task.id,
        "title": task.title,
        "attempt": attempt.id,
        "n": attempt.n,
        "phase": phase,
        "open_questions": open_questions,
        "report": {
            "request_id": uuid::Uuid::new_v4().to_string(),
            "team": team.id,
            "task": task.id,
            "attempt": attempt.id,
            "text": "..."
        }
    }))
}

/// Why a member has no open attempt: its last task and its state, such as
/// `#7 was rejected and is held by the lead` (spec section 5).
fn no_attempt_text(team: &Team, member: &str) -> String {
    let last = team
        .tasks
        .iter()
        .filter(|task| task.assignee == member || task.attempts.iter().any(|attempt| attempt.member == member))
        .max_by_key(|task| task.id);
    let Some(task) = last else { return "You have no open attempt".into() };
    let attempt = task.attempts.iter().rev().find(|attempt| attempt.member == member);
    // U36: a reported task in a stage of its pipeline says which.
    if task.status.in_stage() && attempt.is_some_and(|attempt| attempt.state == AttemptState::Reported) {
        let kind = if task.status == TaskStatus::InTest { "test" } else { "review" };
        return format!("#{} has a report in its {kind} stage", task.id);
    }
    // U46: a hand-out Zerus could not prepare is no delivery the member waits
    // for; the lead or the owner retries it.
    let state = if task.waits.as_ref().is_some_and(|wait| wait.state == "waiting") {
        "is being prepared by Zerus for its next hand-out"
    } else if task.waits.is_some() {
        "could not be prepared by Zerus; the lead or you retry it"
    } else {
        match attempt.map(|attempt| attempt.state) {
            Some(AttemptState::Rejected) if task.hold.is_some() => "was rejected and is held by the lead",
            Some(AttemptState::Rejected) => "was rejected and waits for another hand-out",
            Some(AttemptState::Reported) => "has a report waiting for the lead's decision",
            Some(AttemptState::Revoked) => "was returned to the lead",
            Some(AttemptState::Refused) => "was not delivered; Zerus tries it again",
            Some(AttemptState::Delivering | AttemptState::Delivered | AttemptState::Unconfirmed) => "is open to another attempt",
            None => match task.status {
                TaskStatus::Accepted => "was accepted",
                TaskStatus::Cancelled => "was cancelled",
                TaskStatus::Submitted => "has a report waiting for the lead's decision",
                TaskStatus::Draft => "is a draft",
                _ => "is waiting for a hand-out",
            },
        }
    };
    format!("#{} {state}", task.id)
}

/// The team a `current` request names (N3): only one that still runs
/// (Planning, Running or Paused), never a finished team.
fn current_team<'a>(teams: &'a [Team], named: &str, mut member_of: impl FnMut(&Team) -> Option<String>) -> anyhow::Result<(&'a Team, String)> {
    let mut found: Vec<(&Team, String)> = Vec::new();
    for team in teams {
        if !matches!(team.status, TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused) {
            continue;
        }
        if !named.is_empty() && team.id != named {
            continue;
        }
        if let Some(member) = member_of(team) {
            found.push((team, member));
        }
    }
    match found.as_slice() {
        [] if named.is_empty() => bail!("This session is not a member of a running team"),
        [] => bail!("This session is not a member of team {named}"),
        [only] => Ok((only.0, only.1.clone())),
        many => bail!("This session is a member of {} teams; name one with \"team\"", many.len()),
    }
}

/// U38: the timelines of one task (`task`), or a page of a phase's (`phase`)
/// or the plan's tasks by id after `after`, at most `limit` (50). One bounded,
/// indexed read of the journal serves the page. Read only.
fn spans_view(input: &Value) -> anyhow::Result<Value> {
    use super::spans;
    let store = Store::open(false)?;
    let team = store.load(text(input, "team"))?.context("Unknown team")?;
    let now = now();
    let limit = input["limit"].as_u64().map_or(spans::TASK_PAGE, |n| (n as usize).clamp(1, spans::TASK_PAGE));
    let mut chosen: Vec<&Task> = match input["task"].as_u64() {
        Some(id) => vec![team.task(id as u32).with_context(|| format!("Unknown task #{id}"))?],
        None => {
            let after = input["after"].as_u64().unwrap_or(0);
            let phase = input["phase"].as_u64();
            let mut list: Vec<&Task> = team
                .tasks
                .iter()
                .filter(|task| u64::from(task.id) > after && phase.is_none_or(|phase| u64::from(task.phase) == phase))
                .collect();
            list.sort_by_key(|task| task.id);
            list
        }
    };
    let more = chosen.len() > limit;
    chosen.truncate(limit);
    let (journal, cut) = match spans::window(&team, &chosen, now) {
        Some((from, to)) => store.span_events(&team.id, from, to, spans::EVENT_LIMIT)?,
        None => (Vec::new(), false),
    };
    let holders = spans::holders(&team, now);
    // Owner 2026-10-31: each timeline carries the task's estimate and, while it
    // runs, the time spent against it.
    let history = super::estimates::load();
    let tasks: Vec<Value> = chosen
        .iter()
        .map(|task| {
            let mut value = spans::spans(&team, task, &journal, &holders, now);
            let estimate = super::estimates::estimate_from(&history, &team, task);
            value["estimate_minutes"] = estimate.minutes.map_or(Value::Null, |minutes| json!(minutes));
            value["estimate_source"] = json!(estimate.source);
            value["estimate_text"] = json!(estimate.text());
            if let Some(spent) = super::estimates::spent_minutes(task, now) {
                value["estimate_spent_minutes"] = json!(spent.round());
            }
            if cut {
                let mut unknown: BTreeSet<String> = serde_json::from_value(value["unknown"].clone()).unwrap_or_default();
                unknown.insert("events".into());
                value["unknown"] = json!(unknown);
            }
            value
        })
        .collect();
    let next = if more { chosen.last().map(|task| task.id) } else { None };
    Ok(json!({"team":team.id,"tasks":tasks,"next_cursor":next,"events":{"read":journal.len(),"truncated":cut}}))
}

/// U38: the profile of one task (`task`), a phase (`phase`) or the plan: the
/// losses ranked by time and by tokens (pages of `limit` after `after_time` or
/// `after_tokens`), the planned and the real critical path, per member, role
/// and model the utilization and tokens, and a page of task rows after
/// `after_task`. `compare` [a, b] sets two phases side by side; `tokens`:false
/// reads no session. Read only; one bounded journal read per scope.
fn profile_view(input: &Value) -> anyhow::Result<Value> {
    use super::profile;
    let store = Store::open(false)?;
    let team = store.load(text(input, "team"))?.context("Unknown team")?;
    let now = now();
    let read = input["tokens"].as_bool() != Some(false);
    let page = profile::Page {
        limit: input["limit"].as_u64().map_or(profile::LOSS_PAGE, |n| n as usize),
        after_time: Some(input["after_time"].clone()).filter(Value::is_object),
        after_tokens: Some(input["after_tokens"].clone()).filter(Value::is_object),
        after_task: input["after_task"].as_u64().map_or(0, |n| n as u32),
    };
    // A draft has no timeline yet; cancelled work stays, its time was spent.
    let scope = |phase: Option<u64>| -> Vec<&Task> {
        let mut list: Vec<&Task> = team
            .tasks
            .iter()
            .filter(|task| task.status != TaskStatus::Draft && phase.is_none_or(|phase| u64::from(task.phase) == phase))
            .collect();
        list.sort_by_key(|task| task.id);
        list
    };
    if let Some(pair) = input["compare"].as_array() {
        let phases: Vec<u32> = pair.iter().filter_map(Value::as_u64).filter_map(|n| u32::try_from(n).ok()).collect();
        ensure!(phases.len() == 2, "compare names two phases");
        let first = profile_of(&store, &team, &scope(Some(u64::from(phases[0]))), &profile::Page { limit: 3, ..Default::default() }, read, now)?;
        let second = profile_of(&store, &team, &scope(Some(u64::from(phases[1]))), &profile::Page { limit: 3, ..Default::default() }, read, now)?;
        return Ok(json!({"team":team.id,"compare":profile::compare((phases[0], &first), (phases[1], &second))}));
    }
    let (chosen, label) = match (input["task"].as_u64(), input["phase"].as_u64()) {
        (Some(id), _) => (vec![team.task(id as u32).with_context(|| format!("Unknown task #{id}"))?], json!({"task":id})),
        (None, Some(phase)) => (scope(Some(phase)), json!({"phase":phase})),
        (None, None) => (scope(None), json!({"plan":true})),
    };
    let mut value = profile_of(&store, &team, &chosen, &page, read, now)?;
    value["team"] = json!(team.id);
    value["scope"] = label;
    Ok(value)
}

/// The profile of `chosen` tasks: their timelines from one bounded journal
/// read, the tokens of their spans (at most `profile::TOKEN_READS` reads) and
/// the ranked losses.
fn profile_of(store: &Store, team: &Team, chosen: &[&Task], page: &super::profile::Page, read: bool, now: f64) -> anyhow::Result<Value> {
    use super::{profile, spans};
    let (journal, cut) = match spans::window(team, chosen, now) {
        Some((from, to)) => store.span_events(&team.id, from, to, spans::EVENT_LIMIT)?,
        None => (Vec::new(), false),
    };
    let holders = spans::holders(team, now);
    let timelines: Vec<Value> = chosen.iter().map(|task| spans::spans(team, task, &journal, &holders, now)).collect();
    // #736: every phase's timelines decide whether a member was really idle:
    // work on another phase's task (or one moved between phases) is not idle.
    let all: Vec<Value> = team.tasks.iter().map(|task| spans::spans(team, task, &journal, &holders, now)).collect();
    let asks = profile::asks(&timelines, now);
    let (tokens, capped) = if read {
        profile::read_tokens(team, &asks)
    } else {
        (asks.iter().map(|ask| ((ask.task, ask.part.clone()), None)).collect(), false)
    };
    let mut value = profile::profile(team, chosen, &timelines, &all, &tokens, page, now);
    value["events"] = json!({"read":journal.len(),"truncated":cut});
    value["token_reads"] = json!({"asked":asks.len(),"read":if read { asks.len().min(profile::TOKEN_READS) } else { 0 },"truncated":capped});
    Ok(value)
}

/// #717: the suite's targets from the project's map for the task's diff against
/// the lead branch; an empty list when the project has no map or no change.
fn affected_targets(project: &std::path::Path, team: &str, suite: &str) -> anyhow::Result<Vec<String>> {
    let loaded = crate::test_discipline::load(project, Some(team))?;
    let config = &loaded.config;
    let text = std::fs::read_to_string(project.join(&config.map.file)).map_err(|error| anyhow::anyhow!("{error}"))?;
    let map: crate::test_discipline::map::TestMap = serde_json::from_str(&text)?;
    let durations = crate::test_discipline::durations::Durations::read(&project.join(&config.map.durations)).map_err(|error| anyhow::anyhow!("{error}"))?;
    let changed = super::dispatcher::git_at(&project.to_string_lossy(), &["diff", "--name-only", "teams-v2/lead"]).unwrap_or_default();
    let files: Vec<String> = changed.lines().map(|line| line.trim().to_owned()).filter(|line| !line.is_empty()).collect();
    let selection = crate::test_discipline::affected::select(project, config, &map, &durations, &files, &Default::default(), false);
    Ok(selection.runs.iter().filter(|run| run.provider == suite).flat_map(|run| run.targets.clone()).collect())
}

/// #717: `hgs team test submit|force|results` on the team's test database;
/// #719: `queue` lists what waits and what runs now.
fn test_command(input: &Value) -> anyhow::Result<Value> {
    use super::test_queue::{self, Load, TestStore};
    let team_id = text(input, "team").to_owned();
    let store = Store::open(false)?;
    let team = store.load(&team_id)?.context("Unknown team")?;
    let action = text(input, "action");
    let database = TestStore::open(false)?;
    let project = std::path::PathBuf::from(&team.folder);
    let asked_by = if text(input, "by").is_empty() { input["asked_by"].as_str().unwrap_or_default().to_owned() } else { text(input, "by").to_owned() };
    let load = Load {
        cpu_free: input["cpu_free"].as_u64().map_or(16, |n| n as u32),
        ram_free_mb: input["ram_free_mb"].as_u64().map_or(32_000, |n| n as u32),
        gpu_free: input["gpu_free"].as_bool().unwrap_or(true),
        at_night: input["night"].as_bool().unwrap_or(false),
    };
    let classes = match crate::test_discipline::load(&project, Some(&team_id)) {
        Ok(loaded) => loaded.config.resources,
        Err(_) => Default::default(),
    };
    match action {
        "submit" | "force" => {
            let suite = text(input, "suite");
            ensure!(!suite.is_empty(), "A test request names a suite");
            let mut targets: Vec<String> = input["targets"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
            // #717: without listed targets the task's diff against the lead
            // branch names them through the project's test map.
            if targets.is_empty() {
                if let Ok(found) = affected_targets(&project, &team_id, suite) {
                    targets = found;
                }
            }
            let mut request = test_queue::suite_from_config(&project, Some(&team_id), suite, &targets)?;
            request.id = uuid::Uuid::new_v4().to_string();
            request.team = team_id.clone();
            request.task = input["task"].as_u64().and_then(|id| u32::try_from(id).ok());
            request.asked_by = if asked_by.is_empty() { "You".to_owned() } else { asked_by };
            // #719: the desktop's Force run queues the suite as forced: the worker
            // runs it first at its next tick, never a reused result, and no
            // desktop call waits for the run.
            request.forced = action == "force" || input["forced"].as_bool() == Some(true);
            request.commit = if text(input, "commit").is_empty() {
                super::dispatcher::git_at(&project.to_string_lossy(), &["rev-parse", "HEAD"]).unwrap_or_default()
            } else {
                text(input, "commit").to_owned()
            };
            // #718: the suite's inputs as the map names them now: source hashes,
            // or the Qt test binary and its data; a provider never reused (Rust
            // by default) has none and always runs.
            let loaded = crate::test_discipline::load(&project, Some(&team_id)).map_err(|error| anyhow::anyhow!("{error}"))?;
            let policy = super::test_results::Policy::of(&loaded.config);
            let provider = loaded.config.providers.iter().find(|provider| provider.name == suite).cloned();
            let section = provider.as_ref().and_then(|_| super::test_results::section(&project, &loaded.config, suite));
            let inputs = match (&provider, &section) {
                (Some(provider), Some(section)) => super::test_results::input_hash(&project, provider, section, &request.targets),
                _ => None,
            };
            request.input_hash = inputs.clone().unwrap_or_default();
            let now = super::super::now();
            let mut records = Vec::new();
            let mut reused = None;
            if let (false, Some(hash)) = (request.forced, inputs.as_ref()) {
                if let Some(source) = database.last_green(&team_id, &request.suite, hash)? {
                    reused = Some(database.write_reused(&request, &source, now)?);
                }
            }
            if let Some(record) = &reused {
                records.push(record.clone());
            } else {
                database.enqueue(&request)?;
            }
            // #718: a suite over its size budget in test lines is a lead item.
            let size = match (&provider, &section) {
                (Some(provider), Some(section)) => {
                    let budget = super::test_results::lines_budget(&loaded.config, provider);
                    let lines = super::test_results::test_lines(&project, section);
                    (budget > 0 && lines > budget as usize).then_some((lines, budget))
                }
                _ => None,
            };
            if reused.is_none() && action == "force" {
                let queued = database.queued(&team_id)?;
                let chosen = test_queue::schedule(&queued, &classes, &load);
                records = test_queue::run_due(&database, &project, &chosen, now, &policy)?;
            }
            let estimate = super::test_results::full_estimate(&project, &loaded.config);
            if !records.is_empty() || size.is_some() {
                let mut store = Store::open(true)?;
                store.mutate(None, &team_id, None, |team, events| {
                    super::test_results::apply(team, &records, &database, &policy, now, estimate)?;
                    if let Some((lines, budget)) = size {
                        super::test_results::raise(team, "size_budget", suite, now, format!(
                            "{suite} has {lines} test lines, over its size budget of {budget}: split it by area or raise budget_lines in .zerus/tests.toml."
                        ), 0);
                    }
                    for record in &records {
                        events.push(Event::new("test_result", json!({"suite":record.suite,"status":record.status,"record":record.id,"asked_by":record.asked_by})));
                    }
                    Ok(Value::Null)
                })?;
            }
            if let Some(record) = reused {
                return Ok(json!({"status":"reused","record":record,"suite":request.suite}));
            }
            if action == "submit" {
                return Ok(json!({"status":"queued","request":request,"suite":request.suite}));
            }
            Ok(json!({"status":"ran","suite":request.suite,"results":records}))
        }
        // #718: the lead's or owner's answer to a test item, or a full run done.
        "dismiss" | "full-done" => {
            let mut store = Store::open(true)?;
            let caller = actor(&team, "test", input)?;
            ensure!(matches!(caller, Actor::Human) || caller.is_lead(&team), "Only the lead or you settle test items");
            store.mutate(None, &team_id, None, |team, _| super::test_results::settle(team, action, input, super::super::now()))
        }
        "results" => {
            let limit = input["limit"].as_u64().map_or(20, |n| n as usize);
            let before = input["before"].as_f64();
            // #719: a task's drawer reads its own runs; the profiler its window.
            let task = input["task"].as_u64().and_then(|id| u32::try_from(id).ok());
            let records = database.results(Some(&team_id), before, limit, task, input["from"].as_f64())?;
            let next = records.last().map(|record| record.at);
            Ok(json!({"status":"ok","items":records,"next_cursor":next}))
        }
        "queue" => Ok(json!({"status":"ok","items":database.pending(&team_id)?})),
        other => anyhow::bail!("Unknown test action {other}; use submit, force, queue, results, dismiss or full-done"),
    }
}

/// The member's live attempt from its own session; `team` is optional when the
/// session belongs to one team (spec section 5). Read only.
fn current(input: &Value) -> anyhow::Result<Value> {
    let named = text(input, "team");
    // A member on another computer has no local identity: its attempt id names
    // it, as report does (N2).
    let attempt = text(input, "attempt");
    if !attempt.is_empty() {
        let teams = Store::open(false)?.active_teams(ACTIVE_TEAMS)?;
        let found = teams.iter().find_map(|team| {
            if !named.is_empty() && team.id != named {
                return None;
            }
            if !matches!(team.status, TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused) {
                return None;
            }
            team.tasks
                .iter()
                .flat_map(|task| &task.attempts)
                .find(|found| found.id == attempt)
                .map(|found| (team, found.member.clone()))
                .or_else(|| {
                    // U36: a stage run id names its member the same way.
                    team.tasks
                        .iter()
                        .filter_map(|task| task.pipeline_run.as_ref())
                        .flat_map(|run| &run.runs)
                        .find(|run| run.id == attempt && !run.member.is_empty())
                        .map(|run| (team, run.member.clone()))
                })
        });
        let (team, member) = found.with_context(|| format!("No attempt {attempt} runs in a team"))?;
        return current_view(team, &member);
    }
    let record = caller_record();
    let conversation = nonempty_env("DSH_SESSION_ID").unwrap_or_default();
    if record.is_none() {
        ensure!(!conversation.is_empty(), "hgs team current runs in a member's session");
    }
    // Only the teams that still run, from the indexed query (N3, C1).
    let teams = Store::open(false)?.active_teams(ACTIVE_TEAMS)?;
    let (team, member) = current_team(&teams, named, |team| match &record {
        Some(record) => calling_member(team, record),
        None => dsh_member(team, &conversation),
    })?;
    current_view(team, &member)
}

/// Whether a status request reads the live listing: only a finished team, for
/// the members it left running (F7). The desktop polls the watched team's
/// status every 3 s, so a running team's status never lists sessions (C1, L3).
fn status_lists_sessions(status: TeamStatus) -> bool {
    matches!(status, TeamStatus::Done | TeamStatus::Stopped)
}

/// The live sessions of this computer and its peers, as the dispatcher reads
/// them (F7, F8).
fn live_sessions() -> anyhow::Result<Vec<super::dispatcher::Seen>> {
    #[cfg(test)]
    if let Some(found) = LIVE_SESSIONS.with(|sessions| sessions.borrow().clone()) {
        return Ok(found);
    }
    let output = super::dispatcher::hgs("", &["ls", "--json", "--no-archives"], None, std::time::Duration::from_secs(15))?;
    ensure!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr).trim());
    let listing: Value = serde_json::from_slice(&output.stdout).context("invalid hgs ls output")?;
    Ok(super::dispatcher::seen(&listing))
}

#[cfg(test)]
thread_local! {
    /// N3/N4: a test's own live listing instead of a real `hgs ls`.
    static LIVE_SESSIONS: std::cell::RefCell<Option<Vec<super::dispatcher::Seen>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn live_sessions_override(sessions: Option<Vec<super::dispatcher::Seen>>) {
    LIVE_SESSIONS.with(|slot| *slot.borrow_mut() = sessions);
}

/// How long a finished team's status reuses one live listing (C1, NEW-2): the
/// desktop polls the watched team every 3 s, and a full listing may take 15 s.
const LISTING_REUSE: f64 = 30.0;

/// The live sessions for a finished team's left_running, from a listing at
/// most LISTING_REUSE old: polling a Done team never runs hgs ls each time.
fn recent_sessions() -> anyhow::Result<Vec<super::dispatcher::Seen>> {
    let path = root().join("teams/left-running-listing.json");
    let now = super::super::now();
    if let Some(cached) = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()) {
        if cached["at"].as_f64().is_some_and(|at| at <= now && now - at < LISTING_REUSE) {
            return Ok(super::dispatcher::seen(&cached["listing"]));
        }
    }
    let output = super::dispatcher::hgs("", &["ls", "--json", "--no-archives"], None, std::time::Duration::from_secs(15))?;
    ensure!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr).trim());
    let listing: Value = serde_json::from_slice(&output.stdout).context("invalid hgs ls output")?;
    // Best effort: a failed write only means the next poll lists again.
    let _ = std::fs::write(&path, serde_json::to_vec(&json!({"at":now,"listing":listing})).unwrap_or_default());
    Ok(super::dispatcher::seen(&listing))
}

/// Where a member works when it adopts an existing session (F8): the session's
/// own folder, stored as `directory`, and whether it keeps its worktree.
#[derive(Debug)]
struct Adopted {
    session: SessionRef,
    directory: String,
    worktree: bool,
}

/// The same git common dir: this folder already belongs to the team's
/// repository, so a worktree member may adopt the session in it (F8).
fn same_repository(folder: &str, team_folder: &str) -> bool {
    let common = |folder: &str| -> Option<std::path::PathBuf> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let output = super::super::worktrees::git(
            std::path::Path::new(folder),
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            deadline,
        )
        .ok()?;
        let text = String::from_utf8(output).ok()?;
        let text = text.trim();
        (!text.is_empty()).then(|| std::path::PathBuf::from(text))
    };
    match (common(folder), common(team_folder)) {
        (Some(folder), Some(team)) => std::fs::canonicalize(folder)
            .ok()
            .zip(std::fs::canonicalize(team).ok())
            .is_some_and(|(folder, team)| folder == team),
        _ => false,
    }
}

/// Checks the existing session a member adopts (F8): it must run on the
/// member's computer, be no lead or member of another active team, and a
/// worktree member needs it inside the team's repository. The member then
/// works in the session's own folder.
fn adopted_session(team: &Team, member: &Member, teams: &[Team], sessions: &[super::dispatcher::Seen]) -> anyhow::Result<Adopted> {
    let session = member.session.as_ref().context("A member has no session")?;
    ensure!(!session.name.is_empty(), "An existing session needs a name");
    let host = if session.host.is_empty() { member.computer.clone() } else { session.host.clone() };
    let here = if host.is_empty() { "this computer".to_owned() } else { host.clone() };
    ensure!(
        host == member.computer,
        "{} runs on {}, not on {}",
        session.name,
        here,
        if member.computer.is_empty() { "this computer" } else { member.computer.as_str() }
    );
    let found = sessions
        .iter()
        .find(|seen| seen.host == host && seen.name == session.name && (session.run_id.is_empty() || seen.run_id == session.run_id))
        .with_context(|| format!("{} is not running on {here}", session.name))?;
    ensure!(found.live, "{} is not running on {here}", session.name);
    ensure!(!found.cwd.is_empty(), "{} has no folder", session.name);
    // It may not be the lead or a member of another team that is still active.
    for other in teams.iter().filter(|other| other.id != team.id && matches!(other.status, TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused)) {
        if other
            .member(&other.lead)
            .and_then(|lead| lead.session.as_ref())
            .is_some_and(|lead| lead.name == session.name && lead.host == host)
        {
            bail!("{} is the lead of team {}", session.name, other.name);
        }
        if other.members.iter().any(|member| {
            member.state != MemberState::Removed
                && member.id != other.lead
                && member.session.as_ref().is_some_and(|member| member.name == session.name && member.host == host)
        }) {
            bail!("{} is a member of team {}", session.name, other.name);
        }
    }
    // Nor this team's lead or another of its members.
    if team
        .member(&team.lead)
        .and_then(|lead| lead.session.as_ref())
        .is_some_and(|lead| lead.name == session.name && lead.host == host)
    {
        bail!("{} is the lead of team {}", session.name, team.name);
    }
    if team.members.iter().any(|other| {
        other.id != member.id
            && other.state != MemberState::Removed
            && other.session.as_ref().is_some_and(|other| other.name == session.name && other.host == host)
    }) {
        bail!("{} is already a member of team {}", session.name, team.name);
    }
    let session = SessionRef {
        host: host.clone(),
        name: session.name.clone(),
        run_id: if session.run_id.is_empty() { found.run_id.clone() } else { session.run_id.clone() },
        conversation_id: if session.conversation_id.is_empty() { found.conversation_id.clone() } else { session.conversation_id.clone() },
    };
    let worktree = member.folder == MemberFolder::Worktree;
    if worktree {
        // Git runs on this computer; a session elsewhere cannot be checked, so
        // it is refused with that reason instead of a wrong "not a repository" (L5).
        ensure!(
            host.is_empty(),
            "{} runs on {}; a worktree member's session must be on this computer so its repository can be checked",
            session.name,
            here
        );
        ensure!(same_repository(&found.cwd, &team.folder), "{}'s folder {} is not the team's repository", session.name, found.cwd);
    }
    Ok(Adopted { session, directory: found.cwd.clone(), worktree })
}

/// Adopts the existing sessions a plan or a new member names (F8): the session
/// as `hgs ls` shows it and its own folder, or a clear refusal before the team
/// changes. A request that already has a receipt returns before any of it, so
/// a retry gives the stored receipt instead of listing sessions again (L4).
fn adopt_existing_sessions(store: &Store, request: &str, input: &Value, op: &str, filled: &mut Value) -> anyhow::Result<()> {
    if store.receipt(request, input)?.is_some() {
        return Ok(());
    }
    let Ok(Some(team)) = store.load(text(input, "team")) else { return Ok(()) };
    let planned = op == "plan" && team.status == TeamStatus::Planning;
    // C3: a draft's members bind sessions under the same checks.
    let drafted = op == "draft" && text(input, "action") == "update" && team.status == TeamStatus::Draft;
    let joined = op == "add-member" && matches!(team.status, TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused);
    let swapped = op == "swap-member"
        && matches!(team.status, TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused)
        && filled["session"].is_object();
    if planned {
        if let Some(members) = filled["members"].as_array_mut() {
            adopt_members(store, &team, members)?;
        }
    } else if drafted {
        // The lead keeps its own session; the others are checked.
        for member in filled["set"]["members"].as_array_mut().into_iter().flatten().filter(|member| member["id"] != team.lead.as_str()) {
            adopt_members(store, &team, std::slice::from_mut(member))?;
        }
    } else if joined {
        adopt_members(store, &team, std::slice::from_mut(&mut filled["member"]))?;
    } else if swapped {
        // H6: the session named for the swap is adopted under the same checks
        // as a new member's existing session.
        let member = team.member(text(input, "member")).context("Unknown member")?.clone();
        let mut with_session = member;
        with_session.session = serde_json::from_value(filled["session"].clone()).ok();
        let sessions = live_sessions()?;
        let teams = store.active_teams(ACTIVE_TEAMS)?;
        let adopted = adopted_session(&team, &with_session, &teams, &sessions)?;
        filled["session"] = serde_json::to_value(&adopted.session)?;
        // N32: the adopted session's own folder, as for a new member's.
        filled["directory"] = json!(adopted.directory);
        filled["folder"] = serde_json::to_value(if adopted.worktree { MemberFolder::Worktree } else { MemberFolder::Shared })?;
    }
    Ok(())
}

/// Checks and fills the members that name an existing session (F8).
fn adopt_members(store: &Store, team: &Team, members: &mut [Value]) -> anyhow::Result<()> {
    if !members.iter().any(|member| member["session"].is_object()) {
        return Ok(());
    }
    let sessions = live_sessions()?;
    // Only the teams that still run, from the indexed query (C1).
    let teams = store.active_teams(ACTIVE_TEAMS)?;
    for value in members.iter_mut() {
        if !value["session"].is_object() {
            continue;
        }
        let member: Member = serde_json::from_value(value.clone()).context("Invalid member")?;
        let adopted = adopted_session(team, &member, &teams, &sessions)?;
        value["session"] = serde_json::to_value(&adopted.session)?;
        value["directory"] = json!(adopted.directory);
        value["folder"] = serde_json::to_value(if adopted.worktree { MemberFolder::Worktree } else { MemberFolder::Shared })?;
    }
    Ok(())
}

/// U20/U42: the member writes a sandbox may not put in the store, queued in
/// its own folder: a report, a question, a commit or a budget (`expect`).
fn queues_instead(op: &str) -> bool {
    matches!(op, "report" | "ask" | "commit" | "reply" | "expect")
}

/// The team's files refuse writes here: SQLite opened them read-only, or the
/// system denies access (in any language: Windows localizes its message).
fn read_only(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}").to_lowercase();
    ["readonly", "read-only", "access is denied", "permission denied", "os error 5)", "os error 13)", "unable to open database"]
        .iter()
        .any(|sign| text.contains(sign))
}

/// N5: where a member's sandboxed write queues - the folder the worker reads a
/// member's outbox from: the member's own folder as the team records it, never
/// this process's directory. A member that ran `cd tray` and then reports would
/// otherwise leave the request in `tray/.zerus-team/outbox`, where the worker
/// never looks.
fn member_outbox_folder(team: &Team, member: &Member) -> anyhow::Result<std::path::PathBuf> {
    match super::dispatcher::member_folder_path(team, member) {
        Ok(folder) if !folder.is_empty() => Ok(std::path::PathBuf::from(folder)),
        // A team without a folder has no member folder either; the session's own
        // directory is all there is.
        _ => std::env::current_dir().context("The member has no folder to queue in"),
    }
}

/// U20: a member in a sandbox may not write the team store, but it writes its
/// own folder. The report, ask, commit or budget waits there as
/// `<request_id>.json` for the worker, which applies it with the same identity
/// checks (folder + attempt id); `--wait` polls for the outcome (up to 30 s).
/// U42: one id never replaces a queued request with another: a poll that
/// carried a placeholder overwrote #512's whole report before the worker read
/// it. The first whole text stays, or nothing is queued.
fn queue_write(op: &str, input: &Value, wait: bool) -> anyhow::Result<Value> {
    let store = Store::open(false)?;
    let team = store.load(text(input, "team"))?.context("Unknown team")?;
    let Actor::Member(member_id) = actor(&team, op, input)? else {
        bail!("Only a member queues this request; a sandbox does not block the lead");
    };
    let request_id = text(input, "request_id").to_owned();
    let member = team.member(&member_id).context("Unknown member")?;
    let folder = member_outbox_folder(&team, member)?;
    let request = json!({"op":op, "request_id":request_id, "team":team.id, "member":member_id,
        "attempt":text(input, "attempt"), "input":input});
    // U42: a request already applied answers its outcome, and one still queued
    // answers its state (or waits, with `--wait`); a different request under the
    // same id is refused, so a placeholder can never replace the whole text that
    // was queued first.
    if let Some(result) = super::outbox::result(&folder, &request_id) {
        return Ok(result);
    }
    let existing = super::outbox::request(&folder, &request_id);
    if let Some(queued) = &existing {
        ensure!(
            queued["op"] == request["op"] && queued["input"] == request["input"],
            "request_id {request_id} already queues another request; use a new request_id for another one"
        );
    }
    let path = match &existing {
        Some(_) => super::outbox::request_path(&folder, &request_id),
        None => super::outbox::push(&folder, &request)?,
    };
    let queued = json!({
        "status":"queued", "request_id":request_id, "path":path.to_string_lossy(),
        "note": if existing.is_some() {
            "This request is already queued; ask again with the same request_id (or --wait) to read the outcome."
        } else {
            "The sandbox blocks the team store: the request waits in the member's folder and the team worker applies it. Run it again with --wait to read the outcome."
        }
    });
    if !wait {
        return Ok(queued);
    }
    Ok(super::outbox::wait(&folder, &request_id, 30.0).unwrap_or_else(|| json!({
        "status":"queued", "request_id":request_id, "path":path.to_string_lossy(),
        "note":"The team worker has not applied it in 30 s; it stays queued in the member's folder."
    })))
}

/// The teams a finished team's `left_running` looks at: only the ones that
/// still run, from the indexed query, never every stored team (C1).
fn active_team_records(store: &Store) -> anyhow::Result<Vec<Team>> {
    store.active_teams(ACTIVE_TEAMS)
}

/// The stored team list the desktop polls: no live session listing (C1).
fn list_items(teams: &[Team], include_cancelled: bool) -> Value {
    json!({"items":teams.iter().map(|team| summary(team, include_cancelled)).collect::<Vec<_>>()})
}

/// The lead's inbox (C2): every report, question, permission request, its own
/// task and stopped member, oldest first. Read only; the coalesced message the
/// dispatcher sends lists the new ones.
fn inbox_view_for(team: &Team, reviewer: &str) -> Value {
    json!({"items": super::inbox::items_for(team, reviewer).iter().map(super::inbox::Item::value).collect::<Vec<_>>()})
}

/// 9.5: files a finished sub-team's summary on its parent task.
fn file_subteam_report(store: &mut Store, child_id: &str) -> anyhow::Result<()> {
    let child = store.load(child_id)?.context("Unknown team")?;
    let Some(link) = child.parent.clone() else { return Ok(()) };
    if child.status != TeamStatus::Done || store.load(&link.team)?.is_none() {
        return Ok(());
    }
    if store.load(child_id)?.is_some_and(|team| team.status != TeamStatus::Done) {
        return Ok(());
    }
    store.mutate(None, &link.team, None, |parent, events| {
        let task = ops::file_subteam_report(&child, parent)?;
        events.push(Event::new("subteam_report", json!({"team":child_id,"task":task})));
        Ok(Value::Null)
    })
    .map(|_| ())
}

/// 9.5: a rejected sub-team report reopens the sub-team with a Rework draft.
/// N28: only a non-final reject of a finished sub-team reopens it; a stopped or
/// running child keeps its state instead of failing the stored reject.
fn reopen_rejected_subteam(store: &mut Store, parent_id: &str, input: &Value) -> anyhow::Result<()> {
    if input["final"].as_bool() == Some(true) {
        return Ok(());
    }
    let parent = store.load(parent_id)?.context("Unknown team")?;
    let Some(task) = parent.task(input["task"].as_u64().unwrap_or_default() as u32) else { return Ok(()) };
    let Some(child) = task.subteam.clone() else { return Ok(()) };
    let Some(finished) = store.load(&child)? else { return Ok(()) };
    if finished.status != TeamStatus::Done {
        return Ok(());
    }
    let reason = text(input, "reason").to_owned();
    store.mutate(None, &child, None, |team, events| {
        let phase = ops::reopen_subteam(team, &reason)?;
        events.push(Event::new("subteam_reopened", json!({"team":child,"phase":phase,"reason":reason})));
        Ok(Value::Null)
    })
    .map(|_| ())
}

/// 9.7: pause, resume or stop of a team reaches its sub-teams, level by level.
fn cascade_status(store: &mut Store, team_id: &str, op: &str, depth: usize) -> anyhow::Result<()> {
    if depth >= 8 {
        return Ok(());
    }
    let Some(team) = store.load(team_id)? else { return Ok(()) };
    for child_id in team.children.iter().take(32) {
        let Some(child) = store.load(child_id)? else { continue };
        let applies = match op {
            "pause" => child.status == TeamStatus::Running,
            "resume" => child.status == TeamStatus::Paused,
            "stop" => matches!(child.status, TeamStatus::Running | TeamStatus::Paused | TeamStatus::Planning),
            _ => false,
        };
        if applies {
            store.mutate(None, child_id, None, |child, events| {
                ops::apply(child, op, &json!({}), &Actor::Human, &BTreeSet::new(), events)?;
                events.push(Event::new("cascaded", json!({"parent":team_id,"op":op})));
                Ok(Value::Null)
            })?;
        }
        cascade_status(store, child_id, op, depth + 1)?;
    }
    Ok(())
}

/// U26/U32/N15: the rule the owner's sentence must meet for this roster
/// change; an empty rule when the request changes no roster or cap (so no word
/// is needed). A roster change names the member by its id or its role, and
/// says the operation in English or Russian, as an add names its member.
fn roster_word_rule(team: &Team, op: &str, input: &Value) -> super::owner_word::WordRule {
    use super::owner_word::{WordRule, MAX_VERBS, REMOVE_VERBS, ROSTER_PHRASES, SWAP_VERBS};
    let names_for = |id: &str| -> Vec<String> {
        let mut names = vec![id.to_owned()];
        if let Some(member) = team.member(id) {
            names.extend(super::owner_word::role_aliases(&member.role).iter().map(|alias| (*alias).to_owned()));
        }
        names
    };
    match op {
        "add-member" => {
            let member = &input["member"];
            if text(member, "id").is_empty() {
                WordRule::default()
            } else {
                // U32: the sentence says to add and names the member's role or
                // agent/model, never the generated id; that naming and the
                // count are checked once the sentence is verified.
                WordRule { phrases: &[], all: Vec::new(), add: true, expect: format!("add a {}", text(member, "role")), ..Default::default() }
            }
        }
        "remove-member" => {
            let id = text(input, "member");
            if id.is_empty() {
                WordRule::default()
            } else {
                WordRule { phrases: ROSTER_PHRASES, all: Vec::new(), add: false, verbs: REMOVE_VERBS, names: names_for(id), expect: format!("remove {id}") }
            }
        }
        "scale" => {
            // U49: the sentence says to add ("add"/"добав") and names the count;
            // the role it names is checked once the sentence is verified.
            match input["count"].as_u64() {
                Some(count) if (1..=32).contains(&count) && !text(input, "role").trim().is_empty() => {
                    WordRule { phrases: &[], all: vec![count.to_string()], add: true, expect: format!("add {count} {}", text(input, "role").trim()), ..Default::default() }
                }
                _ => WordRule::default(),
            }
        }
        "drain" => {
            // U49: a drain is a removal; cancelling one changes no roster.
            let id = text(input, "member");
            if id.is_empty() || input["draining"].as_bool() == Some(false) {
                WordRule::default()
            } else {
                WordRule { phrases: super::owner_word::DRAIN_PHRASES, all: vec![id.to_owned()], add: false, expect: format!("drain {id}"), ..Default::default() }
            }
        }
        "swap-member" => {
            // U54: a bulk swap names every member it moves.
            let mut ids: Vec<String> = input["members"]
                .as_array()
                .map(|ids| ids.iter().filter_map(|id| id.as_str()).map(str::to_owned).collect())
                .unwrap_or_default();
            if ids.is_empty() {
                ids.push(text(input, "member").to_owned());
            }
            if ids.iter().any(String::is_empty) {
                WordRule::default()
            } else {
                WordRule {
                    phrases: ROSTER_PHRASES,
                    all: Vec::new(),
                    add: false,
                    verbs: SWAP_VERBS,
                    names: ids.iter().flat_map(|id| names_for(id)).collect(),
                    expect: format!("swap {}", ids.join(", ")),
                }
            }
        }
        "relaunch" => {
            // U54: the lead relaunches a member's session on the owner's word.
            // #728: except a stuck member when the owner set lead_restarts_stuck;
            // that route is the lead's own decision.
            let id = text(input, "member");
            if id.is_empty()
                || (team.settings.lead_restarts_stuck && team.member(id).is_some_and(|member| member.stuck.is_some()))
            {
                WordRule::default()
            } else {
                WordRule {
                    phrases: ROSTER_PHRASES,
                    all: Vec::new(),
                    add: false,
                    verbs: super::owner_word::RELAUNCH_VERBS,
                    names: names_for(id),
                    expect: format!("relaunch {id}"),
                }
            }
        }
        "settings" => {
            let cap = input["settings"]["max_members"].as_u64();
            match cap {
                Some(cap) if cap as usize != team.settings.max_members => {
                    WordRule { phrases: ROSTER_PHRASES, all: vec![cap.to_string()], add: false, verbs: MAX_VERBS, names: Vec::new(), expect: format!("max members {cap}") }
                }
                _ => WordRule::default(),
            }
        }
        _ => WordRule::default(),
    }
}

/// U26: fills the verified roster word for a lead caller, or refuses. The
/// verifier is a parameter so a test can use a fixture events database.
fn check_roster_word(
    team: &Team,
    op: &str,
    input: &Value,
    filled: &mut Value,
    caller: &Actor,
    verify: impl Fn(&Team, &Value, &super::owner_word::WordRule, f64) -> anyhow::Result<super::model::OwnerWord>,
) -> anyhow::Result<()> {
    if !matches!(caller, Actor::Member(id) if *id == team.lead) {
        return Ok(());
    }
    let rule = roster_word_rule(team, op, filled);
    if !rule.needed() {
        return Ok(());
    }
    // `roster_word` itself is never the caller's to send: strip it first, so a
    // caller cannot assert a word that was not verified.
    if let Some(object) = filled.as_object_mut() {
        object.remove("roster_word");
    }
    // The sentence is the request's `owner_word` object, as `approve` takes it.
    let word_input = input.get("owner_word").filter(|word| word.is_object()).with_context(|| {
        format!("The lead changes the roster only on your word: pass \"owner_word\" with the owner's sentence, such as \"{}\"", rule.expect)
    })?;
    // U56: the refusal names a sentence that would pass.
    let word = verify(team, word_input, &rule, super::super::now()).with_context(|| {
        format!("The lead changes the roster only on your word: quote your own sentence of the last 30 minutes that says \"{}\"; without it, propose the change (F8)", rule.expect)
    })?;
    // U32/N7: a count word ("add two backends") authorizes that many adds and
    // the cap grows by one for each; every other word pays for one change.
    let (allowed, change) = if op == "add-member" {
        // The sentence names what is added by role or agent/model, in either
        // language; the generated id is not needed.
        let member = filled["member"].clone();
        let (role, agent, model) =
            (text(&member, "role").to_owned(), text(&member, "agent").to_owned(), text(&member, "model").to_owned());
        let described = [role.as_str(), agent.as_str(), model.as_str()]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(" / ");
        ensure!(
            super::owner_word::names_member(&word.text, &role, &agent, &model),
            "The owner's sentence does not name the member to add ({described}); quote a sentence naming its role or agent"
        );
        (super::owner_word::add_count(&word.text), format!("adding {role}"))
    } else if op == "scale" {
        // U49: the sentence names the role to add and the count it asks for.
        let (role, agent, model) =
            (text(filled, "role").to_owned(), text(filled, "agent").to_owned(), text(filled, "model").to_owned());
        let described = [role.as_str(), agent.as_str(), model.as_str()]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(" / ");
        ensure!(
            super::owner_word::names_member(&word.text, &role, &agent, &model),
            "The owner's sentence does not name the role to add ({described}); quote a sentence naming its role or agent"
        );
        let asked = filled["count"].as_u64().unwrap_or(0);
        let said = super::owner_word::add_count(&word.text) as u64;
        ensure!(said == asked, "The owner's sentence names {said} members, not {asked}; quote a sentence naming the count");
        (1, format!("scaling {role} by {asked}"))
    } else if op == "drain" {
        (1, "draining the member".to_owned())
    } else {
        (1, "the roster change".to_owned())
    };
    if !team.owner_word_allowed_again(word.event, allowed, &change) {
        let used = team.owner_word_used(word.event).unwrap_or_default();
        bail!("That word was already used for {used}; ask the owner again");
    }
    filled["owner_word_count"] = json!(allowed);
    filled["owner_word_change"] = json!(change);
    filled["owner_word_checked"] = serde_json::to_value(&word)?;
    filled["owner_word_event"] = json!(word.event);
    filled["roster_word"] = Value::Bool(true);
    Ok(())
}

/// U29: fills the verified owner word of an addition to a running phase for
/// a lead caller: the request, or the confirmation newer than the warning of
/// the pending addition it names. The verifier is a parameter for tests.
fn check_phase_word(
    team: &Team,
    input: &Value,
    filled: &mut Value,
    caller: &Actor,
    verify: impl Fn(&Team, &Value, u32, super::owner_word::PhaseWord, f64, f64) -> anyhow::Result<super::model::OwnerWord>,
) -> anyhow::Result<()> {
    // A decline is the owner's alone and needs no word; the change refuses
    // anyone else.
    if !matches!(caller, Actor::Member(id) if *id == team.lead) || input["decline"].as_bool() == Some(true) {
        return Ok(());
    }
    let phase = input["phase"].as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0).context("A phase number is required")?;
    let word = input
        .get("owner_word")
        .filter(|word| word.is_object())
        .with_context(|| format!("The lead adds a task to a running phase only on your word: pass \"owner_word\" with the owner's sentence, such as \"add a task to phase {phase}\""))?;
    let (kind, after, change) = match input["confirm"].as_str().filter(|id| !id.is_empty()) {
        None => (super::owner_word::PhaseWord::Request, 0.0, "the request to add to a running phase"),
        Some(id) => {
            let pending = team.pending_additions.iter().find(|pending| pending.id == id).context("That addition is unknown or expired; ask again")?;
            (super::owner_word::PhaseWord::Confirm, pending.warned_at, "the confirmation of an addition to a running phase")
        }
    };
    let verified = verify(team, word, phase, kind, after, super::super::now())?;
    if let Some(used) = team.owner_word_used(verified.event) {
        // U58: one sentence naming a member and its tasks pays for both, the
        // member first: the roster add, then this request for its tasks.
        ensure!(
            team.owner_word_allows_its_tasks(verified.event),
            "That word was already used for {used}; ask the owner again"
        );
    }
    filled["owner_word_checked"] = serde_json::to_value(&verified)?;
    filled["owner_word_event"] = json!(verified.event);
    filled["owner_word_change"] = json!(change);
    Ok(())
}

/// 9.7: a task with a sub-team cannot be cancelled or reassigned without
/// saying whether the sub-team stops or keeps running.
fn choose_subteam(store: &mut Store, team: &Team, input: &Value) -> anyhow::Result<()> {
    let Some(task) = team.task(input["task"].as_u64().unwrap_or_default() as u32) else { return Ok(()) };
    if !ops::subteam_stop_choice(input, task)? {
        return Ok(());
    }
    let Some(child) = task.subteam.clone() else { return Ok(()) };
    let Some(found) = store.load(&child)? else { return Ok(()) };
    if matches!(found.status, TeamStatus::Running | TeamStatus::Paused | TeamStatus::Planning) {
        store.mutate(None, &child, None, |team, events| {
            ops::apply(team, "stop", &json!({}), &Actor::Human, &BTreeSet::new(), events)?;
            events.push(Event::new("subteam_stopped", json!({"task":task.id})));
            Ok(Value::Null)
        })?;        cascade_status(store, &child, "stop", 1)?;
    }
    Ok(())
}

/// 9.7: archive only after the children are stopped or done.
fn ensure_children_closed(store: &Store, team: &Team) -> anyhow::Result<()> {
    for child_id in team.children.iter().take(32) {
        let Some(child) = store.load(child_id)? else { continue };
        ensure!(
            matches!(child.status, TeamStatus::Done | TeamStatus::Stopped | TeamStatus::Archived),
            "The sub-team {} is not stopped or done; stop it before archiving",
            child.name
        );
    }
    Ok(())
}

/// 9.6: puts a forwarded question on the parent task (creating an attempt when
/// the parent task has none).
fn forward_to_parent(
    store: &mut Store,
    team: &Team,
    parent_question: &str,
    child_question: &str,
    text: &str,
    link: &Parent,
) -> anyhow::Result<()> {
    let child_id = team.id.clone();
    let child_name = team.name.clone();
    store.mutate(None, &link.team, None, |parent, events| {
        let task = parent.task_mut(link.task).context("The parent task is gone")?;
        ensure!(!task.status.finished(), "The parent task is already decided");
        if task.open_attempt().is_none() {
            let member = if link.member.is_empty() { task.assignee.clone() } else { link.member.clone() };
            let n = task.attempts.len() as u32 + 1;
            task.attempts.push(Attempt {
                n,
                id: uuid::Uuid::new_v4().to_string(),
                member,
                sent_at: now(),
                delivered_at: Some(now()),
                state: AttemptState::Delivered,
                ..Default::default()
            });
        }
        let attempt = task.attempts.iter_mut().rev().find(|attempt| attempt.state.open()).context("The parent task has no open attempt")?;
        attempt.questions.push(Question {
            id: parent_question.to_owned(),
            text: format!("From the sub-team {child_name}:\n{text}"),
            at: now(),
            // N24: the child question this one carries; `forwarded` stays for
            // the level above if this one is passed on again.
            source: child_question.to_owned(),
            ..Default::default()
        });
        events.push(Event::new("subteam_question", json!({"team":child_id,"question":parent_question,"task":link.task})));
        Ok(Value::Null)
    })?;
    Ok(())
}

/// 9.6: a sub-lead passes one of its open questions to the parent lead as a
/// question on the parent task; the parent's reply is copied back down.
/// 9.6: the parent question that carries a sub-team permission request with
/// the same command, source and reason as the lead's card.
fn permission_question(child: &Team, member: &str, pending: &PendingQuestion) -> Question {
    let tool = pending.card["questions"][0]["header"].as_str().or_else(|| pending.card["tool"].as_str()).unwrap_or_default();
    let command = super::dispatcher::permission_command_line(&pending.card);
    let reason: String = pending.card["questions"][0]["body"].as_str().unwrap_or_default().chars().take(2000).collect();
    Question {
        id: pending.question_id.clone(),
        text: format!(
            "Permission request from {member}: {tool}\n{command}Reason: {reason}\nAnswer the native card on the sub-team:\n  {{\"team\":\"{}\",\"member\":\"{member}\",\"question_id\":\"{}\",\"expected_question_hash\":\"{}\",\"answers\":[...]}} | hgs team answer --json",
            child.id, pending.question_id, pending.question_hash
        ),
        at: now(),
        ..Default::default()
    }
}

fn escalate(input: &Value) -> anyhow::Result<Value> {
    let request = text(input, "request_id");
    ensure!(canonical_uuid(request), "A canonical request_id is required");
    let mut store = Store::open(true)?;
    // N28: a replay returns the first receipt instead of duplicating the
    // parent question.
    if let Some(receipt) = store.receipt(request, input)? {
        return Ok(receipt);
    }
    let team = store.load(text(input, "team"))?.context("Unknown team")?;
    let link = team.parent.clone().context("Only a sub-team escalates a question")?;
    let by = actor(&team, "escalate", input)?;
    ensure!(matches!(by, Actor::Human) || by.is_lead(&team), "Only the sub-team lead or you escalate a question");
    let question_id = text(input, "question");
    ensure!(!question_id.is_empty(), "A question id is required");
    let found = team
        .tasks
        .iter()
        .flat_map(|task| &task.attempts)
        .flat_map(|attempt| &attempt.questions)
        .find(|question| question.id == question_id && question.open())
        .cloned();
    // Section 9.6: a permission request travels the same way; the parent lead
    // answers the native card on the sub-team, by its exact ids.
    let question = match found {
        Some(question) => question,
        None => {
            let (member, pending) = team
                .members
                .iter()
                .find_map(|member| {
                    member
                        .pending
                        .as_ref()
                        .filter(|pending| pending.question_id == input["question"].as_str().unwrap_or_default())
                        .map(|pending| (member.id.clone(), pending.clone()))
                })
                .context("No open question or permission request with that id in this sub-team")?;
            permission_question(&team, &member, &pending)
        }
    };
    let parent_question = ops::forward_question(&team, &question);
    let child_id = team.id.clone();
    let receipt = json!({"status":"escalated","team":child_id,"question":parent_question,"parent":link.team});
    // N27: a retry that finds the forward landed but its receipt lost writes
    // the receipt instead of refusing the question as already escalated.
    if question.forwarded == parent_question {
        store.mutate(Some((request, input)), &child_id, None, |_team, _events| Ok(receipt.clone()))?;
        return Ok(receipt);
    }
    // N24: one forward per level. A question the level below escalated (it
    // carries the child's id in `source`, or in `forwarded` on a team stored
    // before that field) may go one level up; one already passed on is refused.
    ensure!(!question.forwarded.starts_with("sub:"), "This question was already escalated");
    let child_name = team.name.clone();
    // N27: the child link is marked without a receipt; the receipt waits for
    // the parent question, so a retry after a failed forward escalates again
    // instead of returning an empty one.
    store.mutate(None, &child_id, None, |team, events| {
        for task in &mut team.tasks {
            for attempt in &mut task.attempts {
                for question in &mut attempt.questions {
                    if question.id == question_id {
                        // A question stored before `source` kept the child id
                        // in `forwarded`; keep it as the down-link.
                        if question.source.is_empty() && !question.forwarded.is_empty() {
                            question.source = question.forwarded.clone();
                        }
                        question.forwarded = parent_question.clone();
                    }
                }
            }
        }
        events.push(Event::new("escalated", json!({"question":question_id,"to":parent_question,"parent":link.team})));
        Ok(Value::Null)
    })?;
    let _ = child_name;
    if let Err(error) = forward_to_parent(&mut store, &team, &parent_question, question_id, &question.text, &link) {
        // N28: a failure upstream must not leave the child marked forwarded.
        let _ = store.mutate(None, &child_id, None, |team, _| {
            for task in &mut team.tasks {
                for attempt in &mut task.attempts {
                    for question in &mut attempt.questions {
                        if question.forwarded == parent_question {
                            question.forwarded.clear();
                        }
                    }
                }
            }
            Ok(Value::Null)
        });
        return Err(error);
    }
    // N27: the receipt is written only now, once the parent has the question.
    store.mutate(Some((request, input)), &child_id, None, |_team, _events| Ok(receipt.clone()))?;
    Ok(receipt)
}

/// 9.6/N24: the question `id` on `task`.
fn question_in(task: &Task, id: &str) -> Option<Question> {
    task.attempts.iter().flat_map(|attempt| &attempt.questions).find(|question| question.id == id).cloned()
}

/// 9.6/N24: the question `id` in `team` with the task that holds it.
fn question_with_task(team: &Team, id: &str) -> Option<(u32, Question)> {
    team.tasks.iter().find_map(|task| question_in(task, id).map(|question| (task.id, question)))
}

/// 9.6: copies the parent lead's answer into the sub-team's question. N23: the
/// inbox's reply names only the question, so its task is found here. N24: an
/// answer travels one level per call, on down the chain of escalations. A
/// queued reply from a sandboxed sub-lead is applied by the worker and routed
/// here too.
pub(super) fn route_reply_down(store: &mut Store, parent_id: &str, input: &Value) -> anyhow::Result<()> {
    let parent = store.load(parent_id)?.context("Unknown team")?;
    let question_id = text(input, "question");
    let asked_task = input["task"].as_u64().and_then(|task| u32::try_from(task).ok());
    let found = match asked_task {
        Some(id) => parent.task(id).and_then(|task| question_in(task, question_id)).map(|question| (id, question)),
        None => question_with_task(&parent, question_id),
    };
    let Some((task_id, question)) = found else { return Ok(()) };
    let Some(child) = parent.task(task_id).and_then(|task| task.subteam.clone()) else { return Ok(()) };
    // A question asked inside this team has no sub-team question to reach.
    if question.source.is_empty() && question.forwarded.is_empty() {
        return Ok(());
    }
    let answer = text(input, "text").trim().to_owned();
    if answer.is_empty() || store.load(&child)?.is_none() {
        return Ok(());
    }
    carry_answer_down(store, parent_id, question_id, &answer, 0)?;
    Ok(())
}

/// 9.6: the answer of `carrier` (a question in `team_id` that carries a
/// sub-team's question) goes to the child question it carries, and on down if
/// that child question carries one itself (N24).
fn carry_answer_down(store: &mut Store, team_id: &str, carrier: &str, answer: &str, depth: usize) -> anyhow::Result<bool> {
    if depth >= 8 {
        return Ok(false);
    }
    let Some(team) = store.load(team_id)? else { return Ok(false) };
    let Some((task_id, _)) = question_with_task(&team, carrier) else { return Ok(false) };
    let Some(child_id) = team.task(task_id).and_then(|task| task.subteam.clone()) else { return Ok(false) };
    let mut next: Option<String> = None;
    let mut delivered = false;
    store.mutate(None, &child_id, None, |child, events| {
        if let Some(answered) = ops::answer_forwarded(child, carrier, answer) {
            delivered = true;
            events.push(Event::new("answered_from_parent", json!({"question":carrier})));
            // The answered question may itself carry a deeper sub-team's one.
            let carries = child
                .tasks
                .iter()
                .flat_map(|task| &task.attempts)
                .flat_map(|attempt| &attempt.questions)
                .find(|question| question.id == answered)
                .is_some_and(|question| !question.source.is_empty());
            if carries {
                next = Some(answered);
            }
        }
        Ok(Value::Null)
    })?;
    if let Some(next) = next {
        carry_answer_down(store, &child_id, &next, answer, depth + 1)?;
    }
    Ok(delivered)
}

/// N4: how long `hgs team run --wait` waits by default: a short poll a member's
/// shell can afford, never the template's whole bound (up to an hour).
const RUN_WAIT_SECONDS: f64 = 90.0;

/// U21: a member asks Zerus to run one built-in command template. Zerus runs it
/// outside the member's sandbox (a detached runner, C1), so the request goes
/// through the U20 outbox in the member's own folder. N4: an answer already in
/// the folder is the answer and never queues the run again; the wait is short,
/// and asking again with the same request_id reads the result.
fn team_run(input: &Value, wait: bool) -> anyhow::Result<Value> {
    // N5: queue where the worker reads - the member's own folder, never this
    // process's directory.
    let store = Store::open(false)?;
    let team = store.load(text(input, "team"))?.context("Unknown team")?;
    let Actor::Member(member_id) = actor(&team, "run", input)? else {
        bail!("Only a member runs a template; the lead runs commands itself")
    };
    let folder = member_outbox_folder(&team, team.member(&member_id).context("Unknown member")?)?;
    team_run_in(&folder, input, wait, RUN_WAIT_SECONDS)
}

fn team_run_in(folder: &std::path::Path, input: &Value, wait: bool, wait_seconds: f64) -> anyhow::Result<Value> {
    let request_id = text(input, "request_id").to_owned();
    ensure!(canonical_uuid(&request_id), "A canonical request_id is required");
    let store = Store::open(false)?;
    let team = store.load(text(input, "team"))?.context("Unknown team")?;
    let Actor::Member(member_id) = actor(&team, "run", input)? else {
        bail!("Only a member runs a template; the lead runs commands itself")
    };
    ensure!(member_id != team.lead, "The lead runs commands itself");
    let attempt_id = text(input, "attempt").to_owned();
    let attempt = team
        .tasks
        .iter()
        .find(|task| task.open_attempt().is_some_and(|attempt| attempt.id == attempt_id && attempt.member == member_id));
    let Some(task) = attempt else { bail!("attempt {attempt_id} does not run for this member") };
    // N4: an outcome already there answers at once; the same request never runs twice.
    if let Some(result) = super::outbox::result(folder, &request_id) {
        return Ok(result);
    }
    if super::outbox::running(folder, &request_id).is_some() {
        return Ok(if wait {
            super::outbox::wait(folder, &request_id, wait_seconds).unwrap_or_else(|| running_note(&request_id))
        } else {
            running_note(&request_id)
        });
    }
    if let Some(queued) = super::outbox::request(folder, &request_id) {
        // A push of the same id is already queued: answer its state, do not push
        // a second file (the member retried because the first call did not wait).
        ensure!(
            queued["template"] == input["template"] && queued["args"] == input["args"],
            "request_id {request_id} is already used by another run; use a new UUID"
        );
        return Ok(json!({
            "status":"queued", "request_id":request_id,
            "path":super::outbox::request_path(folder, &request_id).to_string_lossy(),
            "note":"This run is already queued; ask again with the same request_id (or --wait) to read its result."
        }));
    }
    // Validate here too: a bad template never reaches the worker's queue.
    let template = text(input, "template").to_owned();
    super::templates::plan(&template, &input["args"], folder)?;
    // U62 (#715): the full suite is not a member's run. The team's policy says
    // who may start it; a one-time owner grant is bound to a release and used
    // once. The refusal names the estimate and how to ask.
    let full = full_test_run(&template, &input["args"]);
    let use_grant = if full { check_full_run(&team, folder)? } else { false };
    let request = json!({"op":"run","request_id":request_id,"team":team.id,"member":member_id,"attempt":attempt_id,
        "template":template,"args":input["args"].clone(),"task":task.id,"input":input});
    let path = super::outbox::push(folder, &request)?;
    if use_grant {
        let mut store = Store::open(false)?;
        let release = team.full_tests_grant.as_ref().map(|grant| grant.release.clone()).unwrap_or_default();
        store.mutate(None, &team.id, None, |team, events| {
            if team.full_tests_grant.is_some() {
                team.full_tests_grant = None;
                events.push(Event::new("full_tests_grant_used", json!({"release":release})));
            }
            Ok(Value::Null)
        })?;
    }
    let queued = json!({
        "status":"queued", "request_id":request_id, "path":path.to_string_lossy(),
        "note":"Zerus runs the template outside your sandbox; ask again with the same request_id (or --wait) to read the exit code."
    });
    if !wait {
        return Ok(queued);
    }
    Ok(super::outbox::wait(folder, &request_id, wait_seconds).unwrap_or_else(|| json!({
        "status":"running", "request_id":request_id,
        "note":"The run has not finished yet; ask again with the same request_id to read its exit code."
    })))
}

/// N4: the answer while a runner still holds the claim.
fn running_note(request_id: &str) -> Value {
    json!({"status":"running","request_id":request_id,
        "note":"Zerus is running it; ask again with the same request_id to read the exit code."})
}

/// U62 (#715): the templates that run a whole suite when nothing narrows them.
/// A QtTest binary is one suite, a piece the map names; cargo-test without a
/// filter, binary or package and the Android unit tests run everything.
fn full_test_run(template: &str, args: &Value) -> bool {
    let empty = |key: &str| text(args, key).is_empty();
    match template {
        "cargo-test" => empty("filter") && empty("bin") && empty("package"),
        "android-test" => true,
        _ => false,
    }
}

/// U62 (#715): the full run's recorded time from the project's map, for the
/// refusal; "unknown time" when the project has no map.
fn full_estimate(folder: &std::path::Path, team: &Team) -> String {
    crate::test_discipline::full_seconds(folder, Some(&team.id))
        .ok()
        .flatten()
        .map(crate::test_discipline::affected::duration)
        .unwrap_or_else(|| "unknown time".to_owned())
}

/// U62 (#715): whether a full run may start; true when it spends the owner's
/// one-time grant. Pure, so the refusal is tested without a store.
fn check_full_run(team: &Team, folder: &std::path::Path) -> anyhow::Result<bool> {
    use crate::state::teams::model::FullTests;
    let policy = team.settings.full_tests;
    if policy == FullTests::Allowed {
        return Ok(false);
    }
    let granted = team.full_tests_grant.is_some();
    if !granted || policy == FullTests::Never {
        let estimate = full_estimate(folder, team);
        bail!(
            "This is a full test run ({estimate}). The team's full_tests is {}: ask the owner for one run bound to a release, then set it with {} | hgs team full-tests --json, or the owner sets \"full_tests\":\"allowed\".",
            match policy {
                FullTests::Never => "\"never\"",
                FullTests::Ask => "\"ask\"",
                FullTests::Allowed => "\"allowed\"",
            },
            json!({"request_id":"<new UUID>","team":team.id,"release":"<release>"})
        );
    }
    Ok(true)
}

/// U21: the built-in set, the owner's additions and the global off switch.
/// Only the owner (the desktop, not an agent) changes them; they live in Zerus
/// state, never in a member's folder, so a sandboxed member cannot add one.
fn templates_command(input: &Value) -> anyhow::Result<Value> {
    match text(input, "action") {
        "" | "get" => Ok(super::templates::listed()),
        "set" => {
            // N3/C6: the same owner gate as every other owner path - no caller
            // record and no session marker (dsh conversation, hgs session or
            // hgs run), and the request says it is you. An added template runs
            // outside the member's sandbox (U21), so an agent never adds one.
            let (dsh, session, run) = caller_identities();
            owner_caller(input, caller_record().is_some(), dsh.is_some(), session.is_some(), run.is_some())?;
            if let Some(enabled) = input["enabled"].as_bool() {
                super::templates::set_enabled(enabled)?;
            }
            if let Some(list) = input["templates"].as_array() {
                super::templates::set_additions(list)?;
            }
            Ok(super::templates::listed())
        }
        other => bail!("templates action is get or set (got {other})"),
    }
}

/// The owner, 2026-10-28: the shapes of members' requests the lead allows
/// without being asked again (`safe_actions`). The lead or the owner lists,
/// adds and removes them; each change is in the team's journal.
fn safe_action_command(input: &Value) -> anyhow::Result<Value> {
    let team = Store::open(false)?.load(text(input, "team"))?.context("Unknown team")?;
    let by = actor(&team, "safe-action", input)?;
    ensure!(
        matches!(&by, Actor::Human) || matches!(&by, Actor::Member(id) if *id == team.lead),
        "Only the lead or the owner records safe actions"
    );
    let who = if matches!(&by, Actor::Human) { "owner" } else { "lead" };
    let listed = |team: &Team| json!({"actions":super::safe_actions::list().into_iter().filter(|action| action["team"] == team.id.as_str()).collect::<Vec<_>>()});
    let (event, detail) = match text(input, "action") {
        "" | "list" => return Ok(listed(&team)),
        "add" => {
            let added = super::safe_actions::add(&team, input, who)?;
            ("safe_action_added", json!({"action":added["id"],"shape":added["shape"],"member":added["member"],"role":added["role"],"by":who}))
        }
        "remove" => {
            super::safe_actions::remove(&team, text(input, "id"))?;
            ("safe_action_removed", json!({"action":text(input, "id"),"by":who}))
        }
        other => bail!("safe-action action is list, add or remove (got {other})"),
    };
    Store::open(true)?.mutate(None, &team.id, None, |_, events| {
        events.push(Event::new(event, detail.clone()));
        Ok(Value::Null)
    })?;
    Ok(listed(&team))
}

/// U51: the owner creates an intake key (shown once, never stored) or revokes
/// one by id. The team keeps only the key's hash.
fn intake_key(input: &Value) -> anyhow::Result<Value> {
    let request = text(input, "request_id");
    ensure!(canonical_uuid(request), "A canonical request_id is required");
    let team_id = text(input, "team");
    ensure!(!team_id.is_empty(), "Name the team");
    let mut secret = None;
    let mut store = Store::open(true)?;
    let mut receipt = store.mutate(Some((request, input)), team_id, input["revision"].as_u64(), |team, events| {
        let by = actor(team, "intake-key", input)?;
        let mut value = ops::apply(team, "intake-key", input, &by, &BTreeSet::new(), events)?;
        // The stored receipt names the key, never its secret.
        if let Some(object) = value.as_object_mut() {
            secret = object.remove("key").and_then(|key| key.as_str().map(str::to_owned));
        }
        Ok(value)
    })?;
    if receipt["status"] == "created" {
        if let Some(secret) = secret {
            receipt["key"] = json!(secret);
        }
    }
    Ok(receipt)
}

/// U51: a process presenting the owner's intake key adds one bounded urgent
/// phase. No session identity is needed: the key is the whole authority, and
/// the team names its label in the feed. The key never enters the stored
/// request or the receipt.
fn intake(input: &Value) -> anyhow::Result<Value> {
    let key = text(input, "key");
    ensure!(!key.is_empty(), "An intake request carries the owner's key");
    let team_id = text(input, "team");
    ensure!(!team_id.is_empty(), "Name the team");
    let request = text(input, "request_id");
    let generated = uuid::Uuid::new_v4().to_string();
    let request = if request.is_empty() { generated.as_str() } else { request };
    ensure!(canonical_uuid(request), "A canonical request_id is required when one is given");
    // The stored request keeps no secret; a retry with the same key replays.
    let mut stored = input.clone();
    if let Some(object) = stored.as_object_mut() {
        object["key"] = json!("");
    }
    let mut store = Store::open(true)?;
    store.mutate(Some((request, &stored)), team_id, input["revision"].as_u64(), |team, events| {
        let label = ops::intake_label(team, key).context("Unknown or revoked intake key for this team")?;
        ops::apply(team, "intake", input, &Actor::Intake(label), &BTreeSet::new(), events)
    })
}

fn run(op: &str, input: &Value, wait: bool) -> anyhow::Result<Value> {
    // U51: an intake key adds tasks; it is refused for every other operation,
    // and nothing else may present one.
    let presented = text(input, "key");
    ensure!(
        presented.is_empty() || op == "intake",
        "An intake key adds urgent tasks only: use hgs team intake"
    );
    if op == "intake" {
        return intake(input);
    }
    if op == "intake-key" {
        return intake_key(input);
    }
    // F5: the draft's read and its start are not mutations.
    if op == "draft" {
        match text(input, "action") {
            "get" => return draft_get(input),
            "start" => return draft_start(input),
            _ => {}
        }
    }
    // U21: a member's template run and the owner's off switch are not team
    // mutations: the worker runs the template outside the member's sandbox.
    if op == "run" {
        return team_run(input, wait);
    }
    if op == "templates" {
        return templates_command(input);
    }
    if op == "safe-action" {
        return safe_action_command(input);
    }
    // #717: the team's test queue and its results database; only the runner
    // writes a result, never a report.
    if op == "test" {
        return test_command(input);
    }
    // F8: a decision may create the next team, so it runs with the store.
    if op == "decide-proposal" {
        return decide_proposal(input);
    }
    // #742: the owner's Allow or Deny of a missing tool's install; not a team's.
    if op == "decide-tool" {
        return crate::test_discipline::tools::decide(input);
    }
    match op {
        "create" => create(input),
        "answer" => answer(input),
        // U20: a sandbox that cannot write the store queues the whole commit, so
        // no commit exists without the attempt recording it.
        "commit" => match commit(input) {
            Err(error) if read_only(&error) => queue_write("commit", input, wait),
            result => result,
        },
        "next-goal" => next_goal(input),
        "leftovers" => {
            let store = Store::open(false)?;
            let teams = store.teams(true, 1000)?;
            let sessions = live_sessions()?;
            Ok(json!({"items":leftovers(&teams, &sessions)}))
        }
        // U50: what waits for the owner across every team, read only.
        "waiting" => {
            // #742: a missing tool's offer waits for the owner with or without a team.
            let tools = crate::test_discipline::tools::waiting_items();
            if !store::path().exists() {
                let items = super::owner_waiting::sorted(tools);
                let before = Some(&input["before"]).filter(|cursor| !cursor.is_null());
                return Ok(super::owner_waiting::page(&items, before, input["limit"].as_u64()));
            }
            let teams = active_team_records(&Store::open(false)?)?;
            let mut items = super::owner_waiting::items(&teams, super::super::now());
            items.extend(tools);
            let items = super::owner_waiting::sorted(items);
            let before = Some(&input["before"]).filter(|cursor| !cursor.is_null());
            Ok(super::owner_waiting::page(&items, before, input["limit"].as_u64()))
        }
        "list" => {
            let path = store::path();
            if !path.exists() {
                return Ok(json!({"items":[]}));
            }
            let limit = input["limit"].as_i64().unwrap_or(200).clamp(1, 1000);
            let store = Store::open(false)?;
            let teams = store.teams(input["include_archived"].as_bool() == Some(true), limit)?;
            // The desktop lists teams while it shows them: a dispatcher that
            // stopped (an update, a crash) starts again while a team needs one.
            if teams.iter().any(|t| t.status == TeamStatus::Running || (t.status == TeamStatus::Planning && t.brief == Notice::Pending)) {
                if let Err(error) = super::dispatcher::ensure_worker() {
                    eprintln!("hgs: the team dispatcher did not start: {error}");
                }
            }
            // Stored teams only: a live session listing belongs to status, not
            // to this list the desktop polls (C1).
            Ok(list_items(&teams, input["include_cancelled"].as_bool() == Some(true)))
        }
        "status" => {
            let store = Store::open(false)?;
            let team = store.load(text(input, "team"))?.context("Unknown team")?;
            let finished = matches!(team.status, TeamStatus::Done | TeamStatus::Stopped);
            // Only a finished team reads the live listing, for its members left
            // running (F7); a watched running team is polled often and must not
            // spawn one (C1, L3).
            let sessions = if status_lists_sessions(team.status) { recent_sessions().ok() } else { None };
            // Only the teams that still run, from the indexed query, so a
            // finished team's status never reads every team (C1).
            let mut teams = if finished { active_team_records(&store)? } else { Vec::new() };
            // Section 9.4: the linked sub-teams are read by id (at most 32),
            // whatever their status, for the parent task card and the children
            // list; this is bounded and only for a team that has children.
            for child in team.children.iter().take(32) {
                if teams.iter().any(|team| &team.id == child) {
                    continue;
                }
                if let Some(child) = store.load(child)? {
                    teams.push(child);
                }
            }
            // U58: cancelled tasks and phases stay out of the status unless the
            // caller asks for them.
            let include_cancelled = input["include_cancelled"].as_bool() == Some(true);
            let mut value = view_with_cancelled(&team, &teams, sessions.as_deref(), include_cancelled);
            // D2: the findings page takes a before cursor.
            value["findings"] = findings_page(&team, input["before"].as_u64());
            // U38: the profile only on request, as hgs team profile gives it.
            if input["profile"].is_object() {
                let mut ask = input["profile"].clone();
                ask["team"] = json!(team.id);
                value["profile"] = profile_view(&ask)?;
            }
            // #756: with the build broker on, each member's build state for
            // task nodes and member cards; the Builds tab reads the rest.
            if team.settings.build_broker {
                value["build_states"] = super::broker::builds_view(&team, now())["members"].clone();
            }
            Ok(value)
        }
        "inbox" => {
            let team = Store::open(false)?.load(text(input, "team"))?.context("Unknown team")?;
            // Spec 5.6: a sub-lead reads its own inbox; you may name one.
            let named = text(input, "member").to_owned();
            let reviewer = if !named.is_empty() {
                named
            } else {
                match actor(&team, "inbox", input) {
                    Ok(Actor::Member(id)) if team.member(&id).is_some_and(|member| member.sub) => id,
                    _ => team.lead.clone(),
                }
            };
            Ok(inbox_view_for(&team, &reviewer))
        }
        "events" => {
            let before = match &input["before"] {
                Value::Null => None,
                value => Some(value.as_i64().filter(|n| *n > 0).context("Invalid feed cursor")?),
            };
            Store::open(false)?.events(text(input, "team"), before, input["limit"].as_i64().unwrap_or(PAGE))
        }
        "spans" => spans_view(input),
        "profile" => profile_view(input),
        // #756: the build broker's state per shared folder, for the Builds tab.
        "builds" => {
            let team = Store::open(false)?.load(text(input, "team"))?.context("Unknown team")?;
            Ok(super::broker::builds_view(&team, super::super::now()))
        }
        "current" => current(input),
        "escalate" => escalate(input),
        op if MUTATIONS.contains(&op) => {
            let request = text(input, "request_id");
            ensure!(canonical_uuid(request), "A canonical request_id is required");
            let team_id = text(input, "team");
            let revision = expected_revision(op, input);
            // A member in a sandbox may not write the team's files: its report
            // waits in its own folder for the dispatcher, which checks it.
            // SQLite may open the store read-only without a word, so the
            // refusal can come with the write as well as with the open.
            let mut store = match Store::open(true) {
                Ok(store) => store,
                Err(error) if queues_instead(op) && read_only(&error) => {
                    return queue_write(op, input, wait)
                }
                // U20: a write the sandbox blocked says what to do, not a bare
                // SQLite error.
                Err(error) => return Err(super::store::write_error(error)),
            };
            // U54: a renamed session's member keeps its team record in step, so
            // commands after the rename match it by name again.
            follow_session_rename(&mut store, team_id, caller_record().as_ref())?;
            let mut filled = input.clone();
            // C1: a caller can never hand in a verified owner word; only the
            // check below fills it, from the lead's own session.
            if let Some(object) = filled.as_object_mut() {
                object.remove("owner_word_checked");
                object.remove("owner_word_event");
                object.remove("owner_word_change");
                object.remove("roster_word");
            }
            // U54: one owner action moves every member whose session really
            // runs on from_account, of the requested agent, the lead included.
            // A same-provider move keeps the conversation through U53's switch;
            // a member whose switch does not confirm stays where it is and the
            // answer names why.
            if op == "swap-member" && !text(&filled, "from_account").is_empty() {
                let from = text(&filled, "from_account").to_owned();
                let to = text(&filled, "to_account").to_owned();
                ensure!(!to.is_empty(), "A bulk swap needs to_account");
                let provider = text(&filled, "provider").to_owned();
                let team = store.load(team_id)?.context("Unknown team")?;
                let sessions = live_sessions()?;
                let ids = bulk_swap_members(&team, &sessions, &from, &provider);
                ensure!(!ids.is_empty(), format!("No member of team {} runs on account {from}", team.name));
                let mut moved = Vec::new();
                let mut fresh = Vec::new();
                let mut notes = Vec::new();
                let mut failed = Vec::new();
                for id in &ids {
                    let member = team.member(id).context("Unknown member")?;
                    let Some(session) = member.session.as_ref().filter(|session| session.host.is_empty() && !session.name.is_empty()) else {
                        failed.push(json!({"member":id,"error":"the member has no local session to move"}));
                        continue;
                    };
                    // A deterministic request id makes a replayed bulk swap
                    // the same U53 request, never a second switch.
                    use sha2::{Digest, Sha256};
                    let digest = Sha256::digest(format!("{}:{}:{}", text(&filled, "request_id"), id, to).as_bytes());
                    let mut bytes = [0u8; 16];
                    bytes.copy_from_slice(&digest[..16]);
                    let request = uuid::Uuid::from_bytes(bytes).to_string();
                    match switch_member_account(&session.name, &to, &request) {
                        Ok(receipt) if receipt["status"] == "switched" => moved.push(id.clone()),
                        Ok(receipt) => match retire_old_session(&session.name, &from) {
                            Ok(note) => {
                                fresh.push(id.clone());
                                notes.push(json!({"member":id,"note":format!("the account switch is {}; {note}", switch_status(&receipt))}));
                            }
                            Err(wait) => failed.push(json!({"member":id,"error":format!("the account switch is {}; {wait}", switch_status(&receipt))})),
                        },
                        Err(error) => match retire_old_session(&session.name, &from) {
                            Ok(note) => {
                                fresh.push(id.clone());
                                notes.push(json!({"member":id,"note":format!("{error}; {note}")}));
                            }
                            Err(wait) => failed.push(json!({"member":id,"error":format!("{error}; {wait}")})),
                        },
                    }
                }
                ensure!(!moved.is_empty() || !fresh.is_empty(), format!("No member of team {} could move to account {to}", team.name));
                fill_bulk_swap(&mut filled, &moved, &fresh, notes, failed, &to);
            }
            if op == "use-last-reply" && text(input, "attempt").is_empty() {
                // Without an attempt, the task's open one.
                let team = store.load(team_id)?.context("Unknown team")?;
                let task = team.task(input["task"].as_u64().unwrap_or_default() as u32).context("Unknown task")?;
                filled["attempt"] = json!(task.open_attempt().context("The task has no open attempt")?.id);
            }
            if op == "use-last-reply" && text(input, "text").trim().is_empty() {
                // The member's last reply becomes the report; reading it is not
                // part of the request's identity.
                let team = store.load(team_id)?.context("Unknown team")?;
                let task = team.task(input["task"].as_u64().unwrap_or_default() as u32).context("Unknown task")?;
                let attempt = task.attempts.iter().find(|a| a.id == text(&filled, "attempt")).context("Unknown attempt")?;
                let session = team.member(&attempt.member).and_then(|m| m.session.clone()).context("The member has no session")?;
                filled["text"] = json!(super::dispatcher::last_reply(&session)?);
            }
            if matches!(op, "plan" | "add-member" | "swap-member" | "draft") {
                // An existing session must run here and be free of another
                // active team; its own folder replaces the planned one (F8).
                // A retry returns here before any listing (L4).
                adopt_existing_sessions(&store, request, input, op, &mut filled)?;
            }
            // Section 9.2: a change that can bind a new session keeps the whole
            // tree within its master's budget (extend binds none, and is
            // checked for a tree already over it).
            if matches!(op, "plan" | "approve" | "add-member" | "scale" | "swap-member" | "extend") {
                let team = store.load(team_id)?.context("Unknown team")?;
                // Bounded: ancestors by id and children by id; a flat team
                // reads nothing beyond the team itself.
                let tree = tree_around(&team, &mut |id| store.load(id))?;
                ops::ensure_tree_sessions(&team, &tree, added_sessions(&team, op, &filled))?;
                if op == "add-member" {
                    // U32: a cap raise on the owner's word stays within the
                    // master's session budget.
                    filled["tree_sessions"] = json!(ops::root_of(&team, &tree).settings.tree_sessions);
                }
            }
            if op == "archive" {
                let team = store.load(team_id)?.context("Unknown team")?;
                ensure_children_closed(&store, &team)?;
            }
            // C6: a sub-team's plan and phases are decided by the parent lead,
            // whose session is proven in the parent team, not in this one.
            let parent_lead = matches!(op, "approve" | "approve-phase" | "discard-phase" | "return-to-draft" | "reject-phase" | "undo-reject-phase") && {
                let team = store.load(team_id)?.context("Unknown team")?;
                parent_lead_calls(&store, &team)?
            };
            if matches!(op, "approve" | "approve-phase") {
                // F13: the lead approves only on the owner's exact sentence,
                // typed in the lead's own session; the CLI checks it here and
                // records the verified quote for the state machine and the feed.
                let team = store.load(team_id)?.context("Unknown team")?;
                let caller = if parent_lead { Actor::ParentLead } else { actor(&team, op, &filled)? };
                let auto_plan = op == "approve" && team.settings.approval == Approval::Auto;
                if matches!(&caller, Actor::Member(member) if *member == team.lead) && !auto_plan {
                    // U56: a refusal names the sentence this approval expects.
                    let phase = (op == "approve-phase").then(|| input["phase"].as_u64().and_then(|n| u32::try_from(n).ok())).flatten();
                    let sentence = super::owner_word::approval_sentence(phase);
                    let word = input.get("owner_word").filter(|word| word.is_object()).with_context(|| {
                        format!("The lead approves only on the owner's word: pass \"owner_word\" with the owner's sentence, which says {sentence}")
                    })?;
                    let verified = super::owner_word::verify(&team, word, phase, super::super::now())?;
                    if let Some(used) = team.owner_word_used(verified.event) {
                        bail!("That word was already used for {used}; ask the owner again");
                    }
                    filled["owner_word_checked"] = serde_json::to_value(&verified)?;
                    filled["owner_word_event"] = json!(verified.event);
                    filled["owner_word_change"] = json!(if op == "approve" { "the plan approval" } else { "the phase approval" });
                }
            }
            // U42 (#587): the lead discards an accepted phase, or returns one to
            // a draft, on the owner's word naming the phase and the change; the
            // CLI checks it here as it does for an approval.
            if matches!(op, "discard-phase" | "return-to-draft") && !parent_lead {
                let team = store.load(team_id)?.context("Unknown team")?;
                let caller = actor(&team, op, &filled)?;
                check_phase_change_word(&team, op, input, &mut filled, &caller, super::owner_word::verify_phase_change)?;
            }
            // U26: a lead changes the roster (or max_members) only on the
            // owner's fresh word, and only for the change that word names.
            if matches!(op, "add-member" | "scale" | "drain" | "swap-member" | "remove-member" | "relaunch" | "settings") {
                let team = store.load(team_id)?.context("Unknown team")?;
                let caller = actor(&team, op, &filled)?;
                check_roster_word(&team, op, input, &mut filled, &caller, |team, input, rule, now| {
                    super::owner_word::verify_roster(team, input, rule, now)
                })?;
            }
            // U29: a task joins a running phase on the owner's word twice.
            if op == "add-to-phase" {
                let team = store.load(team_id)?.context("Unknown team")?;
                let caller = actor(&team, op, &filled)?;
                check_phase_word(&team, input, &mut filled, &caller, super::owner_word::verify_phase)?;
            }
            // N7: a replayed discard does not stop the lead's session twice.
            let replayed = op == "draft"
                && text(&filled, "action") == "discard"
                && store.receipt(request, input)?.is_some();
            let mut receipt = match store.mutate(Some((request, input)), team_id, revision, |team, events| {
                let actor = if parent_lead { Actor::ParentLead } else { actor(team, op, &filled)? };
                ops::apply(team, op, &filled, &actor, &BTreeSet::new(), events)
            }) {
                Ok(receipt) => receipt,
                Err(error) if queues_instead(op) && read_only(&error) => {
                    return queue_write(op, input, wait)
                }
                Err(error) => return Err(super::store::write_error(error)),
            };
            // N7: discarding a draft stops the lead's session when the owner
            // asked for it (spec 3); a failure leaves the discard done and names
            // it in the answer.
            if op == "draft" && text(&filled, "action") == "discard" && receipt["stop_lead"].as_bool() == Some(true) && !replayed {
                let team = store.load(team_id)?.context("Unknown team")?;
                match team.member(&team.lead).and_then(|member| member.session.clone()) {
                    Some(session) => match stop_lead_session(&session) {
                        Ok(()) => receipt["lead_stopped"] = json!(true),
                        Err(error) => receipt["lead_stop_error"] = json!(format!("{error:#}")),
                    },
                    None => receipt["lead_stopped"] = json!(false),
                }
            }
            if starts_worker(op) {
                // The dispatcher runs while any team runs; it exits by itself.
                if let Err(error) = super::dispatcher::ensure_worker() {
                    eprintln!("hgs: the team dispatcher did not start: {error}");
                }
            }
            if receipt["status"] == "stale" {
                bail!("This attempt is no longer current for task #{}; the report was not accepted", receipt["task"]);
            }
            // Sections 9.5 and 9.7: the tree reacts to a finished, rejected,
            // paused, resumed or stopped team, and to a parent answer. N28: the
            // change above is already stored, so a tree hook that fails says so
            // in the receipt instead of turning the stored change into an error.
            if op == "finish" {
                if let Err(error) = file_subteam_report(&mut store, team_id) {
                    receipt["subteam_report_error"] = json!(format!("{error:#}"));
                }
            }
            if op == "reject" {
                if let Err(error) = reopen_rejected_subteam(&mut store, team_id, &filled) {
                    receipt["subteam_reopen_error"] = json!(format!("{error:#}"));
                }
            }
            if matches!(op, "cancel" | "reassign") {
                // C5: the sub-team stops only once the cancel or reassign
                // itself went through for its caller.
                let team = store.load(team_id)?.context("Unknown team")?;
                if let Err(error) = choose_subteam(&mut store, &team, &filled) {
                    receipt["subteam_error"] = json!(format!("{error:#}"));
                }
            }
            if matches!(op, "pause" | "resume" | "stop") {
                if let Err(error) = cascade_status(&mut store, team_id, op, 0) {
                    receipt["cascade_error"] = json!(format!("{error:#}"));
                }
            }
            if op == "reply" {
                if let Err(error) = route_reply_down(&mut store, team_id, &filled) {
                    receipt["reply_down_error"] = json!(format!("{error:#}"));
                }
            }
            Ok(receipt)
        }
        _ => bail!("Unknown team operation {op}"),
    }
}

pub(crate) fn dispatch(args: &[String]) -> Result<i32> {
    if args.len() == 1 && args[0] == "worker" {
        return super::dispatcher::worker();
    }
    // The desktop sends this on an explicit Quit: the worker writes its pid and
    // heartbeat, so it can be ended without touching agents (U2).
    if args.len() == 2 && args[0] == "worker" && args[1] == "--stop" {
        return super::dispatcher::stop_worker();
    }
    // C1: the detached runner of one claimed template run; the worker starts it,
    // hands it the request on standard input and never waits for a command.
    if args.len() == 2 && args[0] == "worker" && args[1] == "--run" {
        return super::dispatcher::worker_run();
    }
    // H2: `--json` reads stdin; `--json-file PATH` reads the same request from
    // a file (PowerShell 5.1 turns Cyrillic in a pipe into "?").
    let mut args = args.to_vec();
    let wait = args.iter().any(|arg| arg == "--wait");
    args.retain(|arg| arg != "--wait");
    let file = if let Some(index) = args.iter().position(|arg| arg == "--json-file") {
        let path = args
            .get(index + 1)
            .cloned()
            .ok_or_else(|| "--json-file needs a path".to_owned())?;
        args.drain(index..=index + 1);
        Some(path)
    } else {
        None
    };
    let catalog = args.first().map(String::as_str) == Some("catalog");
    if catalog {
        args.remove(0);
    }
    if args.len() != 2 || (args[1] != "--json" && file.is_none()) {
        return Err(USAGE.into());
    }
    let operation = args[0].clone();
    let bytes = if let Some(path) = file {
        std::fs::read(&path).map_err(|e| format!("Cannot read {path}: {e}"))?
    } else {
        let mut bytes = Vec::new();
        io::stdin().take(256 * 1024 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        bytes
    };
    if bytes.len() > 256 * 1024 {
        return Err("Team request exceeds 256 KiB".into());
    }
    let input: Value = super::super::json_request(&bytes)?;
    if !input.is_object() {
        return Err("A JSON object is required".into());
    }
    let result = if catalog {
        // N4/N3: the catalog's owner path applies the same agent-session rule.
        let human = catalog_owner(&input).map_err(|error| error.to_string())?;
        super::catalog::run(&operation, &input, human)
    } else {
        run(&operation, &input, wait)
    }
    .map_err(|e| e.to_string())?;
    println!("{result}");
    Ok(0)
}

#[cfg(test)]
#[path = "tests/support_commands.rs"]
mod support;

#[cfg(test)]
#[path = "tests/commands_proposals.rs"]
mod commands_proposals;

#[cfg(test)]
#[path = "tests/commands_owner_word.rs"]
mod commands_owner_word;

#[cfg(test)]
#[path = "tests/commands_runs.rs"]
mod commands_runs;

#[cfg(test)]
#[path = "tests/commands_profile.rs"]
mod commands_profile;

#[cfg(test)]
#[path = "tests/commands_status.rs"]
mod commands_status;

#[cfg(test)]
#[path = "tests/commands_other.rs"]
mod commands_other;
