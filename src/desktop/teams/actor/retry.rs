mod card;
mod held;
mod unreadable;
use super::*;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::desktop::teams) use crate::state::team_provider_stop::binding_stamp as provider_binding_stamp;
use serde::{Deserialize, Serialize};

const RECOVERED_REASON: &str = "The service check recovered";

#[derive(Default, Serialize, Deserialize)]
pub(super) struct Gate {
    revision: String,
    failures: u32,
    #[serde(default)]
    deferrals: u32,
    at: i64,
    card: Option<u64>,
    notice: Option<Request>,
    #[serde(default)]
    event: Option<String>,
    /// Held steps only: the hold's age and its newest reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hold: Option<held::Hold>,
}
impl Runtime {
    pub(super) fn due(
        &mut self,
        store: &mut Store,
        team: &Team,
        key: &str,
        revision: &str,
        now: i64,
    ) -> Result<bool> {
        self.for_row(|a| a.due_read(store, team, key, revision, now))
    }
    pub(super) fn failure(
        &mut self,
        store: &mut Store,
        team: &Team,
        key: &str,
        revision: &str,
        e: &Problem,
        now: i64,
    ) -> Result<()> {
        self.for_row(|a| a.failure_write(store, team, (key, revision, None), e, now, false))
    }
    pub(super) fn recovered(
        &mut self,
        store: &mut Store,
        team: &Team,
        key: &str,
        now: i64,
    ) -> Result<()> {
        self.for_row(|a| a.close_gate(store, team, key, RECOVERED_REASON, now))
    }

    pub(super) fn force_failure(
        &mut self,
        store: &mut Store,
        team: &Team,
        key: &str,
        revision: &str,
        e: &Problem,
        now: i64,
    ) -> Result<()> {
        self.for_row(|a| a.failure_write(store, team, (key, revision, None), e, now, true))
    }
    pub(super) fn recorded_failure(
        &mut self,
        store: &mut Store,
        team: &Team,
        scope: (&str, &str),
        event: &str,
        e: &Problem,
        now: i64,
    ) -> Result<()> {
        self.for_row(|a| {
            a.failure_write(store, team, (scope.0, scope.1, Some(event)), e, now, false)
        })
    }
    pub(super) fn cooldown(
        &mut self,
        store: &mut Store,
        team: &Team,
        key: &str,
        revision: &str,
        at: i64,
    ) -> Result<()> {
        self.recovered(store, team, key, at.saturating_sub(60_000))?;
        self.for_row(|a| {
            a.put(
                &team.id,
                &format!("gate:{key}"),
                &Gate {
                    revision: revision.into(),
                    at,
                    ..Default::default()
                },
            )?;
            Ok(())
        })
    }
    /// A stale task is an answer, not a fault. Observe before calling this;
    /// keep the normal bounded backoff without an error count or a new card.
    pub(super) fn defer(
        &mut self,
        store: &mut Store,
        team: &Team,
        key: &str,
        revision: &str,
        now: i64,
    ) -> Result<()> {
        self.for_row(|a| {
            let deferrals = a
                .load::<Gate>(&team.id, &format!("gate:{key}"))?
                .map_or(1, |g| g.deferrals.saturating_add(1));
            a.close_gate(store, team, key, RECOVERED_REASON, now)?;
            a.put(
                &team.id,
                &format!("gate:{key}"),
                &Gate {
                    revision: revision.into(),
                    deferrals,
                    at: now.saturating_add(delay(deferrals)),
                    ..Default::default()
                },
            )?;
            Ok(())
        })
    }
    fn due_read(
        &mut self,
        store: &mut Store,
        team: &Team,
        key: &str,
        revision: &str,
        now: i64,
    ) -> Result<bool> {
        let Some(mut g) = self.load::<Gate>(&team.id, &format!("gate:{key}"))? else {
            return Ok(true);
        };
        if let Some(id) = g.card {
            if store
                .card(&team.id, id)?
                .is_some_and(|c| c.state != "open" && c.decision.as_deref() == Some("retry"))
            {
                g.card = None;
                g.notice = None;
                g.at = now;
                self.put(&team.id, &format!("gate:{key}"), &g)?;
                return Ok(true);
            }
        }
        Ok(now >= g.at || !revision.is_empty() && revision != g.revision)
    }
    fn failure_write(
        &mut self,
        store: &mut Store,
        team: &Team,
        scope: (&str, &str, Option<&str>),
        e: &Problem,
        now: i64,
        force: bool,
    ) -> Result<()> {
        let (key, revision, event) = scope;
        let name = format!("gate:{key}");
        let row = self.ledger.get(&team.id, &name)?;
        let mut g: Gate = row
            .as_ref()
            .map(|r| serde_json::from_value(r.data.clone()))
            .transpose()
            .map_err(crate::teams::store::storage)?
            .unwrap_or_default();
        if event.is_none() || g.event.as_deref() != event {
            g.failures = g.failures.saturating_add(1);
            g.revision = revision.into();
            g.at = now.saturating_add(delay(g.failures));
            g.event = if key.starts_with("held:") {
                Some(e.message.clone())
            } else {
                event.map(String::from)
            };
            // A changed sentence ends this key's card however it stood: a
            // dismissal lasts while the sentence is the same
            // (zerus-63rru.23).
            if key == "project_limit" && g.card.is_some() {
                let card = g.card.expect("checked");
                if let Some(kept) = store.card(&team.id, card)? {
                    if kept.state == "open" {
                        write(
                            store,
                            &team.id,
                            Operation::CloseCard {
                                card,
                                reason: "The project limit's sentence changed".into(),
                            },
                            now,
                        )?;
                    }
                }
                g.card = None;
                g.notice = None;
            }
        }
        let Some(mut row) = self.reserve_gate(&team.id, &name, row, &g)? else {
            return Ok(());
        };
        boundary("gate_saved");
        // A pending pre-upgrade opener must first replay its own receipt;
        // the second iteration can replace its now-outdated decision list.
        for _ in 0..2 {
            if !self.current_gate_card(store, team, &mut row, &mut g, now)? {
                return Ok(());
            }
            if !(force || g.failures >= 3 || key == "project_limit") || g.card.is_some() {
                return Ok(());
            }
            if !self.open_gate_card(store, team, (key, e), &mut row, &mut g, now)? {
                return Ok(());
            }
        }
        Ok(())
    }
    fn close_gate(
        &mut self,
        store: &mut Store,
        team: &Team,
        key: &str,
        reason: &str,
        now: i64,
    ) -> Result<()> {
        let name = format!("gate:{key}");
        if let Some(mut row) = self.ledger.get(&team.id, &name)? {
            let mut g: Gate =
                serde_json::from_value(row.data.clone()).map_err(crate::teams::store::storage)?;
            // Settle an interrupted opener before forgetting its receipt.
            if g.card.is_none()
                && g.notice.is_some()
                && !self.apply_gate_notice(store, team, &mut row, &mut g, now)?
            {
                return Ok(());
            }
            if let Some(card) = g.card {
                if store
                    .card(&team.id, card)?
                    .is_some_and(|c| c.state == "open")
                {
                    boundary("recovery_before_close");
                    write(
                        store,
                        &team.id,
                        Operation::CloseCard {
                            card,
                            reason: reason.into(),
                        },
                        now,
                    )?;
                    boundary("recovery_closed");
                }
            }
            // This is the entry version, advanced only by our own notice
            // replay. A newer writer's version is never adopted for removal.
            self.ledger.remove(&row)?;
            boundary("recovery_removed");
        }
        Ok(())
    }

