use super::*;
use std::cell::Cell;
use std::time::Instant;
use zerus_replica::{Inventory, NewTask, SessionKey, Target, TaskAttachment};

/// What `f` decoded and stamped.
fn counted<T>(f: impl FnOnce() -> T) -> (T, usize, usize) {
    DECODED.with(|n| n.set(0));
    STAMPED.with(|n| n.set(0));
    let out = f();
    (out, DECODED.with(Cell::get), STAMPED.with(Cell::get))
}

fn new_task(store: &ProjectStore, id: &str, stage: Stage, rank: &str, text: &str) -> TaskDoc {
    TaskDoc::create(NewTask {
        project: store.project_id(),
        id,
        text,
        stage,
        target: Target::Any,
        author: Actor::Person,
        rank,
        now: 1,
    })
    .unwrap()
}

fn agent() -> SessionKey {
    SessionKey {
        machine: "example-computer".into(),
        session: "codex/example/worker".into(),
        session_id: "run_id:example".into(),
    }
}

/// Dates a task's files an hour back, so they are settled: the bounded-work
/// tests count reads without waiting for `SETTLE_MS`.
fn age(store: &ProjectStore, uuid: &str) {
    let past = std::time::SystemTime::now() - Duration::from_secs(3600);
    for ext in ["loro", "updates", "managed"] {
        let path = store.dir().join("tasks").join(format!("{uuid}.{ext}"));
        if let Ok(file) = fs::File::options().write(true).open(path) {
            file.set_modified(past).unwrap();
        }
    }
}

/// Saves `task` through the store, then dates its files back.
fn aged_save(store: &ProjectStore, task: &mut TaskDoc) {
    store.save(task).unwrap();
    age(store, task.uuid());
}

/// Writes `task` as a whole snapshot, as an older hgs could, without the
/// change journal and without syncing; its file is dated back.
fn write_plain(store: &ProjectStore, task: &TaskDoc) {
    let path = store.dir().join("tasks").join(format!("{}.loro", task.uuid()));
    fs::write(path, task.snapshot().unwrap()).unwrap();
    age(store, task.uuid());
}

/// A project in every stage: equal ranks, a long multi-byte text, files,
/// agent starts, and one task file that does not load.
fn project(temp: &Path) -> ProjectStore {
    let store = ProjectStore::open(temp, "example").unwrap();
    let long = "é".repeat(TEXT_BYTES);
    for (n, (stage, rank)) in [
        (Stage::Todo, "V"),
        (Stage::Todo, "U"),
        (Stage::Todo, "U"),
        (Stage::Todo, "a"),
        (Stage::Todo, "W"),
        (Stage::Backlog, "V"),
        (Stage::Backlog, "U"),
    ]
    .into_iter()
    .enumerate()
    {
        let text = if n == 1 { long.clone() } else { format!("Task {n}") };
        let mut task = new_task(&store, &format!("T-aaaa{n:02}"), stage, rank, &text);
        if n == 3 {
            let file = TaskAttachment {
                name: "a.txt".into(),
                mime: "text/plain".into(),
                data_base64: "aGk=".into(),
                reference: "[File #1]".into(),
            };
            task.set_attachments(&[file], &Actor::Person, 2).unwrap();
        }
        store.save(&mut task).unwrap();
    }
    for (n, end) in [(10, Stage::Active), (11, Stage::Review), (12, Stage::Done), (13, Stage::Done)] {
        let mut task = new_task(&store, &format!("T-aaaa{n}"), Stage::Todo, "V", "Sent");
        task.record_send(&agent(), &format!("request-{n}"), &Actor::Agent(agent()), now())
            .unwrap();
        if end != Stage::Active {
            task.record_report("Result", &Actor::Agent(agent()), now()).unwrap();
        }
        if end == Stage::Done {
            task.set_stage(Stage::Done, &Actor::Person, now()).unwrap();
        }
        store.save(&mut task).unwrap();
    }
    let damaged = store.dir().join("tasks").join(format!("{}.loro", ids::new_uuid()));
    fs::write(damaged, b"not a task").unwrap();
    store
}

fn all_pages(store: &ProjectStore, stage: Stage, limit: usize) -> Vec<Value> {
    let mut index = Index::open(store).unwrap();
    let mut anchor = Anchor::Start;
    let mut rows = Vec::new();
    loop {
        let page = index.page(stage, limit, &anchor).unwrap();
        let documents = super::super::documents_page(store, stage, limit, &anchor, None).unwrap();
        assert_eq!(json!(page.rows), documents["tasks"], "{stage:?} {limit}");
        assert_eq!(json!(page.next), documents["page"]["next"]);
        assert_eq!(json!(page.previous), documents["page"]["previous"]);
        assert_eq!(json!(page.position), documents["page"]["position"]);
        assert_eq!(page.position, rows.len());
        rows.extend(page.rows);
        match page.next {
            Some(next) => {
                let (rank, uuid) = parse_cursor(store.project_id(), stage, &next).unwrap();
                anchor = Anchor::After(rank, uuid);
            }
            None => break,
        }
    }
    rows
}

#[test]
fn pages_hold_the_documents_rows_in_each_stage_s_order() {
    let temp = crate::host::staging::private_tempdir();
    let store = project(temp.path());
    let (tasks, damaged) = store.listing().unwrap();
    assert_eq!(damaged.len(), 1);
    for stage in [Stage::Review, Stage::Active, Stage::Todo, Stage::Backlog, Stage::Done] {
        let expected: Vec<Value> = tasks
            .iter()
            .filter(|t| t.stage().unwrap() == stage)
            .map(|t| capped(row(t.view().unwrap(), t.revision()).unwrap()))
            .collect();
        for limit in 1..=4 {
            assert_eq!(all_pages(&store, stage, limit), expected, "{stage:?} {limit}");
        }
    }
    let index = Index::open(&store).unwrap();
    let counts = index.counts().unwrap();
    assert_eq!(counts["todo"], 5);
    assert_eq!(counts["done"], 2);
    let (shown, total) = index.damaged().unwrap();
    assert_eq!((shown.len(), total), (1, 1));
    assert_eq!(shown[0]["uuid"], json!(damaged[0].uuid));
    assert_eq!(shown[0]["error"], json!(capped_error(&damaged[0].error)));
    // The long text is cut at a character boundary and marked.
    let long = tasks.iter().find(|t| t.id() == "T-aaaa01").unwrap();
    let listed = capped(row(long.view().unwrap(), long.revision()).unwrap());
    assert!(listed["text"].as_str().unwrap().len() <= TEXT_BYTES);
    assert_eq!(listed["text_truncated"], json!(true));
    // Lookups answer as the store does.
    for task in &tasks {
        let stage = task.stage().unwrap();
        let place = tasks.iter().filter(|t| t.stage().unwrap() == stage).position(|t| t.uuid() == task.uuid()).unwrap() + 1;
        assert_eq!(index.place(task.uuid()).unwrap(), Some(place));
        assert_eq!(index.short_id(&task.id()).unwrap(), (vec![task.uuid().to_owned()], true));
    }
    assert_eq!(index.short_id("T-zzzzzz").unwrap(), (vec![], true));
}

#[test]
fn pages_go_back_and_a_cursor_holds_its_place_as_tasks_arrive() {
    let temp = crate::host::staging::private_tempdir();
    let store = project(temp.path());
    let project = store.project_id().to_owned();
    let mut index = Index::open(&store).unwrap();
    let first = index.page(Stage::Todo, 2, &Anchor::Start).unwrap();
    let next = first.next.clone().unwrap();
    let (rank, uuid) = parse_cursor(&project, Stage::Todo, &next).unwrap();
    let second = index.page(Stage::Todo, 2, &Anchor::After(rank.clone(), uuid.clone())).unwrap();
    // Back from the second page is the first page again.
    let (rank0, uuid0) = parse_cursor(&project, Stage::Todo, second.previous.as_ref().unwrap()).unwrap();
    let back = index.page(Stage::Todo, 2, &Anchor::Before(rank0, uuid0)).unwrap();
    assert_eq!(back.rows, first.rows);
    assert_eq!(back.previous, None);
    drop(index);
    // A task added at the top does not shift the page after a cursor.
    let mut top = new_task(&store, "T-bbbbbb", Stage::Todo, "1", "New");
    store.save(&mut top).unwrap();
    let mut index = Index::open(&store).unwrap();
    let again = index.page(Stage::Todo, 2, &Anchor::After(rank, uuid)).unwrap();
    assert_eq!(again.rows, second.rows);
    assert_eq!(again.position, second.position + 1);
    // Cursors name their project and stage.
    assert!(parse_cursor(&project, Stage::Backlog, &next).is_err());
    assert!(parse_cursor("another", Stage::Todo, &next).is_err());
    for garbage in ["", "t1", "t1.x.y.z", &next.replace("t1.", "t2."), &format!("{next}.extra")] {
        assert!(parse_cursor(&project, Stage::Todo, garbage).is_err(), "{garbage}");
    }
    // Past either end: the nearest whole page, from the index and the documents alike.
    let all = index.page(Stage::Todo, 200, &Anchor::Start).unwrap();
    let key = |row: &Value| (row["rank"].as_str().unwrap().to_owned(), row["uuid"].as_str().unwrap().to_owned());
    let (rank, uuid) = key(all.rows.last().unwrap());
    let past = Anchor::After(rank, uuid);
    let page = index.page(Stage::Todo, 2, &past).unwrap();
    assert_eq!(page.rows, all.rows[all.rows.len() - 2..]);
    assert!(page.next.is_none() && page.previous.is_some());
    assert_eq!(page.position, all.rows.len() - 2);
    let documents = super::super::documents_page(&store, Stage::Todo, 2, &past, None).unwrap();
    assert_eq!(documents["tasks"], json!(page.rows));
    assert_eq!(documents["page"]["previous"], json!(page.previous));
    let (rank, uuid) = key(&all.rows[0]);
    let before = Anchor::Before(rank, uuid);
    let page = index.page(Stage::Todo, 2, &before).unwrap();
    assert_eq!(page.rows, all.rows[..2]);
    assert!(page.next.is_some() && page.previous.is_none() && page.position == 0);
    let documents = super::super::documents_page(&store, Stage::Todo, 2, &before, None).unwrap();
    assert_eq!(documents["tasks"], json!(page.rows));
    assert_eq!(documents["page"]["next"], json!(page.next));
    // A one-row stage clamps to that row; an empty stage has an empty page.
    let one = index.page(Stage::Review, 2, &Anchor::After("z".into(), ids::new_uuid())).unwrap();
    assert!(one.rows.len() == 1 && one.next.is_none() && one.previous.is_none());
    drop(index);
    let empty = crate::host::staging::private_tempdir();
    let empty = ProjectStore::open(empty.path(), "empty").unwrap();
    let mut index = Index::open(&empty).unwrap();
    let none = index.page(Stage::Todo, 2, &Anchor::After("V".into(), ids::new_uuid())).unwrap();
    assert!(none.rows.is_empty() && none.next.is_none() && none.previous.is_none() && none.position == 0);
    assert_eq!(index.counts().unwrap().get("todo"), None);
}

