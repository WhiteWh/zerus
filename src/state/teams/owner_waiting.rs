//! WhiteWh fork: U50, one list of what waits for the owner across every team
//! (docs/superpowers/specs/2026-10-28-team-customization.md section 7). It
//! reads only what the teams record (the attention list, phases, proposals,
//! holds and decisions) and stores nothing; `hgs team waiting --json` pages it
//! newest first.
use super::model::{PhaseState, TaskStatus, Team, TeamStatus};
use serde_json::{json, Value};

/// A page holds at most this many items.
pub(crate) const MAX_PAGE: usize = 200;
const DEFAULT_PAGE: usize = 50;

/// Every item of these teams that waits for the owner, newest first; ties
/// in their stable id order, so a cursor never skips or repeats one.
pub(crate) fn items(teams: &[Team], now: f64) -> Vec<Value> {
    let mut out = Vec::new();
    for team in teams {
        team_items(team, now, &mut out);
    }
    sorted(out)
}

/// Newest first, ties in id order: the page order, also of items from beyond
/// the teams (#742's tool offers).
pub(crate) fn sorted(mut items: Vec<Value>) -> Vec<Value> {
    items.sort_by(|a, b| since(b).total_cmp(&since(a)).then_with(|| id(a).cmp(id(b))));
    items
}

fn since(item: &Value) -> f64 {
    item["since"].as_f64().unwrap_or_default()
}

fn id(item: &Value) -> &str {
    item["id"].as_str().unwrap_or_default()
}

/// One page after the cursor `{since, id}` (the last item of the previous
/// page): items that arrive later are newer and never move an older page.
pub(crate) fn page(items: &[Value], before: Option<&Value>, limit: Option<u64>) -> Value {
    let limit = limit.map_or(DEFAULT_PAGE, |limit| (limit as usize).clamp(1, MAX_PAGE));
    let start = match before.filter(|cursor| cursor.is_object()) {
        None => 0,
        Some(cursor) => {
            let (at, key) = (since(cursor), id(cursor));
            items.iter().position(|item| since(item) < at || (since(item) == at && id(item) > key)).unwrap_or(items.len())
        }
    };
    let shown: Vec<Value> = items[start..].iter().take(limit).cloned().collect();
    let next = (start + shown.len() < items.len())
        .then(|| shown.last().map(|item| json!({"since":item["since"],"id":item["id"]})))
        .flatten();
    json!({"items":shown,"next":next,"total":items.len()})
}

fn item(team: &Team, kind: &str, key: &str, title: String, since: f64, action: &str) -> Value {
    json!({"id":format!("{}/{kind}/{key}", team.id),"team":team.id,"team_name":team.name,"kind":kind,
        "title":title,"since":since,"action":action})
}

