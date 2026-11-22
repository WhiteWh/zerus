//! The CLI against a temporary state directory, with the preview on and the
//! caller chosen by the test instead of by the environment.
mod board;
mod catalog;
mod catalog148;
mod create_binding;
mod dependencies;
mod dispatch;
mod drawer_pages;
mod editing;
mod evidence;
mod estimate_preview;
mod fixtures;
// WhiteWh fork: `hgs guide team` and `team-lead` are the fork's own pages.
#[cfg(any())]
mod guides;
mod harness;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod hosted;
// Upstream merge 2026-11-17: the fork keeps upstream's team client unwired;
// these upstream tests send as a Person, which its engine now refuses without
// a native gesture, or write the removed UI team fixtures.
#[cfg(any())]
mod lanes;
mod member_sessions;
#[cfg(unix)]
mod phase_controls;
mod phase_metrics;
mod phases;
mod placement;
mod planning;
mod progress;
mod progress157;
mod providers;
#[cfg(unix)]
mod publication;
mod reads;
mod recorded_timings;
#[cfg(unix)]
mod rework;
mod session_groups;
mod status_items;
mod storage_failure;
// Upstream merge 2026-11-17: the fork keeps upstream's team client unwired;
// these upstream tests send as a Person, which its engine now refuses without
// a native gesture, or write the removed UI team fixtures.
#[cfg(any())]
mod teams_followup;
#[cfg(any())]
mod teams_screen;
mod verdict;
mod writes;

mod observed_timing;
