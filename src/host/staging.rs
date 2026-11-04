//! Explicit enablement of local sessions on Windows (spec D2): the stable CLI,
//! the content-addressed host images and the marker that turns a control console
//! into a computer that runs sessions.
//!
//! - **Stable CLI** `%USERPROFILE%\.local\bin\hgs.exe`, the only path hooks,
//!   recipes, supervisors and the recovery worker name (D8). An update copies the
//!   new build to a temporary name in the same folder, renames the current file
//!   aside (`hgs.exe.old-<seconds>-<random>`) and renames the copy in. A running
//!   image is never overwritten: Windows lets a running executable be renamed,
//!   but not written or deleted. Later installs delete aside files no process
//!   runs; deleting a running one fails and is left for the next install.
//! - **Host images** `%USERPROFILE%\.local\share\hgs\host\<first 16 hex digits of
//!   the SHA-256 of hgs.exe>\` hold `hgs.exe`, the bundled ConPTY pair when both
//!   files sit next to the running hgs.exe (D6), and `version`. Every copy is
//!   verified by hash. The pointer file `host\current`, written last through a
//!   temporary file and a rename, names the image the next host starts from; a
//!   rollback is a reinstall of the older build. Other images are removed once no
//!   host runs from them: none of their files may be in use (`in_use`), and the
//!   folder is renamed out of the way first, which Windows also refuses while a
//!   file in it is open, so a running image is never taken apart.
//! - **Marker** `local-sessions` in the configuration directory (`HGS_CONFIG_DIR`
//!   or `~/.config/hgs`, as `Config` reads it). The marker alone decides that the
//!   computer is enabled (`platform::local_sessions` on Windows); staged files
//!   never do.
//!
//! Paths come from a `Layout`, so tests stage into temporary folders. The file
//! logic is portable and tested on every platform; what only Windows enforces
//! (running files resist writing, deletion and folder renames) is tested there.
use crate::{config, platform};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use std::{
    fmt, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// The executable's file name, in the stable CLI's folder and in each image.
pub const EXECUTABLE: &str = "hgs.exe";
/// The bundled ConPTY pair (D6), staged only together.
pub const PAIR: [&str; 2] = ["conpty.dll", "OpenConsole.exe"];
/// Below the images folder: the name of the image the next host starts from.
pub const POINTER: &str = "current";
/// In an image: the hgs version of its executable.
pub const VERSION_FILE: &str = "version";
/// In the configuration directory.
pub const MARKER: &str = "local-sessions";
/// Below the images folder; serializes installs.
const LOCK: &str = "install.lock";
const ASIDE_PREFIX: &str = "hgs.exe.old-";
const NEW_PREFIX: &str = ".hgs.exe.new-";
const STAGING_PREFIX: &str = ".staging-";
const TRASH_PREFIX: &str = ".trash-";
/// Hex digits of an image name.
const NAME_LENGTH: usize = 16;
/// How long an install waits for another one to finish.
const LOCK_WAIT: Duration = Duration::from_secs(30);

/// Where enablement keeps its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The stable CLI, `<home>\.local\bin\hgs.exe`.
    pub cli: PathBuf,
    /// The host images, `<home>\.local\share\hgs\host`.
    pub images: PathBuf,
    /// The marker, `<config dir>\local-sessions`.
    pub marker: PathBuf,
}

impl Layout {
    pub fn new(home: &str, config_dir: &Path) -> Self {
        Self {
            cli: PathBuf::from(platform::below(home, ".local/bin")).join(EXECUTABLE),
            images: PathBuf::from(platform::below(home, ".local/share/hgs/host")),
            marker: config_dir.join(MARKER),
        }
    }

    /// This user's: below `platform::home()` and the configuration directory
    /// `Config` uses. None without a home.
    pub fn current() -> Option<Self> {
        let home = platform::home()?;
        Some(Self::new(&home, &config::config_dir(&home)))
    }

    /// Whether local sessions are enabled: the marker exists.
    pub fn enabled(&self) -> bool {
        self.marker.is_file()
    }
}

/// Whether this user enabled local sessions on this computer (D2).
pub fn enabled() -> bool {
    Layout::current().is_some_and(|layout| layout.enabled())
}

/// Why staging stopped. Nothing that a running host uses has changed then, and
/// the pointer still names the image it named before.
#[derive(Debug)]
pub enum StageError {
    /// A file operation on this path failed.
    Io(PathBuf, io::Error),
    /// The copy of this file did not match it.
    Corrupt(PathBuf),
    /// The image folder of this build holds other files, and a host runs from it.
    InUse(PathBuf),
    /// Another install held the lock for too long.
    Busy,
}

impl fmt::Display for StageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Corrupt(path) => write!(
                f,
                "the copy of {} did not match it; nothing was enabled, try again",
                path.display()
            ),
            Self::InUse(path) => write!(
                f,
                "{} holds a different build of this hgs and a session host runs from it; \
                 stop the session host and install again",
                path.display()
            ),
            Self::Busy => f.write_str("another hgs session-host install is running"),
        }
    }
}

impl std::error::Error for StageError {}

fn at(path: &Path) -> impl FnOnce(io::Error) -> StageError + '_ {
    move |error| StageError::Io(path.to_owned(), error)
}

