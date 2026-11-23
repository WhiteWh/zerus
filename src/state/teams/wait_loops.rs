//! Task 161 (owner, 2026-11-07): neither the lead nor a member turns a task
//! into a waiting loop; waiting on an outside event is always a park (task
//! 160), which Zerus watches and reports.
//!
//! - The lead's reply or message telling a member to wait for an outside event
//!   (an install, a CI run, a card, a time) is refused with the park command.
//! - A member's `hgs run` or team run whose purpose is waiting (a job named a
//!   wait, a loop of sleeps or pings around a repeated check) is refused with
//!   the park command; a member ending its turn waiting is offered the park.
//! - After the fact: a session building only for its own wait job, running a
//!   waiting background command, or repeating the same read-only check is
//!   parked automatically, and the lead is told once with the guessed
//!   condition.
use serde_json::{json, Value};

/// A member's attempt is looked at for a wait loop this long after its
/// hand-out, and at most this often.
pub(crate) const DETECT_AFTER: f64 = 600.0;
pub(crate) const DETECT_EVERY: f64 = 300.0;
/// The estimate of a park Zerus makes from a guessed condition.
pub(crate) const GUESSED_ETA_MINUTES: u64 = 60;

fn lower(text: &str) -> String {
    text.to_lowercase()
}

fn any(text: &str, words: &[&str]) -> bool {
    words.iter().any(|word| text.contains(word))
}

/// Words that name an event outside the task.
const OUTSIDE: [&str; 28] = [
    "install", "version", "release", "ci ", "ci.", "ci,", "ci run", "pipeline", "workflow", "card", "approval", "approve", "the owner",
    "deploy", "tomorrow", "minute", "hour", "o'clock", "nightly", "dev build", "new build", "build number", "build to ", "the build is",
    "установ", "сборк", "релиз", "владел",
];

/// Task #276 (owner, 2026-11-08): the detector judges an *instruction* to wait,
/// never the co-occurrence of a wait word and an event word. The lead's
/// ordinary answers ("held until #5 is accepted", "the owner allows card p77")
/// describe state and pass; "wait for the owner to answer", "check again in 5
/// minutes" or "poll the CI until it is green" are instructions, and their
/// phrase comes back to be quoted with the park command.
pub(crate) fn lead_wait_instruction(text: &str) -> Option<String> {
    for clause in clauses(text) {
        if let Some(phrase) = wait_instruction_phrase(&clause) {
            return Some(shorten(&phrase));
        }
    }
    None
}

/// The clauses of a text, so an instruction is judged apart from the state
/// beside it: sentences, and what follows a comma, a semicolon or a colon.
fn clauses(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (index, ch) in chars.iter().enumerate() {
        // A dot ends a sentence only before a space or the end: "1.58.0" is a
        // version, not three clauses.
        let end = matches!(ch, '!' | '?' | '\n' | ';' | ':') || (*ch == '.' && chars.get(index + 1).is_none_or(|next| next.is_whitespace()));
        if end {
            if !current.trim().is_empty() {
                out.push(current.trim().to_owned());
            }
            current.clear();
        } else {
            current.push(*ch);
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_owned());
    }
    out
}

/// The imperative forms that tell a member to wait. A bare "check", "watch" or
/// "hold" is ordinary work unless it names a repetition or a condition.
const WAIT_VERBS: [&str; 22] = [
    "wait", "keep waiting", "keep polling", "keep checking", "keep trying", "poll", "sleep", "retry", "recheck", "re-check",
    "check again", "check back", "hold on", "hold off", "hold the task", "monitor", "watch for", "watch until", "loop",
    "подожди", "жди", "дождись",
];
/// Words that make a wait a condition on an outside event (the park's).
const CONDITION: [&str; 25] = [
    "install", "version", "release", "ci ", "ci.", "ci,", "ci run", "pipeline", "workflow", "card", "approval", "approve", "the owner",
    "deploy", "tomorrow", "minute", "hour", "second", "day", "o'clock", "nightly", "dev build", "new build", "build number", "пока",
];
/// A duration names a time condition without any other word.
fn duration(text: &str) -> bool {
    let has_number = text.split_whitespace().any(|word| {
        let word = word.trim_matches(|ch: char| !ch.is_ascii_digit() && ch != '.');
        word.parse::<f64>().is_ok_and(|number| number > 0.0)
    });
    let unit = any(text, &["min", "hour", "sec", "day", "minute", "hour"]);
    let short = text.split_whitespace().any(|word| word.chars().any(|ch| ch.is_ascii_digit()) && word.chars().any(|ch| matches!(ch, 's' | 'm' | 'h' | 'd')));
    (has_number && unit) || short
}

