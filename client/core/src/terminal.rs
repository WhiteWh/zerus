//! The session Terminal, ported from the Qt client: when a session has a
//! terminal and why not, when a new target reconnects, how dropped file paths
//! are pasted, the status texts, and the attach client itself (`hgs a NAME
//! --existing` in a pseudoterminal).
//!
//! Reference: `tray/src/NativeSessionCapabilities.h` (`terminal`,
//! `terminalReason`), `tray/src/TerminalView.cpp` (`setSession`,
//! `connectSession`, statuses), `tray/src/PtyProcess.cpp`,
//! `tray/src/SessionsWindow.cpp` (availability, pasting dropped paths).
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::OsString;
use std::sync::mpsc::{sync_channel, Receiver};

/// What a session reports about its terminal: its row's fields, which the
/// selected session's inspected details override.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalFacts {
    /// The agent command (`cmd`, else `agent`), such as `claude` or `dsh`.
    pub cmd: String,
    /// The row's `terminal_supported`, true when it reports none.
    pub terminal_supported: bool,
    /// The row has a boolean `terminal_supported`.
    pub terminal_reported: bool,
    pub terminal_attach_supported: bool,
    /// The row has a boolean `terminal_attach_supported` (OpenCode).
    pub terminal_attach_reported: bool,
    /// The row's `terminal_reason`.
    pub terminal_reason: String,
    /// The row's `terminal_attach_reason` (OpenCode).
    pub terminal_attach_reason: String,
}

fn flag(details: &Value, key: &str) -> Option<bool> {
    details.get(key).and_then(Value::as_bool)
}

/// Whether the session has a terminal (`NativeSessionCapabilities::terminal`,
/// as the frontend's `terminal()` in `actions-ui.ts` reads it: DeepSeek under
/// either command name has Native UI instead).
pub fn supported(facts: &TerminalFacts, details: &Value) -> bool {
    if facts.cmd == "dsh" || facts.cmd == "deepseek" {
        return false;
    }
    if facts.cmd == "opencode" {
        if let Some(attach) = flag(details, "terminal_attach_supported") {
            return attach;
        }
        if facts.terminal_attach_reported {
            return facts.terminal_attach_supported;
        }
    }
    if let Some(supported) = flag(details, "terminal_supported") {
        return supported;
    }
    if facts.cmd == "opencode" && !facts.terminal_reported {
        return false;
    }
    facts.terminal_supported
}

/// Why the session has no terminal (`NativeSessionCapabilities::terminalReason`).
pub fn unsupported_reason(facts: &TerminalFacts, details: &Value) -> String {
    let text = |key: &str, fallback: &str| -> String {
        details
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(fallback)
            .to_owned()
    };
    if facts.cmd == "opencode"
        && (flag(details, "terminal_attach_supported").is_some() || facts.terminal_attach_reported)
    {
        let reason = text("terminal_attach_reason", &facts.terminal_attach_reason);
        return if reason.is_empty() {
            "Refresh this session after its OpenCode service reconnects.".into()
        } else {
            reason
        };
    }
    let reason = text("terminal_reason", &facts.terminal_reason);
    if reason.is_empty() {
        "This native adapter does not provide Terminal.".into()
    } else {
        reason
    }
}

/// Whether the Terminal can connect now, else the reason the view shows
/// (`SessionsWindow.cpp`, `m_terminal->setAvailable`).
pub fn available(
    facts: &TerminalFacts,
    details: &Value,
    state: &str,
    online: bool,
    restored_pending: bool,
) -> Result<(), String> {
    let supported = supported(facts, details);
    if online && state == "running" && supported && !restored_pending {
        return Ok(());
    }
    Err(if !online {
        "Machine unavailable. Reconnect when it is online.".into()
    } else if restored_pending {
        "Refresh the restored session before opening Terminal.".into()
    } else if !supported {
        unsupported_reason(facts, details)
    } else {
        "Resume this session before opening its terminal.".into()
    })
}

/// Where a Terminal attaches: `hgs`, the computer's alias (empty for this
/// computer), the session name and its run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub hgs: String,
    pub host: String,
    pub name: String,
    pub run_id: String,
}

