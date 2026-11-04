//! The Rust core of the Zerus desktop client: one `hgs [@host] watch --json`
//! per computer, decoded with `zerus-watch-protocol`, kept as catalog and
//! Activity state and handed out as keyed [`Change`]s that a [`Coalescer`]
//! folds into one [`Batch`] per display frame.
//!
//! Pure Rust with no Tauri or async runtime by default, so it tests fast. See
//! `docs/superpowers/plans/2026-10-28-tauri-client-phase1.md` (Tasks 3 and 4)
//! and the "Event stream" section of `docs/reference.md`.

pub mod account_actions;
pub mod actions;
pub mod analytics;
pub mod approvals;
pub mod attachments;
pub mod attention;
pub mod attention_store;
pub mod batch;
pub mod cache;
pub mod child_environment;
#[cfg(windows)]
mod conpty;
pub mod content_search;
pub mod creation;
pub mod desktop;
pub mod details;
pub mod download_cache;
pub mod drafts;
pub mod end_action;
pub mod external_file;
pub mod file_download;
pub mod fleet;
pub mod fsx;
pub mod input_queue;
pub mod launch;
pub mod legacy;
pub mod linux_notify;
pub mod local_messages;
pub mod machine;
pub mod machine_connections;
pub mod macos_notify;
pub mod maintenance;
pub mod marks;
pub mod menubar;
pub mod migration;
pub mod mobile;
pub mod native_notice;
pub mod operation_recovery;
pub mod organization;
pub mod paths;
pub mod process_view;
pub mod processes;
#[cfg(unix)]
mod pty;
pub mod qt_lock;
pub mod questions;
pub mod quit_journal;
pub mod readmarks;
pub mod reads;
pub mod readsync;
pub mod recovery_settings;
pub mod repository;
pub mod row_preferences;
pub mod run;
pub mod send;
pub mod session_file_reference;
pub mod session_usage;
pub mod settings;
pub mod shell;
pub mod source;
pub mod speech_settings;
pub mod state;
pub mod stream;
pub mod swarm;
pub mod task_board;
pub mod task_cancel;
pub mod task_details;
pub mod task_executor;
pub mod task_feedback;
pub mod task_settings;
pub mod task_start;
pub mod task_view;
pub mod tasks;
pub mod tasks_write;
pub mod terminal;
pub mod terminal_launch;
pub mod termination_cleanup;
pub mod text;
pub mod trayicon;
pub mod untracked_shell;
pub mod updates;
pub mod video_posters;
pub mod view;
pub mod views;
pub mod voice;
pub mod voice_composer;
pub mod voice_driver;
pub mod worktree_overview;
pub mod worktree_policy;
pub mod worktree_review;
pub mod worktrees;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

pub use actions::{Action, Exit, Invocation, Outcome, Target, WorktreeCleanup};
pub use batch::MachineOut;
pub use batch::OrganizationOut;
pub use batch::ProcessesOut;
pub use batch::TasksOut;
pub use batch::{ActivityOut, Batch, Coalescer, ComputerOut, KeyOut, ReplaceOut, RowOut};
pub use fleet::{Fleet, FollowError};
pub use machine::{LocalHost, MachineMeta};
pub use organization::{OrganizationSnapshot, OrganizationState, OrganizationUpdate};
pub use processes::{ProcessesState, ProcessesUpdate};
pub use reads::{Read, ReadError};
pub use run::{run, run_cancellable, run_limited, run_with_env, Stopped, TooLarge};
pub use state::{ActivityState, CatalogState};
pub use stream::{StreamEvent, StreamHandle, Timing};
pub use swarm::grouping::{GroupOut, GroupingOut, HeaderOut, SyncView};
pub use tasks::{TasksState, TasksUpdate};
pub use view::ViewRow;
pub use zerus_watch_protocol::SessionRef;

/// A computer by the alias `hgs @alias` takes; the empty string is this
/// computer. Peers are never keyed by the host name their stream reports.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ComputerId(pub String);

