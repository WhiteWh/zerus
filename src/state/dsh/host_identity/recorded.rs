//! The DeepSeek hosts this computer records, and whether one is an ancestor
//! of a command (zerus-ppd26.15). One resident host serves every session of
//! its agent home, so process ancestry through it proves no session: a
//! command under a recorded host can be only that host's own hosted session,
//! never another agent's session and never the person. This is the one
//! check; the send and task proofs, question routing, `hgs self`, desktop
//! mode registration and Agent Teams' caller proof all call it.
//!
//! The reader is Agent Teams' (`caller_hosts::deepseek`), moved here beside
//! the writer with the same limits and record rules. DeepSeek hosts are this
//! computer's own records: one folder per agent home under `dsh/hosts`, each
//! naming the host's process ID and its birth as `platform::process_identity`
//! reads it. A state folder, `dsh` or `hosts` that does not exist records no
//! host. Only directories are host folders, and a plain file beside them,
//! such as `.DS_Store`, is ignored. Any other entry refuses like an unreadable
//! record: a link in place of a host folder may lead to a live host. More
//! than [`HOST_FOLDERS`] directories, or more than [`HOST_ENTRIES`] entries of
//! any kind, refuse with a sentence of their own, since these folders are
//! kept per agent home and nothing removes them. A folder or record that
//! exists but cannot be read, or is malformed, refuses, and the refusal names
//! that path and how to clear it.
//!
//! A running record names its host with its birth. A starting record names
//! the previous host with its birth and a PID file without one; its launcher
//! is the `hgs` that started the attempt, not a host. An older `host.pid`
//! names a process ID without a birth, unless the record publishes that same
//! ID. A record that carries the identity of another boot, in the very form
//! the writer stamps, names no process of this boot; this boot is read by the
//! writer's own function, so the two cannot drift apart, and a record whose
//! boot is in any other form, or a boot that cannot be read, sets nothing
//! aside. A host spawned by an attempt that ended unconfirmed, before its
//! running record, is named nowhere: the stated limit. The writer publishes
//! every record atomically (a synced temporary file renamed into place, then
//! the folder synced), so a crash leaves the previous record or the new one.
//!
//! The walk is Agent Teams' ancestry rule without its environment reads: from
//! the command up through processes of the same effective user, at most
//! [`ANCESTRY_STEPS`], to the first process of another user, which is the
//! top and is not inspected further. A parent of zero, a loop, a process that
//! cannot be read or changes while it is read, a missing birth anywhere on
//! the chain (the top included) and a chain that reaches the bound without
//! the top refuse, each as its own cause. A recorded host matches an ancestor
//! by process ID and birth; a birth that differs is not that host, and a
//! record without a birth whose ID is an ancestor's cannot be told apart and
//! refuses. The chain is read
//! again after the records, so a negative answer holds for the chain as it
//! is. A command that leaves its host's tree (a double fork, setsid, a
//! background job whose shell exits) is adopted by the user's service
//! manager and is outside this check: the reparenting limit.
use crate::state::repositories::directory::Directory;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// At most this many DeepSeek host folders, one per agent home.
pub(crate) const HOST_FOLDERS: usize = 64;
/// At most this many entries of any kind are listed in `dsh/hosts`, so that
/// stray files cannot make the listing unbounded.
pub(crate) const HOST_ENTRIES: usize = 1024;
/// At most this many processes are walked from a command to the top.
pub(crate) const ANCESTRY_STEPS: usize = 64;

/// A process that a host record names, with the birth it recorded, or none
/// when the record holds only a process ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Named {
    pub(crate) pid: u32,
    pub(crate) birth: Option<String>,
}

/// A named process and the record file that names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Recorded {
    pub(crate) named: Named,
    pub(crate) record: PathBuf,
}

/// The recorded DeepSeek host that is an ancestor: its process ID and the
/// birth both the record and the walk read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Host {
    pub(crate) pid: u32,
    pub(crate) birth: String,
}

