use super::*;
use crate::fsx::windows::{
    cleanup_failed_temporary, info, verify, Allocation, Parents, READ_CONTROL,
};
use crate::readmarks::ReadMark;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    fs::OpenOptions,
    io::Write,
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::PathBuf,
};
use windows_sys::Win32::{
    Foundation::{ERROR_SUCCESS, GENERIC_WRITE},
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SE_FILE_OBJECT},
        DACL_SECURITY_INFORMATION,
    },
    Storage::FileSystem::*,
};
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "read-checkpoint-win-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        Self(root)
    }
    fn path(&self) -> PathBuf {
        self.0.join("marks.json")
    }
    fn write(&self, data: &[u8]) {
        drop(publish(&self.path(), data, None).unwrap());
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn mark(n: usize) -> ReadMark {
    ReadMark {
        computer: String::new(),
        name: format!("agent/example/{n}"),
        run_id: format!("run-{n}"),
        conversation_id: format!("conversation-{n}"),
        reply_id: Some(format!("reply-{n}")),
        attention_signature: None,
    }
}
fn identity(path: &Path) -> (u32, u32, u32) {
    let f = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(7)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .unwrap();
    let v = info(&f).unwrap();
    (v.dwVolumeSerialNumber, v.nFileIndexHigh, v.nFileIndexLow)
}
#[test]
fn native_empty_and_dirty_checkpoint_are_durable_private_and_capture_every_row() {
    let t = Temp::new();
    let mut marks = ReadMarks::load(t.path());
    let empty = marks.checkpoint().unwrap();
    assert_eq!((empty.bytes, empty.marks), (2, 0));
    assert_eq!(marks.checkpoint_bytes(&empty).unwrap(), b"[]");
    drop(empty);
    marks.add_all((0..193).map(mark));
    let checkpoint = marks.checkpoint().unwrap();
    assert_eq!(checkpoint.marks, 193);
    let data = marks.checkpoint_bytes(&checkpoint).unwrap();
    let capture = crate::desktop::pending_reads::Capture::from_bytes(&data).unwrap();
    assert_eq!(capture.source().rows, 193);
    assert_eq!(capture.source().sha256, checkpoint.sha256);
    let mut after = 0;
    let mut count = 0;
    while after < capture.source().rows {
        let page = capture.page(after).unwrap();
        count += page.marks.len();
        after = page.through();
    }
    assert_eq!(count, 193);
    verify(&checkpoint.original.lock().unwrap().file, false).unwrap();
    drop(checkpoint);
    assert_eq!(ReadMarks::load(t.path()).pending(), marks.pending());
}
#[test]
fn native_unchanged_unknown_fields_keep_exact_bytes_and_file_identity() {
    let t = Temp::new();
    let data=br#"[ {"computer":"","name":"one","run_id":"r","conversation_id":"c","reply_id":"p","future":{"keep":true}} ]"#;
    t.write(data);
    let original = identity(&t.path());
    let mut marks = ReadMarks::load(t.path());
    let checkpoint = marks.checkpoint().unwrap();
    assert_eq!(marks.checkpoint_bytes(&checkpoint).unwrap(), data);
    drop(checkpoint);
    assert_eq!(std::fs::read(t.path()).unwrap(), data);
    assert_eq!(identity(&t.path()), original);
}
#[test]
fn native_checkpoint_pins_leaf_and_ancestor_until_the_original_receipt_drops() {
    let t = Temp::new();
    t.write(b"[]");
    let mut marks = ReadMarks::load(t.path());
    let checkpoint = marks.checkpoint().unwrap();
    assert!(OpenOptions::new().write(true).open(t.path()).is_err());
    assert!(std::fs::remove_file(t.path()).is_err());
    let renamed = t.0.with_extension("renamed");
    assert!(std::fs::rename(&t.0, &renamed).is_err());
    assert_eq!(marks.checkpoint_bytes(&checkpoint).unwrap(), b"[]");
    drop(checkpoint);
    assert_eq!(std::fs::read(t.path()).unwrap(), b"[]");
    std::fs::rename(&t.0, &renamed).unwrap();
    std::fs::rename(&renamed, &t.0).unwrap();
}
#[test]
fn native_checkpoint_ancestor_pins_are_not_masked_by_its_exclusive_leaf() {
    let t = Temp::new();
    let spare = Temp::new();
    spare.write(b"placeholder");
    let folders = [t.0.clone(), t.0.join("outer"), t.0.join("outer/hgdev")];
    let path = folders.last().unwrap().join("marks.json");
    drop(publish(&path, b"[]", None).unwrap());
    let mut marks = ReadMarks::load(path.clone());
    let checkpoint = marks.checkpoint().unwrap();
    assert_eq!(marks.checkpoint_bytes(&checkpoint).unwrap(), b"[]");
    // Keep this ACTUAL checkpoint's original Parents, releasing only its
    // exclusive child handle. A busy child must not mask broken ancestor pins.
    // The fixture no longer calls checkpoint_bytes on this deliberately altered
    // native receipt; no production constructor or permission is introduced.
    let leaf = std::mem::replace(
        &mut checkpoint.original.lock().unwrap().file,
        std::fs::File::open(spare.path()).unwrap(),
    );
    drop(leaf);
    // No leaf lock remains: the exact source can now be opened exclusively.
    drop(
        OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&path)
            .unwrap(),
    );
    for folder in &folders {
        assert!(std::fs::rename(folder, folder.with_extension("moved")).is_err());
    }
    let created = path.parent().unwrap().join("ordinary-file.txt");
    std::fs::write(&created, b"creation inside a pinned folder is allowed").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"[]");
    drop(checkpoint);
    // All fixture-owned ancestors are movable once the SAME receipt drops.
    // This also excludes ACL/path refusal as a false positive in the loop.
    for folder in &folders {
        let moved = folder.with_extension("moved");
        std::fs::rename(folder, &moved).unwrap();
        std::fs::rename(&moved, folder).unwrap();
    }
}
#[test]
fn native_busy_file_refuses_without_mutation_and_can_retry_after_original_close() {
    let t = Temp::new();
    t.write(b"[ ]");
    let mut marks = ReadMarks::load(t.path());
    let held = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(7)
        .open(t.path())
        .unwrap();
    assert!(marks.checkpoint().is_err());
    drop(held);
    let checkpoint = marks.checkpoint().unwrap();
    assert_eq!(marks.checkpoint_bytes(&checkpoint).unwrap(), b"[ ]");
}
#[test]
fn native_malformed_changed_hardlinked_and_overlimit_sources_stay_untouched() {
    let malformed = Temp::new();
    malformed.write(b"not json");
    let mut marks = ReadMarks::load(malformed.path());
    assert!(marks.checkpoint().is_err());
    assert_eq!(std::fs::read(malformed.path()).unwrap(), b"not json");
    let changed = Temp::new();
    changed.write(b"[]");
    let mut marks = ReadMarks::load(changed.path());
    std::fs::write(changed.path(), b"[ ]").unwrap();
    assert!(marks.checkpoint().is_err());
    assert_eq!(std::fs::read(changed.path()).unwrap(), b"[ ]");
    let linked = Temp::new();
    linked.write(b"[]");
    let mut marks = ReadMarks::load(linked.path());
    std::fs::hard_link(linked.path(), linked.0.join("other")).unwrap();
    assert!(marks.checkpoint().is_err());
    assert_eq!(std::fs::read(linked.path()).unwrap(), b"[]");
    let large = Temp::new();
    large.write(b"[]");
    let mut marks = ReadMarks::load(large.path());
    OpenOptions::new()
        .write(true)
        .open(large.path())
        .unwrap()
        .set_len((CHECKPOINT_BYTES + 1) as u64)
        .unwrap();
    assert!(marks.checkpoint().is_err());
    assert_eq!(
        std::fs::metadata(large.path()).unwrap().len(),
        (CHECKPOINT_BYTES + 1) as u64
    );
}
#[test]
fn native_public_acl_refuses_and_is_never_repaired() {
    use windows_sys::Win32::Security::Authorization::SetSecurityInfo;
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
    };
    let t = Temp::new();
    t.write(b"[]");
    let mut marks = ReadMarks::load(t.path());
    let sddl: Vec<u16> = "D:P(A;;FA;;;WD)".encode_utf16().chain(Some(0)).collect();
    let mut raw = std::ptr::null_mut();
    assert_ne!(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut raw,
                std::ptr::null_mut(),
            )
        },
        0
    );
    let _sd = Allocation(raw);
    let mut acl = std::ptr::null_mut();
    let mut present = 0;
    let mut defaulted = 0;
    assert_ne!(
        unsafe { GetSecurityDescriptorDacl(raw, &mut present, &mut acl, &mut defaulted) },
        0
    );
    assert_ne!(present, 0);
    let f = OpenOptions::new()
        .access_mode(WRITE_DAC | READ_CONTROL)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(t.path())
        .unwrap();
    assert_eq!(
        unsafe {
            SetSecurityInfo(
                f.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl,
                std::ptr::null(),
            )
        },
        ERROR_SUCCESS
    );
    drop(f);
    assert!(marks.checkpoint().is_err());
    assert_eq!(std::fs::read(t.path()).unwrap(), b"[]");
    assert!(verify(
        &OpenOptions::new()
            .access_mode(READ_CONTROL)
            .open(t.path())
            .unwrap(),
        false
    )
    .is_err());
}

