//! Explicit local review of a kept damaged copy. Nothing here imports or sends.
use super::*;
use std::io::Read;

const REVIEW_BYTES: u64 = 64 * 1024 * 1024;
const REVIEW_TIME: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CopyReview {
    pub uuid: String,
    pub fingerprint: String,
    pub files: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CopyReviewOutcome {
    /// moved, or unknown after an operation might have changed the files.
    pub state: String,
    pub backup: String,
    pub reason: Option<String>,
}

fn directory(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(Error::failed(
            "task copy review requires existing regular directories",
        ));
    }
    Ok(())
}

fn regular(path: &Path, write: bool) -> std::io::Result<File> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(std::io::Error::other(
            "task copy review refuses links and special files",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(write);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other(
            "task copy review refuses special files",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(std::io::Error::other("task copy review refuses hard links"));
        }
    }
    Ok(file)
}

impl ProjectStore {
    /// Existing store only, no directory creation, temporary-file cleanup or
    /// task enumeration. Only its existing lock is opened for writing.
    pub fn open_copy_review(root: &Path, project: &str, wait: Duration) -> Result<Option<Self>> {
        if project.is_empty() || project.len() > 128 || project.chars().any(char::is_control) {
            return Err(Error::failed("invalid project ID"));
        }
        directory(root)?;
        let dir = root.join(format!("{:x}", Sha256::digest(project.as_bytes())));
        directory(&dir)?;
        directory(&dir.join("tasks"))?;
        let mut identity = Vec::new();
        regular(&dir.join("project-id"), false)?
            .take(129)
            .read_to_end(&mut identity)?;
        if identity != project.as_bytes() {
            return Err(Error::failed("task copy review project identity changed"));
        }
        let lock = regular(&dir.join(".lock"), true)?;
        let deadline = Instant::now() + wait;
        loop {
            match lock.try_lock_exclusive() {
                Ok(()) => break,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error.into()),
            }
        }
        let lock_owner_pid = std::process::id();
        // Recheck the named directories and identity after waiting for a writer.
        directory(root)?;
        directory(&dir)?;
        directory(&dir.join("tasks"))?;
        identity.clear();
        regular(&dir.join("project-id"), false)?
            .take(129)
            .read_to_end(&mut identity)?;
        if identity != project.as_bytes() {
            return Err(Error::failed("task copy review project identity changed"));
        }
        Ok(Some(Self {
            dir,
            project: project.into(),
            _lock: lock,
            lock_owner_pid,
            journal: std::sync::Mutex::new(()),
            admission: Admission::default(),
            durable: std::sync::Mutex::new(std::collections::HashMap::new()),
        }))
    }

    /// Bounded inspection only. The fingerprint binds explicit confirmation to
    /// the exact local files, including missing files. It grants no execution.
    pub fn prepare_copy_review(&mut self, uuid: &str) -> Result<CopyReview> {
        if !uuid::Uuid::parse_str(uuid).is_ok_and(|id| id.to_string() == uuid) {
            return Err(Error::failed(
                "task copy review requires the exact canonical UUID",
            ));
        }
        directory(&self.dir)?;
        let folder = self.dir.join("tasks");
        directory(&folder)?;
        let deadline = Instant::now() + REVIEW_TIME;
        let mut hash = Sha256::new();
        hash.update(self.project.as_bytes());
        hash.update(uuid.as_bytes());
        let mut files = Vec::new();
        let mut contents = BTreeMap::new();
        let mut remaining = REVIEW_BYTES;
        for extension in ["loro", "updates", "managed"] {
            hash.update(extension.as_bytes());
            let path = folder.join(format!("{uuid}.{extension}"));
            let mut file = match regular(&path, false) {
                Ok(file) => file,
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    hash.update([0]);
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            hash.update([1]);
            let length = file.metadata()?.len();
            if length > remaining {
                return Err(Error::failed(
                    "task copy review exceeds its 64 MiB read bound",
                ));
            }
            remaining -= length;
            hash.update(length.to_le_bytes());
            let mut read = 0;
            let mut bytes = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                if Instant::now() >= deadline {
                    return Err(Error::failed("task copy review inspection timed out"));
                }
                let n = file.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                read += n as u64;
                if read > length {
                    return Err(Error::failed("task copy changed during review"));
                }
                hash.update(&buffer[..n]);
                bytes.extend_from_slice(&buffer[..n]);
            }
            if read != length {
                return Err(Error::failed("task copy changed during review"));
            }
            files.push(extension.into());
            contents.insert(path, bytes);
        }
        let kept = contents.contains_key(&self.managed_path(uuid))
            || contents
                .get(&self.updates_path(uuid))
                .is_some_and(|bytes| !blocks(bytes).1);
        let readable = self
            .counted_with(uuid, |path| {
                contents
                    .get(path)
                    .cloned()
                    .ok_or_else(|| std::io::Error::from(ErrorKind::NotFound))
            })
            .is_ok();
        if files.is_empty() || readable || !kept {
            return Err(Error::failed("this is not a kept damaged task copy"));
        }
        if Instant::now() >= deadline {
            return Err(Error::failed("task copy review inspection timed out"));
        }
        Ok(CopyReview {
            uuid: uuid.into(),
            fingerprint: format!("{:x}", hash.finalize()),
            files,
        })
    }

    /// Move only the reviewed snapshot/updates; remove the mark last. A partial
    /// filesystem failure is unknown and must never be automatically replayed.
    pub fn move_reviewed_copy(&mut self, uuid: &str, expected: &str) -> Result<CopyReviewOutcome> {
        self.move_reviewed_copy_with(uuid, expected, || Ok(()))
    }

    fn move_reviewed_copy_with(
        &mut self,
        uuid: &str,
        expected: &str,
        before_mark: impl FnOnce() -> std::io::Result<()>,
    ) -> Result<CopyReviewOutcome> {
        let review = self.prepare_copy_review(uuid)?;
        if review.fingerprint != expected {
            return Err(Error::failed(
                "the task copy changed; review it again before confirming",
            ));
        }
        let folder = self.dir.join("tasks");
        let backup = folder.join(format!("{uuid}.review-{}", ids::new_uuid()));
        #[cfg(unix)]
        let mut builder = fs::DirBuilder::new();
        #[cfg(not(unix))]
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&backup)?;
        let changed = (|| -> std::io::Result<()> {
            for extension in ["loro", "updates"] {
                if review.files.iter().any(|file| file == extension) {
                    let name = format!("{uuid}.{extension}");
                    regular(&folder.join(&name), false)?;
                    fs::rename(folder.join(&name), backup.join(&name))?;
                }
            }
            #[cfg(unix)]
            {
                File::open(&backup)?.sync_all()?;
                File::open(&folder)?.sync_all()?;
            }
            if review.files.iter().any(|file| file == "managed") {
                before_mark()?;
                regular(&self.managed_path(uuid), false)?;
                fs::remove_file(self.managed_path(uuid))?;
            }
            #[cfg(unix)]
            File::open(&folder)?.sync_all()?;
            Ok(())
        })();
        self.note_change(uuid);
        Ok(CopyReviewOutcome {
            state: if changed.is_ok() { "moved" } else { "unknown" }.into(),
            backup: backup.to_string_lossy().into_owned(),
            reason: changed.err().map(|error| error.to_string()),
        })
    }
}

#[cfg(test)]
mod tests;