/// Why it cannot be told whether a recorded DeepSeek host is above a command.
/// Every cause refuses the command as an agent session and as the person,
/// and each is told apart: the person reads which, and for a record its path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// More host folders than [`HOST_FOLDERS`] in this `dsh/hosts` folder.
    TooManyFolders(PathBuf),
    /// More entries of any kind than [`HOST_ENTRIES`] in this folder.
    TooManyEntries(PathBuf),
    /// A folder or record that exists cannot be read, is malformed, or is
    /// neither a folder nor a plain file.
    UnreadableRecord(PathBuf),
    /// A record without a birth names an ancestor's process ID.
    BirthlessRecord(PathBuf),
    /// The walk reached [`ANCESTRY_STEPS`] processes without reaching the top.
    AncestryBound,
    /// This process on the chain has no readable birth.
    UnreadableBirth(u32),
    /// A process on the chain cannot be read or changed while it was read, or
    /// its parent is zero, itself or already on the chain.
    UnreadableAncestry,
}

impl Refusal {
    /// What the person reads: the cause, the exact path and how to clear it.
    pub(crate) fn sentence(&self) -> String {
        const NO_PROOF: &str =
            "so no command on this computer can be proven as an agent session or as the person";
        const THIS: &str =
            "until it can, this command cannot be proven as an agent session or as the person";
        match self {
            Refusal::TooManyFolders(path) => format!(
                "This computer records more DeepSeek agent host folders than the check reads ({HOST_FOLDERS}, one per agent home) in {}, {NO_PROOF}. Remove the folders of agent homes that are no longer used from {}.",
                path.display(),
                path.display()
            ),
            Refusal::TooManyEntries(path) => format!(
                "The DeepSeek agent host records in {} hold more than {HOST_ENTRIES} entries, most of them stray files beside the host folders, {NO_PROOF}. Remove the stray files from {}.",
                path.display(),
                path.display()
            ),
            Refusal::UnreadableRecord(path) => format!(
                "Cannot verify the DeepSeek agent host record {}: it cannot be read or is malformed, {NO_PROOF}. Close the DeepSeek sessions of that agent home, then remove {}.",
                path.display(),
                path.display()
            ),
            Refusal::BirthlessRecord(path) => format!(
                "The DeepSeek agent host record {} names a process above this command without its birth (a host start that has not finished, or an older host.pid), so it cannot be told apart from that host, {NO_PROOF}. Retry after the DeepSeek launch finishes; if it never does, close the DeepSeek sessions of that agent home and remove {}.",
                path.display(),
                path.display()
            ),
            Refusal::AncestryBound => format!(
                "This command runs below more than {ANCESTRY_STEPS} processes of this user, more than the check for a DeepSeek agent host walks; {THIS}. Run it from a shorter process chain, such as a new terminal."
            ),
            Refusal::UnreadableBirth(pid) => format!(
                "Cannot read when process {pid}, between this command and the top of this user's processes, started, so it cannot be told apart from a recorded DeepSeek agent host; {THIS}."
            ),
            Refusal::UnreadableAncestry => format!(
                "Cannot complete the check for a DeepSeek agent host above this command: a process between it and the top of this user's processes cannot be read or changed while it was read; {THIS}."
            ),
        }
    }
}

/// The refusal for a command under a recorded DeepSeek host.
pub(crate) const UNDER_HOST: &str = "This command runs under a DeepSeek agent host, so it can act only as that host's own hosted session, never as another agent's session or as the person.";

/// The recorded DeepSeek host that is an ancestor of this process, `None`
/// only when the whole walk and every record were read and none is.
pub(crate) fn hosted_ancestor() -> Result<Option<Host>, Refusal> {
    let chain = walk(std::process::id(), inspect)?;
    let hosts = recorded_hosts()?;
    let found = hosted(&chain, &hosts)?;
    if found.is_none() {
        unchanged(&chain, inspect)?;
    }
    Ok(found)
}

/// The recorded DeepSeek hosts of this computer, read within their bounds.
pub(crate) fn recorded_hosts() -> Result<Vec<Recorded>, Refusal> {
    recorded_hosts_at(
        &crate::state::storage::root(),
        super::boot().ok().as_deref(),
    )
}

fn unreadable(path: &Path) -> Refusal {
    Refusal::UnreadableRecord(path.to_owned())
}

