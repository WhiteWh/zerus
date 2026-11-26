//! Full-Zerus host-access runtime (epic zerus-un7n, phase 5): which mode this
//! host runs in and who the caller is.
//!
//! A host is in multi-person mode exactly when its operator directory holds a
//! policy file. The directory and every file Zerus reads from it must belong to
//! the operator (root on Unix; Administrators or SYSTEM on Windows) and must
//! not be writable by the caller. A policy that exists but fails any check puts
//! the host in the broken mode, which refuses every gated route: it never falls
//! back to the legacy single-person behaviour.
//!
//! Full Zerus is reached through SSH as each person's own OS account (the spec's
//! execution context). The caller is therefore derived from the verified OS
//! identity of this process, mapped through the operator's private binding
//! file to a policy context and from there to the person. A `person`, `owner`
//! or `HGS_*` value supplied by the caller is never consulted. An OS identity
//! without a binding keeps its legacy private use of its own state and gains
//! no shared access.
use super::{verify, Policy, PolicyError};
use serde::Deserialize;
use sha2::Digest;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The contract name of the operator's OS binding file. It is full-Zerus host
/// configuration, not part of `zerus-host-access-v1`: the executor has its own
/// isolation backend and never reads it.
pub const BINDINGS_CONTRACT: &str = "zerus-full-host-bindings-v1";
const MAX_BINDINGS_BYTES: u64 = 256 * 1024;
const MAX_BINDINGS: usize = 256;
const MAX_HOST_ID_BYTES: u64 = 128;

/// The operator directory: fixed per platform. On Windows `ProgramData` is read
/// from the environment, which the caller controls; that is safe because every
/// file must still be owned by Administrators or SYSTEM, which a non-admin
/// caller cannot fake. Redirecting it can only turn shared access off.
pub fn operator_dir() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        base.join("Zerus").join("host-access")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/etc/zerus/host-access")
    }
}

/// A verified OS identity: `uid:<n>` on Unix, the user's `S-1-...` SID on
/// Windows.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OsIdentity(pub String);

impl OsIdentity {
    pub fn parse(value: &str) -> Option<Self> {
        let ok = if let Some(uid) = value.strip_prefix("uid:") {
            !uid.is_empty() && uid.len() <= 10 && uid.bytes().all(|b| b.is_ascii_digit()) && uid.parse::<u32>().is_ok()
        } else {
            value.len() <= 184
                && value.starts_with("S-1-")
                && value[4..].split('-').all(|part| !part.is_empty() && part.len() <= 10 && part.bytes().all(|b| b.is_ascii_digit()))
        };
        ok.then(|| Self(value.to_owned()))
    }

    /// This process's own identity.
    pub fn current() -> Option<Self> {
        #[cfg(unix)]
        {
            // SAFETY: geteuid has no preconditions.
            Some(Self(format!("uid:{}", unsafe { libc::geteuid() })))
        }
        #[cfg(windows)]
        {
            crate::platform::user_sid_string().and_then(|sid| Self::parse(&sid))
        }
    }
}

/// Who is calling, once verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    pub person: String,
    pub context: String,
    pub os: OsIdentity,
}