#[test]
fn reads_decode_only_what_changed() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let mut tasks: Vec<TaskDoc> = (0..300)
        .map(|n| new_task(&store, &format!("T-{n:06}"), Stage::Todo, "V", "Plain"))
        .collect();
    for task in &tasks {
        write_plain(&store, task);
    }
    let (_, decoded, _) = counted(|| Index::open(&store).unwrap());
    assert_eq!(decoded, 300, "the first read builds the index");
    // Nothing changed: no task is decoded or stamped.
    let (_, decoded, stamped) = counted(|| Index::open(&store).unwrap());
    assert_eq!((decoded, stamped), (0, 1), "only the task folder is stamped");
    // The index started the journal: a saved change is found through it,
    // stamping only the folder and that task's three files (snapshot,
    // updates and mark).
    aged_save(&store, &mut tasks[7]);
    let (_, decoded, stamped) = counted(|| Index::open(&store).unwrap());
    assert_eq!((decoded, stamped), (1, 4));
    tasks[7].comment("note", &Actor::Person, 5).unwrap();
    aged_save(&store, &mut tasks[7]);
    let (index, decoded, stamped) = counted(|| Index::open(&store).unwrap());
    assert_eq!((decoded, stamped), (1, 4), "the folder and the changed task's three files");
    let revision = index.revision().unwrap();
    drop(index);
    // A writer without the journal, in place: a page's rows are checked
    // before they are served, so the changed one is indexed again.
    tasks[0].set_title("Retitled", &Actor::Person, 6).unwrap();
    write_plain(&store, &tasks[0]);
    let mut index = Index::open(&store).unwrap();
    let (page, decoded, _) = counted(|| index.page(Stage::Todo, 300, &Anchor::Start).unwrap());
    assert_eq!(decoded, 1);
    let listed = page.rows.iter().find(|r| r["uuid"] == json!(tasks[0].uuid())).unwrap();
    assert_eq!(listed["title"], json!("Retitled"));
    assert_ne!(index.revision().unwrap(), revision);
    // ... and the periodic full check finds one outside any page.
    tasks[1].set_title("Elsewhere", &Actor::Person, 7).unwrap();
    write_plain(&store, &tasks[1]);
    index.db.execute("UPDATE meta SET value='0' WHERE key='checked'", []).unwrap();
    drop(index);
    let (_, decoded, stamped) = counted(|| Index::open(&store).unwrap());
    assert_eq!(decoded, 1);
    // Every file the folder lists is looked up; files it does not list are
    // not (zerus-cg5h.30).
    let listed = fs::read_dir(store.dir().join("tasks")).unwrap().count();
    assert!(listed > 300 && stamped >= listed && stamped < listed + 10, "{stamped} {listed}");
    // A new task file from such a writer changes the folder: a full check.
    let extra = new_task(&store, "T-extra1", Stage::Todo, "Z", "Late");
    write_plain(&store, &extra);
    let (index, decoded, _) = counted(|| Index::open(&store).unwrap());
    assert_eq!(decoded, 1);
    assert_eq!(index.counts().unwrap()["todo"], 301);
}

/// Windows has no inode and coarse file times: a rewrite of the same length
/// can keep its stamp. A task the journal names is indexed again anyway.
#[test]
fn a_task_the_journal_names_is_indexed_again_even_with_an_unchanged_stamp() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let mut task = new_task(&store, "T-aaaaaa", Stage::Todo, "V", "Plain");
    aged_save(&store, &mut task);
    let mut other = new_task(&store, "T-bbbbbb", Stage::Todo, "W", "Other");
    aged_save(&store, &mut other);
    drop(Index::open(&store).unwrap());
    task.set_title("Retitled", &Actor::Person, 5).unwrap();
    aged_save(&store, &mut task);
    // As if the rewritten files kept their stamps: the index already holds
    // the new stamps beside the old row. No index connection is open here.
    {
        let db = Connection::open(store.dir().join(FILE)).unwrap();
        db.execute(
            "UPDATE tasks SET files=?1 WHERE uuid=?2",
            params![files(store.dir(), task.uuid()), task.uuid()],
        )
        .unwrap();
    }
    let (mut index, decoded, _) = counted(|| Index::open(&store).unwrap());
    assert_eq!(decoded, 1, "the journal names it");
    let page = index.page(Stage::Todo, 10, &Anchor::Start).unwrap();
    let row = page.rows.iter().find(|r| r["uuid"] == json!(task.uuid())).unwrap();
    assert_eq!(row["title"], json!("Retitled"));
}

/// review-n on #1350: when the journal can no longer say what changed, a
/// current writer's change whose files kept their stamps is still found:
/// every row read before then is suspect, a page never serves one unread,
/// and each read reads a bounded batch of them again.
#[test]
fn an_unknown_journal_makes_every_earlier_row_suspect() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let mut tasks: Vec<TaskDoc> = (0..300)
        .map(|n| new_task(&store, &format!("T-{n:06}"), Stage::Todo, "V", "Plain"))
        .collect();
    for task in &mut tasks {
        aged_save(&store, task);
    }
    drop(Index::open(&store).unwrap());
    let (index, decoded, _) = counted(|| Index::open(&store).unwrap());
    assert_eq!(decoded, 0);
    let before = index.revision().unwrap();
    drop(index);
    // Nothing changed, but the journal is unknown: rows are read again in
    // batches, and the revision stays.
    fs::write(store.dir().join("changes"), b"damaged").unwrap();
    let (index, decoded, _) = counted(|| Index::open(&store).unwrap());
    assert_eq!(decoded, BATCH as usize);
    assert_eq!(index.revision().unwrap(), before, "the rows read the same");
    drop(index);
    let (index, decoded, _) = counted(|| Index::open(&store).unwrap());
    assert_eq!(decoded, 300 - BATCH as usize);
    assert_eq!(index.revision().unwrap(), before);
    drop(index);
    // Now a change whose files keep their stamps, then the journal is lost.
    tasks[250].set_title("Retitled", &Actor::Person, 9).unwrap();
    aged_save(&store, &mut tasks[250]);
    {
        let db = Connection::open(store.dir().join(FILE)).unwrap();
        db.execute(
            "UPDATE tasks SET files=?1 WHERE uuid=?2",
            params![files(store.dir(), tasks[250].uuid()), tasks[250].uuid()],
        )
        .unwrap();
    }
    fs::remove_file(store.dir().join("changes")).unwrap();
    let (mut index, decoded, _) = counted(|| Index::open(&store).unwrap());
    assert!(decoded <= BATCH as usize, "{decoded}");
    let page = index.page(Stage::Todo, 200, &Anchor::Start).unwrap();
    let all = index.page(Stage::Todo, 200, &Anchor::End).unwrap();
    let found = page.rows.iter().chain(&all.rows).find(|r| r["uuid"] == json!(tasks[250].uuid())).unwrap();
    assert_eq!(found["title"], json!("Retitled"));
    drop(index);
    let index = Index::open(&store).unwrap();
    let floor = index.floor(now()).unwrap();
    let suspect: i64 = index.db.query_row("SELECT count(*) FROM tasks WHERE checked < ?1 OR trusted = 0", [floor], |r| r.get(0)).unwrap();
    assert_eq!(suspect, 0, "every row was read again");
}

/// A row read longer ago than the maximum age is read again before a page
/// serves it, whatever its stamps; one that reads the same keeps the revision.
#[test]
fn rows_past_the_maximum_age_are_read_again() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let tasks: Vec<TaskDoc> = (0..20u8)
        .map(|n| new_task(&store, &format!("T-{n:06}"), Stage::Todo, "V", "Plain"))
        .collect();
    for task in &tasks {
        write_plain(&store, task);
    }
    let revision = Index::open(&store).unwrap().revision().unwrap();
    let aged = |sql: &str| {
        let db = Connection::open(store.dir().join(FILE)).unwrap();
        db.execute(sql, params![now() - MAX_AGE_MS - 1000]).unwrap();
    };
    aged("UPDATE tasks SET checked=?1");
    let (index, decoded, _) = counted(|| Index::open(&store).unwrap());
    assert_eq!(decoded, 20);
    assert_eq!(index.revision().unwrap(), revision);
    drop(index);
    // An unnoticed change on an aged row: the page reads it first.
    {
        let db = Connection::open(store.dir().join(FILE)).unwrap();
        db.execute("UPDATE tasks SET row=json_set(row, '$.text', 'Stale') WHERE uuid=?1", [tasks[5].uuid()]).unwrap();
    }
    let mut index = Index::open(&store).unwrap();
    {
        let db = Connection::open(store.dir().join(FILE)).unwrap();
        db.execute("UPDATE tasks SET checked=?1 WHERE uuid=?2", params![now() - MAX_AGE_MS - 1000, tasks[5].uuid()]).unwrap();
    }
    let (page, decoded, _) = counted(|| index.page(Stage::Todo, 50, &Anchor::Start).unwrap());
    assert_eq!(decoded, 1);
    let row = page.rows.iter().find(|r| r["uuid"] == json!(tasks[5].uuid())).unwrap();
    assert_eq!(row["text"], json!("Plain"));
}

/// A file modified within `SETTLE_MS` could change again in the same clock
/// tick: its row stays suspect and is read again.
#[test]
fn an_unsettled_row_is_read_again() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let mut task = new_task(&store, "T-aaaaaa", Stage::Todo, "V", "Plain");
    store.save(&mut task).unwrap();
    let (_, settled) = stamped(store.dir(), task.uuid(), now());
    assert!(!settled);
    assert!(stamped(store.dir(), task.uuid(), now() + SETTLE_MS + 1000).1);
    let index = Index::open(&store).unwrap();
    let trusted: bool = index.db.query_row("SELECT trusted FROM tasks WHERE uuid=?1", [task.uuid()], |r| r.get(0)).unwrap();
    assert!(!trusted, "kept suspect");
    drop(index);
    let (_, decoded, _) = counted(|| Index::open(&store).unwrap());
    assert_eq!(decoded, 1, "read again");
}

/// review-n on #1350: `--if-revision` answers only after the page's rows
/// were checked against their files.
#[test]
fn an_unchanged_revision_is_only_answered_after_the_page_check() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let mut tasks: Vec<TaskDoc> = (0..5)
        .map(|n| new_task(&store, &format!("T-{n:06}"), Stage::Todo, "V", "Plain"))
        .collect();
    for task in &tasks {
        write_plain(&store, task);
    }
    let mut index = Index::open(&store).unwrap();
    let first = super::super::indexed_page(&mut index, Stage::Todo, 10, &Anchor::Start, None).unwrap();
    let revision = first["revision"].as_str().unwrap().to_owned();
    let unchanged = super::super::indexed_page(&mut index, Stage::Todo, 10, &Anchor::Start, Some(&revision)).unwrap();
    assert_eq!(unchanged["unchanged"], json!(true));
    drop(index);
    // An older writer changes a task in place, without the journal.
    tasks[0].set_title("Retitled", &Actor::Person, 5).unwrap();
    write_plain(&store, &tasks[0]);
    let mut index = Index::open(&store).unwrap();
    let polled = super::super::indexed_page(&mut index, Stage::Todo, 10, &Anchor::Start, Some(&revision)).unwrap();
    assert!(polled.get("unchanged").is_none(), "{polled}");
    let row = polled["tasks"].as_array().unwrap().iter().find(|r| r["uuid"] == json!(tasks[0].uuid())).unwrap();
    assert_eq!(row["title"], json!("Retitled"));
}

/// review-n on #1350: a cursor's rank must be ASCII hex before it is cut.
#[test]
fn a_cursor_with_a_non_hex_rank_is_refused_without_panicking() {
    let uuid = ids::new_uuid();
    for rank in ["aéa", "zz", "4", "é0"] {
        let cursor = format!("t1.{}.{rank}.{uuid}", scope("example", Stage::Todo));
        assert!(parse_cursor("example", Stage::Todo, &cursor).is_err(), "{rank}");
    }
    let good = cursor("example", Stage::Todo, "V", &uuid);
    assert_eq!(parse_cursor("example", Stage::Todo, &good).unwrap(), ("V".to_owned(), uuid));
}

/// review-n on #1350 R2: a clock set back must not make earlier reads look
/// recent. Reads dated after now are suspect, and so is a suspect mark.
#[test]
fn a_clock_set_back_leaves_no_row_trusted_by_its_future_date() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let tasks: Vec<TaskDoc> = (0..20u8)
        .map(|n| new_task(&store, &format!("T-{n:06}"), Stage::Todo, "V", "Plain"))
        .collect();
    for task in &tasks {
        write_plain(&store, task);
    }
    drop(Index::open(&store).unwrap());
    // As if every read and mark happened an hour ahead of the clock now, and
    // one row changed unnoticed meanwhile.
    let ahead = now() + 3_600_000;
    {
        let db = Connection::open(store.dir().join(FILE)).unwrap();
        db.execute("UPDATE tasks SET checked=?1", [ahead]).unwrap();
        db.execute("INSERT OR REPLACE INTO meta VALUES('suspect', ?1)", [ahead.to_string()]).unwrap();
        db.execute("UPDATE meta SET value=?1 WHERE key='checked'", [ahead.to_string()]).unwrap();
        db.execute("UPDATE tasks SET row=json_set(row, '$.text', 'Stale') WHERE uuid=?1", [tasks[3].uuid()]).unwrap();
    }
    let (mut index, decoded, _) = counted(|| Index::open(&store).unwrap());
    assert_eq!(decoded, 20, "every future-dated read is read again");
    let page = index.page(Stage::Todo, 50, &Anchor::Start).unwrap();
    let row = page.rows.iter().find(|r| r["uuid"] == json!(tasks[3].uuid())).unwrap();
    assert_eq!(row["text"], json!("Plain"));
    let mark: i64 = index.meta("suspect").unwrap().unwrap().parse().unwrap();
    assert!(mark <= now(), "the suspect mark is not left in the future");
    // And it settles: the next read decodes nothing.
    drop(index);
    let (_, decoded, _) = counted(|| Index::open(&store).unwrap());
    assert_eq!(decoded, 0);
}