/// [`recorded_hosts`] under the state folder `state`, with this boot `boot`.
pub(super) fn recorded_hosts_at(
    state: &Path,
    boot: Option<&str>,
) -> Result<Vec<Recorded>, Refusal> {
    let state_root = match std::fs::canonicalize(state) {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(unreadable(state)),
    };
    let root = Directory::open(&state_root).map_err(|_| unreadable(&state_root))?;
    let Some(dsh) = optional_child(&root, "dsh").map_err(unreadable_at(&state_root, "dsh"))? else {
        return Ok(Vec::new());
    };
    let Some(hosts) = optional_child(&dsh, "hosts").map_err(unreadable_at(&dsh.path, "hosts"))?
    else {
        return Ok(Vec::new());
    };
    let mut folders = Vec::new();
    for (count, entry) in std::fs::read_dir(&hosts.path)
        .map_err(|_| unreadable(&hosts.path))?
        .enumerate()
    {
        if count == HOST_ENTRIES {
            return Err(Refusal::TooManyEntries(hosts.path.clone()));
        }
        let entry = entry.map_err(|_| unreadable(&hosts.path))?;
        let kind = entry.file_type().map_err(|_| unreadable(&entry.path()))?;
        if kind.is_file() {
            continue;
        }
        if !kind.is_dir() {
            return Err(unreadable(&entry.path()));
        }
        if folders.len() == HOST_FOLDERS {
            return Err(Refusal::TooManyFolders(hosts.path.clone()));
        }
        let name = entry.file_name();
        folders.push(name.into_string().map_err(|_| unreadable(&entry.path()))?);
    }
    hosts.verify().map_err(|_| unreadable(&hosts.path))?;
    let mut named = Vec::new();
    for folder in folders {
        let path = hosts.path.join(&folder);
        let folder = hosts.child(&folder).map_err(|_| unreadable(&path))?;
        let found = host(&folder, boot).map_err(|path| unreadable(&path))?;
        named.extend(
            found
                .into_iter()
                .map(|(named, record)| Recorded { named, record }),
        );
    }
    hosts.verify().map_err(|_| unreadable(&hosts.path))?;
    Ok(named)
}

fn unreadable_at<'a, E>(parent: &'a Path, name: &'a str) -> impl FnOnce(E) -> Refusal + 'a {
    move |_| unreadable(&parent.join(name))
}

/// A child folder of a pinned folder, or none when it is absent.
fn optional_child(root: &Directory, name: &str) -> std::result::Result<Option<Directory>, ()> {
    root.verify().map_err(|_| ())?;
    let child = match std::fs::symlink_metadata(root.path.join(name)) {
        Ok(_) => Some(root.child(name).map_err(|_| ())?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(()),
    };
    root.verify().map_err(|_| ())?;
    Ok(child)
}

/// Whether a record's boot is in the one form the writer stamps: a canonical,
/// lowercase, hyphenated UUID that is not nil. Any other form is an unknown
/// boot, and the record stays named.
fn written_form(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
}

/// This boot's identity as the writer reads it, for the tests' records.
#[cfg(test)]
pub(crate) fn written_boot() -> Option<String> {
    super::boot().ok()
}

/// What one host folder names, for the tests of the record rules.
#[cfg(test)]
pub(crate) fn named_in(
    folder: &Path,
    boot: Option<&str>,
) -> std::result::Result<Vec<Named>, PathBuf> {
    let folder = std::fs::canonicalize(folder).map_err(|_| folder.to_owned())?;
    let pinned = Directory::open(&folder).map_err(|_| folder.clone())?;
    Ok(host(&pinned, boot)?
        .into_iter()
        .map(|(named, _)| named)
        .collect())
}

#[cfg(test)]
thread_local! { static READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
/// How many record files this thread has read, for the bound tests.
#[cfg(test)]
pub(crate) fn reads() -> usize {
    READS.with(|n| n.get())
}

/// What one host folder names, each with the file that names it: a running
/// host with its birth; a starting record's previous host with its birth and
/// its PID file without one; an older `host.pid` without one, unless the
/// record publishes that same ID (the running host's, or the starting
/// record's PID file). A record that carries another boot's identity than
/// `boot` names nothing, and so neither does the `host.pid` it publishes. An
/// error names the file or folder that could not be read.
fn host(
    folder: &Directory,
    boot: Option<&str>,
) -> std::result::Result<Vec<(Named, PathBuf)>, PathBuf> {
    let identity_path = folder.path.join("host.identity.json");
    let pid_path = folder.path.join("host.pid");
    let other_boot = |record: &Value| {
        boot.is_some_and(|now| {
            record["boot"]
                .as_str()
                .is_some_and(|then| written_form(then) && then != now)
        })
    };
    let identity = small(folder, "host.identity.json", 1024)?;
    let legacy = small(folder, "host.pid", 32)?
        .map(|bytes| {
            std::str::from_utf8(&bytes)
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok())
                .filter(|pid| valid(*pid))
                .ok_or_else(|| pid_path.clone())
        })
        .transpose()?;
    let mut named = Vec::new();
    let mut published = None;
    if let Some(bytes) = identity {
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| identity_path.clone())?;
        match value["state"].as_str() {
            Some("running") => {
                let host = recorded(&value).ok_or_else(|| identity_path.clone())?;
                published = Some(host.pid);
                if !other_boot(&value) {
                    named.push((host, identity_path.clone()));
                }
            }
            Some("starting") => {
                let previous = (!value["previous"].is_null())
                    .then(|| recorded(&value["previous"]).ok_or_else(|| identity_path.clone()))
                    .transpose()?;
                let pid_file = (!value["pid_file"].is_null())
                    .then(|| {
                        value["pid_file"][0]
                            .as_u64()
                            .and_then(|pid| u32::try_from(pid).ok())
                            .filter(|pid| valid(*pid))
                            .ok_or_else(|| identity_path.clone())
                    })
                    .transpose()?;
                published = pid_file;
                if !other_boot(&value) {
                    if let Some(previous) = previous.filter(|_| !other_boot(&value["previous"])) {
                        named.push((previous, identity_path.clone()));
                    }
                    if let Some(pid) = pid_file {
                        named.push((Named { pid, birth: None }, identity_path.clone()));
                    }
                }
            }
            _ => return Err(identity_path),
        }
    }
    if let Some(pid) = legacy.filter(|pid| Some(*pid) != published) {
        named.push((Named { pid, birth: None }, pid_path));
    }
    Ok(named)
}

