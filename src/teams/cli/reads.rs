//! The reads of `hgs team`. Each one is a bounded page of the store, shaped
//! once here so the desktop's projections and a person's terminal see the same
//! reply. Status never carries the whole team unless `--bundle` asks for it.
use super::actor::{self, Caller};
use super::input::{self, Options};
use super::{not_found_team, storage};
use crate::config::Config;
use crate::teams::store::{Cursor, Page, Store, TeamRow};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use zerus_team_plan::model::{Node, SessionKey, TeamStatus};
use zerus_team_plan::problem::code;
use zerus_team_plan::Problem;

/// Explicit, bounded Git read for the Advance base form, never a polling read.
pub(super) fn base(config: &Config, caller: &Caller, args: &[String]) -> Result<Value, Problem> {
    let options = Options::parse(args, &["--team"], &[])?;
    options.nothing_else("base")?;
    let store = Store::open(&config.state)?;
    let identity = actor::resolve(&store, caller)?;
    actor::person_only(&identity, "base")?;
    let id = actor::target_team(&store, &identity, options.team()?)?;
    let team = store.team(&id)?.ok_or_else(|| not_found_team(&id))?;
    let repo = crate::desktop::teams::repository(config, &team)?;
    let refs = crate::teams::integrate::main_refs(&repo)?;
    Ok(
        json!({"ok":true,"actor":actor::view(&identity.actor),"team":id,"revision":team.revision,"base_commit":team.base_commit,"refs":refs}),
    )
}

/// `status [--team ID] [--bundle]`: the team, its members and counts.
pub(super) fn status(config: &Config, caller: &Caller, args: &[String]) -> Result<Value, Problem> {
    let options = Options::parse(args, &["--team"], &["--bundle"])?;
    options.nothing_else("status")?;
    let store = Store::open(&config.state)?;
    let identity = actor::resolve(&store, caller)?;
    let team = actor::target_team(&store, &identity, options.team()?)?;
    let row = store.team(&team)?.ok_or_else(|| not_found_team(&team))?;
    // The team-specific diagnostic keeps the hard roster-bound refusal.
    let _ = store.member_sessions(&team)?;
    let mut view = serde_json::to_value(&row).map_err(storage)?;
    if !options.has("--bundle") {
        if let Some(object) = view.as_object_mut() {
            object.remove("bundle");
        }
    } else if let Some(settings) = view
        .pointer_mut("/bundle/template/settings")
        .and_then(Value::as_object_mut)
    {
        // The team's own settings (including the delegations chosen at create)
        // are authoritative. The pinned template in the bundle is only the
        // source it came from, so its delegations are not shown a second time.
        settings.remove("delegations");
    }
    let members = store.complete_member_rows(&team)?;
    let running = crate::desktop::teams::presence(config);
    // A stopped or archived team takes no cards: its service conditions and
    // refused cancels are shown here. A live team's cancels refused have the
    // lead's inbox item, and its failing work its cards.
    let terminal = matches!(row.status, TeamStatus::Stopped | TeamStatus::Archived);
    let cancel_refusals = if terminal {
        Some(store.cancel_refusal_page(&team)?)
    } else {
        None
    };
    let mut reply = json!({
        "ok": true,
        "actor": actor::view(&identity.actor),
        "team": view,
        "members": super::board::members(&row,members, &store.member_edit_work(&team)?)?,
        "member_roles": row.bundle.roles.iter().map(|r|json!({"id":r.id,"name":r.name,"agent":r.agent,"model":r.model,"effort":r.effort,"account":r.account})).collect::<Vec<_>>(),
        "counts": counts(&store, &team)?,
        "service_running": running,
        "health": crate::desktop::teams::health(&config.state),
        "service_items": crate::desktop::teams::service_items(&config.state, &team, running, input::now_ms()),
        "key_refusals": store.key_refusal_page(&team)?,
        "cancel_refusals": cancel_refusals,
    });
    if row.status == TeamStatus::Planning {
        reply["planning"] = serde_json::to_value(store.planning(&team)?).map_err(storage)?;
    }
    Ok(reply)
}

