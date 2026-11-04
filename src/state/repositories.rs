//! Explicit acquisition of a repository on this computer. Receipts reserve a
//! request before mkdir/Git; a repeated or uncertain request never runs again.
use super::*;
use sha2::{Digest, Sha256};
use std::process::Command;
use std::time::{Duration, Instant};

pub(super) mod directory;
#[cfg(unix)]
mod worker;
#[cfg(unix)]
pub(super) fn git_worker(args: &[String]) -> Result<i32> {
    worker::dispatch(args)
}

const MAX_RECEIPT_BYTES: u64 = 16_384;

/// New external steps are separately checked by the admitted desktop actor.
/// Writing an original result after cancellation records facts; it starts no work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Reserve,
    Create,
    Clone,
    Verify,
}
impl Step {
    #[cfg(unix)]
    pub(crate) fn ordinal(self) -> usize {
        match self {
            Self::Reserve => 0,
            Self::Create => 1,
            Self::Clone => 2,
            Self::Verify => 3,
        }
    }
}

/// The service supplies its actual per-step actor check and original custody.
/// This is not constructed from CLI arguments or imported recovery records.
pub(crate) struct Control<'a> {
    pub before: &'a dyn Fn(Step) -> Result<()>,
    #[cfg(unix)]
    pub cancelled: &'a std::sync::atomic::AtomicBool,
    #[cfg(unix)]
    pub custody: &'a std::sync::Arc<zerus_client_core::run::custody::InheritedLock>,
}
fn before(control: Option<&Control<'_>>, step: Step) -> Result<()> {
    if let Some(control) = control {
        (control.before)(step)?;
    }
    Ok(())
}

const USAGE: &str = "usage: hgs repository check|clone --url URL --parent PATH --name NAME [--request-id UUID] --json; hgs repository status --request-id UUID --json";

fn valid_url(url: &str) -> bool {
    // A single drive letter is ambiguous with SCP and Git for Windows treats
    // it as local. Such SSH aliases must use an explicit ssh:// URL.
    let drive = url
        .as_bytes()
        .get(0..2)
        .is_some_and(|b| b[0].is_ascii_alphabetic() && b[1] == b':');
    if drive
        || url.contains('\\')
        || url.starts_with("//")
        || url.is_empty()
        || url.len() > 4096
        || url.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return false;
    }
    if let Some(rest) = url.strip_prefix("https://") {
        let host = rest.split('/').next().unwrap_or("");
        return !host.is_empty()
            && !host.contains('@')
            && !rest.contains(['?', '#'])
            && rest.contains('/');
    }
    if let Some(rest) = url.strip_prefix("ssh://") {
        let host = rest.split('/').next().unwrap_or("");
        return !host.is_empty()
            && !host.rsplit('@').next().unwrap_or("").is_empty()
            && !host
                .split_once('@')
                .is_some_and(|(user, _)| user.contains(':'))
            && !rest.contains(['?', '#'])
            && rest.contains('/');
    }
    // SCP syntax, including configured SSH host aliases. Reject local paths and
    // Git remote helpers: only the reviewed SSH and HTTPS transports are offered.
    if url.contains("://") || url.starts_with('-') || url.contains("::") {
        return false;
    }
    url.split_once(':').is_some_and(|(host, path)| {
        !host.is_empty() && !host.contains(['/', '\\']) && !path.is_empty()
    })
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 240
        || name != name.trim()
        || matches!(name, "." | "..")
        || name
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
        || name.ends_with('.')
    {
        return Err(
            "Choose a single new folder name without separators or control characters".into(),
        );
    }
    // Win32 resolves device stems even with extensions and ASCII spaces before
    // the first dot. Keep this portable, like the forbidden leaf characters.
    // COM/LPT also reserve the superscript digits documented by Microsoft:
    // https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    let numbered = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|digit| {
            matches!(
                digit,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        });
    if numbered
        || matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        )
    {
        return Err("Choose a folder name that is not a reserved Windows device name".into());
    }
    Ok(())
}

fn validate(url: &str, parent: &str, name: &str) -> Result<PathBuf> {
    if !valid_url(url) {
        return Err("Use an SSH or HTTPS repository address without embedded passwords, tokens, query or fragment".into());
    }
    validate_name(name)?;
    if parent.is_empty() || parent.len() > 4096 || parent.chars().any(char::is_control) {
        return Err("Choose an existing parent folder".into());
    }
    let path = if parent == "~" {
        home()
    } else if let Some(rest) = parent.strip_prefix("~/") {
        if Path::new(rest).has_root() {
            return Err("Invalid home-relative parent folder".into());
        }
        home().join(rest)
    } else {
        PathBuf::from(parent)
    };
    if !path.is_absolute() {
        return Err("Parent folder must be absolute or home-relative".into());
    }
    let path = crate::platform::canonical(path)
        .map_err(|e| format!("Parent folder is unavailable: {e}"))?;
    if !path.is_dir() {
        return Err("Parent is not a folder".into());
    }
    Ok(path)
}