/// review-n on #1350 R2: the size limit is in bytes, whatever the page size.
#[test]
fn the_index_size_limit_is_in_bytes_whatever_its_page_size() {
    assert_eq!(budget(4096, 10), Some(65_536));
    assert_eq!(budget(8192, 10), Some(32_768));
    assert_eq!(budget(8192, 40_000), None, "over the budget");
    assert_eq!(budget(0, 1), None);
    assert_eq!(budget(-4096, 1), None);
    let temp = crate::host::staging::private_tempdir();
    let store = project(temp.path());
    drop(Index::open(&store).unwrap());
    {
        let db = Connection::open(store.dir().join(FILE)).unwrap();
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA page_size=8192; VACUUM;").unwrap();
        let size: i64 = db.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
        assert_eq!(size, 8192);
    }
    let index = Index::open(&store).unwrap();
    let pages: i64 = index.db.query_row("PRAGMA max_page_count", [], |r| r.get(0)).unwrap();
    assert_eq!(pages * 8192, BUDGET_BYTES);
}

/// review-n's starvation finding on #1350 R2: the background queue is
/// ordered by when each row was last read, apart from whether it is trusted.
/// Rows that stay unsettled (here, files dated in the future) go to the back
/// each time they are read, so an expired row whose stage changed unnoticed
/// is reached, and its stage count and membership become right.
#[test]
fn permanently_unsettled_rows_cannot_starve_an_expired_row() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let mut tasks: Vec<TaskDoc> = (0..300)
        .map(|n| new_task(&store, &format!("T-{n:06}"), Stage::Backlog, "V", "Plain"))
        .collect();
    for task in &tasks {
        write_plain(&store, task);
    }
    drop(Index::open(&store).unwrap());
    // 260 tasks whose files are dated a day ahead: never settled.
    let ahead = std::time::SystemTime::now() + Duration::from_secs(86_400);
    for task in &tasks[..260] {
        let path = store.dir().join("tasks").join(format!("{}.loro", task.uuid()));
        fs::File::options().write(true).open(path).unwrap().set_modified(ahead).unwrap();
    }
    // The last one moves to To do; its new files keep the stamps the index holds.
    let moved = 299;
    tasks[moved].set_stage(Stage::Todo, &Actor::Person, 5).unwrap();
    write_plain(&store, &tasks[moved]);
    {
        let db = Connection::open(store.dir().join(FILE)).unwrap();
        for task in &tasks[..260] {
            db.execute("UPDATE tasks SET files=?1, checked=0 WHERE uuid=?2", params![files(store.dir(), task.uuid()), task.uuid()]).unwrap();
        }
        db.execute(
            "UPDATE tasks SET files=?1, checked=?2 WHERE uuid=?3",
            params![files(store.dir(), tasks[moved].uuid()), now() - MAX_AGE_MS - 5_000, tasks[moved].uuid()],
        )
        .unwrap();
    }
    let mut seen = false;
    for read in 0..3 {
        let mut index = Index::open(&store).unwrap();
        let page = index.page(Stage::Todo, 10, &Anchor::Start).unwrap();
        let count = index.counts().unwrap().get("todo").copied().unwrap_or(0);
        if page.rows.len() == 1 && count == 1 {
            assert_eq!(page.rows[0]["uuid"], json!(tasks[moved].uuid()));
            seen = true;
            assert!(read <= 1, "reached within ceil(261 / 256) reads, not {read}");
            break;
        }
    }
    assert!(seen, "the moved task never reached To do");
}

/// review-n on #1350 R3: a cached row that is not the object the index
/// wrote for that task is a damaged index, never served or labelled.
#[test]
fn a_cached_row_of_another_shape_is_damage() {
    let temp = crate::host::staging::private_tempdir();
    let store = project(temp.path());
    // Settled files: the page serves its cached rows without reading them again.
    for uuid in store.uuids().unwrap() {
        age(&store, &uuid);
    }
    let good = Index::open(&store).unwrap().page(Stage::Todo, 200, &Anchor::Start).unwrap().rows;
    let first = good[0]["uuid"].as_str().unwrap().to_owned();
    let other = good[1]["uuid"].as_str().unwrap().to_owned();
    for raw in [
        "true".to_owned(),
        "\"text\"".to_owned(),
        "[1, 2]".to_owned(),
        "{}".to_owned(),
        "not json".to_owned(),
        json!({"uuid": other, "stage": "todo", "id": "T-aaaa00"}).to_string(),
        json!({"uuid": first, "stage": "backlog", "id": "T-aaaa00"}).to_string(),
    ]
    .into_iter()
    .chain(
        // The real row with one identity field changed: review-n's R4 probe.
        [
            ("id", None),
            ("id", Some(json!(7))),
            ("id", Some(good[1]["id"].clone())),
            ("rank", Some(json!("zzz"))),
            ("uuid", Some(json!(other))),
        ]
        .into_iter()
        .map(|(field, value)| {
            let mut row = Index::open(&store).unwrap().page(Stage::Todo, 200, &Anchor::Start).unwrap().rows[0].clone();
            match value {
                Some(value) => row[field] = value,
                None => drop(row.as_object_mut().unwrap().remove(field)),
            }
            row.to_string()
        }),
    ) {
        let mut index = Index::open(&store).unwrap();
        index.db.execute("UPDATE tasks SET row=?1 WHERE uuid=?2", params![raw, first]).unwrap();
        assert!(matches!(index.page(Stage::Todo, 200, &Anchor::Start), Err(Failure::Damaged)), "{raw}");
        let polled = super::super::indexed_page(&mut index, Stage::Todo, 200, &Anchor::Start, None);
        assert!(matches!(polled, Err(Failure::Damaged)), "{raw}");
        index.discard(&Failure::Damaged);
        // Built again from the documents.
        assert_eq!(Index::open(&store).unwrap().page(Stage::Todo, 200, &Anchor::Start).unwrap().rows, good, "{raw}");
    }
}

/// The same for a cached history: a task the journal names is read again
/// even when its files kept their stamps.
#[test]
fn a_changed_task_s_history_is_read_again_even_with_an_unchanged_stamp() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let mut task = new_task(&store, "T-aaaaaa", Stage::Todo, "V", "Plain");
    aged_save(&store, &mut task);
    let history = |store: &ProjectStore, uuid: &str| super::super::history::page(store, uuid, 80, None, false).unwrap();
    history(&store, task.uuid());
    task.comment("later", &Actor::Person, 5).unwrap();
    aged_save(&store, &mut task);
    {
        let db = Connection::open(store.dir().join(FILE)).unwrap();
        db.execute(
            "UPDATE histories SET files=?1 WHERE uuid=?2",
            params![files(store.dir(), task.uuid()), task.uuid()],
        )
        .unwrap();
    }
    let page = history(&store, task.uuid());
    assert!(page["log"].as_array().unwrap().iter().any(|e| e["text"] == json!("later")), "{page}");
}

/// review-n on #1360: a cached history is held to the same trust rules as a
/// page's rows. With other suspect rows ahead in the background queue, an
/// expired or future-dated requested task is read again before its history
/// is served, even when an older writer's change kept its stamps.
#[test]
fn a_requested_history_is_revalidated_like_a_page_row() {
    for future in [false, true] {
        let temp = crate::host::staging::private_tempdir();
        let store = ProjectStore::open(temp.path(), "example").unwrap();
        let mut tasks: Vec<TaskDoc> = (0..300)
            .map(|n| new_task(&store, &format!("T-{n:06}"), Stage::Todo, "V", "Plain"))
            .collect();
        for task in &tasks {
            write_plain(&store, task);
        }
        let target = 299;
        let history = |store: &ProjectStore, uuid: &str| super::super::history::page(store, uuid, 80, None, false).unwrap();
        history(&store, tasks[target].uuid());
        // An older writer comments without the journal; its files keep the
        // stamps the index holds, for the row and for the cached history.
        tasks[target].comment("newer", &Actor::Person, 9).unwrap();
        write_plain(&store, &tasks[target]);
        let new_files = files(store.dir(), tasks[target].uuid());
        {
            let db = Connection::open(store.dir().join(FILE)).unwrap();
            db.execute("UPDATE tasks SET files=?1 WHERE uuid=?2", params![new_files, tasks[target].uuid()]).unwrap();
            db.execute("UPDATE histories SET files=?1 WHERE uuid=?2", params![new_files, tasks[target].uuid()]).unwrap();
            // 256 other rows wait ahead of it in the suspect queue.
            let long_ago = now() - 5 * MAX_AGE_MS;
            for task in &tasks[..256] {
                db.execute("UPDATE tasks SET checked=?1 WHERE uuid=?2", params![long_ago, task.uuid()]).unwrap();
            }
            let requested = if future { now() + 3_600_000 } else { now() - MAX_AGE_MS - 60_000 };
            db.execute("UPDATE tasks SET checked=?1 WHERE uuid=?2", params![requested, tasks[target].uuid()]).unwrap();
        }
        let page = history(&store, tasks[target].uuid());
        let texts: Vec<&str> = page["log"].as_array().unwrap().iter().filter_map(|e| e["text"].as_str()).collect();
        assert!(texts.contains(&"newer"), "future={future}: {texts:?}");
    }
}

/// The tasks window (zerus-cg5h.2 slice C): the first page of every stage,
/// the same from the index and from the documents read once.
#[cfg(unix)]
#[test]
fn a_window_is_the_first_page_of_every_stage_from_the_index_or_the_documents() {
    use std::os::unix::fs::PermissionsExt;
    let temp = crate::host::staging::private_tempdir();
    let store = project(temp.path());
    let indexed = super::super::window_pages(&store, 2).unwrap();
    assert_eq!(indexed.len(), 5);
    for (stage, page) in super::super::LIST_STAGES.iter().zip(&indexed) {
        let expected = super::super::documents_page(&store, *stage, 2, &Anchor::Start, None).unwrap();
        assert_eq!(page["tasks"], expected["tasks"], "{stage:?}");
        assert_eq!(page["page"]["total"], expected["page"]["total"], "{stage:?}");
        assert_eq!(page["page"]["next"], expected["page"]["next"], "{stage:?}");
    }
    // The folder is not private: the index cannot be used, the documents can.
    fs::set_permissions(store.dir(), fs::Permissions::from_mode(0o755)).unwrap();
    let read = super::super::window_pages(&store, 2).unwrap();
    fs::set_permissions(store.dir(), fs::Permissions::from_mode(0o700)).unwrap();
    for (page, from_documents) in indexed.iter().zip(&read) {
        assert_eq!(page["tasks"], from_documents["tasks"]);
        assert_eq!(page["page"], from_documents["page"]);
        assert_eq!(page["counts"], from_documents["counts"]);
    }
    assert!(read[0]["revision"].as_str().unwrap().starts_with("documents-"));
}

#[test]
fn a_window_keeps_a_task_a_later_page_moves_to_an_earlier_stage() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let mut task = new_task(&store, "T-aaaaaa", Stage::Todo, "V", "Plain");
    aged_save(&store, &mut task);
    let initial = super::super::window_pages(&store, 1).unwrap();
    assert_eq!(initial[2]["tasks"].as_array().unwrap().len(), 1);
    // An older hgs moves the cached To do task to Review before the next
    // window, without a journal note: only the To do page's check finds it.
    task.record_send(&agent(), "window-request", &Actor::Agent(agent()), now()).unwrap();
    task.record_report("Done; please review", &Actor::Agent(agent()), now() + 1).unwrap();
    write_plain(&store, &task);
    let pages = super::super::window_pages(&store, 1).unwrap();
    let expected = super::super::documents_page(&store, Stage::Review, 1, &Anchor::Start, None).unwrap();
    assert_eq!(pages[0]["tasks"], expected["tasks"]);
    assert_eq!(pages[0]["page"]["total"], json!(1));
    assert!(pages.iter().all(|p| p["revision"] == pages[0]["revision"] && p["counts"] == pages[0]["counts"]));
    let shown: usize = pages.iter().map(|p| p["tasks"].as_array().unwrap().len()).sum();
    assert_eq!(shown, 1, "the task is in exactly one stage of the window");
}

