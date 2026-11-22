use super::ports::{Availability, Fact};
use super::terminal::TerminalState;
use super::project_limit::ProjectState;
use super::*;
use crate::teams::store::EffectIntent;
use std::collections::{BTreeMap, BTreeSet};
use zerus_team_plan::ready::{self, MemberView, PlanView};

/// Unreadable members needed by ready tasks or their open hand-outs.
#[derive(Default)]
pub(super) struct Needs {
    /// Members whose read this pass named a fact, with role and fact.
    unreadable: BTreeMap<String, (String, Fact)>,
    /// Members whose read was not due or failed: their rows stay as they are.
    unread: BTreeSet<String>,
    /// The needed ones: member, its waiting or held task, role and fact.
    pub needed: BTreeMap<String, (u32, String, Fact)>,
    /// An open hand-out needs its holder even when no task is ready.
    pub holders: BTreeSet<String>,
}
impl Needs {
    /// "Needs" is the kernel's own selection run once more with the
    /// unreadable members counted as free: a member is needed when that
    /// selection gives it a task. They come after every other member, so
    /// the members the first selection could use take what they took there
    /// (the kernel picks the first qualifying member in order): a task the
    /// second gives an unreadable member is one the first left waiting.
    /// `handed` are the tasks already handed out whose observation has not
    /// moved yet (an open effect or a pending delivery): the kernel still
    /// lists them as ready, but nobody is needed for them.
    fn select(&mut self, view: &PlanView, handed: &BTreeSet<u32>) {
        if self.unreadable.is_empty() {
            return;
        }
        let nodes: Vec<Node> = view
            .nodes
            .iter()
            .map(|n| Node {
                hold: n.hold || handed.contains(&n.id),
                ..n.clone()
            })
            .collect();
        let (known, unreadable): (Vec<&MemberView>, Vec<&MemberView>) = view
            .members
            .iter()
            .partition(|m| !self.unreadable.contains_key(&m.id));
        let members: Vec<MemberView> = known
            .into_iter()
            .cloned()
            .chain(unreadable.into_iter().map(|m| MemberView {
                free: true,
                ..m.clone()
            }))
            .collect();
        let again = PlanView {
            nodes: &nodes,
            members: &members,
            ..*view
        };
        for h in ready::handouts(&again) {
            if let Some((role, fact)) = self.unreadable.get(&h.member) {
                self.needed
                    .insert(h.member.clone(), (h.node, role.clone(), fact.clone()));
            }
        }
    }
    /// Whether this pass leaves `member`'s row as it is.
    pub fn unread(&self, member: &str) -> bool {
        self.unread.contains(member)
    }
}
impl Runtime {
    fn members(&self, store: &Store, team: &str) -> Result<Vec<Member>> {
        let mut out = Vec::new();
        let mut cursor = None;
        for _ in 0..2 {
            let page = store.members(team, 200, cursor.as_ref())?;
            out.extend(page.rows);
            cursor = page.next;
            if cursor.is_none() {
                return Ok(out);
            }
        }
        Err(problem("Member page bound exceeded"))
    }
    /// The member's availability, or None when its read is not due this
    /// pass (a failed read waits for its backoff) or failed.
    fn member_free(
        &mut self,
        store: &mut Store,
        ports: &mut impl Ports,
        team: &Team,
        (m, held): (&Member, Option<&EffectIntent>),
        now: i64,
        readings: &mut Vec<ops::MemberReading>,
    ) -> Result<Option<Availability>> {
        let key = format!("member_free:{}", m.id);
        if !self.due(store, team, &key, "", now)? {
            return Ok(None);
        }
        let (answer, confirmation) = match held.filter(|effect| effect.state == "submitted") {
            Some(effect) => {
                let gate = self.provider_confirmation(team, m)?;
                let answer = ports.held_availability(
                    store,
                    team,
                    m,
                    effect,
                    gate.as_ref().map(|(_, proof)| proof),
                    now,
                );
                (answer, gate)
            }
            None => (
                ports.availability(team, m).map(|answer| (answer, None)),
                None,
            ),
        };
        match answer {
            Ok((answer, stopped)) => {
                if let Some(binding) = stopped {
                    self.provider_stop_confirmed(
                        store,
                        team,
                        m,
                        binding,
                        confirmation.as_ref(),
                        now,
                    )?;
                }
                self.recovered(store, team, &key, now)?;
                if let Some(session) = &m.session {
                    readings.push(ops::MemberReading {
                        member: m.id.clone(),
                        session: session.clone(),
                        unreadable: matches!(&answer, Availability::Unreadable(fact)
                            if !matches!(fact, Fact::FirstMessage | Fact::TerminalWaiting { .. })),
                    });
                }
                Ok(Some(answer))
            }
            Err(e) => {
                self.failure(store, team, &key, "", &e, now)?;
                Ok(None)
            }
        }
    }
    fn record_readability(
        &self,
        store: &mut Store,
        team: &Team,
        mut readings: Vec<ops::MemberReading>,
        now: i64,
    ) -> Result<()> {
        if readings.is_empty() {
            return Ok(());
        }
        let previous = store.member_readability(&team.id)?;
        readings.retain(|reading| {
            let state = previous.get(&reading.member).copied().unwrap_or_default();
            state.observe(reading.unreadable, now) != state
        });
        if readings.is_empty() {
            return Ok(());
        }
        match write(
            store,
            &team.id,
            Operation::RecordMemberReadability { readings },
            now,
        ) {
            Ok(_) => Ok(()),
            Err(error) if error.code == code::STALE_REVISION => Ok(()),
            Err(error) => Err(error),
        }
    }
    fn cooled_readiness(
        &mut self,
        store: &mut Store,
        team: &Team,
        mut r: ops::Readiness,
        now: i64,
    ) -> Result<ops::Readiness> {
        let view = PlanView {
            phases: &r.phases,
            nodes: &r.nodes,
            members: &[],
            lead: &team.lead,
            integration: team.settings.integration,
        };
        let mut cooling = std::collections::BTreeSet::new();
        for id in ready::ready(&view) {
            let revision = store
                .task_observation(&team.id, id)?
                .map_or(String::new(), |o| o.revision);
            if !self.due(store, team, &format!("handout:{id}"), &revision, now)? {
                cooling.insert(id);
            }
        }
        // This narrows the view only. Each selected node is checked again
        // against the store's unchanged kernel view before its intent.
        for n in &mut r.nodes {
            if cooling.contains(&n.id) {
                n.hold = true;
            }
        }
        Ok(r)
    }
    pub(super) fn handouts(
        &mut self,
        store: &mut Store,
        ports: &mut impl Ports,
        team: &Team,
        now: i64,
    ) -> Result<()> {
        // The team's project fact (zerus-63rru.23): while it stands the
        // pass delivers nothing. Once its time passed, the first ready task
        // is the probe; the check below stops the pass unless the probe
        // ended the fact.
        let limit = self.project_limit_state(store, team, now)?;
        let lapsed = limit == ProjectState::Lapsed;
        let standing = limit == ProjectState::Standing;
        // One readiness read: uncooled for the unstaffed comparison, then
        // cooled for the hand-outs. The bounded effects read also finds
        // holders whose task has started; members are read once when either
        // ready work or an open hand-out needs them.
        let raw = ops::readiness(store, &team.id)?;
        let any_ready = !ready::ready(&PlanView {
            phases: &raw.phases,
            nodes: &raw.nodes,
            members: &[],
            lead: &team.lead,
            integration: team.settings.integration,
        })
        .is_empty();
        let effects = self.open_effects(store, &team.id)?;
        let all_members = if any_ready || !effects.is_empty() {
            self.members(store, &team.id)?
        } else {
            Vec::new()
        };
        let mut r = self.cooled_readiness(store, team, raw.clone(), now)?;
        let initial = PlanView {
            phases: &r.phases,
            nodes: &r.nodes,
            members: &[],
            lead: &team.lead,
            integration: team.settings.integration,
        };
        let ready = ready::ready(&initial);
        let ready_work = !standing && !ready.is_empty();
        // A task already handed out stays ready in the kernel's view until
        // its observation moves, and the store refuses a second intent for
        // it (zerus-63rru.71). Held here like a cooling task, it takes no
        // free member from a task that waits, and its refusal is never
        // counted as a hand-out that failed.
        let mut handed = BTreeSet::new();
        for id in ready {
            if effects.iter().any(|e| e.node == id) || self.ledger.pending_node(&team.id, id)? {
                handed.insert(id);
            }
        }
        for n in &mut r.nodes {
            if handed.contains(&n.id) {
                n.hold = true;
            }
        }
        let mut members = Vec::new();
        let mut needs = Needs::default();
        let mut readings = Vec::new();
        for m in &all_members {
            let held = effects.iter().find(|e| e.member == m.id);
            // The member is in the active set: a later leave tells the lead
            // again (zerus-63rru.86).
            if ready_work && m.state == MemberState::Active && m.session.is_some() {
                self.forget_paused_started(store, team, &m.id, now)?;
            }
            let answer = if m.id != team.lead
                && m.state == MemberState::Active
                && m.session.is_some()
                && held.is_some()
            {
                // An occupied seat remains unavailable for new work. Only
                // an undecidable binding or first-message readiness needs
                // this card: decisive ends use delivery recovery (.85).
                self.member_free(store, ports, team, (m, held), now, &mut readings)?
            } else if ready_work
                && m.id != team.lead
                && m.state == MemberState::Active
                && m.session.is_some()
                && !self.ledger.pending_member(&team.id, &m.id)?
                && held.is_none()
            {
                let terminal = self.terminal_state(ports, team, m, now)?;
                // A terminal refusal blocks delivery but does not replace a
                // fresh readability observation.
                let answer = self.member_free(store, ports, team, (m, held), now, &mut readings)?;
                match terminal {
                    // Its terminal holds input (zerus-63rru.79): not free, and
                    // needed like a member that cannot be read.
                    TerminalState::Standing { dialog } => {
                        Some(Availability::Unreadable(Fact::TerminalWaiting { dialog }))
                    }
                    // Tried again: a card for it stays as it is this pass.
                    TerminalState::Lapsed => {
                        needs.unread.insert(m.id.clone());
                        answer
                    }
                    TerminalState::None => answer,
                }
            } else {
                // A member out of the active set that holds started work is
                // told to the lead once (zerus-63rru.86).
                if ready_work && (m.state != MemberState::Active || m.session.is_none()) {
                    self.tell_paused_started(store, team, m, now)?;
                }
                Some(Availability::Busy)
            };
            match &answer {
                Some(Availability::Unreadable(fact)) => {
                    if let Some(effect) = held {
                        if matches!(fact, Fact::NoHandle | Fact::Unavailable)
                            || matches!(fact, Fact::FirstMessage) && effect.state == "reserved"
                        {
                            needs.holders.insert(m.id.clone());
                            needs.needed.insert(
                                m.id.clone(),
                                (effect.node, m.role.clone(), fact.clone()),
                            );
                        }
                    } else {
                        needs
                            .unreadable
                            .insert(m.id.clone(), (m.role.clone(), fact.clone()));
                    }
                }
                None => {
                    needs.unread.insert(m.id.clone());
                }
                Some(_) => {}
            }
            members.push(MemberView {
                id: m.id.clone(),
                role: m.role.clone(),
                state: m.state,
                free: held.is_none() && answer == Some(Availability::Free),
            });
        }
        // Capacity blocks new delivery, but a stopped holder can still
        // return its existing work and close the matching unreadable card.
        if standing {
            return Ok(());
        }
        let observed_at = ports.clock_ms(now);
        self.record_readability(store, team, readings, observed_at)?;
        // Even a pass without ready work ends episodes that left the set.
        self.unstaffed(store, team, &raw, &all_members, &effects, observed_at)?;
        let view = PlanView {
            phases: &r.phases,
            nodes: &r.nodes,
            members: &members,
            lead: &team.lead,
            integration: team.settings.integration,
        };
        if !needs.unreadable.is_empty() {
            let mut handed: BTreeSet<u32> = effects.iter().map(|e| e.node).collect();
            for id in ready::ready(&view) {
                if self.ledger.pending_node(&team.id, id)? {
                    handed.insert(id);
                }
            }
            needs.select(&view, &handed);
        }
        self.unreadable_members(store, team, &needs, now)?;
        let mut first = true;
        for h in ready::handouts(&view).into_iter().take(8) {
            let key = format!("handout:{}", h.node);
            let revision = store
                .task_observation(&team.id, h.node)?
                .map_or(String::new(), |o| o.revision);
            if !self.due(store, team, &key, &revision, now)? {
                continue;
            }
            let result = self.handout(store, ports, team, h, lapsed && first, now);
            first = false;
            if let Err(e) = result {
                self.failure(store, team, &key, &revision, &e, now)?;
            }
            // A refusal of the project's limit stops this pass at once,
            // whether this hand-out was the probe or the one that found it
            // (zerus-63rru.23): one refused effect per probe, and nothing
            // more while the fact exists.
            if self.project_limit_state(store, team, now)? != ProjectState::None {
                return Ok(());
            }
        }
        Ok(())
    }
    /// The lead is told once per member and task while a member out of the
    /// active set holds started work (zerus-63rru.86). The actor's ledger
    /// rows `paused_started:<member>:<node>` are the once-only memory; the
    /// member's return removes them, so a later leave tells the lead again.
    fn tell_paused_started(
        &mut self,
        store: &mut Store,
        team: &Team,
        m: &Member,
        now: i64,
    ) -> Result<()> {
        // The member's started work: its submitted hand-outs of tasks in
        // progress, read from the store's own effects.
        let mut started = std::collections::BTreeSet::new();
        let mut cursor = None;
        for _ in 0..4 {
            let page = store.effects(&team.id, 200, cursor.as_ref())?;
            for e in page.rows {
                if e.member == m.id && e.state == "submitted" {
                    started.insert(e.node);
                }
            }
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        for node in started {
            let key = format!("paused_started:{}:{}", m.id, node);
            if self.ledger.get(&team.id, &key)?.is_some() {
                continue;
            }
            let (actor, request) = request(
                store,
                &team.id,
                Operation::TellPausedStarted {
                    member: m.id.clone(),
                    node,
                },
            )?;
            ops::apply(store, &actor, &request, now)?;
            self.ledger.reserve(&team.id, &key, serde_json::json!({}))?;
        }
        Ok(())
    }

    /// The member is in the active set again: its once-only memory ends, so
    /// a later leave tells the lead again (zerus-63rru.86).
    fn forget_paused_started(
        &mut self,
        store: &mut Store,
        team: &Team,
        member: &str,
        now: i64,
    ) -> Result<()> {
        // The close's decision comes from the store's own open items, not
        // the actor's ledger (zerus-63rru.86 round 3): a crash between the
        // apply and the ledger write cannot leave an open item unclosed,
        // and the ledger rows stay an optimization only.
        if store.open_member_started(&team.id, member)? == 0 {
            // Nothing to close: no ops call, so an unchanged pass stays a
            // no-op and its revision does not move.
            return Ok(());
        }
        let (actor, request) = request(
            store,
            &team.id,
            Operation::ClosePausedStarted {
                member: member.to_owned(),
            },
        )?;
        ops::apply(store, &actor, &request, now)?;
        // The ledger rows are keyed paused_started:{member}:{node}; the
        // cleanup's prefix carries the member's delimiter, so m1's return
        // never removes m10's rows.
        let ledger_prefix = format!("paused_started:{member}:");
        let mut after = String::new();
        for _ in 0..4 {
            let rows = self.ledger.prefix_page(&team.id, &ledger_prefix, &after)?;
            for row in &rows {
                self.ledger.remove(row)?;
            }
            match rows.last() {
                Some(last) if rows.len() == 200 => after = last.key.clone(),
                _ => break,
            }
        }
        Ok(())
    }

    /// zerus-63rru.45: the kernel's unstaffed set over the uncooled view,
    /// compared with the told episodes by the store's own function; the
    /// store reconciles only when that finds a change and recomputes the
    /// set itself. The request is kept in the ledger before it is applied,
    /// so a restart replays it by its request ID and gets its saved reply. A
    /// race (`unstaffed_unchanged`, or a revision that moved) is settled,
    /// never a gate failure or a card.
    fn unstaffed(
        &mut self,
        store: &mut Store,
        team: &Team,
        raw: &ops::Readiness,
        members: &[Member],
        effects: &[EffectIntent],
        now: i64,
    ) -> Result<()> {
        if let Some(saved) = self.load::<Request>(&team.id, UNSTAFFED)? {
            self.apply_unstaffed(store, team, saved, now)?;
        }
        let open: BTreeSet<u32> = effects.iter().map(|e| e.node).collect();
        #[allow(unused_mut)]
        let mut set = ops::unstaffed_of(
            team,
            raw,
            members,
            &open,
            &store.unreadable_members(&team.id)?,
        );
        #[cfg(test)]
        STALE_VIEW.with(|stale| {
            if let Some(node) = stale.get() {
                set.push(zerus_team_plan::ready::Unstaffed {
                    node,
                    reason: "A stale view.".into(),
                    unreadable: false,
                });
            }
        });
        // Stale items (older unended items of a node, left by an earlier
        // layout) are ended a page per pass in their own operation, before
        // the cap is judged on the current episodes.
        if store.stale_unstaffed(&team.id)? && self.due(store, team, UNSTAFFED_STALE, "", now)? {
            self.end_stale_unstaffed(store, team, now)?;
        }
        let told = store.told_unstaffed(&team.id)?;
        let changes = ops::unstaffed_changes(&set, &ops::unstaffed_stages(raw), &told);
        if changes.is_empty() || !self.due(store, team, UNSTAFFED, "", now)? {
            return Ok(());
        }
        let (_, request) = request(store, &team.id, Operation::ReconcileUnstaffed)?;
        self.put(&team.id, UNSTAFFED, &request)?;
        self.apply_unstaffed(store, team, request, now)
    }
    /// One page of stale items ended. A lost result costs nothing: an ended
    /// item is never ended again, and the next pass asks with a new request.
    fn end_stale_unstaffed(&mut self, store: &mut Store, team: &Team, now: i64) -> Result<()> {
        let actor = Actor::Service {
            person: team.owner.clone(),
            team: team.id.clone(),
        };
        let (_, request) = request(store, &team.id, Operation::EndStaleUnstaffed)?;
        match ops::apply(store, &actor, &request, now) {
            Ok(_) => self.recovered(store, team, UNSTAFFED_STALE, now),
            Err(e) if e.code == code::UNSTAFFED_UNCHANGED || e.code == code::STALE_REVISION => {
                Ok(())
            }
            Err(e) => self.failure(store, team, UNSTAFFED_STALE, "", &e, now),
        }
    }
    fn apply_unstaffed(
        &mut self,
        store: &mut Store,
        team: &Team,
        request: Request,
        now: i64,
    ) -> Result<()> {
        let actor = Actor::Service {
            person: team.owner.clone(),
            team: team.id.clone(),
        };
        match ops::apply(store, &actor, &request, now) {
            Ok(_) => {
                unstaffed_boundary("applied");
                self.recovered(store, team, UNSTAFFED, now)?;
            }
            Err(e) if e.code == code::UNSTAFFED_UNCHANGED || e.code == code::STALE_REVISION => {}
            // Not saved: the same request ID is applied again next pass.
            Err(e) if e.code == code::STORAGE_FAILED => {
                return self.failure(store, team, UNSTAFFED, "", &e, now);
            }
            Err(e) => self.failure(store, team, UNSTAFFED, "", &e, now)?,
        }
        if let Some(row) = self.ledger.get(&team.id, UNSTAFFED)? {
            self.ledger.remove(&row)?;
        }
        Ok(())
    }
    fn handout(
        &mut self,
        store: &mut Store,
        ports: &mut impl Ports,
        team: &Team,
        h: ready::Handout,
        probe: bool,
        now: i64,
    ) -> Result<()> {
        if self.ledger.pending_node(&team.id, h.node)? {
            return Ok(());
        }
        if !self.due(store, team, &format!("observe:{}", h.node), "", now)? {
            return Ok(());
        }
        let Some(n) = store.node(&team.id, h.node)? else {
            return Ok(());
        };
        let Some(task) = n.task.as_ref() else {
            return Ok(());
        };
        ports.observe(store, task)?;
        let r = ops::readiness(store, &team.id)?;
        let view = PlanView {
            phases: &r.phases,
            nodes: &r.nodes,
            members: &[],
            lead: &team.lead,
            integration: team.settings.integration,
        };
        if !ready::ready(&view).contains(&h.node) {
            return Ok(());
        }
        let Some(node) = store.node(&team.id, h.node)? else {
            return Ok(());
        };
        let Some(m) = store.member(&team.id, &h.member)? else {
            return Ok(());
        };
        if m.state != MemberState::Active || !ports.free(team, &m)? {
            return Ok(());
        }
        self.send(store, ports, team, (&node, &m), "handout", probe, now)
    }
    pub(super) fn reopens(
        &mut self,
        store: &mut Store,
        ports: &mut impl Ports,
        team: &Team,
        now: i64,
    ) -> Result<()> {
        // While the team's project fact exists, no reopen is delivered
        // whatever the member (zerus-63rru.23).
        if self.project_limit_state(store, team, now)? != ProjectState::None {
            return Ok(());
        }
        let mut cursor = None;
        for _ in 0..4 {
            let page = store.pending_reopens(&team.id, 200, cursor.as_ref())?;
            for d in page.rows {
                if ports.stopped() {
                    return Ok(());
                }
                let key = format!("reopen:{}", d.seq);
                if !self.due(store, team, &key, "", now)? {
                    continue;
                }
                if let Err(e) = self.reopen(store, ports, team, &d, now) {
                    self.failure(store, team, &key, "", &e, now)?;
                }
            }
            cursor = page.next;
            if cursor.is_none() {
                return Ok(());
            }
        }
        Err(problem("Reopen page bound exceeded"))
    }
    fn reopen(
        &mut self,
        store: &mut Store,
        ports: &mut impl Ports,
        team: &Team,
        d: &crate::teams::store::Decision,
        now: i64,
    ) -> Result<()> {
        let ready = store.own_task_current(&team.id, d.node, d.generation)?;
        if !self.held_step(
            store,
            team,
            ("reopen", d.node, d.generation, d.seq, d.kind.as_str()),
            ready,
            now,
        )? {
            return Ok(());
        }
        let Some(node) = store.node(&team.id, d.node)? else {
            return Ok(());
        };
        let member = if d.kind == "reassign" {
            node.assignee.clone()
        } else {
            self.open_effects(store, &team.id)?
                .iter()
                .filter(|e| e.node == node.id)
                .max_by_key(|e| e.seq)
                .map(|e| e.member.clone())
                .or(store
                    .report(&team.id, node.id, d.generation)?
                    .map(|r| r.member))
        };
        let Some(m) = member
            .map(|id| store.member(&team.id, &id))
            .transpose()?
            .flatten()
        else {
            return Ok(());
        };
        if m.state != MemberState::Active
            || self.terminal_standing(ports, team, &m, now)?
            || !ports.free(team, &m)?
        {
            return Ok(());
        }
        self.send(store, ports, team, (&node, &m), "reopen", false, now)?;
        self.recovered(store, team, &format!("reopen:{}", d.seq), now)
    }
}

/// The ledger key of the service's reconcile of unstaffed nodes.
const UNSTAFFED: &str = "unstaffed";
const UNSTAFFED_STALE: &str = "unstaffed_stale";
#[cfg(test)]
thread_local! {
    /// A test's stale view: the service believes this node is unstaffed.
    pub(super) static STALE_VIEW: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
    pub(super) static UNSTAFFED_CRASH: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}
fn unstaffed_boundary(point: &str) {
    #[cfg(test)]
    UNSTAFFED_CRASH.with(|slot| {
        if slot.get() == Some(point) {
            slot.set(None);
            panic!("fixture unstaffed restart at {point}");
        }
    });
    #[cfg(not(test))]
    let _ = point;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::teams::ops::tests::*;

    /// The kernel picks the first qualifying member in order. Counted free
    /// after every other member, an unreadable m1 listed before a free m2
    /// does not take the task m2 takes in the first selection: it is needed
    /// for the task that waits.
    #[test]
    fn an_unreadable_member_is_needed_for_the_task_the_first_selection_left() {
        let mut h = Harness::new();
        h.bind("m1");
        h.bind("m2");
        h.add(&person(), 1);
        h.add(&person(), 2);
        let team = h.store.team(TEAM).unwrap().unwrap();
        let r = ops::readiness(&h.store, TEAM).unwrap();
        let members: Vec<MemberView> = ["m1", "m2"]
            .iter()
            .map(|id| {
                let m = h.store.member(TEAM, id).unwrap().unwrap();
                MemberView {
                    id: m.id,
                    role: m.role,
                    state: m.state,
                    free: *id == "m2",
                }
            })
            .collect();
        let view = PlanView {
            phases: &r.phases,
            nodes: &r.nodes,
            members: &members,
            lead: &team.lead,
            integration: team.settings.integration,
        };
        let first = ready::handouts(&view);
        assert_eq!(
            first
                .iter()
                .map(|h| (h.node, h.member.as_str()))
                .collect::<Vec<_>>(),
            [(1, "m2")]
        );
        let mut needs = Needs::default();
        needs
            .unreadable
            .insert("m1".into(), ("developer".into(), Fact::Children));
        needs.select(&view, &BTreeSet::new());
        assert_eq!(
            needs.needed.into_iter().collect::<Vec<_>>(),
            [("m1".to_owned(), (2, "developer".to_owned(), Fact::Children))]
        );
    }
}
