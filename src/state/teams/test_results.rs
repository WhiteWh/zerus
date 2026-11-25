//! U62, #718 (owner, 2026-10-30): what Zerus makes of the test queue's
//! results. An unchanged suite reuses its last green result, marked reused and
//! due for re-measuring, until enough reuse proposes a full run; a suite that
//! passes only on its automatic retry is flaky, quarantined for a while and,
//! after a few episodes, a lead item to file a task; a crash is never retried;
//! a suite that grows past its recorded time or its budgets is flagged; a
//! report carries the test lines and functions it adds and removes. Every
//! threshold is the project's `.zerus/tests.toml`.

use super::model::{FullRunProposal, Team, TestAlert, TestQuarantine};
use super::test_queue::{Record, TestStore};
use crate::test_discipline::config::{Provider, ReuseBy, TestsConfig};
use crate::test_discipline::map::{ProviderMap, ALL};
use crate::test_discipline::{glob, map};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

/// The project's thresholds the runner and the team apply.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Policy {
    /// Automatic retries of a failed (not crashed) suite.
    pub retries: u32,
    pub grow_percent: u32,
    pub episodes: u32,
    pub quarantine_days: u32,
    pub full_after: u32,
    pub max_age_days: f64,
}

impl Policy {
    pub(crate) fn of(config: &TestsConfig) -> Self {
        Self {
            retries: config.retries.flaky,
            grow_percent: config.budgets.grow_percent,
            episodes: config.retries.episodes,
            quarantine_days: config.retries.quarantine_days,
            full_after: config.reuse.full_after,
            max_age_days: config.reuse.max_age_days,
        }
    }
}

/// A run ended abnormally: a crash exit code, or a sanitizer, heap or
/// segmentation report in its output. It is never retried to green.
pub(crate) fn crashed(exit: Option<i32>, output: &str) -> bool {
    const SIGNS: [&str; 8] = [
        "AddressSanitizer",
        "LeakSanitizer",
        "UndefinedBehaviorSanitizer",
        "heap corruption",
        "HEAP CORRUPTION",
        "Segmentation fault",
        "core dumped",
        "STATUS_HEAP_CORRUPTION",
    ];
    super::isolation::crashed(exit) || SIGNS.iter().any(|sign| output.contains(sign))
}

const CODE: [&str; 9] = ["c", "cc", "cpp", "cxx", "h", "hpp", "mm", "m", "rs"];

/// The hash a suite's unchanged result is reused by, or None when it is never
/// reused (cargo by default) or its inputs cannot be read.
pub(crate) fn input_hash(project: &Path, provider: &Provider, section: &ProviderMap, targets: &[String]) -> Option<String> {
    // Source/binary hashes on the builder cannot establish a verdict on a
    // selected device. Remote receipts retain their complete artifact identity.
    if super::artifacts::profile_of(&provider.command).is_some() { return None; }
    use sha2::{Digest, Sha256};
    let reuse = provider.reuse_by();
    if reuse == ReuseBy::Never {
        return None;
    }
    let chosen: Vec<&String> = if targets.is_empty() { section.targets.keys().collect() } else { targets.iter().collect() };
    let mapped = |id: &str| -> Vec<&String> {
        section
            .files
            .iter()
            .chain(section.test_files.iter())
            .filter(|(_, ids)| ids.iter().any(|known| known == id || known == ALL))
            .map(|(file, _)| file)
            .collect()
    };
    let mut hash = Sha256::new();
    hash.update(serde_json::to_vec(provider).ok()?);
    let mut inputs: BTreeSet<String> = BTreeSet::new();
    for id in &chosen {
        let target = section.targets.get(id.as_str())?;
        hash.update(id.as_bytes());
        hash.update(serde_json::to_vec(target).ok()?);
        if reuse == ReuseBy::Binary {
            // The test binary itself, and the data it reads: its mapped files
            // that are no compiled source.
            let binary = project.join(&target.binary);
            let bytes = std::fs::read(&binary).ok().filter(|_| !target.binary.is_empty())?;
            hash.update(Sha256::digest(&bytes));
            inputs.extend(
                mapped(id)
                    .into_iter()
                    .filter(|file| !Path::new(file.as_str()).extension().is_some_and(|ext| CODE.contains(&ext.to_string_lossy().as_ref())))
                    .cloned(),
            );
        } else {
            inputs.extend(mapped(id).into_iter().cloned());
        }
    }
    for (file, blob) in blob_hashes(project, &inputs.into_iter().collect::<Vec<_>>())? {
        hash.update(file.as_bytes());
        hash.update([0]);
        hash.update(blob.as_bytes());
    }
    Some(format!("{:x}", hash.finalize()))
}

