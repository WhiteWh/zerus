"""SDK-free controls for the read-visibility simulator UI campaign (zerus-a4mub.31).

The campaign runner and project generator are the approval UI fixture's hardened ones with only the fixture's own
identity changed; these tests pin that derivation, the exact native case inventory, the generated project and the
workflow job, so the read-visibility campaign cannot drift from the reviewed approval campaign's guarantees.
"""
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
CASES = ("test1TheOpeningReplyIsReadAfterTheDwell", "test2ASliverIsNotReadButTheWholeRowIs",
         "test3TheQuestionsPaneCountsFromItsReadablePart")


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, ROOT / path)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def native_log(*cases, passed=True, succeeded=True, extra=""):
    lines = []
    for case in cases:
        lines.append(f"Test Case '-[ReadVisibilityUIFixtureTests.ReadVisibilityUITests {case}]' started.")
        state = "passed" if passed else "failed"
        lines.append(f"Test Case '-[ReadVisibilityUIFixtureTests.ReadVisibilityUITests {case}]' {state} (13.402 seconds).")
    lines.append(extra)
    lines.append("** TEST SUCCEEDED **" if succeeded else "** TEST FAILED **")
    return "\n".join(lines) + "\n"


class ReadVisibilityHarness(unittest.TestCase):
    def test_result_requires_each_native_case_once(self):
        ios = module("ios_read_visibility_ui", "scripts/ci/ios-read-visibility-ui.py")
        self.assertEqual(ios.CASES, CASES)
        self.assertEqual(ios.results(native_log(*CASES)), {"started": list(CASES), "passed": list(CASES), "failed": 0, "skipped": 0})
        for text in (native_log(), native_log(*CASES[:2]), native_log(*CASES, CASES[0]), native_log(*CASES[:2], "testSomethingElse"),
                     native_log(*CASES, passed=False), native_log(*CASES, succeeded=False),
                     native_log(*CASES, extra="Test Case '-[X y]' skipped (0.1 seconds).")):
            with self.subTest(text=text), self.assertRaises(AssertionError):
                ios.results(text)

    def test_runner_is_the_approval_runner_with_only_the_fixture_identity_changed(self):
        approval = (ROOT / "scripts/ci/ios-approval-ui.py").read_text()
        runner = (ROOT / "scripts/ci/ios-read-visibility-ui.py").read_text()
        approval_body, runner_body = approval.split("\nimport hashlib\n", 1)[1], runner.split("\nimport hashlib\n", 1)[1]
        replaced = {
            # The one native case and its inventory.
            "CASES": None,
            "ios-approval-ui-evidence": "ios-read-visibility-ui-evidence",
            '"Zerus approval "': '"Zerus read visibility "',
            "scripts/ci/ios-approval-ui-project.py": "scripts/ci/ios-read-visibility-ui-project.py",
            "ApprovalUIFixture": "ReadVisibilityUIFixture",
            '"test.example.zerus.approval-fixture"': '"dev.zerus.fixture.readvisibility"',
            '"seven native SwiftUI/memory callback cases only; no real delivery, owner device or keychain"':
                '"one native read-visibility case; synthetic rows, no relay, owner device or keychain"',
        }
        expected = approval_body
        for old, new in replaced.items():
            if new is not None:
                self.assertIn(old, expected)
                expected = expected.replace(old, new)
        # The case inventory and its counted assertions are the only other difference.
        expected = re.sub(r"CASES = \(\n.*?\n\)\n", "CASES = (\n    \"" + "\", \"".join(CASES[:2]) + "\",\n    \"" + CASES[2] + "\",\n)\n",
                          expected, flags=re.S)
        expected = expected.replace("ApprovalChoicesUITests", "ReadVisibilityUITests")
        expected = expected.replace('== 7, "no additional native tests"', '== len(CASES), "no additional native tests"')
        expected = expected.replace("passed \\([0-9.]+ seconds\\)\\.\", text)) == 7", "passed \\([0-9.]+ seconds\\)\\.\", text)) == len(CASES)")
        expected = expected.replace("len(started) == len(passed) == 7", "len(started) == len(passed) == len(CASES)")
        self.assertEqual(runner_body, expected)
        # The hardened limits stay those of the reviewed approval campaign.
        for literal in ("end = time.monotonic() + 900", "phase_end = enter_cleanup() if cleaning else end - 60",
                        '"-test-timeouts-enabled", "YES"', '"CODE_SIGNING_ALLOWED=NO"', 'assert len(sys.argv) == 1'):
            self.assertIn(literal, runner)

    def test_runner_refuses_arguments_before_any_runtime(self):
        run = subprocess.run([sys.executable, "-I", "-B", str(ROOT / "scripts/ci/ios-read-visibility-ui.py"), "extra"],
                             capture_output=True, text=True, timeout=30, cwd=ROOT)
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("AssertionError", run.stderr)

    def test_project_references_exact_production_sources_and_installable_identifiers(self):
        generator = module("ios_read_visibility_ui_project", "scripts/ci/ios-read-visibility-ui-project.py")
        sources = ["mobile/ios/ReadVisibilityUIFixture/ReadVisibilityUIFixtureApp.swift",
                   "mobile/ios/Zerus/Conversation/ConversationReadLayout.swift",
                   "mobile/ios/ReadVisibilityUIFixture/ReadVisibilityUITests.swift"]
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / "new"
            project = generator.generate(ROOT, output)
            body = (project / "project.pbxproj").read_text()
            for name in sources:
                self.assertIn(json.dumps(str(ROOT / name)), body)
            self.assertEqual(body.count("isa = PBXFileReference; lastKnownFileType = sourcecode.swift;"), 3)
            self.assertIn(str(ROOT / "mobile/ios/ZerusKit"), body)
            for absent in ("App/ZerusApp.swift", "App/AppModel.swift", "ConversationView.swift", "BRIDGING_HEADER", "CODE_SIGNING_ALLOWED = YES"):
                self.assertNotIn(absent, body)
            # The iOS 26.0 simulator runtime refuses test.example.* identifiers (zerus-a4mub.34).
            self.assertIn("PRODUCT_BUNDLE_IDENTIFIER = dev.zerus.fixture.readvisibility;", body)
            self.assertIn("PRODUCT_BUNDLE_IDENTIFIER = dev.zerus.fixture.readvisibility.uitests;", body)
            self.assertNotIn("test.example", body)
            scheme = ET.parse(project / "xcshareddata/xcschemes/ReadVisibilityUIFixture.xcscheme").getroot()
            testable = scheme.findall("./TestAction/Testables/TestableReference")
            self.assertEqual([item.find("BuildableReference").get("BlueprintName") for item in testable], ["ReadVisibilityUIFixtureTests"])
            identities = json.loads((output / "source-identities.json").read_text())["sources"]
            for name in sources:
                self.assertEqual(identities[name], hashlib.sha256((ROOT / name).read_bytes()).hexdigest())
            with self.assertRaises(FileExistsError):
                generator.generate(ROOT, output)

    def test_fixture_drives_production_placement_and_holds_no_live_client(self):
        tests = (ROOT / "mobile/ios/ReadVisibilityUIFixture/ReadVisibilityUITests.swift").read_text()
        self.assertEqual(re.findall(r"func (test\w+)\(", tests), list(CASES))
        app = (ROOT / "mobile/ios/ReadVisibilityUIFixture/ReadVisibilityUIFixtureApp.swift").read_text()
        for production in ("ConversationReadLayout()", ".readingItem(", "ConversationReadViewport.eventIds(",
                           "ConversationReadViewport.flags(", "ConversationReadPolicies.lowerBound(", ".milliseconds(350)"):
            self.assertIn(production, app)
        for live in ("RelayClient(", "KeychainConnectionStore(", "URLSession(", "AppModel("):
            self.assertNotIn(live, app)
        # The fixture never decides visibility itself: no thresholds or fractions of its own.
        self.assertNotIn("onScrollTargetVisibilityChange", app)
        self.assertNotIn("threshold", app)

    def test_workflow_job_is_the_reviewed_approval_job_with_only_its_fixture_changed(self):
        workflow = (ROOT / ".github/workflows/ios-approval-ui.yml").read_text()
        jobs = workflow.split("\njobs:\n", 1)[1]
        approval, visibility = jobs.split("\n  # zerus-a4mub.31:", 1)
        visibility = visibility.split("\n  read-visibility-ui:\n", 1)[1]
        approval = approval.split("  approval-ui:\n", 1)[1]
        expected = (approval.replace("python3 -I -B tests/test_approval_ui_harness.py", "python3 -I -B tests/test_read_visibility_ui_harness.py")
                    .replace("Check SDK-free admission, inventory and ownership controls", "Check SDK-free read-visibility admission and inventory controls")
                    .replace("One unsigned native simulator UI campaign", "One unsigned native simulator read-visibility campaign")
                    .replace("scripts/ci/ios-approval-ui.py", "scripts/ci/ios-read-visibility-ui.py")
                    .replace("ios-approval-ui-", "ios-read-visibility-ui-").replace("ApprovalUIFixture", "ReadVisibilityUIFixture"))
        self.assertEqual(visibility.rstrip("\n"), expected.rstrip("\n"))
        for path in ("mobile/ios/ReadVisibilityUIFixture/**", "scripts/ci/ios-read-visibility-ui*.py",
                     "tests/test_read_visibility_ui_harness.py", "mobile/ios/Zerus/Conversation/ConversationReadLayout.swift"):
            self.assertIn("      - '" + path + "'", workflow)
        self.assertIn("permissions:\n  contents: read", workflow)
        self.assertIn("cache-mode: none", workflow)
        self.assertNotIn("secrets.", workflow)
        self.assertEqual(workflow.count("    timeout-minutes: 25\n"), 2)


if __name__ == "__main__":
    unittest.main()
