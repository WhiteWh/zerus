//! Native temporary-stage ownership, separate from import/effect evidence.
//! Adjacent sidecars keep the desktop-import-v1 input inventory unchanged.
use super::*;
use crate::platform::FileIdentity;
use serde::Serialize;
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

const OWNER: &str = ".owner-v1.json";
const LEASE: &str = ".lease-v1";
const AGE: u64 = 24 * 60 * 60;
const ENTRIES: usize = 256;
const REMOVE: usize = 2;
const FILES: usize = 2052;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Kind {
    Input,
    Claim { source: String, directory: [u64; 2] },
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    configuration: String,
    import_id: String,
    nonce: String,
    stage: String,
    directory: [u64; 2],
    lock: [u64; 2],
    created: u64,
    ownership: Kind,
}
fn stamp() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
fn leaf(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && value.len() > prefix.len()
        && value.len() <= 80
        && value[prefix.len()..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn file(path: &Path, create: bool) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create_new(create);
    platform::private_open(&mut options);
    platform::no_follow(&mut options);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        platform::refuse_link(&mut options);
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    }
    let file = options.open(path)?;
    ensure!(
        platform::verify_private_file(&file)? && file.metadata()?.len() == 0,
        "unsafe staging lease"
    );
    Ok(file)
}
fn id(file: &fs::File) -> Result<[u64; 2]> {
    super::held::key(file)
}
fn paths(root: &Path, name: &str) -> (PathBuf, PathBuf) {
    (
        root.join(format!("{name}{OWNER}")),
        root.join(format!("{name}{LEASE}")),
    )
}