impl ComputerId {
    /// This computer.
    pub fn local() -> Self {
        Self(String::new())
    }

    pub fn is_local(&self) -> bool {
        self.0.is_empty()
    }

    /// The peer's alias, None for this computer.
    pub fn alias(&self) -> Option<&str> {
        (!self.0.is_empty()).then_some(self.0.as_str())
    }
}

impl From<&str> for ComputerId {
    fn from(alias: &str) -> Self {
        Self(alias.to_owned())
    }
}

/// The connection state of one computer's stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComputerStatus {
    /// Checking `hgs [@host] -h` for the watch-v1 marker before the first stream.
    Probing,
    /// Its CLI lacks the marker: no stream runs; it is probed again every 10 min.
    /// Until a listing of it succeeds it has no rows.
    Unsupported,
    /// Its CLI lacks the marker, and it is polled as the Qt client does: its
    /// listing every 60 s and followed sessions every few seconds (see
    /// [`legacy`]). A failed listing makes it `Reconnecting` with its rows
    /// kept; the next listing is the retry.
    Polled,
    /// A stream delivered its catalog snapshot.
    Live,
    /// The stream ended, failed or stayed silent; `attempt` counts the failures
    /// that set the restart delay (5 s, 15 s, 60 s, then 5 min). Its rows stay
    /// as last known.
    Reconnecting { attempt: u32 },
    /// The computer left the fleet or the fleet stopped.
    Ended,
}

impl ComputerStatus {
    /// The snake_case name a [`Batch`] carries.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Probing => "probing",
            Self::Unsupported => "unsupported",
            Self::Polled => "polled",
            Self::Live => "live",
            Self::Reconnecting { .. } => "reconnecting",
            Self::Ended => "ended",
        }
    }

    pub fn attempt(&self) -> u32 {
        match self {
            Self::Reconnecting { attempt } => *attempt,
            _ => 0,
        }
    }
}

/// One change of a followed session's Activity.
#[derive(Clone, Debug, PartialEq)]
pub enum ActivityUpdate {
    /// Events after the previous cursor, oldest first.
    Events { events: Vec<Value>, cursor: i64 },
    /// A new event window that replaces what the view holds (a tail snapshot
    /// after a gap, or a DeepSeek or OpenCode window).
    Replace { events: Vec<Value>, cursor: i64 },
    /// A replacement of one open event by its stable ID; a higher `rev` wins.
    /// The core merges activity `event` frames into [`ActivityUpdate::State`]
    /// instead; this variant stays for consumers of later frame kinds.
    Event { id: String, rev: u64, event: Value },
    /// The session's latest inspection summary, with provider messages merged.
    State { value: Value, rev: u64 },
    /// Read transport readiness. This never declares native work finished or
    /// replaces the last known transcript; a new route resets state revisions.
    Connection {
        ready: bool,
        pending: bool,
        code: String,
    },
    /// The subscription ended (the session left the catalog or changed run).
    Ended,
}

/// One keyed change for the view.
// Upserts dominate and carry their row inline, as the plan's interface fixes;
// boxing it would allocate twice per upsert for no gain.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Upsert {
        computer: ComputerId,
        key: String,
        row: ViewRow,
    },
    /// `reason` is one of `replaced`, `archived`, `forgotten` or `ended`; an
    /// unknown or missing reason is None. The row leaves the list either way.
    Remove {
        computer: ComputerId,
        key: String,
        reason: Option<String>,
    },
    /// All rows of the computer, replacing what the view holds, sorted by key.
    Replace {
        computer: ComputerId,
        rows: Vec<(String, ViewRow)>,
    },
    Computer {
        computer: ComputerId,
        status: ComputerStatus,
    },
    Activity {
        computer: ComputerId,
        key: String,
        update: ActivityUpdate,
    },
    /// A followed session's processes; see [`processes`].
    Processes {
        computer: ComputerId,
        key: String,
        update: ProcessesUpdate,
    },
    /// A followed project's task list, keyed by its project ID; see [`tasks`].
    Tasks {
        computer: ComputerId,
        key: String,
        update: TasksUpdate,
    },
    /// The computer's card data from its `catalog.meta`, sent when it changed;
    /// see [`machine`].
    Machine {
        computer: ComputerId,
        machine: MachineMeta,
    },
    /// The followed shared project catalog; see [`organization`].
    Organization {
        computer: ComputerId,
        update: OrganizationUpdate,
    },
}