/// Each file's content hash as it is now on disk; a missing file hashes as
/// "missing", so its deletion is a change too.
fn blob_hashes(project: &Path, files: &[String]) -> Option<Vec<(String, String)>> {
    use sha2::{Digest, Sha256};
    let mut out: Vec<(String, String)> = files
        .iter()
        .map(|file| {
            let hash = std::fs::read(project.join(file)).map(|bytes| format!("{:x}", Sha256::digest(&bytes))).unwrap_or_else(|_| "missing".to_owned());
            (file.clone(), hash)
        })
        .collect();
    out.sort();
    Some(out)
}
/// A suite's test lines: every test file the map gives its provider.
pub(crate) fn test_lines(project: &Path, section: &ProviderMap) -> usize {
    section.test_files.keys().filter_map(|file| std::fs::read_to_string(project.join(file)).ok()).map(|text| text.lines().count()).sum()
}

/// A provider's size budget in test lines; 0 is none.
pub(crate) fn lines_budget(config: &TestsConfig, provider: &Provider) -> u32 {
    if provider.budget_lines > 0 {
        provider.budget_lines
    } else {
        config.budgets.suite_lines
    }
}

/// Applies a tick's or a request's results to the team: their notices, a flaky
/// suite's quarantine and, after enough episodes, one lead item; a time budget
/// gone over; and the proposal of a full run once enough results were reused.
/// `estimate` is the full run's recorded seconds and the targets without one.
pub(crate) fn apply(team: &mut Team, records: &[Record], database: &TestStore, policy: &Policy, now: f64, estimate: (f64, usize)) -> anyhow::Result<()> {
    super::test_queue::record_notices(team, records, now);
    team.test_quarantine.retain(|quarantine| quarantine.until > now);
    for record in records {
        if record.status == "flaky" {
            let until = now + f64::from(policy.quarantine_days) * 86_400.0;
            let episodes = database.flaky_history(&team.id, &record.suite, 0.0)?.len();
            match team.test_quarantine.iter_mut().find(|quarantine| quarantine.suite == record.suite) {
                Some(quarantine) => {
                    quarantine.until = until;
                    quarantine.episodes = episodes;
                }
                None if policy.quarantine_days > 0 => team.test_quarantine.push(TestQuarantine { suite: record.suite.clone(), since: now, until, episodes }),
                None => {}
            }
            // Episodes since the suite's last item: every `episodes` of them is one.
            let since = team.test_alerts.iter().filter(|alert| alert.kind == "flaky" && alert.suite == record.suite).map(|alert| alert.at).fold(0.0, f64::max);
            let since = if since > 0.0 { since + 1e-6 } else { 0.0 };
            let history = database.flaky_history(&team.id, &record.suite, since)?;
            if policy.episodes > 0 && history.len() >= policy.episodes as usize {
                let records: Vec<String> = history.iter().map(|(record, _)| record.clone()).collect();
                raise(team, "flaky", &record.suite, now, format!(
                    "{} passed only on its automatic retry {} times: {}. File a task to fix it; it stays quarantined until {}.",
                    record.suite,
                    history.len(),
                    records.join(", "),
                    day(until)
                ), history.len());
            }
        }
        if record.flags.iter().any(|flag| flag == "over_time_budget") {
            raise(team, "time_budget", &record.suite, now, format!(
                "{} ran {} s, over its time budget (record {}): split it or raise its budget in .zerus/tests.toml.",
                record.suite,
                record.duration_ms / 1000,
                record.id
            ), 0);
        }
    }
    // The proposal of a full run: enough reuse since the last full run, or a
    // reused measurement grown too old.
    if team.test_full_proposal.is_none() {
        let (count, oldest) = database.reused_since(&team.id, team.test_full_at)?;
        let by_count = policy.full_after > 0 && count >= policy.full_after as usize;
        let by_age = policy.max_age_days > 0.0 && count > 0 && oldest.is_some_and(|oldest| now - oldest >= policy.max_age_days * 86_400.0);
        if by_count || by_age {
            team.test_full_proposal = Some(FullRunProposal {
                at: now,
                reused: count,
                oldest_at: oldest.unwrap_or(now),
                seconds: estimate.0,
                unknown: estimate.1,
                reason: if by_count { "count".into() } else { "age".into() },
            });
        }
    }
    Ok(())
}

