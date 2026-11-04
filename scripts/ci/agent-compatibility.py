#!/usr/bin/env python3
"""Keyless latest-release Codex, Claude Code and OpenCode contracts and capability snapshots."""
import argparse
import contextlib
import base64
import datetime
import hashlib
import http.client
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = Path(__file__).with_suffix('.json')
MAX_PACKAGE_BYTES = 512 * 1024 * 1024
WINDOWS = 'win32-x64'
HOST_COVERAGE = ('tracked launch, hooks, exact input, input keys (Esc, Ctrl+C, Ctrl+S, clearing), dialog answers, '
                 'clear, pause, resume and terminate through the session host')
# Official npm packages per target. Only the native executable that `members` names
# is extracted, under its own name. On Windows the agents run in the session host,
# where the suite, timeout and coverage of `windows` replace the tmux suites'.
# `targets` maps a host to another package target, `environment` adds suite inputs,
# and `private_xdg` gives the runner's own probes private XDG folders as well.
AGENTS = {
    'codex': dict(
        name='Codex', variable='HGS_CODEX_TEST_BIN', version_output='codex-cli {}',
        tarball='https://registry.npmjs.org/@openai/codex/-/codex-{version}-{target}.tgz',
        metadata='https://registry.npmjs.org/@openai/codex/{version}-{target}', published='{version}-{target}',
        # The Windows package also holds the sandbox's setup program and command
        # runner, the code-mode host, rg and voice libraries. The suites run Codex
        # without its sandbox and code mode, so none of them is extracted.
        members={'linux-x64': 'package/vendor/x86_64-unknown-linux-musl/bin/codex',
                 WINDOWS: 'package/vendor/x86_64-pc-windows-msvc/bin/codex.exe'},
        coverage='clear, draft preservation, literal at-mentions with file/mention completion, identity, resumed clear and pause before SessionStart, async question panels and their unviewed growth, '
                 'two terminal sizes, animation modes, manual rate-limit model choices and the session-start task instructions',
        timeout=180,
        suites=['test_clear_context_native.NativeCodexClear', 'test_codex_update.NativeCodexUpdate',
                'test_codex_rate_limit.NativeCodexRateLimit',
                'test_external_agents_native.NativeExternalAgents', 'test_codex_async_questions_native',
                'test_codex_input_native', 'test_busy_input_native.NativeBusyCodex',
                'test_task_context_native.NativeCodexTaskContext', 'test_terminate_native.NativeCodexTermination'],
        windows=dict(suites=['test_windows_native_agents.NativeWindowsCodex'], timeout=900, coverage=HOST_COVERAGE)),
    'claude': dict(
        name='Claude Code', variable='HGS_CLAUDE_TEST_BIN', version_output='{} (Claude Code)',
        tarball='https://registry.npmjs.org/@anthropic-ai/claude-code-{target}/-/claude-code-{target}-{version}.tgz',
        metadata='https://registry.npmjs.org/@anthropic-ai/claude-code-{target}/{version}', published='{version}',
        members={'linux-x64': 'package/claude', 'darwin-arm64': 'package/claude', WINDOWS: 'package/claude.exe'},
        coverage='input, dim prompt suggestions and draft preservation, questions, approvals, trust, resume, usage-limit and auto-mode choices, clear, compaction, session-only model settings and '
                 'status line context capacity and the session-start task instructions',
        timeout=900,
        suites=['test_claude_input_native', 'test_claude_suggestions_native', 'test_busy_input_native.NativeBusyClaude', 'test_claude_questions_native', 'test_claude_approval_native', 'test_claude_safety_native',
                'test_claude_trust_native', 'test_claude_resume_native', 'test_claude_usage_limit_native', 'test_clear_context_native.NativeClearContext',
                'test_compact_context_native', 'test_claude_settings_native', 'test_claude_statusline_native',
                'test_claude_permission_native', 'test_task_context_native.NativeClaudeTaskContext',
                'test_terminate_native.NativeClaudeTermination'],
        windows=dict(suites=['test_windows_native_agents.NativeWindowsClaude'], timeout=1200,
                     coverage=HOST_COVERAGE + '; exec-form answer channel for questions and approvals, Terminal answers, '
                     'owned hook exit and channel-absent unviewed-pane growth')),
    # OpenCode 2 is accepted at or above the recorded minimum; CI tests only the
    # newest release without changing that product boundary. Its x86_64 Linux package
    # without AVX2 requirements is named `baseline` and runs on every x86_64 host.
    # The suite owns its private homes, service port and keyless model.
    'opencode': dict(
        name='OpenCode', variable='ZERUS_TEST_OPENCODE_BIN', version_output='opencode v{}',
        tarball='https://registry.npmjs.org/@opencode/cli-{target}/-/cli-{target}-{version}.tgz',
        metadata='https://registry.npmjs.org/@opencode/cli-{target}/{version}', published='{version}',
        members={'linux-x64-baseline': 'package/bin/opencode', 'darwin-arm64': 'package/bin/opencode'},
        targets={'linux-x64': 'linux-x64-baseline'},
        environment={'ZERUS_TEST_OPENCODE_NATIVE': '1'},
        private_xdg=True,
        coverage='private service start and port, own session IDs, lost launch and message responses, queue, Stop, '
                 'history, question forms, permissions, model and effort, provider errors, OpenCode 1 bindings and '
                 'incompatible or unresponsive services',
        timeout=600,
        suites=['test_opencode_native']),
}
# Suite inputs taken from the caller; every other HGS_ variable is dropped.
FORWARDED = ('HGS_TEST_CONPTY_DIR',)
HINT_METHODS = ('test_lifecycle', 'test_lost_mark', 'test_pre_feature_first_resume', 'test_optouts_and_instructions')
HINT_CLASSES = {
    'claude': ('NativeClaudeHint', 'NativeWindowsClaudeHint'),
    'codex': ('NativeCodexHint', 'NativeWindowsCodexHint'),
}
PRE_FEATURE_SOURCE = 'ad3285c19013cf2ff78ec8eaf6ab2fbdd5253dce'


