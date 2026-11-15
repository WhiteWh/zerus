//! The calls behind the held-messages bar and the Terminal draft bar:
//! - `hgs [@alias] send-cancel NAME --json` with `{"request_id"}` on standard
//!   input cancels a message hgs still holds. Only a receipt naming the
//!   same request with status "cancelled" is a cancel; anything else leaves
//!   the message where it was.
//! - `hgs [@alias] terminal NAME --json` with `"action":"draft"` reads the
//!   text left in the agent's own Terminal input, and `"submit_draft"`
//!   presses Enter there only while the input still holds exactly the draft
//!   whose hash was shown. Each call has a fresh request id; nothing is
//!   retried, and an Enter that may have gone is never sent again.
use crate::actions::{failure_detail, prepend_alias, Exit, Invocation, Target};
use serde_json::{json, Value};
use std::time::Duration;

/// Qt's failed cancel without hgs's own error (`requestCancelHeld`).
pub const NOT_CANCELLED: &str = crate::source!("The message could not be cancelled.");
/// Qt's text for a refused Enter without hgs's own error.
pub const NOT_SENT: &str = crate::source!("Not sent");
/// The answer was not a Terminal receipt.
pub const INVALID: &str = crate::source!("Invalid Terminal response");
/// Qt's uncertain Enter; `%1` is hgs's own error, if any.
pub const MAYBE_SENT: &str =
    crate::source!("Enter may have reached Terminal; check it before sending again. %1");
/// A Terminal call needs the session's exact run.
const NO_RUN: &str = "Refresh the session before reading its Terminal.";
/// The largest answer either call reads back.
pub const REPLY_BYTES: usize = 1 << 20;

/// What `submit_draft` refused, or why a call failed. `uncertain`: Enter may
/// have reached Terminal ([`MAYBE_SENT`] with `detail` as its argument).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftError {
    pub detail: String,
    pub uncertain: bool,
}

/// A Terminal draft read: the text, empty when there is none, and the hash
/// `submit_draft` must name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftRead {
    pub draft: String,
    pub hash: String,
}

fn timeout(remote: bool) -> Duration {
    Duration::from_secs(if remote { 45 } else { 15 })
}

fn stderr_of(exit: Exit) -> String {
    failure_detail(exit)
}

/// The stdin and arguments of `hgs send-cancel NAME --json` for the held
/// message `held_request`. Its own request is the held one's: a cancel is
/// idempotent, and hgs answers a repeat with the same receipt.
pub fn cancel_invocation(target: &Target, held_request: &str) -> Result<Invocation, String> {
    if target.name.is_empty() || uuid::Uuid::parse_str(held_request).is_err() {
        return Err(NOT_CANCELLED.into());
    }
    let mut args = vec!["send-cancel".to_owned(), target.name.clone(), "--json".to_owned()];
    let remote = prepend_alias(&mut args, &target.computer);
    Ok(Invocation {
        args,
        stdin: Some(
            serde_json::to_vec(&json!({ "request_id": held_request }))
                .map_err(|error| error.to_string())?,
        ),
        timeout: timeout(remote),
        request_id: Some(held_request.to_owned()),
    })
}

/// A cancel's end: `Ok` only for a receipt that names `held_request` with
/// status "cancelled"; otherwise the error the notice shows.
pub fn classify_cancel(held_request: &str, exit: Exit) -> Result<Value, String> {
    match exit {
        Exit::Code {
            code: 0, stdout, ..
        } => {
            let receipt: Value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
            let ok = receipt["request_id"].as_str() == Some(held_request)
                && receipt["status"].as_str() == Some("cancelled");
            if ok {
                Ok(receipt)
            } else {
                Err(NOT_CANCELLED.into())
            }
        }
        exit => Err(stderr_of(exit)),
    }
}

/// The stdin and arguments of `hgs terminal NAME --json`: a read of the
/// draft, or with `submit` the Enter for the draft whose hash it names.
/// `request_id` is a fresh UUID for this one call.
pub fn draft_invocation(
    target: &Target,
    request_id: &str,
    submit: Option<&str>,
) -> Result<Invocation, String> {
    if target.name.is_empty() || target.run_id.is_empty() {
        return Err(NO_RUN.into());
    }
    let mut input = json!({
        "request_id": request_id,
        "action": if submit.is_some() { "submit_draft" } else { "draft" },
        "expected_run_id": target.run_id,
        "expected_conversation_id": target.conversation_id,
    });
    if let Some(hash) = submit {
        if hash.len() != 64 || !hash.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
            return Err(INVALID.into());
        }
        input["draft_hash"] = json!(hash);
    }
    let mut args = vec!["terminal".to_owned(), target.name.clone(), "--json".to_owned()];
    let remote = prepend_alias(&mut args, &target.computer);
    Ok(Invocation {
        args,
        stdin: Some(serde_json::to_vec(&input).map_err(|error| error.to_string())?),
        timeout: timeout(remote),
        request_id: Some(request_id.to_owned()),
    })
}

