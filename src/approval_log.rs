//! Private desktop automatic-approval journal. Reserve before native input;
//! an existing reservation never authorizes replay, even after a crash.
use crate::{
    cli::{Error, Result},
    config::Config,
    platform,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    path::Path,
    time::Duration,
};

const MAX_INPUT: u64 = 128 * 1024;
const PAGE: usize = 50;
fn bad(message: impl ToString) -> Error {
    Error::new(1, message.to_string())
}
fn input() -> Result<Value> {
    let mut bytes = Vec::new();
    io::stdin().take(MAX_INPUT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_INPUT {
        return Err(bad("Automatic approval data exceeds 128 KiB"));
    }
    let payload: Value = serde_json::from_slice(crate::state::without_bom(&bytes)).map_err(bad)?;
    if !payload.is_object() {
        return Err(bad("Approval journal input must be a JSON object"));
    }
    Ok(payload)
}
fn string<'a>(value: &'a Value, key: &str, max: usize) -> Result<&'a str> {
    let text = value[key]
        .as_str()
        .ok_or_else(|| bad(format!("Missing {key}")))?;
    if text.is_empty() || text.len() > max || text.chars().any(char::is_control) {
        return Err(bad(format!("Invalid {key}")));
    }
    Ok(text)
}

// Select only reviewed one-request grants. Never infer consent from position,
// a default selection, an arbitrary Yes label or a persistent permission option.
// Claude's permission hook (`claude_hook`) carries the same grant as the keys
// do, only for a card whose complete native panel was verified; its free-text
// field denies with feedback and is never chosen here.
fn answers(question: &Value) -> Option<Value> {
    if question["approval"] != true
        || question["can_answer"] != true
        || question["trust_request"] == true
        // U19: the owner (or a member's lead) confirms subagents; an
        // automatic approval never does.
        || question["subagent_confirmation"] == true
        || matches!(question["questions"][0]["header"].as_str(), Some("Agent" | "Task" | "Subagents"))
    {
        return None;
    }
    let hook = question["source"] == "claude_tool_approval"
        && question["answer_transport"] == "claude_hook"
        && question["panel_verified"] == true;
    let items = question["questions"].as_array()?;
    if items.len() != 1
        || items[0]["multi_select"] == true
        || (items[0]["allow_other"] == true && !hook)
    {
        return None;
    }
    let item = &items[0];
    let options = item["options"].as_array()?;
    let grants: Vec<_> = options
        .iter()
        .filter(|option| match question["source"].as_str() {
            Some("claude_tool_approval") => {
                (question["answer_transport"] == "claude_tui" || hook)
                    && question["approval_choices"] == true
                    && item["id"] == "approval"
                    && option["scope"] == "once"
                    && matches!(
                        option["label"].as_str(),
                        Some("Yes" | "Yes, but ask again next time")
                    )
            }
            Some("opencode_native") => {
                question["answer_transport"] == "native_opencode"
                    && question["kind"] == "opencode_permission"
                    && item["id"] == "permission"
                    && option["id"] == "once"
            }
            Some("dsh_native") => {
                question["answer_transport"] == "native_dsh"
                    && item["id"] == "decision"
                    && option["id"] == "allow"
                    && option["label"] == "Allow once"
            }
            _ => false,
        })
        .collect();
    if grants.len() != 1 {
        return None;
    }
    string(grants[0], "id", 128).ok()?;
    Some(json!([{"question_id":item["id"],"selected_option_ids":[grants[0]["id"]],"text":""}]))
}

