use super::*;
use std::fs;
struct Private {
    root: PathBuf,
    path: PathBuf,
}
impl Private {
    fn new() -> Self {
        // The store refuses symlinked ancestors, and macOS's TMPDIR sits under
        // the /var symlink; resolve it so only the fixture's own paths are tested.
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(crate::fsx::temporary(Path::new("legacy-config")));
        fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self {
            path: root.join("config/connector.json"),
            root,
        }
    }
    fn edit(&self, relay: &str, token: &str, dir: &str) -> Edit {
        Edit {
            relay: relay.into(),
            replacement: token.into(),
            directory: self.root.join(dir),
        }
    }
    fn initial(&self) -> Snapshot {
        save(
            &self.path,
            &load(&self.path).unwrap(),
            self.edit(
                "https://relay.example.invalid",
                "fixture-credential",
                "receipts-one",
            ),
            &self.root,
        )
        .unwrap()
    }
}
impl Drop for Private {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn opening_missing_configuration_creates_nothing() {
    let p = Private::new();
    assert!(!load(&p.path).unwrap().exists());
    assert!(!p.path.parent().unwrap().exists());
}
#[test]
fn saves_privately_retains_hidden_credential_unknown_fields_and_receipts() {
    let p = Private::new();
    let mut s = p.initial();
    s.object["provider_unknown"] = json!({"version":7});
    // Unknown fields are a real input file, rather than a renderer-supplied object.
    crate::fsx::write_private_atomic(&p.path, &serde_json::to_vec(&s.object).unwrap(), true)
        .unwrap();
    let s = load(&p.path).unwrap();
    fs::write(p.root.join("receipts-one/keep"), b"original receipt").unwrap();
    let saved = save(
        &p.path,
        &s,
        p.edit("https://relay.example.invalid/", "", "receipts-one"),
        &p.root,
    )
    .unwrap();
    assert_eq!(saved.object["node_token"], "fixture-credential");
    assert_eq!(saved.object["provider_unknown"]["version"], 7);
    assert_eq!(
        fs::read(p.root.join("receipts-one/keep")).unwrap(),
        b"original receipt"
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(&p.path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(p.root.join("receipts-one"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}
#[test]
fn new_binding_requires_new_credential_and_unused_receipts() {
    let p = Private::new();
    let s = p.initial();
    assert!(save(
        &p.path,
        &s,
        p.edit("https://other.example.invalid", "", "new"),
        &p.root
    )
    .is_err());
    assert!(!p.root.join("new").exists());
    assert!(save(
        &p.path,
        &s,
        p.edit(
            "https://other.example.invalid",
            "new-credential",
            "receipts-one"
        ),
        &p.root
    )
    .is_err());
    let next = save(
        &p.path,
        &s,
        p.edit("https://other.example.invalid", "new-credential", "new"),
        &p.root,
    )
    .unwrap();
    assert_eq!(next.relay(), "https://other.example.invalid");
    assert!(p.root.join("receipts-one").is_dir());
}
#[test]
fn official_migration_preserves_receipt_namespace() {
    let p = Private::new();
    let s = p.initial();
    let mut object = s.object.clone();
    object["server_url"] = json!(PREVIOUS);
    crate::fsx::write_private_atomic(&p.path, &serde_json::to_vec(&object).unwrap(), true).unwrap();
    let s = load(&p.path).unwrap();
    let saved = save(&p.path, &s, p.edit(OFFICIAL, "", "receipts-one"), &p.root).unwrap();
    assert_eq!(saved.object["identity_url"], PREVIOUS);
    assert_eq!(saved.object["node_token"], s.object["node_token"]);
}
#[test]
fn stale_even_byte_identical_replacement_is_refused_before_new_receipts() {
    let p = Private::new();
    let s = p.initial();
    crate::fsx::write_private_atomic(&p.path, s.bytes.as_ref().unwrap(), true).unwrap();
    assert_eq!(
        save(
            &p.path,
            &s,
            p.edit("https://other.example.invalid", "new-credential", "new"),
            &p.root
        )
        .err()
        .unwrap()
        .message(),
        CHANGED
    );
    assert!(!p.root.join("new").exists());
}
#[test]
fn encrypted_or_malformed_private_config_never_becomes_a_legacy_edit() {
    let p = Private::new();
    let s = p.initial();
    let mut object = s.object;
    object["encrypted_workspace"] = json!({"version":1});
    crate::fsx::write_private_atomic(&p.path, &serde_json::to_vec(&object).unwrap(), true).unwrap();
    let s = load(&p.path).unwrap();
    assert_eq!(
        save(&p.path, &s, p.edit(OFFICIAL, "", "receipts-one"), &p.root)
            .err()
            .unwrap()
            .message(),
        ENCRYPTED
    );
    fs::write(&p.path, b"{ malformed credential content").unwrap();
    assert_eq!(load(&p.path).err().unwrap(), NOT_OBJECT);
    fs::write(&p.path, b"[]").unwrap();
    assert_eq!(load(&p.path).err().unwrap(), NOT_OBJECT);
    fs::write(&p.path, vec![b' '; 65537]).unwrap();
    assert_eq!(load(&p.path).err().unwrap(), PRIVATE_FILE);
}
#[test]
fn symlinks_hardlinks_public_files_and_fifo_fail_without_side_effects() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let p = Private::new();
    p.initial();
    let other = p.root.join("untouched");
    fs::write(&other, b"private evidence").unwrap();
    fs::remove_file(&p.path).unwrap();
    symlink(&other, &p.path).unwrap();
    assert_eq!(load(&p.path).err().unwrap(), UNREADABLE);
    fs::remove_file(&p.path).unwrap();
    fs::hard_link(&other, &p.path).unwrap();
    assert_eq!(load(&p.path).err().unwrap(), PRIVATE_FILE);
    fs::remove_file(&p.path).unwrap();
    fs::write(&p.path, b"{}").unwrap();
    fs::set_permissions(&p.path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(load(&p.path).err().unwrap(), PRIVATE_FILE);
    fs::remove_file(&p.path).unwrap();
    let c = std::ffi::CString::new(p.path.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: an owned scratch path, valid NUL-terminated string and private FIFO mode.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    assert_eq!(load(&p.path).err().unwrap(), PRIVATE_FILE);
    assert_eq!(fs::read(&other).unwrap(), b"private evidence");
}
#[test]
fn parent_symlinks_and_relative_segments_are_refused() {
    use std::os::unix::fs::symlink;
    let p = Private::new();
    fs::create_dir(p.root.join("target")).unwrap();
    symlink(p.root.join("target"), p.path.parent().unwrap()).unwrap();
    assert!(load(&p.path).is_err());
    assert!(save(
        &p.path,
        &Snapshot::default(),
        p.edit(OFFICIAL, "fixture-credential", "new"),
        &p.root
    )
    .is_err());
    assert!(fs::read_dir(p.root.join("target"))
        .unwrap()
        .next()
        .is_none());
    let p = Private::new();
    assert!(save(
        &p.path,
        &Snapshot::default(),
        p.edit(OFFICIAL, "fixture-credential", "../escape"),
        &p.root
    )
    .is_err());
}
