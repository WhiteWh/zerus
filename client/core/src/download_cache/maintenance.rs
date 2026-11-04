//! One frozen cache pass. Preparation holds the existing Qt folder lock but
//! changes nothing. The service must durably reserve `binding()` before calling
//! `execute`; none of these data structures establishes execution authority.
use super::*;
use serde::{Deserialize, Serialize};
use std::io::{Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub const INDEX_BYTES: u64 = 1 << 20;
pub const INDEX_ENTRIES: usize = 1024;
pub const CANDIDATES: usize = 16;
pub const CONTENT_BYTES: u64 = 64 << 20;
pub const DEADLINE: Duration = Duration::from_secs(15);
const REFUSED: &str =
    "The downloaded files changed or could not be verified. Existing files were preserved.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
    pub object: String,
    pub identity: String,
    /// Stable across requests and unrelated index edits. An unknown removal
    /// of this original file/content must fence a later request, even if moved.
    pub content_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub root: String,
    pub directory: String,
    pub index_sha256: String,
    pub index_identity: String,
    pub limit: u64,
    pub candidates: Vec<Candidate>,
}
impl Binding {
    pub fn valid(&self) -> bool {
        Path::new(&self.root).is_absolute()
            && self.root.len() <= 8192
            && !self.root.chars().any(char::is_control)
            && digest(&self.directory)
            && digest(&self.index_sha256)
            && digest(&self.index_identity)
            && self.candidates.len() <= CANDIDATES
            && self.candidates.iter().all(|c| {
                safe_name(&c.name)
                    && c.name.len() <= 1024
                    && digest(&c.sha256)
                    && digest(&c.identity)
                    && digest(&c.object)
                    && c.content_id == content_id(&c.object, &c.sha256)
            })
            && self
                .candidates
                .iter()
                .enumerate()
                .all(|(i, c)| self.candidates[..i].iter().all(|old| old.name != c.name))
    }
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn content_id(object: &str, sha256: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"desktop-cache-content-v1\0");
    for field in [object, sha256] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    hex(&hash.finalize())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Removed,
    Kept,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub index_sha256: String,
    pub files: Vec<State>,
    pub bytes: u64,
    pub count: usize,
}

/// Owns the actual folder exclusion and pinned original files, not a decoded
/// plan. Never Clone/Serialize. Keep it on one off-actor worker through return.
pub struct Prepared {
    ledger: Ledger,
    directory: File,
    index: File,
    header: Map<String, Value>,
    binding: Binding,
    files: Vec<File>,
    usage: Usage,
    at: Instant,
}
impl Prepared {
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
    /// A known pre-unlink refusal of this exact original frozen pass.
    pub fn kept_receipt(&self) -> Receipt {
        self.receipt(vec![State::Kept; self.files.len()])
    }

    /// No directory creation, index rewrite, hashing of cached content or
    /// deletion. Refuses oversized legacy indexes without truncating them.
    pub fn open(root: &Path, limit: u64, now: i64, cancel: &AtomicBool) -> Result<Self, String> {
        let at = Instant::now();
        live(cancel, at)?;
        let canonical = fs::canonicalize(root).map_err(|_| REFUSED)?;
        if canonical != root || root.to_str().is_none_or(|s| s.len() > 8192) {
            return Err(REFUSED.into());
        }
        let directory = open(root, true)?;
        let mut ledger = Ledger::open_bounded(root, INDEX_BYTES)?;
        if ledger.entries.len() > INDEX_ENTRIES {
            return Err(REFUSED.into());
        }
        let index = open(&root.join(INDEX), false)?;
        let raw = read_index(&index)?;
        let index_sha256 = hex(&Sha256::digest(&raw));
        if ledger.original_sha256.as_ref() != Some(&index_sha256) {
            return Err(REFUSED.into());
        }
        let mut header = decode_index(&raw)?;
        header.remove("files");
        let directory_id = identity(&directory, true)?;
        let mut binding = Binding {
            root: root.to_str().ok_or(REFUSED)?.into(),
            directory: directory_id,
            index_sha256,
            index_identity: identity(&index, false)?,
            limit,
            candidates: Vec::new(),
        };
        let mut files = Vec::new();
        let mut usage = Usage::default();
        ledger.entries.sort_by_key(|entry| integer(entry, "saved"));
        for entry in &ledger.entries {
            live(cancel, at)?;
            let path = root.join(text(entry, "name"));
            let Ok(file) = open(&path, false) else {
                continue;
            };
            let meta = file.metadata().map_err(|_| REFUSED)?;
            if meta.len() as i128 != i128::from(integer(entry, "bytes")) {
                continue;
            }
            usage.bytes = usage.bytes.checked_add(meta.len()).ok_or(REFUSED)?;
            usage.files += 1;
            if limit > 0
                && now.saturating_sub(integer(entry, "saved")) >= OPENING_GRACE_MS
                && binding.candidates.len() < CANDIDATES
            {
                let name = text(entry, "name").to_owned();
                let sha256 = text(entry, "sha256").to_owned();
                let (object, identity) = identities(&file, false)?;
                binding.candidates.push(Candidate {
                    content_id: content_id(&object, &sha256),
                    object,
                    name,
                    sha256,
                    bytes: meta.len(),
                    identity,
                });
                files.push(file);
            }
        }
        let prepared = Self {
            ledger,
            directory,
            index,
            header,
            binding,
            files,
            usage,
            at,
        };
        prepared.verify()?;
        if !prepared.binding.valid() {
            return Err(REFUSED.into());
        }
        Ok(prepared)
    }