def hint_suites(agent, target):
    name = HINT_CLASSES[agent][target == WINDOWS]
    return ['test_agent_hint_native.' + name + '.' + method for method in HINT_METHODS]


def hint_input(path, source):
    """Byte seal only. Reviewed build receipts must establish the declared source.

    No version probe can establish a binary's Git source. The qualification
    grant must bind that source and hash to its separately reviewed build.
    """
    if not source or not re.fullmatch(r'[0-9a-f]{40}', source):
        raise ValueError('Hint source inputs need complete reviewed Git object IDs')
    path = Path(path).resolve()
    if not path.is_file() or path.stat().st_size > MAX_PACKAGE_BYTES:
        raise ValueError('Hint source input is absent or exceeds its byte bound')
    value = hashlib.sha256()
    with path.open('rb') as file:
        for block in iter(lambda: file.read(1024 * 1024), b''):
            value.update(block)
    return dict(path=str(path), source=source, sha256=value.hexdigest())


NATIVE_RUNNER = '''
import faulthandler, json, os, sys, threading, time, unittest
current = None

def diagnostic(event, **fields):
    # unittest's verbose prefix has no newline while a test is running.
    print(file=sys.stderr)
    print('ZERUS_NATIVE_DIAGNOSTIC=' + json.dumps(dict(event=event, **fields)), file=sys.stderr, flush=True)

class DiagnosticResult(unittest.TextTestResult):
    def startTest(self, test):
        global current
        super().startTest(test)
        current = dict(id=test.id(), started_monotonic=time.monotonic())
        diagnostic('start', **current)

    def stopTest(self, test):
        global current
        now = time.monotonic()
        diagnostic('end', id=test.id(), ended_monotonic=now,
                   elapsed=now - current['started_monotonic'])
        current = None
        super().stopTest(test)

def deadline_snapshot():
    now, case = time.monotonic(), current
    diagnostic('before-aggregate-timeout', monotonic=now, current=case,
               case_elapsed=now - case['started_monotonic'] if case else None)
    # CPython bounds each dump to 100 threads and 100 frames per thread. This
    # records Python wait sites only; it cannot prove a native child is stuck.
    faulthandler.dump_traceback(file=sys.stderr, all_threads=True)

timer = None
deadline = os.environ.get('ZERUS_NATIVE_DIAGNOSTIC_DEADLINE')
if deadline is not None:
    # Snapshot just before the parent's existing timeout kills this interpreter.
    # It neither extends that timeout nor changes any test outcome or cleanup.
    timer = threading.Timer(max(0, float(deadline) - time.monotonic() - 1), deadline_snapshot)
    timer.daemon = True
    timer.start()
try:
    suite = unittest.defaultTestLoader.loadTestsFromNames(sys.argv[1:])
    def ids(node):
        return [item for child in node for item in ids(child)] if isinstance(node, unittest.TestSuite) else [node.id()]
    expected = json.loads(os.environ.get('ZERUS_NATIVE_EXPECTED_IDS', 'null'))
    if expected is not None and (ids(suite) != expected or len(expected) != 4):
        diagnostic('discovery-mismatch', actual_ids=ids(suite), expected_ids=expected,
                   loader_errors=[error[:8192] for error in unittest.defaultTestLoader.errors[:4]])
        raise ValueError('Named hint contract discovery differs from its fixed four-method inventory')
    result = unittest.TextTestRunner(verbosity=2, resultclass=DiagnosticResult,
                                   failfast=os.environ.get('ZERUS_NATIVE_FAILFAST') == '1').run(suite)
finally:
    if timer is not None:
        timer.cancel()
print('ZERUS_NATIVE_RESULT=' + json.dumps({'tests': result.testsRun, 'skipped': len(result.skipped)}))
sys.exit(not result.wasSuccessful() or bool(result.skipped) or result.testsRun == 0)
'''


