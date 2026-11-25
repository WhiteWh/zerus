//! #752 (owner, 2026-10-31): a build broker for members who share one folder.
//!
//! Several agents in one folder (the normal case for a game project, where a
//! worktree per member would copy hundreds of GB of assets) each change their
//! own files and need to compile. Without the broker their builds run in the
//! one build folder and compile each other's half-done edits.
//!
//! With the owner's team setting `build_broker` a build template of a member
//! runs in that member's own snapshot, `.zerus-team/runs/broker-<member>/tree`
//! below the shared folder: the folder's tracked files at HEAD, with the
//! member's own changed files (the `files` it names in its build requests) from
//! its working copy. Another member's changed file is taken at HEAD, so its
//! half-written state never reaches this build. Source files are copies, never
//! hard links: a hard link shares the file's data, so a write on either side
//! would change the other (1.2, 2026-10-31). The project's asset and linked
//! folders are one link each, so a 300 GB project costs no second copy; their
//! contents count in the tree's key by a bounded listing, and a tree whose
//! listing is too large to know is never served by another build's result.
//! Files are written again only when their content changed (a git blob or a
//! content digest, never a size and a time), so the build stays incremental,
//! and the shared compiler cache (#721) still serves it. Builds of one build
//! folder run one at a time (a lock is the queue); the same tree, template,
//! arguments and toolchain built by another member is waited for (an atomic
//! marker), and its verdict serves a check template (tests, lint). A build
//! template's result serves only the member whose tree holds its outputs. A
//! failure in a file another member is changing is reported as an observation
//! to both, never as a verdict on who broke it.
//!
//! Shared mode (owner, 2026-10-31) is for projects where a snapshot costs too
//! much: an Unreal project (a `*.uproject` at the root) uses it unless the
//! tests file says otherwise. A build runs in the shared folder itself, so
//! there is no copy, no second Intermediate and the editor loads the DLL that
//! was built. Builds of the folder run one at a time, and each first waits
//! (bounded) until every other member has asked for a build since it last
//! changed one of its files: a member that asks for a build says its edits are
//! ready. Past that wait the build runs anyway and names the unfinished
//! changes it included. An Unreal project's `Content` and `Plugins/*/Content`
//! count as asset folders without being configured.
//!
//! `.zerus/tests.toml` configures it in `[build_broker]`; the team setting
//! switches it. Off, nothing changes.
use super::model::{Member, Team};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The project's broker configuration.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Config {
    /// False when the project vetoes the broker (`enabled = false`).
    pub enabled: bool,
    /// The templates that build in a snapshot.
    pub templates: Vec<String>,
    /// Folders linked whole (assets): never copied, never read.
    pub assets: Vec<String>,
    /// Untracked folders the build needs (third-party, generated), linked.
    pub link: Vec<String>,
    /// Copied files above this size are named in the build's note (every
    /// file is a copy; the key stays for older tests files).
    pub copy_limit: u64,
    /// Builds run in the shared folder itself, after the others' edits are
    /// ready (`mode = "shared"`, or an Unreal project under `"auto"`).
    pub shared: bool,
    /// How long a shared build waits for the others' edits to be ready.
    pub ready_wait: Duration,
    /// The folder is an Unreal project (a `*.uproject` at its root).
    pub unreal: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            // Task 85: the CMake and MSBuild templates of any project too.
            templates: ["cargo-build", "cargo-test", "cargo-clippy", "cmake-configure", "cmake-build", "ctest", "msbuild-build", "vstest"]
                .map(str::to_owned)
                .to_vec(),
            assets: Vec::new(),
            link: Vec::new(),
            copy_limit: 4 * 1024 * 1024,
            shared: false,
            ready_wait: Duration::from_secs(300),
            unreal: false,
        }
    }
}

/// The `*.uproject` file at the folder's root, if any.
fn unreal_project(folder: &Path) -> bool {
    fs::read_dir(folder).into_iter().flatten().flatten().take(2000).any(|entry| {
        entry.path().extension().is_some_and(|extension| extension.eq_ignore_ascii_case("uproject")) && entry.file_type().is_ok_and(|kind| kind.is_file())
    })
}

/// An Unreal project's asset folders: `Content` and each plugin's `Content`
/// (one or two levels below `Plugins`), bounded.
fn unreal_assets(folder: &Path) -> Vec<String> {
    let mut found = Vec::new();
    if folder.join("Content").is_dir() {
        found.push("Content".to_owned());
    }
    let plugins = folder.join("Plugins");
    let dirs = |path: &Path| -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .take(500)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    for plugin in dirs(&plugins) {
        if plugins.join(&plugin).join("Content").is_dir() {
            found.push(format!("Plugins/{plugin}/Content"));
            continue;
        }
        // A plugin group folder (Plugins/Group/Plugin/Content).
        for inner in dirs(&plugins.join(&plugin)) {
            if plugins.join(&plugin).join(&inner).join("Content").is_dir() {
                found.push(format!("Plugins/{plugin}/{inner}/Content"));
            }
        }
    }
    found
}

/// The `[build_broker]` table of the project's tests file over the defaults; a
/// value of the wrong type keeps its default.
pub(crate) fn config(folder: &Path) -> Config {
    let mut config = Config::default();
    let mut mode = String::from("auto");
    read_table(folder, &mut config, &mut mode);
    config.unreal = unreal_project(folder);
    config.shared = match mode.as_str() {
        "shared" => true,
        "snapshot" => false,
        _ => config.unreal,
    };
    if config.unreal {
        for asset in unreal_assets(folder) {
            if !config.assets.contains(&asset) {
                config.assets.push(asset);
            }
        }
    }
    config
}

