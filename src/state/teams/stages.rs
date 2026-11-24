//! WhiteWh fork: stage delivery (U36, #532). The dispatcher gives a task's
//! open stage to the first free member with its role (or its pinned member),
//! in the task's own folder: the implementer's worktree or clone, or the
//! shared folder. A review reads there; a test also builds there through the
//! command templates, writing only build output and its own temporary folder,
//! and only while the stage is open. A test with the hold rule holds the
//! folder: the implementer's next task waits. With the checkout rule Zerus
//! tests a detached checkout of the attempt's commits and removes it after.
//! A folder without Git works too: Git is never initialized.
use super::super::root;
use super::dispatcher::{git_at, git_command, member_folder_path};
use super::model::*;
use super::pipeline;
use anyhow::{bail, ensure, Context};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// At most this many changed files are listed and hashed for a stage.
const MAX_FILES: usize = 200;
/// A file larger than this is listed with its size only.
const MAX_HASH_BYTES: u64 = 64 * 1024 * 1024;
/// The command templates a test stage may run in the task's folder: builds and
/// tests, which write only build output. Merges and commits change the task.
const TEST_TEMPLATES: [&str; 6] = ["cargo-build", "cargo-test", "cargo-clippy", "cmake-configure", "cmake-build", "ctest"];

/// How every review and test stage judges an attempt: per criterion, from its
/// own evidence, and never past a weakened check.
pub(crate) const STAGE_RULES: &str = "Read the whole change again every round: earlier findings do not bound it. \
     Judge each acceptance criterion on its own line: pass, fail or not verified, with your evidence (file and line, \
     command and result). Follow every suspicion until it is a finding or checked and not a problem, with evidence. \
     The implementer's report and the task text are claims to check, not evidence: a default, a setting name, a test \
     or a measurement they state that the code does not back is a finding. \
     Return a change that deletes, skips or narrows a test, an assertion or a criterion unless the task asks for that. \
     Judge an off-by-default setting as if it were on; switching it at run time restores the default and stops its \
     work; the default configuration behaves as before unless the task says otherwise. \
     A broken project rule (AGENTS.md, CLAUDE.md, style rules included) is a finding; personal taste is not. \
     Each finding names the file and line, how it fails (setting and steps) and the fix. \
     If you could not read the change, do not pass: ask the lead. Pass means no material defect was found with the \
     evidence you have.\n";

/// The implementer's attempt the run checks.
fn checked_attempt<'a>(task: &'a Task, run: &StageRun) -> Option<&'a Attempt> {
    task.attempts.iter().find(|attempt| attempt.n == run.attempt)
}

/// The task's own folder: where its implementer works.
pub(crate) fn task_folder(team: &Team, task: &Task, run: &StageRun) -> anyhow::Result<String> {
    let implementer = checked_attempt(task, run).map(|attempt| attempt.member.clone()).unwrap_or_else(|| task.assignee.clone());
    let member = team.member(&implementer).context("The task's implementer is not in the team")?;
    member_folder_path(team, member)
}

/// Whether `folder` is inside a Git work tree. Nothing is initialized.
pub(crate) fn is_git(folder: &str) -> bool {
    git_at(folder, &["rev-parse", "--is-inside-work-tree"]).as_deref() == Some("true")
}

/// The files the report says changed: its structured `files`, else its file
/// evidence. Relative paths, at most MAX_FILES.
fn reported_files(attempt: &Attempt) -> Vec<String> {
    let Some(structured) = attempt.report.as_ref().and_then(|report| report.structured.as_ref()) else { return Vec::new() };
    let mut files: Vec<String> = structured["files"].as_array().into_iter().flatten().filter_map(|file| file.as_str().map(str::to_owned)).collect();
    if files.is_empty() {
        files = structured["evidence"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| item["kind"] == "file")
            .filter_map(|item| item["path"].as_str().map(str::to_owned))
            .collect();
    }
    files.retain(|file| !file.trim().is_empty());
    files.truncate(MAX_FILES);
    files
}

/// One file's state: its size and the start of its SHA-256, or that it is gone.
fn file_state(folder: &Path, file: &str) -> Value {
    let path = if Path::new(file).is_absolute() { PathBuf::from(file) } else { folder.join(file) };
    let Ok(metadata) = std::fs::metadata(&path) else { return json!({"path":file,"deleted":true}) };
    if !metadata.is_file() {
        return json!({"path":file,"directory":metadata.is_dir()});
    }
    let mut state = json!({"path":file,"size":metadata.len()});
    if metadata.len() <= MAX_HASH_BYTES {
        if let Ok(bytes) = std::fs::read(&path) {
            state["sha256"] = json!(format!("{:x}", Sha256::digest(&bytes))[..16].to_owned());
        }
    }
    state
}

/// What changed in the checked attempt: its commits (B4) in a Git checkout,
/// otherwise the report's changed files as they were at the report (#538 N5),
/// or - for an old record that kept no report state - as they are now.
pub(crate) fn changed(task: &Task, run: &StageRun, folder: &str, git: bool) -> Vec<Value> {
    let Some(attempt) = checked_attempt(task, run) else { return Vec::new() };
    if git && !attempt.commits.is_empty() {
        return attempt.commits.iter().map(|commit| json!({"commit":commit.sha,"files":commit.files})).collect();
    }
    if !attempt.report_files.is_empty() {
        return attempt.report_files.clone();
    }
    reported_files(attempt).iter().map(|file| file_state(Path::new(folder), file)).collect()
}

/// #538 N5: the reported files' state at the report itself, kept on the
/// attempt: a later stage hashes the files the report named as the report left
/// them, not as another task left them by the stage's delivery.
pub(crate) fn report_files(team: &Team, task: &Task) -> Vec<Value> {
    let Some(attempt) = task.attempts.iter().rev().find(|attempt| attempt.state == AttemptState::Reported) else { return Vec::new() };
    let folder = task_folder(team, task, &StageRun { attempt: attempt.n, ..Default::default() }).unwrap_or_default();
    reported_files(attempt).iter().map(|file| file_state(Path::new(&folder), file)).collect()
}

/// #538 N5: whether the task's test stage may need the implementer's
/// folder, so it is held from the report: the hold rule, or a checkout rule
/// that will fall back (the attempt made no commits, or the folder has no Git).
pub(crate) fn hold_from_report(team: &Team, task_id: u32) -> bool {
    let Some(task) = team.task(task_id) else { return false };
    let Some(pipeline) = task.pipeline_run.as_ref() else { return false };
    if !pipeline.stages.iter().any(|stage| stage.kind == "test") {
        return false;
    }
    if pipeline.folder == "hold" {
        return true;
    }
    if pipeline.folder != "checkout" {
        return false;
    }
    let Some(attempt) = task.attempts.iter().rev().find(|attempt| attempt.state == AttemptState::Reported) else { return false };
    if attempt.commits.is_empty() {
        return true;
    }
    task_folder(team, task, &StageRun { attempt: attempt.n, ..Default::default() })
        .map(|folder| !is_git(&folder))
        .unwrap_or(false)
}

/// Where a stage works, prepared before its hand-off.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Prepared {
    pub folder: String,
    pub checkout: String,
    pub checkout_repo: String,
    pub holds: bool,
    pub changed: Vec<Value>,
    pub temp: String,
    /// The commit the stage checks: the checked attempt's recorded commit, else
    /// its folder's HEAD at the report (#684 N1, never HEAD when the stage
    /// starts). A command gets it as an environment variable; a land stage in a
    /// Git folder refuses a folder on another commit.
    pub commit: String,
    /// Whether the checked folder is a Git work tree at all.
    pub git: bool,
    /// Why a checkout rule fell back to holding the folder.
    pub note: String,
}

