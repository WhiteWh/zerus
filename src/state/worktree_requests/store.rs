//! The original receipt directory and ID lock, shared by every creator and
//! explicit Stop. A new fence records future exclusion, never historical absence.
use super::*;
use crate::platform::FileIdentity;
use crate::state::repositories::directory::Directory;
use std::io::Read;

const ANCHOR: &str = "worktree-requests.identity.json";
const STORE_LOCK: &str = "worktree-requests.identity.lock";
const LIMIT: usize = 32768;

pub(super) struct Store {
    root: Arc<Directory>,
    directory: Directory,
    anchor: Option<(File, FileIdentity)>,
}
fn same_named(directory: &Directory, name: &str, file: &File) -> Result<()> {
    let named = directory.read_private(name)?;
    let a = FileIdentity::of(file).map_err(|e| e.to_string())?;
    let b = FileIdentity::of(&named).map_err(|e| e.to_string())?;
    if a != b {
        return Err("Worktree receipt object changed".into());
    }
    directory.verify()
}
fn same_named_lock(
    directory: &Directory,
    name: &str,
    guard: &crate::platform::FileLock,
) -> Result<()> {
    let named = directory.read_private(name)?;
    let original = guard.identity().map_err(|e| e.to_string())?;
    let current = FileIdentity::of(&named).map_err(|e| e.to_string())?;
    if original != current {
        return Err("Worktree receipt object changed".into());
    }
    directory.verify()
}