// Native junction fixture: no shell/PowerShell, symlink privilege or helper.
fn junction(link: &Path, target: &Path) {
    drop(Parents::open(link, true).unwrap());
    let target = target
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_owned();
    let substitute: Vec<u16> = format!(r"\??\{target}").encode_utf16().collect();
    let print: Vec<u16> = target.encode_utf16().collect();
    assert!(substitute.len() + print.len() < 4096);
    let mut data = Vec::new();
    data.extend_from_slice(&0xA0000003u32.to_le_bytes()); // IO_REPARSE_TAG_MOUNT_POINT
    data.extend_from_slice(&((8 + (substitute.len() + print.len() + 2) * 2) as u16).to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&((substitute.len() * 2) as u16).to_le_bytes());
    data.extend_from_slice(&(((substitute.len() + 1) * 2) as u16).to_le_bytes());
    data.extend_from_slice(&((print.len() * 2) as u16).to_le_bytes());
    for value in substitute
        .into_iter()
        .chain(Some(0))
        .chain(print)
        .chain(Some(0))
    {
        data.extend_from_slice(&value.to_le_bytes());
    }
    let file = OpenOptions::new()
        .access_mode(GENERIC_WRITE)
        .share_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(link)
        .unwrap();
    let mut used = 0;
    assert_ne!(
        unsafe {
            windows_sys::Win32::System::IO::DeviceIoControl(
                file.as_raw_handle(),
                589988, // FSCTL_SET_REPARSE_POINT
                data.as_ptr().cast(),
                data.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut used,
                std::ptr::null_mut(),
            )
        },
        0,
        "{}",
        io::Error::last_os_error()
    );
}
#[test]
fn native_junction_ancestor_is_refused_without_following_it_for_checkpoint() {
    let t = Temp::new();
    let target = Temp::new();
    target.write(b"[]");
    drop(Parents::open(&t.0, true).unwrap());
    let link = t.0.join("junction");
    junction(&link, &target.0);
    assert!(
        current(&link).is_err(),
        "a reparse leaf is not a regular source either"
    );
    let mut marks = ReadMarks::load(link.join("marks.json"));
    assert!(marks.checkpoint().is_err());
    assert_eq!(std::fs::read(target.path()).unwrap(), b"[]");
    std::fs::remove_dir(link).unwrap();
}