/// The host's mode for this caller.
#[derive(Clone, Debug)]
pub enum Mode {
    /// No operator policy: the existing single-person behaviour.
    Legacy,
    /// Multi-person host; the caller's OS identity has no binding. It keeps the
    /// private use of its own state and has no shared or cross-person access.
    Unbound { policy: Box<Policy>, os: OsIdentity },
    /// Multi-person host and a bound caller.
    Bound { policy: Box<Policy>, principal: Principal },
    /// A policy exists but cannot be trusted or verified: every gated route
    /// is refused with this code.
    Broken(&'static str),
}

impl Mode {
    pub fn multi_person(&self) -> bool {
        !matches!(self, Self::Legacy)
    }
    pub fn policy(&self) -> Option<&Policy> {
        match self {
            Self::Unbound { policy, .. } | Self::Bound { policy, .. } => Some(policy),
            _ => None,
        }
    }
    pub fn principal(&self) -> Option<&Principal> {
        match self {
            Self::Bound { principal, .. } => Some(principal),
            _ => None,
        }
    }
}

/// How the files of an operator directory are checked. Production always uses
/// [`Trust::Operator`]; the explicit test variant exists only in test builds,
/// so no configuration or environment switch can select it.
#[derive(Clone, Copy, Debug)]
pub enum Trust {
    Operator,
    #[cfg(test)]
    TestOwned,
}

/// The mode of this process, from the fixed operator directory.
pub fn current() -> Mode {
    #[cfg(test)]
    if let Some(mode) = TEST_MODE.with(|mode| mode.borrow().clone()) {
        return mode;
    }
    match OsIdentity::current() {
        Some(os) => load(&operator_dir(), &os, Trust::Operator, &crate::state::state_root()),
        // Without a policy the identity is irrelevant; with one, an unreadable
        // identity fails closed.
        None if !operator_dir().join("policy.json").exists() && !seen_any(&crate::state::state_root()) => Mode::Legacy,
        None => Mode::Broken("os-identity"),
    }
}

#[cfg(test)]
thread_local! {
    static TEST_MODE: std::cell::RefCell<Option<Mode>> = const { std::cell::RefCell::new(None) };
}

/// Runs `body` with this thread's mode fixed (tests only).
#[cfg(test)]
pub fn with_mode<T>(mode: Mode, body: impl FnOnce() -> T) -> T {
    let previous = TEST_MODE.with(|slot| slot.replace(Some(mode)));
    let result = body();
    TEST_MODE.with(|slot| *slot.borrow_mut() = previous);
    result
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct WireBindings {
    contract: String,
    host: String,
    revision: u64,
    bindings: Vec<WireBinding>,
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct WireBinding {
    context: String,
    os_identity: String,
}

/// Loads the mode from `dir` for the caller `os`, whose own state folder is
/// `state_root`.
///
/// Downgrade and rollback protection (review of milestone 2): the first time
/// this caller sees a valid multi-person policy for a host ID, its own process
/// records a marker and a high-water mark (revision and policy digest) in its
/// own state folder (`host-access/seen/` and `host-access/high-water/`). After
/// that, a missing or unreadable policy is `policy-lost`, never legacy; a
/// lower revision is `policy-rollback`; the same revision with another digest
/// is `policy-replaced`; a lost or corrupt mark is `high-water-lost` or
/// `high-water-unreadable`. There is no rebootstrap. The Go executor keeps one
/// such mark per host in its own private state; full Zerus keeps it per caller
/// because a non-admin caller cannot write the operator directory. A host
/// this caller never saw as multi-person stays legacy.
pub fn load(dir: &Path, os: &OsIdentity, trust: Trust, state_root: &Path) -> Mode {
    let seen = seen_any(state_root);
    let policy_path = dir.join("policy.json");
    match std::fs::symlink_metadata(&policy_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return if seen { Mode::Broken("policy-lost") } else { Mode::Legacy };
        }
        Err(_) if seen => return Mode::Broken("policy-lost"),
        Err(_) => return Mode::Broken("policy-unreadable"),
        Ok(_) => {}
    }
    match load_multi(dir, os, trust, state_root) {
        Ok(mode) => mode,
        Err(code) => Mode::Broken(code),
    }
}

const MARK_SCHEMA: &str = "zerus-host-access-high-water-v1";

#[derive(serde::Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HighWater {
    schema: String,
    host: String,
    revision: u64,
    digest: String,
}

fn seen_dir(state_root: &Path) -> PathBuf {
    state_root.join("host-access").join("seen")
}
fn mark_path(state_root: &Path, host: &str) -> PathBuf {
    state_root.join("host-access").join("high-water").join(format!("{host}.json"))
}

/// Whether this caller ever saw a multi-person policy on any host. An
/// unreadable marker folder counts as seen (fail closed).
pub fn seen_any(state_root: &Path) -> bool {
    match std::fs::read_dir(seen_dir(state_root)) {
        Ok(mut entries) => entries.next().is_some(),
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

fn write_durable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().expect("mark has a parent");
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        crate::platform::make_private(&file)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Checks `policy` (with `digest`) against this caller's mark for `host` and
/// advances it. The mark is written before the marker, so a crash between the
/// two leaves a mark that is still honored.
fn check_high_water(state_root: &Path, host: &str, revision: u64, digest: &str) -> Result<(), &'static str> {
    let marker = seen_dir(state_root).join(host);
    let marked = match std::fs::symlink_metadata(&marker) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return Err("high-water-unreadable"),
    };
    let path = mark_path(state_root, host);
    let current = match std::fs::read(&path) {
        Ok(bytes) if bytes.len() <= 4096 => match serde_json::from_slice::<HighWater>(&bytes) {
            Ok(mark) if mark.schema == MARK_SCHEMA && mark.host == host => Some(mark),
            _ => return Err("high-water-unreadable"),
        },
        Ok(_) => return Err("high-water-unreadable"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if marked {
                return Err("high-water-lost");
            }
            None
        }
        Err(_) => return Err("high-water-unreadable"),
    };
    if let Some(mark) = &current {
        if revision < mark.revision {
            return Err("policy-rollback");
        }
        if revision == mark.revision && digest != mark.digest {
            return Err("policy-replaced");
        }
    }
    if current.as_ref().is_none_or(|mark| revision > mark.revision) {
        let mark = HighWater { schema: MARK_SCHEMA.into(), host: host.into(), revision, digest: digest.into() };
        write_durable(&path, &serde_json::to_vec(&mark).expect("mark serializes")).map_err(|_| "high-water-write")?;
    }
    if !marked {
        write_durable(&marker, host.as_bytes()).map_err(|_| "high-water-write")?;
    }
    Ok(())
}

fn load_multi(dir: &Path, os: &OsIdentity, trust: Trust, state_root: &Path) -> Result<Mode, &'static str> {
    check_trusted(dir, true, trust)?;
    let host_bytes = read_trusted(&dir.join("host-id"), MAX_HOST_ID_BYTES, trust).map_err(|_| "host-id-untrusted")?;
    let host = std::str::from_utf8(&host_bytes).map_err(|_| "host-id-malformed")?.trim().to_owned();
    if !super::host_id(&host) {
        return Err("host-id-malformed");
    }
    let policy_bytes = read_trusted(&dir.join("policy.json"), super::MAX_POLICY_BYTES as u64 + 1, trust).map_err(|_| "policy-untrusted")?;
    let policy = verify(&policy_bytes, &host).map_err(policy_code)?;
    let digest: String = sha2::Sha256::digest(&policy_bytes).iter().map(|b| format!("{b:02x}")).collect();
    check_high_water(state_root, &host, policy.revision, &digest)?;
    let binding_bytes = read_trusted(&dir.join("os-bindings.json"), MAX_BINDINGS_BYTES + 1, trust).map_err(|_| "bindings-untrusted")?;
    let bindings = parse_bindings(&binding_bytes, &policy)?;
    let Some(context) = bindings.get(os) else {
        return Ok(Mode::Unbound { policy: Box::new(policy), os: os.clone() });
    };
    // The context must name exactly one person of the policy.
    let mut people = policy.contexts.iter().filter(|(_, c)| &c.context == context).map(|(person, _)| person.clone());
    let person = people.next().ok_or("binding-context-unknown")?;
    if people.next().is_some() {
        return Err("binding-context-ambiguous");
    }
    if policy.revoked_persons.contains(&person) {
        return Err("person-revoked");
    }
    Ok(Mode::Bound {
        principal: Principal { person, context: context.clone(), os: os.clone() },
        policy: Box::new(policy),
    })
}

