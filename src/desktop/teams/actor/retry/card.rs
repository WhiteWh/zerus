//! Kept card state and opener receipts share one policy for both gate paths.
use super::*;
pub(super) fn decisions() -> Vec<String> {
    vec!["retry".into(), "acknowledged".into()]
}
impl Runtime {
    pub(super) fn reserve_gate(
        &mut self,
        team: &str,
        name: &str,
        row: Option<Row>,
        g: &Gate,
    ) -> Result<Option<Row>> {
        if let Some(mut row) = row {
            return Ok(self.save_gate(&mut row, g)?.then_some(row));
        }
        let data = serde_json::to_value(g).map_err(crate::teams::store::storage)?;
        self.ledger.reserve(team, name, data).map(Some)
    }
    pub(super) fn save_gate(&mut self, row: &mut Row, g: &Gate) -> Result<bool> {
        row.data = serde_json::to_value(g).map_err(crate::teams::store::storage)?;
        self.ledger.save_if_current(row)
    }
    pub(super) fn current_gate_card(
        &mut self,
        store: &mut Store,
        team: &Team,
        row: &mut Row,
        g: &mut Gate,
        now: i64,
    ) -> Result<bool> {
        let Some(id) = g.card else { return Ok(true) };
        let kept = store.card(&team.id, id)?;
        if kept
            .as_ref()
            .is_some_and(|c| crate::desktop::teams::kept::stands(c, &decisions(), &["retry"]))
        {
            return Ok(true);
        }
        let open = kept.is_some_and(|c| c.state == "open");
        let reason = "The service card's current decisions changed.";
        self.replace_gate_card(store, team, (row, g), open.then_some(reason), now)
    }
    /// Closes the kept card when `close` names a reason, then forgets it
    /// under the owned row version so that the opener opens the current one.
    pub(super) fn replace_gate_card(
        &mut self,
        store: &mut Store,
        team: &Team,
        gate: (&mut Row, &mut Gate),
        close: Option<&str>,
        now: i64,
    ) -> Result<bool> {
        let (row, g) = gate;
        if let (Some(card), Some(reason)) = (g.card, close) {
            write(
                store,
                &team.id,
                Operation::CloseCard {
                    card,
                    reason: reason.into(),
                },
                now,
            )?;
            boundary("replacement_closed");
        }
        g.card = None;
        g.notice = None;
        if !self.save_gate(row, g)? {
            return Ok(false);
        }
        boundary("replacement_reset");
        Ok(true)
    }
    pub(super) fn open_gate_card(
        &mut self,
        store: &mut Store,
        team: &Team,
        scope: (&str, &Problem),
        row: &mut Row,
        g: &mut Gate,
        now: i64,
    ) -> Result<bool> {
        let (key, error) = scope;
        if g.notice.is_none() {
            g.notice = Some(request(store, &team.id, operation(key, error))?.1);
            if !self.save_gate(row, g)? {
                return Ok(false);
            }
            boundary("notice_reserved");
        }
        self.apply_gate_notice(store, team, row, g, now)
    }
    pub(super) fn apply_gate_notice(
        &mut self,
        store: &mut Store,
        team: &Team,
        row: &mut Row,
        g: &mut Gate,
        now: i64,
    ) -> Result<bool> {
        let actor = Actor::Service {
            person: team.owner.clone(),
            team: team.id.clone(),
        };
        match ops::apply(store, &actor, g.notice.as_ref().expect("reserved"), now) {
            Ok(reply) => {
                boundary("notice_applied");
                g.card = reply.item;
            }
            Err(e) if e.code == code::STALE_REVISION => g.notice = None,
            Err(e) => return Err(e),
        }
        if !self.save_gate(row, g)? {
            return Ok(false);
        }
        boundary("notice_saved");
        Ok(true)
    }
}
fn operation(key: &str, error: &Problem) -> Operation {
    let timing = if key.starts_with("finish_refused:") {
        "The service waits for this task's observation or acceptance to change before another finish attempt. Retry checks that state on the next pass."
    } else {
        "Automatic checks use a backoff from one to fifteen minutes. Choosing Retry checks on the next service pass (60-second safety interval)."
    };
    Operation::OpenCard {
        kind: "team_actor_blocked".into(),
        parameters: Some(vec![("code".into(), error.code.clone())]),
        node: key
            .strip_prefix("held:")
            .and_then(|s| s.rsplit(':').next())
            .and_then(|s| s.parse().ok()),
        decisions: Some(decisions()),
        // A held step's or an unreadable member's sentence carries its own
        // timing; neither row's key is shown.
        text: if key == "project_limit" {
            let message = cut(&error.message, 2000);
            format!(
                "The project's own limit refused the latest hand-out: {}. The team's tasks wait and are handed out by themselves when the project allows it. {}",
                message.trim_end_matches('.'),
                timing
            )
        } else if key.starts_with("held:") || unreadable::key_is_unreadable(key) {
            cut(&error.message, 3000)
        } else {
            let message = cut(&error.message, 2000);
            format!(
                "{}: {}. {}",
                cut(key, 200),
                message.trim_end_matches('.'),
                timing
            )
        },
    }
}
