//! The attachment cache and file reads on a real disk, in a private scratch
//! folder per test.
use std::fs;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use zerus_client_core::attachments::{
    cached_at, prepare, prune, read_file, read_stored, store_at, touch, Attachment, StoredFile,
    MAX_ATTACHMENT_BYTES,
};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let path = std::env::temp_dir().join(format!(
            "zerus-client-core-attachments-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Scratch(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const HELLO_SHA256: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

fn attachment(name: &str, bytes: &[u8]) -> Attachment {
    Attachment {
        name: name.into(),
        mime: "text/plain".into(),
        bytes: Arc::from(bytes),
        reference: "[File #1]".into(),
    }
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn copies_are_content_addressed_private_and_reused() {
    let scratch = Scratch::new("store");
    let root = scratch.0.join("cache");
    let path = store_at(&root, "/home/example/notes: v1.txt", b"hello").unwrap();
    assert_eq!(path, root.join(HELLO_SHA256).join("notes_ v1.txt"));
    assert_eq!(fs::read(&path).unwrap(), b"hello");
    #[cfg(unix)]
    {
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&root.join(HELLO_SHA256)), 0o700);
        assert_eq!(mode(&path), 0o600);
    }
    // An identical copy is reused, not rewritten (it is only marked used).
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&path).unwrap().ino()
    };
    assert_eq!(store_at(&root, "notes: v1.txt", b"hello").unwrap(), path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    }
    // A damaged copy is replaced; no temporary file stays behind.
    fs::write(&path, b"HELLO").unwrap();
    assert_eq!(store_at(&root, "notes: v1.txt", b"hello").unwrap(), path);
    assert_eq!(fs::read(&path).unwrap(), b"hello");
    assert_eq!(fs::read_dir(root.join(HELLO_SHA256)).unwrap().count(), 1);
    // Out of bounds.
    assert!(store_at(&root, "empty", b"").is_none());
    assert!(store_at(&root, "big", &vec![0; MAX_ATTACHMENT_BYTES + 1]).is_none());
}

#[cfg(unix)]
#[test]
fn links_are_never_followed() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new("links");
    let root = scratch.0.join("cache");
    let outside = scratch.0.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("target"), b"keep").unwrap();
    // A link in place of the copy is removed, and the target is untouched.
    fs::create_dir_all(root.join(HELLO_SHA256)).unwrap();
    let path = root.join(HELLO_SHA256).join("a.txt");
    symlink(outside.join("target"), &path).unwrap();
    assert_eq!(store_at(&root, "a.txt", b"hello").unwrap(), path);
    assert!(!fs::symlink_metadata(&path)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(outside.join("target")).unwrap(), b"keep");
    // A digest folder that leads elsewhere refuses the copy.
    let other = scratch.0.join("other-cache");
    fs::create_dir_all(&other).unwrap();
    symlink(&outside, other.join(HELLO_SHA256)).unwrap();
    assert!(store_at(&other, "a.txt", b"hello").is_none());
    assert!(!outside.join("a.txt").exists());
    // A cached path must stay inside the root after resolving links.
    let escape = root.join(HELLO_SHA256).join("escape");
    symlink(outside.join("target"), &escape).unwrap();
    assert!(cached_at(&root, &escape).is_none());
}

#[test]
fn cached_paths_are_nonempty_files_inside_the_root() {
    let scratch = Scratch::new("cached");
    let root = scratch.0.join("cache");
    let path = store_at(&root, "a.txt", b"hello").unwrap();
    assert_eq!(
        cached_at(&root, &path),
        Some(fs::canonicalize(&path).unwrap())
    );
    assert!(
        cached_at(&root, &root.join(HELLO_SHA256)).is_none(),
        "a folder"
    );
    assert!(cached_at(&root, &root).is_none());
    let outside = scratch.0.join("outside.txt");
    fs::write(&outside, b"hello").unwrap();
    assert!(cached_at(&root, &outside).is_none());
    let empty = root.join("empty");
    fs::write(&empty, b"").unwrap();
    assert!(cached_at(&root, &empty).is_none());
    assert!(cached_at(&root, &root.join("missing")).is_none());
}

