//! The Windows session host (spec `docs/superpowers/specs/2026-10-13-windows-native-host-design.md`,
//! D1-D6, D9-D11, D13, D14): `hgs.exe __host serve` owns the ConPTY sessions of
//! one user and state directory and serves the CLI, attach clients and
//! supervisors over a private named pipe.
//!
//! Plain Rust that compiles and runs its tests on every platform: `protocol`
//! (framing and the protocol 1 vocabulary), `endpoint` (pipe name, secret and
//! host lock), `staging` (explicit enablement and the staged CLI and host
//! images, D2), `gate` (who may run a host, from which image, and the test
//! override), the argument handling of `admin` (`hgs session-host`), the
//! portable half of `spawn` (environment blocks and command lines), `keys`
//! (tmux's key names and bytes), `sequences` (query and reply filtering), `link`
//! (connections as frames), `session` (a session's screen model, pumps and
//! attached clients), `registry` (sessions and requests), `channel` (the
//! host's end of the supervisor channel and its judgement of input evidence),
//! `supervisor` (the `--run` supervisor's end), `server`'s connection protocol,
//! `client`'s connection, `attach` (the attach client of `hgs a`, D13),
//! `worker` (D14) and `log`.
//! Windows only: `pipe`, `token`, `conpty`, `job`, `console` (sessions on
//! ConPTY), `server::serve`, `client::{connect, connect_or_start}`,
//! `supervisor::open`, `attach::Console` and `command` (`hgs.exe __host`).
// On Unix nothing outside the tests uses the host; the Windows lifecycle (M4)
// and attach client (M5) use more of it than the host process does today.
#![cfg_attr(not(all(windows, test)), allow(dead_code))]

pub(crate) mod admin;
pub(crate) mod attach;
mod channel;
pub(crate) mod client;
#[cfg(windows)]
mod command;
#[cfg(windows)]
mod conpty;
#[cfg(windows)]
mod console;
pub(crate) mod endpoint;
pub(crate) mod gate;
#[cfg(windows)]
pub(crate) mod job;
pub(crate) mod keys;
// WhiteWh fork: the session host kept without the desktop app.
#[cfg(windows)]
pub(crate) mod keeper;
mod link;
pub(crate) mod log;
mod pe;
#[cfg(windows)]
pub(crate) mod pipe;
pub(crate) mod protocol;
mod registry;
pub(crate) mod sequences;
pub(crate) mod server;
mod session;
pub(crate) mod spawn;
pub(crate) mod staging;
pub(crate) mod supervisor;
#[cfg(test)]
mod tests;
#[cfg(windows)]
pub(crate) mod token;
mod worker;

#[cfg(windows)]
pub(crate) use self::command::run;
// What an ended launch keeps of its screen, also where hgs records it (`state::launches`).
pub(crate) use self::registry::last_lines;
