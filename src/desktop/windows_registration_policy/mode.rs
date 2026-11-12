//! Bounded installer metadata, never a caller or ownership permission.
use super::*;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

pub(in crate::desktop) const RECORD_BYTES: usize = 8192;
pub(in crate::desktop) mod first;
pub(in crate::desktop) mod refresh;
pub(in crate::desktop) mod review;
pub(in crate::desktop) mod timing;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::desktop) enum Mode {
    Passive,
    Background,
}
impl Mode {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "passive" => Ok(Self::Passive),
            "background" => Ok(Self::Background),
            _ => bail!("Mode must be background or passive."),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Passive => "passive",
            Self::Background => "background",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::desktop) enum Phase {
    Prepared,
    RevokeOffered,
    Held,
    StopOffered,
    Stopped,
    UpdateOffered,
    Updated,
    StartOffered,
    Ready,
}
impl Phase {
    pub fn pre_manager(self) -> bool {
        matches!(self, Self::Prepared | Self::RevokeOffered | Self::Held)
    }
}

/// Each logical phase gets a fresh bound. The outer original-child exchange
/// remains within the shared runner's existing 30-second maximum.
#[derive(Clone, Copy, Debug)]
pub(in crate::desktop) enum Budget {
    Preflight,
    Rollback,
    Hold,
    Stop,
    Update,
    Start,
    Readiness,
    Release,
}
impl Budget {
    pub const ALL: [Self; 8] = [
        Self::Preflight,
        Self::Rollback,
        Self::Hold,
        Self::Stop,
        Self::Update,
        Self::Start,
        Self::Readiness,
        Self::Release,
    ];
    pub fn duration(self) -> Duration {
        Duration::from_secs(match self {
            Self::Preflight => 2,
            Self::Rollback => 6,
            Self::Hold => 3,
            Self::Stop | Self::Update | Self::Start | Self::Release => 2,
            Self::Readiness => 8,
        })
    }
    pub fn deadline(self, now: Instant) -> Instant {
        now + self.duration()
    }
    pub fn exchange_timeout() -> Duration {
        Self::ALL.into_iter().map(Self::duration).sum::<Duration>() + Duration::from_secs(3)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::desktop) struct ObservedInstance {
    pub guid: String,
    pub pid: u32,
    pub birth: u64,
    pub epoch: String,
}
impl ObservedInstance {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            uuid::Uuid::parse_str(&self.guid).is_ok_and(|id| !id.is_nil())
                && self.pid > 0
                && self.birth > 0
                && canonical_id(&self.epoch),
            "Invalid observed task identity."
        );
        Ok(())
    }
}