    /// A retry decision on `key`'s card, consumed once: the card reference
    /// and its notice are forgotten and the gate moves to now, so the caller
    /// reads it as lapsed and probes on the next pass (zerus-63rru.23).
    pub(super) fn consume_gate_retry(
        &mut self,
        store: &Store,
        team: &str,
        key: &str,
        now: i64,
    ) -> Result<bool> {
        let Some(mut row) = self.ledger.get(team, &format!("gate:{key}"))? else {
            return Ok(false);
        };
        let mut g: Gate =
            serde_json::from_value(row.data.clone()).map_err(crate::teams::store::storage)?;
        let asked = match g.card {
            Some(card) => store
                .card(team, card)?
                .is_some_and(|c| c.state != "open" && c.decision.as_deref() == Some("retry")),
            None => false,
        };
        if asked {
            g.card = None;
            g.notice = None;
            g.at = now;
            row.data = serde_json::to_value(&g).map_err(crate::teams::store::storage)?;
            self.ledger.save_if_current(&mut row)?;
        }
        Ok(asked)
    }
}

pub(super) fn delay(failures: u32) -> i64 {
    60_000 * (1i64 << failures.saturating_sub(1).min(4)).min(15)
}

#[cfg(test)]
pub(super) type Interleave = (&'static str, Box<dyn FnOnce()>);
#[cfg(test)]
thread_local! {
    pub(super) static CRASH: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
    pub(super) static INTERLEAVE: std::cell::RefCell<Option<Interleave>> = const { std::cell::RefCell::new(None) };
}
fn boundary(point: &str) {
    #[cfg(test)]
    {
        let action = INTERLEAVE.with(|slot| {
            if slot.borrow().as_ref().is_some_and(|(at, _)| *at == point) {
                slot.borrow_mut().take()
            } else {
                None
            }
        });
        if let Some((_, action)) = action {
            action();
        }
        CRASH.with(|slot| {
            if slot.get() == Some(point) {
                slot.set(None);
                panic!("fixture card restart at {point}");
            }
        });
    }
    #[cfg(not(test))]
    let _ = point;
}