fn policy_code(error: PolicyError) -> &'static str {
    match error {
        PolicyError::TooLarge => "policy-too-large",
        PolicyError::Malformed => "policy-malformed",
        PolicyError::HostMismatch | PolicyError::Host => "policy-host",
        _ => "policy-invalid",
    }
}

/// OS identity -> context. Every identity and every context appears once.
fn parse_bindings(bytes: &[u8], policy: &Policy) -> Result<BTreeMap<OsIdentity, String>, &'static str> {
    if bytes.len() as u64 > MAX_BINDINGS_BYTES {
        return Err("bindings-too-large");
    }
    let wire: WireBindings = serde_json::from_slice(bytes).map_err(|_| "bindings-malformed")?;
    if wire.contract != BINDINGS_CONTRACT || wire.host != policy.host || wire.revision < 1 || wire.bindings.len() > MAX_BINDINGS {
        return Err("bindings-invalid");
    }
    let mut map = BTreeMap::new();
    let mut contexts = std::collections::BTreeSet::new();
    for binding in wire.bindings {
        let os = OsIdentity::parse(&binding.os_identity).ok_or("bindings-invalid")?;
        if !super::name(&binding.context) || !contexts.insert(binding.context.clone()) {
            return Err("bindings-invalid");
        }
        if map.insert(os, binding.context).is_some() {
            return Err("bindings-invalid");
        }
    }
    Ok(map)
}

fn read_trusted(path: &Path, limit: u64, trust: Trust) -> Result<Vec<u8>, ()> {
    use std::io::Read;
    check_trusted(path, false, trust).map_err(|_| ())?;
    let file = std::fs::File::open(path).map_err(|_| ())?;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes).map_err(|_| ())?;
    if bytes.len() as u64 >= limit {
        return Err(());
    }
    Ok(bytes)
}

/// Whether `path` is an operator-owned, caller-read-only file or directory
/// (not a link).
fn check_trusted(path: &Path, directory: bool, trust: Trust) -> Result<(), &'static str> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| "operator-dir-unreadable")?;
    if meta.file_type().is_symlink() || meta.is_dir() != directory {
        return Err("operator-file-type");
    }
    match trust {
        #[cfg(test)]
        Trust::TestOwned => Ok(()),
        Trust::Operator => platform_trusted(path, &meta, directory),
    }
}

#[cfg(unix)]
fn platform_trusted(_path: &Path, meta: &std::fs::Metadata, _directory: bool) -> Result<(), &'static str> {
    use std::os::unix::fs::MetadataExt;
    if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        return Err("operator-owner");
    }
    Ok(())
}