def host_target():
    system = {'Linux': 'linux', 'Darwin': 'darwin', 'Windows': 'win32'}.get(platform.system())
    machine = {'x86_64': 'x64', 'amd64': 'x64', 'arm64': 'arm64', 'aarch64': 'arm64'}.get(platform.machine().lower())
    return system + '-' + machine if system and machine else None


def agent_target(agent, target):
    """The package target of `agent` for a host target."""
    return AGENTS[agent].get('targets', {}).get(target, target)


def target_spec(agent, target):
    """The agent's specification for one target: on Windows its session host suites."""
    spec = AGENTS[agent]
    return dict(spec, **spec['windows']) if target == WINDOWS else spec


def check_manifest(manifest):
    """Each reviewed version is pinned once for every target its agent supports.

    Only an older version kept as a regression boundary, whose pins all name the
    reason as `retained`, may cover fewer targets; the newest version never does.
    """
    for agent, spec in AGENTS.items():
        pins = manifest[agent]
        versions = list(dict.fromkeys(package['version'] for package in pins))
        if not versions:
            raise ValueError(spec['name'] + ' has no reviewed version')
        for version in versions:
            label = spec['name'] + ' ' + version
            rows = [package for package in pins if package['version'] == version]
            targets = [package['target'] for package in rows]
            if len(set(targets)) != len(targets) or not set(targets) <= set(spec['members']):
                raise ValueError(label + ' pins an unsupported target or one target twice')
            reasons = {package.get('retained') for package in rows}
            if len(reasons) != 1:
                raise ValueError(label + ' is retained on some targets only')
            reason = reasons.pop()
            if reason is None:
                if set(targets) != set(spec['members']):
                    raise ValueError(label + ' needs a pin for every target: ' + ', '.join(sorted(spec['members'])))
            elif not isinstance(reason, str) or not reason.strip() or version == versions[-1]:
                raise ValueError(label + ': only an older version may be retained, with its reason')


def fetch_json(url):
    request = urllib.request.Request(url, headers={'User-Agent': 'Zerus-agent-compatibility'})
    with urllib.request.urlopen(request, timeout=30) as response:
        data = response.read(8 * 1024 * 1024 + 1)
    if len(data) > 8 * 1024 * 1024:
        raise ValueError('Upstream metadata exceeds its size limit')
    return json.loads(data)


def version_text(value):
    if not isinstance(value, str) or not re.fullmatch(r'[A-Za-z0-9._+-]{1,64}', value):
        raise ValueError('Invalid published version')
    return value


def release(version):
    """(major, minor, patch) of a plain release version, else None."""
    match = re.fullmatch(r'(\d+)\.(\d+)\.(\d+)', version)
    return tuple(int(part) for part in match.groups()) if match else None


def minimum_review(version, minimum):
    """How a published version relates to the reviewed minimum of an agent that is
    accepted at or above it within one major version."""
    found, floor = release(version), release(minimum)
    if not found or not floor:
        return 'needs review'
    if found[0] != floor[0]:
        return 'another major version than the reviewed minimum; needs review'
    if found < floor:
        return 'older than the reviewed minimum'
    if found == floor:
        return 'same as the reviewed minimum'
    return 'newer than the reviewed minimum; see its canary'


def discover_upstream(manifest):
    rows = []
    for source in manifest['upstream']:
        row = {key: source[key] for key in ('name', 'url', 'notes', 'reviewed_versions')}
        try:
            value = fetch_json(source['url'])
            for key in source['version_path']:
                value = value[key]
            row['version'] = version_text(value)
            if source.get('policy') == 'minimum':
                # Accepted at or above the reviewed minimum once its canary passes.
                row['review'] = minimum_review(value, source['reviewed_versions'][-1])
            elif value in source['reviewed_versions']:
                row['review'] = 'reviewed pin'
            else:
                row['review'] = 'needs review'
        except Exception as error:
            row['error'] = str(error)
        rows.append(row)
    return rows