#[test]
fn a_prepared_message_reads_back_until_its_copy_changes() {
    let scratch = Scratch::new("prepare");
    let root = scratch.0.join("cache");
    let attachments = [attachment("a.txt", b"hello"), attachment("b.txt", b"world")];
    let files = prepare(&root, &attachments).unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(
        (
            files[0].name.as_str(),
            files[0].bytes,
            files[0].reference.as_str()
        ),
        ("a.txt", 5, "[File #1]")
    );
    assert_eq!(read_stored(&root, &files).unwrap(), attachments);
    let value = serde_json::to_value(&files[0]).unwrap();
    assert_eq!(
        serde_json::from_value::<StoredFile>(value).unwrap(),
        files[0]
    );
    let unavailable =
        "The saved attachment is unavailable or changed. Attach it again before sending.";
    // The same size with other content no longer matches its folder's digest.
    fs::write(&files[0].local_path, b"jello").unwrap();
    assert_eq!(read_stored(&root, &files).unwrap_err(), unavailable);
    let mut other = files.clone();
    other[1].bytes = 4;
    assert_eq!(read_stored(&root, &other[1..]).unwrap_err(), unavailable);
    let mut moved = files.clone();
    moved[1].local_path = scratch.0.join("elsewhere.txt");
    fs::write(&moved[1].local_path, b"world").unwrap();
    assert_eq!(read_stored(&root, &moved[1..]).unwrap_err(), unavailable);
    // Bounds are checked before anything is written.
    let failed = "Could not save the attachments. Your draft was kept without sending.";
    let nine: Vec<_> = (0..9).map(|i| attachment("n", &[i])).collect();
    assert_eq!(prepare(&root, &nine).unwrap_err(), failed);
    assert_eq!(prepare(&root, &[attachment("e", b"")]).unwrap_err(), failed);
    assert!(prepare(&root, &[]).unwrap().is_empty());
}

#[test]
fn chosen_files_are_read_with_qt_texts() {
    let scratch = Scratch::new("read");
    let path = scratch.0.join("diagram.png");
    fs::write(&path, b"\x89PNG\r\n\x1a\n0000").unwrap();
    let read = read_file(&path).unwrap();
    assert_eq!(
        (read.name.as_str(), read.mime.as_str(), read.bytes.len()),
        ("diagram.png", "image/png", 12)
    );
    let too_large = "Choose a file of at most 10 MiB.";
    assert_eq!(
        read_file(&scratch.0.join("missing")).unwrap_err(),
        too_large
    );
    assert_eq!(read_file(&scratch.0).unwrap_err(), too_large);
    let big = scratch.0.join("big.bin");
    fs::write(&big, vec![0; MAX_ATTACHMENT_BYTES + 1]).unwrap();
    assert_eq!(read_file(&big).unwrap_err(), too_large);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let locked = scratch.0.join("locked.txt");
        fs::write(&locked, b"secret").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads anything; the text is checked only where the read fails.
        if let Err(error) = read_file(&locked) {
            assert_eq!(error, "Cannot read locked.txt: Permission denied");
        }
    }
}

#[test]
fn the_cache_drops_its_least_recently_used_copies_but_never_a_pinned_one() {
    let scratch = Scratch::new("prune");
    let root = scratch.0.join("cache");
    let mut paths = Vec::new();
    for index in 0..5u8 {
        paths.push(store_at(&root, &format!("{index}.bin"), &vec![index + 1; 1000]).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    // The oldest is pinned (open for preview); the second is used again.
    touch(&paths[1]);
    let pinned = fs::canonicalize(&paths[0]).unwrap();
    let report = prune(&root, 2500, |path| {
        fs::canonicalize(path).is_ok_and(|path| path == pinned)
    });
    assert_eq!(report.removed, 3);
    assert_eq!(report.bytes, 2000);
    assert!(paths[0].exists(), "pinned");
    assert!(paths[1].exists(), "recently used");
    for path in &paths[2..4] {
        assert!(
            !path.exists() && !path.parent().unwrap().exists(),
            "evicted with its folder"
        );
    }
    assert!(!paths[4].exists());
    // Under the limit nothing goes; nothing but digest folders is touched.
    fs::write(root.join("stray.txt"), vec![0; 10_000]).unwrap();
    assert_eq!(prune(&root, 1500, |_| false).removed, 1, "the older copy");
    assert!(root.join("stray.txt").exists());
    assert_eq!(prune(&root, 1_000_000, |_| false).removed, 0);
}

#[test]
fn prepared_images_carry_geometry_into_local_rows_and_old_records_still_decode() {
    let scratch = Scratch::new("image-presentation");
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend(2560_u32.to_be_bytes());
    png.extend(720_u32.to_be_bytes());
    let mut image = attachment("wide.png", &png);
    image.mime = "image/png".into();
    image.reference = "[Image #1]".into();
    let files = prepare(&scratch.0.join("cache"), &[image]).unwrap();
    assert_eq!(
        files[0].image_size,
        Some(zerus_client_core::attachments::image::Size {
            width: 2560,
            height: 720
        })
    );
    let mut messages = zerus_client_core::local_messages::LocalMessages::default();
    messages.sending(
        "session",
        "request",
        None,
        "[Image #1]",
        &files,
        "run",
        "conversation",
        10.0,
    );
    let file = &messages.rows("session")[0].attachments[0];
    assert_eq!(
        file["image_size"],
        serde_json::json!({"width":2560,"height":720})
    );
    assert!(file["preview_path"].as_str().unwrap().starts_with("cache/"));
    assert!(!file["preview_path"]
        .as_str()
        .unwrap()
        .contains(scratch.0.to_str().unwrap()));
    let mut old = serde_json::to_value(&files[0]).unwrap();
    old.as_object_mut().unwrap().remove("image_size");
    assert_eq!(
        serde_json::from_value::<StoredFile>(old)
            .unwrap()
            .image_size,
        None
    );
}