pub(super) struct Lease {
    record: Record,
    root: PathBuf,
    marker: FileIdentity,
    keep: bool,
    lock: Option<platform::FileLock>,
}
impl Lease {
    pub fn input(location: &Location, path: &Path, import_id: &str) -> Result<Self> {
        Self::new(location, path, import_id, Kind::Input, stamp()?)
    }
    pub fn claim(
        location: &Location,
        path: &Path,
        import_id: &str,
        source: &str,
        directory: [u64; 2],
    ) -> Result<Self> {
        Self::new(
            location,
            path,
            import_id,
            Kind::Claim {
                source: source.into(),
                directory,
            },
            stamp()?,
        )
    }
    fn new(
        location: &Location,
        path: &Path,
        import_id: &str,
        ownership: Kind,
        created: u64,
    ) -> Result<Self> {
        let stage = path
            .file_name()
            .and_then(|p| p.to_str())
            .context("invalid native stage")?
            .to_owned();
        let root = location.root.join("imports");
        ensure!(
            path.parent() == Some(root.as_path()) && canonical_uuid(import_id),
            "invalid native stage scope"
        );
        let (marker, lock_path) = paths(&root, &stage);
        let lock = file(&lock_path, true)?;
        fs2::FileExt::try_lock_exclusive(&lock)?;
        let record = Record {
            version: 1,
            configuration: location.configuration.clone(),
            import_id: import_id.into(),
            nonce: uuid::Uuid::new_v4().to_string(),
            stage,
            directory: id(&super::held::directory_handle(path)?)?,
            lock: id(&lock)?,
            created,
            ownership,
        };
        let lock = platform::FileLock::held(lock);
        private_write(&marker, &serde_json::to_vec(&record)?)?;
        let (_, marker) = super::held::read_body(&marker, 4096)?;
        platform::sync_dir(&root)?;
        Ok(Self {
            record,
            root,
            marker,
            keep: false,
            lock: Some(lock),
        })
    }
    pub fn retain(&mut self) {
        self.keep = true;
    }
    pub fn discard(&mut self) {
        self.keep = false;
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if !self.keep {
            let _ = erase_sidecars(&self.root, &self.record, self.marker);
        }
        self.lock = None;
    }
}
fn erase_sidecars(root: &Path, record: &Record, marker: FileIdentity) -> Result<()> {
    let (metadata, lock) = paths(root, &record.stage);
    let (_, identity) = super::held::read_body(&metadata, 4096)?;
    ensure!(
        identity == marker && id(&file(&lock, false)?)? == record.lock,
        "stage ownership replaced"
    );
    fs::remove_file(metadata)?;
    fs::remove_file(lock)?;
    Ok(())
}
struct Proof {
    record: Record,
    marker: FileIdentity,
    _lock: platform::FileLock,
}
fn proof(location: &Location, root: &Path, name: &str, now: u64) -> Result<Proof> {
    let (marker, lock) = paths(root, name);
    let (bytes, identity) = super::held::read_body(&marker, 4096)?;
    let record: Record = serde_json::from_slice(&bytes)?;
    ensure!(
        record.version == 1
            && record.configuration == location.configuration
            && record.stage == name
            && canonical_uuid(&record.import_id)
            && canonical_uuid(&record.nonce)
            && now
                .checked_sub(record.created)
                .is_some_and(|age| age >= AGE),
        "unverified or fresh stage"
    );
    match &record.ownership {
        Kind::Input => ensure!(leaf(name, ".stage-"), "invalid input stage"),
        Kind::Claim { source, .. } => ensure!(
            leaf(name, ".held-import-") && leaf(source, ".stage-"),
            "invalid claimed stage"
        ),
    }
    let lock = file(&lock, false)?;
    ensure!(id(&lock)? == record.lock, "stage lease replaced");
    fs2::FileExt::try_lock_exclusive(&lock)?;
    Ok(Proof {
        record,
        marker: identity,
        _lock: platform::FileLock::held(lock),
    })
}
struct Entry {
    path: PathBuf,
    identity: FileIdentity,
    directory: bool,
}
fn inspect(path: &Path, directory: bool) -> Result<FileIdentity> {
    if directory {
        return Ok(FileIdentity::of(&super::held::directory_handle(path)?)?);
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    platform::no_follow(&mut options);
    #[cfg(windows)]
    platform::refuse_link(&mut options);
    let file = options.open(path)?;
    ensure!(platform::verify_private_file(&file)?, "unsafe staged data");
    Ok(FileIdentity::of(&file)?)
}
fn data(path: &Path, expected: [u64; 2], out: &mut Vec<Entry>, bytes: &mut u64) -> Result<()> {
    let directory = super::held::directory_handle(path)?;
    ensure!(id(&directory)? == expected, "stage directory replaced");
    for entry in fs::read_dir(path)?.take(4) {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("invalid stage name"))?;
        let path = entry.path();
        if matches!(name.as_str(), "snapshots" | "pages") {
            let identity = inspect(&path, true)?;
            for (index, child) in fs::read_dir(&path)?.take(1025).enumerate() {
                ensure!(index < 1024, "stage directory limit");
                let child = child?;
                let name = child
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("invalid stage name"))?;
                ensure!(
                    name.len() == 69
                        && name.ends_with(".json")
                        && name[..64]
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                    "unknown staged data"
                );
                add_file(child.path(), MAX_SNAPSHOT_BYTES, out, bytes)?;
            }
            ensure!(inspect(&path, true)? == identity, "stage directory changed");
            out.push(Entry {
                path,
                identity,
                directory: true,
            });
        } else {
            ensure!(name == "record-v1.json", "unknown staged data");
            add_file(path, MAX_IMPORT_RECORD_BYTES as u64, out, bytes)?;
        }
        ensure!(out.len() <= FILES, "stage file limit");
    }
    ensure!(
        FileIdentity::of(&directory)? == inspect(path, true)?,
        "stage directory changed"
    );
    out.push(Entry {
        path: path.into(),
        identity: FileIdentity::of(&directory)?,
        directory: true,
    });
    Ok(())
}
fn add_file(path: PathBuf, limit: u64, out: &mut Vec<Entry>, bytes: &mut u64) -> Result<()> {
    let identity = inspect(&path, false)?;
    let size = fs::symlink_metadata(&path)?.len();
    *bytes = bytes.checked_add(size).context("stage byte overflow")?;
    ensure!(
        size <= limit && *bytes <= MAX_BYTES && out.len() < FILES,
        "stage data limit"
    );
    out.push(Entry {
        path,
        identity,
        directory: false,
    });
    Ok(())
}
fn plan(
    root: &Path,
    owned: &Proof,
    location: &Location,
    now: u64,
) -> Result<(Vec<Entry>, Option<Proof>)> {
    let mut entries = vec![];
    let mut bytes = 0;
    let path = root.join(&owned.record.stage);
    let source = match &owned.record.ownership {
        Kind::Input => {
            match fs::symlink_metadata(&path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => (), // only native orphan sidecars
                Err(error) => return Err(error.into()),
                Ok(_) => data(&path, owned.record.directory, &mut entries, &mut bytes)?,
            }
            None
        }
        Kind::Claim { source, directory } => {
            let pin = super::held::directory_handle(&path)?;
            ensure!(
                id(&pin)? == owned.record.directory,
                "claimed directory replaced"
            );
            let names: Vec<_> = fs::read_dir(&path)?.take(2).collect::<io::Result<_>>()?;
            let source = if names.is_empty() {
                None
            } else {
                ensure!(
                    names.len() == 1 && names[0].file_name() == "input",
                    "unknown claimed data"
                );
                ensure!(
                    fs::symlink_metadata(root.join(source))
                        .is_err_and(|e| e.kind() == io::ErrorKind::NotFound),
                    "source stage still present"
                );
                let original = match proof(location, root, source, now) {
                    Ok(original) => {
                        ensure!(
                            matches!(original.record.ownership, Kind::Input)
                                && original.record.directory == *directory
                                && original.record.import_id == owned.record.import_id,
                            "unverified source stage"
                        );
                        Some(original)
                    }
                    Err(error)
                        if error
                            .downcast_ref::<io::Error>()
                            .is_some_and(|e| e.kind() == io::ErrorKind::NotFound)
                            && fs::symlink_metadata(paths(root, source).0)
                                .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
                    {
                        None
                    }
                    Err(error) => return Err(error),
                };
                data(&path.join("input"), *directory, &mut entries, &mut bytes)?;
                original
            };
            entries.push(Entry {
                path,
                identity: FileIdentity::of(&pin)?,
                directory: true,
            });
            source
        }
    };
    Ok((entries, source))
}

