//! Durable logical-edge tombstones and bounded per-conversation policy state.
//! Mapping is committed BEFORE ledger creation. A crash cannot lose the first
//! original run/token and recreate the same logical edge under a resumed run.
use super::projection::{self, Candidate, Projection, STATE_BYTES};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{Map, Value};
use zerus_client_core::{
    attention,
    desktop::notifications::{Action, Outcome, Receipt},
};
use zerus_desktop_ledger::{self as ledger, Identity, Kind};

pub(super) const MAX_EDGES: i64 = 262_144;
pub(super) const MAX_EDGE_BYTES: i64 = 128 << 20;
pub(super) const MAX_SESSIONS: i64 = 65_536;
pub(super) const MAX_STATE_BYTES: i64 = 64 << 20;

pub(super) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE desktop_attention_edges(
        id INTEGER PRIMARY KEY AUTOINCREMENT, logical TEXT NOT NULL UNIQUE,
        session TEXT NOT NULL, intent TEXT NOT NULL, identity TEXT NOT NULL, payload TEXT NOT NULL,
        blocked INTEGER NOT NULL, ready INTEGER NOT NULL DEFAULT 0, live INTEGER NOT NULL DEFAULT 1, dirty INTEGER NOT NULL DEFAULT 1,
        bytes INTEGER NOT NULL);
      CREATE INDEX desktop_attention_intent ON desktop_attention_edges(intent);
      CREATE INDEX desktop_attention_live ON desktop_attention_edges(live,id);
      CREATE INDEX desktop_attention_dirty ON desktop_attention_edges(dirty,id);
      CREATE INDEX desktop_attention_session ON desktop_attention_edges(session,live,id);
      CREATE TABLE desktop_attention_sessions(
        id TEXT PRIMARY KEY, computer TEXT NOT NULL, catalog_key TEXT NOT NULL, scope TEXT NOT NULL, row_hash TEXT NOT NULL, policy_revision INTEGER NOT NULL, state TEXT NOT NULL, bytes INTEGER NOT NULL);
      CREATE INDEX desktop_attention_row ON desktop_attention_sessions(computer,catalog_key);
      CREATE INDEX desktop_attention_changed_key ON desktop_catalog_changes(generation,key,revision);
      CREATE INDEX desktop_attention_catalog_identity ON desktop_catalog_rows(generation,
        CASE WHEN json_valid(json) THEN json_extract(json,'$.cmd') END,
        CASE WHEN json_valid(json) THEN json_extract(json,'$.conversation_id') END);
      CREATE TABLE desktop_attention_totals(id INTEGER PRIMARY KEY CHECK(id=1),edges INTEGER NOT NULL,edge_bytes INTEGER NOT NULL,sessions INTEGER NOT NULL,state_bytes INTEGER NOT NULL);
      INSERT INTO desktop_attention_totals VALUES(1,0,0,0,0);
      CREATE TRIGGER desktop_attention_edge_add AFTER INSERT ON desktop_attention_edges BEGIN UPDATE desktop_attention_totals SET edges=edges+1,edge_bytes=edge_bytes+new.bytes WHERE id=1; END;
      CREATE TRIGGER desktop_attention_session_add AFTER INSERT ON desktop_attention_sessions BEGIN UPDATE desktop_attention_totals SET sessions=sessions+1,state_bytes=state_bytes+new.bytes WHERE id=1; END;
      CREATE TRIGGER desktop_attention_session_update AFTER UPDATE ON desktop_attention_sessions BEGIN UPDATE desktop_attention_totals SET state_bytes=state_bytes+new.bytes-old.bytes WHERE id=1; END;")?;
    Ok(())
}
/// A baseline reads live rows in bounded pages, not a historical snapshot.
/// A key changed after its starting watermark is eligible for delta catchup
/// if that conversation already has saved Tracker state. The caller baselines
/// a never-seen conversation before allowing its later observations to post.
/// Retention loss invalidates the interval instead of treating an absent journal row as
/// proof that the catalog value existed at the baseline's start.
pub(super) enum BaselineKey {
    Unchanged,
    Changed,
    Gap,
}
pub(super) fn baseline_key(
    db: &Connection,
    scope: &zerus_desktop_catalog::changes::Scope,
    since: i64,
    key: &str,
) -> Result<BaselineKey> {
    let (generation,floor):(i64,i64)=db.query_row("SELECT v.generation,v.floor FROM desktop_catalog_sources s JOIN desktop_catalog_change_views v ON v.generation=s.visible WHERE s.computer=?1 AND v.projection=?2",params![scope.source.computer,scope.projection],|r|Ok((r.get(0)?,r.get(1)?)))?;
    if since < floor {
        return Ok(BaselineKey::Gap);
    }
    let changed:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM desktop_catalog_changes WHERE generation=?1 AND key=?2 AND revision>?3 LIMIT 1)",params![generation,key,since],|r|r.get(0))?;
    Ok(if changed {
        BaselineKey::Changed
    } else {
        BaselineKey::Unchanged
    })
}

