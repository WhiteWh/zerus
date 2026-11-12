//! Native capabilities from `hgs --help` (`connector.py:498-544`): Python's
//! frozen help-marker table, its operation and feature formula
//! (`connector.py:490-494`), and the operations this build implements.
//!
//! An operation is advertised only when the native CLI offers it, as in
//! Python, and this connector implements it; a later slice adds to
//! [`IMPLEMENTED`]. [`CONNECTOR_ONLY`] operations follow their own native
//! gates and are never Python's. Python resets every flag before probing, so a failed
//! probe disables everything for 300 s; here a probe that fails keeps the
//! previous flags (divergence a).
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use zerus_relay::protocol::OPERATIONS;

/// The native help text is read again this often (seconds).
pub const REPROBE_SECONDS: f64 = 300.0;
/// `hgs --help` budget (seconds).
pub const PROBE_TIMEOUT: f64 = 3.0;
/// Windows v1 deliberately excludes POSIX paths, one-hop routing, native
/// providers absent from the host, speech and voice. Same table drives
/// advertisement and dispatch refusal, after durable duplicate handling.
pub const WINDOWS_HELP_MARKER: &str = "Mobile gateway ABI: windows-local-v1";
pub const WINDOWS_PROVIDER_REFUSAL: &str = "provider is unavailable on a Windows local gateway";
pub fn windows_provider(provider: &Value) -> bool {
    matches!(provider.as_str(), Some("codex" | "claude" | "kimi"))
}
pub const WINDOWS_OPERATIONS: &[&str] = &[
    "inspect",
    "history",
    "archive_list",
    "send",
    "answer",
    "interrupt",
    "send_now",
    "settings",
    "clear_context",
    "compact_context",
    "mark_read",
    "session_notifications",
    "session_question_routing",
    "terminal_snapshot",
    "terminal_input",
    "hooks_status",
    "process_output",
    "process_stop",
    "archive",
    "forget",
    "fork",
    "pause",
    "remove_queued",
    "rename",
    "restore",
    "resume",
    "terminate",
];
pub const CLEAR_CONTEXT_DISMISS_HELP_MARKER: &str = "Mobile clear-context ABI: dismiss-prompt-v1";

/// Operations this connector executes, with the session mutations of
/// [`crate::mutations::OPERATIONS`] and the creations of
/// [`crate::launch::OPERATIONS`]. Every other known operation is refused
/// as unsupported and never advertised.
pub const IMPLEMENTED: &[&str] = &[
    "analytics_read",
    "archive_list",
    "attachment_read",
    "catalog",
    "dirs",
    "file_read",
    "git_status",
    "repository_check",
    "repository_clone",
    "repository_status",
    "history",
    "history_projection",
    "hooks_status",
    "inspect",
    "mark_read",
    "session_notifications",
    "session_question_routing",
    "session_question_route_person",
    "process_output",
    "project_assignment",
    "project_assign",
    "speech_cancel",
    "speech_config",
    "speech_config_status",
    "speech_profile_remove",
    "speech_profile_test",
    "speech_profile_upsert",
    "speech_profiles",
    "speech_stats",
    "speech_status",
    "speech_submit",
    "task_cancel",
    "task_done",
    "task_exchange",
    "task_send",
    "terminal_input",
    "terminal_snapshot",
    "voice_cancel",
    "voice_profiles",
    "voice_status",
    "voice_submit",
    "worktrees",
];

pub fn implemented(operation: &str) -> bool {
    IMPLEMENTED.contains(&operation)
        || crate::mutations::handles(operation)
        || crate::launch::handles(operation)
}

/// Python's `FEATURES | {"accounts_snapshot"}` and the conditional features,
/// each with the operation it needs.
const FEATURE_OPERATIONS: &[(&str, &str)] = &[
    ("clear_context_dismiss", "clear_context"),
    ("voice_video", "voice_submit"),
    ("accounts_snapshot", "inspect"),
    ("inspect_after", "inspect"),
    ("inspect_agent", "inspect"),
    ("send_agent", "send"),
    ("directory_create_v1", "directory_create"),
    ("launch_project", "launch"),
    (crate::session_projects::FEATURE, "project_assign"),
    ("gateway_one_hop", "send"),
];

