//! Qt's bounded read-only service probe; commands are returned for explicit copy only.
use crate::actions::{Exit, Invocation};
use serde::Serialize;
use std::{collections::HashMap, path::Path, time::Duration};
#[derive(Default, Serialize)]
pub struct Service {
    pub status: String,
    pub implementation: String,
    pub protection: String,
    pub commands: Vec<(&'static str, String)>,
}
pub fn probe(program: &Path, args: Vec<String>, configured: bool, mac: bool) -> Service {
    let invocation = Invocation {
        args,
        stdin: None,
        timeout: Duration::from_secs(3),
        request_id: None,
    };
    let answer = crate::run::run_without(
        program,
        &invocation,
        &[],
        &super::super::CONNECTOR_UNSET,
        16384,
    )
    .ok();
    let mut view = match answer {
        Some(Exit::Code { code, stdout, .. }) => {
            parse(&String::from_utf8_lossy(&stdout), code, configured, mac)
        }
        _ => Service {
            status: format!(
                "Connector service: unknown ({} is unavailable)",
                if mac { "launchctl" } else { "systemctl" }
            ),
            ..Service::default()
        },
    };
    if !configured {
        view.status = format!(
            "Connector: not configured. Save a configuration before starting the service. {}",
            view.status
        );
    }
    view
}
fn parse(output: &str, code: i32, configured: bool, mac: bool) -> Service {
    let mut v = Service::default();
    if mac {
        v.status = if code != 0 {
            "Connector service: not installed. Follow the setup documentation."
        } else if output.lines().any(|l| l.trim() == "state = running") {
            "Connector service: running. Relay connectivity is not verified here."
        } else {
            "Connector service: loaded, not running"
        }
        .into();
        if let Some(program) = output
            .lines()
            .map(str::trim)
            .find_map(|l| l.strip_prefix("program = "))
        {
            if program.len() <= 4096 && program.ends_with("/zerus-mobile-connector") {
                v.implementation = format!("Implementation: Rust connector ({program}).");
            }
        }
        if code == 0 && configured {
            // SAFETY: geteuid has no preconditions; the command is copied, never run.
            #[cfg(unix)]
            let uid = unsafe { libc::geteuid() };
            #[cfg(not(unix))]
            let uid = 0;
            v.commands.push((
                "Copy restart command",
                format!("launchctl kickstart -k gui/{uid}/com.hgdev.zerus-mobile-connector"),
            ));
        }
        return v;
    }
    let fields: HashMap<_, _> = output.lines().filter_map(|l| l.split_once('=')).collect();
    let field = |key| fields.get(key).copied().unwrap_or("");
    v.status = match (field("LoadState"), field("ActiveState")) {
        ("not-found", _) => "Connector service: not installed. Follow the setup documentation.",
        ("loaded", "active") => {
            "Connector service: running. Relay connectivity is not verified here."
        }
        ("loaded", "inactive") => "Connector service: stopped",
        ("loaded", "failed") => "Connector service: failed. Review its private service logs.",
        ("loaded", _) => "Connector service: changing state or unknown",
        _ => "Connector service: unknown (user service manager unavailable)",
    }
    .into();
    if field("UnitFileState") == "disabled" {
        v.status
            .push_str(" It is not enabled, so it does not start at login.");
    }
    if configured {
        v.commands.push((
            "Copy start command",
            "systemctl --user start zerus-mobile-connector.service".into(),
        ));
    }
    if field("LoadState") == "loaded" {
        if field("KillMode") == "process" {
            v.protection="A restart stops only the connector process (KillMode=process). Native agents keep running.".into();
            if configured {
                v.commands.push((
                    "Copy restart command",
                    "systemctl --user restart zerus-mobile-connector.service".into(),
                ));
            }
        } else {
            let mode = match field("KillMode") {
                "control-group" => "control-group",
                "mixed" => "mixed",
                "none" => "none",
                _ => "unknown",
            };
            v.protection=format!("The service reports KillMode={mode}, not process: restarting it can also stop agents launched from the phone. Protect them first. This command adds a drop-in and reloads the user service manager without restarting anything; then choose Reload configuration.");
            v.commands.push(("Copy protection command","mkdir -p ~/.config/systemd/user/zerus-mobile-connector.service.d && printf '[Service]\\nKillMode=process\\n' > ~/.config/systemd/user/zerus-mobile-connector.service.d/50-zerus-killmode.conf && systemctl --user daemon-reload".into()));
        }
        if let Some(argv) = field("ExecStart").split("argv[]=").nth(1) {
            if let Some(program) = argv
                .split_whitespace()
                .next()
                .filter(|p| p.len() <= 4096 && p.ends_with("/zerus-mobile-connector"))
            {
                // A service command is not a credential source: never expose arbitrary argv.
                let python = Path::new(program)
                    .parent()
                    .and_then(Path::parent)
                    .is_some_and(|p| p.join("pyvenv.cfg").exists());
                v.implementation = if python {
                    format!("Implementation: legacy Python connector ({program}). The Rust connector replaces it: after phone actions drain and the service is stopped, zerus-setup --mobile-connector migrate switches the service without starting it (docs/mobile-deployment.md).")
                } else if field("DropInPaths").contains("/60-zerus-rust-connector.conf") {
                    format!("Implementation: Rust connector ({program}), switched by zerus-setup --mobile-connector migrate; rollback returns to the Python connector.")
                } else {
                    format!("Implementation: Rust connector ({program}).")
                };
            }
        }
    }
    v
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_requires_effective_process_kill_mode_and_configuration() {
        for mode in ["process", "control-group", "mixed", "", "untrusted"] {
            for configured in [true, false] {
                let v = parse(
                    &format!("LoadState=loaded\nActiveState=active\nKillMode={mode}"),
                    0,
                    configured,
                    false,
                );
                assert_eq!(
                    v.commands.iter().any(|(t, _)| *t == "Copy restart command"),
                    mode == "process" && configured
                );
                assert_eq!(
                    v.commands
                        .iter()
                        .any(|(t, _)| *t == "Copy protection command"),
                    mode != "process"
                );
            }
        }
    }
}
