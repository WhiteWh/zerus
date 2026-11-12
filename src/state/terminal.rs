//! Explicit raw Terminal, pinned to the tracked pane/process and short deadline.
//! Read frames never resize the desktop pane. Native input UUIDs never replay.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_SCREEN: usize = 128 * 1024;
const KEYS: &[&str] = &[
    "Enter", "Escape", "Tab", "BTab", "BSpace", "Up", "Down", "Left", "Right", "Home", "End",
    "PPage", "NPage", "DC", "C-c", "C-d", "C-l", "C-a", "C-e", "C-u", "C-w",
];
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    request_id: String,
    action: String,
    expected_run_id: String,
    expected_conversation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_binding_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enter: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<f64>,
}
fn validate(request: &Request) -> Result<()> {
    let id =
        uuid::Uuid::parse_str(&request.request_id).map_err(|_| "Invalid terminal request UUID")?;
    if id.is_nil()
        || id.to_string() != request.request_id
        || request.expected_run_id.is_empty()
        || request.expected_run_id.len() > 128
        || request.expected_conversation_id.len() > 256
        || request.expected_run_id.chars().any(char::is_control)
        || request
            .expected_conversation_id
            .chars()
            .any(char::is_control)
    {
        return Err("Invalid terminal identity".into());
    }
    match request.action.as_str() {
        "snapshot" | "composer"
            if request.terminal_binding_id.is_none()
                && request.text.is_none()
                && request.enter.is_none()
                && request.key.is_none()
                && request.expires_at.is_none() =>
        {
            Ok(())
        }
        "input" => {
            let binding = request
                .terminal_binding_id
                .as_deref()
                .ok_or("Terminal input needs a verified binding")?;
            if binding.len() != 64
                || !binding
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            {
                return Err("Invalid terminal binding".into());
            }
            let expires = request
                .expires_at
                .ok_or("Terminal input needs a relay deadline")?;
            if !expires.is_finite() || expires <= 0.0 {
                return Err("Terminal input expired or has an invalid deadline".into());
            }
            if let Some(key) = &request.key {
                if request.text.is_some()
                    || request.enter.is_some()
                    || !KEYS.contains(&key.as_str())
                {
                    return Err("Invalid terminal key".into());
                }
            } else {
                let text = request
                    .text
                    .as_deref()
                    .ok_or("Choose terminal text or a key")?;
                if request.enter.is_none()
                    || text.len() > 64 * 1024
                    || (text.is_empty() && request.enter != Some(true))
                    || text
                        .chars()
                        .any(|c| c.is_control() && c != '\n' && c != '\t')
                {
                    return Err("Invalid literal terminal text".into());
                }
            }
            Ok(())
        }
        // zerus-wkj9l: the exact composer text the person confirmed to discard.
        "clear-input" => {
            let binding = request.terminal_binding_id.as_deref().ok_or("Clear input needs a verified binding")?;
            if binding.len() != 64 || !binding.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) {
                return Err("Invalid terminal binding".into());
            }
            let expires = request.expires_at.ok_or("Clear input needs a relay deadline")?;
            if !expires.is_finite() || expires <= 0.0 {
                return Err("Clear input expired or has an invalid deadline".into());
            }
            let text = request.text.as_deref().ok_or("Clear input needs the confirmed text")?;
            if request.key.is_some() || request.enter.is_some() || text.trim().is_empty()
                || text.chars().count() > input::settle::RESIDUE_CHARS
            {
                return Err("Invalid clear input request".into());
            }
            Ok(())
        }
        _ => Err("Unsupported terminal action".into()),
    }
}
fn checked(name: &str, request: &Request) -> Result<Value> {
    // zerus-ucck P3c: the request's identities select among sessions that
    // share the label.
    expect(name, &request.expected_run_id, &request.expected_conversation_id)?;
    if opencode::exists(name) {return Err(backend::OPENCODE_TERMINAL.text().into());}
    if dsh::exists(name) {
        return Err(backend::DEEPSEEK_TERMINAL.text().into());
    }
    let record = read(name)?;
    if string(&record, "run_id") != request.expected_run_id
        || string(&record, "conversation_id") != request.expected_conversation_id
        || !string(&record, "archive_id").is_empty()
        || !process_alive(&record)
        || !matches(&record, panes_of(&live()?, &record))
    {
        return Err("Terminal no longer belongs to this exact live session".into());
    }
    Ok(record)
}
fn frame(record: &Value) -> Result<Value> {
    let pane = string(record, "pane");
    if !pane
        .strip_prefix('%')
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|c| c.is_ascii_digit()))
    {
        return Err("Invalid tracked pane".into());
    }
    use backend::PaneField::*;
    let info = backend::current().pane_info(
        backend::Target::Pane(pane),
        &[
            SessionId,
            PaneId,
            ServerPid,
            PanePid,
            Width,
            Height,
            CursorX,
            CursorY,
            InMode,
            Dead,
            RunMarker,
            SessionName,
            BracketedPaste,
            Tty,
        ],
    )?;
    let fields = info.fields();
    if fields.len() != 14
        || fields[1] != pane
        || fields[9] != "0"
        || fields[10] != string(record, "run_id")
        || fields[11] != terminal_name(record)
        || !fields[0]
            .strip_prefix('$')
            .is_some_and(|id| !id.is_empty() && id.bytes().all(|c| c.is_ascii_digit()))
    {
        return Err("Terminal pane identity changed".into());
    }
    let numeric = |index: usize| {
        fields[index]
            .parse::<u32>()
            .map_err(|_| "Invalid terminal dimensions or process identity".to_string())
    };
    let (server, pane_pid, cols, rows, x, y) = (
        numeric(2)?,
        numeric(3)?,
        numeric(4)?,
        numeric(5)?,
        numeric(6)?,
        numeric(7)?,
    );
    if cols == 0
        || rows == 0
        || cols > 1000
        || rows > 1000
        || x >= cols
        || y >= rows
        || server == 0
        || pane_pid == 0
    {
        return Err("Unsupported terminal geometry".into());
    }
    let (server_start, pane_start) = (process_start(server), process_start(pane_pid));
    if server_start.is_empty() || pane_start.is_empty() {
        return Err("Terminal process birth identity is unavailable".into());
    }
    let pid = record["pid"]
        .as_u64()
        .ok_or("Missing tracked process")?
        .to_string();
    if !backend::current().on_terminal(&pid, fields[13])? {
        return Err("Tracked process is no longer attached to this Terminal".into());
    }
    let binding = format!(
        "{:x}",
        Sha256::digest(
            json!([
                record["run_id"],
                record["conversation_id"],
                pane,
                fields[0],
                server,
                server_start,
                pane_pid,
                pane_start,
                record["pid"],
                record["process_start"]
            ])
            .to_string()
        )
    );
    Ok(
        json!({"terminal_binding_id":binding,"cols":cols,"rows":rows,"cursor":{"x":x,"y":y},"input_mode":fields[8]=="0","bracket_paste":fields[12]=="1"}),
    )
}
fn identity(request: &Request, name: &str, status: &str) -> Value {
    json!({"request_id":request.request_id,"name":name,"run_id":request.expected_run_id,"conversation_id":request.expected_conversation_id,"status":status})
}
fn check_handoff(name: &str, request: &Request) -> Result<Value> {
    if request.expires_at.unwrap_or(0.0) <= now() || request.expires_at.unwrap_or(0.0) > now() + 6.0
    {
        return Err("Terminal input expired before native handoff".into());
    }
    let record = checked(name, request)?;
    let metadata = frame(&record)?;
    if metadata["terminal_binding_id"].as_str() != request.terminal_binding_id.as_deref() {
        return Err("Terminal binding changed before input".into());
    }
    if metadata["input_mode"] != true {
        return Err("Exit tmux copy mode before sending terminal input".into());
    }
    if request
        .text
        .as_ref()
        .is_some_and(|text| text.contains('\n'))
        && metadata["bracket_paste"] != true
    {
        return Err("Multiline terminal text requires verified bracketed paste mode".into());
    }
    Ok(record)
}
fn claim(record: &Value) -> backend::InputClaim<'_> {
    backend::InputClaim {
        pane: string(record, "pane"),
        name: terminal_name(record),
        run_id: string(record, "run_id"),
        agent: string(record, "agent"),
        pid: record["pid"].as_u64(),
    }
}