/// One page of the queue, the lead inbox, the owner cards or the journal.
pub(super) fn list(
    config: &Config,
    caller: &Caller,
    command: &str,
    args: &[String],
) -> Result<Value, Problem> {
    let flags: &[&str] = if command == "journal" {
        &["--recorded-timings"]
    } else {
        &[]
    };
    let options = Options::parse(
        args,
        &["--team", "--limit", "--after", "--timing-seqs"],
        flags,
    )?;
    options.nothing_else(command)?;
    if options.get("--timing-seqs").is_some()
        && (command != "journal" || !options.has("--recorded-timings"))
    {
        return Err(input::invalid(
            "Selected timing sequences require journal --recorded-timings.",
        ));
    }
    let store = Store::open(&config.state)?;
    let identity = actor::resolve(&store, caller)?;
    let team = actor::target_team(&store, &identity, options.team()?)?;
    actor::lead_reader(&store, &identity, &team)?;
    let (limit, cursor) = options.page()?;
    let cursor = cursor.map(input::parse_cursor).transpose()?;
    let cursor = cursor.as_ref();
    match command {
        "queue" => {
            // One extra team primary-key lookup per page, no per-row SQL:
            // stage kinds, rounds and roles all use this pinned bundle.
            let pinned = store.team(&team)?.ok_or_else(|| not_found_team(&team))?;
            page(
                &identity,
                &team,
                super::board::queue(&pinned, store.queue_rows(&team, limit, cursor)?)?,
            )
        }
        "inbox" => page(&identity, &team, store.inbox(&team, limit, cursor)?),
        "cards" => page(&identity, &team, store.cards(&team, limit, cursor)?),
        "journal" if options.has("--recorded-timings") => {
            let recorded = if let Some(ids) = options.get("--timing-seqs") {
                if ids.len() > 200 * 17 {
                    return Err(input::invalid("Too many selected timing sequences."));
                }
                let ids: Vec<u64> = if ids == "none" {
                    Vec::new()
                } else {
                    ids.split(',')
                        .map(|id| {
                            id.parse::<u64>()
                                .ok()
                                .filter(|n| n.to_string() == id)
                                .ok_or_else(|| input::invalid("Invalid selected timing sequence."))
                        })
                        .collect::<Result<_, _>>()?
                };
                store.recorded_selected_journal(&team, limit, cursor, &ids)?
            } else {
                store.recorded_journal(&team, limit, cursor)?
            };
            let mut reply = page(&identity, &team, recorded.page)?;
            reply["timings"] = json!({"schema": 1, "revision": recorded.revision});
            Ok(reply)
        }
        "journal" => page(&identity, &team, store.journal(&team, limit, cursor)?),
        _ => Err(super::unknown(command)),
    }
}

fn page<T: Serialize>(
    identity: &actor::Identity,
    team: &str,
    page: Page<T>,
) -> Result<Value, Problem> {
    let next = page.next.as_ref().map(input::cursor_token).transpose()?;
    Ok(json!({
        "ok": true,
        "actor": actor::view(&identity.actor),
        "team": team,
        "rows": page.rows,
        "next": next,
    }))
}

/// `list [--limit] [--after]`: the teams of this computer, in ID order. Any
/// proved caller may read it; it is metadata only.
pub(super) fn list_teams(
    config: &Config,
    caller: &Caller,
    args: &[String],
) -> Result<Value, Problem> {
    let options = Options::parse(args, &["--limit", "--after"], &[])?;
    options.nothing_else("list")?;
    let store = Store::open(&config.state)?;
    let identity = actor::resolve(&store, caller)?;
    let (limit, cursor) = options.page()?;
    let cursor = cursor.map(input::parse_cursor).transpose()?;
    let page = store.teams(limit, cursor.as_ref())?;
    let mut progress = store.team_progress(&page.rows)?;
    let ids = page
        .rows
        .iter()
        .map(|row| row.id.as_str())
        .collect::<Vec<_>>();
    let blocked = crate::desktop::teams::blocked_service_teams(
        &config.state,
        &ids,
        crate::teams::store::STUCK_SERVICE_KINDS,
    )
    .unwrap_or_default();
    for (id, summary) in &mut progress {
        summary.stuck |= blocked.contains(id);
    }
    let rows = page
        .rows
        .into_iter()
        .map(|row| {
            let summary = progress.remove(&row.id).unwrap_or_default();
            listed_team(&store, row, summary)
        })
        .collect::<Result<Vec<_>, _>>()?;
    global_rows_with_next(&identity, rows, page.next.as_ref())
}