/// The receipt of a Terminal call, when it is one for this request.
fn receipt(request_id: &str, exit: Exit) -> Result<Value, Exit> {
    match exit {
        Exit::Code {
            code: 0, stdout, ..
        } => {
            let receipt: Value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
            if receipt.is_object() && receipt["request_id"].as_str() == Some(request_id) {
                Ok(receipt)
            } else {
                Err(Exit::Code {
                    code: 0,
                    stdout: Vec::new(),
                    stderr: INVALID.into(),
                })
            }
        }
        exit => Err(exit),
    }
}

/// A draft read's end. A failed read changes nothing shown (Qt keeps the
/// bar as it was), so its error is only for the log.
pub fn classify_draft_read(request_id: &str, exit: Exit) -> Result<DraftRead, String> {
    let receipt = receipt(request_id, exit).map_err(stderr_of)?;
    let draft = receipt["draft"].as_str().unwrap_or("").to_owned();
    let hash = receipt["draft_hash"].as_str().unwrap_or("").to_owned();
    if !draft.is_empty() && hash.len() != 64 {
        return Err(INVALID.into());
    }
    Ok(DraftRead { draft, hash })
}

/// An Enter's end: `Ok` once hgs says "submitted". A receipt that says
/// "failed" sent nothing; "uncertain", a timeout, a broken transfer or an
/// abnormal end may have pressed Enter.
pub fn classify_draft_submit(request_id: &str, exit: Exit) -> Result<(), DraftError> {
    let failed = |detail: String| DraftError {
        detail,
        uncertain: false,
    };
    let maybe = |detail: String| DraftError {
        detail,
        uncertain: true,
    };
    let receipt = match receipt(request_id, exit) {
        Ok(receipt) => receipt,
        Err(Exit::StartFailed(error)) => return Err(failed(error)),
        Err(exit @ (Exit::TimedOut | Exit::WriteFailed | Exit::Abnormal { .. })) => {
            return Err(maybe(stderr_of(exit)))
        }
        // hgs refused it before any Enter (a changed draft, another run).
        Err(Exit::Code {
            code,
            stdout,
            stderr,
        }) if code != 0 => {
            let unsure = stderr.to_lowercase().contains("uncertain");
            let detail = stderr_of(Exit::Code {
                code,
                stdout,
                stderr,
            });
            return Err(if unsure { maybe(detail) } else { failed(detail) });
        }
        // Exit 0 without a receipt for this request: Enter may have gone.
        Err(_) => return Err(maybe(INVALID.into())),
    };
    let error = receipt["error"].as_str().unwrap_or("").trim().to_owned();
    match receipt["status"].as_str() {
        Some("submitted") => Ok(()),
        Some("failed") => Err(failed(if error.is_empty() { NOT_SENT.into() } else { error })),
        _ => Err(maybe(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELD: &str = "11111111-2222-4333-8444-555555555555";
    const REQUEST: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn target(computer: &str) -> Target {
        Target {
            computer: computer.into(),
            name: "claude/sample-project/review".into(),
            run_id: "run-1".into(),
            conversation_id: "conv-1".into(),
        }
    }

    fn ok(stdout: Value) -> Exit {
        Exit::Code {
            code: 0,
            stdout: stdout.to_string().into_bytes(),
            stderr: String::new(),
        }
    }

    fn refused(stderr: &str) -> Exit {
        Exit::Code {
            code: 1,
            stdout: Vec::new(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn a_cancel_names_only_the_held_request_on_stdin() {
        let local = cancel_invocation(&target(""), HELD).unwrap();
        assert_eq!(local.args, ["send-cancel", "claude/sample-project/review", "--json"]);
        assert_eq!(local.timeout, Duration::from_secs(15));
        let input: Value = serde_json::from_slice(local.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(input, json!({"request_id": HELD}));
        let remote = cancel_invocation(&target("mac"), HELD).unwrap();
        assert_eq!(remote.args[0], "@mac");
        assert_eq!(remote.timeout, Duration::from_secs(45));
        assert!(cancel_invocation(&target(""), "not-a-uuid").is_err());
    }

    #[test]
    fn only_a_cancelled_receipt_for_the_same_request_is_a_cancel() {
        assert!(classify_cancel(HELD, ok(json!({"request_id": HELD, "status": "cancelled"}))).is_ok());
        assert_eq!(
            classify_cancel(HELD, ok(json!({"request_id": HELD, "status": "queued"}))).unwrap_err(),
            NOT_CANCELLED
        );
        assert_eq!(
            classify_cancel(HELD, ok(json!({"request_id": REQUEST, "status": "cancelled"})))
                .unwrap_err(),
            NOT_CANCELLED
        );
        assert_eq!(
            classify_cancel(HELD, refused("the message was already delivered; it cannot be cancelled"))
                .unwrap_err(),
            "the message was already delivered; it cannot be cancelled"
        );
        assert_eq!(classify_cancel(HELD, Exit::TimedOut).unwrap_err(), "timed out");
    }

    #[test]
    fn a_terminal_draft_call_names_its_run_and_conversation() {
        let read = draft_invocation(&target(""), REQUEST, None).unwrap();
        assert_eq!(read.args, ["terminal", "claude/sample-project/review", "--json"]);
        let input: Value = serde_json::from_slice(read.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(
            input,
            json!({"request_id": REQUEST, "action": "draft", "expected_run_id": "run-1", "expected_conversation_id": "conv-1"})
        );
        let submit = draft_invocation(&target("mac"), REQUEST, Some(HASH)).unwrap();
        assert_eq!(submit.args[0], "@mac");
        let input: Value = serde_json::from_slice(submit.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(input["action"], "submit_draft");
        assert_eq!(input["draft_hash"], HASH);
        assert!(draft_invocation(&target(""), REQUEST, Some("ABC")).is_err());
        let mut no_run = target("");
        no_run.run_id.clear();
        assert!(draft_invocation(&no_run, REQUEST, None).is_err());
    }

    #[test]
    fn a_draft_read_returns_the_text_and_its_hash() {
        let read = classify_draft_read(
            REQUEST,
            ok(json!({"request_id": REQUEST, "status": "draft", "draft": "half a thought", "draft_hash": HASH, "draft_supported": true})),
        )
        .unwrap();
        assert_eq!(read, DraftRead { draft: "half a thought".into(), hash: HASH.into() });
        let none = classify_draft_read(
            REQUEST,
            ok(json!({"request_id": REQUEST, "status": "draft", "draft": null, "draft_hash": HASH})),
        )
        .unwrap();
        assert!(none.draft.is_empty());
        // Another request's answer, or a draft without its hash, is not one.
        assert!(classify_draft_read(REQUEST, ok(json!({"request_id": HELD, "draft": "x", "draft_hash": HASH}))).is_err());
        assert!(classify_draft_read(REQUEST, ok(json!({"request_id": REQUEST, "draft": "x"}))).is_err());
        assert!(classify_draft_read(REQUEST, refused("Invalid terminal identity")).is_err());
    }

    #[test]
    fn an_enter_is_submitted_failed_or_maybe_sent() {
        assert_eq!(
            classify_draft_submit(REQUEST, ok(json!({"request_id": REQUEST, "status": "submitted"}))),
            Ok(())
        );
        assert_eq!(
            classify_draft_submit(REQUEST, ok(json!({"request_id": REQUEST, "status": "failed"}))),
            Err(DraftError { detail: NOT_SENT.into(), uncertain: false })
        );
        assert_eq!(
            classify_draft_submit(REQUEST, refused("The Terminal draft changed; nothing was sent")),
            Err(DraftError { detail: "The Terminal draft changed; nothing was sent".into(), uncertain: false })
        );
        assert_eq!(
            classify_draft_submit(REQUEST, ok(json!({"request_id": REQUEST, "status": "uncertain", "error": "tmux lost"}))),
            Err(DraftError { detail: "tmux lost".into(), uncertain: true })
        );
        assert!(classify_draft_submit(REQUEST, Exit::TimedOut).unwrap_err().uncertain);
        assert!(classify_draft_submit(REQUEST, Exit::WriteFailed).unwrap_err().uncertain);
        // A receipt for another request may hide an Enter that went.
        assert!(
            classify_draft_submit(REQUEST, ok(json!({"request_id": HELD, "status": "submitted"})))
                .unwrap_err()
                .uncertain
        );
        assert!(!classify_draft_submit(REQUEST, Exit::StartFailed("missing".into())).unwrap_err().uncertain);
    }
}
