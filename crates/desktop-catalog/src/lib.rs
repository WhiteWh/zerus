//! Raw watch rows on disk, never a per-session resident map. Coverage is not
//! freshness or effect authority. The caller retains the source ingress permit
//! through each transaction and separately checks its monotonic receive age.
use anyhow::{bail, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::Value;
use zerus_watch_protocol::{self as watch, topic, Body, Frame};

pub mod changes;
/// Typed older-peer replacements, without manufacturing watch frames.
pub mod legacy;

pub const MAX_SOURCES: i64 = 64;
pub const MAX_ROWS: i64 = 1_000_000;
pub const MAX_BYTES: i64 = 128 << 20;
pub const MAX_GENERATIONS: i64 = 256;
pub const PAGE_ROWS: usize = 128;
// One legal near-frame-sized row must still be readable without being dropped.
pub const PAGE_BYTES: usize = watch::MAX_FRAME_BYTES;
pub const GC_ROWS: usize = 256;
pub const GC_BYTES: usize = 4 << 20;

/// Selected alias (empty is local), actor lifetime and exact source callback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub computer: String,
    pub service_epoch: String,
    pub incarnation: u64,
    pub generation: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    Unchanged,
    Staging,
    Visible,
    /// Includes a sequence hole or catalog source error. Request a resnapshot;
    /// retained visible rows remain readable, but coverage is incomplete.
    Incomplete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub computer: String,
    pub generation: i64,
    pub through: i64,
    pub after: i64,
}

#[derive(Debug)]
pub struct Row {
    pub id: i64,
    pub key: String,
    /// Original private JSON, including unknown fields and exact run identities.
    pub json: String,
}

#[derive(Debug)]
pub struct Page {
    pub rows: Vec<Row>,
    pub next: Option<Cursor>,
    /// Complete current catalog only; never permission to write effects.
    pub complete: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Status {
    pub visible_rows: i64,
    pub stored_rows: i64,
    pub stored_bytes: i64,
    pub complete: bool,
}

/// Configuration totals: two bounded metadata tables and one maintained row.
#[derive(Debug)]
pub struct Totals {
    pub retained_sources: i64,
    pub stored_rows: i64,
    pub stored_bytes: i64,
    pub obsolete_generations: i64,
}

pub fn totals(db: &Connection) -> Result<Totals> {
    Ok(db.query_row(
        "SELECT (SELECT count(*) FROM desktop_catalog_sources), rows, bytes,
         (SELECT count(*) FROM desktop_catalog_generations WHERE obsolete=1)
         FROM desktop_catalog_totals WHERE id=1",
        [],
        |r| {
            Ok(Totals {
                retained_sources: r.get(0)?,
                stored_rows: r.get(1)?,
                stored_bytes: r.get(2)?,
                obsolete_generations: r.get(3)?,
            })
        },
    )?)
}

pub fn migrate(db: &Connection) -> Result<()> {
    migrate_base(db)?;
    changes::migrate(db)
}

/// Original catalog schema, for the service's ordered historical migrations.
pub fn migrate_base(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE desktop_catalog_owner(id INTEGER PRIMARY KEY CHECK(id=1), epoch TEXT NOT NULL);
         INSERT INTO desktop_catalog_owner VALUES(1,'');
         CREATE TABLE desktop_catalog_sources(
           -- Nonempty stream: watch seq/cursor/hello. Empty stream: legacy
           -- seq=declared pages, cursor=declared rows, next_page=received pages,
           -- hello=provisional metadata, meta=last published metadata.
           computer TEXT PRIMARY KEY, epoch TEXT NOT NULL, incarnation INTEGER NOT NULL,
           generation INTEGER NOT NULL, stream TEXT NOT NULL, seq INTEGER NOT NULL,
           visible INTEGER, staged INTEGER, next_page INTEGER NOT NULL DEFAULT 0,
           cursor INTEGER NOT NULL DEFAULT 0, complete INTEGER NOT NULL DEFAULT 0,
           active INTEGER NOT NULL DEFAULT 1,
           hello TEXT NOT NULL, meta TEXT);
         CREATE TABLE desktop_catalog_generations(
           id INTEGER PRIMARY KEY AUTOINCREMENT, computer TEXT NOT NULL,
           obsolete INTEGER NOT NULL DEFAULT 0, rows INTEGER NOT NULL DEFAULT 0,
           bytes INTEGER NOT NULL DEFAULT 0);
         CREATE INDEX desktop_catalog_obsolete ON desktop_catalog_generations(obsolete,id);
         CREATE TABLE desktop_catalog_rows(
           id INTEGER PRIMARY KEY AUTOINCREMENT, generation INTEGER NOT NULL,
           key TEXT NOT NULL, json TEXT NOT NULL, bytes INTEGER NOT NULL,
           UNIQUE(generation,key));
         CREATE INDEX desktop_catalog_page ON desktop_catalog_rows(generation,id);
         CREATE TABLE desktop_catalog_totals(id INTEGER PRIMARY KEY CHECK(id=1), rows INTEGER NOT NULL, bytes INTEGER NOT NULL);
         INSERT INTO desktop_catalog_totals VALUES(1,0,0);
         CREATE TRIGGER desktop_catalog_insert AFTER INSERT ON desktop_catalog_rows BEGIN
           UPDATE desktop_catalog_generations SET rows=rows+1,bytes=bytes+new.bytes WHERE id=new.generation;
           UPDATE desktop_catalog_totals SET rows=rows+1,bytes=bytes+new.bytes WHERE id=1;
         END;
         CREATE TRIGGER desktop_catalog_delete AFTER DELETE ON desktop_catalog_rows BEGIN
           UPDATE desktop_catalog_generations SET rows=rows-1,bytes=bytes-old.bytes WHERE id=old.generation;
           UPDATE desktop_catalog_totals SET rows=rows-1,bytes=bytes-old.bytes WHERE id=1;
         END;
         CREATE TRIGGER desktop_catalog_update AFTER UPDATE OF json,bytes ON desktop_catalog_rows BEGIN
           UPDATE desktop_catalog_generations SET bytes=bytes+new.bytes-old.bytes WHERE id=new.generation;
           UPDATE desktop_catalog_totals SET bytes=bytes+new.bytes-old.bytes WHERE id=1;
         END;",
    )?;
    migrate_metadata(db)
}