fn bytes(directory: &Directory, name: &str, mut file: &File) -> Result<Vec<u8>> {
    let before = FileIdentity::with_metadata(file).map_err(|e| e.to_string())?;
    if before.1.len() > LIMIT as u64 {
        return Err("Worktree receipt exceeds its limit".into());
    }
    let mut result = Vec::new();
    file.by_ref()
        .take(LIMIT as u64 + 1)
        .read_to_end(&mut result)
        .map_err(|e| e.to_string())?;
    let after = FileIdentity::with_metadata(file).map_err(|e| e.to_string())?;
    if result.len() > LIMIT || before.0 != after.0 || before.1.len() != after.1.len() {
        return Err("Worktree receipt changed while reading".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (before.1.ctime(), before.1.ctime_nsec()) != (after.1.ctime(), after.1.ctime_nsec()) {
            return Err("Worktree receipt changed while reading".into());
        }
    }
    same_named(directory, name, file)?;
    Ok(result)
}
#[derive(PartialEq, Eq)]
struct Version {
    identity: FileIdentity,
    len: u64,
    changed: (i64, i64),
}
fn version(file: &File) -> Result<Version> {
    let (identity, metadata) = FileIdentity::with_metadata(file).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    let changed = {
        use std::os::unix::fs::MetadataExt;
        (metadata.ctime(), metadata.ctime_nsec())
    };
    #[cfg(not(unix))]
    let changed = (0, 0);
    Ok(Version {
        identity,
        len: metadata.len(),
        changed,
    })
}
struct Pin {
    name: String,
    file: File,
    version: Version,
}
impl Pin {
    fn new(name: String, file: File) -> Result<Self> {
        Ok(Self {
            name,
            version: version(&file)?,
            file,
        })
    }
    fn verify(&self, directory: &Directory) -> Result<()> {
        if version(&self.file)? != self.version {
            return Err("Original worktree receipt changed".into());
        }
        same_named(directory, &self.name, &self.file)
    }
}

impl Store {
    pub(super) fn open(path: &Path, create: bool) -> Result<Self> {
        Self::acquire(path, create, true)
    }
    pub(super) fn observe(path: &Path) -> Result<Self> {
        Self::acquire(path, false, false)
    }
    fn acquire(path: &Path, create: bool, initialize: bool) -> Result<Self> {
        if create {
            private_dir(path)?;
        }
        // Configured root aliases are resolved once; every component BELOW
        // that trusted root remains no-follow. Both native routes use it.
        let path = crate::platform::canonical(path).map_err(|e| e.to_string())?;
        let root = Arc::new(Directory::open(&path)?);
        Self::acquire_root(root, create, initialize)
    }
    pub(super) fn acquire_root(
        root: Arc<Directory>,
        create: bool,
        initialize: bool,
    ) -> Result<Self> {
        root.verify()?;
        if !root.private()? {
            return Err("Worktree receipt root must be private".into());
        }
        let directory = if create {
            root.ensure_child("worktree-requests")?
        } else {
            root.child("worktree-requests")?
        };
        if !directory.private()? {
            return Err("Worktree receipt store must be private".into());
        }
        // The anchor is OUTSIDE the replaceable receipt namespace. Installing
        // it for a legacy store protects its future incarnation only; it is
        // deliberately never a no-past-effects witness.
        let guard = if initialize {
            let file = root.open_private_lock(STORE_LOCK)?;
            file.try_lock_exclusive()
                .map_err(|_| "Worktree receipt store is busy")?;
            let guard = crate::platform::FileLock::held(file);
            #[cfg(all(test, unix))]
            crate::platform::residual_lock_tests::reached("request-init");
            same_named_lock(&root, STORE_LOCK, &guard)?;
            Some(guard)
        } else {
            None
        };
        let object = directory.object()?;
        let expected = json!({"schema":1,"directory_device":object.0,"directory_inode":object.1});
        let anchor = match root.read_private_optional(ANCHOR)? {
            Some(file) => {
                let actual: Value = serde_json::from_slice(&bytes(&root, ANCHOR, &file)?)
                    .map_err(|_| "Unreadable worktree receipt store identity")?;
                if actual != expected {
                    return Err(
                        "Worktree receipt store was replaced; its historical results are unknown"
                            .into(),
                    );
                }
                Some(file)
            }
            None if initialize => {
                cut_write()?;
                Some(root.write_new_file(ANCHOR, &expected.to_string().into_bytes())?)
            }
            None => None,
        }
        .map(|file| {
            FileIdentity::of(&file)
                .map(|identity| (file, identity))
                .map_err(|e| e.to_string())
        })
        .transpose()?;
        let store = Self {
            root,
            directory,
            anchor,
        };
        store.verify()?;
        // Explicit unlock prevents an unrelated fork retaining this lock until
        // exec. It does not unlock any request or release native custody.
        if let Some(guard) = guard {
            same_named_lock(&store.root, STORE_LOCK, &guard)?;
            guard.release().map_err(|e| e.to_string())?;
        }
        Ok(store)
    }
    pub(super) fn verify(&self) -> Result<()> {
        self.root.verify()?;
        self.directory.verify()?;
        if let Some((file, original)) = &self.anchor {
            same_named(&self.root, ANCHOR, file)?;
            if FileIdentity::of(file).map_err(|e| e.to_string())? != *original {
                return Err("Worktree receipt store identity changed".into());
            }
        }
        Ok(())
    }
    pub(super) fn observe_lock(self: &Arc<Self>, id: &str) -> Result<Lock> {
        let file = self.directory.read_private(&format!("{id}.lock"))?;
        self.acquire_lock(id, file)?
            .ok_or_else(|| "Worktree request is still running".into())
    }
    pub(super) fn lock(self: &Arc<Self>, id: &str) -> Result<Lock> {
        self.try_lock(id)?
            .ok_or_else(|| "Worktree request is still running or being reserved".into())
    }
    pub(super) fn try_lock(self: &Arc<Self>, id: &str) -> Result<Option<Lock>> {
        self.verify()?;
        let file = self.directory.open_private_lock(&format!("{id}.lock"))?;
        self.acquire_lock(id, file)
    }
    fn acquire_lock(self: &Arc<Self>, id: &str, file: File) -> Result<Option<Lock>> {
        let name = format!("{id}.lock");
        same_named(&self.directory, &name, &file)?;
        match file.try_lock_exclusive() {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                same_named(&self.directory, &name, &file)?;
                self.verify()?;
                return Ok(None);
            }
            Err(error) => return Err(error.to_string()),
        }
        let receipt_name = format!("{id}.json");
        let guard = Lock {
            store: self.clone(),
            name: format!("{id}.lock"),
            file,
            receipt_name: receipt_name.clone(),
            receipt: std::sync::Mutex::new(None),
        };
        let receipt = self
            .directory
            .read_private_optional(&receipt_name)?
            .map(|file| Pin::new(receipt_name, file))
            .transpose()?;
        *guard
            .receipt
            .lock()
            .map_err(|_| "Worktree receipt pin unavailable")? = receipt;
        guard.verify()?;
        Ok(Some(guard))
    }
    pub(super) fn cancel_path(&self, id: &str) -> PathBuf {
        self.directory.path.join(format!("{id}.cancel"))
    }
    pub(super) fn request_cancel(&self, id: &str) -> Result<()> {
        self.verify()?;
        let name = format!("{id}.cancel");
        if let Some(file) = self.directory.read_private_optional(&name)? {
            if bytes(&self.directory, &name, &file)? != b"cancel" {
                return Err("Worktree cancellation marker changed".into());
            }
        } else {
            cut_write()?;
            self.directory.write_new(&name, b"cancel")?;
        }
        self.verify()
    }
    pub(super) fn read(&self, id: &str) -> Result<Option<Value>> {
        self.verify()?;
        let name = format!("{id}.json");
        let result = self
            .directory
            .read_private_optional(&name)?
            .map(|file| -> Result<Value> {
                let value: Value = serde_json::from_slice(&bytes(&self.directory, &name, &file)?)
                    .map_err(|_| "Unreadable worktree receipt")?;
                if value["schema"] != 1 || value["request_id"] != id || !value["status"].is_string()
                {
                    return Err("Unsupported worktree receipt".into());
                }
                Ok(value)
            })
            .transpose()?;
        self.verify()?;
        Ok(result)
    }
}
pub(super) struct Lock {
    pub(super) store: Arc<Store>,
    name: String,
    file: File,
    receipt_name: String,
    receipt: std::sync::Mutex<Option<Pin>>,
}
impl Lock {
    pub(super) fn verify(&self) -> Result<()> {
        self.store.verify()?;
        same_named(&self.store.directory, &self.name, &self.file)?;
        let receipt = self
            .receipt
            .lock()
            .map_err(|_| "Worktree receipt pin unavailable")?;
        if let Some(receipt) = receipt.as_ref() {
            receipt.verify(&self.store.directory)?;
        } else if self
            .store
            .directory
            .read_private_optional(&self.receipt_name)?
            .is_some()
        {
            return Err("Worktree receipt appeared after exclusion".into());
        }
        Ok(())
    }
    pub(super) fn write(&self, id: &str, value: &Value, replace: bool) -> Result<()> {
        self.verify()?;
        let name = format!("{id}.json");
        if name != self.receipt_name {
            return Err("Worktree receipt identity changed".into());
        }
        let data = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        if data.len() > LIMIT {
            return Err("Worktree receipt exceeds its limit".into());
        }
        cut_write()?;
        self.file.sync_all().map_err(|e| e.to_string())?;
        let file = if replace {
            self.store.directory.replace_file_retained(&name, &data)?
        } else {
            self.store.directory.write_new_file(&name, &data)?
        };
        *self
            .receipt
            .lock()
            .map_err(|_| "Worktree receipt pin unavailable")? = Some(Pin::new(name, file)?);
        self.verify()?;
        if self.store.read(id)?.as_ref() != Some(value) {
            return Err("Worktree receipt publication is unconfirmed".into());
        }
        Ok(())
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(all(test, unix))]
mod residual_lock_tests;