def latest_packages(agents, target, upstream):
    """Resolve one exact official package per selected agent before any native run.

    The registry's latest tag is read once, not once per terminal case. Historical
    pins are never a fallback for a missing release, target package or integrity.
    """
    packages, errors = {}, []
    for agent in agents:
        spec = AGENTS[agent]
        try:
            rows = [row for row in upstream if row['name'] == spec['name']]
            if len(rows) != 1 or 'error' in rows[0]:
                raise ValueError('Latest release discovery is absent, ambiguous or failed')
            version = version_text(rows[0]['version'])
            package_target = agent_target(agent, target)
            metadata = fetch_json(spec['metadata'].format(version=version, target=package_target))
            if metadata['version'] != spec['published'].format(version=version, target=package_target):
                raise ValueError('Latest platform package version mismatch')
            url, integrity = metadata['dist']['tarball'], metadata['dist']['integrity']
            if url != spec['tarball'].format(version=version, target=package_target):
                raise ValueError('Latest platform package is not the official distribution')
            algorithm, encoded = integrity.split('-', 1)
            if algorithm != 'sha512' or len(base64.b64decode(encoded, validate=True)) != 64:
                raise ValueError('Latest platform package needs an exact SHA-512 integrity')
            packages[agent] = [dict(version=version, target=package_target, url=url, integrity=integrity)]
        except Exception as error:
            errors.append(spec['name'] + ' latest package: ' + str(error))
    return packages, errors


DOWNLOAD_ATTEMPTS = 3


def download(url, archive):
    """Fetch a bounded package into archive and return its SHA-512 digest.

    A dropped connection or a server error is retried; the caller still
    verifies every byte against the pin, so a retry cannot change the package.
    """
    for attempt in range(1, DOWNLOAD_ATTEMPTS + 1):
        digest = hashlib.sha512()
        size = 0
        try:
            with urllib.request.urlopen(url, timeout=60) as response, archive.open('wb') as output:
                while chunk := response.read(1024 * 1024):
                    size += len(chunk)
                    if size > MAX_PACKAGE_BYTES:
                        raise ValueError('Native package exceeds its size limit')
                    digest.update(chunk)
                    output.write(chunk)
            return digest.digest()
        except urllib.error.HTTPError as error:
            if error.code < 500 or attempt == DOWNLOAD_ATTEMPTS:
                raise
        except (OSError, http.client.HTTPException):
            if attempt == DOWNLOAD_ATTEMPTS:
                raise
        print('Retrying the package download after a dropped connection', flush=True)
        time.sleep(5 * attempt)


def install_native(agent, package, destination, archive_input=None, deadline=None):
    """Verify published bytes, then extract exactly one regular native executable."""
    spec = AGENTS[agent]
    url = package['url']
    if package['target'] not in spec['members'] or url != spec['tarball'].format(
            version=version_text(package['version']), target=package['target']):
        raise ValueError('Expected the official ' + spec['name'] + ' package distribution')
    name = spec['members'][package['target']]
    algorithm, encoded = package['integrity'].split('-', 1)
    if algorithm != 'sha512':
        raise ValueError('Expected a SHA-512 package pin')
    expected = base64.b64decode(encoded, validate=True)
    if len(expected) != 64:
        raise ValueError('Invalid package integrity digest')
    destination.mkdir(parents=True, exist_ok=True)
    archive = destination / 'native.tgz'
    try:
        if archive_input is None:
            actual = download(url, archive)
        else:
            # The hint contract consumes public pinned cache data only. No
            # download, retry or fallback is hidden in its preparation budget.
            actual, size = hashlib.sha512(), 0
            with Path(archive_input).open('rb') as source, archive.open('wb') as output:
                while block := source.read(1024 * 1024):
                    if time.monotonic() >= deadline:
                        raise TimeoutError('Original hint row work end exhausted copying package')
                    size += len(block)
                    if size > MAX_PACKAGE_BYTES:
                        raise ValueError('Native package exceeds its size limit')
                    actual.update(block)
                    output.write(block)
            actual = actual.digest()
        if actual != expected:
            raise ValueError('Native package integrity mismatch')
        with tarfile.open(archive, 'r:gz') as tar:
            member = tar.getmember(name)
            if not member.isfile() or not 0 < member.size <= MAX_PACKAGE_BYTES:
                raise ValueError('Expected one regular bounded native executable')
            native = destination / name.rsplit('/', 1)[-1]
            with tar.extractfile(member) as source, native.open('wb') as output:
                if deadline is None:
                    shutil.copyfileobj(source, output)
                else:
                    while block := source.read(1024 * 1024):
                        if time.monotonic() >= deadline:
                            raise TimeoutError('Original hint row work end exhausted extracting package')
                        output.write(block)
        native.chmod(0o755)
        return native
    finally:
        archive.unlink(missing_ok=True)


