use super::*;

pub(super) use store::OPEN_EFFECT_STATES;
pub(super) struct Context<'a> {
    pub db: &'a Connection,
    pub state: &'a std::path::Path,
    pub actor: &'a Actor,
    pub team: &'a Team,
    pub op: &'a Operation,
    pub request_id: &'a str,
    pub provider_stop: Option<ProviderStopProof<'a>>,
    pub closed_confirmation_card: Option<u64>,
    pub task_custody: Option<&'a Result<store::phase_controls::task_custody::TaskCustody>>,
    pub now: i64,
    pub reply: Reply,
    pub event: Option<&'static str>,
    pub land_skipped_for: Option<u32>,
    /// The earlier decision this operation's decision replaced, for the
    /// journal.
    pub superseded: Option<u64>,
    pub timing: Option<super::observed_timing::Decision>,
}
impl Context<'_> {
    pub fn node(&self, id: u32) -> Result<Node> {
        rows::node(self.db, &self.team.id, id)?.ok_or_else(missing)
    }
    pub fn live_node(&self, id: u32) -> Result<Node> {
        let n = self.node(id)?;
        if self.node_table(id)? != "nodes" {
            return Err(invalid("Task has left the live plan."));
        }
        if matches!(n.state, NodeState::Accepted | NodeState::Cancelled) {
            return Err(invalid("Task is already decided."));
        }
        Ok(n)
    }
    pub fn member(&self, id: &str) -> Result<Member> {
        rows::member(self.db, &self.team.id, id)?
            .ok_or_else(|| Problem::new(code::UNKNOWN_MEMBER, "Member does not exist."))
    }
    pub fn generation(&self, id: u32) -> Result<u32> {
        self.db.query_row("SELECT generation FROM nodes WHERE team=?1 AND id=?2 UNION ALL SELECT generation FROM node_archive WHERE team=?1 AND id=?2 LIMIT 1", params![self.team.id,id], |r|r.get(0)).map_err(storage)
    }
    pub fn active(&self) -> Result<()> {
        if matches!(self.team.status, TeamStatus::Running | TeamStatus::Paused) {
            Ok(())
        } else {
            Err(invalid("The team is not active."))
        }
    }
    pub fn count(&self, admitted: bool) -> Result<u32> {
        self.db
            .query_row(
                "SELECT count(*) FROM nodes WHERE team=?1 AND admitted=?2",
                params![self.team.id, admitted],
                |r| r.get(0),
            )
            .map_err(storage)
    }
    /// Work charged to the admitted-task cap includes separate draft rows.
    pub fn task_capacity_used(&self) -> Result<u32> {
        Ok(self
            .count(true)?
            .saturating_add(store::drafts::count(self.db, &self.team.id)?))
    }
    pub fn person_stopped(&self, member: &str) -> Result<bool> {
        self.db
            .query_row(
                "SELECT person_stopped FROM members WHERE team=?1 AND id=?2",
                params![self.team.id, member],
                |r| r.get(0),
            )
            .map_err(storage)
    }
    pub fn close_inbox(&self, node: u32) -> Result<()> {
        self.db
            .execute(
                "UPDATE inbox SET state='closed' WHERE team=?1 AND node=?2 AND state='open'",
                params![self.team.id, node],
            )
            .map_err(storage)?;
        Ok(())
    }
    pub fn pending(&self, node: u32) -> Result<Option<(u64, String, DecisionState)>> {
        self.db.query_row(&format!("SELECT seq,kind,state FROM decisions WHERE team=?1 AND node=?2 AND state<>'{}' LIMIT 1",DecisionState::Settled.as_str()),params![self.team.id,node], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(storage)
    }
    pub fn undecided(&self, node: u32) -> Result<()> {
        if self.pending(node)?.is_some() {
            Err(invalid("The previous decision has not settled."))
        } else {
            Ok(())
        }
    }
    /// The node's Return or Reassign that has not started: unsettled, and no
    /// reopen was reserved for it (the reopen intent moves it from Pending to
    /// EffectReserved, which `reopening` reads). The rule is
    /// `DecisionState::replaceable`, which the queue row shows as well.
    pub fn unstarted(&self, node: u32) -> Result<Option<(u64, String)>> {
        let Some((seq, kind, state)) = self.pending(node)? else {
            return Ok(None);
        };
        Ok(state.replaceable(&kind).then_some((seq, kind)))
    }
    /// The one-unsettled-decision rule for a lead's or the person's Return,
    /// Reassign or Cancel. An earlier Return or Reassign that has not started
    /// is settled as superseded in this operation and named in its journal
    /// entry; any other unsettled decision refuses the new one as before.
    pub fn supersede(&mut self, node: u32) -> Result<Option<(u64, String)>> {
        if self.pending(node)?.is_none() {
            return Ok(None);
        }
        let earlier = self
            .unstarted(node)?
            .ok_or_else(|| invalid("The previous decision has not settled."))?;
        self.settle(node)?;
        self.superseded = Some(earlier.0);
        Ok(Some(earlier))
    }
    pub fn settle(&self, node: u32) -> Result<()> {
        self.settle_node(node, false)
    }
    pub fn settle_override(&self, node: u32) -> Result<()> {
        self.settle_node(node, true)
    }
    fn settle_node(&self, node: u32, preserve_ready: bool) -> Result<()> {
        rows::settle_decisions(
            self.db,
            &self.team.id,
            Some(node),
            &encode(self.actor)?,
            preserve_ready,
        )?;
        for table in ["nodes", "node_archive"] {
            self.db
                .execute(
                    &format!("UPDATE {table} SET requested=NULL WHERE team=?1 AND id=?2"),
                    params![self.team.id, node],
                )
                .map_err(storage)?;
        }
        Ok(())
    }
    pub fn reopening(&self, node: u32) -> Result<bool> {
        Ok(self.pending(node)?.is_some_and(|(_, kind, state)| {
            matches!(kind.as_str(), "return" | "reassign") && state == DecisionState::EffectReserved
        }))
    }
    pub fn advance(&self, node: u32, n: &mut Node) -> Result<()> {
        let next = self
            .generation(node)?
            .checked_add(1)
            .ok_or_else(|| over("Attempt generations are exhausted."))?;
        if n.kind == NodeKind::Task {
            if let Some(previous) = n.implementer.as_deref() {
                super::handout::record_earlier(self, node, previous)?;
            }
            n.implementer = None;
        }
        if n.kind != NodeKind::Stage {
            n.round = n
                .round
                .checked_add(1)
                .ok_or_else(|| over("Task rounds are exhausted."))?;
        }
        self.db.execute("UPDATE nodes SET generation=?3,data=?4,decided_by=NULL,integrated=0,integration_commit=NULL WHERE team=?1 AND id=?2",params![self.team.id,node,next,rows::node_payload(n)?]).map_err(storage)?;
        Ok(())
    }
    pub fn node_table(&self, id: u32) -> Result<&'static str> {
        let live: bool = self
            .db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM nodes WHERE team=?1 AND id=?2)",
                params![self.team.id, id],
                |r| r.get(0),
            )
            .map_err(storage)?;
        Ok(if live { "nodes" } else { "node_archive" })
    }
    pub fn decision(&self, node: u32, kind: &str, text: &str, state: DecisionState) -> Result<u64> {
        self.undecided(node)?;
        self.db.execute("INSERT INTO decisions(team,node,generation,actor,kind,text,state) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![self.team.id,node,self.generation(node)?,encode(self.actor)?,kind,text,state]).map_err(storage)?;
        let seq = self.db.last_insert_rowid() as u64;
        if state != DecisionState::Settled {
            self.db
                .execute(
                    "UPDATE nodes SET requested=?3 WHERE team=?1 AND id=?2",
                    params![self.team.id, node, kind],
                )
                .map_err(storage)?;
        }
        // A pending decision's hold is the decision's, read from it and never
        // stored. A hand-back's hold belongs to this next decision, which
        // takes it over (pending) or ends it (settled at once). An explicit
        // hold stays.
        rows::set_hand_back(self.db, &self.team.id, node, false)?;
        Ok(seq)
    }
    pub fn inbox(&self, node: u32, kind: &str, text: &str) -> Result<u64> {
        self.db.execute("INSERT INTO inbox(team,kind,node,generation,actor,text,created_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![self.team.id,kind,node,self.generation(node)?,encode(self.actor)?,text,self.now]).map_err(storage)?;
        Ok(self.db.last_insert_rowid() as u64)
    }
}
pub(super) fn invalid(text: &str) -> Problem {
    Problem::new(code::INVALID_STATE, text)
}
pub(super) fn over(text: &str) -> Problem {
    Problem::new(code::OVER_CAP, text)
}
pub(super) fn missing() -> Problem {
    Problem::new(code::NOT_FOUND, "Record does not exist.")
}
pub(super) fn stale() -> Problem {
    Problem::new(code::STALE_REVISION, "Refresh before making a new request.")
}
pub(super) fn uuid(value: &str) -> Result<()> {
    if uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value) {
        Ok(())
    } else {
        Err(invalid("Expected a lowercase canonical UUID."))
    }
}
pub(super) fn text(value: &str, limit: usize) -> Result<()> {
    if value.len() <= limit {
        Ok(())
    } else {
        Err(over("Text exceeds its size limit."))
    }
}
pub(super) fn reason(value: &str) -> Result<()> {
    text(value, 16_384)?;
    if value.trim().is_empty() {
        Err(invalid("A reason is required."))
    } else {
        Ok(())
    }
}
pub(super) fn is_person(actor: &Actor) -> bool {
    matches!(actor, Actor::Person { .. })
}
pub(super) fn is_router(ctx: &Context<'_>) -> bool {
    is_person(ctx.actor) || matches!(ctx.actor, Actor::Session{member,..} if member==&ctx.team.lead)
}