#[test]
fn native_row_limits_preserve_the_complete_last_checkpoint_without_coalescing_the_bound() {
    let t = Temp::new();
    let mut marks = ReadMarks::load(t.path());
    marks.add_all((0..CHECKPOINT_MARKS).map(mark));
    let checkpoint = marks.checkpoint().unwrap();
    assert_eq!(checkpoint.marks, CHECKPOINT_MARKS);
    let original = checkpoint.sha256.clone();
    drop(checkpoint);
    marks.add_all([mark(CHECKPOINT_MARKS)]);
    assert!(marks.checkpoint().is_err());
    assert_eq!(sha256(&std::fs::read(t.path()).unwrap()), original);
    let duplicate = Temp::new();
    let data = serde_json::to_vec(&vec![mark(0); CHECKPOINT_MARKS + 1]).unwrap();
    assert!(data.len() < CHECKPOINT_BYTES);
    duplicate.write(&data);
    let mut marks = ReadMarks::load(duplicate.path());
    assert_eq!(marks.pending().len(), 1);
    assert!(marks.checkpoint().is_err());
    assert_eq!(std::fs::read(duplicate.path()).unwrap(), data);
}

#[test]
fn native_capture_bytes_require_the_original_path_and_private_checkpoint_digest() {
    let t = Temp::new();
    t.write(b"[]");
    let mut marks = ReadMarks::load(t.path());
    let mut checkpoint = marks.checkpoint().unwrap();
    assert!(ReadMarks::load(t.0.join("other.json"))
        .checkpoint_bytes(&checkpoint)
        .is_err());
    checkpoint.sha256 = "b".repeat(64);
    assert!(marks.checkpoint_bytes(&checkpoint).is_err());
    drop(checkpoint);
    assert_eq!(std::fs::read(t.path()).unwrap(), b"[]");
}