/// Root schema 6: add source metadata to the maintained byte total. The caller
/// owns the migration transaction. At most 64 existing metadata rows are read
/// once here; page admission continues to read only the maintained total.
pub fn migrate_metadata(db: &Connection) -> Result<()> {
    let installed: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND name IN
         ('desktop_catalog_source_insert','desktop_catalog_source_update','desktop_catalog_source_delete')",
        [], |r|r.get(0),
    )?;
    ensure!(
        installed == 0 || installed == 3,
        "incomplete catalog metadata migration"
    );
    if installed == 3 {
        return Ok(());
    }
    let sources: i64 = db.query_row(
        "SELECT count(*) FROM (SELECT 1 FROM desktop_catalog_sources LIMIT ?1)",
        [MAX_SOURCES + 1],
        |r| r.get(0),
    )?;
    ensure!(
        sources <= MAX_SOURCES,
        "catalog source capacity exceeded during migration"
    );
    db.execute_batch(
        "UPDATE desktop_catalog_totals SET bytes=bytes+coalesce((SELECT sum(
           length(CAST(hello AS BLOB))+coalesce(length(CAST(meta AS BLOB)),0))
           FROM desktop_catalog_sources),0) WHERE id=1;
         CREATE TRIGGER desktop_catalog_source_insert AFTER INSERT ON desktop_catalog_sources BEGIN
           UPDATE desktop_catalog_totals SET bytes=bytes+length(CAST(new.hello AS BLOB))+
             coalesce(length(CAST(new.meta AS BLOB)),0) WHERE id=1;
         END;
         CREATE TRIGGER desktop_catalog_source_update AFTER UPDATE OF hello,meta ON desktop_catalog_sources BEGIN
           UPDATE desktop_catalog_totals SET bytes=bytes+length(CAST(new.hello AS BLOB))+
             coalesce(length(CAST(new.meta AS BLOB)),0)-length(CAST(old.hello AS BLOB))-
             coalesce(length(CAST(old.meta AS BLOB)),0) WHERE id=1;
         END;
         CREATE TRIGGER desktop_catalog_source_delete AFTER DELETE ON desktop_catalog_sources BEGIN
           UPDATE desktop_catalog_totals SET bytes=bytes-length(CAST(old.hello AS BLOB))-
             coalesce(length(CAST(old.meta AS BLOB)),0) WHERE id=1;
         END;",
    )?;
    Ok(())
}