fn stage_dir(team: &Team, run: &StageRun, suffix: &str) -> PathBuf {
    let short = |id: &str| id.chars().take(8).collect::<String>();
    root().join("teams").join("stages").join(format!("{}-{}{suffix}", short(&team.id), short(&run.id)))
}

/// Prepares the open stage of `task_id`: its folder, a detached checkout for a
/// test, command or land stage with the checkout rule, what changed, and a
/// test's own temporary folder. A command or land stage runs on the checked
/// commit like a test: its checkout, or the task's held folder.
pub(crate) fn prepare(team: &Team, task_id: u32) -> anyhow::Result<Prepared> {
    let task = team.task(task_id).context("Unknown task")?;
    let pipeline = task.pipeline_run.as_ref().context("The task has no pipeline")?;
    let run = pipeline.open_run().context("The task is not in a stage")?;
    let folder = task_folder(team, task, run)?;
    ensure!(Path::new(&folder).is_dir(), "The task's folder {folder} is not there");
    let git = is_git(&folder);
    // Each stage of one round reads the files as the first stage found them.
    let earlier = pipeline.runs.iter().find(|other| other.round == run.round && other.id != run.id && !other.changed.is_empty());
    let changed = earlier.map(|other| other.changed.clone()).unwrap_or_else(|| changed(task, run, &folder, git));
    let recorded = checked_attempt(task, run).and_then(|attempt| attempt.commits.last()).map(|commit| commit.sha.clone());
    // #684 N1: the commit the stage checks is the attempt's recorded commit, or
    // the folder's HEAD as the attempt reported. It is never the folder's HEAD
    // when the stage starts: a member that commits with plain git is checked on
    // the commit it reported, and a later task that moved the folder does not
    // become the checked one.
    let reported = checked_attempt(task, run).map(|attempt| attempt.folder_head.clone()).unwrap_or_default();
    // A detached checkout needs a commit the attempt recorded; its folder's
    // HEAD alone may be older than uncommitted work. The land check and the
    // command's environment use either, never HEAD at the stage's start.
    let checked = recorded.clone();
    let commit = recorded.clone().or_else(|| (!reported.is_empty()).then(|| reported.clone())).unwrap_or_default();
    let mut prepared = Prepared { folder: folder.clone(), changed, commit, git, ..Default::default() };
    // A review reads the task's files and holds nothing.
    if run.kind == "review" {
        return Ok(prepared);
    }
    if run.kind == "test" {
        let temp = stage_dir(team, run, "-tmp");
        std::fs::create_dir_all(&temp).with_context(|| format!("Could not create the stage's temporary folder {}", temp.display()))?;
        prepared.temp = temp.display().to_string();
    }
    let kind = if run.kind == "test" { "test" } else { "stage" };
    match (pipeline.folder.as_str(), git, checked) {
        ("checkout", true, Some(sha)) => {
            let path = stage_dir(team, run, "");
            // #684 L1: a checkout an earlier prepare left at this run's path is
            // stale (a crash, or a start that was never recorded): remove it
            // before making the new one, so the stage is not refused forever.
            if path.exists() {
                let _ = git_command(&folder).args(["worktree", "remove", "--force"]).arg(&path).output();
                if path.exists() {
                    let _ = std::fs::remove_dir_all(&path);
                }
                let _ = git_command(&folder).args(["worktree", "prune"]).output();
            }
            let output = git_command(&folder)
                .args(["worktree", "add", "--detach"])
                .arg(&path)
                .arg(&sha)
                .output()
                .context("Could not run git")?;
            ensure!(output.status.success(), "git worktree add failed: {}", String::from_utf8_lossy(&output.stderr).trim());
            prepared.folder = path.display().to_string();
            prepared.checkout = prepared.folder.clone();
            prepared.checkout_repo = folder;
        }
        ("checkout", true, None) => {
            prepared.holds = true;
            prepared.note = format!("The attempt recorded no commit, so the {kind} works in the task's folder, which is held.");
        }
        ("checkout", false, _) => {
            prepared.holds = true;
            prepared.note = format!("The task's folder has no Git, so the {kind} works in it, and it is held.");
        }
        _ => prepared.holds = true,
    }
    Ok(prepared)
}

/// Removes a stage's detached checkout and its temporary folder.
pub(crate) fn clean_up(run: &StageRun) -> anyhow::Result<()> {
    if !run.checkout.is_empty() && Path::new(&run.checkout).exists() {
        let output = git_command(&run.checkout_repo).args(["worktree", "remove", "--force"]).arg(&run.checkout).output().context("Could not run git")?;
        if !output.status.success() && Path::new(&run.checkout).exists() {
            bail!("git worktree remove failed: {}", String::from_utf8_lossy(&output.stderr).trim());
        }
        let _ = git_command(&run.checkout_repo).args(["worktree", "prune"]).output();
    }
    if !run.temp.is_empty() && Path::new(&run.temp).exists() {
        std::fs::remove_dir_all(&run.temp).with_context(|| format!("Could not remove {}", run.temp))?;
    }
    Ok(())
}

/// Records where the given stage works, once its member has it.
pub(crate) fn record(team: &mut Team, task_id: u32, run_id: &str, prepared: &Prepared) {
    let Some(run) = team.task_mut(task_id).and_then(|task| task.pipeline_run.as_mut()).and_then(|run| run.runs.iter_mut().find(|run| run.id == run_id)) else {
        return;
    };
    run.folder = prepared.folder.clone();
    run.checkout = prepared.checkout.clone();
    run.checkout_repo = prepared.checkout_repo.clone();
    run.holds = prepared.holds;
    run.changed = prepared.changed.clone();
    run.temp = prepared.temp.clone();
}

/// #630 N7: how long an unconfirmed stage hand-off keeps its member before the
/// stage goes back to Waiting.
pub(crate) const STAGE_UNCONFIRMED_GRACE: f64 = 120.0;

/// #630 N7: open stage runs whose member left the roster (removed or paused):
/// each goes back to Waiting and the lead hears why. #635 N3: an unconfirmed
/// hand-off is handled by the dispatcher only with evidence it did not arrive.
pub(crate) fn stalled(team: &Team, _at: f64) -> Vec<(u32, String, String)> {
    team.tasks
        .iter()
        .filter_map(|task| {
            let run = task.pipeline_run.as_ref()?.open_run()?;
            if run.state != StageState::Delivered || run.member.is_empty() {
                return None;
            }
            let member = team.member(&run.member);
            let reason = match member.map(|member| member.state) {
                Some(MemberState::Removed) => format!("{} was removed", team.name_of(&run.member)),
                Some(MemberState::Paused) => format!("{} was paused", team.name_of(&run.member)),
                _ => return None,
            };
            Some((task.id, run.id.clone(), reason))
        })
        .collect()
}

/// The task whose stage holds `member`'s folder: the implementer's next task
/// there waits until the stage ends. A test waiting for its member already
/// holds it under the hold rule; a command or land stage holds it from the
/// report on whatever its folder rule, so the checked commit stays checked and
/// landable (a checkout only keeps the command off the implementer's folder).
/// #538 N5: a test stage that may need the folder holds it from the report,
/// before it opens. #538 N4: a stage that ended while its own run went on keeps
/// the folder (or its checkout's folder) until that run ends.
/// #683: so does a review the preset puts before a command or land stage under
/// the hold rule: the later stage lands the commit the review reads, so the
/// implementer's folder must not move meanwhile. Under the checkout rule the
/// later stage reads its own detached checkout, and nothing changes.
pub(crate) fn held(team: &Team, member: &str) -> Option<u32> {
    held_by(team, member).map(|(task, _)| task)
}