/// The composer in one stable frame (the same cursor around the capture).
fn composer_now(terminal: &dyn backend::TerminalBackend, record: &Value) -> Option<input::composer::Composer> {
    let (screen, x, y) = input::settle::frame(terminal, &claim(record))?;
    Some(input::composer::read(record, &screen, x, y))
}

/// The Terminal tab's read for Clear input (zerus-wkj9l): the composer's text
/// and whether the person may clear it now, with the reason when not.
fn composer_state(record: &Value, metadata: &Value) -> Value {
    use input::composer::Composer;
    let composer = composer_now(backend::current(), record);
    let (state, text) = match &composer {
        Some(Composer::Empty) => ("empty", String::new()),
        Some(Composer::Text(text)) => ("text", text.clone()),
        _ => ("unknown", String::new()),
    };
    let reason = if !input::composer::supported(string(record, "agent")) {
        "Clear input is not available for this agent"
    } else if metadata["input_mode"] != true {
        "Exit tmux copy mode first"
    } else if string(record, "activity") == "busy" {
        "Wait until the agent is idle"
    } else if state == "unknown" {
        "The input is not recognized here; clear it in Terminal"
    } else if state == "empty" {
        "The input is empty"
    } else {
        ""
    };
    json!({"state": state, "text": text, "clearable": reason.is_empty(), "reason": reason})
}