/// Whether moving from `old` to `new` disconnects and clears the screen
/// (`TerminalView::setSession`): another endpoint, another name without the
/// same run, or another known run.
pub fn target_changed(old: &Target, new: &Target) -> bool {
    let same_endpoint = old.hgs == new.hgs && old.host == new.host;
    let same_run = !old.run_id.is_empty() && old.run_id == new.run_id;
    !same_endpoint
        || (old.name != new.name && !same_run)
        || (!old.run_id.is_empty() && !new.run_id.is_empty() && old.run_id != new.run_id)
}

/// The run a Terminal keeps after a new target: an inspection that omits the
/// run never erases an established one unless the target changed.
pub fn kept_run(old: &Target, new: &Target) -> String {
    if target_changed(old, new) || !new.run_id.is_empty() {
        new.run_id.clone()
    } else {
        old.run_id.clone()
    }
}

/// Qt's SessionCommands::powerShellQuote (SessionCommands.h:40-51).
fn powershell_quote(path: &str) -> String {
    let native = path.replace('/', "\\");
    if !native.is_empty()
        && native
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./\\:=+-".contains(c))
    {
        return native;
    }
    let mut escaped = String::with_capacity(native.len() + 2);
    escaped.push('\'');
    for c in native.chars() {
        escaped.push(c);
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}') {
            escaped.push(c);
        }
    }
    escaped.push('\'');
    escaped
}

/// Dropped file paths as Terminal input: each quoted for the session's shell
/// (POSIX single quotes, or PowerShell on Windows with native separators),
/// joined by spaces with a trailing space.
pub fn paste_paths(paths: &[String], windows: bool) -> Result<String, String> {
    let mut quoted = Vec::with_capacity(paths.len());
    for path in paths {
        if path.chars().any(|c| (c as u32) < 0x20 || c == '\u{7f}') {
            return Err(
                "File names containing control characters cannot be pasted into Terminal.".into(),
            );
        }
        quoted.push(if windows {
            powershell_quote(path)
        } else {
            format!("'{}'", path.replace('\'', "'\\''"))
        });
    }
    Ok(format!("{} ", quoted.join(" ")))
}

/// Scrollback of the webview terminal, as Qt's `MaxHistoryLines`.
pub const HISTORY_LINES: usize = 5000;
/// Output chunks waiting for the consumer before the reader waits: at most
/// 2 MiB with chunks of up to 256 KiB.
const PENDING_CHUNKS: usize = 8;

/// What a running attach reports, in order: output, then at most one finish.
/// Nothing follows a stop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttachEvent {
    /// At most 256 KiB of terminal output.
    Output { data: Vec<u8> },
    /// The attach client ended with this code (128 plus the signal for a
    /// signalled end).
    Finished { code: i32 },
}

/// `hgs [@host] a NAME --existing [--run-id RUN]`.
pub fn attach_args(target: &Target) -> Vec<String> {
    let mut args = Vec::new();
    if !target.host.is_empty() {
        args.push(format!("@{}", target.host));
    }
    args.extend(["a".to_owned(), target.name.clone(), "--existing".to_owned()]);
    if !target.run_id.is_empty() {
        args.extend(["--run-id".to_owned(), target.run_id.clone()]);
    }
    args
}

/// The attach client's environment: the client's own without tmux's
/// variables or an agent's attribution, plus `HGS_TAB=0` and a 256-colour,
/// true-colour terminal.
pub fn attach_env(
    base: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    const REMOVED: [&str; 7] = [
        "TMUX",
        "TMUX_PANE",
        "HGS_SESSION",
        "HGS_RUN_ID",
        "HGS_TAB",
        "TERM",
        "COLORTERM",
    ];
    let mut env: Vec<(OsString, OsString)> = crate::child_environment::restored(base)
        .into_iter()
        .filter(|(key, _)| !REMOVED.iter().any(|removed| key == removed))
        .collect();
    env.extend([
        ("HGS_TAB".into(), "0".into()),
        ("TERM".into(), "xterm-256color".into()),
        ("COLORTERM".into(), "truecolor".into()),
    ]);
    env
}

