#!/usr/bin/env python3
"""Finite keyless approvals gate for disposable macOS/Windows hosted runners.

No install, auto-update, replay or provider account. Downloads are preparatory;
execution reaches only each reviewed fixture's loopback model/platform. This is
not a kernel egress enclosure. An outer deadline retains roots and reports
detached cleanup UNKNOWN; terminating the unreaped suite child proves no more.
"""
import argparse
import ast
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import stat
import subprocess
import sys
import tempfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('kimi_retained_gate', Path(__file__).with_name('kimi-native.py'))
retained = importlib.util.module_from_spec(spec)
spec.loader.exec_module(retained)
NAMES = ('test_a_danger_warning_is_part_of_the_reviewed_request',
         'test_a_viewed_clipped_panel_is_never_resized_or_answered',
         'test_an_unviewed_clipped_panel_grows_until_answerable',
         'test_approve_for_this_session_records_its_scope',
         'test_approve_once_runs_the_command',
         'test_reject_never_runs_the_command',
         'test_reject_with_feedback_reaches_the_model')
GOAL_NAMES = ('test_goal_fullscreen_manual_starts_in_manual',
              'test_goal_fullscreen_yolo_auto_switches_to_never_ask',
              'test_goal_fullscreen_yolo_cancel_does_not_start',
              'test_goal_fullscreen_yolo_keeps_ask_when_needed',
              'test_goal_manual_starts_in_manual',
              'test_goal_yolo_auto_switches_to_never_ask',
              'test_goal_yolo_cancel_does_not_start',
              'test_goal_yolo_keeps_ask_when_needed')
PLAN_NAMES = ('test_plan_review_approve',)
LOG_LIMIT, SECONDS = 64 * 1024**2, 900
digest, strict, write = retained.digest, retained.strict, retained.write


def target():
    found = {('Darwin', 'arm64'): 'darwin-arm64', ('Windows', 'amd64'): 'win32-x64'}.get(
        (platform.system(), platform.machine().lower()))
    if found is None:
        raise ValueError('only reviewed macOS arm64 and Windows x64 hosted gates exist')
    return found


def methods(platform_id):
    source = ast.parse((ROOT / 'tests/test_kimi_approvals_native.py').read_text(encoding='utf-8'))
    common = next(node for node in source.body if isinstance(node, ast.ClassDef) and node.name == 'ApprovalCases')
    names = tuple(sorted(node.name for node in common.body if isinstance(node, ast.FunctionDef)
                         and node.name.startswith('test_')))
    if names != NAMES:
        raise ValueError('the exact seven semantic methods require a new review')
    cls = ('test_kimi_approvals_windows.NativeWindowsKimiApprovals' if platform_id == 'win32-x64'
           else 'test_kimi_approvals_native.NativeKimiApprovals')
    return tuple(cls + '.' + name for name in NAMES)


def goal_methods(platform_id):
    source = ast.parse((ROOT / 'tests/test_kimi_approvals_native.py').read_text(encoding='utf-8'))
    common = next(node for node in source.body if isinstance(node, ast.ClassDef) and node.name == 'GoalCases')
    names = tuple(sorted(node.name for node in common.body if isinstance(node, ast.FunctionDef)
                         and node.name.startswith('test_')))
    if names != GOAL_NAMES:
        raise ValueError('the exact eight goal methods require a new review')
    cls = ('test_kimi_approvals_windows.NativeWindowsKimiApprovals' if platform_id == 'win32-x64'
           else 'test_kimi_approvals_native.NativeKimiApprovals')
    return tuple(cls + '.' + name for name in GOAL_NAMES)


def plan_methods(platform_id):
    source = ast.parse((ROOT / 'tests/test_kimi_approvals_native.py').read_text(encoding='utf-8'))
    common = next(node for node in source.body if isinstance(node, ast.ClassDef) and node.name == 'PlanCases')
    names = tuple(sorted(node.name for node in common.body if isinstance(node, ast.FunctionDef)
                         and node.name.startswith('test_')))
    if names != PLAN_NAMES:
        raise ValueError('the exact one plan method requires a new review')
    cls = ('test_kimi_approvals_windows.NativeWindowsKimiApprovals' if platform_id == 'win32-x64'
           else 'test_kimi_approvals_native.NativeKimiApprovals')
    return tuple(cls + '.' + name for name in PLAN_NAMES)


def selected_methods(platform_id):
    return methods(platform_id) + goal_methods(platform_id) + plan_methods(platform_id)


