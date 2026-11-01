use super::super::ops::{self, Actor};
use super::*;
use super::support::*;

// ---- #480: the phase 35 review's dispatcher findings, through steps()
// and run_step() on a private store.


#[cfg(test)]
#[path = "dispatcher_replies.rs"]
mod dispatcher_replies;

#[cfg(test)]
#[path = "dispatcher_inbox_questions.rs"]
mod dispatcher_inbox_questions;

#[cfg(test)]
#[path = "dispatcher_handout.rs"]
mod dispatcher_handout;

#[cfg(test)]
#[path = "dispatcher_report_holds.rs"]
mod dispatcher_report_holds;

#[cfg(test)]
#[path = "dispatcher_handoff_brief.rs"]
mod dispatcher_handoff_brief;

#[cfg(test)]
#[path = "dispatcher_worktrees.rs"]
mod dispatcher_worktrees;

#[cfg(test)]
#[path = "dispatcher_worker.rs"]
mod dispatcher_worker;

#[cfg(test)]
#[path = "dispatcher_stage_land.rs"]
mod dispatcher_stage_land;

#[cfg(test)]
#[path = "dispatcher_work_ahead.rs"]
mod dispatcher_work_ahead;