def fixture_environment(home):
    # Windows spells every inherited name in upper case, so the prefixes match there
    # too, and SYSTEMROOT, PATHEXT, TEMP and the like pass through unchanged.
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(('HGS_', 'OPENAI_', 'ANTHROPIC_', 'CODEX_', 'CLAUDE', 'DEEPSEEK_', 'KIMI_', 'OPENCODE_'))}
    env.update({key: os.environ[key] for key in FORWARDED if key in os.environ})
    home.mkdir(parents=True, exist_ok=True)
    (home / '.codex').mkdir(exist_ok=True)
    # UTF-8 mode: suites and their Python fakes log non-ASCII prompts, which a
    # Windows code page cannot encode.
    env.update(HOME=str(home), CODEX_HOME=str(home / '.codex'),
               CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1', DISABLE_AUTOUPDATER='1',
               PYTHONPATH=str(ROOT / 'tests'), PYTHONUTF8='1', TERM='xterm-256color')
    return env


def private_profile(env, home):
    """`env` with the Windows profile in `home` as well: Windows programs find it
    through these rather than HOME."""
    env = dict(env)
    for key, path in (('USERPROFILE', home), ('APPDATA', home / 'AppData' / 'Roaming'),
                      ('LOCALAPPDATA', home / 'AppData' / 'Local')):
        path.mkdir(parents=True, exist_ok=True)
        env[key] = str(path)
    return env


def private_xdg(env, home):
    """`env` with every XDG data folder in `home`, so a probe never writes the caller's."""
    env = dict(env)
    for key, path in (('XDG_CONFIG_HOME', '.config'), ('XDG_DATA_HOME', '.local/share'),
                      ('XDG_CACHE_HOME', '.cache'), ('XDG_STATE_HOME', '.local/state')):
        (home / path).mkdir(parents=True, exist_ok=True)
        env[key] = str(home / path)
    return env


def probe(native, arguments, env, timeout=60):
    """A native executable's output. The first start of a fresh copy may wait for a
    malware scan on Windows."""
    return subprocess.check_output([str(native), *arguments], env=env, stdin=subprocess.DEVNULL,
                                   encoding='utf-8', errors='replace', timeout=timeout)


def run_suites(suites, env, timeout, scratch, log, absolute_end=None):
    """Run unittest names in a fresh interpreter and keep its stdout, then stderr, in
    `log`; returns the exit status and stdout. Files rather than pipes: reading a pipe
    waits for every process that inherited it, a session host or agent a suite left
    behind included (on Windows even past the timeout), and a run that times out
    still leaves its log."""
    out, err = scratch / (log.stem + '.stdout'), scratch / (log.stem + '.stderr')
    try:
        with out.open('wb') as stdout, err.open('wb') as stderr:
            if absolute_end is None:
                deadline = time.monotonic() + timeout
            else:
                deadline = absolute_end
                timeout = min(timeout, deadline - time.monotonic())
                if timeout <= 0:
                    raise TimeoutError('Original hint row end exhausted before runner spawn')
            diagnostics_env = dict(env, ZERUS_NATIVE_DIAGNOSTIC_DEADLINE=str(deadline))
            run = subprocess.run([sys.executable, '-c', NATIVE_RUNNER, *suites], cwd=ROOT, env=diagnostics_env,
                                 stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr, timeout=timeout)
    finally:
        output = out.read_bytes() + err.read_bytes()
        log.write_bytes(output)
        print(output.decode('utf-8', 'replace').replace('\r\n', '\n'), flush=True)
    return run.returncode, out.read_bytes().decode('utf-8', 'replace')


def run_macos_claude_suites(suites, env, timeout, scratch, log):
    """Only this fully private matrix uses two processes under its original end."""
    spec = importlib.util.spec_from_file_location('native_suite_workers', ROOT / 'scripts/ci/native_suite_workers.py')
    workers = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(workers)
    return workers.run_suites(suites, env, timeout, scratch, log)


def method_names(value):
    methods = set()
    if isinstance(value, dict):
        method = value.get('properties', {}).get('method', {})
        if isinstance(method, dict):
            methods.update(item for item in method.get('enum', []) if isinstance(item, str))
            if isinstance(method.get('const'), str):
                methods.add(method['const'])
        for child in value.values():
            methods.update(method_names(child))
    elif isinstance(value, list):
        for child in value:
            methods.update(method_names(child))
    return methods


def schema_snapshot(directory):
    methods, files = set(), {}
    for path in sorted(directory.rglob('*.json')):
        data = json.loads(path.read_text(encoding='utf-8'))
        methods.update(method_names(data))
        normalized = json.dumps(data, sort_keys=True, separators=(',', ':')).encode()
        files[path.relative_to(directory).as_posix()] = hashlib.sha256(normalized).hexdigest()
    if not files or not methods:
        raise ValueError('Native protocol schema did not expose any methods')
    return {'methods': sorted(methods), 'files': files}


