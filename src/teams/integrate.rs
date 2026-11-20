//! Integration branches are advanced with Git plumbing, without a checkout.
#![allow(dead_code)] // The teams actor consumes this API in a later package.

use std::collections::BTreeSet;
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};
use zerus_team_plan::{problem::code, Problem};

#[derive(Clone, Debug)]
pub struct Repo {
    /// The existing repository's common Git directory, shared by its worktrees.
    pub git_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Merge {
    Merged {
        commit: String,
    },
    Conflict {
        files: Vec<String>,
    },
    /// Another writer changed or deleted the ref; compute the merge again.
    Stale,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reported {
    pub commit: String,
    pub branch: String,
    pub dirty: bool,
}

mod base;
mod member_refs;
pub(crate) use base::{main_refs, validate_base_advance};
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
pub use member_refs::member_branch_available;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use member_refs::MEMBER_REF_CONFLICT;

/// Read a validated local ref through the same bounded, hook-free Git adapter.
pub fn branch_exists(repo: &Repo, branch: &str) -> Result<bool, Problem> {
    let git = Git::repo(repo)?;
    let reference = git.branch_ref(branch)?;
    let output = git.run(&["show-ref", "--verify", "--quiet", &reference])?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(git_failure(&output)),
    }
}
/// Create an absent local branch. Never replace an existing branch.
pub fn create_branch(repo: &Repo, branch: &str, base: &str) -> Result<(), Problem> {
    let git = Git::repo(repo)?;
    let reference = git.branch_ref(branch)?;
    git.available(&reference)?;
    let base = git.commit(base)?;
    let absent = "0".repeat(base.len());
    let message = format!("Zerus: create integration branch {branch}");
    let output = git.run(&[
        "update-ref",
        "--no-deref",
        "-m",
        &message,
        &reference,
        &base,
        &absent,
    ])?;
    success(output).map(|_| ())
}

/// Read the member's immutable commit identity, branch (empty if detached),
/// and dirty state. Optional index writes and external filters are disabled.
pub fn reported_commit(worktree: &Path) -> Result<Reported, Problem> {
    let git = Git::open(worktree, false)?;
    let commit = git.commit("HEAD")?;
    let branch = git.head_branch()?;
    // A nested status has its own repository configuration. Until it has a
    // separate guarded reader, do not let it execute nested filters or hooks.
    // Modes alone keep this bounded even for very large indexes and long paths.
    let index = git.read(&["ls-files", "--format=%(objectmode)", "-z"])?;
    if index.split(|byte| *byte == 0).any(|row| row == b"160000") {
        return Err(unavailable(
            "Worktree reports with submodules are unavailable",
        ));
    }
    let status = git.read(&[
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=normal",
        "--ignore-submodules=all",
        "--no-renames",
    ])?;
    // Disabling a clean filter can make a clean transformed file look dirty.
    // Refuse that case rather than execute the filter or invent its result.
    if !git.filters.is_empty() && !status.is_empty() {
        let mut paths = Vec::new();
        let mut bytes = 0;
        for row in status.split(|byte| *byte == 0).filter(|row| row.len() >= 3) {
            let path = std::str::from_utf8(&row[3..])
                .map_err(|_| unavailable("A changed path is not UTF-8"))?;
            let size = path.len() + 1;
            // Keep both Unix argv and Windows command lines well below their
            // limits, with room for Git's fixed arguments and quoting.
            if size > 8192 {
                return Err(unavailable(
                    "A changed path exceeds the attribute query bound",
                ));
            }
            if bytes + size > 8192 || paths.len() == 128 {
                git.check_filters(&paths)?;
                paths.clear();
                bytes = 0;
            }
            paths.push(path);
            bytes += size;
        }
        if !paths.is_empty() {
            git.check_filters(&paths)?;
        }
    }
    if commit != git.commit("HEAD")? || branch != git.head_branch()? {
        return Err(unavailable(
            "The worktree HEAD changed while reading the report",
        ));
    }
    Ok(Reported {
        commit,
        branch,
        dirty: !status.is_empty(),
    })
}

/// Check the task's stated base before accepting its reported commit.
pub fn descends(repo: &Repo, commit: &str, base: &str) -> Result<bool, Problem> {
    let git = Git::repo(repo)?;
    let commit = git.commit(commit)?;
    let base = git.commit(base)?;
    let output = git.run(&["merge-base", "--is-ancestor", &base, &commit])?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(git_failure(&output)),
    }
}

/// The pinned integration tip proven to contain this report, or a known
/// negative ancestry result. Neither an unreadable ref nor a failed read is
/// a negative result. One Git deadline covers the pin and its ancestry check.
pub fn containing_tip(repo: &Repo, branch: &str, commit: &str) -> Result<Option<String>, Problem> {
    let git = Git::repo(repo)?;
    let reference = git.branch_ref(branch)?;
    let tip = git.commit(&reference)?;
    let incoming = git.commit(commit)?;
    let output = git.run(&["merge-base", "--is-ancestor", &incoming, &tip])?;
    match output.status.code() {
        Some(0) => Ok(Some(tip)),
        Some(1) => Ok(None),
        _ => Err(git_failure(&output)),
    }
}

/// Merge a pinned report into an unchecked-out integration branch. The actor
/// first checks [`descends`] against the base assigned to that task. A lost
/// ref-write result returns `delivery_uncertain`, never permission to retry.
pub fn merge(repo: &Repo, branch: &str, commit: &str, message: &str) -> Result<Merge, Problem> {
    merge_with_hook(repo, branch, commit, message, || {})
}

/// Team-owned admission at the ref writer. Existing merge callers keep their
/// behavior; this guard lives only across update-ref, never tree computation.
pub fn merge_admitted<T>(
    repo: &Repo,
    branch: &str,
    commit: &str,
    message: &str,
    admit: impl FnOnce() -> Result<T, Problem>,
) -> Result<Merge, Problem> {
    let mut git = Git::repo(repo)?;
    git.ref_budget = Some(Duration::from_secs(5));
    merge_git(
        &git,
        branch,
        commit,
        message,
        || {
            #[cfg(test)]
            if let Some(hook) = BEFORE_ADMISSION.with(|s| s.borrow_mut().take()) {
                hook();
            }
        },
        admit,
    )
}
#[cfg(test)]
thread_local! {
    pub(crate) static BEFORE_ADMISSION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const {std::cell::RefCell::new(None)};
    pub(crate) static BEFORE_REF_TURN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const {std::cell::RefCell::new(None)};
    pub(crate) static BEFORE_REF_WRITE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const {std::cell::RefCell::new(None)};
}

/// A new empty directory that only this user can enter. A person's merge
/// driver runs there, and what it writes must not be readable by other users
/// of a shared computer.
fn private_merge_dir() -> std::io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("zerus-merge-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir()
}

fn merge_with_hook(
    repo: &Repo,
    branch: &str,
    commit: &str,
    message: &str,
    before_update: impl FnOnce(),
) -> Result<Merge, Problem> {
    merge_git(
        &Git::repo(repo)?,
        branch,
        commit,
        message,
        before_update,
        || Ok(()),
    )
}

fn merge_git<T>(
    git: &Git,
    branch: &str,
    commit: &str,
    message: &str,
    before_update: impl FnOnce(),
    admit: impl FnOnce() -> Result<T, Problem>,
) -> Result<Merge, Problem> {
    if git.repository_drivers {
        return Err(unavailable(
            "The repository declares an external merge driver",
        ));
    }
    let reference = git.branch_ref(branch)?;
    git.available(&reference)?;
    let old = git.commit(&reference)?;
    let incoming = git.commit(commit)?;
    let ancestry = git.run(&["merge-base", "--is-ancestor", &incoming, &old])?;
    match ancestry.status.code() {
        Some(0) => {
            before_update();
            git.available(&reference)?;
            return Ok(if git.reference(&reference)? == Some(old.clone()) {
                Merge::Merged { commit: old }
            } else {
                Merge::Stale
            });
        }
        Some(1) => {}
        _ => return Err(git_failure(&ancestry)),
    }
    // Drivers and Git's .merge_file_* files stay in this private directory.
    // Never use a person's checkout, including on timeout or driver failure.
    // Git before attr.tree support sees an empty attribute context instead.
    let merge_worktree = private_merge_dir()
        .map_err(|error| unavailable(format!("Cannot create private merge directory: {error}")))?;
    let path = merge_worktree
        .path()
        .to_str()
        .ok_or_else(|| unavailable("The private merge path is not UTF-8"))?;
    let attributes = format!("attr.tree={old}");
    let output = git.run(&[
        "--work-tree",
        path,
        "-C",
        path,
        "-c",
        &attributes,
        "merge-tree",
        "--write-tree",
        "--name-only",
        "-z",
        &old,
        &incoming,
    ])?;
    let mut fields = output.stdout.split(|byte| *byte == 0);
    let tree = fields.next().unwrap_or_default();
    if !valid_oid(tree) {
        return Err(git_failure(&output));
    }
    match output.status.code() {
        Some(1) => {
            let files = fields
                .take_while(|field| !field.is_empty())
                .map(|field| {
                    String::from_utf8(field.to_vec())
                        .map_err(|_| unavailable("A conflicting path is not UTF-8"))
                })
                .collect::<Result<BTreeSet<_>, _>>()?;
            if files.is_empty() {
                return Err(git_failure(&output));
            }
            return Ok(Merge::Conflict {
                files: files.into_iter().collect(),
            });
        }
        Some(0) => {}
        _ => return Err(git_failure(&output)),
    }
    let tree = std::str::from_utf8(tree).map_err(|_| unavailable("Invalid merge tree"))?;
    let signing = git.run(&["config", "--bool", "--get", "commit.gpgsign"])?;
    let sign = match signing.status.code() {
        Some(0) => String::from_utf8_lossy(&signing.stdout).trim() == "true",
        Some(1) => false,
        _ => return Err(git_failure(&signing)),
    };
    let mut args = vec![
        "commit-tree",
        tree,
        "-p",
        &old,
        "-p",
        &incoming,
        "-m",
        message,
    ];
    if sign {
        args.push("-S");
    }
    let new = oid(&git.read(&args)?)?;
    // The hook is a private injection point for the CAS race test. It is never
    // a Git hook or repository code, and production passes an empty closure.
    before_update();
    git.available(&reference)?;
    let reflog = format!("Zerus: merge {incoming}: {message}");
    // Reserve the process turn before taking the store's write fence.
    // A failed turn wait is a proven non-start with its own retry outcome.
    #[cfg(test)]
    if let Some(hook) = BEFORE_REF_TURN.with(|s| s.borrow_mut().take()) {
        hook();
    }
    #[cfg(unix)]
    let turn = crate::platform::spawn_turn_before(
        git.deadline
            .min(Instant::now() + git.ref_budget.unwrap_or(Duration::from_secs(30))),
        &|| false,
    )
    .map_err(|error| unavailable(format!("Git ref writer did not start: {error:?}")))?;
    let admission = admit()?;
    let ref_deadline = git
        .ref_budget
        .map(|budget| git.deadline.min(Instant::now() + budget))
        .unwrap_or(git.deadline);
    #[cfg(test)]
    if let Some(hook) = BEFORE_REF_WRITE.with(|s| s.borrow_mut().take()) {
        hook();
    }
    let output = git.run_prepared(
        &[
            "update-ref",
            "--no-deref",
            "-m",
            &reflog,
            &reference,
            &new,
            &old,
        ],
        ref_deadline,
        #[cfg(unix)]
        Some(turn),
    );
    // The admission and every lock it holds (the store's writer fence, and
    // for a team merge the accepted task's project lock) end only after
    // update-ref has returned, within the ref deadline.
    drop(admission);
    let output = output?;
    if output.status.success() {
        return Ok(Merge::Merged { commit: new });
    }
    match git.reference(&reference)? {
        Some(current) if current == new => Ok(Merge::Merged { commit: new }),
        Some(current) if current == old => Err(git_failure(&output)),
        _ => Ok(Merge::Stale),
    }
}