fn listed_team(
    store: &Store,
    row: TeamRow,
    progress: crate::teams::store::TeamProgress,
) -> Result<Value, Problem> {
    // One indexed roster read supplies both fields for each valid live row.
    // An oversized or invalid roster omits memberships and uses one lead PK lookup;
    // inactive rows keep just the existing lead lookup. No bundle is loaded.
    let live = matches!(
        row.status,
        TeamStatus::Planning | TeamStatus::Running | TeamStatus::Paused
    );
    let sessions = if live {
        match store.member_sessions(&row.id) {
            Ok(sessions) => Some(sessions),
            Err(problem) if problem.code == code::OVER_CAP => None,
            Err(problem)
                if problem.code == code::INVALID_STATE && problem.refs == ["member_sessions"] =>
            {
                None
            }
            Err(problem) => return Err(problem),
        }
    } else {
        None
    };
    let lead_session = match &sessions {
        Some(sessions) => sessions
            .iter()
            .find(|member| member.member == row.lead)
            .map(|member| SessionKey {
                machine: member.machine.clone(),
                session_id: member.session_id.clone(),
            }),
        None => store.member_session(&row.id, &row.lead)?,
    };
    let mut value = serde_json::to_value(&row).map_err(storage)?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "progress".into(),
            serde_json::to_value(progress).map_err(storage)?,
        );
        object.insert(
            "lead_session".into(),
            serde_json::to_value(lead_session).map_err(storage)?,
        );
        if let Some(sessions) = sessions.filter(|sessions| !sessions.is_empty()) {
            object.insert(
                "member_sessions".into(),
                serde_json::to_value(sessions).map_err(storage)?,
            );
        }
    }
    Ok(value)
}

/// A global page's rows with their next cursor.
fn global_rows_with_next(
    identity: &actor::Identity,
    rows: Vec<Value>,
    next: Option<&Cursor>,
) -> Result<Value, Problem> {
    Ok(json!({
        "ok": true,
        "actor": actor::view(&identity.actor),
        "rows": rows,
        "next": next.map(input::cursor_token).transpose()?,
    }))
}

/// `waiting [--limit] [--after]`: every open owner card, oldest first. The
/// person's read; a lead sees its own team's cards with `cards`.
pub(super) fn waiting(config: &Config, caller: &Caller, args: &[String]) -> Result<Value, Problem> {
    let options = Options::parse(args, &["--limit", "--after"], &[])?;
    options.nothing_else("waiting")?;
    let store = Store::open(&config.state)?;
    let identity = actor::resolve(&store, caller)?;
    actor::person_only(&identity, "waiting")?;
    let (limit, cursor) = options.page()?;
    let cursor = cursor.map(input::parse_cursor).transpose()?;
    let page = store.waiting(limit, cursor.as_ref())?;
    let rows = page
        .rows
        .into_iter()
        .map(|waiting| {
            let mut row = serde_json::to_value(&waiting.card).map_err(storage)?;
            if let Some(object) = row.as_object_mut() {
                object.insert("team".into(), json!(waiting.team));
                if let Some(reference) = store.phase_card_ref(&waiting.team, waiting.card.seq)? {
                    object.insert("phase_reference".into(), json!(reference));
                }
            }
            Ok(row)
        })
        .collect::<Result<Vec<_>, Problem>>()?;
    global_rows_with_next(&identity, rows, page.next.as_ref())
}