/// One item per kind and suite: a new one replaces the old (open or dismissed).
pub(crate) fn raise(team: &mut Team, kind: &str, suite: &str, now: f64, detail: String, episodes: usize) {
    team.test_alerts.retain(|alert| !(alert.kind == kind && alert.suite == suite));
    team.test_alerts.push(TestAlert { kind: kind.to_owned(), suite: suite.to_owned(), at: now, detail, episodes, dismissed: false });
    let extra = team.test_alerts.len().saturating_sub(50);
    team.test_alerts.drain(0..extra);
}

fn day(at: f64) -> String {
    chrono::DateTime::from_timestamp(at as i64, 0).map(|time| time.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

/// The lead's (or owner's) answer to a test item: `dismiss` one alert, or
/// `full-done` (a full run happened: the reuse count starts again).
pub(crate) fn settle(team: &mut Team, action: &str, input: &Value, now: f64) -> anyhow::Result<Value> {
    match action {
        "dismiss" => {
            let kind = input["kind"].as_str().unwrap_or_default();
            let suite = input["suite"].as_str().unwrap_or_default();
            // Kept, so a flaky suite's next item counts only newer episodes.
            let alert = team
                .test_alerts
                .iter_mut()
                .find(|alert| alert.kind == kind && alert.suite == suite && !alert.dismissed)
                .ok_or_else(|| anyhow::anyhow!("No open {kind} item for {suite}"))?;
            alert.dismissed = true;
            // A flaky suite's next item counts the episodes after this answer.
            alert.at = now;
            Ok(json!({"status":"dismissed","kind":kind,"suite":suite}))
        }
        "full-done" => {
            team.test_full_at = now;
            team.test_full_proposal = None;
            Ok(json!({"status":"full_run_recorded"}))
        }
        other => anyhow::bail!("Unknown test action {other}"),
    }
}

/// Task #401: more changed files than this, and a report gets the counts only.
pub(crate) const DELTA_FILES: usize = 200;
/// Task #401: a delta that already took this long reads no old contents.
/// Task #44 (team ede724c4): under tests at least a minute, which a machine
/// busy with parallel builds fits: the first git diff alone used the 5 s up
/// and the report came back in its bounded form.
#[cfg(not(test))]
const DELTA_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(test)]
const DELTA_BUDGET: std::time::Duration = std::time::Duration::from_secs(60);

/// #718: the test lines and test functions a report's change adds and
/// removes, from its folder's diff against `base`. Test files are the
/// project's test globs and the Rust files that hold tests.
///
/// Task #401 (2026-11-09): a report used to read every changed file and run
/// one `git show` per file before it knew whether the file was a test, against
/// a day-old lead commit: some 1300 files, 10-20 minutes of the worker. Now
/// `base` is the task branch's own start (`report_git`), the paths are filtered
/// first (test globs, then a Rust file's own markers), the old contents come
/// from one `git cat-file --batch`, and a change of more than `DELTA_FILES`
/// files, or one whose diff alone took `DELTA_BUDGET`, gets its counts only.
pub(crate) fn report_delta(folder: &str, base: &str, config: &TestsConfig) -> Option<Value> {
    let started = std::time::Instant::now();
    let root = Path::new(folder);
    let range = format!("{base}...HEAD");
    let numstat = super::dispatcher::git_at(folder, &["diff", "--numstat", "--no-renames", &range])?;
    let changed: Vec<(u64, u64, &str)> = numstat
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let (plus, minus, file) = (parts.next()?, parts.next()?, parts.next()?);
            Some((plus.parse().unwrap_or(0), minus.parse().unwrap_or(0), file))
        })
        .collect();
    let bounded = changed.len() > DELTA_FILES || started.elapsed() > DELTA_BUDGET;
    // Paths first: a test glob, or a Rust file whose new text holds a test. A
    // Rust file without one may have lost its tests: its old text decides.
    let mut tests: Vec<(usize, String)> = Vec::new();
    let mut maybe: Vec<(usize, String)> = Vec::new();
    for (index, (_, _, file)) in changed.iter().enumerate() {
        if config.providers.iter().any(|provider| glob::any(&provider.tests, file)) {
            tests.push((index, std::fs::read_to_string(root.join(file)).unwrap_or_default()));
        } else if file.ends_with(".rs") {
            let new = std::fs::read_to_string(root.join(file)).unwrap_or_default();
            if has_tests(&new) {
                tests.push((index, new));
            } else if !bounded {
                maybe.push((index, new));
            }
        }
    }
    let (mut added, mut removed) = (0u64, 0u64);
    let mut functions_added = Vec::new();
    let mut functions_removed = Vec::new();
    let mut files = 0;
    if bounded {
        for (index, _) in &tests {
            files += 1;
            added += changed[*index].0;
            removed += changed[*index].1;
        }
        if files == 0 {
            return Some(json!({"files":0,"lines_added":0,"lines_removed":0,"functions_added":[],"functions_removed":[],
                "bounded":true,"changed_files":changed.len()}));
        }
        return Some(json!({"files":files,"lines_added":added,"lines_removed":removed,
            "functions_added":{"total":0,"names":[]},"functions_removed":{"total":0,"names":[]},
            "bounded":true,"changed_files":changed.len()}));
    }
    let specs: Vec<String> = tests.iter().chain(maybe.iter()).map(|(index, _)| format!("{base}:{}", changed[*index].2)).collect();
    let olds = super::dispatcher::git_cat_files(folder, &specs).unwrap_or_else(|| vec![None; specs.len()]);
    for ((index, new), old) in tests.iter().chain(maybe.iter()).zip(olds) {
        let old = old.unwrap_or_default();
        let file = changed[*index].2;
        if !is_test_file(config, file, &old, new) {
            continue;
        }
        files += 1;
        added += changed[*index].0;
        removed += changed[*index].1;
        let (before, after) = (test_functions(file, &old), test_functions(file, new));
        functions_added.extend(after.difference(&before).map(|name| format!("{file}: {name}")));
        functions_removed.extend(before.difference(&after).map(|name| format!("{file}: {name}")));
    }
    if files == 0 {
        return Some(json!({"files":0,"lines_added":0,"lines_removed":0,"functions_added":[],"functions_removed":[]}));
    }
    let bounded = |list: Vec<String>| -> Value {
        let total = list.len();
        json!({"total":total,"names":list.into_iter().take(40).collect::<Vec<_>>()})
    };
    Some(json!({"files":files,"lines_added":added,"lines_removed":removed,
        "functions_added":bounded(functions_added),"functions_removed":bounded(functions_removed)}))
}

