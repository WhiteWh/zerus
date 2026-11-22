//! Held decisions use the existing retry rows, with their own node and text.
//! A hold is a decision and its generation; its reason is the sentence of
//! the newest observation. The hold's age opens the first card, a reason
//! replaces a kept card only once it has stood for the period, and the card
//! closes when the hold ends.
use super::*;

/// How long a hold stays silent, and how long a changed reason must stand
/// before it replaces the kept card: the third one-minute check.
const PERIOD: i64 = 180_000;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Hold {
    /// The first pass that found this decision and generation held.
    start: i64,
    /// The newest reason, standing since `since`.
    reason: String,
    since: i64,
}

impl Runtime {
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(in crate::desktop::teams) fn test_hold_scoped(
        &mut self,
        store: &mut Store,
        team: &Team,
        hold: (&str, u64, u32, Option<&str>),
        observed: (&str, i64),
    ) -> Result<()> {
        self.for_row(|actor| actor.hold_scoped(store, team, hold, observed, || {}))
    }

    pub(in crate::desktop::teams::actor) fn held_step(
        &mut self,
        store: &mut Store,
        team: &Team,
        target: (&str, u32, u32, u64, &str),
        ready: bool,
        now: i64,
    ) -> Result<bool> {
        let (step, node, generation, decision, kind) = target;
        let key = format!("held:{step}:{node}");
        if ready {
            self.recovered(store, team, &key, now)?;
            return Ok(true);
        }
        let observation = store.task_observation(&team.id, node)?;
        let reason = sentence(node, kind, decision, observation.as_ref());
        self.for_row(|a| a.hold_write(store, team, (&key, decision, generation), &reason, now))?;
        Ok(false)
    }
    pub(super) fn hold_write(
        &mut self,
        store: &mut Store,
        team: &Team,
        hold: (&str, u64, u32),
        reason: &str,
        now: i64,
    ) -> Result<()> {
        let (key, decision, generation) = hold;
        self.hold_scoped(store, team, (key, decision, generation, None), (reason, now), || {})
    }