/// The phrase of one clause that tells a member to wait, or None for state,
/// work and ordinary direction.
fn wait_instruction_phrase(clause: &str) -> Option<String> {
    let lower = clause.to_lowercase().trim().to_owned();
    if lower.is_empty() {
        return None;
    }
    // An instruction begins the clause, or follows a conjunction, a
    // politeness or a comma inside it ("Keep 138 and wait for X").
    let mut candidates: Vec<&str> = vec![lower.as_str()];
    for marker in [" and ", " then ", " please ", " just ", " now ", " also ", " kindly ", ", ", "; ", " so "] {
        if let Some(at) = lower.find(marker) {
            candidates.push(lower[at + marker.len()..].trim_start());
        }
    }
    for candidate in candidates {
        let mut rest = candidate.trim_start();
        for prefix in ["please ", "just ", "now ", "also ", "kindly ", "you should ", "you need to ", "you must ", "you ", "so "] {
            if let Some(stripped) = rest.strip_prefix(prefix) {
                rest = stripped.trim_start();
            }
        }
        // A negation is not an instruction to wait.
        if ["don't ", "do not ", "never ", "no need to ", "there is no need to "].iter().any(|negation| rest.starts_with(negation)) {
            continue;
        }
        let Some(verb) = WAIT_VERBS.iter().filter(|verb| {
            rest.strip_prefix(*verb).is_some_and(|after| after.is_empty() || !after.starts_with(|ch: char| ch.is_alphabetic()))
        }).max_by_key(|verb| verb.len()) else { continue };
        let phrase = rest.to_owned();
        // A loop instruction is waiting by itself; any other wait needs a
        // condition (an outside event, a time, "until"/"after"/"in").
        let loopish = matches!(*verb, "keep waiting" | "keep polling" | "keep checking" | "keep trying" | "loop" | "sleep");
        if loopish || any(&phrase, &CONDITION) || duration(&phrase) || phrase.contains("until") || phrase.contains("till") || phrase.contains("after") || phrase.contains(" in ") {
            return Some(phrase);
        }
    }
    None
}

/// The quoted trigger: one line, bounded.
fn shorten(phrase: &str) -> String {
    let line: String = phrase.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > 140 { format!("{}...", line.chars().take(137).collect::<String>()) } else { line }
}

/// A job name or command whose purpose is waiting, with why; None for real
/// work. A loop needs a pause and a repetition around a check.
pub(crate) fn waiting_job(label: &str, command: &str) -> Option<String> {
    let label = lower(label);
    let command = lower(command);
    let named = ["wait", "poll", "watch for", "until ", "ожида"].iter().find(|word| label.contains(*word));
    if let Some(word) = named {
        return Some(format!("its name says it waits (\"{word}\")"));
    }
    let pause = ["sleep ", "start-sleep", "timeout /t", "timeout -t", "ping -n", "ping -c", "choice /t"].iter().find(|word| command.contains(*word));
    let repeat = ["while ", "while(", "until ", "for /l", "for ((", "loop", "goto ", ":retry", "repeat"].iter().find(|word| command.contains(*word));
    match (pause, repeat) {
        (Some(pause), Some(repeat)) => Some(format!("it loops (\"{}\") around a pause (\"{}\")", repeat.trim(), pause.trim())),
        _ => None,
    }
}

/// A member's last reply that says it waits or is blocked on something
/// outside its task.
pub(crate) fn waiting_reply(text: &str) -> bool {
    let text = lower(text);
    let waiting = any(&text, &["waiting for", "i'll wait", "i will wait", "blocked on", "blocked until", "keep polling", "will check again", "жду", "ожидаю", "заблокирован"]);
    waiting && any(&text, &OUTSIDE)
}

/// The same read-only check a member's turn repeated at intervals (three
/// times or more, each a minute or more after the one before): the command.
pub(crate) fn repeated_check(events: &[Value], since: f64) -> Option<(String, usize)> {
    // One event per call: the PreToolUse ones, or the PostToolUse ones of an
    // agent that journals only those.
    let kind = if events.iter().any(|event| event["type"] == "PreToolUse") { "PreToolUse" } else { "PostToolUse" };
    let mut runs: Vec<(String, f64)> = Vec::new();
    for event in events {
        if event["type"] != kind {
            continue;
        }
        let at = event["at"].as_str().and_then(|text| text.parse::<f64>().ok()).or_else(|| event["at"].as_f64()).unwrap_or(0.0);
        if at <= since {
            continue;
        }
        let tool = event["tool"].as_str().unwrap_or_default();
        if !matches!(tool, "Bash" | "PowerShell" | "shell" | "exec_command") {
            continue;
        }
        let command = event["detail"].as_str().unwrap_or_default().lines().next().unwrap_or_default().trim().to_owned();
        if command.is_empty() || !read_only(&command) {
            continue;
        }
        runs.push((command, at));
    }
    let mut best: Option<(String, usize)> = None;
    for (command, _) in &runs {
        let times: Vec<f64> = runs.iter().filter(|(other, _)| other == command).map(|(_, at)| *at).collect();
        let spaced = times.windows(2).filter(|pair| pair[1] - pair[0] >= 60.0).count() + 1;
        if spaced >= 3 && best.as_ref().is_none_or(|(_, count)| spaced > *count) {
            best = Some((command.clone(), spaced));
        }
    }
    best
}