fn text(row: &rusqlite::Row<'_>, index: usize, cap: usize) -> rusqlite::Result<String> {
    let value = row.get_ref(index)?;
    let value = value.as_str().map_err(|_| rusqlite::Error::InvalidQuery)?;
    if value.len() > cap {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(value.to_owned())
}
/// An empty saved Tracker is still a known first observation. Absence and an
/// empty map must not collapse into the same baseline-catchup decision.
pub(super) fn has_previous(db: &Connection, session: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM desktop_attention_sessions WHERE id=?1)",
        [session],
        |r| r.get(0),
    )?)
}
pub(super) fn previous(db: &Connection, session: &str) -> Result<Map<String, Value>> {
    let raw = db
        .query_row(
            "SELECT state FROM desktop_attention_sessions WHERE id=?1",
            [session],
            |r| text(r, 0, STATE_BYTES),
        )
        .optional()?;
    Ok(raw
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or_default())
}
pub(super) fn label(db: &Connection, computer: &str) -> Result<String> {
    if !computer.is_empty() {
        return Ok(computer.into());
    }
    let host:Option<String>=db.query_row("SELECT CASE WHEN json_valid(hello) THEN substr(json_extract(hello,'$.host'),1,128) END FROM desktop_catalog_sources WHERE computer=?1",[computer],|r|r.get(0))?;
    Ok(host
        .filter(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
        .unwrap_or_else(|| "This computer".into()))
}

pub(super) fn session(computer: &str, raw: &Value) -> Result<String> {
    let row = zerus_client_core::view::row(raw);
    projection::digest(&(
        computer,
        row.notify.cmd,
        row.conversation_id
            .context("missing attention conversation")?,
    ))
}

pub(super) struct Position<'a> {
    pub scope: &'a str,
    pub row_hash: &'a str,
    pub policy_revision: i64,
}
pub(super) fn save(
    db: &mut Connection,
    computer: &str,
    key: &str,
    position: Position<'_>,
    mut projected: Projection,
    baseline: bool,
) -> Result<()> {
    let Position {
        scope,
        row_hash,
        policy_revision,
    } = position;
    // Remap every temporary/new token back to the first original payload. State
    // comparisons and later cleared callbacks must retain that exact token.
    db.execute(
        "UPDATE desktop_attention_edges SET ready=0 WHERE session=?1 AND live=1",
        [&projected.conversation],
    )?;
    for candidate in &projected.candidates {
        let (intent, payload) = remember(db, &projected.conversation, candidate, baseline)?;
        let original: Action = serde_json::from_str(&payload)?;
        let replacement = original.token();
        for entry in projected.state.values_mut() {
            if entry.get("token").and_then(Value::as_str) == Some(candidate.action.token()) {
                entry["token"] = Value::String(replacement.into());
            }
        }
        install(db, candidate, &intent, &payload)?;
    }
    for token in &projected.cleared {
        if !projected
            .state
            .values()
            .any(|e| e.get("token").and_then(Value::as_str) == Some(token))
        {
            retire_token(db, token)?;
        }
    }
    let state = serde_json::to_string(&projected.state)?;
    ensure!(state.len() <= STATE_BYTES, "attention state too large");
    let charge = state.len()
        + computer.len()
        + key.len()
        + projected.conversation.len()
        + scope.len()
        + row_hash.len();
    let tx = db.transaction()?;
    let old: Option<i64> = tx
        .query_row(
            "SELECT bytes FROM desktop_attention_sessions WHERE id=?1",
            [&projected.conversation],
            |r| r.get(0),
        )
        .optional()?;
    let (sessions, bytes): (i64, i64) = tx.query_row(
        "SELECT sessions,state_bytes FROM desktop_attention_totals WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        sessions + i64::from(old.is_none()) <= MAX_SESSIONS
            && bytes - old.unwrap_or(0) + charge as i64 <= MAX_STATE_BYTES,
        "attention state quota"
    );
    tx.execute("INSERT INTO desktop_attention_sessions VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(id) DO UPDATE SET computer=excluded.computer,catalog_key=excluded.catalog_key,scope=excluded.scope,row_hash=excluded.row_hash,policy_revision=excluded.policy_revision,state=excluded.state,bytes=excluded.bytes",params![projected.conversation,computer,key,scope,row_hash,policy_revision,state,charge as i64])?;
    tx.commit()?;
    Ok(())
}
fn install(db: &mut Connection, candidate: &Candidate, intent: &str, payload: &str) -> Result<()> {
    let identity: Identity = db
        .query_row(
            "SELECT identity FROM desktop_attention_edges WHERE logical=?1",
            [&candidate.logical],
            |r| text(r, 0, 4096),
        )
        .map(|s| serde_json::from_str(&s))??;
    let blocked: bool = db.query_row(
        "SELECT blocked FROM desktop_attention_edges WHERE logical=?1",
        [&candidate.logical],
        |r| r.get(0),
    )?;
    let result = if blocked {
        ledger::put_baseline(db, &identity, 1, &serde_json::from_str(payload)?)
    } else {
        ledger::put_intent(db, &identity, 1, &serde_json::from_str(payload)?)
    };
    if let Err(error) = result.as_ref() {
        let withdrawn: bool = db
            .query_row(
                "SELECT withdrawn FROM desktop_intents WHERE id=?1",
                [intent],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);
        if withdrawn {
            return Ok(());
        }
        return Err(anyhow::anyhow!("{error:#}"));
    }
    ensure!(result? == intent, "attention intent identity changed");
    Ok(())
}
/// Historical duplicates consume only previously unseen edges; they never
/// replace a current conversation's state, token, policy or callback mapping.
pub(super) fn consume_history(db: &mut Connection, p: Projection) -> Result<()> {
    for c in &p.candidates {
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM desktop_attention_edges WHERE logical=?1)",
            [&c.logical],
            |r| r.get(0),
        )?;
        if !exists {
            let (intent, payload) = remember(db, &p.conversation, c, true)?;
            install(db, c, &intent, &payload)?;
        }
    }
    Ok(())
}
/// At most two indexed live candidates. Ambiguity is not currentness. Raw
/// catalog data never supplies a writer grant; the owned source is checked too.
pub(super) fn current_key(
    db: &Connection,
    computer: &str,
    cmd: &str,
    conversation: &str,
) -> Result<Option<String>> {
    let mut q=db.prepare("SELECT r.key FROM desktop_catalog_rows r JOIN desktop_catalog_sources s ON s.visible=r.generation WHERE s.computer=?1
      AND CASE WHEN json_valid(r.json) THEN json_extract(r.json,'$.cmd') END=?2
      AND CASE WHEN json_valid(r.json) THEN json_extract(r.json,'$.conversation_id') END=?3
      AND COALESCE(json_extract(r.json,'$.state'),'running')='running'
      AND COALESCE(json_extract(r.json,'$.process_state'),'')<>'exited' ORDER BY r.id LIMIT 2")?;
    let rows = q
        .query_map(params![computer, cmd, conversation], |r| text(r, 0, 2048))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows.len() == 1).then(|| rows[0].clone()))
}

