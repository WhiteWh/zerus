//! The one original `hgs send <session> --json` child and the custody it holds.
//!
//! The child is this computer's own `hgs` with the plan's exact arguments, in
//! its own process group. Its stdin is exactly the original's frozen bytes;
//! its output is drained and discarded within a bound, because only the CLI's
//! own receipt settles the send. It inherits the effect's close-only custody
//! lock and, before anything else, marks it close-on-exec ([`adopt_custody`]).
//! Only that child is ever signalled, and only at the deadline.
//!
//! The caller's resources guard (the custody lock and the original's consent,
//! with its lease and serve-owner loan) outlives every original pipe: an
//! output pipe still open on return keeps a guard copy in its own reader until
//! its real end. The runner is shared ([`crate::desktop::original_child`]).
// macOS admission refuses every original until a qualified child seam attests
// the code its child runs, so only tests start this runner there.
#![cfg_attr(target_os = "macos", allow(dead_code))]
use crate::desktop::original_child::{self, Codes, Input, Output, Retain, Spec, Stdin};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;
use zerus_client_core::run::custody::InheritedLock;

/// Names the inherited custody descriptor of a deferred send child.
pub(super) const CUSTODY_FD: &str = "HGS_DEFERRED_SEND_CUSTODY_FD";
/// The reply kept to bound the drain; its content settles nothing.
const STDOUT_BYTES: usize = 64 << 10;
/// This kind's reasons for an Unknown result.
const CODES: Codes = Codes {
    unobserved: "deferred_send_unobserved",
    unreadable: "deferred_send_output_unreadable",
    signalled: "deferred_send_signalled",
    timed_out: "deferred_send_timed_out",
    retained: "deferred_send_output_retained",
};

/// Called first in `main`. A deferred send child started by the desktop actor
/// holds the actor's close-only custody lock for exactly its own lifetime.
pub(crate) fn adopt_custody() -> Result<(), String> {
    original_child::adopt(
        CUSTODY_FD,
        "Invalid deferred send custody",
        "Deferred send custody is unavailable",
    )
}

/// How the original child ended. Its exit never settles the send.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::desktop) enum Ended {
    /// It read all of its stdin and exited with `code`, and both output pipes
    /// ended, within the deadline.
    Exited { code: i32 },
    /// It never started: refused before any child existed. Nothing ran.
    NotStarted,
    /// Its end is unknown, for the named reason.
    Unknown(&'static str),
}

/// Runs the original child to its end, or to the deadline. `final_check` runs
/// inside the spawn turn, last before the spawn: its refusal means that no
/// child ever existed.
pub(super) fn run(
    program: &Path,
    args: &[String],
    input: &[u8],
    custody: &Arc<InheritedLock>,
    resources: &Arc<dyn Send + Sync>,
    deadline: Instant,
    final_check: impl FnOnce(&mut Command) -> std::io::Result<()>,
) -> Ended {
    let spec = Spec {
        custody_env: CUSTODY_FD,
        input: Input::Frozen {
            stdin: Stdin::Bytes(input),
            unwritten: "deferred_send_input_unwritten",
        },
        output: Output::Stdout {
            bytes: STDOUT_BYTES,
            too_large: "deferred_send_reply_too_large",
        },
        codes: CODES,
        retain: Some(Retain {
            guard: resources,
            reader: "deferred-send-output",
        }),
    };
    match original_child::run(program, args, spec, custody, deadline, final_check) {
        original_child::Ended::Exited { code, .. } => Ended::Exited { code },
        original_child::Ended::NotStarted => Ended::NotStarted,
        original_child::Ended::Unknown(code) => Ended::Unknown(code),
    }
}
