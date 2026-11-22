use super::*;
use std::process::Command;

const CASE: &str = "HGS_TEST_NATIVE_ARCHIVE_CENSUS";

fn in_private_state(name: &str, probe: impl FnOnce()) {
    if std::env::var(CASE).as_deref() == Ok(name) {
        assert_eq!(root().file_name().unwrap(), "state");
        assert_eq!(
            root().parent().unwrap().canonicalize().unwrap(),
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
        probe();
        return;
    }
    let module = module_path!().split_once("::").unwrap().1;
    let test = format!("{module}::{name}");
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", &test, "--nocapture"])
        .current_dir(home.path())
        .env_clear()
        .env("HOME", home.path())
        .env("HGS_STATE_DIR", home.path().join("state"))
        .env(CASE, name);
    if let Some(system) = std::env::var_os("SystemRoot") {
        child.env("SystemRoot", system);
    }
    let output = child.output().unwrap();
    assert!(
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn archived(tag: &str) -> String {
    // Retirement hands this saved metadata to publish. Let the real writer
    // allocate the ID and encode the indexed identity and the body together.
    publish(&json!({
        "version": 1,
        "agent": "dsh",
        "backend": "dsh",
        "name": format!("dsh/test/{tag}"),
        "session_id": uuid::Uuid::new_v4().to_string(),
        "run_id": uuid::Uuid::new_v4().to_string(),
        "conversation_id": format!("session-{}", uuid::Uuid::new_v4()),
        "cwd": std::env::current_dir().unwrap(),
    }))
    .unwrap()
}

fn census(max_rows: usize, rows: &mut Vec<Value>) -> Result<()> {
    visit_references(
        Instant::now() + Duration::from_secs(5),
        max_rows,
        usize::MAX,
        |record| {
            rows.push(record);
            Ok(())
        },
    )
}

#[test]
fn census_refuses_a_changed_archive_index_identity() {
    in_private_state("census_refuses_a_changed_archive_index_identity", || {
        let id = archived("identity");
        let mut rows = Vec::new();
        census(1, &mut rows).unwrap();
        let [record] = rows.as_slice() else {
            panic!("expected one published archive")
        };
        assert_eq!(record["archive_id"], id);

        let db = open(&root().join(FILE), true).unwrap();
        for (update, key) in [
            ("UPDATE archive_rows SET name=?1 WHERE id=?2", "name"),
            ("UPDATE archive_rows SET run=?1 WHERE id=?2", "run_id"),
            (
                "UPDATE archive_rows SET conversation=?1 WHERE id=?2",
                "conversation_id",
            ),
        ] {
            // Corrupt only the index projection of an otherwise valid archive.
            assert_eq!(db.execute(update, params!["changed", id]).unwrap(), 1);
            let mut visited = Vec::new();
            assert_eq!(
                census(1, &mut visited).unwrap_err(),
                "DeepSeek archive reference identity changed",
                "{key}"
            );
            assert!(visited.is_empty(), "the mismatched {key} was visited");
            assert_eq!(
                db.execute(update, params![record[key].as_str(), id])
                    .unwrap(),
                1
            );
        }
    });
}

#[test]
fn census_refuses_one_more_archive_than_its_row_bound() {
    in_private_state("census_refuses_one_more_archive_than_its_row_bound", || {
        let mut ids = [archived("first"), archived("second")];
        ids.sort_unstable();
        let mut rows = Vec::new();
        census(2, &mut rows).unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| string(row, "archive_id"))
                .collect::<Vec<_>>(),
            ids.each_ref().map(String::as_str)
        );

        archived("third");
        rows.clear();
        assert_eq!(
            census(2, &mut rows).unwrap_err(),
            "DeepSeek archive references exceeded their review bound"
        );
        assert_eq!(rows.len(), 2);
    });
}