fn remember(
    db: &mut Connection,
    session: &str,
    c: &Candidate,
    baseline: bool,
) -> Result<(String, String)> {
    let tx = db.transaction()?;
    let existing: Option<(String, String)> = tx
        .query_row(
            "SELECT intent,payload FROM desktop_attention_edges WHERE logical=?1",
            [&c.logical],
            |r| Ok((text(r, 0, 64)?, text(r, 1, ledger::PAYLOAD_BYTES)?)),
        )
        .optional()?;
    if let Some(old) = existing {
        // Baseline alone never withdraws an existing delivered mapping. Actual
        // mute/read/focus suppression does, and can never clear a tombstone.
        let unattempted: bool = tx.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM desktop_attempts WHERE intent_id=?1)",
            [&old.0],
            |r| r.get(0),
        )?;
        if c.consumed || (baseline && unattempted) {
            tx.execute("UPDATE desktop_attention_edges SET blocked=1,live=0,dirty=1 WHERE logical=?1 AND blocked=0",[&c.logical])?;
        }
        tx.execute(
            "UPDATE desktop_attention_edges SET ready=?2 WHERE logical=?1",
            params![c.logical, c.ready],
        )?;
        tx.commit()?;
        return Ok(old);
    }
    let identity = serde_json::to_string(&c.key.identity)?;
    let payload = serde_json::to_string(&c.action)?;
    ensure!(
        identity.len() <= 4096
            && payload.len() <= ledger::PAYLOAD_BYTES
            && c.action
                .valid(zerus_client_core::desktop::notifications::Platform::Linux),
        "invalid attention action"
    );
    let charge = identity.len() + payload.len() + session.len() + c.logical.len() + c.key.id.len();
    let (count, bytes): (i64, i64) = tx.query_row(
        "SELECT edges,edge_bytes FROM desktop_attention_totals WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        count < MAX_EDGES && bytes + charge as i64 <= MAX_EDGE_BYTES,
        "attention edge quota"
    );
    tx.execute("INSERT INTO desktop_attention_edges(logical,session,intent,identity,payload,blocked,bytes) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![c.logical,session,c.key.id,identity,payload,baseline||c.consumed,charge as i64])?;
    tx.execute(
        "UPDATE desktop_attention_edges SET ready=?2 WHERE logical=?1",
        params![c.logical, c.ready],
    )?;
    if baseline || c.consumed {
        tx.execute(
            "UPDATE desktop_attention_edges SET live=0 WHERE logical=?1",
            [&c.logical],
        )?;
    }
    tx.commit()?;
    Ok((c.key.id.clone(), payload))
}
fn retire_token(db: &mut Connection, token: &str) -> Result<()> {
    let Some(target) = attention::target(token) else {
        return Ok(());
    };
    let Some(intent) = target.get("notice").and_then(Value::as_str) else {
        return Ok(());
    };
    ledger::withdraw(db, intent, 1)?;
    db.execute(
        "UPDATE desktop_attention_edges SET live=0,dirty=1 WHERE intent=?1 AND live=1",
        [intent],
    )?;
    Ok(())
}