pub(crate) fn open(root: &Path) -> Result<Connection> {
    // #526: made owner-only here, never trusted to inherit it.
    platform::owner_only_dir(root)?;
    // #569: a store only too open is made private again; one that stays
    // refused is reported with its fix.
    if !crate::private_stores::ensure(root, platform::Private::Directory)? {
        return Err(bad("The approval journal folder must be private"));
    }
    let path = root.join("journal.sqlite3");
    let file = platform::refuse_link(platform::private_open(
        fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true),
    ))
    .open(&path)?;
    if !crate::private_stores::ensure_file(&path, &file)? {
        return Err(bad("The approval journal must be a private regular file"));
    }
    let db = Connection::open(path).map_err(bad)?;
    db.busy_timeout(Duration::from_secs(5)).map_err(bad)?;
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(bad)?;
    if !(0..=1).contains(&version) {
        return Err(bad("The approval journal requires a newer hgs CLI"));
    }
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
      CREATE TABLE IF NOT EXISTS attempts (
        id INTEGER PRIMARY KEY, identity TEXT NOT NULL UNIQUE, request_id TEXT NOT NULL UNIQUE,
        at INTEGER NOT NULL, host TEXT NOT NULL, session TEXT NOT NULL, provider TEXT NOT NULL,
        prompt TEXT NOT NULL, question TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'prepared', detail TEXT NOT NULL DEFAULT ''
      );
      CREATE INDEX IF NOT EXISTS attempts_session ON attempts(session,id DESC);
      CREATE INDEX IF NOT EXISTS attempts_host ON attempts(host,id DESC);
      CREATE INDEX IF NOT EXISTS attempts_host_session ON attempts(host,session,id DESC);").map_err(bad)?;
    if version == 0 {
        db.execute_batch("PRAGMA user_version=1").map_err(bad)?;
    }
    platform::sync_dir(root)?;
    if let Some(parent) = root.parent() {
        platform::sync_dir(parent)?;
    }
    Ok(db)
}