def schema_changes(before, after):
    first, second = before['files'], after['files']
    return {
        'added_methods': sorted(set(after['methods']) - set(before['methods'])),
        'removed_methods': sorted(set(before['methods']) - set(after['methods'])),
        'added_schemas': sorted(set(second) - set(first)),
        'removed_schemas': sorted(set(first) - set(second)),
        'changed_schemas': sorted(name for name in first.keys() & second.keys() if first[name] != second[name]),
    }


def qualify(agent, package, work, results, hgs, hint_inputs=None, package_cache=None):
    # Hint selection has one original 1200-second row end, preparation included.
    # Its fixtures reserve the last 60 seconds for their existing owned cleanup.
    hint_end = time.monotonic() + 1200 if hint_inputs is not None else None
    target = package['target']
    spec = target_spec(agent, target)
    version = version_text(package['version'])
    print('Checking real', spec['name'], version, flush=True)
    row = {'agent': agent, 'version': version, 'target': target, 'package_url': package['url'],
           'package_integrity': package['integrity'], 'status': 'failed'}
    if hint_inputs is not None:
        row.update(contract='agent-hint', source_inputs=hint_inputs, original_end_monotonic=hint_end,
                   coverage='four named hook-output/context lifecycle, lost-mark, real pre-feature migration and opt-out/instruction cases; '
                            'fixture retained-hook interposition, no customer-receipt or kernel-isolation claim')
    directory = results / agent / version
    directory.mkdir(parents=True, exist_ok=True)
    try:
        if hint_inputs is None:
            native = install_native(agent, package, work / agent / version)
        else:
            archive = package_cache / package['url'].rsplit('/', 1)[-1]
            native = install_native(agent, package, work / agent / version,
                                    archive_input=archive, deadline=hint_end - 60)
        home = work / agent / version / 'home'
        env = fixture_environment(home)
        # The runner's own probes get a private Windows profile too. The suites keep
        # the caller's: each case gives its hosts and agents a profile of its own, and
        # Codex prints folders relative to the real one, which a suite reads from
        # USERPROFILE to place its projects.
        probes = private_profile(env, home) if target == WINDOWS else env
        if spec.get('private_xdg'):
            probes = private_xdg(probes, home)
        def checked_probe(arguments):
            remaining = 60 if hint_end is None else min(60, hint_end - time.monotonic() - 60)
            if remaining <= 0:
                raise TimeoutError('Original hint row work end exhausted')
            return probe(native, arguments, probes, timeout=remaining)
        actual = checked_probe(['--version']).strip()
        if actual != spec['version_output'].format(version):
            raise ValueError('Native executable version mismatch: ' + actual)
        help_text = checked_probe(['--help'])
        (directory / 'cli-help.txt').write_text(help_text, encoding='utf-8')
        row['cli_help_sha256'] = hashlib.sha256(help_text.encode()).hexdigest()
        cases = {'default': {}}
        if agent == 'codex':
            for label, flags in [('stable', []), ('experimental', ['--experimental'])]:
                schemas = directory / ('schemas-' + label)
                checked_probe(['app-server', 'generate-json-schema', '--out', str(schemas), *flags])
                row['schema_' + label] = schema_snapshot(schemas)
            # Only the tmux suites take a terminal size and animation mode.
            if target != WINDOWS:
                cases = {geometry + '-' + mode: dict(HGS_CODEX_TEST_SIZE=geometry, HGS_CODEX_TEST_ANIMATIONS=animations)
                         for geometry in ('95x47', '110x35')
                         for mode, animations in (('animated', 'true'), ('reduced', 'false'))}
        row['cases'] = {}
        if hint_inputs is not None:
            # Context delivery is a separate named contract, not four extra
            # claims about geometry/animation. The default UI matrix is intact.
            cases = {'agent-hint': {}}
        for case, settings in cases.items():
            test_env = dict(env, HGS_TEST_BIN=str(hgs), **{spec['variable']: str(native)},
                            **spec.get('environment', {}), **settings)
            suites, timeout = spec['suites'], spec['timeout']
            if hint_inputs is not None:
                suites = hint_suites(agent, target)
                timeout = hint_end - time.monotonic()
                if timeout <= 60:
                    raise TimeoutError('Original hint row leaves no work and cleanup budget')
                test_env.update(ZERUS_HINT_INPUTS=json.dumps(hint_inputs),
                                ZERUS_HINT_RESULTS=str(directory / 'hook-output'),
                                ZERUS_NATIVE_EXPECTED_IDS=json.dumps(suites), ZERUS_NATIVE_FAILFAST='1')
            runner = run_macos_claude_suites if hint_inputs is None and (agent, target) == ('claude', 'darwin-arm64') else run_suites
            arguments = (suites, test_env, timeout, work / agent / version, directory / (case + '.log'))
            if hint_inputs is None:
                status, stdout = runner(*arguments)
            else:
                status, stdout = runner(*arguments, absolute_end=hint_end)
            marker = next((line.removeprefix('ZERUS_NATIVE_RESULT=') for line in stdout.splitlines()
                           if line.startswith('ZERUS_NATIVE_RESULT=')), None)
            if marker is not None:
                row['cases'][case] = json.loads(marker)
                if hint_inputs is not None and row['cases'][case] != {'tests': 4, 'skipped': 0}:
                    raise ValueError('Hint contract did not complete its exact four methods without skips')
            if status or marker is None:
                raise ValueError('Native contract failed at ' + case)
            if hint_inputs is not None:
                cleanup = [json.loads((directory / 'hook-output' / name / 'cleanup.json').read_text()) for name in suites]
                if any(record.get('verified') is not True for record in cleanup):
                    raise ValueError('Hint owned cleanup is UNKNOWN; runtime namespaces retained')
                row['owned_cleanup_verified'] = True
        if hint_inputs is not None and time.monotonic() >= hint_end:
            raise TimeoutError('Original hint row expired during result collection')
        row['status'] = 'passed'
    except Exception as error:
        row['error'] = str(error)
        print(spec['name'], version, 'failed:', error, flush=True)
    return row