fn git(
    directory: &directory::Directory,
    args: &[&str],
    seconds: u64,
    control: Option<&Control<'_>>,
) -> Result<Vec<u8>> {
    directory.verify()?;
    #[cfg(unix)]
    let result = worker::run(directory, args, seconds, control);
    #[cfg(windows)]
    let result = {
        let _ = control; // The first admitted service clone leaf is POSIX-only.
        run_git(Some(directory), args, seconds, &|| false)
    };
    directory.verify()?;
    result
}

fn run_git(
    directory: Option<&directory::Directory>,
    args: &[&str],
    seconds: u64,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<u8>> {
    #[cfg(unix)]
    let program = {
        let found = crate::platform::which("git").ok_or("Cannot find Git")?;
        let path = crate::platform::canonical(&found).map_err(|e| e.to_string())?;
        if crate::platform::file_identity(&path)
            .zip(crate::platform::image_identity())
            .is_some_and(|(git, running)| git == running)
        {
            return Err("Git resolves to the running hgs image".into());
        }
        found
    };
    #[cfg(windows)]
    let program = "git";
    let mut cmd = Command::new(program);
    #[cfg(unix)]
    cmd.env(worker::CONTEXT, "1");
    if let Some(directory) = directory {
        directory.anchor(&mut cmd)?;
    }
    cmd.args(["--no-pager", "-c", "core.fsmonitor=false"])
        .args(args)
        .env("LC_ALL", "C");
    for key in [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
    ] {
        cmd.env_remove(key);
    }
    crate::platform::git_noninteractive(&mut cmd);
    let (status, out, err) = crate::platform::run_bounded(
        &mut cmd,
        [65536, 65536],
        Instant::now() + Duration::from_secs(seconds),
        cancelled,
    )
    .map_err(|e| match e {
        crate::platform::RunError::Spawn(e) => format!("Cannot start Git: {e}"),
        crate::platform::RunError::Io(e) => format!("Cannot read Git result: {e}"),
        crate::platform::RunError::TooLarge => {
            "Git output exceeded the limit; inspect the destination before continuing".into()
        }
        _ => "Git timed out; inspect the destination before continuing".into(),
    })?;
    if !status.success() {
        return Err(format!(
            "Git failed: {}",
            String::from_utf8_lossy(&err).trim()
        ));
    }
    Ok(out)
}

fn token(id: &str) -> Result<()> {
    if !uuid::Uuid::parse_str(id).is_ok_and(|u| !u.is_nil() && u.to_string() == id) {
        return Err("--request-id must be a canonical nonempty UUID".into());
    }
    Ok(())
}
fn receipt_path(state: &Path, id: &str) -> PathBuf {
    state.join("repository-clones").join(format!("{id}.json"))
}
pub(crate) fn read_receipt(state: &Path, id: &str) -> Result<Value> {
    use std::io::Read;
    token(id)?;
    let p = receipt_path(state, id);
    let before =
        std::fs::symlink_metadata(&p).map_err(|e| format!("Clone receipt unavailable: {e}"))?;
    if !before.is_file() || before.file_type().is_symlink() || before.len() > MAX_RECEIPT_BYTES {
        return Err("Invalid clone receipt".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    crate::platform::no_follow(&mut options);
    #[cfg(windows)]
    crate::platform::refuse_link(&mut options);
    let file = options
        .open(&p)
        .map_err(|e| format!("Clone receipt unavailable: {e}"))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > MAX_RECEIPT_BYTES {
        return Err("Invalid clone receipt".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES {
        return Err("Invalid clone receipt".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

fn bounded_receipt(value: &Value) -> Result<Value> {
    let mut value = value.clone();
    if value.to_string().len() > MAX_RECEIPT_BYTES as usize {
        if let Some(detail) = value
            .get("detail")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            value["detail"] = json!("");
            let base = value.to_string().len();
            let available = (MAX_RECEIPT_BYTES as usize)
                .checked_sub(base)
                .ok_or("Clone receipt fields exceed the encoded size limit")?;
            let suffix = " [truncated]";
            let budget = available.saturating_sub(suffix.len());
            let mut bounded = String::new();
            let mut used = 0;
            for c in detail.chars() {
                let size = serde_json::to_string(&c.to_string()).unwrap().len() - 2;
                if used + size > budget {
                    break;
                }
                used += size;
                bounded.push(c);
            }
            if bounded.len() != detail.len() && available >= suffix.len() {
                bounded.push_str(suffix);
            }
            value["detail"] = json!(bounded);
        }
    }
    let limit = if value["status"] == "unconfirmed" {
        MAX_RECEIPT_BYTES - 64
    } else {
        MAX_RECEIPT_BYTES
    };
    if value.to_string().len() as u64 > limit {
        return Err("Clone receipt fields exceed the encoded size limit".into());
    }
    Ok(value)
}

fn write_receipt(state: &Path, id: &str, value: &Value) -> Result<Value> {
    let value = bounded_receipt(value)?;
    atomic(&receipt_path(state, id), &value.to_string())?;
    Ok(value)
}

pub(super) fn dispatch(args: &[String]) -> Result<i32> {
    println!("{}", execute(args, &root(), None)?);
    Ok(0)
}

#[cfg(unix)]
pub(crate) fn clone_owned(
    state: &Path,
    id: &str,
    url: &str,
    parent: &str,
    name: &str,
    control: &Control<'_>,
) -> Result<Value> {
    execute(
        &[
            "clone",
            "--request-id",
            id,
            "--url",
            url,
            "--parent",
            parent,
            "--name",
            name,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>(),
        state,
        Some(control),
    )
}

fn execute(args: &[String], state: &Path, control: Option<&Control<'_>>) -> Result<Value> {
    let action = args.first().map(String::as_str).ok_or(USAGE)?;
    let mut options = std::collections::HashMap::new();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--json" {
            i += 1;
            continue;
        }
        if !["--url", "--parent", "--name", "--request-id"].contains(&args[i].as_str())
            || i + 1 >= args.len()
            || options
                .insert(args[i].as_str(), args[i + 1].as_str())
                .is_some()
        {
            return Err(USAGE.into());
        }
        i += 2;
    }
    let get = |key| options.get(key).copied().ok_or_else(|| USAGE.to_string());
    if action == "status" {
        if options.len() != 1 {
            return Err(USAGE.into());
        }
        let id = get("--request-id")?;
        token(id)?;
        return read_receipt(state, id);
    }
    if !matches!(action, "check" | "clone")
        || options.len() != if action == "clone" { 4 } else { 3 }
    {
        return Err(USAGE.into());
    }
    let (url, parent, name) = (get("--url")?, get("--parent")?, get("--name")?);
    // Reject unusable destination names before creating a receipt directory,
    // request lock or checkout. Status reads of older receipts stay available.
    validate_name(name)?;
    let fingerprint = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&json!([url, parent, name])).unwrap())
    );
    let id = if action == "clone" {
        let id = get("--request-id")?;
        token(id)?;
        id
    } else {
        ""
    };
    // A per-request lock prevents duplicate clones without serializing unrelated
    // repositories. Status never waits on a long-running clone.
    let _guard = if action == "clone" {
        before(control, Step::Reserve)?;
        let lock_path = receipt_path(state, id).with_extension("lock");
        private_dir(lock_path.parent().ok_or("Invalid receipt directory")?)?;
        let file = lock_file(&lock_path)?;
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => Some(crate::platform::FileLock::held(file)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(
                    "The original clone is running; read its receipt with repository status".into(),
                )
            }
            Err(e) => return Err(e.to_string()),
        }
    } else {
        None
    };
    if action == "clone"
        && receipt_path(state, id)
            .try_exists()
            .map_err(|e| e.to_string())?
    {
        let saved = read_receipt(state, id)?;
        if saved["fingerprint"] != fingerprint {
            return Err("Request ID already belongs to a different clone".into());
        }
        return Ok(saved);
    }
    let canonical = validate(url, parent, name)?;
    let destination = canonical.join(name);
    let parent_directory = directory::Directory::open(&canonical)?;
    if std::fs::symlink_metadata(&destination).is_ok() {
        return Err(
            "Destination already exists. Choose another name or use the existing folder".into(),
        );
    }
    if action == "check" {
        git(&parent_directory, &["--version"], 5, control)?;
        git(
            &parent_directory,
            &["ls-remote", "--symref", "--", url, "HEAD"],
            20,
            control,
        )?;
        return Ok(json!({"status":"ready","parent":canonical,"path":destination,"name":name}));
    }
    let mut receipt = json!({"status":"unconfirmed","request_id":id,"fingerprint":fingerprint,
        "parent":canonical,"path":destination,"name":name});
    // The fallback location is durable before even a staging directory exists.
    let staging = format!(".zerus-clone-{}", uuid::Uuid::new_v4());
    #[cfg(unix)]
    {
        receipt["staging_path"] = json!(canonical.join(&staging));
    }
    write_receipt(state, id, &receipt)?;
    let result: Result<()> = (|| {
        before(control, Step::Create)?;
        let owned = parent_directory.create(name, &staging)?;
        before(control, Step::Clone)?;
        git(&owned, &["clone", "--quiet", "--", url, "."], 600, control)?;
        parent_directory.verify()?;
        owned.verify()?;
        before(control, Step::Verify)?;
        let top = git(&owned, &["rev-parse", "--show-toplevel"], 5, control)?;
        let top = crate::platform::canonical(String::from_utf8_lossy(&top).trim())
            .map_err(|e| e.to_string())?;
        if top != destination {
            return Err("Git did not confirm the requested checkout".into());
        }
        parent_directory.verify()?;
        owned.verify()?;
        Ok(())
    })();
    match result {
        Ok(()) => receipt["status"] = json!("cloned"),
        Err(error) => {
            receipt["status"] = json!("failed");
            receipt["detail"] = json!(error);
        }
    }
    write_receipt(state, id, &receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::fs;

    #[cfg(unix)]
    fn owned_fixture() -> (
        PathBuf,
        std::sync::Arc<zerus_client_core::run::custody::InheritedLock>,
    ) {
        let folder = std::env::temp_dir().join(format!("hgs-owned-clone-{}", uuid::Uuid::new_v4()));
        private_dir(&folder).unwrap();
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(folder.join("custody.lock"))
            .unwrap();
        let custody = zerus_client_core::run::custody::InheritedLock::try_lock(file).unwrap();
        (folder, custody)
    }

    #[test]
    #[cfg(unix)]
    fn actor_refusal_precedes_clone_receipt_or_directory_creation() {
        let (folder, custody) = owned_fixture();
        let state = folder.join("selected-state");
        let calls = std::sync::Mutex::new(Vec::new());
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let check = |step| {
            calls.lock().unwrap().push(step);
            Err("creation_lease_revoked".into())
        };
        let error = clone_owned(
            &state,
            "11111111-1111-4111-8111-111111111111",
            "https://example.invalid/project.git",
            folder.to_str().unwrap(),
            "checkout",
            &Control {
                before: &check,
                cancelled: &cancelled,
                custody: &custody,
            },
        )
        .unwrap_err();
        assert_eq!(error, "creation_lease_revoked");
        assert_eq!(*calls.lock().unwrap(), [Step::Reserve]);
        assert!(!state.exists());
        assert!(!folder.join("checkout").exists());
        drop(custody);
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn clone_step_refusal_records_only_the_original_selected_receipt() {
        let (folder, custody) = owned_fixture();
        let state = folder.join("selected-state");
        let calls = std::sync::Mutex::new(Vec::new());
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let check = |step| {
            calls.lock().unwrap().push(step);
            if step == Step::Create {
                Err("creation_lease_revoked".into())
            } else {
                Ok(())
            }
        };
        let id = "22222222-2222-4222-8222-222222222222";
        let receipt = clone_owned(
            &state,
            id,
            "https://example.invalid/project.git",
            folder.to_str().unwrap(),
            "checkout",
            &Control {
                before: &check,
                cancelled: &cancelled,
                custody: &custody,
            },
        )
        .unwrap();
        assert_eq!(receipt["status"], "failed");
        assert_eq!(receipt["detail"], "creation_lease_revoked");
        assert_eq!(*calls.lock().unwrap(), [Step::Reserve, Step::Create]);
        assert!(!folder.join("checkout").exists());
        assert_eq!(read_receipt(&state, id).unwrap(), receipt);
        // Inspection neither creates a new selected state nor repairs receipts.
        let other = folder.join("other-state");
        assert!(read_receipt(&other, id).is_err());
        assert!(!other.exists());
        drop(custody);
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn exact_clone_gate_refusal_never_enters_the_git_worker() {
        let (folder, custody) = owned_fixture();
        let state = folder.join("selected-state");
        let calls = std::sync::Mutex::new(Vec::new());
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let check = |step| {
            calls.lock().unwrap().push(step);
            if step == Step::Clone {
                Err("creation_lease_revoked".into())
            } else {
                Ok(())
            }
        };
        let starts = worker::STARTS.with(std::cell::Cell::get);
        let id = "33333333-3333-4333-8333-333333333333";
        let receipt = clone_owned(
            &state,
            id,
            "https://example.invalid/project.git",
            folder.to_str().unwrap(),
            "checkout",
            &Control {
                before: &check,
                cancelled: &cancelled,
                custody: &custody,
            },
        )
        .unwrap();
        assert_eq!(
            worker::STARTS.with(std::cell::Cell::get),
            starts,
            "a refused Clone must not start even the original Git worker"
        );
        assert_eq!(
            *calls.lock().unwrap(),
            [Step::Reserve, Step::Create, Step::Clone]
        );
        assert_eq!(receipt["status"], "failed");
        assert_eq!(receipt["detail"], "creation_lease_revoked");
        assert!(
            folder.join("checkout").is_dir(),
            "keep the original partial checkout"
        );
        assert!(!folder.join("checkout/.git").exists());
        assert_eq!(read_receipt(&state, id).unwrap(), receipt);
        drop(custody);
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn accepts_only_repository_addresses() {
        for url in [
            "https://example.test/team/repo.git",
            "git@example.test:team/repo.git",
            "ssh://git@example.test/team/repo.git",
            "ssh://example.test:2222/repo.git",
            "work:repo",
            "ssh://c/repo",
        ] {
            assert!(valid_url(url), "{url}");
        }
        for url in [
            "",
            "--upload-pack=bad",
            "ext::bad",
            "/local/repo",
            "file:///repo",
            "C:/source",
            "C:source",
            r"C:\source",
            r"\\server\share\repo",
            r"\\?\C:\repo",
            r"\\.\C:\repo",
            "//server/share/repo",
            "./repo",
            "../repo",
            "https://token@example.test/repo",
            "ssh://user:pass@example.test/repo",
            "https://example.test/repo?token=x",
            "git@example.test:repo\n",
        ] {
            assert!(!valid_url(url), "{url}");
        }
    }
    #[test]
    fn destination_names_refuse_device_stems_without_rejecting_ordinary_unicode() {
        for name in [
            "CON",
            "prn",
            "AuX",
            "NUL",
            "CONIN$",
            "conout$",
            "con.txt",
            "NUL.tar.gz",
            "COM1",
            "COM2",
            "COM3",
            "COM4",
            "COM5",
            "COM6",
            "COM7",
            "COM8",
            "com9.git",
            "LPT1",
            "LPT2",
            "LPT3",
            "LPT4",
            "LPT5",
            "LPT6",
            "LPT7",
            "LPT8",
            "lpt9.txt",
            "CON  .git",
            "COM1 .tar.gz",
            "CONIN$ .txt",
            "CONOUT$.git",
            "COM¹",
            "com².git",
            "COM³.txt",
            "LPT¹",
            "lpt².tar.gz",
            "LpT³  .git",
        ] {
            assert!(validate_name(name).is_err(), "{name}");
        }
        for name in [
            "checkout",
            "console",
            "auxiliary",
            "NUL-copy",
            "COM0",
            "COM10",
            "LPT0",
            "LPT10.git",
            "COM1-copy",
            "COM¹0",
            "LPT³-copy",
            ".CON",
            "something.CON",
            "repo with spaces",
            "Проект",
            "コード",
            "☃",
            "ＣＯＮ",
            "COM₁",
            "COM١",
            "COM⁴",
        ] {
            assert!(validate_name(name).is_ok(), "{name}");
        }
        for name in ["", ".", "..", "a/b", "a\\b", "a:b", "a\n", "a.", " a", "a "] {
            assert!(validate_name(name).is_err(), "{name}");
        }
        assert!(validate_name(&"x".repeat(241)).is_err());
        assert!(validate_name(&"x".repeat(240)).is_ok());
    }
    #[test]
    fn receipt_limit_includes_escaped_detail_and_fields() {
        for name in ["x".repeat(100), "x".repeat(16_200)] {
            let receipt =
                json!({"status":"failed", "name":name, "detail":"\u{1}\\\"☃".repeat(10_000)});
            let bounded = bounded_receipt(&receipt).unwrap();
            assert!(bounded.to_string().len() <= MAX_RECEIPT_BYTES as usize);
            assert!(bounded["detail"]
                .as_str()
                .unwrap()
                .ends_with(" [truncated]"));
            assert_eq!(bounded_receipt(&bounded).unwrap(), bounded);
        }
        assert!(
            bounded_receipt(&json!({"status":"unconfirmed","name":"x".repeat(16_384)})).is_err()
        );
    }
}
