//! Task 139 (owner, 2026-11-07, the Adventure team): `hgs team status` often
//! named a member's state wrong (working while it waited on an approval, a
//! Kimi prompt not read, a delivery left in the composer), and the lead had to
//! be kicked by hand. Each source's view of a member is compared:
//!
//! - hooks: the session record the native hooks and events keep (activity,
//!   phase, a question or approval it reported);
//! - record: a request Zerus read from the session (`pending`,
//!   `terminal_question`);
//! - screen: the Terminal's recognised panel (a question or approval, a draft
//!   in the composer, an idle prompt, a working spinner);
//! - process: whether its process runs, its own jobs and Zerus builds;
//! - activity: the time of its last event against the stuck bound.
//!
//! Sources that disagree beyond `GRACE` make the member `uncertain`, with the
//! disagreeing views named; it is never shown as a confident `working`. A tool
//! call that is open (Claude's `active_tools`, a DeepSeek session's PreToolUse
//! with no result after it) and a Zerus build the member waits for are work,
//! held to the tool bound, never silence (task 181). The
//! screen is read only for a suspect member, at most `MAX_CHECKS` at a time and
//! off the dispatcher's tick. `hgs team look <member>` gives the same compact
//! snapshot in one call, and an uncertain member is told to the lead once.
use super::model::{AttemptState, LeadStall, Member, MemberCheck, MemberState, Team};
use super::plan;
use serde_json::{json, Value};

/// Sources must disagree this long (two reads) before the member is uncertain.
pub(crate) const GRACE: f64 = 60.0;
/// A member is suspect after this long busy without an event, idle with an
/// unreported hand-out, or asking without a request Zerus read.
pub(crate) const SUSPECT_AFTER: f64 = 180.0;
/// A suspect member's screen is read at most this often; a disagreement is
/// read again after `RECHECK` to confirm or clear it.
pub(crate) const CHECK_EVERY: f64 = 180.0;
pub(crate) const RECHECK: f64 = 60.0;
/// At most this many screens are read at a time, in all teams.
pub(crate) const MAX_CHECKS: usize = 2;
/// The snapshot keeps the screen's last this many non-blank lines.
pub(crate) const SCREEN_LINES: usize = 12;
/// Task 181: a finished turn (an idle prompt on the screen, no agent rows) the
/// hooks still call working is settled in the record once the session has been
/// silent this long and two reads agree.
pub(crate) const SETTLE_AFTER: f64 = 120.0;

/// What the session record says (its native hooks and events).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Hooks {
    pub live: bool,
    pub activity: String,
    pub phase: String,
    pub asking: bool,
    pub background: bool,
    pub building: bool,
    /// Task 36: the label of the build Zerus runs for the session, empty when
    /// it is not building (or has none).
    pub build_label: String,
    pub tool: String,
    /// Task 181: when the open tool call began (Claude's record, or a DeepSeek
    /// session's PreToolUse with no result after it).
    pub tool_since: Option<f64>,
    /// Task 278: the jobs the listing registers for it (its Zerus build, a
    /// background job its hooks registered, Background work under a budget).
    pub jobs: Vec<WaitJob>,
    pub last_event_at: Option<f64>,
    /// Seconds since its last progress, and the stuck bound it is held to.
    pub quiet: f64,
    pub limit: f64,
}

impl Hooks {
    pub(crate) fn busy(&self) -> bool {
        self.activity == "busy" || matches!(self.phase.as_str(), "working" | "tool" | "compacting")
    }
    pub(crate) fn asks(&self) -> bool {
        self.asking || matches!(self.phase.as_str(), "question" | "approval" | "input")
    }
    /// The hooks' own verdict.
    pub(crate) fn view(&self) -> &'static str {
        if !self.live {
            "stopped"
        } else if self.asks() {
            "asking"
        } else if self.busy() {
            "working"
        } else if self.activity == "idle" || self.phase == "idle" {
            "idle"
        } else {
            "unknown"
        }
    }
}

/// Task 278: a job a member waits on, and since when.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct WaitJob {
    pub what: String,
    pub since: f64,
}

/// "14:05" for a start time, empty when it is unknown.
pub(crate) fn clock(at: f64) -> String {
    use chrono::TimeZone;
    if at <= 0.0 {
        return String::new();
    }
    chrono::Local.timestamp_opt(at as i64, 0).single().map(|time| time.format("%H:%M").to_string()).unwrap_or_default()
}