#[cfg(windows)]
fn platform_trusted(path: &Path, _meta: &std::fs::Metadata, directory: bool) -> Result<(), &'static str> {
    if !windows::owner_is_operator(path) {
        return Err("operator-owner");
    }
    // An actual access check with this caller's token: write access to the
    // file, or the right to add or delete entries of the directory, would let
    // the caller substitute the policy.
    if windows::caller_can_modify(path, directory) {
        return Err("operator-writable");
    }
    Ok(())
}

/// Whether `path` is an operator-owned directory the caller cannot modify:
/// the shared project root must be one, so no member can replace another's
/// partition.
pub fn operator_owned_dir(path: &Path) -> Result<(), &'static str> {
    check_trusted(path, true, Trust::Operator)
}

/// Whether `path` (a state folder) belongs to the caller. A missing folder is
/// the caller's to create.
pub fn owned_by(path: &Path, os: &OsIdentity) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return !path.exists() };
    if meta.file_type().is_symlink() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        os.0 == format!("uid:{}", meta.uid())
    }
    #[cfg(windows)]
    {
        windows::owner_sid(path).is_some_and(|sid| sid == os.0)
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Foundation::{LocalFree, ERROR_SUCCESS};
    use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        IsWellKnownSid, WinBuiltinAdministratorsSid, WinLocalSystemSid, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    fn with_owner<T>(path: &Path, read: impl FnOnce(PSID) -> T) -> Option<T> {
        let wide = wide(path);
        let mut owner: PSID = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated; out pointers are locals; the
        // descriptor is freed below and `owner` points into it.
        let code = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if code != ERROR_SUCCESS || owner.is_null() {
            if !descriptor.is_null() {
                // SAFETY: allocated by GetNamedSecurityInfoW.
                unsafe { LocalFree(descriptor as _) };
            }
            return None;
        }
        let value = read(owner);
        // SAFETY: allocated by GetNamedSecurityInfoW; not used afterwards.
        unsafe { LocalFree(descriptor as _) };
        Some(value)
    }

    pub fn owner_is_operator(path: &Path) -> bool {
        // SAFETY: `owner` is a valid SID inside the live descriptor.
        with_owner(path, |owner| unsafe {
            IsWellKnownSid(owner, WinLocalSystemSid) != 0 || IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0
        })
        .unwrap_or(false)
    }

    pub fn owner_sid(path: &Path) -> Option<String> {
        with_owner(path, |owner| {
            let mut text: windows_sys::core::PWSTR = std::ptr::null_mut();
            // SAFETY: `owner` is valid; `text` is freed with LocalFree.
            if unsafe { ConvertSidToStringSidW(owner, &mut text) } == 0 || text.is_null() {
                return None;
            }
            // SAFETY: a NUL-terminated string from ConvertSidToStringSidW.
            let value = unsafe {
                let mut length = 0;
                while *text.add(length) != 0 {
                    length += 1;
                }
                String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
            };
            // SAFETY: allocated by ConvertSidToStringSidW.
            unsafe { LocalFree(text as _) };
            Some(value)
        })
        .flatten()
    }

    /// Opens `path` with modifying access using this process's token; success
    /// means the caller could change it. Nothing is written.
    pub fn caller_can_modify(path: &Path, directory: bool) -> bool {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_WRITE_DATA: u32 = 0x0002;
        const FILE_ADD_FILE: u32 = 0x0002;
        const FILE_ADD_SUBDIRECTORY: u32 = 0x0004;
        const FILE_DELETE_CHILD: u32 = 0x0040;
        const WRITE_DAC: u32 = 0x0004_0000;
        const WRITE_OWNER: u32 = 0x0008_0000;
        const DELETE: u32 = 0x0001_0000;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        let rights: &[u32] = if directory {
            &[FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_DELETE_CHILD, WRITE_DAC, WRITE_OWNER]
        } else {
            &[FILE_WRITE_DATA, WRITE_DAC, WRITE_OWNER, DELETE]
        };
        rights.iter().any(|right| {
            let mut options = std::fs::OpenOptions::new();
            options.access_mode(*right).share_mode(7);
            if directory {
                options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
            }
            options.open(path).is_ok()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_identities_have_one_canonical_shape() {
        for good in ["uid:0", "uid:1001", "S-1-5-21-1-2-3-1001", "S-1-5-18"] {
            assert!(OsIdentity::parse(good).is_some(), "{good}");
        }
        for bad in ["", "uid:", "uid:-1", "uid:99999999999", "uid:1 ", "S-1-", "S-1-5--1", "S-1-5-x", "s-1-5-18", "alice"] {
            assert!(OsIdentity::parse(bad).is_none(), "{bad}");
        }
    }
}
