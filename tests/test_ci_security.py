"""Exercise the checked-in workflow trust predicate and final success gate."""
import base64
import datetime
import hashlib
import importlib.util
import io
import json
from itertools import product, takewhile
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import tempfile
import textwrap
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]


def job(text, name):
    match = re.search(rf"(?ms)^  {name}:\n(.*?)(?=^  [a-zA-Z][\w-]*:|\Z)", text)
    if not match:
        raise AssertionError(f"Missing workflow job: {name}")
    return match[1]


def steps(text):
    """The steps of one job, in order, each starting at its list marker."""
    return re.split(r"(?m)^(?=      - )", text.split("\n    steps:\n", 1)[1])[1:]


def inline_python(text):
    return textwrap.dedent(re.search(r"(?s)python3 - <<'PY'\n(.*?)\n          PY", text)[1])


def field(text, name):
    match = re.search(rf"(?m)^    {name}: (.*)$", text)
    if not match:
        raise AssertionError(f"Missing job field: {name}")
    if match[1] == ">-":
        lines = text[match.end():].splitlines()[1:]
        return " ".join(line.strip() for line in takewhile(lambda line: line.startswith("      "), lines))
    return match[1]


class WorkflowSecurityTests(unittest.TestCase):
    def setUp(self):
        self.ci = (ROOT / ".github/workflows/ci.yml").read_text()
        self.qualification = (ROOT / ".github/workflows/ci-dev-qualification.yml").read_text()
        self.tail = (ROOT / ".github/workflows/ci-dev-tail.yml").read_text()

    def dev_selected(self, workflow, name, results, cancelled=False, event='push',
                     ref='refs/heads/main', repository='ufna/zerus', enabled='true', private=False, macos='true'):
        expression = field(job(workflow, name), 'if')
        # Without a status function GitHub adds success() over the dependency
        # chain, including the deliberately skipped PR/manual branch on main.
        self.assertIn('!cancelled()', expression)
        expression = expression.replace('&&', ' and ').replace('||', ' or ')
        expression = re.sub(r'!(?!=)', 'not ', expression)
        expression = re.sub(r'\bfalse\b', 'False', expression)
        # Job IDs such as dev-android are not Python identifiers.
        expression = re.sub(r"\bneeds\.([\w-]+)\.", r"needs['\1'].", expression)
        return bool(eval(expression, {'__builtins__': {}}, {
            'cancelled': lambda: cancelled,
            'github': SimpleNamespace(repository=repository, ref=ref, event_name=event,
                                      event=SimpleNamespace(repository=SimpleNamespace(private=private))),
            'vars': SimpleNamespace(DEV_UPDATES_ENABLED=enabled, MACOS_CASKS_ENABLED=macos),
            'needs': {key: SimpleNamespace(result=value) for key,value in results.items()},
        }))

    def test_dev_requires_exact_main_gate_without_inheriting_skipped_review(self):
        admitted = {'gate': 'success', 'qualification': 'success'}
        self.assertTrue(self.dev_selected(self.ci, 'dev-tail', admitted))
        for key in admitted:
            for result in ['failure', 'cancelled', 'skipped']:
                with self.subTest(key=key, result=result):
                    self.assertFalse(self.dev_selected(self.ci, 'dev-tail', {**admitted, key: result}))
        for overrides in [dict(cancelled=True), dict(event='pull_request'), dict(event='workflow_dispatch'),
                          dict(event='schedule'), dict(ref='refs/heads/feature'), dict(repository='example/zerus'),
                          dict(enabled=''), dict(private=True)]:
            with self.subTest(overrides=overrides):
                self.assertFalse(self.dev_selected(self.ci, 'dev-tail', admitted, **overrides))
        passed = {'dev-android': 'success', 'dev-macos': 'success'}
        self.assertTrue(self.dev_selected(self.tail, 'dev', passed))
        for key in passed:
            for result in ['failure', 'cancelled', 'skipped']:
                self.assertFalse(self.dev_selected(self.tail, 'dev', {**passed, key: result}))
        self.assertTrue(self.dev_selected(self.tail, 'dev', {**passed, 'dev-macos': 'skipped'}, macos=''))
        self.assertFalse(self.dev_selected(self.tail, 'dev', {**passed, 'dev-macos': 'failure'}, macos=''))
        self.assertTrue(self.dev_selected(self.tail, 'dev-macos', passed))
        for name in ('dev', 'dev-macos'):
            for overrides in [dict(enabled=''), dict(event='pull_request'), dict(event='workflow_dispatch'),
                              dict(ref='refs/heads/feature'), dict(repository='example/zerus'),
                              dict(private=True), dict(cancelled=True)]:
                self.assertFalse(self.dev_selected(self.tail, name, passed, **overrides))
        self.assertFalse(self.dev_selected(self.tail, 'dev-macos', passed, macos=''))

    def test_dev_reusable_jobs_preserve_status_and_candidate_success_checks(self):
        dev = (ROOT / '.github/workflows/dev.yml').read_text()
        android = (ROOT / '.github/workflows/dev-android.yml').read_text()
        passed = {'desktop':'success', 'review':'skipped'}
        for workflow, name in [(dev,'desktop'),(dev,'channel'),(android,'android')]:
            self.assertTrue(self.dev_selected(workflow,name,passed))
            for overrides in [dict(cancelled=True),dict(event='pull_request'),
                              dict(ref='refs/heads/feature'),dict(repository='example/zerus')]:
                with self.subTest(name=name,overrides=overrides):
                    self.assertFalse(self.dev_selected(workflow,name,passed,**overrides))
        # The caller checks the Android candidate; the tag waits for the desktop seal.
        self.assertEqual(field(job(dev,'channel'),'needs'),'[desktop]')
        for result in ['failure','cancelled','skipped']:
            with self.subTest(result=result):
                self.assertFalse(self.dev_selected(dev,'channel',{**passed,'desktop':result}))
        self.assertNotRegex(dev, r'(?m)^  android:')
        self.assertNotIn('zerus-dev-android', dev)

    def test_android_candidate_starts_only_after_successful_dev_main_checks(self):
        admission = job(self.ci, 'dev-tail')
        self.assertEqual(field(admission, 'needs'), '[gate, qualification]')
        self.assertEqual(field(admission, 'uses'), './.github/workflows/ci-dev-tail.yml')
        caller = job(self.tail, 'dev-android')
        self.assertEqual(field(caller, 'uses'), './.github/workflows/dev-android.yml')
        self.assertIn('    permissions:\n      contents: read\n', caller)
        self.assertNotIn('actions: read', caller)
        passed = {'gate': 'success', 'qualification': 'success'}
        self.assertTrue(self.dev_selected(self.ci, 'dev-tail', passed))
        for key in passed:
            for result in ('failure', 'cancelled', 'skipped'):
                self.assertFalse(self.dev_selected(self.ci, 'dev-tail', {**passed, key: result}))
        self.assertTrue(self.dev_selected(self.tail, 'dev-android', {}))
        for overrides in [dict(cancelled=True), dict(event='pull_request'), dict(event='workflow_dispatch'),
                          dict(event='schedule'), dict(ref='refs/heads/feature'), dict(repository='example/zerus'),
                          dict(enabled=''), dict(private=True)]:
            self.assertFalse(self.dev_selected(self.tail, 'dev-android', {}, **overrides))

    def test_dev_passes_only_required_android_secrets_to_protected_build(self):
        workflow = (ROOT / '.github/workflows/dev-android.yml').read_text()
        names = {'ANDROID_KEYSTORE_BASE64','ANDROID_STORE_PASSWORD','ANDROID_KEY_ALIAS',
                 'ANDROID_KEY_PASSWORD','ANDROID_GOOGLE_SERVICES_JSON'}
        caller = job(self.tail,'dev-android')
        passed = dict(re.findall(r'(?m)^      (\w+): \$\{\{ secrets\.(\w+) \}\}$',caller))
        self.assertEqual(passed,{name:name for name in names})
        self.assertNotIn('secrets: inherit',caller)
        header = workflow.split('\npermissions:',1)[0]
        declared = set(re.findall(r'(?m)^      (\w+):\n        required: true$',header))
        self.assertEqual(declared,names)
        android = job(workflow,'android')
        self.assertEqual(field(android,'environment'),'release')
        self.assertEqual(set(re.findall(r'secrets\.(\w+)', workflow)), names)
        self.assertEqual(set(re.findall(r'secrets\.(\w+)', android)), names)
        # Dev assembly and the tag move no longer receive the signer.
        self.assertNotIn('secrets', job(self.tail,'dev'))
        self.assertNotIn('secrets', (ROOT / '.github/workflows/dev.yml').read_text())
        for path in sorted((ROOT / '.github/workflows').glob('*.yml')):
            text = path.read_text()
            if path.name == 'ci.yml':
                bridge = job(text, 'dev-tail')
                self.assertTrue(names <= set(re.findall(r'secrets\.(\w+)', bridge)))
                text = text.replace(bridge, '')
            elif path.name == 'ci-dev-tail.yml':
                text = text.replace(caller, '')
                # The wrapper declares names but never consumes signing values.
                text = text.split('\njobs:\n', 1)[1]
            elif path.name == 'dev-android.yml':
                continue
            elif path.name == 'android-release.yml':
                stable = job(text, 'android')
                self.assertEqual(field(stable, 'environment'), 'release')
                self.assertEqual(set(re.findall(r'secrets\.(\w+)', text)), names)
                for guard in ("github.repository == 'ufna/zerus'", "github.ref == 'refs/heads/main'",
                              "github.event_name == 'workflow_dispatch'"):
                    self.assertIn(guard, field(stable, 'if'))
                self.assertLess(stable.index('android-stable-candidate.py admit'), stable.index('secrets.ANDROID_'))
                self.assertNotRegex(text, r'contents: write|actions: write|actions/cache|secrets: inherit')
                continue
            for name in names:
                with self.subTest(workflow=path.name, secret=name):
                    self.assertNotIn(name, text)

    def test_dev_main_build_finishes_while_newer_main_waits(self):
        expression = re.search(r'(?m)^  cancel-in-progress: \$\{\{ (.*) \}\}$', self.ci)[1]
        expression = expression.replace('&&', ' and ')
        expression = re.sub(r'!(?!=)', 'not ', expression)
        expression = re.sub(r'\bfalse\b', 'False', expression)
        for event, ref, repository, enabled, private, cancels in [
            ('push', 'refs/heads/main', 'ufna/zerus', 'true', False, False),
            ('push', 'refs/heads/main', 'ufna/zerus', '', False, True),
            ('push', 'refs/heads/main', 'ufna/zerus', 'true', True, True),
            ('push', 'refs/heads/main', 'example/zerus', 'true', False, True),
            ('push', 'refs/heads/feature', 'ufna/zerus', 'true', False, True),
            ('pull_request', 'refs/pull/123/merge', 'ufna/zerus', 'true', False, True),
            ('workflow_dispatch', 'refs/heads/main', 'ufna/zerus', 'true', False, True),
        ]:
            github = SimpleNamespace(repository=repository, ref=ref, event_name=event,
                                     event=SimpleNamespace(repository=SimpleNamespace(private=private)))
            with self.subTest(event=event, repository=repository, enabled=enabled, private=private):
                self.assertEqual(bool(eval(expression, {'__builtins__': {}}, {
                    'github': github, 'vars': SimpleNamespace(DEV_UPDATES_ENABLED=enabled)})), cancels)

    def test_dev_publisher_accepts_only_successful_main_events_or_main_retry(self):
        workflow = (ROOT / '.github/workflows/publish-dev.yml').read_text()
        expression = field(job(workflow, 'publish'), 'if').replace('&&', ' and ').replace('||', ' or ')
        expression = re.sub(r'\bfalse\b', 'False', expression)
        fixtures = [
            ('workflow_run', 'push', 'success', 'main', 'ufna/zerus', 'refs/heads/main', 'true', False, True),
            ('workflow_run', 'push', 'failure', 'main', 'ufna/zerus', 'refs/heads/main', 'true', False, False),
            ('workflow_run', 'push', 'cancelled', 'main', 'ufna/zerus', 'refs/heads/main', 'true', False, False),
            ('workflow_run', 'pull_request', 'success', 'main', 'ufna/zerus', 'refs/heads/main', 'true', False, False),
            ('workflow_run', 'push', 'success', 'feature', 'ufna/zerus', 'refs/heads/main', 'true', False, False),
            ('workflow_run', 'push', 'success', 'main', 'example/zerus', 'refs/heads/main', 'true', False, False),
            ('workflow_run', 'push', 'success', 'main', 'ufna/zerus', 'refs/heads/main', '', False, False),
            ('workflow_run', 'push', 'success', 'main', 'ufna/zerus', 'refs/heads/main', 'true', True, False),
            ('workflow_dispatch', 'push', 'success', 'main', 'ufna/zerus', 'refs/heads/main', 'true', False, True),
            ('workflow_dispatch', 'push', 'success', 'main', 'ufna/zerus', 'refs/heads/feature', 'true', False, False),
            ('pull_request', 'push', 'success', 'main', 'ufna/zerus', 'refs/heads/main', 'true', False, False),
        ]
        for event, source_event, conclusion, branch, repository, ref, enabled, private, allowed in fixtures:
            source = SimpleNamespace(event=source_event, conclusion=conclusion, head_branch=branch,
                                     head_repository=SimpleNamespace(full_name=repository))
            github = SimpleNamespace(repository='ufna/zerus', ref=ref, event_name=event,
                                     event=SimpleNamespace(repository=SimpleNamespace(private=private), workflow_run=source))
            with self.subTest(event=event, source=source, ref=ref, enabled=enabled, private=private):
                self.assertEqual(bool(eval(expression, {'__builtins__': {}}, {
                    'github': github, 'vars': SimpleNamespace(DEV_UPDATES_ENABLED=enabled)})), allowed)
        self.assertEqual(field(job(workflow, 'publish'), 'environment'), 'release')
        self.assertIn('actions: read', workflow)
        self.assertIn('contents: read', workflow)
        self.assertNotRegex(workflow, r'(?m)^\s*(actions|contents): write')
        self.assertEqual(re.search(r'(?m)^cache-mode: (.*)$', workflow)[1], 'none')

    def selected(self, name, event, ref="refs/heads/main", test_ref="", repository="ufna/zerus",
                 enabled="", private=False, cancelled=False):
        # Evaluate the checked-in condition with synthetic event identities.
        expression = field(job(self.ci, name), "if")
        expression = expression.replace("&&", " and ").replace("||", " or ")
        expression = re.sub(r"!(?!=)", "not ", expression)
        expression = re.sub(r"\bfalse\b", "False", expression)
        return bool(eval(expression, {"__builtins__": {}}, {
            "always": lambda: True, "cancelled": lambda: cancelled,
            "github": SimpleNamespace(repository=repository, ref=ref, event_name=event, sha="a" * 40,
                                      event=SimpleNamespace(repository=SimpleNamespace(private=private))),
            "inputs": SimpleNamespace(ref=test_ref),
            "vars": SimpleNamespace(DEV_UPDATES_ENABLED=enabled),
        }))

    def test_only_exact_upstream_main_push_or_schedule_can_write_caches(self):
        fixtures = [
            ("push", "refs/heads/main", "", "ufna/zerus", True),
            ("schedule", "refs/heads/main", "a" * 40, "ufna/zerus", True),
            ("push", "refs/heads/main", "refs/pull/123/head", "ufna/zerus", False),
            ("pull_request", "refs/pull/123/merge", "", "ufna/zerus", False),
            ("pull_request_target", "refs/heads/main", "", "ufna/zerus", False),
            ("workflow_dispatch", "refs/heads/main", "", "ufna/zerus", False),
            ("workflow_dispatch", "refs/heads/main", "refs/pull/123/head", "ufna/zerus", False),
            ("workflow_dispatch", "refs/heads/main", "b" * 40, "ufna/zerus", False),
            ("push", "refs/heads/feature", "", "ufna/zerus", False),
            ("push", "refs/heads/main", "", "example/zerus", False),
        ]
        for event, ref, test_ref, repository, trusted in fixtures:
            with self.subTest(event=event, ref=ref, test_ref=test_ref, repository=repository):
                self.assertEqual(self.selected("automatic", event, ref, test_ref, repository), trusted)
                self.assertEqual(self.selected("review", event, ref, test_ref, repository), not trusted)
                self.assertEqual(self.selected("cache-trim", event, ref, test_ref, repository), trusted)
        self.assertEqual(field(job(self.ci, "automatic"), "cache-mode"), "write")
        self.assertEqual(field(job(self.ci, "review"), "cache-mode"), "read")
        for name in ("automatic", "review"):
            self.assertEqual(field(job(self.ci, name), "uses"), "./.github/workflows/checks.yml")
        # A callee cannot request broader permissions than its caller's limit.
        checks = (ROOT / ".github/workflows/checks.yml").read_text()
        self.assertEqual({name: field(job(checks, name), 'cache-mode') for name in
                          ('cli-macos-native', 'cli-macos')},
                         {'cli-macos-native': 'none', 'cli-macos': 'none'})
        self.assertNotRegex(job(checks, 'cli-macos-debug'), r'(?m)^    cache-mode:')
        self.assertNotRegex(checks, r"(?m)^\s*cache-mode: write$")

    def test_final_gate_rejects_failure_cancellation_and_all_skipped(self):
        gate = job(self.ci, "gate")
        self.assertEqual(field(gate, "name"), "CI gate")
        self.assertEqual(field(gate, "needs"), "[automatic, review, qualification]")
        self.assertEqual(field(gate, "cache-mode"), "none")
        code = compile(inline_python(gate), "workflow-CI-gate", "exec")
        for conclusions in product(('success', 'skipped', 'failure', 'cancelled'), repeat=3):
            results = {name: {'result': result} for name, result in zip(
                ('automatic', 'review', 'qualification'), conclusions)}
            success = sorted(conclusions) == ['skipped', 'skipped', 'success']
            with self.subTest(results=results), patch.dict(os.environ, {"RESULTS": json.dumps(results)}):
                if success:
                    exec(code, {})
                else:
                    with self.assertRaises(SystemExit):
                        exec(code, {})

    def test_windows_and_ios_coverage_are_explicit_and_independent(self):
        checks = (ROOT / '.github/workflows/checks.yml').read_text()
        source = job(checks, 'source')
        for platform in ('windows', 'ios'):
            self.assertIn(f"RUN_{platform.upper()}: ${{{{ inputs.run_{platform} }}}}", source)
            self.assertIn(f"run_{platform}: ${{{{ steps.revision.outputs.run_{platform} }}}}", source)
            self.assertRegex(self.ci, rf"(?m)^      run_{platform}:\n        description: .+\n        type: boolean\n        default: true$")
        for name in ('nightly.yml', 'release.yml'):
            workflow = (ROOT / '.github/workflows' / name).read_text()
            self.assertIn('      run_windows: true\n', job(workflow, 'verify'))
            self.assertIn('      run_ios: true\n', job(workflow, 'verify'))
            assemble = job((ROOT / '.github/workflows/release-assemble.yml').read_text(), 'assemble') if name == 'release.yml' else job(workflow, 'assemble')
            self.assertIn('name: windows-x86_64-${{ github.sha }}\n          path: artifacts/windows\n', assemble)
            self.assertIn('--windows-zip "${portables[0]}"', assemble)
        review = job(self.ci, 'review')
        self.assertIn("filter_pr_platforms: ${{ github.event_name == 'pull_request' }}", review)
        self.assertIn('fetch-depth: 0', source)
        self.assertIn('python3 scripts/ci/select-coverage.py', source)

    def test_windows_job_is_bounded_credential_free_and_pinned(self):
        windows = job((ROOT / ".github/workflows/checks.yml").read_text(), "windows-desktop")
        self.assertEqual(field(windows, "if"), "needs.source.outputs.run_windows == 'true'")
        self.assertEqual(field(windows, "runs-on"), "windows-2025")
        self.assertRegex(field(windows, "timeout-minutes"), r"^\d+$")
        self.assertNotIn("secrets.", windows)
        self.assertNotIn("permissions", windows)
        # Line endings are fixed before checking out the exact tested commit.
        self.assertLess(windows.index("core.autocrlf false"), windows.index("actions/checkout@"))
        self.assertIn("ref: ${{ needs.source.outputs.commit }}\n          persist-credentials: false", windows)
        self.assertIn("uses: actions/setup-python@6d4fd22b20465ccc0f7354476588e8df7c9ca0e0 # v7.1.0", windows)
        self.assertNotIn("install-qt", windows)
        self.assertNotIn("windeployqt", windows)
        self.assertNotIn("gui.sh", windows)
        self.assertNotIn("pip install", windows)
        self.assertIn("uses: actions/setup-node@949feb2413d6458794dcd2491c4babbbce0c15c1", windows)
        self.assertIn("package-manager-cache: false", windows)
        self.assertIn("npm ci </dev/null", windows)
        self.assertIn("npm run tauri -- build --ci --no-bundle --config src-tauri/tauri.windows.conf.json --features video </dev/null", windows)
        self.assertIn("CARGO_BUILD_JOBS: 4", windows)
        self.assertIn("timeout-minutes: 25", windows)
        self.assertIn("--gui target/windows-next/release/hgs-tray.exe", windows)
        # Unit/lint contracts run in parallel, but stay mandatory in the suite gate.
        unit = job((ROOT / '.github/workflows/checks.yml').read_text(), 'windows-tests')
        self.assertEqual(field(unit, 'needs'), 'source')
        self.assertEqual(field(unit, 'if'), field(windows, 'if'))
        self.assertEqual(field(unit, 'runs-on'), 'windows-2025')
        self.assertNotIn('secrets.', unit)
        self.assertNotIn('environment:', unit)
        self.assertNotIn('actions/cache/save', unit)
        self.assertIn("cargo +1.99.1 clippy --locked --all-targets -- -D warnings", unit)
        self.assertNotRegex(unit, r"(?m)^\s*cargo clippy")
        self.assertNotIn('cargo test --locked', windows)
        self.assertIn("'windows-tests': json.loads(os.environ['RUN_WINDOWS'])", job((ROOT / '.github/workflows/checks.yml').read_text(), 'gate'))
        self.assertIn("rustup toolchain install 1.99.1 --profile minimal --component clippy", windows)
        # The runner's stdin is a synchronous pipe that Windows OpenSSH never exits on.
        for line in ("cargo test --locked </dev/null", "test_windows_client.py -v </dev/null",
                     "test_windows_lifecycle test_windows_hooks test_backend_contract </dev/null",
                     "test_windows_mobile_gateway.py -v </dev/null",
                     "--output artifacts/zerus-windows-x86_64.zip </dev/null",
                     "--run --selftest \\\n            </dev/null"):
            self.assertIn(line, unit if line.startswith('cargo test') else windows)
        for line in ("name: windows-x86_64-${{ needs.source.outputs.commit }}",
                     "retention-days: ${{ github.event_name == 'schedule' && 3 || 7 }}",
                     "check-windows-portable.py artifacts/zerus-windows-x86_64.zip --run"):
            self.assertIn(line, windows)

    def test_android_check_runs_in_every_call_without_credentials(self):
        checks = (ROOT / ".github/workflows/checks.yml").read_text()
        android = job(checks, "android")
        self.assertEqual(field(android, "name"), "Android unit tests and lint")
        self.assertEqual(field(android, "runs-on"), "ubuntu-24.04")
        self.assertRegex(field(android, "timeout-minutes"), r"^\d+$")
        # PR, main, nightly and release verification calls all run it, and the gate requires it.
        self.assertNotRegex(android, r"(?m)^    if:")
        self.assertIn("'android': True,", job(checks, "gate"))
        # No signer, Firebase project, protected environment or broader token.
        # continue-on-error would report a failed check to the gate as success.
        self.assertRegex(checks, r"(?m)^permissions:\n  contents: read\n\njobs:\n")
        for text in ("secrets", "vars.", "environment", "permissions", "github.token", "ANDROID_", "continue-on-error"):
            with self.subTest(forbidden=text):
                self.assertNotIn(text, android.replace("$ANDROID_HOME/", ""))
        self.assertEqual(re.findall(r"uses: ([\w/-]+)@[0-9a-f]{40} # v", android),
                         ["actions/checkout", "actions/cache/restore", "actions/upload-artifact",
                          "actions/cache/save", "actions/upload-artifact"])
        listed = self.checks_steps("android")
        names = [re.match(r"      - (?:name|uses): (.*)", step)[1] for step in listed]
        configure, gradle = (names.index("JDK 17 and synthetic Firebase configuration"),
                             names.index("Unit tests and release lint"))
        self.assertLess(configure, gradle)
        self.assertLess(names.index("Restore Gradle cache"), gradle)
        self.assertLess(gradle, names.index("Save Gradle cache"))
        # cargo-ndk comes from the cache when it holds this version.
        self.assertLess(names.index("Restore Gradle cache"), names.index("Rust and cargo-ndk for the task replica"))
        # The signed build's toolchain and Firebase variant, with placeholder values only.
        signed = self.dev_android()
        sdkmanager = r'(?m)^ +"\$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" (.+)$'
        self.assertEqual(re.findall(sdkmanager, android), re.findall(sdkmanager, signed))
        self.assertIn('echo "JAVA_HOME=$JAVA_HOME_17_X64" >> "$GITHUB_ENV"', listed[configure])
        self.assertIn("os.environ['JAVA_HOME_17_X64']", signed)
        self.assertIn("        working-directory: mobile/android\n"
                      "        run: ./gradlew --no-daemon testDebugUnitTest lintRelease checkReplicaPackaging -PzerusFirebase=true\n", listed[gradle])
        # The task replica is built for every packaged ABI with the pinned toolchain and NDK, in
        # the check and in the signed build alike (the sdkmanager lines match above).
        for line in ("rustup toolchain install 1.99.1 --profile minimal --target aarch64-linux-android,armv7-linux-androideabi,x86_64-linux-android",
                     "cargo +1.99.1 install cargo-ndk --version 4.1.2 --locked"):
            with self.subTest(line=line):
                self.assertIn(line, android)
                self.assertIn(line, signed)
        self.assertIn("'ndk;28.2.13676358'", android)
        self.assertIn("-PzerusFirebase=true", signed)
        # Retain the three actual JVM suites on success as well as failure, using
        # only explicit files staged outside all cache paths. Missing any
        # suite must fail collection rather than silently yielding partial proof.
        collect = names.index("Collect approval publication and binding unit results")
        retain = names.index("Retain approval publication and binding unit results")
        self.assertLess(gradle, collect)
        self.assertLess(collect, retain)
        for step in (listed[collect], listed[retain]):
            self.assertIn("        if: always()\n", step)
        result_names = ("InspectionPublicationTest", "QuestionSendConfirmationTest", "QuestionReviewProbeTest")
        self.assertEqual(re.findall(r"^          cp (.+)$", listed[collect], re.M), [
            f"mobile/android/app/build/test-results/testDebugUnitTest/TEST-app.zerus.mobile.{name}.xml "
            "artifacts/android-approval-unit-results/" for name in result_names])
        self.assertNotIn("|| true", listed[collect])
        self.assertEqual(self.step_paths(listed[retain]), [
            f"artifacts/android-approval-unit-results/TEST-app.zerus.mobile.{name}.xml"
            for name in result_names])
        self.assertIn("          if-no-files-found: error\n", listed[retain])
        # The notices must list exactly the resolved release classpath, with its artifacts,
        # of the same Firebase build; the generator downloads only missing POM files.
        notices = names.index("Android library notices match the release classpath")
        self.assertLess(gradle, notices)
        self.assertLess(notices, names.index("Save Gradle cache"))
        self.assertIn("        working-directory: mobile/android\n        run: |\n"
                      "          ./gradlew --no-daemon -q --init-script ../../scripts/android-resolved-artifacts.gradle"
                      " -PzerusFirebase=true :app:zerusResolvedNotices\n"
                      "          python3 -I ../../scripts/android-notices.py --check\n", listed[notices])
        config = re.search(r"(?s)cat > mobile/android/app/google-services\.json <<'JSON'\n(.*?)\n          JSON\n",
                           listed[configure])[1]
        self.assertEqual(json.loads(textwrap.dedent(config)), {
            "project_info": {"project_number": "000000000000", "project_id": "zerus-ci-example"},
            "client": [{"client_info": {"mobilesdk_app_id": "1:000000000000:android:0000000000000000",
                                        "android_client_info": {"package_name": "app.zerus.mobile"}},
                        "api_key": [{"current_key": "synthetic-placeholder"}]}],
            "configuration_version": "1"})
        # Reports leave the job only after a failure, from inside artifacts/.
        for step in listed[-2:]:
            self.assertIn("        if: failure()\n", step)
        self.assertIn("name: android-results\n          path: artifacts/test-results\n", listed[-1])

    def test_android_gradle_cache_is_a_monthly_content_keyed_family(self):
        restore = self.checks_step("android", "gradle-cache")
        self.assertIn('echo "epoch=$(date -u +%Y-%m)" >> "$GITHUB_OUTPUT"', self.checks_step("android", "gradle"))
        # Cargo's registry and the installed cargo-ndk (with Cargo's records of it) travel along.
        self.assertEqual(self.step_paths(restore), ["~/.gradle/caches", "~/.gradle/wrapper", "~/.cargo/registry/index",
                                                    "~/.cargo/registry/cache", "~/.cargo/bin/cargo-ndk",
                                                    "~/.cargo/.crates.toml", "~/.cargo/.crates2.json"])
        family = "android-gradle-v1-${{ steps.gradle.outputs.epoch }}-"
        content = ("${{ hashFiles('mobile/android/gradle/wrapper/gradle-wrapper.properties', "
                   "'mobile/android/**/*.gradle.kts', 'crates/replica/Cargo.lock', 'crates/replica-ffi/Cargo.lock') }}")
        self.assertIn(f"          key: {family}{content}\n          restore-keys: {family}\n", restore)
        # cache-trim groups main entries by the key without its last segment:
        # that segment must be the content hash, so the month stays in the family.
        rendered = (family + content).replace("${{ steps.gradle.outputs.epoch }}", "2026-10").replace(content, "f" * 64)
        self.assertEqual(rendered.rsplit("-", 1)[0], "android-gradle-v1-2026-10")
        checks = (ROOT / ".github/workflows/checks.yml").read_text()
        self.assertEqual(checks.count("android-gradle-v1-"), 2)

    def test_checks_gate_requires_exactly_the_selected_suites(self):
        gate = job((ROOT / ".github/workflows/checks.yml").read_text(), "gate")
        self.assertIn("needs: [source, quick, client-frontend, relay, connector, connector-macos, ios, android, client-native-linux, client-native-macos-core, client-native-macos-tauri, client-native-windows, cli, "
                      "cli-macos-debug, cli-macos-core, cli-macos-native, cli-macos, clippy-macos, arch-package, windows-tests, windows-desktop, windows-host]", gate)
        self.assertIn("RUN_WINDOWS: ${{ needs.source.outputs.run_windows == 'true' }}", gate)
        script = re.search(r"(?s)python3 - <<'PY'\n(.*?)\n          PY", gate)[1]
        code = compile(textwrap.dedent(script), "workflow-checks-gate", "exec")
        base = {"source": "success", "quick": "success", "client-frontend": "success", "relay": "success", "connector": "success",
                "android": "success", "client-native-linux": "success", "client-native-macos-core": "success",
                "client-native-macos-tauri": "success", "cli": "success", "cli-macos-debug": "success", "cli-macos-core": "success",
                "cli-macos-native": "success", "cli-macos": "success", "clippy-macos": "success", "arch": "success", "arch-package": "success",
                "mac-desktop": "success", "connector-macos": "success", "ios": "success"}
        # All Windows jobs are selected together, and each must pass on its own.
        windows = ("client-native-windows", "windows-tests", "windows-desktop", "windows-host")
        for name in windows:
            for selected, result, success in [("true", "success", True), ("true", "failure", False),
                                              ("true", "skipped", False), ("true", "cancelled", False),
                                              ("false", "skipped", True), ("false", "success", False)]:
                other = "success" if selected == "true" else "skipped"
                results = {key: {"result": value}
                           for key, value in {**base, **dict.fromkeys(windows, other), name: result}.items()}
                environment = {"RESULTS": json.dumps(results), "RUN_LINUX": "true", "RUN_ARCH": "true",
                               "RUN_MACOS": "true", "RUN_IOS": "true", "RUN_WINDOWS": selected}
                with self.subTest(job=name, selected=selected, result=result), patch.dict(os.environ, environment):
                    if success:
                        exec(code, {})
                    else:
                        with self.assertRaises(SystemExit):
                            exec(code, {})
        # Quick checks, the connector and the Android check always run; the Arch
        # package is selected exactly with Arch desktop.
        base.update(dict.fromkeys(windows, "success"))
        arch_skipped = {**base, "arch": "skipped", "arch-package": "skipped"}
        for results, run_arch, success in [
            ({**base, "client-frontend": "failure"}, "true", False),
            ({**base, "client-frontend": "skipped"}, "true", False),
            ({**base, "client-frontend": "cancelled"}, "true", False),
            ({**base, "quick": "failure"}, "true", False), ({**base, "quick": "skipped"}, "true", False),
            ({**base, "connector": "failure"}, "true", False), ({**base, "connector": "skipped"}, "true", False),
            ({**base, "connector": "cancelled"}, "true", False), ({**arch_skipped, "connector": "skipped"}, "false", False),
            ({**base, "android": "failure"}, "true", False), ({**base, "android": "skipped"}, "true", False),
            ({**base, "android": "cancelled"}, "true", False), ({**arch_skipped, "android": "skipped"}, "false", False),
            ({**base, "client-native-linux": "failure"}, "true", False),
            ({**base, "client-native-macos-core": "failure"}, "true", False),
            ({**base, "client-native-macos-tauri": "failure"}, "true", False),
            ({**base, "clippy-macos": "failure"}, "true", False),
            ({**base, "clippy-macos": "skipped"}, "true", False),
            ({**base, "clippy-macos": "cancelled"}, "true", False),
            ({**base, "arch-package": "failure"}, "true", False), ({**base, "arch-package": "skipped"}, "true", False),
            ({**base, "arch-package": "cancelled"}, "true", False), (arch_skipped, "false", True),
            ({**arch_skipped, "arch-package": "success"}, "false", False),
        ]:
            environment = {"RESULTS": json.dumps({name: {"result": value} for name, value in results.items()}),
                           "RUN_LINUX": "true", "RUN_ARCH": run_arch, "RUN_MACOS": "true", "RUN_IOS": "true", "RUN_WINDOWS": "true"}
            with self.subTest(results=results, run_arch=run_arch), patch.dict(os.environ, environment):
                if success:
                    exec(code, {})
                else:
                    with self.assertRaises(SystemExit):
                        exec(code, {})

    def test_frontend_budget_is_checked_after_the_production_build(self):
        frontend = job((ROOT / ".github/workflows/checks.yml").read_text(), "client-frontend")
        self.assertIn("persist-credentials: false", frontend)
        self.assertIn("package-manager-cache: false", frontend)
        commands = ("npm ci", "npm run check", "npm test", "npm run build", "python3 client/scripts/check-bundle-size.py")
        positions = [frontend.index(command) for command in commands]
        self.assertEqual(positions, sorted(positions))
        self.assertNotIn("continue-on-error", frontend)
        self.assertNotIn("actions/cache/save@", frontend)

    def test_every_crate_is_formatted_linted_and_tested(self):
        # A crate taken by path is only built by its users; its own tests run nowhere else.
        quick = job((ROOT / ".github/workflows/checks.yml").read_text(), "quick")
        crates = sorted(path.parent.name for path in (ROOT / "crates").glob("*/Cargo.toml"))
        self.assertIn("desktop-read-policy", crates)
        for crate in crates:
            manifest = f"--manifest-path crates/{crate}/Cargo.toml"
            with self.subTest(crate=crate):
                self.assertIn(f"cargo +1.99.1 fmt --check {manifest}\n", quick)
                self.assertRegex(quick, rf"cargo \+1\.99\.1 clippy --locked {re.escape(manifest)} --target-dir target --all-targets -- -D warnings\n")
                self.assertRegex(quick, rf"cargo \+1\.99\.1 test --locked {re.escape(manifest)} --target-dir target\n")

    def test_native_client_jobs_cover_media_and_platform_code(self):
        checks = (ROOT / ".github/workflows/checks.yml").read_text()
        macos_steps = {
            "client-native-macos-core": (("Core tests", "core", ""), ("Media and video check", "media", "")),
            "client-native-macos-tauri": (("Tauri tests", "tauri-test", "        timeout-minutes: 25\n"),
                                          ("Tauri clippy", "tauri-clippy", "        if: always()\n        timeout-minutes: 25\n")),
        }
        for name, runner, selected, timeout in (("client-native-linux", "ubuntu-24.04", "linux", "45"),
                                                ("client-native-macos-core", "macos-15", "macos", "90"),
                                                ("client-native-macos-tauri", "macos-15", "macos", "90"),
                                                ("client-native-windows", "windows-2025", "windows", "60")):
            native = job(checks, name)
            with self.subTest(job=name):
                self.assertEqual(field(native, "runs-on"), runner)
                self.assertEqual(field(native, "needs"), "source")
                self.assertEqual(field(native, "if"), f"needs.source.outputs.run_{selected} == 'true'")
                self.assertEqual(field(native, "timeout-minutes"), timeout)
                self.assertIn("ref: ${{ needs.source.outputs.commit }}\n          persist-credentials: false", native)
                if name in macos_steps:
                    for step_name, stage, settings in macos_steps[name]:
                        self.assertIn(f"name: {step_name}\n{settings}        run: bash scripts/ci/client-native.sh {selected} {stage}", native)
                else:
                    self.assertIn(f"run: bash scripts/ci/client-native.sh {selected}", native)
                self.assertIn("CARGO_INCREMENTAL: 0", native)
                if selected != "linux":
                    self.assertIn("CARGO_PROFILE_DEV_DEBUG: 0", native)
                    self.assertIn("CARGO_PROFILE_TEST_DEBUG: 0", native)
                self.assertIn("hashFiles('client/core/Cargo.lock', 'client/media/Cargo.lock', 'client/src-tauri/Cargo.lock')", native)
                cache = self.checks_step(name, "rust-cache")
                self.assertNotIn("run_id", cache)
                self.assertEqual(self.step_paths(cache), ["~/.cargo/registry", "~/.cargo/git"])
        self.assertNotIn("actions/cache/save@", job(checks, "client-native-macos-core"))
        self.assertIn("actions/cache/save@", job(checks, "client-native-macos-tauri"))
        linux = job(checks, "client-native-linux")
        for package in ("libasound2-dev", "libgstreamer1.0-dev", "libgstreamer-plugins-base1.0-dev",
                        "gstreamer1.0-plugins-base", "gstreamer1.0-plugins-good", "gstreamer1.0-plugins-bad",
                        "gstreamer1.0-libav", "libgtk-3-dev", "libwebkit2gtk-4.1-dev", "xvfb"):
            self.assertIn(package, linux)
        self.assertNotIn("libasound2-dev", job(checks, "quick"))
        script = (ROOT / "scripts/ci/client-native.sh").read_text()
        for command in (
            "env -u DISPLAY WAYLAND_DISPLAY=zerus-none GDK_BACKEND=x11 WEBKIT_DISABLE_DMABUF_RENDERER=1 ZERUS_CLIENT_HEADLESS=1",
            "xvfb-run -a -s \"-screen 0 1440x900x24\"",
            "cargo +1.99.1 test --locked --manifest-path client/media/Cargo.toml --features video",
            "cargo +1.99.1 clippy --locked --manifest-path client/src-tauri/Cargo.toml --all-targets --features video -- -D warnings",
            "cargo +1.99.1 test --locked --manifest-path client/core/Cargo.toml",
            "cargo +1.99.1 test --locked --manifest-path client/src-tauri/Cargo.toml",
            "cargo +1.99.1 check --locked --manifest-path client/media/Cargo.toml --all-targets --features video",
            "cargo +1.99.1 clippy --locked --manifest-path client/src-tauri/Cargo.toml --all-targets -- -D warnings",
        ):
            self.assertIn(command, script)
        self.assertNotIn("cargo +1.99.1 check --locked --manifest-path client/src-tauri/Cargo.toml --all-targets\n", script)
        for stage in ("macos:core", "macos:media", "macos:tauri-test", "macos:tauri-clippy"):
            self.assertIn(stage, script)

    def test_macos_native_selection_checks_do_not_enable_signed_peer_fixtures(self):
        script = (ROOT / "scripts/ci/client-native.sh").read_text()
        self.assertIn('if [ "$platform" = macos ]; then', script)
        self.assertIn('--features macos-native-peer macos_notify::peer::installation', script)
        self.assertIn('--features macos-native-peer --all-targets -- -D warnings', script)
        self.assertNotIn('--ignored', script)
        self.assertNotIn('ZERUS_MAC_PEER_TEAM_REQUIREMENT=', script)

    def test_windows_shell_candidate_is_disposable_and_unprivileged(self):
        native = job((ROOT / ".github/workflows/checks.yml").read_text(), "client-native-windows")
        self.assertNotIn("secrets.", native)
        self.assertNotIn("environment:", native)
        self.assertIn("run: bash scripts/ci/windows-next-shell.sh", native)
        self.assertIn("retention-days: 3", native)
        script = (ROOT / "scripts/ci/windows-next-shell.sh").read_text()
        self.assertIn("cargo test --locked", script)
        self.assertIn("--debug --no-bundle", script)
        self.assertNotIn("--install-login-item", script)
        self.assertNotIn("--sessions", script)
        # Independent native stages still fail the job; no continue-on-error
        # can hide a failed ancestry test or prevent the pipe checks running.
        self.assertNotIn("continue-on-error", native)
        self.assertIn('exit "$native_status"', script)
        self.assertIn("|| native_status=1", script)
        for stage in ("core", "media", "tauri-clippy"):
            self.assertIn(f"run: bash scripts/ci/client-native.sh windows {stage}", native)
        self.assertIn("name: Windows shell tests and disposable app\n        if: ${{ !cancelled() }}", native)

    def checks_steps(self, name):
        """One checks.yml job's steps, each with its indented body."""
        body = job((ROOT / ".github/workflows/checks.yml").read_text(), name).split("\n    steps:\n", 1)[1]
        return [step for step in re.split(r"(?m)^(?=      - )", body) if step.startswith("      - ")]

    def checks_step(self, name, step_id):
        return next(step for step in self.checks_steps(name) if f"\n        id: {step_id}\n" in step)

    @staticmethod
    def step_paths(step):
        match = re.search(r"(?m)^          path: (.*)$", step)
        if match[1] != "|":
            return [match[1]]
        lines = step[match.end():].splitlines()[1:]
        return [line.strip() for line in takewhile(lambda line: line.startswith("            "), lines)]

    def test_source_only_resolves_and_every_suite_starts_from_it(self):
        checks = (ROOT / ".github/workflows/checks.yml").read_text()
        steps = self.checks_steps("source")
        self.assertEqual(len(steps), 2)
        self.assertIn("uses: actions/checkout@", steps[0])
        self.assertIn("\n        id: revision\n", steps[1])
        quick = job(checks, "quick")
        self.assertEqual(field(quick, "name"), "Quick checks (source and Rust 1.99)")
        for line in ("run: python3 scripts/ci/check-source.py", "-p 'test_*publication.py'", "-p test_ci_security.py",
                     "sha256sum --check --ignore-missing actionlint_1.7.12_checksums.txt",
                     "run: cargo +1.99.1 test --locked",
                     "cargo +1.99.1 test --locked --manifest-path crates/replica/Cargo.toml --target-dir target",
                     "cargo +1.99.1 fmt --check --manifest-path crates/replica-ffi/Cargo.toml",
                     "cargo +1.99.1 clippy --locked --manifest-path crates/replica-ffi/Cargo.toml --target-dir target --all-targets -- -D warnings",
                     "cargo +1.99.1 test --locked --manifest-path crates/replica-ffi/Cargo.toml --target-dir target",
                     "cargo +1.99.1 fmt --check --manifest-path crates/watch-protocol/Cargo.toml",
                     "cargo +1.99.1 clippy --locked --manifest-path crates/watch-protocol/Cargo.toml --target-dir target --all-targets -- -D warnings",
                     "cargo +1.99.1 test --locked --manifest-path crates/watch-protocol/Cargo.toml --target-dir target",
                     'run: python3 scripts/collect-licenses.py "$RUNNER_TEMP/rust-licenses"'):
            self.assertIn(line, quick)
        for name in ("quick", "relay", "connector", "connector-macos", "android", "client-native-linux", "client-native-macos-core",
                     "client-native-macos-tauri", "client-native-windows", "cli", "cli-macos-debug", "arch-package", "windows-desktop", "windows-host"):
            with self.subTest(job=name):
                self.assertEqual(field(job(checks, name), "needs"), "source")
                self.assertIn("ref: ${{ needs.source.outputs.commit }}", job(checks, name))
        for name in ('cli-macos-core', 'cli-macos-native'):
            self.assertEqual(field(job(checks, name), 'needs'), '[source, cli-macos-debug]')
            self.assertIn('ref: ${{ needs.source.outputs.commit }}', job(checks, name))
        # Builds use every runner vCPU; recipes leave the job count to Ninja.
        self.assertNotIn("CARGO_BUILD_JOBS", job(checks, "cli"))
        for name, jobs in (("windows-desktop", 4),):
            self.assertRegex(job(checks, name), rf"(?m)^      ZERUS_BUILD_JOBS: {jobs}$")
        for recipe in ("packaging/aur/zerus-git/PKGBUILD", "packaging/aur/zerus/PKGBUILD.in"):
            with self.subTest(recipe=recipe):
                text = (ROOT / recipe).read_text()
                self.assertIn("npm exec -- tauri build --no-bundle", text)
                self.assertIn("--features video -- --frozen", text)
                self.assertNotIn("--parallel", text)
                # The published recipes keep their check() and never use ccache.
                self.assertIn("\ncheck() {\n", text)
                self.assertIn("checkdepends=('xorg-server-xvfb')", text)
                self.assertNotIn("ccache", text)
        self.assertIn("xorg-server-xvfb", job(checks, "arch-package"))
        self.assertIn("\tcheckdepends = xorg-server-xvfb\n",
                      (ROOT / "packaging/aur/zerus-git/.SRCINFO").read_text())
        # Dev explicitly builds a usable package without the regression check().
        # The normal script and distributed recipes retain their full checks.
        dev_package = (ROOT / '.github/workflows/dev-arch.yml').read_text()
        self.assertIn('ZERUS_RELEASE_CHANNEL=dev', dev_package)
        self.assertIn('scripts/build-arch-package.sh --build-only', dev_package)
        self.assertNotIn('--build-only', job(checks, 'arch-package'))
        # Its cold release build and full check() need headroom over a slow
        # standard runner's 45 minutes (zerus-wnm5z).
        self.assertEqual(field(job(checks, 'arch-package'), 'timeout-minutes'), '75')

    def test_relay_job_runs_every_mobile_service_module(self):
        relay = job((ROOT / ".github/workflows/checks.yml").read_text(), "relay")
        python = '"$RUNNER_TEMP/relay-venv/bin/python" -m unittest discover -s services/mobile/tests'
        # One discover covers every module except the black-box connector suites
        # against Rust; only the speech suites repeat against the Python reference relay.
        self.assertEqual(re.findall(rf"(?m)^          (.*){re.escape(python)}(.*)$", relay),
                         [("", " -p 'test_[!b]*.py' -v"),
                          ("env -u ZERUS_RELAY_BINARY ", " -p 'test_mobile_speech*.py' -v")])
        # The pattern leaves out exactly the black-box modules, which the connector job runs.
        tests = ROOT / "services/mobile/tests"
        self.assertEqual({path.name for path in tests.glob("test_b*.py")},
                         {path.name for path in tests.glob("test_bb_*.py")})
        self.assertTrue(list(tests.glob("test_bb_*.py")))
        for variable in ("ZERUS_RELAY_BINARY", "ZERUS_DIRECTORY_TEST_HGS", "ZERUS_TASK_TEST_HGS"):
            self.assertIn(f"\n          export {variable}=", relay)
        # The relay fixtures' database driver is a test-only requirement.
        self.assertIn('\n          "$RUNNER_TEMP/relay-venv/bin/pip" install ./services/mobile'
                      " -r services/mobile/tests/requirements.txt\n", relay)

    def test_relay_job_runs_contracts_and_fixtures_on_postgresql(self):
        relay = job((ROOT / ".github/workflows/checks.yml").read_text(), "relay")
        # Without the service URL the Rust contracts fall back to SQLite or are
        # ignored and the Rust fixture suites skip; the requirement fails instead.
        postgres = [step for step in steps(relay) if "ZERUS_RELAY_TEST_DATABASE_URL" in step]
        self.assertEqual([re.match(r"      - name: (.*)", step)[1] for step in postgres],
                         ["Rust format, lint and PostgreSQL storage contracts",
                          "Encrypted endpoint protocol and workspace contracts",
                          "Released HTTP and outbound connector compatibility"])
        for step in postgres:
            self.assertIn("\n        env:\n          ZERUS_RELAY_TEST_DATABASE_URL: postgresql://postgres@127.0.0.1:"
                          "${{ job.services.postgres.ports['5432'] }}/postgres\n"
                          '          ZERUS_REQUIRE_RELAY_E2E: "1"\n', step)
        for contract in ("", " --test contracts postgres_cross_worker -- --ignored"):
            self.assertIn(f"\n          cargo +1.99.1 test --locked{contract}\n", postgres[0])
        self.assertIn('\n          "$RUNNER_TEMP/relay-venv/bin/python" -m unittest discover -s services/mobile/tests'
                      " -p 'test_[!b]*.py' -v\n", postgres[-1])
        # The end-to-end suites also run the Rust connector, built beside the relay.
        build = ("\n          cargo +1.99.1 build --locked --manifest-path services/mobile/connector/Cargo.toml"
                 " --target-dir services/mobile/relay/target\n")
        export = ('\n          export ZERUS_CONNECTOR_E2E_COMMAND="$GITHUB_WORKSPACE/services/mobile/relay/target/debug/'
                  'zerus-mobile-connector"\n')
        self.assertIn(build, postgres[-1])
        self.assertLess(postgres[-1].index(build), postgres[-1].index(export))
        self.assertLess(postgres[-1].index(export), postgres[-1].index("-p 'test_[!b]*.py'"))
        self.assertIn("cargo +1.99.1 test --locked --manifest-path services/mobile/crypto/Cargo.toml",postgres[1])
        self.assertIn("cargo +1.99.1 test --locked --manifest-path services/mobile/workspace-tests/Cargo.toml",postgres[1])

    def test_connector_job_runs_the_black_box_suites_against_both_connectors(self):
        connector = job((ROOT / ".github/workflows/checks.yml").read_text(), "connector")
        self.assertEqual(field(connector, "runs-on"), "ubuntu-24.04")
        self.assertNotRegex(connector, r"(?m)^    if:")
        self.assertIn("'connector': True,", job((ROOT / ".github/workflows/checks.yml").read_text(), "gate"))
        for text in ("secrets", "vars.", "environment:", "permissions", "github.token", "continue-on-error"):
            with self.subTest(forbidden=text):
                self.assertNotIn(text, connector)
        self.assertEqual(re.findall(r"uses: ([\w/-]+)@[0-9a-f]{40} # v", connector),
                         ["actions/checkout", "actions/cache/restore", "actions/cache/save"])
        rust = next(step for step in steps(connector) if "name: Format, lint and dependency boundaries" in step)
        for command in ("fmt --check", "clippy --locked --all-targets -- -D warnings",
                        "clippy --locked --all-targets --features test-hooks -- -D warnings",
                        "build --locked --features test-hooks", "build --locked"):
            self.assertIn(f"\n          cargo +1.99.1 {command}\n", rust)
        self.assertIn("        working-directory: services/mobile/connector\n", rust)
        self.assertIn("""grep -q 'serde_json feature "preserve_order"'""", rust)
        self.assertIn("(axum|sqlx|reqwest|openssl-sys|native-tls)", rust)
        # The plain build, the last one in this step, carries no test seam.
        self.assertIn("\n          if grep -q ZERUS_CONNECTOR_TEST_ target/debug/zerus-mobile-connector; then\n", rust)
        self.assertLess(rust.index("\n          cargo +1.99.1 build --locked\n"), rust.index("grep -q ZERUS_CONNECTOR_TEST_"))
        tests = next(step for step in steps(connector) if "name: Vector, binary and handoff tests" in step)
        for variable in ("ZERUS_CONNECTOR_TEST_PYTHON", "ZERUS_CONNECTOR_TEST_HGS"):
            self.assertIn(f"\n          {variable}: ", tests)
        self.assertIn("run: cargo +1.99.1 test --locked\n", tests)
        python = '"$RUNNER_TEMP/connector-venv/bin/python" -m unittest discover -s services/mobile/tests'
        black_box = next(step for step in steps(connector) if "name: Black-box suites against the" in step)
        self.assertEqual(re.findall(rf"(?m)^ +(.*){re.escape(python)}(.*)$", black_box),
                         [("", " -p 'test_bb_*.py' -v \\"), ("", " -p 'test_bb_*.py' -v \\"),
                          ("", " -p test_bb_soak.py -v \\")])
        # cargo test rebuilt the binary without the test hooks the Rust runs need.
        hooks = "\n          (cd services/mobile/connector && cargo +1.99.1 build --locked --features test-hooks)\n"
        self.assertTrue(black_box.startswith(f"      - name: Black-box suites against the Python and Rust "
                                             f"connectors, differential traces and soak\n        run: |{hooks}"))
        self.assertIn('\n          rust="$GITHUB_WORKSPACE/services/mobile/connector/target/debug/zerus-mobile-connector"\n',
                      black_box)
        for run in ('ZERUS_CONNECTOR_TRACE_DIR="$traces/python" \\\n',
                    'ZERUS_CONNECTOR_TRACE_DIR="$traces/rust" ZERUS_CONNECTOR_COMMAND="$rust" \\\n',
                    'ZERUS_CONNECTOR_SOAK_SECONDS=300 ZERUS_CONNECTOR_COMMAND="$rust" \\\n'):
            self.assertEqual(black_box.count(run), 1, run)
        # Every run is waited for, and the traces are compared after all three.
        for run in ('finish python "$python"', 'finish rust "$rust_suites"', 'finish soak "$soak"'):
            self.assertIn(f"\n          {run}\n", black_box)
            self.assertLess(black_box.index(run), black_box.index("differential.py compare"))
        self.assertIn('            if wait "$2"; then\n', black_box)
        self.assertIn('\n            "$traces/python" "$traces/rust" || status=1\n          exit "$status"\n', black_box)
        baselines = next(step for step in steps(connector) if "name: Rollback baseline Python connectors" in step)
        self.assertIn('echo "ZERUS_CONNECTOR_ROLLBACK_COMMANDS=$commands" >> "$GITHUB_ENV"', baselines)
        self.assertLess(connector.index(baselines), connector.index(black_box))
        recorded = json.loads((ROOT / "services/mobile/tests/rollback-baselines.json").read_text())["baselines"]
        for baseline in recorded:
            self.assertRegex(baseline["commit"], r"^[0-9a-f]{40}$")
        # Every black-box module runs against Rust except the harness self-test,
        # which runs in every mode anyway.
        ported = json.loads((ROOT / "services/mobile/tests/ported.json").read_text())["rust"]
        self.assertEqual(ported, sorted(set(ported)))
        modules = {path.stem for path in (ROOT / "services/mobile/tests").glob("test_bb_*.py")}
        self.assertEqual(set(ported), modules - {"test_bb_harness"})

    def test_connector_macos_job_is_a_selected_macos_smoke(self):
        checks = (ROOT / ".github/workflows/checks.yml").read_text()
        smoke = job(checks, "connector-macos")
        self.assertEqual(field(smoke, "runs-on"), "macos-15")
        self.assertEqual(field(smoke, "if"), "needs.source.outputs.run_macos == 'true'")
        for text in ("secrets", "vars.", "environment:", "permissions", "github.token", "continue-on-error", "cache"):
            with self.subTest(forbidden=text):
                self.assertNotIn(text, smoke)
        self.assertEqual(re.findall(r"uses: ([\w/-]+)@[0-9a-f]{40} # v", smoke), ["actions/checkout"])
        self.assertIn("persist-credentials: false", smoke)
        self.assertIn("cargo +1.99.1 test --locked --lib ", smoke)
        self.assertIn("(cd ../connector && cargo +1.99.1 build --locked --features test-hooks)", smoke)
        self.assertIn("test_bb_handoff", smoke)
        self.assertIn("'connector-macos': json.loads(os.environ['RUN_MACOS']),", job(checks, "gate"))
        # Selected with the other macOS jobs: a failure or a skip fails the gate.
        gate = job(checks, "gate")
        script = re.search(r"(?s)python3 - <<'PY'\n(.*?)\n          PY", gate)[1]
        code = compile(textwrap.dedent(script), "workflow-checks-gate", "exec")
        names = ("source", "quick", "client-frontend", "relay", "connector", "connector-macos", "ios", "android", "client-native-linux", "client-native-macos-core", "client-native-macos-tauri", "client-native-windows",
                 "cli", "cli-macos-debug", "cli-macos-core", "cli-macos-native", "cli-macos", "clippy-macos", "arch-package",
                 "windows-tests", "windows-desktop", "windows-host")
        for run_macos, smoke_result, success in [("true", "success", True), ("true", "failure", False),
                                                 ("true", "skipped", False), ("false", "skipped", True),
                                                 ("false", "success", False)]:
            results = dict.fromkeys(names, "success")
            if run_macos == "false":
                results["client-native-macos-core"] = "skipped"
                results["client-native-macos-tauri"] = "skipped"
                results["mac-desktop"] = "skipped"
                results["ios"] = "skipped"
                for name in ("cli-macos-debug", "cli-macos-core", "cli-macos-native", "cli-macos", "clippy-macos"):
                    results[name] = "skipped"
            results["connector-macos"] = smoke_result
            environment = {"RESULTS": json.dumps({name: {"result": value} for name, value in results.items()}),
                           "RUN_LINUX": "true", "RUN_ARCH": "true", "RUN_MACOS": run_macos, "RUN_IOS": run_macos, "RUN_WINDOWS": "true"}
            with self.subTest(run_macos=run_macos, result=smoke_result), patch.dict(os.environ, environment):
                if success:
                    exec(code, {})
                else:
                    with self.assertRaises(SystemExit):
                        exec(code, {})

    def test_ios_job_is_a_selected_credential_free_macos_suite(self):
        checks = (ROOT / ".github/workflows/checks.yml").read_text()
        ios = job(checks, "ios")
        self.assertEqual(field(ios, "runs-on"), "macos-15")
        self.assertEqual(field(ios, "if"), "needs.source.outputs.run_ios == 'true'")
        self.assertRegex(field(ios, "timeout-minutes"), r"^\d+$")
        for text in ("secrets", "vars.", "environment:", "permissions", "github.token", "continue-on-error", "cache",
                     "allowProvisioningUpdates", "DEVELOPMENT_TEAM"):
            with self.subTest(forbidden=text):
                self.assertNotIn(text, ios)
        self.assertEqual(re.findall(r"uses: ([\w/-]+)@[0-9a-f]{40} # v", ios), ["actions/checkout"])
        self.assertIn("persist-credentials: false", ios)
        self.assertIn("swift test", ios)
        # ZerusKit links the project task replica, built before its tests.
        self.assertLess(ios.index("bash mobile/ios/scripts/build-replica.sh"), ios.index("swift test"))
        self.assertIn("CODE_SIGNING_ALLOWED=NO", ios)
        self.assertIn("'ios': json.loads(os.environ['RUN_IOS']),", job(checks, "gate"))
        gate = job(checks, "gate")
        script = re.search(r"(?s)python3 - <<'PY'\n(.*?)\n          PY", gate)[1]
        code = compile(textwrap.dedent(script), "workflow-checks-gate", "exec")
        names = ("source", "quick", "client-frontend", "relay", "connector", "connector-macos", "ios", "android", "client-native-linux", "client-native-macos-core", "client-native-macos-tauri", "client-native-windows",
                 "cli", "cli-macos-debug", "cli-macos-core", "cli-macos-native", "cli-macos", "clippy-macos", "arch-package",
                 "windows-tests", "windows-desktop", "windows-host")
        for run_ios, ios_result, success in [("true", "success", True), ("true", "failure", False),
                                               ("true", "skipped", False), ("true", "cancelled", False),
                                               ("false", "skipped", True), ("false", "success", False)]:
            results = dict.fromkeys(names, "success")
            results["ios"] = ios_result
            environment = {"RESULTS": json.dumps({name: {"result": value} for name, value in results.items()}),
                           "RUN_LINUX": "true", "RUN_ARCH": "true", "RUN_MACOS": "true", "RUN_IOS": run_ios, "RUN_WINDOWS": "true"}
            with self.subTest(run_ios=run_ios, result=ios_result), patch.dict(os.environ, environment):
                if success:
                    exec(code, {})
                else:
                    with self.assertRaises(SystemExit):
                        exec(code, {})

    def test_relay_job_checks_the_protocol_library_alone(self):
        relay = job((ROOT / ".github/workflows/checks.yml").read_text(), "relay")
        rust = next(step for step in steps(relay) if "name: Rust format, lint and PostgreSQL storage contracts" in step)
        # The connector links the library without the server feature on Rust 1.99,
        # and that build reproduces the golden vectors.
        for command in ("clippy --locked --no-default-features --lib --test codec --test vectors -- -D warnings",
                        "build --locked --no-default-features --lib",
                        "test --locked --no-default-features --test codec --test vectors"):
            self.assertIn(f"\n          cargo +1.99.1 {command}\n", rust)
        self.assertIn('library="$(cargo +1.99.1 tree --locked --no-default-features --target all -e normal', rust)
        self.assertIn("\n          serde_features\n          serde_features --no-default-features\n", rust)
        self.assertIn("""grep -q 'serde_json feature "preserve_order"'""", rust)

    def test_arch_package_job_alone_produces_the_candidate_artifact(self):
        checks = (ROOT / ".github/workflows/checks.yml").read_text()
        package = job(checks, "arch-package")
        self.assertEqual(field(package, "if"), "needs.source.outputs.run_arch == 'true'")
        self.assertRegex(package, r"(?m)^      image: archlinux@sha256:[0-9a-f]{64}$")
        self.assertNotRegex(checks, r"(?m)^  (?:arch|mac-desktop):$")
        self.assertNotIn("scripts/ci/gui.sh", checks)
        self.assertEqual(checks.count("name: arch-x86_64-"), 1)
        upload = next(step for step in self.checks_steps("arch-package") if "name: arch-x86_64-" in step)
        self.assertIn("\n        if: always()\n", upload)
        self.assertIn("name: arch-x86_64-${{ needs.source.outputs.commit }}\n          path: artifacts\n", upload)
        for line in ("ZERUS_DEV_BUILD_NUMBER: ${{ github.repository == 'ufna/zerus'", "fetch-depth: 0",
                     "runuser -u user -- bash scripts/build-arch-package.sh",
                     "runuser -u user -- python3 scripts/check-installed-package.py"):
            self.assertIn(line, package)
        self.assertLess(package.index("build-arch-package.sh"), package.index("name: arch-x86_64-"))

    def test_only_trusted_main_checks_request_cache_saves(self):
        self.assertIn("\n      save_caches: true\n", job(self.ci, "automatic"))
        self.assertNotIn("save_caches", job(self.ci, "review"))
        for path in sorted((ROOT / ".github/workflows").glob("*.yml")):
            text = path.read_text()
            with self.subTest(workflow=path.name):
                passed = re.findall(r"(?m)^ +save_caches: *(.*)$", text)
                if path.name in {"ci.yml", "ci-dev-qualification.yml", "nightly.yml"}:
                    self.assertEqual(passed, ["true"])
                elif path.name in {"checks.yml", "dev-arch.yml"}:
                    self.assertEqual(passed, [""])
                    self.assertRegex(text, r"(?m)^      save_caches:\n(?:        description: .+\n)?"
                                           r"        type: boolean\n        default: false$")
                elif path.name == 'ci-regression.yml':
                    self.assertEqual(passed, ['false'])
                    self.assertRegex(text, r'(?m)^cache-mode: read$')
                else:
                    self.assertNotIn("save_caches", text)

    def test_checks_caches_are_restored_everywhere_and_saved_only_on_request(self):
        checks = (ROOT / ".github/workflows/checks.yml").read_text()
        # No implicit post-job save: every cache is an explicit restore/save pair.
        self.assertEqual(set(re.findall(r"uses: (actions/cache[\w/]*)@", checks)),
                         {"actions/cache/restore", "actions/cache/save"})
        names = re.findall(r"(?m)^  ([a-z][\w-]*):\n", checks.split("\njobs:\n", 1)[1])
        # Restore-only consumers never save: two jobs saving one key would race.
        read_only = {"windows-tests": {"rust-cache"}, "windows-host": {"rust-cache"}, "client-native-macos-core": {"rust-cache"},
                     "clippy-macos": {"rust-cache"}}
        saves = 0
        for name in names:
            restores, saved = {}, []
            for step in self.checks_steps(name):
                if "uses: actions/cache/restore@" in step:
                    self.assertNotRegex(step, r"(?m)^        if:", name)
                    restores[re.search(r"(?m)^        id: (\S+)$", step)[1]] = self.step_paths(step)
                elif "uses: actions/cache/save@" in step:
                    saves += 1
                    with self.subTest(job=name, step=step.splitlines()[0]):
                        condition = re.search(r"(?m)^        if: (.*)$", step)[1]
                        self.assertTrue(condition.startswith("inputs.save_caches && "), condition)
                        self.assertNotIn("always()", condition)
                        source = re.search(r"(?m)^          key: \$\{\{ steps\.([\w-]+)\.outputs\.cache-primary-key \}\}$",
                                           step)[1]
                        # The same paths, so later restores find the saved entry.
                        self.assertEqual(self.step_paths(step), restores[source])
                        saved.append(source)
            with self.subTest(job=name):
                self.assertEqual(sorted(saved), sorted(set(restores) - read_only.get(name, set())))
        self.assertGreaterEqual(saves, 10)
        self.assertEqual(job(checks, "windows-host").count("actions/cache/"), 1)
        desktop, host = (self.checks_step(name, "rust-cache") for name in ("windows-desktop", "windows-host"))
        self.assertEqual(self.step_paths(host), self.step_paths(desktop))
        for line in ("key", "restore-keys"):
            self.assertEqual(re.search(rf"(?m)^          {line}: (.*)$", host)[1],
                             re.search(rf"(?m)^          {line}: (.*)$", desktop)[1])
        # Qt is saved only after the desktop build has used the installation.
        windows = job(checks, "windows-desktop")
        self.assertLess(windows.index("npm run tauri -- build"), windows.index("name: Save Rust cache"))

    def test_compiler_caches_refresh_every_saving_run_and_rust_caches_do_not(self):
        families = {"arch-package": "arch-package-ccache-v1-"}
        for name, family in families.items():
            text = job((ROOT / ".github/workflows/checks.yml").read_text(), name)
            restore, stats = self.checks_step(name, "ccache-restore"), self.checks_step(name, "ccache")
            with self.subTest(job=name):
                self.assertIn(f"          key: {family}${{{{ github.run_id }}}}\n          restore-keys: {family}\n", restore)
                self.assertRegex(text, r"(?m)^      CCACHE_MAXSIZE: \d+M$")
                self.assertIn("ccache -z", self.checks_step(name, "ccache-start"))
                # Statistics always print; a save needs a successful build that
                # compiled something and keeps only the entries that build used.
                self.assertIn("        if: ${{ !cancelled() }}\n", stats)
                self.assertIn("SAVE: ${{ inputs.save_caches && steps.build.outcome == 'success' }}", stats)
                self.assertIn("--print-stats | awk -F'\\t' '$1 == \"cache_miss\" { print $2 + 0 }'", stats)
                self.assertIn('if [ "$SAVE" = true ] && [ "${misses:-0}" -gt 0 ]; then', stats)
                self.assertIn('ccache --evict-older-than "$(( $(date +%s) - STARTED + 60 ))s"', stats)
                self.assertIn("if: inputs.save_caches && steps.ccache.outputs.save == 'true'",
                              next(step for step in self.checks_steps(name) if "name: Save ccache" in step))
        # Different compiler flags never share a family.
        for first in families.values():
            for second in families.values():
                self.assertTrue(first == second or not first.startswith(second))
        identity = "${{ steps.rust.outputs.rustc }}-${{ steps.rust.outputs.epoch }}-"
        rust = {"quick": ("msrv-v2-${{ runner.os }}-${{ runner.arch }}-", "rustc +1.99.1 -Vv"),
                "cli": ("cli-${{ runner.os }}-${{ runner.arch }}-", "rustc -Vv"),
                "cli-macos-debug": ("cli-${{ runner.os }}-${{ runner.arch }}-", "rustc -Vv"),
                "cli-macos-core": ("cli-${{ runner.os }}-${{ runner.arch }}-", "rustc -Vv"),
                "windows-desktop": ("windows-cli-${{ runner.arch }}-", "rustc -Vv"),
                "windows-host": ("windows-cli-${{ runner.arch }}-", "rustc -Vv"),
                "connector": ("connector-${{ runner.os }}-${{ runner.arch }}-", "rustc +1.99.1 -Vv")}
        for name, (family, rustc) in rust.items():
            with self.subTest(job=name):
                self.assertIn(f'echo "rustc=$({rustc} | git hash-object --stdin | cut -c1-16)" >> "$GITHUB_OUTPUT"\n'
                              f'          echo "epoch=$(date -u +%Y-%m)" >> "$GITHUB_OUTPUT"\n',
                              self.checks_step(name, "rust"))
                restore = self.checks_step(name, "rust-cache")
                # Each cache follows the lock of what it builds.
                lock = "services/mobile/connector/Cargo.lock" if name == "connector" else "Cargo.lock"
                if name in ("windows-desktop", "windows-host"):
                    lock = "Cargo.lock', 'client/core/Cargo.lock', 'client/media/Cargo.lock', 'client/src-tauri/Cargo.lock"
                if name in ('cli-macos-debug', 'cli-macos-core'):
                    profile, other = ('debug', 'complete') if name.endswith('debug') else ('complete', 'debug')
                    prefix = f"{family}{identity}${{{{ hashFiles('{lock}') }}}}"
                    self.assertIn(f"          key: {prefix}{profile}\n          restore-keys: |\n"
                                  f"            {prefix}{other}\n            {prefix}\n            {family}{identity}\n", restore)
                else:
                    self.assertIn(f"          key: {family}{identity}${{{{ hashFiles('{lock}') }}}}\n"
                                  f"          restore-keys: {family}{identity}\n", restore)
        cargo = self.checks_step("arch-package", "cargo-cache")
        # The package also builds the mobile connector, which has its own lock.
        self.assertIn("          key: arch-cargo-v1-${{ steps.epoch.outputs.month }}-"
                      "${{ hashFiles('Cargo.lock', 'services/mobile/connector/Cargo.lock', 'client/src-tauri/Cargo.lock', 'client/media/Cargo.lock', 'client/core/Cargo.lock') }}\n"
                      "          restore-keys: arch-cargo-v1-${{ steps.epoch.outputs.month }}-\n", cargo)
        for name, step_id in (("quick", "rust-cache"), ("cli", "rust-cache"), ("cli-macos-debug", "rust-cache"), ("cli-macos-core", "rust-cache"), ("windows-desktop", "rust-cache"),
                              ("connector", "rust-cache"), ("arch-package", "cargo-cache"), ("android", "gradle-cache")):
            with self.subTest(job=name, cache=step_id):
                # Exact content keys: 200-800 MB entries are saved only when missing.
                self.assertNotIn("run_id", self.checks_step(name, step_id))
                save = next(step for step in self.checks_steps(name)
                            if f"key: ${{{{ steps.{step_id}.outputs.cache-primary-key }}}}" in step)
                self.assertIn(f"if: inputs.save_caches && steps.{step_id}.outputs.cache-hit != 'true'\n", save)

    def test_hosted_ubuntu_jobs_bound_disk_overlap_without_changing_cache_trust(self):
        checks = (ROOT / ".github/workflows/checks.yml").read_text()
        quick, relay = job(checks, "quick"), job(checks, "relay")
        for text in (quick, relay):
            self.assertIn('runs-on: ubuntu-24.04', text)
            self.assertIn('CARGO_INCREMENTAL: "0"', text)
            self.assertIn('CARGO_PROFILE_DEV_DEBUG: "0"', text)
            self.assertIn('CARGO_PROFILE_TEST_DEBUG: "0"', text)
            self.assertIn('scripts/ci/ubuntu-disk.py prepare', text)
            self.assertIn('scripts/ci/ubuntu-disk.py report', text)
            self.assertNotIn('docker system prune', text)
        targets = re.findall(r"--target-dir (\S+)", quick)
        self.assertTrue(targets)
        self.assertEqual(set(targets), {"target"})
        self.assertIn('key: msrv-v2-', quick)
        self.assertIn('restore-keys: msrv-v2-', quick)
        self.assertNotIn('restore-keys: msrv-${{', quick)
        retired = relay.index('scripts/ci/ubuntu-disk.py relay-before-image')
        self.assertLess(relay.index('name: Released HTTP'), retired)
        self.assertLess(retired, relay.index('docker build --tag zerus-relay-contract'))
        self.assertIn('name: Enforce the 256 MiB runtime envelope', relay)

    def test_native_linux_disk_preparation_precedes_dependencies_and_preserves_checks(self):
        native = job((ROOT / ".github/workflows/checks.yml").read_text(), "client-native-linux")
        self.assertEqual(native.count("ubuntu-disk.py prepare"), 1)
        self.assertNotIn("ubuntu-disk.py relay-before-image", native)
        self.assertNotIn("CARGO_PROFILE_DEV_DEBUG", native)
        self.assertNotIn("CARGO_PROFILE_TEST_DEBUG", native)
        phases = ("after-native-checkout", "after-native-dependencies",
                  "after-native-cache-restore", "after-native-checks")
        indices = [native.index(f"ubuntu-disk.py report {phase}") for phase in phases]
        self.assertEqual(indices, sorted(indices))
        self.assertLess(indices[0], native.index("sudo apt-get update"))
        self.assertLess(indices[0], native.index("ubuntu-disk.py prepare"))
        self.assertLess(native.index("ubuntu-disk.py prepare"), native.index("sudo apt-get update"))
        self.assertLess(native.index("rustup toolchain install"), indices[1])
        self.assertLess(native.index("actions/cache/restore@"), indices[2])
        self.assertLess(indices[2], native.index("bash scripts/ci/client-native.sh linux"))
        self.assertIn("name: Disk usage after native checks\n        if: ${{ !cancelled() }}\n", native)
        script = (ROOT / "scripts/ci/client-native.sh").read_text()
        linux = script.split("    linux:all)\n", 1)[1].split("        ;;", 1)[0]
        boundaries = (
            ("presence_tests", "after-native-presence-tests"),
            ("--manifest-path client/media/Cargo.toml --features video", "after-native-media-tests"),
            ("--manifest-path client/src-tauri/Cargo.toml\n", "after-native-tauri-tests"),
            ("--all-targets --features video -- -D warnings", "after-native-tauri-clippy"),
            ("--all-targets --features linux-native-notify -- -D warnings", "before-native-core-test-linking"),
            ("--features linux-native-notify linux_notify", "after-native-core-tests"),
        )
        for command, phase in boundaries:
            with self.subTest(phase=phase):
                self.assertLess(linux.index(command), linux.index(f"disk_checkpoint {phase}"))
        self.assertLess(linux.index("disk_checkpoint before-native-core-test-linking"),
                        linux.index("dbus-run-session -- cargo"))

    def test_historical_deepseek_lock_keeps_exact_registry_integrities(self):
        fixture = ROOT / "scripts/ci/dsh-fixture"
        manifest = json.loads((fixture / "package.json").read_text())
        lock = json.loads((fixture / "package-lock.json").read_text())
        # Historical evidence remains internally exact, not a CI version selector.
        version = manifest["dependencies"]["@deepseek-ai/dsh"]
        self.assertRegex(version, r'^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$')
        self.assertEqual(manifest["dependencies"], {"@deepseek-ai/dsh": version})
        self.assertEqual(lock["lockfileVersion"], 3)
        packages = lock["packages"]
        self.assertEqual(packages[""]["dependencies"], manifest["dependencies"])
        self.assertEqual(packages["node_modules/@deepseek-ai/dsh"]["version"], version)
        self.assertEqual(manifest["overrides"], {'@modelcontextprotocol/client': '2.3.0', 'fflate': '0.8.3'})
        self.assertEqual(packages["node_modules/@modelcontextprotocol/client"]["version"], '2.3.0')
        self.assertFalse(any(path.endswith('node_modules/fflate') and entry['version'] == '0.8.2'
                             for path, entry in packages.items()))
        # Every transitive package is a registry tarball checked by SHA-512.
        for path, entry in packages.items():
            if not path:
                continue
            self.assertTrue(path.startswith("node_modules/"), path)
            self.assertNotIn("link", entry, path)
            name = path.rsplit("node_modules/", 1)[1]
            tarball = f"{name.rsplit('/', 1)[-1]}-{entry['version']}.tgz"
            self.assertEqual(entry["resolved"], f"https://registry.npmjs.org/{name}/-/{tarball}", path)
            self.assertRegex(entry["integrity"], r"^sha512-[A-Za-z0-9+/]{86}==$", path)
    def test_deepseek_ci_selects_and_requires_a_sealed_latest_install_before_tests(self):
        steps = self.checks_steps("cli")
        step = next(step for step in steps if "DeepSeek contract fixture" in step)
        self.assertIn("if: runner.os == 'Linux'", step)
        self.assertIn('python3 scripts/ci/dsh_fixture.py --prefix "$RUNNER_TEMP/dsh"', step)
        self.assertIn('--evidence "$GITHUB_WORKSPACE/artifacts/test-results/dsh-fixture"', step)
        self.assertIn('echo "$RUNNER_TEMP/dsh/node_modules/.bin" >> "$GITHUB_PATH"', step)
        self.assertIn('ZERUS_TEST_DSH_RECEIPT=$GITHUB_WORKSPACE/artifacts/test-results/dsh-fixture/installed.json', step)
        self.assertIn('>> "$GITHUB_ENV"', step)
        self.assertLess(steps.index(step), next(i for i, value in enumerate(steps) if 'scripts/ci/cli.sh' in value))
        self.assertNotIn('scripts/ci/dsh-fixture/package-lock.json', step)
        self.assertNotIn("npm install", job((ROOT / ".github/workflows/checks.yml").read_text(), "cli"))
        workflows = "".join(path.read_text() for path in (ROOT / ".github/workflows").glob("*.yml"))
        self.assertNotIn("@deepseek-ai/dsh@", workflows)

    def test_windows_conpty_pair_is_fetched_once_from_its_pin(self):
        windows = job((ROOT / ".github/workflows/checks.yml").read_text(), "windows-desktop")
        steps = self.checks_steps("windows-desktop")
        fetch = next(step for step in steps if "fetch-conpty.py" in step)
        host = next(step for step in steps if "- name: Windows session host tests\n" in step)
        package = next(step for step in steps if "scripts/package-windows.py" in step)
        # One fetch, from the URL and digests in packaging/windows/conpty.json only.
        self.assertEqual(windows.count("scripts/fetch-conpty.py"), 1)
        self.assertIn('\n        run: python scripts/fetch-conpty.py --output "$RUNNER_TEMP/conpty" </dev/null\n', fetch)
        self.assertNotRegex(windows, r"(?i)nuget\.org|\.nupkg")
        self.assertLess(steps.index(fetch), steps.index(host))
        self.assertLess(steps.index(host), steps.index(package))
        # The host suites get the folder; the zip takes the same pair.
        self.assertIn('HGS_TEST_CONPTY_DIR=$(cygpath -w "$RUNNER_TEMP/conpty")\n', host)
        self.assertIn("export HGS_TEST_BIN HGS_TEST_FAKE_AGENT HGS_TEST_FAKE_SSH HGS_TEST_CONPTY_DIR\n", host)
        modules = [word for word in re.search(r"python -m unittest -v ([\w \\\n]+?)</dev/null", host)[1].split()
                   if word != "\\"]
        self.assertEqual(modules, ["test_windows_host", "test_windows_host_conpty", "test_windows_host_survival",
                                   "test_windows_lifecycle", "test_windows_hooks", "test_backend_contract"])
        connector = next(step for step in steps if "Release CLI and connector for host and package checks" in step)
        unit_connector = next(step for step in self.checks_steps("windows-tests") if "Windows connector filesystem contracts" in step)
        self.assertIn("--test windows_gateway </dev/null", unit_connector)
        self.assertIn("--locked --release --manifest-path services/mobile/connector/Cargo.toml", connector)
        self.assertNotIn("--features test-hooks", connector)
        self.assertLess(steps.index(connector), steps.index(host))
        gateway = next(step for step in steps if "Windows mobile gateway isolated custody gate" in step)
        self.assertIn("--custody-only --timeout 900", gateway)
        self.assertIn("test_windows_mobile_gateway.py", gateway)
        self.assertNotIn("--end", gateway)
        self.assertNotIn("test_windows_mobile_gateway", host)
        self.assertNotIn("gateway-bundle", host)
        self.assertIn("--connector target/release/zerus-mobile-connector.exe", package)
        for module in modules:
            self.assertTrue((ROOT / "tests" / f"{module}.py").is_file(), module)
        self.assertIn('--conpty "$RUNNER_TEMP/conpty" --output artifacts/zerus-windows-x86_64.zip </dev/null', package)
        # Outside the uploaded artifacts and every cache.
        for step in steps:
            if "uses: actions/cache/" in step or "uses: actions/upload-artifact@" in step:
                self.assertFalse([path for path in self.step_paths(step) if "conpty" in path.lower()], step)

    def test_windows_encrypted_runtime_requires_setup_but_not_root_test_success(self):
        selected = self.checks_steps('windows-tests')
        root = next(step for step in selected if 'name: Root unit tests and pinned lints\n' in step)
        encrypted = next(step for step in selected if 'name: Windows encrypted runtime contracts\n' in step)
        logs = next(step for step in selected if 'name: Retain Windows unit and connector logs\n' in step)
        self.assertIn('        id: root-tests\n', root)
        expression = re.search(r'if: \$\{\{ (.*?) \}\}', encrypted)[1]
        expression = expression.replace('&&', ' and ').replace('||', ' or ')
        expression = re.sub(r'!(?!=)', 'not ', expression)
        expression = re.sub(r'\bsteps\.([\w-]+)\.', r"steps['\1'].", expression)
        for cancelled, outcome in product((False, True), ('success', 'failure', 'skipped', 'cancelled', '')):
            with self.subTest(cancelled=cancelled, root_outcome=outcome):
                admitted = eval(expression, {'__builtins__': {}}, {
                    'cancelled': lambda: cancelled,
                    'steps': {'root-tests': SimpleNamespace(outcome=outcome)},
                })
                self.assertEqual(admitted, not cancelled and outcome in ('success', 'failure'))
        self.assertIn('--locked --manifest-path services/mobile/connector/Cargo.toml --target-dir target --lib encrypted_runtime </dev/null', encrypted)
        self.assertIn('artifacts/test-results/windows-encrypted-runtime.log', encrypted)
        self.assertNotIn('continue-on-error', job((ROOT / '.github/workflows/checks.yml').read_text(), 'windows-tests'))
        self.assertLess(selected.index(root), selected.index(encrypted))
        self.assertLess(selected.index(encrypted), selected.index(logs))
        self.assertIn('        if: always()\n', logs)

    def test_windows_real_agents_run_in_a_bounded_credential_free_job_of_their_own(self):
        checks = (ROOT / ".github/workflows/checks.yml").read_text()
        host, desktop = job(checks, "windows-host"), job(checks, "windows-desktop")
        self.assertEqual(field(host, "name"), "Windows real agents through the session host")
        for name in ("if", "runs-on"):
            self.assertEqual(field(host, name), field(desktop, name))
        for text in ("secrets", "vars.", "environment", "permissions", "github.token", "continue-on-error",
                     "save_caches", "actions/cache/save"):
            with self.subTest(forbidden=text):
                self.assertNotIn(text, host)
        self.assertEqual(re.findall(r"uses: ([\w/-]+)@[0-9a-f]{40} # v", host),
                         ["actions/checkout", "actions/setup-python", "actions/cache/restore", "actions/upload-artifact"])
        # The same text-file, interpreter and checkout rules as windows-desktop.
        self.assertLess(host.index("core.autocrlf false"), host.index("actions/checkout@"))
        self.assertIn("ref: ${{ needs.source.outputs.commit }}\n          persist-credentials: false", host)
        setup = next(step for step in self.checks_steps("windows-desktop") if "actions/setup-python@" in step)
        self.assertIn(setup, self.checks_steps("windows-host"))
        steps = self.checks_steps("windows-host")
        build = next(step for step in steps if "cargo build" in step)
        fetch = next(step for step in steps if "fetch-conpty.py" in step)
        agents = next(step for step in steps if "agent-compatibility.py" in step)
        upload = next(step for step in steps if "actions/upload-artifact@" in step)
        self.assertEqual(build, "      - run: cargo build --release --locked\n")
        self.assertEqual(fetch.splitlines()[:2], next(step for step in self.checks_steps("windows-desktop")
                                                      if "fetch-conpty.py" in step).splitlines()[:2])
        self.assertLess(steps.index(build), steps.index(agents))
        self.assertLess(steps.index(fetch), steps.index(agents))
        # Outside the step's job, which forbids the hosts' breakaway, with every
        # wait bounded: the runner's own timeouts, the harness's, the step's, the job's.
        self.assertIn('HGS_TEST_BIN=$(cygpath -w "$PWD/target/release/hgs.exe")\n', agents)
        self.assertIn('HGS_TEST_CONPTY_DIR=$(cygpath -w "$RUNNER_TEMP/conpty")\n', agents)
        self.assertIn("export HGS_TEST_BIN HGS_TEST_CONPTY_DIR\n", agents)
        self.assertIn("          PYTHONUTF8: 1\n", agents)
        command = re.search(r"python scripts/ci/outside-job\.py (.*?)</dev/null\n", agents, re.S)[1]
        self.assertTrue(command.rstrip().endswith("-- \\\n            python scripts/ci/agent-compatibility.py"), command)
        harness = int(re.search(r"--timeout (\d+) ", command)[1])
        step = int(re.search(r"(?m)^        timeout-minutes: (\d+)$", agents)[1])
        spec = importlib.util.spec_from_file_location("agent_compatibility", ROOT / "scripts/ci/agent-compatibility.py")
        compat = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(compat)
        # Only agents with a reviewed Windows package run there, as main() selects them.
        windows = [agent for agent, pinned in compat.AGENTS.items()
                   if compat.agent_target(agent, compat.WINDOWS) in pinned["members"]]
        self.assertEqual(windows, ["codex", "claude"])
        suites = sum(compat.target_spec(agent, compat.WINDOWS)["timeout"] for agent in windows)
        self.assertLess(suites + 5 * 60, harness)
        self.assertLess(harness + 3 * 60, step * 60)
        self.assertLess(step + 20, int(field(host, "timeout-minutes")))
        # Leftovers end by executable path: hosts of the release build, the hgs-host-*
        # folders of HostCase (copies and staged images) and the extracted agents.
        self.assertEqual(re.findall(r"--end ('[^']*'|\"[^\"]*\")", command),
                         ['"$HGS_TEST_BIN"', "'*\\hgs-host-*\\*.exe'", "'*\\zerus-native-compat-*\\*.exe'"])
        self.assertIn('prefix="hgs-host-"', (ROOT / "tests/test_windows_host.py").read_text())
        self.assertIn("prefix='zerus-native-compat-'", (ROOT / "scripts/ci/agent-compatibility.py").read_text())
        for line in ("        if: always() && (github.event_name != 'schedule' || failure())\n",
                     "name: windows-host-results\n          path: artifacts/test-results\n",
                     "retention-days: ${{ github.event_name == 'schedule' && 3 || 7 }}"):
            self.assertIn(line, upload)
        self.assertIn("'windows-host': json.loads(os.environ['RUN_WINDOWS']),", job(checks, "gate"))

    def test_windows_next_build_does_not_download_qt_or_ccache(self):
        windows = job((ROOT / ".github/workflows/checks.yml").read_text(), "windows-desktop")
        self.assertNotIn("ccache", windows)
        self.assertNotIn("install-qt-windows.py", windows)
        self.assertNotRegex(windows, r"\b(?:choco|winget|scoop)\b")
        self.assertIn("client/src-tauri/Cargo.lock", windows)
        self.assertIn("path: artifacts", windows)
        self.assertIn("python scripts/ci/ensure-webview2.py </dev/null", windows)
        self.assertIn("python client/scripts/collect-notices.py artifacts/windows-frontend-licenses </dev/null", windows)
        self.assertIn("--frontend-licenses artifacts/windows-frontend-licenses", windows)
        self.assertLess(windows.index("ensure-webview2.py"), windows.index("check-windows-portable.py"))
        quick = job((ROOT / ".github/workflows/checks.yml").read_text(), "quick")
        self.assertIn("python3 -m unittest test_windows_installer test_webview2_ci", quick)

    def test_quick_checks_scan_all_history_for_secrets_with_a_verified_gitleaks(self):
        checks = (ROOT / ".github/workflows/checks.yml").read_text()
        quick = job(checks, "quick")
        self.assertEqual(quick.count("gitleaks/releases/download"), 1)
        self.assertNotIn("gitleaks-action", checks)
        # Every branch and tag, not only the tested commit.
        self.assertIn("          ref: ${{ needs.source.outputs.commit }}\n          fetch-depth: 0\n", quick)
        scan = next(step for step in self.checks_steps("quick") if "gitleaks/releases/download" in step)
        version = re.search(r"\n          curl -fsSLO https://github\.com/gitleaks/gitleaks/releases/download/"
                            r"v(\d+\.\d+\.\d+)/gitleaks_\1_linux_x64\.tar\.gz\n", scan)[1]
        archive = re.escape(f"gitleaks_{version}_linux_x64.tar.gz")
        self.assertRegex(scan, rf"\n          echo '[0-9a-f]{{64}}  {archive}' \| sha256sum --check --strict\n")
        verified = scan.index("sha256sum --check")
        self.assertLess(scan.index("curl -fsSLO"), verified)
        self.assertLess(verified, scan.index("tar xzf"))
        # A token generated in the job, committed with an allow comment and then
        # deleted, must be reported with the distinct exit code before the real scan.
        control = '"$scan_dir/gitleaks" git --no-banner --redact --ignore-gitleaks-allow --exit-code 3 control || status=$?'
        self.assertIn(control, scan)
        self.assertIn("import secrets", scan)
        self.assertIn('# gitleaks:allow', scan)
        self.assertIn("git -C control rm -q leak.py", scan)
        self.assertIn('if [ "$status" != 3 ]; then', scan)
        self.assertLess(verified, scan.index(control))
        # Public logs never show a finding's secret; inline allow comments do not count.
        for target in ("git", "dir"):
            command = f'"$scan_dir/gitleaks" {target} --no-banner --redact --ignore-gitleaks-allow --exit-code 1 .'
            self.assertIn(command, scan)
            self.assertLess(scan.index(control), scan.index(command))
        # Keep the default rules. Only these reviewed immutable false positives
        # may be ignored; a new commit at the same path/line must still fail.
        self.assertFalse((ROOT / ".gitleaks.toml").exists())
        fingerprints = [line.strip() for line in (ROOT / ".gitleaksignore").read_text().splitlines()
                        if line.strip() and not line.lstrip().startswith("#")]
        self.assertEqual(fingerprints, [
            "54ece01be80c6dfb552eb9fc31a6455011fda1a2:mobile/ios/ZerusKit/Tests/ZerusKitTests/TasksPresentationTests.swift:generic-api-key:148",
            "54ece01be80c6dfb552eb9fc31a6455011fda1a2:mobile/android/app/src/test/java/app/zerus/mobile/TasksPresentationTest.kt:generic-api-key:134",
            "907468c65d867742cca2ce8342cf3d5107ac4823:client/src-tauri/src/native_ui.rs:generic-api-key:138",
        ])

    def test_cache_directories_stay_outside_uploaded_paths(self):
        checks = (ROOT / ".github/workflows/checks.yml").read_text()

        def inside(path, directory):
            return path == directory or path.startswith(directory.rstrip("/") + "/")

        for name in re.findall(r"(?m)^  ([a-z][\w-]*):\n", checks.split("\njobs:\n", 1)[1]):
            steps = self.checks_steps(name)
            cached = [path for step in steps if "uses: actions/cache/" in step for path in self.step_paths(step)]
            uploaded = [path for step in steps if "uses: actions/upload-artifact@" in step for path in self.step_paths(step)]
            for upload in uploaded:
                with self.subTest(job=name, upload=upload):
                    self.assertTrue(inside(upload, "artifacts"), upload)
                    for path in cached:
                        self.assertFalse(inside(path, upload) or inside(upload, path), path)
            for path in cached:
                if "ccache" in path:
                    # Compiler caches live outside the checkout entirely.
                    self.assertTrue(path.startswith(("/home/user/", "~/Library/Caches/", "${{ runner.temp }}/")), path)
        for name in ("arch-package",):
            self.assertIn("      CCACHE_DIR: /home/user/.cache/ccache\n", job(checks, name))
        self.assertIn("CARGO_TARGET_DIR: ${{ github.workspace }}/target/windows-next",
                      job(checks, "windows-desktop"))
        # makepkg uses ccache only through a drop-in in the disposable package
        # container, after checking that it changes nothing but ccache.
        package = job(checks, "arch-package")
        guard = package.index("grep -qx 'BUILDENV=(!distcc color !ccache check !sign)' /etc/makepkg.conf")
        self.assertLess(guard, package.index(
            "echo 'BUILDENV=(!distcc color ccache check !sign)' > /etc/makepkg.conf.d/ccache.conf"))
        self.assertEqual(checks.count("makepkg.conf"), 2)

    def test_actions_are_pinned_and_runners_are_standard(self):
        # Standard runners are free for this public repository; larger ones are not.
        labels = {"ubuntu-24.04", "macos-15", "windows-2025"}
        for path in sorted((ROOT / ".github/workflows").glob("*.yml")):
            text = path.read_text()
            with self.subTest(workflow=path.name):
                for action in re.findall(r"(?m)^\s*(?:- )?uses: (\S.*)$", text):
                    if not action.startswith("./"):
                        self.assertRegex(action, r"^[\w.-]+/[\w./-]+@[0-9a-f]{40} # v\d+(?:\.\d+)*$")
                for runner in re.findall(r"(?m)^\s*runs-on: (.+)$", text):
                    if runner != "${{ matrix.os }}":
                        self.assertIn(runner, labels)
                for runner in re.findall(r'"((?:ubuntu|macos|windows)-[^"]+)"', text):
                    self.assertIn(runner, labels)

    def test_dev_candidates_require_arch_mac_android_builds(self):
        automatic = job(self.ci, 'automatic')
        for name in ('linux', 'arch', 'macos'):
            self.assertIn("run_" + name + ": ${{ github.event_name == 'push' || github.event_name == 'pull_request' || inputs.run_" + name + " }}", automatic)
        self.assertIn('      run_windows: false\n      run_ios: false', automatic)
        checks = job(self.qualification, 'checks')
        self.assertEqual(field(checks, 'uses'), './.github/workflows/dev-arch.yml')
        self.assertIn('      save_caches: true\n', checks)
        bundle = job(self.qualification, 'bundle')
        self.assertIn('      run_tests: false\n', bundle)
        regression = job((ROOT / '.github/workflows/ci-regression.yml').read_text(), 'checks')
        self.assertEqual(field(regression, 'uses'), './.github/workflows/checks.yml')
        for name in ('linux', 'arch', 'macos'):
            self.assertIn('      run_' + name + ': true\n', regression)
        self.assertIn('      run_windows: false\n      run_ios: false', regression)
        self.assertEqual(field(job(self.ci, 'dev-tail'), 'needs'), '[gate, qualification]')
        self.assertEqual(field(job(self.tail, 'dev'), 'needs'), '[dev-android, dev-macos]')
        dev = (ROOT / '.github/workflows/dev.yml').read_text()
        self.assertNotIn('windows-x86_64', dev)
        self.assertNotIn('dev-ios:', self.ci)
        self.assertEqual(field(job(self.qualification, 'gate'), 'needs'), '[checks, bundle]')

    def test_privileged_publishers_have_no_cache_access(self):
        for name in ("publish.yml", "publish-nightly.yml", "dev.yml", "dev-android.yml", "ci-dev-tail.yml", "android-release.yml", "publish-dev.yml"):
            with self.subTest(workflow=name):
                text = (ROOT / ".github/workflows" / name).read_text()
                self.assertRegex(text, r"(?m)^cache-mode: none$")
                self.assertNotRegex(text, r"actions/cache[@/]")
                self.assertNotRegex(text, r"(?m)^    cache-mode:")

    def dev_android(self):
        return job((ROOT / ".github/workflows/dev-android.yml").read_text(), "android")

    def test_android_candidate_needs_no_polling_runner_or_actions_token(self):
        android = self.dev_android()
        listed = steps(android)
        removal, upload = listed[-2:]
        self.assertIn('name: Remove private signing inputs', removal)
        self.assertIn('        if: always()\n', removal)
        for path in ('"$RUNNER_TEMP/zerus-dev-signing"', 'mobile/android/app/google-services.json'):
            self.assertIn(path, removal)
        self.assertNotIn('github.token', android)
        self.assertNotIn('actions: read', android)
        self.assertNotIn('time.sleep', android)
        self.assertNotIn('Wait for', android)
        self.assertEqual(field(android, 'environment'), 'release')
        self.assertEqual(int(field(android, 'timeout-minutes')), 40)
        for line in ('name: zerus-dev-android-a${{ github.run_attempt }}', 'path: artifacts/dev-android',
                     'retention-days: 3', 'if-no-files-found: error'):
            self.assertIn('          ' + line + '\n', upload)
        self.assertNotRegex(upload, r'(?m)^        if:')

    def test_signed_android_job_only_assembles_after_the_credential_free_checks(self):
        workflow = (ROOT / ".github/workflows/dev-android.yml").read_text()
        listed = steps(self.dev_android())
        build = next(step for step in listed if "./gradlew" in step)
        self.assertEqual(workflow.count("./gradlew"), 1)
        self.assertIn('          ./gradlew --no-daemon assembleRelease \\\n'
                      '            -PzerusFirebase=true -PzerusVersionCode="$version_code" \\\n'
                      '            -PzerusDevBuildNumber="$DEV_RUN_NUMBER"\n', build)
        # Unit tests and lint run in checks.yml, never with the signer.
        self.assertNotRegex(workflow, r"\b(?:test|lint|check|connected)[A-Z]\w*")
        # Dependency admission requires the same run's gate and trusted checks.
        self.assertEqual(field(job(self.ci, 'dev-tail'), 'needs'), '[gate, qualification]')
        gate = job(self.ci, "gate")
        self.assertEqual(field(gate, "name"), "CI gate")
        self.assertEqual(field(gate, "needs"), "[automatic, review, qualification]")
        for name in ("automatic", "review"):
            self.assertEqual(field(job(self.ci, name), "uses"), "./.github/workflows/checks.yml")
        checks = (ROOT / ".github/workflows/checks.yml").read_text()
        suites = job(checks, "gate")
        self.assertIn("android", field(suites, "needs").strip("[]").split(", "))
        self.assertIn("'android': True,", suites)
        self.assertIn("testDebugUnitTest lintRelease checkReplicaPackaging -PzerusFirebase=true", job(checks, "android"))

    def test_cache_trim_is_main_only_housekeeping_without_checked_out_code(self):
        trim = job(self.ci, "cache-trim")
        self.assertEqual(field(trim, "needs"), "[automatic, qualification]")
        for event, expected in (("push", True), ("schedule", True), ("pull_request", False), ("workflow_dispatch", False)):
            self.assertEqual(self.selected("cache-trim", event, enabled="true"), expected)
            self.assertFalse(self.selected("cache-trim", event, repository="fork/zerus", enabled="true"))
        self.assertEqual(field(trim, "cache-mode"), "none")
        self.assertEqual(field(trim, "runs-on"), "ubuntu-24.04")
        self.assertLessEqual(int(field(trim, "timeout-minutes")), 10)
        # A housekeeping failure must not fail the run, which gates Publish Dev.
        self.assertEqual(field(trim, "continue-on-error"), "true")
        self.assertIn("    permissions:\n      actions: write\n    steps:\n", trim)
        self.assertEqual(len(re.findall(r"(?m)^\s*actions: write", self.ci)), 1)
        # Only the inline trusted script runs with the write token.
        self.assertNotRegex(trim, r"(?m)^\s*(?:- )?uses:")
        self.assertNotIn("checkout", trim)
        self.assertEqual(len(steps(trim)), 1)
        self.assertIn("GH_TOKEN: ${{ github.token }}", trim)
        self.assertIn("'repos/ufna/zerus/actions/caches'", trim)

    def test_cache_trim_keeps_the_two_newest_main_entries_per_key_family(self):
        namespace = {}
        exec(compile(inline_python(job(self.ci, "cache-trim")), "workflow-cache-trim", "exec"), namespace)
        def entry(number, key, created, ref="refs/heads/main"):
            return {"id": number, "key": key, "ref": ref, "created_at": created, "size_in_bytes": 1}
        qt = "windows-qt-6.10.3-win64_msvc2022_64-"
        caches = [
            entry(1, "arch-ccache-v1-100", "2026-10-12T10:00:00.123456Z"),
            entry(2, "arch-ccache-v1-101", "2026-10-12T11:00:00Z"),
            entry(3, "arch-ccache-v1-102", "2026-10-12T12:00:00Z"),
            entry(4, "arch-package-ccache-v1-100", "2026-10-12T10:00:00Z"),
            # Equal timestamps keep the later entry.
            entry(5, "mac-desktop-ccache-v1-ARM64-100", "2026-10-12T10:00:00Z"),
            entry(6, "mac-desktop-ccache-v1-ARM64-101", "2026-10-12T10:00:00Z"),
            entry(7, "mac-desktop-ccache-v1-ARM64-102", "2026-10-12T10:00:00Z"),
            entry(8, "windows-ccache-v1-X64-qt6.10.3-100", "2026-10-12T10:00:00Z"),
            entry(9, "cli-Linux-X64-1a2b3c-2026w41-" + "a" * 64, "2026-10-10T10:00:00Z"),
            entry(10, "cli-Linux-X64-1a2b3c-2026w41-" + "b" * 64, "2026-10-12T10:00:00Z"),
            entry(11, "cli-Linux-X64-1a2b3c-2026w41-" + "c" * 64, "2026-10-11T10:00:00Z"),
            entry(12, "cli-macOS-ARM64-1a2b3c-2026w41-" + "a" * 64, "2026-10-01T10:00:00Z"),
            entry(13, "msrv-Linux-1.85-2026w41-" + "a" * 64, "2026-10-01T10:00:00Z"),
            entry(14, qt + "d" * 64, "2026-10-01T10:00:00Z"),
            entry(15, qt + "e" * 64, "2026-10-12T10:00:00Z"),
            entry(16, "arch-ccache-v1-90", "2026-10-12T11:59:59Z", "refs/pull/7/merge"),
            entry(17, "arch-ccache-v1-91", "2026-10-12T12:00:01Z", "refs/pull/8/merge"),
            entry(18, "arch-ccache-v1-1", "2026-09-01T00:00:00Z", "refs/heads/feature"),
        ]
        now = datetime.datetime(2026, 10, 13, 12, tzinfo=datetime.timezone.utc)
        self.assertEqual(sorted(cache["id"] for cache in namespace["stale"](caches, now)), [1, 5, 9, 16])

    def test_cache_trim_tolerates_concurrent_deletion_and_reports_totals(self):
        code = compile(inline_python(job(self.ci, "cache-trim")), "workflow-cache-trim", "exec")
        caches = [{"id": n, "key": f"arch-ccache-v1-{n}", "ref": "refs/heads/main",
                   "created_at": f"2026-10-0{n}T00:00:00Z", "size_in_bytes": n * 2**20} for n in range(1, 6)]
        pages = [{"total_count": 5, "actions_caches": caches[:3]}, {"total_count": 5, "actions_caches": caches[3:]}]
        done = SimpleNamespace(returncode=0, stdout="", stderr="")
        gone = SimpleNamespace(returncode=1, stdout="{}", stderr="gh: Not Found (HTTP 404)\n")
        denied = SimpleNamespace(returncode=1, stdout="{}", stderr="gh: Resource not accessible by integration (HTTP 403)\n")
        for results, succeeds in [({1: done, 2: gone, 3: done}, True), ({1: done, 2: denied, 3: done}, False)]:
            deleted = []
            def run(command, **kwargs):
                self.assertEqual(command[:2], ["gh", "api"])
                if command[3:] == ["--method", "DELETE"]:
                    number = int(command[2].removeprefix("repos/ufna/zerus/actions/caches/"))
                    deleted.append(number)
                    return results[number]
                page = int(command[2].removeprefix("repos/ufna/zerus/actions/caches?per_page=100&page="))
                return SimpleNamespace(returncode=0, stdout=json.dumps(pages[page - 1]), stderr="")
            with (self.subTest(succeeds=succeeds), patch("subprocess.run", run),
                  patch("sys.stdout", new_callable=io.StringIO) as output):
                if succeeds:
                    exec(code, {"__name__": "__main__"})
                else:
                    with self.assertRaises(SystemExit):
                        exec(code, {"__name__": "__main__"})
                self.assertEqual(sorted(deleted), [1, 2, 3])
                log = output.getvalue()
                self.assertIn("Deleted refs/heads/main arch-ccache-v1-1 (1.0 MiB)\n", log)
                self.assertIn("Deleted refs/heads/main arch-ccache-v1-3 (3.0 MiB)\n", log)
                self.assertIn(("Already deleted " if succeeds else "Not deleted ") + "refs/heads/main arch-ccache-v1-2", log)
                self.assertIn("Deleted 2 of 5 entries, 0.00 of 0.01 GiB.\n", log)

    def test_candidate_verification_has_a_read_only_reusable_chain(self):
        for name in ("release.yml", "nightly.yml"):
            with self.subTest(workflow=name):
                text = (ROOT / ".github/workflows" / name).read_text()
                verify = job(text, "verify")
                self.assertEqual(field(verify, "cache-mode"), "write" if name == "nightly.yml" else "read")
                self.assertEqual(field(verify, "uses"), "./.github/workflows/checks.yml")
                checks = (ROOT / ".github/workflows/checks.yml").read_text()
                # GitHub validates nested permissions even for skipped jobs.
                # A candidate must never request publication write permissions.
                self.assertNotRegex(checks, r"(?m)^\s*contents:\s*write\s*$")
                self.assertNotIn("./.github/workflows/dev.yml", checks)
                self.assertIn("run_linux: true", verify)
                self.assertIn("run_arch: true", verify)
                self.assertIn("run_macos:", verify)


    def test_apple_signing_secrets_stay_in_the_signing_job(self):
        text = (ROOT / ".github/workflows/macos-cask.yml").read_text()
        self.assertRegex(text, r"(?m)^cache-mode: none$")
        self.assertNotRegex(text, r"actions/cache[@/]")
        sign = job(text, "sign")
        self.assertRegex(sign, r"(?m)^    environment: macos-signing$")
        apple_secrets = {"MACOS_DEVELOPER_ID_P12", "MACOS_DEVELOPER_ID_P12_PASSWORD", "APPLE_NOTARY_API_KEY_P8",
                         "APPLE_NOTARY_API_KEY_ID", "APPLE_NOTARY_API_ISSUER_ID"}
        declarations = text.split("    secrets:\n", 1)[1].split("\npermissions:", 1)[0]
        self.assertEqual(set(re.findall(r"(?m)^      (\w+):$", declarations)), apple_secrets)
        for secret in apple_secrets:
            self.assertIn(f"secrets.{secret}", sign)
            required = "false" if secret == "APPLE_NOTARY_API_ISSUER_ID" else "true"
            self.assertIn(f"      {secret}:\n        required: {required}", declarations)
        self.assertIn("vars.MACOS_TEAM_ID", sign)
        self.assertIn('test "$WORKFLOW_REF" = refs/heads/main', sign)
        self.assertRegex(sign, r"if: always\(\)\n        run: bash scripts/ci/macos-keychain.sh delete")
        for name in ("bundle", "verify"):
            self.assertNotIn("secrets.", job(text, name), name)
            self.assertNotIn("environment:", job(text, name), name)
        self.assertNotIn("environment: release", text)
        for name in ("release.yml", "nightly.yml", "ci-dev-tail.yml"):
            workflow = (ROOT / ".github/workflows" / name).read_text()
            self.assertNotIn("secrets: inherit", workflow, name)
            macos = job(workflow, "dev-macos" if name == "ci-dev-tail.yml" else "macos")
            forwarded = dict(re.findall(r"(?m)^      (\w+): \$\{\{ secrets\.(\w+) \}\}$", macos))
            self.assertEqual(forwarded, {secret: secret for secret in apple_secrets}, name)
            for secret in apple_secrets:
                self.assertNotIn(f"secrets.{secret}", workflow.replace(macos, ""), name)

        bridge = job(self.ci, 'dev-tail')
        forwarded = set(re.findall(r'secrets\.(\w+)', bridge))
        self.assertEqual(forwarded, apple_secrets | {'ANDROID_KEYSTORE_BASE64', 'ANDROID_STORE_PASSWORD',
                         'ANDROID_KEY_ALIAS', 'ANDROID_KEY_PASSWORD', 'ANDROID_GOOGLE_SERVICES_JSON'})
        for secret in apple_secrets:
            self.assertNotIn(f'secrets.{secret}', self.ci.replace(bridge, ''))
        self.assertNotIn('secrets: inherit', self.ci)

    def test_app_store_connect_secrets_stay_in_the_ios_upload_job(self):
        text = (ROOT / ".github/workflows/ios-testflight.yml").read_text()
        self.assertRegex(text, r"(?m)^cache-mode: none$")
        self.assertNotRegex(text, r"actions/cache[@/]")
        upload = job(text, "upload")
        self.assertRegex(upload, r"(?m)^    environment: ios-testflight$")
        self.assertEqual(field(upload, "runs-on"), "macos-15")
        self.assertRegex(field(upload, "timeout-minutes"), r"^\d+$")
        self.assertIn('test "$WORKFLOW_REF" = refs/heads/main', upload)
        self.assertIn('test "$WORKFLOW_REPOSITORY" = ufna/zerus', upload)
        self.assertEqual(re.findall(r"uses: ([\w/-]+)@[0-9a-f]{40} # v", upload), ["actions/checkout"])
        self.assertIn("persist-credentials: false", upload)
        # The task replica's Rust build, whose dependencies run build scripts, ends before any secret is in the environment.
        self.assertLess(upload.index("bash mobile/ios/scripts/build-replica.sh"), upload.index("secrets."))
        self.assertIn('if: always()\n        run: |\n          security delete-keychain '
                      '"$RUNNER_TEMP/app-store-connect/signing.keychain-db" 2> /dev/null || true\n'
                      '          rm -rf "$RUNNER_TEMP/app-store-connect"', upload)
        asc_secrets = {"APP_STORE_CONNECT_API_KEY_P8", "APP_STORE_CONNECT_API_KEY_ID", "APP_STORE_CONNECT_API_ISSUER_ID",
                       "IOS_DEVELOPMENT_P12", "IOS_DEVELOPMENT_P12_PASSWORD"}
        declarations = text.split("    secrets:\n", 1)[1].split("\n  workflow_dispatch:", 1)[0]
        self.assertEqual(set(re.findall(r"(?m)^      (\w+):$", declarations)), asc_secrets)
        for secret in asc_secrets:
            self.assertIn(f"secrets.{secret}", upload)
        self.assertIn("vars.IOS_TEAM_ID", upload)
        # Dispatch offers only Stable; Dev build numbers come from the CI sequence.
        dispatch = text.split("  workflow_dispatch:\n", 1)[1].split("\npermissions:", 1)[0]
        self.assertIn("options: [stable]", dispatch)
        ci = (ROOT / '.github/workflows/nightly.yml').read_text()
        self.assertNotIn("secrets: inherit", ci)
        dev_ios = job(ci, "ios")
        self.assertEqual(field(dev_ios, "uses"), "./.github/workflows/ios-testflight.yml")
        self.assertIn("channel: dev", dev_ios)
        forwarded = dict(re.findall(r"(?m)^      (\w+): \$\{\{ secrets\.(\w+) \}\}$", dev_ios))
        self.assertEqual(forwarded, {secret: secret for secret in asc_secrets})
        for secret in asc_secrets:
            self.assertNotIn(f"secrets.{secret}", ci.replace(dev_ios, ""))
        for workflow in (ROOT / ".github/workflows").glob("*.yml"):
            if workflow.name not in {"nightly.yml", "ios-testflight.yml"}:
                self.assertNotIn("APP_STORE_CONNECT_API", workflow.read_text(), workflow.name)
                self.assertNotIn("IOS_DEVELOPMENT_P12", workflow.read_text(), workflow.name)

    def test_ios_testflight_dev_follows_only_full_nightly_verification(self):
        expression = field(job((ROOT / '.github/workflows/nightly.yml').read_text(), 'ios'), "if")
        self.assertIn("!cancelled()", expression)
        expression = expression.replace("&&", " and ").replace("||", " or ")
        expression = re.sub(r"!(?!=)", "not ", expression)
        expression = re.sub(r"\bfalse\b", "False", expression)
        expression = re.sub(r"\bneeds\.([\w-]+)\.", r"needs['\1'].", expression)

        def runs(gate="success", event="schedule", ref="refs/heads/main", enabled="true",
                 repository="ufna/zerus", private=False, cancelled=False):
            return bool(eval(expression, {"__builtins__": {}}, {
                "cancelled": lambda: cancelled,
                "github": SimpleNamespace(repository=repository, ref=ref, event_name=event,
                                          event=SimpleNamespace(repository=SimpleNamespace(private=private))),
                "vars": SimpleNamespace(IOS_TESTFLIGHT_ENABLED=enabled),
                "needs": {"gate": SimpleNamespace(result=gate)},
            }))

        self.assertTrue(runs())
        for change in ({"gate": "failure"}, {"gate": "skipped"}, {"gate": "cancelled"},
                       {"event": "pull_request"}, {"event": "push"}, {"ref": "refs/heads/feature"},
                       {"enabled": ""}, {"repository": "fork/zerus"}, {"private": True}, {"cancelled": True}):
            with self.subTest(change=change):
                self.assertFalse(runs(**change))

    def assemble_runs(self, workflow, verify, macos, enabled, requested=True, source="success"):
        expression = field(job(workflow, "assemble"), "if")
        self.assertIn("!cancelled()", expression)
        expression = expression.replace("&&", " and ").replace("||", " or ")
        expression = re.sub(r"!(?!=)", "not ", expression)
        expression = re.sub(r"(?<!')\btrue\b(?!')", "True", expression)
        return bool(eval(expression, {"__builtins__": {}}, {
            "cancelled": lambda: False, "inputs": SimpleNamespace(macos_cask=requested),
            "vars": SimpleNamespace(MACOS_CASKS_ENABLED="true" if enabled else ""),
            "needs": SimpleNamespace(verify=SimpleNamespace(result=verify), gate=SimpleNamespace(result=verify), macos=SimpleNamespace(result=macos), source=SimpleNamespace(result=source)),
        }))

    def test_casks_are_built_only_once_signing_is_enabled(self):
        for name, channel in (("release.yml", "stable"), ("nightly.yml", "nightly")):
            with self.subTest(workflow=name):
                workflow = (ROOT / ".github/workflows" / name).read_text()
                macos = job(workflow, "macos")
                self.assertEqual(field(macos, "uses"), "./.github/workflows/macos-cask.yml")
                self.assertIn(f"channel: {channel}", macos)
                self.assertIn("vars.MACOS_CASKS_ENABLED == 'true'", field(macos, "if"))
                self.assertIn("gate" if name == "nightly.yml" else "verify", field(macos, "needs"))
                for requested in (True, False):
                    if name == "nightly.yml" and not requested:
                        continue
                    for enabled in (True, False):
                        wanted = enabled and requested
                        expected = {"success": True, "skipped": not wanted, "failure": False, "cancelled": False}
                        for result, runs in expected.items():
                            self.assertEqual(self.assemble_runs(workflow, "success", result, enabled, requested), runs,
                                             (requested, enabled, result))
                        self.assertFalse(self.assemble_runs(workflow, "failure", "skipped", enabled, requested))

    def test_release_rehearsal_cannot_sign_publish_or_succeed_with_a_partial_matrix(self):
        ci = (ROOT / '.github/workflows/ci.yml').read_text()
        source = job(ci, 'rehearsal-source')
        for platform in ('linux', 'arch', 'macos', 'windows', 'ios'):
            self.assertIn('inputs.run_' + platform, field(source, 'if'))
        self.assertEqual(field(source, 'uses'), './.github/workflows/release-source.yml')
        for name in ('rehearsal-source', 'rehearsal-macos', 'rehearsal-assemble', 'rehearsal-seal'):
            selected = job(ci, name)
            self.assertIn("github.event_name == 'workflow_dispatch'", field(selected, 'if'))
            self.assertIn('inputs.release_rehearsal', field(selected, 'if'))
            self.assertNotIn('secrets.', selected)
            self.assertNotIn('environment:', selected)
            self.assertEqual(field(selected, 'cache-mode'), 'none')
        seal = job(ci, 'rehearsal-seal')
        self.assertIn('release-rehearsal-${{ github.sha }}', seal)
        self.assertNotIn('-candidate', seal)
        code = inline_python(seal)
        for failed in (None, 'review', 'rehearsal-source', 'rehearsal-macos', 'rehearsal-assemble'):
            results = {key: {'result': 'success'} for key in ('review', 'rehearsal-source', 'rehearsal-macos', 'rehearsal-assemble')}
            if failed: results[failed]['result'] = 'skipped'
            with self.subTest(failed=failed), patch.dict(os.environ, RESULTS=json.dumps(results)):
                if failed:
                    with self.assertRaises(AssertionError): exec(code, {})
                else:
                    exec(code, {})
        # Only the existing trusted-main push path can reach the signing tail.
        self.assertIn("github.event_name == 'push'", field(job(ci, 'dev-tail'), 'if'))

    def test_stable_unsigned_overlap_keeps_the_full_gate_and_exact_version(self):
        release = (ROOT / '.github/workflows/release.yml').read_text()
        bundle = job(release, 'macos-unsigned')
        self.assertEqual(field(bundle, 'needs'), 'version')
        self.assertEqual(field(bundle, 'uses'), './.github/workflows/macos-bundle.yml')
        self.assertNotIn('secrets.', bundle)
        self.assertIn('run_tests: false', bundle)
        self.assertIn('tests_in_verify: true', bundle)
        self.assertIn('run_macos: true', job(release, 'verify'))
        self.assertIn('arch_check_in_source: true', job(release, 'verify'))
        macos = job(release, 'macos')
        self.assertEqual(field(macos, 'needs'), '[verify, macos-unsigned]')
        self.assertIn("needs.verify.result == 'success'", field(macos, 'if'))
        self.assertIn("needs.macos-unsigned.result == 'success'", field(macos, 'if'))
        self.assertIn('reuse_unsigned: true', macos)
        self.assertIn('unsigned_release: ${{ needs.macos-unsigned.outputs.release }}', macos)
        signing = job((ROOT / '.github/workflows/macos-cask.yml').read_text(), 'sign')
        code = inline_python(signing)
        self.assertLess(signing.index('Bind the parallel candidate'), signing.index('Prepare a temporary signing keychain'))
        with tempfile.TemporaryDirectory(prefix='zerus-macos-reuse-') as tmp:
            root = Path(tmp); (root / 'VERSION').write_text('0.99.0'); (root / 'DEV_VERSION').write_text('0.100.0')
            for channel, event, version, admitted in [
                    ('stable', 'workflow_dispatch', '0.99.0', True),
                    ('dev', 'push', '0.100.0-dev.123', True),
                    ('stable', 'push', '0.99.0', False),
                    ('stable', 'pull_request', '0.99.0', False),
                    ('stable', 'workflow_dispatch', '0.98.0', False),
                    ('nightly', 'workflow_dispatch', '0.99.0', False),
                    ('dev', 'workflow_dispatch', '0.100.0-dev.123', False),
                    ('dev', 'push', '0.100.0-dev.122', False)]:
                with self.subTest(channel=channel, event=event, version=version), \
                        patch.dict(os.environ, CHANNEL=channel, EVENT_NAME=event, RELEASE=version, RUN_NUMBER='123'), \
                        patch('pathlib.Path', side_effect=lambda p: root / p):
                    if admitted:
                        exec(code, {})
                    else:
                        with self.assertRaises(AssertionError): exec(code, {})

    def test_release_parallel_source_and_install_jobs_cannot_bypass_sealing(self):
        workflow = (ROOT / '.github/workflows/release.yml').read_text()
        source = job(workflow, 'source')
        self.assertEqual(field(source, 'needs'), 'version')
        self.assertEqual(field(source, 'cache-mode'), 'none')
        self.assertNotIn('secrets.', source)
        self.assertNotIn('actions/cache', source)
        source_worker = job((ROOT / '.github/workflows/release-source.yml').read_text(), 'source')
        self.assertEqual(field(source, 'uses'), './.github/workflows/release-source.yml')
        self.assertIn('--source-only', source_worker)
        assemble = job(workflow, 'assemble')
        self.assertEqual(field(assemble, 'needs'), '[verify, source, macos]')
        self.assertEqual(field(assemble, 'uses'), './.github/workflows/release-assemble.yml')
        worker = job((ROOT / '.github/workflows/release-assemble.yml').read_text(), 'assemble')
        self.assertIn('--source-check artifacts/release-source', worker)
        self.assertIn('--binary-only --defer-seal', worker)
        self.assertNotIn('scripts/seal-release.py', worker)
        for result in ('failure', 'cancelled', 'skipped'):
            self.assertFalse(self.assemble_runs(workflow, 'success', 'success', True, source=result))
        self.assertIn('verify_installation: false', job(workflow, 'macos'))
        install = job(workflow, 'cask-install')
        self.assertEqual(field(install, 'needs'), 'macos')
        self.assertEqual(field(install, 'uses'), './.github/workflows/macos-cask-install.yml')
        seal = job(workflow, 'seal')
        self.assertEqual(field(seal, 'needs'), '[assemble, cask-install]')
        self.assertIn('scripts/seal-release.py dist', seal)
        # Evaluate the real workflow predicate for every terminal result and
        # optional-cask setting; a failed selected install can never seal.
        expression = field(seal, 'if').replace('needs.cask-install', 'needs.install')
        expression = expression.replace('&&', ' and ').replace('||', ' or ')
        expression = re.sub(r'!(?!=)', 'not ', expression)
        for assembled, installed, wanted, cancelled in product(
                ('success', 'failure', 'cancelled', 'skipped'),
                ('success', 'failure', 'cancelled', 'skipped'), (True, False), (True, False)):
            actual = bool(eval(expression, {'__builtins__': {}}, {
                'cancelled': lambda: cancelled,
                'inputs': SimpleNamespace(macos_cask=wanted),
                'vars': SimpleNamespace(MACOS_CASKS_ENABLED='true'),
                'needs': SimpleNamespace(assemble=SimpleNamespace(result=assembled), install=SimpleNamespace(result=installed)),
            }))
            self.assertEqual(actual, not cancelled and assembled == 'success' and
                             (installed == 'success' or (installed == 'skipped' and not wanted)))

    def test_release_draft_survives_a_skipped_cask(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        expression = field(job(workflow, "draft"), "if")
        # Without a status function GitHub adds success() over the whole chain,
        # which would skip the draft whenever the optional cask was skipped.
        self.assertIn("!cancelled()", expression)
        self.assertIn("needs.seal.result == 'success'", expression)
        self.assertIn("inputs.create_draft", expression)

    def test_tap_deploy_key_is_only_available_to_the_publishers(self):
        for workflow in (ROOT / ".github/workflows").glob("*.yml"):
            text = workflow.read_text()
            if "HOMEBREW_TAP_DEPLOY_KEY" in text:
                self.assertIn(workflow.name, {"publish.yml", "publish-nightly.yml", "publish-dev.yml"})
                publish = job(text, "homebrew" if workflow.name == "publish-dev.yml" else "publish")
                self.assertRegex(publish, r"(?m)^    environment: release$")
                self.assertIn("--tap-ssh-key", publish)
                if workflow.name == "publish-dev.yml":
                    self.assertIn("    needs: publish\n", publish)
                    self.assertIn("group: release-publication", publish)
                    self.assertIn("persist-credentials: false", publish)


SECURITY_STUB = r"""#!/usr/bin/env bash
case "$1" in
list-keychains)
    if [ "${4:-}" = -s ]; then shift 4; echo "list-keychains-set $*" >> "$FAKE_LOG"; printf '%s\n' "$@" > "$FAKE_LIST"
    else sed 's/.*/    "&"/' "$FAKE_LIST"; fi ;;
create-keychain)
    # Like macOS, a new keychain joins the user's search list.
    echo "create-keychain $4" >> "$FAKE_LOG"; : > "$4"; echo "$4" >> "$FAKE_LIST" ;;
delete-keychain) echo "delete-keychain $2" >> "$FAKE_LOG"; rm -f "$2" ;;
import) echo "import" >> "$FAKE_LOG"; test -s "$2" ;;
find-identity) printf '%s\n' "$FAKE_IDENTITIES" ;;
find-certificate) cat "$FAKE_CERTIFICATE" ;;
*) echo "$1" >> "$FAKE_LOG" ;;
esac
"""

XCODEBUILD_STUB = r"""#!/usr/bin/env bash
for argument in "$@"; do
    if [ "$argument" = -exportArchive ]; then echo "export" >> "$FAKE_LOG"; exit 0; fi
done
echo "archive" >> "$FAKE_LOG"
[ "${FAKE_ARCHIVE:-}" != fail ] || exit 65
while [ "$#" -gt 0 ]; do [ "$1" != -archivePath ] || archive=$2; shift; done
mkdir -p "$archive/Products/Applications/Zerus.app"
cp "$FAKE_INFO_PLIST" "$archive/Products/Applications/Zerus.app/Info.plist"
"""

CODESIGN_STUB = r"""#!/usr/bin/env bash
for argument in "$@"; do
    case "$argument" in --extract-certificates=*) cp "$FAKE_LEAF" "${argument#*=}0" ;; esac
done
"""


class TestflightUploadSigningTests(unittest.TestCase):
    """The upload script with stand-ins for security, xcodebuild and codesign."""

    script = ROOT / "mobile/ios/scripts/testflight-upload.sh"
    team = "ABCDE12345"
    password = "p12-password-for-tests"

    def setUp(self):
        self.temp = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(self.temp)], check=True))
        bin_dir = self.temp / "bin"
        bin_dir.mkdir()
        for name, body in (("security", SECURITY_STUB), ("xcodebuild", XCODEBUILD_STUB), ("codesign", CODESIGN_STUB)):
            (bin_dir / name).write_text(body)
            (bin_dir / name).chmod(0o755)
        self.certificate, self.leaf, self.sha1 = self.make_certificate("ci", self.team)
        _, self.other_leaf, _ = self.make_certificate("other", self.team)
        version = (ROOT / "mobile/ios/DEV_VERSION").read_text().strip()
        info = self.temp / "Info.plist"
        info.write_bytes(plistlib.dumps({
            "CFBundleIdentifier": "app.zerus.mobile", "CFBundleShortVersionString": version,
            "CFBundleVersion": "948", "ZerusBuildChannel": "dev", "ZerusApnsEnvironment": "production"}))
        self.work = self.temp / "runner/app-store-connect"
        self.log = self.temp / "calls.log"
        self.original = str(self.temp / "login.keychain-db")
        self.search_list = self.temp / "search-list"
        self.search_list.write_text(self.original + "\n")
        self.env = {
            "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}", "HOME": str(self.temp),
            "RUNNER_TEMP": str(self.temp / "runner"), "GITHUB_ACTIONS": "true", "CHANNEL": "dev",
            "BUILD_NUMBER": "948", "SOURCE_COMMIT": "a" * 40, "TEAM_ID": self.team, "ASC_KEY": "fake-p8",
            "ASC_KEY_ID": "ABCDEFGH12", "ASC_ISSUER_ID": "12345678-1234-1234-1234-123456789abc",
            "IOS_DEVELOPMENT_P12": base64.b64encode(b"fake-p12").decode(),
            "IOS_DEVELOPMENT_P12_PASSWORD": self.password,
            "FAKE_LOG": str(self.log), "FAKE_LIST": str(self.search_list),
            "FAKE_IDENTITIES": self.identities((self.sha1, "Apple Development: Created via API (TESTUID001)")),
            "FAKE_CERTIFICATE": str(self.certificate), "FAKE_LEAF": str(self.leaf), "FAKE_INFO_PLIST": str(info),
        }

    def make_certificate(self, name, team):
        pem, der = self.temp / f"{name}.pem", self.temp / f"{name}.der"
        subject = f"/UID=TESTUID001/CN=Apple Development: Created via API (TESTUID001)/OU={team}/O=Example/C=US"
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout",
                        str(self.temp / f"{name}.key"), "-out", str(pem), "-days", "2", "-subj", subject],
                       check=True, capture_output=True)
        subprocess.run(["openssl", "x509", "-in", str(pem), "-outform", "DER", "-out", str(der)],
                       check=True, capture_output=True)
        return pem, der, hashlib.sha1(der.read_bytes()).hexdigest().upper()

    @staticmethod
    def identities(*entries):
        lines = [f'  {index}) {sha1} "{name}"' for index, (sha1, name) in enumerate(entries, 1)]
        return "\n".join(lines + [f"     {len(entries)} valid identities found"])

    def run_upload(self, **changes):
        env = {**self.env, **changes}
        env = {key: value for key, value in env.items() if value is not None}
        result = subprocess.run(["bash", str(self.script)], env=env, capture_output=True, text=True, timeout=60)
        calls = self.log.read_text().splitlines() if self.log.exists() else []
        self.assertNotIn(self.password, result.stdout + result.stderr + "\n".join(calls))
        return result, calls

    def assert_cleaned(self, calls):
        keychain = str(self.work / "signing.keychain-db")
        changed = calls.index(f"list-keychains-set {keychain} {self.original}")
        self.assertEqual(calls[changed + 1:].count(f"list-keychains-set {self.original}"), 1)
        self.assertIn(f"delete-keychain {keychain}", calls)
        self.assertEqual(self.search_list.read_text().splitlines(), [self.original])
        for name in ("signing.keychain-db", "development.p12", "AuthKey_ABCDEFGH12.p8"):
            self.assertFalse((self.work / name).exists(), name)

    def test_ci_requires_the_development_identity_before_signing(self):
        for missing in ("IOS_DEVELOPMENT_P12", "IOS_DEVELOPMENT_P12_PASSWORD"):
            with self.subTest(missing=missing):
                result, calls = self.run_upload(**{missing: None})
                self.assertEqual(result.returncode, 2)
                self.assertIn("IOS_DEVELOPMENT_P12 and IOS_DEVELOPMENT_P12_PASSWORD are missing", result.stderr)
                self.assertEqual(calls, [])

    def test_identity_import_is_refused_outside_ci(self):
        result, calls = self.run_upload(GITHUB_ACTIONS=None)
        self.assertEqual(result.returncode, 2)
        self.assertIn("imported only on a CI runner", result.stderr)
        self.assertEqual(calls, [])

    def test_failed_archive_keeps_its_status_and_restores_the_keychains(self):
        result, calls = self.run_upload(FAKE_ARCHIVE="fail")
        self.assertEqual(result.returncode, 65)
        self.assertNotIn("export", calls)
        self.assert_cleaned(calls)

    def test_archive_signed_by_another_identity_is_not_uploaded(self):
        result, calls = self.run_upload(FAKE_LEAF=str(self.other_leaf))
        self.assertEqual(result.returncode, 1)
        self.assertIn("signed by an unexpected identity", result.stderr)
        self.assertIn("archive", calls)
        self.assertNotIn("export", calls)
        self.assert_cleaned(calls)

    def test_only_one_current_development_identity_of_the_team_is_accepted(self):
        foreign, _, foreign_sha1 = self.make_certificate("foreign", "ZYXWV98765")
        name = "Apple Development: Created via API (TESTUID001)"
        cases = {
            "none": {"FAKE_IDENTITIES": self.identities()},
            "two": {"FAKE_IDENTITIES": self.identities((self.sha1, name), ("B" * 40, name))},
            "distribution": {"FAKE_IDENTITIES": self.identities((self.sha1, "Apple Distribution: Example (ABCDE12345)"))},
            "mismatch": {"FAKE_IDENTITIES": self.identities(("C" * 40, name))},
            "team": {"FAKE_IDENTITIES": self.identities((foreign_sha1, name)), "FAKE_CERTIFICATE": str(foreign)},
        }
        for case, changes in cases.items():
            with self.subTest(case=case):
                self.log.unlink(missing_ok=True)
                self.search_list.write_text(self.original + "\n")
                result, calls = self.run_upload(**changes)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertNotIn("archive", calls)
                self.assert_cleaned(calls)

    def test_archive_signed_by_the_imported_identity_continues_to_the_upload(self):
        result, calls = self.run_upload()
        self.assertNotIn("unexpected identity", result.stderr)
        self.assert_cleaned(calls)
        if sys.platform == "darwin":
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(calls.count("export"), 1)
        else:
            # PlistBuddy exists only on macOS; the identity checks above ran first.
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("export", calls)