/// `current`: what a member holds now, bounded to this computer's open nodes.
pub(super) fn current(config: &Config, caller: &Caller, args: &[String]) -> Result<Value, Problem> {
    let options = Options::parse(args, &[], &[])?;
    options.nothing_else("current")?;
    let store = Store::open(&config.state)?;
    let identity = actor::resolve(&store, caller)?;
    actor::member_session(&identity, "current")?;
    let team = identity
        .team
        .clone()
        .ok_or_else(|| not_found_team("the caller's team"))?;
    let member = identity
        .member
        .clone()
        .ok_or_else(|| Problem::new(code::NOT_FOUND, "This session is not a team member."))?;
    let row = store
        .member(&team, &member)?
        .ok_or_else(|| Problem::new(code::UNKNOWN_MEMBER, "Member does not exist."))?;
    let team_row = store.team(&team)?.ok_or_else(|| not_found_team(&team))?;
    let mut reply = json!({
        "ok": true,
        "actor": actor::view(&identity.actor),
        "team": team,
        "revision": team_row.revision,
        "member": row,
        "nodes": held(&store, &team, &member)?,
    });
    if member == team_row.lead && team_row.status == TeamStatus::Planning {
        reply["planning"] = json!({"goal":team_row.goal,"state":store.planning(&team)?});
    }
    Ok(reply)
}

/// A member's own live nodes. The queue is bounded by the team's cap, and this
/// walk stops after three pages even when a page keeps growing.
fn held(store: &Store, team: &str, member: &str) -> Result<Vec<Value>, Problem> {
    let mut found = Vec::new();
    let mut cursor: Option<Cursor> = None;
    for _ in 0..3 {
        let page = store.queue(team, crate::teams::store::MAX_PAGE, cursor.as_ref())?;
        let mut rows = page.rows;
        cursor = page.next;
        for node in rows.drain(..) {
            if let Some(holder) = store.attempt_holder(team, node.id)? {
                if holder.member == member {
                    let questions = store.questions(team, node.id, holder.generation, member)?;
                    found.push(held_view(node, holder.generation, questions)?);
                }
            }
        }
        if cursor.is_none() {
            break;
        }
    }
    Ok(found)
}

/// A held node with the attempt generation `ask` and a verdict need. The node
/// itself does not store it; the store keeps it beside the row. The member's
/// own questions of this attempt follow, newest first, with the lead's answer.
fn held_view(
    node: Node,
    generation: u32,
    questions: crate::teams::store::timers::Questions,
) -> Result<Value, Problem> {
    let mut value = serde_json::to_value(&node).map_err(storage)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("generation".into(), json!(generation));
        object.insert("questions".into(), json!(questions.rows));
        object.insert("questions_more".into(), json!(questions.more));
    }
    Ok(value)
}

/// The counts a team bar shows. Each is one aggregate query, never a whole
/// team in one value.
fn counts(store: &Store, team: &str) -> Result<Value, Problem> {
    let Some(db) = store.db.as_ref() else {
        return Ok(empty_counts());
    };
    let member_counts = by_state(
        db,
        "SELECT state,count(*) FROM members WHERE team=?1 GROUP BY state",
        team,
    )?;
    let node_counts = by_state(
        db,
        "SELECT node_state,count(*) FROM nodes WHERE team=?1 GROUP BY node_state",
        team,
    )?;
    let inbox = scalar(
        db,
        "SELECT count(*) FROM inbox WHERE team=?1 AND state='open'",
        team,
    )?;
    let cards = scalar(
        db,
        "SELECT count(*) FROM cards WHERE team=?1 AND state='open'",
        team,
    )?;
    let count = |row: &BTreeMap<String, u32>, key: &str| row.get(key).copied().unwrap_or(0);
    let total: u32 = count(&member_counts, "not_created")
        + count(&member_counts, "active")
        + count(&member_counts, "paused")
        + count(&member_counts, "removed");
    Ok(json!({
        "members": {
            "total": total,
            "not_created": count(&member_counts, "not_created"),
            "active": count(&member_counts, "active"),
            "paused": count(&member_counts, "paused"),
            "removed": count(&member_counts, "removed"),
        },
        "nodes": {
            "waiting": count(&node_counts, "waiting"),
            "pending": count(&node_counts, "pending"),
            "in_progress": count(&node_counts, "in_progress"),
            "reported": count(&node_counts, "reported"),
            "accepted": count(&node_counts, "accepted"),
            "cancelled": count(&node_counts, "cancelled"),
        },
        "inbox": {"open": inbox},
        "cards": {"open": cards},
    }))
}