/// Whether an operation only reads native state or may change it. The relay
/// classifies it the same way (`READS`), and a mutation keeps its journal
/// identity and recovers as uncertain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Read,
    Mutation,
}

/// The native help marker that gates a [`CONNECTOR_ONLY`] operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// `session-action <session> --json`, the scoped session actions.
    Lifecycle,
    /// The bounded, computer-scoped Git checkout projection.
    GitStatus,
    /// The phone's task exchange and the projects' tasks digests.
    TaskExchange,
    /// The phone's Send now, Cancel and Done list.
    TaskActions,
    Repository,
    /// The person's per-child question route switch (zerus-sa9qn.33).
    QuestionRouting,
    /// A phone's signed latch statement (zerus-sa9qn.50).
    QuestionRoutePerson,
}

impl Gate {
    /// Whether the native CLI offers it.
    pub fn open(self, flags: &Flags) -> bool {
        match self {
            Gate::Lifecycle => flags.lifecycle,
            Gate::GitStatus => flags.git_status,
            Gate::TaskExchange => flags.task_exchange,
            Gate::TaskActions => flags.task_actions,
            Gate::Repository => flags.repository,
            Gate::QuestionRouting => flags.question_routing,
            Gate::QuestionRoutePerson => flags.question_route_person,
        }
    }

    /// Its marker name in `services/mobile/tests/rust-only-operations.json`,
    /// which is also the frozen marker's name in `operations.json`.
    pub fn name(self) -> &'static str {
        match self {
            Gate::Lifecycle => "lifecycle",
            Gate::GitStatus => "git_status",
            Gate::TaskExchange => "task_exchange",
            Gate::TaskActions => "task_actions",
            Gate::Repository => "repository",
            Gate::QuestionRouting => "question_routing",
            Gate::QuestionRoutePerson => "question_route_person",
        }
    }

    /// A help line that offers it.
    pub fn help_line(self) -> &'static str {
        match self {
            Gate::Lifecycle => "hgs session-action <session> --json",
            Gate::GitStatus => crate::git_status::HELP_MARKER,
            Gate::TaskExchange => "hgs task exchange|digests --json",
            Gate::TaskActions => "hgs task mobile-send|mobile-cancel|mobile-done --json",
            Gate::Repository => crate::repositories::HELP_MARKER,
            Gate::QuestionRouting => crate::session_question_routing::HELP_MARKER,
            Gate::QuestionRoutePerson => crate::session_question_route_person::HELP_MARKER,
        }
    }
}

/// An operation of the relay's set that only this connector executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectorOnly {
    pub operation: &'static str,
    pub gate: Gate,
    pub class: Class,
}

/// The operations of the relay's set that only this connector executes, each
/// advertised exactly when its native marker is offered. The Python connector
/// never lists or advertises them, so they are outside
/// [`Flags::python_operations`]. The tests of the relay, of this connector and
/// of the black-box suites read the same table from
/// `services/mobile/tests/rust-only-operations.json`.
pub const CONNECTOR_ONLY: &[ConnectorOnly] = &[
    ConnectorOnly {
        // The session-action marker predates queue removal: an older hgs prints it
        // and refuses the request. Only `remove_queued` in the session's
        // `allowed_actions` shows that hgs can remove the message.
        operation: "remove_queued",
        gate: Gate::Lifecycle,
        class: Class::Mutation,
    },
    ConnectorOnly {
        operation: "git_status",
        gate: Gate::GitStatus,
        class: Class::Read,
    },
    ConnectorOnly {
        operation: "repository_check",
        gate: Gate::Repository,
        class: Class::Read,
    },
    ConnectorOnly {
        operation: "repository_clone",
        gate: Gate::Repository,
        class: Class::Mutation,
    },
    ConnectorOnly {
        operation: "repository_status",
        gate: Gate::Repository,
        class: Class::Read,
    },
    // The phone's tasks (docs/superpowers/specs/2026-10-18-task-replication-loro-design.md, "Phone").
    ConnectorOnly {
        operation: "task_exchange",
        gate: Gate::TaskExchange,
        class: Class::Read,
    },
    ConnectorOnly {
        operation: "task_send",
        gate: Gate::TaskActions,
        class: Class::Mutation,
    },
    ConnectorOnly {
        operation: "task_cancel",
        gate: Gate::TaskActions,
        class: Class::Mutation,
    },
    ConnectorOnly {
        operation: "task_done",
        gate: Gate::TaskActions,
        class: Class::Read,
    },
    // The person's per-child question route switch (zerus-sa9qn.33).
    ConnectorOnly {
        operation: "session_question_routing",
        gate: Gate::QuestionRouting,
        class: Class::Mutation,
    },
    // A phone's signed latch statement (zerus-sa9qn.50).
    ConnectorOnly {
        operation: "session_question_route_person",
        gate: Gate::QuestionRoutePerson,
        class: Class::Mutation,
    },
];