fn remove(entries: Vec<Entry>, root: &Path, key: [u64; 2]) -> Result<()> {
    let directories: BTreeMap<_, _> = entries
        .iter()
        .filter(|entry| entry.directory)
        .map(|entry| {
            (
                entry.path.clone(),
                [entry.identity.device, entry.identity.inode],
            )
        })
        .collect();
    for entry in entries {
        ensure!(
            id(&super::held::directory_handle(root)?)? == key,
            "import root replaced"
        );
        let mut path = entry.path.parent();
        let mut parents = vec![];
        while let Some(parent) = path {
            if parent == root {
                break;
            }
            parents.push(parent);
            path = parent.parent();
        }
        ensure!(path == Some(root), "stage parent escaped");
        for parent in parents.into_iter().rev() {
            ensure!(
                directories.get(parent) == Some(&id(&super::held::directory_handle(parent)?)?),
                "stage parent replaced"
            );
        }
        let current = inspect(&entry.path, entry.directory)?;
        ensure!(
            if entry.directory {
                current.device == entry.identity.device && current.inode == entry.identity.inode
            } else {
                current == entry.identity
            },
            "staged data replaced"
        );
        if entry.directory {
            fs::remove_dir(entry.path)?
        } else {
            fs::remove_file(entry.path)?
        }
    }
    Ok(())
}
/// Explicit cleanup by the still-held native creator after an exact reply.
/// Cleanup failure preserves the copy and cannot change publication certainty.
pub(super) fn discard_input(path: &Path, expected: [u64; 2]) -> Result<()> {
    let root = path.parent().context("missing stage parent")?;
    let key = id(&super::held::directory_handle(root)?)?;
    let mut entries = vec![];
    data(path, expected, &mut entries, &mut 0)?;
    remove(entries, root, key)
}

#[derive(Default, Debug)]
pub(super) struct Swept {
    pub removed: usize,
    pub kept: usize,
    pub incomplete: bool,
}
pub(super) fn sweep(location: &Location, lock: &super::super::store::HandoffLock) -> Result<Swept> {
    sweep_at(location, lock, stamp()?)
}
fn sweep_at(
    location: &Location,
    lock: &super::super::store::HandoffLock,
    now: u64,
) -> Result<Swept> {
    lock.verify(location)?;
    location.check_existing()?;
    let root = location.root.join("imports");
    let pin = super::held::directory_handle(&root)?;
    let key = id(&pin)?;
    let mut result = Swept::default();
    let until = Instant::now() + Duration::from_millis(100);
    let mut directory = fs::read_dir(&root)?;
    for _ in 0..ENTRIES {
        if result.removed >= REMOVE || Instant::now() >= until {
            result.incomplete = true;
            break;
        }
        let Some(entry) = directory.next() else {
            return Ok(result);
        };
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str().and_then(|name| name.strip_suffix(OWNER)) else {
            continue;
        };
        if !leaf(name, ".stage-") && !leaf(name, ".held-import-") {
            continue;
        }
        let attempt = (|| -> Result<()> {
            let proof = proof(location, &root, name, now)?;
            let (entries, source) = plan(&root, &proof, location, now)?;
            ensure!(
                id(&super::held::directory_handle(&root)?)? == key,
                "import root replaced"
            );
            remove(entries, &root, key)?;
            erase_sidecars(&root, &proof.record, proof.marker)?;
            if let Some(source) = source {
                erase_sidecars(&root, &source.record, source.marker)?;
            }
            Ok(())
        })();
        if attempt.is_ok() {
            result.removed += 1
        } else {
            result.kept += 1
        }
    }
    result.incomplete = true;
    Ok(result)
}

#[cfg(test)]
mod tests;
