//! Task 160 (owner, 2026-11-07): a task blocked on an outside event parks,
//! frees its member, and comes back by itself.
//!
//! A member (or its reviewer) parks a task on a named condition with an
//! estimate: a CI run finishing, a build installed, a task accepted, an owner
//! card answered, a time. The task is Pending with a `park` and its attempt
//! `Parked`, which is not open: the member is free, takes other ready tasks
//! at once, and a parked task never holds its queue.
//!
//! Zerus watches the condition itself, on a thread beside the dispatcher's
//! tick and with bounded checks: task and card states and the clock on every
//! pass, the installed build at most once a minute, a CI run through the
//! fork's API at most every two minutes. No agent polls. When the condition
//! holds the task goes back to the same member at its next free turn, in the
//! same attempt, with the notes it parked with; when it fails (the run fails,
//! the card is denied) the lead is told with the reason.
use super::model::{Park, ParkCondition, TaskStatus, Team, TeamStatus};
use super::store::{Event, Store};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// How often the watcher looks at the teams' parked tasks.
pub(crate) const PASS_EVERY: Duration = Duration::from_secs(30);
/// At most this often one CI run is read from the fork's API.
pub(crate) const CI_EVERY: f64 = 120.0;
/// At most this often the installed build is read.
pub(crate) const BUILD_EVERY: f64 = 60.0;
/// The longest estimate a park takes: a week.
pub(crate) const ETA_MAX_MINUTES: u64 = 7 * 24 * 60;
/// A probe's network or process call is bounded by this.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) const WAITING: &str = "waiting";
pub(crate) const MET: &str = "met";
pub(crate) const FAILED: &str = "failed";

/// What the condition is, in words: "CI run 123 of WhiteWh/zerus finishing".
pub(crate) fn label(condition: &ParkCondition) -> String {
    match condition {
        ParkCondition::Ci { repo, run } => format!("CI run {run} of {repo} finishing"),
        ParkCondition::Build { version, build, commit, program } => {
            let mut wanted = Vec::new();
            if !version.is_empty() {
                wanted.push(format!("version {version}"));
            }
            if *build > 0 {
                wanted.push(format!("build {build}"));
            }
            if !commit.is_empty() {
                wanted.push(format!("commit {}", commit.chars().take(12).collect::<String>()));
            }
            let what = if program.is_empty() { "hgs".to_owned() } else { program_name(program) };
            format!("{what} {} installed", wanted.join(", "))
        }
        ParkCondition::Task { task } => format!("task #{task} accepted"),
        ParkCondition::Card { proposal } => format!("the owner's answer on card {proposal}"),
        ParkCondition::Time { at } => local_time(*at).to_string(),
        ParkCondition::Run { request } => format!("its run {request} finishing"),
    }
}

fn program_name(program: &str) -> String {
    std::path::Path::new(program).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| program.to_owned())
}