/// Clears exactly the confirmed composer text (zerus-wkj9l): an explicit person
/// action, never automatic and never used by a delivery retry. End and Ctrl+U
/// clear a line in Claude Code, Codex and Kimi and do nothing on an empty
/// composer (Ctrl+C would quit Codex there), and each round is re-read; it
/// stops as soon as the composer is empty or stops changing.
fn clear(name: &str, request: &Request, path: &Path, hash: &str) -> Result<Value> {
    use input::composer::Composer;
    let _guard = lock(None)?;
    let record = check_handoff(name, request)?;
    if string(&record, "activity") == "busy" {
        return Err("The agent is busy; clear its input once it is idle".into());
    }
    let expected = request.text.clone().unwrap_or_default();
    let terminal = backend::current();
    match composer_now(terminal, &record) {
        Some(Composer::Text(text)) if text == expected => {}
        Some(Composer::Text(_)) => return Err("The input changed; review it again before clearing".into()),
        Some(Composer::Empty) => return Err("The input is already empty".into()),
        _ => return Err("The input is not recognized here; clear it in Terminal".into()),
    }
    let mut answer = identity(request, name, "claimed");
    answer["request_hash"] = json!(hash);
    answer["terminal_binding_id"] = json!(request.terminal_binding_id);
    atomic(path, &answer.to_string())?;
    let pane = string(&record, "pane");
    let rounds = (expected.lines().count() + 2).min(12);
    let mut shown = expected.clone();
    let outcome: std::result::Result<usize, (String, Option<String>)> = (|| {
        for round in 1..=rounds {
            check_handoff(name, request).map_err(|error| (error, Some(shown.clone())))?;
            terminal
                .send(pane, &[backend::Key::Named("End"), backend::Key::Named("C-u")])
                .map_err(|error| (error, Some(shown.clone())))?;
            std::thread::sleep(std::time::Duration::from_millis(150));
            match composer_now(terminal, &record) {
                Some(Composer::Empty) => return Ok(round),
                Some(Composer::Text(text)) if text != shown => shown = text,
                Some(Composer::Text(text)) => return Err(("The input did not change after clearing".into(), Some(text))),
                _ => return Err(("The input could not be read after clearing".into(), None)),
            }
        }
        Err(("The input still holds text".into(), Some(shown.clone())))
    })();
    match outcome {
        Ok(rounds) => {
            answer["status"] = json!("cleared");
            answer["discarded"] = json!(expected);
            answer["rounds"] = json!(rounds);
            // The person's action, kept with the session: it also settles an
            // unconfirmed input whose residue this was (input::input_unconfirmed).
            if let Ok(mut current) = read(name) {
                if string(&current, "run_id") == string(&record, "run_id") {
                    current["input_cleared_at"] = json!(now());
                    current["input_cleared"] = json!({"request_id": request.request_id, "chars": expected.chars().count(),
                        "sha256": format!("{:x}", Sha256::digest(expected.as_bytes()))});
                    let _ = write(&mut current);
                }
            }
        }
        Err((error, residue)) => {
            answer["status"] = json!("uncertain");
            answer["error"] = json!(error);
            answer["composer"] = json!(residue);
        }
    }
    atomic(path, &answer.to_string())?;
    Ok(answer)
}