fn canonical_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|uuid| !uuid.is_nil() && uuid.to_string() == id)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::desktop) struct Pending {
    pub request_id: String,
    pub desired: Mode,
    pub phase: Phase,
    pub original: Option<ObservedInstance>,
    pub started: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumer: Option<review::Consumer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<review::Disposition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<refresh::Scope>,
}
impl Pending {
    fn prepared(request_id: String, desired: Mode, original: Option<ObservedInstance>) -> Self {
        Self { request_id, desired, phase: Phase::Prepared, original, started: None,
            consumer: None, review: None, refresh: None }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::desktop) struct Record {
    pub schema: u8,
    pub configuration: String,
    pub sid: String,
    pub command: Vec<String>,
    pub pending: Option<Pending>,
    pub cancelled: Option<String>,
    pub completed: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviewed: Vec<review::Archive>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<review::Receipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub first_choices: Vec<first::Receipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refreshes: Vec<refresh::Receipt>,
}
impl Record {
    pub fn empty(plan: &Plan) -> Result<Self> {
        ensure!(
            plan.argv.len() == 7 || (plan.argv.len() == 8 && plan.argv[7] == "--background"),
            "Invalid maintained desktop command."
        );
        Ok(Self {
            schema: 1,
            configuration: plan.configuration.clone(),
            sid: plan.sid.clone(),
            command: plan.argv[..7].to_vec(),
            pending: None,
            cancelled: None,
            completed: None,
            reviewed: Vec::new(),
            choices: Vec::new(),
            first_choices: Vec::new(),
            refreshes: Vec::new(),
        })
    }
    pub fn decode(bytes: &[u8], plan: &Plan) -> Result<Self> {
        ensure!(
            bytes.len() <= RECORD_BYTES,
            "Desktop mode record exceeds its bound."
        );
        let value: Self = serde_json::from_slice(bytes)
            .context("Invalid desktop mode record; inspect it before changing the task.")?;
        let expected = Self::empty(plan)?;
        ensure!(
            value.schema == 1
                && value.configuration == expected.configuration
                && value.sid == expected.sid
                && value.command == expected.command,
            "Desktop mode record selects another registration."
        );
        if let Some(pending) = &value.pending {
            ensure!(
                canonical_id(&pending.request_id),
                "Invalid pending transition UUID."
            );
            if let Some(original) = &pending.original {
                original.validate()?;
            }
            if let Some(started) = &pending.started {
                ensure!(
                    uuid::Uuid::parse_str(started).is_ok_and(|id| !id.is_nil()),
                    "Invalid original started task UUID."
                );
            }
        }
        ensure!(
            value.cancelled.as_deref().is_none_or(canonical_id),
            "Invalid cancelled transition UUID."
        );
        ensure!(
            value.completed.as_deref().is_none_or(canonical_id),
            "Invalid completed transition UUID."
        );
        value.check_reviews()?;
        value.check_first()?;
        value.check_refreshes()?;
        Ok(value)
    }
    pub fn encode(&self, plan: &Plan) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec(self)?;
        Self::decode(&bytes, plan)?;
        Ok(bytes)
    }
    pub fn begin(&mut self, desired: Mode, original: Option<ObservedInstance>) -> Result<&Pending> {
        self.begin_id(desired, original, uuid::Uuid::new_v4().to_string())
    }
    pub fn begin_id(&mut self, desired: Mode, original: Option<ObservedInstance>, id: String) -> Result<&Pending> {
        self.ensure_review_slot()?;
        ensure!(canonical_id(&id) && self.completed.as_deref() != Some(id.as_str())
            && self.cancelled.as_deref() != Some(id.as_str())
            && !self.reviewed.iter().any(|a|a.original.request_id == id)
            && !self.refreshes.iter().any(|r| r.request == id), "A NEW original request UUID is required.");
        ensure!(self.pending.is_none(), "An original transition is pending; inspect it or explicitly cancel its exact UUID before a new choice.");
        if let Some(original) = &original {
            original.validate()?;
        }
        let pending = Pending::prepared(id, desired, original);
        self.ensure_review_capacity(&pending)?;
        self.pending = Some(pending);
        Ok(self.pending.as_ref().expect("inserted"))
    }
    pub fn advance(&mut self, expected: Phase, next: Phase) -> Result<()> {
        let pending = self
            .pending
            .as_mut()
            .context("Original transition is absent.")?;
        ensure!(
            pending.review.is_none() && pending.phase == expected,
            "Original transition phase changed."
        );
        let allowed = matches!(
            (expected, next),
            (Phase::Prepared, Phase::RevokeOffered | Phase::Held)
                | (Phase::RevokeOffered, Phase::Held)
                | (Phase::Held, Phase::StopOffered | Phase::Stopped)
                | (Phase::StopOffered, Phase::Stopped)
                | (Phase::Stopped, Phase::UpdateOffered)
                | (Phase::UpdateOffered, Phase::Updated)
                | (Phase::Updated, Phase::StartOffered)
                | (Phase::StartOffered, Phase::Ready)
        );
        let allowed = if pending.refresh.is_some() {
            matches!((expected, next),
                (Phase::Prepared, Phase::Held)
                | (Phase::Held, Phase::StopOffered)
                | (Phase::StopOffered, Phase::Stopped)
                | (Phase::Stopped, Phase::StartOffered)
                | (Phase::StartOffered, Phase::Ready))
        } else { allowed };
        ensure!(allowed, "A manager effect cannot be replayed or skipped.");
        pending.phase = next;
        Ok(())
    }
    /// A policy requirement, never a grant or native permit. The caller checks
    /// this actual grant before and after the record-only passive cancellation.
    pub fn cancellation_requires_grant(&self, request_id: &str) -> Result<bool> {
        let pending = self
            .pending
            .as_ref()
            .context("No original transition is pending.")?;
        ensure!(
            pending.review.is_none() && canonical_id(request_id) && pending.request_id == request_id,
            "Pending transition UUID changed."
        );
        ensure!(pending.phase.pre_manager(),
            "A manager effect was already offered; inspect its unknown result rather than discarding the original identity.");
        Ok(pending.desired == Mode::Passive)
    }
    /// Call only after fresh native caller and, for passive rollback, current
    /// Service-grant checks under the installer lock. No native side effect.
    pub fn cancel(&mut self, request_id: &str) -> Result<()> {
        self.cancellation_requires_grant(request_id)?;
        let refresh = self.pending_refresh();
        self.choice_phase(request_id, review::ChoicePhase::Refused)?;
        if !refresh { self.cancelled = Some(request_id.into()); }
        self.pending = None;
        Ok(())
    }
    /// Select metadata for a read-only native observation, never permission to
    /// discard it. The caller must bind this GUID to the ready requested mode.
    pub fn ready_cancellation(&self, request_id: &str) -> Result<&Pending> {
        ensure!(!self.pending_refresh(), "An original refresh is lookup-only after an offered effect; review its original UUID.");
        ensure!(!self.choices.iter().any(|c| c.request == request_id)
            && !self.first_choices.iter().any(|c| c.request == request_id),
            "Use Review interrupted mode change for this original; cancellation cannot discard its unknown custody.");
        let pending = self
            .pending
            .as_ref()
            .context("No original transition is pending.")?;
        ensure!(
            pending.review.is_none() && canonical_id(request_id) && pending.request_id == request_id,
            "Pending transition UUID changed."
        );
        ensure!(matches!(pending.phase, Phase::StartOffered | Phase::Ready)
            && pending.started.is_some(),
            "An offered transition can be cancelled only after its recorded original start is verified ready.");
        Ok(pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plan() -> Plan {
        super::super::tests::plan("check")
    }
    #[test]
    fn explicit_mode_names_are_closed_and_round_trip_without_enabling_a_default() {
        for value in ["passive", "background"] {
            assert_eq!(Mode::parse(value).unwrap().name(), value);
        }
        for value in ["", "auto", "service", "Background"] {
            assert!(Mode::parse(value).is_err());
        }
    }
    #[test]
    fn readiness_has_a_fresh_budget_after_rollback_and_the_exchange_remains_bounded() {
        let start = Instant::now();
        let after_rollback = Budget::Rollback.deadline(start);
        let ready = Budget::Readiness.deadline(after_rollback);
        assert_eq!(
            ready.duration_since(after_rollback),
            Budget::Readiness.duration()
        );
        assert_eq!(Budget::Readiness.duration(), Duration::from_secs(8));
        assert_eq!(Budget::exchange_timeout(), Duration::from_secs(30));
        assert_eq!(
            Budget::ALL
                .into_iter()
                .map(Budget::duration)
                .sum::<Duration>(),
            Duration::from_secs(27)
        );
    }
    #[test]
    fn either_pre_manager_mode_can_be_cancelled_with_its_own_grant_requirement() {
        for mode in [Mode::Passive, Mode::Background] {
            for phase in [Phase::Prepared, Phase::RevokeOffered, Phase::Held] {
                let mut record = Record::empty(&plan()).unwrap();
                let id = record.begin(mode, None).unwrap().request_id.clone();
                record.pending.as_mut().unwrap().phase = phase;
                assert_eq!(
                    record.cancellation_requires_grant(&id).unwrap(),
                    mode == Mode::Passive
                );
                record.cancel(&id).unwrap();
                assert!(record.pending.is_none());
                assert_eq!(record.cancelled.as_deref(), Some(id.as_str()));
            }
        }
    }
    #[test]
    fn pending_identity_never_rotates_or_replays_an_offered_manager_phase() {
        let mut record = Record::empty(&plan()).unwrap();
        let original = record
            .begin(Mode::Passive, None)
            .unwrap()
            .request_id
            .clone();
        record
            .advance(Phase::Prepared, Phase::RevokeOffered)
            .unwrap();
        for desired in [Mode::Passive, Mode::Background] {
            assert!(record.begin(desired, None).is_err());
        }
        assert_eq!(record.pending.as_ref().unwrap().request_id, original);
        record.advance(Phase::RevokeOffered, Phase::Held).unwrap();
        record.advance(Phase::Held, Phase::StopOffered).unwrap();
        let bytes = record.encode(&plan()).unwrap();
        let mut restored = Record::decode(&bytes, &plan()).unwrap();
        assert!(restored.advance(Phase::Held, Phase::StopOffered).is_err());
        assert!(restored
            .advance(Phase::StopOffered, Phase::StopOffered)
            .is_err());
        restored
            .advance(Phase::StopOffered, Phase::Stopped)
            .unwrap();
        assert_eq!(restored.pending.unwrap().request_id, original);
    }
    #[test]
    fn only_exact_record_cancellation_allows_a_separately_explicit_new_intent() {
        let mut record = Record::empty(&plan()).unwrap();
        let original = record
            .begin(Mode::Passive, None)
            .unwrap()
            .request_id
            .clone();
        let saved = record.clone();
        assert!(record.cancel(&uuid::Uuid::new_v4().to_string()).is_err());
        assert_eq!(record, saved);
        record.cancel(&original).unwrap();
        assert_eq!(record.cancelled.as_deref(), Some(original.as_str()));
        let next = record
            .begin(Mode::Passive, None)
            .unwrap()
            .request_id
            .clone();
        assert_ne!(next, original);
        assert_eq!(record.command, saved.command);
    }
    #[test]
    fn malformed_foreign_or_unbounded_metadata_never_resets_the_default() {
        let record = Record::empty(&plan()).unwrap();
        let raw = serde_json::to_value(&record).unwrap();
        for (field, value) in [
            ("schema", json!(2)),
            ("configuration", json!("b".repeat(64))),
            ("sid", json!("S-1-5-21-9999")),
            ("command", json!([])),
            ("permission", json!(true)),
        ] {
            let mut other = raw.clone();
            other[field] = value;
            assert!(
                Record::decode(&serde_json::to_vec(&other).unwrap(), &plan()).is_err(),
                "{field}"
            );
        }
        assert!(Record::decode(&vec![b' '; RECORD_BYTES + 1], &plan()).is_err());
        assert!(Record::decode(b"{", &plan()).is_err());
        assert_eq!(
            Record::decode(&record.encode(&plan()).unwrap(), &plan()).unwrap(),
            record
        );
    }
    #[test]
    fn an_unknown_stop_update_or_start_cannot_be_discarded_as_an_unsent_rollback() {
        for phase in [
            Phase::StopOffered,
            Phase::Stopped,
            Phase::UpdateOffered,
            Phase::Updated,
            Phase::StartOffered,
            Phase::Ready,
        ] {
            let mut record = Record::empty(&plan()).unwrap();
            let id = record
                .begin(Mode::Passive, None)
                .unwrap()
                .request_id
                .clone();
            record.pending.as_mut().unwrap().phase = phase;
            let saved = record.clone();
            assert!(record.cancel(&id).is_err());
            assert_eq!(record, saved);
        }
    }
    #[test]
    fn offered_cancellation_selects_only_the_exact_recorded_start_for_native_observation() {
        for mode in [Mode::Passive, Mode::Background] {
            for phase in [
                Phase::Prepared,
                Phase::RevokeOffered,
                Phase::Held,
                Phase::StopOffered,
                Phase::Stopped,
                Phase::UpdateOffered,
                Phase::Updated,
                Phase::StartOffered,
                Phase::Ready,
            ] {
                let mut record = Record::empty(&plan()).unwrap();
                let id = record.begin(mode, None).unwrap().request_id.clone();
                record.pending.as_mut().unwrap().phase = phase;
                assert!(
                    record.ready_cancellation(&id).is_err(),
                    "missing GUID: {phase:?}"
                );
                record.pending.as_mut().unwrap().started = Some(uuid::Uuid::new_v4().to_string());
                let saved = record.clone();
                assert!(record
                    .ready_cancellation(&uuid::Uuid::new_v4().to_string())
                    .is_err());
                assert_eq!(
                    record.ready_cancellation(&id).is_ok(),
                    matches!(phase, Phase::StartOffered | Phase::Ready)
                );
                assert_eq!(
                    record, saved,
                    "observation selection cannot cancel metadata"
                );
            }
        }
    }
}