/// Task 278: "waiting for <job> since T" in words, the first registered job
/// and how many more.
pub(crate) fn job_words(jobs: &[WaitJob]) -> Option<String> {
    let first = jobs.first()?;
    let since = clock(first.since);
    let mut text = if since.is_empty() { first.what.clone() } else { format!("{} since {since}", first.what) };
    if jobs.len() > 1 {
        text += &format!(" (and {} more)", jobs.len() - 1);
    }
    Some(text)
}

/// The Terminal's recognised panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Screen {
    /// A turn runs: a spinner or "esc to interrupt".
    Working,
    /// A question or approval panel waits for a choice.
    Asking,
    /// The agent's empty prompt.
    Prompt,
    /// The composer holds an unsent paste.
    Draft,
    Unknown,
}

impl Screen {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Screen::Working => "working",
            Screen::Asking => "asking",
            Screen::Prompt => "idle_prompt",
            Screen::Draft => "draft",
            Screen::Unknown => "unknown",
        }
    }
}

const SPINNER_GLYPHS: [char; 16] = ['·', '✢', '✳', '✶', '✻', '✽', '*', '⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠏'];

fn working_row(row: &str) -> bool {
    let lower = row.to_lowercase();
    if lower.contains("esc to interrupt") || lower.contains("ctrl+c to interrupt") || lower.contains("ctrl-c to interrupt") {
        return true;
    }
    let trimmed = row.trim_start();
    // Codex: "• Working (12s • esc to interrupt)"; Claude: "✢ Actualizing… (3s · ↓ 52 tokens)";
    // a braille spinner: "⠋ Thinking..." (Kimi, DeepSeek).
    let mut chars = trimmed.chars();
    let glyph = chars.next().is_some_and(|first| SPINNER_GLYPHS.contains(&first)) && chars.next() == Some(' ');
    glyph && (trimmed.contains('…') || trimmed.contains("...")) && trimmed.chars().count() <= 160
}

/// Claude's rows for work that outlives an empty prompt: the agent list under
/// the prompt ("◯ general-purpose  Review the diff · 12.3k tokens") and the
/// footer's "N shells still running". The prompt is empty; the member works.
fn work_row(row: &str) -> bool {
    let trimmed = row.trim().trim_start_matches('│').trim_start();
    let lower = trimmed.to_lowercase();
    let agent = trimmed.starts_with(['\u{25ef}', '\u{25cb}', '\u{25c9}', '\u{25ce}']) && lower.contains("token");
    agent || lower.contains("shell still running") || lower.contains("shells still running")
}

/// The question row of a choice panel: the nearest row above that ends with '?'.
fn question_above(rows: &[String], at: usize) -> String {
    rows[..=at]
        .iter()
        .rev()
        .map(|row| row.trim().trim_start_matches(['▶', '?', '●']).trim())
        .find(|row| row.ends_with('?'))
        .unwrap_or_else(|| rows[at].trim())
        .chars()
        .take(160)
        .collect()
}

fn asking_row(row: &str) -> bool {
    let trimmed = row.trim();
    let lower = trimmed.to_lowercase();
    // The choice panels of Claude ("❯ 1. Yes"), Kimi ("▶ 1. Approve once"), Codex
    // ("› 1. Yes, proceed") and DeepSeek, and their key hints.
    let choice = ["❯ 1.", "▶ 1.", "› 1.", "> 1.", "→ [1]"].iter().any(|start| trimmed.starts_with(start));
    choice
        || lower.contains("esc to cancel")
        || lower.contains("↵ confirm")
        || lower.contains("enter to select")
        || lower.contains("enter to confirm")
        || lower.starts_with("do you want to")
        || lower.starts_with("would you like to run")
        || lower.contains("allow command?")
        || lower.ends_with("[y/n]")
        || lower.ends_with("(y/n)")
}

fn prompt_row(agent: &str, row: &str) -> Option<String> {
    let trimmed = row.trim_start();
    let boxed = trimmed.strip_prefix('│').map(str::trim_start).unwrap_or(trimmed);
    let tail = match agent {
        "codex" => boxed.strip_prefix('›').or_else(|| boxed.strip_prefix('»')),
        "kimi" => boxed.strip_prefix('>'),
        // Qwen Code's mode glyph: `>` default, `*` YOLO, `!` shell.
        "qwen" => boxed.strip_prefix("> ").or_else(|| boxed.strip_prefix("* ")),
        _ => boxed.strip_prefix('❯').or_else(|| boxed.strip_prefix('>')).or_else(|| boxed.strip_prefix('›')),
    }?;
    let tail = tail.trim_matches(|ch: char| ch.is_whitespace() || ch == '\u{a0}' || ch == '│');
    Some(tail.to_owned())
}

