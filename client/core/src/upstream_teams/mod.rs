//! The Teams read model and actions of the desktop client: what the Team tab
//! and the Waiting list show, read from `hgs team <op> --json`, projected into
//! compact typed views, and the guarded actions the person and the lead send.
//!
//! - [`view`] projects a checked reply into the view models; the webview gets
//!   these structures and never parses an `hgs` reply.
//! - [`read`] runs one read through `hgs` with its timeouts and output bound,
//!   and checks the shape the projection relies on.
//! - [`model`] decides which read is due for the followed views, keeps the
//!   latest projected view per key, and folds each reply into
//!   [`zerus_client_core::Change::Teams`] changes.
//! - [`poller`] runs the model on one thread and hands every change to the
//!   client pump.
//! - [`action`] builds and classifies the team actions as guarded calls with
//!   request identities; an uncertain outcome is surfaced, never sent again.
//!
//! The views are bounded: at most [`view::PAGE`] rows per list and the caps of
//! [`view`]. The reads are package 5's CLI: `hgs team list|status|queue|waiting`
//! with flags (`--team`, `--limit`, `--after`), a page reply of
//! `{ok, actor, team?, rows, next}` and a status of
//! `{ok, actor, team, members, counts}`.
//!
//! A request that only the person can answer inside a team *session* is not
//! part of [`read::TeamRead::Waiting`]: it arrives in the desktop's session
//! attention data. [`view::WaitingView`] carries the open owner cards only.
//!
//! An action's intent lives in memory for this milestone
//! ([`action::Intents`]): a restarted desktop forgets it, so a new gesture
//! gets a new request id and carries the revision of what the person saw. The
//! store's own request record still answers a repeated id, which is why an id
//! is made once and never replaced. A page of a followed key stays selected
//! until the person chooses another; every later read reads that page.
//!
//! Teams are inactive unless the person turned the preview on; with it off
//! every read and action answers `teams_inactive` and the desktop shows no
//! entry point.
pub mod action;
pub mod catalog;
pub mod catalog_edit;
pub mod drawers;
pub mod estimate_comparison;
pub mod estimate_projection;
pub mod evidence;
pub mod model;
pub mod observed_timing;
// WhiteWh fork: the client polls the fork's teams; upstream's poller is left out.
pub mod profile_export;
pub mod providers;
pub mod read;
pub mod recorded_timings;
mod refusal;
pub mod settings;
pub mod timeline;
pub mod tolerant;
pub mod view;

pub use action::{Confirmation, TeamAction, TeamCall, TeamOutcome};
pub use model::{team_key, TeamsFollow, TeamsModel, TeamsUpdate, POLL};
pub use read::{read, valid_team, TeamRead};