/// Whether only this connector executes the operation.
pub fn connector_only(operation: &str) -> bool {
    CONNECTOR_ONLY.iter().any(|row| row.operation == operation)
}

pub const LIFECYCLE: &[&str] = &[
    "archive",
    "forget",
    "fork",
    "pause",
    "remove_queued",
    "rename",
    "restore",
    "resume",
    "terminate",
];
pub const TERMINAL: &[&str] = &["terminal_input", "terminal_snapshot"];
pub const LAUNCH: &[&str] = &[
    "catalog",
    "dirs",
    "directory_create",
    "launch",
    "worktree_create",
    "worktrees",
];
pub const WORKTREES: &[&str] = &["worktree_create", "worktrees"];
pub const SPEECH: &[&str] = &[
    "speech_cancel",
    "speech_profiles",
    "speech_stats",
    "speech_status",
    "speech_submit",
];
pub const VOICE: &[&str] = &[
    "voice_cancel",
    "voice_profiles",
    "voice_status",
    "voice_submit",
];
pub const SPEECH_CONFIG: &[&str] = &[
    "speech_config",
    "speech_config_status",
    "speech_profile_remove",
    "speech_profile_test",
    "speech_profile_upsert",
];

/// What the native CLI offers, from its help text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    pub windows_gateway: bool,
    pub analytics: bool,
    pub archive: bool,
    pub lifecycle: bool,
    pub terminal: bool,
    pub recovery: bool,
    pub history: bool,
    pub history_projection: bool,
    pub hook_health: bool,
    pub session_projects: bool,
    pub launch: bool,
    pub project_launch: bool,
    pub worktree: bool,
    pub directory_create: bool,
    pub read: bool,
    pub session_notifications: bool,
    /// The exact `session-question-routing-v1` line (zerus-sa9qn.33).
    pub question_routing: bool,
    /// The exact `question-route-person-v1` line (zerus-sa9qn.50).
    pub question_route_person: bool,
    pub preference: bool,
    pub attachment_read: bool,
    pub file_read: bool,
    pub git_status: bool,
    pub repository: bool,
    pub clear_context_dismiss: bool,
    pub speech: bool,
    pub voice: bool,
    pub voice_video: bool,
    pub speech_config: bool,
    /// Both peer markers (`fleet.supported`).
    pub peers: bool,
    /// The phone's task exchange, which the Python connector never offers.
    pub task_exchange: bool,
    /// The phone's Send now, Cancel and Done list, which it never offers either.
    pub task_actions: bool,
    /// `hgs watch --json` (watch-v1): the heartbeat follows the local listing
    /// as a stream instead of running `ls` (`watch_listing`). Not an operation.
    pub watch: bool,
    /// `session-id-selector-v1`: the CLI selects a session by `--session-id`.
    /// Not an operation (`session_selector`).
    pub session_selector: bool,
    /// `mobile-peer-session-id-v1`: its mobile peer entry points accept that
    /// flag after the session. Not an operation.
    pub peer_session_id: bool,
    /// Complete current-session reads and ID project membership, not just mutation selection.
    pub session_labels: bool,
    /// `inspect <session> --current-read`: a main inspection names the reply
    /// and attention a read mark may acknowledge now. Not an operation.
    pub current_read: bool,
}

