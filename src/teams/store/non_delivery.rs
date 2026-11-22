//! Bounded compatibility reads for effects recorded before known non-delivery
//! was closed. These hints never authorize a delivery; ops rechecks the proof.
use super::*;
use crate::teams::ops::HandoutOutcome;

// Only a notice for the latest effect of a node's current hand-back can
// still ask the lead to act. There is at most one such notice per live node.
// The partial index omits all other inbox kinds and the closed history.
const STALE_REOPEN_NOTICES: &str = "
    SELECT i.seq FROM inbox i INDEXED BY inbox_open_reopen_refused
    WHERE i.team=?1 AND i.state='open' AND i.kind GLOB 'reopen_refused:*'
      AND NOT EXISTS (
        SELECT 1 FROM nodes n
        WHERE n.team=i.team AND n.id=i.node AND n.generation=i.generation AND n.hand_back
          AND i.kind='reopen_refused:' || (
            SELECT e.request_id FROM effects e
            WHERE e.team=n.team AND e.node=n.id AND e.generation=n.generation
            ORDER BY e.seq DESC LIMIT 1
          )
      )
    ORDER BY i.seq LIMIT ?2";

pub(in crate::teams) fn close_stale_reopen_notices(db: &Connection, team: &str) -> Result<usize> {
    db.execute(
        &format!("UPDATE inbox SET state='closed' WHERE seq IN ({STALE_REOPEN_NOTICES})"),
        params![team, 200],
    )
    .map_err(storage)
}

/// The original successful outcome receipt binds the inbox row exactly. Text,
/// timestamps and a coincident number from another table grant no authority.
pub(in crate::teams) fn non_delivery_item(
    db: &Connection,
    team: &str,
    id: &str,
    node: u32,
    generation: u32,
    kind: &str,
    outcome: &HandoutOutcome,
) -> Result<Option<u64>> {
    let op = if kind == "handout" {
        "handout_outcome"
    } else if kind == "reopen" {
        "reopen_outcome"
    } else {
        return Ok(None);
    };
    let encoded = serde_json::to_value(outcome).map_err(storage)?;
    let mut query = db.prepare("SELECT i.seq,r.content,r.result,r.actor FROM requests r INDEXED BY requests_effect_outcome JOIN inbox i ON i.seq=CASE WHEN json_valid(r.result) THEN json_extract(r.result,'$.Success.item') END WHERE r.team=?1 AND CASE WHEN json_valid(r.content) THEN json_extract(r.content,'$.effect_request_id') END=?2 AND i.team=?1 AND i.node=?3 AND i.generation=?4 AND i.kind='delivery_uncertain' AND i.state='open' ORDER BY r.id LIMIT 2").map_err(storage)?;
    let rows = query
        .query_map(params![team, id, node, generation], |r| {
            Ok((
                r.get::<_, u64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(storage)?;
    let mut item = None;
    for row in rows {
        let (seq, content, result, actor) = row.map_err(storage)?;
        let receipt: serde_json::Value = serde_json::from_str(&result).map_err(storage)?;
        let who: serde_json::Value = serde_json::from_str(&actor).map_err(storage)?;
        if receipt["Success"]["ok"] != true
            || receipt["Success"]["item"].as_u64() != Some(seq)
            || who["kind"] != "service"
            || who["team"] != team
        {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(&content).map_err(storage)?;
        if value["team"] != team || value["op"] != op || value["outcome"] != encoded {
            continue;
        }
        if item.is_some() {
            return Ok(None);
        }
        item = Some(seq);
    }
    Ok(item)
}

pub(in crate::teams) fn legacy_non_delivery_state(state: &str) -> bool {
    matches!(state, "cutting_over" | "refused" | "superseded")
}

impl Store {
    pub fn has_stale_reopen_notices(&self, team: &str) -> Result<bool> {
        self.active()?;
        let Some(db) = &self.db else {
            return Ok(false);
        };
        db.prepare(STALE_REOPEN_NOTICES)
            .and_then(|mut query| query.exists(params![team, 1]))
            .map_err(storage)
    }

    pub fn non_delivery_pending(&self, team: &str, effect: &EffectIntent) -> Result<bool> {
        self.active()?;
        let db = self
            .db
            .as_ref()
            .ok_or_else(|| storage("Teams storage is closed."))?;
        let current:Option<(u32,u32,String,String,Option<String>)>=db.query_row("SELECT node,generation,kind,state,outcome FROM effects WHERE team=?1 AND request_id=?2",params![team,effect.request_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(storage)?;
        let Some((node, generation, kind, state, Some(raw))) = current else {
            return Ok(false);
        };
        let Ok(outcome @ HandoutOutcome::CuttingOver { .. }) =
            serde_json::from_str::<HandoutOutcome>(&raw)
        else {
            return Ok(false);
        };
        if !matches!(kind.as_str(), "handout" | "reopen") {
            return Ok(false);
        }
        if !legacy_non_delivery_state(&state) {
            return Ok(false);
        }
        if state == "cutting_over" {
            return Ok(true);
        }
        non_delivery_item(
            db,
            team,
            &effect.request_id,
            node,
            generation,
            &kind,
            &outcome,
        )
        .map(|item| item.is_some())
    }

    /// A monotonic sequence checkpoint survives actor pruning. At most one
    /// indexed history page is examined per pass, once per existing effect.
    pub fn non_delivery_history(&self, team: &str) -> Result<Page<EffectIntent>> {
        self.active()?;
        let db = self
            .db
            .as_ref()
            .ok_or_else(|| storage("Teams storage is closed."))?;
        let after: u64 = db
            .query_row(
                "SELECT after_seq FROM non_delivery_progress WHERE team=?1",
                [team],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?
            .unwrap_or(0);
        let ceiling: u64 = db
            .query_row(
                "SELECT COALESCE(MAX(seq),0) FROM effects WHERE team=?1",
                [team],
                |r| r.get(0),
            )
            .map_err(storage)?;
        self.effects(
            team,
            200,
            Some(&Cursor {
                scope: format!("{team}:effects"),
                ceiling: ceiling.max(after),
                after,
                rank: String::new(),
            }),
        )
    }

    pub fn non_delivery_advanced(&mut self, team: &str, after: u64) -> Result<()> {
        let db = self
            .db
            .as_mut()
            .ok_or_else(|| storage("Teams storage is closed."))?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(begin_error)?;
        timers::fence(&tx)?;
        tx.execute("INSERT INTO non_delivery_progress(team,after_seq) VALUES(?1,?2) ON CONFLICT(team) DO UPDATE SET after_seq=MAX(after_seq,excluded.after_seq)",params![team,after]).map_err(storage)?;
        tx.commit().map_err(storage)
    }
}