    fn verify(&self) -> Result<(), String> {
        let root = Path::new(&self.binding.root);
        let named = open(root, true)?;
        if fs::canonicalize(root).ok().as_deref() != Some(root)
            || identity(&self.directory, true)? != self.binding.directory
            || identity(&named, true)? != self.binding.directory
            || identity(&self.index, false)? != self.binding.index_identity
            || !same_file(&self.index, &root.join(INDEX))
            || hex(&Sha256::digest(read_index(&self.index)?)) != self.binding.index_sha256
        {
            return Err(REFUSED.into());
        }
        Ok(())
    }

    /// Called ONCE after the actor reserves this exact binding. False entries
    /// are existing durable unknown-content fences; never reinterpret them as
    /// retry permissions. Cancellation before unlink keeps that file.
    pub fn execute(mut self, allowed: &[bool], cancel: &AtomicBool) -> Receipt {
        let mut states = vec![State::Kept; self.files.len()];
        let mut charged = 0u64;
        if allowed.len() != self.files.len() {
            return self.receipt(states);
        }
        for (i, permitted) in allowed.iter().enumerate() {
            let candidate = &self.binding.candidates[i];
            if !permitted
                || self.usage.bytes <= self.binding.limit
                || live(cancel, self.at).is_err()
            {
                continue;
            }
            if candidate.bytes > CONTENT_BYTES.saturating_sub(charged) {
                continue;
            }
            charged += candidate.bytes;
            let file = &mut self.files[i];
            let mut hasher = Sha256::new();
            let mut bytes = 0u64;
            let mut buffer = [0u8; 64 * 1024];
            let verified = (|| -> Result<(), String> {
                if identity(file, false)? != candidate.identity {
                    return Err(REFUSED.into());
                }
                file.seek(SeekFrom::Start(0)).map_err(|_| REFUSED)?;
                loop {
                    live(cancel, self.at)?;
                    // One byte beyond the frozen length detects a grown file;
                    // allocation and hashing remain strictly bounded.
                    let take = buffer.len().min(
                        (candidate.bytes.saturating_sub(bytes).saturating_add(1)).min(65536)
                            as usize,
                    );
                    let count = file.read(&mut buffer[..take]).map_err(|_| REFUSED)?;
                    if count == 0 {
                        break;
                    }
                    bytes += count as u64;
                    if bytes > candidate.bytes {
                        return Err(REFUSED.into());
                    }
                    hasher.update(&buffer[..count]);
                }
                if bytes != candidate.bytes
                    || hex(&hasher.finalize()) != candidate.sha256
                    || identity(file, false)? != candidate.identity
                    || !same_file(file, &self.ledger.root.join(&candidate.name))
                {
                    return Err(REFUSED.into());
                }
                Ok(())
            })();
            if verified.is_err() || self.verify().is_err() || live(cancel, self.at).is_err() {
                continue;
            }
            // The existing Qt lock excludes all cooperating cache writers.
            // Recheck the exact opened original immediately before unlink.
            if identity(&self.files[i], false).ok().as_ref() != Some(&candidate.identity)
                || !same_file(&self.files[i], &self.ledger.root.join(&candidate.name))
            {
                continue;
            }
            states[i] = unlink_outcome(fs::remove_file(self.ledger.root.join(&candidate.name)));
            if states[i] == State::Removed {
                self.usage.bytes = self.usage.bytes.saturating_sub(candidate.bytes);
                self.usage.files = self.usage.files.saturating_sub(1);
            }
        }
        if states.contains(&State::Removed) && self.save(&states).is_err() {
            for state in &mut states {
                if *state == State::Removed {
                    *state = State::Unknown;
                }
            }
        }
        self.receipt(states)
    }
    fn receipt(&self, files: Vec<State>) -> Receipt {
        Receipt {
            index_sha256: self.binding.index_sha256.clone(),
            files,
            bytes: self.usage.bytes,
            count: self.usage.files,
        }
    }
    fn save(&mut self, states: &[State]) -> Result<(), String> {
        self.verify()?;
        let removed: std::collections::HashSet<_> = states
            .iter()
            .enumerate()
            .filter(|(_, state)| **state == State::Removed)
            .map(|(i, _)| self.binding.candidates[i].name.as_str())
            .collect();
        let retained: Vec<_> = self
            .ledger
            .entries
            .iter()
            .filter(|entry| !removed.contains(text(entry, "name")))
            .cloned()
            .collect();
        let mut document = self.header.clone();
        document.insert("files".into(), json!(retained));
        let encoded = serde_json::to_vec(&document).map_err(|_| REFUSED)?;
        if encoded.len() as u64 > INDEX_BYTES {
            return Err(REFUSED.into());
        }
        let (path, mut file) =
            fresh_file(&self.ledger.root, ".zerus-files.json.").map_err(|_| REFUSED)?;
        let result = file.write_all(&encoded).and_then(|()| file.sync_all());
        if result.is_err()
            || self.verify().is_err()
            || fs::rename(&path, self.ledger.root.join(INDEX)).is_err()
        {
            let _ = fs::remove_file(path);
            return Err(REFUSED.into());
        }
        #[cfg(unix)]
        self.directory.sync_all().map_err(|_| REFUSED)?;
        Ok(())
    }
}

