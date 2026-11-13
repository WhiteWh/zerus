#!/usr/bin/env python3
"""Bounded private native unittest workers, sharing one original wall end.

This schedules existing suite subtrees; it never retries a case or changes a
fixture. A reaped Python worker is not proof that a native daemon has exited.
Failed or canceled runs retain their private roots and report cleanup unknown.
"""
import collections
import faulthandler
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
import traceback
import unittest

CLEANUP_SECONDS = 10
KILL_RESERVE_SECONDS = 1
MAX_REPORT_BYTES = 1024 * 1024
MAX_BINARY_BYTES = 512 * 1024 * 1024
# Linux's debug CLI exceeds the provider executable bound. Keep provider images
# at 512 MiB; only the same-byte CLI staging copy gets this separate ceiling.
MAX_HGS_BYTES = 1024 * 1024 * 1024


def case_ids(suite):
    if isinstance(suite, unittest.TestSuite):
        return [name for child in suite for name in case_ids(child)]
    return [suite.id()]


def partition(inventory, worker_count=2):
    """Keep each original named suite intact and every occurrence, including duplicates."""
    if type(worker_count) is not int or worker_count not in (2, 3) or not worker_count <= len(inventory) <= 128 or any(
            not rows or any(not isinstance(name, str) or not name or len(name) > 4096 for name in rows)
            for rows in inventory):
        raise ValueError('Expected at least two nonempty native suite inventories')
    if sum(map(len, inventory)) > 10000:
        raise ValueError('Native suite inventory is too large')
    assignments, loads = [[] for _ in range(worker_count)], [0] * worker_count
    for index in sorted(range(len(inventory)), key=lambda value: (-len(inventory[value]), value)):
        worker = min(range(worker_count), key=lambda value: (loads[value], value))
        assignments[worker].append(index)
        loads[worker] += len(inventory[index])
    return [sorted(indexes) for indexes in assignments]


def write_json(path, value):
    encoded = (json.dumps(value, sort_keys=True) + '\n').encode()
    if len(encoded) > MAX_REPORT_BYTES:
        raise ValueError('Native worker report exceeds its bound')
    temporary = path.with_suffix(path.suffix + '.pending')
    with temporary.open('xb') as output:
        output.write(encoded)
    temporary.replace(path)


def read_json(path):
    with path.open('rb') as source:
        encoded = source.read(MAX_REPORT_BYTES + 1)
    if len(encoded) > MAX_REPORT_BYTES:
        raise ValueError('Native worker report exceeds its bound')
    return json.loads(encoded)


def stage_binary(source, destination, end, *, max_bytes=MAX_BINARY_BYTES):
    """A same-byte, read-only shared copy; no chmod of the caller's build/image."""
    digest, size = hashlib.sha256(), 0
    with Path(source).open('rb') as input_file, destination.open('xb') as output:
        while chunk := input_file.read(1024 * 1024):
            if time.monotonic() >= end:
                raise TimeoutError('Native aggregate deadline expired during staging')
            size += len(chunk)
            if size > max_bytes:
                raise ValueError('Native worker binary exceeds its bound')
            digest.update(chunk)
            output.write(chunk)
    destination.chmod(0o500)
    copied = hashlib.sha256()
    with destination.open('rb') as input_file:
        while chunk := input_file.read(1024 * 1024):
            if time.monotonic() >= end:
                raise TimeoutError('Native aggregate deadline expired during staging verification')
            copied.update(chunk)
    if copied.digest() != digest.digest() or time.monotonic() >= end:
        raise ValueError('Native worker binary staging did not verify')
    return dict(source=str(Path(source).resolve()), staged=str(destination),
                size=size, sha256=copied.hexdigest(), mode='0500')