/// A command that only reads: a version, a status, a listing, a CI run.
fn read_only(command: &str) -> bool {
    let command = lower(command);
    // Task #413 (owner, 2026-11-09): a command that changes the repository is
    // work, never a poll, whatever read-only words it also carries. merger2's
    // 'git merge --ff-only ...; git log -1; git branch --show-current' matched
    // 'git log' and was parked as a wait loop although each run merged the next
    // task and fast-forwarded the line.
    if any(&command, &[
        " merge", "rebase", "cherry-pick", " reset", "checkout", " switch", " worktree", " pull", " push",
        "revert", "git apply", "git am", "stash",
    ]) {
        return false;
    }
    any(&command, &["--version", "-v ", "--build-info", " status", " ls", "inspect", "gh run", "actions/runs", "git log", "git fetch", "curl ", "hgs team current", "hgs team inbox"])
        && !any(&command, &["install", " build", " test", "commit", "push", " rm ", "del "])
}

/// Task #413: a repeated read-only check is a wait loop only while the state it
/// reads has not changed. The member's folder HEAD is the state Zerus can name
/// (the merger's fast-forward moves it on every run); a first observation is
/// never enough, and a moved HEAD is work.
pub(crate) fn unchanged_state(head: &str, previous: &str) -> bool {
    !head.is_empty() && !previous.is_empty() && head == previous
}

/// The condition a wait most likely means, with what it is: a CI run when
/// the text names one, an installed build when it names a version, else a
/// time half an hour on (the lead sets the real condition).
pub(crate) fn guess_condition(text: &str, now: f64) -> (Value, String) {
    let lower = lower(text);
    if let Some(at) = lower.find("actions/runs/") {
        let after = &text[at + "actions/runs/".len()..];
        let run: String = after.chars().take_while(char::is_ascii_digit).collect();
        let before = &text[..at];
        let repo = before.trim_end_matches('/').rsplit("repos/").next().unwrap_or_default().trim_end_matches('/');
        let repo: String = repo.rsplit("github.com/").next().unwrap_or(repo).trim_end_matches('/').to_owned();
        if let (Ok(run), true) = (run.parse::<u64>(), repo.split('/').count() == 2) {
            return (json!({"kind":"ci","repo":repo,"run":run}), format!("CI run {run} of {repo}"));
        }
    }
    let version: Option<String> = lower
        .split(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .find(|word| word.matches('.').count() >= 1 && word.split('.').all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit())))
        .map(str::to_owned);
    if let (true, Some(version)) = (any(&lower, &["--version", "install", "build", "version"]), version) {
        return (json!({"kind":"build","version":version}), format!("hgs {version} installed"));
    }
    (json!({"kind":"time","at":(now + 1800.0).round()}), "a check in 30 minutes (guessed; set the real condition)".into())
}

/// The park request a refusal names, ready to send.
pub(crate) fn park_command(team: &str, task: Option<u32>, attempt: &str) -> String {
    let task = task.map_or("N".to_owned(), |id| id.to_string());
    let attempt = if attempt.is_empty() { "ATTEMPT" } else { attempt };
    format!(
        "{{\"team\":\"{team}\",\"task\":{task},\"attempt\":\"{attempt}\",\"condition\":{{\"kind\":\"build\",\"version\":\"1.58.0\"}},\"eta_minutes\":60,\"note\":\"...\"}} | hgs team park --json (conditions: ci, build, task, card, time, run; hgs guide team-park)"
    )
}