fn read_table(folder: &Path, config: &mut Config, mode: &mut String) {
    let Ok(text) = fs::read_to_string(folder.join(".zerus").join("tests.toml")) else { return };
    let Ok(document) = text.parse::<toml_edit::DocumentMut>() else { return };
    let Some(table) = document.get("build_broker").and_then(toml_edit::Item::as_table_like) else { return };
    let strings = |item: &toml_edit::Item| -> Option<Vec<String>> {
        item.as_array().map(|list| list.iter().filter_map(|value| value.as_str()).filter(|value| relative(value)).map(str::to_owned).collect())
    };
    for (key, item) in table.iter() {
        match key {
            "enabled" => config.enabled = item.as_bool().unwrap_or(config.enabled),
            "templates" => config.templates = item.as_array().map(|list| list.iter().filter_map(|value| value.as_str()).map(str::to_owned).collect()).unwrap_or(config.templates.clone()),
            "assets" => config.assets = strings(item).unwrap_or_default(),
            "link" => config.link = strings(item).unwrap_or_default(),
            "copy_limit_mb" => config.copy_limit = item.as_integer().filter(|mb| *mb >= 0).map_or(config.copy_limit, |mb| mb as u64 * 1024 * 1024),
            "mode" => {
                if let Some(value) = item.as_str().filter(|value| ["auto", "shared", "snapshot"].contains(value)) {
                    *mode = value.to_owned();
                }
            }
            "ready_wait_s" => config.ready_wait = item.as_integer().filter(|s| *s >= 0).map_or(config.ready_wait, |s| Duration::from_secs(s as u64)),
            _ => {}
        }
    }
}

/// A path below the folder: relative, no "..".
fn relative(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty() && !path.is_absolute() && !value.contains(':')
        && !path.components().any(|part| matches!(part, std::path::Component::ParentDir))
}

/// Whether `template` of `member` builds through the broker: the team setting
/// is on, the project does not veto it and the template is one of its builds.
pub(crate) fn applies(team: &Team, template: &str, folder: &Path) -> Option<Config> {
    if !team.settings.build_broker {
        return None;
    }
    let config = config(folder);
    (config.enabled && config.templates.iter().any(|id| id == template)).then_some(config)
}

/// The members (not removed) working in the same folder as `member`.
pub(crate) fn sharing(team: &Team, member: &Member) -> Vec<String> {
    let Ok(mine) = super::dispatcher::member_folder_path(team, member) else { return Vec::new() };
    team.members
        .iter()
        .filter(|other| other.id != member.id && other.state != super::model::MemberState::Removed)
        .filter(|other| super::dispatcher::member_folder_path(team, other).is_ok_and(|theirs| same(&theirs, &mine)))
        .map(|other| other.id.clone())
        .collect()
}

/// Groups of two or more members (the lead not counted: it plans, it does not
/// build) that share one folder, for the lead's item.
/// Owner feedback 2026-11-04 (19): whether the folder builds at all: a
/// project build file at its root, an Unreal project or the project's own
/// build_broker table. A folder of pages and scripts never gets the broker offer.
pub(crate) fn builds(folder: &Path) -> bool {
    const FILES: &[&str] = &[
        "Cargo.toml", "CMakeLists.txt", "Makefile", "makefile", "GNUmakefile", "build.gradle", "build.gradle.kts", "pom.xml",
        "go.mod", "meson.build", "BUILD.bazel", "WORKSPACE", "build.zig", "SConstruct", "package.json", "tsconfig.json",
    ];
    const EXTENSIONS: &[&str] = &["sln", "csproj", "vcxproj", "uproject", "xcodeproj"];
    // The project's own tests.toml names its builds; the default templates
    // alone say nothing about the folder.
    let declared = fs::read_to_string(folder.join(".zerus").join("tests.toml")).is_ok_and(|text| text.contains("build_broker"));
    if unreal_project(folder) || declared || FILES.iter().any(|file| folder.join(file).is_file()) {
        return true;
    }
    std::fs::read_dir(folder).into_iter().flatten().flatten().take(500).any(|entry| {
        entry.path().extension().and_then(|ext| ext.to_str()).is_some_and(|ext| EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
    })
}

pub(crate) fn shared_folders(team: &Team) -> Vec<(String, Vec<String>)> {
    let mut by_folder: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for member in team.members.iter().filter(|member| member.id != team.lead && member.state != super::model::MemberState::Removed) {
        let Ok(folder) = super::dispatcher::member_folder_path(team, member) else { continue };
        let entry = by_folder.entry(key(&folder)).or_insert_with(|| (folder.clone(), Vec::new()));
        entry.1.push(member.id.clone());
    }
    by_folder.into_values().filter(|(_, members)| members.len() >= 2).collect()
}

fn key(path: &str) -> String {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path).replace('\\', "/");
    let path = path.trim_end_matches('/').to_owned();
    if cfg!(windows) { path.to_lowercase() } else { path }
}

fn same(left: &str, right: &str) -> bool {
    key(left) == key(right)
}

/// A git command in `folder` with the repository's hooks, fsmonitor and
/// signing off, its output bounded.
fn git(folder: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(folder)
        .args(["-c", "core.hooksPath=", "-c", "core.fsmonitor=false", "-c", "core.quotepath=false"])
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    anyhow::ensure!(output.status.success(), "git {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    Ok(output.stdout)
}

fn nul_list(bytes: &[u8]) -> Vec<String> {
    bytes.split(|byte| *byte == 0).filter(|part| !part.is_empty()).map(|part| String::from_utf8_lossy(part).into_owned()).collect()
}

/// Where one snapshot file comes from; each names its content, so a file is
/// written again exactly when its content changed.
#[derive(Clone, Debug, PartialEq)]
enum Source {
    /// A clean tracked file: the working copy, whose content is its index blob.
    Clean { blob: String },
    /// The member's own changed file: the working copy, by its content digest.
    Own { digest: String },
    /// The HEAD blob: another member's changed file.
    Head { blob: String },
}

impl Source {
    fn stamp(&self) -> String {
        match self {
            Source::Clean { blob } => format!("b:{blob}"),
            Source::Own { digest } => format!("o:{digest}"),
            Source::Head { blob } => format!("h:{blob}"),
        }
    }
}

/// Entries a linked folder's listing may have before its contents count as
/// unknown.
const LISTING_LIMIT: usize = 200_000;

/// A linked folder's contents as a digest of its files' paths, sizes and
/// times; None when it has more than `LISTING_LIMIT` entries or cannot be read.
fn listing(folder: &Path) -> Option<String> {
    let mut entries = Vec::new();
    let mut stack = vec![folder.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).ok()? {
            let entry = entry.ok()?;
            let meta = entry.metadata().ok()?;
            let path = entry.path();
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            let modified = meta.modified().ok().and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |at| at.as_nanos());
            let relative = path.strip_prefix(folder).ok()?.display().to_string().replace('\\', "/");
            entries.push(format!("{relative}\0{}\0{modified}", meta.len()));
            if entries.len() > LISTING_LIMIT {
                return None;
            }
        }
    }
    entries.sort();
    Some(format!("{:x}", Sha256::digest(entries.join("\n").as_bytes())))
}

