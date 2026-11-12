//! Capability DATA, cached per exact helper image in the launching process.
//! Unavailable observations preserve the integer recipe for legacy/new
//! sessions. An existing ID-layout session needs a positive answer: an old
//! helper could otherwise select its label's different legacy holder.
//! None grants native admission, consent, custody or ID-layout publication.
use super::*;
#[cfg(unix)]
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};
#[cfg(unix)]
use std::{
    process::Command,
    time::{Duration, Instant},
};

#[cfg(unix)]
pub(crate) const MARKER: &str = "session-launch-identity-v1";
#[cfg(unix)]
const HELPERS: usize = 64;

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct Image {
    identity: crate::platform::FileIdentity,
    bytes: u64,
}
#[cfg(unix)]
fn image(path: &Path) -> Option<Image> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some(Image {
        identity: crate::platform::file_identity(path)?,
        bytes: meta.len(),
    })
}
#[cfg(unix)]
#[derive(Clone, Copy)]
struct Answer {
    image: Option<Image>,
    required: bool,
}
#[cfg(unix)]
type Slot = Arc<Mutex<Option<Answer>>>;
#[cfg(unix)]
static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, Slot>>> = OnceLock::new();

pub(crate) struct Observed {
    required: bool,
}
impl Observed {
    /// Same-image answers stay cached, positive or negative. A busy cache,
    /// bound, timeout, unreadable response or replacement keeps legacy. No
    /// probe is repeated until this path's observed image changes.
    pub(crate) fn probe(executable: &str) -> Self {
        // Windows constructors retain their existing integer/environment
        // handoff until that platform's continuity method is qualified.
        #[cfg(windows)]
        {
            let _ = executable;
            Self { required: false }
        }
        #[cfg(unix)]
        {
            let path = Path::new(executable);
            let key = image(path);
            let slot = {
                let Ok(mut cache) = CACHE.get_or_init(Default::default).try_lock() else {
                    return Self { required: false };
                };
                if !cache.contains_key(path) && cache.len() >= HELPERS {
                    return Self { required: false };
                }
                cache.entry(path.to_owned()).or_default().clone()
            };
            // Another original owns a cold query: this launch keeps its
            // integer recipe instead of waiting on another launch's helper.
            let Ok(mut answer) = slot.try_lock() else {
                return Self { required: false };
            };
            if let Some(old) = *answer {
                if old.image == key {
                    return Self {
                        required: old.required,
                    };
                }
            }
            let required = key.is_some()
                && (same_running_image(path, key) || query(path))
                && image(path) == key;
            *answer = Some(Answer {
                image: key,
                required,
            });
            Self { required }
        }
    }
    pub(crate) fn count(&self, session: &str, run: &str, count: usize) -> Result<String> {
        if self.required {
            required_count(session, run, count)
        } else {
            Ok(count.to_string())
        }
    }
    pub(crate) fn saved_count(&self, session: &str, run: &str) -> Result<String> {
        if !self.required {
            let config = crate::config::Config::load().map_err(|e| e.message)?;
            let host = config.host();
            return Err(format!("On {host}, the selected hgs helper cannot confirm this saved session's ID. Update hgs on {host} before resuming it."));
        }
        required_count(session, run, 0)
    }
    pub(crate) fn environment(&self, allocation: &Allocation) -> [(&'static str, String); 3] {
        if self.required || cfg!(windows) {
            allocation.environment()
        } else {
            [
                (DECLARATION, String::new()),
                ("HGS_SESSION_ID", String::new()),
                ("HGS_RUN_ID", allocation.run().to_owned()),
            ]
        }
    }
}

#[cfg(unix)]
fn query(path: &Path) -> bool {
    let mut command = Command::new(path);
    command.arg("--help");
    for key in [
        "HGS_SESSION",
        "HGS_SESSION_ID",
        "HGS_RUN_ID",
        DECLARATION,
        "HGS_RESUME_SESSION_ID",
        "HGS_EXPECTED_ID",
        "HGS_LAUNCH_ID",
    ] {
        command.env_remove(key);
    }
    crate::platform::run_bounded(
        &mut command,
        [64 << 10, 4096],
        Instant::now() + Duration::from_secs(2),
        &|| false,
    )
    .is_ok_and(|(status, out, _)| {
        status.success()
            && std::str::from_utf8(&out)
                .is_ok_and(|text| text.lines().any(|line| line.trim() == MARKER))
    })
}

// Production hgs knows its own parser. A libtest executable has a different
// entry point, so tests must use the real external CLI for capability probes.
#[cfg(unix)]
fn same_running_image(path: &Path, selected: Option<Image>) -> bool {
    #[cfg(test)]
    {
        let _ = (path, selected);
        false
    }
    #[cfg(all(not(test), target_os = "linux"))]
    {
        let own = image(Path::new("/proc/self/exe"));
        if own.is_some() && own == selected {
            return true;
        }
        same_linux_bytes(path)
    }
    #[cfg(all(not(test), target_os = "macos"))]
    {
        let _ = selected;
        zerus_client_core::macos_notify::peer::installation::same_running_code_for_capability(path)
    }
    #[cfg(all(not(test), not(any(target_os = "linux", target_os = "macos"))))]
    {
        let _ = (path, selected);
        false
    }
}

#[cfg(all(not(test), target_os = "linux"))]
fn same_linux_bytes(path: &Path) -> bool {
    use sha2::{Digest, Sha256};
    use std::{fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt};
    const BYTES: u64 = 64 << 20;
    fn hash(path: &Path) -> Option<Vec<u8>> {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .ok()?;
        let before = crate::platform::FileIdentity::of(&file).ok()?;
        let meta = file.metadata().ok()?;
        if !meta.is_file() || meta.len() > BYTES {
            return None;
        }
        let mut magic = [0; 4];
        file.read_exact(&mut magic).ok()?;
        if magic != *b"\x7fELF" {
            return None;
        }
        let mut digest = Sha256::new();
        digest.update(magic);
        let mut total = 4;
        let mut chunk = [0u8; 16384];
        loop {
            let n = file.read(&mut chunk).ok()?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > BYTES {
                return None;
            }
            digest.update(&chunk[..n]);
        }
        if total != meta.len() || crate::platform::FileIdentity::of(&file).ok()? != before {
            return None;
        }
        Some(digest.finalize().to_vec())
    }
    static OWN: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    let own = OWN.get_or_init(|| hash(Path::new("/proc/self/exe")));
    own.as_ref()
        .is_some_and(|own| hash(path).as_ref() == Some(own))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Parent writes inert data; a reaped copy child alone owns the executable
    /// writer. A sibling fork can retain the inert writer without delaying a
    /// shared stdin EOF or making the executable busy (zerus-0w3zj.6).
    /// Production only probes the selected helper; it never stages it.
    pub(super) fn stage_script(path: &std::path::Path, text: &str) {
        use std::io::Write as _;
        use std::process::Stdio;
        let source = path.with_file_name(format!(
            ".{}.source",
            path.file_name().unwrap().to_string_lossy()
        ));
        let mut file = std::fs::File::create(&source).expect("stage helper data");
        file.write_all(text.as_bytes()).expect("write helper data");
        #[cfg(target_os = "linux")]
        super::staging_tests::capture(&file);
        drop(file);
        match std::fs::remove_file(path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                panic!("cannot replace {}: {error}", path.display())
            }
            _ => {}
        }
        let status = Command::new("/bin/cp")
            .arg(&source)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("stage helper script");
        std::fs::remove_file(source).expect("remove helper data");
        assert!(status.success(), "copy helper script");
    }