class WindowsInstallerPublicationWorkflow(unittest.TestCase):
    def test_nightly_version_reaches_the_same_checks_that_build_and_test_the_installer(self):
        nightly = (ROOT / '.github/workflows/nightly.yml').read_text()
        checks = (ROOT / '.github/workflows/checks.yml').read_text()
        for name in ('verify', 'review'):
            part = job(nightly, name)
            self.assertIn('needs: version', part)
            self.assertIn('windows_installer_version: ${{ needs.version.outputs.installer }}', part)
        version = job(nightly, 'version')
        self.assertIn('cache-mode: none', version)
        self.assertIn('persist-credentials: false', version)
        self.assertIn("github.ref == 'refs/heads/main'", version)
        self.assertNotIn('secrets.', version)
        ci = (ROOT / '.github/workflows/ci.yml').read_text()
        self.assertIn("windows_installer_version: ${{ inputs.windows_installer_version || '' }}", job(ci, 'review'))
        windows = job(checks, 'windows-desktop')
        self.assertIn('WINDOWS_INSTALLER_VERSION: ${{ inputs.windows_installer_version }}', windows)
        self.assertIn('--info artifacts/installer/windows-installer-info.json', windows)
        self.assertIn('HGS_TEST_INSTALLER_VERSION=', windows)
        for workflow in ('release.yml', 'nightly.yml'):
            assembly = job((ROOT / '.github/workflows' / ('release-assemble.yml' if workflow == 'release.yml' else workflow)).read_text(), 'assemble')
            self.assertIn('--windows-installer artifacts/windows/installer/zerus-windows-x86_64-setup.exe', assembly)
            self.assertIn('--windows-installer-info artifacts/windows/installer/windows-installer-info.json', assembly)
            self.assertNotIn('secrets.', assembly)