def write_report(report, results):
    (results / 'report.json').write_text(json.dumps(report, indent=2, sort_keys=True) + '\n')
    lines = ['# Native agent compatibility', '', 'Synthetic fixtures only; no model credentials or live sessions.', '']
    for row in report.get('upstream', []):
        if 'error' in row:
            lines.append('- ' + row['name'] + ': discovery failed (see report.json).')
        else:
            lines.append(f"- {row['name']}: `{row['version']}` ({row['review']}). [Official changes]({row['notes']})")
    for agent, spec in AGENTS.items():
        rows = [row for row in report['contracts'] if row['agent'] == agent]
        if rows:
            lines.extend(['', '## Real ' + spec['name'] + ' contracts', ''])
            lines.extend(f"- `{row['version']}` ({row['target']}): {row['status']}; "
                         f"{row.get('coverage', target_spec(agent, row['target'])['coverage'])}." for row in rows)
    for label, changes in report.get('capability_changes', {}).items():
        lines.extend(['', '## ' + label + ' protocol changes', '', 'Experimental methods remain separate from supported APIs.'])
        for kind in ('added_methods', 'removed_methods', 'added_schemas', 'removed_schemas', 'changed_schemas'):
            values = changes[kind]
            lines.append(f'- {kind.replace("_", " ")}: {len(values)}')
            if kind.endswith('methods'):
                lines.extend('  - `' + value.replace('`', '') + '`' for value in values)
    for agent, changed in report.get('cli_help_changed', {}).items():
        lines.extend(['', AGENTS[agent]['name'] + ' CLI help changed: ' +
                      (f'yes; review {agent}-cli-help.diff' if changed else 'no') + '.'])
    lines.extend(['', 'Review release notes and captured capabilities before implementing new features.', ''])
    summary = '\n'.join(lines)
    (results / 'summary.md').write_text(summary, encoding='utf-8')
    if path := os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(path, 'a', encoding='utf-8') as file:
            file.write(summary)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--latest', action='store_true',
                        help='Also discover other providers; native execution still uses one latest release per agent')
    parser.add_argument('--agent', choices=sorted(AGENTS), action='append',
                        help='Qualify only this agent (repeatable); default: every agent supported on this host')
    parser.add_argument('--results', type=Path, default=ROOT / 'artifacts/test-results/agent-compatibility')
    debug = ROOT / 'target/debug' / ('hgs.exe' if os.name == 'nt' else 'hgs')
    parser.add_argument('--hgs', type=Path, default=os.environ.get('HGS_TEST_BIN') or debug,
                        help='The hgs build under test; default: HGS_TEST_BIN, else the debug build')
    parser.add_argument('--contract', choices=['agent-hint'], help='Select the fixed hint-output contract instead of default UI suites')
    parser.add_argument('--pre-feature-hgs', type=Path, help='Reviewed real ad328 main CLI for the migration case')
    parser.add_argument('--hook-observer', type=Path, help='Reviewed build of the fixture-only session_start_observer example')
    parser.add_argument('--hgs-source', help='Complete source object bound to --hgs by the reviewed build receipt')
    parser.add_argument('--observer-source', help='Complete source object bound to --hook-observer by its reviewed build receipt')
    parser.add_argument('--native-package-cache', type=Path,
                        help='Public official tarballs named by the pin URLs; agent-hint never downloads missing inputs')
    args = parser.parse_args()
    hint_inputs = None
    if args.contract:
        if args.latest or not args.pre_feature_hgs or not args.hook_observer or not args.native_package_cache:
            parser.error('agent-hint requires sealed CLI/observer inputs and --native-package-cache; --latest is separate')
        try:
            hint_inputs = dict(candidate=hint_input(args.hgs, args.hgs_source),
                               pre_feature=hint_input(args.pre_feature_hgs, PRE_FEATURE_SOURCE),
                               observer=hint_input(args.hook_observer, args.observer_source))
        except (OSError, ValueError) as error:
            parser.error(str(error))
    elif any((args.pre_feature_hgs, args.hook_observer, args.hgs_source, args.observer_source, args.native_package_cache)):
        parser.error('Hint source inputs require --contract agent-hint')
    manifest = json.loads(MANIFEST.read_text(encoding='utf-8'))
    try:
        check_manifest(manifest)
    except ValueError as error:
        parser.error(str(error))
    results = args.results.resolve()
    results.mkdir(parents=True, exist_ok=True)
    target = host_target()
    agents = list(dict.fromkeys(args.agent)) if args.agent else [
        agent for agent, spec in AGENTS.items() if agent_target(agent, target) in spec['members']]
    if hint_inputs is not None:
        if args.agent and any(agent not in HINT_CLASSES for agent in agents):
            parser.error('The agent-hint contract covers Claude and Codex only')
        agents = [agent for agent in agents if agent in HINT_CLASSES]
    unsupported = [agent for agent in agents if agent_target(agent, target) not in AGENTS[agent]['members']]
    if not agents or unsupported:
        parser.error('No supported native packages for ' + str(target) + ': ' + ', '.join(unsupported or AGENTS))
    report = {'checked_at': datetime.datetime.now(datetime.timezone.utc).isoformat(), 'target': target,
              'contracts': [], 'errors': []}
    if hint_inputs is not None:
        # This separately selected historical fixture consumes its frozen grants
        # and package cache. It is not an ordinary CI compatibility matrix.
        report['selection_policy'] = 'historical-agent-hint'
        packages = {agent: [package for package in manifest[agent]
                            if package['target'] == agent_target(agent, target)] for agent in agents}
    else:
        report['selection_policy'] = 'latest-only'
        names = {AGENTS[agent]['name'] for agent in agents}
        sources = manifest['upstream'] if args.latest else [source for source in manifest['upstream']
                                                           if source['name'] in names]
        report['upstream'] = discover_upstream({'upstream': sources})
        report['errors'].extend(row['name'] + ': ' + row['error'] for row in report['upstream'] if 'error' in row)
        packages, errors = latest_packages(agents, target, report['upstream'])
        report['errors'].extend(errors)
        report['selected_packages'] = [dict(agent=agent, **package) for agent in agents
                                       for package in packages.get(agent, [])]
        # Preserve the exact selection even if a later native invocation times out.
        write_report(report, results)
        if errors:
            return 1
    if args.latest:
        report['canary_versions'] = {}
        for agent in agents:
            report['canary_versions'][agent] = packages[agent][0]['version']
    # A scanner or a leftover process may still hold a file there on Windows.
    work_context = (tempfile.TemporaryDirectory(prefix='zerus-native-compat-', ignore_cleanup_errors=True)
                    if hint_inputs is None else contextlib.nullcontext(tempfile.mkdtemp(prefix='zerus-hint-compat-')))
    with work_context as temporary:
        for agent in agents:
            for package in packages[agent]:
                if hint_inputs is None:
                    row = qualify(agent, package, Path(temporary), results, args.hgs.resolve())
                else:
                    row = qualify(agent, package, Path(temporary), results, args.hgs.resolve(), hint_inputs,
                                  args.native_package_cache.resolve())
                    row['work_root'] = temporary
                report['contracts'].append(row)
                if hint_inputs is not None and row['status'] != 'passed':
                    write_report(report, results)
                    return 1  # First failed pin stops; never replay or run later pins.
        if hint_inputs is not None:
            # No finalizer removes failed or unknown hint work. All case cleanup
            # records were checked before removing this successful cache root.
            shutil.rmtree(temporary)
    write_report(report, results)
    return bool(report['errors'] or any(row['status'] != 'passed' for row in report['contracts']))


if __name__ == '__main__':
    # Native logs may hold text that a Windows code page cannot encode.
    sys.stdout.reconfigure(errors='backslashreplace')
    sys.exit(main())