fn local_time(at: f64) -> String {
    chrono::DateTime::from_timestamp(at as i64, 0)
        .map(|time| time.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default()
}

/// "parked until CI run 1 of o/r finishing, ETA 14:30 (in 25 min)", the line
/// status, Waiting and the lead's inbox show.
pub(crate) fn summary(park: &Park, now: f64) -> String {
    let left = ((park.eta - now) / 60.0).round() as i64;
    let when = if left >= 0 { format!("in {left} min") } else { format!("{} min late", -left) };
    let mut text = format!("parked until {}, ETA {} ({when})", label(&park.condition), local_time(park.eta));
    match park.state.as_str() {
        MET => text.push_str(&format!("; it held ({}), the task goes back at the member's next free turn", park.detail)),
        FAILED => text.push_str(&format!("; it failed: {}", park.detail)),
        _ => {}
    }
    text
}

/// The condition a park request names, checked against the team.
pub(crate) fn condition_of(team: &Team, task: u32, value: &Value) -> anyhow::Result<ParkCondition> {
    let condition: ParkCondition = serde_json::from_value(value.clone()).map_err(|error| {
        anyhow::anyhow!(
            "A park condition is one of {{\"kind\":\"ci\",\"repo\":\"OWNER/REPO\",\"run\":N}}, {{\"kind\":\"build\",\"version\":\"1.58.0\"|\"build\":N|\"commit\":\"SHA\",\"program\":\"PATH\"}}, {{\"kind\":\"task\",\"task\":N}}, {{\"kind\":\"card\",\"proposal\":\"pN\"}}, {{\"kind\":\"time\",\"at\":UNIX_SECONDS}} or {{\"kind\":\"run\",\"request\":\"RUN_REQUEST_ID\"}}: {error}"
        )
    })?;
    match &condition {
        ParkCondition::Ci { repo, run } => {
            let parts: Vec<&str> = repo.split('/').collect();
            let slug = |part: &str| !part.is_empty() && part.len() <= 100 && part.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'));
            anyhow::ensure!(parts.len() == 2 && parts.iter().all(|part| slug(part)), "A CI condition names its repository as OWNER/REPO");
            anyhow::ensure!(*run > 0, "A CI condition names its run's number");
        }
        ParkCondition::Build { version, build, commit, program } => {
            anyhow::ensure!(!version.is_empty() || *build > 0 || !commit.is_empty(), "A build condition names a version, a build number or a commit");
            anyhow::ensure!(version.len() <= 64 && version.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '+')), "A build version is like 1.58.0");
            anyhow::ensure!(commit.len() <= 64 && commit.chars().all(|ch| ch.is_ascii_hexdigit()), "A build commit is a hexadecimal sha");
            anyhow::ensure!(program.is_empty() || (std::path::Path::new(program).is_absolute() && program.chars().count() <= 1000), "A build condition's program is an absolute path");
        }
        ParkCondition::Task { task: other } => {
            anyhow::ensure!(*other != task, "A task does not park on itself");
            anyhow::ensure!(team.task(*other).is_some(), "Unknown task #{other}");
        }
        ParkCondition::Card { proposal } => {
            anyhow::ensure!(team.proposals.iter().any(|card| card["id"] == proposal.as_str()), "Unknown card {proposal}");
        }
        ParkCondition::Time { at } => {
            anyhow::ensure!(*at > 0.0, "A time condition names when, in seconds since 1970");
        }
        ParkCondition::Run { request } => {
            anyhow::ensure!(super::store::canonical_uuid(request), "A run condition names the request_id of your hgs team run");
        }
    }
    Ok(condition)
}

/// What a probe of the outside world saw.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Installed {
    pub version: String,
    pub build: u64,
    pub commit: String,
}

/// The two conditions Zerus reads outside its own state.
pub(crate) trait Probes {
    /// A CI run's (status, conclusion), as GitHub names them.
    fn ci(&self, repo: &str, run: u64) -> Result<(String, String), String>;
    /// The installed build of `program` ("" is the hgs this worker runs).
    fn build(&self, program: &str) -> Result<Installed, String>;
    /// Task #23: a member's run in its outbox folder, read from the files.
    fn run(&self, folder: &std::path::Path, request: &str) -> RunSeen {
        run_seen(folder, request)
    }
}

/// Task #23: what a member's outbox shows of one run.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RunSeen {
    /// Queued or claimed by its runner.
    Waiting,
    /// Its result: the worker's outcome as the member reads it.
    Done(Value),
    /// Neither a request, a claim nor a result.
    Gone,
}

/// Task #23: the run's files, read in the order they follow one another
/// (request, claim, result), so a run moving on is never seen gone; nothing
/// seen is read once more before it counts as gone.
pub(crate) fn run_seen(folder: &std::path::Path, request: &str) -> RunSeen {
    for _ in 0..2 {
        if super::outbox::request_path(folder, request).is_file() || super::outbox::running_path(folder, request).is_file() {
            return RunSeen::Waiting;
        }
        if let Some(result) = super::outbox::result(folder, request) {
            return RunSeen::Done(result);
        }
    }
    RunSeen::Gone
}