/// The panel on the screen's last rows (plain text, blank rows dropped).
pub(crate) fn classify(agent: &str, rows: &[String]) -> (Screen, String) {
    let tail: Vec<String> = rows[rows.len().saturating_sub(25)..].to_vec();
    if let Some(at) = tail.iter().rposition(|row| asking_row(row)) {
        return (Screen::Asking, question_above(&tail, at));
    }
    if let Some(row) = tail.iter().rev().find(|row| working_row(row)) {
        return (Screen::Working, row.trim().chars().take(160).collect());
    }
    // Task 181: running subagents or shells make an empty prompt a working one.
    if let Some(row) = tail.iter().rev().find(|row| work_row(row)) {
        return (Screen::Working, row.trim().chars().take(160).collect());
    }
    for row in tail.iter().rev().take(8) {
        if let Some(text) = prompt_row(agent, row) {
            if text.contains("[Pasted text") || text.starts_with("[Image") {
                return (Screen::Draft, text.chars().take(160).collect());
            }
            return (Screen::Prompt, String::new());
        }
    }
    (Screen::Unknown, String::new())
}

/// Whether the member is suspect, and if so whether its screen is due to be
/// read now: None, not suspect (its last check goes); Some(due) otherwise.
/// Suspect: busy without an event for `SUSPECT_AFTER`; asking without a
/// request Zerus read for that long; idle with a hand-out delivered that long
/// ago and no report; or a disagreement still open.
pub(crate) fn due(team: &Team, member: &Member, hooks: &Hooks, now: f64) -> Option<bool> {
    if member.id == team.lead || member.state != MemberState::Active || member.session.is_none() {
        return None;
    }
    let unreported = team.tasks.iter().any(|task| {
        task.open_attempt().is_some_and(|attempt| {
            attempt.member == member.id
                && attempt.state == AttemptState::Delivered
                && attempt.delivered_at.is_some_and(|at| now - at >= SUSPECT_AFTER)
        })
    });
    let quiet = hooks.quiet >= SUSPECT_AFTER;
    let unread = hooks.asks() && member.pending.is_none() && member.terminal_question.is_none();
    let open = member.check.as_ref().is_some_and(|check| check.state != "agree");
    // Task #269: a member waiting on its own Zerus build, Background work or a
    // tool call is quiet for a reason; it is not suspect.
    let explained = waiting(hooks).is_some();
    let suspect = (hooks.busy() && !hooks.asks() && quiet && !explained)
        || (unread && quiet)
        || (!hooks.busy() && !hooks.asks() && !hooks.background && !hooks.building && hooks.jobs.is_empty() && unreported)
        || open;
    if !suspect {
        return None;
    }
    let every = if open { RECHECK } else { CHECK_EVERY };
    Some(member.check.as_ref().is_none_or(|check| now - check.at >= every))
}

/// Task #269: what a quiet member is waiting on, when its own work explains
/// the quiet: a Zerus build (`hgs team run`, `hgs run`), Background work (a
/// job, subagents, a declared budget), or a tool call in flight. The
/// dispatcher's stuck rule already exempts the first two; the look must not
/// call them uncertain either, and names the wait instead.
pub(crate) fn waiting(hooks: &Hooks) -> Option<String> {
    // Task 278: a registered job names itself and its start.
    if let Some(words) = job_words(&hooks.jobs) {
        return Some(words);
    }
    if hooks.building {
        let label = hooks.build_label.trim();
        return Some(if label.is_empty() {
            "its own Zerus build".into()
        } else {
            format!("its own Zerus build ({label})")
        });
    }
    if hooks.background {
        return Some("Background work in flight (a job, subagents or a declared budget)".into());
    }
    let tool = hooks.tool.trim();
    if !tool.is_empty() {
        return Some(format!("a tool call ({tool})"));
    }
    None
}

