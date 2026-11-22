//! Private, lazy Teams storage. Reading a missing store never creates it.
#![allow(dead_code)] // Consumed by the following CLI and service packages.

#[cfg(test)]
mod tests;

pub(crate) mod actor_reads;
mod cross;
mod guards;
pub(crate) use guards::Guard;
mod decision_state;
pub(crate) mod dependency_edit;
mod disposition;
pub(crate) mod drafts;
pub(crate) mod editing;
pub(crate) mod evidence;
mod holder;
pub(crate) mod layout;
pub(crate) mod phase_controls;
mod phase_read;
pub(crate) mod phase_snapshot;
pub(crate) mod planning;
pub(crate) mod publication;
pub(crate) mod rework;
pub(crate) use holder::{hand_back_return, holder_of, return_without_holder, NOT_RECEIVED};
pub(crate) mod member_readability;
mod member_sessions;
mod native_floor;
mod native_person;
pub(in crate::teams) mod non_delivery;
pub(crate) mod observed_timing;
pub(crate) mod person_actions;
pub use decision_state::DecisionState;
pub(crate) mod bases;
pub(crate) mod pages;
mod progress;
mod recorded_timings;
mod records;
pub(crate) use progress::{TeamProgress, STUCK_SERVICE_KINDS};
mod roster;
pub(crate) mod rows;
mod stage;
pub(crate) mod timers;
pub(crate) mod unstaffed;
pub(crate) mod versions;
pub use records::*;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use zerus_team_plan::{model::*, problem::code, Problem};

pub(crate) const OPEN_EFFECT_STATES: &str = "('reserved','submitted','uncertain','cutting_over')";
// Cumulative native admission preserves the original layout-four nonce and
// receipts while installing every publication/rework prerequisite at seven.
pub(crate) const LEGACY_QUEUE_LAYOUT: u32 = versions::CORE_LEGACY;
pub(crate) const LEGACY_PLANNING_LAYOUT: u32 = versions::CORE_PLANNING;
pub(crate) const NATIVE_PERSON_LAYOUT: u32 = versions::CORE_NATIVE_PERSON;
pub(crate) const SCHEMA_VERSION: u32 = versions::CORE_CURRENT;

pub type Result<T> = std::result::Result<T, Problem>;

#[derive(Debug)]
pub struct Store {
    state: PathBuf,
    pub(crate) db: Option<Connection>,
}

pub(crate) fn storage(error: impl std::fmt::Display) -> Problem {
    Problem::new(
        code::STORAGE_FAILED,
        format!("Teams storage is unavailable: {error}"),
    )
}
pub(crate) fn encode(value: &impl Serialize) -> Result<String> {
    serde_json::to_string(value).map_err(storage)
}
pub(crate) fn decode<T: DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(storage)
}
pub(crate) fn sql_json<T: DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    col: usize,
) -> rusqlite::Result<T> {
    let value: String = row.get(col)?;
    serde_json::from_str(&value).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(col, rusqlite::types::Type::Text, Box::new(e))
    })
}