/// Task #23: the park's outcome from what the outbox shows. A finished run is
/// met whatever its exit code, so the member reads the result.
fn run_outcome(request: &str, seen: RunSeen) -> Option<Outcome> {
    match seen {
        RunSeen::Waiting => None,
        RunSeen::Done(result) => Some(Outcome::Met(match result["exit"].as_i64() {
            Some(exit) => format!("run {request} finished with exit code {exit}{}", if result["timed_out"] == true { " (timed out)" } else { "" }),
            None => format!("run {request} ended without running: {}", result["error"].as_str().filter(|error| !error.is_empty()).unwrap_or("no reason given")),
        })),
        RunSeen::Gone => Some(Outcome::Failed(format!("run {request} is gone from the member's outbox without a result"))),
    }
}

/// The outcome a check reached.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Outcome {
    Met(String),
    Failed(String),
}

/// Whether the condition holds now. `last` is when the outside world was last
/// read for this park (0: never); a read that is not due yet answers None.
pub(crate) fn evaluate(team: &Team, park: &Park, now: f64, last: f64, probes: &dyn Probes) -> Option<Outcome> {
    match &park.condition {
        ParkCondition::Time { at } => (now >= *at).then(|| Outcome::Met(format!("it is {}", local_time(now)))),
        ParkCondition::Task { task } => {
            let found = team.task(*task)?;
            match found.status {
                TaskStatus::Accepted => Some(Outcome::Met(format!("task #{task} was accepted"))),
                TaskStatus::Cancelled => Some(Outcome::Failed(format!("task #{task} was cancelled"))),
                TaskStatus::Rejected => Some(Outcome::Failed(format!("task #{task} was rejected"))),
                _ => None,
            }
        }
        ParkCondition::Card { proposal } => {
            let card = team.proposals.iter().find(|card| card["id"] == proposal.as_str())?;
            let note = card["note"].as_str().filter(|note| !note.trim().is_empty()).unwrap_or("no reason given");
            match card["state"].as_str() {
                Some("accepted") => Some(Outcome::Met(format!("the owner allowed card {proposal}"))),
                Some("rejected") => Some(Outcome::Failed(format!("the owner denied card {proposal}: {note}"))),
                Some("withdrawn") => Some(Outcome::Failed(format!("card {proposal} was withdrawn"))),
                _ => None,
            }
        }
        ParkCondition::Ci { repo, run } => {
            if now - last < CI_EVERY {
                return None;
            }
            match probes.ci(repo, *run) {
                Ok((status, conclusion)) if status == "completed" => Some(if conclusion == "success" {
                    Outcome::Met(format!("CI run {run} succeeded"))
                } else {
                    Outcome::Failed(format!("CI run {run} of {repo} finished: {}", if conclusion.is_empty() { "no conclusion" } else { conclusion.as_str() }))
                }),
                _ => None,
            }
        }
        ParkCondition::Run { request } => {
            let member = team.member(&park.member)?;
            let folder = super::dispatcher::member_folder_path(team, member).ok().filter(|folder| !folder.is_empty())?;
            run_outcome(request, probes.run(std::path::Path::new(&folder), request))
        }
        ParkCondition::Build { version, build, commit, program } => {
            if now - last < BUILD_EVERY {
                return None;
            }
            let installed = probes.build(program).ok()?;
            let version_ok = version.is_empty() || at_least(&installed.version, version);
            let build_ok = *build == 0 || installed.build >= *build;
            let commit_ok = commit.is_empty() || (!installed.commit.is_empty() && (installed.commit.starts_with(commit.as_str()) || commit.starts_with(installed.commit.as_str())));
            (version_ok && build_ok && commit_ok).then(|| {
                let mut seen = vec![format!("version {}", installed.version)];
                if installed.build > 0 {
                    seen.push(format!("build {}", installed.build));
                }
                Outcome::Met(format!("{} is installed", seen.join(", ")))
            })
        }
    }
}

