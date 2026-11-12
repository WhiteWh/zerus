//! Coordinated locator seam for zerus-a4mub.8. These are catalogue coverage
//! statements, not submitted-answer proof. The consumer validates every original.
use super::*;

#[allow(dead_code)] // The independent question-recovery consumer stacks on this API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Coverage {
    Complete,
    Partial,
    Gap,
}
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QuestionReceiptRow {
    pub(crate) request_id: String,
    pub(crate) path: PathBuf,
}
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QuestionReceiptPage {
    pub(crate) rows: Vec<QuestionReceiptRow>,
    pub(crate) next_cursor: Option<String>,
    pub(crate) coverage: Coverage,
    pub(crate) restart_required: bool,
}
fn no_progress(after: Option<&str>) -> QuestionReceiptPage {
    QuestionReceiptPage {
        rows: Vec::new(),
        next_cursor: after.map(str::to_owned),
        coverage: Coverage::Partial,
        restart_required: false,
    }
}

/// Some(deadline) is inspection: no discovery walk, and an expired call does
/// no index/source I/O. None is explicit answer authority: reconcile only the
/// question family synchronously on a fresh head, then Complete or Err. The
/// answer consumer must still validate originals and drain next_cursor.
#[allow(dead_code)]
pub(crate) fn question_page(
    name: &str,
    run: &str,
    conversation: &str,
    after: Option<&str>,
    limit: usize,
    deadline: Option<Instant>,
) -> Result<QuestionReceiptPage> {
    if deadline.is_some_and(|d| Instant::now() >= d) && after.is_none_or(|c| c.len() <= 2048) {
        return Ok(no_progress(after));
    }
    question_page_at(&root(), name, run, conversation, after, limit, deadline)
}
fn question_page_at(
    state: &Path,
    name: &str,
    run: &str,
    conversation: &str,
    after: Option<&str>,
    limit: usize,
    deadline: Option<Instant>,
) -> Result<QuestionReceiptPage> {
    if deadline.is_some_and(|d| Instant::now() >= d) && after.is_none_or(|c| c.len() <= 2048) {
        return Ok(no_progress(after));
    }
    if name.is_empty()
        || run.is_empty()
        || !bounded_identity(name, 512)
        || !bounded_identity(run, 128)
        || !bounded_identity(conversation, 512)
        || !(1..=PAGE_LIMIT).contains(&limit)
        || after.is_some_and(|c| c.len() > 2048)
    {
        return Err("Invalid question receipt locator query".into());
    }
    let scope = Scope {
        source: Source::Question,
        name: Some(name.into()),
        run: run.into(),
        conversation: conversation.into(),
        agent: String::new(),
    };
    if deadline.is_none() && after.is_none() {
        // A fresh explicit authority read seals the current original family,
        // including writers from older builds that do not update the locator.
        let mut migration = migration::Migration::questions(state)?;
        while !migration.step()? {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let catalog = match Catalog::open(state, false) {
        Ok(catalog) => catalog,
        Err(error) => {
            if deadline.is_none() {
                return Err(error);
            }
            if !deadline.is_some_and(|d| Instant::now() >= d) {
                migration::request(state);
            }
            return Ok(no_progress(after));
        }
    };
    let page = match catalog.page_until(&scope, after.unwrap_or(""), limit, false, deadline) {
        Ok(page) => page,
        Err(PageFailure::Cursor) => {
            let head = catalog
                .page_until(&scope, "", 1, false, deadline)
                .map_err(|e| e.message().to_owned())?;
            let ready = head.metadata["migration_complete"] == true
                && head.metadata["unattributed_errors"] == 0;
            if deadline.is_none() && !ready {
                return Err("Question receipt coverage is incomplete".into());
            }
            return Ok(QuestionReceiptPage {
                rows: Vec::new(),
                next_cursor: None,
                coverage: if ready {
                    Coverage::Complete
                } else {
                    Coverage::Partial
                },
                restart_required: true,
            });
        }
        Err(error) => {
            if deadline.is_none() {
                return Err(error.message().into());
            }
            return Ok(no_progress(after));
        }
    };
    let ready = page.metadata["migration_complete"] == true;
    let gaps = page.metadata["unattributed_errors"] != 0;
    if deadline.is_none() && (!ready || gaps) {
        return Err("Question receipt coverage is incomplete or has gaps".into());
    }
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return Ok(no_progress(after));
    }
    if page.metadata["restart_required"] == true {
        return Ok(QuestionReceiptPage {
            rows: Vec::new(),
            next_cursor: None,
            coverage: Coverage::Complete,
            restart_required: true,
        });
    }
    if !ready || gaps {
        migration::request(state);
    }
    let coverage = if gaps {
        Coverage::Gap
    } else if ready {
        Coverage::Complete
    } else {
        Coverage::Partial
    };
    let rows = page
        .locators
        .into_iter()
        .map(|row| QuestionReceiptRow {
            path: state
                .join(Source::Question.directory())
                .join(format!("{}.json", row.id)),
            request_id: row.id,
        })
        .collect();
    let next_cursor = page.metadata["next_cursor"]
        .as_str()
        .filter(|c| !c.is_empty())
        .map(str::to_owned);
    Ok(QuestionReceiptPage {
        rows,
        next_cursor,
        coverage,
        restart_required: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn worker_lease(state: &Path) -> Guard {
        Guard::exclusive(lock_file(&state.join("receipt-index-worker.lock")).unwrap()).unwrap()
    }
    fn original(root: &Path, n: u128, name: &str, run: &str, conversation: &str) {
        let directory = root.join("question_receipts");
        fs::create_dir_all(&directory).unwrap();
        let id = uuid::Uuid::from_u128(n).to_string();
        fs::write(directory.join(format!("{id}.json")),json!({"request_id":id,"name":name,"run_id":run,"conversation_id":conversation,"status":"submitted","question_id":format!("q-{n}"),"question_hash":"fixture-hash","digest":"fixture-digest","submitted_at":n as f64}).to_string()).unwrap();
    }
    #[test]
    fn expired_call_has_no_filesystem_effect_and_keeps_the_cursor() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("not-created");
        let page = question_page_at(
            &missing,
            "codex/p/tag",
            "run",
            "conversation",
            Some("unchanged"),
            64,
            Some(Instant::now() - Duration::from_secs(1)),
        )
        .unwrap();
        assert_eq!(page, no_progress(Some("unchanged")));
        assert!(!missing.exists());
    }
    #[test]
    fn exact_identity_and_original_paths_are_paged_without_other_families() {
        let temp = tempfile::tempdir().unwrap();
        let _worker = worker_lease(temp.path());
        original(temp.path(), 1, "codex/p/tag", "run", "conversation");
        original(temp.path(), 2, "codex/foreign/tag", "run", "conversation");
        original(temp.path(), 3, "codex/p/tag", "foreign-run", "conversation");
        original(temp.path(), 4, "codex/p/tag", "run", "foreign-conversation");
        original(temp.path(), 5, "codex/p/tag", "run", "conversation");
        // An unreadable send namespace must not affect explicit question authority.
        fs::write(temp.path().join("input_receipts"), b"foreign source").unwrap();
        let first = question_page_at(
            temp.path(),
            "codex/p/tag",
            "run",
            "conversation",
            None,
            1,
            None,
        )
        .unwrap();
        assert_eq!(first.coverage, Coverage::Complete);
        assert_eq!(
            first.rows[0].request_id,
            uuid::Uuid::from_u128(5).to_string()
        );
        assert!(first.rows[0].path.is_file());
        assert!(first.next_cursor.is_some());
        let second = question_page_at(
            temp.path(),
            "codex/p/tag",
            "run",
            "conversation",
            first.next_cursor.as_deref(),
            1,
            Some(Instant::now() + Duration::from_secs(1)),
        )
        .unwrap();
        assert_eq!(second.coverage, Coverage::Complete);
        assert_eq!(
            second.rows[0].request_id,
            uuid::Uuid::from_u128(1).to_string()
        );
        let foreign = question_page_at(
            temp.path(),
            "codex/foreign/tag",
            "run",
            "conversation",
            first.next_cursor.as_deref(),
            1,
            Some(Instant::now() + Duration::from_secs(1)),
        )
        .unwrap();
        assert!(foreign.restart_required && foreign.rows.is_empty());
    }
    #[test]
    fn startup_identity_is_exact_and_modified_cursors_request_a_restart() {
        let temp = tempfile::tempdir().unwrap();
        let _worker = worker_lease(temp.path());
        original(temp.path(), 1, "codex/p/tag", "run", "");
        original(temp.path(), 2, "codex/p/tag", "run", "");
        original(temp.path(), 3, "codex/p/tag", "foreign-run", "");
        let first = question_page_at(temp.path(), "codex/p/tag", "run", "", None, 1, None).unwrap();
        assert_eq!(first.coverage, Coverage::Complete);
        assert_eq!(
            first.rows[0].request_id,
            uuid::Uuid::from_u128(2).to_string()
        );
        let cursor = first.next_cursor.unwrap();
        let (body, signature) = cursor.split_once('.').unwrap();
        let mut position: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(body).unwrap()).unwrap();
        position["at"] = json!(0);
        position["complete"] = json!(true);
        let tampered = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(position.to_string()),
            signature
        );
        let rejected = question_page_at(
            temp.path(),
            "codex/p/tag",
            "run",
            "",
            Some(&tampered),
            1,
            Some(Instant::now() + Duration::from_secs(1)),
        )
        .unwrap();
        assert!(rejected.restart_required && rejected.rows.is_empty());
        assert_eq!(rejected.next_cursor, None);
        let second = question_page_at(
            temp.path(),
            "codex/p/tag",
            "run",
            "",
            Some(&cursor),
            1,
            None,
        )
        .unwrap();
        assert!(!second.restart_required);
        assert_eq!(
            second.rows[0].request_id,
            uuid::Uuid::from_u128(1).to_string()
        );
        assert_eq!(second.next_cursor, None);
    }
    #[test]
    fn a_prefix_snapshot_cannot_become_complete_after_legacy_migration() {
        let temp = tempfile::tempdir().unwrap();
        let _worker = worker_lease(temp.path());
        for n in 1..=70 {
            original(temp.path(), n, "codex/p/tag", "run", "conversation");
        }
        let mut migration = migration::Migration::questions(temp.path()).unwrap();
        assert!(!migration.step().unwrap());
        drop(migration);
        let partial = question_page_at(
            temp.path(),
            "codex/p/tag",
            "run",
            "conversation",
            None,
            1,
            Some(Instant::now() + Duration::from_secs(1)),
        )
        .unwrap();
        assert_eq!(partial.coverage, Coverage::Partial);
        assert!(partial.next_cursor.is_some());
        let mut migration = migration::Migration::questions(temp.path()).unwrap();
        while !migration.step().unwrap() {}
        let retry = question_page_at(
            temp.path(),
            "codex/p/tag",
            "run",
            "conversation",
            partial.next_cursor.as_deref(),
            1,
            Some(Instant::now() + Duration::from_secs(1)),
        )
        .unwrap();
        assert!(retry.restart_required && retry.rows.is_empty());
        let fresh = question_page_at(
            temp.path(),
            "codex/p/tag",
            "run",
            "conversation",
            None,
            64,
            None,
        )
        .unwrap();
        assert_eq!(fresh.coverage, Coverage::Complete);
        assert!(!fresh.restart_required);
    }
    #[test]
    fn corrupt_legacy_records_abort_explicit_authority() {
        let temp = tempfile::tempdir().unwrap();
        let _worker = worker_lease(temp.path());
        original(temp.path(), 1, "codex/p/tag", "run", "conversation");
        fs::write(temp.path().join("question_receipts/bad.json"), b"{").unwrap();
        assert!(question_page_at(
            temp.path(),
            "codex/p/tag",
            "run",
            "conversation",
            None,
            64,
            None
        )
        .is_err());
        let page = question_page_at(
            temp.path(),
            "codex/p/tag",
            "run",
            "conversation",
            None,
            64,
            Some(Instant::now() + Duration::from_secs(1)),
        )
        .unwrap();
        assert_eq!(page.coverage, Coverage::Gap);
    }
    /// Unrelated send receipts, bound to their own rows as the trigger
    /// witness requires. Written directly: the page reads none of them.
    fn send_history(state: &Path, rows: u32) {
        let db = rusqlite::Connection::open(state.join(DATABASE)).unwrap();
        let transaction = db.unchecked_transaction().unwrap();
        let start: i64 = transaction
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM receipt_meta WHERE key='change_seq'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        {
            let mut insert = transaction
                .prepare(
                    "INSERT INTO receipt_rows(source,request,name,run,conversation,agent,first_message,at,files,binding,stamp,change)
                     VALUES('terminal',?1,'codex/p/other','other-run','other-conversation','codex',0,?2,0,'fixture-binding','missing',?3)",
                )
                .unwrap();
            for n in 1..=rows {
                insert
                    .execute(params![
                        uuid::Uuid::from_u128(1_000_000 + u128::from(n)).to_string(),
                        f64::from(n),
                        start + i64::from(n)
                    ])
                    .unwrap();
            }
        }
        transaction
            .execute(
                "UPDATE receipt_meta SET value=? WHERE key='change_seq'",
                [(start + i64::from(rows)).to_string()],
            )
            .unwrap();
        transaction.commit().unwrap();
    }
    /// zerus-hn36l: the coverage check of a question page must not depend on
    /// how many receipts other sessions have sent. A large send history once
    /// exhausted the page's SQLite work budget inside that check, which then
    /// read as unverified: every Codex question page stayed Partial and every
    /// answer was refused, however long the person waited.
    #[test]
    fn a_large_send_history_keeps_question_coverage_complete() {
        let temp = tempfile::tempdir().unwrap();
        let _worker = worker_lease(temp.path());
        original(temp.path(), 1, "codex/p/tag", "run", "conversation");
        let page = |deadline| {
            question_page_at(
                temp.path(),
                "codex/p/tag",
                "run",
                "conversation",
                None,
                64,
                deadline,
            )
        };
        let inspect = || page(Some(Instant::now() + Duration::from_secs(60))).unwrap();
        assert_eq!(page(None).unwrap().coverage, Coverage::Complete);
        send_history(temp.path(), 30_000);
        let inspection = inspect();
        assert_eq!(inspection.coverage, Coverage::Complete);
        assert!(!inspection.restart_required);
        assert_eq!(
            inspection.rows[0].request_id,
            uuid::Uuid::from_u128(1).to_string()
        );
        // An answer (no deadline) is accepted as well.
        let answer = page(None).unwrap();
        assert_eq!(answer.coverage, Coverage::Complete);
        assert_eq!(answer.rows.len(), 1);
        // The row-set witness still refuses a history it cannot account for.
        let db = rusqlite::Connection::open(temp.path().join(DATABASE)).unwrap();
        let execute = |statement: &str| db.execute_batch(statement).unwrap();
        execute("UPDATE receipt_sources SET indexed=indexed+1 WHERE source='terminal'");
        assert_eq!(inspect().coverage, Coverage::Partial);
        assert!(page(None).is_err());
        execute("UPDATE receipt_sources SET indexed=indexed-1 WHERE source='terminal'");
        assert_eq!(inspect().coverage, Coverage::Complete);
        // So do the per-source consistency seeks: an unseeded row of this or
        // any other source, and a frontier behind any source's change.
        let question: i64 = db
            .query_row(
                "SELECT change FROM receipt_rows WHERE source='question'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        execute("UPDATE receipt_rows SET change=0 WHERE source='question'");
        assert_eq!(inspect().coverage, Coverage::Partial);
        db.execute(
            "UPDATE receipt_rows SET change=? WHERE source='question'",
            [question],
        )
        .unwrap();
        assert_eq!(inspect().coverage, Coverage::Complete);
        let last = uuid::Uuid::from_u128(1_030_000).to_string();
        let change: i64 = db
            .query_row(
                "SELECT change FROM receipt_rows WHERE source='terminal' AND request=?",
                [&last],
                |row| row.get(0),
            )
            .unwrap();
        db.execute(
            "UPDATE receipt_rows SET change=0 WHERE source='terminal' AND request=?",
            [&last],
        )
        .unwrap();
        assert_eq!(inspect().coverage, Coverage::Partial);
        db.execute(
            "UPDATE receipt_rows SET change=? WHERE source='terminal' AND request=?",
            params![change, last],
        )
        .unwrap();
        assert_eq!(inspect().coverage, Coverage::Complete);
        db.execute(
            "UPDATE receipt_meta SET value=? WHERE key='change_seq'",
            [(change - 1).to_string()],
        )
        .unwrap();
        assert_eq!(inspect().coverage, Coverage::Partial);
    }
}
