use super::*;

struct Fixture {
    _temp: tempfile::TempDir,
    reader: Reader,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        Self {
            reader: Reader {
                root: temp.path().join("approval-audit"),
                configuration: "a".repeat(64),
                epoch: uuid::Uuid::new_v4().to_string(),
                journal: None,
            },
            _temp: temp,
        }
    }
    fn query(&self) -> Value {
        json!({"op":"approval_audit_list","request_id":uuid::Uuid::new_v4().to_string(),
            "configuration":self.reader.configuration,"epoch":self.reader.epoch})
    }
    fn rows(&self, count: usize) -> Connection {
        let db = crate::approval_log::open(&self.reader.root).unwrap();
        for n in 1..=count {
            db.execute(
                "INSERT INTO attempts(identity,request_id,at,host,session,provider,prompt,question)
                VALUES(?1,?2,?3,?4,'agent/project/fixture','claude','Fixture question',?5)",
                params![
                    format!("identity-{n}"),
                    uuid::Uuid::new_v4().to_string(),
                    n as i64,
                    if n % 2 == 0 { "a.example" } else { "b.example" },
                    json!({"question":{"question_id":format!("fixture-{n}")},"answers":[]})
                        .to_string()
                ],
            )
            .unwrap();
        }
        db
    }
    fn send(&mut self, request: &Value) -> Value {
        let value = self.reader.request(request);
        let typed: Request = serde_json::from_value(request.clone()).unwrap();
        assert!(typed.matches(&value), "bounded selected-service envelope");
        value
    }
}
#[test]
fn absent_journal_is_empty_without_creating_or_authorizing_anything() {
    let mut f = Fixture::new();
    let reply = f.send(&f.query());
    assert_eq!(reply["accepted"], true);
    assert_eq!(reply["result"]["entries"], json!([]));
    assert_eq!(reply["journal"], Value::Null);
    assert!(!f.reader.root.exists());
}
#[test]
fn indexed_pages_hold_their_high_water_and_detail_uses_the_same_journal() {
    let mut f = Fixture::new();
    let db = f.rows(123);
    let mut request = f.query();
    let first = f.send(&request);
    assert_eq!(first["result"]["entries"].as_array().unwrap().len(), 50);
    assert_eq!(first["result"]["entries"][0]["id"], 123);
    request["journal"] = first["journal"].clone();
    request["through"] = first["result"]["through"].clone();
    db.execute(
        "INSERT INTO attempts(identity,request_id,at,host,session,provider,prompt,question)
        VALUES('later','later',999,'a.example','later','claude','later','{}')",
        [],
    )
    .unwrap();
    let mut ids = first["result"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect::<Vec<_>>();
    let mut page = first.clone();
    while !page["result"]["next"].is_null() {
        request["before"] = page["result"]["next"].clone();
        page = f.send(&request);
        ids.extend(
            page["result"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["id"].as_i64().unwrap()),
        );
    }
    assert_eq!(ids, (1..=123).rev().collect::<Vec<_>>());
    let detail = json!({"op":"approval_audit_detail","request_id":uuid::Uuid::new_v4().to_string(),
        "configuration":f.reader.configuration,"epoch":f.reader.epoch,"journal":first["journal"],"id":123});
    let reply = f.send(&detail);
    assert_eq!(reply["result"]["question"]["question_id"], "fixture-123");
    let mut filtered = f.query();
    filtered["host"] = json!("a.example");
    let reply = f.send(&filtered);
    assert_eq!(reply["accepted"], true);
    assert!(reply["result"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["host"] == "a.example"));
    let journal = f.reader.journal.as_ref().unwrap();
    assert!(journal.db.execute("DELETE FROM attempts", []).is_err());
    assert!(journal
        .db
        .execute_batch("CREATE TABLE forbidden(value)")
        .is_err());
    assert_eq!(
        db.query_row("SELECT count(*) FROM attempts", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        124
    );
}
#[test]
fn stale_scope_and_unknown_or_mutating_shapes_never_read_the_journal() {
    let mut f = Fixture::new();
    let mut request = f.query();
    request["epoch"] = json!(uuid::Uuid::new_v4().to_string());
    assert_eq!(
        f.reader.request(&request)["code"],
        "approval_audit_scope_changed"
    );
    request = f.query();
    request["path"] = json!("/not/selected");
    assert_eq!(f.reader.request(&request)["accepted"], false);
    request = f.query();
    request["op"] = json!("approval_audit_begin");
    assert_eq!(f.reader.request(&request)["accepted"], false);
    assert!(f.reader.journal.is_none());
    assert!(!f.reader.root.exists());
}
#[test]
fn oversized_records_or_missing_indexes_refuse_without_truncation_or_repair() {
    let mut f = Fixture::new();
    let db = f.rows(1);
    let first = f.send(&f.query());
    db.execute(
        "UPDATE attempts SET question=?1",
        ["x".repeat(wire::REPLY_BYTES)],
    )
    .unwrap();
    let detail = json!({"op":"approval_audit_detail","request_id":uuid::Uuid::new_v4().to_string(),
        "configuration":f.reader.configuration,"epoch":f.reader.epoch,"journal":first["journal"],"id":1});
    assert_eq!(f.send(&detail)["accepted"], false);
    db.execute("DROP INDEX attempts_host", []).unwrap();
    let mut request = f.query();
    request["host"] = json!("a.example");
    assert_eq!(f.send(&request)["accepted"], false);
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='attempts_host'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}
#[test]
fn an_unknown_schema_is_not_migrated_by_an_audit_read() {
    let mut f = Fixture::new();
    let db = f.rows(1);
    db.execute_batch("PRAGMA user_version=0").unwrap();
    assert_eq!(f.send(&f.query())["accepted"], false);
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn sqlite_connection_must_match_the_original_guard_even_if_its_path_is_unchanged() {
    let mut f = Fixture::new();
    let _original = f.rows(1);
    let first = f.send(&f.query());
    let other = Fixture::new();
    let _other = other.rows(2);
    // Deterministic open-race control: the guard still owns A, while SQLite
    // opened B. A path-only check of the separate guard would miss this.
    f.reader.journal.as_mut().unwrap().db = Connection::open_with_flags(
        other.reader.root.join("journal.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let detail = json!({"op":"approval_audit_detail","request_id":uuid::Uuid::new_v4().to_string(),
        "configuration":f.reader.configuration,"epoch":f.reader.epoch,"journal":first["journal"],"id":1});
    assert_eq!(f.send(&detail)["accepted"], false);
}
#[cfg(unix)]
#[test]
fn replacement_cannot_rebind_an_old_detail_id_or_cursor() {
    let mut f = Fixture::new();
    let db = f.rows(1);
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")
        .unwrap();
    drop(db);
    let first = f.send(&f.query());
    fs::rename(
        f.reader.root.join("journal.sqlite3"),
        f.reader.root.join("retained.sqlite3"),
    )
    .unwrap();
    let db = f.rows(2);
    let detail = json!({"op":"approval_audit_detail","request_id":uuid::Uuid::new_v4().to_string(),
        "configuration":f.reader.configuration,"epoch":f.reader.epoch,"journal":first["journal"],"id":1});
    assert_eq!(f.send(&detail)["accepted"], false);
    let next = f.send(&f.query());
    assert_eq!(next["accepted"], true);
    assert_ne!(next["journal"], first["journal"]);
    assert_eq!(
        db.query_row("SELECT count(*) FROM attempts", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
}
