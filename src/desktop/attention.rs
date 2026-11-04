//! Actor-owned attention discovery. No host, writer or UI fallback is created.
mod identity;
mod projection;
mod state;
#[cfg(test)]
mod tests;

use super::{feed, fleet, notifications::Outbox, store::Store};
use anyhow::{ensure, Context, Result};
pub use projection::Policy;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    time::{Duration, Instant},
};
use zerus_desktop_catalog::{self as catalog, changes};

const FRESH: Duration = Duration::from_secs(15);
const ROWS_PER_TURN: usize = 4;
const EFFECTS_PER_TURN: usize = 8;
const FOCUS_TTL: Duration = Duration::from_secs(5);
pub(super) fn migrate(db: &rusqlite::Connection) -> Result<()> {
    state::migrate(db)
}
struct Source {
    scope: changes::Scope,
    at: Instant,
    baseline: Option<catalog::Cursor>,
    baselined: bool,
    since: i64,
    delta: Option<changes::Cursor>,
    reset_since: bool,
}
struct Row {
    key: String,
    raw: Option<String>,
    baseline: bool,
}
struct Pending {
    scope: changes::Scope,
    rows: VecDeque<Row>,
}
#[derive(Default)]
struct Retirement {
    after: i64,
    through: Option<i64>,
}
pub struct Runtime {
    epoch: String,
    policy: Policy,
    policy_revision: i64,
    foreground_until: Option<Instant>,
    sources: BTreeMap<String, Source>,
    pending: Option<Pending>,
    next_source: usize,
    stopping: bool,
    error: Option<String>,
    effect_after: i64,
    effect_through: i64,
    retirement: Option<Retirement>,
    #[cfg(test)]
    clock: Option<Instant>,
}
impl Runtime {
    pub fn new(epoch: &str) -> Self {
        Self {
            epoch: epoch.into(),
            policy: Policy::default(),
            policy_revision: 0,
            foreground_until: None,
            sources: BTreeMap::new(),
            pending: None,
            next_source: 0,
            stopping: false,
            error: None,
            effect_after: 0,
            effect_through: 0,
            retirement: None,
            #[cfg(test)]
            clock: None,
        }
    }
    fn now(&self) -> Instant {
        #[cfg(test)]
        if let Some(now) = self.clock {
            return now;
        }
        Instant::now()
    }
    /// Only the original service's authenticated configuration/focus admission
    /// calls these. Neither method is a desktop request or a native writer grant.
    pub fn set_policy(&mut self, policy: Policy) {
        if self.policy != policy {
            if self.policy.enabled && !policy.enabled {
                self.request_retirement();
            }
            self.policy = policy;
            self.reset_policy();
        }
    }
    pub fn enabled(&self) -> bool {
        self.policy.enabled
    }
    /// The lifecycle also calls this when resuming a durable policy-off change
    /// after restart. It grants no post or native withdrawal by itself.
    pub fn request_retirement(&mut self) {
        self.retirement.get_or_insert_with(Retirement::default);
        self.pending = None;
    }
    pub fn retiring(&self) -> bool {
        self.retirement.is_some()
    }
    fn reset_policy(&mut self) {
        let Some(revision) = self.policy_revision.checked_add(1) else {
            self.fail(anyhow::anyhow!("attention policy revisions exhausted"));
            return;
        };
        self.policy_revision = revision;
        self.pending = None;
        for source in self.sources.values_mut() {
            source.baselined = false;
            source.baseline = None;
            source.delta = None;
            source.reset_since = true;
        }
    }
    pub fn suppress_foreground(&mut self, now: Instant) {
        self.foreground_until = Some(now + FOCUS_TTL);
    }
    pub fn set_foreground_until(&mut self, until: Option<Instant>) {
        self.foreground_until = until;
    }