#[test]
fn a_reported_unknown_after_native_publication_is_sticky_even_when_bytes_match() {
    let t = Temp::new();
    let data = serde_json::to_vec(&vec![mark(0)]).unwrap();
    t.write(&data);
    let mut marks = ReadMarks::load(t.path());
    marks.add_all([mark(0)]);
    // Real native publication succeeds, then the caller receives a synthetic
    // uncertain completion. This tests recovery policy, not disk-fault coverage.
    let error = checkpoint_with(&mut marks, |path, data, old| {
        drop(publish(path, data, old)?);
        Err(PublicationFailure::Unknown(io::Error::from_raw_os_error(
            32,
        )))
    })
    .unwrap_err();
    assert_eq!(
        error.raw_os_error(),
        None,
        "unknown is never sharing-retryable"
    );
    assert!(error.to_string().contains("outcome is unknown"));
    let published = identity(&t.path());
    assert!(checkpoint_with(&mut marks, |_, _, _| panic!("unknown source was rewritten")).is_err());
    assert_eq!(identity(&t.path()), published);
    assert_eq!(std::fs::read(t.path()).unwrap(), data);
    // An explicit new owner can read/sync the retained source without a rewrite.
    let mut reloaded = ReadMarks::load(t.path());
    let checkpoint = reloaded.checkpoint().unwrap();
    drop(checkpoint);
    assert_eq!(identity(&t.path()), published);
}

#[test]
fn native_failed_preparation_discards_only_its_owned_unpublished_temporary() {
    let t = Temp::new();
    t.write(b"[]");
    let original = identity(&t.path());
    let error = publish_with(
        &t.path(),
        b"new data",
        current(&t.path()).unwrap(),
        |file, _| {
            file.write_all(b"partial")?;
            Err(io::Error::other("synthetic preparation failure"))
        },
    )
    .unwrap_err();
    let PublicationFailure::Before(error) = error else {
        panic!("preparation did not attempt publication")
    };
    assert_eq!(error.to_string(), "synthetic preparation failure");
    assert_eq!(std::fs::read(t.path()).unwrap(), b"[]");
    assert_eq!(identity(&t.path()), original);
    let entries: Vec<_> = std::fs::read_dir(&t.0).unwrap().collect();
    assert_eq!(
        entries.len(),
        1,
        "failed temp was deleted on original close"
    );
}

#[test]
fn native_failed_disposition_retains_primary_and_cleanup_errors_without_path_fallback() {
    let t = Temp::new();
    t.write(b"[]");
    let path = t.0.join("own-unpublished.tmp");
    let own = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    // This freshly owned fixture handle deliberately lacks DELETE access.
    let error = cleanup_failed_temporary(&own, io::Error::from_raw_os_error(32));
    assert_eq!(error.raw_os_error(), None, "failed cleanup cannot retry");
    assert!(error
        .to_string()
        .contains("owned checkpoint temporary cleanup failed"));
    drop(own);
    assert!(path.is_file(), "no path-selected cleanup fallback");
    assert_eq!(std::fs::read(t.path()).unwrap(), b"[]");
}