/// The idle lead's message (task 95's stall, told to the lead itself): what
/// waits for its decision, oldest first, at most five.
pub(crate) fn lead_idle_text(team: &Team, stall: &LeadStall, now: f64) -> String {
    let minutes = ((now - stall.oldest).max(0.0) / 60.0).round() as u64;
    // Task #289: the nag names the decisions the stall counts, never a notice
    // (a decided proposal, a cancelled dependency, an unread count, a digest).
    let mut items: Vec<super::inbox::Item> = super::inbox::items(team).into_iter().filter(super::lead_stall::decision).collect();
    items.sort_by(|a, b| a.since.total_cmp(&b.since));
    let mut text = format!(
        "[Zerus team \u{b7} from: Zerus \u{b7} look]\nYou are idle while {} decision(s) wait, the oldest for {minutes} min:\n",
        stall.items
    );
    for item in items.iter().take(5) {
        text += &format!("  - {}\n", item.summary);
    }
    text += &format!(
        "Decide them now, or say what blocks you. Status: {} | hgs team status --json",
        json!({"team":team.id})
    );
    text
}

/// The verdict of comparing the sources: the reconciled state, the
/// disagreements in words and every source's view.
pub(crate) struct Compared {
    /// The comparison's own state (task 269's tests read it; the listing does
    /// not need it, so nothing else may assume it).
    #[cfg_attr(not(test), allow(dead_code))]
    pub state: String,
    pub disagree: Vec<String>,
    pub sources: Value,
    /// Task #269: what the member waits on (its own build, Background work or a
    /// tool call), when that explains a quiet hook.
    #[cfg_attr(not(test), allow(dead_code))]
    pub waiting: Option<String>,
}

/// Compares the views. `record` names a request Zerus read from the session;
/// `screen` is None when the screen could not be read.
pub(crate) fn compare(hooks: &Hooks, record: Option<&str>, screen: Option<(Screen, &str)>) -> Compared {
    let said = hooks.view();
    let mut disagree = Vec::new();
    let minutes = |seconds: f64| (seconds.max(0.0) / 60.0).round() as u64;
    // Task #269: the member is waiting on its own build, Background work or a
    // tool call; a quiet hook is expected and is named, never uncertain. The
    // build and Background work are exempt like the dispatcher's stuck rule;
    // a tool call gets the dispatcher's longer tool bound (`limit`).
    // Task 181: a member inside one long tool call is held to the tool bound
    // (`limit` is already that bound then).
    let hard = hooks.building || hooks.background || !hooks.jobs.is_empty();
    let waiting = waiting(hooks);
    if said == "working" && hooks.limit > 0.0 && hooks.quiet >= hooks.limit && !hard {
        disagree.push(format!(
            "activity: no session event for {} min while hooks say working (bound {} min)",
            minutes(hooks.quiet),
            minutes(hooks.limit)
        ));
    }
    if let Some((kind, detail)) = screen {
        let shown = |kind: Screen| match kind {
            Screen::Asking => format!("a question or approval panel ({detail})"),
            Screen::Working => "a working spinner".to_owned(),
            Screen::Prompt => "an idle prompt".to_owned(),
            Screen::Draft => format!("an unsent draft in the composer ({detail})"),
            Screen::Unknown => String::new(),
        };
        let clash = match (said, kind) {
            (_, Screen::Unknown) => false,
            // While a build, job or tool runs, the agent is normally back at its
            // prompt; the wait is not a disagreement. An asking panel stays one.
            ("working", Screen::Prompt | Screen::Draft) => waiting.is_none(),
            ("working", Screen::Asking) => true,
            ("asking", Screen::Prompt | Screen::Working | Screen::Draft) => true,
            ("idle", Screen::Working | Screen::Draft) => true,
            ("idle", Screen::Asking) => record.is_none(),
            ("stopped", Screen::Working | Screen::Asking) => true,
            _ => false,
        };
        if clash {
            disagree.push(format!("hooks say {said}; the screen shows {}", shown(kind)));
        }
        if record.is_some() && matches!(kind, Screen::Prompt | Screen::Working) && said != "asking" {
            disagree.push(format!("a request is recorded; the screen shows {}", shown(kind)));
        }
    }
    let state = if !disagree.is_empty() {
        "uncertain".to_owned()
    } else if said == "unknown" {
        screen.map(|(kind, _)| kind.name().to_owned()).unwrap_or_else(|| "unknown".into())
    } else {
        said.to_owned()
    };
    let sources = json!({
        "hooks":{"view":said,"activity":hooks.activity,"phase":hooks.phase,"asking":hooks.asking,"tool":hooks.tool,
            "tool_since":hooks.tool_since,"building":hooks.building,"build_label":hooks.build_label,"last_event_at":hooks.last_event_at},
        "record":{"view":if record.is_some() { "asking" } else { "none" },"request":record},
        "screen":screen.map(|(kind, detail)| json!({"view":kind.name(),"detail":detail})).unwrap_or(json!({"view":"unread"})),
        "process":{"view":if hooks.live { "running" } else { "stopped" },"background":hooks.background,"building":hooks.building},
        "waiting":waiting,
        "activity":{"minutes_quiet":minutes(hooks.quiet),"stuck_after_minutes":minutes(hooks.limit)},
    });
    Compared { state, disagree, sources, waiting }
}

