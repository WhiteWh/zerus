use super::*;
use rusqlite::params;

pub(crate) const MEMBER_PAGE_SQL: &str = "SELECT data,person_stopped FROM members WHERE team=?1 AND rowid<=?2 AND id>?3 ORDER BY id LIMIT ?4";
/// A live row's holder, as package 9's attempt holder reads it (the newest
/// effect of the current generation, unless reserved, refused or
/// superseded), and the kind of its one unsettled decision: two point
/// searches per row in the page's own query. An archived row has no holder.
/// The node page's one query: the rows of one state in id order, or the
/// whole queue in rank order, with each row's facts.
pub(crate) fn node_page_sql(table: &str, filtered: bool) -> String {
    let facts = row_facts(table);
    if filtered {
        format!("SELECT {},{facts} FROM {table} WHERE team=?1 AND node_state=?5 AND id>?2 AND id<=?3 ORDER BY id LIMIT ?4",rows::node_select(table))
    } else {
        format!("SELECT {},{facts} FROM nodes WHERE team=?1 AND id<=?3 AND (rank>?5 OR (rank=?5 AND id>?2)) ORDER BY rank,id LIMIT ?4",rows::node_select("nodes"))
    }
}
fn row_facts(table: &str) -> String {
    let holder = if table == "nodes" {
        format!("(SELECT CASE WHEN e.state IN {} THEN NULL ELSE e.member END FROM effects e WHERE e.team=nodes.team AND e.node=nodes.id AND e.generation=nodes.generation ORDER BY e.seq DESC LIMIT 1)", super::holder::NOT_HOLDING)
    } else {
        "NULL".to_owned()
    };
    // The stop facts, read from the store's own records (zerus-63rru.52):
    // the newest recorded cancel, the key clear, and whether a cancel is
    // still pending. Additive columns; the plain node pages ignore them.
    // The stop facts are bound to the stop's OWN identity, the monotonic
    // stop sequence stamped on the decisions the stop itself wrote: a
    // cancel or key clear recorded for an earlier live decision is never
    // shown as this stop's, whatever its text (zerus-63rru.52 round 3).
    let base = format!("(SELECT {} FROM effects e {} WHERE e.team={table}.team AND e.node={table}.id AND e.generation={table}.generation AND e.state NOT IN ('refused','superseded') ORDER BY e.seq DESC LIMIT 1)", super::bases::VALUE, super::bases::JOINS);
    let facts = format!(
        "{holder},(SELECT d.kind FROM decisions d WHERE d.team={table}.team AND d.node={table}.id AND d.state<>'settled'),(SELECT d.state FROM decisions d WHERE d.team={table}.team AND d.node={table}.id AND d.state<>'settled'),(SELECT i.text FROM inbox i WHERE i.team={table}.team AND i.node={table}.id AND i.kind LIKE 'unstaffed:%' AND i.reply IS NULL ORDER BY i.seq DESC LIMIT 1),(SELECT o.state FROM cancel_outcomes o JOIN decisions d ON d.seq=o.decision AND d.stop=(SELECT stop_seq FROM teams WHERE id={table}.team) WHERE o.team={table}.team AND o.node={table}.id ORDER BY o.decision DESC LIMIT 1),(SELECT o.reason FROM cancel_outcomes o JOIN decisions d ON d.seq=o.decision AND d.stop=(SELECT stop_seq FROM teams WHERE id={table}.team) WHERE o.team={table}.team AND o.node={table}.id ORDER BY o.decision DESC LIMIT 1),(SELECT CASE WHEN EXISTS(SELECT 1 FROM cancel_outcomes o JOIN decisions d ON d.seq=o.decision AND d.stop=(SELECT stop_seq FROM teams WHERE id={table}.team) WHERE o.team={table}.team AND o.node={table}.id AND o.state IN ('cancelled','released')) THEN k.state END FROM key_clears k WHERE k.team={table}.team AND k.node={table}.id),(SELECT CASE WHEN EXISTS(SELECT 1 FROM cancel_outcomes o JOIN decisions d ON d.seq=o.decision AND d.stop=(SELECT stop_seq FROM teams WHERE id={table}.team) WHERE o.team={table}.team AND o.node={table}.id AND o.state IN ('cancelled','released')) THEN k.reason END FROM key_clears k WHERE k.team={table}.team AND k.node={table}.id),(SELECT EXISTS(SELECT 1 FROM decisions d WHERE d.team={table}.team AND d.node={table}.id AND d.kind='cancel' AND d.state='pending' AND d.stop=(SELECT stop_seq FROM teams WHERE id={table}.team))),(SELECT stop_seq FROM teams WHERE id={table}.team)",
        table = table
    );
    // zerus-3u5yu (5): a stage row's target's earlier implementers, whom the
    // stage's Reassign and hand-out refuse; an empty array for other rows.
    let earlier = format!("(SELECT json_group_array(member) FROM (SELECT t.member FROM target_implementers t WHERE t.team={table}.team AND t.target=json_extract({table}.data,'$.stage_of') ORDER BY t.member LIMIT 200))");
    format!(
        "{facts},{},{base},{earlier}",
        super::dependency_edit::eligible_sql(table)
    )
}