/// At most MAX_SOURCES rows. Restart preserves last-known rows for display, but
/// discards partial snapshots and cannot inherit coverage from a previous actor.
pub fn begin_service(db: &mut Connection, epoch: &str) -> Result<()> {
    text(epoch, 128)?;
    let tx = db.transaction()?;
    tx.execute(
        "UPDATE desktop_catalog_owner SET epoch=?1 WHERE id=1",
        [epoch],
    )?;
    tx.execute_batch(
        "UPDATE desktop_catalog_generations SET obsolete=1 WHERE id IN
           (SELECT staged FROM desktop_catalog_sources WHERE staged IS NOT NULL);
         UPDATE desktop_catalog_sources SET staged=NULL,complete=0,next_page=0,active=0,
           hello=CASE WHEN stream='' THEN '' ELSE hello END;",
    )?;
    tx.commit()?;
    Ok(())
}

/// The actor must first verify hello.machine_id against its configured computer.
/// A hello token is stored for diagnostics; its host name never selects an alias.
pub fn connect(db: &mut Connection, source: &Source, frame: &Frame) -> Result<()> {
    validate_source(source)?;
    check_frame(frame)?;
    let Body::Hello(hello) = &frame.body else {
        bail!("source must start with hello")
    };
    text(&hello.stream, 1024)?;
    let hello_json = serde_json::to_string(hello)?;
    ensure!(hello_json.len() <= 16 << 10, "hello metadata too large");
    let tx = db.transaction()?;
    replace_source(&tx, source, &hello.stream, frame.seq as i64, &hello_json)?;
    check_budget(&tx)?;
    tx.commit()?;
    Ok(())
}

// Empty stream is reserved for the typed legacy adapter. Real watch hellos
// require a nonempty stream above; neither mode can accept the other's data.
fn replace_source(
    tx: &Transaction<'_>,
    source: &Source,
    stream: &str,
    seq: i64,
    hello: &str,
) -> Result<()> {
    owner(tx, source)?;
    if let Some(old) = state(tx, &source.computer)? {
        ensure!(
            old.epoch != source.service_epoch
                || (source.incarnation, source.generation)
                    > (old.incarnation as u64, old.generation as u64),
            "source callback is not a new incarnation/generation"
        );
        abandon(tx, &source.computer)?;
    } else {
        let count: i64 = tx.query_row("SELECT count(*) FROM desktop_catalog_sources", [], |r| {
            r.get(0)
        })?;
        ensure!(count < MAX_SOURCES, "catalog source capacity reached");
    }
    tx.execute(
        "INSERT INTO desktop_catalog_sources(computer,epoch,incarnation,generation,stream,seq,hello)
         VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(computer) DO UPDATE SET
         epoch=excluded.epoch,incarnation=excluded.incarnation,generation=excluded.generation,
         stream=excluded.stream,seq=excluded.seq,hello=excluded.hello,
         meta=CASE WHEN excluded.stream='' THEN desktop_catalog_sources.meta ELSE NULL END,
         complete=0,active=1",
        params![source.computer, source.service_epoch, source.incarnation as i64,
            source.generation as i64, stream, seq, hello],
    )?;
    Ok(())
}

/// All frames, including other topics and heartbeats, must arrive in source
/// order. A rejected current frame invalidates coverage without deleting the
/// visible generation. A stale callback cannot invalidate its replacement.
pub fn apply(db: &mut Connection, source: &Source, frame: &Frame) -> Result<Applied> {
    let result = apply_inner(db, source, frame);
    if result.is_err() {
        // If storage itself fails this can also fail: the actor must pause that
        // component on ANY error, rather than trust a previously complete row.
        let _ = disconnect(db, source);
    }
    result
}

pub fn disconnect(db: &mut Connection, source: &Source) -> Result<()> {
    let tx = db.transaction()?;
    current(&tx, source)?;
    abandon(&tx, &source.computer)?;
    tx.execute(
        "UPDATE desktop_catalog_sources SET active=0 WHERE computer=?1",
        [&source.computer],
    )?;
    tx.commit()?;
    Ok(())
}