/// Revoke callback permission before attempting a native withdrawal. Unknown
/// posts stay tombstoned without inventing an OS handle or resending the post.
pub(super) enum Withdrawal {
    Waiting,
    Absent,
    Ready(String),
}
pub(super) fn withdrawal(db: &mut Connection, intent: &str) -> Result<Withdrawal> {
    let pending: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM desktop_attempts WHERE intent_id=?1 AND state='reserved')",
        [intent],
        |r| r.get(0),
    )?;
    ledger::withdraw(db, intent, 1)?;
    if pending {
        return Ok(Withdrawal::Waiting);
    }
    let raw: Option<String>=db.query_row("SELECT receipt FROM desktop_attempts WHERE intent_id=?1 AND state='delivered' ORDER BY seq DESC LIMIT 1",[intent],|r|text(r,0,ledger::RECEIPT_BYTES)).optional()?;
    let (identity, payload): (String, String) = db.query_row(
        "SELECT identity,payload FROM desktop_attention_edges WHERE intent=?1 LIMIT 1",
        [intent],
        |r| Ok((text(r, 0, 4096)?, text(r, 1, ledger::PAYLOAD_BYTES)?)),
    )?;
    let Some(raw) = raw else {
        return Ok(Withdrawal::Absent);
    };
    let receipt: Receipt = serde_json::from_str(&raw)?;
    let Outcome::Delivered { native_id } = receipt.outcome else {
        return Ok(Withdrawal::Absent);
    };
    let original: Action = serde_json::from_str(&payload)?;
    let mut identity: Identity = serde_json::from_str(&identity)?;
    identity.kind = Kind::Withdrawal;
    // Exactly the original target and edge, with a distinct effect kind.
    let id = ledger::put_intent(
        db,
        &identity,
        1,
        &serde_json::to_value(Action::Withdraw {
            token: original.token().into(),
            native_id,
        })?,
    )?;
    Ok(Withdrawal::Ready(id))
}

