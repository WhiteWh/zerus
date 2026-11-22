//! Existing-store discovery for the person's exact task-attempt disposition.
//! The preview switch is untouched. No migration or Teams data write is
//! permitted on this path; the caller retains the normal writer admission.
use super::*;

const MIGRATE: &str =
    "Teams store needs migration before this attempt can be abandoned; no team data was changed";

impl Store {
    /// Original-decision evidence is read as-is. An old or incomplete schema
    /// is unavailable; a receipt lookup never performs a migration.
    pub(crate) fn person_evidence(state: &Path) -> Result<Self> {
        if !super::super::enabled(state) {
            return Err(Problem::new(code::TEAMS_INACTIVE, "Teams preview is off."));
        }
        let store = Self::existing_unmigrated(state)?;
        if let Some(db) = &store.db {
            let version: u32 = db
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .map_err(storage)?;
            if !layout::native(version) {
                return Err(Problem::new(code::INVALID_STATE, MIGRATE));
            }
            as_is(db)?;
            native_person::validate(db)?;
        }
        Ok(store)
    }
    pub(super) fn existing_unmigrated(state: &Path) -> Result<Self> {
        let state = crate::platform::canonical(state).map_err(storage)?;
        let path = state.join("teams/teams-v1.sqlite3");
        let db = match std::fs::symlink_metadata(&path) {
            Ok(_) => Some(connect_mode(&path, false, false)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(storage(error)),
        };
        Ok(Self { state, db })
    }
}

/// Check only: every additive extension an ordinary open would migrate must
/// already be present. Run under the original team's writer admission.
pub(super) fn as_is(db: &Connection) -> Result<()> {
    let version: u32 = db
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(storage)?;
    if !layout::supported(version) {
        return Err(Problem::new(code::INVALID_STATE, MIGRATE));
    }
    timers::as_is(db)?;
    for (table, column) in [("teams", "stop_seq"), ("decisions", "stop")]
        .into_iter()
        .chain(NOTES_COLUMNS)
    {
        if !has_column(db, table, column)? {
            return Err(Problem::new(
                code::INVALID_STATE,
                format!("{MIGRATE}: missing {table}.{column}"),
            ));
        }
    }
    let objects = COMMON_SCHEMA
        .iter()
        .flat_map(|sql| created(sql))
        .chain(MEMBER_BINDING_OBJECTS)
        .chain([
            ("table", "key_clear_finished"),
            ("index", "cancel_node_newest"),
        ]);
    for (kind, name) in objects {
        let present: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type=?1 AND name=?2)",
                params![kind, name],
                |row| row.get(0),
            )
            .map_err(storage)?;
        if !present {
            return Err(Problem::new(
                code::INVALID_STATE,
                format!("{MIGRATE}: missing {kind} {name}"),
            ));
        }
    }
    let unique: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_index_list('cards') WHERE name='integration_card' AND \"unique\"=1)",
        [], |row| row.get(0),
    ).map_err(storage)?;
    if unique {
        return Err(Problem::new(code::INVALID_STATE, MIGRATE));
    }
    Ok(())
}
