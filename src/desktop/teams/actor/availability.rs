//! Point reads of the fleet rows already committed by Broker. No session cache
//! and no catalog walk: the existing (generation,key) index selects one row.
use super::ports::{Availability, Fact};
use super::*;
use crate::{
    config::Config,
    desktop::store::Location,
    state::{
        team_provider_stop::{self, ProviderStop},
        TeamRecord,
    },
};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use std::time::{Duration, Instant};
const SQL:&str="SELECT r.json FROM desktop_catalog_sources s JOIN desktop_catalog_owner o ON o.id=1 AND o.epoch=s.epoch JOIN desktop_catalog_rows r ON r.generation=s.visible AND r.key=?1 WHERE s.computer='' AND s.epoch=?2 AND s.active=1 AND s.complete=1 AND json_extract(s.hello,'$.machine_id')=?3 AND r.bytes<=1048576 LIMIT 1";
/// Whether `member` is live and idle in its current run. With `backfill`, a
/// member bound before handles gets its handle recorded once it is verified;
/// a read inside an admission passes false and never writes.
#[cfg(test)]
pub(super) fn free(
    config: &Config,
    epoch: &str,
    team: &str,
    member: &Member,
    backfill: bool,
) -> Result<bool> {
    Ok(availability(config, epoch, team, member, backfill)? == Availability::Free)
}
/// [`free`]'s answer from the same read, naming what cannot be read
/// (zerus-63rru.55): a binding that resolves to no run, or an unknown count
/// of an otherwise free member. A member with no row, or with one that is
/// busy, is busy.
#[cfg(test)]
pub(super) fn availability(
    config: &Config,
    epoch: &str,
    team: &str,
    member: &Member,
    backfill: bool,
) -> Result<Availability> {
    with_first(config, epoch, team, member, backfill, |_| false)
}
/// The same bound point read, with a native first-send witness supplied only
/// for an eligible fresh member. Admission supplies an already-read witness;
/// it never starts a terminal reader while holding the spawn fence.
pub(super) fn with_first(
    config: &Config,
    epoch: &str,
    team: &str,
    member: &Member,
    backfill: bool,
    first_ready: impl FnOnce(&Value) -> bool,
) -> Result<Availability> {
    Ok(answer(
        validate_host(read(config, epoch, team, member, backfill, true)?, member),
        first_ready,
    ))
}