/// A running attach client in its own pseudoterminal (a ConPTY
/// pseudoconsole on Windows). Dropping it stops it.
pub struct Attach {
    #[cfg(unix)]
    pty: crate::pty::Pty,
    #[cfg(windows)]
    pty: crate::conpty::Pty,
}

impl Attach {
    /// Start the attach client for `target` at `cols` x `rows` (2-1000 by
    /// 2-500). Its events arrive on the receiver; when it is full the reader
    /// waits.
    pub fn start(
        target: &Target,
        cols: u16,
        rows: u16,
    ) -> Result<(Attach, Receiver<AttachEvent>), String> {
        let (sender, events) = sync_channel::<AttachEvent>(PENDING_CHUNKS);
        #[cfg(any(unix, windows))]
        {
            #[cfg(windows)]
            use crate::conpty::Pty;
            #[cfg(unix)]
            use crate::pty::Pty;
            let pty = Pty::start(
                std::path::Path::new(&target.hgs),
                &attach_args(target),
                &attach_env(std::env::vars_os()),
                cols,
                rows,
                sender,
            )?;
            Ok((Attach { pty }, events))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, cols, rows, sender, events);
            Err("Cannot open terminal: Terminal is not available on this platform.".into())
        }
    }

    /// Queue input; over 4 MiB pending it fails with Qt's text and queues
    /// nothing. Input after the end is ignored.
    pub fn write(&self, bytes: &[u8]) -> Result<(), String> {
        #[cfg(any(unix, windows))]
        return self.pty.write(bytes);
        #[cfg(not(any(unix, windows)))]
        {
            let _ = bytes;
            Ok(())
        }
    }

    /// Resize with the same clamps.
    pub fn resize(&self, cols: u16, rows: u16) {
        #[cfg(any(unix, windows))]
        self.pty.resize(cols, rows);
        #[cfg(not(any(unix, windows)))]
        let _ = (cols, rows);
    }

    /// The last 1 MiB of output, for a view shown again.
    pub fn replay(&self) -> Vec<u8> {
        #[cfg(any(unix, windows))]
        return self.pty.replay();
        #[cfg(not(any(unix, windows)))]
        Vec::new()
    }

    /// The attach client's process ID while it runs.
    pub fn process_id(&self) -> Option<u32> {
        #[cfg(any(unix, windows))]
        return self.pty.pid();
        #[cfg(not(any(unix, windows)))]
        None
    }

    /// Hang up the attach client and close its terminal. The session keeps
    /// running.
    pub fn stop(&mut self) {
        #[cfg(any(unix, windows))]
        self.pty.stop();
    }
}

/// The Terminal's status texts.
pub mod status {
    pub const IDLE: &str = "Connect to use the session's live terminal";

    pub fn connecting(name: &str) -> String {
        format!("Connecting to {name}…")
    }

    pub fn connected(name: &str) -> String {
        format!("Connected: {name}")
    }

    /// tmux keeps Unix sessions, the session host Windows ones.
    pub fn disconnected(machine: &str) -> String {
        if machine.is_empty() {
            "Disconnected — the session keeps running".into()
        } else {
            format!("Disconnected — the session continues on {machine}")
        }
    }