/// Retire one producer's at-most-64 exact callbacks in one transaction. Already
/// retired or replaced callbacks are harmless; none can retire a replacement.
pub fn disconnect_many(db: &mut Connection, sources: &[Source]) -> Result<()> {
    ensure!(
        sources.len() <= MAX_SOURCES as usize,
        "too many source retirements"
    );
    let tx = db.transaction()?;
    for source in sources {
        validate_source(source)?;
        owner(&tx, source)?;
        let Some(old) = state(&tx, &source.computer)? else {
            continue;
        };
        if old.active
            && old.epoch == source.service_epoch
            && old.incarnation as u64 == source.incarnation
            && old.generation as u64 == source.generation
        {
            abandon(&tx, &source.computer)?;
            tx.execute(
                "UPDATE desktop_catalog_sources SET active=0 WHERE computer=?1",
                [&source.computer],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Remove a configured alias's exact projection, without scanning its rows.
/// Generation storage is reclaimed by the usual indexed, bounded collector.
pub fn remove_source(db: &mut Connection, source: &Source) -> Result<()> {
    validate_source(source)?;
    let tx = db.transaction()?;
    owner(&tx, source)?;
    if let Some(old) = state(&tx, &source.computer)? {
        ensure!(
            old.epoch == source.service_epoch
                && old.incarnation as u64 == source.incarnation
                && old.generation as u64 == source.generation,
            "stale catalog source removal"
        );
        tx.execute(
            "UPDATE desktop_catalog_generations SET obsolete=1 WHERE id IN (?1,?2)",
            params![old.visible, old.staged],
        )?;
        tx.execute(
            "UPDATE desktop_catalog_change_views SET dirty=1 WHERE generation=?1",
            [old.visible],
        )?;
        tx.execute(
            "DELETE FROM desktop_catalog_sources WHERE computer=?1",
            [&source.computer],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn apply_inner(db: &mut Connection, source: &Source, frame: &Frame) -> Result<Applied> {
    check_frame(frame)?;
    let tx = db.transaction()?;
    let mut old = current(&tx, source)?;
    ensure!(!old.stream.is_empty(), "watch frame for legacy source");
    ensure!(frame.seq as i64 > old.seq, "repeated or reordered frame");
    let hole = frame.seq as i64 != old.seq + 1;
    if hole {
        abandon(&tx, &source.computer)?;
        old.staged = None;
        old.complete = false;
    }
    let mut result = if hole {
        Applied::Incomplete
    } else {
        Applied::Unchanged
    };
    match &frame.body {
        Body::Hello(_) => bail!("duplicate hello"),
        Body::Gap(gap) if gap.topic == topic::CATALOG => {
            abandon(&tx, &source.computer)?;
            result = Applied::Incomplete;
        }
        Body::Error(error) if error.fatal || error.topic.as_deref() == Some(topic::CATALOG) => {
            abandon(&tx, &source.computer)?;
            tx.execute(
                "UPDATE desktop_catalog_sources SET active=0 WHERE computer=?1",
                [&source.computer],
            )?;
            result = Applied::Incomplete;
        }
        Body::Snapshot(snapshot) if snapshot.topic == topic::CATALOG => {
            ensure!(
                snapshot.key.is_none() && snapshot.value.is_none(),
                "invalid catalog snapshot"
            );
            ensure!(snapshot.cursor >= 0, "invalid catalog cursor");
            if snapshot.page == 0 {
                abandon(&tx, &source.computer)?;
                old.staged = Some(start_generation(&tx, source)?);
                old.next_page = 0;
                old.cursor = snapshot.cursor;
            }
            let stage = old.staged.context("snapshot page without start")?;
            ensure!(
                snapshot.page as i64 == old.next_page && snapshot.cursor == old.cursor,
                "non-contiguous snapshot"
            );
            for item in snapshot.items.as_ref().context("snapshot has no items")? {
                let item: watch::CatalogItem = serde_json::from_value(item.clone())?;
                let json = row_json(&item)?;
                tx.execute("INSERT INTO desktop_catalog_rows(generation,key,json,bytes) VALUES(?1,?2,?3,?4)",
                    params![stage, item.key, json, charge(&item.key, &json)])?;
            }
            tx.execute("UPDATE desktop_catalog_sources SET staged=?2,next_page=?3,cursor=?4,complete=0 WHERE computer=?1",
                params![source.computer, stage, old.next_page + 1, snapshot.cursor])?;
            if snapshot.last {
                publish(&tx, source, &old)?;
                result = Applied::Visible;
            } else {
                result = Applied::Staging;
            }
        }
        Body::Delta(delta) if delta.topic == topic::CATALOG => {
            // watch-v1 sends snapshots contiguously before deltas. Do not patch
            // an old retained generation while a new one is incomplete.
            ensure!(
                old.complete && old.staged.is_none() && !hole,
                "delta without complete snapshot"
            );
            ensure!(delta.cursor > old.cursor, "repeated catalog revision");
            let visible = old.visible.context("delta without visible generation")?;
            for item in &delta.upsert {
                let json = row_json(item)?;
                tx.execute("INSERT INTO desktop_catalog_rows(generation,key,json,bytes) VALUES(?1,?2,?3,?4)
                    ON CONFLICT(generation,key) DO UPDATE SET json=excluded.json,bytes=excluded.bytes",
                    params![visible, item.key, json, charge(&item.key, &json)])?;
            }
            for removal in &delta.remove {
                text(&removal.key, 2048)?;
                tx.execute(
                    "DELETE FROM desktop_catalog_rows WHERE generation=?1 AND key=?2",
                    params![visible, removal.key],
                )?;
            }
            changes::record(&tx, visible, delta)?;
            tx.execute(
                "UPDATE desktop_catalog_sources SET cursor=?2 WHERE computer=?1",
                params![source.computer, delta.cursor],
            )?;
            result = Applied::Visible;
        }
        Body::Snapshot(snapshot) if snapshot.topic == topic::CATALOG_META => {
            ensure!(
                snapshot.page == 0 && snapshot.last && snapshot.key.is_none(),
                "invalid catalog metadata"
            );
            let meta = serde_json::to_string(
                snapshot
                    .value
                    .as_ref()
                    .context("missing catalog metadata")?,
            )?;
            tx.execute(
                "UPDATE desktop_catalog_sources SET meta=?2 WHERE computer=?1",
                params![source.computer, meta],
            )?;
        }
        _ => (),
    }
    check_budget(&tx)?;
    tx.execute(
        "UPDATE desktop_catalog_sources SET seq=?2 WHERE computer=?1",
        params![source.computer, frame.seq as i64],
    )?;
    tx.commit()?;
    Ok(result)
}

fn start_generation(tx: &Transaction<'_>, source: &Source) -> Result<i64> {
    let generations: i64 = tx.query_row(
        "SELECT count(*) FROM desktop_catalog_generations",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        generations < MAX_GENERATIONS,
        "catalog generation capacity reached; collect obsolete generations"
    );
    tx.execute(
        "INSERT INTO desktop_catalog_generations(computer) VALUES(?1)",
        [&source.computer],
    )?;
    Ok(tx.last_insert_rowid())
}

fn publish(tx: &Transaction<'_>, source: &Source, old: &State) -> Result<()> {
    tx.execute(
        "UPDATE desktop_catalog_generations SET obsolete=1 WHERE id=?1",
        [old.visible],
    )?;
    tx.execute("UPDATE desktop_catalog_sources SET visible=staged,staged=NULL,complete=1 WHERE computer=?1", [&source.computer])?;
    changes::publish(tx, source, old.visible)?;
    Ok(())
}

fn check_budget(db: &Connection) -> Result<()> {
    let (rows, bytes): (i64, i64) = db.query_row(
        "SELECT rows,bytes FROM desktop_catalog_totals WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        rows <= MAX_ROWS && bytes <= MAX_BYTES,
        "catalog storage budget exhausted; collect obsolete generations"
    );
    Ok(())
}

/// Captures a generation and insertion watermark. Replacement invalidates the
/// cursor; updates/removals may change rows, but inserts cannot extend a walk.
pub fn page(db: &Connection, computer: &str, cursor: Option<&Cursor>) -> Result<Page> {
    let old = state(db, computer)?.context("unknown catalog source")?;
    let Some(generation) = old.visible else {
        ensure!(cursor.is_none(), "stale catalog cursor");
        return Ok(Page {
            rows: vec![],
            next: None,
            complete: false,
        });
    };
    let cursor = match cursor {
        Some(cursor) => {
            ensure!(
                cursor.computer == computer
                    && cursor.generation == generation
                    && cursor.after >= 0
                    && cursor.through >= cursor.after,
                "stale or mismatched catalog cursor"
            );
            cursor.clone()
        }
        None => Cursor {
            computer: computer.into(),
            generation,
            after: 0,
            through: db.query_row(
                "SELECT coalesce(max(id),0) FROM desktop_catalog_rows WHERE generation=?1",
                [generation],
                |r| r.get(0),
            )?,
        },
    };
    let mut statement = db.prepare("SELECT id,key,json,bytes FROM desktop_catalog_rows WHERE generation=?1 AND id>?2 AND id<=?3 ORDER BY id LIMIT ?4")?;
    let mut query = statement.query(params![
        generation,
        cursor.after,
        cursor.through,
        (PAGE_ROWS + 1) as i64
    ])?;
    let mut rows = Vec::new();
    let mut bytes = 0;
    let mut more = false;
    while let Some(row) = query.next()? {
        let charge: i64 = row.get(3)?;
        ensure!(
            charge > 0 && charge as usize <= PAGE_BYTES,
            "invalid stored catalog row"
        );
        if rows.len() == PAGE_ROWS || bytes + charge as usize > PAGE_BYTES {
            more = true;
            break;
        }
        bytes += charge as usize;
        rows.push(Row {
            id: row.get(0)?,
            key: row.get(1)?,
            json: row.get(2)?,
        });
    }
    let next = if more {
        Some(Cursor {
            after: rows.last().context("catalog page cannot make progress")?.id,
            ..cursor
        })
    } else {
        None
    };
    Ok(Page {
        rows,
        next,
        complete: old.complete,
    })
}

pub fn status(db: &Connection, computer: &str) -> Result<Status> {
    let old = state(db, computer)?.context("unknown catalog source")?;
    let visible_rows = db
        .query_row(
            "SELECT rows FROM desktop_catalog_generations WHERE id=?1",
            [old.visible],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let (stored_rows, stored_bytes) = db.query_row(
        "SELECT rows,bytes FROM desktop_catalog_totals WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(Status {
        visible_rows,
        stored_rows,
        stored_bytes,
        complete: old.complete,
    })
}

/// One indexed obsolete generation, at most 256 deletes and 4 MiB charged rows.
/// No cascade or full catalog scan; the actor yields between calls. Call outside
/// input transactions.
pub fn collect(db: &mut Connection) -> Result<Option<usize>> {
    if let Some(n) = changes::collect(db)? {
        return Ok(Some(n));
    }
    let tx = db.transaction()?;
    let generation: Option<i64> = tx
        .query_row(
            "SELECT id FROM desktop_catalog_generations WHERE obsolete=1 ORDER BY id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let Some(generation) = generation else {
        return Ok(None);
    };
    let through = {
        let mut statement = tx.prepare(
            "SELECT id,bytes FROM desktop_catalog_rows WHERE generation=?1 ORDER BY id LIMIT ?2",
        )?;
        let mut rows = statement.query(params![generation, GC_ROWS as i64])?;
        let (mut through, mut bytes) = (0i64, 0usize);
        while let Some(row) = rows.next()? {
            let charge: i64 = row.get(1)?;
            ensure!(
                charge > 0 && charge as usize <= PAGE_BYTES,
                "invalid stored catalog row"
            );
            if bytes + charge as usize > GC_BYTES {
                break;
            }
            bytes += charge as usize;
            through = row.get(0)?;
        }
        through
    };
    let deleted = tx.execute(
        "DELETE FROM desktop_catalog_rows WHERE generation=?1 AND id<=?2",
        params![generation, through],
    )?;
    tx.execute(
        "DELETE FROM desktop_catalog_change_views WHERE generation=?1 AND NOT EXISTS
         (SELECT 1 FROM desktop_catalog_changes WHERE generation=?1) AND EXISTS
         (SELECT 1 FROM desktop_catalog_generations WHERE id=?1 AND rows=0)",
        [generation],
    )?;
    tx.execute(
        "DELETE FROM desktop_catalog_generations WHERE id=?1 AND rows=0 AND NOT EXISTS
         (SELECT 1 FROM desktop_catalog_changes WHERE generation=?1)",
        [generation],
    )?;
    tx.commit()?;
    Ok(Some(deleted))
}

struct State {
    stream: String,
    epoch: String,
    incarnation: i64,
    generation: i64,
    seq: i64,
    visible: Option<i64>,
    staged: Option<i64>,
    next_page: i64,
    cursor: i64,
    complete: bool,
    active: bool,
}
fn state(db: &Connection, computer: &str) -> Result<Option<State>> {
    Ok(db.query_row("SELECT epoch,incarnation,generation,seq,visible,staged,next_page,cursor,complete,active,stream FROM desktop_catalog_sources WHERE computer=?1", [computer], |r| Ok(State {
        epoch:r.get(0)?,incarnation:r.get(1)?,generation:r.get(2)?,seq:r.get(3)?,visible:r.get(4)?,staged:r.get(5)?,next_page:r.get(6)?,cursor:r.get(7)?,complete:r.get(8)?,active:r.get(9)?,stream:r.get(10)?,
    })).optional()?)
}
fn owner(db: &Connection, source: &Source) -> Result<()> {
    let epoch: String = db.query_row(
        "SELECT epoch FROM desktop_catalog_owner WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    ensure!(epoch == source.service_epoch, "stale catalog service epoch");
    Ok(())
}
fn current(db: &Connection, source: &Source) -> Result<State> {
    owner(db, source)?;
    let old = state(db, &source.computer)?.context("unknown catalog source")?;
    ensure!(
        old.active
            && old.epoch == source.service_epoch
            && old.incarnation as u64 == source.incarnation
            && old.generation as u64 == source.generation,
        "stale catalog source callback"
    );
    Ok(old)
}
fn abandon(tx: &Transaction<'_>, computer: &str) -> Result<()> {
    tx.execute("UPDATE desktop_catalog_generations SET obsolete=1 WHERE id=(SELECT staged FROM desktop_catalog_sources WHERE computer=?1)", [computer])?;
    tx.execute(
        "UPDATE desktop_catalog_sources SET staged=NULL,next_page=0,complete=0,
         hello=CASE WHEN stream='' THEN '' ELSE hello END WHERE computer=?1",
        [computer],
    )?;
    Ok(())
}
fn text(value: &str, max: usize) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control),
        "invalid catalog identity"
    );
    Ok(())
}
fn validate_source(source: &Source) -> Result<()> {
    if !source.computer.is_empty() {
        text(&source.computer, 255)?;
        ensure!(
            !source.computer.starts_with(['-', '@'])
                && !source.computer.chars().any(char::is_whitespace),
            "invalid computer alias"
        );
    }
    text(&source.service_epoch, 128)?;
    ensure!(
        (1..=i64::MAX as u64).contains(&source.incarnation)
            && (1..=i64::MAX as u64).contains(&source.generation),
        "invalid source generation"
    );
    Ok(())
}
fn check_frame(frame: &Frame) -> Result<()> {
    ensure!(
        frame.v == watch::VERSION && (1..=i64::MAX as u64).contains(&frame.seq),
        "invalid watch version/sequence"
    );
    watch::encode(frame)?;
    Ok(())
}
fn charge(key: &str, json: &str) -> i64 {
    (key.len() + json.len() + 64) as i64
}
fn row_json(item: &watch::CatalogItem) -> Result<String> {
    text(&item.key, 2048)?;
    let name = item
        .row
        .get("name")
        .and_then(Value::as_str)
        .context("catalog row has no name")?;
    text(name, 1024)?;
    ensure!(
        item.key == watch::catalog_key(&item.row).context("invalid catalog key")?,
        "catalog key/row identity mismatch"
    );
    for field in ["run_id", "conversation_id"] {
        if let Some(value) = item.row.get(field).filter(|v| !v.is_null()) {
            let value = value.as_str().context("non-string catalog identity")?;
            if !value.is_empty() {
                text(value, 1024)?;
            }
        }
    }
    let json = serde_json::to_string(&item.row)?;
    ensure!(
        charge(&item.key, &json) as usize <= PAGE_BYTES,
        "catalog row too large"
    );
    Ok(json)
}

#[cfg(test)]
mod metadata_tests;
#[cfg(test)]
mod tests;