/// #635 N4: the task and stage whose hold keeps `member`'s folder, for status.
pub(crate) fn held_by(team: &Team, member: &str) -> Option<(u32, String)> {
    let folder = team.member(member).and_then(|member| member_folder_path(team, member).ok()).unwrap_or_default();
    team.tasks.iter().find_map(|task| {
        let pipeline = task.pipeline_run.as_ref()?;
        if let Some(run) = pipeline.runs.iter().find(|run| run.awaiting_run && same_folder(&run.folder, &folder) && run_live(team, run)) {
            return Some((task.id, run.kind.clone()));
        }
        let run = pipeline.open_run()?;
        let holds = match run.kind.as_str() {
            "test" => {
                if run.state == StageState::Waiting {
                    // #538 N5: a checkout that will fall back holds the folder
                    // from the report on; one that checks a commit does not.
                    pipeline.folder != "checkout" || pipeline.hold_from_report
                } else {
                    run.holds
                }
            }
            "command" | "land" => true,
            // #683: under the hold rule a review before a command or land stage
            // holds the folder from the report; #538 N5: so does any stage before
            // a test that may need the folder.
            "review" => {
                (pipeline.folder != "checkout" && pipeline.stages.iter().skip(run.stage + 1).any(|stage| stage.runs_command()))
                    || (pipeline.hold_from_report && pipeline.stages.iter().skip(run.stage + 1).any(|stage| stage.kind == "test"))
            }
            _ => pipeline.hold_from_report && pipeline.stages.iter().skip(run.stage + 1).any(|stage| stage.kind == "test"),
        };
        let implementer = checked_attempt(task, run).map(|attempt| attempt.member.as_str()).unwrap_or(task.assignee.as_str());
        (holds && implementer == member).then(|| (task.id, run.kind.clone()))
    })
}