fn input(name: &str, request: &Request, path: &Path, hash: &str) -> Result<Value> {
    let _guard = lock(None)?;
    let record = check_handoff(name, request)?;
    let mut answer = identity(request, name, "claimed");
    answer["request_hash"] = json!(hash);
    answer["terminal_binding_id"] = json!(request.terminal_binding_id);
    let buffer = format!("hgs-terminal-{}", request.request_id);
    let staged = request.text.as_ref().is_some_and(|text| !text.is_empty());
    let terminal = backend::current();
    let cleanup = || {
        if staged {
            terminal.discard(&buffer);
        }
    };
    if staged {
        let stage: Result<()> = (|| {
            if !terminal.stage(
                &buffer,
                request.text.as_ref().unwrap().as_bytes(),
                backend::StageErrors::Discard,
            )? {
                return Err("Could not stage terminal text; nothing was delivered".into());
            }
            Ok(())
        })();
        if let Err(error) = stage {
            cleanup();
            return Err(error);
        }
    }
    if let Err(error) = check_handoff(name, request) {
        cleanup();
        return Err(error);
    }
    if let Err(error) = atomic(path, &answer.to_string()) {
        cleanup();
        return Err(error);
    }
    let result: Result<()> = (|| {
        let pane = string(&record, "pane");
        if let Some(key) = &request.key {
            terminal.send(pane, &[backend::Key::Named(key)])?;
        } else {
            if staged {
                // Use literal stdin buffers even for a single line; terminal
                // text must not appear in child-process command arguments.
                let bracketed = frame(&record)?["bracket_paste"] == true;
                terminal.paste(&buffer, pane, backend::Paste::Terminal { bracketed })?;
            }
            if request.enter == Some(true) {
                if staged {
                    std::thread::sleep(std::time::Duration::from_millis(180));
                }
                check_handoff(name, request)?;
                terminal.send(pane, &[backend::Key::Named("Enter")])?;
            }
        }
        Ok(())
    })();
    cleanup();
    answer["status"] = json!(if result.is_ok() {
        "submitted"
    } else {
        "uncertain"
    });
    if let Err(error) = result {
        answer["error"] = json!(error);
    }
    atomic(path, &answer.to_string())?;
    Ok(answer)
}
pub(super) fn dispatch(args: &[String]) -> Result<i32> {
    if args.len() != 2 || args[1] != "--json" {
        return Err("usage: hgs terminal <session> --json".into());
    }
    let mut bytes = Vec::new();
    io::stdin()
        .take(512 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 512 * 1024 {
        return Err("Terminal request exceeds size limit".into());
    }
    let request: Request = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    validate(&request)?;
    let name = &args[0];
    let mut answer = if matches!(request.action.as_str(), "snapshot" | "composer") {
        let _guard = lock(None)?;
        let record = checked(name, &request)?;
        let metadata = frame(&record)?;
        let output =
            backend::current().capture(string(&record, "pane"), backend::Capture::Styled)?;
        let screen = String::from_utf8_lossy(&output);
        let truncated = screen.len() > MAX_SCREEN;
        let mut boundary = screen.len().min(MAX_SCREEN);
        while !screen.is_char_boundary(boundary) {
            boundary -= 1;
        }
        let mut answer = identity(&request, name, "snapshot");
        answer
            .as_object_mut()
            .unwrap()
            .extend(metadata.as_object().unwrap().clone());
        if frame(&checked(name, &request)?)?["terminal_binding_id"]
            != metadata["terminal_binding_id"]
        {
            return Err("Terminal changed during screen capture".into());
        }
        if request.action == "composer" {
            let mut answer = identity(&request, name, "composer");
            answer["terminal_binding_id"] = metadata["terminal_binding_id"].clone();
            answer["composer"] = composer_state(&record, &metadata);
            if frame(&checked(name, &request)?)?["terminal_binding_id"] != metadata["terminal_binding_id"] {
                return Err("Terminal changed during the composer read".into());
            }
            println!("{answer}");
            return Ok(0);
        }
        answer["input_supported"] = metadata["input_mode"].clone();
        answer["text_supported"] = metadata["input_mode"].clone();
        answer["multiline_supported"] =
            json!(metadata["input_mode"] == true && metadata["bracket_paste"] == true);
        answer["input_reason"] = json!(if metadata["input_mode"] == true {
            ""
        } else {
            "Exit tmux copy mode before input"
        });
        answer["screen"] = json!(&screen[..boundary]);
        answer["truncated"] = json!(truncated);
        answer["captured_at"] = json!(now());
        answer
    } else {
        let directory = absolute_root()?.join("terminal_receipts");
        let path = directory.join(format!("{}.json", request.request_id));
        let _claim = lock(Some(
            &directory.join(format!("{}.lock", request.request_id)),
        ))?;
        let hash = format!("{:x}", Sha256::digest(json!([name, request]).to_string()));
        if path.exists() {
            let mut receipt: Value =
                serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            if receipt["request_hash"] != hash {
                return Err("Terminal UUID already used for another input".into());
            }
            if receipt["status"] == "claimed" {
                receipt["status"] = json!("uncertain");
                receipt["error"] =
                    json!("Earlier terminal input was interrupted; it was not replayed");
                atomic(&path, &receipt.to_string())?;
            }
            receipt
        } else {
            let attempt = if request.action == "clear-input" {
                clear(name, &request, &path, &hash)
            } else {
                input(name, &request, &path, &hash)
            };
            match attempt {
                Ok(answer) => answer,
                Err(error) => {
                    let status = if path.exists() { "uncertain" } else { "failed" };
                    let mut answer = identity(&request, name, status);
                    answer["request_hash"] = json!(hash);
                    answer["terminal_binding_id"] = json!(request.terminal_binding_id);
                    answer["error"] = json!(error);
                    atomic(&path, &answer.to_string())?;
                    answer
                }
            }
        }
    };
    answer.as_object_mut().unwrap().remove("request_hash");
    println!("{answer}");
    Ok(0)
}