impl Store {
    /// The state directory this store was opened on, for the actor's
    /// document-side maintenance (zerus-63rru.32).
    pub(crate) fn state(&self) -> &Path {
        &self.state
    }
    pub fn open(state: &Path) -> Result<Self> {
        if !super::enabled(state) {
            return Err(Problem::new(code::TEAMS_INACTIVE, "Teams preview is off."));
        }
        // SQLite NOFOLLOW rejects ancestor aliases too (macOS /var and /tmp).
        // Resolve only the configured state directory, never the database or
        // Teams-owned subdirectories, and retain it for lazy creation/refresh.
        let state = crate::platform::canonical(state).map_err(storage)?;
        let path = state.join("teams/teams-v1.sqlite3");
        let db = match std::fs::symlink_metadata(&path) {
            Ok(_) => Some(connect(&path, false)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(storage(e)),
        };
        Ok(Self { state, db })
    }

    /// Retry discovery after another process creates the first team. Absence
    /// is never persisted as an inventory fact and this never creates a file.
    pub fn refresh(&mut self) -> Result<()> {
        self.active()?;
        if self.db.is_none() {
            let path = self.state.join("teams/teams-v1.sqlite3");
            match std::fs::symlink_metadata(&path) {
                Ok(_) => self.db = Some(connect(&path, false)?),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(storage(e)),
            }
        }
        Ok(())
    }
    pub(crate) fn ensure(&mut self) -> Result<()> {
        self.active()?;
        if self.db.is_none() {
            let dir = self.state.join("teams");
            crate::platform::private_dir(&dir).map_err(storage)?;
            self.db = Some(connect(&dir.join("teams-v1.sqlite3"), true)?);
        }
        Ok(())
    }
    pub(crate) fn active(&self) -> Result<()> {
        if super::enabled(&self.state) {
            if let Some(db) = &self.db {
                execution_fence(db)?;
            }
            Ok(())
        } else {
            Err(Problem::new(code::TEAMS_INACTIVE, "Teams preview is off."))
        }
    }
    /// The original create request, by receipt identity, for catalog-independent replay.
    pub fn creation_request(&self, id: &str) -> Result<Option<super::ops::Request>> {
        self.active()?;
        self.db.as_ref().map_or(Ok(None), |db| {
            // Measure before reading the row: an oversized saved request is
            // refused from its length, without materializing it in memory.
            let length: Option<i64> = db
                .query_row(
                    "SELECT length(CAST(content AS BLOB)) FROM requests WHERE id=?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(storage)?;
            if length.is_some_and(|length| length > 262_144) {
                return Err(Problem::new(
                    code::OVER_CAP,
                    "The saved request exceeds its bound.",
                ));
            }
            let content: Option<String> = db
                .query_row("SELECT content FROM requests WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(storage)?;
            content.map(|text| decode(&text)).transpose()
        })
    }
    /// Whether the node still lives in the team's plan; an archived node has
    /// left live work even while its task document keeps its observed stage.
    pub fn node_live(&self, team: &str, node: u32) -> Result<bool> {
        self.active()?;
        self.db.as_ref().map_or(Ok(false), |db| {
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM nodes WHERE team=?1 AND id=?2)",
                params![team, node],
                |r| r.get(0),
            )
            .map_err(storage)
        })
    }
    pub fn team(&self, id: &str) -> Result<Option<Team>> {
        self.active()?;
        self.db.as_ref().map_or(Ok(None), |db| rows::team(db, id))
    }
    pub fn suggestion(&self, team: &str, node: u32) -> Result<Option<RoutingSuggestion>> {
        self.active()?;
        self.db.as_ref().map_or(Ok(None), |db| {
            db.query_row(
                "SELECT role,pipeline FROM suggestions WHERE team=?1 AND node=?2",
                rusqlite::params![team, node],
                |r| {
                    Ok(RoutingSuggestion {
                        role: r.get(0)?,
                        pipeline: r.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(storage)
        })
    }
    pub fn card(&self, team: &str, seq: u64) -> Result<Option<Card>> {
        self.active()?;
        self.db
            .as_ref()
            .map_or(Ok(None), |db| rows::card(db, team, seq))
    }
    pub fn member(&self, team: &str, id: &str) -> Result<Option<Member>> {
        self.active()?;
        self.db
            .as_ref()
            .map_or(Ok(None), |db| rows::member(db, team, id))
    }
    pub fn node(&self, team: &str, id: NodeId) -> Result<Option<Node>> {
        self.active()?;
        self.db.as_ref().map_or(Ok(None), |db| {
            let tx = db.unchecked_transaction().map_err(storage)?;
            let node = rows::node(&tx, team, id)?;
            tx.commit().map_err(storage)?;
            Ok(node)
        })
    }
    pub fn task_observation(
        &self,
        team: &str,
        id: NodeId,
    ) -> Result<Option<super::ops::TaskObservation>> {
        self.active()?;
        self.db
            .as_ref()
            .map_or(Ok(None), |db| rows::observation(db, team, id))
    }
    pub fn integration_fact(&self, team: &str, node: NodeId) -> Result<Option<IntegrationFact>> {
        self.active()?;
        self.db.as_ref().map_or(Ok(None), |db| db.query_row(
            "SELECT decision,node,actor,commit_id,conflicts,created_ms FROM integration_facts WHERE team=?1 AND node=?2 ORDER BY decision DESC LIMIT 1",
            rusqlite::params![team,node], |r|Ok(IntegrationFact{decision:r.get(0)?,node:r.get(1)?,actor:sql_json(r,2)?,commit:r.get(3)?,conflicts:sql_json(r,4)?,created_ms:r.get(5)?}))
            .optional().map_err(storage))
    }
    pub fn generation(&self, team: &str, id: NodeId) -> Result<Option<u32>> {
        self.active()?;
        self.db.as_ref().map_or(Ok(None), |db| db.query_row(
            "SELECT generation FROM nodes WHERE team=?1 AND id=?2 UNION ALL SELECT generation FROM node_archive WHERE team=?1 AND id=?2 LIMIT 1",
            rusqlite::params![team,id], |r| r.get(0)).optional().map_err(storage))
    }
}

fn connect(path: &Path, create: bool) -> Result<Connection> {
    connect_mode(path, create, true)
}

/// The disposition reader uses the same native path/privacy binding but
/// never initializes, migrates or changes a persistent pragma.
fn connect_mode(path: &Path, create: bool, migrate: bool) -> Result<Connection> {
    let dir = path
        .parent()
        .ok_or_else(|| Guard::MissingDirectory.problem())?;
    if !crate::platform::verify_private(dir, crate::platform::Private::Directory)
        .map_err(storage)?
    {
        return Err(Guard::DirectoryPrivacy.problem());
    }
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let leaf = dir.join(format!("teams-v1.sqlite3{suffix}"));
        match crate::platform::verify_private(&leaf, crate::platform::Private::File) {
            Ok(true) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Ok(false) => return Err(Guard::FilePrivacy.problem()),
            Err(error) => return Err(storage(error)),
        }
    }
    #[cfg(unix)]
    let identity = {
        if create {
            crate::sqlite_file::create(path).map_err(storage)?;
        }
        crate::sqlite_file::private(path).map_err(storage)?
    };
    #[cfg(not(unix))]
    if create {
        let file = crate::platform::no_follow(crate::platform::private_open(
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true),
        ))
        .open(path)
        .map_err(storage)?;
        if !crate::platform::verify_private_file(&file).map_err(storage)? {
            return Err(Guard::FilePrivacy.problem());
        }
    }
    let mut db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(storage)?;
    #[cfg(unix)]
    crate::sqlite_file::bind_private(&db, path, identity).map_err(storage)?;
    bases::register_writer(&db)?;
    db.busy_timeout(Duration::from_secs(8)).map_err(storage)?;
    if migrate {
        initialize(&mut db)?;
        db.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )
        .map_err(storage)?;
    }
    Ok(db)
}

fn initialize(db: &mut Connection) -> Result<()> {
    let version: u32 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(storage)?;
    if version == versions::RETIRED_PREVIEW {
        return Err(Problem::new(
            code::INVALID_STATE,
            "Version-1 Teams store belongs to an unsupported pre-release build.",
        ));
    }
    if version > SCHEMA_VERSION {
        return Err(Problem::new(
            code::INVALID_STATE,
            "store schema is newer than this version of Zerus",
        ));
    }
    if version == versions::CORE_NATIVE_FLOOR {
        native_floor::validate(db)?;
        return schema_extensions(db);
    }
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(begin_error)?;
    let current: u32 = tx
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(storage)?;
    let sql = match current {
        versions::UNINITIALIZED => Some(include_str!("schema.sql")),
        versions::RETIRED_PREVIEW => {
            return Err(Problem::new(
                code::INVALID_STATE,
                "Version-1 Teams store belongs to an unsupported pre-release build.",
            ))
        }
        LEGACY_QUEUE_LAYOUT | LEGACY_PLANNING_LAYOUT => None,
        native if layout::native(native) => None,
        _ => return Err(Problem::new(code::INVALID_STATE, "store schema changed")),
    };
    if let Some(sql) = sql {
        tx.execute_batch(
            &sql.replace(
                "__CORE_LEGACY_VERSION__",
                &versions::CORE_LEGACY.to_string(),
            )
            .replace("__OPEN_EFFECT_STATES__", OPEN_EFFECT_STATES)
            .replace("__DECISION_PENDING__", DecisionState::Pending.as_str())
            .replace("__DECISION_SETTLED__", DecisionState::Settled.as_str()),
        )
        .map_err(storage)?;
    }
    if current == versions::CORE_NATIVE_FLOOR {
        // Another current opener won the writer. A current but damaged leaf
        // cannot be mistaken for an unmigrated legacy store.
        native_floor::validate(&tx)?;
    } else {
        if layout::native(current) {
            native_person::validate(&tx)?;
        }
        native_floor::legacy(&tx, current)?;
        schema_extensions(&tx)?;
        native_floor::migrate(&tx)?;
    }
    tx.commit().map_err(storage)
}

// Add empty companion storage and missing read indexes on first creation
// and when a version-2 store from before these seams is opened. Every write
// here reads the timers mark again under the lock it writes with, so that
// nothing is written over a store a newer program marked after the first,
// unlocked fence; an ordinary open finds every object and takes no lock.
fn schema_extensions(db: &Connection) -> Result<()> {
    // zerus-63rru.52 round 3: the stop identity columns, added
    // conditionally so an older build keeps opening the store (it never
    // reads them).
    for (table, column, kind) in [
        ("teams", "stop_seq", "INTEGER NOT NULL DEFAULT 0"),
        ("decisions", "stop", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        let present: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
                [table, column],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if !present {
            db.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {kind};"
            ))
            .map_err(storage)?;
        }
    }
    timers::fence(db)?;
    #[cfg(test)]
    timers::after_fence()?;
    notes_schema(db)?;
    common_schema(db)?;
    member_binding_schema(db)?;
    key_clear_schema(db)?;
    card_parameters(db)?;
    drafts::extend(db)?;
    planning::extend(db)?;
    publication::extend(db)?;
    rework::validate(db)?;
    phase_controls::extend(db)?;
    read_indexes(db)?;
    timers::extend(db)
}

/// Recheck under every writer lock before execution or native receipt replay.
/// The combined layout fences both pre-planning and pre-native engines.
pub(crate) fn execution_fence(db: &Connection) -> Result<()> {
    let version: u32 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(storage)?;
    if version > SCHEMA_VERSION {
        return Err(Problem::new(
            code::INVALID_STATE,
            "store schema is newer than this version of Zerus",
        ));
    }
    if version == versions::CORE_NATIVE_FLOOR {
        native_floor::validate(db)?;
    }
    Ok(())
}

/// Raise the private execution floor without lowering an already-published
/// store when another team starts planning. Called under the operation lock.
pub(crate) fn raise_execution_floor(db: &Connection, floor: u32) -> Result<()> {
    execution_fence(db)?;
    let current: u32 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(storage)?;
    if current < floor {
        db.pragma_update(None, "user_version", floor)
            .map_err(storage)?;
    }
    Ok(())
}

/// The notes columns beside the member's own words (zerus-63rru.69). They are
/// added in place, not by a version bump: a version-2 store from before them
/// keeps its rows and takes the additive columns on its next open. Each add
/// runs under the write lock after the mark is read again under it; an
/// ordinary open finds all three columns and takes no lock.
const NOTES_COLUMNS: [(&str, &str); 3] = [
    ("reports", "notes"),
    ("inbox", "notes"),
    ("cards", "notes"),
];

fn notes_schema(db: &Connection) -> Result<()> {
    let mut complete = true;
    for (table, column) in NOTES_COLUMNS {
        complete &= has_column(db, table, column)?;
    }
    if complete {
        return Ok(());
    }
    let own = db.is_autocommit();
    db.execute_batch(if own {
        "BEGIN IMMEDIATE;"
    } else {
        "SAVEPOINT notes_schema;"
    })
    .map_err(begin_error)?;
    let result = timers::fence(db).and_then(|()| {
        for (table, column) in NOTES_COLUMNS {
            if !has_column(db, table, column)? {
                db.execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN {column} TEXT NOT NULL DEFAULT '[]'"
                ))
                .map_err(storage)?;
            }
        }
        Ok(())
    });
    let end = match (&result, own) {
        (Ok(()), true) => "COMMIT;",
        (Ok(()), false) => "RELEASE notes_schema;",
        (Err(_), true) => "ROLLBACK;",
        (Err(_), false) => "ROLLBACK TO notes_schema; RELEASE notes_schema;",
    };
    db.execute_batch(end).map_err(storage)?;
    result
}

