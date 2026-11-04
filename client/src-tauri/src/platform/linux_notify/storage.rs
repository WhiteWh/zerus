//! Private dormant filesystem adapter for the existing supplied-data policy.
//! No source/current receipt, native caller, worker, effect or resource refund.
//! The reviewed fixture caller supplies a freshly independently opened root.
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::Read,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::PathBuf,
};
use zerus_client_core::{
    linux_notify::owned::{
        data::transitions::{Disposition, Image, Model, WriteOutcome, WritePhase, WritePlan},
        inspection::{inspect_selected, Inputs},
    },
    linux_notify::FILE_LIMIT,
    native_notice::Owner,
};

const NAMES: [&str; 3] = [
    "linux-notifications-owned.json",
    "linux-update-notifications-owned.json",
    "linux-notifications.json",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refused {
    Io,
    Unsafe,
    Changed,
    Limit,
    Policy,
    Stopped,
}
type Result<T> = std::result::Result<T, Refused>;
fn io<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|_| Refused::Io)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn uid() -> u32 {
    // SAFETY: geteuid has no preconditions and retains no resource.
    unsafe { libc::geteuid() }
}

#[derive(PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    links: u64,
    len: u64,
    changed: (i64, i64),
    modified: (i64, i64),
}
impl Identity {
    fn of(meta: &Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            uid: meta.uid(),
            gid: meta.gid(),
            mode: meta.mode(),
            links: meta.nlink(),
            len: meta.len(),
            changed: (meta.ctime(), meta.ctime_nsec()),
            modified: (meta.mtime(), meta.mtime_nsec()),
        }
    }
}
#[derive(PartialEq, Eq)]
struct Snapshot {
    identity: Identity,
    bytes: Vec<u8>,
    sha256: String,
}
type Pair = [Option<Snapshot>; 3];

