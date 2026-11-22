//! Durable, team-local availability observations. Marks are advisory and
//! confer no ability to deliver work or act for a member.
use super::*;
use rusqlite::params;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const MAX_READINGS: usize = super::member_sessions::MAX_ROSTER - 1;
const PERIOD_MS: i64 = 180_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Readability {
    marked: bool,
    pending_since_ms: Option<i64>,
}

impl Readability {
    /// A contrary observation must persist for a full period before it
    /// changes the mark. A matching observation cancels a pending change.
    pub(crate) fn observe(self, unreadable: bool, now: i64) -> Self {
        if unreadable == self.marked {
            return Self {
                pending_since_ms: None,
                ..self
            };
        }
        match self.pending_since_ms.filter(|since| *since <= now) {
            Some(since) if now.saturating_sub(since) >= PERIOD_MS => Self {
                marked: unreadable,
                pending_since_ms: None,
            },
            Some(_) => self,
            None => Self {
                pending_since_ms: Some(now),
                ..self
            },
        }
    }
}

pub(crate) type Readabilities = BTreeMap<String, Readability>;

pub(in crate::teams) fn read(db: &Connection, team: &str) -> Result<Readabilities> {
    let rows = db
        .prepare("SELECT member,marked,pending_since_ms FROM team_member_readability WHERE team=?1 ORDER BY member LIMIT ?2")
        .and_then(|mut query| {
            query
                .query_map(params![team, MAX_READINGS + 1], |row| {
                    Ok((row.get(0)?, Readability {
                        marked: row.get(1)?,
                        pending_since_ms: row.get(2)?,
                    }))
                })?
                .collect::<rusqlite::Result<Readabilities>>()
        })
        .map_err(storage)?;
    if rows.len() > MAX_READINGS {
        return Err(Problem::new(
            code::OVER_CAP,
            "The team has too many member readability facts.",
        ));
    }
    Ok(rows)
}

pub(in crate::teams) fn save(
    db: &Connection,
    team: &str,
    member: &str,
    state: Readability,
) -> Result<()> {
    if state == Readability::default() {
        db.execute(
            "DELETE FROM team_member_readability WHERE team=?1 AND member=?2",
            params![team, member],
        )
    } else {
        db.execute(
            "INSERT INTO team_member_readability(team,member,marked,pending_since_ms)
             VALUES (?1,?2,?3,?4) ON CONFLICT(team,member) DO UPDATE SET
               marked=excluded.marked,pending_since_ms=excluded.pending_since_ms",
            params![team, member, state.marked, state.pending_since_ms],
        )
    }
    .map_err(storage)?;
    Ok(())
}

pub(in crate::teams) fn marked(db: &Connection, team: &str) -> Result<BTreeSet<String>> {
    Ok(read(db, team)?
        .into_iter()
        .filter_map(|(id, readability)| readability.marked.then_some(id))
        .collect())
}

impl Store {
    pub(crate) fn member_readability(&self, team: &str) -> Result<Readabilities> {
        self.active()?;
        let db = self
            .db
            .as_ref()
            .ok_or_else(|| storage("Teams storage is closed."))?;
        read(db, team)
    }

    pub(crate) fn unreadable_members(&self, team: &str) -> Result<BTreeSet<String>> {
        self.active()?;
        let db = self
            .db
            .as_ref()
            .ok_or_else(|| storage("Teams storage is closed."))?;
        marked(db, team)
    }
}
