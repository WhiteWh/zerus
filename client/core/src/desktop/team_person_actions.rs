//! Versioned Teams Person plans. Serialized plans are data, never admission.
use super::{creation_clone::Scope, team_person};
use crate::upstream_teams::action::{TeamAction, TeamCall};
use base64::Engine;
use serde::{Deserialize, Serialize};

pub const HELP_MARKER: &str = "Desktop Teams Person actions ABI: desktop-team-person-actions-v2";
/// A service that resolves a window Route's kept task fields
/// (`keep_task_fields`, zerus-3u5yu). An older one refuses that plan; the
/// window asks for this marker first and says to update the service.
pub const PLACEMENT_MARKER: &str =
    "Desktop Teams Person placement fields: desktop-team-person-placement-v1";

/// The help markers the selected service must advertise for this plan.
pub fn required_markers(plan: &Plan) -> Vec<&'static str> {
    let keeps = matches!(&plan.action, Action::Team { change }
        if matches!(change.as_ref(), TeamAction::Route { keep_task_fields: true, .. }));
    if keeps {
        vec![HELP_MARKER, PLACEMENT_MARKER]
    } else {
        vec![HELP_MARKER]
    }
}
pub const BYTES: usize = 768 << 10;
pub const PLAN_BYTES: usize = 262_144;
pub const PIECE_BYTES: usize = 32 << 10;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub request_id: String,
    pub team: String,
    pub expected_revision: u64,
    pub action: Action,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Team {
        change: Box<TeamAction>,
    },
    NewTask {
        project: String,
        title: String,
        backlog: bool,
    },
    Intake {
        project: String,
        task: String,
        revision: String,
        role: Option<String>,
        pipeline: Option<String>,
    },
}

fn text(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

impl Plan {
    pub fn reference(&self) -> Option<Reference> {
        let bytes = serde_json::to_vec(self).ok()?;
        (self.valid() && bytes.len() <= PLAN_BYTES).then(|| Reference {
            request_id: self.request_id.clone(),
            team: self.team.clone(),
            expected_revision: self.expected_revision,
            sha256: digest(self).expect("bounded serialized plan"),
            bytes: bytes.len() as u64,
        })
    }
    pub fn from_call(call: &TeamCall) -> Option<Self> {
        let plan = Self {
            request_id: call.request_id.clone(),
            team: call.team.clone(),
            expected_revision: if !call.action.names_team() {
                0
            } else {
                call.expected_revision?
            },
            action: Action::Team {
                change: Box::new(call.action.clone()),
            },
        };
        plan.valid().then_some(plan)
    }

    /// The native dialog supplies consent separately. `confirmed` here only
    /// selects the existing pure input encoder, never an execution path.
    pub fn call(&self) -> Option<TeamCall> {
        let Action::Team { change } = &self.action else {
            return None;
        };
        Some(TeamCall {
            team: self.team.clone(),
            team_name: String::new(),
            action: change.as_ref().clone(),
            expected_revision: Some(self.expected_revision),
            confirmed: true,
            request_id: self.request_id.clone(),
        })
    }

    pub fn valid(&self) -> bool {
        let uuid = zerus_desktop_migration::canonical_uuid;
        if !uuid(&self.request_id)
            || !uuid(&self.team)
            || self.expected_revision > i64::MAX as u64
            || !serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= PLAN_BYTES)
        {
            return false;
        }
        match &self.action {
            Action::Team { change } => {
                // The existing CLI reserves Ask for a proved member session.
                // A new native Person lane must preserve that restriction.
                !matches!(change.as_ref(), TeamAction::Ask { .. })
                    && (change.names_team() || self.expected_revision == 0)
                    && self
                        .call()
                        .is_some_and(|call| crate::upstream_teams::action::prepare(&call).is_ok())
            }
            Action::NewTask { project, title, .. } => {
                text(project, 128) && text(title, 512) && !title.trim().is_empty()
            }
            Action::Intake {
                project,
                task,
                revision,
                role,
                pipeline,
            } => {
                text(project, 256)
                    && uuid(task)
                    && text(revision, 4096)
                    && role.as_deref().is_none_or(|value| text(value, 256))
                    && pipeline.as_deref().is_none_or(|value| text(value, 256))
            }
        }
    }
}

