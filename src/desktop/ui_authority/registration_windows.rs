//! Native caller evidence for explicit registration changes, never a G1 grant.
use super::*;
use std::{
    fs::{File, OpenOptions},
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
};
use windows_sys::Win32::{
    Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
    },
    System::Console::{GetConsoleMode, GetConsoleProcessList, GetConsoleWindow},
    UI::WindowsAndMessaging::{GetForegroundWindow, IsWindowVisible},
};

const CONSOLE_PROCESSES: usize = 128;
const GUIDANCE: &str = "desktop mode changes require Zerus Settings or an attached foreground classic console outside agents; this terminal topology could not be verified";

pub(in crate::desktop) struct Caller {
    ancestry: Ancestry,
    evidence: Evidence,
    configuration: String,
    session: u32,
}
enum Evidence {
    Frontend {
        pid: u32,
        birth: u64,
        file: File,
        identity: platform::FileIdentity,
    },
    Console {
        input: File,
        window: usize,
        parent: u32,
        birth: u64,
    },
}

fn same_object(left: &platform::FileIdentity, right: &platform::FileIdentity) -> bool {
    left.inode != 0 && left.device == right.device && left.inode == right.inode
}

struct ConsoleFacts {
    window: usize,
    foreground: usize,
    visible: bool,
    input: bool,
    processes: Vec<u32>,
}
impl ConsoleFacts {
    fn read(input: &File) -> Result<Self> {
        let mut mode = 0;
        // The original handle names this process's already-attached CONIN$.
        // No AttachConsole, AllocConsole or foreground manipulation is used.
        let is_input = unsafe { GetConsoleMode(input.as_raw_handle(), &mut mode) } != 0;
        let window = unsafe { GetConsoleWindow() };
        let foreground = unsafe { GetForegroundWindow() };
        let visible = !window.is_null() && unsafe { IsWindowVisible(window) } != 0;
        let mut processes = [0; CONSOLE_PROCESSES];
        let count =
            unsafe { GetConsoleProcessList(processes.as_mut_ptr(), CONSOLE_PROCESSES as u32) }
                as usize;
        ensure!((1..=CONSOLE_PROCESSES).contains(&count), "{GUIDANCE}");
        Ok(Self {
            window: window as usize,
            foreground: foreground as usize,
            visible,
            input: is_input,
            processes: processes[..count].to_vec(),
        })
    }
    fn matches(&self, window: usize, caller: u32, parent: u32) -> bool {
        self.input
            && self.visible
            && window != 0
            && self.window == window
            && self.foreground == window
            && caller != 0
            && parent != 0
            && caller != parent
            && self.processes.len() <= CONSOLE_PROCESSES
            && !self.processes.contains(&0)
            && self.processes.contains(&caller)
            && self.processes.contains(&parent)
    }
}