/// Whether the dotted `installed` version is at least `wanted` (numbers
/// compared part by part; a missing part is 0, a pre-release suffix ignored).
pub(crate) fn at_least(installed: &str, wanted: &str) -> bool {
    let parts = |text: &str| -> Vec<u64> {
        text.split(['-', '+']).next().unwrap_or_default().split('.').map(|part| part.parse().unwrap_or(0)).collect()
    };
    let (have, want) = (parts(installed), parts(wanted));
    if installed.trim().is_empty() {
        return false;
    }
    for index in 0..have.len().max(want.len()) {
        let (a, b) = (have.get(index).copied().unwrap_or(0), want.get(index).copied().unwrap_or(0));
        if a != b {
            return a > b;
        }
    }
    true
}

/// One watcher pass: every waiting park of the running teams is checked;
/// only an outcome changes the team. `reads` keeps when each park's outside
/// condition was last read (team, task).
pub(crate) fn watch(store: &mut Store, teams: &[Team], now: f64, probes: &dyn Probes, reads: &mut HashMap<(String, u32), f64>) -> anyhow::Result<usize> {
    let mut changed = 0;
    for team in teams.iter().filter(|team| matches!(team.status, TeamStatus::Running | TeamStatus::Paused)) {
        let mut outcomes: Vec<(u32, Outcome)> = Vec::new();
        for task in &team.tasks {
            let Some(park) = task.park.as_ref().filter(|park| park.state == WAITING) else { continue };
            let key = (team.id.clone(), task.id);
            let last = reads.get(&key).copied().unwrap_or(0.0);
            let outside = matches!(park.condition, ParkCondition::Ci { .. } | ParkCondition::Build { .. });
            let due = !outside || now - last >= if matches!(park.condition, ParkCondition::Ci { .. }) { CI_EVERY } else { BUILD_EVERY };
            let outcome = evaluate(team, park, now, last, probes);
            if outside && due {
                reads.insert(key, now);
            }
            if let Some(outcome) = outcome {
                outcomes.push((task.id, outcome));
            }
        }
        if outcomes.is_empty() {
            continue;
        }
        store.mutate(None, &team.id, None, |team, events| {
            for (id, outcome) in &outcomes {
                let Some(park) = team.task_mut(*id).and_then(|task| task.park.as_mut()).filter(|park| park.state == WAITING) else { continue };
                let (state, detail) = match outcome {
                    Outcome::Met(detail) => (MET, detail.clone()),
                    Outcome::Failed(detail) => (FAILED, detail.clone()),
                };
                park.state = state.into();
                park.detail = detail.clone();
                park.checked_at = now;
                let kind = if state == MET { "park_met" } else { "park_failed" };
                events.push(Event::new(kind, json!({"task":id,"member":park.member,"condition":label(&park.condition),"detail":detail})));
            }
            Ok(Value::Null)
        })?;
        changed += outcomes.len();
    }
    Ok(changed)
}

/// The fork's API and the installed programs.
pub(crate) struct Real;