#[test]
fn a_window_is_read_again_until_its_pages_share_one_revision() {
    let pages = |revisions: &[&str]| -> Vec<Value> { revisions.iter().map(|r| json!({"revision": r})).collect() };
    let mut passes = vec![pages(&["a", "a", "b", "b", "b"]), pages(&["b"; 5])].into_iter();
    let mut reads = 0;
    let window = super::super::coherent_window(|| {
        reads += 1;
        passes.next().ok_or(())
    });
    assert_eq!((window, reads), (Ok(Some(pages(&["b"; 5]))), 2));
    // A window that keeps changing is not returned: the documents answer.
    let mut reads = 0;
    let window = super::super::coherent_window(|| {
        reads += 1;
        Ok::<_, ()>(pages(&[&reads.to_string(), "next", "next", "next", "next"]))
    });
    assert_eq!((window, reads), (Ok(None), super::super::WINDOW_ATTEMPTS));
    // A failed pass is not repeated.
    let mut reads = 0;
    let window = super::super::coherent_window(|| {
        reads += 1;
        Err::<Vec<Value>, _>("failed")
    });
    assert_eq!((window, reads), (Err("failed"), 1));
}

#[test]
fn a_damaged_foreign_or_shared_index_is_built_again() {
    let temp = crate::host::staging::private_tempdir();
    let store = project(temp.path());
    let path = store.dir().join(FILE);
    let expected = |store: &ProjectStore| {
        let mut index = Index::open(store).expect("the index is usable again");
        index.page(Stage::Todo, 200, &Anchor::Start).unwrap().rows
    };
    let good = expected(&store);
    // Not a database.
    fs::write(&path, b"garbage that is not SQLite").unwrap();
    for suffix in ["-wal", "-shm"] {
        let _ = fs::remove_file(store.dir().join(format!("{FILE}{suffix}")));
    }
    assert_eq!(expected(&store), good);
    // Another schema version, and a missing table.
    {
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA user_version=9").unwrap();
    }
    assert_eq!(expected(&store), good);
    {
        let db = Connection::open(&path).unwrap();
        db.execute_batch("DROP TABLE tasks").unwrap();
    }
    assert_eq!(expected(&store), good);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Readable by others: removed and built again owner-only.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(expected(&store), good);
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        // A link in its place is removed; what it names is never touched.
        let outside = temp.path().join("outside");
        fs::write(&outside, b"kept").unwrap();
        for suffix in ["", "-wal", "-shm"] {
            let _ = fs::remove_file(store.dir().join(format!("{FILE}{suffix}")));
        }
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert_eq!(expected(&store), good);
        assert_eq!(fs::read(&outside).unwrap(), b"kept");
        assert!(!fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
    }
}

#[test]
fn queries_use_their_indexes() {
    let temp = crate::host::staging::private_tempdir();
    let store = project(temp.path());
    let index = Index::open(&store).unwrap();
    let plan = |sql: &str| -> String {
        let mut query = index.db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        query
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .map(|r| r.unwrap())
            .collect::<Vec<_>>()
            .join("; ")
    };
    for (sql, wanted) in [
        ("SELECT uuid, rank, files, row FROM tasks WHERE stage='todo' AND (rank, uuid) > ('V', 'u') ORDER BY rank, uuid LIMIT 3", "tasks_order"),
        ("SELECT uuid, rank, files, row FROM tasks WHERE stage='todo' AND (rank, uuid) < ('V', 'u') ORDER BY rank DESC, uuid DESC LIMIT 3", "tasks_order"),
        ("SELECT count(*) FROM tasks WHERE stage='todo' AND (rank, uuid) < ('V', 'u')", "tasks_order"),
        ("SELECT uuid FROM tasks WHERE short_id='T-aaaa01'", "tasks_short"),
        ("SELECT count(*) FROM sends WHERE at >= 5", "sends_at"),
        ("SELECT id, payload FROM entries WHERE uuid='u' AND (at, ordinal) < (5, 1) ORDER BY at DESC, ordinal DESC LIMIT 3", "entries_place"),
        ("SELECT at, ordinal FROM entries WHERE uuid='u' AND id='1@2'", "entries_id"),
    ] {
        let found = plan(sql);
        assert!(found.contains(wanted) && !found.contains("TEMP B-TREE"), "{sql}: {found}");
    }
}

/// zerus-iiee: a second index open in this process never closes a raw
/// descriptor of the live file, so the live connection keeps its locks.
#[cfg(unix)]
#[test]
fn opening_the_index_again_keeps_a_live_index_s_locks() {
    let temp = crate::host::staging::private_tempdir();
    let store = project(temp.path());
    let path = store.dir().join(FILE);
    let live = Index::open(&store).unwrap();
    live.db.execute_batch("BEGIN").unwrap();
    assert_eq!(live.counts().unwrap()["todo"], 5);
    assert!(crate::sqlite_file::foreign_lock_refused(&path), "the live index holds its lock");
    for _ in 0..2 {
        let again = Index::open(&store).expect("a second index opens beside a reader");
        assert_eq!(again.counts().unwrap()["todo"], 5);
        drop(again);
        assert!(crate::sqlite_file::foreign_lock_refused(&path), "opening again kept the lock");
    }
    drop(fs::File::open(&path).unwrap());
    assert!(
        !crate::sqlite_file::foreign_lock_refused(&path),
        "the control: a closed raw descriptor dropped the lock"
    );
    live.db.execute_batch("COMMIT").unwrap();
}

/// A fixture of `count` To do tasks written directly, then indexed once.
fn scaled(temp: &Path, count: usize) -> ProjectStore {
    let store = ProjectStore::open(temp, "example").unwrap();
    for (n, rank) in zerus_replica::rank::spread(count).iter().enumerate() {
        write_plain(&store, &new_task(&store, &format!("T-{n:06}"), Stage::Todo, rank, "Plain"));
    }
    store
}

/// Holds the index's clock at a time until dropped.
struct HeldClock;
impl HeldClock {
    fn at(at: i64) -> Self {
        CLOCK.with(|clock| clock.set(Some(at)));
        Self
    }
}
impl Drop for HeldClock {
    fn drop(&mut self) {
        CLOCK.with(|clock| clock.set(None));
    }
}

#[test]
fn a_large_project_pages_and_polls_with_bounded_work() {
    let temp = crate::host::staging::private_tempdir();
    let store = scaled(temp.path(), 5_000);
    // The build and the polls read one time, past the settling of the
    // fixture's writes: on a loaded host the build can outlast FULL_CHECK_MS,
    // and the poll after it would then rightly be a full check, not the quiet
    // one this test is for (zerus-iezgx). That full check is pinned last.
    let held = now() + SETTLE_MS + 1;
    let _clock = HeldClock::at(held);
    let (_, decoded, _) = counted(|| Index::open(&store).unwrap());
    assert_eq!(decoded, 5_000);
    // A poll with nothing changed: one stamp, no task decoded.
    let (index, decoded, stamped) = counted(|| Index::open(&store).unwrap());
    assert_eq!((decoded, stamped), (0, 1));
    let revision = index.revision().unwrap();
    drop(index);
    // A page reads its rows and checks only their files.
    let mut index = Index::open(&store).unwrap();
    let (page, decoded, stamped) = counted(|| index.page(Stage::Todo, 50, &Anchor::Start).unwrap());
    assert_eq!((page.rows.len(), decoded, stamped), (50, 0, 150));
    assert_eq!(index.revision().unwrap(), revision);
    let deep = page.next.unwrap();
    let (rank, uuid) = parse_cursor(store.project_id(), Stage::Todo, &deep).unwrap();
    let (page, decoded, _) = counted(|| index.page(Stage::Todo, 50, &Anchor::After(rank, uuid)).unwrap());
    assert_eq!((page.position, decoded), (50, 0));
    drop(index);
    // A full-check period later, the poll checks every task file again.
    let _clock = HeldClock::at(held + FULL_CHECK_MS);
    let (_, decoded, stamped) = counted(|| Index::open(&store).unwrap());
    assert_eq!((decoded, stamped), (0, 5_002));
}

/// design-tima's scale smoke: run it in release, with
/// `cargo test --release --bin hgs -- --ignored --exact tasks::index::tests::twenty_thousand_tasks_scale_smoke --nocapture`.
#[test]
#[ignore = "scale smoke; run explicitly in release"]
fn twenty_thousand_tasks_scale_smoke() {
    let temp = crate::host::staging::private_tempdir();
    let started = Instant::now();
    let store = scaled(temp.path(), 20_000);
    let fixture = started.elapsed();
    let started = Instant::now();
    let (_, decoded, _) = counted(|| Index::open(&store).unwrap());
    let build = started.elapsed();
    assert_eq!(decoded, 20_000);
    let revision = {
        let mut index = Index::open(&store).unwrap();
        super::super::indexed_page(&mut index, Stage::Todo, 50, &Anchor::Start, None).unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let mut polls = Vec::new();
    for _ in 0..20 {
        // `--if-revision`: open, check the page's rows, answer unchanged.
        let started = Instant::now();
        let (answer, decoded, stamped) = counted(|| {
            let mut index = Index::open(&store).unwrap();
            super::super::indexed_page(&mut index, Stage::Todo, 50, &Anchor::Start, Some(&revision)).unwrap()
        });
        polls.push(started.elapsed());
        assert_eq!(answer["unchanged"], json!(true));
        assert_eq!((decoded, stamped), (0, 151), "the poll stamps the folder and its page");
    }
    let started = Instant::now();
    let mut index = Index::open(&store).unwrap();
    let (page, decoded, stamped) = counted(|| index.page(Stage::Todo, 50, &Anchor::Start).unwrap());
    let first_page = started.elapsed();
    assert_eq!((page.rows.len(), decoded, stamped), (50, 0, 150));
    let counts = index.counts().unwrap();
    assert_eq!(counts["todo"], 20_000);
    drop(index);
    let started = Instant::now();
    let legacy = store.listing().unwrap().0.len();
    let full_decode = started.elapsed();
    assert_eq!(legacy, 20_000);
    polls.sort();
    println!(
        "scale smoke 20000 tasks: fixture {fixture:?}, index build {build:?}, poll median {:?} max {:?}, first page {first_page:?}, legacy full decode {full_decode:?}",
        polls[polls.len() / 2],
        polls[polls.len() - 1]
    );
    assert!(first_page < full_decode / 10, "{first_page:?} vs {full_decode:?}");
}

/// review-n on #1415 (R5): a save that fails after its mark advanced (the
/// append is refused) leaves the copy kept, and a warm index shows it as
/// damaged, never as its old healthy cached row.
#[cfg(unix)]
#[test]
fn a_mark_only_change_reaches_the_warm_listing() {
    use std::os::unix::fs::PermissionsExt;
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let mut task = new_task(&store, "T-aaaaaa", Stage::Todo, "V", "Plain");
    assert!(task.floor().unwrap());
    aged_save(&store, &mut task);
    let uuid = task.uuid().to_owned();
    drop(Index::open(&store).unwrap());
    let updates = store.dir().join("tasks").join(format!("{uuid}.updates"));
    fs::set_permissions(&updates, fs::Permissions::from_mode(0o400)).unwrap();
    task.set_title("Never appended", &Actor::Person, 5).unwrap();
    let failed = store.save(&mut task).is_err();
    fs::set_permissions(&updates, fs::Permissions::from_mode(0o600)).unwrap();
    // A writer that ignores permissions cannot show this case.
    if !failed {
        return;
    }
    assert!(store.load(&uuid).is_err());
    assert_eq!(store.tasks().unwrap().1.len(), 1);
    let mut index = Index::open(&store).unwrap();
    let page = index.page(Stage::Todo, 10, &Anchor::Start).unwrap();
    assert!(page.rows.iter().all(|r| r["uuid"] != json!(uuid)), "{:?}", page.rows);
    let (shown, total) = index.damaged().unwrap();
    assert_eq!(total, 1);
    assert_eq!(shown[0]["uuid"], json!(uuid));
}

/// A mark changed without a journal note (a writer that does not journal)
/// is still found: the task's stamps include its mark.
#[test]
fn a_mark_changed_outside_the_journal_is_found_by_its_stamp() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let mut task = new_task(&store, "T-aaaaaa", Stage::Todo, "V", "Plain");
    assert!(task.floor().unwrap());
    aged_save(&store, &mut task);
    let uuid = task.uuid().to_owned();
    drop(Index::open(&store).unwrap());
    let tasks = store.dir().join("tasks");
    let path = tasks.join(format!("{uuid}.managed"));
    let files = |ext: &str| fs::read(tasks.join(format!("{uuid}.{ext}"))).unwrap();
    let (snapshot, updates) = (files("loro"), files("updates"));
    let before = stamp(&tasks.join(format!("{uuid}.loro"))) + &stamp(&tasks.join(format!("{uuid}.updates")));
    let mut mark: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for counter in mark["saved"].as_object_mut().unwrap().values_mut() {
        *counter = json!(counter.as_i64().unwrap() + 1);
    }
    // Replaced as this store replaces it, with no journal note; only the
    // mark is dated back, so the snapshot and updates keep their stamps.
    let temporary = tasks.join("mark-outside-the-journal");
    fs::write(&temporary, serde_json::to_vec(&mark).unwrap()).unwrap();
    fs::rename(&temporary, &path).unwrap();
    let past = std::time::SystemTime::now() - Duration::from_secs(3600);
    fs::File::options().write(true).open(&path).unwrap().set_modified(past).unwrap();
    assert_eq!((files("loro"), files("updates")), (snapshot, updates));
    assert_eq!(
        stamp(&tasks.join(format!("{uuid}.loro"))) + &stamp(&tasks.join(format!("{uuid}.updates"))),
        before
    );
    assert!(store.load(&uuid).is_err());
    let mut index = Index::open(&store).unwrap();
    let page = index.page(Stage::Todo, 10, &Anchor::Start).unwrap();
    assert!(page.rows.iter().all(|r| r["uuid"] != json!(uuid)), "{:?}", page.rows);
    assert_eq!(index.damaged().unwrap().1, 1);
}