def fetch(url, output, size, sha256):
    """One official GET; no retries, exclusive output, finite bytes and time."""
    redirects = retained.OfficialRedirect()
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), redirects)
    end, count, checksum = time.monotonic() + 180, 0, hashlib.sha256()
    with opener.open(urllib.request.Request(url, headers={'Accept-Encoding': 'identity'}, method='GET'),
                     timeout=30) as response:
        if (response.status != 200 or response.headers.get_all('Content-Length') != [str(size)]
                or response.headers.get('Content-Encoding', 'identity') != 'identity'):
            raise ValueError('download metadata differs from the exact pin')
        fd = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, 'O_NOFOLLOW', 0), 0o600)
        with os.fdopen(fd, 'wb') as stream:
            while count < size:
                if time.monotonic() >= end:
                    raise TimeoutError('download deadline')
                data = response.read(min(65536, size - count))
                if time.monotonic() >= end or not data:
                    raise ValueError('late or short image download')
                stream.write(data)
                checksum.update(data)
                count += len(data)
            if response.read(1):
                raise ValueError('download exceeded the exact pin')
            if time.monotonic() >= end:
                raise TimeoutError('download completed late')
            stream.flush()
            os.fsync(stream.fileno())
        if count != size or checksum.hexdigest() != sha256:
            raise ValueError('download size/digest changed')
    Path(output).chmod(0o400)
    return dict(url=url, redirects=redirects.hops, size=count, sha256=sha256)


def child(result_file, custody_file, oracle=False):
    import unittest
    sys.path.insert(0, str(ROOT / 'tests'))
    platform_id = target()
    if platform_id == 'win32-x64':
        import test_kimi_approvals_windows as windows
        cls = windows.NativeWindowsKimiApprovals
    else:
        import test_kimi_approvals_native as native
        cls = native.NativeKimiApprovals
    selected = (('test_kimi_approvals_windows.WindowsEnvironmentOracle.'
                 'test_a_known_private_child_environment_matches_the_bounded_reader',) if oracle
                else selected_methods(platform_id))
    original = cls.__setattr__

    def assigned(case, name, value):
        original(case, name, value)
        if name in ('root', 'images') and value:
            path = Path(value).resolve(strict=True)
            info = path.lstat()
            with custody_file.open('a', encoding='utf-8') as stream:
                stream.write(json.dumps(dict(case=case.id(), path=str(path), device=info.st_dev,
                                             inode=info.st_ino, uid=info.st_uid, kind='directory')) + '\n')
                stream.flush()
                os.fsync(stream.fileno())
    cls.__setattr__ = assigned
    started, completed = [], []

    class Result(unittest.TextTestResult):
        def startTest(self, case):
            started.append(case.id())
            super().startTest(case)

        def stopTest(self, case):
            completed.append(case.id())
            super().stopTest(case)
    suite = unittest.defaultTestLoader.loadTestsFromNames(selected)
    result = unittest.TextTestRunner(verbosity=2, failfast=True, resultclass=Result).run(suite)
    value = dict(tests=result.testsRun, started=started, completed=completed,
                 failures=len(result.failures), errors=len(result.errors), skipped=len(result.skipped),
                 expected_failures=len(result.expectedFailures), unexpected_successes=len(result.unexpectedSuccesses))
    write(result_file, value)
    return 0 if qualifies(value, selected) else 1


def qualifies(result, selected):
    return (type(result.get('tests')) is int and result['tests'] == len(selected)
            and result.get('started') == list(selected) and result.get('completed') == list(selected)
            and all(type(result.get(key)) is int and result[key] == 0 for key in
                    ('failures', 'errors', 'skipped', 'expected_failures', 'unexpected_successes')))