def worker_environment(env, root, temporary=None):
    """Before interpreter startup: short, disjoint roots, no shared bytecode writes.

    Do not set CLAUDE_CONFIG_DIR: the existing cases set their own HOME and expect
    their own ~/.claude hooks/settings. Their native env -i remains unchanged.
    """
    private = dict(env)
    for key in ('TMUX', 'TMUX_PANE', 'CLAUDE_CONFIG_DIR'):
        private.pop(key, None)
    for key, name in (('HOME', 'home'), ('TMPDIR', 'tmp'), ('HGS_STATE_DIR', 'state'),
                      ('HGS_CONFIG_DIR', 'config'), ('XDG_CONFIG_HOME', 'xdg-config'),
                      ('XDG_DATA_HOME', 'xdg-data'), ('XDG_CACHE_HOME', 'xdg-cache'),
                      ('XDG_STATE_HOME', 'xdg-state'), ('XDG_RUNTIME_DIR', 'runtime'), ('CODEX_HOME', 'codex')):
        path = temporary if key == 'TMPDIR' and temporary is not None else root / name
        if path != temporary:
            path.mkdir(mode=0o700)
        private[key] = str(path)
    private['PYTHONDONTWRITEBYTECODE'] = '1'
    # Longest selected fixture prefix is hgs-claude-settings-native- + eight
    # tempfile characters. The fixtures append /socket and require <=104 bytes.
    socket = Path(private['TMPDIR']) / ('hgs-claude-settings-native-' + 'x' * 8) / 'socket'
    if len(os.fsencode(socket)) > 104:
        raise ValueError('Native worker temporary root would exceed the tmux socket bound')
    return private


class WorkerCanceled(Exception):
    pass


def worker_main(root, inventory_path, result_path, end, names):
    """Discover without running fixtures, wait for the parent's checked assignment."""
    current, result, canceled = None, None, False
    started, completed = [], []

    def diagnostic(event, **fields):
        print(file=sys.stderr)
        print('ZERUS_NATIVE_DIAGNOSTIC=' + json.dumps(dict(event=event, **fields)), file=sys.stderr, flush=True)

    class DiagnosticResult(unittest.TextTestResult):
        def startTest(self, test):
            nonlocal current
            super().startTest(test)
            started.append(test.id())
            current = dict(id=test.id(), occurrence=len(started) - 1, started_monotonic=time.monotonic())
            diagnostic('start', **current)

        def stopTest(self, test):
            nonlocal current
            now = time.monotonic()
            diagnostic('end', id=test.id(), occurrence=len(completed), ended_monotonic=now,
                       elapsed=now - current['started_monotonic'])
            completed.append(test.id())
            current = None
            super().stopTest(test)

    def canceled_by_parent(signum, frame):
        nonlocal canceled
        canceled = True
        if result is not None:
            result.stop()
        # unittest records this as an ERROR and runs its registered fixture
        # cleanups. It is deliberately not KeyboardInterrupt, which bypasses them.
        raise WorkerCanceled('Native worker canceled; remaining coverage is incomplete')

    def deadline_snapshot():
        now, case = time.monotonic(), current
        diagnostic('before-aggregate-timeout', monotonic=now, current=case,
                   case_elapsed=now - case['started_monotonic'] if case else None)
        faulthandler.dump_traceback(file=sys.stderr, all_threads=True)

    signal.signal(signal.SIGTERM, canceled_by_parent)
    timer = threading.Timer(max(0, end - time.monotonic() - 1), deadline_snapshot)
    timer.daemon = True
    timer.start()
    assigned, error = [], None
    try:
        worker_count = int(os.environ.get('ZERUS_NATIVE_WORKER_COUNT', '2'))
        suite = unittest.defaultTestLoader.loadTestsFromNames(names)
        groups = list(suite)
        inventory = [case_ids(group) for group in groups]
        partition(inventory, worker_count)  # Refuse empty/import-error discovery before the barrier.
        if unittest.defaultTestLoader.errors:
            raise ValueError('Native suite discovery failed: ' + repr(unittest.defaultTestLoader.errors))
        write_json(inventory_path, dict(names=names, suites=inventory))
        line = sys.stdin.readline(MAX_REPORT_BYTES + 1)
        if len(line.encode()) > MAX_REPORT_BYTES or time.monotonic() >= end:
            raise ValueError('Native worker assignment is late or too large')
        plan = json.loads(line)
        index = plan['worker']
        if type(index) is not int or index not in range(worker_count) or plan['suites'] != partition(inventory, worker_count)[index]:
            raise ValueError('Native worker assignment disagrees with discovered occurrences')
        indexes = plan['suites']
        assigned = [name for group in indexes for name in inventory[group]]
        runner = unittest.TextTestRunner(verbosity=2, resultclass=DiagnosticResult)
        # Assign the result before the first setup so cancellation can stop the
        # next case while the current TestCase runs its existing owned cleanups.
        result = runner._makeResult()
        runner._makeResult = lambda: result
        runner.run(unittest.TestSuite([groups[group] for group in indexes]))
    except BaseException as exception:
        error = type(exception).__name__ + ': ' + str(exception)
        traceback.print_exc()
    finally:
        timer.cancel()
    temporary_entries = sorted(path.name for path in Path(os.environ['TMPDIR']).iterdir())
    report = dict(assigned=assigned, started=started, completed=completed, canceled=canceled,
                  tests=result.testsRun if result is not None else 0,
                  failures=len(result.failures) if result is not None else 0,
                  errors=len(result.errors) if result is not None else 0,
                  skipped=len(result.skipped) if result is not None else 0,
                  expected_failures=len(result.expectedFailures) if result is not None else 0,
                  unexpected_successes=len(result.unexpectedSuccesses) if result is not None else 0,
                  successful=result.wasSuccessful() if result is not None else False,
                  temporary_entries=temporary_entries, error=error)
    write_json(result_path, report)
    print('ZERUS_NATIVE_WORKER_RESULT=' + json.dumps(report), flush=True)
    return int(not healthy_result(report, assigned))


