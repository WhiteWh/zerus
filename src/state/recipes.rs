use super::*;
use std::process::{Command, Output};
use std::time::Duration;

pub(super) fn argv(record: &Value) -> Result<Vec<String>> {
    let values = record["base"]
        .as_array()
        .ok_or("saved launch arguments are missing")?;
    let result: Vec<String> = values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "invalid launch argument".into())
        })
        .collect::<Result<_>>()?;
    if result.first().map(|s| s.is_empty()).unwrap_or(true) {
        return Err("saved agent executable is missing".into());
    }
    Ok(result)
}

pub(super) fn resume_argv(agent: &str, argv: &[String], sid: &str) -> Result<Vec<String>> {
    let value_flags: &[&str] = match agent {
        "claude" => &[
            "--model",
            "--effort",
            "--agent",
            "--agents",
            "--append-system-prompt",
            "--system-prompt",
            "--settings",
            "--setting-sources",
            "--permission-mode",
            "--permission-prompt-tool",
            "--remote-control",
            "--mcp-config",
            "--max-budget-usd",
            "--fallback-model",
            "--debug-file",
            "--betas",
            "--system-prompt-snapshot",
        ],
        "codex" => &[
            "-c",
            "--config",
            "-m",
            "--model",
            "-p",
            "--profile",
            "-s",
            "--sandbox",
            "-a",
            "--ask-for-approval",
            "-C",
            "--cd",
            "--enable",
            "--disable",
            "--local-provider",
        ],
        "kimi" => &[
            "-m",
            "--model",
            "--agent",
            "--agent-file",
            "--skills-dir",
            "--add-dir",
        ],
        _ => return Err(format!("unsupported agent: {agent}")),
    };
    let multi_flags: &[&str] = match agent {
        "claude" => &[
            "--add-dir",
            "--allowedTools",
            "--allowed-tools",
            "--disallowedTools",
            "--disallowed-tools",
            "--tools",
            "--plugin-dir",
        ],
        "codex" => &["--add-dir"],
        _ => &[],
    };
    let bool_flags: &[&str] = match agent {
        "claude" => &[
            "--verbose",
            "--dangerously-skip-permissions",
            "--allow-dangerously-skip-permissions",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--ide",
            "--chrome",
            "--no-chrome",
        ],
        "codex" => &[
            "--no-daemon",
            "--no-alt-screen",
            "--oss",
            "--search",
            "--approve-for-me",
            "--dangerously-bypass-approvals-and-sandbox",
            "--dangerously-bypass-hook-trust",
            "--strict-config",
            "--include-non-interactive",
        ],
        "kimi" => &["-y", "--yolo", "--auto", "--plan"],
        _ => &[],
    };
    argv.first().ok_or("saved agent executable is missing")?;
    let program = crate::project_launcher::program_words(argv);
    let mut result = argv[..program].to_vec();
    result.extend([
        match agent {
            "codex" => "resume",
            "claude" => "--resume",
            _ => "--session",
        }
        .into(),
        sid.into(),
    ]);
    let mut index = program;
    while index < argv.len() {
        let argument = &argv[index];
        let key = argument.split('=').next().unwrap_or(argument);
        if argument == "--" {
            break;
        }
        if agent == "codex" && ["resume", "fork"].contains(&argument.as_str()) {
            index += 1;
            if index < argv.len() && !argv[index].starts_with('-') {
                index += 1;
            }
            continue;
        }
        if agent == "codex" && ["exec", "review", "app-server"].contains(&argument.as_str()) {
            return Err("only interactive Codex sessions support pause/resume".into());
        }
        if agent != "codex" && ["-r", "--resume", "-S", "--session", "--session-id"].contains(&key)
        {
            index += 1;
            if !argument.contains('=') && index < argv.len() && !argv[index].starts_with('-') {
                index += 1;
            }
            continue;
        }
        if (agent == "codex" && ["--last", "--all"].contains(&argument.as_str()))
            || (agent != "codex"
                && ["-c", "--continue", "--fork-session"].contains(&argument.as_str()))
        {
            index += 1;
            continue;
        }
        if value_flags.contains(&key) || multi_flags.contains(&key) {
            result.push(argument.clone());
            index += 1;
            if argument.contains('=') {
                continue;
            }
            if key == "--remote-control" && (index == argv.len() || argv[index].starts_with('-')) {
                continue;
            }
            if index == argv.len() {
                return Err(format!("missing value for {argument}"));
            }
            result.push(argv[index].clone());
            index += 1;
            if agent == "claude" && multi_flags.contains(&key) {
                while index < argv.len() && !argv[index].starts_with('-') {
                    result.push(argv[index].clone());
                    index += 1;
                }
            }
            continue;
        }
        if bool_flags.contains(&argument.as_str()) {
            result.push(argument.clone());
        } else if argument.starts_with('-') {
            return Err(format!(
                "cannot safely restore launch option {argument}; this option is not supported yet"
            ));
        }
        // Other positional arguments are the original prompt, never replayed.
        index += 1;
    }
    Ok(result)
}