fn has_column(db: &Connection, table: &str, column: &str) -> Result<bool> {
    db.prepare(&format!(
        "SELECT 1 FROM pragma_table_info('{table}') WHERE name=?1"
    ))
    .map_err(storage)?
    .exists([column])
    .map_err(storage)
}

/// The additive cards column a localized window's parameters live in
/// (zerus-63rru.34). An older build's explicit column lists ignore it, so a
/// newer store opens there unchanged. Like the other extensions, the add runs
/// under the writer lock after the schema mark and the column's presence are
/// read again: a store a newer program marked after the first, unlocked fence
/// is refused with nothing written, and two opens that both saw the column
/// missing never fail on a duplicate column.
fn card_parameters(db: &Connection) -> Result<()> {
    if parameters_present(db)? {
        return Ok(());
    }
    #[cfg(test)]
    tests::after_missing_column();
    let own = db.is_autocommit();
    db.execute_batch(if own {
        "BEGIN IMMEDIATE;"
    } else {
        "SAVEPOINT card_parameters;"
    })
    .map_err(begin_error)?;
    let result = timers::fence(db).and_then(|()| {
        if parameters_present(db)? {
            return Ok(());
        }
        db.execute_batch("ALTER TABLE cards ADD COLUMN parameters TEXT;")
            .map_err(|error| {
                if is_busy(&error) {
                    Problem::new(
                        code::STORE_BUSY,
                        "The Teams store is busy; retry opening the team.",
                    )
                } else {
                    storage(error)
                }
            })
    });
    let end = match (&result, own) {
        (Ok(()), true) => "COMMIT;",
        (Ok(()), false) => "RELEASE card_parameters;",
        (Err(_), true) => "ROLLBACK;",
        (Err(_), false) => "ROLLBACK TO card_parameters; RELEASE card_parameters;",
    };
    db.execute_batch(end).map_err(storage)?;
    result
}