/// A new task-intake review asks the service for the canonical team revision
/// before confirmation. Recovery never calls this preparation again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDraft {
    pub request_id: String,
    pub team: String,
    pub action: Action,
}
impl TaskDraft {
    pub fn valid(&self) -> bool {
        let mut action = self.action.clone();
        if let Action::Intake { revision, .. } = &mut action {
            // Only the data-only read draft may leave this unset. The
            // returned Plan always binds the canonical task clock before
            // the native dialog, and Plan::valid still refuses an empty one.
            if revision.is_empty() {
                *revision = "unread draft".into();
            }
        }
        !matches!(action, Action::Team { .. })
            && Plan {
                request_id: self.request_id.clone(),
                team: self.team.clone(),
                expected_revision: 0,
                action,
            }
            .valid()
    }
    pub fn matches_plan(&self, plan: &Plan) -> bool {
        if !self.valid() || !plan.valid() {
            return false;
        }
        let mut expected = self.clone();
        if let (
            Action::Intake { revision, .. },
            Action::Intake {
                revision: actual, ..
            },
        ) = (&mut expected.action, &plan.action)
        {
            if revision.is_empty() {
                *revision = actual.clone();
            }
        }
        expected.plan(plan.expected_revision).as_ref() == Some(plan)
    }
    pub fn plan(&self, expected_revision: u64) -> Option<Plan> {
        let plan = Plan {
            request_id: self.request_id.clone(),
            team: self.team.clone(),
            expected_revision,
            action: self.action.clone(),
        };
        (self.valid() && plan.valid()).then_some(plan)
    }
}

/// Persisted data for one task original. It grants no execution authority:
/// reopening a form can only read the exact recorded original result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskOriginal {
    pub plan: Plan,
    pub intent: Intent,
}
impl TaskOriginal {
    pub fn valid(&self) -> bool {
        !matches!(self.plan.action, Action::Team { .. })
            && self.intent.valid()
            && self.plan.reference().as_ref() == Some(&self.intent.plan)
            && matches!(self.intent.store, StoreBinding::Existing { .. })
    }
}

/// Only this bounded reference crosses the original-child control line.
/// The full typed plan is staged as bounded pieces and compared byte-for-byte
/// by its digest before review. Staging data never creates an admission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub request_id: String,
    pub team: String,
    pub expected_revision: u64,
    pub sha256: String,
    pub bytes: u64,
}
impl Reference {
    pub fn valid(&self) -> bool {
        zerus_desktop_migration::canonical_uuid(&self.request_id)
            && zerus_desktop_migration::canonical_uuid(&self.team)
            && self.expected_revision <= i64::MAX as u64
            && super::digest(&self.sha256)
            && (1..=PLAN_BYTES as u64).contains(&self.bytes)
    }
}

/// A data-only native staging request. Its typed consumer cannot decide a
/// team operation; the separate action original is registered after consent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Piece {
    pub request_id: String,
    pub scope: Scope,
    pub plan: Reference,
    pub ordinal: u16,
    pub payload: String,
}
impl Piece {
    pub fn from_bytes(
        request_id: String,
        scope: Scope,
        plan: Reference,
        ordinal: u16,
        bytes: &[u8],
    ) -> Option<Self> {
        if bytes.len() > PIECE_BYTES {
            return None;
        }
        let piece = Self {
            request_id,
            scope,
            plan,
            ordinal,
            payload: base64::engine::general_purpose::STANDARD.encode(bytes),
        };
        piece.valid().then_some(piece)
    }
    pub fn valid(&self) -> bool {
        zerus_desktop_migration::canonical_uuid(&self.request_id)
            && self.scope.valid()
            && self.plan.valid()
            && (self.ordinal as usize) < (self.plan.bytes as usize).div_ceil(PIECE_BYTES)
            && self.payload.len() <= PIECE_BYTES.div_ceil(3) * 4
            && self.decoded().is_some()
    }
    pub fn decoded(&self) -> Option<Vec<u8>> {
        let offset = (self.ordinal as usize).checked_mul(PIECE_BYTES)?;
        let remaining = (self.plan.bytes as usize).checked_sub(offset)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&self.payload)
            .ok()?;
        (bytes.len() == remaining.min(PIECE_BYTES)).then_some(bytes)
    }
}

/// Absence binds the existing canonical state object, before first Create.
/// An original receipt separately records the database object actually born.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum StoreBinding {
    Existing { fingerprint: String },
    Absent { root: String },
}
impl StoreBinding {
    fn valid(&self) -> bool {
        match self {
            Self::Existing { fingerprint } => super::digest(fingerprint),
            Self::Absent { root } => super::digest(root),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    pub scope: Scope,
    pub machine: String,
    pub store: StoreBinding,
    pub facts: String,
    /// Digest of the canonical store request, including a resolved template.
    pub request: String,
    pub plan: Reference,
}
impl Intent {
    pub fn valid(&self) -> bool {
        self.scope.valid()
            && zerus_desktop_migration::canonical_uuid(&self.machine)
            && self.store.valid()
            && super::digest(&self.facts)
            && super::digest(&self.request)
            && self.plan.valid()
    }
}

/// All captions and target descriptions are supplied by canonical reads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub intent: Intent,
    pub team_name: String,
    pub title: String,
    pub subject: String,
    pub details: Vec<(String, String)>,
}
impl Review {
    pub fn valid(&self) -> bool {
        self.intent.valid()
            && text(&self.team_name, 4096)
            && text(&self.title, 256)
            && self.subject.len() <= 8192
            && self.details.len() <= 64
            && self
                .details
                .iter()
                .all(|(key, value)| text(key, 128) && value.len() <= 8192)
            && serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= BYTES)
    }
}