/// The SHA-256 of a file's contents, as lowercase hex.
pub fn sha256(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => hash.update(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// The image folder name of an executable with this SHA-256.
pub fn image_name(hash: &str) -> &str {
    &hash[..NAME_LENGTH.min(hash.len())]
}

fn valid_image_name(name: &str) -> bool {
    name.len() == NAME_LENGTH
        && name
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn random_suffix() -> String {
    format!("{:08x}", OsRng.next_u32())
}

/// A fresh name for the stable CLI moved aside: `hgs.exe.old-<seconds>-<random>`.
fn aside_name() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs());
    format!("{ASIDE_PREFIX}{seconds}-{}", random_suffix())
}

/// Whether `name` is one `aside_name` gave; nothing else is ever deleted.
fn is_aside(name: &str) -> bool {
    name.strip_prefix(ASIDE_PREFIX)
        .and_then(|rest| rest.split_once('-'))
        .is_some_and(|(seconds, random)| {
            !seconds.is_empty()
                && seconds.bytes().all(|c| c.is_ascii_digit())
                && random.len() == 8
                && random
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        })
}

/// `fs::rename`, retried for a moment while a scanner or the indexer holds a file.
fn rename(from: &Path, to: &Path) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match fs::rename(from, to) {
            Err(error)
                if platform::transient_replace_error(&error) && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(20));
            }
            result => return result,
        }
    }
}

/// Move a finished temporary file to `path`, which must not exist.
fn persist_new(mut temporary: tempfile::NamedTempFile, path: &Path) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match temporary.persist_noclobber(path) {
            Ok(_) => return Ok(()),
            Err(error)
                if platform::transient_replace_error(&error.error) && Instant::now() < deadline =>
            {
                temporary = error.file;
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error.error),
        }
    }
}

/// Replace `path`, a file in `dir`, with `bytes` through a temporary file and a
/// rename, so readers see the old contents or the new ones.
fn write_replacing(dir: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temporary = tempfile::Builder::new().prefix(".hgs-").tempfile_in(dir)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    platform::persist(temporary, path).map_err(|error| error.error)?;
    platform::sync_dir(dir)
}

/// Copy `source` to `destination`, a new file, and check the copy against `hash`.
fn copy_verified(
    source: &Path,
    destination: &mut fs::File,
    path: &Path,
    hash: &str,
) -> Result<(), StageError> {
    let mut input = fs::File::open(source).map_err(at(source))?;
    io::copy(&mut input, destination).map_err(at(path))?;
    destination.sync_all().map_err(at(path))?;
    if sha256(path).map_err(at(path))? != hash {
        return Err(StageError::Corrupt(source.to_owned()));
    }
    Ok(())
}

/// Whether a process has `path` open in a way that keeps it from being replaced:
/// Windows refuses to open a running executable or a loaded DLL for writing.
/// Opening changes nothing. A file that cannot be checked counts as in use.
pub fn in_use(path: &Path) -> bool {
    match fs::OpenOptions::new().write(true).open(path) {
        Ok(_) => false,
        Err(error) => error.kind() != io::ErrorKind::NotFound,
    }
}

/// The image the pointer in `images` names, when its folder and executable exist.
pub fn current_image_in(images: &Path) -> Option<PathBuf> {
    let folder = images.join(read_pointer(images)?);
    (folder.is_dir() && folder.join(EXECUTABLE).is_file()).then_some(folder)
}

/// This user's current host image (D2): the folder the next host starts from.
pub fn current_image() -> Option<PathBuf> {
    current_image_in(&Layout::current()?.images)
}

fn read_pointer(images: &Path) -> Option<String> {
    let mut text = String::new();
    fs::File::open(images.join(POINTER))
        .ok()?
        .take(64)
        .read_to_string(&mut text)
        .ok()?;
    let name = text.trim();
    valid_image_name(name).then(|| name.to_owned())
}

/// The serialized section of an install. Released when dropped.
struct InstallLock {
    _lock: platform::FileLock,
}

fn lock(images: &Path) -> Result<InstallLock, StageError> {
    let path = images.join(LOCK);
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(at(&path))?;
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => {
                return Ok(InstallLock {
                    _lock: platform::FileLock::held(file),
                })
            }
            Err(error) if platform::lock_contended(&error) && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) if platform::lock_contended(&error) => return Err(StageError::Busy),
            Err(error) => return Err(StageError::Io(path, error)),
        }
    }
}

/// What staging the stable CLI did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliReport {
    pub path: PathBuf,
    pub sha256: String,
    /// Whether the staged file changed.
    pub changed: bool,
    /// Aside files removed now.
    pub removed: usize,
    /// Aside files left because a process still runs them.
    pub kept: Vec<PathBuf>,
}