/// review-n on #1415 (R6): a task never cut over whose files are removed
/// after the index was warm drops its row; one with only its mark left
/// stays listed as damaged.
#[test]
fn a_removed_task_drops_its_cache_row_and_a_mark_alone_stays_damaged() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let mut plain = new_task(&store, "T-aaaaaa", Stage::Todo, "V", "Plain");
    aged_save(&store, &mut plain);
    let mut floored = new_task(&store, "T-bbbbbb", Stage::Todo, "W", "Floored");
    assert!(floored.floor().unwrap());
    aged_save(&store, &mut floored);
    drop(Index::open(&store).unwrap());
    assert!(gone(&files(store.dir(), "no-such-task")));
    assert!(!gone(&files(store.dir(), plain.uuid())));
    for task in [&plain, &floored] {
        for ext in ["loro", "updates"] {
            let _ = fs::remove_file(store.dir().join("tasks").join(format!("{}.{ext}", task.uuid())));
        }
    }
    assert!(store.tasks().unwrap().1.iter().all(|d| d.uuid != plain.uuid()));
    let mut index = Index::open(&store).unwrap();
    let (shown, total) = index.damaged().unwrap();
    assert_eq!(total, 1, "{shown:?}");
    assert_eq!(shown[0]["uuid"], json!(floored.uuid()));
    let page = index.page(Stage::Todo, 10, &Anchor::Start).unwrap();
    assert!(page.rows.is_empty(), "{:?}", page.rows);
}

/// desktop-u G1 on #1415 (R7): an index made under the rule before the
/// mark's seal remembers a task with an unsealed mark as a healthy row. This
/// build's index version names the mark format, so that index is built again
/// and the task is listed as damaged; the unsealed mark is left as it was.
#[test]
fn an_index_from_before_the_seal_is_built_again() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let mut task = new_task(&store, "T-aaaaaa", Stage::Todo, "V", "Floored");
    assert!(task.floor().unwrap());
    aged_save(&store, &mut task);
    let uuid = task.uuid().to_owned();
    drop(Index::open(&store).unwrap());
    // The mark as R5 wrote it, and the index as R5 left it: a healthy row
    // under the files' current stamps, at the version before the seal.
    let path = store.dir().join("tasks").join(format!("{uuid}.managed"));
    let mut mark: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    mark.as_object_mut().unwrap().remove("seal");
    let unsealed = serde_json::to_vec(&mark).unwrap();
    fs::write(&path, &unsealed).unwrap();
    age(&store, &uuid);
    assert!(store.load(&uuid).is_err());
    {
        let db = Connection::open(store.dir().join(FILE)).unwrap();
        db.execute("UPDATE tasks SET files=?1 WHERE uuid=?2", params![files(store.dir(), &uuid), uuid])
            .unwrap();
        db.execute_batch(&format!("PRAGMA user_version={ROWS};")).unwrap();
    }
    let mut index = Index::open(&store).unwrap();
    let page = index.page(Stage::Todo, 10, &Anchor::Start).unwrap();
    assert!(page.rows.iter().all(|r| r["uuid"] != json!(uuid)), "{:?}", page.rows);
    assert_eq!(index.damaged().unwrap().1, 1);
    assert_eq!(fs::read(&path).unwrap(), unsealed);
}

fn session(machine: &str, name: &str, id: &str) -> SessionKey {
    SessionKey { machine: machine.into(), session: name.into(), session_id: id.into() }
}

fn targeted(store: &ProjectStore, id: &str, stage: Stage, rank: &str, target: Target) -> TaskDoc {
    let mut task = TaskDoc::create(NewTask {
        project: store.project_id(),
        id,
        text: id,
        stage,
        target,
        author: Actor::Person,
        rank,
        now: 1,
    })
    .unwrap();
    aged_save(store, &mut task);
    task
}

/// Every page of `stage` for `scope`, from the index and from the documents,
/// which must agree page by page.
fn scoped_pages(store: &ProjectStore, stage: Stage, limit: usize, scope: &Scope) -> Vec<String> {
    let mut index = Index::open(store).unwrap();
    let mut anchor = Anchor::Start;
    let mut ids = Vec::new();
    loop {
        let page = index.page_in(stage, limit, &anchor, Some(scope)).unwrap();
        let documents = super::super::documents_page_in(store, stage, limit, &anchor, None, Some(scope)).unwrap();
        assert_eq!(json!(page.rows), documents["tasks"], "{stage:?} {limit}");
        assert_eq!(json!(page.next), documents["page"]["next"]);
        assert_eq!(json!(page.previous), documents["page"]["previous"]);
        assert_eq!(json!(page.position), documents["page"]["position"]);
        assert_eq!(page.position, ids.len());
        ids.extend(page.rows.iter().map(|row| row["id"].as_str().unwrap().to_owned()));
        match page.next {
            Some(next) => {
                let (rank, uuid) = parse_cursor(store.project_id(), stage, &next).unwrap();
                anchor = Anchor::After(rank, uuid);
            }
            None => break,
        }
    }
    ids
}

#[test]
fn a_session_reads_exactly_its_own_tasks_even_past_the_first_page() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let mine = session("computer-a", "codex/example/same", "launch_id:mine");
    // Another session with the same name on the same computer stays separate.
    let twin = session("computer-a", "codex/example/same", "launch_id:twin");
    let elsewhere = session("computer-b", "codex/example/same", "launch_id:mine");
    for n in 0..6 {
        targeted(&store, &format!("T-bbbb0{n}"), Stage::Todo, &format!("B{}", n + 1), Target::Session(twin.clone()));
    }
    targeted(&store, "T-cccc01", Stage::Todo, "C1", Target::Session(elsewhere.clone()));
    targeted(&store, "T-aaaa01", Stage::Todo, "D1", Target::Any);
    targeted(&store, "T-aaaa02", Stage::Todo, "E1", Target::Session(mine.clone()));
    targeted(&store, "T-aaaa03", Stage::Todo, "F1", Target::Subagent { parent: mine.clone() });
    targeted(&store, "T-aaaa04", Stage::Backlog, "A1", Target::Session(mine.clone()));
    let mut running = targeted(&store, "T-aaaa05", Stage::Todo, "G1", Target::Any);
    running.record_send(&mine, "request-mine", &Actor::Agent(mine.clone()), now()).unwrap();
    aged_save(&store, &mut running);
    let mut other = targeted(&store, "T-bbbb10", Stage::Todo, "G2", Target::Session(mine.clone()));
    other.record_send(&twin, "request-twin", &Actor::Agent(twin.clone()), now()).unwrap();
    aged_save(&store, &mut other);
    // The session's To do task sits past the first unfiltered page of two.
    let mut index = Index::open(&store).unwrap();
    let first = index.page(Stage::Todo, 2, &Anchor::Start).unwrap();
    assert!(first.rows.iter().all(|row| row["id"] != "T-aaaa02"));
    drop(index);
    let scope = Scope::new("computer-a", "launch_id:mine");
    for limit in 1..=4 {
        // Unassigned tasks, the session's own targets and subagent plans; never the twin's.
        assert_eq!(scoped_pages(&store, Stage::Todo, limit, &scope), ["T-aaaa01", "T-aaaa02", "T-aaaa03"], "{limit}");
        assert_eq!(scoped_pages(&store, Stage::Backlog, limit, &scope), ["T-aaaa04"]);
        // A running task belongs to its executor, whatever its target said.
        assert_eq!(scoped_pages(&store, Stage::Active, limit, &scope), ["T-aaaa05"]);
    }
    let twins = Scope::new("computer-a", "launch_id:twin");
    assert_eq!(scoped_pages(&store, Stage::Todo, 3, &twins).len(), 7, "the twin's six and the unassigned task");
    assert_eq!(scoped_pages(&store, Stage::Active, 3, &twins), ["T-bbbb10"]);
    let index = Index::open(&store).unwrap();
    let counts = index.counts_in(Some(&scope)).unwrap();
    assert_eq!((counts["todo"], counts["backlog"], counts["active"]), (3, 1, 1));
    assert_eq!(index.counts_in(None).unwrap(), index.counts().unwrap());
}