fn empty_counts() -> Value {
    json!({
        "members": {"total": 0, "not_created": 0, "active": 0, "paused": 0, "removed": 0},
        "nodes": {"waiting": 0, "pending": 0, "in_progress": 0, "reported": 0,
                  "accepted": 0, "cancelled": 0},
        "inbox": {"open": 0},
        "cards": {"open": 0},
    })
}

fn by_state(
    db: &rusqlite::Connection,
    sql: &str,
    team: &str,
) -> Result<BTreeMap<String, u32>, Problem> {
    let mut statement = db.prepare(sql).map_err(storage)?;
    let rows = statement
        .query_map([team], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
        })
        .map_err(storage)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage)?;
    Ok(rows
        .into_iter()
        .map(|(state, count)| (state.trim_matches('"').to_owned(), count))
        .collect())
}

fn scalar(db: &rusqlite::Connection, sql: &str, team: &str) -> Result<u32, Problem> {
    db.query_row(sql, [team], |row| row.get(0)).map_err(storage)
}

/// `evidence --team ID --node N` (zerus-5gcbh): the evidence beside one task's
/// decision for the person and the lead. The task's latest report (by the
/// task's implementer, with its commit and the service's notes) and its
/// stages, newest first and bounded, each with its kind and round from the
/// team's pinned bundle and its latest verdict: who gave it, pass or return,
/// and the findings. Nothing is invented for a task that has none.
pub(super) fn evidence(
    config: &Config,
    caller: &Caller,
    args: &[String],
) -> Result<Value, Problem> {
    let options = Options::parse(args, &["--team", "--node"], &[])?;
    options.nothing_else("evidence")?;
    let node = options
        .get("--node")
        .ok_or_else(|| input::invalid("Select a task with --node."))?
        .parse::<u32>()
        .ok()
        .filter(|node| *node > 0)
        .ok_or_else(|| input::invalid("Invalid node."))?;
    let store = Store::open(&config.state)?;
    let identity = actor::resolve(&store, caller)?;
    let team = actor::target_team(&store, &identity, options.team()?)?;
    actor::lead_reader(&store, &identity, &team)?;
    // The revision, the report and every stage come from one snapshot, so a
    // window can tell evidence of the decision it shows from an older one.
    let evidence = store.task_evidence(&team, node)?;
    let stages: Vec<Value> = evidence
        .stages
        .iter()
        .map(|stage| {
            json!({
                "node": stage.node,
                "kind": stage.kind,
                "round": stage.round,
                "state": stage.state,
                "verdict": stage.verdict.as_ref().map(|verdict| json!({
                    "generation": verdict.generation,
                    "member": verdict.actor.get("member").and_then(Value::as_str),
                    "pass": verdict.pass,
                    "text": verdict.text,
                })),
            })
        })
        .collect();
    Ok(json!({
        "ok": true,
        "actor": actor::view(&identity.actor),
        "team": team,
        "revision": evidence.revision,
        "node": node,
        "report": evidence.report.map(|report| json!({
            "generation": report.generation,
            "member": report.member,
            "commit": report.commit,
            "text": report.text,
            "notes": report.notes,
        })),
        "stages": stages,
        "more_stages": evidence.more_stages,
    }))
}
