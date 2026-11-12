"""Golden vectors in services/mobile/vectors stay reproducible from the Python connector."""
import ast
import importlib.util
import json
from pathlib import Path
import re
import sqlite3
import unittest

import relay_oracle
import zerus_mobile

VECTORS = Path(__file__).resolve().parents[1] / "vectors"
# A node_token value, also inside a JSON string holding a configuration file.
TOKEN = re.compile(r'node_token\\*"\s*:\s*\\*"([^"\\]*)')


def load_capture():
    spec = importlib.util.spec_from_file_location("zerus_mobile_vector_capture", VECTORS / "capture.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def strings(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, list):
        for item in value:
            yield from strings(item)
    elif isinstance(value, dict):
        for key, item in value.items():
            yield key
            yield from strings(item)


def records(name, data):
    if name.endswith(".jsonl"):
        return [json.loads(line) for line in data.decode().splitlines()]
    if name.endswith(".json"):
        return [json.loads(data)]
    return []


class Unqualified(ast.NodeTransformer):
    """Reads aiohttp's web.HTTPBadRequest as the bridge's HTTPBadRequest."""

    def visit_Attribute(self, node):
        self.generic_visit(node)
        if isinstance(node.value, ast.Name) and node.value.id == "web":
            return ast.copy_location(ast.Name(id=node.attr, ctx=node.ctx), node)
        return node


def imports(tree):
    """Each module-level imported name: the module and attribute it binds."""
    bound = {}
    for node in tree.body:
        if isinstance(node, ast.ImportFrom):
            module = ".".join(part for part in ("zerus_mobile" if node.level else "", node.module or "") if part)
            bound.update({alias.asname or alias.name: (module, alias.name) for alias in node.names})
        elif isinstance(node, ast.Import):
            bound.update({alias.asname or alias.name: (alias.name, None) for alias in node.names})
    return bound


class GoldenVectorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.capture = load_capture()
        # The relay-oracle and relay columns are frozen data: copied, never recomputed here.
        cls.files = cls.capture.render(cls.capture.frozen_oracle(VECTORS), cls.capture.frozen_relay(VECTORS))

    def cases(self):
        return [row for name, data in self.files.items() if name.startswith("validation/")
                for row in records(name, data) if row.get("kind") == "case"]

    def test_capture_reproduces_every_file_byte_for_byte(self):
        self.assertEqual(self.capture.differences(self.files), [],
                         "regenerate with services/mobile/vectors/capture.py and review the diff")

    def test_files_are_bounded_strict_json_or_sql(self):
        for name, data in self.files.items():
            with self.subTest(name=name):
                self.assertLessEqual(len(data), self.capture.FILE_LIMIT)
                self.assertTrue(data.endswith(b"\n") and not data.endswith(b"\n\n"))
                self.assertNotIn(b" \n", data)
                data.decode("ascii")
                # A strict UTF-8 JSON reader must accept every file: no lone surrogates.
                for record in records(name, data):
                    for text in strings(record):
                        self.assertFalse(any(0xD800 <= ord(char) <= 0xDFFF for char in text))

    def test_credentials_are_obviously_fake(self):
        for name, data in self.files.items():
            for token in TOKEN.findall(data.decode()):
                with self.subTest(name=name, token=token):
                    self.assertTrue(token == "" or token.startswith(("fixture", "synthetic")))

    def test_journal_fixtures_load_with_python_sqlite(self):
        fixtures = [name for name in self.files if name.startswith("journal/")]
        self.assertGreaterEqual(len(fixtures), 7)
        for name in fixtures:
            with self.subTest(name=name):
                db = sqlite3.connect(":memory:")
                try:
                    db.executescript(self.files[name].decode())
                    tables = {row[0] for row in db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
                    self.assertIn("requests", tables)
                finally:
                    db.close()

    def test_operation_tables_have_the_reviewed_sizes(self):
        counts = json.loads(self.files["operations.json"])["counts"]
        self.assertEqual((counts["OPERATIONS"], counts["READ_OPERATIONS"], counts["MUTATIONS"], counts["CONTEXT_KEYS"]),
                         (53, 22, 31, 24))  # + HGS_SESSION_ID (zerus-ucck P1)
        operations = set(json.loads(self.files["operations.json"])["sets"]["OPERATIONS"])
        self.assertTrue({"hooks_status", "analytics_read", "history_projection", "project_assignment", "project_assign"} <= operations)
        self.assertNotIn("remove_queued", operations)  # Rust-only table, never Python's.
        self.assertNotIn("git_status", operations)  # Its separate read marker is Rust-only too.
        # The person's per-child question route switch (zerus-sa9qn.33) is
        # Rust-only: the legacy connector neither offers nor runs it.
        self.assertNotIn("session_question_routing", operations)
        from zerus_mobile.context import OPERATIONS as LIVE
        self.assertNotIn("session_question_routing", LIVE)

    def test_every_operation_has_a_valid_case_and_an_envelope(self):
        operations = set(json.loads(self.files["operations.json"])["sets"]["OPERATIONS"])
        valid = {row["id"].split("/", 1)[1] for row in self.cases() if row["id"].startswith("valid/")}
        envelopes = {row["id"].removeprefix("envelope-") for row in records(".jsonl", self.files["request_envelopes.jsonl"])
                     if row["id"].startswith("envelope-")}
        # The additive projection ABI has its own strict 28-request corpus and
        # connector/relay controls in test_mobile_history_projection.py.
        projection = json.loads((VECTORS / "history_projection" / "envelope.json").read_text())
        self.assertEqual(projection["operation"], "history_projection")
        self.assertEqual(relay_oracle.decision(projection), {"decision": "accept"})
        self.assertEqual(valid | {projection["operation"]}, operations)
        self.assertEqual(envelopes | {projection["operation"]}, operations)

    def test_a_changed_submission_never_reuses_its_frozen_oracle_decision(self):
        frozen = self.capture.frozen_oracle(VECTORS)
        case_id = sorted(frozen)[0]
        frozen[case_id] = ("{}", frozen[case_id][1])
        with self.assertRaisesRegex(self.capture.CaptureError, re.escape(case_id)):
            self.capture.validation_vectors(frozen, self.capture.frozen_relay(VECTORS))

    def test_a_changed_submission_drops_its_frozen_relay_decision(self):
        relay = self.capture.frozen_relay(VECTORS)
        changed, kept = sorted(relay)[:2]
        relay[changed] = ("{}", relay[changed][1])
        files, _ = self.capture.validation_vectors(self.capture.frozen_oracle(VECTORS), relay)
        rows = {row["id"]: row for name, data in files.items() for row in records(name, data) if row["kind"] == "case"}
        self.assertNotIn("relay", rows[changed])
        self.assertIn("relay", rows[kept])

    def test_every_case_has_a_relay_decision(self):
        self.assertEqual([case["id"] for case in self.cases() if "relay" not in case], [],
                         "compute the relay column with ZERUS_BLESS=1 cargo test --test vectors in services/mobile/relay")

    def test_frozen_oracle_matches_the_bridge(self):
        cases = self.cases()
        self.assertGreater(len(cases), 200)
        for case in cases:
            with self.subTest(case=case["id"]):
                self.assertEqual(case["oracle"], relay_oracle.decision(case["submission"]))

    def test_bridge_copies_the_reference_relay_verbatim_while_it_exists(self):
        package = Path(zerus_mobile.__file__).resolve().parent
        if not (package / "server.py").exists():
            self.skipTest("the Python reference relay was removed; relay_oracle.py keeps its validation")
        bridge = ast.parse(Path(relay_oracle.__file__).read_text())
        copies = {node.name: node for node in bridge.body if isinstance(node, ast.FunctionDef)}
        for source, names in (("server.py", {"text", "validate_request"}), ("ingress.py", {"_encoded", "encoded_size"})):
            original = Unqualified().visit(ast.parse((package / source).read_text()))
            functions = [node for node in original.body if isinstance(node, ast.FunctionDef) and node.name in names]
            self.assertEqual({node.name for node in functions}, names)
            for node in functions:
                with self.subTest(source=source, function=node.name):
                    self.assertEqual(ast.dump(copies[node.name]), ast.dump(node))
                    # Every imported name the copy uses binds the same object.
                    theirs, ours = imports(original), imports(bridge)
                    for name in {item.id for item in ast.walk(node) if isinstance(item, ast.Name)} & set(theirs):
                        self.assertEqual(ours.get(name), theirs[name], name)


if __name__ == "__main__":
    unittest.main()