def healthy_result(report, expected):
    return (bool(expected) and report.get('assigned') == expected and report.get('started') == expected
            and report.get('completed') == expected and type(report.get('tests')) is int
            and report['tests'] == len(expected) and report.get('successful') is True
            and report.get('canceled') is False and report.get('error') is None
            and report.get('temporary_entries') == [] and all(type(report.get(key)) is int and report[key] == 0
                for key in ('failures', 'errors', 'skipped', 'expected_failures', 'unexpected_successes')))


def drain_workers(workers, end):
    """Only direct, still-unreaped children. No process-group/native/name cleanup."""
    errors = []
    for worker in workers:
        process = worker['process']
        try:
            if process.stdin is not None:
                process.stdin.close()
        except BaseException as error:
            errors.append('worker stdin close: ' + repr(error))
        try:
            if process.poll() is None:
                process.terminate()
        except BaseException as error:
            errors.append('worker terminate: ' + repr(error))
    drain_end = min(end, time.monotonic() + CLEANUP_SECONDS)
    while any(worker['process'].poll() is None for worker in workers):
        now = time.monotonic()
        if now >= drain_end - KILL_RESERVE_SECONDS:
            for worker in workers:
                process = worker['process']
                if process.poll() is None and not worker.get('killed'):
                    worker['killed'] = True
                    try:
                        process.kill()
                    except BaseException as error:
                        errors.append('worker kill: ' + repr(error))
        if now >= drain_end:
            break
        time.sleep(min(.02, drain_end - now))
    return all(worker['process'].poll() is not None for worker in workers), errors


