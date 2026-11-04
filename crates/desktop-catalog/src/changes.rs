//! Immutable, bounded display deltas. Snapshot replacement needs a new baseline;
//! ordinary changes retain their payload until an explicit retention gap.
use super::*;
use std::collections::BTreeMap;

pub const MAX_ITEMS: i64 = 32_768;
pub const MAX_BYTES: i64 = 16 << 20;

#[derive(Debug)]
pub struct RetentionGap;
impl std::fmt::Display for RetentionGap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("catalog changes retention gap; baseline required")
    }
}
impl std::error::Error for RetentionGap {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub source: Source,
    /// Opaque identity of the visible snapshot, including same-source resnapshots.
    pub projection: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Watermark {
    pub scope: Scope,
    pub revision: Option<i64>,
    pub online: bool,
    pub complete: bool,
}
#[derive(Clone, Debug)]
pub struct Cursor {
    pub scope: Scope,
    pub since: i64,
    pub through: i64,
    pub after_revision: i64,
    pub after_key: String,
}
#[derive(Debug)]
pub struct Change {
    pub revision: i64,
    pub key: String,
    pub json: Option<String>,
}
pub struct Page {
    pub items: Vec<Change>,
    pub next: Option<Cursor>,
    pub through: i64,
}

/// Root schema 13. Existing retained snapshots deliberately have no watermark:
/// restart cannot inherit a prior actor's coverage or revision continuity.
pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE desktop_catalog_change_views(
           generation INTEGER PRIMARY KEY, projection TEXT NOT NULL,
           epoch TEXT NOT NULL, incarnation INTEGER NOT NULL, source_generation INTEGER NOT NULL,
           head INTEGER NOT NULL DEFAULT 0, floor INTEGER NOT NULL DEFAULT 0,
           dirty INTEGER NOT NULL DEFAULT 0);
         CREATE INDEX desktop_catalog_change_dirty ON desktop_catalog_change_views(dirty,generation);
         CREATE TABLE desktop_catalog_changes(
           id INTEGER PRIMARY KEY AUTOINCREMENT, generation INTEGER NOT NULL,
           revision INTEGER NOT NULL, key TEXT NOT NULL, json TEXT, bytes INTEGER NOT NULL,
           UNIQUE(generation,revision,key));
         CREATE TABLE desktop_catalog_change_totals(
           id INTEGER PRIMARY KEY CHECK(id=1), items INTEGER NOT NULL, bytes INTEGER NOT NULL);
         INSERT INTO desktop_catalog_change_totals VALUES(1,0,0);
         CREATE TRIGGER desktop_catalog_change_insert AFTER INSERT ON desktop_catalog_changes BEGIN
           UPDATE desktop_catalog_change_totals SET items=items+1,bytes=bytes+new.bytes WHERE id=1;
         END;
         CREATE TRIGGER desktop_catalog_change_delete AFTER DELETE ON desktop_catalog_changes BEGIN
           UPDATE desktop_catalog_change_totals SET items=items-1,bytes=bytes-old.bytes WHERE id=1;
         END;",
    )?;
    Ok(())
}

pub(super) fn publish(tx: &Transaction<'_>, source: &Source, old: Option<i64>) -> Result<()> {
    tx.execute(
        "UPDATE desktop_catalog_change_views SET dirty=1 WHERE generation=?1",
        [old],
    )?;
    tx.execute(
        "INSERT INTO desktop_catalog_change_views(generation,projection,epoch,incarnation,source_generation)
         SELECT visible,lower(hex(randomblob(16))),epoch,incarnation,generation
         FROM desktop_catalog_sources WHERE computer=?1", [&source.computer],
    )?;
    Ok(())
}

