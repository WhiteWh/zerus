//! Provider authorization reuse (phase 6): the honest support matrix.
//!
//! Each row names one provider and one execution mode and says whether Zerus
//! implements it, and if not, why. Nothing here reads, copies or forwards a
//! provider credential. Rows were checked against the installed native CLIs'
//! own help on 2026-11-26 (Claude Code 2.1.332, Codex CLI 0.172.0) and the
//! account adapters in `src/accounts/`; no authenticated model test was run,
//! so no row claims live qualification.
//!
//! Modes:
//! - `model-free-broker`: build/test jobs only; the broker holds no provider
//!   authority at all.
//! - `local-agent-remote-compute`: the agent and its sign-in stay on the
//!   workstation; the host supplies file and process tools through Zerus's
//!   mediation (`compute.rs`), and the agent's native file and shell tools are
//!   switched off so nothing runs on the wrong computer.
//! - `remote-native-subscription`: a native agent on the host reusing the
//!   workstation's subscription sign-in without another login.
//! - `remote-native-api`: a native agent on the host using an API account the
//!   person explicitly selected, through a least-privilege proxy.
use serde_json::{json, Value};

pub const MODES: [&str; 4] =
    ["model-free-broker", "local-agent-remote-compute", "remote-native-subscription", "remote-native-api"];

pub const PROVIDERS: [&str; 7] = ["claude", "codex", "kimi", "qwen", "hermes", "opencode", "dsh"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Implemented; not qualified against a live account (no authenticated
    /// model tests are run).
    ImplementedUnqualified,
    /// Holds no provider authority, so the provider is irrelevant.
    NotApplicable,
    Unsupported,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Self::ImplementedUnqualified => "implemented-unqualified",
            Self::NotApplicable => "no-provider-authority",
            Self::Unsupported => "unsupported",
        }
    }
    pub fn usable(self) -> bool {
        !matches!(self, Self::Unsupported)
    }
}

/// The status of `provider` in `mode`, with its reason.
pub fn support(provider: &str, mode: &str) -> (Status, &'static str) {
    use Status::*;
    match (mode, provider) {
        ("model-free-broker", _) => (NotApplicable, "jobs run credential-free; no sign-in is involved"),
        ("local-agent-remote-compute", "claude") => (
            ImplementedUnqualified,
            "Claude Code runs on the workstation with built-in tools off (--tools \"\") and only Zerus's remote file and shell tools (--strict-mcp-config)",
        ),
        ("local-agent-remote-compute", "codex") => (
            Unsupported,
            "Codex has no stable switch that removes its native shell and patch tools; its exec-server remote environment is experimental and unqualified",
        ),
        ("local-agent-remote-compute", _) => {
            (Unsupported, "no reviewed way to switch off this agent's native file and shell tools")
        }
        ("remote-native-subscription", "claude") => (
            Unsupported,
            "a subscription sign-in is not portable; no verified non-persisted lease route exists, and copying it would add a second refresh writer",
        ),
        ("remote-native-subscription", "codex") => (
            Unsupported,
            "a ChatGPT sign-in is a rotating refresh grant in auth.json; copying it would add a second refresh writer",
        ),
        ("remote-native-subscription", _) => {
            (Unsupported, "no verified native lease route; Zerus never copies rotating grants or asks for a remote sign-in")
        }
        ("remote-native-api", "claude" | "codex" | "qwen") => (
            Unsupported,
            "the adapter accepts a base URL, but the least-privilege loopback proxy is not implemented; Zerus never substitutes an API account by itself",
        ),
        ("remote-native-api", _) => (Unsupported, "no reviewed base URL and API key route for this adapter"),
        _ => (Unsupported, "unknown mode"),
    }
}

/// The whole matrix, for `hgs host-access support --json` and the desktop.
pub fn matrix() -> Value {
    let rows: Vec<Value> = PROVIDERS
        .iter()
        .flat_map(|provider| {
            MODES.iter().map(move |mode| {
                let (status, reason) = support(provider, mode);
                json!({"provider":provider,"mode":mode,"status":status.name(),"usable":status.usable(),"reason":reason})
            })
        })
        .collect();
    json!({"schema":"zerus-provider-support-v1","checked":"2026-11-26","live_qualified":false,"rows":rows})
}

/// Environment names that can carry provider or Zerus authority. A mediated
/// process and every Zerus argv must be free of them.
pub fn secret_env(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    const EXACT: &[&str] = &[
        "CLAUDE_CONFIG_DIR", "CODEX_HOME", "KIMI_CODE_HOME", "QWEN_HOME", "HERMES_HOME", "SSH_AUTH_SOCK",
        "GH_TOKEN", "GITHUB_TOKEN", "AWS_PROFILE", "GOOGLE_APPLICATION_CREDENTIALS",
    ];
    const PARTS: &[&str] = &["TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL", "API_KEY", "APIKEY", "AUTH", "COOKIE", "PRIVATE_KEY"];
    EXACT.contains(&upper.as_str())
        || PARTS.iter().any(|part| upper.contains(part))
        || upper.starts_with("ANTHROPIC_")
        || upper.starts_with("OPENAI_")
        || upper.starts_with("HGS_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_provider_and_mode_has_a_row_and_unsupported_rows_stay_visible() {
        let matrix = matrix();
        let rows = matrix["rows"].as_array().unwrap();
        assert_eq!(rows.len(), PROVIDERS.len() * MODES.len());
        for row in rows {
            assert!(!row["reason"].as_str().unwrap().is_empty());
        }
        assert_eq!(matrix["live_qualified"], false);
    }

    #[test]
    fn no_remote_native_mode_is_claimed_supported() {
        for provider in PROVIDERS {
            for mode in ["remote-native-subscription", "remote-native-api"] {
                assert_eq!(support(provider, mode).0, Status::Unsupported, "{provider} {mode}");
            }
        }
        assert_eq!(support("claude", "local-agent-remote-compute").0, Status::ImplementedUnqualified);
        assert_eq!(support("codex", "local-agent-remote-compute").0, Status::Unsupported);
    }

    #[test]
    fn secret_environment_names_are_recognized() {
        for name in ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN", "OPENAI_API_KEY", "CODEX_HOME", "HGS_RUN_ID", "my_secret", "SSH_AUTH_SOCK"] {
            assert!(secret_env(name), "{name}");
        }
        for name in ["PATH", "HOME", "LANG", "TEMP", "SystemRoot"] {
            assert!(!secret_env(name), "{name}");
        }
    }
}
