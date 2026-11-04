use super::*;
#[cfg(unix)]
use crate::fsx::write_private_atomic;
#[cfg(unix)]
use serde_json::json;
#[cfg(unix)]
use std::{fs, path::PathBuf};

#[cfg(unix)]
struct Fixture(PathBuf);
#[cfg(unix)]
impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "attention-checkpoint-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        }
        #[cfg(not(unix))]
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn path(&self) -> PathBuf {
        self.0.join("attention-observed.json")
    }
    fn initial(&self, raw: &[u8]) -> (Map<String, Value>, Observation) {
        write_private_atomic(&self.path(), raw, true).unwrap();
        let state = decode_observed(raw).unwrap_or_default();
        let observation = Observation::loaded(&Ok(raw.to_vec()), &state);
        (state, observation)
    }
}
#[cfg(unix)]
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[cfg(unix)]
fn writer(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_private_atomic(path, bytes, true)
}

#[cfg(unix)]
#[test]
fn unchanged_original_spacing_and_unknown_children_are_synced_without_rewrite() {
    let f = Fixture::new("unchanged");
    let raw=b"{ \"peer\": {\"run:one\": {\"token\":\"private-token\",\"unknown\":[1,true]}},\"empty\": {},\"legacy\":7 }\n";
    let (state, mut seen) = f.initial(raw);
    let checkpoint = seen
        .checkpoint(&f.path(), &state, |_, _| {
            panic!("unchanged source rewritten")
        })
        .unwrap();
    assert_eq!(
        checkpoint,
        Checkpoint {
            sha256: sha256(raw),
            bytes: raw.len(),
            hosts: 3,
            entries: 3
        }
    );
    assert_eq!(fs::read(f.path()).unwrap(), raw);
}

#[cfg(unix)]
#[test]
fn an_absent_file_uses_only_the_existing_writer_and_exact_seeded_state() {
    let f = Fixture::new("missing");
    let state = json!({"peer":{"run:one":{"original":"retained"}}})
        .as_object()
        .unwrap()
        .clone();
    let mut seen = Observation::loaded(&Err(io::ErrorKind::NotFound.into()), &state);
    assert!(!f.path().exists());
    let checkpoint = seen.checkpoint(&f.path(), &state, writer).unwrap();
    let raw = fs::read(f.path()).unwrap();
    assert_eq!(decode_observed(&raw).unwrap(), state);
    assert_eq!(checkpoint.sha256, sha256(&raw));
}

#[cfg(unix)]
#[test]
fn a_changed_source_never_overwrites_the_external_bytes() {
    let f = Fixture::new("changed");
    let (state, mut seen) = f.initial(b"{}");
    writer(&f.path(), b"{\"external\":true}").unwrap();
    assert!(seen
        .checkpoint(&f.path(), &state, |_, _| panic!(
            "changed source overwritten"
        ))
        .is_err());
    assert_eq!(fs::read(f.path()).unwrap(), b"{\"external\":true}");
    // Restoring bytes or another save cannot turn a failed barrier into success.
    writer(&f.path(), b"{}").unwrap();
    seen.saved(b"{}");
    assert!(seen.checkpoint(&f.path(), &state, writer).is_err());
}

#[cfg(unix)]
#[test]
fn unsaved_memory_is_refused_until_the_old_writer_records_its_success() {
    let f = Fixture::new("unsaved");
    let (mut state, mut seen) = f.initial(b"{}");
    state.insert("peer".into(), json!({"run:one":{}}));
    let raw = serde_json::to_vec(&state).unwrap();
    writer(&f.path(), &raw).unwrap();
    seen.saved(&raw);
    assert!(seen.checkpoint(&f.path(), &state, writer).is_ok());
    state.insert("late".into(), json!(null));
    assert!(seen.checkpoint(&f.path(), &state, writer).is_err());
}

