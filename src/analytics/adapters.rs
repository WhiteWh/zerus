//! Sanitizing producer adapters. No dashboard/inspection calls these functions.
//! Missing exact native identities reduce coverage; text never supplies proof.
use super::{gap, model::*, observe};
use serde_json::Value;
use std::path::Path;

fn field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
}
pub(super) fn clock(record: &Value) -> Option<Clock> {
    let run = field(record, "run_id")?;
    let process = field(record, "process_start")?;
    #[cfg(unix)]
    {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } != 0 || time.tv_sec < 0
        {
            return None;
        }
        Some(Clock {
            generation: Identity::new(&[run, process]),
            monotonic_ms: (time.tv_sec as u64)
                .checked_mul(1000)?
                .checked_add(time.tv_nsec as u64 / 1_000_000)?,
        })
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::WindowsProgramming::QueryUnbiasedInterruptTime;
        // Boot-relative interrupt ticks exclude sleep/hibernation. Resolution is
        // the system timer's, not 100 ns precision; never adjust the timer here.
        // The reducer still requires exact native boundaries and generation, and
        // rejects wall/monotonic drift above its existing five-second tolerance.
        let mut ticks = 0u64;
        if unsafe { QueryUnbiasedInterruptTime(&mut ticks) } == 0 {
            return None;
        }
        Some(Clock {
            generation: Identity::new(&[run, process]),
            monotonic_ms: ticks.checked_div(10_000)?,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (run, process);
        None
    }
}
fn source(record: &Value, child: &str) -> Option<(Identity, Provider, Producer)> {
    let provider = Provider::parse(field(record, "agent")?);
    let conversation = field(record, "conversation_id")?;
    Some((
        Identity::new(&[provider.name(), conversation, child]),
        provider,
        if child.is_empty() || child == "main" {
            Producer::Main
        } else {
            Producer::Child
        },
    ))
}

/// Called after the validated hook's ordinary Activity transaction commits.
/// The allowlist is deliberately narrower than Activity's presentation model.
pub(crate) fn hook(root: &Path, record: &Value, event: &Value) {
    let Some(hook) = field(event, "hook_event_name") else {
        return;
    };
    if !matches!(
        hook,
        "TurnStarted"
            | "UserPromptSubmit"
            | "Stop"
            | "Interrupt"
            | "PermissionRequest"
            | "PostToolUse"
            | "PermissionResult"
            | "QuestionAnswered"
            | "StopFailure"
    ) {
        return;
    }
    let child = field(event, "agent_id")
        .filter(|s| *s != "main")
        .unwrap_or("");
    let Some((scope, provider, producer)) = source(record, child) else {
        gap(root, Gap::Identity);
        return;
    };
    if matches!(
        provider,
        Provider::Other | Provider::Opencode | Provider::Dsh
    ) {
        gap(root, Gap::Provider);
        return;
    }
    // Kimi UserPromptSubmit can be queued. Only its TurnStarted establishes a turn.
    if provider == Provider::Kimi && hook == "UserPromptSubmit" {
        return;
    }
    // Codex/Claude lifecycle comes from the bounded typed native event reader.
    // Do not count a hook and its native record as two events.
    if matches!(provider, Provider::Codex | Provider::Claude)
        && matches!(
            hook,
            "UserPromptSubmit" | "TurnStarted" | "Stop" | "Interrupt" | "StopFailure"
        )
    {
        return;
    }
    let request = field(event, "tool_use_id").or_else(|| field(event, "tool_call_id"));
    let turn = field(event, "turn_id");
    let native = field(event, "event_id").or(turn);
    let (kind, id, correlation) = match hook {
        "TurnStarted" | "UserPromptSubmit" => (Kind::TurnStarted, native, turn),
        "Stop" => (Kind::TurnCompleted, native, turn),
        "Interrupt" => (Kind::TurnInterrupted, native, turn),
        "PermissionRequest" => (
            Kind::WaitStarted(if event["tool_name"] == "AskUserQuestion" {
                Interval::Input
            } else {
                Interval::Approval
            }),
            request,
            request,
        ),
        "PostToolUse" | "PermissionResult" => {
            (Kind::WaitResolved(Interval::Approval), request, request)
        }
        "QuestionAnswered" => (Kind::WaitResolved(Interval::Input), request, request),
        "StopFailure" => {
            let failure = match field(event, "error_type") {
                Some("authentication_error") => Failure::Authentication,
                Some("rate_limit_error") => Failure::RateLimit,
                Some("quota_exceeded") => Failure::Quota,
                Some("policy_error") => Failure::Policy,
                Some("network_error") => Failure::Network,
                Some("server_error" | "overloaded_error") => Failure::Service,
                _ => Failure::Unknown,
            };
            (Kind::Failure(failure), native, None)
        }
        _ => return,
    };
    let Some(id) = id else {
        gap(root, Gap::Identity);
        return;
    };
    let conversation = field(record, "conversation_id").unwrap();
    // Receipt time cannot be a replay-safe native timestamp: once its dedup
    // tombstone expires, a replay would otherwise look new. Unknown stays unknown.
    let observed_ms = event["timestamp"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.timestamp_millis())
        .or_else(|| {
            if hook == "QuestionAnswered" {
                // This exact producer stores its durable answer receipt timestamp in
                // seconds. Never substitute the current read/observation time.
                event["at"]
                    .as_f64()
                    .filter(|n| n.is_finite() && *n > 0.0 && *n < (i64::MAX as f64) / 1000.0)
                    .map(|n| (n * 1000.0) as i64)
            } else {
                None
            }
        });
    let Some(observed_ms) = observed_ms else {
        gap(root, Gap::Identity);
        return;
    };
    let semantic = match hook {
        "PostToolUse" | "PermissionResult" => "approval_resolved",
        _ => hook,
    };
    observe(
        root,
        &Event {
            id: Identity::new(&[provider.name(), conversation, child, semantic, id]),
            scope,
            provider,
            producer,
            correlation: correlation
                .map(|id| Identity::new(&[provider.name(), conversation, child, id])),
            observed_ms,
            // A replayed native hook retains its counts, not a new elapsed-time
            // observation. The original timestamp must still be live.
            clock: if (chrono::Utc::now().timestamp_millis() - observed_ms).abs() <= 2000 {
                clock(record)
            } else {
                None
            },
            kind,
        },
    );
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(crate) enum Delivery {
    Submitted,
    // Reserved for an authoritative failed receipt; pre-delivery errors do not prove one.
    #[allow(dead_code)]
    Failed,
    Uncertain,
}

/// Metadata only, carried out of the native keeper before any SQLite access.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum DeliveryNotice {
    IdentityGap,
    Event { request_id: String, agent: String, conversation: String, child: String, at: f64, outcome: Delivery },
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn describe_delivery(record: &Value, receipt: &Value, outcome: Delivery) -> Option<DeliveryNotice> {
    let request_id = field(receipt,"request_id")?;
    let child = field(receipt,"agent_id").unwrap_or("");
    if source(record,child).is_none() { return Some(DeliveryNotice::IdentityGap); }
    let at = receipt["submitted_at"].as_f64().filter(|n| n.is_finite() && *n > 0.0)?;
    Some(DeliveryNotice::Event {
        request_id:request_id.into(),agent:field(record,"agent")?.into(),
        conversation:field(record,"conversation_id")?.into(),child:child.into(),at,outcome,
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn emit_delivery_notice(root: &Path, notice: DeliveryNotice) {
    match notice {
        DeliveryNotice::IdentityGap => gap(root,Gap::Identity),
        DeliveryNotice::Event { request_id,agent,conversation,child,at,outcome } => delivery(root,
            &serde_json::json!({"agent":agent,"conversation_id":conversation}),
            &serde_json::json!({"request_id":request_id,"agent_id":child,"submitted_at":at}),outcome),
    }
}

pub(crate) fn delivery(root: &Path, record: &Value, receipt: &Value, outcome: Delivery) {
    let Some(id) = field(receipt, "request_id") else {
        return;
    };
    let Some((scope, provider, producer)) =
        source(record, field(receipt, "agent_id").unwrap_or(""))
    else {
        gap(root, Gap::Identity);
        return;
    };
    let Some(at) = receipt["submitted_at"]
        .as_f64()
        .filter(|n| n.is_finite() && *n > 0.0)
    else {
        return;
    };
    let (kind, label) = match outcome {
        Delivery::Submitted => (Kind::DeliverySubmitted, "submitted"),
        Delivery::Failed => (Kind::DeliveryFailed, "failed"),
        Delivery::Uncertain => (Kind::DeliveryUncertain, "uncertain"),
    };
    observe(
        root,
        &Event {
            id: Identity::new(&[provider.name(), id, label]),
            scope,
            provider,
            producer,
            correlation: None,
            observed_ms: (at * 1000.0) as i64,
            clock: None,
            kind,
        },
    );
}

/// Job state is the recovery worker's durable transition, not its reason text.
pub(crate) fn recovery(root: &Path, job: &Value) {
    let Some(id) = field(job, "id") else { return };
    let Some(state) = field(job, "state") else {
        return;
    };
    let kind = match state {
        "dispatching" => Kind::RecoveryAttempt,
        "succeeded" => Kind::RecoverySucceeded,
        "exhausted" => Kind::RecoveryExhausted,
        _ => return,
    };
    let Some(conversation) = job["identity"][1]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 256)
    else {
        return;
    };
    let provider = Provider::parse(field(job, "agent").unwrap_or(""));
    let attempt = job["attempt"].as_u64().unwrap_or(0).to_string();
    let Some(at) = job["updated_at"]
        .as_f64()
        .filter(|n| n.is_finite() && *n > 0.0)
    else {
        gap(root, Gap::Identity);
        return;
    };
    observe(
        root,
        &Event {
            id: Identity::new(&[id, &attempt, state]),
            scope: Identity::new(&[provider.name(), conversation]),
            provider,
            producer: Producer::Main,
            correlation: None,
            observed_ms: (at * 1000.0) as i64,
            clock: None,
            kind,
        },
    );
}