impl Change {
    pub fn computer(&self) -> &ComputerId {
        match self {
            Self::Upsert { computer, .. }
            | Self::Remove { computer, .. }
            | Self::Replace { computer, .. }
            | Self::Computer { computer, .. }
            | Self::Activity { computer, .. }
            | Self::Processes { computer, .. }
            | Self::Tasks { computer, .. }
            | Self::Machine { computer, .. }
            | Self::Organization { computer, .. } => computer,
        }
    }
}

/// The `hgs` the client runs: `ZERUS_CLIENT_HGS` when set, otherwise the first
/// `hgs` on `PATH` or in the folders the installers use. An app started from
/// Finder or a desktop launcher gets a short `PATH` without `~/.local/bin`.
/// Falls back to the bare name, so a missing `hgs` reports as not found.
pub fn default_hgs() -> PathBuf {
    std::env::var_os("ZERUS_CLIENT_HGS")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| find_hgs(std::env::var_os("PATH"), std::env::var_os("HOME")))
        .unwrap_or_else(|| PathBuf::from(HGS))
}

const HGS: &str = if cfg!(windows) { "hgs.exe" } else { "hgs" };

/// The first `hgs` in `path`, then in `~/.local/bin` and the Homebrew and
/// `/usr/local` folders.
fn find_hgs(path: Option<std::ffi::OsString>, home: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let mut folders: Vec<PathBuf> = path
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    if let Some(home) = home.filter(|home| !home.is_empty()) {
        folders.push(PathBuf::from(home).join(".local").join("bin"));
    }
    if !cfg!(windows) {
        folders.push(PathBuf::from("/opt/homebrew/bin"));
        folders.push(PathBuf::from("/usr/local/bin"));
    }
    folders
        .into_iter()
        .map(|folder| folder.join(HGS))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod hgs_tests {
    use super::*;
    use std::path::Path;

    /// A scratch folder, removed again when the test ends.
    struct Folder(PathBuf);

    impl std::ops::Deref for Folder {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn folder(name: &str) -> Folder {
        let dir =
            std::env::temp_dir().join(format!("zerus-client-core-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Folder(dir)
    }

    #[test]
    fn path_comes_before_the_install_folders() {
        let home = folder("home");
        let bin = folder("path-bin");
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        std::fs::write(home.join(".local/bin").join(HGS), b"").unwrap();
        std::fs::write(bin.join(HGS), b"").unwrap();
        let path = std::env::join_paths([bin.to_path_buf()]).unwrap();
        assert_eq!(
            find_hgs(Some(path), Some(home.to_path_buf().into())),
            Some(bin.join(HGS))
        );
    }

    #[test]
    fn a_finder_path_still_finds_the_local_bin() {
        let home = folder("finder-home");
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        std::fs::write(home.join(".local/bin").join(HGS), b"").unwrap();
        let empty = folder("finder-path");
        let path = std::env::join_paths([empty.to_path_buf()]).unwrap();
        assert_eq!(
            find_hgs(Some(path), Some(home.to_path_buf().into())),
            Some(home.join(".local/bin").join(HGS))
        );
    }

    #[test]
    fn a_folder_named_hgs_is_not_a_program() {
        let bin = folder("dir-bin");
        std::fs::create_dir_all(bin.join(HGS)).unwrap();
        let path = std::env::join_paths([bin.to_path_buf()]).unwrap();
        let found = find_hgs(Some(path), None);
        assert!(found.is_none_or(|found| found.is_file()));
    }
}