/// Bring in an explicitly selected local base branch using the same plumbing.
pub fn sync_base(repo: &Repo, branch: &str, base_branch: &str) -> Result<Merge, Problem> {
    let git = Git::repo(repo)?;
    let base = git.branch_ref(base_branch)?;
    let commit = git.commit(&base)?;
    merge_git(
        &git,
        branch,
        &commit,
        &format!("Sync base branch {base_branch}"),
        || {},
        || Ok(()),
    )
}

/// The outer deadline of one Git: every command of a report, a merge or a
/// sync shares it. Tests open Git with the same value unless a fixture's
/// budget lengthens it for one deliberately huge case.
const GIT_BUDGET: Duration = Duration::from_secs(30);

fn unavailable(message: impl Into<String>) -> Problem {
    Problem::new(code::INTEGRATION_UNAVAILABLE, message)
}

/// Git's own words, which can quote a branch, a path or a hook's output:
/// cleaned to one bounded line before they reach any card (zerus-63rru.59).
fn git_failure(output: &Output) -> Problem {
    unavailable(format!(
        "Git refused integration: {}",
        super::clean::sentence(&String::from_utf8_lossy(&output.stderr), 1024)
    ))
}

fn success(output: Output) -> Result<Vec<u8>, Problem> {
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(git_failure(&output))
    }
}

fn valid_oid(bytes: &[u8]) -> bool {
    matches!(bytes.len(), 40 | 64) && bytes.iter().all(u8::is_ascii_hexdigit)
}

fn oid(bytes: &[u8]) -> Result<String, Problem> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| unavailable("Git returned an invalid object ID"))?
        .trim();
    if !valid_oid(text.as_bytes()) {
        return Err(unavailable("Git returned an invalid object ID"));
    }
    Ok(text.to_owned())
}

fn supported_version(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let Some(version) = text.trim().strip_prefix("git version ") else {
        return false;
    };
    let mut parts = version.split('.');
    matches!((parts.next().and_then(|n| n.parse::<u32>().ok()), parts.next().and_then(|n| n.parse::<u32>().ok())),
        (Some(major), Some(minor)) if major > 2 || (major == 2 && minor >= 38))
}

struct Git {
    program: PathBuf,
    path: PathBuf,
    common_dir: bool,
    hooks: tempfile::TempDir,
    overrides: Vec<String>,
    filters: BTreeSet<String>,
    repository_drivers: bool,
    deadline: Instant,
    ref_budget: Option<Duration>,
}

impl Git {
    fn repo(repo: &Repo) -> Result<Self, Problem> {
        Self::open(&repo.git_dir, true)
    }

    fn open(path: &Path, common_dir: bool) -> Result<Self, Problem> {
        let mut git = Self {
            program: {
                #[cfg(test)]
                {
                    tests::program()
                }
                #[cfg(not(test))]
                {
                    PathBuf::from("git")
                }
            },
            // Not `canonicalize`: on Windows it returns a verbatim `\\?\` path,
            // where `/` is an ordinary character, and Git appends `/HEAD` and
            // `/objects` to `--git-dir`, so it finds no repository there
            // (zerus-63rru.111). The plain form names the same directory.
            path: crate::platform::canonical(path)
                .map_err(|e| unavailable(format!("Cannot open repository: {e}")))?,
            common_dir,
            hooks: tempfile::tempdir()
                .map_err(|e| unavailable(format!("Cannot disable Git hooks: {e}")))?,
            overrides: Vec::new(),
            filters: BTreeSet::new(),
            repository_drivers: false,
            // One deadline covers every command of this Git. Only a test
            // fixture can lengthen it, for a deliberately huge case.
            deadline: Instant::now() + {
                #[cfg(test)]
                {
                    tests::budget()
                }
                #[cfg(not(test))]
                {
                    GIT_BUDGET
                }
            },
            ref_budget: None,
        };
        if !supported_version(&git.read(&["--version"])?) {
            return Err(unavailable("Integration requires Git 2.38 or newer"));
        }
        // Do not run content filters or lazy fetches.
        // Filters are cleared even for status; a report that needs one refuses.
        let config = git.run(&["config", "--null", "--get-regexp",
            "^(filter\\..*\\.(clean|smudge|process)|extensions\\.partialclone|remote\\..*\\.promisor)$"])?;
        if !config.status.success() && config.status.code() != Some(1) {
            return Err(git_failure(&config));
        }
        for entry in config
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let entry = std::str::from_utf8(entry)
                .map_err(|_| unavailable("Invalid Git driver configuration"))?;
            let (key, value) = entry
                .split_once('\n')
                .ok_or_else(|| unavailable("Invalid Git driver configuration"))?;
            if key.contains('=') || key.contains('\r') || value.contains('\n') {
                return Err(unavailable(
                    "Git driver configuration cannot be safely overridden",
                ));
            }
            if let Some(filter) = key.strip_prefix("filter.") {
                let (name, _) = filter
                    .rsplit_once('.')
                    .ok_or_else(|| unavailable("Invalid Git filter configuration"))?;
                if !value.is_empty() {
                    git.filters.insert(name.to_owned());
                }
                git.overrides.push(format!("{key}="));
                git.overrides.push(format!("filter.{name}.required=false"));
            } else if (!value.is_empty() || key.ends_with(".promisor"))
                && !(key.ends_with(".promisor") && matches!(value, "false" | "no" | "off" | "0"))
            {
                return Err(unavailable("Integration cannot use partial clones"));
            }
        }
        // A person's configured signer is honoured. A repository cannot name
        // its own executable or key-discovery command for us to run.
        let extension = git.run(&[
            "config",
            "--local",
            "--bool",
            "--get",
            "extensions.worktreeConfig",
        ])?;
        let worktree_config = match extension.status.code() {
            Some(0) => String::from_utf8_lossy(&extension.stdout).trim() == "true",
            Some(1) => false,
            _ => return Err(git_failure(&extension)),
        };
        for scope in ["--local", "--worktree"] {
            if scope == "--worktree" && !worktree_config {
                continue;
            }
            for key in git.config_keys(Some(scope), "^(hook\\..*\\.(command|event)|merge\\..*\\.driver|gpg(\\..*)?\\.(program|defaultkeycommand))$")? {
                if key.starts_with("hook.") {
                    return Err(unavailable("The repository declares a config hook"));
                }
                if key.starts_with("gpg.") {
                    return Err(unavailable("The repository declares an external signing command"));
                }
                git.repository_drivers = true;
            }
        }
        // Config-defined hooks ignore core.hooksPath. Disable every personal
        // hook by name; repository/worktree definitions were refused above.
        // The ref-event switch is also fixed off from the very first command.
        for key in git.config_keys(None, "^hook\\..*\\.(command|event)$")? {
            let (name, _) = key
                .rsplit_once('.')
                .ok_or_else(|| unavailable("Invalid hook configuration"))?;
            git.overrides.push(format!("{name}.enabled=false"));
        }
        Ok(git)
    }

    fn config_keys(&self, scope: Option<&str>, pattern: &str) -> Result<Vec<String>, Problem> {
        let mut args = vec!["config", "--includes", "--null", "--name-only"];
        if let Some(scope) = scope {
            args.push(scope);
        }
        args.extend(["--get-regexp", pattern]);
        let output = self.run(&args)?;
        match output.status.code() {
            Some(1) => Ok(Vec::new()),
            Some(0) => output
                .stdout
                .split(|byte| *byte == 0)
                .filter(|key| !key.is_empty())
                .map(|key| {
                    let key = std::str::from_utf8(key)
                        .map_err(|_| unavailable("Invalid Git configuration key"))?;
                    if key.contains(['=', '\n', '\r']) {
                        return Err(unavailable("Git configuration cannot be safely overridden"));
                    }
                    Ok(key.to_owned())
                })
                .collect(),
            _ => Err(git_failure(&output)),
        }
    }

    fn run(&self, args: &[&str]) -> Result<Output, Problem> {
        self.run_until(args, self.deadline)
    }
    fn run_until(&self, args: &[&str], deadline: Instant) -> Result<Output, Problem> {
        self.run_prepared(
            args,
            deadline,
            #[cfg(unix)]
            None,
        )
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.program);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_")
                && !matches!(
                    key.to_str(),
                    Some("GIT_CONFIG_GLOBAL" | "GIT_CONFIG_SYSTEM" | "GIT_CONFIG_NOSYSTEM")
                )
            {
                command.env_remove(key);
            }
        }
        command
            .args([
                "--no-optional-locks",
                "--no-replace-objects",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "hook.reference-transaction.enabled=false",
                "-c",
                "gc.auto=0",
                "-c",
                "maintenance.auto=false",
                "-c",
                "merge.renormalize=false",
                "-c",
            ])
            .arg(format!("core.hooksPath={}", self.hooks.path().display()));
        for config in &self.overrides {
            command.arg("-c").arg(config);
        }
        if self.common_dir {
            command.arg("--git-dir").arg(&self.path);
        } else {
            command.arg("-C").arg(&self.path);
        }
        command
            .current_dir(&self.path)
            .args(args)
            .env("LC_ALL", "C")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("SSH_ASKPASS_REQUIRE", "never");
        #[cfg(test)]
        tests::configure(&mut command, args.first().copied());
        command
    }
    fn run_prepared(
        &self,
        args: &[&str],
        deadline: Instant,
        #[cfg(unix)] turn: Option<zerus_client_core::unix_spawn::Turn>,
    ) -> Result<Output, Problem> {
        if Instant::now() >= deadline {
            return Err(unavailable(
                "Git deadline passed before starting the command",
            ));
        }
        let mut command = self.command(args);
        #[cfg(all(test, unix))]
        let cancelled = tests::driver_clock::timed_out;
        #[cfg(not(all(test, unix)))]
        let cancelled = || false;
        #[cfg(unix)]
        let result = match turn {
            Some(turn) => crate::platform::run_bounded_in_turn(
                &mut command,
                Stdio::null(),
                [8 * 1024 * 1024, 64 * 1024],
                deadline,
                &cancelled,
                turn,
            ),
            None => crate::platform::run_bounded(
                &mut command,
                [8 * 1024 * 1024, 64 * 1024],
                deadline,
                &cancelled,
            ),
        };
        #[cfg(not(unix))]
        let result = crate::platform::run_bounded(
            &mut command,
            [8 * 1024 * 1024, 64 * 1024],
            deadline,
            &cancelled,
        );
        let (status, stdout, stderr) = result.map_err(|error| {
            // Spawn proves non-start. Stopped is opaque: on Unix it also
            // covers time spent waiting for the spawn turn. Do not infer that
            // a writer started, or permit retry, from that unknown outcome.
            let code = if args.first() == Some(&"update-ref")
                && !matches!(error, crate::platform::RunError::Spawn(_))
            {
                code::DELIVERY_UNCERTAIN
            } else {
                code::INTEGRATION_UNAVAILABLE
            };
            Problem::new(
                code,
                format!("Git did not return a settled result: {error:?}"),
            )
        })?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }

    fn read(&self, args: &[&str]) -> Result<Vec<u8>, Problem> {
        success(self.run(args)?)
    }

    fn check_filters(&self, paths: &[&str]) -> Result<(), Problem> {
        let mut args = vec!["check-attr", "-z", "filter", "--"];
        args.extend_from_slice(paths);
        let attrs = self.read(&args)?;
        let fields: Vec<_> = attrs.split(|byte| *byte == 0).collect();
        if fields.as_chunks::<3>().0.iter().any(|field| {
            self.filters
                .contains(&String::from_utf8_lossy(field[2]).into_owned())
        }) {
            Err(unavailable(
                "The worktree needs an external content filter to assess its changes",
            ))
        } else {
            Ok(())
        }
    }

    fn commit(&self, name: &str) -> Result<String, Problem> {
        oid(&self.read(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{name}^{{commit}}"),
        ])?)
    }

    fn branch_ref(&self, branch: &str) -> Result<String, Problem> {
        let name = branch.strip_prefix("refs/heads/").unwrap_or(branch);
        if name == "HEAD"
            || name.starts_with('-')
            || (branch.starts_with("refs/") && !branch.starts_with("refs/heads/"))
        {
            return Err(unavailable("Integration requires a local branch name"));
        }
        let reference = format!("refs/heads/{name}");
        self.read(&["check-ref-format", &reference])?;
        Ok(reference)
    }

    fn reference(&self, reference: &str) -> Result<Option<String>, Problem> {
        let output = self.run(&[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            reference,
        ])?;
        match output.status.code() {
            Some(0) => Ok(Some(oid(&output.stdout)?)),
            Some(1) => Ok(None),
            _ => Err(git_failure(&output)),
        }
    }

    fn head_branch(&self) -> Result<String, Problem> {
        let output = self.run(&["symbolic-ref", "-q", "HEAD"])?;
        match output.status.code() {
            Some(0) => {
                let text = String::from_utf8(output.stdout)
                    .map_err(|_| unavailable("The branch name is not UTF-8"))?;
                Ok(text
                    .trim_end_matches('\n')
                    .strip_prefix("refs/heads/")
                    .unwrap_or(text.trim_end_matches('\n'))
                    .to_owned())
            }
            Some(1) => Ok(String::new()),
            _ => Err(git_failure(&output)),
        }
    }

    fn available(&self, reference: &str) -> Result<(), Problem> {
        let symbolic = self.run(&["symbolic-ref", "-q", reference])?;
        if symbolic.status.code() != Some(1) {
            return Err(unavailable("The integration branch must be a direct ref"));
        }
        let trees = self.read(&["worktree", "list", "--porcelain", "-z"])?;
        let branch = format!("branch {reference}");
        if trees
            .split(|byte| *byte == 0)
            .any(|field| field == branch.as_bytes())
        {
            return Err(unavailable(
                "The integration branch is checked out in a worktree",
            ));
        }
        if protects_branch(&self.path, reference, self.deadline)? {
            return Err(unavailable(
                "The integration branch is being rebased or bisected",
            ));
        }
        let worktrees = match std::fs::read_dir(self.path.join("worktrees")) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(unavailable(format!(
                    "Cannot inspect worktree operation state: {error}"
                )))
            }
        };
        for (n, entry) in worktrees.enumerate() {
            if n >= 20_000 || Instant::now() >= self.deadline {
                return Err(unavailable(
                    "Worktree operation inspection exceeded its bound",
                ));
            }
            let entry = entry.map_err(|error| {
                unavailable(format!("Cannot inspect worktree operation state: {error}"))
            })?;
            if protects_branch(&entry.path(), reference, self.deadline)? {
                return Err(unavailable(
                    "The integration branch is being rebased or bisected",
                ));
            }
        }
        Ok(())
    }
}