    #[test]
    fn unchanged_images_cache_positive_and_negative_answers_and_replacements_reprobe() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("helper");
        let counter = temp.path().join("calls");
        let write = |marker: bool| {
            let staging = temp.path().join("new-helper");
            let text = if marker { MARKER } else { "legacy helper" };
            let script = format!(
                "#!/bin/sh\nprintf 'query\\n' >> {}\nprintf '%s\\n' '{}'\n",
                crate::platform::quote(&counter.to_string_lossy()),
                text
            );
            stage_script(&staging, &script);
            std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::rename(staging, &path).unwrap();
        };
        write(true);
        let start = Instant::now();
        assert!(Observed::probe(path.to_str().unwrap()).required);
        let cold = start.elapsed();
        let start = Instant::now();
        for _ in 0..100 {
            assert!(Observed::probe(path.to_str().unwrap()).required);
        }
        let warm = start.elapsed() / 100;
        assert_eq!(
            std::fs::read_to_string(&counter).unwrap().lines().count(),
            1
        );
        write(false);
        assert!(!Observed::probe(path.to_str().unwrap()).required);
        for _ in 0..100 {
            assert!(!Observed::probe(path.to_str().unwrap()).required);
        }
        assert_eq!(
            std::fs::read_to_string(&counter).unwrap().lines().count(),
            2
        );
        println!(
            "helper capability cold_us={} warm_ns={} samples=100",
            cold.as_micros(),
            warm.as_nanos()
        );
    }

    #[test]
    fn an_unavailable_helper_uses_integer_and_clears_parent_id_markers() {
        let temp = tempfile::tempdir().unwrap();
        let helper = Observed::probe(temp.path().join("missing").to_str().unwrap());
        let allocation = Allocation::fresh();
        assert_eq!(
            helper
                .count(allocation.session(), allocation.run(), 3)
                .unwrap(),
            "3"
        );
        assert!(helper
            .saved_count(allocation.session(), allocation.run())
            .unwrap_err()
            .contains("cannot confirm this saved session's ID"));
        assert_eq!(
            Observed { required: true }
                .saved_count(allocation.session(), allocation.run())
                .unwrap(),
            required_count(allocation.session(), allocation.run(), 0).unwrap()
        );
        assert_eq!(
            helper.environment(&allocation),
            [
                (DECLARATION, String::new()),
                ("HGS_SESSION_ID", String::new()),
                ("HGS_RUN_ID", allocation.run().to_owned())
            ]
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "helper/staging_tests.rs"]
mod staging_tests;