fn recorded(value: &Value) -> Option<Named> {
    let pid = value["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| valid(*pid))?;
    let birth = value["birth"]
        .as_str()
        .filter(|birth| !birth.is_empty() && birth.len() <= 64)?;
    Some(Named {
        pid,
        birth: Some(birth.to_owned()),
    })
}

fn valid(pid: u32) -> bool {
    pid > 0 && pid <= i32::MAX as u32
}

/// A small regular file of a pinned folder, or none when it is absent. An
/// error names the file, or the folder when the folder itself changed.
fn small(
    folder: &Directory,
    name: &str,
    limit: usize,
) -> std::result::Result<Option<Vec<u8>>, PathBuf> {
    folder.verify().map_err(|_| folder.path.clone())?;
    let path = folder.path.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(path),
    }
    let bytes = folder.read_file(name, limit).map_err(|_| path.clone())?;
    #[cfg(test)]
    READS.with(|n| n.set(n.get() + 1));
    folder.verify().map_err(|_| folder.path.clone())?;
    Ok(Some(bytes))
}

/// One process as the walk reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Process {
    pub(super) parent: u32,
    pub(super) birth: Option<String>,
}

/// A process of the same effective user, or the first one of another user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Inspection {
    Own(Process),
    Foreign,
}

/// One same-user process of the walk; the last one is the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Ancestor {
    pub(super) pid: u32,
    pub(super) identity: Process,
    pub(super) top: bool,
}

/// The same-user ancestry from `pid` to the top, by Agent Teams' rule, with
/// a birth read for every process on it, the top included. A chain that
/// reaches the bound without the top is a refusal, never a top.
pub(super) fn walk(
    mut pid: u32,
    mut inspect: impl FnMut(u32) -> Result<Inspection, ()>,
) -> Result<Vec<Ancestor>, Refusal> {
    let mut read = |pid| inspect(pid).map_err(|()| Refusal::UnreadableAncestry);
    let mut chain: Vec<Ancestor> = Vec::new();
    let mut current = read(pid)?;
    for _ in 0..ANCESTRY_STEPS {
        let Inspection::Own(identity) = current else {
            return Err(Refusal::UnreadableAncestry);
        };
        let parent = identity.parent;
        if chain.iter().any(|row| row.pid == pid) || parent == pid || parent == 0 {
            return Err(Refusal::UnreadableAncestry);
        }
        if identity.birth.is_none() {
            return Err(Refusal::UnreadableBirth(pid));
        }
        let next = read(parent)?;
        let top = matches!(next, Inspection::Foreign);
        if !matches!(read(pid)?, Inspection::Own(ref now) if *now == identity) {
            return Err(Refusal::UnreadableAncestry);
        }
        chain.push(Ancestor { pid, identity, top });
        if top {
            return Ok(chain);
        }
        pid = parent;
        current = next;
    }
    Err(Refusal::AncestryBound)
}