/// Task 181: a DeepSeek session's open tool call: the oldest PreToolUse of
/// `record["events"]` that no PostToolUse (the same tool's, else the oldest
/// open one) or turn boundary (a message, a prompt, a Stop) followed, as
/// (tool, started). The listing does not show it: its `last_event_at` does not
/// advance with the native tool events.
pub(crate) fn open_tool(record: &Value) -> Option<(String, f64)> {
    let mut events: Vec<&Value> = record["events"].as_array()?.iter().collect();
    events.sort_by_key(|event| event["seq"].as_i64().unwrap_or(0));
    let mut open: Vec<(String, f64)> = Vec::new();
    for event in events {
        let at = event["at"].as_f64().unwrap_or(0.0);
        match event["type"].as_str().unwrap_or_default() {
            "PreToolUse" if at > 0.0 => open.push((event["tool"].as_str().unwrap_or_default().to_owned(), at)),
            "PostToolUse" => {
                let tool = event["tool"].as_str().unwrap_or_default();
                let at = open.iter().position(|(name, _)| name == tool).unwrap_or(0);
                if at < open.len() {
                    open.remove(at);
                }
            }
            "AgentMessage" | "UserPromptSubmit" | "Stop" | "StopFailure" => open.clear(),
            _ => {}
        }
    }
    open.into_iter().next()
}

/// Folds an open tool call into the hooks' view: the call's start is progress
/// (quiet counts from it) and its bound replaces the stuck bound.
pub(crate) fn note_open_tool(hooks: &mut Hooks, open: Option<(String, f64)>, bound: f64, now: f64) {
    let Some((tool, since)) = open else { return };
    hooks.tool = tool;
    hooks.tool_since = Some(since);
    hooks.quiet = hooks.quiet.min((now - since).max(0.0));
    hooks.limit = hooks.limit.max(bound);
}

/// Task 181: whether this read settles a finished turn the hooks still call
/// working. A hook-driven agent (a DeepSeek session's state is its host's, not a
/// record), the hooks say working with no request recorded, the screen shows
/// an idle prompt (no spinner, no agent row, no shell, no draft; a wedged
/// subagent counts as Background work, so that alone never blocks it), the session
/// has been silent for `SETTLE_AFTER`, and an earlier read at least `GRACE` ago
/// showed the same clash.
pub(crate) fn settle_due(agent: &str, hooks: &Hooks, record: Option<&str>, screen: Option<(Screen, &str)>, previous: Option<&MemberCheck>, now: f64) -> bool {
    if agent == "dsh" || record.is_some() || hooks.view() != "working" || hooks.building {
        return false;
    }
    if !matches!(screen, Some((Screen::Prompt, _))) || hooks.quiet < SETTLE_AFTER {
        return false;
    }
    previous.is_some_and(|check| {
        check.state != "agree"
            && check.since.is_some_and(|since| now - since >= GRACE)
            && check.disagree.iter().any(|line| line.starts_with("hooks say working; the screen shows an idle prompt"))
    })
}

/// The member's next check record from this comparison: a disagreement
/// becomes `uncertain` only once it lasted `GRACE`; agreement ends it.
pub(crate) fn next_check(previous: Option<&MemberCheck>, compared: &Compared, screen: Vec<String>, now: f64) -> MemberCheck {
    if compared.disagree.is_empty() {
        return MemberCheck { at: now, state: "agree".into(), sources: compared.sources.clone(), screen, ..Default::default() };
    }
    let episode = previous.filter(|check| check.state != "agree");
    let since = episode.and_then(|check| check.since).unwrap_or(now);
    MemberCheck {
        at: now,
        state: if now - since >= GRACE { "uncertain".into() } else { "pending".into() },
        since: Some(since),
        sources: compared.sources.clone(),
        disagree: compared.disagree.clone(),
        screen,
        told: episode.is_some_and(|check| check.told),
    }
}

