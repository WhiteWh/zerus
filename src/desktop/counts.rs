//! Shared host presentation counts, maintained during bounded catalog writes.
//! No row scan at startup/status and no local marks or effect authority.
use anyhow::Result;
use rusqlite::{functions::FunctionFlags, Connection};
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub fn register(db: &Connection) -> Result<()> {
    db.create_scalar_function(
        "desktop_shared_flags",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let raw: String = ctx.get(0)?;
            let value: Value = serde_json::from_str(&raw)
                .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))?;
            let row = zerus_client_core::view::row(&value);
            Ok(i64::from(row.state != "archived")
                | (i64::from(!row.muted && row.needs_attention) << 1))
        },
    )?;
    Ok(())
}

pub fn migrate(db: &Connection) -> Result<()> {
    // Old generations stay explicitly uncounted until their next replacement.
    // Migration touches only schema and the bounded generations metadata.
    db.execute_batch(
        "ALTER TABLE desktop_catalog_rows ADD COLUMN shared_flags INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE desktop_catalog_generations ADD COLUMN counts_ready INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE desktop_catalog_generations ADD COLUMN visible_sessions INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE desktop_catalog_generations ADD COLUMN attention_sessions INTEGER NOT NULL DEFAULT 0;
         CREATE TRIGGER desktop_counts_generation AFTER INSERT ON desktop_catalog_generations BEGIN
           UPDATE desktop_catalog_generations SET counts_ready=1 WHERE id=new.id;
         END;
         CREATE TRIGGER desktop_counts_insert AFTER INSERT ON desktop_catalog_rows BEGIN
           UPDATE desktop_catalog_rows SET shared_flags=desktop_shared_flags(new.json) WHERE id=new.id;
           UPDATE desktop_catalog_generations SET
             visible_sessions=visible_sessions+((SELECT shared_flags FROM desktop_catalog_rows WHERE id=new.id)&1),
             attention_sessions=attention_sessions+(((SELECT shared_flags FROM desktop_catalog_rows WHERE id=new.id)>>1)&1)
             WHERE id=new.generation AND counts_ready=1;
         END;
         CREATE TRIGGER desktop_counts_delete AFTER DELETE ON desktop_catalog_rows BEGIN
           UPDATE desktop_catalog_generations SET
             visible_sessions=visible_sessions-(old.shared_flags&1),
             attention_sessions=attention_sessions-((old.shared_flags>>1)&1)
             WHERE id=old.generation AND counts_ready=1;
         END;
         CREATE TRIGGER desktop_counts_update AFTER UPDATE OF json ON desktop_catalog_rows BEGIN
           UPDATE desktop_catalog_rows SET shared_flags=desktop_shared_flags(new.json) WHERE id=new.id;
           UPDATE desktop_catalog_generations SET
             visible_sessions=visible_sessions+((SELECT shared_flags FROM desktop_catalog_rows WHERE id=new.id)&1)-(old.shared_flags&1),
             attention_sessions=attention_sessions+(((SELECT shared_flags FROM desktop_catalog_rows WHERE id=new.id)>>1)&1)-((old.shared_flags>>1)&1)
             WHERE id=new.generation AND counts_ready=1;
         END;",
    )?;
    Ok(())
}

pub fn projection(
    db: &Connection,
    epoch: &str,
    coverage: impl Fn(&str) -> bool,
    expected: Option<&BTreeSet<String>>,
) -> Result<Value> {
    // Primary-key join over <=64 sources; never parses or counts session rows.
    let mut statement = db.prepare(
        "SELECT s.computer,s.epoch,s.incarnation,s.generation,s.complete,s.active,
                g.counts_ready,g.visible_sessions,g.attention_sessions
         FROM desktop_catalog_sources s LEFT JOIN desktop_catalog_generations g ON g.id=s.visible
         ORDER BY s.computer LIMIT 64",
    )?;
    let mut query = statement.query([])?;
    let mut computers = Vec::new();
    let mut seen = BTreeSet::new();
    let (mut total, mut attention, mut complete, mut totals_known) = (0i64, 0i64, true, true);
    while let Some(row) = query.next()? {
        let computer: String = row.get(0)?;
        if expected.is_some_and(|expected| !expected.contains(&computer)) {
            continue;
        }
        seen.insert(computer.clone());
        let current = row.get::<_, String>(1)? == epoch;
        let online = current && row.get::<_, bool>(5)? && coverage(&computer);
        let ready = row.get::<_, Option<bool>>(6)?.unwrap_or(false);
        let covered = online && row.get::<_, bool>(4)? && ready;
        let visible: Option<i64> = row.get(7)?;
        let needed: Option<i64> = row.get(8)?;
        total += if ready { visible.unwrap_or(0) } else { 0 };
        // Retained offline values are last known, never current attention.
        attention += if covered { needed.unwrap_or(0) } else { 0 };
        complete &= covered;
        totals_known &= ready;
        computers.push(
            json!({"computer":computer,"incarnation":row.get::<_,i64>(2)?,
            "generation":row.get::<_,i64>(3)?,"online":online,"complete":covered,
            "total":if ready {visible} else {None},
            "attention":if covered {needed} else {None}}),
        );
    }
    if let Some(expected) = expected {
        for computer in expected.difference(&seen) {
            complete = false;
            totals_known = false;
            computers.push(json!({"computer":computer,"online":false,"complete":false,
                "generation":null,"incarnation":null,"total":null,"attention":null}));
        }
    }
    complete &= !computers.is_empty();
    Ok(
        json!({"epoch":epoch,"scope":"shared_host_state","display_only":true,"authority":false,
        "local_marks_included":false,"computers":computers,
        "aggregate":{"total":if totals_known {Some(total)} else {None},"known_total":total,
            "known_attention":attention,"attention":if complete {Some(attention)} else {None},
            "complete":complete}}),
    )
}

#[cfg(test)]
mod tests;
