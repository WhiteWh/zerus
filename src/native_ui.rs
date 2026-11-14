//! Browser handoff for the native harness. Remote loopback stays behind SSH;
//! launch tokens never enter the session catalog or application logs.
#[cfg(unix)]
use crate::platform;
use crate::{
    cli::{Error, Result},
    config::Config,
};
#[cfg(unix)]
use serde_json::{json, Value};
#[cfg(unix)]
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::{
    fs,
    net::TcpListener,
    os::unix::fs::DirBuilderExt,
    process::{Command, Stdio},
};

/// Another machine's UI needs a tunnel, which relies on OpenSSH connection
/// sharing (ControlMaster) that the Windows OpenSSH client does not provide.
/// This computer's own UI is already on its loopback.
#[cfg(not(unix))]
const UNSUPPORTED: &str =
    "The native DeepSeek UI of another machine is not available from Windows yet; open it on that machine.";
#[cfg(not(unix))]
pub fn address(config: &Config, target: &str, name: &str, _selector: Option<&str>) -> Result<String> {
    if !target.is_empty() && !config.is_self(target) {
        return Err(Error::new(2, UNSUPPORTED));
    }
    if !crate::platform::local_sessions() {
        return Err(Error::new(
            2,
            "The native DeepSeek UI needs local sessions on this computer; enable them in Machines.",
        ));
    }
    let url = crate::state::dsh::native_ui_url(name).map_err(|e| Error::new(1, e))?;
    let pattern =
        regex::Regex::new(r"^http://127\.0\.0\.1:[0-9]+/\?token=[A-Za-z0-9_%.-]+$").unwrap();
    if !pattern.is_match(&url) {
        return Err(Error::new(1, "Invalid native loopback URL"));
    }
    Ok(url)
}
#[cfg(not(unix))]
pub fn open(config: &Config, target: &str, name: &str, selector: Option<&str>, dry: bool) -> Result<i32> {
    if dry {
        println!("Open official DeepSeek UI for {name}");
        return Ok(0);
    }
    let url = address(config, target, name, selector)?;
    // Explorer hands the URL to the default browser; its exit code says nothing.
    std::process::Command::new("explorer.exe")
        .arg(&url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    Ok(0)
}

// Shared by the embedded view and external browser. The returned launch URL
// is private; callers must never put it in ordinary session state or logs.
// `selector` is a remote `--session-id`, which only a peer that lists the
// selector receives and resolves itself; `name` may then be empty.
#[cfg(unix)]
pub fn address(config: &Config, target: &str, name: &str, selector: Option<&str>) -> Result<String> {
    if crate::wsl::profile(config, target).is_some() {
        return Err(Error::new(2, "Open the native UI in the selected WSL environment; this transport does not create SSH tunnels"));
    }
    let remote = !target.is_empty() && !config.is_self(target);
    let mut url = if remote {
        if target.starts_with('-') || target.chars().any(char::is_whitespace) {
            return Err(Error::new(1, "Invalid SSH target"));
        }
        let mut words = vec!["__dsh-ui-url".to_owned()];
        if !name.is_empty() { words.push(name.to_owned()); }
        if let Some(id) = selector { words.extend(["--session-id".into(), id.into()]); }
        let command = match selector {
            Some(id) => {
                let mut args = vec!["__dsh-ui-url".to_owned()];
                if !name.is_empty() {
                    args.push(name.to_owned());
                }
                args.extend(["--session-id".to_owned(), id.to_owned()]);
                crate::cli::gated(&[crate::cli::Marker::Line(crate::cli::SELECTOR)], &args)
            }
            None => format!("~/.local/bin/hgs __dsh-ui-url {}", platform::remote_quote(name)),
        };
        let markers = if selector.is_some() { vec![(crate::cli::SELECTOR, true)] } else { vec![] };
        let command = crate::remote_shell::adapt(config, target, &words, &command, &markers, "Update the selected computer to select sessions by ID")?;
        let out = platform::ssh_output(crate::ssh_bytes::command(&command)?
            .args(crate::machines::ssh_options(config, target))
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=5",
                target,
                &command,
            ])
            )?;
        if !out.status.success() {
            return Err(Error::new(1, String::from_utf8_lossy(&out.stderr).trim()));
        }
        let response: Value = serde_json::from_slice(&out.stdout)
            .map_err(|_| Error::new(1, "Invalid native UI response"))?;
        response["url"]
            .as_str()
            .ok_or_else(|| Error::new(1, "Missing native UI address"))?
            .to_owned()
    } else {
        if selector.is_some() {
            return Err(Error::new(1, "A local session ID is resolved before the native UI"));
        }
        crate::state::dsh::native_ui_url(name).map_err(|e| Error::new(1, e))?
    };
    let pattern =
        regex::Regex::new(r"^http://127\.0\.0\.1:([0-9]+)/\?token=[A-Za-z0-9_%.-]+$").unwrap();
    let captures = pattern
        .captures(&url)
        .ok_or_else(|| Error::new(1, "Invalid native loopback URL"))?;
    let port = captures[1]
        .parse::<u16>()
        .map_err(|_| Error::new(1, "Invalid native UI port"))?;
    if remote {
        let dir = config.dir.join("native-tunnels");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        let key = format!(
            "{:x}",
            Sha256::digest(format!("{target}:{port}").as_bytes())
        );
        let control = dir.join(format!("{}.sock", &key[..16]));
        let port_file = dir.join(format!("{}.json", &key[..16]));
        let alive = platform::ssh()
            .arg("-S")
            .arg(&control)
            .args(["-O", "check", target])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success();
        let saved = fs::read(&port_file)
            .ok()
            .and_then(|v| serde_json::from_slice::<Value>(&v).ok())
            .and_then(|v| v["port"].as_u64())
            .filter(|n| *n > 0 && *n <= 65535);
        let local = if let Some(port) = saved.filter(|_| alive) {
            port as u16
        } else {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let local = listener.local_addr()?.port();
            drop(listener);
            let status = platform::ssh()
                .args(crate::machines::ssh_options(config, target))
                .args(["-M", "-S"])
                .arg(&control)
                .args([
                    "-fN",
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "ConnectTimeout=5",
                    "-o",
                    "ExitOnForwardFailure=yes",
                    "-o",
                    "ControlPersist=600",
                    "-L",
                ])
                .arg(format!("127.0.0.1:{local}:127.0.0.1:{port}"))
                .arg(target)
                .status()?;
            if !status.success() {
                return Err(Error::new(1, "Could not open the native UI SSH tunnel"));
            }
            fs::write(port_file, json!({"port":local}).to_string())?;
            local
        };
        url = url.replacen(&format!(":{port}/"), &format!(":{local}/"), 1);
    }
    Ok(url)
}

#[cfg(unix)]
pub fn open(config: &Config, target: &str, name: &str, selector: Option<&str>, dry: bool) -> Result<i32> {
    if dry {
        match selector {
            Some(id) => println!("Open official DeepSeek UI for session ID {id}"),
            None => println!("Open official DeepSeek UI for {name}"),
        }
        return Ok(0);
    }
    let url = address(config, target, name, selector)?;
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let status = Command::new(opener)
        .arg(&url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(Error::new(
            1,
            "Could not open the browser for the native DeepSeek UI",
        ));
    }
    Ok(0)
}
