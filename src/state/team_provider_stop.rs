#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProviderStopKind {
    Quota,
    SessionLimit,
    Authentication,
    ProviderPolicy,
    ContextLimit,
    ModelUnavailable,
    Provider,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProviderStop {
    pub kind: ProviderStopKind,
    pub at_ms: i64,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProviderObservation {
    pub stop: Option<ProviderStop>,
    pub progress_ms: Option<i64>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix {
    use super::*;
    use crate::state::{recovery, storage, string};
    use serde_json::Value;
    use std::path::Path;
    use std::time::{Duration, Instant};

    // Pin the normal state writer while reading and committing a hand-back.
    // Never start a native process, open a terminal or run writer recovery here.
    pub(crate) struct ProviderWriter {
        _lock: storage::Guard,
    }
    impl ProviderWriter {
        pub(crate) fn take(state: &Path) -> Option<Self> {
            let deadline = Instant::now() + Duration::from_millis(25);
            // Match the configured-root resolution in the record reader.
            // Descendants, including the lock itself, remain no-follow.
            let state = crate::platform::canonical(state).ok()?;
            let lock = storage::existing_lock_until(&state.join(".lock"), deadline).ok()??;
            Some(Self { _lock: lock })
        }
    }

    pub(crate) fn saved_stop(record: &Value) -> Option<ProviderStop> {
        let error = &record["provider_error"];
        let run = string(record, "run_id");
        let at = error["at"].as_f64()?;
        if error["source"] != "provider_hook"
            || run.is_empty()
            || !same_timestamp(string(error, "message_id"), &format!("{run}:"), at)
        {
            return None;
        }
        classify(record, record["last_main_progress_at"].as_f64())
    }

    pub(crate) fn projected_stop(row: &Value, progress_ms: Option<i64>) -> Option<ProviderStop> {
        let error = &row["provider_error"];
        let source = string(error, "source");
        let id = string(error, "message_id");
        let ended = match (string(row, "agent"), source) {
            ("codex", "codex_native_transcript") => {
                error["native_turn_complete"] == true
                    && !string(error, "turn_id").is_empty()
                    && same_timestamp(
                        id,
                        &format!("codex-turn-error-{}-", string(error, "turn_id")),
                        error["at"].as_f64()?,
                    )
            }
            ("dsh", "dsh_native") => id
                .strip_prefix("dsh-error-")
                .is_some_and(|seq| seq.parse::<u64>().is_ok()),
            ("opencode", "opencode_native") => id.starts_with("opencode-error-msg_"),
            _ => false,
        };
        if !ended || row["process_state"] != "running" || row["runtime_state"] != "live" {
            return None;
        }
        let stop = classify(row, progress_ms.map(|t| t as f64 / 1000.0))?;
        if stop.kind == ProviderStopKind::Provider
            && recovery::same_active_failure(row, &row["recovery"])
        {
            return None;
        }
        Some(stop)
    }

    fn same_timestamp(id: &str, prefix: &str, at: f64) -> bool {
        id.len() <= 1024
            && id
                .strip_prefix(prefix)
                .and_then(|value| value.parse::<f64>().ok())
                .is_some_and(|written_at| (written_at - at).abs() < 0.000_5)
    }

    fn classify(record: &Value, progress: Option<f64>) -> Option<ProviderStop> {
        let error = &record["provider_error"];
        if record["phase"] != "error"
            || error["type"] != "StopFailure"
            || error["agent_id"] != ""
            || string(record, "conversation_id").is_empty()
        {
            return None;
        }
        let at = error["at"].as_f64()?;
        let at_ms = (at * 1000.0).round();
        if !at_ms.is_finite()
            || at_ms <= 0.0
            || at_ms >= i64::MAX as f64
            || progress.is_some_and(|progress| progress > at)
        {
            return None;
        }
        let kind = match string(error, "error_kind") {
            "quota" => ProviderStopKind::Quota,
            "session_limit" => ProviderStopKind::SessionLimit,
            "authentication" => ProviderStopKind::Authentication,
            "provider_policy" => ProviderStopKind::ProviderPolicy,
            "context_limit" => ProviderStopKind::ContextLimit,
            "model_unavailable" => ProviderStopKind::ModelUnavailable,
            "provider" if recovery::class(error) == "manual" => ProviderStopKind::Provider,
            _ => return None,
        };
        Some(ProviderStop {
            kind,
            at_ms: at_ms as i64,
        })
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use unix::{projected_stop, saved_stop, ProviderWriter};

pub(crate) fn binding_stamp(launch: &str, session: &str, run: &str, conversation: &str) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "{:x}",
        Sha256::digest(serde_json::json!([launch, session, run, conversation]).to_string())
    )
}
