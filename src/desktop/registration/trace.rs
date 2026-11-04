//! Opt-in phase evidence survives the runner's intentionally bare TimedOut.
//! It never contributes to registration outcome or exposes native identities.
#[cfg(windows)]
use std::{io::Read, time::Instant};

const CAP: usize = 4096;
const PHASES: &[&str] = &[
    "main",
    "encoding",
    "opened",
    "bytes",
    "decoded",
    "input",
    "sid",
    "mutex",
    "created",
    "connected",
    "folder",
    "task",
    "found",
    "task_error",
    "done",
];

#[cfg(windows)]
pub(in crate::desktop) struct Trace {
    enabled: bool,
    started: Instant,
    file: Option<(tempfile::NamedTempFile, String)>,
}
#[cfg(windows)]
impl Trace {
    pub fn new() -> Self {
        let enabled = std::env::var("HGS_DESKTOP_REGISTRATION_TRACE").as_deref() == Ok("1");
        let file = enabled
            .then(|| {
                let file = tempfile::Builder::new()
                    .prefix("hgs-registration-phase-")
                    .tempfile()
                    .ok()?;
                let path = file.path().to_str()?.to_owned();
                Some((file, path))
            })
            .flatten();
        let started = Instant::now();
        if enabled {
            eprintln!("hgs-registration-phase runner_call 0");
        }
        Self {
            enabled,
            started,
            file,
        }
    }
    pub fn path(&self) -> Option<&str> {
        self.file.as_ref().map(|(_, path)| path.as_str())
    }
    pub fn finish(self) {
        if !self.enabled {
            return;
        }
        eprintln!(
            "hgs-registration-phase runner_return {}",
            self.started.elapsed().as_millis()
        );
        let Some((file, _)) = self.file else {
            eprintln!("hgs-registration-phase evidence_unavailable");
            return;
        };
        // Read only our retained file handle, never reopen a supplied path.
        let mut bytes = Vec::new();
        let result = file
            .as_file()
            .take((CAP + 1) as u64)
            .read_to_end(&mut bytes);
        if result.is_err() {
            eprintln!("hgs-registration-phase evidence_unreadable");
            return;
        }
        match checked(&bytes) {
            Some(lines) => {
                for line in lines {
                    eprintln!("hgs-registration-phase child {line}");
                }
            }
            None => eprintln!("hgs-registration-phase evidence_refused"),
        }
    }
}

fn checked(bytes: &[u8]) -> Option<Vec<&str>> {
    if bytes.len() > CAP {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = Vec::new();
    for line in text.lines() {
        if lines.len() == 32 || line.len() > 64 {
            return None;
        }
        let (phase, elapsed) = line.split_once(' ')?;
        if !PHASES.contains(&phase)
            || elapsed.is_empty()
            || !elapsed.bytes().all(|byte| byte.is_ascii_digit())
            || elapsed.parse::<u64>().is_err()
        {
            return None;
        }
        lines.push(line);
    }
    Some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retains_partial_phase_prefix_without_inventing_a_completion() {
        assert_eq!(
            checked(b"main 0\ninput 4\nmutex 6\ncreated 12\n"),
            Some(vec!["main 0", "input 4", "mutex 6", "created 12"])
        );
        assert_eq!(checked(b""), Some(Vec::new()));
    }
    #[test]
    fn refuses_unknown_fields_private_text_and_invalid_numbers() {
        for bytes in [
            b"path private".as_slice(),
            b"main 1 extra",
            b"connected -1",
            b"main 18446744073709551616",
            b"main \xff",
        ] {
            assert!(checked(bytes).is_none());
        }
    }
    #[test]
    fn refuses_more_than_the_fixed_evidence_bounds() {
        assert!(checked(&vec![b'x'; CAP + 1]).is_none());
        assert!(checked("main 0\n".repeat(33).as_bytes()).is_none());
        assert!(checked(format!("main {}\n", "0".repeat(64)).as_bytes()).is_none());
    }
}
