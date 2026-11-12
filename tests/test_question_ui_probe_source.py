#!/usr/bin/env python3
"""SDK-free preservation checks, not Kotlin compilation or Compose/native evidence."""
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[1]
_focus_spec = importlib.util.spec_from_file_location("review_focus_projection", ROOT / "tests/review_focus_projection.py")
_focus_projection = importlib.util.module_from_spec(_focus_spec)
_focus_spec.loader.exec_module(_focus_projection)
original_review_fixture = _focus_projection.original_review_fixture
MAIN = "mobile/android/app/src/main/java/app/zerus/mobile/"
FIXTURE = "mobile/android/app/src/androidTest/java/app/zerus/mobile/ApprovalChoicesSmokeInstrumentation.kt"
ORIGINAL_UI = "0fa146a7ccaa38be221768b901f3d28fe93a95947620f9e43e84127955b5a81a"
# Fixture pins include the parent-selection and Compose role-child corrections; the approval harness
# reverses them to exact 96fc.
ORIGINAL_FIXTURE = "1443287232e93dfeedb401064bc2c2cf022cbe1dccffc01e3ee4ed415cf4d729"


def strip_probe(source, expected):
    pattern = re.compile(r"^([ ]*)// BEGIN QUESTION_UI_PROBE ([a-z-]+)\n[\s\S]*?^\1// END QUESTION_UI_PROBE \2\n", re.M)
    names = [match[2] for match in pattern.finditer(source)]
    if names != expected:
        raise ValueError("unexpected probe seams")
    return pattern.sub("", source)


def original_ui(source):
    return hashlib.sha256(strip_probe(source, ["entry", "request", "committed", "showing"]).encode()).hexdigest() == ORIGINAL_UI


def original_fixture(source):
    source = strip_send_diagnostics(source)
    source = strip_probe(source, ["fixture-report"])
    flat = r' | ${actionProbeEvidence().replace("\n", " | ")}'
    raw = r'\n${actionProbeEvidence()}'
    if source.count("    private fun smokeBody(store: PrivateStore) {\n") != 1 or source.count(flat) != 1 or source.count(raw) != 1:
        raise ValueError("unexpected fixture edits")
    source = source.replace("    private fun smokeBody(store: PrivateStore) {\n", "    private fun smoke(store: PrivateStore) {\n")
    source = source.replace(flat, "").replace(raw, "")
    return hashlib.sha256(source.encode()).hexdigest() == ORIGINAL_FIXTURE


def strip_send_diagnostics(source):
    pattern = re.compile(r"^([ ]*)// BEGIN QUESTION_SEND_DIAGNOSTIC ([a-z-]+)\n[\s\S]*?^\1// END QUESTION_SEND_DIAGNOSTIC \2\n", re.M)
    if [match[2] for match in pattern.finditer(source)] != ["definition", "model", "post", "operation", "admitted", "recorded"]:
        raise ValueError("unexpected send diagnostic seams")
    return pattern.sub("", source)


def diagnostic_text_memory(source):
    """Translate only the exact classifier's literal/empty/prefix predicates and bounded hash format."""
    body = source.split("    private fun failureText(value: String): String = try {\n", 1)[1].split('    } catch (_: Throwable) { "k=UNKNOWN" }', 1)[0]
    choices = body.split("        val kind = when {\n", 1)[1].split("        }\n", 1)[0]
    branches = []
    for line in choices.splitlines():
        match = re.fullmatch(r'            (value\.isEmpty\(\)|value == "[^"\n]*"|value\.startsWith\("[^"\n]*"\)|else) -> "([a-z-]+)"', line)
        assert match
        branches.append(match.groups())
    assert branches[-1] == ("else", "unknown")
    assert 'val prefix = value.take(4096)' in body
    assert 'java.security.MessageDigest.getInstance("SHA-256").digest(prefix.toByteArray(Charsets.UTF_8))' in body
    assert body.rstrip().endswith('"k=$kind n=${value.length} p=${prefix.length} sha256Prefix=$digest"')
    def classify(value):
        for condition, kind in branches:
            if condition == "value.isEmpty()": accepted = value == ""
            elif condition == "else": accepted = True
            elif condition.startswith("value == "): accepted = value == json.loads(condition[len("value == "):])
            else: accepted = value.startswith(json.loads(condition[len("value.startsWith("):-1]))
            if accepted:
                prefix = value[:4096]
                return f"k={kind} n={len(value)} p={len(prefix)} sha256Prefix={hashlib.sha256(prefix.encode()).hexdigest()}"
        raise AssertionError("missing fixed default")
    return classify, branches