#[test]
fn a_session_cursor_holds_its_place_as_tasks_arrive() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let mine = session("computer-a", "codex/example/worker", "launch_id:mine");
    for n in 0..4 {
        targeted(&store, &format!("T-aaaa0{n}"), Stage::Todo, &format!("M{}", n + 1), Target::Session(mine.clone()));
    }
    let scope = Scope::new("computer-a", "launch_id:mine");
    let mut index = Index::open(&store).unwrap();
    let first = index.page_in(Stage::Todo, 2, &Anchor::Start, Some(&scope)).unwrap();
    let next = first.next.clone().unwrap();
    drop(index);
    // New tasks before and after the cursor, for this session and others.
    targeted(&store, "T-bbbb01", Stage::Todo, "A1", Target::Session(mine.clone()));
    targeted(&store, "T-bbbb02", Stage::Todo, "M15", Target::Session(session("computer-a", "codex/example/worker", "launch_id:other")));
    targeted(&store, "T-bbbb03", Stage::Todo, "Z1", Target::Session(mine.clone()));
    let (rank, uuid) = parse_cursor(store.project_id(), Stage::Todo, &next).unwrap();
    let mut index = Index::open(&store).unwrap();
    let second = index.page_in(Stage::Todo, 2, &Anchor::After(rank, uuid), Some(&scope)).unwrap();
    let ids: Vec<&str> = second.rows.iter().map(|row| row["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["T-aaaa02", "T-aaaa03"]);
    assert_eq!(second.position, 3, "the task added above shifts the position, never the rows");
    let third = second.next.clone().unwrap();
    let (rank, uuid) = parse_cursor(store.project_id(), Stage::Todo, &third).unwrap();
    let last = index.page_in(Stage::Todo, 2, &Anchor::After(rank, uuid), Some(&scope)).unwrap();
    assert_eq!(last.rows.iter().map(|row| row["id"].as_str().unwrap()).collect::<Vec<_>>(), ["T-bbbb03"]);
    assert!(last.next.is_none());
}

#[test]
fn scoped_queries_use_their_indexes() {
    let temp = crate::host::staging::private_tempdir();
    let store = project(temp.path());
    let index = Index::open(&store).unwrap();
    let plan = |sql: &str| -> String {
        let mut query = index.db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        query.query_map([], |r| r.get::<_, String>(3)).unwrap().map(|r| r.unwrap()).collect::<Vec<_>>().join("; ")
    };
    for (sql, wanted) in [
        ("SELECT uuid, rank, files, row FROM tasks WHERE owner='m\nid' AND stage='todo' AND (rank, uuid) > ('V', 'u') ORDER BY rank, uuid LIMIT 3", "tasks_owner"),
        ("SELECT uuid, rank, files, row FROM tasks WHERE owner='m\nid' AND stage='todo' AND (rank, uuid) < ('V', 'u') ORDER BY rank DESC, uuid DESC LIMIT 3", "tasks_owner"),
        ("SELECT uuid, rank, files, row FROM tasks WHERE unassigned=1 AND stage='todo' AND (rank, uuid) > ('V', 'u') ORDER BY rank, uuid LIMIT 3", "tasks_unassigned"),
        ("SELECT count(*) FROM tasks WHERE owner='m\nid' AND stage='todo' AND (rank, uuid) < ('V', 'u')", "tasks_owner"),
        ("SELECT count(*) FROM tasks WHERE unassigned=1 AND stage='todo' AND (rank, uuid) < ('V', 'u')", "tasks_unassigned"),
    ] {
        let found = plan(sql);
        assert!(found.contains(wanted) && !found.contains("TEMP B-TREE"), "{sql}: {found}");
    }
}

#[test]
fn an_older_hgs_index_is_never_touched() {
    // An older hgs keeps its own task-index-v1.sqlite3 (ROWS 4) and rebuilds
    // any index not of its schema. This index lives in another file, so the
    // two never rebuild each other's while both binaries are in use.
    let temp = crate::host::staging::private_tempdir();
    let store = project(temp.path());
    let old = store.dir().join("task-index-v1.sqlite3");
    fs::write(&old, b"an older hgs's index").unwrap();
    let mut index = Index::open(&store).unwrap();
    index.page(Stage::Todo, 2, &Anchor::Start).unwrap();
    drop(index);
    assert_eq!(fs::read(&old).unwrap(), b"an older hgs's index");
    assert!(store.dir().join(FILE).exists());
    assert_ne!(FILE, "task-index-v1.sqlite3");
}

/// What the index holds for the phone's inventory (zerus-cg5h.21): the
/// project document's version, and each loaded task's version by whether it
/// is Done.
fn held(index: &Index) -> (Option<zerus_replica::Version>, Inventory, Inventory) {
    let project = index.meta("project_version").unwrap().map(|v| serde_json::from_str(&v).unwrap());
    let (mut live, mut done) = (Inventory::default(), Inventory::default());
    let mut query = index.db.prepare("SELECT uuid, version, done FROM tasks WHERE version IS NOT NULL").unwrap();
    let rows = query
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?)))
        .unwrap();
    for row in rows {
        let (uuid, version, is_done) = row.unwrap();
        let into = if is_done { &mut done } else { &mut live };
        into.tasks.insert(uuid, serde_json::from_str(&version).unwrap());
    }
    (project, live, done)
}

/// The index's inventory against the store's own: the live view, and every
/// loaded task (Done ones included).
fn assert_holds_the_inventory(store: &ProjectStore, index: &Index) {
    let (project, live, done) = held(index);
    let (expected_live, _) = store.live_inventory().unwrap();
    let (everything, _) = store.inventory().unwrap();
    assert_eq!(project.as_ref(), Some(&expected_live.project));
    assert_eq!(live.tasks, expected_live.tasks);
    let mut all = live.tasks.clone();
    all.extend(done.tasks);
    assert_eq!(all, everything.tasks);
}

#[test]
fn the_index_holds_the_inventory_of_every_loaded_task_and_the_project() {
    let temp = crate::host::staging::private_tempdir();
    let store = project(temp.path());
    let index = Index::open(&store).unwrap();
    // Before the project document was ever saved its version is empty.
    assert_eq!(held(&index).0, Some(Default::default()));
    assert_holds_the_inventory(&store, &index);
    // The damaged task file holds no version.
    let damaged: i64 = index
        .db
        .query_row("SELECT count(*) FROM tasks WHERE error IS NOT NULL AND version IS NULL", [], |r| r.get(0))
        .unwrap();
    assert_eq!(damaged, 1);
    drop(index);
    // Saves through the store: a note, a task completed, the project's settings.
    // The project's damaged file has a random UUID: take loaded tasks only.
    let uuids: Vec<String> = store.uuids().unwrap().into_iter().filter(|uuid| store.load(uuid).is_ok()).collect();
    let mut noted = store.load(&uuids[0]).unwrap();
    noted.comment("More detail", &Actor::Person, 5).unwrap();
    store.save(&mut noted).unwrap();
    let mut finished = uuids
        .iter()
        .map(|uuid| store.load(uuid))
        .filter_map(std::result::Result::ok)
        .find(|task| task.stage().unwrap() == Stage::Review)
        .unwrap();
    finished.set_stage(Stage::Done, &Actor::Person, now()).unwrap();
    store.save(&mut finished).unwrap();
    let mut doc = store.project().unwrap();
    doc.set_settings(&zerus_replica::Settings { agent_limit: 3, ..doc.settings() }).unwrap();
    store.save_project(&doc).unwrap();
    let index = Index::open(&store).unwrap();
    assert!(!held(&index).0.unwrap().is_empty());
    assert_holds_the_inventory(&store, &index);
    let revision = index.revision().unwrap();
    drop(index);
    // A project write without the journal is found by the full check; it
    // leaves the task rows, and with them the revision, alone.
    doc.set_settings(&zerus_replica::Settings { agent_limit: 4, ..doc.settings() }).unwrap();
    fs::write(store.dir().join("project.loro"), doc.snapshot().unwrap()).unwrap();
    let index = Index::open(&store).unwrap();
    index.db.execute("UPDATE meta SET value='0' WHERE key='checked'", []).unwrap();
    drop(index);
    let index = Index::open(&store).unwrap();
    assert_holds_the_inventory(&store, &index);
    assert_eq!(index.revision().unwrap(), revision);
}

#[test]
fn the_phone_s_inventories_come_from_the_index_without_reading_every_task() {
    let temp = crate::host::staging::private_tempdir();
    let store = project(temp.path());
    for uuid in store.uuids().unwrap() {
        age(&store, &uuid);
    }
    let mut index = Index::open(&store).unwrap();
    assert_eq!(index.live_inventory().unwrap(), store.live_inventory().unwrap().0);
    // A phone holding a Done task and one this computer never had.
    let (all, _) = store.inventory().unwrap();
    let done = all.tasks.keys().find(|uuid| !store.live_inventory().unwrap().0.tasks.contains_key(*uuid)).unwrap();
    let mut theirs = Inventory::default();
    theirs.tasks.insert(done.clone(), Default::default());
    theirs.tasks.insert(ids::new_uuid(), Default::default());
    assert_eq!(index.phone_inventory(&theirs).unwrap(), store.phone_inventory(&theirs).unwrap());
    assert!(index.phone_inventory(&theirs).unwrap().tasks.contains_key(done));
    // A quiet read decodes no task and stamps only the task folder.
    let (_, decoded, stamped) = counted(|| index.live_inventory().unwrap());
    assert_eq!((decoded, stamped), (0, 1));
    // A save through the store is in the next read: only that task is decoded.
    let uuid = store.live_inventory().unwrap().0.tasks.keys().next().unwrap().clone();
    let mut task = store.load(&uuid).unwrap();
    task.comment("From the phone's side", &Actor::Person, 9).unwrap();
    aged_save(&store, &mut task);
    let (live, decoded, _) = counted(|| index.live_inventory().unwrap());
    assert_eq!(decoded, 1);
    assert_eq!(live, store.live_inventory().unwrap().0);
    assert_eq!(live.digest(), store.live_inventory().unwrap().0.digest());
}

/// A task cut over to the execution schema whose stored schema an older
/// replica's merged upgrade lowered, saved as such: the store's inventory
/// repairs it before reading its version (zerus-cg5h.21, review-win-e).
fn unrepaired(store: &ProjectStore) -> TaskDoc {
    unrepaired_as(store, "T-repair")
}

fn unrepaired_as(store: &ProjectStore, id: &str) -> TaskDoc {
    unrepaired_ranked(store, id, "V")
}

fn unrepaired_ranked(store: &ProjectStore, id: &str, rank: &str) -> TaskDoc {
    let mut task = new_task(store, id, Stage::Todo, rank, "Repair me");
    assert!(task.floor().unwrap());
    task.lower_stored_schema_for_test(5);
    assert!(task.needs_schema_repair());
    store.save(&mut task).unwrap();
    assert!(store.load(task.uuid()).unwrap().needs_schema_repair());
    task
}

#[test]
fn the_index_repairs_a_task_before_it_holds_its_version() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let stale = unrepaired(&store);
    let index = Index::open(&store).unwrap();
    // The index's read repaired and saved it, as the store's inventory does.
    let saved = store.load(stale.uuid()).unwrap();
    assert!(!saved.needs_schema_repair());
    let (_, live, _) = held(&index);
    assert_eq!(live.tasks[stale.uuid()], saved.inventory_version());
    assert_ne!(live.tasks[stale.uuid()], stale.inventory_version());
    assert_holds_the_inventory(&store, &index);
    // Its stamps are those of the repaired files.
    let files: String = index.db.query_row("SELECT files FROM tasks WHERE uuid=?1", [stale.uuid()], |r| r.get(0)).unwrap();
    assert_eq!(files, stamped(store.dir(), stale.uuid(), now()).0);
}

#[test]
fn rows_an_older_build_held_are_read_again_with_the_repair() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let stale = unrepaired(&store);
    // A warm index of the build before this rule (#1724): rows read
    // without the repair, and no mark of the rule.
    LEGACY_DERIVE.with(|legacy| legacy.set(true));
    let index = Index::open(&store).unwrap();
    LEGACY_DERIVE.with(|legacy| legacy.set(false));
    assert_eq!(held(&index).1.tasks[stale.uuid()], stale.inventory_version());
    index.db.execute("DELETE FROM meta WHERE key = 'derived'", []).unwrap();
    drop(index);
    let index = Index::open(&store).unwrap();
    assert!(!store.load(stale.uuid()).unwrap().needs_schema_repair());
    assert_holds_the_inventory(&store, &index);
}

#[test]
fn a_row_an_older_build_writes_later_is_read_again_with_the_repair() {
    // review-win-e R2 on #1736: this build's index is current, then an older
    // build sharing the file reads a task without the repair and stores its
    // row; the next read here must not serve that row.
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    drop(Index::open(&store).unwrap());
    let stale = unrepaired(&store);
    age(&store, stale.uuid());
    LEGACY_DERIVE.with(|legacy| legacy.set(true));
    let index = Index::open(&store).unwrap();
    LEGACY_DERIVE.with(|legacy| legacy.set(false));
    assert_eq!(held(&index).1.tasks[stale.uuid()], stale.inventory_version());
    drop(index);
    let mut index = Index::open(&store).unwrap();
    assert_eq!(index.live_inventory().unwrap(), store.live_inventory().unwrap().0);
    assert_holds_the_inventory(&store, &index);
    // Once the repaired files settle, a read is quiet again: nothing is
    // decoded and only the folder is stamped.
    age(&store, stale.uuid());
    index.live_inventory().unwrap();
    let (_, decoded, stamped) = counted(|| index.live_inventory().unwrap());
    assert_eq!((decoded, stamped), (0, 1));
}

