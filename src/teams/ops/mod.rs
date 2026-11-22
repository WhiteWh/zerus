//! Transactional team decisions and service-recorded facts. No operation
//! performs an effect in a session, a task document, or Git.
#![allow(dead_code)] // Used by the following CLI and service packages.
mod apply;
mod authority;
mod base;
mod common;
mod decisions;
mod delegation;
mod dependencies;
mod dependency_graph;
mod drafts;
mod editing;
pub(super) mod handout;
pub(crate) use handout::handout_admissible;
mod integration;
mod journal;
pub(crate) mod lead_queue;
mod members;
pub(crate) mod observed_timing;
mod phase_controls;
pub(crate) use phase_controls::prepare as prepare_phase_controls;
mod phase_review;
mod pipeline;
mod planning;
mod provider_settings;
mod provider_stop;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use apply::apply_provider_stop;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use provider_stop::CHANGED as PROVIDER_STOP_CHANGED;
pub(crate) use provider_stop::{ProviderConfirmation, ProviderStopProof, CONFIRMATION_CLOSED};
pub(crate) mod publication;
mod queue;
#[cfg(test)]
pub(crate) use queue::injected;
mod readiness;
pub(crate) mod rework;
mod stage_task;
mod talk;
mod team;
mod timers;
mod types;
mod unstaffed;
pub use unstaffed::{unstaffed_changes, unstaffed_of, unstaffed_stages};
mod validation;
use super::store::{self, rows, DecisionState, Store};
#[cfg(test)]
pub(crate) use apply::apply_with_native;
#[cfg(test)]
pub(crate) use apply::apply_with_repository;
#[allow(unused_imports)]
pub use apply::{apply, apply_with_config};
pub(crate) use apply::{
    apply_with_gesture, apply_with_native_context, observe_native_intake, replay,
};
pub use authority::authority;
use common::*;
#[allow(unused_imports)] // The teams actor consumes this read next.
pub use readiness::{readiness, Readiness};
use rusqlite::{params, Connection, OptionalExtension};
use store::{decode, encode, storage};
pub use types::*;
use zerus_team_plan::{model::*, problem::code, Problem};
type Result<T, E = Problem> = std::result::Result<T, E>;

#[cfg(test)]
pub(crate) mod tests;