fn answer(read: Read, first_ready: impl FnOnce(&Value) -> bool) -> Availability {
    match read {
        Read::Row(row) => {
            let facts = super::fleet::Facts::of(&row);
            if facts.free_with_first(facts.first_candidate() && first_ready(&row)) {
                Availability::Free
            } else {
                facts
                    .unknown()
                    .map_or(Availability::Busy, Availability::Unreadable)
            }
        }
        Read::Stopped(..) | Read::Missing => Availability::Busy,
        Read::Unresolved(why) => Availability::Unreadable(why.fact()),
    }
}
pub(super) fn record(
    config: &Config,
    epoch: &str,
    team: &str,
    member: &Member,
    backfill: bool,
) -> Result<Option<Value>> {
    Ok(
        match validate_host(read(config, epoch, team, member, backfill, false)?, member) {
            Read::Row(row) => Some(row),
            Read::Stopped(..) | Read::Missing | Read::Unresolved(_) => None,
        },
    )
}
/// What one read of a member found.
enum Read {
    /// The current fleet row of its current run.
    Row(Value),
    Stopped(Box<TeamRecord>, ProviderStop),
    /// Unbound, or no current row that names its run (and launch).
    Missing,
    /// Its binding resolves to no run, for the reason named.
    Unresolved(Unresolved),
}
fn read(
    config: &Config,
    epoch: &str,
    team: &str,
    member: &Member,
    backfill: bool,
    provider: bool,
) -> Result<Read> {
    let Some(session) = member.session.as_ref() else {
        return Ok(Read::Missing);
    };
    let binding = current_record(config, team, member, backfill, provider)?;
    let (run, record) = match binding {
        CurrentRecord::Run(ref run) => (run.as_str(), None),
        CurrentRecord::Bound(ref record) => (record.run_id.as_str(), Some(record.as_ref())),
        CurrentRecord::Unbound => return Ok(Read::Missing),
        CurrentRecord::Unresolved(why) => return Ok(Read::Unresolved(why)),
    };
    if let Some((record, stop)) =
        record.and_then(|record| Some((record, record.provider.as_ref()?.stop?)))
    {
        return Ok(Read::Stopped(Box::new(record.clone()), stop));
    }
    let db = connection(config)?;
    let row: Option<String> = db
        .query_row(
            SQL,
            params![format!("run:{run}"), epoch, session.machine],
            |r| r.get(0),
        )
        .optional()
        .map_err(crate::teams::store::storage)?;
    let Some(row) = row else {
        return Ok(Read::Missing);
    };
    let value: Value = serde_json::from_str(&row).map_err(crate::teams::store::storage)?;
    let named = super::fleet::Identity::of(&value);
    let identity = named.run == run
        && match session.session_id.strip_prefix("launch_id:") {
            Some(id) => named.launch == id,
            None => true,
        };
    let bound_run_record = if provider && identity && record.is_none() {
        match crate::state::observe_team_run_for_provider(
            &config.state,
            run,
            value["session_id"].as_str().unwrap_or_default(),
            &member.agent,
            named.name,
            Instant::now() + Duration::from_millis(300),
        ) {
            crate::state::TeamSession::Record(record)
                if record.cwd == member.worktree && record.account == member.account =>
            {
                Some(record)
            }
            _ => None,
        }
    } else {
        None
    };
    if let Some(record) = record.or(bound_run_record.as_ref()).filter(|record| {
        provider
            && identity
            && named.conversation == record.conversation_id
            && value["agent"] == record.agent
    }) {
        let progress = record
            .provider
            .as_ref()
            .and_then(|provider| provider.progress_ms);
        let stop = record
            .provider
            .as_ref()
            .and_then(|provider| provider.stop)
            .or_else(|| team_provider_stop::projected_stop(&value, progress));
        if let Some(stop) = stop {
            return Ok(Read::Stopped(Box::new(record.clone()), stop));
        }
    }
    Ok(if identity {
        Read::Row(value)
    } else {
        Read::Missing
    })
}

fn validate_host(read: Read, member: &Member) -> Read {
    if let Read::Row(row) = &read {
        if member.agent == "dsh" {
            let named = super::fleet::Identity::of(row);
            return match crate::state::dsh::team_live(
                named.name,
                named.run,
                named.conversation,
                Instant::now() + Duration::from_secs(2),
            ) {
                Ok(true) => read,
                Ok(false) => Read::Missing,
                Err(_) => Read::Unresolved(Unresolved::Unavailable),
            };
        }
    }
    read
}

pub(super) fn held(
    config: &Config,
    epoch: &str,
    team: &str,
    member: &Member,
    stop: impl FnOnce(&TeamRecord, ProviderStop) -> Result<()>,
    first_ready: impl FnOnce(&Value) -> bool,
) -> Result<Availability> {
    // The normal writer cannot publish main-turn recovery between the
    // observation and the team commit. A contended lock leaves work where it is.
    let writer = team_provider_stop::ProviderWriter::take(&config.state);
    let member_read = read(config, epoch, team, member, true, true)?;
    if let (Some(_writer), Read::Stopped(record, failure)) = (writer, &member_read) {
        stop(record, *failure)?;
    }
    Ok(answer(validate_host(member_read, member), first_ready))
}

