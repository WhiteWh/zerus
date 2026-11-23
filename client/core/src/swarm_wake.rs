//! Task #20 of team c536f0e5 (P2.6c): a peer's watch stream showed its swarm
//! head move (its catalog or its tasks changed), so this computer's swarm
//! worker syncs with that peer at once instead of at its periodic sync. The
//! desktop touches `<hgs configuration>/swarm/wake/<alias>`; the worker,
//! which watches that folder, reads and removes it. An older worker ignores
//! the folder and syncs on its own schedule.
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The hgs configuration folder, as `hgs` resolves it: `HGS_CONFIG_DIR`,
/// else `.config/hgs` in the home folder.
pub fn hgs_configuration() -> Option<PathBuf> {
    let absolute = |name: &str| std::env::var_os(name).map(PathBuf::from).filter(|path| path.is_absolute());
    if let Some(dir) = absolute("HGS_CONFIG_DIR") {
        return Some(dir);
    }
    let home = absolute("HOME").or_else(|| if cfg!(windows) { absolute("USERPROFILE") } else { None })?;
    Some(home.join(".config").join("hgs"))
}

/// Asks the swarm worker under `configuration` to sync with `alias` now.
/// False when the alias cannot name a member or the file cannot be written.
pub fn touch(configuration: &Path, alias: &str) -> bool {
    if !crate::fleet::valid_alias(alias) || alias.contains(['/', '\\']) || alias.starts_with('.') {
        return false;
    }
    let swarm = configuration.join("swarm");
    // No swarm here: no worker to wake.
    if !swarm.is_dir() {
        return false;
    }
    let dir = swarm.join("wake");
    std::fs::create_dir_all(&dir).is_ok() && std::fs::write(dir.join(alias), b"").is_ok()
}

/// The swarm head of one stream, and whether it moved since it was last
/// asked ([`SwarmHead::take_moved`]).
#[derive(Debug, Default)]
pub struct SwarmHead {
    head: Option<Value>,
    moved: bool,
}

impl SwarmHead {
    /// The `swarm` row of a snapshot or a delta. The first one of a stream
    /// counts as moved too: changes made while it was down were not seen.
    pub fn row(&mut self, row: &Value) {
        if self.head.as_ref() != Some(row) {
            self.head = Some(row.clone());
            self.moved = true;
        }
    }

    /// A gap, a failure or a new stream: the head is unknown.
    pub fn reset(&mut self) {
        self.head = None;
    }

    /// Whether the head moved since the last call.
    pub fn take_moved(&mut self) -> bool {
        std::mem::take(&mut self.moved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_moved_head_is_told_once_and_wakes_the_member_by_its_file() {
        let mut head = SwarmHead::default();
        assert!(!head.take_moved());
        head.row(&json!({"catalog":"a","tasks":"t"}));
        assert!(head.take_moved(), "the first head of a stream");
        assert!(!head.take_moved());
        head.row(&json!({"catalog":"a","tasks":"t"}));
        assert!(!head.take_moved(), "the same head");
        head.row(&json!({"catalog":"b","tasks":"t"}));
        assert!(head.take_moved());
        head.reset();
        head.row(&json!({"catalog":"b","tasks":"t"}));
        assert!(head.take_moved(), "after a gap the head is new again");

        let dir = std::env::temp_dir().join(format!("zerus-swarm-wake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!touch(&dir, "msi-old"), "no swarm folder, no worker");
        std::fs::create_dir_all(dir.join("swarm")).unwrap();
        assert!(touch(&dir, "msi-old"));
        assert!(dir.join("swarm/wake/msi-old").is_file());
        for bad in ["", "-x", "a b", "../x", "a/b"] {
            assert!(!touch(&dir, bad), "{bad}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