fn unlink_outcome(result: io::Result<()>) -> State {
    match result {
        Ok(()) => State::Removed,
        Err(error) => {
            // A viewer may deny delete sharing. This exact Windows refusal
            // precedes deletion; it is not a lost destructive result. Do not
            // generalize to ErrorKind, access denied, missing files or other
            // errors, which retain the original conservative Unknown outcome.
            // https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-deletefile
            #[cfg(windows)]
            if error.raw_os_error()
                == Some(windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION as i32)
            {
                return State::Kept;
            }
            let _ = error;
            State::Unknown
        }
    }
}

fn live(cancel: &AtomicBool, at: Instant) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) || at.elapsed() >= DEADLINE {
        Err(REFUSED.into())
    } else {
        Ok(())
    }
}
fn read_index(file: &File) -> Result<Vec<u8>, String> {
    if file.metadata().map_err(|_| REFUSED)?.len() > INDEX_BYTES {
        return Err(REFUSED.into());
    }
    let mut file = file.try_clone().map_err(|_| REFUSED)?;
    file.seek(SeekFrom::Start(0)).map_err(|_| REFUSED)?;
    let mut bytes = Vec::new();
    file.take(INDEX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| REFUSED)?;
    if bytes.len() as u64 > INDEX_BYTES {
        return Err(REFUSED.into());
    }
    Ok(bytes)
}
fn open(path: &Path, directory: bool) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(
            libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK
                | if directory { libc::O_DIRECTORY } else { 0 },
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::*;
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(
                FILE_FLAG_OPEN_REPARSE_POINT
                    | if directory {
                        FILE_FLAG_BACKUP_SEMANTICS
                    } else {
                        0
                    },
            );
    }
    let file = options.open(path).map_err(|_| REFUSED)?;
    identity(&file, directory)?;
    Ok(file)
}
fn identity(file: &File, directory: bool) -> Result<String, String> {
    Ok(identities(file, directory)?.1)
}
fn identities(file: &File, directory: bool) -> Result<(String, String), String> {
    let meta = file.metadata().map_err(|_| REFUSED)?;
    if meta.is_dir() != directory || (!directory && !meta.is_file()) {
        return Err(REFUSED.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !directory && meta.nlink() != 1 {
            return Err(REFUSED.into());
        }
        let object = format!("{}:{}", meta.dev(), meta.ino());
        let value = if directory {
            object.clone()
        } else {
            format!(
                "{}:{}:{}:{}:{}:{}:{}",
                meta.dev(),
                meta.ino(),
                meta.len(),
                meta.mtime(),
                meta.mtime_nsec(),
                meta.ctime(),
                meta.ctime_nsec()
            )
        };
        Ok((
            hex(&Sha256::digest(object.as_bytes())),
            hex(&Sha256::digest(value.as_bytes())),
        ))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::*;
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0
            || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || (!directory && info.nNumberOfLinks != 1)
        {
            return Err(REFUSED.into());
        }
        let mut value = format!(
            "{}:{}:{}",
            info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow
        );
        let object = value.clone();
        if !directory {
            value.push_str(&format!(
                ":{}:{}:{}:{}",
                meta.len(),
                info.ftLastWriteTime.dwHighDateTime,
                info.ftLastWriteTime.dwLowDateTime,
                info.dwFileAttributes
            ));
        }
        Ok((
            hex(&Sha256::digest(object.as_bytes())),
            hex(&Sha256::digest(value.as_bytes())),
        ))
    }
}

#[cfg(test)]
mod tests;