/// The chain as it was walked: each process unchanged, and the top's parent
/// still another user's.
pub(super) fn unchanged(
    chain: &[Ancestor],
    mut inspect: impl FnMut(u32) -> Result<Inspection, ()>,
) -> Result<(), Refusal> {
    let mut read = |pid| inspect(pid).map_err(|()| Refusal::UnreadableAncestry);
    for ancestor in chain {
        if !matches!(read(ancestor.pid)?, Inspection::Own(ref now) if *now == ancestor.identity) {
            return Err(Refusal::UnreadableAncestry);
        }
        if ancestor.top && !matches!(read(ancestor.identity.parent)?, Inspection::Foreign) {
            return Err(Refusal::UnreadableAncestry);
        }
    }
    Ok(())
}

/// The recorded host that is one of these ancestors, matched by process ID
/// and birth. A birth that differs is not that host: the record's birth is
/// the walk's own `process_identity` value, so the recorded host itself
/// always matches, and a different birth means it has exited and its ID was
/// reused. A record without a birth whose ID is an ancestor's cannot be told
/// apart and refuses; so does an ancestor without a readable birth, which is
/// never read as a mismatch.
pub(super) fn hosted(chain: &[Ancestor], hosts: &[Recorded]) -> Result<Option<Host>, Refusal> {
    for ancestor in chain {
        let Some(birth) = &ancestor.identity.birth else {
            return Err(Refusal::UnreadableBirth(ancestor.pid));
        };
        for host in hosts.iter().filter(|host| host.named.pid == ancestor.pid) {
            match &host.named.birth {
                Some(recorded) if recorded == birth => {
                    return Ok(Some(Host {
                        pid: ancestor.pid,
                        birth: birth.clone(),
                    }))
                }
                Some(_) => (),
                None => return Err(Refusal::BirthlessRecord(host.record.clone())),
            }
        }
    }
    Ok(None)
}

#[cfg(target_os = "linux")]
pub(super) fn inspect(pid: u32) -> Result<Inspection, ()> {
    let status = bounded(&format!("/proc/{pid}/status"))?;
    if !effective_owner(&status, unsafe { libc::geteuid() })? {
        return Ok(Inspection::Foreign);
    }
    let stat = bounded(&format!("/proc/{pid}/stat"))?;
    let (_, tail) = stat.rsplit_once(')').ok_or(())?;
    let fields: Vec<_> = tail.split_whitespace().collect();
    if fields.len() < 20 || matches!(fields[0], "Z" | "X") {
        return Err(());
    }
    let parent = fields[1].parse().map_err(|_| ())?;
    let native = crate::platform::process_identity(pid);
    if native
        .as_ref()
        .is_some_and(|native| native.parent != parent)
    {
        return Err(());
    }
    Ok(Inspection::Own(Process {
        parent,
        birth: native.map(|native| native.birth),
    }))
}

#[cfg(target_os = "linux")]
fn effective_owner(status: &str, owner: u32) -> Result<bool, ()> {
    let uids = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .ok_or(())?
        .split_whitespace()
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ())?;
    if uids.len() != 4 {
        return Err(());
    }
    Ok(uids[1] == owner)
}

#[cfg(target_os = "linux")]
fn bounded(path: &str) -> Result<String, ()> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| ())?
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() > 65536 {
        return Err(());
    }
    String::from_utf8(bytes).map_err(|_| ())
}

#[cfg(target_os = "macos")]
pub(super) fn inspect(pid: u32) -> Result<Inspection, ()> {
    let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let count = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdshortinfo).cast(),
            size,
        )
    };
    if count != size || info.pbsi_pid != pid {
        return Err(());
    }
    if info.pbsi_uid != unsafe { libc::geteuid() } {
        return Ok(Inspection::Foreign);
    }
    if info.pbsi_status == 5 {
        return Err(());
    }
    let native = crate::platform::process_identity(pid);
    if native
        .as_ref()
        .is_some_and(|native| native.parent != info.pbsi_ppid)
    {
        return Err(());
    }
    Ok(Inspection::Own(Process {
        parent: info.pbsi_ppid,
        birth: native.map(|native| native.birth),
    }))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn inspect(_pid: u32) -> Result<Inspection, ()> {
    Err(())
}

#[cfg(test)]
mod tests;