/// The request Zerus read from the member's session, in a few words.
pub(crate) fn recorded_request(member: &Member) -> Option<String> {
    let asked = member.pending.as_ref().or(member.terminal_question.as_ref())?;
    let question = &asked.card["questions"][0];
    let text = [question["header"].as_str(), question["question"].as_str()]
        .into_iter()
        .flatten()
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join(": ");
    Some(if text.is_empty() { "a request".into() } else { text.chars().take(160).collect() })
}

/// What Zerus does next about this member, in one sentence.
pub(crate) fn next_action(team: &Team, member: &Member, state: &str) -> String {
    let open = team.tasks.iter().find_map(|task| task.open_attempt().filter(|attempt| attempt.member == member.id).map(|attempt| (task, attempt)));
    if state == "uncertain" {
        return "Zerus tells the lead once and reads the screen again in about a minute; it sends nothing to the member.".into();
    }
    if member.terminal_question.is_some() {
        return "Only its Terminal takes the request: the owner sees it at once, with the screen after the defer time.".into();
    }
    // Task 173: work outside any hand-out is the lead's to stop or hand out.
    if let Some(flag) = &member.unassigned {
        return format!("It {} with no task handed to it: the lead hands the task out (retry or reassign) or tells it to stop.", flag.what);
    }
    if member.pending.is_some() {
        return "Its request waits for the lead's answer (hgs team answer), or the owner queue after the defer time.".into();
    }
    match (open, state) {
        (Some((task, attempt)), "working") if attempt.state == AttemptState::Delivered => {
            format!("Waits for its report on #{}; marks it stuck after the stuck bound without an event.", task.id)
        }
        (Some((task, _)), _) => format!("Waits for its report on #{}; reminds it when its turn ended without one.", task.id),
        // Task 263 (owner, 2026-11-08): the member is idle because another
        // open task's outputs hold its ready work; name the holder so the
        // lead sees what stands in the way instead of "hands it the next".
        (None, "idle") if member.state == MemberState::Active => {
            let held = plan::held_ready(team, &member.id);
            if let Some((task, holder)) = held.first().copied() {
                let more = if held.len() > 1 { format!(" and {} more task(s)", held.len() - 1) } else { String::new() };
                format!("Its ready task #{task}{more} is held by #{holder}'s outputs; {}.", plan::holder_unblock(team, holder))
            } else {
                // Task 295: a refused or running hand-out, or a task queued behind
                // another that writes the same files, is named too.
                held_task(team, member).unwrap_or_else(|| "Hands it the next ready task, if one is ready for it.".into())
            }
        }
        (None, _) if member.state == MemberState::Active => match held_task(team, member) {
            Some(why) => why,
            None => "Hands it the next ready task, if one is ready for it.".into(),
        },
        _ => "Nothing: the member is not active.".into(),
    }
}

/// Task 295: why the member's own ready task is not handed out, when
/// something other than the lead holds it: another open task writing the same
/// files, or a hand-out step that was refused.
fn held_task(team: &Team, member: &Member) -> Option<String> {
    let ready = super::plan::ready(team);
    let task = team
        .tasks
        .iter()
        .filter(|task| task.assignee == member.id && task.status == super::model::TaskStatus::Pending && task.hold.is_none())
        .filter(|task| task.waits.is_some() || ready.contains(&task.id))
        .min_by_key(|task| task.id)?;
    if let Some(wait) = task.waits.as_ref() {
        return Some(if wait.running() {
            format!("Its task #{} is being prepared; it is handed out once that is confirmed.", task.id)
        } else {
            format!("Its task #{} waits: {} (retry or reassign it).", task.id, wait.reason.chars().take(200).collect::<String>())
        });
    }
    let queue = super::plan::output_queue(team, task);
    let first = queue.first()?;
    Some(format!("Its task #{} waits for #{first}, which writes the same files; Zerus hands it out when #{first} ends.", task.id))
}