fn parameters_present(db: &Connection) -> Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('cards') WHERE name='parameters')",
        [],
        |r| r.get(0),
    )
    .map_err(storage)
}

/// The common batches: only `CREATE ... IF NOT EXISTS` statements.
const COMMON_SCHEMA: [&str; 6] = [
    include_str!("pipeline-schema.sql"),
    include_str!("observation-schema.sql"),
    include_str!("validation-settings.sql"),
    include_str!("read-indexes.sql"),
    include_str!("base-schema.sql"),
    include_str!("non-delivery-schema.sql"),
];
/// The closed-key sweep's finished marker (zerus-63rru.32 round 2). Applied
/// once per store, when the table is absent, so the ordinary open performs
/// no schema write.
const KEY_CLEAR_SCHEMA: &str = include_str!("key-clear-schema.sql");
/// Member-binding facts and the versioned triggers that invalidate them.
/// These additive batches move no version or mark. A trigger body can hold
/// statements of its own, so the presence check uses explicit object names.
const MEMBER_BINDING_SCHEMA: [&str; 2] = [
    include_str!("handles-schema.sql"),
    include_str!("member-readability.sql"),
];
const MEMBER_BINDING_OBJECTS: [(&str, &str); 5] = [
    ("table", "member_handles"),
    ("trigger", "member_handles_terminal_v1"),
    ("trigger", "member_handles_removed_v1"),
    ("table", "team_member_readability"),
    ("trigger", "team_member_readability_binding_v1"),
];