pub(super) struct Dirty {
    pub id: i64,
    pub intent: String,
    pub computer: String,
    pub live: bool,
    pub blocked: bool,
    pub scope: String,
    pub row_hash: String,
    pub key: String,
    pub policy_revision: i64,
    pub ready: bool,
}
pub(super) fn dirty(db: &Connection, after: i64, through: i64, limit: usize) -> Result<Vec<Dirty>> {
    let mut q=db.prepare("SELECT e.id,e.intent,COALESCE(s.computer,''),e.live,e.blocked,COALESCE(s.scope,''),COALESCE(s.row_hash,''),COALESCE(s.catalog_key,''),COALESCE(s.policy_revision,0),e.ready FROM (SELECT * FROM desktop_attention_edges WHERE dirty=1 AND id>?1 AND id<=?2 ORDER BY id LIMIT ?3) e LEFT JOIN desktop_attention_sessions s ON s.id=e.session ORDER BY e.id")?;
    let rows = q
        .query_map(params![after, through, limit as i64], |r| {
            Ok(Dirty {
                id: r.get(0)?,
                intent: text(r, 1, 64)?,
                computer: text(r, 2, 255)?,
                live: r.get(3)?,
                blocked: r.get(4)?,
                scope: text(r, 5, 64)?,
                row_hash: text(r, 6, 64)?,
                key: text(r, 7, 2048)?,
                policy_revision: r.get(8)?,
                ready: r.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
pub(super) fn clean(db: &Connection, id: i64) -> Result<()> {
    db.execute(
        "UPDATE desktop_attention_edges SET dirty=0 WHERE id=?1",
        [id],
    )?;
    Ok(())
}
pub(super) fn remove(db: &mut Connection, computer: &str, key: &str) -> Result<()> {
    // Bound corruption/identity accumulation too: a key cannot expand this
    // actor turn into an arbitrary history update.
    let mut q=db.prepare("SELECT e.id,e.intent,e.identity,e.payload FROM desktop_attention_edges e JOIN desktop_attention_sessions s ON s.id=e.session WHERE s.computer=?1 AND s.catalog_key=?2 AND e.live=1 LIMIT 129")?;
    let rows = q
        .query_map(params![computer, key], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                text(r, 1, 64)?,
                text(r, 2, 4096)?,
                text(r, 3, ledger::PAYLOAD_BYTES)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        rows.len() <= 128,
        "attention removal exceeds live edge limit"
    );
    drop(q);
    for (id, intent, identity, payload) in rows {
        let identity: Identity = serde_json::from_str(&identity)?;
        let action: Action = serde_json::from_str(&payload)?;
        let target =
            attention::target(action.token()).context("attention removal token missing")?;
        if let ledger::Target::Session { conversation, .. } = &identity.target {
            // All canonical tokens carry agent, including main/child notices.
            let cmd = target.get("agent").and_then(Value::as_str).unwrap_or("");
            if current_key(db, computer, cmd, conversation)?.is_some() {
                continue;
            }
        }
        ledger::withdraw(db, &intent, 1)?;
        db.execute(
            "UPDATE desktop_attention_edges SET live=0,dirty=1 WHERE id=?1",
            [id],
        )?;
    }
    Ok(())
}

pub(super) fn head(db: &Connection) -> Result<i64> {
    Ok(db.query_row(
        "SELECT COALESCE(MAX(id),0) FROM desktop_attention_edges",
        [],
        |r| r.get(0),
    )?)
}

pub(super) fn retirement_page(
    db: &Connection,
    after: i64,
    through: i64,
    limit: usize,
) -> Result<Vec<(i64, String)>> {
    // Page the live index directly: one turn reads at most limit rows,
    // independently of the retained non-live history.
    let mut query = db.prepare(
        "SELECT id,intent FROM desktop_attention_edges WHERE live=1 AND id>?1 AND id<=?2 ORDER BY id LIMIT ?3",
    )?;
    let rows = query
        .query_map(params![after, through, limit as i64], |r| {
            Ok((r.get(0)?, text(r, 1, 64)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
pub(super) fn retire(db: &Connection, id: i64) -> Result<()> {
    db.execute(
        "UPDATE desktop_attention_edges SET live=0,blocked=1,dirty=1 WHERE id=?1",
        [id],
    )?;
    Ok(())
}
pub(super) fn has_dirty(db: &Connection) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM desktop_attention_edges WHERE dirty=1 LIMIT 1)",
        [],
        |r| r.get(0),
    )?)
}