impl Caller {
    pub(in crate::desktop) fn capture(scope: &Scope) -> Result<Self> {
        ensure!(
            ["HGS_SESSION", "HGS_RUN_ID"]
                .iter()
                .all(|key| std::env::var_os(key).is_none()),
            "agent session cannot change desktop mode; use Settings outside Zerus agents"
        );
        let session = crate::host::token::windows_session()?;
        ensure!(
            session > 0,
            "desktop mode changes require an interactive user session"
        );
        let sid = crate::host::token::user_sid()?;
        ensure!(
            !matches!(sid.as_str(), "S-1-5-18" | "S-1-5-19" | "S-1-5-20")
                && !sid.starts_with("S-1-5-80-"),
            "a system/service account cannot change desktop mode"
        );
        let until = Instant::now() + WAIT;
        let ancestry = Ancestry::inspect(std::process::id(), until)?;
        // These are the SAME native objects inspected above. In particular,
        // the checking CLI cannot serve as its own installed frontend.
        let selected = ancestry
            .ancestors()
            .find(|process| scope.selected_frontend(Path::new(&process.image)));
        let evidence = if let Some(process) = selected {
            let file = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&process.image)?;
            let (identity, metadata) = platform::FileIdentity::with_metadata(&file)?;
            use std::os::windows::fs::MetadataExt;
            ensure!(
                metadata.is_file()
                    && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
                "installed frontend image changed"
            );
            ensure!(
                scope.selected_frontend(Path::new(&process.image)) && process.current(),
                "installed frontend changed"
            );
            Evidence::Frontend {
                pid: process.pid,
                birth: process.birth,
                file,
                identity,
            }
        } else {
            let parent = ancestry.ancestors().next().context(GUIDANCE)?;
            let input = OpenOptions::new()
                .read(true)
                .open(r"\\.\CONIN$")
                .context(GUIDANCE)?;
            let facts = ConsoleFacts::read(&input)?;
            ensure!(
                facts.matches(facts.window, std::process::id(), parent.pid),
                "{GUIDANCE}"
            );
            Evidence::Console {
                input,
                window: facts.window,
                parent: parent.pid,
                birth: parent.birth,
            }
        };
        let result = Self {
            ancestry,
            evidence,
            configuration: scope.configuration.clone(),
            session,
        };
        result.check_until(scope, until)?;
        Ok(result)
    }

    pub(in crate::desktop) fn source(&self) -> &'static str {
        match self.evidence {
            Evidence::Frontend { .. } => "installed_frontend",
            Evidence::Console { .. } => "interactive_terminal",
        }
    }

    /// Recheck the originals under one fresh bounded observation, never a new
    /// PID walk. The caller retains this evidence across its original effects.
    pub(in crate::desktop) fn check(&self, scope: &Scope) -> Result<()> {
        self.check_until(scope, Instant::now() + WAIT)
    }

    fn check_until(&self, scope: &Scope, until: Instant) -> Result<()> {
        ensure!(
            self.configuration == scope.configuration
                && self.session == crate::host::token::windows_session()?,
            "mode caller scope changed"
        );
        self.ancestry.validate(until)?;
        match &self.evidence {
            Evidence::Frontend {
                pid,
                birth,
                file,
                identity,
            } => {
                let process = self
                    .ancestry
                    .ancestors()
                    .find(|process| process.pid == *pid && process.birth == *birth)
                    .context("original installed frontend unavailable")?;
                ensure!(
                    same_object(identity, &platform::FileIdentity::of(file)?)
                        && platform::file_identity(Path::new(&process.image))
                            .is_some_and(|current| same_object(identity, &current))
                        && scope.selected_frontend(Path::new(&process.image))
                        && process.current(),
                    "original installed frontend changed"
                );
            }
            Evidence::Console {
                input,
                window,
                parent,
                birth,
            } => {
                ensure!(
                    self.ancestry
                        .ancestors()
                        .any(|process| process.pid == *parent
                            && process.birth == *birth
                            && process.current()),
                    "original console ancestor changed"
                );
                ensure!(
                    ConsoleFacts::read(input)?.matches(*window, std::process::id(), *parent),
                    "{GUIDANCE}"
                );
            }
        }
        self.ancestry.validate(until)?;
        Ok(())
    }
}

pub(in crate::desktop) fn caller(scope: &Scope) -> Result<&'static str> {
    let original = Caller::capture(scope)?;
    original.check(scope)?;
    Ok(original.source())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_visible_foreground_input_console_with_both_original_processes_matches() {
        let mut facts = ConsoleFacts {
            window: 10,
            foreground: 10,
            visible: true,
            input: true,
            processes: vec![41, 42],
        };
        assert!(facts.matches(10, 41, 42));
        for processes in [
            vec![],
            vec![41],
            vec![42],
            vec![0, 41, 42],
            vec![41; CONSOLE_PROCESSES + 1],
        ] {
            facts.processes = processes;
            assert!(!facts.matches(10, 41, 42));
        }
        facts.processes = vec![41, 42];
        facts.visible = false;
        assert!(!facts.matches(10, 41, 42));
        facts.visible = true;
        facts.input = false;
        assert!(!facts.matches(10, 41, 42));
        facts.input = true;
        facts.foreground = 0;
        assert!(!facts.matches(10, 41, 42));
        facts.foreground = 11;
        assert!(!facts.matches(10, 41, 42));
        facts.foreground = 10;
        assert!(!facts.matches(11, 41, 42));
        assert!(!facts.matches(0, 41, 42));
        assert!(!facts.matches(10, 41, 41));
    }

    #[test]
    fn file_object_identity_uses_volume_and_index_instead_of_timestamps() {
        let original = platform::FileIdentity {
            device: 1,
            inode: 2,
            modified: 3,
            modified_nanos: 4,
        };
        let mut observed = original;
        observed.modified += 1;
        observed.modified_nanos += 1;
        assert!(same_object(&original, &observed));
        observed.inode += 1;
        assert!(!same_object(&original, &observed));
        observed.inode = original.inode;
        observed.device += 1;
        assert!(!same_object(&original, &observed));
    }
}