impl Probes for Real {
    fn ci(&self, repo: &str, run: u64) -> Result<(String, String), String> {
        use std::io::Read;
        use ureq::tls::{RootCerts, TlsConfig};
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(PROBE_TIMEOUT))
            .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
            .build()
            .new_agent();
        let mut request = agent
            .get(format!("https://api.github.com/repos/{repo}/actions/runs/{run}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "zerus-hgs");
        // A private fork needs the owner's token; a public one reads without.
        if let Some(token) = ["GITHUB_TOKEN", "GH_TOKEN"].iter().find_map(|name| std::env::var(name).ok().filter(|token| !token.trim().is_empty())) {
            request = request.header("Authorization", format!("Bearer {}", token.trim()));
        }
        let response = request.call().map_err(|error| error.to_string())?;
        let mut raw = Vec::new();
        response.into_body().into_reader().take(256 * 1024).read_to_end(&mut raw).map_err(|error| error.to_string())?;
        let value: Value = serde_json::from_slice(&raw).map_err(|error| error.to_string())?;
        Ok((value["status"].as_str().unwrap_or_default().to_owned(), value["conclusion"].as_str().unwrap_or_default().to_owned()))
    }

    fn build(&self, program: &str) -> Result<Installed, String> {
        let path = if program.is_empty() { std::env::current_exe().map_err(|error| error.to_string())? } else { PathBuf::from(program) };
        // A desktop answers --build-info (JSON); hgs answers --version.
        let flag = if program.is_empty() { "--version" } else { "--build-info" };
        let mut command = std::process::Command::new(&path);
        command.arg(flag).stdin(std::process::Stdio::null());
        let outcome = super::templates::run_tree(&mut command, PROBE_TIMEOUT, None);
        if outcome.exit != Some(0) {
            return Err(format!("{} {flag} failed", path.display()));
        }
        Ok(parse_installed(&outcome.tail))
    }
}

/// `hgs 1.58.0` or a desktop's `--build-info` JSON.
pub(crate) fn parse_installed(text: &str) -> Installed {
    if let Some(start) = text.find('{') {
        if let Ok(value) = serde_json::from_str::<Value>(&text[start..text.rfind('}').map_or(text.len(), |end| end + 1)]) {
            return Installed {
                version: value["version"].as_str().unwrap_or_default().to_owned(),
                build: value["devBuildNumber"].as_u64().unwrap_or(0),
                commit: value["sourceCommit"].as_str().unwrap_or_default().to_owned(),
            };
        }
    }
    let version = text.split_whitespace().find(|word| word.chars().next().is_some_and(|ch| ch.is_ascii_digit())).unwrap_or_default();
    // A Dev build reads 1.58.0-dev.42.
    let build = version.split("-dev.").nth(1).and_then(|rest| rest.split(|ch: char| !ch.is_ascii_digit()).next()).and_then(|digits| digits.parse().ok()).unwrap_or(0);
    Installed { version: version.to_owned(), build, commit: String::new() }
}

/// The watcher beside the worker's tick: every 30 s, the running teams with a
/// waiting park. A test drives `watch` itself.
pub(crate) fn start_background(log: impl Fn(&str) + Send + 'static) {
    if cfg!(test) {
        return;
    }
    std::thread::spawn(move || {
        let mut reads: HashMap<(String, u32), f64> = HashMap::new();
        loop {
            std::thread::sleep(PASS_EVERY);
            let Ok(mut store) = Store::open(true) else { continue };
            let Ok(teams) = store.active_teams(1000) else { continue };
            let parked: Vec<Team> = teams.into_iter().filter(|team| team.tasks.iter().any(|task| task.park.as_ref().is_some_and(|park| park.state == WAITING))).collect();
            if parked.is_empty() {
                reads.clear();
                continue;
            }
            if let Err(error) = watch(&mut store, &parked, super::super::now(), &Real, &mut reads) {
                log(&format!("park watcher: {error}"));
            }
            // Bounded: a park that ended leaves its last read behind.
            reads.retain(|(team, task), _| parked.iter().any(|found| &found.id == team && found.task(*task).is_some_and(|task| task.park.is_some())));
        }
    });
}

/// The message that gives a parked task back to its member.
pub(crate) fn resume_text(task: u32, title: &str, park: &Park) -> String {
    let notes = if park.note.trim().is_empty() { String::new() } else { format!("\nYour notes when you parked it: {}", park.note.trim()) };
    format!(
        "[Zerus team] Your parked task #{task} ({title}) resumes: {} held ({}).{notes}\nContinue the same attempt and finish it with hgs team report (attempt {}). Your task: {{\"attempt\":\"{}\"}} | hgs team current --json",
        label(&park.condition),
        park.detail,
        park.attempt,
        park.attempt
    )
}

#[cfg(test)]
#[path = "tests/park.rs"]
mod tests;