/// The refusal of a wait the lead tells a member to do: it quotes the phrase
/// that triggered it (task #276) and offers the park of the condition it names.
pub(crate) fn lead_refusal(team: &str, task: Option<u32>, attempt: &str, phrase: &str) -> String {
    let (condition, what) = guess_condition(phrase, super::super::now());
    let task = task.map_or("N".to_owned(), |id| id.to_string());
    let attempt = if attempt.is_empty() { "ATTEMPT" } else { attempt };
    format!(
        "Waiting is a park, never an instruction: \"{phrase}\" tells a member to wait. Park the task on its condition and Zerus watches it, frees the member and tells you the outcome: {{\"team\":\"{team}\",\"task\":{task},\"attempt\":\"{attempt}\",\"condition\":{condition},\"eta_minutes\":60,\"note\":\"...\"}} | hgs team park --json (conditions: ci, build, task, card, time, run; hgs guide team-park). The condition this phrase names: {what}."
    )
}

/// The refusal of a member's job whose purpose is waiting.
pub(crate) fn job_refusal(why: &str, team: &str, task: Option<u32>, attempt: &str) -> String {
    format!(
        "This job waits ({why}); a wait is a park, never a job: {}. Zerus watches the condition and gives the task back to you.",
        park_command(team, task, attempt)
    )
}

/// `hgs send` from the lead's session to a member of its team: a wait
/// instruction is refused with the park of the member's open task.
pub(crate) fn lead_send_check(target: &str, text: &str) -> anyhow::Result<()> {
    let Some(phrase) = lead_wait_instruction(text) else {
        return Ok(());
    };
    let Some(record) = super::super::session_graph::caller_record() else { return Ok(()) };
    let Ok(store) = super::store::Store::open(false) else { return Ok(()) };
    for team in store.active_teams(1000).unwrap_or_default() {
        if super::commands::calling_member(&team, &record).as_deref() != Some(team.lead.as_str()) {
            continue;
        }
        let Some(member) = team.members.iter().find(|member| member.id != team.lead && member.session.as_ref().is_some_and(|session| session.name == target.trim())) else {
            continue;
        };
        let open = team.tasks.iter().find_map(|task| task.open_attempt().filter(|attempt| attempt.member == member.id).map(|attempt| (task.id, attempt.id.clone())));
        anyhow::bail!("{}", lead_refusal(&team.id, open.as_ref().map(|found| found.0), open.as_ref().map_or("", |found| found.1.as_str()), &phrase));
    }
    Ok(())
}

/// The script a job runs, when its command names one in its folder (a .cmd,
/// .bat, .ps1, .sh or .py file): its first 64 KB, so a loop inside it is seen.
pub(crate) fn script_text(command: &str, cwd: &std::path::Path) -> String {
    let first = command.trim().trim_start_matches(['&', ' ']).trim();
    let first = if let Some(rest) = first.strip_prefix('"') { rest.split('"').next().unwrap_or_default() } else { first.split_whitespace().next().unwrap_or_default() };
    let mut candidates = vec![first.to_owned()];
    // `powershell -File x.ps1`, `bash x.sh`, `cmd /c x.cmd`: the script argument.
    candidates.extend(command.split_whitespace().skip(1).map(|word| word.trim_matches('"').to_owned()));
    for candidate in candidates.iter().take(8) {
        let lower = candidate.to_lowercase();
        if ![".cmd", ".bat", ".ps1", ".sh", ".py"].iter().any(|ext| lower.ends_with(ext)) {
            continue;
        }
        let path = std::path::Path::new(candidate);
        let path = if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) };
        if let Ok(mut file) = std::fs::File::open(&path) {
            use std::io::Read;
            let mut bytes = Vec::new();
            let _ = (&mut file).take(64 * 1024).read_to_end(&mut bytes);
            return String::from_utf8_lossy(&bytes).into_owned();
        }
    }
    String::new()
}

/// The refusal of `hgs run` for a waiting job, naming the caller's own task
/// and attempt when it is a team member.
pub(crate) fn caller_job_refusal(why: &str) -> String {
    // Tests never read the live team store.
    if cfg!(test) {
        return job_refusal(why, "TEAM", None, "");
    }
    let found = super::super::session_graph::caller_record().and_then(|record| {
        let store = super::store::Store::open(false).ok()?;
        store.active_teams(1000).unwrap_or_default().into_iter().find_map(|team| {
            let member = super::commands::calling_member(&team, &record)?;
            let open = team.tasks.iter().find_map(|task| task.open_attempt().filter(|attempt| attempt.member == member).map(|attempt| (task.id, attempt.id.clone())));
            Some((team.id.clone(), open))
        })
    });
    match found {
        Some((team, open)) => job_refusal(why, &team, open.as_ref().map(|found| found.0), open.as_ref().map_or("", |found| found.1.as_str())),
        None => job_refusal(why, "TEAM", None, ""),
    }
}

#[cfg(test)]
#[path = "tests/wait_loops.rs"]
mod tests;