fn has_tests(text: &str) -> bool {
    text.contains("#[test]") || text.contains("#[tokio::test]")
}

fn is_test_file(config: &TestsConfig, file: &str, old: &str, new: &str) -> bool {
    if config.providers.iter().any(|provider| glob::any(&provider.tests, file)) {
        return true;
    }
    file.ends_with(".rs") && (has_tests(old) || has_tests(new))
}

/// Test function names a file declares, by its language.
fn test_functions(file: &str, text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let ext = Path::new(file).extension().map(|ext| ext.to_string_lossy().into_owned()).unwrap_or_default();
    match ext.as_str() {
        "rs" => {
            let mut pending = false;
            for line in text.lines().map(str::trim) {
                if line.starts_with("#[test]") || line.starts_with("#[tokio::test") {
                    pending = true;
                } else if pending {
                    if let Some(rest) = line.split("fn ").nth(1) {
                        names.insert(rest.split('(').next().unwrap_or_default().trim().to_owned());
                        pending = false;
                    } else if !line.starts_with("#[") {
                        pending = false;
                    }
                }
            }
        }
        "cpp" | "cc" | "cxx" | "mm" => names.extend(crate::test_discipline::cmake::qt_functions(text)),
        "py" => names.extend(text.lines().filter_map(|line| line.trim_start().strip_prefix("def test")).map(|rest| format!("test{}", rest.split('(').next().unwrap_or_default()))),
        "kt" | "java" => {
            let mut pending = false;
            for line in text.lines().map(str::trim) {
                if line.starts_with("@Test") {
                    pending = true;
                } else if pending && (line.contains("fun ") || line.contains("void ")) {
                    let rest = line.split("fun ").nth(1).or_else(|| line.split("void ").nth(1)).unwrap_or_default();
                    names.insert(rest.split('(').next().unwrap_or_default().trim().trim_matches('`').to_owned());
                    pending = false;
                }
            }
        }
        "ts" | "tsx" | "js" | "jsx" => {
            for line in text.lines().map(str::trim) {
                for opener in ["it(", "test("] {
                    if let Some(rest) = line.strip_prefix(opener) {
                        let quote = rest.chars().next().unwrap_or('"');
                        if let Some(name) = rest[quote.len_utf8()..].split(quote).next() {
                            names.insert(name.to_owned());
                        }
                    }
                }
            }
        }
        _ => {}
    }
    names.retain(|name| !name.is_empty());
    names
}