/// Why a member bound to a launch has no current run (zerus-63rru.53): a
/// named state of the reader, never a silent none. Each leaves the member
/// not free; zerus-63rru.55's card tells the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Unresolved {
    /// Bound before handles, with no source left to take one from.
    NoHandle,
    /// No live or saved session carries the handle: archived or removed.
    Gone,
    /// The archive exists on disk and its metadata has not been indexed.
    Archive,
    /// The handle's session names another launch, agent or folder.
    Changed,
    /// The person started the session fresh: a new conversation is another
    /// executor of tasks, so the binding ends.
    StartedFresh,
    /// The store, the record or the deadline did not answer; a later read may.
    Unavailable,
}
impl Unresolved {
    /// The fact a card names for this result.
    pub(super) fn fact(&self) -> Fact {
        match self {
            Unresolved::NoHandle => Fact::NoHandle,
            Unresolved::Gone => Fact::Gone,
            Unresolved::Archive => Fact::Archive,
            Unresolved::Changed => Fact::Changed,
            Unresolved::StartedFresh => Fact::StartedFresh,
            Unresolved::Unavailable => Fact::Unavailable,
        }
    }
}
/// A member's current run, or why it has none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Current {
    Run(String),
    /// No binding this reader resolves: unbound, or not a launch or a run.
    Unbound,
    Unresolved(Unresolved),
}
/// The run `member` is in now. A member bound by run is that run (it has no
/// launch and so no handle). A member bound to a launch resolves through its
/// durable handle, its session's own ID: the session's current record, which
/// must still carry the bound launch, agent and folder. A member bound before
/// handles takes one from the first source that names its session (the
/// launch ledger, kept while the team lives; a completed standing resume; the
/// launch's evidence while it lasts), verified the same way; with `backfill`
/// it is recorded, once. The evidence serves only that first binding.
pub(super) fn current(
    config: &Config,
    team: &str,
    member: &Member,
    backfill: bool,
) -> Result<Current> {
    Ok(
        match current_record(config, team, member, backfill, false)? {
            CurrentRecord::Run(run) => Current::Run(run),
            CurrentRecord::Bound(record) => Current::Run(record.run_id),
            CurrentRecord::Unbound => Current::Unbound,
            CurrentRecord::Unresolved(why) => Current::Unresolved(why),
        },
    )
}

enum CurrentRecord {
    Run(String),
    Bound(Box<TeamRecord>),
    Unbound,
    Unresolved(Unresolved),
}

