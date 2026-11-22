//! Whether the DeepSeek host of an agent home can serve Teams members
//! (zerus-63rru.28). The only advertisement is the host's own `ping`
//! answer (`callTokens`, `inputWitness`, `teamLifecycle`): its identity record holds no capability and the
//! adapter version did not change with the proof. So this asks the host the
//! way the hosted caller proof does, through the same reads: only a process
//! this boot's record names with its birth, still running with that birth,
//! through the socket whose peer is that very process, within one deadline.
//! A host started before the proof stays as it is; nothing here restarts,
//! reloads or signals a host, and no host is started to answer.
//!
//! Linux and macOS only, where DeepSeek hosts run.
use zerus_team_plan::{problem::code, Problem};

/// The longest one host is asked.
const WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// What the agent home's DeepSeek host can prove.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostState {
    /// No live host is recorded for the home: a launch starts one from this
    /// hgs's bridge, and that host must then advertise call tokens.
    NoHost,
    /// The recorded live host advertises all required Teams capabilities.
    Proves,
    /// A recorded live host does not advertise them: it was started before
    /// the proof existed.
    Older,
}

/// The refusal for a home whose host cannot prove its sessions' commands. A
/// person can act on it; nothing restarts the host.
pub(crate) fn older(agent_home: &str) -> Problem {
    Problem::new(
        code::INVALID_STATE,
        format!("The DeepSeek host that serves {agent_home} lacks the caller, task-input or lifecycle capabilities required by Agent Teams. They become available after its next own start, which Zerus never forces."),
    )
    .with_detail(super::OLDER_DEEPSEEK_HOST)
    .with_refs([agent_home])
}

/// The state of the host of `agent_home`. Every recorded live host is asked;
/// one that does not advertise tokens makes the home `Older` even beside one
/// that does. A recorded process that no longer runs with its birth is no
/// host. A record that cannot be read, or a live host that does not answer
/// in time or answers from another process, is an error, never a state.
pub(crate) fn deepseek_host(agent_home: &str) -> Result<HostState, Problem> {
    let unknown = |reason: &str| {
        Problem::new(
            code::INVALID_STATE,
            format!("Cannot tell whether the DeepSeek host that serves {agent_home} can prove its sessions' commands: {reason}"),
        )
        .with_refs([agent_home])
    };
    let hosts = crate::state::session_graph::caller_host_for_home(agent_home).map_err(unknown)?;
    let mut state = HostState::NoHost;
    for (pid, birth) in hosts {
        if crate::platform::process_identity(pid).is_none_or(|process| process.birth != birth) {
            continue;
        }
        match crate::state::dsh::host_advertises_team_inputs(agent_home, (pid, &birth), WAIT) {
            Ok(true) if state == HostState::NoHost => state = HostState::Proves,
            Ok(true) => (),
            Ok(false) => state = HostState::Older,
            Err(reason) => return Err(unknown(reason)),
        }
    }
    Ok(state)
}

#[cfg(test)]
mod tests;