/// The resume recipe keeps only safe launch options and never replays the
/// original prompt; without its selector it starts a new conversation.
pub(super) fn fresh_argv(agent: &str, argv: &[String]) -> Result<Vec<String>> {
    let mut argv = resume_argv(agent, argv, "-")?;
    let program = crate::project_launcher::program_words(&argv);
    argv.drain(program..program + 2);
    Ok(argv)
}

pub(super) fn output_timeout(command: &mut Command, duration: Duration) -> Result<Output> {
    // Regular files avoid a blocked pipe when an agent writes substantial
    // diagnostics before it exits, without an unbounded reader-thread join.
    crate::platform::output_until(command, duration).map_err(|error| match error {
        crate::platform::RunError::Spawn(e) | crate::platform::RunError::Io(e) => e.to_string(),
        _ => "timed out verifying agent history".into(),
    })
}

pub(super) fn verify_history(record: &Value, allow_error: bool) -> Result<()> {
    let name = string(record, "name");
    let conversation = string(record, "conversation_id");
    if conversation.is_empty() {
        return Err(format!(
            "{name}: conversation ID is not confirmed; check agent hooks"
        ));
    }
    if !allow_error && !string(record, "error").is_empty() {
        return Err(format!("{name}: {}", string(record, "error")));
    }
    if string(record, "agent") == "kimi" {
        let mut command = Command::new("kimi");
        command.args(["session", "list", "--all", "--json"]);
        if string(record, "agent_home").is_empty() {
            command.env_remove("KIMI_CODE_HOME");
        } else {
            command.env("KIMI_CODE_HOME", string(record, "agent_home"));
        }
        let output = output_timeout(&mut command, Duration::from_secs(10))?;
        if !output.status.success() {
            return Err(format!(
                "cannot verify Kimi history: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let sessions: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
        if !sessions
            .as_array()
            .ok_or("invalid Kimi session list")?
            .iter()
            .any(|session| string(session, "id") == conversation)
        {
            return Err(format!("{name}: saved Kimi conversation no longer exists"));
        }
    } else if !transcript_saved(record) {
        return Err(format!(
            "{name}: persisted conversation is missing or empty"
        ));
    }
    Ok(())
}

/// Whether the record's transcript (Claude's transcript, Codex's rollout) is
/// a non-empty regular file: one metadata read, never its content.
fn transcript_saved(record: &Value) -> bool {
    Path::new(string(record, "transcript"))
        .metadata()
        .map(|metadata| metadata.is_file() && metadata.len() > 0)
        .unwrap_or(false)
}

/// Whether a Claude or Codex archive has the conversation a restore resumes
/// (zerus-wyrqg): a session that exited before its first message reported a
/// conversation ID but wrote no transcript. A presentation gate; the
/// restore's own `verify_history` stays the authority. Other agents are not
/// read here (Kimi's check runs its CLI).
pub(super) fn restore_conversation_saved(record: &Value) -> bool {
    !matches!(string(record, "agent"), "claude" | "codex") || transcript_saved(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }
    #[test]
    fn powershell_wrappers_keep_the_prefix_across_resume_and_fresh() {
        let prefix = strings(&[r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe", "-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", r"C:\project\run_codex.ps1"]);
        for agent in ["codex", "claude", "kimi"] {
            let mut base = prefix.clone();
            base.extend(strings(&["--model", "model-name", "prompt must not replay"]));
            let resume = resume_argv(agent, &base, "same-id").unwrap();
            assert_eq!(&resume[..7], prefix.as_slice());
            assert_eq!(resume[8], "same-id");
            assert_eq!(&resume[9..], ["--model", "model-name"]);
            assert_eq!(fresh_argv(agent, &resume).unwrap(), [prefix.clone(), strings(&["--model", "model-name"])].concat());
        }
        let mut bad = prefix;bad.push("--unreviewed-option".into());
        assert!(resume_argv("codex", &bad, "id").is_err());
    }
    #[test]
    fn fresh_launches_keep_options_without_conversation_or_prompt() {
        assert_eq!(
            fresh_argv("codex", &strings(&["codex", "resume", "old", "-m", "gpt-6-astra", "prompt"])).unwrap(),
            strings(&["codex", "-m", "gpt-6-astra"])
        );
        assert_eq!(
            fresh_argv("claude", &strings(&["claude", "--resume", "old", "--effort", "high", "--fork-session"])).unwrap(),
            strings(&["claude", "--effort", "high"])
        );
        assert_eq!(
            fresh_argv("kimi", &strings(&["kimi", "--session", "old", "--plan"])).unwrap(),
            strings(&["kimi", "--plan"])
        );
        assert!(fresh_argv("codex", &strings(&["codex", "--unknown"])).is_err());
    }
}
