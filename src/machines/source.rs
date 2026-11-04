//! Private, bounded CLI source snapshot for explicit machine setup.
//! Follow local Cargo path dependencies, never package build outputs or a GUI.
use super::*;
use std::{collections::VecDeque, path::PathBuf};
use toml_edit::{DocumentMut, Item};

const MAX_PACKAGES: usize = 32;
const MAX_ENTRIES: usize = 20_000;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_FILE: usize = 8 * 1024 * 1024;
const MAX_MANIFEST: usize = 1024 * 1024;
const MAX_DEPTH: usize = 32;
const FAILED: &str = "Could not snapshot the HGS CLI sources. Use a complete, unchanged checkout with local path dependencies inside it.";

pub(super) struct Snapshot {
    directory: tempfile::TempDir,
    root: PathBuf,
    files: BTreeSet<PathBuf>,
    entries: usize,
    bytes: usize,
    deadline: Instant,
}
impl Snapshot {
    pub(super) fn new(root: &Path) -> Result<Self> {
        let mut snapshot = Self {
            directory: tempfile::tempdir()?,
            root: fs::canonicalize(root)?,
            files: BTreeSet::new(),
            entries: 0,
            bytes: 0,
            deadline: Instant::now() + Duration::from_secs(5),
        };
        for file in ["Cargo.lock", "install.sh", "hgs_state.py"] {
            snapshot.file(Path::new(file), MAX_FILE)?;
        }
        // The CLI includes these pure shared modules by #[path], outside the
        // Cargo package graph. Older source checkouts may not have them.
        for file in [
            "services/mobile/relay/src/voice_video.rs",
            "services/mobile/relay/src/voice_rejection.rs",
        ] {
            match fs::symlink_metadata(snapshot.root.join(file)) {
                Ok(_) => {
                    snapshot.file(Path::new(file), MAX_FILE)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        snapshot.tree(Path::new("scripts"), 0)?;
        let mut pending = VecDeque::from([snapshot.root.clone()]);
        let mut packages = BTreeSet::new();
        while let Some(package) = pending.pop_front() {
            snapshot.check()?;
            let package = fs::canonicalize(package)?;
            if !package.starts_with(&snapshot.root) {
                return Err(Error::new(1, FAILED));
            }
            if !packages.insert(package.clone()) {
                continue;
            }
            if packages.len() > MAX_PACKAGES {
                return Err(Error::new(1, FAILED));
            }
            let relative = package
                .strip_prefix(&snapshot.root)
                .map_err(|_| Error::new(1, FAILED))?;
            let manifest = snapshot.file(&relative.join("Cargo.toml"), MAX_MANIFEST)?;
            let text = std::str::from_utf8(&manifest).map_err(|_| Error::new(1, FAILED))?;
            let document = text
                .parse::<DocumentMut>()
                .map_err(|_| Error::new(1, FAILED))?;
            // Workspace inheritance needs its own source-root contract. Current
            // hgs and its path packages declare complete independent manifests.
            if ["workspace", "patch", "replace"]
                .iter()
                .any(|key| document.contains_key(key))
            {
                return Err(Error::new(1, FAILED));
            }
            snapshot.tree(&relative.join("src"), 0)?;
            let build = document.get("package").and_then(|p| p.get("build"));
            if let Some(build) = build.and_then(Item::as_str) {
                snapshot.file(&relative.join(build), MAX_FILE)?;
            } else if build.and_then(Item::as_bool) != Some(false)
                && package.join("build.rs").exists()
            {
                snapshot.file(&relative.join("build.rs"), MAX_FILE)?;
            }
            if let Some(path) = document
                .get("lib")
                .and_then(|p| p.get("path"))
                .and_then(Item::as_str)
            {
                snapshot.file(&relative.join(path), MAX_FILE)?;
            }
            if let Some(binaries) = document.get("bin").and_then(Item::as_array_of_tables) {
                for binary in binaries {
                    if let Some(path) = binary.get("path").and_then(Item::as_str) {
                        snapshot.file(&relative.join(path), MAX_FILE)?;
                    }
                }
            }
            let mut paths = Vec::new();
            dependency_paths(document.as_item(), 0, &mut paths)?;
            for path in paths {
                if Path::new(&path).is_absolute() {
                    return Err(Error::new(1, FAILED));
                }
                pending.push_back(package.join(path));
                if pending.len() > MAX_ENTRIES {
                    return Err(Error::new(1, FAILED));
                }
            }
        }
        Ok(snapshot)
    }

    pub(super) fn path(&self) -> &Path {
        self.directory.path()
    }
    fn check(&self) -> Result<()> {
        if Instant::now() >= self.deadline || self.entries > MAX_ENTRIES {
            Err(Error::new(
                1,
                "HGS source snapshot exceeded its bounded setup budget.",
            ))
        } else {
            Ok(())
        }
    }
    fn source(&self, relative: &Path) -> Result<PathBuf> {
        if relative.is_absolute()
            || relative
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(Error::new(1, FAILED));
        }
        let path = self.root.join(relative);
        if !fs::canonicalize(&path)?.starts_with(&self.root) {
            return Err(Error::new(1, FAILED));
        }
        Ok(path)
    }
    fn tree(&mut self, relative: &Path, depth: usize) -> Result<()> {
        self.check()?;
        let path = self.source(relative)?;
        if depth > MAX_DEPTH || !fs::symlink_metadata(&path)?.is_dir() {
            return Err(Error::new(1, FAILED));
        }
        for entry in fs::read_dir(path)? {
            self.entries += 1;
            self.check()?;
            let entry = entry?;
            let name = entry.file_name();
            let text = name.to_str().ok_or_else(|| Error::new(1, FAILED))?;
            if text.starts_with('.') || matches!(text, "target" | "node_modules" | "__pycache__") {
                continue;
            }
            let child = relative.join(name);
            let kind = entry.file_type()?;
            if kind.is_dir() {
                self.tree(&child, depth + 1)?;
            } else if kind.is_file() {
                self.file(&child, MAX_FILE)?;
            } else {
                return Err(Error::new(1, FAILED));
            }
        }
        Ok(())
    }
    fn file(&mut self, relative: &Path, limit: usize) -> Result<Vec<u8>> {
        self.check()?;
        let path = self.source(relative)?;
        let relative = fs::canonicalize(&path)?
            .strip_prefix(&self.root)
            .map_err(|_| Error::new(1, FAILED))?
            .to_owned();
        let file = platform::no_follow(fs::OpenOptions::new().read(true)).open(&path)?;
        let (identity, metadata) = platform::FileIdentity::with_metadata(&file)?;
        if !metadata.is_file()
            || metadata.len() > limit as u64
            || fs::symlink_metadata(&path)?.file_type().is_symlink()
        {
            return Err(Error::new(1, FAILED));
        }
        let mut bytes = Vec::new();
        (&file).take((limit + 1) as u64).read_to_end(&mut bytes)?;
        if bytes.len() > limit
            || bytes.len() as u64 != metadata.len()
            || platform::FileIdentity::of(&file)? != identity
            || platform::FileIdentity::at(&path)?.0 != identity
        {
            return Err(Error::new(1, FAILED));
        }
        if self.files.insert(relative.clone()) {
            self.entries += 1;
            self.bytes = self
                .bytes
                .checked_add(bytes.len())
                .ok_or_else(|| Error::new(1, FAILED))?;
            self.check()?;
            if self.bytes > MAX_BYTES {
                return Err(Error::new(1, FAILED));
            }
            let destination = self.path().join(relative);
            fs::create_dir_all(destination.parent().ok_or_else(|| Error::new(1, FAILED))?)?;
            fs::write(destination, &bytes)?;
        }
        Ok(bytes)
    }
}

fn dependency_paths(item: &Item, depth: usize, paths: &mut Vec<String>) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::new(1, FAILED));
    }
    if let Some(table) = item.as_table_like() {
        for (key, value) in table.iter() {
            if matches!(
                key,
                "dependencies" | "build-dependencies" | "dev-dependencies"
            ) {
                if let Some(dependencies) = value.as_table_like() {
                    for (_, dependency) in dependencies.iter() {
                        if let Some(path) = dependency.get("path").and_then(Item::as_str) {
                            paths.push(path.to_owned());
                        }
                        if paths.len() > MAX_ENTRIES || dependency.get("workspace").is_some() {
                            return Err(Error::new(1, FAILED));
                        }
                    }
                }
            } else {
                dependency_paths(value, depth + 1, paths)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for (path, body) in [
            ("Cargo.toml", "[package]\nname='fixture'\nversion='0.1.0'\n[dependencies]\nchild={path='crates/child'}\n"),
            ("Cargo.lock", "# fixture"), ("install.sh", "# fixture"), ("hgs_state.py", "# fixture"),
            ("src/main.rs", "fn main() {}"), ("scripts/install.py", "# fixture"),
            ("crates/child/Cargo.toml", "[package]\nname='child'\nversion='0.1.0'\n[target.'cfg(unix)'.dependencies]\ncore={path='../../client/core'}\n"),
            ("crates/child/src/lib.rs", "// child"),
            ("client/core/Cargo.toml", "[package]\nname='core'\nversion='0.1.0'\n[[bin]]\nname='fixture-helper'\npath='tests/helper.rs'\n"),
            ("client/core/src/lib.rs", "// core"), ("client/core/tests/helper.rs", "fn main() {}"),
        ] {
            let path = root.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
        root
    }
    #[test]
    fn follows_transitive_target_dependencies_and_declared_targets_only() {
        let root = fixture();
        let shared = root.path().join("services/mobile/relay/src");
        fs::create_dir_all(&shared).unwrap();
        for file in ["voice_video.rs", "voice_rejection.rs"] {
            fs::write(shared.join(file), "// pure shared validator").unwrap();
        }
        fs::write(shared.join("private.secret"), "must not travel").unwrap();
        for path in [
            "target/private",
            "client/core/target/private",
            "scripts/__pycache__/private",
            "scripts/.private",
            ".git/private",
            "owner.secret",
        ] {
            let path = root.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "must not travel").unwrap();
        }
        let snapshot = Snapshot::new(root.path()).unwrap();
        assert!(snapshot.path().join("crates/child/src/lib.rs").is_file());
        assert!(snapshot
            .path()
            .join("client/core/tests/helper.rs")
            .is_file());
        assert_eq!(snapshot.files.len(), 13);
        assert!(snapshot
            .path()
            .join("services/mobile/relay/src/voice_video.rs")
            .is_file());
        assert!(!snapshot
            .path()
            .join("services/mobile/relay/src/private.secret")
            .exists());
        for path in [
            "target",
            "client/core/target",
            "scripts/__pycache__",
            "scripts/.private",
            ".git",
            "owner.secret",
        ] {
            assert!(!snapshot.path().join(path).exists(), "{path}");
        }
    }
    #[test]
    fn snapshot_does_not_follow_later_source_changes() {
        let root = fixture();
        let snapshot = Snapshot::new(root.path()).unwrap();
        fs::write(root.path().join("src/main.rs"), "changed after snapshot").unwrap();
        assert_eq!(
            fs::read_to_string(snapshot.path().join("src/main.rs")).unwrap(),
            "fn main() {}"
        );
    }
    #[test]
    fn refuses_external_or_workspace_inherited_dependencies() {
        let root = fixture();
        let outside = fixture();
        fs::write(
            root.path().join("crates/child/Cargo.toml"),
            format!(
                "[dependencies]\noutside={{path={:?}}}\n",
                outside.path().display().to_string()
            ),
        )
        .unwrap();
        assert!(Snapshot::new(root.path()).is_err());
        fs::write(
            root.path().join("crates/child/Cargo.toml"),
            "[dependencies]\nunknown.workspace=true\n",
        )
        .unwrap();
        assert!(Snapshot::new(root.path()).is_err());
    }
    #[test]
    fn refuses_oversized_inputs_before_packaging_or_ssh() {
        let root = fixture();
        let f = fs::File::create(root.path().join("src/oversized.rs")).unwrap();
        f.set_len(MAX_FILE as u64 + 1).unwrap();
        assert!(Snapshot::new(root.path()).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn refuses_symbolic_source_entries() {
        let root = fixture();
        std::os::unix::fs::symlink("main.rs", root.path().join("src/alias.rs")).unwrap();
        assert!(Snapshot::new(root.path()).is_err());
    }
}