class QuestionUiProbeSourceTests(unittest.TestCase):
    def test_failure_classifiers_and_post_observations_leave_all_original_client_and_ui_code_exact(self):
        source = original_review_fixture()
        self.assertEqual(hashlib.sha256(strip_send_diagnostics(source).encode()).hexdigest(),
            "fa989a142274bf10ac59eacd8e753227c0ace5635b9a226ab28d5ff13a9ef320")
        model = source.split("    private fun modelEvidence(", 1)[1].split("    private fun set(", 1)[0]
        self.assertTrue(model.startswith('model: ZerusViewModel, target: Target, activity: MainActivity, offered: JSONObject?): String = main {'))
        self.assertIn('failureText(model.error)', model)
        self.assertIn('failureText(model.contextNotice)', model)
        self.assertNotIn('model.draft(', model)
        self.assertNotIn('getPreparingTargets', source)
        self.assertTrue(original_fixture(source))

    def test_error_text_classification_never_emits_unknown_payload_and_hashes_only_a_bounded_prefix(self):
        source = original_review_fixture()
        classify, branches = diagnostic_text_memory(source)
        for condition, kind in branches:
            if condition.startswith('value == '):
                value = json.loads(condition[len('value == '):])
                self.assertTrue(classify(value).startswith('k=' + kind + ' '))
        for value in ('', 'unknown controlled-fixture error', 'Answer not sent. private-marker-example.test',
                      'untrusted-marker-example.test\n' + 'x' * 5000):
            result = classify(value)
            self.assertLess(len('owned-failure error=' + result), 160)
            self.assertNotIn('private-marker-example.test', result)
            self.assertNotIn('untrusted-marker-example.test', result)
            self.assertNotIn('\n', result)
            self.assertIn('p=' + str(min(len(value), 4096)), result)
        self.assertTrue(classify('Answer not sent. private-marker-example.test').startswith('k=answer-failed-unknown '))
        with self.assertRaises(AssertionError):
            diagnostic_text_memory(source.replace('"k=$kind n=${value.length} p=${prefix.length} sha256Prefix=$digest"', '"raw=$value"'))

    def test_post_stage_counters_saturate_unknown_without_changing_delegates_or_refusing_actions(self):
        source = original_review_fixture()
        definition = source.split('// BEGIN QUESTION_SEND_DIAGNOSTIC definition\n', 1)[1].split('// END QUESTION_SEND_DIAGNOSTIC definition', 1)[0]
        self.assertIn('AtomicIntegerArray(7)', definition)
        self.assertIn('postStages.updateAndGet(index) { if (it < 128) it + 1 else 128 } == 128', definition)
        self.assertIn('catch (_: Throwable) { postStagesUnknown = true }', definition)
        self.assertNotIn('throw', definition)
        self.assertIn('val values = IntArray(7) { postStages.get(it) }', definition)
        self.assertIn('postStagesUnknown || values.any { it >= 128 }', definition)
        self.assertLess(definition.index('val values = '), definition.index('val status = '))
        for index in range(7): self.assertIn('${values[' + str(index) + ']}', definition)
        current, unknown = 0, False
        for _ in range(140):
            current = current + 1 if current < 128 else 128
            unknown = unknown or current == 128
        self.assertEqual((current, unknown), (128, True))
        for marker, call in [('post', 'postStage(0)'), ('admitted', 'postStage(3)'), ('recorded', 'postStage(4)')]:
            block = source.split('// BEGIN QUESTION_SEND_DIAGNOSTIC ' + marker + '\n', 1)[1].split('// END QUESTION_SEND_DIAGNOSTIC ' + marker, 1)[0]
            self.assertEqual(block.strip(), call)
        self.assertIn('postStage(when (input.string("operation")) { "inspect" -> 1; "answer" -> 2; "voice_profiles" -> 5; else -> 6 })', source)
        self.assertEqual(source.count('synchronized(calls) { calls.add(JSONObject(payload.toString())) }'), 1)

    def test_pass_serializer_keeps_multiline_probe_inside_one_field_for_the_actual_parser(self):
        source = original_review_fixture()
        lines = [line for line in source.splitlines() if 'result.putString("stream", "Approval choices smoke passed:' in line]
        self.assertEqual(len(lines), 1)
        flat = r' | ${actionProbeEvidence().replace("\n", " | ")}'
        old = lines[0].replace(flat, r'\n${actionProbeEvidence()}')
        self.assertNotEqual(old, lines[0])
        spec = importlib.util.spec_from_file_location("probe_pass_parser", ROOT / "scripts/ci/android-project-ui.py")
        parser = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(parser)
        parser.select_suite(["--approval-choices"])
        cases = parser.CASES.split(",")
        self.assertEqual(len(cases), 7)
        probe = "compose-action status=observed closed=true fault=NONE records=1 schema=1\ncompose-action Record(stage=ENTRY, synthetic)"
        def serialized(line):
            prefix, suffix = '            result.putString("stream", "', '")'
            self.assertTrue(line.startswith(prefix) and line.endswith(suffix))
            template = line[len(prefix):-len(suffix)]
            replacements = {r'${cases.joinToString(",")}': ",".join(cases),
                r'${touchResultEvidence().replace("\n", " | ")}': "owned-touch observed | restored=true",
                r'${actionProbeEvidence().replace("\n", " | ")}': probe.replace("\n", " | "),
                r'${actionProbeEvidence()}': probe}
            for expression, value in replacements.items():
                template = template.replace(expression, value)
            self.assertNotIn("${", template)
            stream = template.replace(r"\n", "\n")
            setup = [(stage, "-", 0) for stage in ("ON_START", "STORE_BEGIN", "STORE_READY", "BOOTSTRAP_BEGIN",
                "ACTIVITY_LAUNCH_BEGIN", "ACTIVITY_LAUNCH_READY", "BOOTSTRAP_READY")]
            steps = setup + [(stage, case, index + (stage == "CASE_COMPLETE")) for index, case in enumerate(cases)
                for stage in ("CASE_BEGIN", "CASE_COMPLETE")]
            progress = ''.join(f'INSTRUMENTATION_STATUS: zerus_approval_progress=1|{index}|{stage}|{case}|{complete}|{1000 + index}\n'
                'INSTRUMENTATION_STATUS_CODE: 1\n' for index, (stage, case, complete) in enumerate(steps, 1))
            return (progress + "INSTRUMENTATION_RESULT: approval_cases=7\nINSTRUMENTATION_RESULT: approval_case_ids=" + ",".join(cases) +
                "\nINSTRUMENTATION_RESULT: stream=" + stream + "\nINSTRUMENTATION_CODE: -1\n")
        with self.assertRaises(AssertionError):
            parser.instrumentation_result(serialized(old))
        actual = parser.instrumentation_result(serialized(lines[0]))
        self.assertEqual(actual["approval_cases"], "7")
        self.assertEqual(actual["approval_case_ids"], parser.CASES)
        self.assertIn(" | compose-action Record(", actual["stream"])
        self.assertIn(r'\n${actionProbeEvidence()}\n")', source)  # Original FAIL stream remains raw.

    def test_original_actions_guards_bindings_and_seven_case_oracles_reconstruct_exactly(self):
        ui = (ROOT / MAIN / "QuestionUi.kt").read_text()
        fixture = original_review_fixture()
        self.assertTrue(original_ui(ui))
        self.assertTrue(original_fixture(fixture))
        # A changed delegate/guard/oracle outside the observation cannot pass this admission.
        for old, new in [("::primaryAction", "{ primaryAction() }"), ("else if (reviewed)", "else if (true)"),
                         ("Button(onClick = onClick", "Button(onClick = {}")]:
            self.assertIn(old, ui)
            self.assertFalse(original_ui(ui.replace(old, new, 1)))
        self.assertFalse(original_fixture(fixture.replace('cases == CASES', 'cases.size <= CASES.size', 1)))

    def test_entry_keeps_exact_original_free_variables_and_commit_reads_before_side_effect(self):
        ui = (ROOT / MAIN / "QuestionUi.kt").read_text()
        self.assertIn("if (QuestionUiProbe.enabled) QuestionUiProbe.entry(question, options, step.nextPage != null, step.canAdvance, editing, submitPending, choices, reviewed, broader.open)", ui)
        self.assertIn("if (QuestionUiProbe.enabled) QuestionUiProbe.requested(question, options, confirmation)", ui)
        committed = ui.split("// BEGIN QUESTION_UI_PROBE committed\n", 1)[1].split("// END QUESTION_UI_PROBE committed", 1)[0]
        self.assertLess(committed.index("val committed = QuestionUiProbe.Committed("), committed.index("SideEffect"))
        self.assertIn("SideEffect { QuestionUiProbe.committed(question, committedOptions, committed) }", committed)
        self.assertNotIn("remember", committed)
        self.assertNotIn("DisposableEffect", committed)
        self.assertEqual(ui.count("::primaryAction"), 2)

    def test_inactive_observer_returns_before_any_clock_fingerprint_record_or_detail(self):
        probe = (ROOT / MAIN / "QuestionUiProbe.kt").read_text()
        observe = probe.split("private inline fun observe(", 1)[1].split("    fun entry(", 1)[0]
        guard = "if (!BuildConfig.DEBUG || !owned.healthy()) return"
        self.assertLess(observe.index("val owned = session ?: return"), observe.index(guard))
        for work in ("SystemClock.uptimeMillis()", "fingerprint(", "selection(options)", "Record(", "detail()"):
            self.assertLess(observe.index(guard), observe.index(work))
        self.assertIn("@Volatile private var session: Session? = null", probe)
        self.assertIn("check(BuildConfig.DEBUG && session == null)", probe)
        self.assertIn("check(session === owned)", probe)

    def test_recorder_is_bounded_typed_and_catches_only_its_own_observation(self):
        probe = (ROOT / MAIN / "QuestionUiProbe.kt").read_text()
        self.assertIn("const val MAX_RECORDS = 256", probe)
        self.assertIn("if (records.size == MAX_RECORDS) { fault = Fault.OVERFLOW; return }", probe)
        self.assertIn("catch (_: Throwable) { owned.failed() }", probe)
        self.assertIn("parts.size <= 40", probe)
        self.assertIn("options.size <= 4", probe)
        self.assertIn("it.size <= 8", probe)
        self.assertIn("it.length <= 256", probe)
        self.assertIn('MessageDigest.getInstance("SHA-256")', probe)
        self.assertIn("left.selection == right.selection && left.detail == right.detail", probe)
        self.assertNotIn("left.uptimeMs", probe)
        # The diagnostic has no delegate/model/action callback, raw text field or IO/network facility.
        for forbidden in ("submitAnswer(", ".request(", ".send(", "ZerusViewModel", "Context", "java.io", "okhttp", "question.prompts", "choice.label", "choice.description", "() -> Unit"):
            self.assertNotIn(forbidden, probe)

    def test_owned_content_snapshot_exposes_only_the_stable_production_review_start(self):
        ui = (ROOT / MAIN / "QuestionUi.kt").read_text()
        committed = ui.split("// BEGIN QUESTION_UI_PROBE committed\n", 1)[1].split("// END QUESTION_UI_PROBE committed", 1)[0]
        self.assertIn("review.startedAt)", committed)
        self.assertNotIn("reviewNow", committed)
        self.assertTrue(original_ui(ui))
        probe = (ROOT / MAIN / "QuestionUiProbe.kt").read_text()
        getter = probe.split("    @Synchronized fun contentSnapshot(owned:", 1)[1].split("    private fun fingerprint(", 1)[0]
        self.assertLess(getter.index("check(BuildConfig.DEBUG && session === owned)"), getter.index("owned.contentSnapshot()"))
        self.assertIn("owned.firstReviewSnapshot(afterGeneration)", getter)
        self.assertIn("if (snapshot == null) return null", getter)
        self.assertIn("snapshot.record.card == fingerprint(listOf(question.id, question.hash, question.run, question.conversation))", getter)
        self.assertIn("snapshot.record.questionObject == System.identityHashCode(question)", getter)
        self.assertIn("check(snapshot.record.detail is Committed)", getter)
        session = probe.split("    class Session internal constructor()", 1)[1].split("    @Volatile", 1)[0]
        snapshot = session.split("internal fun contentSnapshot():", 1)[1].split("@Synchronized internal fun firstReviewSnapshot", 1)[0]
        self.assertLess(snapshot.index("check(healthy())"), snapshot.index("checkNotNull(content)"))
        self.assertIn("if (record.stage == Stage.CONTENT_COMMIT) { content = record; contentGeneration++ }", session)
        self.assertLess(session.index("same(content, record)"), session.index("contentGeneration++"))
        for forbidden in ("SystemClock", "record.", "append(", "remember", "launch(", "delay("):
            self.assertNotIn(forbidden, snapshot)
        first = session.split("internal fun firstReviewSnapshot(", 1)[1].split("@Synchronized internal fun close", 1)[0]
        self.assertIn("check(healthy() && afterGeneration in 0..contentGeneration)", first)
        self.assertIn("for (record in records)", first)
        self.assertIn("generation > afterGeneration && detail.eligible && detail.reviewStartedAt != null", first)
        self.assertIn("return ContentSnapshot(generation, record)", first)
        self.assertNotIn("last", first)

    def test_fixture_closes_owned_session_and_keeps_original_exception_and_result_schema(self):
        source = original_review_fixture()
        report = source.split("// BEGIN QUESTION_UI_PROBE fixture-report\n", 1)[1].split("// END QUESTION_UI_PROBE fixture-report", 1)[0]
        self.assertIn("val owned = QuestionUiProbe.install()", report)
        self.assertIn("try { smokeBody(store) }", report)
        self.assertIn("catch (error: Throwable) { primary = error; throw error }", report)
        self.assertIn("finally {", report)
        self.assertIn("main { QuestionUiProbe.close(owned) }", report)
        self.assertIn("original.addSuppressed(cleanup)", report)
        self.assertIn('else "UNKNOWN"', report)
        self.assertIn("it.length <= 1024", report)
        self.assertIn("it.length <= 512 * 1024", report)
        self.assertTrue(original_fixture(source))


if __name__ == "__main__":
    unittest.main()