def invoke(out, name, env, selected, oracle=False):
    result, custody = out / (name + '-result.json'), out / (name + '-custody.jsonl')
    argv = [sys.executable, '-s', '-B', str(Path(__file__).resolve()), '--suite-child', str(result), str(custody)]
    if oracle:
        argv.append('--oracle')
    suite_argv = argv
    launch_env = env
    outside_log = None
    if target() == 'win32-x64' and not oracle:
        # Only this fixture's private host may leave the runner's enclosing
        # job. The reviewed wrapper never uses PID/tree/image cleanup here.
        outside_log = out / (name + '-outside-job.log')
        argv = [sys.executable, '-s', '-B', str(ROOT / 'scripts/ci/outside-job.py'),
                '--custody-only', '--timeout', str(SECONDS), '--log', str(outside_log), '--', *suite_argv]
        system = Path(env['SystemRoot'])
        launch_env = dict(env, PATH=os.pathsep.join((str(system / 'System32'),
                         str(system / 'System32/WindowsPowerShell/v1.0'))))
    write(out / (name + '-command.json'), dict(argv=argv, suite_argv=suite_argv, environment=launch_env,
          methods=selected, seconds=SECONDS, outside_job_log=str(outside_log) if outside_log else None,
          combined_log_limit=LOG_LIMIT,
          timeout_outcome='UNKNOWN; retain wrapper/fixture, never replay'))
    log = out / (name + '.log')
    with log.open('xb') as stream:
        process = subprocess.Popen(argv, env=launch_env, cwd=ROOT, stdin=subprocess.DEVNULL, stdout=stream,
                                   stderr=stream, creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
        code = retained.await_child(process, log, seconds=SECONDS)
    if outside_log and (not outside_log.is_file() or outside_log.stat().st_size + log.stat().st_size > LOG_LIMIT):
        raise ValueError('outside-job native log is missing or combined logs exceed the original cap')
    value = strict(result.read_bytes())
    if code != 0 or not qualifies(value, selected):
        raise ValueError('the exact native methods failed or skipped; retained roots are not retried')
    return value


def retain_declared_roots(evidence, runtime, archive):
    """Reuse the reviewed Unix dirfd/no-follow diagnostic snapshot.

    Windows unknown roots stay in place. No race-safe Windows archive reader
    is claimed: logs retain their exact declared paths/IDs, and the disposable
    VM is the outer boundary. An interrupted Windows run cannot qualify.
    """
    if os.name == 'nt':
        raise ValueError('Windows roots retained in place; no safe unknown-writer archive is qualified')
    return retained.retain(list(evidence.glob('*-custody.jsonl')), runtime, archive)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-sha', required=True)
    parser.add_argument('--hgs', type=Path, required=True)
    parser.add_argument('--tmux', type=Path)
    parser.add_argument('--conpty', type=Path)
    parser.add_argument('--git-bash', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if os.environ.get('GITHUB_ACTIONS') != 'true' or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted':
        raise ValueError('only an explicitly reviewed disposable standard hosted runner may invoke this gate')
    temporary = Path(os.environ['RUNNER_TEMP']).resolve(strict=True)
    if Path(os.environ['GITHUB_WORKSPACE']).resolve(strict=True) != ROOT:
        raise ValueError('this checkout is not the declared workspace')
    if not args.hgs.resolve(strict=True).is_relative_to(temporary):
        raise ValueError('the hgs build must be private to this runner')
    if not args.output.resolve().is_relative_to(ROOT / 'artifacts/test-results'):
        raise ValueError('the result root is outside the fixed artifact directory')
    if subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip() != args.source_sha:
        raise ValueError('the exact source changed')
    platform_id = target()
    selected = selected_methods(platform_id)
    os.umask(0o077)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.mkdir(mode=0o700)
    out = args.output.resolve()
    images, evidence = out / 'images', out / 'evidence'
    images.mkdir(mode=0o700)
    evidence.mkdir(mode=0o700)
    runtime = Path(tempfile.mkdtemp(prefix='ka-run-', dir=temporary if os.name == 'nt' else '/tmp')).resolve()
    source = retained.source_map()
    hgs = args.hgs.resolve(strict=True)
    tools = {str(hgs): digest(hgs), str(Path(sys.executable).resolve()): digest(Path(sys.executable).resolve())}
    env = dict(PYTHONUTF8='1', PYTHONDONTWRITEBYTECODE='1', PYTHONUNBUFFERED='1',
               HGS_TEST_BIN=str(hgs), HGS_KIMI_EVIDENCE_DIR=str(evidence))
    for key, folder in (('HOME', 'home'), ('USERPROFILE', 'home'), ('TMPDIR', 'tmp'),
                        ('TEMP', 'tmp'), ('TMP', 'tmp'), ('XDG_CONFIG_HOME', 'config'),
                        ('XDG_DATA_HOME', 'data'), ('XDG_CACHE_HOME', 'cache'), ('XDG_STATE_HOME', 'state'),
                        ('XDG_RUNTIME_DIR', 'runtime')):
        path = runtime / folder
        path.mkdir(mode=0o700, exist_ok=True)
        env[key] = str(path)
    if platform_id == 'darwin-arm64':
        tmux = args.tmux.resolve(strict=True)
        if not tmux.is_relative_to(temporary):
            raise ValueError('tmux must be the private reviewed runner build')
        helpers = runtime / 'helpers'
        helpers.mkdir(mode=0o700)
        (helpers / 'python3').symlink_to(Path(sys.executable).resolve())
        (helpers / 'tmux').symlink_to(tmux)
        env.update(PATH=str(helpers) + ':/usr/bin:/bin', LANG='en_US.UTF-8')
        tools.update({str(tmux): digest(tmux), '/usr/bin/cc': digest('/usr/bin/cc')})
    else:
        system = Path(os.environ['SystemRoot'])
        conpty = args.conpty.resolve(strict=True)
        if not conpty.is_relative_to(temporary):
            raise ValueError('the reviewed ConPTY pair must be private to this runner')
        bash = args.git_bash.resolve(strict=True)
        if bash.name.lower() != 'bash.exe' or bash.parent.name.lower() != 'bin':
            raise ValueError('use the reviewed installed Git bin/bash.exe, not a lone copied shell')
        git_root = bash.parent.parent
        runtime_files = {bash, git_root / 'usr/bin/bash.exe', git_root / 'usr/bin/msys-2.0.dll'}
        for directory in (git_root / 'bin', git_root / 'usr/bin'):
            runtime_files.update(directory.glob('*.dll'))
        if len(runtime_files) > 512 or sum(path.stat().st_size for path in runtime_files) > 512 * 1024**2:
            raise ValueError('installed Git runtime inventory exceeded its bound')
        tools.update({str(path): digest(path) for path in sorted(runtime_files)})
        env.update(SystemRoot=str(system), WINDIR=str(system), RUNNER_TEMP=str(runtime),
                   HGS_TEST_CONPTY_DIR=str(conpty), HGS_KIMI_GIT_BASH=str(bash),
                   HGS_KIMI_GIT_BASH_SHA256=digest(bash), PATH=str(system / 'System32'))
        tools.update({str(bash): digest(bash)})
        powershell = system / 'System32/WindowsPowerShell/v1.0/powershell.exe'
        tools[str(powershell)] = digest(powershell)
        for name in ('conpty.dll', 'OpenConsole.exe'):
            tools[str(conpty / name)] = digest(conpty / name)
    write(evidence / 'plan.json', dict(source=args.source_sha, platform=platform_id, tools=tools,
          methods=selected, selection='official latest once; no fallback', native_cases=len(selected),
          mac_shim_compiles=len(selected) if os.name != 'nt' else 0,
          environment=env, runtime=str(runtime), no_retries=True))
    write(evidence / 'source-before.json', source)
    success = False
    try:
        if os.name == 'nt':
            oracle = ('test_kimi_approvals_windows.WindowsEnvironmentOracle.'
                      'test_a_known_private_child_environment_matches_the_bounded_reader',)
            invoke(evidence, 'windows-environment-oracle', env, oracle, oracle=True)
        release, image, record_path = retained.stage_latest(images, evidence, platform_id)
        version = release['version']
        selected_seals = {str(path): digest(path) for path in (image, record_path, images / 'manifest.json')}
        write(evidence / 'reservation.json', dict(state='SPENT', versions=[version], native_cases=len(selected),
              selected_release=release))
        if retained.source_map() != source or any(digest(path) != value for path, value in tools.items()):
            raise ValueError('source or tools changed before execution')
        case_evidence = evidence / ('native-' + version)
        case_evidence.mkdir(mode=0o700)
        native_env = dict(env, HGS_KIMI_TEST_BIN=str(image),
            HGS_KIMI_TEST_SHA256=release['image_sha256'], HGS_KIMI_TEST_RECORD=str(record_path),
            HGS_KIMI_GUARD=str(image), HGS_KIMI_EVIDENCE_DIR=str(case_evidence))
        invoke(evidence, version, native_env, selected)
        if retained.source_map() != source or any(digest(path) != value for path, value in tools.items()):
            raise ValueError('source/tool post-seal changed')
        if any(digest(path) != sha for path, sha in selected_seals.items()):
            raise ValueError('a sealed selected release changed')
        success = True
    except BaseException as error:
        write(evidence / 'failure.json', dict(type=type(error).__name__, message=str(error),
              cleanup='UNKNOWN on interrupted/failed child; keep all declared roots, no retry'))
        try:
            write(evidence / 'runtime-snapshot.json',
                  retain_declared_roots(evidence, runtime, out / 'unknown-runtime.tar.gz'))
        except BaseException as snapshot_error:
            write(evidence / 'runtime-snapshot-error.json', dict(type=type(snapshot_error).__name__,
                  message=str(snapshot_error), consistency='UNKNOWN; no successful retention seal'))
        raise
    finally:
        write(evidence / 'receipt.json', dict(source=args.source_sha, outcome='PASS' if success else 'FAIL',
              native_cases=len(selected) if success else None, platform=platform_id, runtime=str(runtime),
              images_retained=str(images), owner_install=False))
    return 0


if __name__ == '__main__':
    if len(sys.argv) in (4, 5) and sys.argv[1] == '--suite-child':
        raise SystemExit(child(Path(sys.argv[2]), Path(sys.argv[3]), oracle=len(sys.argv) == 5))
    raise SystemExit(main())