/// The objects a batch of `CREATE ... IF NOT EXISTS` statements creates.
fn created(sql: &str) -> Vec<(&'static str, &str)> {
    let words: Vec<&str> = sql.split_whitespace().collect();
    let mut found = Vec::new();
    for (at, word) in words.iter().enumerate() {
        if !word.eq_ignore_ascii_case("CREATE") {
            continue;
        }
        let mut rest = &words[at + 1..];
        if rest
            .first()
            .is_some_and(|w| w.eq_ignore_ascii_case("UNIQUE"))
        {
            rest = &rest[1..];
        }
        let kind = match rest.first().map(|w| w.to_ascii_uppercase()).as_deref() {
            Some("TABLE") => "table",
            Some("INDEX") => "index",
            Some("TRIGGER") => "trigger",
            _ => continue,
        };
        if let [_, guard_if, guard_not, guard_exists, name, ..] = rest {
            if guard_if.eq_ignore_ascii_case("IF")
                && guard_not.eq_ignore_ascii_case("NOT")
                && guard_exists.eq_ignore_ascii_case("EXISTS")
            {
                found.push((kind, name.trim_end_matches('(')));
            }
        }
    }
    found
}

/// The batches write only on a store that lacks one of their objects; then
/// they run under the write lock, after the mark is read again under it.
fn common_schema(db: &Connection) -> Result<()> {
    let mut complete = true;
    for (kind, name) in COMMON_SCHEMA.iter().flat_map(|sql| created(sql)) {
        complete &= db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type=?1 AND name=?2)",
                params![kind, name],
                |r| r.get::<_, bool>(0),
            )
            .map_err(storage)?;
    }
    if complete {
        return Ok(());
    }
    let own = db.is_autocommit();
    db.execute_batch(if own {
        "BEGIN IMMEDIATE;"
    } else {
        "SAVEPOINT common_schema;"
    })
    .map_err(begin_error)?;
    let result = timers::fence(db).and_then(|()| {
        COMMON_SCHEMA
            .iter()
            .try_for_each(|sql| db.execute_batch(sql).map_err(storage))
    });
    let end = match (&result, own) {
        (Ok(()), true) => "COMMIT;",
        (Ok(()), false) => "RELEASE common_schema;",
        (Err(_), true) => "ROLLBACK;",
        (Err(_), false) => "ROLLBACK TO common_schema; RELEASE common_schema;",
    };
    db.execute_batch(end).map_err(storage)?;
    result
}

