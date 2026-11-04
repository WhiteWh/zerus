//! Static failure labels only. Helper text and reply data never enter a log.
use zerus_client_core::run::OwnedError;

#[derive(Debug, PartialEq)]
pub(super) enum Reason {
    Preparation,
    Selection,
    Runner(&'static str),
    OutputLimit,
    ReplyDecode,
    ReplyIdentity,
    Refused(Refusal),
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    ForeignContext,
    WorkerJoin,
}

#[derive(Debug, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Refusal {
    FrontendAuthenticationRefused,
    FrontendIdentityExpired,
    ActivationReceiverUnavailable,
}

impl Reason {
    pub(super) fn runner(error: &OwnedError, stderr: Option<&str>) -> Self {
        if *error == OwnedError::ChildFailed {
            let known = match stderr.map(str::trim) {
                Some("hgs: frontend process image unavailable") => {
                    Some("frontend_process_image_unavailable")
                }
                Some("hgs: frontend process identity unavailable") => {
                    Some("frontend_process_identity_unavailable")
                }
                Some("hgs: agent-descended frontend request refused") => {
                    Some("agent_descended_frontend_refused")
                }
                Some("hgs: canonical frontend evidence unavailable") => {
                    Some("canonical_frontend_evidence_unavailable")
                }
                _ => None,
            };
            if let Some(code) = known {
                return Self::Runner(code);
            }
        }
        Self::Runner(match error {
            OwnedError::InvalidInput => "invalid_input",
            OwnedError::Cancelled => "cancelled",
            OwnedError::TimedOut => "timed_out",
            OwnedError::StartFailed => "start_failed",
            OwnedError::WriteFailed => "write_failed",
            OwnedError::ChildFailed => "child_failed",
            OwnedError::OutputTooLarge => "output_too_large",
            OwnedError::Incomplete => "incomplete",
            OwnedError::ExitUnverified => "exit_unverified",
        })
    }

    pub(super) fn reply(bytes: &[u8], expected_id: &str) -> Self {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Refused {
            request_id: String,
            accepted: bool,
            durable: bool,
            code: Refusal,
        }
        if bytes.len() <= zerus_watch_protocol::CONTROL_LINE_BYTES {
            if let Ok(reply) = serde_json::from_slice::<Refused>(bytes) {
                if reply.request_id == expected_id && !reply.accepted && !reply.durable {
                    return Self::Refused(reply.code);
                }
            }
        }
        Self::ReplyDecode
    }

    pub(super) fn code(&self) -> &'static str {
        match self {
            Self::Preparation => "preparation",
            Self::Selection => "relay_selection",
            Self::Runner(code) => code,
            Self::OutputLimit => "reply_output_limit",
            Self::ReplyDecode => "reply_decode",
            Self::ReplyIdentity => "reply_identity",
            Self::Refused(Refusal::FrontendAuthenticationRefused) => {
                "frontend_authentication_refused"
            }
            Self::Refused(Refusal::FrontendIdentityExpired) => "frontend_identity_expired",
            Self::Refused(Refusal::ActivationReceiverUnavailable) => {
                "activation_receiver_unavailable"
            }
            Self::ForeignContext => "foreign_platform_context",
            Self::WorkerJoin => "worker_join",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_text_cannot_become_a_diagnostic() {
        for error in [
            OwnedError::InvalidInput,
            OwnedError::Cancelled,
            OwnedError::TimedOut,
            OwnedError::StartFailed,
            OwnedError::WriteFailed,
            OwnedError::ChildFailed,
            OwnedError::OutputTooLarge,
            OwnedError::Incomplete,
            OwnedError::ExitUnverified,
        ] {
            let ordinary = Reason::runner(&error, None);
            for text in [
                "private fixture text",
                "hgs: frontend process image unavailable\nprivate fixture text",
            ] {
                assert_eq!(Reason::runner(&error, Some(text)), ordinary);
            }
        }
        assert_eq!(
            Reason::runner(
                &OwnedError::ChildFailed,
                Some("hgs: frontend process image unavailable\n")
            )
            .code(),
            "frontend_process_image_unavailable"
        );
        assert_eq!(
            Reason::runner(
                &OwnedError::TimedOut,
                Some("hgs: frontend process image unavailable")
            )
            .code(),
            "timed_out"
        );
    }

    #[test]
    fn only_closed_matching_refusals_select_static_labels() {
        let id = "11111111-1111-4111-8111-111111111111";
        let mut value = serde_json::json!({"request_id":id,"accepted":false,"durable":false,
            "code":"frontend_authentication_refused"});
        let classify =
            |value: &serde_json::Value| Reason::reply(&serde_json::to_vec(value).unwrap(), id);
        assert_eq!(classify(&value).code(), "frontend_authentication_refused");
        for code in [
            "frontend_identity_expired",
            "activation_receiver_unavailable",
        ] {
            value["code"] = code.into();
            assert_eq!(classify(&value).code(), code);
        }
        let original = value.clone();
        for (field, content) in [
            ("request_id", serde_json::json!("private fixture id")),
            ("code", serde_json::json!("private fixture text")),
            ("accepted", serde_json::json!(true)),
            ("durable", serde_json::json!(true)),
            ("extra", serde_json::json!("private fixture text")),
        ] {
            value = original.clone();
            value[field] = content;
            assert_eq!(classify(&value), Reason::ReplyDecode);
        }
        assert_eq!(Reason::reply(b"not json", id), Reason::ReplyDecode);
        assert_eq!(
            Reason::reply(
                &vec![b' '; zerus_watch_protocol::CONTROL_LINE_BYTES + 1],
                id
            ),
            Reason::ReplyDecode
        );
    }
}