fn start(
    db: Option<&Connection>,
    scope: String,
    table: &str,
    team: &str,
    limit: u32,
    cursor: Option<&Cursor>,
) -> Result<Cursor> {
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(Problem::new(
            code::OVER_CAP,
            "Page limit must be between 1 and 200.",
        ));
    }
    if let Some(cursor) = cursor {
        if cursor.scope != scope || cursor.after > cursor.ceiling {
            return Err(Problem::new(
                code::INVALID_STATE,
                "Cursor belongs to a different list.",
            ));
        }
        return Ok(cursor.clone());
    }
    let ceiling = if let Some(db) = db {
        let sql=match table {
            "nodes"=>"SELECT COALESCE(MAX(id),0) FROM node_identity WHERE team=?1".to_owned(),
            "journal"=>"SELECT MAX(COALESCE((SELECT MAX(seq) FROM journal WHERE team=?1),0),COALESCE((SELECT MAX(seq) FROM journal_archive WHERE team=?1),0))".to_owned(),
            "members"=>"SELECT COALESCE(MAX(rowid),0) FROM members WHERE team=?1".to_owned(),
            _=>format!("SELECT COALESCE(MAX(seq),0) FROM {table} WHERE team=?1"),
        };
        db.query_row(&sql, [team], |r| r.get(0)).map_err(storage)?
    } else {
        0
    };
    Ok(Cursor {
        scope,
        ceiling,
        after: 0,
        rank: String::new(),
    })
}
pub(super) fn finish<T>(
    rows: Vec<T>,
    mut cursor: Cursor,
    limit: u32,
    key: impl FnOnce(&T) -> (u64, String),
) -> Page<T> {
    let next = if rows.len() == limit as usize {
        if let Some(last) = rows.last() {
            let (after, rank) = key(last);
            cursor.after = after;
            cursor.rank = rank;
        }
        Some(cursor)
    } else {
        None
    };
    Page { rows, next }
}
impl Store {
    pub fn members(&self, team: &str, limit: u32, cursor: Option<&Cursor>) -> Result<Page<Member>> {
        let page = self.member_rows(team, limit, cursor)?;
        Ok(Page {
            rows: page.rows.into_iter().map(|r| r.member).collect(),
            next: page.next,
        })
    }
    /// The members page with whether the person stopped each, read in the
    /// page's own query.
    pub fn member_rows(
        &self,
        team: &str,
        limit: u32,
        cursor: Option<&Cursor>,
    ) -> Result<Page<MemberRow>> {
        self.active()?;
        let c = start(
            self.db.as_ref(),
            format!("{team}:members"),
            "members",
            team,
            limit,
            cursor,
        )?;
        let mut rows = Vec::new();
        if let Some(db) = &self.db {
            let mut s = db.prepare(MEMBER_PAGE_SQL).map_err(storage)?;
            rows = s
                .query_map(params![team, c.ceiling, c.rank, limit], |r| {
                    Ok(MemberRow {
                        member: sql_json(r, 0)?,
                        person_stopped: r.get(1)?,
                    })
                })
                .map_err(storage)?
                .collect::<std::result::Result<Vec<MemberRow>, _>>()
                .map_err(storage)?;
        }
        Ok(finish(rows, c, limit, |r| (0, r.member.id.clone())))
    }
    pub fn phases(&self, team: &str, limit: u32, cursor: Option<&Cursor>) -> Result<Page<Phase>> {
        self.active()?;
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(Problem::new(
                code::OVER_CAP,
                "Page limit must be between 1 and 200.",
            ));
        }
        let scope = format!("{team}:phases");
        let c = if let Some(c) = cursor {
            if c.scope != scope || c.after > c.ceiling {
                return Err(Problem::new(
                    code::INVALID_STATE,
                    "cursor belongs to another list",
                ));
            }
            c.clone()
        } else {
            let ceiling = self.db.as_ref().map_or(Ok(0), |db| {
                db.query_row(
                    "SELECT COALESCE(MAX(n),0)+1 FROM phases WHERE team=?1",
                    [team],
                    |r| r.get(0),
                )
                .map_err(storage)
            })?;
            Cursor {
                scope,
                ceiling,
                after: 0,
                rank: String::new(),
            }
        };
        let mut rows = Vec::new();
        if let Some(db) = &self.db {
            let mut s = db
                .prepare(
                    "SELECT data FROM phases WHERE team=?1 AND n>=?2 AND n<?3 ORDER BY n LIMIT ?4",
                )
                .map_err(storage)?;
            rows = s
                .query_map(params![team, c.after, c.ceiling, limit], |r| sql_json(r, 0))
                .map_err(storage)?
                .collect::<std::result::Result<Vec<Phase>, _>>()
                .map_err(storage)?;
        }
        Ok(finish(rows, c, limit, |r| {
            (u64::from(r.n) + 1, String::new())
        }))
    }
    pub fn queue(&self, team: &str, limit: u32, cursor: Option<&Cursor>) -> Result<Page<Node>> {
        self.node_page(team, None, limit, cursor)
    }
    /// The queue page with each row's holder and pending decision, read in
    /// the page's own query.
    pub fn queue_rows(
        &self,
        team: &str,
        limit: u32,
        cursor: Option<&Cursor>,
    ) -> Result<Page<QueueRow>> {
        self.node_rows_with(team, None, limit, cursor, || {})
    }
    pub fn nodes_by_state(
        &self,
        team: &str,
        state: NodeState,
        limit: u32,
        cursor: Option<&Cursor>,
    ) -> Result<Page<Node>> {
        self.node_page(team, Some(state), limit, cursor)
    }
    fn node_page(
        &self,
        team: &str,
        state: Option<NodeState>,
        limit: u32,
        cursor: Option<&Cursor>,
    ) -> Result<Page<Node>> {
        self.node_page_with(team, state, limit, cursor, || {})
    }
    pub(super) fn node_page_with(
        &self,
        team: &str,
        state: Option<NodeState>,
        limit: u32,
        cursor: Option<&Cursor>,
        after_rows: impl FnOnce(),
    ) -> Result<Page<Node>> {
        let page = self.node_rows_with(team, state, limit, cursor, after_rows)?;
        Ok(Page {
            rows: page.rows.into_iter().map(|r| r.node).collect(),
            next: page.next,
        })
    }
    fn node_rows_with(
        &self,
        team: &str,
        state: Option<NodeState>,
        limit: u32,
        cursor: Option<&Cursor>,
        after_rows: impl FnOnce(),
    ) -> Result<Page<QueueRow>> {
        self.active()?;
        let filter = state
            .map(|s| encode(&s))
            .transpose()?
            .map(|s| s.trim_matches('"').to_owned());
        let scope = format!("{team}:nodes:{filter:?}");
        let Some(db) = &self.db else {
            let c = start(None, scope, "nodes", team, limit, cursor)?;
            return Ok(finish(Vec::new(), c, limit, |n: &QueueRow| {
                (u64::from(n.node.id), n.node.rank.clone())
            }));
        };
        // The high-water mark, node rows and every dependency row share the
        // same SQLite snapshot, including while a concurrent Route commits.
        let tx = db.unchecked_transaction().map_err(storage)?;
        let c = start(Some(&tx), scope, "nodes", team, limit, cursor)?;
        let table = if matches!(state, Some(NodeState::Accepted | NodeState::Cancelled)) {
            "node_archive"
        } else {
            "nodes"
        };
        let sql = node_page_sql(table, filter.is_some());
        let mut nodes = {
            let mut statement = tx.prepare(&sql).map_err(storage)?;
            let mapped = statement
                .query_map(
                    params![
                        team,
                        c.after,
                        c.ceiling,
                        limit,
                        filter.as_deref().unwrap_or(&c.rank)
                    ],
                    |r| {
                        let pending: Option<String> = r.get(8)?;
                        let state: Option<DecisionState> = r.get(9)?;
                        Ok(QueueRow {
                            deps_editable: r.get(17)?,
                            assigned_base: r.get(18)?,
                            earlier_implementers: sql_json(r, 19)?,
                            node: rows::node_row(r)?,
                            holder: r.get(7)?,
                            pending_replaceable: pending
                                .as_deref()
                                .zip(state)
                                .is_some_and(|(kind, state)| state.replaceable(kind)),
                            pending,
                            unstaffed: r.get(10)?,
                            stop: Some(StopFacts {
                                cancel: r.get(11)?,
                                cancel_reason: r.get(12)?,
                                key: r.get(13)?,
                                key_reason: r.get(14)?,
                                pending_cancel: r.get::<_, bool>(15)?,
                                stop_seq: r.get(16)?,
                            }),
                        })
                    },
                )
                .map_err(storage)?;
            mapped
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(storage)?
        };
        after_rows();
        for n in &mut nodes {
            n.node.deps = rows::deps(&tx, team, n.node.id)?;
        }
        tx.commit().map_err(storage)?;
        Ok(finish(nodes, c, limit, |n| {
            (
                u64::from(n.node.id),
                if state.is_none() {
                    n.node.rank.clone()
                } else {
                    String::new()
                },
            )
        }))
    }
    /// The open `member_started` items of a member, for the actor's close
    /// decision (zerus-63rru.86 round 3): the member's segment with its
    /// delimiter, so a prefix of another member never matches.
    pub fn open_member_started(&self, team: &str, member: &str) -> Result<i64> {
        let db = self.db.as_ref().ok_or_else(|| storage("Teams storage is closed."))?;
        rows::open_member_started(db, team, member)
    }

    pub fn inbox(
        &self,
        team: &str,
        limit: u32,
        cursor: Option<&Cursor>,
    ) -> Result<Page<InboxItem>> {
        self.seq_page(
            team,
            "inbox",
            limit,
            cursor,
            |r| {
                Ok(InboxItem {
                    seq: r.get(0)?,
                    kind: r.get(1)?,
                    node: r.get(2)?,
                    generation: r.get(3)?,
                    actor: sql_json(r, 4)?,
                    text: r.get(5)?,
                    notes: sql_json(r, 6)?,
                    state: r.get(7)?,
                    reply: r.get(8)?,
                    reply_actor: r
                        .get::<_, Option<String>>(9)?
                        .map(|s| serde_json::from_str(&s))
                        .transpose()
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                    created_ms: r.get(10)?,
                })
            },
            "seq,kind,node,generation,actor,text,notes,state,reply,reply_actor,created_ms",
        )
    }
    pub fn cards(&self, team: &str, limit: u32, cursor: Option<&Cursor>) -> Result<Page<Card>> {
        self.seq_page(
            team,
            "cards",
            limit,
            cursor,
            rows::card_row,
            "seq,kind,node,actor,text,state,decision,decided_actor,created_ms,decisions,notes,parameters",
        )
    }
    pub fn journal(
        &self,
        team: &str,
        limit: u32,
        cursor: Option<&Cursor>,
    ) -> Result<Page<JournalEntry>> {
        self.seq_page(
            team,
            "journal",
            limit,
            cursor,
            |r| {
                Ok(JournalEntry {
                    seq: r.get(0)?,
                    at_ms: r.get(1)?,
                    actor: sql_json(r, 2)?,
                    kind: r.get(3)?,
                    request_id: r.get(4)?,
                    text: r.get(5)?,
                    refs: sql_json(r, 6)?,
                })
            },
            "seq,at_ms,actor,kind,request_id,text,refs",
        )
    }
    pub fn effects(
        &self,
        team: &str,
        limit: u32,
        cursor: Option<&Cursor>,
    ) -> Result<Page<EffectIntent>> {
        self.seq_page(
            team,
            "effects",
            limit,
            cursor,
            |r| {
                Ok(EffectIntent {
                    seq: r.get(0)?,
                    node: r.get(1)?,
                    member: r.get(2)?,
                    generation: r.get(3)?,
                    request_id: r.get(4)?,
                    actor: sql_json(r, 5)?,
                    state: r.get(6)?,
                    kind: r.get(9)?,
                    outcome: r
                        .get::<_, Option<String>>(7)?
                        .map(|s| serde_json::from_str(&s))
                        .transpose()
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                    created_ms: r.get(8)?,
                })
            },
            "seq,node,member,generation,request_id,actor,state,outcome,created_ms,kind",
        )
    }
    pub fn decisions(
        &self,
        team: &str,
        limit: u32,
        cursor: Option<&Cursor>,
    ) -> Result<Page<Decision>> {
        self.seq_page(
            team,
            "decisions",
            limit,
            cursor,
            |r| {
                Ok(Decision {
                    state: r.get(6)?,
                    seq: r.get(0)?,
                    node: r.get(1)?,
                    generation: r.get(2)?,
                    actor: sql_json(r, 3)?,
                    kind: r.get(4)?,
                    text: r.get(5)?,
                })
            },
            "seq,node,generation,actor,kind,text,state",
        )
    }
    /// The node's latest decision's sequence, for the native admission's
    /// authority check (zerus-63rru.37 round 3): a later decision — the
    /// person's review that supersedes the acceptance — revokes an older
    /// Ready acceptance's Git write authority by its own durable row,
    /// whether or not the supersede has run.
    pub fn latest_decision(&self, team: &str, node: u32) -> Result<Option<u64>> {
        let db = self.db.as_ref().ok_or_else(|| storage("Teams storage is closed."))?;
        db.query_row(
            "SELECT MAX(seq) FROM decisions WHERE team=?1 AND node=?2",
            params![team, node],
            |r| r.get(0),
        )
        .map_err(storage)
    }

    /// The node's bound task and the task revision that its last applied
    /// ObserveTask wrote, from the live or the archived row; None without the
    /// node (zerus-63rru.37 round 4). Native merge admission compares this
    /// revision with the task's current one in the replica: a Git write needs
    /// a store that has applied the accepted task's current state.
    pub fn applied_task(&self, team: &str, node: u32) -> Result<Option<(Option<TaskRef>, String)>> {
        self.active()?;
        let db = self.db.as_ref().ok_or_else(|| storage("Teams storage is closed."))?;
        db.query_row(
            "SELECT data,task_revision FROM nodes WHERE team=?1 AND id=?2 UNION ALL SELECT data,task_revision FROM node_archive WHERE team=?1 AND id=?2 LIMIT 1",
            params![team, node],
            |r| Ok((sql_json::<Node>(r, 0)?.task, r.get(1)?)),
        )
        .optional()
        .map_err(storage)
    }

    /// Discover only Ready accepts among open decisions, in node order.
    /// The limit bounds examined open rows, so an empty page can have `next`.
    /// Preserved settled accepts are facts already in flight, never new work.
    pub fn integration_accepts(
        &self,
        team: &str,
        limit: u32,
        cursor: Option<&Cursor>,
    ) -> Result<Page<IntegrationAccept>> {
        self.active()?;
        let c = start(
            self.db.as_ref(),
            format!("{team}:integration_accepts"),
            "nodes",
            team,
            limit,
            cursor,
        )?;
        let mut scanned = Vec::new();
        if self
            .team(team)?
            .is_some_and(|t| t.settings.integration == zerus_team_plan::model::Integration::Branch)
        {
            if let Some(db) = &self.db {
                let mut s = db.prepare(&integration_accepts_sql()).map_err(storage)?;
                scanned = s
                    .query_map(
                        params![team, c.after, c.ceiling, limit, DecisionState::Ready],
                        |r| {
                            let row = IntegrationAccept {
                                seq: r.get(0)?,
                                node: r.get(1)?,
                                generation: r.get(2)?,
                            };
                            Ok((row.node, r.get::<_, bool>(3)?.then_some(row)))
                        },
                    )
                    .map_err(storage)?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(storage)?;
            }
        }
        let page = finish(scanned, c, limit, |r| (u64::from(r.0), String::new()));
        Ok(Page {
            rows: page.rows.into_iter().filter_map(|(_, r)| r).collect(),
            next: page.next,
        })
    }
    fn seq_page<T>(
        &self,
        team: &str,
        table: &str,
        limit: u32,
        cursor: Option<&Cursor>,
        read: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
        columns: &str,
    ) -> Result<Page<T>> {
        self.active()?;
        let c = start(
            self.db.as_ref(),
            format!("{team}:{table}"),
            table,
            team,
            limit,
            cursor,
        )?;
        let mut rows = Vec::new();
        if let Some(db) = &self.db {
            let sql = if table == "journal" {
                format!("SELECT {columns} FROM journal WHERE team=?1 AND seq>?2 AND seq<=?3 UNION ALL SELECT {columns} FROM journal_archive WHERE team=?1 AND seq>?2 AND seq<=?3 ORDER BY seq LIMIT ?4")
            } else {
                format!("SELECT {columns} FROM {table} WHERE team=?1 AND seq>?2 AND seq<=?3 ORDER BY seq LIMIT ?4")
            };
            let mut s = db.prepare(&sql).map_err(storage)?;
            // seq is read separately so the generic row itself need not expose it.
            rows = s
                .query_map(params![team, c.after, c.ceiling, limit], {
                    let mut read = read;
                    move |r| Ok((r.get::<_, u64>(0)?, read(r)?))
                })
                .map_err(storage)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(storage)?;
        }
        let page = finish(rows, c, limit, |r| (r.0, String::new()));
        Ok(Page {
            rows: page.rows.into_iter().map(|(_, r)| r).collect(),
            next: page.next,
        })
    }
}

pub(super) fn integration_accepts_sql() -> String {
    format!(
        "SELECT d.seq,d.node,d.generation,
            d.kind='accept' AND d.state=?5
            AND NOT EXISTS(SELECT 1 FROM integration_facts f WHERE f.decision=d.seq)
            AND EXISTS(
                SELECT 1 FROM nodes n WHERE n.team=d.team AND n.id=d.node
                    AND COALESCE(json_extract(n.data,'$.kind'),'task')<>'stage'
                UNION ALL
                SELECT 1 FROM node_archive n WHERE n.team=d.team AND n.id=d.node
                    AND COALESCE(json_extract(n.data,'$.kind'),'task')<>'stage')
         FROM decisions d
         WHERE d.team=?1 AND d.node>?2 AND d.node<=?3 AND d.state<>'{}'
         ORDER BY d.node LIMIT ?4",
        DecisionState::Settled.as_str()
    )
}