impl Flags {
    /// The frozen help markers (`operations.json` `capabilities.markers`).
    pub fn from_help(help: &str) -> Flags {
        let has = |marker: &str| help.contains(marker);
        let flags = Flags {
            windows_gateway: help.lines().any(|line| line.trim() == WINDOWS_HELP_MARKER),
            analytics: has("hgs [@host] analytics --json"),
            archive: has("archives --json"),
            lifecycle: help
                .lines()
                .any(|line| line.contains("session-action <session> --json")),
            terminal: has("terminal <session> --json"),
            recovery: has("recovery action --scoped-json"),
            history: has("history <session> --json"),
            history_projection: has("history-projection <session> --json"),
            hook_health: has(crate::hook_health::HELP_MARKER),
            session_projects: crate::session_projects::from_help(help),
            launch: has("--launch-id"),
            project_launch: has("swarm assign-launch --json"),
            worktree: has("Mobile worktree ABI: worktrees-v1"),
            directory_create: has("dirs --create <path>"),
            read: has("read <session> --json"),
            current_read: has("inspect <session> --current-read"),
            session_notifications: help
                .lines()
                .any(|line| line.trim() == crate::session_notifications::HELP_MARKER),
            question_routing: help
                .lines()
                .any(|line| line.trim() == crate::session_question_routing::HELP_MARKER),
            question_route_person: help
                .lines()
                .any(|line| line.trim() == crate::session_question_route_person::HELP_MARKER),
            preference: has("swarm preference set mobile_delivery"),
            attachment_read: has("attachment <session> --request ID --index N"),
            file_read: has("file-read <session> --json"),
            repository: help
                .lines()
                .any(|line| line.trim() == crate::repositories::HELP_MARKER),
            git_status: help
                .lines()
                .any(|line| line.trim() == crate::git_status::HELP_MARKER),
            clear_context_dismiss: help
                .lines()
                .any(|line| line.trim() == CLEAR_CONTEXT_DISMISS_HELP_MARKER),
            speech: has("speech --json"),
            voice: has("voice <session> --json"),
            voice_video: has("voice-video-v1"),
            speech_config: has("Speech profile management v1"),
            peers: has("mobile-peers --json") && has("mobile-peer --json"),
            task_exchange: has("hgs task exchange|digests --json"),
            task_actions: has("hgs task mobile-send|mobile-cancel|mobile-done --json"),
            watch: help
                .lines()
                .any(|line| line.trim() == zerus_watch_protocol::HELP_MARKER),
            session_selector: help
                .lines()
                .any(|line| line.trim() == crate::session_selector::SELECTOR_MARKER),
            peer_session_id: help
                .lines()
                .any(|line| line.trim() == crate::session_selector::PEER_MARKER),
            session_labels: help
                .lines()
                .any(|line| line.trim() == crate::session_selector::LABELS_MARKER),
        };
        if flags.windows_gateway {
            return flags.windows_local();
        }
        flags
    }

    pub fn for_native_platform(self) -> Self {
        if cfg!(windows) {
            self.windows_local()
        } else {
            self
        }
    }
    pub fn windows_local(mut self) -> Self {
        self.windows_gateway = true;
        self.peers = false;
        self.voice = false;
        self.voice_video = false;
        self.speech = false;
        self.speech_config = false;
        self.launch = false;
        self.project_launch = false;
        self.worktree = false;
        self.directory_create = false;
        self.file_read = false;
        self.attachment_read = false;
        self.git_status = false;
        self.repository = false;
        self.analytics = false;
        self.watch = false;
        self
    }