    /// The actor calls this only after OWN successful Broker::apply, while the
    /// original ingress permit is held. A passive feed has no FleetEvent owner.
    pub fn observe(&mut self, store: &Store, event: &fleet::FleetEvent) {
        if let Err(error) = self.observe_packet(store, &event.packet) {
            self.fail(error);
        }
    }
    fn observe_packet(&mut self, store: &Store, packet: &feed::Packet) -> Result<()> {
        if !self.enabled() || self.stopping || self.error.is_some() {
            return Ok(());
        }
        let Some((computer, generation)) = packet.scope() else {
            return Ok(());
        };
        if matches!(
            packet,
            feed::Packet::Offline { .. } | feed::Packet::Remove { .. }
        ) {
            self.reject(computer);
            return Ok(());
        }
        let mark = changes::watermark(&store.db, computer)?;
        if !mark.complete
            || !mark.online
            || mark.scope.source.service_epoch != self.epoch
            || mark.scope.source.generation != generation
        {
            self.reject(computer);
            return Ok(());
        }
        ensure!(
            self.sources.contains_key(computer)
                || self.sources.len() < catalog::MAX_SOURCES as usize,
            "attention source quota"
        );
        let now = self.now();
        if let Some(source) = self
            .sources
            .get_mut(computer)
            .filter(|s| s.scope == mark.scope)
        {
            source.at = now;
        } else {
            self.reject(computer);
            self.sources.insert(
                computer.into(),
                Source {
                    scope: mark.scope,
                    at: now,
                    baseline: None,
                    baselined: false,
                    since: mark.revision.context("missing attention watermark")?,
                    delta: None,
                    reset_since: false,
                },
            );
        }
        Ok(())
    }
    pub fn reject(&mut self, computer: &str) {
        self.sources.remove(computer);
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.scope.source.computer == computer)
        {
            self.pending = None;
        }
    }
    pub fn reject_event(&mut self, event: &fleet::FleetEvent) {
        self.reject_packet(&event.packet);
    }
    fn reject_packet(&mut self, packet: &feed::Packet) {
        if let Some((computer, _)) = packet.scope() {
            self.reject(computer);
        } else {
            // An invalid unscoped packet cannot leave an old source eligible.
            self.sources.clear();
            self.pending = None;
        }
    }
    pub fn work_pending(&self) -> bool {
        self.pending.as_ref().is_some_and(|p| !p.rows.is_empty())
    }
    fn current(&self, store: &Store, scope: &changes::Scope) -> Result<bool> {
        if !self.sources.get(&scope.source.computer).is_some_and(|s| {
            s.scope == *scope && self.now().saturating_duration_since(s.at) <= FRESH
        }) {
            return Ok(false);
        }
        let mark = changes::watermark(&store.db, &scope.source.computer)?;
        Ok(mark.complete
            && mark.online
            && mark.scope == *scope
            && mark.scope.source.service_epoch == self.epoch)
    }
    pub fn poll(&mut self, store: &mut Store, outbox: Option<&mut Outbox>) {
        if self.stopping {
            return;
        }
        if self.retiring() {
            if let Some(outbox) = outbox {
                self.poll_retirement(store, outbox);
            }
            return;
        }
        if !self.enabled() {
            return;
        }
        if self.error.is_some() {
            return;
        }
        if let Err(error) = self.turn(store, outbox) {
            self.fail(error);
        }
    }
    fn turn(&mut self, store: &mut Store, outbox: Option<&mut Outbox>) -> Result<()> {
        if self.pending.is_none() {
            self.fill(store)?;
        }
        for _ in 0..ROWS_PER_TURN {
            let Some(pending) = self.pending.as_ref() else {
                break;
            };
            let scope = pending.scope.clone();
            if !self.current(store, &scope)? {
                self.pending = None;
                break;
            }
            let Some(row) = self.pending.as_mut().unwrap().rows.pop_front() else {
                self.pending = None;
                break;
            };
            let mut changed_during_baseline = false;
            if row.baseline {
                let since = self.sources[&scope.source.computer].since;
                match state::baseline_key(&store.db, &scope, since, &row.key)? {
                    state::BaselineKey::Changed => changed_during_baseline = true,
                    state::BaselineKey::Gap => {
                        let watermark = changes::watermark(&store.db, &scope.source.computer)?;
                        let source = self.sources.get_mut(&scope.source.computer).unwrap();
                        source.baselined = false;
                        source.baseline = None;
                        source.delta = None;
                        source.since = watermark
                            .revision
                            .context("attention baseline reset watermark")?;
                        self.pending = None;
                        break;
                    }
                    state::BaselineKey::Unchanged => {}
                }
            }
            if let Some(raw) = row.raw {
                ensure!(
                    raw.len() <= catalog::PAGE_BYTES,
                    "attention input exceeds frame cap"
                );
                let value: Value = serde_json::from_str(&raw)?;
                // Missing genuine identities are ineligible, without a name,
                // generation or epoch fallback. They do not poison other rows.
                let view = zerus_client_core::view::row(&value);
                if view.run_id.as_deref().is_none_or(str::is_empty)
                    || view.conversation_id.as_deref().is_none_or(str::is_empty)
                {
                    continue;
                }
                let computer = &scope.source.computer;
                let session = state::session(computer, &value)?;
                // Catch up a known conversation through deltas. An unseen
                // conversation has no start-state from which to distinguish
                // its old unread edges from new ones: consume its first value
                // as baseline, even if a late page already sees an update.
                if changed_during_baseline && state::has_previous(&store.db, &session)? {
                    continue;
                }
                let label = state::label(&store.db, computer)?;
                let current_key = state::current_key(
                    &store.db,
                    computer,
                    &view.notify.cmd,
                    view.conversation_id.as_deref().unwrap(),
                )?;
                if current_key.as_deref().is_some_and(|key| key != row.key) {
                    let history = projection::project(
                        computer,
                        &label,
                        &value,
                        serde_json::Map::new(),
                        Policy::default(),
                        false,
                        true,
                    )?;
                    state::consume_history(&mut store.db, history)?;
                    continue;
                }
                // Multiple live rows are ambiguous; no candidate may post.
                // An actual historic row still consumes its initial baseline.
                if current_key.is_none()
                    && view.state == "running"
                    && view.process_state != "exited"
                {
                    continue;
                }
                let previous = state::previous(&store.db, &session)?;
                // A retained delta can be superseded before this actor turn.
                // Consume it as historical data, never post from it. The next
                // delta carries the current edge; no cached row is permission.
                let actual: Option<String>=store.db.query_row("SELECT r.json FROM desktop_catalog_rows r JOIN desktop_catalog_sources s ON s.visible=r.generation WHERE s.computer=?1 AND r.key=?2 AND length(CAST(r.json AS BLOB))<=?3",params![computer,row.key,catalog::PAGE_BYTES as i64],|r|r.get(0)).optional()?;
                let stale = actual.as_deref() != Some(&raw);
                let projection = projection::project(
                    computer,
                    &label,
                    &value,
                    previous,
                    self.policy,
                    self.foreground_until.is_some_and(|t| self.now() < t),
                    row.baseline || stale,
                )?;
                state::save(
                    &mut store.db,
                    computer,
                    &row.key,
                    state::Position {
                        scope: &projection::digest(&(
                            &scope.source.computer,
                            &scope.source.service_epoch,
                            scope.source.incarnation,
                            scope.source.generation,
                            &scope.projection,
                        ))?,
                        row_hash: &projection::digest(&value)?,
                        policy_revision: self.policy_revision,
                    },
                    projection,
                    row.baseline || stale,
                )?;
            } else {
                state::remove(&mut store.db, &scope.source.computer, &row.key)?;
            }
        }
        // At most eight indexed live/dirty edges, independently of the number
        // of retained historical attempts. No automatic refusal/unknown retry.
        if let Some(outbox) = outbox {
            self.effects(store, outbox, false)?;
        }
        Ok(())
    }
    fn fill(&mut self, store: &Store) -> Result<()> {
        if self.sources.is_empty() {
            return Ok(());
        }
        self.next_source %= self.sources.len();
        let computer = self.sources.keys().nth(self.next_source).unwrap().clone();
        self.next_source += 1;
        let scope = self.sources[&computer].scope.clone();
        if !self.current(store, &scope)? {
            self.reject(&computer);
            return Ok(());
        }
        let s = self.sources.get_mut(&computer).unwrap();
        let rows = if !s.baselined {
            if s.reset_since {
                s.since = changes::watermark(&store.db, &computer)?
                    .revision
                    .context("attention policy watermark")?;
                s.reset_since = false;
            }
            let page = catalog::page(&store.db, &computer, s.baseline.as_ref())?;
            ensure!(page.complete, "attention baseline incomplete");
            s.baselined = page.next.is_none();
            s.baseline = page.next;
            page.rows
                .into_iter()
                .map(|r| Row {
                    key: r.key,
                    raw: Some(r.json),
                    baseline: true,
                })
                .collect()
        } else {
            let page = match changes::page(&store.db, &s.scope, s.since, s.delta.as_ref()) {
                Ok(page) => page,
                Err(error) if error.downcast_ref::<changes::RetentionGap>().is_some() => {
                    // A lost interval is never replayed as fresh attention.
                    let mark = changes::watermark(&store.db, &computer)?;
                    s.baselined = false;
                    s.baseline = None;
                    s.delta = None;
                    s.since = mark.revision.context("attention reset watermark")?;
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            if page.next.is_none() {
                s.since = page.through;
            }
            s.delta = page.next;
            page.items
                .into_iter()
                .map(|r| Row {
                    key: r.key,
                    raw: r.json,
                    baseline: false,
                })
                .collect()
        };
        self.pending = Some(Pending { scope, rows });
        Ok(())
    }
    fn poll_retirement(&mut self, store: &mut Store, outbox: &mut Outbox) {
        outbox.cancel_queued_posts();
        let Some(mut retirement) = self.retirement.take() else {
            return;
        };
        let result = (|| -> Result<bool> {
            let through = *retirement.through.get_or_insert(state::head(&store.db)?);
            let rows =
                state::retirement_page(&store.db, retirement.after, through, EFFECTS_PER_TURN)?;
            for (id, intent) in &rows {
                // Revoke callbacks and original queued posts before changing
                // the edge. An already-started post keeps its receipt/resources.
                outbox.retire_intent(store, intent)?;
                state::retire(&store.db, *id)?;
                retirement.after = *id;
            }
            if rows.is_empty() {
                retirement.after = through;
            }
            self.effects(store, outbox, true)?;
            Ok(retirement.after == through
                && !state::has_dirty(&store.db)?
                && outbox.operations_idle())
        })();
        match result {
            Ok(true) => (), // Keep the original host connected and idle.
            Ok(false) => self.retirement = Some(retirement),
            Err(error) => {
                self.retirement = Some(retirement);
                self.fail(error);
            }
        }
    }
    fn effects(&mut self, store: &mut Store, outbox: &mut Outbox, retiring: bool) -> Result<()> {
        if !outbox.available() {
            return Ok(());
        }
        if self.effect_after >= self.effect_through {
            self.effect_after = 0;
            self.effect_through = state::head(&store.db)?;
        }
        let rows = state::dirty(
            &store.db,
            self.effect_after,
            self.effect_through,
            EFFECTS_PER_TURN,
        )?;
        let next = rows.last().map_or(self.effect_through, |r| r.id);
        for edge in rows {
            if !outbox.available() {
                break;
            }
            if !edge.live || edge.blocked {
                outbox.retire_intent(store, &edge.intent)?;
                match state::withdrawal(&mut store.db, &edge.intent)? {
                    state::Withdrawal::Waiting => continue,
                    state::Withdrawal::Ready(id) => {
                        outbox
                            .offer_intent(store, &id, 1, Instant::now())
                            .context("notification withdrawal offer failed")?;
                    }
                    state::Withdrawal::Absent => {}
                }
            } else if !retiring {
                // An original source must still be current and its live row
                // present. A restart never dispatches cached historical work.
                let Some(scope) = self.sources.get(&edge.computer).map(|s| s.scope.clone()) else {
                    continue;
                };
                if !self.current(store, &scope)? {
                    continue;
                }
                let stamp = projection::digest(&(
                    &scope.source.computer,
                    &scope.source.service_epoch,
                    scope.source.incarnation,
                    scope.source.generation,
                    &scope.projection,
                ))?;
                if self.foreground_until.is_some_and(|t| self.now() < t)
                    || !edge.ready
                    || !self.policy.enabled
                    || edge.policy_revision != self.policy_revision
                    || edge.scope != stamp
                {
                    continue;
                }
                let raw: Option<String>=store.db.query_row("SELECT r.json FROM desktop_catalog_rows r JOIN desktop_catalog_sources s ON s.visible=r.generation WHERE s.computer=?1 AND r.key=?2 AND length(CAST(r.json AS BLOB))<=?3",params![edge.computer,edge.key,catalog::PAGE_BYTES as i64],|r|r.get(0)).optional()?;
                let Some(raw) = raw.filter(|v| v.len() <= catalog::PAGE_BYTES) else {
                    continue;
                };
                let value: Value = serde_json::from_str(&raw)?;
                if projection::digest(&value)? != edge.row_hash {
                    continue;
                }
                let cmd = value.get("cmd").and_then(Value::as_str).unwrap_or("");
                let conversation = value
                    .get("conversation_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if state::current_key(&store.db, &edge.computer, cmd, conversation)?.as_deref()
                    != Some(&edge.key)
                {
                    continue;
                }
                outbox
                    .offer_intent(store, &edge.intent, 1, Instant::now())
                    .context("notification post offer failed")?;
            }
            state::clean(&store.db, edge.id)?;
        }
        self.effect_after = next;
        Ok(())
    }
    fn fail(&mut self, error: anyhow::Error) {
        if self.error.is_none() {
            self.error = Some(format!("{error:#}").chars().take(256).collect());
        }
        self.pending = None;
    }
    pub fn request_stop(&mut self) {
        self.stopping = true;
        self.sources.clear();
        self.pending = None;
    }
    pub fn drained(&self) -> bool {
        self.stopping && self.pending.is_none()
    }
    pub fn failed(&self) -> bool {
        self.error.is_some()
    }
    pub fn status(&self) -> Value {
        json!({"sources":self.sources.len(),"pending_rows":self.pending.as_ref().map_or(0,|p|p.rows.len()),"enabled":self.policy.enabled,"optional_questions":self.policy.optional_questions,"retiring":self.retiring(),"stopping":self.stopping,"error":self.error,"stopped_on_error":self.error.is_some(),"error_retryable":self.error.as_ref().map(|_|false),"source_limit":catalog::MAX_SOURCES,"rows_per_turn":ROWS_PER_TURN,"effects_per_turn":EFFECTS_PER_TURN})
    }
}
