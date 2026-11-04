//! Native controls: every source is a private temporary fixture on this runner.
use super::*;
use serde_json::json;
use std::{
    fs,
    fs::OpenOptions,
    io::Write,
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "zerus-g3-capture-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        drop(crate::fsx::windows::Parents::open(&path, true).unwrap());
        Self(path)
    }
    fn write(&self, name: &str, data: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        crate::fsx::write_private_durable(&path, data).unwrap();
        path
    }
    fn settings(&self) -> Settings {
        Settings::open(self.write("settings.json", ORIGINAL), None)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
// QSaveFile uses only classic replacement. Rust fs::rename may fall back to
// FileRenameInfoEx POSIX semantics and cannot prove this Qt operation works.
fn qt_replace(from: &std::path::Path, to: &std::path::Path) {
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING};
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    let result = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING) };
    assert_ne!(
        result,
        0,
        "Qt-style replacement failed: {}",
        std::io::Error::last_os_error()
    );
}
const ORIGINAL: &[u8] = br#"{
 "future-root":{"opaque":7},"version":1,"values":{}
}
"#;
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

#[test]
fn native_settings_capture_keeps_exact_bytes_and_original_writer_lease() {
    let scratch = Scratch::new();
    let settings = scratch.settings();
    let capture = Arc::new(settings.capture_for_migration(deadline()).unwrap());
    assert_eq!(capture.bytes(), ORIGINAL);
    assert_eq!(capture.recheck(deadline()).unwrap(), sha256(ORIGINAL));
    assert!(settings.set("workspace/language", json!("en")).is_err());
    assert!(OpenOptions::new()
        .write(true)
        .open(&settings.shared.path)
        .is_err());
    assert!(fs::rename(&scratch.0, scratch.0.with_extension("moved")).is_err());
    let retained = capture.clone();
    drop(capture);
    assert!(settings.set("workspace/language", json!("en")).is_err());
    drop(retained);
    assert_eq!(fs::read(&settings.shared.path).unwrap(), ORIGINAL);
    settings.set("workspace/language", json!("en")).unwrap();
    assert!(settings.flush_within(Duration::from_secs(5)));
}
#[test]
fn native_qt_capture_keeps_parent_pins_but_no_leaf_handle_between_reads() {
    let scratch = Scratch::new();
    let data = b"[workspace]\nlanguage=en\n[future]\nopaque=7\n";
    let path = scratch.write("outer/hgdev/hgs-tray.ini", data);
    let folders = [
        scratch.0.clone(),
        scratch.0.join("outer"),
        scratch.0.join("outer/hgdev"),
    ];
    let store = QtStore::Ini(path.clone());
    let stop = AtomicBool::new(false);
    let captured = store
        .capture_for_migration(Arc::new(()), &stop)
        .unwrap()
        .unwrap();
    assert_eq!(captured.snapshot().sha256, sha256(data));
    assert_eq!(captured.snapshot().source, "qt-ini");
    assert_eq!(captured.recheck(Arc::new(()), &stop).unwrap(), sha256(data));
    let concurrent = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    assert_eq!(fs::read(&path).unwrap(), data);
    drop(concurrent);
    // An exclusive competing open proves no report-owned leaf survived the
    // bounded capture/recheck calls, even though parent pins remain held.
    let exclusive = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    drop(exclusive);
    // Every fixture-owned ancestor is an actual member of the pinned chain:
    // scratch, outer and the final hgdev source folder. Do not rename system,
    // profile or temp ancestors outside this fixture, which are also opened.
    for folder in &folders {
        assert!(fs::rename(folder, folder.with_extension("moved")).is_err());
    }
    let created = path.parent().unwrap().join("created-during-capture.txt");
    fs::write(&created, b"ordinary creation remains available").unwrap();
    assert_eq!(
        fs::read(&created).unwrap(),
        b"ordinary creation remains available"
    );
    stop.store(true, Ordering::Release);
    assert!(captured.recheck(Arc::new(()), &stop).is_err());
    drop(captured);
    assert_eq!(fs::read(&path).unwrap(), data);
    // The same renames succeed after the original parent handles drop,
    // excluding an ACL or unrelated path error as the reason for refusal.
    for folder in &folders {
        let moved = folder.with_extension("moved");
        qt_replace(folder, &moved);
        qt_replace(&moved, folder);
    }
    fs::rename(&path, path.with_extension("kept")).unwrap();
}
#[test]
fn native_qt_atomic_replacement_succeeds_while_capture_is_held_and_recheck_refuses() {
    let scratch = Scratch::new();
    let data = b"[workspace]\nlanguage=en\n";
    let path = scratch.write("hgs-tray.ini", data);
    let temporary = scratch.write("qt-save.tmp", data);
    let captured = QtStore::Ini(path.clone())
        .capture_for_migration(Arc::new(()), &AtomicBool::new(false))
        .unwrap()
        .unwrap();
    let writer_path = path.clone();
    std::thread::spawn(move || {
        // Qt can read its settings and commit with its exact classic rename
        // while the report retains parent pins, stamp/hash and no leaf handle.
        let concurrent = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&writer_path)
            .unwrap();
        drop(concurrent);
        qt_replace(&temporary, &writer_path);
    })
    .join()
    .unwrap();
    assert_eq!(fs::read(&path).unwrap(), data);
    assert_eq!(
        captured.recheck(Arc::new(()), &AtomicBool::new(false)),
        Err(SourceError::SourceChanged)
    );
}
#[test]
fn native_qt_same_bytes_with_original_leaf_still_linked_cannot_replace_its_identity() {
    let scratch = Scratch::new();
    let data = b"[workspace]\nlanguage=en\n";
    let path = scratch.write("hgs-tray.ini", data);
    let replacement = scratch.write("qt-save.tmp", data);
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    OpenOptions::new()
        .write(true)
        .open(&replacement)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    assert_eq!(
        fs::metadata(&replacement).unwrap().modified().unwrap(),
        modified
    );
    assert_eq!(fs::read(&replacement).unwrap(), data);
    let captured = QtStore::Ini(path.clone())
        .capture_for_migration(Arc::new(()), &AtomicBool::new(false))
        .unwrap()
        .unwrap();
    // Keep the old file linked at another name; the new leaf has identical
    // bytes/size/mtime. Only its different volume/file-index stamp detects it.
    qt_replace(&path, &scratch.0.join("original.ini"));
    qt_replace(&replacement, &path);
    assert_eq!(
        captured.recheck(Arc::new(()), &AtomicBool::new(false)),
        Err(SourceError::SourceChanged)
    );
}
#[test]
fn native_qt_in_place_data_or_last_write_changes_refuse_without_blocking_the_writer() {
    for metadata_only in [false, true] {
        let scratch = Scratch::new();
        let data = b"[workspace]\nlanguage=en\n";
        let path = scratch.write("hgs-tray.ini", data);
        let captured = QtStore::Ini(path.clone())
            .capture_for_migration(Arc::new(()), &AtomicBool::new(false))
            .unwrap()
            .unwrap();
        let mut writer = OpenOptions::new().write(true).open(&path).unwrap();
        if metadata_only {
            // Fixed distinct timestamp, independent of the runner's resolution.
            writer
                .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_000_000))
                .unwrap();
        } else {
            writer.write_all(b"[workspace]\nlanguage=ru\n").unwrap();
        }
        writer.sync_all().unwrap();
        drop(writer);
        if metadata_only {
            assert_eq!(fs::read(&path).unwrap(), data);
        }
        assert_eq!(
            captured.recheck(Arc::new(()), &AtomicBool::new(false)),
            Err(SourceError::SourceChanged)
        );
    }
}
#[test]
fn native_busy_and_expired_settings_capture_leave_mutations_and_source_intact() {
    let scratch = Scratch::new();
    let settings = scratch.settings();
    assert!(settings.capture_for_migration(Instant::now()).is_err());
    let held = fs::File::open(&settings.shared.path).unwrap();
    assert!(settings
        .capture_for_migration(Instant::now() + Duration::from_millis(30))
        .is_err());
    assert!(!settings.shared.lock().capturing);
    drop(held);
    let captured = settings.capture_for_migration(deadline()).unwrap();
    assert_eq!(captured.bytes(), ORIGINAL);
    assert_eq!(
        captured.recheck(Instant::now()),
        Err(SourceError::SourceUnreadable)
    );
    drop(captured);
    assert_eq!(fs::read(&settings.shared.path).unwrap(), ORIGINAL);
}
#[test]
fn native_changed_settings_refuse_without_rewriting_or_adopting_new_contents() {
    let scratch = Scratch::new();
    let settings = scratch.settings();
    // Let the existing reader finish before replacing its disk source.
    let first = settings.capture_for_migration(deadline()).unwrap();
    drop(first);
    let changed = br#"{"version":1,"values":{"workspace/language":"ru"}}"#;
    crate::fsx::write_private_durable(&settings.shared.path, changed).unwrap();
    assert_eq!(
        settings.capture_for_migration(deadline()).err(),
        Some(SourceError::SourceChanged)
    );
    assert_eq!(fs::read(&settings.shared.path).unwrap(), changed);
    assert!(!settings.shared.lock().capturing);
}
#[test]
fn native_absent_hardlinked_and_oversized_qt_are_not_empty_success() {
    let scratch = Scratch::new();
    let path = scratch.0.join("hgs-tray.ini");
    let store = QtStore::Ini(path.clone());
    let stop = AtomicBool::new(false);
    assert!(store
        .capture_for_migration(Arc::new(()), &stop)
        .unwrap()
        .is_none());
    scratch.write("hgs-tray.ini", b"[workspace]\n");
    let other = scratch.0.join("linked");
    fs::hard_link(&path, &other).unwrap();
    assert!(store.capture_for_migration(Arc::new(()), &stop).is_err());
    fs::remove_file(&other).unwrap();
    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(super::super::MAX_QT_STORE + 1)
        .unwrap();
    assert!(store.capture_for_migration(Arc::new(()), &stop).is_err());
    assert_eq!(
        fs::metadata(&path).unwrap().len(),
        super::super::MAX_QT_STORE + 1
    );
}
#[test]
fn native_report_sources_refuse_unc_paths_and_non_windows_qt_backends() {
    let stop = AtomicBool::new(false);
    let unc = QtStore::Ini(PathBuf::from(r"\\server\share\hgdev\hgs-tray.ini"));
    assert!(unc.capture_for_migration(Arc::new(()), &stop).is_err());
    let plist = QtStore::Plist {
        domain: None,
        path: PathBuf::from(r"C:\example.plist"),
    };
    assert_eq!(
        plist.capture_for_migration(Arc::new(()), &stop).err(),
        Some(SourceError::UnsupportedSource)
    );
}

