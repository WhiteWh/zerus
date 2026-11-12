"""Receipt index row-set guard (zerus-z5uax).

Readers trust a witness token that a worker verified against the exact rows:
afterwards every insert and delete must go through the receipt_rows triggers.
SQLite's REPLACE conflict resolution deletes a row without firing its DELETE
trigger, an upsert rewrites a counted row or a counter in place, and an insert
that chooses its own seq can hide below the token's sequence. The token would
notice such a desync only at the next worker witness, so the source must never
contain one.
"""
import re
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
TABLES = re.compile(r"\breceipt_(?:rows|sources)\b")
FORBIDDEN = (
    (re.compile(r"\bOR\s+REPLACE\b", re.I), "OR REPLACE deletes rows without their DELETE trigger"),
    (re.compile(r"\bREPLACE\s+INTO\b", re.I), "REPLACE INTO deletes rows without their DELETE trigger"),
    (re.compile(r"\bON\s+CONFLICT\s+REPLACE\b", re.I),
     "an ON CONFLICT REPLACE constraint deletes rows without their DELETE trigger"),
)
INSERT = re.compile(r"\bINSERT\b(?:\s+OR\s+\w+)?\s+INTO\s+(receipt_rows|receipt_sources)\b([^\";]*)", re.I)


def findings(files):
    """Problems as 'path:line: reason' for every file that names a guarded table."""
    problems = []
    for path, text in sorted(files.items()):
        if not TABLES.search(text):
            continue
        for pattern, reason in FORBIDDEN:
            for match in pattern.finditer(text):
                # INSERT OR REPLACE INTO is reported once, as OR REPLACE.
                if "INTO" in pattern.pattern and re.search(r"\bOR\s+$", text[:match.start()], re.I):
                    continue
                problems.append(f"{path}:{text.count(chr(10), 0, match.start()) + 1}: {reason}")
        for match in INSERT.finditer(text):
            line = text.count("\n", 0, match.start()) + 1
            table, rest = match.group(1).lower(), match.group(2)
            if re.search(r"\bON\s+CONFLICT\b", rest, re.I):
                problems.append(f"{path}:{line}: an upsert into {table} rewrites a counted row or counter in place")
            columns = re.match(r"\s*\(([^)]*)\)", rest)
            if table == "receipt_rows" and columns and re.search(r"(?:^|,)\s*seq\s*(?:,|$)", columns.group(1), re.I):
                problems.append(f"{path}:{line}: an insert into receipt_rows must not choose its own seq")
    return problems


class CurrentTreeTest(unittest.TestCase):
    def test_no_rust_source_bypasses_the_row_set_triggers(self):
        files = {path.relative_to(ROOT).as_posix(): path.read_text(encoding="utf-8")
                 for path in (ROOT / "src").rglob("*.rs")}
        guarded = [path for path, text in files.items() if TABLES.search(text)]
        # The guard reads every file that names a table, tests included.
        self.assertIn("src/state/receipt_index.rs", guarded)
        self.assertIn("src/state/receipt_index/migration.rs", guarded)
        self.assertEqual(findings(files), [])


class RuleTest(unittest.TestCase):
    def test_every_trigger_bypass_is_rejected(self):
        problems = findings({
            "src/a.rs": 'db.execute("INSERT OR REPLACE INTO receipt_rows(source,request) VALUES(?,?)")',
            "src/b.rs": 'db.execute("REPLACE INTO receipt_sources(source) VALUES(?)")',
            "src/c.rs": 'db.execute("UPDATE OR REPLACE receipt_rows SET request=? WHERE seq=?")',
            "src/d.rs": "CREATE TABLE receipt_rows(seq INTEGER PRIMARY KEY,request TEXT,UNIQUE(request) ON CONFLICT REPLACE)",
            "src/e.rs": 'db.execute("INSERT INTO receipt_rows(source,request) VALUES(?,?)\n ON CONFLICT(source,request) DO UPDATE SET at=excluded.at")',
            "src/f.rs": 'db.execute("INSERT INTO receipt_sources(source,indexed) VALUES(?,?) ON CONFLICT(source) DO UPDATE SET indexed=excluded.indexed")',
            "src/g.rs": 'db.execute("INSERT INTO receipt_rows(seq,source,request) VALUES(?,?,?)")',
            "src/h.rs": 'db.execute("insert into receipt_rows (source, seq, request) values (?,?,?)")',
        })
        self.assertEqual(problems, [
            "src/a.rs:1: OR REPLACE deletes rows without their DELETE trigger",
            "src/b.rs:1: REPLACE INTO deletes rows without their DELETE trigger",
            "src/c.rs:1: OR REPLACE deletes rows without their DELETE trigger",
            "src/d.rs:1: an ON CONFLICT REPLACE constraint deletes rows without their DELETE trigger",
            "src/e.rs:1: an upsert into receipt_rows rewrites a counted row or counter in place",
            "src/f.rs:1: an upsert into receipt_sources rewrites a counted row or counter in place",
            "src/g.rs:1: an insert into receipt_rows must not choose its own seq",
            "src/h.rs:1: an insert into receipt_rows must not choose its own seq",
        ])

    def test_trigger_preserving_writes_are_accepted(self):
        self.assertEqual(findings({
            "src/a.rs": 'db.execute("INSERT INTO receipt_rows(source,request,change) VALUES(?,?,?)")',
            "src/b.rs": 'db.execute("INSERT OR IGNORE INTO receipt_sources(source) VALUES(?)")',
            "src/c.rs": 'db.execute("INSERT INTO receipt_errors VALUES(?,?,?) ON CONFLICT(source,name) DO UPDATE SET reason=excluded.reason")\n'
                        'db.execute("UPDATE receipt_rows SET stamp=? WHERE source=?")',
            # A file that never names a guarded table is out of scope.
            "src/d.rs": 'db.execute("INSERT OR REPLACE INTO meta(key,value) VALUES(?,?)")',
        }), [])


if __name__ == "__main__":
    unittest.main()