    pub(super) fn hold_scoped(
        &mut self,
        store: &mut Store,
        team: &Team,
        hold: (&str, u64, u32, Option<&str>),
        observed: (&str, i64),
        changed: impl FnOnce(),
    ) -> Result<()> {
        let (key, decision, generation, binding) = hold;
        let (reason, now) = observed;
        let name = format!("gate:{key}");
        let mut row = self.ledger.get(&team.id, &name)?;
        let close_reason = row.as_ref().and_then(|row| match held_by(row) {
            // Unscoped legacy rows could store a reason in the third field.
            Some((_, _, previous)) if binding.is_some() && previous.as_deref() != binding => {
                Some("The member's session binding changed")
            }
            Some((d, g, _)) if (d, g) != (decision, generation) => {
                Some("The decision or generation for this check changed")
            }
            Some(_) => None,
            None => Some("The previous hold could not be verified"),
        });
        if let Some(close_reason) = close_reason {
            self.close_gate(store, team, key, close_reason, now)?;
            row = self.ledger.get(&team.id, &name)?;
            if row.is_some() {
                return Ok(());
            }
        }
        let mut g: Gate = row
            .as_ref()
            .map(|r| serde_json::from_value(r.data.clone()))
            .transpose()
            .map_err(crate::teams::store::storage)?
            .unwrap_or_default();
        g.revision = serde_json::to_string(&(decision, generation, binding))
            .map_err(crate::teams::store::storage)?;
        let new_reason = g.hold.as_ref().is_none_or(|h| h.reason != reason);
        let h = g.hold.get_or_insert_with(|| Hold {
            start: now,
            reason: reason.into(),
            since: now,
        });
        if h.reason != reason {
            h.reason = reason.into();
            h.since = now;
        }
        let h = h.clone();
        if let Some(id) = g.card {
            // Retry checks again on this pass, as it does for every gate.
            if store
                .card(&team.id, id)?
                .is_some_and(|c| c.state != "open" && c.decision.as_deref() == Some("retry"))
            {
                g.card = None;
                g.notice = None;
            }
        }
        let data = serde_json::to_value(&g).map_err(crate::teams::store::storage)?;
        let mut row = match row {
            Some(row) if row.data == data => row,
            row => match self.reserve_gate(&team.id, &name, row, &g)? {
                Some(row) => row,
                None => return Ok(()),
            },
        };
        if new_reason {
            changed();
        }
        // A pending opener first replays its own receipt; the second
        // iteration can then replace a card whose reason has moved on.
        for _ in 0..2 {
            if !self.current_gate_card(store, team, &mut row, &mut g, now)? {
                return Ok(());
            }
            if g.card.is_some() {
                if g.event.as_deref() == Some(reason) || now.saturating_sub(h.since) < PERIOD {
                    return Ok(());
                }
                let open = g
                    .card
                    .map(|id| store.card(&team.id, id))
                    .transpose()?
                    .flatten()
                    .is_some_and(|c| c.state == "open");
                let close = if unreadable::key_is_unreadable(key) {
                    "What cannot be read about this member changed; its new card states it."
                } else {
                    "The reason this task waits changed; its new card states it."
                };
                if !self.replace_gate_card(
                    store,
                    team,
                    (&mut row, &mut g),
                    open.then_some(close),
                    now,
                )? {
                    return Ok(());
                }
            } else if g.notice.is_none() && now.saturating_sub(h.start) < PERIOD {
                return Ok(());
            }
            if g.notice.is_none() {
                g.event = Some(reason.into());
            }
            if !self.open_gate_card(store, team, (key, &problem(reason)), &mut row, &mut g, now)? {
                return Ok(());
            }
        }
        Ok(())
    }
}
/// The decision and generation a held row stands for, also in the rows of
/// earlier builds, whose stamp carried the reason as well.
pub(super) fn held_by(row: &Row) -> Option<(u64, u32, Option<String>)> {
    serde_json::from_str(row.data.get("revision")?.as_str()?).ok()
}
impl Runtime {
    pub(in crate::desktop::teams::actor) fn held_retired(
        &self,
        store: &Store,
        team: &Team,
        row: &Row,
        step: &str,
        node: u32,
        reopens: &std::collections::BTreeSet<u64>,
    ) -> Result<bool> {
        let gate: Gate =
            serde_json::from_value(row.data.clone()).map_err(crate::teams::store::storage)?;
        let (decision, generation, _): (u64, u32, Option<String>) =
            serde_json::from_str(&gate.revision).map_err(crate::teams::store::storage)?;
        let Some(n) = store.node(&team.id, node)? else {
            return Ok(true);
        };
        if store.generation(&team.id, node)? != Some(generation) {
            return Ok(true);
        }
        if step == "reopen" {
            return Ok(!reopens.contains(&decision));
        }
        if !store.ready_accept(&team.id, node, generation, decision)? {
            return Ok(true);
        }
        Ok(if step == "merge" {
            team.settings.integration != Integration::Branch || n.integrated
        } else {
            team.settings.integration == Integration::Branch
                && n.kind != NodeKind::Stage
                && !n.integrated
        })
    }
}

fn sentence(node: u32, kind: &str, decision: u64, o: Option<&ops::TaskObservation>) -> String {
    let seen = match o {
        Some(o) if o.attempt_conflict => "shows executions that conflict".into(),
        Some(o) if o.stage == ops::TaskStage::InReview => {
            "is in review under another execution than the team's".into()
        }
        Some(o) => format!("shows the task at {}", stage(o.stage)),
        None => "has no readable observation of the bound task".into(),
    };
    format!("Task {} waits for {} decision {}. Its newest observation {}. It continues automatically when this is settled on the board.", node, kind, decision, seen)
}
fn stage(stage: ops::TaskStage) -> &'static str {
    match stage {
        ops::TaskStage::Backlog => "Backlog",
        ops::TaskStage::Todo => "To do",
        ops::TaskStage::InProgress => "In progress",
        ops::TaskStage::InReview => "In review",
        ops::TaskStage::Done => "Done",
    }
}
