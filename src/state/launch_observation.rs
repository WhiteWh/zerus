//! Bounded, read-only evidence about one original creation launch, for the
//! desktop actor's lookup of a launch whose original result is Unknown.
//!
//! Narrowed from the dormant target-side reader of #850 (zerus-5wmh.66.4.3.4):
//! the actor supplies its verified state root and local machine, so there is
//! no CLI entry, argument probe, help capability, cwd syscall or environment
//! root resolution. What stays is its closed source subset: the launch's own
//! start and end records and, for a started launch, the binding record of the
//! start's exact session name. Every directory is pinned with no-follow walks
//! and must be private; every file must be a private single-link regular file
//! within its byte cap; JSON with duplicate keys is refused; open, metadata
//! and byte budgets and a deadline are hard limits; and the whole selected set
//! is read again and must be unchanged. Any fault, bound, missing record,
//! present name route or conflict is `Unknown`.
//!
//! The result is display evidence only. It never settles a launch, releases
//! a charge, rotates a launch ID or authorizes another launch.
use std::path::Path;
use std::time::Instant;
use zerus_client_core::desktop::creation_launch::Observation;

/// Evidence about original launch `launch` under the state root `state`, read
/// within `deadline`. The caller runs it off its actor thread.
// Called by the launch actor slice; until then only Unix tests use it.
#[cfg_attr(any(not(test), not(unix)), allow(dead_code))]
pub(crate) fn observe(state: &Path, launch: &str, deadline: Instant) -> Observation {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    return unix::observe(state, launch, deadline);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (state, launch, deadline);
        Observation::Unknown
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TeamRecord {
    pub launch_id: String,
    pub name: String,
    pub agent: String,
    pub account: String,
    pub session_id: String,
    pub run_id: String,
    pub conversation_id: String,
    pub cwd: String,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub provider: Option<Box<super::team_provider_stop::ProviderObservation>>,
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TeamObservation {
    Record(TeamRecord),
    Absent { legacy: bool },
    Ended { code: i32 },
    Mismatch { field: String },
    Unavailable,
}
/// Bounded identity facts for Teams, including a record at a trust prompt.
/// This is not the strict creation result and never settles a CLI outcome.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn observe_team_launch(
    state: &Path,
    launch: &str,
    deadline: Instant,
) -> TeamObservation {
    team::observe(state, launch, deadline)
}
/// What a team member's durable handle, its session's own `session_id`,
/// resolves to now (zerus-63rru.53). Unlike the launch's evidence, which is
/// gone once the first run ends or after a week, the handle lasts as long
/// as the session's record.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TeamSession {
    /// The live or saved record that carries the handle and the bound launch.
    Record(TeamRecord),
    /// No live or saved session carries the handle: it was archived or
    /// forgotten, or an older hgs rewrote its record without the ID.
    Gone,
    /// The session that carries the handle names another launch's ID, or
    /// none without having started fresh.
    LaunchChanged,
    /// The session started fresh (`hgs resume --fresh`): its record keeps
    /// the session ID but no launch ID. A fresh conversation is another
    /// executor of tasks, so the binding ends here.
    StartedFresh,
    /// A fault, a bound or the deadline; another read may answer.
    Unavailable,
}
/// The session whose durable ID is `session`, bound to launch `launch`:
/// its own record in the ID layout, else the ID index's label, else the
/// label `hint` (the name at binding) with at most three rename hops. Every
/// record read must carry the ID; the answer is a record only when it also
/// carries `launch`. Bounded and pinned like [`observe_team_launch`].
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn observe_team_session(
    state: &Path,
    session: &str,
    launch: &str,
    agent: &str,
    hint: &str,
    deadline: Instant,
) -> TeamSession {
    team::session(state, session, launch, agent, hint, deadline, false)
}
/// The availability scan reads the main-turn provider evidence with the
/// same pinned identity. Other Teams readers need only that identity.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn observe_team_session_for_availability(
    state: &Path,
    session: &str,
    launch: &str,
    agent: &str,
    hint: &str,
    deadline: Instant,
) -> TeamSession {
    team::session(state, session, launch, agent, hint, deadline, true)
}
/// Resolve an exact run using its bounded catalog route and the same pinned
/// record reader as launch-bound provider stops.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn observe_team_run_for_provider(
    state: &Path,
    run: &str,
    session: &str,
    agent: &str,
    hint: &str,
    deadline: Instant,
) -> TeamSession {
    team::run(state, run, session, agent, hint, deadline)
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod team;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix {
    use super::*;
    use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
    pub(super) use serde_json::Value;
    pub(super) use sha2::{Digest, Sha256};
    use std::collections::BTreeSet;
    use std::fs::{self, File, Metadata};
    use std::io::{self, Read};
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    };
    use std::path::Component;
    pub(super) use std::path::PathBuf;

    pub(super) fn canonical(value: &str) -> bool {
        uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
    }

    pub(super) const RAW: usize = 1 << 20;
    // Conservative reservations, not measured syscall counts. Exact1.99.1 can
    // perform statx + availability probe + legacy stat, and audited libc statx
    // can add fstatat on ENOSYS. Further target paths withhold qualification.
    const META_COST: usize = 5;
    pub(super) const SMALL: usize = 8 << 10;
    #[derive(Clone, Copy, Debug)]
    pub(super) struct Fault;
    pub(super) type Checked<T> = std::result::Result<T, Fault>;

    #[derive(Default)]
    pub(super) struct Counts {
        pub(super) walks: usize,
        pub(super) opens: usize,
        pub(super) metadata: usize,
        /// The observation refuses once this passes; checked before each open
        /// and metadata reservation.
        pub(super) deadline: Option<Instant>,
    }
    impl Counts {
        fn walk(&mut self, path: &Path) -> Checked<()> {
            let cost = path
                .components()
                .filter(|c| matches!(c, Component::Normal(_)))
                .count()
                + 1;
            if cost > 33 || self.walks >= 16 {
                return Err(Fault);
            }
            self.open(cost)?;
            self.walks += 1;
            Ok(())
        }
        fn open(&mut self, cost: usize) -> Checked<()> {
            if self.deadline.is_some_and(|d| Instant::now() >= d) || cost > 1024 - self.opens {
                return Err(Fault);
            }
            self.opens += cost;
            Ok(())
        }
        fn meta(&mut self, cost: usize) -> Checked<()> {
            if self.deadline.is_some_and(|d| Instant::now() >= d) || cost > 4096 - self.metadata {
                return Err(Fault);
            }
            self.metadata += cost;
            Ok(())
        }
    }
    #[derive(PartialEq, Eq)]
    pub(super) struct Stamp {
        device: u64,
        inode: u64,
        mode: u32,
        uid: u32,
        gid: u32,
        links: u64,
        length: u64,
        modified: (i64, i64),
        changed: (i64, i64),
    }
    impl Stamp {
        fn of(meta: &Metadata) -> Self {
            Self {
                device: meta.dev(),
                inode: meta.ino(),
                mode: meta.mode(),
                uid: meta.uid(),
                gid: meta.gid(),
                links: meta.nlink(),
                length: meta.len(),
                modified: (meta.mtime(), meta.mtime_nsec()),
                changed: (meta.ctime(), meta.ctime_nsec()),
            }
        }
    }
    pub(super) fn path_stamp(path: &Path, counts: &mut Counts) -> Checked<Option<Stamp>> {
        valid_path(path)?;
        counts.meta(META_COST)?;
        match fs::symlink_metadata(path) {
            Ok(meta) => Ok(Some(Stamp::of(&meta))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(Fault),
        }
    }
    fn file_stamp(file: &File, counts: &mut Counts) -> Checked<Stamp> {
        counts.meta(META_COST)?;
        file.metadata()
            .map(|meta| Stamp::of(&meta))
            .map_err(|_| Fault)
    }
    // Existing Directory's read-only pattern, private to this command. Its
    // generic OpenOptions root open retries EINTR; no such wrapper is used here.
    struct ReadDirectory {
        path: PathBuf,
        file: File,
    }
    fn adopted(fd: libc::c_int) -> Checked<File> {
        if fd < 0 {
            return Err(Fault);
        }
        // SAFETY: every caller passes its one newly acquired owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    fn component(name: &std::ffi::OsStr) -> Checked<std::ffi::CString> {
        let bytes = name.as_encoded_bytes();
        if bytes.is_empty() || bytes.len() > 4096 || bytes.contains(&0) {
            return Err(Fault);
        }
        // Bounds/NUL were checked before this owned copy.
        std::ffi::CString::new(bytes).map_err(|_| Fault)
    }
    impl ReadDirectory {
        fn open(path: &Path, counts: &mut Counts) -> Checked<Self> {
            valid_path(path)?;
            // Reserve EVERY potential open before beginning this finite walk.
            counts.walk(path)?;
            let flags = libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK
                | libc::O_NOCTTY
                | libc::O_CLOEXEC;
            // SAFETY: fixed absolute root, read-only flags, no variadic mode.
            let mut file = adopted(unsafe { libc::open(c"/".as_ptr(), flags) })?;
            for part in path.components() {
                match part {
                    Component::RootDir => (),
                    Component::Normal(name) => {
                        let name = component(name)?;
                        // SAFETY: owned live parent and one bounded NUL-free
                        // component. EINTR/error refuses, with NO retry/fallback.
                        file = adopted(unsafe {
                            libc::openat(file.as_raw_fd(), name.as_ptr(), flags)
                        })?;
                    }
                    _ => return Err(Fault),
                }
            }
            Ok(Self {
                path: path.into(),
                file,
            })
        }
        fn private(&self, counts: &mut Counts) -> Checked<bool> {
            // The existing predicate has one metadata API and a possible eUID
            // query. Charge both BEFORE calling, even if it short-circuits.
            counts.meta(META_COST + 1)?;
            crate::platform::verify_private_directory(&self.file).map_err(|_| Fault)
        }
        fn verify(&self, counts: &mut Counts) -> Checked<()> {
            let current = Self::open(&self.path, counts)?;
            let held = file_stamp(&self.file, counts)?;
            let now = file_stamp(&current.file, counts)?;
            if (held.device, held.inode) != (now.device, now.inode) {
                return Err(Fault);
            }
            Ok(())
        }
        fn read_private(&self, leaf: &str, counts: &mut Counts) -> Checked<File> {
            if leaf.is_empty() || leaf.contains(['/', '\\', ':']) || matches!(leaf, "." | "..") {
                return Err(Fault);
            }
            let name = component(std::ffi::OsStr::new(leaf))?;
            counts.open(1)?;
            // SAFETY: held parent plus one fixed bounded leaf. No retry, no
            // std open wrapper, no create, and immediate owned FD adoption.
            let file = adopted(unsafe {
                libc::openat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY
                        | libc::O_NOFOLLOW
                        | libc::O_NONBLOCK
                        | libc::O_NOCTTY
                        | libc::O_CLOEXEC,
                )
            })?;
            counts.meta(META_COST + 1)?;
            if !crate::platform::verify_private_file(&file).map_err(|_| Fault)? {
                return Err(Fault);
            }
            Ok(file)
        }
    }
    pub(super) struct Root {
        directory: ReadDirectory,
        stamp: Stamp,
    }
    impl Root {
        pub(super) fn pin(path: &Path, counts: &mut Counts) -> Checked<Self> {
            valid_path(path)?;
            let stamp = path_stamp(path, counts)?.ok_or(Fault)?;
            let directory = ReadDirectory::open(path, counts)?;
            if !directory.private(counts)? {
                return Err(Fault);
            }
            directory.verify(counts)?;
            if path_stamp(path, counts)?.as_ref() != Some(&stamp) {
                return Err(Fault);
            }
            Ok(Self { directory, stamp })
        }
        pub(super) fn finish(&self, counts: &mut Counts) -> Checked<()> {
            if !self.directory.private(counts)? {
                return Err(Fault);
            }
            self.directory.verify(counts)?;
            if path_stamp(&self.directory.path, counts)?.as_ref() != Some(&self.stamp) {
                return Err(Fault);
            }
            Ok(())
        }
        fn path(&self, leaf: &str) -> PathBuf {
            self.directory.path.join(leaf)
        }
        fn open(&self, leaf: &str, counts: &mut Counts) -> Checked<File> {
            valid_path(&self.path(leaf))?;
            self.directory.read_private(leaf, counts)
        }
    }
    pub(super) struct ByteBudget {
        remaining: usize,
    }
    impl ByteBudget {
        pub(super) fn new() -> Self {
            Self { remaining: RAW }
        }
        fn consume(&mut self, bytes: usize) -> Checked<()> {
            if bytes > self.remaining {
                return Err(Fault);
            }
            self.remaining -= bytes;
            Ok(())
        }
    }
    // One stack buffer serves both bounded initial reads and final hash reads.
    // Never retain a sentinel, retry an interrupted read or parse a prefix.
    fn bounded(
        file: &mut File,
        length: usize,
        budget: &mut ByteBudget,
        retain: bool,
    ) -> Checked<(Vec<u8>, [u8; 32])> {
        if length > budget.remaining {
            return Err(Fault);
        }
        let mut bytes = if retain {
            Vec::with_capacity(length)
        } else {
            Vec::new()
        };
        let mut hash = Sha256::new();
        let mut read = 0usize;
        let mut scratch = [0u8; SMALL];
        loop {
            let maximum = scratch.len().min(length.saturating_sub(read) + 1);
            let count = file.read(&mut scratch[..maximum]).map_err(|_| Fault)?;
            if count == 0 {
                break;
            }
            if count > length - read {
                return Err(Fault);
            }
            budget.consume(count)?;
            hash.update(&scratch[..count]);
            if retain {
                bytes.extend_from_slice(&scratch[..count]);
            }
            read += count;
        }
        if read != length {
            return Err(Fault);
        }
        Ok((bytes, hash.finalize().into()))
    }
    pub(super) struct Image {
        file: File,
        stamp: Stamp,
        bytes: Vec<u8>,
        hash: [u8; 32],
    }
    pub(super) fn capture(
        root: &Root,
        leaf: &str,
        cap: usize,
        bytes: &mut ByteBudget,
        counts: &mut Counts,
    ) -> Checked<Option<Image>> {
        let Some(stamp) = path_stamp(&root.path(leaf), counts)? else {
            return Ok(None);
        };
        if stamp.length > cap as u64 || stamp.length > bytes.remaining as u64 {
            return Err(Fault);
        }
        let mut file = root.open(leaf, counts)?;
        if file_stamp(&file, counts)? != stamp {
            return Err(Fault);
        }
        let (raw, hash) = bounded(&mut file, stamp.length as usize, bytes, true)?;
        if file_stamp(&file, counts)? != stamp
            || path_stamp(&root.path(leaf), counts)?.as_ref() != Some(&stamp)
        {
            return Err(Fault);
        }
        Ok(Some(Image {
            file,
            stamp,
            bytes: raw,
            hash,
        }))
    }
    pub(super) fn recheck(
        root: &Root,
        leaf: &str,
        image: Option<&Image>,
        bytes: &mut ByteBudget,
        counts: &mut Counts,
    ) -> Checked<()> {
        let stamp = path_stamp(&root.path(leaf), counts)?;
        let Some(image) = image else {
            return if stamp.is_none() { Ok(()) } else { Err(Fault) };
        };
        if stamp.as_ref() != Some(&image.stamp) || file_stamp(&image.file, counts)? != image.stamp {
            return Err(Fault);
        }
        let mut file = root.open(leaf, counts)?;
        if file_stamp(&file, counts)? != image.stamp {
            return Err(Fault);
        }
        let (_, hash) = bounded(&mut file, image.stamp.length as usize, bytes, false)?;
        if hash != image.hash
            || file_stamp(&file, counts)? != image.stamp
            || file_stamp(&image.file, counts)? != image.stamp
            || path_stamp(&root.path(leaf), counts)?.as_ref() != Some(&image.stamp)
        {
            return Err(Fault);
        }
        Ok(())
    }
    // Refuse duplicate JSON keys before any Value/Store can erase ambiguity.
    // This deliberately also refuses duplicate opaque fields in this reader.
    struct Unique;
    impl<'de> DeserializeSeed<'de> for Unique {
        type Value = ();
        fn deserialize<D: serde::Deserializer<'de>>(
            self,
            decoder: D,
        ) -> std::result::Result<(), D::Error> {
            decoder.deserialize_any(self)
        }
    }
    impl<'de> Visitor<'de> for Unique {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("unambiguous JSON")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> std::result::Result<(), M::Error> {
            let mut keys = BTreeSet::new();
            while let Some(key) = map.next_key::<String>()? {
                if !keys.insert(key) {
                    return Err(serde::de::Error::custom("ambiguous JSON"));
                }
                map.next_value_seed(Unique)?;
            }
            Ok(())
        }
        fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> std::result::Result<(), S::Error> {
            while seq.next_element_seed(Unique)?.is_some() {}
            Ok(())
        }
        fn visit_bool<E: serde::de::Error>(self, _: bool) -> std::result::Result<(), E> {
            Ok(())
        }
        fn visit_i64<E: serde::de::Error>(self, _: i64) -> std::result::Result<(), E> {
            Ok(())
        }
        fn visit_u64<E: serde::de::Error>(self, _: u64) -> std::result::Result<(), E> {
            Ok(())
        }
        fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<(), E> {
            Ok(())
        }
        fn visit_str<E: serde::de::Error>(self, _: &str) -> std::result::Result<(), E> {
            Ok(())
        }
        fn visit_none<E: serde::de::Error>(self) -> std::result::Result<(), E> {
            Ok(())
        }
        fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<(), E> {
            Ok(())
        }
    }
    fn unique(bytes: &[u8]) -> Checked<()> {
        let mut decoder = serde_json::Deserializer::from_slice(bytes);
        Unique.deserialize(&mut decoder).map_err(|_| Fault)?;
        decoder.end().map_err(|_| Fault)
    }
    pub(super) fn json(image: &Image) -> Checked<Value> {
        unique(&image.bytes)?;
        let value: Value = serde_json::from_slice(&image.bytes).map_err(|_| Fault)?;
        if !value.is_object()
            || ["schema", "version"]
                .iter()
                .any(|key| value.get(key).is_some_and(|v| v.as_u64() != Some(1)))
        {
            return Err(Fault);
        }
        Ok(value)
    }
    pub(super) fn text<'a>(value: &'a Value, key: &str) -> Checked<&'a str> {
        value.get(key).and_then(Value::as_str).ok_or(Fault)
    }
    /// The wire's own session-name rule, so evidence the observer produces is
    /// always valid on the wire.
    pub(super) fn name(value: &str) -> bool {
        zerus_client_core::desktop::creation_launch::session_name(value)
    }
    fn finite(value: &Value, key: &str) -> Checked<f64> {
        value
            .get(key)
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v >= 0.0)
            .ok_or(Fault)
    }
    pub(super) fn valid_path(path: &Path) -> Checked<()> {
        if !path.is_absolute()
            || path.as_os_str().len() > 4096
            || path
                .components()
                .filter(|c| matches!(c, Component::Normal(_)))
                .count()
                > 32
            || path
                .components()
                .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        {
            return Err(Fault);
        }
        Ok(())
    }
    fn selected(launch: &str, path: &Path, counts: &mut Counts) -> Checked<Observation> {
        let state = Root::pin(path, counts)?;
        let launches = Root::pin(&path.join("launches"), counts)?;
        let mut bytes = ByteBudget::new();
        let start_leaf = format!("{}.json", launch);
        let end_leaf = format!("{}.end.json", launch);
        let start = capture(&launches, &start_leaf, SMALL, &mut bytes, counts)?;
        let end = capture(&launches, &end_leaf, SMALL, &mut bytes, counts)?;
        let start_value = start.as_ref().map(json).transpose()?;
        let end_value = end.as_ref().map(json).transpose()?;
        let mut routes = None;
        let mut route_absent = None;
        let mut binding = None;
        let mut binding_leaf = None;
        if let Some(start) = &start_value {
            if text(start, "launch_id")? != launch || !name(text(start, "name")?) {
                return Err(Fault);
            }
            finite(start, "started_at")?;
            let leaf = format!("{:x}.json", Sha256::digest(text(start, "name")?.as_bytes()));
            let route_path = path.join("name_routes");
            match path_stamp(&route_path, counts)? {
                None => route_absent = Some(route_path),
                Some(_) => {
                    let root = Root::pin(&route_path, counts)?;
                    if path_stamp(&root.path(&leaf), counts)?.is_some() {
                        return Err(Fault);
                    }
                    routes = Some(root);
                }
            }
            binding = capture(&state, &leaf, RAW, &mut bytes, counts)?;
            binding_leaf = Some(leaf);
        }
        let result = if let Some(binding) = &binding {
            let value = json(binding)?;
            let original_name = text(start_value.as_ref().ok_or(Fault)?, "name")?;
            let agent = text(&value, "agent")?;
            let run = text(&value, "run_id")?;
            let conversation = text(&value, "conversation_id")?;
            if value.get("version").and_then(Value::as_u64) != Some(1)
                || text(&value, "launch_id")? != launch
                || text(&value, "name")? != original_name
                || !matches!(agent, "claude" | "codex")
                || original_name.split('/').next() != Some(agent)
                || !canonical(run)
                || !canonical(conversation)
                || [
                    "expected_id",
                    "creation_pending",
                    "rename_shadow",
                    "rename",
                    "renaming",
                ]
                .iter()
                .any(|key| value.get(key).is_some())
                || end.is_some()
            {
                return Err(Fault);
            }
            Observation::ObservedBinding {
                name: original_name.into(),
                agent: agent.into(),
                run_id: run.into(),
                conversation_id: conversation.into(),
            }
        } else if let Some(end) = &end_value {
            let observed_name = text(end, "name")?;
            if text(end, "launch_id")? != launch
                || !name(observed_name)
                || end.get("draining").is_some()
                || end.get("error").is_some()
                || end.get("output").is_some_and(|v| !v.is_string())
                || start_value.as_ref().is_some_and(|start| {
                    start.get("name").and_then(Value::as_str) != Some(observed_name)
                })
            {
                return Err(Fault);
            }
            let code = end
                .get("code")
                .and_then(Value::as_i64)
                .and_then(|n| i32::try_from(n).ok())
                .ok_or(Fault)?;
            Observation::ObservedEnd {
                name: observed_name.into(),
                code,
                ended_at: finite(end, "ended_at")?,
                binding_unresolved: true,
            }
        } else {
            Observation::Unknown
        };
        let mut rechecked = ByteBudget::new();
        recheck(
            &launches,
            &start_leaf,
            start.as_ref(),
            &mut rechecked,
            counts,
        )?;
        recheck(&launches, &end_leaf, end.as_ref(), &mut rechecked, counts)?;
        if let Some(leaf) = &binding_leaf {
            recheck(&state, leaf, binding.as_ref(), &mut rechecked, counts)?;
            if let Some(root) = &routes {
                if path_stamp(&root.path(leaf), counts)?.is_some() {
                    return Err(Fault);
                }
            }
        }
        if let Some(path) = route_absent {
            if path_stamp(&path, counts)?.is_some() {
                return Err(Fault);
            }
        }
        if let Some(root) = routes {
            root.finish(counts)?;
        }
        launches.finish(counts)?;
        // Its final path_stamp is the last metadata reservation.
        state.finish(counts)?;
        #[cfg(test)]
        if let Some(hook) = AFTER_LAST_RESERVATION.with(|hook| hook.borrow_mut().take()) {
            hook();
        }
        Ok(result)
    }
    pub(super) fn observe(state: &Path, launch: &str, deadline: Instant) -> Observation {
        if !canonical(launch) {
            return Observation::Unknown;
        }
        let mut counts = Counts {
            deadline: Some(deadline),
            ..Counts::default()
        };
        let observation = selected(launch, state, &mut counts).unwrap_or(Observation::Unknown);
        // The counters check the deadline before each reservation; an
        // observation that completes after it, rechecks included, is Unknown.
        if Instant::now() >= deadline {
            return Observation::Unknown;
        }
        observation
    }
    #[cfg(test)]
    thread_local! {
        /// Runs after the last state-root metadata reservation, when every
        /// read and recheck is done, before the final deadline gate.
        static AFTER_LAST_RESERVATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;
        use std::os::unix::fs::{symlink, PermissionsExt};
        use std::time::Duration;

        fn id(n: u8) -> String {
            format!("00000000-1111-4111-8111-{n:012}")
        }
        fn private_dir(path: &Path) {
            fs::create_dir_all(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fn write(path: &Path, bytes: &[u8]) {
            fs::write(path, bytes).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        struct Fixture {
            _root: tempfile::TempDir,
            state: PathBuf,
        }
        impl Fixture {
            pub(super) fn new() -> Self {
                let root = tempfile::tempdir().unwrap();
                let state = root.path().canonicalize().unwrap().join("state");
                for path in [&state, &state.join("launches"), &state.join("name_routes")] {
                    private_dir(path);
                }
                Self { _root: root, state }
            }
            fn start_path(&self) -> PathBuf {
                self.state.join("launches").join(format!("{}.json", id(2)))
            }
            fn end_path(&self) -> PathBuf {
                self.state
                    .join("launches")
                    .join(format!("{}.end.json", id(2)))
            }
            fn leaf(&self) -> String {
                format!("{:x}.json", Sha256::digest(b"codex/fixture/original"))
            }
            fn binding_path(&self) -> PathBuf {
                self.state.join(self.leaf())
            }
            fn start(&self) {
                write(
                    &self.start_path(),
                    &serde_json::to_vec(&json!({
                        "launch_id":id(2),"name":"codex/fixture/original","started_at":1.0
                    }))
                    .unwrap(),
                );
            }
            fn binding(&self) -> Value {
                json!({
                    "version":1,"name":"codex/fixture/original","agent":"codex",
                    "launch_id":id(2),"run_id":id(3),"conversation_id":id(4),
                    "opaque":{"secret":"PRIVATE BODY"}
                })
            }
            fn end(&self) -> Value {
                json!({
                    "launch_id":id(2),"name":"codex/fixture/original","code":0,
                    "ended_at":2.0,"output":"PRIVATE NATIVE OUTPUT"
                })
            }
            fn put_binding(&self, value: &Value) {
                write(&self.binding_path(), &serde_json::to_vec(value).unwrap());
            }
            fn put_end(&self, value: &Value) {
                write(&self.end_path(), &serde_json::to_vec(value).unwrap());
            }
            fn read(&self) -> (Value, Counts) {
                let mut counts = Counts::default();
                let observation =
                    selected(&id(2), &self.state, &mut counts).unwrap_or(Observation::Unknown);
                assert!(observation.valid(), "{observation:?}");
                let encoded = serde_json::to_string(&observation).unwrap();
                assert!(!encoded.contains("PRIVATE"), "no record bodies leave");
                (serde_json::from_str(&encoded).unwrap(), counts)
            }
            fn status(&self) -> String {
                self.read().0["status"].as_str().unwrap().into()
            }
        }
        #[test]
        fn an_optional_allocated_session_in_start_evidence_changes_no_strict_result() {
            let f = Fixture::new();
            f.start();
            f.put_binding(&f.binding());
            let before = f.read().0;
            write(&f.start_path(),&serde_json::to_vec(&json!({"launch_id":id(2),"name":"codex/fixture/original","started_at":1.0,"session_id":id(8)})).unwrap());
            assert_eq!(f.read().0, before);
        }
        #[test]
        fn an_original_binding_is_closed_body_free_evidence() {
            let f = Fixture::new();
            f.start();
            f.put_binding(&f.binding());
            let (reply, counts) = f.read();
            assert_eq!(reply["status"], "observed_binding");
            assert_eq!(reply["name"], "codex/fixture/original");
            assert_eq!(reply["agent"], "codex");
            assert_eq!(reply["run_id"], id(3));
            assert_eq!(reply["conversation_id"], id(4));
            assert_eq!(reply.as_object().unwrap().len(), 5);
            assert!(counts.walks <= 16 && counts.opens <= 1024 && counts.metadata <= 4096);
            // The public entry gives the same answer within its deadline.
            assert!(matches!(
                super::super::observe(&f.state, &id(2), Instant::now() + Duration::from_secs(5)),
                Observation::ObservedBinding { .. }
            ));
        }
        #[test]
        fn an_end_is_nonexclusive_evidence_and_a_missing_start_selects_no_binding() {
            let f = Fixture::new();
            f.put_binding(&f.binding());
            assert_eq!(f.status(), "unknown");
            f.put_end(&f.end());
            let (reply, _) = f.read();
            assert_eq!(reply["status"], "observed_end");
            assert_eq!(reply["binding_unresolved"], true);
            assert_eq!(reply["code"], 0);
            assert_eq!(reply.as_object().unwrap().len(), 5);
            fs::remove_file(f.end_path()).unwrap();
            f.start();
            fs::remove_file(f.binding_path()).unwrap();
            assert_eq!(f.status(), "unknown");
        }
        #[test]
        fn a_matching_end_or_any_present_route_refuses_a_binding() {
            let f = Fixture::new();
            f.start();
            f.put_binding(&f.binding());
            f.put_end(&f.end());
            assert_eq!(f.status(), "unknown");
            fs::remove_file(f.end_path()).unwrap();
            write(
                &f.state.join("name_routes").join(f.leaf()),
                b"not even parsed",
            );
            assert_eq!(f.status(), "unknown");
        }
        #[test]
        fn a_session_name_with_internal_spaces_is_observed() {
            let f = Fixture::new();
            let name = "codex/repo/my task";
            let leaf = format!("{:x}.json", Sha256::digest(name.as_bytes()));
            write(
                &f.start_path(),
                &serde_json::to_vec(&json!({"launch_id":id(2),"name":name,"started_at":1.0}))
                    .unwrap(),
            );
            let mut binding = f.binding();
            binding["name"] = json!(name);
            write(&f.state.join(&leaf), &serde_json::to_vec(&binding).unwrap());
            let (reply, _) = f.read();
            assert_eq!(reply["status"], "observed_binding");
            assert_eq!(reply["name"], name);
            fs::remove_file(f.state.join(&leaf)).unwrap();
            let mut end = f.end();
            end["name"] = json!(name);
            f.put_end(&end);
            let (reply, _) = f.read();
            assert_eq!(reply["status"], "observed_end");
            assert_eq!(reply["name"], name);
        }
        #[test]
        fn only_claude_and_codex_bindings_are_observed() {
            // A complete, self-consistent Kimi binding: only the closed
            // provider subset refuses it.
            let f = Fixture::new();
            let name = "kimi/fixture/original";
            let leaf = format!("{:x}.json", Sha256::digest(name.as_bytes()));
            write(
                &f.start_path(),
                &serde_json::to_vec(&json!({"launch_id":id(2),"name":name,"started_at":1.0}))
                    .unwrap(),
            );
            let mut binding = f.binding();
            binding["name"] = json!(name);
            binding["agent"] = json!("kimi");
            write(&f.state.join(&leaf), &serde_json::to_vec(&binding).unwrap());
            assert_eq!(f.status(), "unknown");
        }
        #[test]
        fn other_providers_identities_pending_and_future_binding_fields_refuse() {
            let f = Fixture::new();
            f.start();
            for (key, value) in [
                ("agent", json!("kimi")),
                ("name", json!("codex/fixture/reused")),
                ("launch_id", json!(id(8))),
                ("run_id", Value::Null),
                ("conversation_id", json!("ses_not_a_codex_id")),
                ("version", json!(2)),
                ("expected_id", Value::Null),
                ("creation_pending", json!(false)),
                ("rename_shadow", json!(false)),
                ("rename", json!({})),
            ] {
                let mut binding = f.binding();
                binding[key] = value;
                f.put_binding(&binding);
                assert_eq!(f.status(), "unknown", "{key}");
            }
        }
        #[test]
        fn an_end_requires_the_completed_encoding_and_valid_fields() {
            let f = Fixture::new();
            for (key, value) in [
                ("name", Value::Null),
                ("name", json!("")),
                ("code", json!(2147483648_i64)),
                ("code", json!(1.0)),
                ("code", Value::Null),
                ("ended_at", json!(-1)),
                ("ended_at", Value::Null),
                ("launch_id", json!(id(8))),
                ("draining", json!(false)),
                ("draining", Value::Null),
                ("error", json!("")),
                ("error", Value::Null),
                ("output", json!({})),
            ] {
                let mut end = f.end();
                end[key] = value;
                f.put_end(&end);
                assert_eq!(f.status(), "unknown", "{key}");
            }
            f.start();
            let mut end = f.end();
            end["name"] = json!("codex/fixture/other");
            f.put_end(&end);
            assert_eq!(f.status(), "unknown");
        }
        #[test]
        fn duplicate_keys_and_per_source_or_combined_bounds_refuse() {
            let f = Fixture::new();
            f.start();
            let raw = serde_json::to_string(&f.binding()).unwrap().replacen(
                "\"run_id\":",
                "\"run_id\":null,\"run_id\":",
                1,
            );
            write(&f.binding_path(), raw.as_bytes());
            assert_eq!(f.status(), "unknown");
            let mut raw = serde_json::to_vec(&f.binding()).unwrap();
            raw.resize(RAW, b' ');
            write(&f.binding_path(), &raw);
            assert_eq!(f.status(), "unknown");
            fs::remove_file(f.binding_path()).unwrap();
            write(&f.end_path(), &vec![b' '; SMALL + 1]);
            assert_eq!(f.status(), "unknown");
            assert!(unique(br#"{"opaque":{"x":1,"x":2}}"#).is_err());
        }
        #[test]
        fn links_shared_files_open_permissions_directories_and_fifos_never_become_a_binding() {
            let f = Fixture::new();
            f.start();
            f.put_binding(&f.binding());
            fs::set_permissions(f.binding_path(), fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(f.status(), "unknown");
            fs::set_permissions(f.binding_path(), fs::Permissions::from_mode(0o600)).unwrap();
            let other = f.state.join("other");
            fs::hard_link(f.binding_path(), &other).unwrap();
            assert_eq!(f.status(), "unknown");
            fs::remove_file(&other).unwrap();
            fs::rename(f.binding_path(), &other).unwrap();
            symlink(&other, f.binding_path()).unwrap();
            assert_eq!(f.status(), "unknown");
            fs::remove_file(f.binding_path()).unwrap();
            private_dir(&f.binding_path());
            assert_eq!(f.status(), "unknown");
            fs::remove_dir(f.binding_path()).unwrap();
            use std::os::unix::ffi::OsStrExt;
            let fifo = std::ffi::CString::new(f.binding_path().as_os_str().as_bytes()).unwrap();
            // SAFETY: the fixture owns this NUL-terminated path; no existing entry.
            assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
            assert_eq!(f.status(), "unknown");
            fs::remove_file(f.binding_path()).unwrap();
            fs::set_permissions(&f.state, fs::Permissions::from_mode(0o755)).unwrap();
            f.put_binding(&f.binding());
            assert_eq!(f.status(), "unknown", "the state root must be private");
        }
        #[test]
        fn a_replaced_file_or_held_root_refuses_the_recheck() {
            let f = Fixture::new();
            f.start();
            f.put_binding(&f.binding());
            let mut counts = Counts::default();
            let root = Root::pin(&f.state, &mut counts).unwrap();
            let image = capture(&root, &f.leaf(), RAW, &mut ByteBudget::new(), &mut counts)
                .unwrap()
                .unwrap();
            let mut changed = f.binding();
            changed["opaque"] = json!("changed");
            f.put_binding(&changed);
            assert!(recheck(
                &root,
                &f.leaf(),
                Some(&image),
                &mut ByteBudget::new(),
                &mut counts
            )
            .is_err());
            let moved = f.state.with_extension("moved");
            fs::rename(&f.state, &moved).unwrap();
            private_dir(&f.state);
            assert!(root.finish(&mut counts).is_err());
        }
        #[test]
        fn path_limits_refuse_before_metadata_or_open() {
            let mut counts = Counts::default();
            let huge = PathBuf::from(format!("/{}", "x".repeat(4096)));
            assert!(path_stamp(&huge, &mut counts).is_err());
            assert_eq!((counts.metadata, counts.opens), (0, 0));
            let deep = PathBuf::from(format!("/{}", vec!["x"; 33].join("/")));
            assert!(path_stamp(&deep, &mut counts).is_err());
            assert_eq!((counts.metadata, counts.opens), (0, 0));
            let f = Fixture::new();
            let root = Root::pin(&f.state, &mut counts).unwrap();
            let before = counts.opens;
            assert!(root.open(&"x".repeat(4096), &mut counts).is_err());
            assert_eq!(counts.opens, before);
            assert!(valid_path(Path::new("/safe/../other")).is_err());
            assert!(valid_path(Path::new("relative")).is_err());
        }
        #[test]
        fn large_siblings_do_not_grow_indexed_counts_and_hard_counters_refuse() {
            let f = Fixture::new();
            f.start();
            f.put_binding(&f.binding());
            let (_, before) = f.read();
            for n in 0..2048 {
                write(&f.state.join(format!("{n:064x}.json")), b"unrelated");
                write(
                    &f.state.join("launches").join(format!("unrelated-{n}.json")),
                    b"unrelated",
                );
            }
            let (reply, after) = f.read();
            assert_eq!(reply["status"], "observed_binding");
            assert_eq!(
                (before.walks, before.opens, before.metadata),
                (after.walks, after.opens, after.metadata)
            );
            let mut counts = Counts::default();
            assert!(counts.open(1025).is_err());
            assert_eq!(counts.opens, 0);
            assert!(counts.meta(4097).is_err());
            assert_eq!(counts.metadata, 0);
            for _ in 0..16 {
                counts.walk(Path::new("/")).unwrap();
            }
            let old = counts.opens;
            assert!(counts.walk(Path::new("/")).is_err());
            assert_eq!(counts.opens, old);
        }
        #[test]
        fn a_deadline_that_passes_after_the_last_reservation_is_unknown() {
            let f = Fixture::new();
            f.start();
            f.put_binding(&f.binding());
            let later = Instant::now() + Duration::from_secs(5);
            assert!(matches!(
                super::super::observe(&f.state, &id(2), later),
                Observation::ObservedBinding { .. }
            ));
            // Every reservation succeeds; the deadline passes only after the
            // last state-root metadata reservation, every recheck done.
            let deadline = Instant::now() + Duration::from_millis(500);
            let reached = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let window = reached.clone();
            AFTER_LAST_RESERVATION.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    assert!(Instant::now() < deadline, "reached before the deadline");
                    window.store(true, std::sync::atomic::Ordering::SeqCst);
                    std::thread::sleep(deadline.saturating_duration_since(Instant::now()))
                }))
            });
            assert!(matches!(
                super::super::observe(&f.state, &id(2), deadline),
                Observation::Unknown
            ));
            assert!(
                reached.load(std::sync::atomic::Ordering::SeqCst),
                "the final window after the last reservation was reached"
            );
        }
        #[test]
        fn a_passed_deadline_or_a_noncanonical_launch_id_is_unknown() {
            let f = Fixture::new();
            f.start();
            f.put_binding(&f.binding());
            let past = Instant::now() - Duration::from_millis(1);
            assert!(matches!(
                super::super::observe(&f.state, &id(2), past),
                Observation::Unknown
            ));
            let later = Instant::now() + Duration::from_secs(5);
            for launch in [
                format!("{{{}}}", id(2)),
                "not-a-uuid".into(),
                "00000000-0000-0000-0000-000000000000".into(),
            ] {
                assert!(
                    matches!(
                        super::super::observe(&f.state, &launch, later),
                        Observation::Unknown
                    ),
                    "{launch}"
                );
            }
        }
    }
}
