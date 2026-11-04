//! Exact terminal evidence for an untracked shell; never a tracked agent run.
use serde::{Deserialize, Serialize};

pub const UNVERIFIED: &str = "This session is not tracked by Zerus and its terminal could not be verified. Open its terminal and close it with exit after its jobs finish.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub version: u8,
    pub socket: String,
    pub socket_dev: String,
    pub socket_ino: String,
    pub server_pid: u32,
    pub server_start: String,
    pub session_id: String,
    pub session_created: u64,
    pub pane_id: String,
    pub pane_pid: u32,
    pub pane_start: String,
    pub shell: String,
}
fn text(s: &str, limit: usize) -> bool {
    !s.is_empty() && s.len() <= limit && !s.chars().any(char::is_control)
}
fn id(s: &str, prefix: char) -> bool {
    s.strip_prefix(prefix)
        .is_some_and(|s| !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()))
}
pub fn shell(command: &str) -> bool {
    matches!(
        command,
        "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "mksh" | "tcsh" | "csh"
    )
}
impl Identity {
    pub fn valid(&self) -> bool {
        self.version == 1
            && text(&self.socket, 4096)
            && self.socket.starts_with('/')
            && self.socket_dev.parse::<u64>().is_ok()
            && self.socket_ino.parse::<u64>().is_ok_and(|n| n > 0)
            && self.server_pid > 1
            && self.pane_pid > 1
            && text(&self.server_start, 128)
            && text(&self.pane_start, 128)
            && id(&self.session_id, '$')
            && id(&self.pane_id, '%')
            && self.session_created > 0
            && shell(&self.shell)
    }
}
