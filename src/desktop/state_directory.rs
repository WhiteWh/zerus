//! Repair only the selected state directory's legacy Unix mode. No ownership,
//! service, journal, child entry or missing directory is created or adopted.
use super::store::Location;
use crate::config::Config;
use anyhow::Result;

pub(super) fn prepare(config: &Config, location: &Location) -> Result<()> {
    #[cfg(unix)]
    {
        tighten(
            &config.state,
            location.root.parent().expect("state directory"),
        )?;
    }
    #[cfg(not(unix))]
    {
        use anyhow::Context;
        let _ = config;
        // The state folder may inherit a sandbox group's read access (Codex
        // grants one on ~/.local) or a second drive's Authenticated Users
        // Modify, and the service would refuse to start. A folder this user
        // owns that Windows only left too open is made private in place; a
        // missing one is not created here, and one that stays open (another
        // owner, a link) is still refused.
        for path in [location.root.parent().context("missing state folder")?, &location.root] {
            match crate::platform::repair_private(path, crate::platform::Private::Directory) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Ok(crate::platform::PrivateRepair::Private | crate::platform::PrivateRepair::Repaired(_)) => {}
                Ok(crate::platform::PrivateRepair::Refused(why)) => {
                    anyhow::bail!("desktop folders must be private directories: {} ({why})", path.display())
                }
                Err(error) => return Err(error.into()),
            }
        }
        location.check_existing()?;
    }
    Ok(())
}

#[cfg(unix)]
fn tighten(selected: &std::path::Path, canonical: &std::path::Path) -> Result<()> {
    use anyhow::ensure;
    use std::{
        fs,
        os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    };
    let named = match fs::symlink_metadata(selected) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if named.file_type().is_symlink() {
        // Location/configuration already resolve aliases. Preserve an existing
        // PRIVATE target without chmod, but never repair through this alias.
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(canonical)?;
        let held = file.metadata()?;
        let resolved = fs::metadata(selected)?;
        let current = fs::symlink_metadata(canonical)?;
        ensure!(
            resolved.is_dir()
                && current.is_dir()
                && (resolved.dev(), resolved.ino()) == (held.dev(), held.ino())
                && (current.dev(), current.ino()) == (held.dev(), held.ino())
                && crate::platform::verify_private_directory(&file)?,
            "desktop state alias must name the same already-private directory"
        );
        return Ok(());
    }
    ensure!(
        named.is_dir() && named.uid() == unsafe { libc::geteuid() },
        "desktop state directory must belong to this user and not be a symlink"
    );
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(selected)?;
    let original = file.metadata()?;
    ensure!(
        original.is_dir() && original.uid() == unsafe { libc::geteuid() },
        "desktop state directory must belong to this user"
    );
    ensure!(
        (named.dev(), named.ino()) == (original.dev(), original.ino()),
        "desktop state directory changed"
    );
    let unchanged = || -> std::io::Result<bool> {
        let same = |path| -> std::io::Result<bool> {
            let now = fs::symlink_metadata(path)?;
            Ok(now.is_dir() && now.dev() == original.dev() && now.ino() == original.ino())
        };
        Ok(same(selected)? && same(canonical)?)
    };
    ensure!(unchanged()?, "desktop state directory changed");
    if original.mode() & 0o077 != 0 {
        // File::set_permissions uses fchmod: never chmod a newly substituted
        // pathname. Only REMOVE access, only on this already-owned directory.
        file.set_permissions(fs::Permissions::from_mode(original.mode() & 0o700))?;
    }
    ensure!(
        unchanged()? && crate::platform::verify_private_directory(&file)?,
        "desktop state directory changed"
    );
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    };

    #[test]
    fn repair_is_owned_directory_only_nonrecursive_and_does_not_create() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("sessions");
        tighten(&state, &state).unwrap();
        assert!(!state.exists());
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).unwrap();
        let child = state.join("keep");
        fs::write(&child, b"original").unwrap();
        fs::set_permissions(&child, fs::Permissions::from_mode(0o644)).unwrap();
        let before = fs::metadata(&state).unwrap();
        tighten(&state, &state).unwrap();
        tighten(&state, &state).unwrap();
        let after = fs::metadata(&state).unwrap();
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        assert_eq!(after.mode() & 0o777, 0o700);
        assert_eq!(fs::read(&child).unwrap(), b"original");
        assert_eq!(fs::metadata(&child).unwrap().mode() & 0o777, 0o644);
        assert_eq!(fs::read_dir(&state).unwrap().count(), 1);
    }

    #[test]
    fn repair_refuses_symlink_file_foreign_owner_and_mismatched_identity() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("sessions");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).unwrap();
        let link = temp.path().join("link");
        symlink(&state, &link).unwrap();
        assert!(tighten(&link, &state).is_err());
        fs::remove_file(&link).unwrap();
        symlink(temp.path().join("missing"), &link).unwrap();
        assert!(tighten(&link, &state).is_err());
        let file = temp.path().join("file");
        fs::write(&file, b"keep").unwrap();
        assert!(tighten(&file, &file).is_err());
        assert!(tighten(&state, temp.path()).is_err());
        assert_eq!(fs::metadata(&state).unwrap().mode() & 0o777, 0o755);
        // A real root-owned directory, never chmodded by this non-root control.
        if unsafe { libc::geteuid() } != fs::metadata("/").unwrap().uid() {
            let before = fs::metadata("/").unwrap().mode();
            assert!(tighten(std::path::Path::new("/"), std::path::Path::new("/")).is_err());
            assert_eq!(fs::metadata("/").unwrap().mode(), before);
        }
    }

    #[test]
    fn an_already_private_alias_is_nonmutating_but_never_authorizes_repair() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("sessions");
        crate::platform::private_dir(&state).unwrap();
        let link = temp.path().join("alias");
        symlink(&state, &link).unwrap();
        let before = fs::metadata(&state).unwrap();
        tighten(&link, &state).unwrap();
        let after = fs::metadata(&state).unwrap();
        assert_eq!(
            (
                before.dev(),
                before.ino(),
                before.mode(),
                before.ctime(),
                before.ctime_nsec()
            ),
            (
                after.dev(),
                after.ino(),
                after.mode(),
                after.ctime(),
                after.ctime_nsec()
            )
        );
        assert!(tighten(&link, temp.path()).is_err());
        fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(tighten(&link, &state).is_err());
        assert_eq!(fs::metadata(&state).unwrap().mode() & 0o777, 0o755);
    }
}