class Null(SimpleNamespace):
    """A GitHub context object: a missing property reads as null."""

    def __getattr__(self, name):
        return None


def github_expression(text, github, inputs=None, variables=None):
    """Evaluate the GitHub expression operators these workflows use."""
    def fmt(pattern, *args):
        return re.sub(r"\{(\d+)\}", lambda match: str(args[int(match[1])]), pattern)
    python = text.replace("&&", " and ").replace("||", " or ")
    python = re.sub(r"!(?!=)", "not ", python)
    python = re.sub(r"\bfalse\b", "False", re.sub(r"\btrue\b", "True", python))
    python = python.replace("format(", "fmt(")
    return eval(python, {"__builtins__": {}}, {
        "fmt": fmt, "github": github, "inputs": inputs or Null(), "vars": variables or Null()})


def rendered(value, github, **context):
    """A workflow value: a whole expression, a template with expressions, or a literal."""
    whole = re.fullmatch(r"\$\{\{ (.*) \}\}", value)
    if whole:
        return github_expression(whole[1], github, **context)
    if value in ("true", "false"):
        return value == "true"
    return re.sub(r"\$\{\{ (.*?) \}\}", lambda match: str(github_expression(match[1], github, **context)), value)


class PullRequestRunCancellationTests(unittest.TestCase):
    """Superseded and closed pull requests stop holding runners (zerus-b8zze)."""

    WORKFLOWS = ROOT / ".github/workflows"

    def pull_request_workflows(self):
        found = {}
        for path in sorted(self.WORKFLOWS.glob("*.yml")):
            text = path.read_text()
            on = re.search(r"(?ms)^on:\n(.*?)^\S", text)[1]
            if re.search(r"(?m)^  pull_request:", on):
                found[path.name] = text
        return found

    def concurrency(self, text):
        match = re.search(r"(?m)^concurrency:\n  group: (.*)\n(?:  #.*\n)*  cancel-in-progress: (.*)$", text)
        self.assertIsNotNone(match, "a workflow-level concurrency group")
        return match[1], match[2]

    @staticmethod
    def event(name, number=7, sha="a" * 40, action="synchronize", label=None, run_id=4242, workflow="Workflow"):
        pull = Null(number=number, head=Null(sha=sha), labels=[])
        payload = Null(action=action, pull_request=pull if name == "pull_request" else None,
                       label=Null(name=label) if label else None, repository=Null(private=False))
        ref = f"refs/pull/{number}/merge" if name == "pull_request" else "refs/heads/main"
        return Null(event_name=name, event=payload, ref=ref, run_id=run_id, repository="ufna/zerus",
                    workflow=workflow, sha=sha)

    def test_every_pull_request_workflow_cancels_its_own_older_runs(self):
        workflows = self.pull_request_workflows()
        # The guard covers every PR workflow, the close handler included.
        self.assertTrue({"ci.yml", "kimi-native.yml", "kimi-approvals.yml", "agent-hint.yml", "android-approval-ui.yml",
                         "android-project-ui.yml", "ios-approval-ui.yml", "pr-closed.yml"} <= set(workflows))
        groups = {}
        for name, text in workflows.items():
            group, cancel = self.concurrency(text)
            context = {"variables": Null(DEV_UPDATES_ENABLED="true")}
            for action in ("opened", "synchronize", "reopened", "closed"):
                if name == "pr-closed.yml" and action != "closed":
                    continue
                with self.subTest(workflow=name, action=action):
                    first = self.event("pull_request", 7, "a" * 40, action, workflow=name)
                    newer = self.event("pull_request", 7, "b" * 40, action, run_id=4343, workflow=name)
                    other = self.event("pull_request", 8, "a" * 40, action, workflow=name)
                    # One group per PR: a new head joins it and an older run is cancelled.
                    self.assertEqual(rendered(group, first, **context), rendered(group, newer, **context))
                    self.assertNotEqual(rendered(group, first, **context), rendered(group, other, **context))
                    self.assertIs(rendered(cancel, newer, **context), True)
            groups[name] = rendered(group, self.event("pull_request", 7, workflow=name), variables=Null())
        # Workflows never share a PR's group, so one cannot cancel another's run.
        self.assertEqual(len(set(groups.values())), len(groups), groups)

    def test_manual_and_main_runs_keep_their_concurrency(self):
        expected = {
            "kimi-native.yml": "kimi-native-refs/heads/main",
            "kimi-approvals.yml": "kimi-approvals-refs/heads/main",
            "agent-hint.yml": "agent-hint-run-4242",
            "android-approval-ui.yml": "android-approval-ui-run-4242",
            "android-project-ui.yml": "android-project-ui-run-4242",
            "ios-approval-ui.yml": "ios-approval-ui-run-4242",
        }
        manual = {name for name, text in self.pull_request_workflows().items()
                  if re.search(r"(?m)^  workflow_dispatch:", text)}
        # ci.yml keeps its own PR/manual and Dev main rules (tested above).
        self.assertEqual(manual - {"ci.yml"}, set(expected))
        for name, value in expected.items():
            group, cancel = self.concurrency((self.WORKFLOWS / name).read_text())
            dispatch = self.event("workflow_dispatch", workflow=name)
            with self.subTest(workflow=name):
                # Kimi keeps its per-ref noncancelling group; the others stay independent per run.
                self.assertEqual(rendered(group, dispatch), value)
                self.assertIs(rendered(cancel, dispatch), False)

    def test_another_label_neither_cancels_nor_repeats_an_opted_in_hint_run(self):
        text = (self.WORKFLOWS / "agent-hint.yml").read_text()
        group, cancel = self.concurrency(text)
        condition = field(job(text, "native"), "if")
        opt_in = "contains(github.event.pull_request.labels.*.name, 'native-agent-hint')"
        self.assertIn(opt_in, condition)
        per_pr = rendered(group, self.event("pull_request", action="synchronize"))
        for action, label, opted, cancels, runs in [
            ("synchronize", None, True, True, True),
            ("labeled", "native-agent-hint", True, True, True),
            ("labeled", "documentation", True, False, False),
            ("labeled", "documentation", False, False, False),
            ("opened", None, False, True, False),
        ]:
            with self.subTest(action=action, label=label, opted=opted):
                github = self.event("pull_request", action=action, label=label)
                self.assertIs(rendered(cancel, github), cancels)
                self.assertEqual(bool(github_expression(condition.replace(opt_in, str(opted)), github)), runs)
                # A newer run replaces a pending one in its group even without
                # cancellation: another label's run stays out of the PR's group,
                # so a new head's run waiting there is never displaced (zerus-b8zze.1).
                self.assertEqual(rendered(group, github) == per_pr, cancels)
        self.assertEqual(rendered(group, self.event("pull_request", action="labeled", label="documentation")),
                         "agent-hint-run-4242")
        dispatch = self.event("workflow_dispatch")
        self.assertIs(rendered(cancel, dispatch), False)
        self.assertTrue(github_expression(condition.replace(opt_in, "False"), dispatch))

    def test_the_close_handler_has_only_an_actions_token_and_its_inline_script(self):
        text = (self.WORKFLOWS / "pr-closed.yml").read_text()
        self.assertIn("on:\n  pull_request:\n    branches: [main]\n    types: [closed]\n\n", text)
        self.assertNotIn("pull_request_target", text)
        self.assertNotIn("paths", text)
        self.assertIn("\npermissions: {}\n", text)
        handler = job(text, "cancel")
        self.assertEqual(field(handler, "if"), "github.repository == 'ufna/zerus'")
        self.assertEqual(field(handler, "cache-mode"), "none")
        self.assertLessEqual(int(field(handler, "timeout-minutes")), 10)
        self.assertIn("    permissions:\n      actions: write\n    steps:\n", handler)
        self.assertEqual(re.findall(r"(?m)^\s+(\w[\w-]*): (?:read|write)$", text), ["actions"])
        # No checkout, action or secret: only the trusted inline script holds the token.
        self.assertNotRegex(text, r"(?m)^\s*(?:- )?uses:")
        self.assertNotRegex(text, r"actions/checkout|git (?:clone|fetch|checkout)")
        self.assertNotIn("secrets.", text)
        self.assertEqual(len(steps(handler)), 1)
        self.assertIn("GH_TOKEN: ${{ github.token }}", handler)
        # Event values reach the script only through its environment.
        script = handler.split("        run: |\n", 1)[1]
        self.assertNotIn("${{", script)

    def test_the_close_handler_cancels_only_this_prs_earlier_active_runs(self):
        namespace = {}
        handler = job((self.WORKFLOWS / "pr-closed.yml").read_text(), "cancel")
        exec(compile(inline_python(handler), "workflow-pr-closed", "exec"), namespace)
        closed = namespace["when"]("2026-11-08T12:00:00Z")

        def run(number, event="pull_request", status="queued", branch="feature", repository="ufna/zerus",
                linked=(7,), created="2026-11-08T11:00:00Z"):
            return {"id": number, "event": event, "status": status, "head_branch": branch,
                    "head_repository": {"full_name": repository}, "created_at": created,
                    "pull_requests": [{"number": value} for value in linked]}

        runs = [
            run(1), run(2, status="in_progress"), run(3, status="waiting"), run(4, linked=()),
            run(5, status="completed"), run(6, event="push"), run(7, branch="other"),
            run(8, repository="fork/zerus"), run(9, linked=(8,)), run(10, linked=(7, 8)),
            run(11, created="2026-11-08T12:00:01Z"), run(12, status="in_progress"), run(13, event="pull_request_target"),
        ]
        self.assertEqual(namespace["doomed"](runs, 7, "feature", "ufna/zerus", 12, closed), [1, 2, 3, 4, 10])
        self.assertEqual(namespace["doomed"]([], 7, "feature", "ufna/zerus", 12, closed), [])
        self.assertEqual(namespace["ACTIVE"], ("queued", "in_progress", "waiting", "pending", "requested"))

    def test_runs_that_ignore_the_cancel_are_force_cancelled_after_a_minute(self):
        namespace = {}
        handler = job((self.WORKFLOWS / "pr-closed.yml").read_text(), "cancel")
        script = inline_python(handler)
        exec(compile(script, "workflow-pr-closed", "exec"), namespace)
        closed = namespace["when"]("2026-11-08T12:00:00Z")

        def sweep(call, sleep):
            with patch("sys.stdout", new_callable=io.StringIO):
                namespace["sweep"](call, sleep, 7, "feature", "ufna/zerus", 12, closed)

        def run(number, status="queued", linked=(7,), attempt=1):
            return {"id": number, "event": "pull_request", "status": status, "head_branch": "feature",
                    "head_repository": {"full_name": "ufna/zerus"}, "created_at": "2026-11-08T11:00:00Z",
                    "pull_requests": [{"number": value} for value in linked], "run_attempt": attempt}

        def harness(first, second, failures=()):
            """A fake gh api: two listings around the one wait, and every POST."""
            calls, waits = [], []
            def call(*args):
                if args[:2] == ("-X", "GET"):
                    listed = second if waits else first
                    status = next(arg.split("=", 1)[1] for arg in args if arg.startswith("status="))
                    return 0, json.dumps({"workflow_runs": [entry for entry in listed if entry["status"] == status]}), ""
                calls.append(args[2].rsplit("/", 2)[1:])
                return next(((1, "", error) for path, error in failures if args[2].endswith(path)), (0, "", ""))
            return call, waits.append, calls, waits

        # 1 ignores the cancel; 2 honours it; 3 is another PR's; 4 is first
        # listed after the wait; 12 is this workflow's own run.
        before = [run(1), run(2, "in_progress"), run(3, linked=(8,)), run(12, "in_progress")]
        after = [run(1), run(3, linked=(8,)), run(4), run(12, "in_progress")]
        call, sleep, calls, waits = harness(before, after)
        sweep(call, sleep)
        self.assertEqual(waits, [60])
        self.assertEqual(calls, [["1", "cancel"], ["2", "cancel"], ["1", "force-cancel"], ["4", "cancel"]])
        # A re-run keeps its ID with a new attempt: attempt 2 of run 5, started
        # during the wait after attempt 1 honoured its cancel, gets an ordinary
        # cancel, never a force-cancel (zerus-b8zze.2.1).
        call, sleep, calls, waits = harness([run(1), run(5)], [run(1), run(5, attempt=2)])
        sweep(call, sleep)
        self.assertEqual(calls, [["1", "cancel"], ["5", "cancel"], ["1", "force-cancel"], ["5", "cancel"]])
        # Nothing to cancel: no wait and no second listing.
        call, sleep, calls, waits = harness([run(3, linked=(8,))], [])
        sweep(call, sleep)
        self.assertEqual((calls, waits), ([], []))
        # HTTP 409 means the run had finished; any other failure fails the job.
        call, sleep, calls, waits = harness([run(1)], [run(1)], [("1/force-cancel", "gh: Conflict (HTTP 409)")])
        sweep(call, sleep)
        call, sleep, calls, waits = harness([run(1)], [run(1)], [("1/force-cancel", "gh: Server Error (HTTP 500)")])
        with self.assertRaises(SystemExit):
            sweep(call, sleep)
        self.assertEqual(calls, [["1", "cancel"], ["1", "force-cancel"]])
        # The only POST is a run's cancel or force-cancel.
        self.assertEqual(re.findall(r"'-X', '(\w+)', f?'([^']*)'", script),
                         [("GET", "repos/ufna/zerus/actions/runs"),
                          ("POST", "repos/ufna/zerus/actions/runs/{run_id}/{endpoint}")])
        self.assertEqual(re.findall(r"stop\(call, .*'([\w-]+)'\)", script), ["cancel", "force-cancel", "cancel"])


if __name__ == "__main__":
    unittest.main()