/// The compact snapshot `hgs team look` returns and the lead is told.
pub(crate) fn snapshot(team: &Team, member: &Member, check: &MemberCheck, now: f64) -> Value {
    let session = member.session.as_ref();
    let task = team.tasks.iter().find_map(|task| {
        task.open_attempt().filter(|attempt| attempt.member == member.id).map(|attempt| {
            json!({"id":task.id,"title":task.title,"status":task.status,"attempt":attempt.n,"attempt_state":attempt.state,
                "delivered_at":attempt.delivered_at,"activity_at":attempt.activity_at})
        })
    });
    // Task #23 of team c536f0e5: with no open attempt, its parked one and the
    // runs it waits for are that task's work, named with it.
    let task = task.or_else(|| {
        team.tasks.iter().find_map(|task| {
            let park = task.park.as_ref().filter(|park| park.member == member.id)?;
            let attempt = task.attempts.iter().find(|attempt| attempt.id == park.attempt && attempt.state == super::model::AttemptState::Parked)?;
            Some(json!({"id":task.id,"title":task.title,"status":task.status,"attempt":attempt.n,"attempt_state":attempt.state,
                "parked":super::park::summary(park, now)}))
        })
    });
    let runs: Vec<Value> = super::dispatcher::member_folder_path(team, member)
        .ok()
        .filter(|folder| !folder.is_empty())
        .map(|folder| super::shadow::own_runs(std::path::Path::new(&folder), &member.id))
        .unwrap_or_default()
        .iter()
        .filter_map(|run| {
            let task = super::shadow::run_task(team, &member.id, run)?;
            Some(json!({"request_id":run.request,"task":task,"template":run.label,"state":if run.running { "running" } else { "queued" }}))
        })
        .collect();
    let minutes = |at: Option<f64>| at.map(|at| ((now - at).max(0.0) / 60.0).round() as u64);
    json!({"team":team.id,"team_name":team.name,"member":member.id,"role":member.role,"agent":member.agent,
        "session":session.map(|s| s.name.clone()),"host":session.map(|s| s.host.clone()).unwrap_or_default(),
        "state":if check.state == "uncertain" { "uncertain".into() } else if member.unassigned.is_some() { "unassigned_work".into() } else { check_state(check) },
        "uncertain":check.state == "uncertain","settling":check.state == "pending",
        "disagree":check.disagree,"uncertain_since":check.since,"checked_at":check.at,
        "sources":check.sources,"screen":check.screen,"task":task,"runs":runs,
        // Task #232: the request it waits on, named ("Bash: Run this command?").
        "request":recorded_request(member),
        "waiting":check.sources.get("waiting").cloned().unwrap_or(Value::Null),
        // Task 173: never shown as idle while it works with no task handed.
        "unassigned":member.unassigned.as_ref().map(|flag| json!({"what":flag.what,"since":flag.since})),
        "pulse":{"minutes_since_event":minutes(member.seen_event_at),"stuck":member.stuck,
            "minutes_since_check":minutes(Some(check.at))},
        "next":next_action(team, member, &check.state)})
}

/// The reconciled state a check names: the hooks' view when they agree.
fn check_state(check: &MemberCheck) -> String {
    check.sources["hooks"]["view"].as_str().filter(|view| *view != "unknown")
        .or_else(|| check.sources["screen"]["view"].as_str())
        .unwrap_or("unknown")
        .to_owned()
}

/// The lead's message about an uncertain member: the snapshot in a few lines.
pub(crate) fn lead_text(team: &Team, member: &Member, check: &MemberCheck, now: f64) -> String {
    let name = team.name_of(&member.id);
    let session = member.session.as_ref().map(|s| s.name.as_str()).unwrap_or_default();
    let minutes = check.since.map(|since| ((now - since).max(0.0) / 60.0).round() as u64).unwrap_or(0);
    let mut text = format!(
        "[Zerus team \u{b7} from: Zerus \u{b7} look]\n{name} ({session}): its status is uncertain for {minutes} min; its sources disagree:\n"
    );
    for line in check.disagree.iter().take(4) {
        text += &format!("  - {line}\n");
    }
    if !check.screen.is_empty() {
        text += "Its screen ends with:\n";
        for line in &check.screen[check.screen.len().saturating_sub(8)..] {
            text += &format!("  | {}\n", line.trim());
        }
    }
    text += &format!(
        "Look again with {} | hgs team look --json (or hgs team look {} --team {}). {}",
        json!({"team":team.id,"member":member.id}),
        member.id,
        team.id,
        next_action(team, member, &check.state)
    );
    text
}