pub(crate) fn begin(db: &mut Connection, payload: &Value) -> Result<Value> {
    let host = string(payload, "host", 256)?;
    let session = string(payload, "session", 1024)?;
    let provider = string(payload, "provider", 64)?;
    let question = &payload["question"];
    if !matches!(
        (provider, question["source"].as_str()),
        ("claude", Some("claude_tool_approval"))
            | ("opencode", Some("opencode_native"))
            | ("dsh", Some("dsh_native"))
    ) {
        return Ok(json!({"eligible":false}));
    }
    let Some(answers) = answers(question) else {
        return Ok(json!({"eligible":false}));
    };
    for key in ["question_id", "question_hash", "run_id", "conversation_id"] {
        string(question, key, 1024)?;
    }
    let hash = string(question, "question_hash", 64)?;
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad("Invalid question hash"));
    }
    let identity = format!(
        "{:x}",
        Sha256::digest(
            json!([
                provider,
                question["run_id"],
                question["conversation_id"],
                question["question_id"],
                hash
            ])
            .to_string()
        )
    );
    let request = uuid::Uuid::new_v4().to_string();
    let at = chrono::Utc::now().timestamp_millis();
    let item = &question["questions"][0];
    let prompt: String = item["question"]
        .as_str()
        .unwrap_or("Approval")
        .chars()
        .take(320)
        .collect();
    let transaction = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(bad)?;
    let inserted = transaction.execute("INSERT OR IGNORE INTO attempts(identity,request_id,at,host,session,provider,prompt,question) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![identity,request,at,host,session,provider,prompt,json!({"question":question,"answers":answers}).to_string()]).map_err(bad)?;
    let (id, request): (i64, String) = transaction
        .query_row(
            "SELECT id,request_id FROM attempts WHERE identity=?1",
            [&identity],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(bad)?;
    transaction.commit().map_err(bad)?;
    Ok(json!({"eligible":true,"fresh":inserted==1,"id":id,"request_id":request,"answers":answers}))
}

pub(crate) fn finish(db: &Connection, payload: &Value) -> Result<Value> {
    let id = payload["id"]
        .as_i64()
        .filter(|id| *id > 0)
        .ok_or_else(|| bad("Invalid approval journal ID"))?;
    let request = string(payload, "request_id", 64)?;
    let status = string(payload, "status", 32)?;
    if !matches!(status, "approved" | "failed" | "unknown" | "cancelled") {
        return Err(bad("Invalid approval outcome"));
    }
    let detail = payload["detail"].as_str().unwrap_or("");
    if detail.len() > 8192 {
        return Err(bad("Approval outcome exceeds 8 KiB"));
    }
    // Never turn a settled decision back into a new opportunity to send.
    let changed = db.execute("UPDATE attempts SET status=?1,detail=?2 WHERE id=?3 AND request_id=?4 AND status='prepared'",params![status,detail,id,request]).map_err(bad)?;
    let current: Option<(String, String)> = db
        .query_row(
            "SELECT status,detail FROM attempts WHERE id=?1 AND request_id=?2",
            params![id, request],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(bad)?;
    if changed == 0 && current != Some((status.into(), detail.into())) {
        return Err(bad("The approval outcome is already final or unavailable"));
    }
    Ok(json!({"id":id,"status":status}))
}

fn list(db: &Connection, payload: &Value) -> Result<Value> {
    let reverse = payload.get("after").is_some();
    let cursor = match payload.get(if reverse { "after" } else { "before" }) {
        Some(value) => value
            .as_i64()
            .ok_or_else(|| bad("Invalid journal cursor"))?,
        None => i64::MAX,
    };
    if cursor <= 0 || (reverse && payload.get("before").is_some()) {
        return Err(bad("Invalid journal cursor"));
    }
    let filter_text = |key: &str| match payload.get(key) {
        Some(value) => value.as_str().ok_or_else(|| bad("Invalid journal filter")),
        None => Ok(""),
    };
    let host = filter_text("host")?;
    let session = filter_text("session")?;
    if host.len() > 256 || session.len() > 1024 {
        return Err(bad("Invalid journal filter"));
    }
    // Separate query shapes keep exact-name filters on their indexes, even
    // with millions of rows. Keyset cursors never scan an OFFSET or COUNT(*).
    let (filter, values): (&str, Vec<&dyn rusqlite::ToSql>) =
        match (host.is_empty(), session.is_empty()) {
            (true, true) => ("", vec![&cursor]),
            (true, false) => (" AND session=?2", vec![&cursor, &session]),
            (false, true) => (" AND host=?2", vec![&cursor, &host]),
            (false, false) => (
                " AND host=?2 AND session=?3",
                vec![&cursor, &host, &session],
            ),
        };
    let operator = if reverse { ">" } else { "<" };
    let order = if reverse { "ASC" } else { "DESC" };
    let mut statement = db.prepare(&format!("SELECT id,at,host,session,provider,prompt,status FROM attempts WHERE id{operator}?1{filter} ORDER BY id {order} LIMIT {}",PAGE+1)).map_err(bad)?;
    let rows = statement.query_map(values.as_slice(),|row| Ok(json!({"id":row.get::<_,i64>(0)?,"at":row.get::<_,i64>(1)?,"host":row.get::<_,String>(2)?,"session":row.get::<_,String>(3)?,"provider":row.get::<_,String>(4)?,"prompt":row.get::<_,String>(5)?,"status":row.get::<_,String>(6)?}))).map_err(bad)?;
    let mut entries = rows
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(bad)?;
    let more = entries.len() > PAGE;
    entries.truncate(PAGE);
    if reverse {
        entries.reverse();
    }
    let next = if more || reverse {
        entries
            .last()
            .map(|v| v["id"].clone())
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let previous = if (!reverse && payload.get("before").is_some()) || (reverse && more) {
        entries
            .first()
            .map(|v| v["id"].clone())
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    Ok(json!({"entries":entries,"next":next,"previous":previous}))
}

pub fn dispatch(config: &Config, target: &str, args: &[String], dry: bool) -> Result<i32> {
    if (!target.is_empty() && !config.is_self(target)) || dry {
        return Err(bad(
            "The approval journal is local and does not support --dry-run",
        ));
    }
    let operation = args.first().map(String::as_str).unwrap_or("");
    if args.len() != 1 || !matches!(operation, "begin" | "finish" | "list" | "detail") {
        return Err(bad(
            "usage: hgs approval-log begin|finish|list|detail (JSON stdin)",
        ));
    }
    let payload = input()?;
    let root = config.state.join("approval-audit");
    if matches!(operation, "list" | "detail") && !root.join("journal.sqlite3").exists() {
        if operation == "detail" {
            return Err(bad("Approval journal entry is unavailable"));
        }
        println!("{}", json!({"entries":[],"next":null,"previous":null}));
        return Ok(0);
    }
    let mut db = open(&root)?;
    let result = match operation {
        "begin" => begin(&mut db, &payload)?,
        "finish" => finish(&db, &payload)?,
        "list" => list(&db, &payload)?,
        _ => {
            let id = payload["id"]
                .as_i64()
                .filter(|id| *id > 0)
                .ok_or_else(|| bad("Invalid approval journal ID"))?;
            let row: Option<(String, String, String)> = db
                .query_row(
                    "SELECT question,status,detail FROM attempts WHERE id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(bad)?;
            let (question, status, detail) =
                row.ok_or_else(|| bad("Approval journal entry is unavailable"))?;
            let saved = serde_json::from_str::<Value>(&question).map_err(bad)?;
            json!({"id":id,"question":saved["question"],"answers":saved["answers"],"status":status,"detail":detail})
        }
    };
    println!("{result}");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn question() -> Value {
        json!({"approval":true,"can_answer":true,"approval_choices":true,"source":"claude_tool_approval","answer_transport":"claude_tui",
            "question_id":"claude-approval:fixture","question_hash":"a".repeat(64),"run_id":"fixture-run","conversation_id":"fixture-conversation",
            "questions":[{"id":"approval","question":"May I run this command?","body":"printf '%s' 'literal $() résumé'","allow_other":false,
                "options":[{"id":"1","label":"Yes","scope":"once"},{"id":"2","label":"Always allow","scope":"broader"},{"id":"3","label":"No","scope":"once"}]}]})
    }
    fn payload() -> Value {
        json!({"host":"desktop-a","session":"claude/sample/review","provider":"claude","question":question()})
    }

    #[test]
    fn selects_only_known_one_time_tool_grants() {
        let q = question();
        assert_eq!(
            answers(&q).unwrap(),
            json!([{"question_id":"approval","selected_option_ids":["1"],"text":""}])
        );
        for (field, value) in [
            ("source", json!("ordinary_question")),
            ("source", json!("claude_folder_trust")),
            ("can_answer", json!(false)),
            ("approval", json!(false)),
            ("trust_request", json!(true)),
            ("answer_transport", json!("unknown")),
        ] {
            let mut changed = q.clone();
            changed[field] = value;
            assert!(answers(&changed).is_none(), "{field}");
        }
        let mut changed = q.clone();
        changed["questions"][0]["options"][0]["scope"] = json!("broader");
        assert!(answers(&changed).is_none());
        let mut changed = q.clone();
        changed["questions"][0]["options"][0]["label"] = json!("Yes, change permissions forever");
        assert!(answers(&changed).is_none());
        let mut changed = q.clone();
        changed["questions"][0]["allow_other"] = json!(true);
        assert!(answers(&changed).is_none());
        let mut changed = q.clone();
        let duplicate = changed["questions"][0]["options"][0].clone();
        changed["questions"][0]["options"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(answers(&changed).is_none());
        // Through Claude's permission hook only with the verified native panel.
        let mut hook = question();
        hook["answer_transport"] = json!("claude_hook");
        assert!(
            answers(&hook).is_none(),
            "an unverified panel is no reviewed grant"
        );
        hook["panel_verified"] = json!(true);
        assert_eq!(answers(&hook), answers(&question()));
        hook["questions"][0]["allow_other"] = json!(true);
        assert_eq!(
            answers(&hook),
            answers(&question()),
            "feedback is never chosen"
        );
        for (field, value) in [
            ("panel_verified", json!("true")),
            ("answer_transport", json!("claude_hooks")),
            ("can_answer", json!(false)),
            ("approval_choices", json!(false)),
            ("source", json!("claude_terminal_safety")),
        ] {
            let mut changed = hook.clone();
            changed[field] = value;
            assert!(answers(&changed).is_none(), "{field}");
        }
        for (label, scope) in [
            ("Yes", "broader"),
            ("Yes, and don't ask again", "once"),
            ("No", "once"),
        ] {
            let mut changed = hook.clone();
            changed["questions"][0]["options"] = json!([{"id":"1","label":label,"scope":scope},{"id":"2","label":"No","scope":"once"}]);
            assert!(answers(&changed).is_none(), "{label} {scope}");
        }
        let mut opencode = q;
        opencode["source"] = json!("opencode_native");
        opencode["answer_transport"] = json!("native_opencode");
        opencode["kind"] = json!("opencode_permission");
        opencode["questions"][0]["id"] = json!("permission");
        opencode["approval_choices"] = json!(true);
        opencode["questions"][0]["options"] = json!([{"id":"once","label":"Allow once","scope":"once"},{"id":"always","label":"Always allow","scope":"broader"},{"id":"reject","label":"Reject","scope":"once"}]);
        assert_eq!(
            answers(&opencode).unwrap()[0]["selected_option_ids"],
            json!(["once"])
        );
        opencode["kind"] = json!("opencode_form");
        assert!(answers(&opencode).is_none());
        let mut dsh = question();
        dsh["source"] = json!("dsh_native");
        dsh["answer_transport"] = json!("native_dsh");
        dsh["questions"][0]["id"] = json!("decision");
        dsh["questions"][0]["options"] =
            json!([{"id":"allow","label":"Allow once"},{"id":"deny","label":"Deny"}]);
        assert_eq!(
            answers(&dsh).unwrap()[0]["selected_option_ids"],
            json!(["allow"])
        );
        dsh["questions"][0]["options"][0]["label"] = json!("Always allow");
        assert!(answers(&dsh).is_none());
    }

    /// U19 (1): an automatic approval never answers a subagent confirmation,
    /// even one shaped like a grantable hook card.
    #[test]
    fn a_subagent_confirmation_is_never_approved_automatically() {
        let card = |subagents: bool| {
            let mut card = json!({"approval":true,"can_answer":true,"source":"claude_tool_approval","answer_transport":"claude_hook",
                "panel_verified":true,"approval_choices":true,"question_id":"claude-approval:x","question_hash":"a".repeat(64),
                "run_id":"run","conversation_id":"conversation",
                "questions":[{"id":"approval","header":"Bash","question":"Run it?","allow_other":true,
                    "options":[{"id":"1","label":"Yes","scope":"once"},{"id":"2","label":"No","scope":"once"}]}]});
            if subagents {
                card["subagent_confirmation"] = json!(true);
                card["questions"][0]["header"] = json!("Subagents");
            }
            card
        };
        assert!(answers(&card(false)).is_some(), "the same card for another tool is grantable");
        assert!(answers(&card(true)).is_none());
        let mut flagged_only = card(false);
        flagged_only["subagent_confirmation"] = json!(true);
        assert!(answers(&flagged_only).is_none());
        let mut agent = card(false);
        agent["questions"][0]["header"] = json!("Agent");
        assert!(answers(&agent).is_none(), "an Agent request without the mark is refused as well");
        // Nor its "don't ask again" choice, which outlives the request.
        let mut always = card(true);
        always["questions"][0]["options"] = json!([{"id":"1","label":"Yes","scope":"once"},
            {"id":"3","label":"Yes, and don't ask again for subagents","scope":"broader"},{"id":"2","label":"No","scope":"once"}]);
        assert!(answers(&always).is_none());
        always.as_object_mut().unwrap().remove("subagent_confirmation");
        always["questions"][0]["header"] = json!("Bash");
        assert_eq!(answers(&always).unwrap()[0]["selected_option_ids"], json!(["1"]), "only ever the one-time Yes");
    }

    #[test]
    fn reserves_durably_and_never_replays_after_crash_or_final_failure() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("audit");
        let mut db = open(&root).unwrap();
        let first = begin(&mut db, &payload()).unwrap();
        assert_eq!(first["fresh"], true);
        assert!(uuid::Uuid::parse_str(first["request_id"].as_str().unwrap()).is_ok());
        let second = begin(&mut db, &payload()).unwrap();
        assert_eq!(second["fresh"], false);
        assert_eq!(second["request_id"], first["request_id"]);
        // A renamed session or another SSH alias is still the same permission.
        let mut renamed = payload();
        renamed["host"] = json!("renamed-alias");
        renamed["session"] = json!("claude/sample/renamed");
        assert_eq!(begin(&mut db, &renamed).unwrap()["fresh"], false);
        drop(db);
        let mut db = open(&root).unwrap();
        assert_eq!(begin(&mut db, &payload()).unwrap()["fresh"], false);
        let failed = json!({"id":first["id"],"request_id":first["request_id"],"status":"unknown","detail":"delivery uncertain"});
        finish(&db, &failed).unwrap();
        finish(&db, &failed).unwrap();
        let mut changed = failed;
        changed["status"] = json!("approved");
        assert!(finish(&db, &changed).is_err());
        assert_eq!(begin(&mut db, &payload()).unwrap()["fresh"], false);
        let saved: String = db
            .query_row("SELECT question FROM attempts", [], |r| r.get(0))
            .unwrap();
        let saved: Value = serde_json::from_str(&saved).unwrap();
        assert_eq!(saved["question"], question());
        assert_eq!(saved["answers"], first["answers"]);
        let mut next = payload();
        next["question"]["question_hash"] = json!("b".repeat(64));
        assert_eq!(begin(&mut db, &next).unwrap()["fresh"], true);
    }

    #[test]
    fn kimi_tool_approvals_are_never_answered_automatically() {
        let mut q = question();
        q["source"] = json!("kimi_tool_approval");
        q["answer_transport"] = json!("kimi_tui");
        q["questions"][0]["options"] = json!([{"id":"1","label":"Approve once","scope":"once"},
            {"id":"2","label":"Approve for this session","scope":"broader"},{"id":"3","label":"Reject","scope":"once"}]);
        assert!(answers(&q).is_none());
        let directory = tempfile::tempdir().unwrap();
        let mut db = open(&directory.path().join("audit")).unwrap();
        for provider in ["kimi", "claude"] {
            let p = json!({"host":"desktop-a","session":"kimi/sample/review","provider":provider,"question":q});
            assert_eq!(begin(&mut db, &p).unwrap()["eligible"], false);
        }
    }

    #[test]
    fn refuses_wrong_provider_and_malformed_identities_without_a_reservation() {
        let directory = tempfile::tempdir().unwrap();
        let mut db = open(&directory.path().join("audit")).unwrap();
        let mut p = payload();
        p["provider"] = json!("codex");
        assert_eq!(begin(&mut db, &p).unwrap()["eligible"], false);
        let mut p = payload();
        p["question"]["run_id"] = json!("");
        assert!(begin(&mut db, &p).is_err());
        let mut p = payload();
        p["question"]["question_hash"] = json!("invalid");
        assert!(begin(&mut db, &p).is_err());
        let count: i64 = db
            .query_row("SELECT count(*) FROM attempts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn pages_and_exact_filters_stay_indexed_with_one_hundred_thousand_entries() {
        let directory = tempfile::tempdir().unwrap();
        let mut db = open(&directory.path().join("audit")).unwrap();
        let transaction = db.transaction().unwrap();
        {
            let mut insert=transaction.prepare("INSERT INTO attempts(identity,request_id,at,host,session,provider,prompt,question) VALUES(?1,?1,?2,?3,?4,'claude','Question','{}')").unwrap();
            for i in 1..=100_000 {
                insert
                    .execute(params![
                        format!("fixture-{i}"),
                        i,
                        if i % 2 == 0 { "desktop-a" } else { "desktop-b" },
                        format!("claude/sample/{}", i % 20_000)
                    ])
                    .unwrap();
            }
        }
        transaction.commit().unwrap();
        let page = list(&db, &json!({})).unwrap();
        assert_eq!(page["entries"].as_array().unwrap().len(), PAGE);
        assert_eq!(page["entries"][0]["id"], 100_000);
        assert!(page["previous"].is_null());
        let older = list(&db, &json!({"before":page["next"]})).unwrap();
        assert_eq!(older["entries"][0]["id"], 99_950);
        let newer = list(&db, &json!({"after":older["previous"]})).unwrap();
        assert_eq!(newer["entries"], page["entries"]);
        assert!(newer["previous"].is_null());
        let filtered = list(
            &db,
            &json!({"host":"desktop-a","session":"claude/sample/42"}),
        )
        .unwrap();
        assert_eq!(filtered["entries"].as_array().unwrap().len(), 5);
        assert!(filtered["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["host"] == "desktop-a" && row["session"] == "claude/sample/42"));
        assert!(
            list(&db, &json!({"session":"' OR 1=1 --"})).unwrap()["entries"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        for (filter, index) in [
            (" AND session='claude/sample/42'", "attempts_session"),
            (
                " AND host='desktop-a' AND session='claude/sample/42'",
                "attempts_host_session",
            ),
        ] {
            let mut plan=db.prepare(&format!("EXPLAIN QUERY PLAN SELECT id,at,host,session,provider,prompt,status FROM attempts WHERE id<99999{filter} ORDER BY id DESC LIMIT 51")).unwrap();
            let detail = plan
                .query_map([], |row| row.get::<_, String>(3))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
                .join(" ");
            assert!(detail.contains(index), "{detail}");
            assert!(
                !detail.contains("SCAN ") && !detail.contains("TEMP B-TREE"),
                "{detail}"
            );
        }
        assert!(list(&db, &json!({"before":0})).is_err());
        assert!(list(&db, &json!({"before":20,"after":10})).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn journal_is_private_and_refuses_symlink_targets() {
        use std::os::unix::{fs::symlink, fs::PermissionsExt};
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("audit");
        let db = open(&root).unwrap();
        drop(db);
        assert_eq!(fs::metadata(&root).unwrap().permissions().mode() & 0o077, 0);
        assert_eq!(
            fs::metadata(root.join("journal.sqlite3"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        let target = directory.path().join("unrelated");
        fs::write(&target, b"keep").unwrap();
        fs::remove_file(root.join("journal.sqlite3")).unwrap();
        symlink(&target, root.join("journal.sqlite3")).unwrap();
        assert!(open(&root).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"keep");
    }
}