#[test]
fn rows_an_older_build_stored_on_both_sides_of_the_last_read_here_are_all_read_again() {
    // review-win-e on #1736 R3 (zerus-cg5h.21.2): an older build whose
    // clock was set back and then caught up stores unrepaired rows dated
    // before and after this build's last write. No clock may decide which
    // are read again: the index's inventory is not used until every row was.
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    drop(Index::open(&store).unwrap());
    let (early, late) = (unrepaired_as(&store, "T-early1"), unrepaired_as(&store, "T-late01"));
    age(&store, early.uuid());
    age(&store, late.uuid());
    LEGACY_DERIVE.with(|legacy| legacy.set(true));
    let index = Index::open(&store).unwrap();
    LEGACY_DERIVE.with(|legacy| legacy.set(false));
    let since: i64 = index.meta("derived_at").unwrap().unwrap().parse().unwrap();
    index.db.execute("UPDATE tasks SET checked = ?1 WHERE uuid = ?2", params![since - 1_000, early.uuid()]).unwrap();
    drop(index);
    let mut index = Index::open(&store).unwrap();
    let held = index.live_inventory();
    let expected = store.live_inventory().unwrap().0;
    // Either not used (the phone reads the store) or already exact.
    if let Ok(inventory) = held {
        assert_eq!(inventory, expected);
    }
    // Once every row was read again here, the index answers, exactly.
    for _ in 0..4 {
        index.live_inventory().ok();
    }
    assert_eq!(index.live_inventory().unwrap(), store.live_inventory().unwrap().0);
    assert!(!store.load(early.uuid()).unwrap().needs_schema_repair());
}

#[test]
fn after_another_build_s_write_the_index_answers_only_once_every_row_was_read_again() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    for n in 0..600 {
        write_plain(&store, &new_task(&store, &format!("T-{n:06}"), Stage::Todo, "V", "Plain"));
    }
    drop(Index::open(&store).unwrap());
    let stale = unrepaired(&store);
    age(&store, stale.uuid());
    LEGACY_DERIVE.with(|legacy| legacy.set(true));
    drop(Index::open(&store).unwrap());
    LEGACY_DERIVE.with(|legacy| legacy.set(false));
    let mut index = Index::open(&store).unwrap();
    // 601 rows, at most 256 read again per read (the open was one): the
    // store answers until every row was read here again.
    assert!(matches!(index.live_inventory(), Err(Failure::Other)));
    let inventory = index.live_inventory().unwrap();
    assert_eq!(inventory, store.live_inventory().unwrap().0);
    assert_eq!(inventory.tasks.len(), 601);
}

#[test]
fn an_older_build_reading_an_unrepaired_row_again_never_vouches_for_it() {
    // review-win-e on #1748: an older build's read of the same row again
    // (only its stamps and read time change, so no new revision) must not
    // count as this build's repaired read of it.
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    for n in 0..600 {
        write_plain(&store, &new_task(&store, &format!("T-{n:06}"), Stage::Todo, "V", "Plain"));
    }
    drop(Index::open(&store).unwrap());
    let early = unrepaired_ranked(&store, "T-early1", "U");
    let late = unrepaired_as(&store, "T-late01");
    age(&store, early.uuid());
    age(&store, late.uuid());
    let stale = early.inventory_version();
    // An older build stores both unrepaired; its clock dates `early` before
    // this build's last write and the good rows earlier still.
    LEGACY_DERIVE.with(|legacy| legacy.set(true));
    let index = Index::open(&store).unwrap();
    LEGACY_DERIVE.with(|legacy| legacy.set(false));
    let since: i64 = index.meta("derived_at").unwrap().unwrap().parse().unwrap();
    index.db.execute("UPDATE tasks SET checked = ?1 WHERE uuid != ?2", params![since - 2_000, late.uuid()]).unwrap();
    index.db.execute("UPDATE tasks SET checked = ?1 WHERE uuid = ?2", params![since - 1_000, early.uuid()]).unwrap();
    drop(index);
    // This build sees the change and reads a first batch again.
    drop(Index::open(&store).unwrap());
    // The older build pages To do: `early` (rank U, first) is read again, unrepaired.
    LEGACY_DERIVE.with(|legacy| legacy.set(true));
    {
        let mut index = Index::open(&store).unwrap();
        index.page(Stage::Todo, 1, &Anchor::Start).unwrap();
    }
    LEGACY_DERIVE.with(|legacy| legacy.set(false));
    // This build never serves `early` unrepaired, and answers once it repaired it.
    let mut index = Index::open(&store).unwrap();
    let mut answered = None;
    for _ in 0..6 {
        if let Ok(inventory) = index.live_inventory() {
            assert_ne!(inventory.tasks[early.uuid()], stale, "an unrepaired row was served");
            answered = Some(inventory);
        }
    }
    assert_eq!(answered.unwrap(), store.live_inventory().unwrap().0);
}

#[test]
fn an_index_from_before_the_repair_epoch_gains_it_in_place_and_reads_every_row_again() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    for n in 0..300 {
        write_plain(&store, &new_task(&store, &format!("T-{n:06}"), Stage::Todo, "V", "Plain"));
    }
    let index = Index::open(&store).unwrap();
    // The file as #1736 and #1748 left it: no epoch column, nothing pending.
    index.db.execute_batch("ALTER TABLE tasks DROP COLUMN repaired; DELETE FROM meta WHERE key = 'uncertified';").unwrap();
    let shape = |index: &Index| -> (i64, i64) {
        let version: i64 = index.db.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        let objects: i64 = index.db.query_row("SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'", [], |r| r.get(0)).unwrap();
        (version, objects)
    };
    let before = shape(&index);
    drop(index);
    let mut index = Index::open(&store).unwrap();
    // Builds before the epoch keep the file: same user version and objects.
    assert_eq!(shape(&index), before);
    // 300 rows: the open read 256 of them again; the rest are pending, and
    // the next read finishes them and answers exactly.
    let pending = |index: &Index| -> i64 {
        index.db.query_row("SELECT count(*) FROM tasks WHERE repaired IS NULL", [], |r| r.get(0)).unwrap()
    };
    assert_eq!(pending(&index), 44);
    assert!(index.meta("uncertified").unwrap().is_some());
    assert_eq!(index.live_inventory().unwrap(), store.live_inventory().unwrap().0);
    assert_eq!(pending(&index), 0);
    assert!(index.meta("uncertified").unwrap().is_none());
}

#[test]
fn an_older_repair_build_cannot_mask_an_unrepaired_row_from_the_epoch() {
    // review-win-d on #1753: a #1736/#1748 build repairs some rows itself and
    // marks "derived_revision" current, after an older build stored
    // unrepaired rows; that must not hide them from this build's epoch.
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    for n in 0..1000 {
        write_plain(&store, &new_task(&store, &format!("T-{n:06}"), Stage::Todo, "V", "Plain"));
    }
    drop(Index::open(&store).unwrap());
    let early = unrepaired_ranked(&store, "T-early1", "U");
    let late = unrepaired_as(&store, "T-late01");
    age(&store, early.uuid());
    age(&store, late.uuid());
    let stale = early.inventory_version();
    // A #1724 build stores both unrepaired, with a clock straddling this
    // build's last write.
    LEGACY_DERIVE.with(|legacy| legacy.set(true));
    let index = Index::open(&store).unwrap();
    LEGACY_DERIVE.with(|legacy| legacy.set(false));
    let since: i64 = index.meta("derived_at").unwrap().unwrap().parse().unwrap();
    index.db.execute("UPDATE tasks SET checked = ?1 WHERE uuid != ?2", params![since - 2_000, late.uuid()]).unwrap();
    index.db.execute("UPDATE tasks SET checked = ?1 WHERE uuid = ?2", params![since - 1_000, early.uuid()]).unwrap();
    drop(index);
    // A #1736/#1748 build repairs `late` and marks its own proof current.
    PROOF_WRITER.with(|proof| proof.set(true));
    let index = Index::open(&store).unwrap();
    PROOF_WRITER.with(|proof| proof.set(false));
    assert!(store.load(early.uuid()).unwrap().needs_schema_repair(), "the scenario needs `early` left unrepaired");
    assert_eq!(index.meta("revision").unwrap(), index.meta("derived_revision").unwrap());
    drop(index);
    let mut index = Index::open(&store).unwrap();
    let mut answered = None;
    for _ in 0..8 {
        if let Ok(inventory) = index.live_inventory() {
            assert_ne!(inventory.tasks[early.uuid()], stale, "an unrepaired row was served");
            answered = Some(inventory);
        }
    }
    assert_eq!(answered.unwrap(), store.live_inventory().unwrap().0);
}

/// A task an add places through the task index, as `hgs task add` does
/// (zerus-cg5h.24).
fn add_placed(store: &ProjectStore, position: &Position) -> TaskDoc {
    let mut index = Index::open(store).unwrap();
    index.check_files().unwrap();
    let placement = IndexPlacement::new(&index);
    let mut task = store
        .new_task_placed(Some(&placement), "Placed", Stage::Todo, Target::Any, Actor::Person, position, "example", 1)
        .unwrap();
    drop(index);
    store.save(&mut task).unwrap();
    task
}

#[test]
fn the_index_places_a_task_as_the_documents_do_reading_only_its_neighbours() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    for n in 0..60 {
        write_plain(&store, &new_task(&store, &format!("T-{n:06}"), Stage::Todo, &format!("V{}", n % 7 + 1), "Plain"));
    }
    let index = Index::open(&store).unwrap();
    let placement = IndexPlacement::new(&index);
    use zerus_replica::Placement as _;
    let order = placement.order(Stage::Todo).unwrap().unwrap();
    let documents: Vec<(String, String, String)> =
        store.ordered(Stage::Todo).unwrap().iter().map(|t| (t.uuid().to_owned(), t.id(), t.rank())).collect();
    assert_eq!(order.iter().map(|p| (p.uuid.clone(), p.id.clone(), p.rank.clone())).collect::<Vec<_>>(), documents);
    let list = store.ordered(Stage::Todo).unwrap();
    for position in [Position::Top, Position::End, Position::Index(30), Position::Before(list[10].id()), Position::After(list[20].id())] {
        let expected = store.rank_for(Stage::Todo, &position, None).unwrap();
        let before = zerus_replica::load_count_for_test();
        let task = store
            .new_task_placed(Some(&placement), "Placed", Stage::Todo, Target::Any, Actor::Person, &position, "example", 1)
            .unwrap();
        assert!(zerus_replica::load_count_for_test() - before <= 2, "{position:?}");
        assert_eq!(task.rank(), expected, "{position:?}");
    }
}

#[test]
fn a_short_id_an_older_build_took_without_the_journal_is_never_reused() {
    // design-tima2's control for zerus-cg5h.24: an older build adds a task
    // without the journal; the next add must not draw its short ID again.
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    write_plain(&store, &new_task(&store, "T-000001", Stage::Todo, "V", "Plain"));
    drop(Index::open(&store).unwrap());
    // The older build's task file only: no journal note, no index row; and a
    // save by this build in the same window, so the journal is not quiet.
    write_plain(&store, &new_task(&store, "T-taken1", Stage::Todo, "W", "Older"));
    let mut busy = new_task(&store, "T-busy01", Stage::Todo, "Y", "Saved here");
    store.save(&mut busy).unwrap();
    zerus_replica::ids::force_short_ids_for_test(&["T-taken1", "T-fresh1"]);
    assert_eq!(add_placed(&store, &Position::End).id(), "T-fresh1");
    // The same when that build also stored its row in the index: the index
    // is then uncertified and the whole write reads the documents.
    write_plain(&store, &new_task(&store, "T-taken2", Stage::Todo, "X", "Older again"));
    LEGACY_DERIVE.with(|legacy| legacy.set(true));
    drop(Index::open(&store).unwrap());
    LEGACY_DERIVE.with(|legacy| legacy.set(false));
    zerus_replica::ids::force_short_ids_for_test(&["T-taken2", "T-fresh2"]);
    let before = zerus_replica::load_count_for_test();
    assert_eq!(add_placed(&store, &Position::End).id(), "T-fresh2");
    assert!(zerus_replica::load_count_for_test() - before >= 4, "the documents were read");
}

#[test]
fn an_uncertified_or_damaged_index_leaves_the_whole_write_to_the_documents() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    for n in 0..20 {
        write_plain(&store, &new_task(&store, &format!("T-{n:06}"), Stage::Todo, "V", "Plain"));
    }
    let index = Index::open(&store).unwrap();
    use zerus_replica::Placement as _;
    assert!(IndexPlacement::new(&index).order(Stage::Todo).unwrap().is_some());
    index.db.execute("INSERT OR REPLACE INTO meta VALUES('uncertified', '1')", []).unwrap();
    let placement = IndexPlacement::new(&index);
    assert!(placement.order(Stage::Todo).unwrap().is_none());
    assert!(placement.short_id_used("T-000001").unwrap().is_none());
    let before = zerus_replica::load_count_for_test();
    store
        .new_task_placed(Some(&placement), "Placed", Stage::Todo, Target::Any, Actor::Person, &Position::Index(5), "example", 1)
        .unwrap();
    assert!(zerus_replica::load_count_for_test() - before >= 20);
    drop(index);
    // A damaged task file: its document may name a short ID the index lacks.
    fs::write(store.dir().join("tasks").join(format!("{}.loro", ids::new_uuid())), b"not a task").unwrap();
    let index = Index::open(&store).unwrap();
    assert!(IndexPlacement::new(&index).order(Stage::Todo).unwrap().is_none());
}