fn team_items(team: &Team, now: f64, out: &mut Vec<Value>) {
    match team.status {
        // The plan waits for Approve & Run when the owner reviews it.
        TeamStatus::Planning => {
            if !team.tasks.is_empty() && team.settings.approval == super::model::Approval::Review {
                let tasks = team.tasks.len();
                out.push(item(
                    team,
                    "plan_draft",
                    "plan",
                    format!("Plan of {tasks} task{} waits for Approve & Run", if tasks == 1 { "" } else { "s" }),
                    team.created,
                    "approve_plan",
                ));
            }
            return;
        }
        TeamStatus::Running | TeamStatus::Paused => {}
        _ => return,
    }
    // The attention list already decides when a report or a permission is
    // escalated to the owner, and which draft phase is next.
    for attention in super::inbox::attention(team, now) {
        let at = attention["since"].as_f64().unwrap_or_default();
        let summary = attention["summary"].as_str().unwrap_or_default().to_owned();
        match attention["kind"].as_str().unwrap_or_default() {
            "phase_draft" => {
                let phase = attention["phase"].as_u64().unwrap_or_default();
                let mut found = item(team, "phase_draft", &phase.to_string(), summary, at, "approve_phase");
                found["phase"] = json!(phase);
                out.push(found);
            }
            "report" => {
                let task = attention["task"].as_u64().unwrap_or_default();
                let title = team.task(task as u32).map(|task| task.title.clone()).unwrap_or_default();
                let member = attention["member"].as_str().unwrap_or_default();
                // U56: the summary names the member by id with its role.
                let named = attention["member_id"].as_str().map(|id| team.name_of(id)).unwrap_or_else(|| member.to_owned());
                let mut found = item(
                    team,
                    "report_escalated",
                    &task.to_string(),
                    format!("Report on #{task} {title} from {named} waits for your decision"),
                    at,
                    "decide_report",
                );
                found["task"] = json!(task);
                found["member"] = json!(member);
                out.push(found);
            }
            "permission" => {
                let member = attention["member"].as_str().unwrap_or_default().to_owned();
                let mut found = item(team, "permission_escalated", &member, summary, at, "answer_permission");
                found["member"] = json!(member);
                // #740: the member's id, which the desktop's answer is bound to.
                found["member_id"] = attention["member_id"].clone();
                out.push(found);
            }
            // N17: a replan pauses the team for the owner's word on the proposal.
            "replan" => {
                let finding = attention["finding"].as_str().unwrap_or_default();
                let mut found = item(team, "owner_word_asked", &format!("replan-{finding}"), summary, at, "decide_replan");
                found["finding"] = json!(finding);
                out.push(found);
            }
            _ => {}
        }
    }
    // F8: the lead's open proposals wait for the owner's Accept or Reject.
    for proposal in team.proposals.iter().filter(|proposal| proposal["state"] == "open") {
        let key = proposal["id"].as_str().unwrap_or_default();
        let kind = proposal["kind"].as_str().unwrap_or("change").replace('-', " ");
        let reason = proposal["reason"].as_str().unwrap_or_default();
        // #740: a question names itself.
        let asked = proposal["change"]["question"].as_str().filter(|_| proposal["kind"] == "question");
        let title = match asked {
            Some(question) => format!("The lead asks: {}", question.split_whitespace().collect::<Vec<_>>().join(" ")),
            None if reason.is_empty() => format!("The lead proposes: {kind}"),
            None => format!("The lead proposes: {kind} \u{2014} {reason}"),
        };
        let mut found = item(team, "owner_word_asked", &format!("proposal-{key}"), title, proposal["at"].as_f64().unwrap_or_default(), "decide_proposal");
        found["proposal"] = json!(key);
        out.push(found);
    }
    // U29: an addition to a running phase waits for the owner's confirmation
    // until it expires; an expired one can no longer be confirmed and is not
    // listed (it leaves the team on its next change).
    for addition in team.pending_additions.iter().filter(|addition| super::ops::addition_fresh(addition, now)) {
        let count = addition.tasks.len();
        let mut found = item(
            team,
            "owner_word_asked",
            &format!("addition-{}", addition.id),
            format!("Adding {count} task{} to running phase {} waits for your confirmation", if count == 1 { "" } else { "s" }, addition.phase),
            addition.warned_at,
            "confirm_addition",
        );
        found["phase"] = json!(addition.phase);
        out.push(found);
    }
    // A5: a task the owner held waits for the owner's release.
    for task in team.tasks.iter().filter(|task| !task.status.finished()) {
        let Some(hold) = task.hold.as_ref().filter(|hold| hold.by == "You") else { continue };
        let reason = if hold.reason.is_empty() { String::new() } else { format!(": {}", hold.reason) };
        let mut found = item(team, "task_waits_owner", &task.id.to_string(), format!("#{} {} is held by you{reason}", task.id, task.title), hold.at, "release_task");
        found["task"] = json!(task.id);
        out.push(found);
    }
    // #581 N7, spec 7: a task waiting for a step Zerus could not take (U45
    // model, U46 context, U48 branch) waits for the lead or the owner, with
    // its reason and the retry that starts the step again, as in the lead's
    // inbox. A context action that still runs waits for nobody.
    for task in team.tasks.iter().filter(|task| task.status == TaskStatus::Pending) {
        let Some(wait) = task.waits.as_ref().filter(|wait| !wait.running()) else { continue };
        let mut found = item(
            team,
            "task_waits_owner",
            &format!("waits-{}", task.id),
            format!("#{} {} waits: {}", task.id, task.title, wait.reason),
            wait.at,
            "retry_task",
        );
        found["task"] = json!(task.id);
        found["reason"] = json!(wait.reason);
        found["waits_for"] = json!(wait.waits_for);
        out.push(found);
    }
    // #718: the proposal of a full test run is the owner's call too, and a
    // flaky suite's quarantine shows the owner until it expires.
    if let Some(proposal) = &team.test_full_proposal {
        let mut found = item(team, "test_full_run", &format!("{}", proposal.at as i64), super::inbox::full_run_text(proposal), proposal.at, "open_team");
        found["seconds"] = json!(proposal.seconds);
        found["reused"] = json!(proposal.reused);
        out.push(found);
    }
    for quarantine in team.test_quarantine.iter().filter(|quarantine| quarantine.until > now) {
        let until = chrono::DateTime::from_timestamp(quarantine.until as i64, 0).map(|time| time.format("%Y-%m-%d").to_string()).unwrap_or_default();
        let title = format!("{} is quarantined as flaky ({} episode(s)) until {until}", quarantine.suite, quarantine.episodes);
        out.push(item(team, "test_quarantine", &quarantine.suite, title, quarantine.since, "open_team"));
    }
    // Every task decided: the team waits for the owner (or the lead) to finish
    // it or add the next phase; no draft phase is waiting.
    if super::ops::all_decided(team) && !team.phases.iter().any(|phase| phase.state == PhaseState::Draft) {
        let decided = team
            .tasks
            .iter()
            .flat_map(|task| task.attempts.iter().filter_map(|attempt| attempt.decided_at))
            .fold(0.0_f64, f64::max);
        let tasks = team.tasks.len();
        out.push(item(
            team,
            "team_finished",
            "finish",
            format!("All {tasks} task{} decided: waits for you to finish", if tasks == 1 { "" } else { "s" }),
            if decided > 0.0 { decided } else { team.updated },
            "finish",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::teams::model::{Attempt, AttemptState, Hold, Member, PendingAddition, Phase, Report, Task, TaskWait};

    fn team(id: &str, status: TeamStatus) -> Team {
        Team {
            id: id.into(),
            name: format!("Team {id}"),
            status,
            lead: "lead".into(),
            created: 100.0,
            updated: 100.0,
            members: vec![
                Member { id: "lead".into(), role: "Lead".into(), ..Default::default() },
                Member { id: "dev".into(), role: "Backend".into(), ..Default::default() },
            ],
            tasks: vec![Task { id: 1, title: "API".into(), assignee: "dev".into(), ..Default::default() }],
            ..Default::default()
        }
    }

    fn kinds(items: &[Value]) -> Vec<&str> {
        items.iter().map(|item| item["kind"].as_str().unwrap()).collect()
    }

    /// One fixture per kind, across several teams, each naming its team.
    #[test]
    fn every_kind_across_teams_names_its_team_newest_first() {
        // Within the addition's confirmation window (warned at 4500).
        let now = 6_200.0;
        let planning = team("t-plan", TeamStatus::Planning);

        let mut phased = team("t-phase", TeamStatus::Running);
        phased.tasks.push(Task { id: 2, phase: 2, title: "UI".into(), ..Default::default() });
        phased.phases.push(Phase { n: 2, title: "Screens".into(), state: PhaseState::Draft, tasks: vec![2], added_at: 1000.0, ..Default::default() });

        let mut reported = team("t-report", TeamStatus::Running);
        reported.settings.remind_owner_minutes = 30;
        reported.tasks[0].status = TaskStatus::Submitted;
        reported.tasks[0].attempts.push(Attempt {
            n: 1,
            member: "dev".into(),
            state: AttemptState::Reported,
            report: Some(Report { text: "Done".into(), at: 2000.0, ..Default::default() }),
            ..Default::default()
        });

        let mut asking = team("t-perm", TeamStatus::Running);
        asking.settings.remind_owner_permission_minutes = 5;
        asking.members[1].pending = Some(super::super::model::PendingQuestion { at: 3000.0, ..Default::default() });

        let mut proposing = team("t-word", TeamStatus::Paused);
        proposing.proposals.push(json!({"id":"p1","kind":"add-member","state":"open","at":4000.0,"reason":"More tests"}));
        proposing.proposals.push(json!({"id":"p0","kind":"remove-member","state":"accepted","at":10.0}));
        proposing.pending_additions.push(PendingAddition { id: "a1".into(), phase: 1, tasks: vec![Task::default()], warned_at: 4500.0, ..Default::default() });

        let mut held = team("t-held", TeamStatus::Running);
        held.tasks[0].hold = Some(Hold { by: "You".into(), at: 5000.0, reason: "After the release".into() });
        held.tasks.push(Task { id: 2, title: "Lead's".into(), hold: Some(Hold { by: "Lead".into(), at: 5100.0, ..Default::default() }), ..Default::default() });

        let mut finished = team("t-done", TeamStatus::Running);
        finished.tasks[0].status = TaskStatus::Accepted;
        finished.tasks[0].attempts.push(Attempt { n: 1, state: AttemptState::Reported, decided_at: Some(6000.0), ..Default::default() });

        // Nothing waits for the owner here: a running team at work, a stopped
        // team and an archived one.
        let quiet = team("t-quiet", TeamStatus::Running);
        let mut stopped = team("t-stop", TeamStatus::Stopped);
        stopped.tasks[0].hold = Some(Hold { by: "You".into(), at: 9000.0, ..Default::default() });

        let teams = vec![planning, phased, reported, asking, proposing, held, finished, quiet, stopped];
        let list = items(&teams, now);
        assert_eq!(
            kinds(&list),
            ["team_finished", "task_waits_owner", "owner_word_asked", "owner_word_asked", "permission_escalated", "report_escalated", "phase_draft", "plan_draft"],
            "{list:#?}"
        );
        for item in &list {
            let team = item["team"].as_str().unwrap();
            assert_eq!(item["team_name"], format!("Team {team}"));
            assert!(!item["title"].as_str().unwrap().is_empty() && !item["action"].as_str().unwrap().is_empty(), "{item}");
        }
        assert_eq!(list[0]["since"], 6000.0);
        assert_eq!(list[1]["task"], 1);
        assert!(list[1]["title"].as_str().unwrap().contains("After the release"));
        assert_eq!((list[2]["action"].as_str(), list[3]["proposal"].as_str()), (Some("confirm_addition"), Some("p1")));
        assert_eq!(list[4]["member"], "Backend");
        assert_eq!((list[5]["task"].as_u64(), list[5]["member"].as_str()), (Some(1), Some("Backend")));
        assert_eq!(list[6]["phase"], 2);
        assert_eq!((list[7]["team"].as_str(), list[7]["action"].as_str()), (Some("t-plan"), Some("approve_plan")));
    }

    /// U29: an addition past its confirmation window is not listed: it can
    /// no longer be confirmed, and it leaves on the team's next change.
    #[test]
    fn an_expired_addition_is_not_listed() {
        let mut asking = team("t", TeamStatus::Running);
        asking.pending_additions.push(PendingAddition { id: "a1".into(), phase: 1, tasks: vec![Task::default()], warned_at: 1000.0, ..Default::default() });
        assert_eq!(kinds(&items(std::slice::from_ref(&asking), 1000.0 + 60.0)), ["owner_word_asked"]);
        let expired = 1000.0 + super::super::owner_word::FRESH_SECONDS + 1.0;
        assert!(items(&[asking], expired).is_empty());
    }

    /// A report or permission is the owner's only once the attention list
    /// escalates it; an auto-approved plan never waits.
    #[test]
    fn nothing_waits_before_it_is_escalated() {
        let mut reported = team("t", TeamStatus::Running);
        reported.settings.remind_owner_minutes = 30;
        reported.tasks[0].status = TaskStatus::Submitted;
        reported.tasks[0].attempts.push(Attempt {
            n: 1,
            state: AttemptState::Reported,
            report: Some(Report { text: "Done".into(), at: 1000.0, ..Default::default() }),
            ..Default::default()
        });
        let mut auto = team("a", TeamStatus::Planning);
        auto.settings.approval = super::super::model::Approval::Auto;
        let empty = Team { tasks: Vec::new(), ..team("e", TeamStatus::Planning) };
        assert!(items(&[reported.clone(), auto, empty], 1000.0 + 60.0).is_empty());
        assert_eq!(kinds(&items(&[reported], 1000.0 + 31.0 * 60.0)), ["report_escalated"]);
    }

    /// #581 N7: a task whose step Zerus could not take waits for the owner
    /// too, with its reason and the retry; one whose context action still
    /// runs, or that is no longer pending, does not.
    #[test]
    fn a_waiting_task_is_listed_with_its_reason_and_the_retry() {
        let wait = |state: &str, at: f64| TaskWait {
            waits_for: "zerus".into(),
            on: vec!["model".into()],
            reason: "hgs: unknown model gpt-5.5 for this account".into(),
            at,
            state: state.into(),
            ..Default::default()
        };
        let mut waiting = team("t", TeamStatus::Running);
        waiting.tasks[0].waits = Some(wait("refused", 700.0));
        waiting.tasks.push(Task { id: 2, title: "Docs".into(), waits: Some(wait("waiting", 800.0)), ..Default::default() });
        waiting.tasks.push(Task { id: 3, title: "Old".into(), status: TaskStatus::Accepted, waits: Some(wait("uncertain", 900.0)), ..Default::default() });
        waiting.tasks[0].hold = Some(Hold { by: "You".into(), at: 600.0, ..Default::default() });
        let list = items(&[waiting], 1000.0);
        assert_eq!(kinds(&list), ["task_waits_owner", "task_waits_owner"], "{list:#?}");
        let found = &list[0];
        assert_eq!((found["id"].as_str(), found["task"].as_u64(), found["action"].as_str()), (Some("t/task_waits_owner/waits-1"), Some(1), Some("retry_task")));
        assert_eq!((found["since"].as_f64(), found["waits_for"].as_str()), (Some(700.0), Some("zerus")));
        assert_eq!(found["reason"], "hgs: unknown model gpt-5.5 for this account");
        assert_eq!(found["title"], "#1 API waits: hgs: unknown model gpt-5.5 for this account");
        // The owner's hold on the same task stays its own item.
        assert_eq!((list[1]["id"].as_str(), list[1]["action"].as_str()), (Some("t/task_waits_owner/1"), Some("release_task")));
    }

    #[test]
    fn an_empty_list_is_one_empty_page() {
        let page = page(&items(&[], 0.0), None, None);
        assert_eq!(page, json!({"items":[],"next":null,"total":0}));
    }

    /// Paging stays stable while items arrive: a newer item lands before the
    /// cursor and the next page neither repeats nor skips an older one; equal
    /// times keep their id order.
    #[test]
    fn pages_stay_stable_while_items_arrive() {
        let held = |n: u32, at: f64| {
            let mut team = team(&format!("t{n:02}"), TeamStatus::Running);
            team.tasks[0].hold = Some(Hold { by: "You".into(), at, ..Default::default() });
            team
        };
        // Two items share a time.
        let mut teams: Vec<Team> = (0..7).map(|n| held(n, 100.0 + f64::from(n / 2 * 2))).collect();
        let all = items(&teams, 0.0);
        let first = page(&all, None, Some(3));
        assert_eq!(first["total"], 7);
        assert_eq!(first["items"].as_array().unwrap().len(), 3);
        let cursor = first["next"].clone();
        // New items arrive, one newer than everything, one at the cursor's time.
        teams.push(held(50, 500.0));
        teams.push(held(51, cursor["since"].as_f64().unwrap()));
        let arrived = items(&teams, 0.0);
        let mut seen: Vec<String> = first["items"].as_array().unwrap().iter().map(|item| id(item).to_owned()).collect();
        let mut next = Some(cursor);
        while let Some(cursor) = next.filter(|cursor| !cursor.is_null()) {
            let page = page(&arrived, Some(&cursor), Some(3));
            seen.extend(page["items"].as_array().unwrap().iter().map(|item| id(item).to_owned()));
            next = Some(page["next"].clone());
        }
        let original: Vec<String> = all.iter().map(|item| id(item).to_owned()).collect();
        // Every original item exactly once, in the original order; the newer
        // arrival is not on the old pages.
        let kept: Vec<String> = seen.iter().filter(|id| original.contains(id)).cloned().collect();
        assert_eq!(kept, original);
        assert!(!seen.iter().any(|id| id.starts_with("t50/")), "{seen:?}");
        assert_eq!(page(&arrived, None, Some(1))["items"][0]["team"], "t50");
        // A page never exceeds the bound.
        assert_eq!(page(&arrived, None, Some(10_000))["items"].as_array().unwrap().len(), arrived.len().min(MAX_PAGE));
    }
}