pub fn digest(value: &impl Serialize) -> Option<String> {
    team_person::digest(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plan() -> Plan {
        Plan {
            request_id: uuid::Uuid::new_v4().to_string(),
            team: uuid::Uuid::new_v4().to_string(),
            expected_revision: 2,
            action: Action::Team {
                change: Box::new(TeamAction::RenameTeam {
                    name: "A reviewed team name".into(),
                }),
            },
        }
    }

    #[test]
    fn plans_are_closed_data_and_do_not_accept_person_claims() {
        let plan = plan();
        assert!(plan.valid());
        for field in ["actor", "origin", "confirmed", "permit"] {
            let mut value = json!(plan);
            value[field] = json!(true);
            assert!(serde_json::from_value::<Plan>(value).is_err(), "{field}");
            let mut value = json!(plan);
            value["action"]["change"][field] = json!(true);
            assert!(serde_json::from_value::<Plan>(value).is_err(), "{field}");
        }
        let mut changed = plan.clone();
        changed.action = Action::Team {
            change: Box::new(TeamAction::RenameTeam {
                name: "Another name".into(),
            }),
        };
        assert_ne!(digest(&plan), digest(&changed));
        changed.request_id = "not-a-request".into();
        assert!(!changed.valid());
    }

    #[test]
    fn both_creation_kinds_pin_zero_revision_without_a_prior_team() {
        let action = TeamAction::StartPlanning {
            template: "queue-default".into(),
            project: "example".into(),
            name: "Reviewed team".into(),
            goal: "Prepare the initial plan".into(),
            home: uuid::Uuid::new_v4().to_string(),
            integration_branch: "teams/example".into(),
            base_commit: "a".repeat(40),
            integration: zerus_team_plan::model::Integration::Branch,
            expected_template: "b".repeat(64),
            delegations: serde_json::from_value(json!({
                "accept_results":false,"answer_members":false,"confirm_trust":false,
                "resume_exited":false,"merge_accepted":false
            }))
            .unwrap(),
        };
        for kind in ["start_planning", "create"] {
            let mut value = serde_json::to_value(&action).unwrap();
            value["kind"] = serde_json::json!(kind);
            let call = TeamCall {
                team: uuid::Uuid::new_v4().to_string(),
                team_name: String::new(),
                request_id: uuid::Uuid::new_v4().to_string(),
                action: serde_json::from_value(value).unwrap(),
                expected_revision: None,
                confirmed: false,
            };
            let mut plan = Plan::from_call(&call).unwrap();
            assert_eq!(plan.expected_revision, 0);
            assert!(plan.valid());
            plan.expected_revision = 1;
            assert!(!plan.valid());
        }
    }

    #[test]
    fn task_draft_may_request_a_fresh_read_but_an_executable_plan_requires_the_exact_revision() {
        let draft = TaskDraft {
            request_id: uuid::Uuid::new_v4().to_string(),
            team: uuid::Uuid::new_v4().to_string(),
            action: Action::Intake {
                project: "example".into(),
                task: uuid::Uuid::new_v4().to_string(),
                revision: String::new(),
                role: None,
                pipeline: None,
            },
        };
        assert!(
            draft.valid(),
            "a read-only preparation can ask for the canonical revision"
        );
        assert!(
            draft.plan(8).is_none(),
            "a draft has no executable plan before its exact task read"
        );
        let mut exact = draft.clone();
        if let Action::Intake { revision, .. } = &mut exact.action {
            *revision = "a".repeat(64);
        }
        let plan = exact.plan(8).unwrap();
        assert!(draft.matches_plan(&plan));
        assert!(exact.matches_plan(&plan));
        let mut changed = plan.clone();
        if let Action::Intake { revision, .. } = &mut changed.action {
            *revision = "b".repeat(64);
        }
        assert!(
            !exact.matches_plan(&changed),
            "a saved original never adopts another task clock"
        );
    }
    #[test]
    fn task_preparation_and_saved_originals_are_data_and_bind_the_exact_plan() {
        let draft = TaskDraft {
            request_id: uuid::Uuid::new_v4().to_string(),
            team: uuid::Uuid::new_v4().to_string(),
            action: Action::NewTask {
                project: "example".into(),
                title: "Reviewed task".into(),
                backlog: false,
            },
        };
        assert!(draft.valid());
        let plan = draft.plan(7).unwrap();
        let intent = Intent {
            scope: Scope {
                configuration: "a".repeat(64),
                service_epoch: uuid::Uuid::new_v4().to_string(),
            },
            machine: uuid::Uuid::new_v4().to_string(),
            store: StoreBinding::Existing {
                fingerprint: "b".repeat(64),
            },
            facts: "c".repeat(64),
            request: "d".repeat(64),
            plan: plan.reference().unwrap(),
        };
        let mut original = TaskOriginal { plan, intent };
        assert!(original.valid());
        original.plan.expected_revision += 1;
        assert!(!original.valid());
        let mut raw = json!(draft);
        raw["actor"] = json!("person");
        assert!(serde_json::from_value::<TaskDraft>(raw).is_err());
        let mut wrong = draft;
        wrong.action = Action::Team {
            change: Box::new(TeamAction::RenameTeam {
                name: "Different".into(),
            }),
        };
        assert!(
            !wrong.valid(),
            "task preparation cannot discover a revision for a general team action"
        );
    }
    #[test]
    fn native_responder_requires_service_mode_and_exact_configuration_for_both_kinds() {
        use crate::desktop::{ownership::Mode, ui_requests::Request, Build, Responder};
        const CASE: &str = "desktop::team_person_actions::tests::native_responder_requires_service_mode_and_exact_configuration_for_both_kinds";
        if std::env::var_os("ZERUS_TEAM_ACTION_RESPONDER_CHILD").is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", CASE, "--test-threads=1", "--nocapture"])
                .env_remove("HGS_SESSION")
                .env_remove("HGS_SESSION_ID")
                .env_remove("HGS_RUN_ID")
                .env("ZERUS_TEAM_ACTION_RESPONDER_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed; 0 failed"));
            return;
        }
        let plan = plan();
        let scope = Scope {
            configuration: "a".repeat(64),
            service_epoch: uuid::Uuid::new_v4().to_string(),
        };
        let intent = Intent {
            scope: scope.clone(),
            machine: uuid::Uuid::new_v4().to_string(),
            store: StoreBinding::Existing {
                fingerprint: "b".repeat(64),
            },
            facts: "c".repeat(64),
            request: "d".repeat(64),
            plan: plan.reference().unwrap(),
        };
        let piece = Piece {
            request_id: uuid::Uuid::new_v4().to_string(),
            scope,
            plan: intent.plan.clone(),
            ordinal: 0,
            payload: base64::engine::general_purpose::STANDARD
                .encode(serde_json::to_vec(&plan).unwrap()),
        };
        for request in [
            Request::TeamPersonAction {
                intent: Box::new(intent),
            },
            Request::TeamPersonPlanPiece {
                piece: Box::new(piece),
            },
        ] {
            assert!(crate::desktop::ui_requests::Envelope {
                abi: crate::desktop::ui_requests::ABI.into(),
                attempt: uuid::Uuid::new_v4().to_string(),
                request: request.clone(),
            }
            .valid());
            for (configuration, mode, accepted) in [
                ("a".repeat(64), None, false),
                ("a".repeat(64), Some(Mode::Native), false),
                ("f".repeat(64), Some(Mode::Service), false),
                ("a".repeat(64), Some(Mode::Service), true),
            ] {
                let responder = Responder::new(
                    "com.hgdev.hgs-tray".into(),
                    Build {
                        version: "1.2.3".into(),
                        source_commit: "a".repeat(40),
                        source_dirty: Some(false),
                    },
                    uuid::Uuid::new_v4().to_string(),
                    true,
                    false,
                );
                assert!(responder.configure_once(configuration));
                if let Some(mode) = mode {
                    assert!(responder.configure_ownership_mode_once(mode));
                }
                assert_eq!(
                    responder
                        .prepare_ui_request(uuid::Uuid::new_v4().to_string(), request.clone())
                        .is_ok(),
                    accepted
                );
            }
        }
    }

    /// zerus-3u5yu (3): a window Route that keeps the task's fields needs a
    /// service that reads them; the window asks for its marker before staging.
    #[test]
    fn a_window_route_needs_the_placement_marker_and_nothing_else_does() {
        let route = |keep_task_fields| Action::Team {
            change: Box::new(TeamAction::Route {
                node: 2,
                role: "developer".into(),
                pipeline: "reviewed".into(),
                keep_task_fields,
                deps: Vec::new(),
                writes: Vec::new(),
                assignee: None,
            }),
        };
        let mut kept = plan();
        kept.action = route(true);
        assert_eq!(required_markers(&kept), [HELP_MARKER, PLACEMENT_MARKER]);
        let mut whole = plan();
        whole.action = route(false);
        assert_eq!(required_markers(&whole), [HELP_MARKER]);
        assert_eq!(required_markers(&plan()), [HELP_MARKER]);
        assert!(PLACEMENT_MARKER.starts_with("Desktop Teams Person placement"));
    }
}