fn protects_branch(admin: &Path, reference: &str, deadline: Instant) -> Result<bool, Problem> {
    for name in [
        "rebase-merge/head-name",
        "rebase-apply/head-name",
        "BISECT_START",
    ] {
        let file = match std::fs::File::open(admin.join(name)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(unavailable(format!(
                    "Cannot read worktree operation state: {error}"
                )))
            }
        };
        let mut value = Vec::new();
        file.take(4097).read_to_end(&mut value).map_err(|error| {
            unavailable(format!("Cannot read worktree operation state: {error}"))
        })?;
        if value.len() > 4096 {
            return Err(unavailable("Worktree operation state exceeded its bound"));
        }
        let value = std::str::from_utf8(&value)
            .map_err(|_| unavailable("Invalid worktree operation state"))?
            .trim_end_matches(['\r', '\n']);
        if value == reference || Some(value) == reference.strip_prefix("refs/heads/") {
            return Ok(true);
        }
    }
    let file = match std::fs::File::open(admin.join("rebase-merge/update-refs")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(unavailable(format!(
                "Cannot read rebase update-refs: {error}"
            )))
        }
    };
    // Each record is a ref name followed by the old and replacement OIDs.
    // Stream it: a rebase can protect many refs, not just its checked-out one.
    const MAX_UPDATE_REFS: usize = 1024 * 1024;
    let mut reader = std::io::BufReader::new(file.take(MAX_UPDATE_REFS as u64 + 1));
    let mut line = String::new();
    let mut bytes = 0;
    let mut n = 0usize;
    loop {
        if Instant::now() >= deadline {
            return Err(unavailable("Worktree operation inspection timed out"));
        }
        let read = reader
            .read_line(&mut line)
            .map_err(|error| unavailable(format!("Cannot read rebase update-refs: {error}")))?;
        if read == 0 {
            break;
        }
        bytes += read;
        if bytes > MAX_UPDATE_REFS || read > 4096 {
            return Err(unavailable("Rebase update-refs exceeded its bound"));
        }
        let value = line.trim_end_matches(['\r', '\n']);
        if n.is_multiple_of(3) {
            if value == reference {
                return Ok(true);
            }
            if !value.starts_with("refs/") {
                return Err(unavailable("Invalid ref in rebase update-refs"));
            }
        } else if !valid_oid(value.as_bytes()) {
            return Err(unavailable("Invalid OID in rebase update-refs"));
        }
        n += 1;
        line.clear();
    }
    if !n.is_multiple_of(3) {
        return Err(unavailable("Incomplete rebase update-refs record"));
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    mod r3;
    use super::*;
    use sha2::{Digest, Sha256};
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::rc::Rc;

    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn the_private_merge_directory_is_closed_to_other_users() {
        use std::os::unix::fs::PermissionsExt;
        let directory = private_merge_dir().unwrap();
        let mode = std::fs::metadata(directory.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[derive(Clone)]
    struct Config {
        global: PathBuf,
        system: PathBuf,
        attrs: Rc<Cell<u32>>,
    }

    thread_local! { static PROGRAM: RefCell<Option<PathBuf>> = const {RefCell::new(None)}; }
    thread_local! { static BUDGET: Cell<Option<Duration>> = const { Cell::new(None) }; }
    /// The outer deadline of a Git opened on this thread: the production
    /// [`GIT_BUDGET`] unless a fixture's [`Budget`] lengthens it.
    pub(super) fn budget() -> Duration {
        BUDGET.with(|budget| budget.get()).unwrap_or(GIT_BUDGET)
    }
    /// A longer outer Git deadline for this thread while the guard lives.
    struct Budget(Option<Duration>);
    impl Budget {
        fn set(budget: Duration) -> Self {
            Self(BUDGET.with(|slot| slot.replace(Some(budget))))
        }
    }
    impl Drop for Budget {
        fn drop(&mut self) {
            BUDGET.with(|slot| slot.set(self.0));
        }
    }
    pub(super) fn program() -> PathBuf {
        PROGRAM
            .with(|p| p.borrow().clone())
            .unwrap_or_else(|| PathBuf::from("git"))
    }
    thread_local! { static CONFIG: RefCell<Option<Config>> = const { RefCell::new(None) }; }

    #[cfg(unix)]
    pub(super) mod driver_clock;

    struct ConfigGuard(Option<Config>);

    impl Drop for ConfigGuard {
        fn drop(&mut self) {
            CONFIG.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }

    pub(super) fn configure(command: &mut Command, operation: Option<&str>) {
        if PROGRAM.with(|p| p.borrow().is_some()) {
            command.env(
                "HGS_FIXTURE_REAL_GIT",
                crate::platform::which("git").unwrap(),
            );
        }
        let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
        command
            .env_remove("GIT_CONFIG_NOSYSTEM")
            .env("GIT_CONFIG_GLOBAL", null)
            .env("GIT_CONFIG_SYSTEM", null);
        CONFIG.with(|slot| {
            if let Some(config) = &*slot.borrow() {
                command
                    .env("GIT_CONFIG_GLOBAL", &config.global)
                    .env("GIT_CONFIG_SYSTEM", &config.system);
                if operation == Some("check-attr") {
                    config.attrs.set(config.attrs.get() + 1);
                }
            }
        });
    }

    struct Fixture {
        root: tempfile::TempDir,
        main: PathBuf,
        repo: Repo,
        base: String,
        config: Config,
        _config_guard: ConfigGuard,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let config = Config {
                global: root.path().join("global-config"),
                system: root.path().join("system-config"),
                attrs: Rc::new(Cell::new(0)),
            };
            fs::write(&config.global, "").unwrap();
            fs::write(&config.system, "").unwrap();
            let guard = ConfigGuard(CONFIG.with(|slot| slot.replace(Some(config.clone()))));
            let main = root.path().join("main");
            fs::create_dir(&main).unwrap();
            git(&main, &["init", "-b", "main"]);
            git(&main, &["config", "user.name", "Developer"]);
            git(
                &main,
                &["config", "user.email", "developer@example.invalid"],
            );
            git(&main, &["config", "commit.gpgsign", "false"]);
            fs::write(main.join("shared.txt"), "base\n").unwrap();
            git(&main, &["add", "."]);
            git(&main, &["commit", "-m", "Base"]);
            let base = git(&main, &["rev-parse", "HEAD"]);
            let repo = Repo {
                git_dir: main.join(".git"),
            };
            Self {
                root,
                main,
                repo,
                base,
                config,
                _config_guard: guard,
            }
        }

        fn branch(&self, name: &str, file: &str, text: &str) -> String {
            git(&self.main, &["checkout", "-b", name, &self.base]);
            fs::write(self.main.join(file), text).unwrap();
            git(&self.main, &["add", "."]);
            git(&self.main, &["commit", "-m", name]);
            let commit = git(&self.main, &["rev-parse", "HEAD"]);
            git(&self.main, &["checkout", "main"]);
            commit
        }

        fn refs(&self) -> String {
            git(&self.main, &["show-ref"])
        }

        fn reachable(&self) -> String {
            git(&self.main, &["rev-list", "--all", "--count"])
        }

        fn objects(&self, commits_only: bool) -> BTreeSet<String> {
            git(
                &self.main,
                &[
                    "cat-file",
                    "--batch-all-objects",
                    "--batch-check=%(objecttype) %(objectname)",
                ],
            )
            .lines()
            .filter(|line| !commits_only || line.starts_with("commit "))
            .map(str::to_owned)
            .collect()
        }

        fn personal(&self, file: &Path, key: &str, value: &str) {
            git(
                &self.main,
                &["config", "--file", file.to_str().unwrap(), key, value],
            );
        }

        fn tip(&self) -> String {
            git(&self.main, &["rev-parse", "refs/heads/integrate/team"])
        }

        fn linked(&self) -> PathBuf {
            self.linked_branch("integrate/team")
        }

        fn linked_branch(&self, branch: &str) -> PathBuf {
            let linked = self.root.path().join("linked");
            git(
                &self.main,
                &["worktree", "add", linked.to_str().unwrap(), branch],
            );
            linked
        }
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn branch_observation_uses_git_for_loose_and_packed_refs_and_refuses_invalid_names() {
        let f = Fixture::new();
        assert!(!branch_exists(&f.repo, "integrate/team").unwrap());
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        assert!(branch_exists(&f.repo, "integrate/team").unwrap());
        git(&f.main, &["pack-refs", "--all"]);
        assert!(branch_exists(&f.repo, "integrate/team").unwrap());
        assert!(branch_exists(&f.repo, "../outside").is_err());
    }
    fn git(path: &Path, args: &[&str]) -> String {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        configure(&mut command, None);
        command
            .current_dir(path)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0");
        let output = fixture_output(&mut command);
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .trim_end()
            .to_owned()
    }

    fn fixture_output(command: &mut Command) -> Output {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::platform::spawn_service_child(command)
            .unwrap()
            .wait_with_output()
            .unwrap()
    }

    // Compare every byte and name in the checkout, including untracked files
    // and its index. Exclude only the shared object/ref database.
    fn snapshot(path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path == root.join(".git") && path.is_dir() {
                    continue;
                }
                if path.is_dir() {
                    visit(root, &path, entries);
                } else {
                    entries.insert(
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        Sha256::digest(fs::read(&path).unwrap()).to_vec(),
                    );
                }
            }
        }
        let mut entries = BTreeMap::new();
        visit(path, path, &mut entries);
        let git_dir = if path.join(".git").is_file() {
            PathBuf::from(
                fs::read_to_string(path.join(".git"))
                    .unwrap()
                    .trim()
                    .strip_prefix("gitdir: ")
                    .unwrap(),
            )
        } else {
            path.join(".git")
        };
        if git_dir.join("index").is_file() {
            entries.insert(
                PathBuf::from(".git/index"),
                Sha256::digest(fs::read(git_dir.join("index")).unwrap()).to_vec(),
            );
        }
        entries
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn clean_merge_advances_only_integration_with_both_parents() {
        let f = Fixture::new();
        let member = f.branch("member", "member.txt", "member\n");
        let prior = f.branch("prior", "prior.txt", "prior\n");
        create_branch(&f.repo, "integrate/team", &prior).unwrap();
        let linked = f.linked_branch("member");
        let before = (snapshot(&f.main), snapshot(&linked));
        let refs = f.refs();
        let Merge::Merged { commit } =
            merge(&f.repo, "integrate/team", &member, "Accept task").unwrap()
        else {
            panic!("expected merge");
        };
        assert_eq!(f.tip(), commit);
        let expected = refs
            .lines()
            .map(|line| {
                let (_, name) = line.split_once(' ').unwrap();
                if name == "refs/heads/integrate/team" {
                    format!("{commit} {name}")
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(f.refs(), expected);
        let reflog = git(
            &f.main,
            &[
                "reflog",
                "show",
                "-1",
                "--format=%gs",
                "refs/heads/integrate/team",
            ],
        );
        assert!(
            reflog.contains("Zerus") && reflog.contains("Accept task"),
            "{reflog}"
        );
        assert_eq!(
            git(&f.main, &["show", "-s", "--format=%P", &commit]),
            format!("{prior} {member}")
        );
        assert_eq!(
            git(&f.main, &["show", &format!("{commit}:member.txt")]),
            "member"
        );
        assert_eq!(
            git(&f.main, &["show", &format!("{commit}:prior.txt")]),
            "prior"
        );
        assert_eq!(git(&f.main, &["rev-parse", "main"]), f.base);
        assert_eq!(before, (snapshot(&f.main), snapshot(&linked)));
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn conflict_preserves_refs_reachable_commits_and_checkout() {
        let f = Fixture::new();
        let member = f.branch("member", "shared.txt", "member\n");
        let prior = f.branch("prior", "shared.txt", "prior\n");
        create_branch(&f.repo, "integrate/team", &prior).unwrap();
        let linked = f.linked_branch("member");
        let commits = f.objects(true);
        let before = (
            f.refs(),
            f.reachable(),
            snapshot(&f.main),
            snapshot(&linked),
        );
        assert_eq!(
            merge(&f.repo, "integrate/team", &member, "Accept").unwrap(),
            Merge::Conflict {
                files: vec!["shared.txt".into()]
            }
        );
        assert_eq!(
            before,
            (
                f.refs(),
                f.reachable(),
                snapshot(&f.main),
                snapshot(&linked)
            )
        );
        assert_eq!(
            commits,
            f.objects(true),
            "a conflict must not create even an unreachable commit"
        );
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn a_moved_ref_is_stale_then_recomputed_without_losing_work() {
        let f = Fixture::new();
        let member = f.branch("member", "member.txt", "member\n");
        let concurrent = f.branch("concurrent", "other.txt", "other\n");
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        let before = snapshot(&f.main);
        let result = merge_with_hook(&f.repo, "integrate/team", &member, "Accept", || {
            git(
                &f.main,
                &[
                    "update-ref",
                    "refs/heads/integrate/team",
                    &concurrent,
                    &f.base,
                ],
            );
        })
        .unwrap();
        assert_eq!(result, Merge::Stale);
        assert_eq!(f.tip(), concurrent);
        let Merge::Merged { commit } =
            merge(&f.repo, "integrate/team", &member, "Accept again").unwrap()
        else {
            panic!("expected recomputed merge");
        };
        assert_eq!(
            git(&f.main, &["show", "-s", "--format=%P", &commit]),
            format!("{concurrent} {member}")
        );
        assert_eq!(
            git(&f.main, &["show", &format!("{commit}:other.txt")]),
            "other"
        );
        assert_eq!(before, snapshot(&f.main));
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn a_deleted_ref_is_stale_and_is_not_recreated() {
        let f = Fixture::new();
        let member = f.branch("member", "new.txt", "new\n");
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        let before = snapshot(&f.main);
        let result = merge_with_hook(&f.repo, "integrate/team", &member, "Accept", || {
            git(
                &f.main,
                &["update-ref", "-d", "refs/heads/integrate/team", &f.base],
            );
        })
        .unwrap();
        assert_eq!(result, Merge::Stale);
        assert!(!f.refs().contains("refs/heads/integrate/team"));
        assert_eq!(before, snapshot(&f.main));
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn a_ref_lock_failure_is_not_misreported_as_stale() {
        let f = Fixture::new();
        let member = f.branch("member", "new.txt", "new\n");
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        let lock = f.repo.git_dir.join("refs/heads/integrate/team.lock");
        let error = merge_with_hook(&f.repo, "integrate/team", &member, "Accept", || {
            fs::write(&lock, "held\n").unwrap();
        })
        .unwrap_err();
        assert_eq!(error.code, "integration_unavailable");
        assert_eq!(f.tip(), f.base);
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn a_checked_out_branch_is_refused_in_every_worktree() {
        let f = Fixture::new();
        let member = f.branch("member", "new.txt", "new\n");
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        let linked = f.linked();
        let before = (f.refs(), snapshot(&f.main), snapshot(&linked));
        assert_eq!(
            merge(&f.repo, "integrate/team", &member, "Accept")
                .unwrap_err()
                .code,
            "integration_unavailable"
        );
        assert_eq!(
            merge(&f.repo, "main", &member, "Accept").unwrap_err().code,
            "integration_unavailable"
        );
        assert_eq!(before, (f.refs(), snapshot(&f.main), snapshot(&linked)));
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn a_branch_checked_out_after_merge_computation_is_refused() {
        let f = Fixture::new();
        let member = f.branch("member", "new.txt", "new\n");
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        let error = merge_with_hook(&f.repo, "integrate/team", &member, "Accept", || {
            f.linked();
        })
        .unwrap_err();
        assert_eq!(error.code, "integration_unavailable");
        assert_eq!(f.tip(), f.base);
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn creation_never_replaces_an_existing_ref() {
        let f = Fixture::new();
        let other = f.branch("other", "other.txt", "other\n");
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        let before = (f.refs(), snapshot(&f.main));
        assert!(create_branch(&f.repo, "integrate/team", &other).is_err());
        for name in ["../main", "refs/tags/tag", "HEAD", "bad..name"] {
            assert!(create_branch(&f.repo, name, &f.base).is_err(), "{name}");
        }
        assert_eq!(before, (f.refs(), snapshot(&f.main)));
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn ancestry_checks_report_unrelated_and_invalid_commits() {
        let f = Fixture::new();
        let member = f.branch("member", "new.txt", "new\n");
        let tree = git(&f.main, &["rev-parse", "HEAD^{tree}"]);
        let unrelated = git(&f.main, &["commit-tree", &tree, "-m", "Unrelated"]);
        let before = (f.refs(), snapshot(&f.main));
        assert!(descends(&f.repo, &member, &f.base).unwrap());
        assert!(!descends(&f.repo, &unrelated, &f.base).unwrap());
        assert!(descends(&f.repo, "--all", &f.base).is_err());
        assert_eq!(before, (f.refs(), snapshot(&f.main)));
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn report_pins_commit_and_branch_and_reports_dirty_without_writes() {
        let f = Fixture::new();
        let before = snapshot(&f.main);
        assert_eq!(
            reported_commit(&f.main).unwrap(),
            Reported {
                commit: f.base.clone(),
                branch: "main".into(),
                dirty: false
            }
        );
        assert_eq!(before, snapshot(&f.main));
        fs::write(f.main.join("shared.txt"), "dirty\n").unwrap();
        fs::write(f.main.join("untracked.txt"), "untracked\n").unwrap();
        let before = snapshot(&f.main);
        assert!(reported_commit(&f.main).unwrap().dirty);
        assert_eq!(before, snapshot(&f.main));
        git(&f.main, &["checkout", "--detach"]);
        assert_eq!(reported_commit(&f.main).unwrap().branch, "");
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn reports_work_for_linked_worktrees_and_untracked_only_changes() {
        let f = Fixture::new();
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        let linked = f.linked();
        assert_eq!(
            reported_commit(&linked).unwrap(),
            Reported {
                commit: f.base.clone(),
                branch: "integrate/team".into(),
                dirty: false
            }
        );
        fs::write(linked.join("untracked.txt"), "untracked\n").unwrap();
        let before = snapshot(&linked);
        assert!(reported_commit(&linked).unwrap().dirty);
        assert_eq!(before, snapshot(&linked));
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn symbolic_integration_refs_cannot_redirect_a_write_to_another_branch() {
        let f = Fixture::new();
        let member = f.branch("member", "new.txt", "new\n");
        git(
            &f.main,
            &[
                "symbolic-ref",
                "refs/heads/integrate/team",
                "refs/heads/member",
            ],
        );
        let before = (f.refs(), snapshot(&f.main));
        assert_eq!(
            merge(&f.repo, "integrate/team", &member, "Accept")
                .unwrap_err()
                .code,
            "integration_unavailable"
        );
        assert!(create_branch(&f.repo, "integrate/team", &f.base).is_err());
        assert_eq!(before, (f.refs(), snapshot(&f.main)));
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn minimum_git_version_is_checked_conservatively() {
        let f = Fixture::new();
        let before = (f.refs(), snapshot(&f.main));
        for version in [
            "git version 2.37.9",
            "git version 1.99.0",
            "git version unknown",
            "2.38.0",
        ] {
            assert!(!supported_version(version.as_bytes()), "{version}");
        }
        for version in [
            "git version 2.38.0",
            "git version 2.56.0.windows.1",
            "git version 2.39.3 (Apple Git-146)",
            "git version 3.0.0",
        ] {
            assert!(supported_version(version.as_bytes()), "{version}");
        }
        assert_eq!(before, (f.refs(), snapshot(&f.main)));
    }

    /// Windows canonicalizes to a verbatim `\\?\` path, which Git cannot use
    /// as `--git-dir`: it refused every repository-scoped command with
    /// "--local can only be used inside a git repository" (zerus-63rru.111).
    #[cfg(windows)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn the_repository_reaches_git_as_a_plain_path() {
        let f = Fixture::new();
        let verbatim = f.repo.git_dir.canonicalize().unwrap();
        assert!(
            verbatim.to_string_lossy().starts_with(r"\\?\"),
            "{verbatim:?}"
        );
        let git = Git::repo(&f.repo).unwrap();
        assert!(
            !git.path.to_string_lossy().starts_with(r"\\?\"),
            "{:?}",
            git.path
        );
        assert_eq!(git.path, crate::platform::canonical(&verbatim).unwrap());
        assert!(!branch_exists(&f.repo, "integrate/team").unwrap());
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        assert_eq!(f.tip(), f.base);
    }

    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn conflict_filenames_are_nul_delimited_without_git_quoting() {
        let f = Fixture::new();
        let path = "line\nquote\".txt";
        fs::write(f.main.join(path), "base\n").unwrap();
        git(&f.main, &["add", "."]);
        git(&f.main, &["commit", "-m", "Add unusual path"]);
        let f = Fixture {
            base: git(&f.main, &["rev-parse", "HEAD"]),
            ..f
        };
        let member = f.branch("member", path, "member\n");
        let prior = f.branch("prior", path, "prior\n");
        create_branch(&f.repo, "integrate/team", &prior).unwrap();
        let before = (f.refs(), f.reachable(), snapshot(&f.main));
        assert_eq!(
            merge(&f.repo, "integrate/team", &member, "Accept").unwrap(),
            Merge::Conflict {
                files: vec![path.into()]
            }
        );
        assert_eq!(before, (f.refs(), f.reachable(), snapshot(&f.main)));
    }

    #[cfg(unix)]
    fn executable(path: &Path, text: &str) {
        use std::os::unix::fs::PermissionsExt;
        fs::write(path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn hooks_and_fsmonitor_are_never_run() {
        let f = Fixture::new();
        let member = f.branch("member", "new.txt", "new\n");
        let hooks = f.root.path().join("hooks");
        fs::create_dir(&hooks).unwrap();
        let marker = f.root.path().join("executed");
        let script = format!("#!/bin/sh\nprintf ran > '{}'\nexit 1\n", marker.display());
        for hook in [
            "pre-commit",
            "post-commit",
            "reference-transaction",
            "fsmonitor",
        ] {
            executable(&hooks.join(hook), &script);
        }
        git(
            &f.main,
            &["config", "core.hooksPath", hooks.to_str().unwrap()],
        );
        git(
            &f.main,
            &[
                "config",
                "core.fsmonitor",
                hooks.join("fsmonitor").to_str().unwrap(),
            ],
        );
        let before = snapshot(&f.main);
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        assert!(matches!(
            merge(&f.repo, "integrate/team", &member, "Accept").unwrap(),
            Merge::Merged { .. }
        ));
        assert!(!reported_commit(&f.main).unwrap().dirty);
        assert!(!marker.exists());
        assert_eq!(before, snapshot(&f.main));
    }

    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn a_repository_cannot_execute_merge_drivers_or_content_filters() {
        let f = Fixture::new();
        let member = f.branch("member", "new.txt", "new\n");
        let marker = f.root.path().join("executed");
        let command = format!("printf ran > '{}'; cat", marker.display());
        git(&f.main, &["config", "merge.custom.driver", &command]);
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        assert!(descends(&f.repo, &member, &f.base).unwrap());
        assert!(!reported_commit(&f.main).unwrap().dirty);
        assert_eq!(
            merge(&f.repo, "integrate/team", &member, "Accept")
                .unwrap_err()
                .code,
            "integration_unavailable"
        );
        assert_eq!(
            sync_base(&f.repo, "integrate/team", "main")
                .unwrap_err()
                .code,
            "integration_unavailable"
        );
        git(&f.main, &["config", "--unset", "merge.custom.driver"]);
        fs::write(f.main.join(".gitattributes"), "shared.txt filter=custom\n").unwrap();
        git(&f.main, &["config", "filter.custom.clean", &command]);
        fs::write(f.main.join("shared.txt"), "changed\n").unwrap();
        let before = snapshot(&f.main);
        assert_eq!(
            reported_commit(&f.main).unwrap_err().code,
            "integration_unavailable"
        );
        assert!(!marker.exists());
        assert_eq!(before, snapshot(&f.main));
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn required_signing_failure_does_not_publish_an_unsigned_commit() {
        let f = Fixture::new();
        let member = f.branch("member", "new.txt", "new\n");
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        git(&f.main, &["config", "commit.gpgsign", "true"]);
        git(&f.main, &["config", "gpg.format", "unsupported"]);
        let before = (f.refs(), f.reachable(), snapshot(&f.main));
        assert_eq!(
            merge(&f.repo, "integrate/team", &member, "Accept")
                .unwrap_err()
                .code,
            "integration_unavailable"
        );
        assert_eq!(before, (f.refs(), f.reachable(), snapshot(&f.main)));
    }

    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn required_signing_produces_a_verifiable_signed_merge() {
        let f = Fixture::new();
        let member = f.branch("member", "new.txt", "new\n");
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        let key = f.root.path().join("signing-key");
        let mut command = Command::new("ssh-keygen");
        command
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key);
        let result = fixture_output(&mut command);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let allowed = f.root.path().join("allowed-signers");
        fs::write(
            &allowed,
            format!(
                "developer@example.invalid {}",
                fs::read_to_string(key.with_extension("pub")).unwrap()
            ),
        )
        .unwrap();
        git(&f.main, &["config", "gpg.format", "ssh"]);
        git(
            &f.main,
            &["config", "user.signingkey", key.to_str().unwrap()],
        );
        git(
            &f.main,
            &[
                "config",
                "gpg.ssh.allowedSignersFile",
                allowed.to_str().unwrap(),
            ],
        );
        git(&f.main, &["config", "commit.gpgsign", "true"]);
        let before = snapshot(&f.main);
        let Merge::Merged { commit } =
            merge(&f.repo, "integrate/team", &member, "Accept signed").unwrap()
        else {
            panic!("expected signed merge");
        };
        assert!(git(&f.main, &["cat-file", "-p", &commit])
            .contains("gpgsig -----BEGIN SSH SIGNATURE-----"));
        git(&f.main, &["verify-commit", &commit]);
        assert_eq!(before, snapshot(&f.main));
    }

    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn a_worktree_cannot_declare_a_signing_program() {
        let f = Fixture::new();
        let member = f.branch("member", "new.txt", "new\n");
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        let marker = f.root.path().join("executed");
        let signer = f.root.path().join("signer");
        executable(
            &signer,
            &format!("#!/bin/sh\nprintf ran > '{}'\nexit 1\n", marker.display()),
        );
        git(&f.main, &["config", "extensions.worktreeConfig", "true"]);
        git(&f.main, &["config", "--worktree", "commit.gpgsign", "true"]);
        git(
            &f.main,
            &[
                "config",
                "--worktree",
                "gpg.program",
                signer.to_str().unwrap(),
            ],
        );
        let before = (f.refs(), snapshot(&f.main));
        assert_eq!(
            merge(&f.repo, "integrate/team", &member, "Accept")
                .unwrap_err()
                .code,
            "integration_unavailable"
        );
        assert!(!marker.exists());
        assert_eq!(before, (f.refs(), snapshot(&f.main)));
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn partial_clone_refuses_before_git_can_fetch_missing_objects() {
        let f = Fixture::new();
        for value in ["true", ""] {
            git(&f.main, &["config", "remote.origin.promisor", value]);
            let before = (f.refs(), snapshot(&f.main));
            assert_eq!(
                create_branch(&f.repo, "integrate/team", &f.base)
                    .unwrap_err()
                    .code,
                "integration_unavailable"
            );
            assert_eq!(before, (f.refs(), snapshot(&f.main)));
        }
    }

    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn a_submodule_report_cannot_run_a_nested_filter() {
        let f = Fixture::new();
        let nested = f.main.join("nested");
        fs::create_dir(&nested).unwrap();
        git(&nested, &["init", "-b", "main"]);
        git(&nested, &["config", "user.name", "Developer"]);
        git(
            &nested,
            &["config", "user.email", "developer@example.invalid"],
        );
        git(&nested, &["config", "commit.gpgsign", "false"]);
        fs::write(nested.join("tracked.txt"), "base\n").unwrap();
        git(&nested, &["add", "."]);
        git(&nested, &["commit", "-m", "Nested base"]);
        let head = git(&nested, &["rev-parse", "HEAD"]);
        git(
            &f.main,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{head},nested"),
            ],
        );
        git(&f.main, &["commit", "-m", "Add gitlink"]);
        let marker = f.root.path().join("executed");
        let command = format!("printf ran > '{}'; cat", marker.display());
        fs::write(nested.join(".gitattributes"), "tracked.txt filter=nested\n").unwrap();
        git(&nested, &["config", "filter.nested.clean", &command]);
        fs::write(nested.join("tracked.txt"), "dirty\n").unwrap();
        let before = snapshot(&f.main);
        assert_eq!(
            reported_commit(&f.main).unwrap_err().code,
            "integration_unavailable"
        );
        assert!(!marker.exists());
        assert_eq!(before, snapshot(&f.main));
    }

    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn personal_config_hooks_are_disabled_in_global_and_system_scopes() {
        for system in [false, true] {
            let f = Fixture::new();
            let member = f.branch("member", "new.txt", "new\n");
            let marker = f.root.path().join("hook-ran");
            let probe = f.root.path().join("hook-probe");
            executable(
                &probe,
                &format!("#!/bin/sh\nprintf ran >> '{}'\n", marker.display()),
            );
            let config = if system {
                &f.config.system
            } else {
                &f.config.global
            };
            f.personal(
                config,
                "hook.integration-probe.command",
                probe.to_str().unwrap(),
            );
            f.personal(
                config,
                "hook.integration-probe.event",
                "reference-transaction",
            );
            let before = snapshot(&f.main);
            create_branch(&f.repo, "integrate/team", &f.base).unwrap();
            merge(&f.repo, "integrate/team", &member, "Accept").unwrap();
            sync_base(&f.repo, "integrate/team", "main").unwrap();
            assert!(
                !marker.exists(),
                "personal config hook ran (system={system})"
            );
            assert_eq!(before, snapshot(&f.main));
        }
    }

    #[cfg(unix)]
    fn repository_command_config_is_refused(worktree: bool, hooks: bool) {
        for included in [false, true] {
            let f = Fixture::new();
            let member = f.branch("member", "new.txt", "new\n");
            create_branch(&f.repo, "integrate/team", &f.base).unwrap();
            let marker = f.root.path().join("ran");
            let probe = f.root.path().join("probe");
            executable(
                &probe,
                &format!("#!/bin/sh\nprintf ran >> '{}'\n", marker.display()),
            );
            if worktree {
                git(&f.main, &["config", "extensions.worktreeConfig", "true"]);
            }
            let scope = if worktree { "--worktree" } else { "--local" };
            let file = f.root.path().join("included-config");
            if included {
                git(
                    &f.main,
                    &["config", scope, "include.path", file.to_str().unwrap()],
                );
            }
            let set = |key: &str, value: &str| {
                if included {
                    f.personal(&file, key, value);
                } else {
                    git(&f.main, &["config", scope, key, value]);
                }
            };
            if hooks {
                set("hook.probe.command", probe.to_str().unwrap());
                set("hook.probe.event", "reference-transaction");
            } else {
                set("merge.custom.driver", probe.to_str().unwrap());
            }
            let before = (f.refs(), snapshot(&f.main));
            if hooks {
                assert_eq!(
                    create_branch(&f.repo, "integrate/new", &f.base)
                        .unwrap_err()
                        .code,
                    "integration_unavailable"
                );
            } else {
                create_branch(&f.repo, "integrate/new", &f.base).unwrap();
                assert!(descends(&f.repo, &member, &f.base).unwrap());
                assert!(!reported_commit(&f.main).unwrap().dirty);
            }
            assert_eq!(
                merge(&f.repo, "integrate/team", &member, "Accept")
                    .unwrap_err()
                    .code,
                "integration_unavailable"
            );
            assert_eq!(
                sync_base(&f.repo, "integrate/team", "main")
                    .unwrap_err()
                    .code,
                "integration_unavailable"
            );
            assert!(!marker.exists());
            if hooks {
                assert_eq!(before, (f.refs(), snapshot(&f.main)));
            }
        }
    }

    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn repository_local_hooks_and_included_hooks_refuse() {
        repository_command_config_is_refused(false, true);
    }
    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn worktree_hooks_and_included_hooks_refuse() {
        repository_command_config_is_refused(true, true);
    }
    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn worktree_drivers_and_includes_only_block_merging() {
        repository_command_config_is_refused(true, false);
    }
    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn local_drivers_and_includes_only_block_merging() {
        repository_command_config_is_refused(false, false);
    }

    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn personal_merge_drivers_are_honored_in_global_and_system_scopes() {
        for system in [false, true] {
            let f = Fixture::new();
            fs::write(f.main.join(".gitattributes"), "shared.txt merge=personal\n").unwrap();
            git(&f.main, &["add", "."]);
            git(&f.main, &["commit", "-m", "Attributes"]);
            let f = Fixture {
                base: git(&f.main, &["rev-parse", "HEAD"]),
                ..f
            };
            let member = f.branch("member", "shared.txt", "member\n");
            let prior = f.branch("prior", "shared.txt", "prior\n");
            let marker = f.root.path().join("driver-ran");
            let driver = f.root.path().join("driver");
            executable(
                &driver,
                &format!(
                    "#!/bin/sh\nprintf ran >> '{}'\ncat \"$3\" > \"$2\"\n",
                    marker.display()
                ),
            );
            let config = if system {
                &f.config.system
            } else {
                &f.config.global
            };
            f.personal(
                config,
                "merge.personal.driver",
                &format!("'{}' %O %A %B", driver.display()),
            );
            create_branch(&f.repo, "integrate/team", &prior).unwrap();
            assert!(!reported_commit(&f.main).unwrap().dirty);
            assert!(descends(&f.repo, &member, &f.base).unwrap());
            assert!(!marker.exists());
            let Merge::Merged { commit } =
                merge(&f.repo, "integrate/team", &member, "Accept").unwrap()
            else {
                panic!("expected personal driver merge");
            };
            assert_eq!(
                git(&f.main, &["show", &format!("{commit}:shared.txt")]),
                "member"
            );
            assert!(marker.exists());
        }
    }

    #[cfg(unix)]
    fn private_driver_context(timeout: bool) {
        let f = Fixture::new();
        fs::write(f.main.join(".gitattributes"), "shared.txt merge=personal\n").unwrap();
        git(&f.main, &["add", "."]);
        git(&f.main, &["commit", "-m", "Attributes"]);
        let f = Fixture {
            base: git(&f.main, &["rev-parse", "HEAD"]),
            ..f
        };
        let member = f.branch("member", "shared.txt", "member\n");
        let prior = f.branch("prior", "shared.txt", "prior\n");
        create_branch(&f.repo, "integrate/team", &prior).unwrap();
        let cwd = f.root.path().join("driver-cwd");
        let primary_temps = f.root.path().join("primary-temps");
        let own_temps = f.root.path().join("driver-temps");
        let ready = f.root.path().join("driver-ready");
        let driver = f.root.path().join("driver");
        let finish = if timeout {
            "exec sleep 5\n"
        } else {
            "cat \"$3\" > \"$2\"\n"
        };
        executable(&driver, &format!("#!/bin/sh\npwd -P > '{}'\nfor file in '{}'/.merge_file_*; do\n if [ -e \"$file\" ]; then printf found >> '{}'; fi\ndone\nfor file in .merge_file_*; do\n if [ -e \"$file\" ]; then printf found >> '{}'; fi\ndone\nprintf temporary > driver-output\nprintf ready > '{}'\n{finish}", cwd.display(), f.main.display(), primary_temps.display(), own_temps.display(), ready.display()));
        f.personal(
            &f.config.global,
            "merge.personal.driver",
            &format!("'{}' %O %A %B", driver.display()),
        );
        let before = (f.refs(), snapshot(&f.main));
        let git = Git::repo(&f.repo).unwrap();
        // Startup keeps Git's normal outer bound; this test's stop clock
        // starts only after the driver has written all its evidence.
        let clock = timeout.then(|| driver_clock::Clock::arm(ready));
        let result = merge_git(&git, "integrate/team", &member, "Accept", || {}, || Ok(()));
        if timeout {
            assert!(
                clock.as_ref().unwrap().expired(),
                "the driver must start and time out"
            );
            assert_eq!(result.unwrap_err().code, "integration_unavailable");
            assert_eq!(f.refs(), before.0);
        } else {
            assert!(matches!(result.unwrap(), Merge::Merged { .. }));
        }
        drop(clock);
        let actual = PathBuf::from(fs::read_to_string(cwd).unwrap().trim_end());
        assert_ne!(actual, f.main.canonicalize().unwrap());
        assert!(
            !primary_temps.exists(),
            "Git created merge files in the primary checkout"
        );
        assert!(
            own_temps.exists(),
            "the probe must observe Git's merge files while running"
        );
        assert!(
            !actual.exists(),
            "the private merge directory must be removed after success or failure"
        );
        assert_eq!(before.1, snapshot(&f.main));
    }

    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn personal_driver_and_merge_files_use_a_private_directory() {
        private_driver_context(false);
    }

    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn timed_out_driver_leaves_no_checkout_files_or_private_directory() {
        private_driver_context(true);
    }

    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn uncommitted_checkout_attributes_cannot_select_a_personal_driver() {
        let f = Fixture::new();
        let member = f.branch("member", "shared.txt", "member\n");
        let prior = f.branch("prior", "shared.txt", "prior\n");
        create_branch(&f.repo, "integrate/team", &prior).unwrap();
        fs::write(f.main.join(".gitattributes"), "shared.txt merge=personal\n").unwrap();
        git(&f.main, &["add", ".gitattributes"]);
        let marker = f.root.path().join("driver-ran");
        let driver = f.root.path().join("driver");
        executable(
            &driver,
            &format!(
                "#!/bin/sh\nprintf ran >> '{}'\ncat \"$3\" > \"$2\"\n",
                marker.display()
            ),
        );
        f.personal(
            &f.config.global,
            "merge.personal.driver",
            &format!("'{}' %O %A %B", driver.display()),
        );
        let before = (f.refs(), f.objects(true), snapshot(&f.main));
        assert_eq!(
            merge(&f.repo, "integrate/team", &member, "Accept").unwrap(),
            Merge::Conflict {
                files: vec!["shared.txt".to_owned()]
            }
        );
        assert!(!marker.exists());
        assert_eq!(before, (f.refs(), f.objects(true), snapshot(&f.main)));
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn many_changed_paths_use_bounded_attribute_argument_batches() {
        let f = Fixture::new();
        let blob = git(&f.main, &["rev-parse", "HEAD:shared.txt"]);
        let mut entries = Vec::new();
        let suffix = "x".repeat(120);
        for n in 0..25_000 {
            write!(entries, "100644 {blob}\tchanged/{n:05}-{suffix}.txt\0").unwrap();
        }
        git_input(&f.main, &["update-index", "-z", "--index-info"], &entries);
        let tree = git(&f.main, &["write-tree"]);
        let commit = git(
            &f.main,
            &[
                "commit-tree",
                &tree,
                "-p",
                &f.base,
                "-m",
                "Missing tracked paths",
            ],
        );
        git(
            &f.main,
            &["update-ref", "refs/heads/main", &commit, &f.base],
        );
        git(&f.main, &["config", "filter.unused.clean", "cat"]);
        // Each report below runs about 440 Git processes under one outer
        // deadline: 7 seconds of the production 30 on an idle machine, all of
        // it on a loaded one. This case is about the batches, not the
        // machine's speed, so its deadline is one no slow machine reaches.
        let _budget = Budget::set(Duration::from_secs(600));
        let before = (f.refs(), snapshot(&f.main));
        assert_eq!(
            reported_commit(&f.main).unwrap(),
            Reported {
                commit,
                branch: "main".into(),
                dirty: true
            }
        );
        assert!(f.config.attrs.get() > 1 && f.config.attrs.get() < 25_000);
        assert_eq!(before, (f.refs(), snapshot(&f.main)));
        // A filter match in the final batch must still refuse the report.
        fs::write(
            f.main.join(".gitattributes"),
            format!("changed/24999-{suffix}.txt filter=unused\n"),
        )
        .unwrap();
        let before = (f.refs(), snapshot(&f.main));
        assert_eq!(
            reported_commit(&f.main).unwrap_err().code,
            "integration_unavailable"
        );
        assert_eq!(before, (f.refs(), snapshot(&f.main)));
    }

    /// A Git opened through the product path, with no fixture budget, has
    /// the production thirty seconds.
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn a_git_without_a_fixture_budget_has_the_production_thirty_seconds() {
        let f = Fixture::new();
        let before = Instant::now();
        let git = Git::repo(&f.repo).unwrap();
        let after = Instant::now();
        let thirty = Duration::from_secs(30);
        assert!(
            git.deadline >= before + thirty && git.deadline <= after + thirty,
            "the outer deadline is not thirty seconds from the opening"
        );
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn a_failed_spawn_is_a_certain_nonwrite() {
        let f = Fixture::new();
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        let member = f.branch("member", "new.txt", "new\n");
        let mut git = Git::repo(&f.repo).unwrap();
        git.program = f.root.path().join("missing-git");
        let before = f.refs();
        assert_eq!(
            git.run(&["update-ref", "refs/heads/integrate/team", &member, &f.base])
                .unwrap_err()
                .code,
            "integration_unavailable"
        );
        assert_eq!(before, f.refs());
    }

    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn a_spawn_turn_timeout_has_no_start_receipt() {
        let f = Fixture::new();
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        let member = f.branch("member", "new.txt", "new\n");
        let mut git = Git::repo(&f.repo).unwrap();
        let before = f.refs();
        let turn = crate::platform::spawn_turn();
        git.deadline = Instant::now() + Duration::from_millis(100);
        let result = git.run(&["update-ref", "refs/heads/integrate/team", &member, &f.base]);
        drop(turn);
        // Stopped carries no launch phase. Retain uncertainty rather than
        // claim a started writer or authorize retry from this opaque result.
        assert_eq!(result.unwrap_err().code, "delivery_uncertain");
        assert_eq!(before, f.refs());
    }

    fn git_input(path: &Path, args: &[&str], input: &[u8]) {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        configure(&mut command, None);
        command
            .current_dir(path)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = crate::platform::spawn_service_child(&mut command).unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn report_handles_a_large_synthetic_index() {
        let f = Fixture::new();
        let blob = git(&f.main, &["rev-parse", "HEAD:shared.txt"]);
        let mut entries = Vec::new();
        let mut paths = Vec::new();
        for n in 0..120_000 {
            let path =
                format!("generated/{n:06}-long-synthetic-name-for-index-scaling-qualification.txt");
            write!(entries, "100644 {blob}\t{path}\0").unwrap();
            write!(paths, "{path}\0").unwrap();
        }
        git_input(&f.main, &["update-index", "-z", "--index-info"], &entries);
        git_input(
            &f.main,
            &["update-index", "--skip-worktree", "-z", "--stdin"],
            &paths,
        );
        let tree = git(&f.main, &["write-tree"]);
        let commit = git(
            &f.main,
            &["commit-tree", &tree, "-p", &f.base, "-m", "Synthetic index"],
        );
        git(
            &f.main,
            &["update-ref", "refs/heads/main", &commit, &f.base],
        );
        let before = snapshot(&f.main);
        assert_eq!(
            reported_commit(&f.main).unwrap(),
            Reported {
                commit,
                branch: "main".into(),
                dirty: false
            }
        );
        assert_eq!(before, snapshot(&f.main));
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn small_changed_path_sets_share_one_attribute_batch() {
        let f = Fixture::new();
        git(&f.main, &["config", "filter.unused.clean", "cat"]);
        fs::write(f.main.join("shared.txt"), "changed\n").unwrap();
        fs::write(f.main.join("other.txt"), "other\n").unwrap();
        assert!(reported_commit(&f.main).unwrap().dirty);
        assert_eq!(f.config.attrs.get(), 1);
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn unchanged_base_sync_and_duplicate_merges_write_nothing() {
        let f = Fixture::new();
        let member = f.branch("member", "new.txt", "new\n");
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        let before = (f.refs(), f.objects(false), snapshot(&f.main));
        let log = f.repo.git_dir.join("logs/refs/heads/integrate/team");
        let reflog = fs::read(&log).unwrap();
        assert_eq!(
            sync_base(&f.repo, "integrate/team", "main").unwrap(),
            Merge::Merged {
                commit: f.base.clone()
            }
        );
        assert_eq!(before, (f.refs(), f.objects(false), snapshot(&f.main)));
        assert_eq!(reflog, fs::read(&log).unwrap());
        let Merge::Merged { commit } = merge(&f.repo, "integrate/team", &member, "Accept").unwrap()
        else {
            panic!("expected merge");
        };
        let before = (f.refs(), f.objects(false), snapshot(&f.main));
        let reflog = fs::read(&log).unwrap();
        assert_eq!(
            merge(&f.repo, "integrate/team", &member, "Accept twice").unwrap(),
            Merge::Merged { commit }
        );
        assert_eq!(before, (f.refs(), f.objects(false), snapshot(&f.main)));
        assert_eq!(reflog, fs::read(&log).unwrap());
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn recompute_after_another_writer_accepted_the_same_commit_is_a_noop() {
        let f = Fixture::new();
        let member = f.branch("member", "new.txt", "new\n");
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        assert_eq!(
            merge_with_hook(&f.repo, "integrate/team", &member, "This writer", || {
                assert!(matches!(
                    merge(&f.repo, "integrate/team", &member, "Other writer").unwrap(),
                    Merge::Merged { .. }
                ));
            })
            .unwrap(),
            Merge::Stale
        );
        let tip = f.tip();
        let before = (f.refs(), f.objects(false), snapshot(&f.main));
        assert_eq!(
            merge(&f.repo, "integrate/team", &member, "Recompute").unwrap(),
            Merge::Merged { commit: tip }
        );
        assert_eq!(before, (f.refs(), f.objects(false), snapshot(&f.main)));
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn detached_rebase_and_bisect_states_keep_the_integration_branch_busy() {
        for state in [
            "rebase-merge/head-name",
            "rebase-apply/head-name",
            "BISECT_START",
            "rebase-merge/update-refs",
        ] {
            for linked in [false, true] {
                let f = Fixture::new();
                let member = f.branch("member", "new.txt", "new\n");
                create_branch(&f.repo, "integrate/team", &f.base).unwrap();
                let path = if linked { f.linked() } else { f.main.clone() };
                git(&path, &["checkout", "--detach"]);
                let admin = PathBuf::from(git(&path, &["rev-parse", "--absolute-git-dir"]));
                let file = admin.join(state);
                fs::create_dir_all(file.parent().unwrap()).unwrap();
                let value = if state.ends_with("update-refs") {
                    let mut value = String::new();
                    for n in 0..200 {
                        value.push_str(&format!("refs/heads/other-{n}\n{}\n{}\n", f.base, f.base));
                    }
                    value.push_str(&format!(
                        "refs/heads/integrate/team\n{}\n{}\n",
                        f.base, f.base
                    ));
                    value
                } else if state == "BISECT_START" {
                    "integrate/team\n".to_owned()
                } else {
                    "refs/heads/integrate/team\n".to_owned()
                };
                fs::write(file, value).unwrap();
                let before = (f.refs(), snapshot(&f.main), snapshot(&path));
                assert_eq!(
                    merge(&f.repo, "integrate/team", &member, "Accept")
                        .unwrap_err()
                        .code,
                    "integration_unavailable",
                    "{state}, linked={linked}"
                );
                assert_eq!(before, (f.refs(), snapshot(&f.main), snapshot(&path)));
            }
        }
    }

    #[cfg(unix)]
    fn delayed_writer(f: &Fixture) -> PathBuf {
        let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|path| path.join("git"))
            .find(|path| path.is_file())
            .unwrap();
        let program = f.root.path().join("delayed-git");
        executable(&program, &format!("#!/bin/sh\nfor arg do\n if [ \"$arg\" = update-ref ]; then\n  '{}' \"$@\" || exit $?\n  printf ran > '{}'\n  exec sleep 5\n fi\ndone\nexec '{}' \"$@\"\n", real.display(), f.root.path().join("writer-ran").display(), real.display()));
        program
    }

    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn an_expired_deadline_is_a_certain_nonwrite() {
        let f = Fixture::new();
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        let member = f.branch("member", "new.txt", "new\n");
        let mut git = Git::repo(&f.repo).unwrap();
        git.program = delayed_writer(&f);
        git.deadline = Instant::now() - Duration::from_secs(1);
        let before = f.refs();
        assert_eq!(
            git.run(&["update-ref", "refs/heads/integrate/team", &member, &f.base])
                .unwrap_err()
                .code,
            "integration_unavailable"
        );
        assert!(!f.root.path().join("writer-ran").exists());
        assert_eq!(before, f.refs());
    }

    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn a_started_writer_with_a_lost_result_is_uncertain() {
        let f = Fixture::new();
        create_branch(&f.repo, "integrate/team", &f.base).unwrap();
        let member = f.branch("member", "new.txt", "new\n");
        let mut git = Git::repo(&f.repo).unwrap();
        git.program = delayed_writer(&f);
        git.deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            git.run(&["update-ref", "refs/heads/integrate/team", &member, &f.base])
                .unwrap_err()
                .code,
            "delivery_uncertain"
        );
        assert!(f.root.path().join("writer-ran").exists());
        assert_eq!(f.tip(), member);
    }

    #[test]

    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.

    #[cfg_attr(windows, ignore)]
    fn sync_base_uses_the_same_merge_and_conflict_path() {
        let f = Fixture::new();
        let prior = f.branch("prior", "prior.txt", "prior\n");
        create_branch(&f.repo, "integrate/team", &prior).unwrap();
        fs::write(f.main.join("shared.txt"), "base advanced\n").unwrap();
        git(&f.main, &["commit", "-am", "Advance base"]);
        let base = git(&f.main, &["rev-parse", "HEAD"]);
        let before = snapshot(&f.main);
        let Merge::Merged { commit } = sync_base(&f.repo, "integrate/team", "main").unwrap() else {
            panic!("expected merge");
        };
        assert_eq!(
            git(&f.main, &["show", "-s", "--format=%P", &commit]),
            format!("{prior} {base}")
        );
        assert_eq!(before, snapshot(&f.main));

        let conflicting = f.branch("conflicting", "shared.txt", "different\n");
        create_branch(&f.repo, "integrate/conflict", &conflicting).unwrap();
        let before = (f.refs(), f.reachable(), snapshot(&f.main));
        assert_eq!(
            sync_base(&f.repo, "integrate/conflict", "main").unwrap(),
            Merge::Conflict {
                files: vec!["shared.txt".into()]
            }
        );
        assert_eq!(before, (f.refs(), f.reachable(), snapshot(&f.main)));
    }
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn containing_tip_pins_the_branch_commit_and_keeps_failed_reads_unknown() {
        let f = Fixture::new();
        let member = f.branch("member", "member.txt", "member");
        create_branch(&f.repo, "teams/integration", &f.base).unwrap();
        assert_eq!(
            containing_tip(&f.repo, "teams/integration", &member).unwrap(),
            None
        );
        let Merge::Merged { commit: tip } =
            merge(&f.repo, "teams/integration", &member, "Integrate").unwrap()
        else {
            panic!("fixture merge")
        };
        assert_ne!(tip, member);
        let refs = f.refs();
        assert_eq!(
            containing_tip(&f.repo, "teams/integration", &member).unwrap(),
            Some(tip)
        );
        assert!(containing_tip(&f.repo, "missing", &member).is_err());
        assert!(containing_tip(&f.repo, "teams/integration", "missing").is_err());
        assert_eq!(f.refs(), refs);
    }
    #[cfg(unix)]
    fn with_fixture_git<T>(fixture: &Fixture, mode: &str, run: impl FnOnce() -> T) -> T {
        use std::os::unix::fs::PermissionsExt;
        let path = fixture.root.path().join("fixture-git");
        let branch = match mode {
            "hung-ref" => "if [ \"$arg\" = update-ref ]; then exec sleep 60; fi",
            "ancestry-error" => "if [ \"$arg\" = merge-base ]; then exit 2; fi",
            _ => panic!("unknown Git fixture"),
        };
        let body = format!(
            "#!/bin/sh\nfor arg do\n {branch}\ndone\nexec \"$HGS_FIXTURE_REAL_GIT\" \"$@\"\n"
        );
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        struct Reset(Option<PathBuf>);
        impl Drop for Reset {
            fn drop(&mut self) {
                PROGRAM.with(|p| *p.borrow_mut() = self.0.take());
            }
        }
        let _reset = Reset(PROGRAM.with(|p| p.replace(Some(path))));
        run()
    }
    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn an_ancestry_command_failure_after_both_commits_resolve_is_not_a_negative() {
        let f = Fixture::new();
        let incoming = f.branch("member", "new.txt", "incoming");
        create_branch(&f.repo, "teams/held", &f.base).unwrap();
        let result = with_fixture_git(&f, "ancestry-error", || {
            containing_tip(&f.repo, "teams/held", &incoming)
        });
        assert!(
            result.is_err(),
            "a failed merge-base was treated as not contained: {result:?}"
        );
    }
    #[cfg(unix)]
    struct TimedAdmission<'a>(
        Option<crate::teams::store::actor_reads::Admission>,
        Instant,
        &'a Cell<Option<Duration>>,
    );
    #[cfg(unix)]
    impl Drop for TimedAdmission<'_> {
        fn drop(&mut self) {
            drop(self.0.take());
            self.2.set(Some(self.1.elapsed()));
        }
    }
    #[cfg(unix)]
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn an_admitted_ref_update_releases_the_store_within_its_five_second_budget() {
        let f = Fixture::new();
        let incoming = f.branch("member", "new.txt", "incoming");
        create_branch(&f.repo, "teams/held", &f.base).unwrap();
        let h = crate::teams::ops::tests::Harness::new();
        let state = h.temp.path().to_path_buf();
        let mark = Rc::new(Cell::new(false));
        let seen = mark.clone();
        BEFORE_REF_WRITE.with(|s| {
            *s.borrow_mut() = Some(Box::new(move || {
                let db = rusqlite::Connection::open(state.join("teams/teams-v1.sqlite3")).unwrap();
                db.busy_timeout(Duration::ZERO).unwrap();
                assert!(db.execute_batch("BEGIN IMMEDIATE").is_err());
                seen.set(true);
            }))
        });
        let held = Cell::new(None);
        let result = with_fixture_git(&f, "hung-ref", || {
            merge_admitted(&f.repo, "teams/held", &incoming, "Admitted fixture", || {
                let guard = crate::teams::store::Store::open(h.temp.path())?.admission()?;
                Ok(TimedAdmission(Some(guard), Instant::now(), &held))
            })
        });
        let elapsed = held.get().expect("the admission was taken and released");
        assert!(mark.get());
        assert_eq!(result.unwrap_err().code, code::DELIVERY_UNCERTAIN);
        let db = rusqlite::Connection::open(h.temp.path().join("teams/teams-v1.sqlite3")).unwrap();
        db.busy_timeout(Duration::ZERO).unwrap();
        db.execute_batch("BEGIN IMMEDIATE;ROLLBACK").unwrap();
        assert_eq!(
            git(&f.main, &["rev-parse", "refs/heads/teams/held"]),
            f.base
        );
        assert!(
            elapsed < Duration::from_secs(8),
            "the store remained held for {elapsed:?}"
        );
    }

    /// Git's error output quotes what it was given: a name with terminal
    /// control sequences and a line break reaches the message as one clean,
    /// bounded line (zerus-63rru.59).
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn gits_own_words_reach_a_failure_as_one_clean_line() {
        let f = Fixture::new();
        let git = Git::repo(&f.repo).unwrap();
        let hostile = format!("nope\u{1b}[31m\u{7}\nforged line{}", "x".repeat(3000));
        let output = git.run(&["cat-file", "-t", &hostile]).unwrap();
        assert!(!output.status.success());
        // Git turns the name's escape and bell into "?" but keeps its line
        // break: the message would carry a second line.
        let raw = String::from_utf8_lossy(&output.stderr);
        assert!(raw.contains("\nforged line"), "{raw:?}");
        let message = success(output).unwrap_err().message;
        assert!(
            message.starts_with("Git refused integration: "),
            "{message}"
        );
        assert!(!message.chars().any(char::is_control), "{message:?}");
        assert!(message.contains(" forged line"), "{message}");
        assert!(message.len() <= "Git refused integration: ".len() + 1024);
    }

    /// A name with a right-to-left override, as Git echoes it: the override
    /// is gone from the failure's sentence (zerus-63rru.59).
    #[test]
    // WhiteWh fork: upstream's Agent Teams run on Linux and macOS only.
    #[cfg_attr(windows, ignore)]
    fn gits_words_lose_a_bidirectional_override() {
        let f = Fixture::new();
        let git = Git::repo(&f.repo).unwrap();
        let output = git
            .run(&["cat-file", "-t", "invoice_\u{202E}fdp.exe"])
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("\u{202E}"));
        let message = success(output).unwrap_err().message;
        assert!(!message.contains("\u{202E}"), "{message:?}");
        assert!(message.contains("invoice_fdp.exe"), "{message}");
    }
}