#[test]
fn an_older_journalless_move_never_places_from_a_stale_stage() {
    // review-win-d on #1767 (zerus-j0os4): an older writer moves an existing
    // task without the journal while a save here keeps the journal busy;
    // the next add must still go after the actual last task.
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let a = new_task(&store, "T-000001", Stage::Todo, "V", "A");
    let mut b = new_task(&store, "T-000002", Stage::Todo, "U", "B");
    write_plain(&store, &a);
    write_plain(&store, &b);
    drop(Index::open(&store).unwrap());
    b.set_rank("z").unwrap();
    write_plain(&store, &b);
    let mut a = store.load(a.uuid()).unwrap();
    a.comment("Saved here", &Actor::Person, 2).unwrap();
    store.save(&mut a).unwrap();
    let mut index = Index::open(&store).unwrap();
    index.check_files().unwrap();
    let expected = store.rank_for(Stage::Todo, &Position::End, None).unwrap();
    let placement = IndexPlacement::new(&index);
    let task = store
        .new_task_placed(Some(&placement), "New", Stage::Todo, Target::Any, Actor::Person, &Position::End, "example", 3)
        .unwrap();
    assert_eq!(task.rank(), expected, "the next add must still go after the actual last task");
}

#[test]
fn task_file_checks_never_postpone_a_due_project_version_refresh() {
    // review-win-d on #1767 (zerus-k2388): checking only task files before a
    // placement must not count as the periodic check, which also reads the
    // project document again.
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let mut doc = store.project().unwrap();
    doc.set_settings(&zerus_replica::Settings { agent_limit: 3, ..doc.settings() }).unwrap();
    store.save_project(&doc).unwrap();
    let mut index = Index::open(&store).unwrap();
    assert_holds_the_inventory(&store, &index);
    // An older writer updates the project without the journal.
    doc.set_settings(&zerus_replica::Settings { agent_limit: 4, ..doc.settings() }).unwrap();
    fs::write(store.dir().join("project.loro"), doc.snapshot().unwrap()).unwrap();
    // The periodic check is due; a task-file check alone must not satisfy it.
    index.db.execute("UPDATE meta SET value='0' WHERE key='checked'", []).unwrap();
    index.check_files().unwrap();
    let actual = index.live_inventory().unwrap();
    assert_eq!(actual, store.live_inventory().unwrap().0, "task-only checking renewed the project-version clock");
}

/// Sends, reports and accepts a new task through `session` at time `at`.
fn accepted(store: &ProjectStore, id: &str, session: &SessionKey, at: i64) -> TaskDoc {
    let mut task = new_task(store, id, Stage::Todo, "V", &format!("Done {id}"));
    task.record_send(session, &format!("request-{id}"), &Actor::Agent(session.clone()), at).unwrap();
    task.record_report("Result", &Actor::Agent(session.clone()), at).unwrap();
    task.set_stage(Stage::Done, &Actor::Person, at).unwrap();
    aged_save(store, &mut task);
    task
}

#[test]
fn the_phone_s_done_page_comes_from_the_index_as_from_the_documents() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let api = SessionKey { machine: "m1".into(), session: "codex/api".into(), session_id: "launch_id:api".into() };
    let web = SessionKey { machine: "m1".into(), session: "codex/web".into(), session_id: "launch_id:web".into() };
    for n in 0..30 {
        let session = if n % 3 == 0 { &web } else { &api };
        accepted(&store, &format!("T-d{n:05}"), session, 1_000 + (n % 7) * 100);
    }
    write_plain(&store, &new_task(&store, "T-live01", Stage::Todo, "V", "Still to do"));
    let mut index = Index::open(&store).unwrap();
    let named_only = SessionKey { session_id: String::new(), ..api.clone() };
    for session in [None, Some(&api), Some(&named_only), Some(&web)] {
        for limit in [1, 7, 50] {
            let mut before = None;
            loop {
                let entries = index.done_entries().unwrap().unwrap();
                let page = store.done_page_of(entries, before.clone(), limit, 1_300, session).unwrap();
                assert_eq!(page, store.done_page(before.clone(), limit, 1_300, session).unwrap(), "{session:?} {limit}");
                before = page.next.clone();
                if before.is_none() {
                    break;
                }
            }
        }
    }
    // Choosing the page decodes no task: it stamps each task file the folder
    // lists once (for writers without the journal), without the store's
    // inventory cache; the page reads only its own documents.
    let (_, decoded, stamped) = counted(|| index.done_entries().unwrap().unwrap());
    assert_eq!((decoded, stamped), (0, fs::read_dir(store.dir().join("tasks")).unwrap().count()));
    let entries = index.done_entries().unwrap().unwrap();
    let loads = zerus_replica::load_count_for_test();
    store.done_page_of(entries, None, 7, 1_300, None).unwrap();
    assert_eq!(zerus_replica::load_count_for_test() - loads, 7);
    // Uncertified or holding a damaged task: the store answers.
    index.db.execute("INSERT OR REPLACE INTO meta VALUES('uncertified', '1')", []).unwrap();
    assert!(index.done_entries().unwrap().is_none());
    drop(index);
    fs::write(store.dir().join("tasks").join(format!("{}.loro", ids::new_uuid())), b"not a task").unwrap();
    let mut index = Index::open(&store).unwrap();
    assert!(index.done_entries().unwrap().is_none());
}

#[test]
fn a_done_row_without_session_metadata_declines_instead_of_hiding_the_task() {
    // review-win-d on #1780 (zerus-7w9zj): a cached row without its session
    // property is not a task sent to no session; the index declines.
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let session = SessionKey { machine: "m1".into(), session: "codex/api".into(), session_id: "launch_id:api".into() };
    let task = accepted(&store, "T-d00001", &session, 1_000);
    let mut index = Index::open(&store).unwrap();
    let raw: String = index.db.query_row("SELECT row FROM tasks WHERE uuid=?1", [task.uuid()], |r| r.get(0)).unwrap();
    let mut row: Value = serde_json::from_str(&raw).unwrap();
    assert!(row["session"].is_object());
    row.as_object_mut().unwrap().remove("session");
    assert_eq!(index.db.execute("UPDATE tasks SET row=?1 WHERE uuid=?2", params![row.to_string(), task.uuid()]).unwrap(), 1);
    let page = match index.done_entries().unwrap() {
        Some(entries) => store.done_page_of(entries, None, 7, 0, Some(&session)).unwrap(),
        None => store.done_page(None, 7, 0, Some(&session)).unwrap(),
    };
    let expected = store.done_page(None, 7, 0, Some(&session)).unwrap();
    assert_eq!(expected.total, 1);
    assert_eq!(page, expected, "missing cached session metadata must fall back, not silently hide the task");
    // An explicit null is a task sent to no session: the index answers.
    row["session"] = Value::Null;
    index.db.execute("UPDATE tasks SET row=?1 WHERE uuid=?2", params![row.to_string(), task.uuid()]).unwrap();
    let entries = index.done_entries().unwrap().unwrap();
    assert_eq!(entries.iter().map(|e| (e.uuid.as_str(), e.sent.clone())).collect::<Vec<_>>(), [(task.uuid(), None)]);
}

/// zerus-cg5h.30: the stamp check before an index placement, like the
/// periodic full check, looks up only the task files the folder lists and
/// reads no stored row of an unchanged task, and still finds what a writer
/// without the journal changed: appended updates, a removed snapshot, a new
/// task and a mark alone.
#[test]
fn a_full_stamp_check_looks_up_only_listed_files_and_still_finds_journalless_changes() {
    let temp = crate::host::staging::private_tempdir();
    let store = ProjectStore::open(temp.path(), "example").unwrap();
    let tasks: Vec<TaskDoc> = (0..20u8)
        .map(|n| {
            let task = new_task(&store, &format!("T-{n:06}"), Stage::Todo, &format!("V{}", char::from(b'a' + n)), "Plain");
            write_plain(&store, &task);
            task
        })
        .collect();
    let mut index = Index::open(&store).unwrap();
    STORED.with(|n| n.set(0));
    let (_, decoded, stamped) = counted(|| index.check_files().unwrap());
    assert_eq!((decoded, stamped, STORED.with(Cell::get)), (0, 20, 0), "one lookup per listed file, no stored row");
    let row = |index: &Index, uuid: &str| -> Option<(Option<String>, Option<String>)> {
        index
            .db
            .query_row("SELECT row, error FROM tasks WHERE uuid=?1", [uuid], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .unwrap()
    };
    let dir = store.dir().join("tasks");
    // Appended updates: the task's snapshot keeps its stamp.
    let mut updated = store.load(tasks[0].uuid()).unwrap();
    let snapshot = stamp(&dir.join(format!("{}.loro", updated.uuid())));
    updated.set_title("Updated outside the index", &Actor::Person, 2).unwrap();
    store.save(&mut updated).unwrap();
    assert!(dir.join(format!("{}.updates", updated.uuid())).exists());
    assert_eq!(stamp(&dir.join(format!("{}.loro", updated.uuid()))), snapshot);
    // A removed task, a new task and a mark alone.
    fs::remove_file(dir.join(format!("{}.loro", tasks[1].uuid()))).unwrap();
    let added = new_task(&store, "T-added1", Stage::Todo, "Z", "Added");
    write_plain(&store, &added);
    let marked = ids::new_uuid();
    fs::write(dir.join(format!("{marked}.managed")), b"{}").unwrap();
    index.check_files().unwrap();
    let (updated_row, _) = row(&index, updated.uuid()).unwrap();
    assert!(updated_row.unwrap().contains("Updated outside the index"));
    assert!(row(&index, tasks[1].uuid()).is_none(), "the removed task keeps its row");
    assert!(row(&index, added.uuid()).unwrap().0.is_some(), "the added task was not found");
    assert!(row(&index, &marked).unwrap().1.is_some(), "the mark alone was not found");
    // Unchanged again: one lookup per listed file.
    STORED.with(|n| n.set(0));
    let (_, decoded, stamped) = counted(|| index.check_files().unwrap());
    let listed = fs::read_dir(&dir).unwrap().count();
    assert_eq!((decoded, stamped, STORED.with(Cell::get)), (0, listed, 0));
}

/// zerus-cg5h.30: the stamps one folder listing gives each task are those a
/// lookup of its files gives (`stamped`), so a full check reads no unchanged
/// task again: for snapshots, appended updates, marks, a mark alone, and a
/// snapshot that is a link (stamped as what it links to) or a dangling one.
#[test]
fn listed_stamps_are_the_stamps_of_a_lookup() {
    let temp = crate::host::staging::private_tempdir();
    let store = project(temp.path());
    let mut floored = new_task(&store, "T-floor1", Stage::Todo, "W", "Floored");
    assert!(floored.floor().unwrap());
    aged_save(&store, &mut floored);
    floored.comment("Appended", &Actor::Person, 3).unwrap();
    store.save(&mut floored).unwrap();
    let tasks = store.dir().join("tasks");
    fs::write(tasks.join(format!("{}.managed", ids::new_uuid())), b"{}").unwrap();
    #[cfg(unix)]
    {
        let linked = new_task(&store, "T-link01", Stage::Todo, "X", "Linked");
        let target = temp.path().join("elsewhere.loro");
        fs::write(&target, linked.snapshot().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, tasks.join(format!("{}.loro", linked.uuid()))).unwrap();
        std::os::unix::fs::symlink(temp.path().join("nowhere"), tasks.join(format!("{}.loro", ids::new_uuid()))).unwrap();
    }
    let listed = super::listed(store.dir()).unwrap();
    assert_eq!(listed.keys().cloned().collect::<Vec<_>>(), store.uuids().unwrap());
    assert!(listed.values().any(|s| !s.ends_with(MISSING)), "no mark in the fixture");
    for (uuid, stamps) in &listed {
        assert_eq!(stamps, &files(store.dir(), uuid), "{uuid}");
    }
}