/// The full run's estimate from the map and the recorded times.
pub(crate) fn full_estimate(project: &Path, config: &TestsConfig) -> (f64, usize) {
    let Ok(text) = std::fs::read_to_string(project.join(&config.map.file)) else { return (0.0, 0) };
    let Ok(test_map) = serde_json::from_str::<map::TestMap>(&text) else { return (0.0, 0) };
    let durations = crate::test_discipline::durations::Durations::read(&project.join(&config.map.durations)).unwrap_or_default();
    config
        .providers
        .iter()
        .filter(|provider| !provider.disabled)
        .map(|provider| crate::test_discipline::durations::provider_seconds(&durations, &test_map, &provider.name))
        .fold((0.0, 0), |(seconds, unknown), (more, missing)| (seconds + more, unknown + missing))
}

/// A suite's map section as the sources give it now (the committed snapshot
/// for a build this checkout cannot read).
pub(crate) fn section(project: &Path, config: &TestsConfig, suite: &str) -> Option<ProviderMap> {
    let committed = std::fs::read_to_string(project.join(&config.map.file)).ok().and_then(|text| serde_json::from_str::<map::TestMap>(&text).ok());
    let files = map::project_files(project).ok()?;
    let only = BTreeSet::from([suite.to_owned()]);
    let fresh = map::generate_only(project, config, &files, committed.as_ref(), Some(&only)).ok()?;
    fresh.providers.get(suite).cloned()
}

/// Kinds a result's lead card names with the record.
pub(crate) fn flag_text(flags: &[String], seconds_recorded: f64, duration_ms: u64) -> String {
    let mut parts = Vec::new();
    for flag in flags {
        match flag.as_str() {
            "reused" => parts.push("reused, due for re-measuring".to_owned()),
            "slower" => parts.push(format!("slower than recorded: {} s, recorded {} s", duration_ms / 1000, seconds_recorded.round() as u64)),
            "over_time_budget" => parts.push("over its time budget".to_owned()),
            "retried" => parts.push("passed only on its automatic retry (flaky)".to_owned()),
            "crash" => parts.push("crashed: never retried".to_owned()),
            "quarantined" => parts.push("quarantined as flaky".to_owned()),
            // Task #18: one environment failure, not the tests'; the record's
            // excerpt names the owner's step.
            "environment" => parts.push("an environment failure, not the tests': the record names the owner's step".to_owned()),
            other => parts.push(other.to_owned()),
        }
    }
    parts.join("; ")
}

#[cfg(test)]
#[path = "test_results_tests.rs"]
mod tests;