    /// The attach client ended: a clean exit is a disconnect.
    pub fn finished(code: i32, machine: &str) -> String {
        if code == 0 {
            disconnected(machine)
        } else {
            format!("Attach client exited ({code}) — session output is preserved above")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn facts(cmd: &str) -> TerminalFacts {
        TerminalFacts {
            cmd: cmd.into(),
            terminal_supported: cmd != "dsh" && cmd != "opencode",
            ..TerminalFacts::default()
        }
    }

    #[test]
    fn support_follows_the_native_capabilities() {
        let none = json!({});
        assert!(supported(&facts("claude"), &none));
        assert!(!supported(&facts("deepseek"), &none));
        assert!(!supported(
            &facts("dsh"),
            &json!({"terminal_supported": true})
        ));
        assert!(!supported(
            &facts("claude"),
            &json!({"terminal_supported": false})
        ));
        // OpenCode: the attach report first, then a reported terminal only.
        assert!(!supported(&facts("opencode"), &none));
        assert!(supported(
            &facts("opencode"),
            &json!({"terminal_attach_supported": true})
        ));
        let mut reported = facts("opencode");
        reported.terminal_attach_reported = true;
        reported.terminal_attach_supported = true;
        assert!(supported(&reported, &none));
        let mut terminal = facts("opencode");
        terminal.terminal_reported = true;
        terminal.terminal_supported = true;
        assert!(supported(&terminal, &none));
    }

    #[test]
    fn availability_gives_qt_reasons_in_order() {
        let none = json!({});
        let claude = facts("claude");
        assert_eq!(available(&claude, &none, "running", true, false), Ok(()));
        assert_eq!(
            available(&claude, &none, "running", false, true).unwrap_err(),
            "Machine unavailable. Reconnect when it is online."
        );
        assert_eq!(
            available(&claude, &none, "running", true, true).unwrap_err(),
            "Refresh the restored session before opening Terminal."
        );
        assert_eq!(
            available(&claude, &none, "paused", true, false).unwrap_err(),
            "Resume this session before opening its terminal."
        );
        assert_eq!(
            available(&facts("dsh"), &none, "running", true, false).unwrap_err(),
            "This native adapter does not provide Terminal."
        );
        assert_eq!(
            available(
                &facts("dsh"),
                &json!({"terminal_reason": "Use Native UI."}),
                "running",
                true,
                false
            )
            .unwrap_err(),
            "Use Native UI."
        );
        let mut opencode = facts("opencode");
        opencode.terminal_attach_reported = true;
        assert_eq!(
            available(&opencode, &none, "running", true, false).unwrap_err(),
            "Refresh this session after its OpenCode service reconnects."
        );
        opencode.terminal_attach_reason = "The service is offline.".into();
        assert_eq!(
            available(&opencode, &none, "running", true, false).unwrap_err(),
            "The service is offline."
        );
    }

    #[test]
    fn targets_reconnect_like_set_session() {
        let target = |host: &str, name: &str, run: &str| Target {
            hgs: "hgs".into(),
            host: host.into(),
            name: name.into(),
            run_id: run.into(),
        };
        let old = target("", "claude/p/a", "r1");
        assert!(!target_changed(&old, &target("", "claude/p/a", "r1")));
        // A rename of the same run keeps the connection.
        assert!(!target_changed(&old, &target("", "claude/p/renamed", "r1")));
        assert!(target_changed(&old, &target("studio", "claude/p/a", "r1")));
        assert!(target_changed(&old, &target("", "claude/p/a", "r2")));
        assert!(target_changed(
            &target("", "claude/p/a", ""),
            &target("", "claude/p/b", "")
        ));
        // An inspection that omits the run keeps the established one.
        assert!(!target_changed(&old, &target("", "claude/p/a", "")));
        assert_eq!(kept_run(&old, &target("", "claude/p/a", "")), "r1");
        assert_eq!(
            kept_run(
                &target("", "claude/p/a", ""),
                &target("", "claude/p/a", "r9")
            ),
            "r9"
        );
    }

    #[test]
    fn dropped_paths_are_quoted_for_the_shell() {
        let paths = vec![
            "/home/example/it's here.txt".to_owned(),
            "/home/example/b".to_owned(),
        ];
        assert_eq!(
            paste_paths(&paths, false).unwrap(),
            "'/home/example/it'\\''s here.txt' '/home/example/b' "
        );
        assert_eq!(
            paste_paths(&["C:/Users/example/it's.txt".to_owned()], true).unwrap(),
            "'C:\\Users\\example\\it''s.txt' "
        );
        assert_eq!(
            paste_paths(&["/home/example/a\nb".to_owned()], false).unwrap_err(),
            "File names containing control characters cannot be pasted into Terminal."
        );
    }

    #[test]
    fn powershell_paths_follow_qt_literals() {
        let cases = [
            ("plain.txt", "plain.txt "),
            (
                "C:/Users/example/plain.txt",
                "C:\\Users\\example\\plain.txt ",
            ),
            ("report's draft.pdf", "'report''s draft.pdf' "),
            ("a&b.txt", "'a&b.txt' "),
            ("cost $5's `q`.txt", "'cost $5''s `q`.txt' "),
            (
                "left\u{2018}$probe`n.txt",
                "'left\u{2018}\u{2018}$probe`n.txt' ",
            ),
            (
                "right\u{2019}$probe`n.txt",
                "'right\u{2019}\u{2019}$probe`n.txt' ",
            ),
            (
                "low\u{201a}$probe`n.txt",
                "'low\u{201a}\u{201a}$probe`n.txt' ",
            ),
            (
                "reversed\u{201b}$probe`n.txt",
                "'reversed\u{201b}\u{201b}$probe`n.txt' ",
            ),
            ("left\u{201c}quote.txt", "'left\u{201c}quote.txt' "),
            ("right\u{201d}quote.txt", "'right\u{201d}quote.txt' "),
            ("путь пример.txt", "'путь пример.txt' "),
            ("a,b.txt", "'a,b.txt' "),
            ("(group).txt", "'(group).txt' "),
            ("{brace}.txt", "'{brace}.txt' "),
            ("@sigil.txt", "'@sigil.txt' "),
            ("", "'' "),
        ];
        for (path, expected) in cases {
            assert_eq!(
                paste_paths(&[path.to_owned()], true).unwrap(),
                expected,
                "{path}"
            );
        }
    }

    #[test]
    fn the_attach_call_is_qt_s() {
        let mut target = Target {
            hgs: "hgs".into(),
            host: String::new(),
            name: "claude/sample-project/review".into(),
            run_id: String::new(),
        };
        assert_eq!(
            attach_args(&target),
            ["a", "claude/sample-project/review", "--existing"]
        );
        target.host = "studio".into();
        target.run_id = "run-1".into();
        assert_eq!(
            attach_args(&target),
            [
                "@studio",
                "a",
                "claude/sample-project/review",
                "--existing",
                "--run-id",
                "run-1"
            ]
        );
        let env = attach_env([
            ("PATH".into(), "/usr/bin".into()),
            ("TMUX".into(), "/tmp/tmux-1000/default,1,0".into()),
            ("TMUX_PANE".into(), "%1".into()),
            ("HGS_SESSION".into(), "claude/p/agent".into()),
            ("HGS_RUN_ID".into(), "run-0".into()),
            ("TERM".into(), "dumb".into()),
            ("HGS_TAB".into(), "3".into()),
        ]);
        let env: Vec<(String, String)> = env
            .into_iter()
            .map(|(k, v)| (k.into_string().unwrap(), v.into_string().unwrap()))
            .collect();
        assert_eq!(
            env,
            [
                ("PATH", "/usr/bin"),
                ("HGS_TAB", "0"),
                ("TERM", "xterm-256color"),
                ("COLORTERM", "truecolor")
            ]
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn attach_client_does_not_inherit_the_guis_private_webkit_stack() {
        let env = attach_env([
            ("PATH".into(), "/usr/bin".into()),
            (
                "LD_LIBRARY_PATH".into(),
                "/private/gtk:/private/webkit".into(),
            ),
            ("WEBKIT_EXEC_PATH".into(), "/private/processes".into()),
            (
                crate::child_environment::WEBKIT_MARKER.into(),
                r#"{"version":1,"LD_LIBRARY_PATH":null,"WEBKIT_EXEC_PATH":null}"#.into(),
            ),
        ]);
        assert!(!env.iter().any(|(key, _)| [
            "LD_LIBRARY_PATH",
            "WEBKIT_EXEC_PATH",
            crate::child_environment::WEBKIT_MARKER
        ]
        .iter()
        .any(|removed| key == removed)));
        assert!(env.contains(&("PATH".into(), "/usr/bin".into())));
        assert!(env.contains(&("TERM".into(), "xterm-256color".into())));
    }

    #[test]
    fn statuses_are_qt_texts() {
        assert_eq!(
            status::connecting("claude/p/a"),
            "Connecting to claude/p/a…"
        );
        assert_eq!(status::connected("claude/p/a"), "Connected: claude/p/a");
        assert_eq!(
            status::finished(0, ""),
            "Disconnected — the session keeps running"
        );
        assert_eq!(
            status::finished(0, "studio"),
            "Disconnected — the session continues on studio"
        );
        assert_eq!(
            status::finished(2, "studio"),
            "Attach client exited (2) — session output is preserved above"
        );
    }
}