fn key_clear_schema(db: &Connection) -> Result<()> {
    let present: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='key_clear_finished')",
            [],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if present {
        return Ok(());
    }
    let own = db.is_autocommit();
    db.execute_batch(if own {
        "BEGIN IMMEDIATE;"
    } else {
        "SAVEPOINT key_clear_schema;"
    })
    .map_err(begin_error)?;
    let result = db
        .execute_batch(KEY_CLEAR_SCHEMA)
        .map_err(storage);
    let end = match (&result, own) {
        (Ok(()), true) => "COMMIT;",
        (Ok(()), false) => "RELEASE key_clear_schema;",
        (Err(_), true) => "ROLLBACK;",
        (Err(_), false) => "ROLLBACK TO key_clear_schema; RELEASE key_clear_schema;",
    };
    db.execute_batch(end).map_err(begin_error)?;
    result
}

/// Installs missing member-binding objects together under the writer fence.
/// A store with all named objects skips the migration.
fn member_binding_schema(db: &Connection) -> Result<()> {
    let mut complete = true;
    for (kind, name) in MEMBER_BINDING_OBJECTS {
        complete &= db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type=?1 AND name=?2)",
                params![kind, name],
                |r| r.get::<_, bool>(0),
            )
            .map_err(storage)?;
    }
    if complete {
        return Ok(());
    }
    let own = db.is_autocommit();
    db.execute_batch(if own {
        "BEGIN IMMEDIATE;"
    } else {
        "SAVEPOINT member_binding_schema;"
    })
    .map_err(begin_error)?;
    let result = timers::fence(db).and_then(|()| {
        MEMBER_BINDING_SCHEMA
            .iter()
            .try_for_each(|sql| db.execute_batch(sql).map_err(storage))
    });
    let end = match (&result, own) {
        (Ok(()), true) => "COMMIT;",
        (Ok(()), false) => "RELEASE member_binding_schema;",
        (Err(_), true) => "ROLLBACK;",
        (Err(_), false) => "ROLLBACK TO member_binding_schema; RELEASE member_binding_schema;",
    };
    db.execute_batch(end).map_err(storage)?;
    result
}