    /// Python's advertised operations, before this build's own filter.
    /// `fleet` is whether a fleet exists (always in the connector).
    pub fn python_operations(&self, fleet: bool) -> BTreeSet<&'static str> {
        let mut operations = self.offered(fleet);
        operations.retain(|operation| !connector_only(operation));
        operations
    }

    /// The relay's operations the native CLI offers, by Python's formula,
    /// and [`CONNECTOR_ONLY`] under their own gates.
    fn offered(&self, fleet: bool) -> BTreeSet<&'static str> {
        let mut removed: BTreeSet<&str> = BTreeSet::new();
        let mut remove = |gate: bool, operations: &[&'static str]| {
            if !gate {
                removed.extend(operations.iter().copied());
            }
        };
        remove(self.worktree, WORKTREES);
        remove(self.analytics, &["analytics_read"]);
        remove(self.archive, &["archive_list"]);
        remove(self.directory_create, &["directory_create"]);
        remove(self.lifecycle, LIFECYCLE);
        remove(self.terminal, TERMINAL);
        let launch: Vec<&'static str> = LAUNCH
            .iter()
            .copied()
            .filter(|operation| *operation != "directory_create")
            .collect();
        remove(self.launch, &launch);
        remove(self.history, &["history"]);
        remove(self.history_projection, &["history_projection"]);
        remove(self.hook_health, &["hooks_status"]);
        remove(self.session_projects, crate::session_projects::OPERATIONS);
        remove(self.recovery, &["recovery_action"]);
        remove(self.read, &["mark_read"]);
        remove(self.session_notifications, &["session_notifications"]);
        remove(self.attachment_read, &["attachment_read"]);
        remove(self.file_read, &["file_read"]);
        remove(self.preference && fleet, &["set_mobile_delivery"]);
        remove(self.speech, SPEECH);
        remove(self.voice, VOICE);
        remove(self.speech_config, SPEECH_CONFIG);
        for row in CONNECTOR_ONLY {
            remove(row.gate.open(self), &[row.operation]);
        }
        OPERATIONS
            .iter()
            .copied()
            .filter(|operation| !removed.contains(operation))
            .filter(|operation| !self.windows_gateway || WINDOWS_OPERATIONS.contains(operation))
            .collect()
    }

    /// Python's features. `one_hop` is `fleet.available`.
    pub fn python_features(&self, one_hop: bool) -> BTreeSet<&'static str> {
        let mut features: BTreeSet<&str> = [
            "accounts_snapshot",
            "inspect_after",
            "inspect_agent",
            "send_agent",
        ]
        .into();
        if self.voice && self.voice_video {
            features.insert("voice_video");
        }
        if self.directory_create {
            features.insert("directory_create_v1");
        }
        if one_hop {
            features.insert("gateway_one_hop");
        }
        if self.session_projects {
            features.insert(crate::session_projects::FEATURE);
        }
        if self.project_launch {
            features.insert("launch_project");
        }
        if self.clear_context_dismiss {
            features.insert("clear_context_dismiss");
        }
        features
    }

    /// `mobile_capabilities` of the main connector, which has a fleet.
    pub fn advertised(&self, one_hop: bool) -> Value {
        self.advertised_for(true, one_hop)
    }

    /// `mobile_capabilities` as this build advertises it: Python's sets and
    /// [`CONNECTOR_ONLY`], limited to implemented operations, and the
    /// features they carry. A peer context has no fleet.
    pub fn advertised_for(&self, fleet: bool, one_hop: bool) -> Value {
        let operations: Vec<&str> = self
            .offered(fleet)
            .into_iter()
            .filter(|operation| implemented(operation))
            .collect();
        let features: Vec<&str> = self
            .python_features(one_hop)
            .into_iter()
            .filter(|feature| {
                FEATURE_OPERATIONS
                    .iter()
                    .find(|(name, _)| name == feature)
                    .is_some_and(|(_, operation)| operations.contains(operation))
            })
            .collect();
        let mut reasons = Map::new();
        if !self.lifecycle {
            reasons.insert(
                "session_actions".into(),
                json!("Native CLI lacks the scoped session-action ABI"),
            );
        }
        if !self.terminal {
            reasons.insert(
                "terminal".into(),
                json!("Native CLI lacks the scoped Terminal ABI"),
            );
        }
        let mut features = features;
        // Connector-only: the Python connector never advertises it, so apps
        // do not send an OpenCode effort to it.
        if operations.contains(&"launch") && !self.windows_gateway {
            features.push("opencode_launch_effort");
            features.sort_unstable();
        }
        if self.windows_gateway {
            features.retain(|feature| {
                ![
                    "send_agent",
                    "gateway_one_hop",
                    "launch_project",
                    "inspect_agent",
                ]
                .contains(feature)
            });
            reasons.insert("windows_scope".into(), json!("Existing local Codex, Claude and Kimi sessions; mobile creation, Windows paths, peers, DeepSeek/OpenCode and voice/speech are unavailable"));
        }
        json!({"protocol_version": 1, "operations": operations, "features": features,
               "reasons": reasons})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_markers_follow_python() {
        // "file-read <session> --json" holds "read <session> --json".
        let flags = Flags::from_help("hgs file-read <session> --json\n");
        assert!(flags.file_read && flags.read);
        assert!(!Flags::from_help("hgs swarm mobile-peers --json").peers);
        assert!(Flags::from_help("x session-action <session> --json y").lifecycle);
    }

    #[test]
    fn only_implemented_operations_and_their_features_are_advertised() {
        let all = Flags::from_help(
            "session-action <session> --json\nterminal <session> --json\nhistory <session> --json\n--launch-id\nMobile worktree ABI: worktrees-v1\ndirs --create <path>\nread <session> --json\n",
        );
        let advertised = all.advertised(false);
        assert_eq!(
            advertised["operations"],
            json!([
                "answer",
                "archive",
                "catalog",
                "clear_context",
                "compact_context",
                "directory_create",
                "dirs",
                "forget",
                "fork",
                "history",
                "inspect",
                "interrupt",
                "launch",
                "mark_read",
                "pause",
                "process_output",
                "process_stop",
                "remove_queued",
                "rename",
                "restore",
                "resume",
                "send",
                "send_now",
                "settings",
                "terminal_input",
                "terminal_snapshot",
                "terminate",
                "worktree_create",
                "worktrees"
            ])
        );
        assert_eq!(
            advertised["features"],
            json!([
                "accounts_snapshot",
                "directory_create_v1",
                "inspect_after",
                "inspect_agent",
                "opencode_launch_effort",
                "send_agent"
            ])
        );
        assert_eq!(advertised["reasons"], json!({}));
        let none = Flags::default().advertised(false);
        assert_eq!(
            none["operations"],
            json!([
                "answer",
                "clear_context",
                "compact_context",
                "inspect",
                "interrupt",
                "process_output",
                "process_stop",
                "send",
                "send_now",
                "settings"
            ])
        );
        assert_eq!(none["reasons"].as_object().unwrap().len(), 2);
    }

    #[test]
    fn connector_only_operations_follow_their_gate_and_are_never_pythons() {
        let advertises = |flags: Flags, fleet: bool, operation: &str| {
            flags.advertised_for(fleet, false)["operations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|advertised| advertised == operation)
        };
        for row in CONNECTOR_ONLY {
            assert!(implemented(row.operation) && OPERATIONS.contains(&row.operation));
            let open = Flags::from_help(&format!("{}\n", row.gate.help_line()));
            assert!(row.gate.open(&open) && !row.gate.open(&Flags::default()));
            for fleet in [false, true] {
                assert!(advertises(open, fleet, row.operation), "{row:?}");
                assert!(!advertises(Flags::default(), fleet, row.operation));
                assert!(!open.python_operations(fleet).contains(row.operation));
            }
        }
    }

    #[test]
    fn remove_queued_follows_the_session_action_abi_and_is_never_pythons() {
        let advertises = |flags: Flags| {
            flags.advertised(false)["operations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|operation| operation == "remove_queued")
        };
        let lifecycle = Flags::from_help("hgs session-action <session> --json\n");
        assert!(lifecycle.lifecycle && advertises(lifecycle));
        assert!(lifecycle.advertised_for(false, false)["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|operation| operation == "remove_queued"));
        // Every other marker without the session-action ABI never offers it.
        let others = Flags::from_help(
            "terminal <session> --json\nhistory <session> --json\n--launch-id\nread <session> --json\nrecovery action --scoped-json\n",
        );
        assert!(!others.lifecycle && !advertises(others));
        assert!(!advertises(Flags::default()));
        // Python's tables never list it, with or without the ABI.
        for flags in [lifecycle, others, Flags::default()] {
            for fleet in [false, true] {
                assert!(!flags.python_operations(fleet).contains("remove_queued"));
            }
        }
        assert_eq!(
            CONNECTOR_ONLY
                .iter()
                .filter(|row| row.operation == "remove_queued")
                .copied()
                .collect::<Vec<_>>(),
            [ConnectorOnly {
                operation: "remove_queued",
                gate: Gate::Lifecycle,
                class: Class::Mutation
            }]
        );
    }
}