fn current_record(
    config: &Config,
    team: &str,
    member: &Member,
    backfill: bool,
    provider: bool,
) -> Result<CurrentRecord> {
    let Some(session) = member.session.as_ref() else {
        return Ok(CurrentRecord::Unbound);
    };
    if let Some(run) = session.session_id.strip_prefix("run_id:") {
        return Ok(checked(run.to_owned()));
    }
    let Some(launch) = session.session_id.strip_prefix("launch_id:") else {
        return Ok(CurrentRecord::Unbound);
    };
    let store = crate::teams::store::Store::open(&config.state)?;
    if let Some(handle) = store.member_handle(team, &member.id)? {
        if handle.launch != launch || handle.machine != session.machine {
            return Ok(CurrentRecord::Unresolved(Unresolved::Changed));
        }
        return Ok(resolve(
            config,
            member,
            launch,
            &handle.session_id,
            &handle.label_hint,
            backfill,
            provider,
        ));
    }
    drop(store);
    let Some((id, hint)) = source(config, team, member, launch)? else {
        return Ok(CurrentRecord::Unresolved(Unresolved::NoHandle));
    };
    let current = resolve(config, member, launch, &id, &hint, backfill, provider);
    if backfill && matches!(current, CurrentRecord::Bound(_)) {
        // A failed write costs only the next read another try.
        let _ = record_handle(config, team, member, &id, &hint);
    }
    Ok(current)
}
/// The member's current run, or none; [`current`] names why.
pub(super) fn current_run(config: &Config, team: &str, member: &Member) -> Result<Option<String>> {
    Ok(match current(config, team, member, false)? {
        Current::Run(run) => Some(run),
        _ => None,
    })
}
fn checked(run: String) -> CurrentRecord {
    if run.is_empty() || run.len() > 256 {
        CurrentRecord::Unresolved(Unresolved::Unavailable)
    } else {
        CurrentRecord::Run(run)
    }
}
/// The session whose durable ID is `id`, as `member`'s current run.
fn resolve(
    config: &Config,
    member: &Member,
    launch: &str,
    id: &str,
    hint: &str,
    repair: bool,
    provider: bool,
) -> CurrentRecord {
    use crate::state::TeamSession;
    let observe = if provider {
        crate::state::observe_team_session_for_availability
    } else {
        crate::state::observe_team_session
    };
    match observe(
        &config.state,
        id,
        launch,
        &member.agent,
        hint,
        Instant::now() + Duration::from_millis(300),
    ) {
        TeamSession::Record(r) if r.cwd == member.worktree && r.agent == member.agent => {
            if r.run_id.is_empty() {
                CurrentRecord::Unresolved(Unresolved::Unavailable)
            } else {
                CurrentRecord::Bound(Box::new(r))
            }
        }
        TeamSession::Record(_) | TeamSession::LaunchChanged => {
            CurrentRecord::Unresolved(Unresolved::Changed)
        }
        TeamSession::StartedFresh => CurrentRecord::Unresolved(Unresolved::StartedFresh),
        TeamSession::Gone if repair => {
            let archive =
                crate::state::archive_catalog::member_archive_pending(&config.state, id, launch);
            CurrentRecord::Unresolved(match archive {
                Ok(true) => Unresolved::Archive,
                Ok(false) => Unresolved::Gone,
                Err(_) => Unresolved::Unavailable,
            })
        }
        TeamSession::Gone => CurrentRecord::Unresolved(Unresolved::Gone),
        TeamSession::Unavailable => CurrentRecord::Unresolved(Unresolved::Unavailable),
    }
}
/// The session ID and name at binding of a member bound before handles,
/// from the first source that names them for this exact launch.
fn source(
    config: &Config,
    team: &str,
    member: &Member,
    launch: &str,
) -> Result<Option<(String, String)>> {
    let canonical = |id: &str| crate::state::session_id::canonical(id);
    if let Some(observed) = crate::desktop::teams::ledger::Ledger::open(&config.state)?
        .get(team, &member.id)?
        .and_then(|row| row.observed)
    {
        if observed.launch_id == launch && canonical(&observed.session_id) {
            return Ok(Some((observed.session_id, observed.name)));
        }
    }
    if let Some(binding) =
        crate::desktop::teams::confirm::resumed_binding(&config.state, team, &member.id, launch)
            .map_err(|error| problem(&error))?
    {
        if canonical(&binding.session_id) {
            return Ok(Some((binding.session_id, String::new())));
        }
    }
    if let crate::state::TeamObservation::Record(r) = crate::state::observe_team_launch(
        &config.state,
        launch,
        Instant::now() + Duration::from_millis(300),
    ) {
        if r.launch_id == launch && canonical(&r.session_id) {
            return Ok(Some((r.session_id, r.name)));
        }
    }
    Ok(None)
}
/// Records the verified handle of a member bound before handles, once.
fn record_handle(config: &Config, team: &str, member: &Member, id: &str, hint: &str) -> Result<()> {
    use crate::teams::ops::{Actor, Operation, Request};
    let mut store = crate::teams::store::Store::open(&config.state)?;
    let current = store
        .team(team)?
        .ok_or_else(|| problem("Team disappeared"))?;
    let actor = Actor::Service {
        person: current.owner.clone(),
        team: team.to_owned(),
    };
    let request = Request {
        request_id: uuid::Uuid::new_v4().to_string(),
        team: team.to_owned(),
        expected_revision: current.revision,
        operation: Operation::SetMemberHandle {
            member: member.id.clone(),
            session_id: id.to_owned(),
            label_hint: hint.to_owned(),
        },
    };
    match crate::teams::ops::apply(
        &mut store,
        &actor,
        &request,
        chrono::Utc::now().timestamp_millis(),
    ) {
        Ok(_) => Ok(()),
        Err(p) if p.code == zerus_team_plan::problem::code::DUPLICATE => Ok(()),
        Err(p) => Err(p),
    }
}
fn connection(config: &Config) -> Result<Connection> {
    // Location resolves the configured state directory; SQLite still refuses
    // symlinks below it, including the database leaf.
    let location = Location::new(config).map_err(|_| problem("Fleet location unavailable"))?;
    location
        .check_existing()
        .map_err(|_| problem("Fleet location is not private"))?;
    crate::desktop::store::database_files(&location)
        .map_err(|_| problem("Fleet database is not private"))?;
    let path = location.root.join("state.sqlite3");
    let identity = crate::sqlite_file::private(&path).map_err(crate::teams::store::storage)?;
    let db = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(crate::teams::store::storage)?;
    crate::sqlite_file::bind_private(&db, &path, identity).map_err(crate::teams::store::storage)?;
    db.busy_timeout(Duration::from_millis(100))
        .map_err(crate::teams::store::storage)?;
    Ok(db)
}
/// Free for work: live, ready and idle, with no job, child or question,
/// each known. The row is read only by [`super::fleet::Facts`].
pub(super) fn idle(row: &Value) -> bool {
    super::fleet::Facts::of(row).free()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::teams::actor::tests::rows::{seed_row, KINDS};
    use serde_json::json;
    fn row() -> Value {
        seed_row(
            "codex",
            &[
                ("run_id", json!("run")),
                ("launch_id", json!("launch")),
                ("conversation_id", json!("conversation")),
            ],
        )
    }
    #[test]
    fn idle_is_required_and_unknown_or_pending_or_background_is_not_free() {
        assert!(idle(&row()));
        for kind in KINDS {
            assert!(idle(&seed_row(kind, &[])), "the producer's idle {kind} row");
        }
        // A complete captured listJobs summary proves this idle host row
        // has no background work; older/incomplete summaries stay unknown.
        let dsh = seed_row("dsh", &[]);
        assert_eq!(dsh["process_work"]["count"], 0);
        assert!(idle(&dsh));
        assert!(!idle(&seed_row("dsh", &[("process_work.count", Value::Null)])));
        for (path, value) in [
            ("activity", json!("busy")),
            ("phase", json!("approval")),
            ("process_state", json!("exited")),
            ("process_state", json!("unknown")),
            ("runtime_state", json!("stopped")),
            ("process_work.count", json!(1)),
            ("optional_questions.count", json!(1)),
            ("subagent_active_count", json!(1)),
            ("process_work.count", Value::Null),
            ("subagent_active_count", Value::Null),
            ("optional_questions", Value::Null),
        ] {
            assert!(
                !idle(&seed_row("codex", &[(path, value.clone())])),
                "{path} {value}"
            );
        }
        // Only a DeepSeek row carries a state of its own.
        for state in ["paused", "stopped", "archived"] {
            assert!(
                !idle(&seed_row("dsh", &[("state", json!(state))])),
                "{state}"
            );
        }
    }
    #[test]
    fn fleet_query_is_indexed_by_generation_and_key_and_requires_the_current_complete_source() {
        let db = Connection::open_in_memory().unwrap();
        zerus_desktop_catalog::migrate(&db).unwrap();
        let mut s = db.prepare(&format!("EXPLAIN QUERY PLAN {SQL}")).unwrap();
        let details = s
            .query_map(params!["run:run", "epoch", "machine"], |r| {
                r.get::<_, String>(3)
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            details.iter().any(|r| r.contains("generation=? AND key=?")),
            "{details:?}"
        );
        assert!(db
            .query_row(SQL, params!["run:run", "epoch", "machine"], |r| r
                .get::<_, String>(0))
            .optional()
            .unwrap()
            .is_none());
    }
}
#[cfg(test)]
mod record_tests {
    use super::*;
    use serde_json::json;
    fn config(temp: &Path) -> Config {
        Config {
            home: String::new(),
            dir: temp.join("config"),
            state: temp.join("state"),
            selves: vec![],
            peers: vec![],
            base_peers: vec![],
            peers_from_environment: false,
            machines: Default::default(),
            tab: false,
            tab_colors: String::new(),
        }
    }
    #[test]
    fn a_current_complete_fleet_record_is_required_and_the_read_changes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let c = config(temp.path());
        crate::platform::private_dir(&c.dir).unwrap();
        crate::platform::private_dir(&c.state.join("desktop")).unwrap();
        let path = c.state.join("desktop/state.sqlite3");
        crate::sqlite_file::create(&path).unwrap();
        let db = Connection::open(&path).unwrap();
        zerus_desktop_catalog::migrate(&db).unwrap();
        db.execute(
            "UPDATE desktop_catalog_owner SET epoch='epoch' WHERE id=1",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO desktop_catalog_sources(computer,epoch,incarnation,generation,stream,seq,visible,complete,active,hello) VALUES('','epoch',1,1,'stream',1,1,1,1,?1)",[json!({"machine_id":"machine"}).to_string()]).unwrap();
        db.execute(
            "INSERT INTO desktop_catalog_generations(id,computer) VALUES(1,'')",
            [],
        )
        .unwrap();
        let row = crate::desktop::teams::actor::tests::rows::seed_row(
            "codex",
            &[
                ("name", json!("codex/example/member")),
                ("run_id", json!("run")),
                ("conversation_id", json!("conversation")),
            ],
        )
        .to_string();
        db.execute(
            "INSERT INTO desktop_catalog_rows(generation,key,json,bytes) VALUES(1,'run:run',?1,?2)",
            params![row, row.len()],
        )
        .unwrap();
        let m = Member {
            launch: None,
            id: "m1".into(),
            role: "developer".into(),
            agent: "codex".into(),
            account: String::new(),
            state: MemberState::Active,
            session: Some(SessionKey {
                machine: "machine".into(),
                session_id: "run_id:run".into(),
            }),
            worktree: String::new(),
            ownership_nonce: String::new(),
        };
        let before = std::fs::read(&path).unwrap();
        assert!(free(&c, "epoch", "team", &m, false).unwrap());
        let native = super::super::native::Native {
            config: c.clone(),
            machine: "machine".into(),
            epoch: "epoch".into(),
            stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let mut team = crate::teams::ops::tests::fixture().team;
        team.home = "machine".into();
        let observed = super::super::auxiliary::presence(&native, &team, &m).unwrap();
        assert!(observed.live && observed.idle());
        assert_eq!(
            (&*observed.run, &*observed.conversation),
            ("run", "conversation")
        );
        assert!(
            std::fs::read(&path).unwrap() == before,
            "availability changed the catalog"
        );
        for (path, value, flag) in [
            ("$.activity", json!("busy"), 0),
            ("$.process_work.count", json!(1), 1),
            ("$.optional_questions.count", json!(1), 2),
        ] {
            db.execute(
                "UPDATE desktop_catalog_rows SET json=json_set(?1,?2,json(?3))",
                params![row, path, value.to_string()],
            )
            .unwrap();
            let observed = super::super::auxiliary::presence(&native, &team, &m).unwrap();
            assert!(!observed.idle());
            assert!(match flag {
                0 => observed.working,
                1 => observed.background,
                _ => observed.pending,
            });
        }
        db.execute("UPDATE desktop_catalog_rows SET json=?1", [&row])
            .unwrap();
        assert!(!free(&c, "old-epoch", "team", &m, false).unwrap());
        db.execute(
            "UPDATE desktop_catalog_rows SET json=json_set(json,'$.run_id','other-run')",
            [],
        )
        .unwrap();
        assert!(
            !free(&c, "epoch", "team", &m, false).unwrap(),
            "a point key did not excuse a different payload identity"
        );
        db.execute(
            "UPDATE desktop_catalog_rows SET json=?1,bytes=?2",
            params![row, row.len()],
        )
        .unwrap();
        for field in ["active", "complete"] {
            db.execute(&format!("UPDATE desktop_catalog_sources SET {field}=0"), [])
                .unwrap();
            assert!(!free(&c, "epoch", "team", &m, false).unwrap());
            assert!(super::super::auxiliary::presence(&native, &team, &m).is_none());
            db.execute(&format!("UPDATE desktop_catalog_sources SET {field}=1"), [])
                .unwrap();
        }
        db.execute("DELETE FROM desktop_catalog_rows", []).unwrap();
        assert!(!free(&c, "epoch", "team", &m, false).unwrap());
    }
    #[test]
    fn an_ended_launch_without_a_resumed_binding_has_no_current_run() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let c = config(temp.path());
        crate::platform::private_dir(&c.state.join("launches")).unwrap();
        let launch = "00000000-0000-4000-8000-000000000001";
        for (leaf, value) in [
            (
                format!("{launch}.json"),
                json!({"launch_id":launch,"name":"kimi/example/member","started_at":1.0}),
            ),
            (
                format!("{launch}.end.json"),
                json!({"launch_id":launch,"name":"kimi/example/member","code":0,"ended_at":2.0}),
            ),
        ] {
            let path = c.state.join("launches").join(leaf);
            std::fs::write(&path, value.to_string()).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let m = Member {
            launch: None,
            id: "m1".into(),
            role: "developer".into(),
            agent: "kimi".into(),
            account: String::new(),
            state: MemberState::Active,
            session: Some(SessionKey {
                machine: "machine".into(),
                session_id: format!("launch_id:{launch}"),
            }),
            worktree: String::new(),
            ownership_nonce: String::new(),
        };
        // Teams is on, the member has no handle, and the launch's evidence
        // has ended: a named state, not a silent none (zerus-63rru.53).
        crate::platform::private_dir(&c.state.join("teams")).unwrap();
        std::fs::write(c.state.join("teams/preview"), "on").unwrap();
        assert_eq!(
            current(&c, "team", &m, true).unwrap(),
            Current::Unresolved(Unresolved::NoHandle)
        );
        assert_eq!(current_run(&c, "team", &m).unwrap(), None);
        assert!(!free(&c, "epoch", "team", &m, false).unwrap());
    }
}