#[test]
fn native_captured_windows_sources_complete_only_a_staged_bundle_with_a_stable_journal_id() {
    use crate::migration::{families, journal, Bundle, BundleItem};
    let scratch = Scratch::new();
    let data = b"[workspace]\nlanguage=en\n";
    let path = scratch.write("hgs-tray.ini", data);
    let stop = Arc::new(AtomicBool::new(false));
    let qt = Arc::new(
        QtStore::Ini(path.clone())
            .capture_for_migration(Arc::new(()), &stop)
            .unwrap()
            .unwrap(),
    );
    let settings = scratch.settings();
    let current = Arc::new(settings.capture_for_migration(deadline()).unwrap());
    let groups = families::retained_qt(qt.snapshot(), current.bytes(), false).unwrap();
    assert_eq!((groups.qt.len(), groups.settings.len()), (16, 3));
    let mut streams = Vec::new();
    for source in groups.qt {
        let qt = qt.clone();
        let stop = stop.clone();
        streams.push(source.recheck(move || qt.recheck(Arc::new(()), &stop)));
    }
    for source in groups.settings {
        let current = current.clone();
        streams.push(source.recheck(move || current.recheck(deadline())));
    }
    let id = "11111111-1111-4111-8111-111111111111";
    let configuration = "a".repeat(64);
    let journal = journal::Journal::new(&scratch.0, &configuration).unwrap();
    let saved = journal
        .save(
            journal::Record {
                version: 1,
                scope: journal::SCOPE.into(),
                configuration: configuration.clone(),
                import_id: id.into(),
                created_at: 1,
                qt_sha256: sha256(data),
                settings_sha256: current.sha256().into(),
                attempt: 0,
                phase: journal::Phase::Reserved,
                expected: None,
            },
            None,
        )
        .unwrap();
    let bundle = Bundle::new(
        zerus_desktop_migration::SOURCE,
        false,
        configuration,
        id.into(),
        1,
        streams,
        64 << 20,
    )
    .unwrap();
    let mut record = None;
    for item in bundle {
        match item.unwrap() {
            BundleItem::Record(value) => {
                assert!(record.replace(value).is_none());
            }
            BundleItem::Snapshot { mut reader, .. } | BundleItem::Page { mut reader, .. } => {
                std::io::copy(&mut reader, &mut std::io::sink()).unwrap();
            }
        }
    }
    let record = record.unwrap();
    assert_eq!(record.evidence, zerus_desktop_migration::Evidence::Staged);
    assert_eq!(record.sources.len(), 19);
    assert_eq!(record.import_id, id);
    let replay = journal.load().unwrap().unwrap();
    assert_eq!(replay.record, saved.record);
    assert_eq!(replay.sha256, saved.sha256);
    assert!(settings.set("workspace/language", json!("ru")).is_err());
    drop(current);
    drop(qt);
    assert_eq!(fs::read(&path).unwrap(), data);
    assert_eq!(fs::read(&settings.shared.path).unwrap(), ORIGINAL);
}