struct Root {
    file: File,
    anchor: PathBuf,
    dev: u64,
    ino: u64,
    gid: u32,
}
impl Root {
    fn new(file: File) -> Result<Self> {
        let meta = io(file.metadata())?;
        let root = Self {
            anchor: PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd())),
            dev: meta.dev(),
            ino: meta.ino(),
            gid: meta.gid(),
            file,
        };
        root.check()?;
        // SAFETY: this owned directory descriptor stays open through all I/O.
        // flock is OFD-scoped: duplicates can share/reacquire this lock. Only
        // independently opened fixture roots have the contention guarantee.
        if unsafe { libc::flock(root.file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(Refused::Unsafe);
        }
        root.check()?;
        Ok(root)
    }
    fn check(&self) -> Result<()> {
        let meta = io(self.file.metadata())?;
        let anchored = io(fs::metadata(&self.anchor))?;
        for value in [&meta, &anchored] {
            if !value.is_dir()
                || value.uid() != uid()
                || value.gid() != self.gid
                || value.mode() & 0o7777 != 0o700
                || (value.dev(), value.ino()) != (self.dev, self.ino)
            {
                return Err(Refused::Unsafe);
            }
        }
        // SAFETY: fcntl reads flags on our live descriptor, with no extra arg.
        let flags = unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_GETFD) };
        if flags < 0 || flags & libc::FD_CLOEXEC == 0 {
            return Err(Refused::Unsafe);
        }
        let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: the pointer is writable for one statfs; success initializes it.
        if unsafe { libc::fstatfs(self.file.as_raw_fd(), info.as_mut_ptr()) } != 0 {
            return Err(Refused::Io);
        }
        // SAFETY: the successful fstatfs above initialized info.
        let info = unsafe { info.assume_init() };
        if ![
            libc::EXT4_SUPER_MAGIC,
            libc::BTRFS_SUPER_MAGIC,
            libc::XFS_SUPER_MAGIC,
            libc::TMPFS_MAGIC,
            libc::OVERLAYFS_SUPER_MAGIC,
        ]
        .contains(&info.f_type)
        {
            return Err(Refused::Unsafe);
        }
        Ok(())
    }
    fn leaf(&self, index: usize) -> PathBuf {
        self.anchor.join(NAMES[index])
    }
    fn read(&self, index: usize, remaining: usize) -> Result<Option<Snapshot>> {
        self.check()?;
        let path = self.leaf(index);
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Refused::Io),
        };
        let meta = io(file.metadata())?;
        if !meta.is_file()
            || meta.uid() != uid()
            || meta.nlink() != 1
            || meta.mode() & 0o7777 != 0o600
        {
            return Err(Refused::Unsafe);
        }
        if meta.len() > remaining as u64 {
            return Err(Refused::Limit);
        }
        let identity = Identity::of(&meta);
        let mut bytes = Vec::new();
        io((&mut file)
            .take(remaining as u64 + 1)
            .read_to_end(&mut bytes))?;
        if bytes.len() > remaining || bytes.len() as u64 != meta.len() {
            return Err(Refused::Limit);
        }
        if Identity::of(&io(file.metadata())?) != identity
            || Identity::of(&io(fs::symlink_metadata(&path))?) != identity
        {
            return Err(Refused::Changed);
        }
        Ok(Some(Snapshot {
            identity,
            sha256: hash(&bytes),
            bytes,
        }))
    }
    fn scan(&self) -> Result<Pair> {
        self.check()?;
        let mut pair: Pair = [None, None, None];
        let mut remaining = FILE_LIMIT as usize;
        for (index, slot) in pair.iter_mut().enumerate() {
            *slot = self.read(index, remaining)?;
            remaining -= slot.as_ref().map_or(0, |image| image.bytes.len());
        }
        self.check()?;
        for (index, slot) in pair.iter().enumerate() {
            match (slot, fs::symlink_metadata(self.leaf(index))) {
                (Some(snapshot), Ok(meta)) if snapshot.identity == Identity::of(&meta) => {}
                (None, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err(Refused::Changed),
            }
        }
        self.check()?;
        // Sequential scans are not an atomic snapshot/CAS and cannot exclude
        // transient/noncooperating same-UID writes. No current proof is returned.
        Ok(pair)
    }
}
// File Drop closes only this descriptor. Never LOCK_UN: an OFD alias can retain
// the lock/custody. File ownership or close alone proves neither quiet nor join.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    BeforePlan,
    BeforeHelper,
    AfterHelper,
    BeforeDirSync,
    AfterDirSync,
    BeforeReadback,
    BeforeAck,
}
#[cfg(test)]
#[derive(Default)]
struct Hooks {
    fail: Option<Stage>,
    event: Option<fn(Stage)>,
    unknown_before_saved: bool,
}
struct Storage {
    root: Root,
    images: Pair,
    model: Model,
    refused: bool,
    #[cfg(test)]
    hooks: Hooks,
}
fn bytes(image: &Option<Snapshot>) -> Option<&[u8]> {
    image.as_ref().map(|image| image.bytes.as_slice())
}
fn matches(image: &Option<Snapshot>, expected: &Image) -> bool {
    image.as_ref().map(|image| image.sha256.as_str()) == expected.sha256()
        && image.as_ref().map(|image| image.bytes.len()) == expected.bytes()
}
fn selected(owner: Owner) -> usize {
    match owner {
        Owner::AgentAttention => 0,
        Owner::SoftwareUpdate => 1,
    }
}
impl Storage {
    fn load(file: File, host: String) -> Result<Self> {
        let root = Root::new(file)?;
        let images = root.scan()?;
        let inspection = inspect_selected(Inputs {
            agent: bytes(&images[0]),
            update: bytes(&images[1]),
            legacy: bytes(&images[2]),
        })
        .map_err(|_| Refused::Policy)?;
        let model = Model::from_selected(inspection, host).map_err(|_| Refused::Policy)?;
        Ok(Self {
            root,
            images,
            model,
            refused: false,
            #[cfg(test)]
            hooks: Hooks::default(),
        })
    }
    fn stage(&self, stage: Stage) -> Result<()> {
        #[cfg(test)]
        {
            if let Some(event) = self.hooks.event {
                event(stage);
            }
            if self.hooks.fail == Some(stage) {
                return Err(Refused::Io);
            }
        }
        let _ = stage;
        Ok(())
    }
    fn unchanged(&self) -> Result<()> {
        if self.root.scan()? != self.images {
            return Err(Refused::Changed);
        }
        Ok(())
    }
    fn commit(&self, plan: &WritePlan) -> Result<Pair> {
        self.unchanged()?;
        let attempt = plan.attempt();
        let index = selected(attempt.owner());
        let payload = plan.bytes().ok_or(Refused::Policy)?;
        if !matches(&self.images[index], attempt.before())
            || attempt.after().bytes() != Some(payload.len())
            || attempt.after().sha256() != Some(hash(payload).as_str())
        {
            return Err(Refused::Policy);
        }
        // Model capacity includes its canonical projections. Untouched files
        // retain their ORIGINAL raw lengths (including padding), so check the
        // next physical pair independently before replacing even one file.
        let mut remaining = FILE_LIMIT as usize;
        for (other, image) in self.images.iter().enumerate() {
            if other != index {
                remaining = remaining
                    .checked_sub(image.as_ref().map_or(0, |image| image.bytes.len()))
                    .ok_or(Refused::Limit)?;
            }
        }
        if payload.len() > remaining {
            return Err(Refused::Limit);
        }
        self.stage(Stage::BeforeHelper)?;
        self.root.check()?;
        // This existing helper's create_dir_all only verifies our existing
        // anchored directory. It never looks up/creates the original root name.
        io(zerus_client_core::fsx::write_private_atomic(
            &self.root.leaf(index),
            payload,
            true,
        ))?;
        self.stage(Stage::AfterHelper)?;
        self.root.check()?;
        self.stage(Stage::BeforeDirSync)?;
        io(self.root.file.sync_all())?;
        self.stage(Stage::AfterDirSync)?;
        self.stage(Stage::BeforeReadback)?;
        let after = self.root.scan()?;
        for (other, image) in after.iter().enumerate() {
            if other == index {
                if !matches(image, attempt.after()) {
                    return Err(Refused::Changed);
                }
            } else if image != &self.images[other] {
                return Err(Refused::Changed);
            }
        }
        // Live-chain checks intentionally inspect raw images, not the restart
        // policy: durable barriers/invalidations must still refuse a NEW load.
        Ok(after)
    }
    fn unknown(&mut self, plan: &mut WritePlan) {
        let outcome = WriteOutcome {
            attempt: plan.attempt(),
            disposition: Disposition::Unknown,
        };
        let _ = self.model.ack_write(plan, outcome);
        self.refused = true;
    }
    fn write_next(&mut self) -> Result<Option<(Owner, WritePhase)>> {
        if self.refused {
            return Err(Refused::Stopped);
        }
        if let Err(error) = self
            .stage(Stage::BeforePlan)
            .and_then(|()| self.unchanged())
        {
            self.refused = true;
            return Err(error);
        }
        let mut plan = match self.model.next_write() {
            Ok(Some(plan)) => plan,
            Ok(None) => return Ok(None),
            Err(_) => {
                self.refused = true;
                return Err(Refused::Policy);
            }
        };
        let mut after = match self.commit(&plan).and_then(|images| {
            self.stage(Stage::BeforeAck)?;
            self.root.check()?;
            Ok(images)
        }) {
            Ok(images) => images,
            Err(error) => {
                self.unknown(&mut plan);
                return Err(error);
            }
        };
        #[cfg(test)]
        if self.hooks.unknown_before_saved {
            self.unknown(&mut plan);
        }
        let attempt = plan.attempt();
        let step = (attempt.owner(), attempt.phase());
        if self
            .model
            .ack_write(
                &mut plan,
                WriteOutcome {
                    attempt,
                    disposition: Disposition::Saved,
                },
            )
            .is_err()
        {
            self.unknown(&mut plan);
            return Err(Refused::Policy);
        }
        // Only the selected snapshot changed, and ONLY after exact Saved ack.
        self.images[selected(step.0)] = after[selected(step.0)].take();
        Ok(Some(step))
    }
}

#[cfg(test)]
mod tests;