#[cfg(unix)]
#[test]
fn malformed_initial_bytes_and_failed_saves_remain_unreliable() {
    let f = Fixture::new("unreliable");
    for raw in [b"broken".as_slice(), b"[]", b"null"] {
        let (state, mut seen) = f.initial(raw);
        writer(&f.path(), b"{}").unwrap();
        seen.saved(b"{}");
        assert!(seen.checkpoint(&f.path(), &state, writer).is_err());
    }
    let (state, mut seen) = f.initial(b"{}");
    seen.failed();
    writer(&f.path(), b"{}").unwrap();
    seen.saved(b"{}");
    assert!(seen.checkpoint(&f.path(), &state, writer).is_err());
}

#[cfg(unix)]
#[test]
fn a_late_directory_sync_failure_is_unknown_even_if_the_new_file_exists() {
    let f = Fixture::new("sync");
    let state = Map::new();
    let mut seen = Observation::loaded(&Err(io::ErrorKind::NotFound.into()), &state);
    assert!(seen
        .checkpoint_with(&f.path(), &state, writer, |_| Err(
            io::ErrorKind::Other.into()
        ))
        .is_err());
    assert_eq!(fs::read(f.path()).unwrap(), b"{}");
    assert!(seen.checkpoint(&f.path(), &state, writer).is_err());
}

#[cfg(unix)]
#[test]
fn a_writer_success_without_the_exact_published_bytes_never_settles() {
    let f = Fixture::new("false-writer");
    let state = Map::new();
    let mut seen = Observation::loaded(&Err(io::ErrorKind::NotFound.into()), &state);
    assert!(seen
        .checkpoint(&f.path(), &state, |path, _| writer(
            path,
            b"{\"different\":1}"
        ))
        .is_err());
}

#[cfg(unix)]
#[test]
fn linked_public_and_nonregular_leaves_are_refused_without_touching_the_target() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let f = Fixture::new("leaves");
    let (state, mut seen) = f.initial(b"{}");
    fs::set_permissions(f.path(), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(seen.checkpoint(&f.path(), &state, writer).is_err());
    let (state, mut seen) = f.initial(b"{}");
    fs::hard_link(f.path(), f.0.join("other")).unwrap();
    assert!(seen.checkpoint(&f.path(), &state, writer).is_err());
    fs::remove_file(f.0.join("other")).unwrap();
    let (state, mut seen) = f.initial(b"{}");
    fs::rename(f.path(), f.0.join("target")).unwrap();
    symlink(f.0.join("target"), f.path()).unwrap();
    assert!(seen.checkpoint(&f.path(), &state, writer).is_err());
    assert_eq!(fs::read(f.0.join("target")).unwrap(), b"{}");
    fs::remove_file(f.path()).unwrap();
    fs::create_dir(f.path()).unwrap();
    let mut seen = Observation::loaded(&Ok(b"{}".to_vec()), &state);
    assert!(seen.checkpoint(&f.path(), &state, writer).is_err());
}

#[test]
fn byte_and_entry_bounds_fail_before_any_publication() {
    let state = Map::new();
    let raw = vec![b' '; CHECKPOINT_BYTES + 1];
    let seen = Observation::loaded(&Ok(raw), &state);
    assert!(!seen.initial_ok);
    let mut too_many = Map::new();
    too_many.insert(
        "peer".into(),
        Value::Object(
            (0..=CHECKPOINT_ENTRIES)
                .map(|i| (i.to_string(), Value::Null))
                .collect(),
        ),
    );
    assert!(entries(&too_many).is_err());
    assert!(view(&too_many).is_err());
    let mut huge = Map::new();
    huge.insert("peer".into(), Value::String("x".repeat(CHECKPOINT_BYTES)));
    assert!(view(&huge).is_err());
}

#[cfg(not(unix))]
#[test]
fn unsupported_checkpoint_never_reads_or_writes_a_path() {
    let state = Map::new();
    let mut seen = Observation::loaded(&Err(io::ErrorKind::NotFound.into()), &state);
    assert_eq!(
        seen.checkpoint(Path::new("unavailable-source"), &state, |_, _| panic!(
            "unsupported write"
        ))
        .unwrap_err()
        .kind(),
        io::ErrorKind::Unsupported
    );
}