/// Stage `source` as the stable CLI at `target`.
pub fn stage_cli(source: &Path, target: &Path) -> Result<CliReport, StageError> {
    let hash = sha256(source).map_err(at(source))?;
    let folder = target
        .parent()
        .ok_or_else(|| StageError::Io(target.to_owned(), io::ErrorKind::InvalidInput.into()))?;
    fs::create_dir_all(folder).map_err(at(folder))?;
    let changed = match sha256(target) {
        Ok(existing) => existing != hash,
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => return Err(StageError::Io(target.to_owned(), error)),
    };
    if changed {
        let mut temporary = tempfile::Builder::new()
            .prefix(NEW_PREFIX)
            .tempfile_in(folder)
            .map_err(at(folder))?;
        let path = temporary.path().to_owned();
        copy_verified(source, temporary.as_file_mut(), &path, &hash)?;
        // The current file, which may be running, moves aside; it is never written.
        let aside = match fs::symlink_metadata(target) {
            Ok(_) => {
                let aside = folder.join(aside_name());
                rename(target, &aside).map_err(at(target))?;
                Some(aside)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(StageError::Io(target.to_owned(), error)),
        };
        if let Err(error) = persist_new(temporary, target) {
            // Put the previous CLI back rather than leave none.
            if let Some(aside) = &aside {
                let _ = rename(aside, target);
            }
            return Err(StageError::Io(target.to_owned(), error));
        }
        let _ = platform::sync_dir(folder);
    }
    let (removed, kept) = remove_aside(folder);
    Ok(CliReport {
        path: target.to_owned(),
        sha256: hash,
        changed,
        removed,
        kept,
    })
}

/// Delete aside CLIs and copies an interrupted install left in `folder`. A
/// running aside file cannot be deleted on Windows; it is kept for a later
/// install.
fn remove_aside(folder: &Path) -> (usize, Vec<PathBuf>) {
    let (mut removed, mut kept) = (0, Vec::new());
    let Ok(entries) = fs::read_dir(folder) else {
        return (removed, kept);
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let aside = is_aside(name);
        if !aside && !name.starts_with(NEW_PREFIX) {
            continue;
        }
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) if aside => removed += 1,
            Ok(()) => {}
            Err(_) if aside => kept.push(entry.path()),
            Err(_) => {}
        }
    }
    kept.sort();
    (removed, kept)
}

/// What sat next to the executable an image was staged from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pair {
    /// Both files of the bundled ConPTY pair; they are in the image.
    Bundled,
    /// Neither file: the host uses inbox ConPTY.
    Absent,
    /// Only one of them: none is staged and the host uses inbox ConPTY.
    Incomplete,
}

/// A staged host image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// The folder name, the first 16 hex digits of `sha256`.
    pub name: String,
    pub path: PathBuf,
    /// The SHA-256 of its executable.
    pub sha256: String,
    /// Whether the bundled ConPTY pair is in it.
    pub bundled: bool,
    /// The hgs version it records.
    pub version: Option<String>,
}

impl Image {
    /// The image in `images` called `name`, when its executable is there. Its
    /// executable is hashed; nothing is verified against a source.
    pub fn read(images: &Path, name: &str) -> Option<Self> {
        let path = images.join(name);
        let sha256 = sha256(&path.join(EXECUTABLE)).ok()?;
        let mut version = String::new();
        let version = fs::File::open(path.join(VERSION_FILE))
            .and_then(|file| file.take(256).read_to_string(&mut version))
            .ok()
            .map(|_| version.trim().to_owned())
            .filter(|version| !version.is_empty());
        Some(Self {
            name: name.into(),
            bundled: PAIR.iter().all(|file| path.join(file).is_file()),
            path,
            sha256,
            version,
        })
    }
}

/// The version of the executable with SHA-256 `hash`, from its staged image in
/// `images` when that image holds exactly this executable.
pub fn version_of(images: &Path, hash: &str) -> Option<String> {
    Image::read(images, image_name(hash))
        .filter(|image| image.sha256 == hash)
        .and_then(|image| image.version)
}

/// What staging a host image did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageReport {
    pub image: Image,
    pub pair: Pair,
    /// Whether the image folder was created or replaced.
    pub changed: bool,
    /// Other images removed now.
    pub removed: Vec<String>,
    /// Other images left because a host runs from them.
    pub kept: Vec<String>,
}

/// A file of an image: its name, where it comes from and its SHA-256.
struct Part {
    name: &'static str,
    source: PathBuf,
    sha256: String,
}

/// The files of the image of `source`: the executable, then the ConPTY pair
/// when both of its files sit next to it.
fn parts(source: &Path) -> Result<(Vec<Part>, Pair), StageError> {
    let hash = |path: &Path| sha256(path).map_err(at(path));
    let mut parts = vec![Part {
        name: EXECUTABLE,
        source: source.to_owned(),
        sha256: hash(source)?,
    }];
    let folder = source.parent().unwrap_or(Path::new(""));
    let present: Vec<PathBuf> = PAIR
        .iter()
        .map(|name| folder.join(name))
        .filter(|path| path.is_file())
        .collect();
    let pair = match present.len() {
        0 => Pair::Absent,
        1 => Pair::Incomplete,
        _ => Pair::Bundled,
    };
    if pair == Pair::Bundled {
        for (name, path) in PAIR.into_iter().zip(present) {
            parts.push(Part {
                name,
                sha256: hash(&path)?,
                source: path,
            });
        }
    }
    Ok((parts, pair))
}

/// Whether `folder` holds every part with its hash.
fn holds(folder: &Path, parts: &[Part]) -> bool {
    parts
        .iter()
        .all(|part| sha256(&folder.join(part.name)).is_ok_and(|hash| hash == part.sha256))
}

/// A new image folder in `images`, under a temporary name until it is complete.
fn build(images: &Path, parts: &[Part], version: &str) -> Result<tempfile::TempDir, StageError> {
    let staged = tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(images)
        .map_err(at(images))?;
    for part in parts {
        let path = staged.path().join(part.name);
        let mut file = fs::File::create(&path).map_err(at(&path))?;
        copy_verified(&part.source, &mut file, &path, &part.sha256)?;
    }
    let path = staged.path().join(VERSION_FILE);
    fs::write(&path, version).map_err(at(&path))?;
    Ok(staged)
}