/// A file's content digest, read in bounded pieces.
fn digest_of(path: &Path) -> anyhow::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// One member's snapshot after a sync: its folder, the tree's hash and the
/// files another member is changing (taken at HEAD here).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Snapshot {
    pub tree: PathBuf,
    pub hash: String,
    /// Path (relative, '/') -> the member changing it.
    pub others: BTreeMap<String, String>,
    pub copied: usize,
    pub linked: usize,
    /// Copied files larger than the project's `copy_limit_mb`.
    pub large: usize,
    /// False when a linked folder's contents are unknown (too many files):
    /// such a tree is never served by another build's result.
    pub known: bool,
}

/// The broker's folder for `member` below the shared folder.
pub(crate) fn member_dir(shared: &Path, member: &str) -> PathBuf {
    shared.join(".zerus-team").join("runs").join(format!("broker-{}", super::dispatcher::tag(member)))
}

/// Brings `member`'s snapshot of `shared` up to date: clean tracked files and
/// the member's own changed `edits` copied from the working copy, other
/// members' changed files at HEAD, asset and linked folders as one link each.
/// Only files whose content changed are written again.
pub(crate) fn sync(shared: &Path, member: &str, edits: &[String], others: &BTreeMap<String, String>, config: &Config) -> anyhow::Result<Snapshot> {
    let dir = member_dir(shared, member);
    let tree = dir.join("tree");
    fs::create_dir_all(&tree)?;
    // The snapshots never show in the shared folder's git status.
    let ignore = shared.join(".zerus-team").join(".gitignore");
    if !ignore.exists() {
        let _ = fs::write(&ignore, "*\n");
    }
    let manifest_path = dir.join("manifest.json");
    let previous: BTreeMap<String, String> = fs::read(&manifest_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let mine: BTreeSet<String> = edits.iter().map(|path| path.replace('\\', "/")).collect();
    // Tracked files with their index blobs ("mode blob stage\tpath").
    let mut blobs: BTreeMap<String, String> = BTreeMap::new();
    for entry in nul_list(&git(shared, &["ls-files", "-s", "-z"])?) {
        if let Some((info, path)) = entry.split_once('\t') {
            if let Some(blob) = info.split_whitespace().nth(1) {
                blobs.insert(path.to_owned(), blob.to_owned());
            }
        }
    }
    // Dirty paths: modified, added, deleted or untracked.
    let status = nul_list(&git(shared, &["status", "--porcelain", "-z", "--untracked-files=all"])?);
    let mut dirty = BTreeSet::new();
    for entry in &status {
        if entry.len() > 3 {
            dirty.insert(entry[3..].to_owned());
        }
    }
    let under = |path: &str, folders: &[String]| folders.iter().any(|folder| path == folder || path.starts_with(&format!("{}/", folder.trim_end_matches('/'))));
    let excluded = |path: &str| path.starts_with(".zerus-team/") || under(path, &config.assets) || under(path, &config.link);
    let mut wanted: BTreeMap<String, Source> = BTreeMap::new();
    let mut taken: BTreeMap<String, String> = BTreeMap::new();
    let mut paths: BTreeSet<String> = blobs.keys().cloned().collect();
    // The member's own new files are part of its tree.
    paths.extend(mine.iter().filter(|path| shared.join(path.as_str()).is_file()).cloned());
    for path in paths {
        if excluded(&path) {
            continue;
        }
        let own = mine.contains(&path);
        if dirty.contains(&path) && !own {
            // Another member's change (or nobody's): the HEAD version, or
            // nothing when the file is new.
            if let Ok(blob) = git(shared, &["rev-parse", &format!("HEAD:{path}")]) {
                wanted.insert(path.clone(), Source::Head { blob: String::from_utf8_lossy(&blob).trim().to_owned() });
            }
            if let Some(owner) = others.get(&path) {
                taken.insert(path.clone(), owner.clone());
            }
            continue;
        }
        let source = shared.join(&path);
        if !source.is_file() {
            continue;
        }
        let found = match (own, blobs.get(&path)) {
            (false, Some(blob)) => Source::Clean { blob: blob.clone() },
            _ => Source::Own { digest: digest_of(&source)? },
        };
        wanted.insert(path, found);
    }
    let mut copied = 0;
    let mut linked = 0;
    let mut large = 0;
    let mut manifest: BTreeMap<String, String> = BTreeMap::new();
    for (path, source) in &wanted {
        let stamp = source.stamp();
        let target = tree.join(path);
        if previous.get(path) != Some(&stamp) || !target.exists() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            // Never a hard link: an old snapshot's link would let a write on
            // either side reach the other, so the old file goes first.
            let _ = fs::remove_file(&target);
            match source {
                Source::Clean { .. } | Source::Own { .. } => {
                    let bytes = fs::copy(shared.join(path), &target)?;
                    copied += 1;
                    if bytes > config.copy_limit {
                        large += 1;
                    }
                }
                Source::Head { blob } => {
                    fs::write(&target, git(shared, &["cat-file", "blob", blob])?)?;
                    copied += 1;
                }
            }
        }
        manifest.insert(path.clone(), stamp);
    }
    // Files the tree no longer has go.
    for path in previous.keys().filter(|path| !manifest.contains_key(*path) && !path.starts_with("link:")) {
        let _ = fs::remove_file(tree.join(path));
    }
    // Asset and linked folders: one link each to the shared folder, never a
    // copy; their contents count in the key by a bounded listing.
    let mut known = true;
    for folder in config.assets.iter().chain(&config.link) {
        let source = shared.join(folder);
        let target = tree.join(folder);
        if source.is_dir() && fs::symlink_metadata(&target).is_err() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            link_dir(&source, &target)?;
            linked += 1;
        }
        let contents = if source.is_dir() { listing(&source) } else { Some("none".into()) };
        known &= contents.is_some();
        manifest.insert(format!("link:{folder}"), contents.unwrap_or_else(|| "unknown".into()));
    }
    // Task 374 (owner, 2026-11-08): installed dependencies are ignored by Git,
    // so a snapshot that copies only tracked files has no node_modules and a
    // web suite cannot start. Link them from the shared folder like an asset,
    // never copy them.
    for folder in dependency_folders(shared) {
        let source = shared.join(&folder);
        let target = tree.join(&folder);
        if fs::symlink_metadata(&target).is_err() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            if link_dir(&source, &target).is_ok() {
                linked += 1;
            }
        }
        manifest.insert(format!("link:{folder}"), "dependencies".into());
    }
    fs::write(&manifest_path, serde_json::to_vec(&manifest)?)?;
    // Every stamp names content (a blob, a digest or a folder listing), so an
    // untouched tree hashes the same and any changed byte changes it.
    let mut hasher = Sha256::new();
    for (path, stamp) in &manifest {
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update(stamp.as_bytes());
        hasher.update([0]);
    }
    Ok(Snapshot { tree, hash: format!("{:x}", hasher.finalize()), others: taken, copied, linked, large, known })
}

