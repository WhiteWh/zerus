//! The evidence beside a task's decision (zerus-5gcbh): the task's latest
//! report and its stages, newest first, each with its latest verdict, read
//! from the rows the report and verdict writers stored. Bounded: one report
//! and at most [`MAX_STAGES`] stages, with `more_stages` beyond them.
use super::*;
use crate::teams::ops::ReportNote;
use rusqlite::{params, OptionalExtension};
use serde_json::Value;

/// The most stages one read returns (rounds times stages of a pipeline).
pub const MAX_STAGES: usize = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportEvidence {
    pub generation: u32,
    /// The member the report's own generation was handed to (its received
    /// effect), the only member the report writer accepts it from; `None`
    /// when that effect is gone. Never the task's current implementer, which
    /// a later Reassign changes before the next report.
    pub member: Option<String>,
    pub text: String,
    pub commit: String,
    pub notes: Vec<ReportNote>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerdictEvidence {
    pub generation: u32,
    /// The verdict's writer as the store encoded it (a proved stage holder).
    pub actor: Value,
    pub pass: bool,
    pub text: String,
}

/// A stage's metadata only: its task text and its copies of the report are
/// never read on this path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageEvidence {
    pub node: NodeId,
    /// The stage's kind in the team's bundle, or `None` when the bundle has
    /// no such pipeline stage.
    pub kind: Option<zerus_team_plan::catalog::StageKind>,
    pub round: u32,
    pub state: NodeState,
    pub verdict: Option<VerdictEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskEvidence {
    /// The team's revision in the same snapshot as the evidence.
    pub revision: u64,
    pub report: Option<ReportEvidence>,
    /// Stage nodes of the task, newest first, with each one's latest verdict.
    pub stages: Vec<StageEvidence>,
    pub more_stages: bool,
}

/// A node's metadata on this path: its kind, pipeline, stage index, round and state.
struct NodeMeta {
    kind: NodeKind,
    pipeline: String,
    stage_index: u32,
    round: u32,
    state: NodeState,
}

/// One node's metadata from `nodes` or `node_archive`, as `rows::node` reads
/// the payload: `kind`, `pipeline`, `stage_index` and `round` are omitted
/// from the payload at their defaults (task, empty, 0, 1).
fn node_meta(tx: &rusqlite::Transaction<'_>, team: &str, id: NodeId) -> Result<Option<NodeMeta>> {
    let columns = |table: &str| {
        format!("json_extract({table}.data,'$.kind'),json_extract({table}.data,'$.pipeline'),json_extract({table}.data,'$.stage_index'),json_extract({table}.data,'$.round'),{table}.node_state")
    };
    let sql = format!(
        "SELECT {} FROM nodes WHERE team=?1 AND id=?2 UNION ALL SELECT {} FROM node_archive WHERE team=?1 AND id=?2 LIMIT 1",
        columns("nodes"),
        columns("node_archive")
    );
    let row = tx
        .query_row(&sql, params![team, id], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<u32>>(2)?,
                r.get::<_, Option<u32>>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .optional()
        .map_err(storage)?;
    let Some((kind, pipeline, stage_index, round, state)) = row else {
        return Ok(None);
    };
    fn word<T: serde::de::DeserializeOwned>(value: String) -> Result<T> {
        serde_json::from_value(Value::String(value)).map_err(storage)
    }
    Ok(Some(NodeMeta {
        kind: kind.map(word).transpose()?.unwrap_or_default(),
        pipeline: pipeline.unwrap_or_default(),
        stage_index: stage_index.unwrap_or(0),
        round: round.unwrap_or(1),
        state: word(state)?,
    }))
}

impl Store {
    /// A task's evidence and the team's revision in one read transaction. A
    /// stage node has no evidence of its own: its verdict is read with its
    /// task.
    pub fn task_evidence(&self, team: &str, id: NodeId) -> Result<TaskEvidence> {
        self.active()?;
        let db = self
            .db
            .as_ref()
            .ok_or_else(|| Problem::new(code::NOT_FOUND, "Team does not exist."))?;
        let tx = db.unchecked_transaction().map_err(storage)?;
        let pinned = rows::team(&tx, team)?
            .ok_or_else(|| Problem::new(code::NOT_FOUND, "Team does not exist."))?;
        let task = node_meta(&tx, team, id)?
            .ok_or_else(|| Problem::new(code::NOT_FOUND, "Task does not exist in this team."))?;
        if task.kind == NodeKind::Stage {
            return Err(Problem::new(
                code::INVALID_STATE,
                "A stage's verdict is read with its task.",
            ));
        }
        let report = tx
            .query_row(
                "SELECT generation,text,commit_id,notes FROM reports WHERE team=?1 AND node=?2 ORDER BY generation DESC LIMIT 1",
                params![team, id],
                |r| {
                    Ok(ReportEvidence {
                        generation: r.get(0)?,
                        member: None,
                        text: r.get(1)?,
                        commit: r.get(2)?,
                        notes: sql_json(r, 3)?,
                    })
                },
            )
            .optional()
            .map_err(storage)?
            .map(|mut report| {
                // The recipient of the report's own generation, as the
                // report writer checked it (handout::recipient).
                report.member = tx
                    .query_row(
                        &format!("SELECT CASE WHEN state IN {NOT_RECEIVED} THEN NULL ELSE member END FROM effects WHERE team=?1 AND node=?2 AND generation=?3 ORDER BY seq DESC LIMIT 1"),
                        params![team, id, report.generation],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .optional()
                    .map(Option::flatten)
                    .map_err(storage)?;
                Ok::<_, Problem>(report)
            })
            .transpose()?;
        let mut statement = tx
            .prepare("SELECT node FROM stage_tasks WHERE team=?1 AND target=?2 ORDER BY node DESC LIMIT ?3")
            .map_err(storage)?;
        let ids = statement
            .query_map(params![team, id, MAX_STAGES as i64 + 1], |r| {
                r.get::<_, u32>(0)
            })
            .map_err(storage)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage)?;
        drop(statement);
        let more_stages = ids.len() > MAX_STAGES;
        let mut stages = Vec::with_capacity(ids.len().min(MAX_STAGES));
        for stage in ids.into_iter().take(MAX_STAGES) {
            // A wanted stage task has no node yet: only created stages show.
            let Some(meta) = node_meta(&tx, team, stage)? else {
                continue;
            };
            let kind = pinned
                .bundle
                .pipelines
                .iter()
                .find(|candidate| candidate.id == meta.pipeline)
                .and_then(|candidate| candidate.stages.get(meta.stage_index as usize))
                .map(|step| step.kind);
            let verdict = tx
                .query_row(
                    "SELECT generation,actor,pass,text FROM verdicts WHERE team=?1 AND node=?2 ORDER BY generation DESC LIMIT 1",
                    params![team, stage],
                    |r| {
                        Ok(VerdictEvidence {
                            generation: r.get(0)?,
                            actor: sql_json(r, 1)?,
                            pass: r.get(2)?,
                            text: r.get(3)?,
                        })
                    },
                )
                .optional()
                .map_err(storage)?;
            stages.push(StageEvidence {
                node: stage,
                kind,
                round: meta.round,
                state: meta.state,
                verdict,
            });
        }
        tx.commit().map_err(storage)?;
        Ok(TaskEvidence {
            revision: pinned.revision,
            report,
            stages,
            more_stages,
        })
    }
}