fn read_indexes(db: &Connection) -> Result<()> {
    let unique: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_index_list('cards') WHERE name='integration_card' AND \"unique\"=1)", [], |r| r.get(0)).map_err(storage)?;
    if !unique {
        return Ok(());
    }
    // Only an older unique index needs replacement. Ordinary opens never
    // rebuild it; a savepoint keeps its replacement atomic, also at creation.
    if let Err(error) = db.execute_batch("SAVEPOINT replace_read_index") {
        return if is_busy(&error) {
            Ok(())
        } else {
            Err(storage(error))
        };
    }
    // The mark again, inside the savepoint and before its first write. The
    // drop takes the write lock on this read's snapshot or fails as busy, so
    // a mark committed after this read can never be written over.
    if let Err(problem) = timers::fence(db) {
        db.execute_batch("ROLLBACK TO replace_read_index; RELEASE replace_read_index")
            .map_err(storage)?;
        return Err(problem);
    }
    let replaced = db
        .execute_batch("DROP INDEX IF EXISTS integration_card")
        .and_then(|()| db.execute_batch(include_str!("read-indexes.sql")))
        .and_then(|()| db.execute_batch("RELEASE replace_read_index"));
    if let Err(error) = replaced {
        db.execute_batch("ROLLBACK TO replace_read_index; RELEASE replace_read_index")
            .map_err(storage)?;
        if is_busy(&error) {
            // This optional index upgrade can wait. No product operation
            // is replayed; the next open checks the actual index again.
            return Ok(());
        }
        return Err(storage(error));
    }
    Ok(())
}

pub(crate) fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(e, _) if matches!(e.code,
        rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
}

/// A busy store or admission: no write of this request began, so its refusal
/// is a non-start the person may repeat, never a lost storage outcome.
pub(crate) fn busy() -> Problem {
    Problem::new(
        code::STORE_BUSY,
        "The team store is busy. Nothing was written; try again.",
    )
}

/// A statement that could not take the store's lock has done no write and
/// saved no request receipt; any other failure is a storage failure.
pub(crate) fn begin_error(error: rusqlite::Error) -> Problem {
    if is_busy(&error) {
        busy()
    } else {
        storage(error)
    }
}
