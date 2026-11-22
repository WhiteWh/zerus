//! Reconcile only recorded proof of non-delivery. This never offers input.
use super::*;
use crate::teams::{ops::HandoutOutcome, store::EffectIntent};
use zerus_team_plan::problem::code::{INVALID_STATE, STALE_REVISION};
impl Runtime {
    pub(super) fn non_deliveries(
        &mut self,
        store: &mut Store,
        team: &Team,
        now: i64,
    ) -> Result<()> {
        // A current held slot is checked immediately even if its older history
        // page is far away. Uncertain or unreadable outcomes are never proof.
        for effect in self.open_effects(store, &team.id)? {
            self.non_delivery(store, team, &effect, now)?;
        }
        let page = store.non_delivery_history(&team.id)?;
        for effect in &page.rows {
            self.non_delivery(store, team, effect, now)?;
        }
        if let Some(last) = page.rows.last() {
            store.non_delivery_advanced(&team.id, last.seq)?;
        }
        if store.has_stale_reopen_notices(&team.id)? {
            match write(store, &team.id, Operation::CloseStaleReopenNotices, now) {
                Ok(_) => (),
                Err(error) if matches!(error.code.as_str(), STALE_REVISION | INVALID_STATE) => (),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    fn non_delivery(
        &mut self,
        store: &mut Store,
        team: &Team,
        effect: &EffectIntent,
        now: i64,
    ) -> Result<()> {
        if !store.non_delivery_pending(&team.id, effect)? {
            return Ok(());
        }
        let Some(raw) = effect.outcome.clone() else {
            return Ok(());
        };
        let outcome: HandoutOutcome =
            serde_json::from_value(raw).map_err(crate::teams::store::storage)?;
        let op = if effect.kind == "reopen" {
            Operation::ReopenOutcome {
                effect_request_id: effect.request_id.clone(),
                outcome,
            }
        } else {
            Operation::HandoutOutcome {
                effect_request_id: effect.request_id.clone(),
                outcome,
            }
        };
        match write(store, &team.id, op, now) {
            Ok(_) => Ok(()),
            Err(error)
                if matches!(error.code.as_str(), STALE_REVISION | INVALID_STATE)
                    && !store.non_delivery_pending(&team.id, effect)? =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}