pub fn watermark(db: &Connection, computer: &str) -> Result<Watermark> {
    let s = state(db, computer)?.context("unknown catalog source")?;
    let view = db.query_row(
        "SELECT projection,head,epoch,incarnation,source_generation FROM desktop_catalog_change_views WHERE generation=?1",
        [s.visible], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?)),
    ).optional()?;
    let matches = view
        .as_ref()
        .is_some_and(|v| v.2 == s.epoch && v.3 == s.incarnation && v.4 == s.generation);
    Ok(Watermark {
        scope: Scope {
            source: Source {
                computer: computer.into(),
                service_epoch: s.epoch,
                incarnation: s.incarnation as u64,
                generation: s.generation as u64,
            },
            projection: view.as_ref().map(|v| v.0.clone()),
        },
        revision: matches.then(|| view.as_ref().unwrap().1),
        online: s.active,
        complete: s.active && s.complete && matches,
    })
}

pub(super) fn record(tx: &Transaction<'_>, generation: i64, delta: &watch::Delta) -> Result<()> {
    // One upstream frame bounds both this temporary map and the work it admits.
    // A key appearing repeatedly in a revision has exactly its final outcome.
    let mut rows = BTreeMap::new();
    for item in &delta.upsert {
        rows.insert(item.key.clone(), Some(row_json(item)?));
    }
    for item in &delta.remove {
        rows.insert(item.key.clone(), None);
    }
    let head: i64 = tx.query_row(
        "SELECT head FROM desktop_catalog_change_views WHERE generation=?1",
        [generation],
        |r| r.get(0),
    )?;
    let revision = head.checked_add(1).context("catalog revisions exhausted")?;
    let bytes: i64 = rows
        .iter()
        .map(|(k, v)| charge(k, v.as_deref().unwrap_or("")))
        .sum();
    let usage = |db: &Connection| -> Result<(i64, i64)> {
        Ok(db.query_row(
            "SELECT items,bytes FROM desktop_catalog_change_totals WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    };
    let (mut count, mut stored) = usage(tx)?;
    if count + rows.len() as i64 > MAX_ITEMS || stored + bytes > MAX_BYTES {
        // Only one bounded prefix per ingress call. If it is insufficient, keep
        // the catalog mutation and record an explicit gap for this revision.
        evict(tx)?;
        (count, stored) = usage(tx)?;
    }
    tx.execute(
        "UPDATE desktop_catalog_change_views SET head=?2 WHERE generation=?1",
        params![generation, revision],
    )?;
    if count + rows.len() as i64 > MAX_ITEMS || stored + bytes > MAX_BYTES {
        tx.execute(
            "UPDATE desktop_catalog_change_views SET floor=?2,dirty=1 WHERE generation=?1",
            params![generation, revision],
        )?;
        return Ok(());
    }
    for (key, json) in rows {
        let bytes = charge(&key, json.as_deref().unwrap_or(""));
        tx.execute("INSERT INTO desktop_catalog_changes(generation,revision,key,json,bytes) VALUES(?1,?2,?3,?4,?5)", params![generation,revision,key,json,bytes])?;
    }
    Ok(())
}

/// One immutable high-water. A gap anywhere after the original start invalidates
/// the whole walk, including already returned pages; callers need a baseline.
pub fn page(db: &Connection, scope: &Scope, since: i64, cursor: Option<&Cursor>) -> Result<Page> {
    let current = watermark(db, &scope.source.computer)?;
    ensure!(
        current.scope == *scope && current.complete,
        "catalog source reset or offline"
    );
    let generation: i64 = db.query_row(
        "SELECT visible FROM desktop_catalog_sources WHERE computer=?1",
        [&scope.source.computer],
        |r| r.get(0),
    )?;
    let (head, floor): (i64, i64) = db.query_row(
        "SELECT head,floor FROM desktop_catalog_change_views WHERE generation=?1",
        [generation],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if since < floor {
        return Err(RetentionGap.into());
    }
    ensure!(since >= 0 && since <= head, "invalid catalog revision");
    if since == head && cursor.is_none() {
        return Ok(Page {
            items: Vec::new(),
            next: None,
            through: head,
        });
    }
    let c = cursor.cloned().unwrap_or_else(|| Cursor {
        scope: scope.clone(),
        since,
        through: head,
        after_revision: since + 1,
        after_key: String::new(),
    });
    ensure!(
        c.scope == *scope
            && c.since == since
            && c.through <= head
            && c.through >= since
            && c.after_revision > since
            && c.after_revision <= c.through,
        "invalid catalog changes cursor"
    );
    let mut stmt = db.prepare(
        "SELECT revision,key,json,bytes FROM desktop_catalog_changes
        WHERE generation=?1 AND (revision,key)>(?2,?3) AND revision<=?4
        ORDER BY revision,key LIMIT ?5",
    )?;
    let mut query = stmt.query(params![
        generation,
        c.after_revision,
        c.after_key,
        c.through,
        (PAGE_ROWS + 1) as i64
    ])?;
    let (mut items, mut bytes, mut more) = (Vec::new(), 0usize, false);
    while let Some(row) = query.next()? {
        let charge: i64 = row.get(3)?;
        ensure!(
            charge > 0 && charge as usize <= PAGE_BYTES,
            "invalid stored catalog change"
        );
        if items.len() == PAGE_ROWS || bytes + charge as usize > PAGE_BYTES {
            more = true;
            break;
        }
        bytes += charge as usize;
        items.push(Change {
            revision: row.get(0)?,
            key: row.get(1)?,
            json: row.get(2)?,
        });
    }
    let through = c.through;
    let next = if more {
        let last = items.last().context("changes page cannot make progress")?;
        Some(Cursor {
            after_revision: last.revision,
            after_key: last.key.clone(),
            ..c
        })
    } else {
        None
    };
    Ok(Page {
        items,
        next,
        through,
    })
}

fn erase(tx: &Transaction<'_>, generation: Option<i64>, through_revision: i64) -> Result<usize> {
    let sql = if generation.is_some() {
        "SELECT id,generation,revision,bytes FROM desktop_catalog_changes WHERE generation=?1 AND revision<=?2 ORDER BY revision,key LIMIT ?3"
    } else {
        "SELECT id,generation,revision,bytes FROM desktop_catalog_changes WHERE id>?1 AND revision<=?2 ORDER BY id LIMIT ?3"
    };
    let mut stmt = tx.prepare(sql)?;
    let rows = stmt.query_map(
        params![generation.unwrap_or(0), through_revision, GC_ROWS as i64],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        },
    )?;
    let mut bytes = 0;
    let mut selected = Vec::new();
    for row in rows {
        let row = row?;
        if bytes + row.3 > GC_BYTES as i64 {
            break;
        }
        bytes += row.3;
        selected.push(row);
    }
    for (id, generation, revision, _) in &selected {
        tx.execute("UPDATE desktop_catalog_change_views SET floor=max(floor,?2),dirty=1 WHERE generation=?1", params![generation,revision])?;
        tx.execute("DELETE FROM desktop_catalog_changes WHERE id=?1", [id])?;
    }
    Ok(selected.len())
}
fn evict(tx: &Transaction<'_>) -> Result<usize> {
    erase(tx, None, i64::MAX)
}

/// At most one generation's bounded prefix. Generation metadata is bounded by
/// MAX_GENERATIONS; rows use the (generation,revision,key) index, never a scan.
pub(super) fn collect(db: &mut Connection) -> Result<Option<usize>> {
    let tx = db.transaction()?;
    let target: Option<(i64, i64)> = tx
        .query_row(
            "SELECT v.generation,CASE WHEN g.obsolete=1 THEN 9223372036854775807 ELSE v.floor END
         FROM desktop_catalog_change_views v JOIN desktop_catalog_generations g ON g.id=v.generation
         WHERE v.dirty=1 ORDER BY v.generation LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((generation, floor)) = target else {
        return Ok(None);
    };
    let deleted = erase(&tx, Some(generation), floor)?;
    tx.execute(
        "UPDATE desktop_catalog_change_views SET dirty=0 WHERE generation=?1",
        [generation],
    )?;
    let pending: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM desktop_catalog_changes WHERE generation=?1 AND revision<=?2)",
        params![generation, floor],
        |r| r.get(0),
    )?;
    if pending {
        tx.execute(
            "UPDATE desktop_catalog_change_views SET dirty=1 WHERE generation=?1",
            [generation],
        )?;
    }
    tx.commit()?;
    // Empty obsolete views must yield to the original row collector.
    Ok((deleted > 0 || pending).then_some(deleted))
}

#[cfg(test)]
mod tests;