/// Task 374: the dependency folders a snapshot links from the shared folder: a
/// `node_modules` at the repository root or in the web client.
fn dependency_folders(shared: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for name in ["node_modules", "client/node_modules"] {
        if shared.join(name).is_dir() {
            found.push(name.to_owned());
        }
    }
    found
}

/// A folder link: a junction on Windows (no privilege needed), a symlink
/// elsewhere.
fn link_dir(source: &Path, target: &Path) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let status = Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(target)
            .arg(source)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        anyhow::ensure!(status.success(), "the link {} could not be made", target.display());
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(source, target)?;
    Ok(())
}

/// The broker's one-at-a-time queue for a build folder: an exclusive lock
/// file, waited for in bounded steps.
pub(crate) fn queue(dir: &Path, timeout: Duration) -> anyhow::Result<fs::File> {
    use fs2::FileExt;
    fs::create_dir_all(dir)?;
    let file = fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("queue.lock"))?;
    let until = Instant::now() + timeout;
    loop {
        if file.try_lock_exclusive().is_ok() {
            return Ok(file);
        }
        anyhow::ensure!(Instant::now() < until, "the build folder's queue did not free in {} s", timeout.as_secs());
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The finished builds of a shared folder by tree, template and arguments,
/// newest last and bounded, so an unchanged tree is built once.
const RESULTS: usize = 64;

fn results_path(shared: &Path) -> PathBuf {
    shared.join(".zerus-team").join("runs").join("broker-results.json")
}

pub(crate) fn request_key(hash: &str, template: &str, args: &Value, toolchain: &str) -> String {
    let mut args = args.clone();
    if let Some(object) = args.as_object_mut() {
        object.remove("files");
    }
    format!("{:x}", Sha256::digest(format!("{hash}\0{template}\0{args}\0{toolchain}").as_bytes()))
}

/// The templates whose result is a verdict (tests, lint): another member's
/// result for the same key serves it. A build template's result is its
/// outputs in one member's tree, so it serves only that member.
fn verdict(template: &str) -> bool {
    matches!(template, "cargo-test" | "cargo-clippy" | "ctest" | "vstest")
}

/// The tools a template builds with, as their version lines; an unknown tool
/// is named so, and its tree is never served by another build. Templates the
/// broker does not know keep their own name.
fn toolchain(template: &str, tree: &Path) -> String {
    let version = |program: &str, args: &[&str]| -> String {
        Command::new(program)
            .args(args)
            .current_dir(tree)
            .stdin(Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map_or_else(|| format!("{program}:unknown"), |output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    if template.starts_with("cargo-") {
        format!("{}\n{}", version("rustc", &["-vV"]), version("cargo", &["-V"]))
    } else if template.starts_with("qt-") || template.starts_with("cmake-") || template == "ctest" {
        // Task 85: CMake's version line; ctest is CMake's.
        version("cmake", &["--version"])
    } else if template == "msbuild-build" || template == "vstest" {
        // Task 85: the MSBuild that builds, as Visual Studio names it.
        let msbuild = super::templates::msbuild_path();
        version(&msbuild, &["-version", "-nologo"])
    } else {
        format!("template:{template}")
    }
}

pub(crate) fn cached(shared: &Path, key: &str) -> Option<Value> {
    let list: Vec<Value> = fs::read(results_path(shared)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok())?;
    list.into_iter().rev().find(|entry| entry["key"] == key)
}

pub(crate) fn remember(shared: &Path, key: &str, entry: Value) {
    let path = results_path(shared);
    let mut list: Vec<Value> = fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    list.retain(|old| old["key"] != key);
    let mut entry = entry;
    entry["key"] = json!(key);
    list.push(entry);
    let excess = list.len().saturating_sub(RESULTS);
    list.drain(..excess);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, serde_json::to_vec(&list).unwrap_or_default());
}

/// The files a failed build's output names, relative to `tree` with '/': the
/// rustc, gcc/clang and MSVC forms (`--> src/a.rs:3:4`, `a.cpp:3:4: error`,
/// `D:\x\a.cpp(3): error`).
pub(crate) fn failing_files(output: &str, tree: &Path) -> BTreeSet<String> {
    let root = tree.display().to_string().replace('\\', "/").to_lowercase();
    let mut found = BTreeSet::new();
    for line in output.lines() {
        let line = line.trim().trim_start_matches("--> ").trim();
        let lower = line.to_lowercase();
        if !(lower.contains("error") || line.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '.' || c == '/')) {
            continue;
        }
        // The path ends at the first "(" or ":<digit>" after its start (a
        // drive's "D:" is part of it).
        let text = line.replace('\\', "/");
        let mut end = text.len();
        let bytes = text.as_bytes();
        for at in 2..bytes.len() {
            if bytes[at] == b'(' || (bytes[at] == b':' && bytes.get(at + 1).is_some_and(u8::is_ascii_digit)) {
                end = at;
                break;
            }
        }
        let candidate = text[..end].trim();
        if candidate.is_empty() || candidate.contains(' ') {
            continue;
        }
        let path = if candidate.to_lowercase().starts_with(&root) { candidate[root.len()..].trim_start_matches('/') } else { candidate };
        if !path.is_empty() && relative(path) && path.contains('.') {
            found.insert(path.to_owned());
        }
    }
    found
}

/// The line the asker reads, and the message its owner gets, for a failure in
/// files another member is changing; None when every failing file is the
/// asker's own (or unknown).
pub(crate) fn blame(failing: &BTreeSet<String>, others: &BTreeMap<String, String>, team: &Team) -> Option<(String, BTreeMap<String, Vec<String>>)> {
    let mut by_member: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in failing {
        if let Some(owner) = others.get(path) {
            by_member.entry(owner.clone()).or_default().push(path.clone());
        }
    }
    if by_member.is_empty() {
        return None;
    }
    let names: Vec<String> = by_member.iter().map(|(member, paths)| format!("{} ({})", team.name_of(member), paths.join(", "))).collect();
    let line = format!(
        "Build broker: this build failed in files another member is changing (taken at HEAD in your tree): {}. \
         That may be their unfinished change or a dependency of yours: check whether your change touches what they \
         use; if not, wait for them and build again. Zerus told them. Never edit their files.",
        names.join("; ")
    );
    Some((line, by_member))
}

/// The files `member` said it changes, kept beside its snapshot (bounded).
fn edits_path(shared: &Path, member: &str) -> PathBuf {
    member_dir(shared, member).join("edits.json")
}

pub(crate) fn edits_of(shared: &Path, member: &str) -> Vec<String> {
    fs::read(edits_path(shared, member)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

/// Adds the files a build request names to the member's own edits; a file
/// that is clean again (its change committed or undone) leaves the list.
fn record_edits(shared: &Path, member: &str, named: &[String]) -> anyhow::Result<Vec<String>> {
    let dirty = dirty_files(shared);
    let mut edits: BTreeSet<String> = edits_of(shared, member).into_iter().filter(|path| dirty.as_ref().is_none_or(|dirty| dirty.contains(path))).collect();
    edits.extend(named.iter().map(|path| path.replace('\\', "/")).filter(|path| relative(path)));
    let edits: Vec<String> = edits.into_iter().take(5000).collect();
    let path = edits_path(shared, member);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec(&edits)?)?;
    Ok(edits)
}

/// The folder's changed files by git; None when git cannot read it (a folder
/// under another version control keeps every recorded edit).
fn dirty_files(shared: &Path) -> Option<BTreeSet<String>> {
    let status = git(shared, &["status", "--porcelain", "-z", "--untracked-files=all"]).ok()?;
    Some(nul_list(&status).into_iter().filter(|entry| entry.len() > 3).map(|entry| entry[3..].to_owned()).collect())
}

/// When `member` last asked for a build (its edits ready then).
fn ready_path(shared: &Path, member: &str) -> PathBuf {
    shared.join(".zerus-team").join("runs").join("broker-ready").join(format!("{member}.json"))
}

fn mark_ready(shared: &Path, member: &str) {
    let path = ready_path(shared, member);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, json!({"at":super::super::now()}).to_string());
}

/// The other members' changed files written since they last asked for a
/// build: their edits are not ready yet. A file no longer changed (committed
/// or undone) does not count.
pub(crate) fn unready(shared: &Path, team: &Team, member: &Member) -> BTreeMap<String, Vec<String>> {
    let dirty = dirty_files(shared);
    let mut found = BTreeMap::new();
    for other in sharing(team, member) {
        let ready = fs::read(ready_path(shared, &other))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|value| value["at"].as_f64())
            .unwrap_or(0.0);
        let files: Vec<String> = edits_of(shared, &other)
            .into_iter()
            .filter(|path| dirty.as_ref().is_none_or(|dirty| dirty.contains(path)))
            .filter(|path| {
                fs::metadata(shared.join(path))
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                    .is_some_and(|at| at.as_secs_f64() > ready)
            })
            .take(20)
            .collect();
        if !files.is_empty() {
            found.insert(other, files);
        }
    }
    found
}

/// Shared mode: the build runs in the folder itself, one at a time, once the
/// other members' edits are ready (bounded by `ready_wait`).
#[allow(clippy::too_many_arguments)]
fn run_shared(
    team: &Team,
    member: &Member,
    shared: &Path,
    template: &str,
    plain: &Value,
    config: &Config,
    timeout: Duration,
    others: &BTreeMap<String, String>,
) -> anyhow::Result<super::templates::Outcome> {
    // Asking for a build says this member's own edits are ready.
    mark_ready(shared, &member.id);
    let waiting = Waiting::start(shared, &member.id, template);
    waiting.note("snapshot", json!(false));
    let _queue = queue(&shared.join(".zerus-team").join("runs").join("broker-shared"), timeout)?;
    let started = Instant::now();
    let until = started + config.ready_wait.min(timeout);
    let mut pending = unready(shared, team, member);
    let initial_pending = !pending.is_empty();
    let mut told = false;
    while !pending.is_empty() && Instant::now() < until {
        if !told {
            waiting.note("waiting_for", json!(pending.keys().collect::<Vec<_>>()));
            told = true;
        }
        std::thread::sleep(Duration::from_millis(500));
        pending = unready(shared, team, member);
    }
    // "waited" is the time spent waiting for other members, not the time this
    // thread happened to be descheduled while checking readiness. A build with
    // no initial wait or a zero ready window reports no wait.
    let waited = if initial_pending && !config.ready_wait.is_zero() { started.elapsed().as_secs() } else { 0 };
    drop(waiting);
    let building = shared.join(".zerus-team").join("runs").join("broker-building");
    fs::create_dir_all(&building)?;
    let marker = building.join(format!("shared-{}", member.id));
    let _ = fs::write(&marker, json!({"member":member.id,"template":template,"started":super::super::now(),"snapshot":false}).to_string());
    let built = (|| -> anyhow::Result<super::templates::Outcome> {
        let plan = super::templates::plan(template, plain, shared)?;
        Ok(super::templates::execute(&plan, shared, timeout, &[]))
    })();
    let _ = fs::remove_file(&marker);
    let mut outcome = built?;
    let mut blamed: Vec<Value> = Vec::new();
    if outcome.exit != Some(0) {
        let failing = failing_files(&outcome.tail, shared);
        if let Some((line, by_member)) = blame(&failing, others, team) {
            blamed = by_member.iter().map(|(owner, paths)| json!({"member":owner,"files":paths})).collect();
            let line = line.replace("(taken at HEAD in your tree)", "(as they are now in the shared folder)");
            outcome.tail = format!("{}\n{line}", outcome.tail.trim_end());
            outcome.error = "the build failed in files another member is changing".into();
            for (owner, paths) in by_member {
                let Some(session) = team.member(&owner).and_then(|found| found.session.clone()) else { continue };
                let text = format!(
                    "[Zerus team \u{b7} from: Zerus \u{b7} build broker]\n{}'s build in the shared folder failed in files you are changing: {}.\n\
                     It built them as they are now, so this is an observation, not a verdict: it may be your unfinished change, or \
                     theirs depending on it. Nothing is decided; finish your change as planned and ask for a build when it is ready.",
                    team.name_of(&member.id),
                    paths.join(", ")
                );
                let _ = super::dispatcher::send(&session, &uuid::Uuid::new_v4().to_string(), &text);
            }
        }
    }
    let unfinished: Vec<String> = pending.iter().map(|(owner, files)| format!("{} ({})", team.name_of(owner), files.join(", "))).collect();
    let note = format!(
        "Build broker: built {template} in the shared folder itself (no copy){}{}",
        if waited > 0 { format!("; waited {waited} s for the other members' edits to be ready") } else { String::new() },
        if unfinished.is_empty() { String::new() } else { format!("; included unfinished changes of {}", unfinished.join("; ")) }
    );
    outcome.tail = format!("{}\n{note}", outcome.tail.trim_end());
    let key = format!("shared-{}", uuid::Uuid::new_v4());
    remember(shared, &key, json!({"member":member.id,"template":template,"at":super::super::now(),"hit":false,"snapshot":false,"blamed":blamed,"outcome":outcome_value(&outcome)}));
    Ok(outcome)
}

fn outcome_value(outcome: &super::templates::Outcome) -> Value {
    json!({"exit":outcome.exit,"tail":outcome.tail,"ms":outcome.ms,"timed_out":outcome.timed_out,"error":outcome.error,
        "failures":outcome.failures,"excerpt":outcome.excerpt,"report":outcome.report})
}

fn outcome_of(value: &Value) -> super::templates::Outcome {
    super::templates::Outcome {
        exit: value["exit"].as_i64().map(|code| code as i32),
        tail: value["tail"].as_str().unwrap_or_default().to_owned(),
        ms: value["ms"].as_u64().unwrap_or_default(),
        timed_out: value["timed_out"].as_bool().unwrap_or(false),
        error: value["error"].as_str().unwrap_or_default().to_owned(),
        failures: value["failures"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect(),
        excerpt: value["excerpt"].as_str().unwrap_or_default().to_owned(),
        report: serde_json::from_value(value["report"].clone()).ok().flatten(),
        tests: None,
    }
}

/// One build template of `member` through the broker: its own edits recorded,
/// its snapshot synced, one build at a time in its build folder, an unchanged
/// tree served by the build that already ran it, and a failure in another
/// member's file told as theirs (and to them).
pub(crate) fn run(team: &Team, member: &Member, shared: &Path, template: &str, args: &Value, config: &Config, timeout: Duration) -> anyhow::Result<super::templates::Outcome> {
    let named: Vec<String> = args["files"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
    let mut plain = args.clone();
    if let Some(object) = plain.as_object_mut() {
        object.remove("files");
    }
    let edits = record_edits(shared, &member.id, &named)?;
    let mut others = BTreeMap::new();
    for other in sharing(team, member) {
        for path in edits_of(shared, &other) {
            others.insert(path, other.clone());
        }
    }
    if config.shared {
        return run_shared(team, member, shared, template, &plain, config, timeout, &others);
    }
    let dir = member_dir(shared, &member.id);
    // #756: who waits, since when and for what, for the Builds tab.
    let waiting = Waiting::start(shared, &member.id, template);
    let _queue = queue(&dir, timeout)?;
    let snapshot = sync(shared, &member.id, &edits, &others, config)?;
    let tools = toolchain(template, &snapshot.tree);
    let key = request_key(&snapshot.hash, template, &plain, &tools);
    // A finished result serves this request only when the tree is fully known,
    // the tools are, and it is a verdict or this member's own outputs.
    let serves = |entry: &Value| snapshot.known && !tools.contains(":unknown") && (verdict(template) || entry["member"] == member.id.as_str());
    // Another member building the very same tree now: wait for it. The marker
    // is taken atomically, so two members never build one key at once; a
    // marker older than the timeout is a crashed build's and goes.
    let building = shared.join(".zerus-team").join("runs").join("broker-building");
    fs::create_dir_all(&building)?;
    let marker = building.join(&key);
    let until = Instant::now() + timeout;
    let mut told = false;
    let served = loop {
        if let Some(entry) = cached(shared, &key).filter(|entry| serves(entry)) {
            break Some(entry);
        }
        match fs::OpenOptions::new().write(true).create_new(true).open(&marker) {
            Ok(mut file) => {
                use std::io::Write;
                let _ = file.write_all(json!({"member":member.id,"template":template,"started":super::super::now()}).to_string().as_bytes());
                break None;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = fs::metadata(&marker).and_then(|meta| meta.modified()).ok().and_then(|at| at.elapsed().ok()).is_some_and(|age| age > timeout);
                if stale {
                    let _ = fs::remove_file(&marker);
                    continue;
                }
                if !told {
                    waiting.coalesced(fs::read(&marker).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()).and_then(|value| value["member"].as_str().map(str::to_owned)));
                    told = true;
                }
                anyhow::ensure!(Instant::now() < until, "the same tree's build by another member did not finish in {} s", timeout.as_secs());
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(error) => return Err(error.into()),
        }
    };
    drop(waiting);
    if let Some(entry) = served {
        // #756: a result served for an unchanged tree counts as a cache hit.
        remember(shared, &key, json!({"member":member.id,"template":template,"at":super::super::now(),"hit":true,"by":entry["member"],"outcome":entry["outcome"]}));
        let mut outcome = outcome_of(&entry["outcome"]);
        let by = entry["member"].as_str().unwrap_or_default();
        outcome.tail = if by == member.id {
            format!("{}\nBuild broker: this tree was already built here at {:.0}; its outputs are in place.", outcome.tail.trim_end(), entry["at"].as_f64().unwrap_or_default())
        } else {
            format!(
                "{}\nBuild broker: the same tree and tools were checked by {} at {:.0}; this is its verdict (no outputs were made here).",
                outcome.tail.trim_end(),
                team.name_of(by),
                entry["at"].as_f64().unwrap_or_default()
            )
        };
        return Ok(outcome);
    }
    let built = (|| -> anyhow::Result<super::templates::Outcome> {
        let plan = super::templates::plan(template, &plain, &snapshot.tree)?;
        Ok(super::templates::execute(&plan, &snapshot.tree, timeout, &[]))
    })();
    let _ = fs::remove_file(&marker);
    let mut outcome = built?;
    let mut blamed: Vec<Value> = Vec::new();
    let note = format!(
        "Build broker: built {} in {} ({} files written{}, {} folders linked{}){}",
        template,
        snapshot.tree.display(),
        snapshot.copied,
        if snapshot.large > 0 { format!(", {} of them large", snapshot.large) } else { String::new() },
        snapshot.linked,
        if snapshot.known { "" } else { "; a linked folder is too large to list, so this result is never reused" },
        if snapshot.others.is_empty() { String::new() } else { format!("; at HEAD, being changed by others: {}", snapshot.others.keys().cloned().collect::<Vec<_>>().join(", ")) }
    );
    if outcome.exit != Some(0) {
        let failing = failing_files(&outcome.tail, &snapshot.tree);
        if let Some((line, by_member)) = blame(&failing, &snapshot.others, team) {
            blamed = by_member.iter().map(|(owner, paths)| json!({"member":owner,"files":paths})).collect();
            outcome.tail = format!("{}\n{line}", outcome.tail.trim_end());
            outcome.error = "the build failed in files another member is changing".into();
            for (owner, paths) in by_member {
                let Some(session) = team.member(&owner).and_then(|found| found.session.clone()) else { continue };
                let text = format!(
                    "[Zerus team \u{b7} from: Zerus \u{b7} build broker]\n{}'s build failed in files you are changing: {}.\n\
                     It built them at HEAD, so this is an observation, not a verdict: it may be your unfinished change, or \
                     theirs depending on it. Nothing is decided; finish your change as planned.",
                    team.name_of(&member.id),
                    paths.join(", ")
                );
                let _ = super::dispatcher::send(&session, &uuid::Uuid::new_v4().to_string(), &text);
            }
        }
    }
    outcome.tail = format!("{}\n{note}", outcome.tail.trim_end());
    remember(shared, &key, json!({"member":member.id,"template":template,"at":super::super::now(),"hit":false,"blamed":blamed,"outcome":outcome_value(&outcome)}));
    Ok(outcome)
}

/// #756: a member waiting for its build folder, or for the same tree another
/// member builds; the file goes when it stops waiting.
struct Waiting {
    path: PathBuf,
    entry: std::cell::RefCell<Value>,
}

impl Waiting {
    fn start(shared: &Path, member: &str, template: &str) -> Self {
        let dir = shared.join(".zerus-team").join("runs").join("broker-waiting");
        let _ = fs::create_dir_all(&dir);
        let waiting = Waiting {
            path: dir.join(format!("{member}.json")),
            entry: std::cell::RefCell::new(json!({"member":member,"template":template,"since":super::super::now()})),
        };
        let _ = fs::write(&waiting.path, waiting.entry.borrow().to_string());
        waiting
    }
    fn coalesced(&self, with: Option<String>) {
        self.note("coalesced_with", json!(with));
    }
    fn note(&self, key: &str, value: Value) {
        let mut entry = self.entry.borrow_mut();
        entry[key] = value;
        let _ = fs::write(&self.path, entry.to_string());
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// The build folder a template builds in: cargo's target or the Qt build.
pub(crate) fn build_folder(template: &str) -> &'static str {
    if template.starts_with("cargo") {
        "target"
    } else {
        ".ci-build/gui"
    }
}

/// The build folders a shared folder's builds are grouped by.
const BUILD_GROUPS: [&str; 3] = ["target", ".ci-build/gui", "."];

/// Whether a running or waiting build belongs to the build folder `name`.
fn in_group(entry: &Value, name: &str) -> bool {
    (if entry["snapshot"] == false { "." } else { build_folder(entry["template"].as_str().unwrap_or("cmake-build")) }) == name
}

/// Each member's build state in the build folder `name`: building, else
/// waiting with how many builds are ahead of it.
fn member_states(running: &[Value], waiting: &[Value], name: &str, now: f64, states: &mut serde_json::Map<String, Value>) {
    let now_running: Vec<&Value> = running.iter().filter(|entry| in_group(entry, name)).collect();
    let mut queue: Vec<&Value> = waiting.iter().filter(|entry| in_group(entry, name)).collect();
    queue.sort_by(|a, b| {
        let since = |entry: &Value| entry["since"].as_f64().unwrap_or(now);
        since(a).partial_cmp(&since(b)).unwrap_or(std::cmp::Ordering::Equal)
    });
    for entry in &now_running {
        if let Some(member) = entry["member"].as_str() {
            states.insert(
                member.to_owned(),
                json!({"state":"building","snapshot":entry["snapshot"] != false,"template":entry["template"],"since":entry["started"].as_f64().unwrap_or(now)}),
            );
        }
    }
    for (ahead, entry) in queue.iter().enumerate() {
        if let Some(member) = entry["member"].as_str() {
            states.entry(member.to_owned()).or_insert_with(|| {
                json!({"state":"waiting","template":entry["template"],"since":entry["since"].as_f64().unwrap_or(now),"ahead":ahead + now_running.len()})
            });
        }
    }
}

/// Task #5 (team ede724c4): `builds_view`'s `members` alone, for the polled
/// status: the building and waiting folders only, without each member's
/// changed files (a git read per member), the timeline or the cache server.
pub(crate) fn build_states(team: &Team, now: f64) -> Value {
    let mut states = serde_json::Map::new();
    for (folder, _) in shared_folders(team) {
        let runs = Path::new(&folder).join(".zerus-team").join("runs");
        let running = read_dir_json(&runs.join("broker-building"));
        let waiting = read_dir_json(&runs.join("broker-waiting"));
        for name in BUILD_GROUPS {
            member_states(&running, &waiting, name, now, &mut states);
        }
    }
    Value::Object(states)
}

fn read_dir_json(dir: &Path) -> Vec<Value> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .take(200)
        .filter_map(|entry| fs::read(entry.path()).ok())
        .filter_map(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(Value::is_object)
        .collect()
}

/// #756: what the Builds tab shows for each folder two or more members
/// share: per build folder the running build against its usual time, the
/// queue (with the requests that wait for the same tree), the cache's hit
/// rate and the recent builds; each member's changed files and the files two
/// members change; and each member's build state for nodes and cards.
pub(crate) fn builds_view(team: &Team, now: f64) -> Value {
    let mut folders = Vec::new();
    let mut states = serde_json::Map::new();
    for (folder, members) in shared_folders(team) {
        let shared = Path::new(&folder);
        let runs = shared.join(".zerus-team").join("runs");
        let results: Vec<Value> = fs::read(results_path(shared)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        let running = read_dir_json(&runs.join("broker-building"));
        let waiting = read_dir_json(&runs.join("broker-waiting"));
        let mut groups = Vec::new();
        for name in BUILD_GROUPS {
            let ours = |entry: &&Value| in_group(entry, name);
            let finished: Vec<&Value> = results.iter().filter(ours).collect();
            let mut built: Vec<f64> = finished
                .iter()
                .filter(|entry| entry["hit"] != true && entry["outcome"]["exit"] == 0)
                .filter_map(|entry| entry["outcome"]["ms"].as_f64())
                .collect();
            built.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let usual = built.get(built.len() / 2).copied();
            let hits = finished.iter().filter(|entry| entry["hit"] == true).count();
            let now_running: Vec<Value> = running
                .iter()
                .filter(ours)
                .map(|entry| {
                    let started = entry["started"].as_f64().unwrap_or(now);
                    json!({"member":entry["member"],"template":entry["template"],"started":started,"seconds":(now - started).max(0.0),
                        "usual_ms":usual,"snapshot":entry["snapshot"] != false})
                })
                .collect();
            let mut queue: Vec<Value> = waiting
                .iter()
                .filter(ours)
                .map(|entry| {
                    let since = entry["since"].as_f64().unwrap_or(now);
                    json!({"member":entry["member"],"template":entry["template"],"since":since,"seconds":(now - since).max(0.0),
                        "coalesced_with":entry["coalesced_with"],"waiting_for":entry["waiting_for"]})
                })
                .collect();
            queue.sort_by(|a, b| a["since"].as_f64().partial_cmp(&b["since"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
            member_states(&running, &waiting, name, now, &mut states);
            let timeline: Vec<Value> = finished
                .iter()
                .rev()
                .take(20)
                .map(|entry| json!({"member":entry["member"],"template":entry["template"],"at":entry["at"],"hit":entry["hit"] == true,"by":entry["by"],
                    "ms":entry["outcome"]["ms"],"exit":entry["outcome"]["exit"],"timed_out":entry["outcome"]["timed_out"],
                    "error":entry["outcome"]["error"],"blamed":entry["blamed"]}))
                .collect();
            if now_running.is_empty() && queue.is_empty() && timeline.is_empty() {
                continue;
            }
            let rate = if finished.is_empty() { Value::Null } else { json!(hits as f64 / finished.len() as f64) };
            groups.push(json!({"name":name,"running":now_running,"queue":queue,"usual_ms":usual,"hit_rate":rate,"timeline":timeline}));
        }
        let mut changes = Vec::new();
        let mut by_file: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for member in &members {
            let files = edits_of(shared, member);
            for file in &files {
                by_file.entry(file.clone()).or_default().push(member.clone());
            }
            changes.push(json!({"member":member,"files":files}));
        }
        let overlaps: Vec<Value> = by_file.into_iter().filter(|(_, who)| who.len() >= 2).map(|(file, who)| json!({"file":file,"members":who})).collect();
        folders.push(json!({"folder":folder,"members":members,"build_folders":groups,"changes":changes,"overlaps":overlaps}));
    }
    // Task 261: the shared sccache server's health: whether it runs, its pid
    // and integrity (a restricted one breaks every build).
    json!({"team":team.id,"enabled":team.settings.build_broker,"folders":folders,"members":states,"deliveries":super::artifacts::view(&team.id),
        "cache_server":super::super::build_cache_server::health()})
}

#[cfg(test)]
#[path = "broker_tests.rs"]
mod tests;