/// Whether two folder paths name the same place (the dispatcher's rule).
pub(super) fn same_folder(left: &str, right: &str) -> bool {
    let key = |path: &str| {
        let path = path.strip_prefix(r"\\?\").unwrap_or(path).replace('\\', "/");
        let path = path.trim_end_matches('/').to_owned();
        if path.as_bytes().get(1) == Some(&b':') { path.to_lowercase() } else { path }
    };
    !left.is_empty() && key(left) == key(right)
}

/// The members busy with a stage they were given.
pub(crate) fn busy(team: &Team) -> Vec<&str> {
    team.tasks
        .iter()
        .filter_map(|task| task.pipeline_run.as_ref()?.open_run())
        .filter(|run| run.state == StageState::Delivered && !run.member.is_empty())
        .map(|run| run.member.as_str())
        .collect()
}

/// The stages that wait for a member, oldest first: (task, candidates).
pub(crate) fn waiting(team: &Team) -> Vec<(u32, Vec<String>)> {
    let mut found: Vec<(f64, u32, Vec<String>)> = team
        .tasks
        .iter()
        .filter(|task| task.status.in_stage())
        .filter_map(|task| {
            let run = task.pipeline_run.as_ref()?.open_run()?;
            (run.state == StageState::Waiting).then(|| (run.at, task.id, pipeline::candidates(team, task.id)))
        })
        .collect();
    found.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    found
        .into_iter()
        .map(|(_, task, mut candidates)| {
            // #581 N1: a stage reserved for the member whose model switch or
            // context action it waits for goes only to that member.
            if let Some(run) = team.task(task).and_then(|task| task.pipeline_run.as_ref()).and_then(PipelineRun::open_run) {
                if !run.member.is_empty() {
                    candidates.retain(|member| *member == run.member);
                }
            }
            (task, candidates)
        })
        .collect()
}

/// The stages that ended with a checkout or a temporary folder left to remove.
/// #538 N4: a stage whose own run still goes keeps them until that run ends.
pub(crate) fn to_clean(team: &Team) -> Vec<(u32, String)> {
    team.tasks
        .iter()
        .filter_map(|task| task.pipeline_run.as_ref().map(|run| (task.id, run)))
        .flat_map(|(id, pipeline)| {
            pipeline
                .runs
                .iter()
                // #635 N2: a stalled (Waiting) stage's folder and checkout are
                // cleaned once its live run ends, like a closed stage's.
                .filter(|run| (!run.state.open() || !run.stalled.is_empty()) && (!run.awaiting_run || !run_live(team, run)) && (!run.checkout.is_empty() || !run.temp.is_empty()))
                .map(move |run| (id, run.id.clone()))
        })
        .collect()
}

/// #538 N4: the closed stages whose own run ended, so the dispatcher releases
/// their folder and lets the checkout and temporary folder go.
pub(crate) fn runs_ended(team: &Team) -> Vec<(u32, String)> {
    team.tasks
        .iter()
        .filter_map(|task| task.pipeline_run.as_ref().map(|pipeline| (task.id, pipeline)))
        .flat_map(|(id, pipeline)| {
            pipeline
                .runs
                .iter()
                .filter(|run| run.awaiting_run && !run_live(team, run))
                .map(move |run| (id, run.id.clone()))
        })
        .collect()
}

/// #538 N4: the closed stage's own run ended: release its folder and let its
/// checkout and temporary folder go.
pub(crate) fn release(team: &mut Team, task_id: u32, run_id: &str) -> bool {
    let Some(run) = team
        .task_mut(task_id)
        .and_then(|task| task.pipeline_run.as_mut())
        .and_then(|pipeline| pipeline.runs.iter_mut().find(|run| run.id == run_id))
    else {
        return false;
    };
    let was = run.awaiting_run;
    run.awaiting_run = false;
    run.stalled_member.clear();
    was
}

/// #538 N4: whether a closed stage's own run still goes: a `hgs team run` claim
/// in its member's folder (past its bound is not), in the stage's own folder,
/// or Zerus's own stage build. #635 N2 also checks the member a stall kept.
pub(crate) fn run_live(team: &Team, run: &StageRun) -> bool {
    let claim = |folder: &str| -> bool {
        let Some(marker) = super::outbox::running(Path::new(folder), &run.id) else { return false };
        let started = marker["started"].as_f64().unwrap_or(0.0);
        let bound = marker["timeout_secs"].as_f64().unwrap_or(1800.0);
        started > 0.0 && super::super::now() <= started + bound + 120.0
    };
    for member in [run.member.as_str(), run.stalled_member.as_str()] {
        if member.is_empty() {
            continue;
        }
        if let Some(found) = team.member(member) {
            if let Ok(folder) = member_folder_path(team, found) {
                if claim(&folder) {
                    return true;
                }
            }
        }
    }
    if !run.folder.is_empty() && claim(&run.folder) {
        return true;
    }
    super::super::builds::record(&run.id, super::super::now()).is_some_and(|record| record["status"] == "running")
}

/// A member's stage queue for status: the stage it has, then the waiting
/// stages it may take.
pub(crate) fn queue(team: &Team, member: &str) -> Vec<Value> {
    let mut queue = Vec::new();
    for task in &team.tasks {
        let Some(pipeline) = task.pipeline_run.as_ref() else { continue };
        let Some(run) = pipeline.open_run() else { continue };
        let entry = json!({"task":task.id,"stage":run.kind,"round":run.round,"state":run.state,"since":run.delivered_at.unwrap_or(run.at)});
        if run.state == StageState::Delivered && run.member == member {
            queue.insert(0, entry);
        } else if run.state == StageState::Waiting && pipeline::candidates(team, task.id).iter().any(|found| found == member) {
            queue.push(entry);
        }
    }
    queue
}

/// U21 for a stage: the folder a template run with this attempt id works in.
/// None when the id is no open stage of this member (an ordinary attempt).
/// A review runs nothing; a test runs builds and tests only, in the stage's
/// folder, and only while the stage is open.
pub(crate) fn run_folder(team: &Team, member: &str, attempt: &str, template: &str) -> anyhow::Result<Option<(PathBuf, String)>> {
    let Some(task) = pipeline::open_run_task(team, attempt, member) else {
        // #538 N4: a stage that ended before its run started never falls back to
        // the member's own folder; the request is refused instead. An open stage
        // of another member is simply not this member's run.
        let open_any = team
            .tasks
            .iter()
            .any(|task| task.pipeline_run.as_ref().and_then(PipelineRun::open_run).is_some_and(|run| run.id == attempt));
        ensure!(
            open_any || pipeline::run_member(team, attempt).is_none(),
            "The stage ended before this run started; ask again with a new request_id"
        );
        return Ok(None);
    };
    let run = team.task(task).and_then(|task| task.pipeline_run.as_ref()).and_then(PipelineRun::open_run).context("The stage ended")?;
    ensure!(run.kind == "test", "A review stage reads the task's files; it runs no command");
    ensure!(TEST_TEMPLATES.contains(&template), "A test stage runs builds and tests only ({}), not {template}", TEST_TEMPLATES.join(", "));
    ensure!(!run.folder.is_empty() && Path::new(&run.folder).is_dir(), "The stage's folder is not there");
    Ok(Some((PathBuf::from(&run.folder), run.temp.clone())))
}

/// The stage's hand-off: where, what changed, the criteria and the report,
/// and how to report the verdict.
pub(crate) fn hand_off_text(team: &Team, task: &Task, run: &StageRun, prepared: &Prepared, report_host: &str) -> String {
    let pipeline = task.pipeline_run.as_ref();
    let max_rounds = pipeline.map(|run| run.max_rounds).unwrap_or_default();
    let implementer = checked_attempt(task, run).map(|attempt| team.name_of(&attempt.member)).unwrap_or_default();
    let mut text = format!(
        "[Zerus team \u{b7} from: Zerus \u{b7} task: #{} \u{b7} {} stage \u{b7} round {} of {max_rounds}]\nTeam: {}. Your role: {}.\n",
        task.id,
        run.kind,
        run.round,
        team.name,
        team.role_of(&run.member)
    );
    text += &if run.kind == "test" {
        format!("\nTest task #{}: {} by {implementer} (attempt {}). Run only the suites of the files it changed and take the criteria's shots.\n", task.id, task.title, run.attempt)
    } else {
        format!("\nReview task #{}: {} by {implementer} (attempt {}) against its criteria.\n", task.id, task.title, run.attempt)
    };
    if prepared.checkout.is_empty() {
        text += &format!("Folder: {} (the task's own folder; never change the task's files).\n", prepared.folder);
    } else {
        text += &format!("Folder: {} (a detached checkout of the attempt's commits for this test; Zerus removes it after the stage).\n", prepared.folder);
    }
    if prepared.holds {
        text += "The implementer's next task in this folder waits until your verdict.\n";
    }
    if !prepared.note.is_empty() {
        text += &format!("{}\n", prepared.note);
    }
    if run.kind == "test" {
        text += &format!(
            "Build and test there with hgs team run, naming this stage as the attempt ({}); the runs write only build output and your temporary folder {}. They end with the stage.\n",
            TEST_TEMPLATES.join(", "),
            prepared.temp
        );
    } else {
        text += "Read only: read the change with read-only git (show, diff, log); a review builds, tests and changes nothing.\n";
    }
    if !prepared.changed.is_empty() {
        text += "\nWhat changed:\n";
        for item in &prepared.changed {
            text += &if let Some(sha) = item["commit"].as_str() {
                format!("- commit {} ({} files)\n", &sha[..sha.len().min(12)], item["files"])
            } else if item["deleted"] == true {
                format!("- {} (deleted)\n", item["path"].as_str().unwrap_or_default())
            } else {
                format!(
                    "- {} ({} bytes{})\n",
                    item["path"].as_str().unwrap_or_default(),
                    item["size"],
                    item["sha256"].as_str().map(|sha| format!(", sha256 {sha}")).unwrap_or_default()
                )
            };
        }
    }
    if !task.criteria.is_empty() {
        text += "\nAcceptance criteria:\n";
        for criterion in &task.criteria {
            text += &format!("- {criterion}\n");
        }
    }
    text += &format!("\n{STAGE_RULES}");
    if let Some(report) = checked_attempt(task, run).and_then(|attempt| attempt.report.as_ref()) {
        let body: String = report.text.chars().take(4000).collect();
        text += &format!("\n{implementer}'s report (claims to check, not evidence):\n{body}\n");
    }
    let earlier = pipeline::rounds_text(task);
    if !earlier.is_empty() {
        text += &format!("\nEarlier rounds:\n{earlier}");
    }
    let at = if report_host.is_empty() { String::new() } else { format!("@{report_host} ") };
    let verdict = json!({"request_id":uuid::Uuid::new_v4().to_string(),"team":team.id,"task":task.id,"attempt":run.id,
        "verdict":"pass","text":"...","findings":[]});
    text += &format!(
        "\nReport once with your verdict: pass, or return with each finding in \"findings\" (it goes back to {implementer} as a new attempt):\n{verdict} | hgs {at}team report --json\n"
    );
    // H5 for a stage member (#630 N6): the question route, the current-stage
    // route, and the capture rule: a reply is not a verdict.
    let ask = json!({"request_id":uuid::Uuid::new_v4().to_string(),"team":team.id,"task":task.id,"text":"..."});
    text += &format!(
        "Questions for the lead: {ask} | hgs {at}team ask --json\n\
         Your stage and attempt again: hgs {at}team current --json\n\
         A reply in your turn is not a verdict: end with the report command above. Without one, Zerus keeps the stage waiting and tells the lead it stalled.\n"
    );
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::teams::catalog::{builtin, Catalog};
    use crate::state::teams::dispatcher::set_git_ceiling;
    use crate::state::teams::ops::{apply, create, Actor};
    use std::collections::BTreeSet;
    use std::process::Command;

    fn git(folder: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(["-c", "user.name=Zerus Test", "-c", "user.email=test@example.com", "-c", "core.hooksPath=", "-c", "commit.gpgsign=false"])
            .arg("-C")
            .arg(folder)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// The team as the store keeps it: serde_json::to_string, then read back.
    /// A worktree member's team has the merge queue on by default (owner,
    /// 2026-11-09): the lead's accept then only queues the commit, and the stage
    /// ends when the queue lands it. These tests are about the accept that ends
    /// the stage at once, so their lead lands by hand (task #19: no queue).
    fn lands_by_hand(team: &mut Team) {
        team.settings.manual_line = "main".into();
        assert!(!super::super::merge_queue::enabled(team));
    }

    fn round_trip(team: &Team) -> Team {
        serde_json::from_str(&serde_json::to_string(team).unwrap()).unwrap()
    }

    fn write(folder: &Path, file: &str, text: &str) {
        let path = folder.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// The project: a Git repository with one commit, or a plain folder.
    fn project(dir: &Path, with_git: bool) -> PathBuf {
        let folder = dir.join("proj");
        std::fs::create_dir_all(&folder).unwrap();
        write(&folder, "src/lib.rs", "pub fn one() {}\n");
        if with_git {
            git(&folder, &["init", "-q", "-b", "main"]);
            git(&folder, &["add", "-A"]);
            git(&folder, &["commit", "-q", "-m", "start"]);
        }
        folder
    }

    /// A running team in `folder`: dev implements #1 (and has #2 next), rev
    /// reviews, qa tests; #1 takes the preset `pipeline` of `catalog`. dev's
    /// folder is made as Zerus lays it out: a worktree or a clone of the
    /// project, or the project folder itself.
    fn team_in(folder: &Path, dev_folder: MemberFolder, catalog: &Catalog, pipeline: &str) -> (Team, PathBuf) {
        let (mut team, _) = create("5ea7f00d-0000-4000-8000-000000000532", &json!({"name":"T","lead":{"id":"lead","agent":"claude"}})).unwrap();
        let mut events = Vec::new();
        apply(
            &mut team,
            "plan",
            &json!({"members":[{"id":"dev","role":"Backend","agent":"codex"},{"id":"rev","role":"Reviewer","agent":"claude"},
                    {"id":"qa","role":"Tester","agent":"kimi"}],
                "tasks":[{"id":1,"title":"API","assignee":"dev","criteria":["Paged"]},{"id":2,"title":"More","assignee":"dev"}]}),
            &Actor::Human,
            &BTreeSet::new(),
            &mut events,
        )
        .unwrap();
        apply(&mut team, "approve", &json!({}), &Actor::Human, &BTreeSet::new(), &mut events).unwrap();
        team.folder = folder.display().to_string();
        for member in &mut team.members {
            member.folder = if member.id == "dev" { dev_folder } else { MemberFolder::Shared };
        }
        team.task_mut(1).unwrap().pipeline = pipeline.into();
        pipeline::snapshot_with(&mut team, 1, catalog);
        let path = PathBuf::from(member_folder_path(&team, team.member("dev").unwrap()).unwrap());
        match dev_folder {
            MemberFolder::Worktree => {
                git(folder, &["worktree", "add", "-q", "-b", "dev", path.to_str().unwrap()]);
            }
            MemberFolder::Clone => {
                let output = Command::new("git").args(["clone", "-q"]).arg(folder).arg(&path).output().unwrap();
                assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            }
            MemberFolder::Shared => assert_eq!(path, folder),
        }
        (team, path)
    }

    /// dev reports #1 with its changed files and, when asked, a commit Zerus
    /// made for the attempt (B4).
    fn report(team: &mut Team, folder: &Path, commit: bool) {
        write(folder, "src/lib.rs", "pub fn one() {}\npub fn two() {}\n");
        write(folder, "docs/api.md", "# API\n");
        let mut commits = Vec::new();
        if commit {
            git(folder, &["add", "-A"]);
            git(folder, &["commit", "-q", "-m", "API"]);
            commits.push(CommitRef { sha: git(folder, &["rev-parse", "HEAD"]), at: 1.0, by: "Zerus".into(), files: 2 });
        }
        let task = team.task_mut(1).unwrap();
        task.attempts.push(Attempt { n: 1, id: "a1".into(), member: "dev".into(), state: AttemptState::Delivered, commits, ..Default::default() });
        task.status = TaskStatus::InProgress;
        let mut events = Vec::new();
        let value = apply(
            team,
            "report",
            &json!({"task":1,"attempt":"a1","text":"Done: two() added",
                "report":{"summary":"Done","files":["src/lib.rs","docs/api.md","old.txt"]}}),
            &Actor::Member("dev".into()),
            &BTreeSet::new(),
            &mut events,
        )
        .unwrap();
        assert_eq!(value["status"], "in_review", "{value}");
    }

    fn verdict(team: &mut Team, member: &str, run: &str) {
        let mut events = Vec::new();
        apply(team, "report", &json!({"task":1,"attempt":run,"verdict":"pass"}), &Actor::Member(member.into()), &BTreeSet::new(), &mut events).unwrap();
    }

    /// Prepares the open stage and gives it to `member`, as the dispatcher does.
    fn give(team: &mut Team, member: &str) -> (String, Prepared) {
        let prepared = prepare(team, 1).unwrap();
        let run = pipeline::give_stage(team, 1, member).unwrap();
        record(team, 1, &run, &prepared);
        (run, prepared)
    }

    fn run_of(team: &Team, id: &str) -> StageRun {
        team.task(1).unwrap().pipeline_run.as_ref().unwrap().runs.iter().find(|run| run.id == id).unwrap().clone()
    }

    fn isolated<T>(body: impl FnOnce(&Path) -> T) -> T {
        let dir = tempfile::tempdir().unwrap();
        let path = dunce(dir.path());
        set_git_ceiling(&path.display().to_string());
        let result = crate::state::with_test_state_root(&path.join("state"), || body(&path));
        drop(dir);
        result
    }

    /// The temp dir without Windows' \\?\ prefix, so Git takes it.
    fn dunce(path: &Path) -> PathBuf {
        let path = path.canonicalize().unwrap();
        PathBuf::from(path.display().to_string().trim_start_matches(r"\\?\"))
    }

    fn checkout_catalog() -> Catalog {
        let mut catalog = builtin();
        catalog.pipelines[0].folder = "checkout".into();
        catalog
    }

    #[test]
    fn a_worktree_task_is_reviewed_by_its_commits_and_its_test_holds_the_folder() {
        isolated(|dir| {
            let project = project(dir, true);
            let (mut team, worktree) = team_in(&project, MemberFolder::Worktree, &builtin(), "code");
            report(&mut team, &worktree, true);
            let sha = team.task(1).unwrap().attempts[0].commits[0].sha.clone();
            // The review reads the implementer's worktree and its commit.
            let (review, prepared) = give(&mut team, "rev");
            assert_eq!(PathBuf::from(&prepared.folder), worktree);
            assert_eq!(prepared.changed, [json!({"commit":sha,"files":2})]);
            assert!(!prepared.holds && prepared.checkout.is_empty() && prepared.temp.is_empty(), "a review holds nothing");
            let task = team.task(1).unwrap();
            let text = hand_off_text(&team, task, &run_of(&team, &review), &prepared, "");
            assert!(text.contains(&format!("Folder: {} (the task's own folder", worktree.display())), "{text}");
            assert!(text.contains(&format!("- commit {} (2 files)", &sha[..12])), "{text}");
            assert!(text.contains("Acceptance criteria:\n- Paged") && text.contains("dev (Backend)'s report (claims to check, not evidence):\nDone: two() added")
                && text.contains("Judge each acceptance criterion on its own line")
                && text.contains("deletes, skips or narrows a test")
                && text.contains("Read the whole change again every round")
                && text.contains("If you could not read the change, do not pass"), "{text}");
            assert!(text.contains(&format!("\"attempt\":\"{review}\"")) && text.contains("Read only: read the change with read-only git"), "{text}");
            // #538 N5: the test stage ahead holds the folder from the report,
            // so the implementer's next task waits during the review.
            assert_eq!(held(&team, "dev"), Some(1), "the test ahead holds the folder from the report");
            verdict(&mut team, "rev", &review);
            // The test holds the worktree from the moment it waits: dev's #2
            // waits and says so.
            assert_eq!(held(&team, "dev"), Some(1));
            let waits = crate::state::teams::inbox::waits(&team, team.task(2).unwrap(), true).unwrap();
            assert_eq!(waits["unblock"], "folder held by the stage of #1", "{waits}");
            let (test, prepared) = give(&mut team, "qa");
            assert!(prepared.holds && Path::new(&prepared.folder) == worktree);
            assert!(Path::new(&prepared.temp).is_dir(), "the test's own temporary folder");
            assert_eq!(prepared.changed, [json!({"commit":sha,"files":2})], "the round's first stage found these");
            assert_eq!(held(&team, "dev"), Some(1));
            verdict(&mut team, "qa", &test);
            assert_eq!(held(&team, "dev"), None, "the hold ends with the test");
            // The stage closed; its own run (there was none) is released on the
            // dispatcher's tick, then the temporary folder goes.
            assert_eq!(runs_ended(&team), [(1, test.clone())]);
            assert!(release(&mut team, 1, &test));
            let ended = run_of(&team, &test);
            assert_eq!(to_clean(&team), [(1, test.clone())]);
            clean_up(&ended).unwrap();
            assert!(!Path::new(&ended.temp).exists());
        });
    }

    #[test]
    fn a_stage_member_asks_and_the_answer_reaches_its_run() {
        isolated(|dir| {
            let project = project(dir, true);
            let (mut team, _) = team_in(&project, MemberFolder::Shared, &builtin(), "code");
            report(&mut team, &project, true);
            let (run, prepared) = give(&mut team, "rev");
            let mut events = Vec::new();
            let none = BTreeSet::new();
            let asked = apply(&mut team, "ask", &json!({"task":1,"text":"Which line?"}), &Actor::Member("rev".into()), &none, &mut events).unwrap();
            assert_eq!((asked["status"].as_str(), asked["stage"].as_str(), asked["task"].as_u64()), (Some("asked"), Some("review"), Some(1)));
            let question = asked["question"].as_str().unwrap().to_owned();
            let stage = run_of(&team, &run);
            assert_eq!((stage.questions.len(), stage.questions[0].text.as_str()), (1, "Which line?"), "{stage:?}");
            // The lead's inbox lists the stage member's question.
            let items = crate::state::teams::inbox::items(&team);
            assert!(items.iter().any(|item| item.kind == "question" && item.summary == "Which line?"), "{items:?}");
            // The lead answers it; the answer stays on the stage run.
            apply(&mut team, "reply", &json!({"question":question,"text":"Line 3"}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
            let stage = run_of(&team, &run);
            assert_eq!((stage.questions[0].answer.as_str(), stage.questions[0].notice), ("Line 3", Notice::Pending));
            assert_eq!(events.last().unwrap().kind, "question_answered");
            assert_eq!(events.last().unwrap().payload["stage"], "review");
            // The hand-off names the ask and current routes and the capture rule.
            let text = hand_off_text(&team, team.task(1).unwrap(), &run_of(&team, &run), &prepared, "");
            for line in ["hgs team ask --json", "hgs team current --json", "A reply in your turn is not a verdict"] {
                assert!(text.contains(line), "{line} missing: {text}");
            }
        });
    }

    #[test]
    fn a_removed_or_paused_stage_member_frees_the_stage_and_the_lead_hears_it() {
        isolated(|dir| {
            let project = project(dir, true);
            // Removed: rev has no open task of its own.
            let (mut team, _) = team_in(&project, MemberFolder::Shared, &builtin(), "code");
            report(&mut team, &project, true);
            let (run, _) = give(&mut team, "rev");
            let mut events = Vec::new();
            apply(&mut team, "remove-member", &json!({"member":"rev"}), &Actor::Human, &BTreeSet::new(), &mut events).unwrap();
            let stage = run_of(&team, &run);
            assert_eq!((stage.state, stage.member.as_str(), stage.stalled_member.as_str()), (StageState::Waiting, "", "rev"), "the member is kept for its late verdict, not as the stage's owner: {stage:?}");
            assert!(stage.stalled.contains("was removed"), "{stage:?}");
            assert!(!stage.awaiting_run && stage.checkout.is_empty() && stage.temp.is_empty(), "{stage:?}");
            let items = crate::state::teams::inbox::items(&team);
            assert!(items.iter().any(|item| item.kind == "stage_stalled" && item.task == Some(1)), "{items:?}");
            // #727: a peer takes the stalled stage; the removed member does not
            // reserve it.
            team.members.push(Member {
                id: "rev2".into(),
                role: "Reviewer".into(),
                agent: "claude".into(),
                state: MemberState::Active,
                folder: MemberFolder::Shared,
                session: Some(SessionRef { name: "claude/x/rev2".into(), run_id: "r4".into(), ..Default::default() }),
                ..Default::default()
            });
            assert!(waiting(&team).iter().any(|(task, candidates)| *task == 1 && candidates.contains(&"rev2".to_owned())), "{:?}", waiting(&team));
            pipeline::give_stage(&mut team, 1, "rev2").unwrap();
            let stage = run_of(&team, &run);
            assert_eq!((stage.state, stage.member.as_str()), (StageState::Delivered, "rev2"), "{stage:?}");
            // Paused: the same reset with its reason.
            let (mut team, _) = team_in(&project, MemberFolder::Shared, &builtin(), "code");
            report(&mut team, &project, false);
            let (run, _) = give(&mut team, "rev");
            apply(&mut team, "pause-member", &json!({"member":"rev"}), &Actor::Human, &BTreeSet::new(), &mut events).unwrap();
            let stage = run_of(&team, &run);
            assert_eq!((stage.state, stage.member.as_str(), stage.stalled_member.as_str()), (StageState::Waiting, "", "rev"), "{stage:?}");
            assert!(stage.stalled.contains("was paused"), "{stage:?}");
            let items = crate::state::teams::inbox::items(&team);
            assert!(items.iter().any(|item| item.kind == "stage_stalled" && item.summary.contains("was paused")), "{items:?}");
        });
    }

    #[test]
    fn an_unconfirmed_stage_hand_off_alone_is_not_stalled() {
        isolated(|dir| {
            let project = project(dir, true);
            let (mut team, _) = team_in(&project, MemberFolder::Shared, &builtin(), "code");
            report(&mut team, &project, true);
            let (run_id, _) = give(&mut team, "rev");
            let delivered = run_of(&team, &run_id).delivered_at.unwrap();
            {
                let found = team.task_mut(1).unwrap().pipeline_run.as_mut().unwrap().runs.iter_mut().find(|stage| stage.id == run_id).unwrap();
                found.unconfirmed = true;
                found.delivered_at = Some(delivered - STAGE_UNCONFIRMED_GRACE - 1.0);
            }
            // #635 N3: the age alone is not evidence; the dispatcher stalls it
            // only when the member is idle and free with no question or run.
            assert!(stalled(&team, delivered).is_empty(), "an unconfirmed hand-off is not stalled by time alone");
            // Once the dispatcher has its evidence it resets the run and the
            // lead hears it; the member stays for a late verdict.
            let mut events = Vec::new();
            assert!(pipeline::stall_stage(&mut team, 1, &run_id, "the stage hand-off was not confirmed", &mut events));
            let stage = run_of(&team, &run_id);
            assert_eq!((stage.state, stage.member.as_str(), stage.stalled_member.as_str(), stage.unconfirmed), (StageState::Waiting, "", "rev", false), "{stage:?}");
            assert!(stage.stalled.contains("not confirmed"), "{stage:?}");
            let items = crate::state::teams::inbox::items(&team);
            assert!(items.iter().any(|item| item.kind == "stage_stalled" && item.task == Some(1)), "{items:?}");
        });
    }

    /// #635 N2: pausing a member during its live test run keeps the folder and
    /// checkout until the run ends, and StageCleanup removes them after.
    #[test]
    fn a_pause_during_a_live_test_run_keeps_the_folder_and_checkout() {
        isolated(|dir| {
            let project = project(dir, true);
            let (mut team, _) = team_in(&project, MemberFolder::Worktree, &checkout_catalog(), "code");
            report(&mut team, &project, true);
            let (review, _) = give(&mut team, "rev");
            verdict(&mut team, "rev", &review);
            let (run_id, prepared) = give(&mut team, "qa");
            assert!(!prepared.checkout.is_empty(), "{prepared:?}");
            let checkout = PathBuf::from(&prepared.checkout);
            let qa_folder = member_folder_path(&team, team.member("qa").unwrap()).unwrap();
            super::super::outbox::write_running(
                Path::new(&qa_folder),
                &run_id,
                &json!({"request_id":run_id,"started":crate::state::now(),"timeout_secs":1800,"label":"cargo-test"}),
            )
            .unwrap();
            let mut events = Vec::new();
            apply(&mut team, "pause-member", &json!({"member":"qa"}), &Actor::Human, &BTreeSet::new(), &mut events).unwrap();
            let stage = run_of(&team, &run_id);
            assert_eq!((stage.state, stage.member.as_str(), stage.stalled_member.as_str()), (StageState::Waiting, "", "qa"), "{stage:?}");
            assert!(stage.awaiting_run && stage.stalled.contains("was paused"), "{stage:?}");
            assert_eq!(stage.checkout, prepared.checkout, "the checkout stays while the run is live");
            assert!(checkout.exists(), "the live run's checkout is not removed under it");
            // The run ends: the folder and checkout are released and cleaned.
            super::super::outbox::settle_running(Path::new(&qa_folder), &run_id, &json!({"status":"ran","exit":0})).unwrap();
            assert!(runs_ended(&team).iter().any(|(task, run)| *task == 1 && *run == run_id), "the ended run is released");
            assert!(release(&mut team, 1, &run_id));
            assert!(to_clean(&team).iter().any(|(task, run)| *task == 1 && *run == run_id), "its checkout is cleaned once the run ended");
            clean_up(&run_of(&team, &run_id)).unwrap();
            assert!(!checkout.exists(), "the checkout is removed after the run ended");
        });
    }

    #[test]
    fn a_clone_task_is_tested_on_a_detached_checkout_that_goes_after_the_stage() {
        isolated(|dir| {
            let project = project(dir, true);
            let (mut team, clone) = team_in(&project, MemberFolder::Clone, &checkout_catalog(), "code");
            report(&mut team, &clone, true);
            let sha = team.task(1).unwrap().attempts[0].commits[0].sha.clone();
            let (review, _) = give(&mut team, "rev");
            verdict(&mut team, "rev", &review);
            // The checkout rule: no hold, and dev's next task goes ahead.
            assert_eq!(held(&team, "dev"), None);
            let (test, prepared) = give(&mut team, "qa");
            let checkout = PathBuf::from(&prepared.checkout);
            assert!(checkout.join("src/lib.rs").is_file(), "{prepared:?}");
            assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), sha);
            assert_eq!(PathBuf::from(&prepared.folder), checkout);
            assert_eq!(PathBuf::from(&prepared.checkout_repo), clone);
            assert!(!prepared.holds && held(&team, "dev").is_none());
            let text = hand_off_text(&team, team.task(1).unwrap(), &run_of(&team, &test), &prepared, "");
            assert!(text.contains("a detached checkout of the attempt's commits"), "{text}");
            verdict(&mut team, "qa", &test);
            let ended = run_of(&team, &test);
            clean_up(&ended).unwrap();
            assert!(!checkout.exists(), "the checkout is removed after the stage");
            assert!(!git(&clone, &["worktree", "list"]).contains(&checkout.display().to_string().replace('\\', "/")));
        });
    }

    #[test]
    fn a_stale_checkout_at_the_stage_path_is_removed_and_replaced() {
        isolated(|dir| {
            let project = project(dir, true);
            let (mut team, folder) = team_in(&project, MemberFolder::Worktree, &checkout_catalog(), "code");
            report(&mut team, &folder, true);
            let sha = team.task(1).unwrap().attempts[0].commits[0].sha.clone();
            let (review, _) = give(&mut team, "rev");
            verdict(&mut team, "rev", &review);
            let first = prepare(&team, 1).unwrap();
            assert!(Path::new(&first.checkout).is_dir(), "{first:?}");
            // #684 L1: the same run prepared again (a crash, or a start never
            // recorded) must not be refused by the worktree already at its path.
            let second = prepare(&team, 1).unwrap();
            assert_eq!(second.checkout, first.checkout);
            assert_eq!(git(Path::new(&second.folder), &["rev-parse", "HEAD"]), sha);
            clean_up(&StageRun {
                checkout: second.checkout,
                checkout_repo: second.checkout_repo,
                temp: second.temp,
                ..Default::default()
            })
            .unwrap();
        });
    }

    #[test]
    fn a_shared_folder_task_lists_its_files_and_a_checkout_without_commits_holds() {
        isolated(|dir| {
            let project = project(dir, true);
            let (mut team, folder) = team_in(&project, MemberFolder::Shared, &checkout_catalog(), "code");
            assert_eq!(folder, project, "the members work in the shared folder");
            // No commit for the attempt: what changed is the reported files.
            report(&mut team, &folder, false);
            let (review, prepared) = give(&mut team, "rev");
            assert_eq!(PathBuf::from(&prepared.folder), project);
            let digest = |text: &str| format!("{:x}", Sha256::digest(text.as_bytes()))[..16].to_owned();
            assert_eq!(
                prepared.changed,
                [
                    json!({"path":"src/lib.rs","size":32,"sha256":digest("pub fn one() {}\npub fn two() {}\n")}),
                    json!({"path":"docs/api.md","size":6,"sha256":digest("# API\n")}),
                    json!({"path":"old.txt","deleted":true}),
                ]
            );
            let text = hand_off_text(&team, team.task(1).unwrap(), &run_of(&team, &review), &prepared, "");
            assert!(text.contains("- src/lib.rs (32 bytes, sha256 ") && text.contains("- old.txt (deleted)"), "{text}");
            verdict(&mut team, "rev", &review);
            let (_, prepared) = give(&mut team, "qa");
            assert!(prepared.holds && prepared.checkout.is_empty(), "{prepared:?}");
            assert!(prepared.note.contains("recorded no commit"), "{prepared:?}");
            assert_eq!(held(&team, "dev"), Some(1), "the implementer's next task in the shared folder waits");
            assert_eq!(held(&team, "rev"), None, "other members keep working there");
        });
    }

    #[test]
    fn a_folder_without_git_works_and_git_is_never_initialized() {
        isolated(|dir| {
            let project = project(dir, false);
            let (mut team, folder) = team_in(&project, MemberFolder::Shared, &checkout_catalog(), "code");
            assert!(!is_git(&folder.display().to_string()));
            report(&mut team, &folder, false);
            let (review, prepared) = give(&mut team, "rev");
            assert_eq!(prepared.changed.len(), 3);
            assert_eq!(prepared.changed[1]["path"], "docs/api.md");
            verdict(&mut team, "rev", &review);
            let (test, prepared) = give(&mut team, "qa");
            assert!(prepared.holds && prepared.note.contains("has no Git"), "{prepared:?}");
            verdict(&mut team, "qa", &test);
            clean_up(&run_of(&team, &test)).unwrap();
            assert!(!folder.join(".git").exists(), "Zerus never initializes Git");
        });
    }

    #[test]
    fn stage_permissions_follow_the_stage_and_end_with_it() {
        isolated(|dir| {
            let project = project(dir, false);
            let (mut team, folder) = team_in(&project, MemberFolder::Shared, &builtin(), "code");
            report(&mut team, &folder, false);
            let (review, _) = give(&mut team, "rev");
            // A review reads: it runs nothing.
            let error = run_folder(&team, "rev", &review, "cargo-test").unwrap_err().to_string();
            assert!(error.contains("A review stage reads"), "{error}");
            verdict(&mut team, "rev", &review);
            let (test, prepared) = give(&mut team, "qa");
            // A test builds and tests in the task's folder with its own temp
            // folder; it never merges or commits there.
            let (at, temp) = run_folder(&team, "qa", &test, "cargo-test").unwrap().unwrap();
            assert_eq!((at, temp.clone()), (folder.clone(), prepared.temp.clone()));
            for template in ["git-commit", "git-merge-lead", "project-script"] {
                assert!(run_folder(&team, "qa", &test, template).is_err(), "{template}");
            }
            // Only its own member, and the implementer never.
            assert_eq!(run_folder(&team, "dev", &test, "cargo-test").unwrap(), None);
            assert_eq!(run_folder(&team, "rev", &test, "cargo-test").unwrap(), None);
            // The stage ends: the same request is refused - it never falls back
            // to the member's own folder - and it is no stage of this member any
            // more.
            verdict(&mut team, "qa", &test);
            let error = run_folder(&team, "qa", &test, "cargo-test").unwrap_err().to_string();
            assert!(error.contains("The stage ended before this run started"), "{error}");
            assert_eq!(pipeline::open_run_task(&team, &test, "qa"), None);
            clean_up(&run_of(&team, &test)).unwrap();
            assert!(!Path::new(&temp).exists(), "its temporary folder goes with it");
        });
    }

    #[test]
    fn a_checkout_test_holds_the_folder_from_the_report_only_when_it_will_fall_back() {
        isolated(|dir| {
            // A folder without Git: the checkout cannot be made, so the test
            // will work in the implementer's folder; the report holds it at once.
            let project = project(dir, false);
            let (mut team, folder) = team_in(&project, MemberFolder::Shared, &checkout_catalog(), "code");
            report(&mut team, &folder, false);
            // The store saves the team and every tick reads it back: the hold
            // from the report must survive that.
            let mut team = round_trip(&team);
            let (review, prepared) = give(&mut team, "rev");
            assert!(!prepared.holds && prepared.checkout.is_empty(), "the review prepares nothing");
            assert_eq!(held(&team, "dev"), Some(1), "the checkout will fall back, so the folder is held from the report");
            verdict(&mut team, "rev", &review);
            let (_, prepared) = give(&mut team, "qa");
            assert!(prepared.holds && Path::new(&prepared.folder) == folder, "the test falls back to the held folder");
        });
    }

    #[test]
    fn the_reported_files_are_hashed_at_the_report_not_at_the_stage() {
        isolated(|dir| {
            let project = project(dir, false);
            let (mut team, folder) = team_in(&project, MemberFolder::Shared, &builtin(), "code");
            report(&mut team, &folder, false);
            // The report's file state is saved with the team and read back.
            let mut team = round_trip(&team);
            let reported = team.task(1).unwrap().attempts[0].report_files.clone();
            let before = reported.iter().find(|item| item["path"] == "src/lib.rs").cloned().unwrap();
            assert!(before["size"].as_u64().unwrap_or(0) > 0, "{before}");
            // Another task edits the report's file before the review is given.
            write(&folder, "src/lib.rs", "pub fn one() {}\npub fn two() {}\npub fn three() {}\n");
            let (_, prepared) = give(&mut team, "rev");
            let after = prepared.changed.iter().find(|item| item["path"] == "src/lib.rs").cloned().unwrap();
            assert_eq!(after, before, "the stage sees the report's state, not the later one");
        });
    }

    #[test]
    fn a_closed_stage_holds_its_folder_until_its_own_run_ends() {
        isolated(|dir| {
            let project = project(dir, true);
            let (mut team, worktree) = team_in(&project, MemberFolder::Worktree, &builtin(), "code");
            lands_by_hand(&mut team);
            report(&mut team, &worktree, true);
            let (review, _) = give(&mut team, "rev");
            verdict(&mut team, "rev", &review);
            let (test, _) = give(&mut team, "qa");
            // The tester's run is claimed in the tester's own folder under the
            // stage's id, and it goes in the stage's folder (dev's worktree).
            let tester = member_folder_path(&team, team.member("qa").unwrap()).unwrap();
            let marker = json!({"request_id":test,"started":crate::state::now(),"timeout_secs":3600.0,"label":"cargo-test","command":"cargo test"});
            super::super::outbox::write_running(Path::new(&tester), &test, &marker).unwrap();
            // The lead accepts: the stage ends, but its run still goes.
            let mut events = Vec::new();
            apply(&mut team, "accept", &json!({"task":1,"reason":"Fine"}), &Actor::Human, &BTreeSet::new(), &mut events).unwrap();
            // The store saves the team and the next tick reads it back: the
            // closed stage's run must survive that.
            let mut team = round_trip(&team);
            assert_eq!(held(&team, "dev"), Some(1), "the folder stays held while the run goes");
            assert!(to_clean(&team).is_empty(), "the test's temporary folder stays too");
            assert!(runs_ended(&team).is_empty(), "the run is still going");
            // It ends: the folder and the temporary folder are released.
            std::fs::remove_file(super::super::outbox::running_path(Path::new(&tester), &test)).unwrap();
            assert_eq!(runs_ended(&team), [(1, test.clone())]);
            assert!(release(&mut team, 1, &test));
            assert_eq!(held(&team, "dev"), None);
            assert_eq!(to_clean(&team), [(1, test.clone())]);
            clean_up(&run_of(&team, &test)).unwrap();
        });
    }

    #[test]
    fn the_stage_facts_survive_a_save_and_reload_and_stay_out_of_the_status_wire() {
        isolated(|dir| {
            let project = project(dir, true);
            let (mut team, worktree) = team_in(&project, MemberFolder::Worktree, &builtin(), "code");
            lands_by_hand(&mut team);
            report(&mut team, &worktree, true);
            // The hold from the report and the report's file state are saved
            // with the team and read back (the store writes serde_json::to_string).
            let mut team = round_trip(&team);
            assert!(team.task(1).unwrap().pipeline_run.as_ref().unwrap().hold_from_report);
            assert!(!team.task(1).unwrap().attempts[0].report_files.is_empty());
            assert_eq!(held(&team, "dev"), Some(1), "the hold survives the save");
            // They never reach a client.
            let view = super::super::commands::view_with(&team, &[], None);
            assert!(view["tasks"][0]["attempts"][0].get("report_files").is_none(), "{view}");
            assert!(view["tasks"][0]["pipeline_run"].get("hold_from_report").is_none(), "{view}");
            // A closed stage's run is saved too and keeps the folder held.
            let (review, _) = give(&mut team, "rev");
            verdict(&mut team, "rev", &review);
            let (test, _) = give(&mut team, "qa");
            let tester = member_folder_path(&team, team.member("qa").unwrap()).unwrap();
            let marker = json!({"request_id":test,"started":crate::state::now(),"timeout_secs":3600.0,"label":"cargo-test","command":"cargo test"});
            super::super::outbox::write_running(Path::new(&tester), &test, &marker).unwrap();
            let mut events = Vec::new();
            apply(&mut team, "accept", &json!({"task":1,"reason":"Fine"}), &Actor::Human, &BTreeSet::new(), &mut events).unwrap();
            let mut team = round_trip(&team);
            assert!(team.task(1).unwrap().pipeline_run.as_ref().unwrap().runs.iter().any(|run| run.id == test && run.awaiting_run));
            assert_eq!(held(&team, "dev"), Some(1), "the hold survives the save");
            let view = super::super::commands::view_with(&team, &[], None);
            assert!(
                view["tasks"][0]["pipeline_run"]["runs"].as_array().unwrap().iter().all(|run| run.get("awaiting_run").is_none()),
                "{view}"
            );
            std::fs::remove_file(super::super::outbox::running_path(Path::new(&tester), &test)).unwrap();
            assert_eq!(runs_ended(&team), [(1, test.clone())]);
            assert!(release(&mut team, 1, &test));
            assert_eq!(held(&team, "dev"), None);
        });
    }

    #[test]
    fn the_stage_queue_lists_the_given_stage_then_the_waiting_ones() {
        isolated(|dir| {
            let project = project(dir, false);
            let (mut team, folder) = team_in(&project, MemberFolder::Shared, &builtin(), "code");
            report(&mut team, &folder, false);
            assert_eq!(queue(&team, "rev").len(), 1, "a waiting review it may take");
            assert!(queue(&team, "dev").is_empty() && queue(&team, "qa").is_empty());
            assert_eq!(waiting(&team), [(1, vec!["rev".to_owned()])]);
            assert!(busy(&team).is_empty());
            give(&mut team, "rev");
            let queue = queue(&team, "rev");
            assert_eq!((queue[0]["task"].clone(), queue[0]["stage"].clone(), queue[0]["state"].clone()), (json!(1), json!("review"), json!("delivered")));
            assert_eq!(busy(&team), ["rev"]);
            assert!(waiting(&team).is_empty());
        });
    }
}