def run_suites(suites, env, timeout, scratch, log, *, native_variable='HGS_CLAUDE_TEST_BIN', worker_count=2):
    """Two or three interpreters; setup, discovery, execution and drain share one end."""
    if native_variable not in ('HGS_CLAUDE_TEST_BIN', 'HGS_CODEX_TEST_BIN'):
        raise ValueError('Unsupported private native worker executable')
    if type(worker_count) is not int or worker_count not in (2, 3):
        raise ValueError('Expected two or three native workers')
    end = time.monotonic() + timeout
    work_end = end - CLEANUP_SECONDS
    root = Path(tempfile.mkdtemp(prefix='znw-', dir=env.get('ZERUS_TEST_SHORT_TMPDIR', '/tmp'))).resolve()
    workers, inventory, assignments, problems = [], None, None, []
    record = dict(root=str(root), deadline=end, work_deadline=work_end, timeout=timeout,
                  cleanup_reserve=CLEANUP_SECONDS, suites=suites, workers=[], binaries={}, temporary_roots=[],
                  worker_count=worker_count)
    previous_signal = signal.getsignal(signal.SIGTERM)
    canceled = False

    def cancel_parent(signum, frame):
        nonlocal canceled
        if not canceled:
            canceled = True
            raise WorkerCanceled('Native matrix parent canceled; never replay')

    signal.signal(signal.SIGTERM, cancel_parent)
    try:
        staged = root / 'staged'
        staged.mkdir(mode=0o700)
        shared_env = dict(env)
        native_name = 'codex' if native_variable == 'HGS_CODEX_TEST_BIN' else 'claude'
        for key, name in (('HGS_TEST_BIN', 'hgs'), (native_variable, native_name)):
            record['binaries'][key] = stage_binary(env[key], staged / name, work_end,
                max_bytes=MAX_HGS_BYTES if key == 'HGS_TEST_BIN' else MAX_BINARY_BYTES)
            shared_env[key] = str(staged / name)
        for index in range(worker_count):
            if time.monotonic() >= work_end:
                raise TimeoutError('Native aggregate deadline expired before worker launch')
            private = root / str(index)
            private.mkdir(mode=0o700)
            paths = {kind: log.with_name(log.stem + f'.worker-{index}.' + kind)
                     for kind in ('stdout', 'stderr', 'inventory.json', 'result.json')}
            temporary = None
            if native_variable == 'HGS_CODEX_TEST_BIN':
                # Codex renders absolute fixture paths in strict trust panels
                # and its bounded title footer. Keep each worker's fixture root
                # short, private and disjoint before importing tempfile.
                temporary = Path(tempfile.mkdtemp(prefix='', dir=env.get('ZERUS_TEST_SHORT_TMPDIR', '/tmp'))).resolve()
                record['temporary_roots'].append(str(temporary))
            worker_env = worker_environment(shared_env, private, temporary)
            worker_env['ZERUS_NATIVE_DIAGNOSTIC_DEADLINE'] = str(end)
            worker_env['ZERUS_NATIVE_WORKER_COUNT'] = str(worker_count)
            with paths['stdout'].open('xb') as out, paths['stderr'].open('xb') as err:
                process = subprocess.Popen([sys.executable, str(Path(__file__).resolve()), '--worker',
                    str(private), str(paths['inventory.json']), str(paths['result.json']), str(end), *suites],
                    cwd=scratch, env=worker_env, stdin=subprocess.PIPE, stdout=out, stderr=err,
                    start_new_session=True)
                # Custody precedes even closing the parent's output descriptors.
                worker = dict(process=process, paths=paths, index=index)
                workers.append(worker)
            record['workers'].append(dict(index=index, pid=process.pid, root=str(private),
                                          paths={key: str(value) for key, value in paths.items()}))
        while not all(worker['paths']['inventory.json'].exists() for worker in workers):
            if any(worker['process'].poll() is not None for worker in workers):
                raise ValueError('Native worker exited before checked suite discovery')
            if time.monotonic() >= work_end:
                raise TimeoutError('Native aggregate deadline expired during discovery')
            time.sleep(.02)
        inventories = [read_json(worker['paths']['inventory.json']) for worker in workers]
        if any(value != inventories[0] for value in inventories[1:]) or inventories[0]['names'] != suites:
            raise ValueError('Native workers disagree about original suite discovery')
        inventory = inventories[0]['suites']
        assignments = partition(inventory, worker_count)
        record.update(inventory=inventory, assignments=assignments)
        for worker in workers:
            if time.monotonic() >= work_end:
                raise TimeoutError('Native aggregate deadline expired before the start barrier')
            plan = dict(worker=worker['index'], suites=assignments[worker['index']])
            worker['process'].stdin.write((json.dumps(plan) + '\n').encode())
            worker['process'].stdin.close()
        while any(worker['process'].poll() is None for worker in workers):
            if any(worker['process'].poll() not in (None, 0) for worker in workers):
                raise ValueError('Native worker failed; peer coverage is not complete')
            if time.monotonic() >= work_end:
                raise TimeoutError('Native aggregate work end reached; cleanup uses the original end')
            time.sleep(.02)
        for worker in workers:
            report = read_json(worker['paths']['result.json'])
            markers = [line.removeprefix('ZERUS_NATIVE_WORKER_RESULT=')
                       for line in worker['paths']['stdout'].read_text().splitlines()
                       if line.startswith('ZERUS_NATIVE_WORKER_RESULT=')]
            expected = [name for index in assignments[worker['index']] for name in inventory[index]]
            if (worker['process'].returncode != 0 or not healthy_result(report, expected)
                    or len(markers) != 1 or json.loads(markers[0]) != report):
                raise ValueError('Native worker result or exact occurrence accounting did not qualify')
            record['workers'][worker['index']]['result'] = report
        # This is a multiset, never a set: the original imported Clear case runs
        # twice and must remain two occurrences in the validated total.
        original = [name for group in inventory for name in group]
        actual = [name for worker in record['workers'] for name in worker['result']['completed']]
        if collections.Counter(original) != collections.Counter(actual) or time.monotonic() >= end:
            raise ValueError('Native aggregate occurrence multiset or original deadline did not qualify')
    except BaseException as error:
        problems.append(type(error).__name__ + ': ' + str(error))
        try:
            reaped, errors = drain_workers(workers, end)
            problems.extend('Direct worker cleanup UNKNOWN: ' + error for error in errors)
            if not reaped:
                problems.append('Direct worker cleanup UNKNOWN at the original deadline')
        except BaseException as cleanup_error:
            problems.append('Direct worker cleanup UNKNOWN: ' + repr(cleanup_error))
    finally:
        try:
            record['problems'] = problems
            record['cleanup'] = ('existing fixture cleanups completed; no all-descendant custody claim' if not problems
                                 else 'UNKNOWN native cleanup; private roots retained; never replay')
            for worker, row in zip(workers, record['workers']):
                row['returncode'] = worker['process'].poll()
                row['killed'] = worker.get('killed', False)
                if worker['paths']['result.json'].exists():
                    try:
                        row['observed_result'] = read_json(worker['paths']['result.json'])
                    except (OSError, ValueError) as error:
                        problems.append('Partial worker result: ' + repr(error))
            output = b''.join(worker['paths'][kind].read_bytes() for worker in workers for kind in ('stdout', 'stderr'))
            log.write_bytes(output)
            print(output.decode('utf-8', 'replace').replace('\r\n', '\n'), flush=True)
            if time.monotonic() >= end:
                problems.append('Native aggregate original deadline expired during final collection')
                record['cleanup'] = 'UNKNOWN native cleanup; private roots retained; never replay'
            write_json(log.with_suffix('.workers.json'), record)
        finally:
            signal.signal(signal.SIGTERM, previous_signal)
    if time.monotonic() >= end and not problems:
        problems.append('Native aggregate original deadline expired during final recording')
        record['cleanup'] = 'UNKNOWN native cleanup; private roots retained; never replay'
        write_json(log.with_suffix('.workers.json'), record)
    result = dict(tests=sum(row['observed_result']['tests'] for row in record['workers']
                            if isinstance(row.get('observed_result'), dict)
                            and type(row['observed_result'].get('tests')) is int),
                  skipped=0, workers=worker_count, qualified=not problems)
    stdout = 'ZERUS_NATIVE_RESULT=' + json.dumps(result) + '\n'
    print(stdout, end='', flush=True)
    return int(bool(problems)), stdout


if __name__ == '__main__':
    if len(sys.argv) < 8 or sys.argv[1] != '--worker':
        raise SystemExit('Only the fixed native unittest worker entry point is supported')
    raise SystemExit(worker_main(Path(sys.argv[2]), Path(sys.argv[3]), Path(sys.argv[4]),
                                 float(sys.argv[5]), sys.argv[6:]))