fn trash_path(images: &Path, name: &str) -> PathBuf {
    images.join(format!("{TRASH_PREFIX}{name}-{}", random_suffix()))
}

/// Move the image folder `folder` out of the way, unless a file in it is in use;
/// Windows also refuses the rename while a file in the folder is open. Anything
/// but a folder in its place is stale and moves as well.
fn retire(images: &Path, folder: &Path, name: &str) -> Option<PathBuf> {
    if fs::symlink_metadata(folder).is_ok_and(|metadata| metadata.is_dir()) {
        let busy = fs::read_dir(folder).map_or(true, |entries| {
            entries.flatten().any(|entry| {
                entry.file_type().is_ok_and(|kind| kind.is_file()) && in_use(&entry.path())
            })
        });
        if busy {
            return None;
        }
    }
    let trash = trash_path(images, name);
    fs::rename(folder, &trash).ok().map(|()| trash)
}

/// Delete a retired image, or what is left of it; what cannot be deleted now
/// goes with a later install.
fn discard(path: &Path) {
    let _ = if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
}

/// Stage the image of `source`, an hgs executable of version `version`, in
/// `images` and point `current` at it.
pub fn stage_image(source: &Path, images: &Path, version: &str) -> Result<ImageReport, StageError> {
    let (parts, pair) = parts(source)?;
    let name = image_name(&parts[0].sha256).to_owned();
    fs::create_dir_all(images).map_err(at(images))?;
    let target = images.join(&name);
    let changed = if target.is_dir() && holds(&target, &parts) {
        let path = target.join(VERSION_FILE);
        if fs::read_to_string(&path).ok().as_deref() != Some(version) {
            write_replacing(&target, &path, version.as_bytes()).map_err(at(&path))?;
        }
        false
    } else {
        let staged = build(images, &parts, version)?;
        // A folder of this name that does not match (an interrupted or altered
        // copy) moves out of the way first; never while a host runs from it.
        let stale = match fs::symlink_metadata(&target) {
            Ok(_) => Some(
                retire(images, &target, &name).ok_or_else(|| StageError::InUse(target.clone()))?,
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(StageError::Io(target, error)),
        };
        if let Err(error) = rename(staged.path(), &target) {
            if let Some(stale) = &stale {
                let _ = fs::rename(stale, &target);
            }
            return Err(StageError::Io(target, error));
        }
        let _ = staged.keep();
        if let Some(stale) = stale {
            discard(&stale);
        }
        true
    };
    // Last: the next host starts from a complete, verified image.
    let pointer = images.join(POINTER);
    if read_pointer(images).as_deref() != Some(name.as_str()) {
        write_replacing(images, &pointer, name.as_bytes()).map_err(at(&pointer))?;
    }
    let (removed, kept) = remove_unused(images, &name);
    let image = Image::read(images, &name)
        .ok_or_else(|| StageError::Io(target.join(EXECUTABLE), io::ErrorKind::NotFound.into()))?;
    Ok(ImageReport {
        image,
        pair,
        changed,
        removed,
        kept,
    })
}

/// Remove every image in `images` but `current` that no host runs from, and
/// what interrupted installs left behind. Returns the removed and kept images.
fn remove_unused(images: &Path, current: &str) -> (Vec<String>, Vec<String>) {
    let (mut removed, mut kept) = (Vec::new(), Vec::new());
    let Ok(entries) = fs::read_dir(images) else {
        return (removed, kept);
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with(TRASH_PREFIX) || name.starts_with(STAGING_PREFIX) {
            discard(&entry.path());
            continue;
        }
        let folder = entry.file_type().is_ok_and(|kind| kind.is_dir());
        if folder && valid_image_name(name) && name != current {
            match retire(images, &entry.path(), name) {
                Some(trash) => {
                    discard(&trash);
                    removed.push(name.to_owned());
                }
                None => kept.push(name.to_owned()),
            }
        }
    }
    removed.sort();
    kept.sort();
    (removed, kept)
}

/// Create the marker; whether it was created now.
pub fn enable(marker: &Path) -> io::Result<bool> {
    if marker.is_file() {
        return Ok(false);
    }
    let dir = marker
        .parent()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    platform::private_dir(dir)?;
    write_replacing(
        dir,
        marker,
        b"Local sessions are enabled on this computer (hgs session-host install).\n",
    )?;
    Ok(true)
}

/// Remove the marker; whether it was there.
pub fn disable(marker: &Path) -> io::Result<bool> {
    match fs::remove_file(marker) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// What `install` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallReport {
    pub cli: CliReport,
    pub image: ImageReport,
    /// Whether the marker was created now (it may have existed).
    pub enabled: bool,
}

/// Install just the maintained CLI for another hgs service. Use the same lock
/// and image-safe replacement as session-host installation, without enabling
/// local sessions or changing their existing enablement choice.
pub fn install_cli(layout: &Layout, source: &Path) -> Result<CliReport, StageError> {
    fs::create_dir_all(&layout.images).map_err(at(&layout.images))?;
    let _lock = lock(&layout.images)?;
    stage_cli(source, &layout.cli)
}

/// Enable local sessions (D2): stage `source`, the running hgs of version
/// `version`, as the stable CLI and as a host image, then write the marker.
/// Idempotent; installs of one user run one at a time.
pub fn install(layout: &Layout, source: &Path, version: &str) -> Result<InstallReport, StageError> {
    fs::create_dir_all(&layout.images).map_err(at(&layout.images))?;
    let _lock = lock(&layout.images)?;
    let cli = stage_cli(source, &layout.cli)?;
    let image = stage_image(source, &layout.images, version)?;
    let enabled = enable(&layout.marker).map_err(at(&layout.marker))?;
    Ok(InstallReport {
        cli,
        image,
        enabled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    /// A source folder with an "executable" of these bytes and, optionally, pair
    /// files.
    fn source(root: &Path, name: &str, bytes: &[u8], pair: &[&str]) -> PathBuf {
        let folder = root.join(name);
        fs::create_dir_all(&folder).unwrap();
        let exe = folder.join(EXECUTABLE);
        fs::write(&exe, bytes).unwrap();
        for file in pair {
            fs::write(folder.join(file), format!("{name} {file}")).unwrap();
        }
        exe
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_layout_follows_home_and_the_configuration_directory() {
        let layout = Layout::new("/home/alice", Path::new("/etc/hgs-config"));
        let below = |relative: &str| PathBuf::from(platform::below("/home/alice", relative));
        assert_eq!(layout.cli, below(".local/bin/hgs.exe"));
        assert_eq!(layout.images, below(".local/share/hgs/host"));
        assert_eq!(
            layout.marker,
            Path::new("/etc/hgs-config").join("local-sessions")
        );
        // Config reads the same directory without HGS_CONFIG_DIR.
        if std::env::var_os("HGS_CONFIG_DIR").is_none() {
            assert_eq!(config::config_dir("/home/alice"), below(".config/hgs"));
        }
    }

    #[test]
    fn hashes_and_names_are_fixed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(sha256(&path).unwrap(), ABC);
        assert_eq!(image_name(ABC), "ba7816bf8f01cfea");
        assert!(valid_image_name("ba7816bf8f01cfea"));
        for name in [
            "ba7816bf8f01cfe",
            "BA7816BF8F01CFEA",
            "ba7816bf8f01cfeg",
            "current",
        ] {
            assert!(!valid_image_name(name), "{name}");
        }
        let aside = aside_name();
        assert!(is_aside(&aside), "{aside}");
        assert_ne!(aside, aside_name());
        for name in [
            "hgs.exe",
            "hgs.exe.old-",
            "hgs.exe.old-backup",
            "hgs.exe.old-12-abc",
            "hgs.exe.old-12-ABCDEF01",
            "hgs.exe.old--abcdef01",
            "hgs.exe.old-1a-abcdef01",
        ] {
            assert!(!is_aside(name), "{name}");
        }
        assert!(is_aside("hgs.exe.old-1760371200-0a1b2c3d"));
    }

    #[test]
    fn the_pointer_names_only_a_complete_image() {
        let dir = tempfile::tempdir().unwrap();
        let images = dir.path();
        assert_eq!(current_image_in(images), None);
        let name = "0123456789abcdef";
        write_replacing(images, &images.join(POINTER), name.as_bytes()).unwrap();
        // No folder yet, then a folder without its executable.
        assert_eq!(current_image_in(images), None);
        fs::create_dir(images.join(name)).unwrap();
        assert_eq!(current_image_in(images), None);
        fs::write(images.join(name).join(EXECUTABLE), b"x").unwrap();
        assert_eq!(current_image_in(images), Some(images.join(name)));
        // A trailing newline from an editor is accepted, anything else is not.
        fs::write(images.join(POINTER), format!("{name}\r\n")).unwrap();
        assert_eq!(current_image_in(images), Some(images.join(name)));
        for text in ["../outside", "0123456789ABCDEF", "", "0123456789abcde"] {
            fs::write(images.join(POINTER), text).unwrap();
            assert_eq!(current_image_in(images), None, "{text:?}");
        }
    }

    #[test]
    fn the_cli_is_replaced_by_rename_and_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let one = source(dir.path(), "one", b"build one", &[]);
        let two = source(dir.path(), "two", b"build two", &[]);
        let target = dir.path().join("bin").join(EXECUTABLE);

        let report = stage_cli(&one, &target).unwrap();
        assert!(report.changed);
        assert_eq!(report.sha256, sha256(&one).unwrap());
        assert_eq!(fs::read(&target).unwrap(), b"build one");
        assert_eq!(names(target.parent().unwrap()), [EXECUTABLE]);
        #[cfg(unix)]
        let first = std::os::unix::fs::MetadataExt::ino(&fs::metadata(&target).unwrap());

        // Idempotent: the same build changes nothing.
        let again = stage_cli(&one, &target).unwrap();
        assert!(!again.changed);
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::MetadataExt::ino(&fs::metadata(&target).unwrap()),
            first
        );

        // Leftovers: an interrupted copy goes, a user's file stays.
        let folder = target.parent().unwrap();
        fs::write(folder.join(".hgs.exe.new-AbC123"), b"partial").unwrap();
        fs::write(folder.join("hgs.exe.old-backup"), b"mine").unwrap();
        let report = stage_cli(&two, &target).unwrap();
        assert!(report.changed);
        assert_eq!(fs::read(&target).unwrap(), b"build two");
        // A new file took the name; the old one moved aside and, running
        // nowhere, was deleted.
        #[cfg(unix)]
        assert_ne!(
            std::os::unix::fs::MetadataExt::ino(&fs::metadata(&target).unwrap()),
            first
        );
        assert_eq!((report.removed, report.kept), (1, Vec::new()));
        assert_eq!(names(folder), [EXECUTABLE, "hgs.exe.old-backup"]);

        // A missing source changes nothing.
        let error = stage_cli(&dir.path().join("missing.exe"), &target).unwrap_err();
        assert!(matches!(error, StageError::Io(..)), "{error}");
        assert_eq!(fs::read(&target).unwrap(), b"build two");
    }

    #[test]
    fn images_are_content_addressed_and_the_pointer_comes_last() {
        let dir = tempfile::tempdir().unwrap();
        let images = dir.path().join("host");
        let one = source(dir.path(), "one", b"build one", &PAIR);
        let two = source(dir.path(), "two", b"build two", &[]);
        let half = source(dir.path(), "half", b"build half", &PAIR[..1]);

        let report = stage_image(&one, &images, "1.0.0").unwrap();
        let name = image_name(&sha256(&one).unwrap()).to_owned();
        assert!(report.changed);
        assert_eq!(report.pair, Pair::Bundled);
        assert_eq!(report.image.name, name);
        assert_eq!(report.image.path, images.join(&name));
        assert!(report.image.bundled);
        assert_eq!(report.image.version.as_deref(), Some("1.0.0"));
        assert_eq!(
            names(&images.join(&name)),
            ["OpenConsole.exe", "conpty.dll", EXECUTABLE, VERSION_FILE]
        );
        for file in PAIR {
            assert_eq!(
                fs::read(images.join(&name).join(file)).unwrap(),
                format!("one {file}").as_bytes()
            );
        }
        assert_eq!(current_image_in(&images), Some(images.join(&name)));
        assert_eq!(fs::read_to_string(images.join(POINTER)).unwrap(), name);
        assert_eq!(
            version_of(&images, &sha256(&one).unwrap()).as_deref(),
            Some("1.0.0")
        );

        // The same build again: nothing changes; a lost version file returns.
        fs::remove_file(images.join(&name).join(VERSION_FILE)).unwrap();
        let again = stage_image(&one, &images, "1.0.0").unwrap();
        assert!(!again.changed);
        assert_eq!(again.image.version.as_deref(), Some("1.0.0"));

        // Another build: a new folder, the pointer moves, the unused image goes.
        let report = stage_image(&two, &images, "1.1.0").unwrap();
        let second = image_name(&sha256(&two).unwrap()).to_owned();
        assert_eq!(report.pair, Pair::Absent);
        assert!(!report.image.bundled);
        assert_eq!(
            (report.removed.clone(), report.kept.clone()),
            (vec![name.clone()], Vec::new())
        );
        assert_eq!(names(&images), sorted([POINTER, &second]));
        assert_eq!(names(&images.join(&second)), [EXECUTABLE, VERSION_FILE]);
        assert_eq!(version_of(&images, &sha256(&one).unwrap()), None);

        // Half a pair is not staged.
        let report = stage_image(&half, &images, "1.2.0").unwrap();
        let third = image_name(&sha256(&half).unwrap()).to_owned();
        assert_eq!(report.pair, Pair::Incomplete);
        assert_eq!(names(&report.image.path), [EXECUTABLE, VERSION_FILE]);

        // A failed stage leaves the pointer and the current image alone.
        assert!(stage_image(&dir.path().join("gone.exe"), &images, "9").is_err());
        assert_eq!(current_image_in(&images), Some(images.join(&third)));
        assert_eq!(names(&images), sorted([POINTER, &third]));
    }

    fn sorted<const N: usize>(names: [&str; N]) -> Vec<String> {
        let mut names: Vec<String> = names.iter().map(|name| name.to_string()).collect();
        names.sort();
        names
    }

    #[test]
    fn a_copy_that_differs_from_its_source_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        fs::write(&from, b"abd").unwrap();
        let mut file = fs::File::create(&to).unwrap();
        assert!(matches!(
            copy_verified(&from, &mut file, &to, ABC),
            Err(StageError::Corrupt(path)) if path == from
        ));
        fs::write(&from, b"abc").unwrap();
        let mut file = fs::File::create(&to).unwrap();
        copy_verified(&from, &mut file, &to, ABC).unwrap();
    }

    #[test]
    fn an_altered_image_is_replaced_and_leftovers_are_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let images = dir.path().join("host");
        let one = source(dir.path(), "one", b"build one", &PAIR);
        let name = image_name(&sha256(&one).unwrap()).to_owned();
        stage_image(&one, &images, "1.0.0").unwrap();
        fs::write(images.join(&name).join(PAIR[0]), b"altered").unwrap();
        fs::create_dir(images.join(".staging-left")).unwrap();
        fs::create_dir(images.join(".trash-left")).unwrap();
        fs::write(images.join(".trash-file"), b"").unwrap();
        fs::create_dir(images.join("unrelated")).unwrap();
        let report = stage_image(&one, &images, "1.0.0").unwrap();
        assert!(report.changed);
        assert_eq!(
            fs::read(images.join(&name).join(PAIR[0])).unwrap(),
            b"one conpty.dll"
        );
        assert_eq!(names(&images), sorted([POINTER, &name, "unrelated"]));

        // A file in the image's place is stale too.
        let two = source(dir.path(), "two", b"build two", &[]);
        let second = image_name(&sha256(&two).unwrap()).to_owned();
        fs::write(images.join(&second), b"not a folder").unwrap();
        stage_image(&two, &images, "1.1.0").unwrap();
        assert_eq!(current_image_in(&images), Some(images.join(&second)));
        assert_eq!(names(&images), sorted([POINTER, &second, "unrelated"]));
    }

    #[test]
    fn service_cli_install_does_not_enable_or_disable_local_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(
            &dir.path().join("home").to_string_lossy(),
            &dir.path().join("config"),
        );
        let one = source(dir.path(), "one", b"build one", &[]);
        assert!(install_cli(&layout, &one).unwrap().changed);
        assert_eq!(fs::read(&layout.cli).unwrap(), b"build one");
        assert!(!layout.enabled());
        assert!(current_image_in(&layout.images).is_none());
        assert!(!install_cli(&layout, &one).unwrap().changed);
        enable(&layout.marker).unwrap();
        let marker = fs::read(&layout.marker).unwrap();
        let two = source(dir.path(), "two", b"build two", &[]);
        assert!(install_cli(&layout, &two).unwrap().changed);
        assert_eq!(fs::read(&layout.marker).unwrap(), marker);
        assert!(layout.enabled());
    }

    #[test]
    fn install_stages_both_then_enables_and_uninstall_keeps_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(
            &dir.path().join("home").to_string_lossy(),
            &dir.path().join("config"),
        );
        let one = source(dir.path(), "one", b"build one", &PAIR);
        assert!(!layout.enabled());
        let report = install(&layout, &one, "1.0.0").unwrap();
        assert!(report.enabled && report.cli.changed && report.image.changed);
        assert!(layout.enabled());
        assert_eq!(fs::read(&layout.cli).unwrap(), b"build one");
        assert_eq!(
            current_image_in(&layout.images),
            Some(report.image.image.path.clone())
        );
        assert_eq!(
            version_of(&layout.images, &report.cli.sha256).as_deref(),
            Some("1.0.0")
        );

        let again = install(&layout, &one, "1.0.0").unwrap();
        assert!(!again.enabled && !again.cli.changed && !again.image.changed);

        assert!(disable(&layout.marker).unwrap());
        assert!(!layout.enabled());
        assert!(!disable(&layout.marker).unwrap());
        // Staged files stay for a later cleanup or a new install.
        assert!(layout.cli.is_file());
        assert!(current_image_in(&layout.images).is_some());
        assert!(enable(&layout.marker).unwrap());
        assert!(!enable(&layout.marker).unwrap());
    }

    /// Where `environment_probe` reports, when a parent test runs it.
    const PROBE_REPORT: &str = "HGS_TEST_STAGING_REPORT";

    /// Run by `the_marker_and_pointer_are_found_through_the_environment` in a
    /// process of its own, whose environment names a temporary home.
    #[test]
    #[ignore = "run by the_marker_and_pointer_are_found_through_the_environment"]
    fn environment_probe() {
        let Some(report) = std::env::var_os(PROBE_REPORT) else {
            return;
        };
        let image = current_image().map_or(String::new(), |path| path.display().to_string());
        let text = format!(
            "enabled={}\nlocal_sessions={}\ncurrent_image={image}\n",
            enabled(),
            platform::local_sessions(),
        );
        fs::write(report, text).unwrap();
    }

    /// What `environment_probe` finds with `home` as HOME and USERPROFILE and
    /// `config` as HGS_CONFIG_DIR.
    fn probe(home: &Path, config: Option<&Path>) -> [String; 3] {
        let report = home.join("report");
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "host::staging::tests::environment_probe",
                "--exact",
                "--ignored",
                "--test-threads=1",
            ])
            .env(PROBE_REPORT, &report)
            .env("HOME", home)
            .env("USERPROFILE", home)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match config {
            Some(dir) => command.env("HGS_CONFIG_DIR", dir),
            None => command.env_remove("HGS_CONFIG_DIR"),
        };
        assert!(command.status().unwrap().success());
        let text = fs::read_to_string(&report).unwrap();
        fs::remove_file(&report).unwrap();
        let lines: Vec<String> = text.lines().map(str::to_owned).collect();
        lines.try_into().unwrap()
    }

    #[test]
    fn the_marker_and_pointer_are_found_through_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir(&home).unwrap();
        let text = home.to_string_lossy().into_owned();
        let layout = Layout::new(&text, &PathBuf::from(platform::below(&text, ".config/hgs")));
        // Unix runs local sessions whatever the marker says.
        let unix = cfg!(unix);
        let expect = |enabled: bool, image: &str| {
            [
                format!("enabled={enabled}"),
                format!("local_sessions={}", enabled || unix),
                format!("current_image={image}"),
            ]
        };
        assert_eq!(probe(&home, None), expect(false, ""));

        let exe = source(dir.path(), "build", b"build", &[]);
        let report = install(&layout, &exe, "1.0.0").unwrap();
        let image = report.image.image.path.display().to_string();
        assert_eq!(probe(&home, None), expect(true, &image));
        // HGS_CONFIG_DIR moves the marker, as it moves the configuration.
        let other = dir.path().join("config");
        assert_eq!(probe(&home, Some(&other)), expect(false, &image));
        enable(&other.join(MARKER)).unwrap();
        assert_eq!(probe(&home, Some(&other)), expect(true, &image));

        disable(&layout.marker).unwrap();
        assert_eq!(probe(&home, None), expect(false, &image));
    }

    /// Set for a copy of this test binary that only waits (`idle_probe`).
    #[cfg(windows)]
    const IDLE: &str = "HGS_TEST_STAGING_IDLE";

    /// Started from staged copies of this test binary, so that they run while a
    /// test installs over them.
    #[cfg(windows)]
    #[test]
    #[ignore = "started by the tests of running copies"]
    fn idle_probe() {
        if std::env::var_os(IDLE).is_some() {
            thread::sleep(Duration::from_secs(60));
        }
    }

    /// Run `exe`, a copy of this test binary, until the result is dropped. Its
    /// job ends it then, also when the test fails half way.
    #[cfg(windows)]
    fn run_copy(exe: &Path) -> platform::Spawned {
        let mut command = std::process::Command::new(exe);
        command
            .args([
                "host::staging::tests::idle_probe",
                "--exact",
                "--ignored",
                "--test-threads=1",
            ])
            .env(IDLE, "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        platform::spawn(&mut command).unwrap()
    }

    /// End a copy started by `run_copy` and wait until it has exited.
    #[cfg(windows)]
    fn end_copy(mut copy: platform::Spawned) {
        copy.terminate();
        copy.wait().unwrap();
    }

    /// Stage again while `kept` says something stayed: a scanner may still hold
    /// a file of a copy that has just ended.
    #[cfg(windows)]
    fn until_released<T>(mut stage: impl FnMut() -> T, kept: impl Fn(&T) -> bool) -> T {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let report = stage();
            if !kept(&report) || Instant::now() >= deadline {
                return report;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Another build of this test binary: the same image with data appended,
    /// which the loader ignores.
    #[cfg(windows)]
    fn other_build(root: &Path) -> PathBuf {
        let mut bytes = fs::read(std::env::current_exe().unwrap()).unwrap();
        bytes.extend_from_slice(b"\0another hgs build for the staging tests");
        source(root, "other", &bytes, &PAIR)
    }

    #[cfg(windows)]
    #[test]
    fn a_running_cli_is_renamed_aside_and_removed_once_it_ends() {
        let dir = tempfile::tempdir().unwrap();
        let this = std::env::current_exe().unwrap();
        let other = other_build(dir.path());
        let target = dir.path().join("bin").join(EXECUTABLE);
        stage_cli(&this, &target).unwrap();
        let running = run_copy(&target);
        assert!(in_use(&target));

        let report = stage_cli(&other, &target).unwrap();
        assert!(report.changed);
        assert_eq!(sha256(&target).unwrap(), sha256(&other).unwrap());
        assert!(!in_use(&target));
        // The running copy moved aside and stays until it ends.
        assert_eq!((report.removed, report.kept.len()), (0, 1));
        let aside = &report.kept[0];
        assert!(is_aside(&aside.file_name().unwrap().to_string_lossy()));
        assert!(in_use(aside));
        assert_eq!(sha256(aside).unwrap(), sha256(&this).unwrap());

        end_copy(running);
        let report = until_released(
            || stage_cli(&other, &target).unwrap(),
            |report| !report.kept.is_empty(),
        );
        assert!(!report.changed);
        assert_eq!((report.removed, report.kept), (1, Vec::new()));
        assert_eq!(names(target.parent().unwrap()), [EXECUTABLE]);
    }

    #[cfg(windows)]
    #[test]
    fn an_image_a_host_runs_from_stays_until_it_ends() {
        let dir = tempfile::tempdir().unwrap();
        let images = dir.path().join("host");
        let this = std::env::current_exe().unwrap();
        let other = other_build(dir.path());
        let first = stage_image(&this, &images, "1.0.0").unwrap().image;
        let running = run_copy(&first.path.join(EXECUTABLE));

        let report = stage_image(&other, &images, "1.1.0").unwrap();
        assert!(report.image.bundled);
        assert_eq!(current_image_in(&images), Some(report.image.path.clone()));
        assert_eq!(
            (report.removed, report.kept),
            (Vec::new(), vec![first.name.clone()])
        );
        // Nothing of the running image was taken apart.
        assert_eq!(names(&first.path), [EXECUTABLE, VERSION_FILE]);
        assert_eq!(Image::read(&images, &first.name), Some(first.clone()));

        end_copy(running);
        let report = until_released(
            || stage_image(&other, &images, "1.1.0").unwrap(),
            |report| !report.kept.is_empty(),
        );
        assert!(!report.changed);
        assert_eq!(
            (report.removed, report.kept),
            (vec![first.name], Vec::new())
        );
        assert_eq!(names(&images), sorted([POINTER, &report.image.name]));
    }

    #[test]
    fn installs_run_one_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let held = lock(dir.path()).unwrap();
        let waiting = {
            let dir = dir.path().to_owned();
            thread::spawn(move || {
                let started = Instant::now();
                lock(&dir).map(|_| started.elapsed())
            })
        };
        thread::sleep(Duration::from_millis(300));
        drop(held);
        let waited = waiting.join().unwrap().unwrap();
        assert!(waited >= Duration::from_millis(200), "{waited:?}");
    }

    #[test]
    fn files_nobody_runs_are_not_in_use() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        fs::write(&path, b"x").unwrap();
        assert!(!in_use(&path));
        assert!(!in_use(&dir.path().join("missing")));
        assert_eq!(fs::read(&path).unwrap(), b"x");
    }
}
