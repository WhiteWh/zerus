//! The Teams read model (task #6): what the Team tab, the plan graph, the
//! Waiting list and New team show, read from `hgs [@host] team <op> --json` as
//! Qt's `TeamController` reads it, every three seconds while a view follows it.
//!
//! - [`read`] runs one read (`list`, `status`, `events`, `inbox`, `waiting`,
//!   `spans`, `profile`) with Qt's timeouts (15 s here, 45 s on a peer) and the
//!   reads' output limit, and checks the reply's top-level shape.
//! - [`view`] projects a reply into a compact typed view model; the webview
//!   gets these structures and never parses an `hgs` reply.
//! - [`TeamsModel`] decides which read is due, keeps the feed's pages within
//!   [`FEED_KEPT`] events, and turns a reply into [`TeamsUpdate`]s only when
//!   what a view shows changed.
//! - [`TeamsPoller`] runs the model on one thread per computer and hands each
//!   update to the client as a [`crate::Change::Teams`], which the per-frame
//!   [`crate::batch::Coalescer`] folds to the latest view per key.
//! - [`action`] runs the team actions the Qt UI offers as guarded calls with
//!   request identities; an unknown outcome is surfaced, never sent again.
//!
//! Memory is bounded: one view per key (a handful of keys per computer), the
//! feed's [`FEED_KEPT`] events, and the caps of [`view`] on members, tasks,
//! proposals and items.
pub mod action;
pub mod catalog_edit;
pub mod heads;
pub mod model;
pub mod plan_edit;
pub mod poller;
pub mod read;
pub mod role_models;
pub mod timeline;
pub mod view;

pub use action::{TeamAction, TeamCall, TeamOutcome};
pub use catalog_edit::{CatalogEdit, CatalogEditor, CatalogEditorView};
pub use heads::{Head, Heads, TeamHeads};
pub use model::{
    TeamsFollow, TeamsModel, TeamsUpdate, FEED_KEPT, POLL, REVISION_ONLY, STREAM_POLL,
};
pub use poller::{TeamsPoller, WaitingShare};
pub use read::{read, TeamRead};

#[cfg(test)]
mod tests;
