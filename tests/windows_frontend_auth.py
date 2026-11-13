"""Explicit Windows native suite, with exact CI-built CLI and frontend fixture.

No WebView or native agent is started. WindowsFrontend uses a private passive
foreground service. WindowsTaskMode additionally exercises its own disposable
hosted-runner logon task and private service configuration.
"""
from contextlib import closing, contextmanager
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import unittest
import uuid
import base64
import tempfile
import time
import threading
import test_desktop as kernel


def windows_process_metadata(pid, handle=None):
    """Read-only diagnostics. Reopened PIDs never authorize cleanup."""
    import ctypes as c
    from ctypes import wintypes as w
    dll = c.WinDLL('kernel32', use_last_error=True)
    dll.OpenProcess.argtypes = (w.DWORD, w.BOOL, w.DWORD)
    dll.OpenProcess.restype = w.HANDLE
    dll.CloseHandle.argtypes = (w.HANDLE,)
    dll.GetProcessTimes.argtypes = (w.HANDLE,) + (c.POINTER(w.FILETIME),) * 4
    dll.QueryFullProcessImageNameW.argtypes = (w.HANDLE, w.DWORD, w.LPWSTR, c.POINTER(w.DWORD))
    result = dict(pid=pid)
    opened = handle is None
    if opened:
        handle = dll.OpenProcess(0x1000, False, pid)  # QUERY_LIMITED_INFORMATION only
    if not handle:
        return dict(result, error=c.get_last_error())
    try:
        created, exited, system, user = (w.FILETIME() for _ in range(4))
        if dll.GetProcessTimes(handle, c.byref(created), c.byref(exited), c.byref(system), c.byref(user)):
            result['birth'] = (created.dwHighDateTime << 32) | created.dwLowDateTime
            result['exited'] = bool(exited.dwHighDateTime or exited.dwLowDateTime)
        else:
            result['times_error'] = c.get_last_error()
        size = w.DWORD(32768)
        image = c.create_unicode_buffer(size.value)
        if dll.QueryFullProcessImageNameW(handle, 0, image, c.byref(size)):
            result['image'] = image.value
        else:
            result['image_error'] = c.get_last_error()
        return result
    finally:
        if opened:
            dll.CloseHandle(handle)


def same_replacement(arranged, observed):
    """The drift arrangement's replacement engine as the worker proved it
    before the release (GUID, PID and native birth), never a process adopted
    afterwards: a complete identity that matches exactly."""
    keys = ('guid', 'pid', 'birth')
    return all(arranged.get(key) for key in keys) and {key: observed.get(key) for key in keys} == {
        key: arranged[key] for key in keys}


def windows_process_parents():
    """Read-only pid -> parent pid from one Toolhelp snapshot."""
    import ctypes as c
    from ctypes import wintypes as w

    class Entry(c.Structure):
        _fields_ = [('size', w.DWORD), ('usage', w.DWORD), ('pid', w.DWORD), ('heap', c.c_size_t),
                    ('module', w.DWORD), ('threads', w.DWORD), ('parent', w.DWORD), ('priority', w.LONG),
                    ('flags', w.DWORD), ('exe', w.WCHAR * 260)]

    dll = c.WinDLL('kernel32', use_last_error=True)
    dll.CreateToolhelp32Snapshot.argtypes = (w.DWORD, w.DWORD)
    dll.CreateToolhelp32Snapshot.restype = w.HANDLE
    dll.Process32FirstW.argtypes = dll.Process32NextW.argtypes = (w.HANDLE, c.POINTER(Entry))
    dll.CloseHandle.argtypes = (w.HANDLE,)
    snapshot = dll.CreateToolhelp32Snapshot(0x2, 0)  # TH32CS_SNAPPROCESS
    assert snapshot and snapshot != c.c_void_p(-1).value, c.get_last_error()
    try:
        entry, parents = Entry(), {}
        entry.size = c.sizeof(Entry)
        more = dll.Process32FirstW(snapshot, c.byref(entry))
        while more:
            parents[entry.pid] = (entry.parent, entry.exe)
            more = dll.Process32NextW(snapshot, c.byref(entry))
        return parents
    finally:
        dll.CloseHandle(snapshot)


def windows_file_users(path):
    """Bounded Restart Manager observation of ONE private image; no shutdown API."""
    import ctypes as c
    from ctypes import wintypes as w

    class Unique(c.Structure):
        _fields_ = [('pid', w.DWORD), ('birth', w.FILETIME)]

    class Info(c.Structure):
        _fields_ = [('process', Unique), ('application', w.WCHAR * 256),
                    ('service', w.WCHAR * 64), ('kind', c.c_int), ('status', w.ULONG),
                    ('session', w.DWORD), ('restartable', w.BOOL)]

    dll = c.WinDLL('rstrtmgr', use_last_error=True)
    dll.RmStartSession.argtypes = (c.POINTER(w.DWORD), w.DWORD, w.LPWSTR)
    dll.RmRegisterResources.argtypes = (w.DWORD, w.UINT, c.POINTER(w.LPCWSTR),
                                       w.UINT, c.POINTER(Unique), w.UINT, c.POINTER(w.LPCWSTR))
    dll.RmGetList.argtypes = (w.DWORD, c.POINTER(w.UINT), c.POINTER(w.UINT),
                            c.POINTER(Info), c.POINTER(w.DWORD))
    dll.RmEndSession.argtypes = (w.DWORD,)
    session, key = w.DWORD(), c.create_unicode_buffer(33)
    code = dll.RmStartSession(c.byref(session), 0, key)
    if code:
        return dict(available=False, operation='start', code=code)
    try:
        paths = (w.LPCWSTR * 1)(str(path))
        code = dll.RmRegisterResources(session, 1, paths, 0, None, 0, None)
        if code:
            return dict(available=False, operation='register', code=code)
        until = time.monotonic() + 2
        for _ in range(3):
            needed, count, reasons = w.UINT(), w.UINT(32), w.DWORD()
            values = (Info * 32)()
            code = dll.RmGetList(session, c.byref(needed), c.byref(count), values, c.byref(reasons))
            if needed.value > 32 or count.value > 32:
                return dict(available=False, operation='bound', count=needed.value)
            if code == 234 and time.monotonic() < until:  # ERROR_MORE_DATA, bounded churn only
                continue
            if code:
                return dict(available=False, operation='list', code=code)
            users = []
            for item in values[:count.value]:
                birth = (item.process.birth.dwHighDateTime << 32) | item.process.birth.dwLowDateTime
                observed = windows_process_metadata(item.process.pid)
                users.append(dict(observed, recorded_birth=birth,
                                  same_birth=observed.get('birth') == birth))
            return dict(available=True, users=users)
        return dict(available=False, operation='churn')
    finally:
        dll.RmEndSession(session)


class WindowsFrontend(unittest.TestCase):
    cli = kernel.Desktop.cli
    serve = kernel.Desktop.serve

    def stop(self, process):
        # Windows-only fixture: keep the original Popen handle, drain its pipes
        # even if poll already saw exit, and explicitly release it before rmtree.
        if getattr(process, '_fixture_settled', False):
            return
        if process.poll() is None:
            process.terminate()  # Only this original Popen/CreateProcess handle.
        process.communicate(timeout=8)
        self.assertIsNotNone(process.returncode)
        process._handle.Close()
        process._fixture_settled = True

    def trace_file_users(self, phase):
        try:
            observed = windows_file_users(kernel.HGS)
        except Exception as error:
            # Diagnostic failure never skips the original-child joins.
            observed = dict(available=False, error=type(error).__name__)
        print('STARTUP_REFUSAL_IMAGE_USERS_' + phase + ' ' + json.dumps(observed), file=sys.stderr)

    def settle_image(self):
        """Wait out a copied hgs.exe image still mapped by a child service.

        Registered right after Desktop.setUp so, with LIFO cleanups, it runs
        after every fixture stop and just before the TemporaryDirectory
        cleanup. Observation only: a real leak fails the test and an
        unavailable observation is diagnostic; nothing is killed or retried.
        """
        selected = getattr(self, 'image', None)
        if selected is None:
            return
        deadline = time.monotonic() + 10
        first = None
        while True:
            try:
                observed = windows_file_users(selected)
            except Exception as error:
                observed = dict(available=False, error=type(error).__name__)
            if not observed.get('available'):
                print('IMAGE_USERS_AT_CLEANUP ' + json.dumps(observed), file=sys.stderr)
                return
            if not observed.get('users'):
                return
            if first is None:
                first = observed
                print('IMAGE_USERS_AT_CLEANUP ' + json.dumps(first), file=sys.stderr)
            if time.monotonic() >= deadline:
                self.fail('the copied hgs.exe image is still in use at cleanup: ' + json.dumps(first))
            time.sleep(0.1)

    def setUp(self):
        self.assertEqual(os.name, 'nt', 'run this native suite on Windows')
        fixture = Path(os.environ['HGS_WINDOWS_FRONTEND_FIXTURE']).resolve(strict=True)
        original_cli = kernel.HGS
        kernel.Desktop.setUp(self)
        # Runs after every later stop cleanup and immediately before the temp
        # root's own cleanup (addCleanup is LIFO); the image is set below.
        self.addCleanup(self.settle_image)
        stage = self.root / 'native'
        stage.mkdir()
        selected = stage / 'hgs.exe'
        self.image = selected
        self.frontend = stage / 'hgs-tray.exe'
        shutil.copy2(original_cli, selected)
        shutil.copy2(fixture, self.frontend)
        kernel.HGS = selected
        self.addCleanup(setattr, kernel, 'HGS', original_cli)
        self.env.update(LOCALAPPDATA=str(self.root / 'AppData' / 'Local'))
        self.control = self.root / 'control.json'

    def invoke(self, case='positive'):
        _, status = self.serve()
        request_id = str(uuid.uuid4())
        self.control.write_text(json.dumps(dict(case=case, configuration=status['configuration'],
                                               cli=str(kernel.HGS), request_id=request_id)), encoding='utf-8')
        env = dict(self.env, HGS_FRONTEND_TEST_CONTROL=str(self.control))
        if case == 'huge_env':
            for index in range(3):
                env[f'ZERUS_FRONTEND_FIXTURE_PAD{index}'] = 'λ' * 20_000
        output = subprocess.run([str(self.frontend), '--exact',
                                 'instance::tests::evidence::native_frontend_request_fixture', '--nocapture'],
                                env=env, capture_output=True, text=True, timeout=15,
                                creationflags=subprocess.CREATE_NO_WINDOW)
        self.assertEqual(output.returncode, 0, (output.stdout + output.stderr)[-8000:])
        results = [json.loads(line.removeprefix('FRONTEND_RESULT ')) for line in output.stdout.splitlines()
                   if line.startswith('FRONTEND_RESULT ')]
        self.assertEqual(len(results), 1, output.stdout[-4096:])
        chains = [json.loads(line.removeprefix('FRONTEND_CHAIN ')) for line in output.stdout.splitlines()
                  if line.startswith('FRONTEND_CHAIN ')]
        self.assertEqual(len(chains), 1, output.stdout[-4096:])
        self.assertLessEqual(len(chains[0]), 32)
        print('FRONTEND_OBSERVED_CHAIN ' + json.dumps(dict(case=case, chain=chains[0])), file=sys.stderr)
        self.assertEqual(results[0]['request_id'], request_id)
        if case != 'positive':
            self.assertFalse(results[0]['accepted'])
            self.assertFalse(results[0]['durable'])
        # Safe passive OFF may commit; neither positive nor negative case can
        # enable approvals or grant writers in this privately launched service.
        after = self.cli('desktop', 'status', '--json')
        self.assertFalse(after['writers_enabled'])
        self.assertFalse(after['automatic_approvals'])
        self.assertFalse((self.root / 'unexpected-tool').exists())
        return results[0]

    def mode_check(self, image=None):
        self.control.write_text(json.dumps(dict(case='mode_check', cli=str(kernel.HGS))), encoding='utf-8')
        output = subprocess.run([str(image or self.frontend), '--exact',
                                 'instance::tests::evidence::native_frontend_request_fixture', '--nocapture'],
                                env=dict(self.env, HGS_FRONTEND_TEST_CONTROL=str(self.control)),
                                capture_output=True, text=True, timeout=15,
                                creationflags=subprocess.CREATE_NO_WINDOW)
        self.assertEqual(output.returncode, 0, (output.stdout + output.stderr)[-8000:])
        results = [json.loads(line.removeprefix('MODE_CALLER_RESULT ')) for line in output.stdout.splitlines()
                   if line.startswith('MODE_CALLER_RESULT ')]
        self.assertEqual(len(results), 1, output.stdout[-4096:])
        self.assertFalse(self.desktop.exists(), 'mode admission is read-only')
        return results[0]

    def test_mode_caller_accepts_the_actual_retained_installed_frontend_without_a_console(self):
        result = self.mode_check()
        self.assertEqual(result['code'], 0, result)
        value = json.loads(result['stdout'])
        self.assertTrue(value['admitted'])
        self.assertEqual(value['source'], 'installed_frontend')

    def test_mode_caller_still_refuses_the_real_marked_ancestor_when_the_child_is_clean(self):
        key = 'HGS_RUN_ID'
        previous = os.environ.get(key)
        os.environ[key] = 'native-mode-parent-fixture'
        try:
            result = self.mode_check()
            self.assertNotEqual(result['code'], 0)
            self.assertIn('agent-descended', result['stderr'])
        finally:
            if previous is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = previous

    def test_mode_caller_refuses_an_agent_named_frontend_image(self):
        agent = self.frontend.with_name('codex.exe')
        shutil.copy2(self.frontend, agent)
        result = self.mode_check(agent)
        self.assertNotEqual(result['code'], 0)
        self.assertIn('agent-descended', result['stderr'])

    def test_checking_cli_cannot_be_its_own_frontend_and_cannot_manufacture_a_console(self):
        shutil.copy2(kernel.HGS, self.frontend)
        result = subprocess.run([str(self.frontend), 'desktop', 'registration', 'mode-check', '--json'],
                                env=self.env, capture_output=True, text=True, timeout=8,
                                creationflags=subprocess.CREATE_NO_WINDOW)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Settings', result.stderr)
        self.assertFalse(self.desktop.exists())

    def test_real_canonical_frontend_and_original_child_disable_commits_in_passive_service(self):
        receipt = self.invoke()
        self.assertIs(receipt['accepted'], True)
        self.assertIs(receipt['durable'], True)
        self.assertIs(receipt['enabled'], False)
        self.assertEqual(receipt, dict(request_id=receipt['request_id'], accepted=True, durable=True,
                                      op='automatic_approvals', enabled=False, revision=1))
        database = self.desktop / 'state.sqlite3'
        self.assertTrue(database.is_file())
        with closing(sqlite3.connect(database.as_uri() + '?mode=ro', uri=True)) as db:
            recorded = db.execute('SELECT receipt FROM desktop_delivery_requests WHERE request_id=?',
                                  (receipt['request_id'],)).fetchone()
            self.assertIsNotNone(recorded)
            self.assertEqual(json.loads(recorded[0]), receipt)
            self.assertEqual(db.execute('SELECT enabled,revision FROM desktop_delivery_policy WHERE id=1')
                             .fetchone(), (0, 1))

    def test_authenticated_enable_still_refuses_in_passive_service(self):
        self.assertEqual(self.invoke('enable')['code'], 'desktop_background_unavailable')

    def test_wrong_native_child_birth_refuses_before_actor_admission(self):
        self.assertEqual(self.invoke('wrong_birth')['code'], 'frontend_authentication_refused')

    def test_wrong_cached_configuration_is_not_native_authority(self):
        self.assertEqual(self.invoke('wrong_config')['code'], 'frontend_authentication_refused')

    def test_registered_startup_refusal_retains_the_guard_reason_without_replacing_the_owner(self):
        original, status = self.serve()
        refused = subprocess.Popen([str(kernel.HGS), 'desktop', 'serve', '--config-dir', str(self.config),
                                    '--state-dir', str(self.state)], env=self.env, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.addCleanup(self.stop, refused)
        try:
            identities = dict(original=windows_process_metadata(original.pid, int(original._handle)),
                              refused=windows_process_metadata(refused.pid, int(refused._handle)),
                              selected_image=str(kernel.HGS))
            print('STARTUP_REFUSAL_ORIGINAL_CHILDREN ' + json.dumps(identities), file=sys.stderr)
            _, error = refused.communicate(timeout=10)
            # Natural exit is required before cleanup can terminate anything.
            self.assertEqual(refused.returncode, 78, error)
            path = self.desktop / 'startup-failure.json'
            data = path.read_bytes()
            self.assertLessEqual(len(data), 4096)
            diagnostic = json.loads(data)
            self.assertEqual(diagnostic['abi'], 'desktop-startup-failure-v1')
            self.assertEqual(diagnostic['exit_code'], 78)
            self.assertEqual(diagnostic['pid'], refused.pid)
            self.assertEqual(diagnostic['birth'], str(identities['refused']['birth']))
            self.assertEqual(diagnostic['configuration'], status['configuration'])
            self.assertIn('acquire desktop owner lock', diagnostic['message'])
            self.assertIn('another desktop service owns this state', diagnostic['message'])
            self.assertIsNone(original.poll())
            self.assertEqual(self.cli('desktop', 'status', '--json')['lifecycle']['pid'], original.pid)
        finally:
            self.trace_file_users('BEFORE_JOIN')
            try:
                self.stop(refused)
            finally:
                try:
                    self.stop(original)
                finally:
                    self.trace_file_users('AFTER_JOIN')
        self.assertTrue(original._handle.closed)
        self.assertTrue(refused._handle.closed)

    def test_registered_passive_start_without_a_console_keeps_the_original_startup_reason(self):
        # Separate private configuration: this diagnoses the no-console launch
        # class before any Scheduler test, never retries an uncertain RunEx.
        self.desktop.mkdir(parents=True)
        original = subprocess.Popen([str(kernel.HGS), 'desktop', 'serve', '--config-dir', str(self.config),
                                     '--state-dir', str(self.state)], env=self.env,
                                    stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    text=True, creationflags=subprocess.CREATE_NO_WINDOW)
        self.addCleanup(self.stop, original)
        until = time.monotonic() + 10
        while time.monotonic() < until:
            if original.poll() is not None:
                _, stderr = original.communicate(timeout=1)
                self.fail('private no-console registered startup refused: ' + stderr[-4096:])
            status = self.cli('desktop', 'status', '--json')
            if status.get('lifecycle', {}).get('ipc_ready'):
                self.assertEqual(status['lifecycle']['pid'], original.pid)
                self.assertEqual(status['lifecycle']['mode'], 'passive')
                return
            time.sleep(.02)
        self.fail('private no-console registered startup did not reach readiness')

    def test_wrong_native_request_fingerprint_refuses_before_actor_admission(self):
        self.assertEqual(self.invoke('wrong_fingerprint')['code'], 'frontend_authentication_refused')

    def test_oversize_unicode_environment_remains_unavailable(self):
        self.assertEqual(self.invoke('huge_env')['code'], 'frontend_authentication_refused')

    def test_child_stripping_markers_cannot_hide_its_real_tagged_ancestor(self):
        # Only this disposable test driver is tagged. The explicit child env
        # remains clean; the service must inspect the original parent chain.
        key = 'HGS_RUN_ID'
        previous = os.environ.get(key)
        os.environ[key] = 'native-fixture-parent'
        try:
            self.assertEqual(self.invoke('agent_parent')['code'], 'frontend_authentication_refused')
        finally:
            if previous is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = previous


class DriftReplacementIdentity(unittest.TestCase):
    """Pure control for the hosted drift arrangement; no Windows API."""

    def test_only_the_engine_proven_before_the_release_counts(self):
        arranged = dict(guid='{4B27B893-B923-49A9-B9E0-3C8C90DCFA73}', pid=2, birth=222)
        self.assertTrue(same_replacement(arranged, dict(arranged)))
        for observed in (dict(arranged, pid=3, birth=333), dict(arranged, birth=333), dict(arranged, pid=3),
                         dict(arranged, guid='{85CE29DB-DBE8-4634-8E4B-E5CB3A107FD6}'), {}):
            self.assertFalse(same_replacement(arranged, observed), observed)
        for partial in ({}, dict(arranged, pid=0), dict(arranged, guid=''), dict(guid=arranged['guid'], pid=2)):
            self.assertFalse(same_replacement(partial, dict(partial)), partial)


class TaskModeFence:
    """One uncertain fixture keeps custody; later cases launch nothing."""
    def __init__(self):
        self.retained = None

    def check(self):
        if self.retained is not None:
            case, task, root = self.retained
            raise unittest.SkipTest(f'TaskMode fixture fenced after first failure {case}; '
                                    f'task {task} retained under custody at {root}')

    def retain(self, case, task, root):
        if self.retained is None:
            self.retained = (case, task, str(root))


@unittest.skipUnless(os.name == 'nt' and os.environ.get('GITHUB_ACTIONS') == 'true'
                     and os.environ.get('RUNNER_ENVIRONMENT') == 'github-hosted',
                     'Task-mode fixture requires a disposable hosted Windows runner')
class WindowsTaskMode(unittest.TestCase):
    fixture_fence = TaskModeFence()

    def setUp(self):
        # Check before allocating paths or probing a shared per-user task.
        self.fixture_fence.check()
        self.root = Path(tempfile.mkdtemp(prefix='zerus-task-mode-')).resolve()
        self.cli_image = self.root / 'portable' / 'hgs.exe'
        self.cli_image.parent.mkdir()
        shutil.copy2(kernel.HGS, self.cli_image)
        self.profile = self.root / 'profile'
        self.profile.mkdir()
        self.frontend = self.profile / '.local' / 'bin' / 'hgs-tray.exe'
        self.frontend_source = Path(os.environ['HGS_WINDOWS_FRONTEND_FIXTURE']).resolve(strict=True)
        self.config = self.root / 'config'
        self.config.mkdir()
        (self.config / 'config').write_text('HGS_SELF=windows-mode-fixture\nHGS_PEERS=\n', encoding='utf-8')
        self.state = self.root / 'state'
        self.control = self.root / 'control.json'
        self.env = {k:v for k,v in os.environ.items() if not k.upper().startswith('HGS_')}
        # HOME selects a wholly private staging profile. USERPROFILE stays
        # real solely for the canonical native instance namespace shared with
        # the Scheduler-launched service; it is not a caller/ownership permit.
        profile = os.environ['USERPROFILE']
        self.env.update(HOME=str(self.profile), USERPROFILE=profile, HGS_CONFIG_DIR=str(self.config),
                        HGS_STATE_DIR=str(self.state), HGS_PEERS='')
        self.task = None
        self.installed = False
        self.task_may_remain = False
        self.latest = None
        self.held_originals = []
        self.addCleanup(self.close_originals)
        self.addCleanup(self.retain_task_custody)

    def retain_task_custody(self):
        if self.task_may_remain:
            self.fixture_fence.retain(self.id(), self.task, self.root)

    def close_originals(self):
        for dll, handle in self.held_originals:
            dll.CloseHandle(handle)
        self.held_originals.clear()

    def pin_original(self, original):
        """Read-only kernel lifetime pin; never a stop/adoption capability."""
        import ctypes as c
        from ctypes import wintypes as w
        dll = c.WinDLL('kernel32', use_last_error=True)
        dll.OpenProcess.argtypes = (w.DWORD, w.BOOL, w.DWORD)
        dll.OpenProcess.restype = w.HANDLE
        dll.CloseHandle.argtypes = (w.HANDLE,)
        handle = dll.OpenProcess(0x1000 | 0x100000, False, original['pid'])
        self.assertTrue(handle, c.get_last_error())
        self.held_originals.append((dll, handle))
        metadata = windows_process_metadata(original['pid'], handle)
        self.assertEqual(metadata['birth'], original['birth'], metadata)
        self.assertFalse(metadata['exited'], metadata)

    def cli(self, *args):
        if args[:3] == ('desktop', 'registration', 'install'):
            # An unsuccessful/lost install reply does not prove absence.
            self.task_may_remain = True
        result = subprocess.run([str(self.cli_image), *args], env=self.env,
                                capture_output=True, text=True, encoding='utf-8', timeout=20)
        self.assertEqual(result.returncode, 0, (result.stderr + result.stdout)[-8192:])
        self.assertLessEqual(len(result.stdout.encode('utf-8')), 65536)
        value = json.loads(result.stdout)
        if args[:3] == ('desktop', 'registration', 'remove') and value.get('registered') is False:
            self.task_may_remain = False
        return value

    def frontend_call(self, control, prefix, *, frontend=None):
        self.control.write_text(json.dumps(dict(control, cli=str(self.cli_image))), encoding='utf-8')
        result = subprocess.run([str(frontend or self.frontend), '--exact',
                                 'instance::tests::evidence::native_frontend_request_fixture', '--nocapture'],
                                env=dict(self.env, HGS_FRONTEND_TEST_CONTROL=str(self.control)),
                                capture_output=True, text=True, timeout=40,
                                creationflags=subprocess.CREATE_NO_WINDOW)
        self.assertEqual(result.returncode, 0, (result.stdout + result.stderr)[-8192:])
        values = [json.loads(line.removeprefix(prefix)) for line in result.stdout.splitlines()
                  if line.startswith(prefix)]
        self.assertEqual(len(values), 1, result.stdout[-8192:])
        return values[0]

    def mode(self, desired):
        result = self.frontend_call(dict(case='mode_switch', desired=desired), 'MODE_CALLER_RESULT ')
        self.assertEqual(result['code'], 0, result)
        return json.loads(result['stdout'])

    def grant(self, case='ownership_grant'):
        status = self.cli('desktop', 'status', '--json')
        ownership = self.cli('desktop', 'ownership-status', '--json')
        result = self.frontend_call(dict(case=case, configuration=status['configuration'],
                                        journal=ownership['journal'], revision=ownership['revision'],
                                        request_id=str(uuid.uuid4())), 'FRONTEND_RESULT ')
        self.assertTrue(result['accepted'], result)
        self.assertTrue(result['durable'], result)
        self.assertEqual(result['outcome']['state'], 'granted', result)

    def requester_bound_ownership_is_a_native_read(self):
        """Real current requester + existing actor row; no synthetic auth positive."""
        lifecycle = self.cli('desktop', 'status', '--json')['lifecycle']
        ownership = self.cli('desktop', 'ownership-status', '--json')
        self.assertEqual(ownership['state'], 'service')
        self.assertTrue(ownership['identity_current'])
        self.assertFalse(ownership['rearm_required'])
        path = self.state / 'desktop' / 'windows-registration-mode.json'
        record = path.read_bytes()
        instances = self.observe()['instances']
        def rows():
            with closing(sqlite3.connect(self.state / 'desktop' / 'state.sqlite3')) as db:
                return tuple(db.execute('SELECT * FROM desktop_ownership')), tuple(db.execute(
                    'SELECT request_id,revision,fingerprint,origin,receipt,final FROM desktop_ownership_requests ORDER BY revision'))
        before = rows()
        query = dict(request_id=str(uuid.uuid4()), configuration=ownership['configuration'],
                     journal=ownership['journal'], owner_epoch=ownership['owner_epoch'],
                     service_epoch=lifecycle['epoch'], generation=lifecycle['generation'])
        control = dict(case='ownership_observe', configuration=ownership['configuration'], query=query)
        positive = self.frontend_call(control, 'OWNERSHIP_OBSERVATION ')
        self.assertTrue(positive['checked'], positive)
        self.assertEqual(positive['current'], ownership)
        # A second natively selected image, with identical bytes, is not the
        # granted Origin. cli_image now names the staged CLI: its sibling is
        # self.frontend itself, so copying there would not change the requester.
        # This class runs ONLY on a disposable hosted runner. Use the other
        # maintained installed-app path, refuse any existing file, and remove
        # only our exact file and the empty directories we created.
        other = Path(self.env['LOCALAPPDATA']) / 'Programs' / 'Zerus' / 'hgs-tray.exe'
        self.assertFalse(other.exists(), 'disposable runner already has a Zerus frontend')
        created = []
        parent = other.parent
        while not parent.exists():
            created.append(parent)
            parent = parent.parent
        original = self.frontend.stat()
        owned = False
        try:
            other.parent.mkdir(parents=True, exist_ok=True)
            with other.open('xb') as target:
                owned = True
                with self.frontend_source.open('rb') as source:
                    shutil.copyfileobj(source, target)
            self.assertFalse(os.path.samefile(other, self.frontend))
            refused = self.frontend_call(control, 'OWNERSHIP_OBSERVATION ', frontend=other)
            self.assertFalse(refused['checked'], refused)
            current = self.frontend.stat()
            self.assertEqual((current.st_dev, current.st_ino, current.st_size, current.st_mtime_ns),
                             (original.st_dev, original.st_ino, original.st_size, original.st_mtime_ns))
        finally:
            if owned:
                other.unlink()
            for directory in created:
                directory.rmdir()
        for key, value in [('service_epoch', str(uuid.uuid4())),
                           ('generation', lifecycle['generation'] + 1),
                           ('owner_epoch', str(uuid.uuid4())), ('journal', str(uuid.uuid4()))]:
            stale = dict(control, query=dict(query, **{key: value}))
            self.assertFalse(self.frontend_call(stale, 'OWNERSHIP_OBSERVATION ')['checked'], key)
        self.assertEqual(rows(), before)
        self.assertEqual(path.read_bytes(), record)
        self.assertEqual(self.observe()['instances'], instances)
        self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), ownership)

    def cancel(self, request_id):
        result = self.frontend_call(dict(case='mode_cancel', request_id=request_id), 'MODE_CALLER_RESULT ')
        self.assertEqual(result['code'], 0, result)
        return json.loads(result['stdout'])

    def review_mode(self, request_id):
        result = self.frontend_call(dict(case='mode_review', request_id=request_id), 'MODE_CALLER_RESULT ')
        self.assertEqual(result['code'], 0, result)
        return json.loads(result['stdout'])

    def choice_after_review(self, desired, reviewed):
        request = str(uuid.uuid4())
        result = self.frontend_call(dict(case='mode_switch', desired=desired, after_review=reviewed,
                                        request_id=request), 'MODE_CALLER_RESULT ')
        self.assertEqual(result['code'], 0, result)
        value = json.loads(result['stdout'])
        self.assertTrue(value['ok'], value)
        self.assertEqual(value['request_id'], request)
        return value

    def first_call(self, desired, request_id):
        return self.frontend_call(dict(case='mode_switch', desired=desired, request_id=request_id),
                                  'MODE_CALLER_RESULT ')

    def refused_first_choice(self, request_id, reason):
        """A refused first choice writes no record and changes no task."""
        path = self.state / 'desktop' / 'windows-registration-mode.json'
        before, record_before = self.observe()['instances'], path.read_bytes()
        grant = self.cli('desktop', 'ownership-status', '--json')
        result = self.first_call('background', request_id)
        self.assertEqual(result['code'], 0, result)
        value = json.loads(result['stdout'])
        self.assertFalse(value['ok'], value)
        self.assertIn(reason, value['error'])
        self.assertEqual(path.read_bytes(), record_before)
        self.assertEqual(self.observe()['instances'], before)
        self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), grant)
        return value

    def overlapping_layout_refuses_the_first_choice(self):
        """zerus-yfbd.1 R4: a config folder inside the state loss domain
        (nested, in other case, or through a junction) never takes a first
        choice. Refused before any anchor, record reservation, UUID or
        manager effect; nothing repairs or moves the layout."""
        path = self.state / 'desktop' / 'windows-registration-mode.json'
        record_before = path.read_bytes()
        before = self.observe()['instances']
        grant = self.cli('desktop', 'ownership-status', '--json')
        junction = self.root / 'state-alias'
        made = subprocess.run(['cmd', '/d', '/c', 'mklink', '/J', str(junction), str(self.state)],
                              capture_output=True, text=True, timeout=20)
        self.assertEqual(made.returncode, 0, made)
        separate, requests = self.env['HGS_CONFIG_DIR'], []
        try:
            for case, folder in (('nested', self.state / 'desktop' / 'config'),
                                 ('other case', Path(str(self.state).upper()) / 'cfg'),
                                 ('junction', junction / 'cfg')):
                self.env['HGS_CONFIG_DIR'] = str(folder)
                request = str(uuid.uuid4())
                requests.append(request)
                result = self.first_call('background', request)
                self.assertNotEqual(result['code'], 0, (case, result))
                self.assertIn('config folder outside the desktop state folder', result['stderr'], case)
                self.assertEqual(list(self.state.rglob('windows-first-choice-*.json')), [], case)
        finally:
            self.env['HGS_CONFIG_DIR'] = separate
            os.rmdir(junction)  # The junction only, never its target.
        self.assertEqual(path.read_bytes(), record_before)
        self.assertEqual(self.observe()['instances'], before)
        self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), grant)
        self.assertEqual(list(self.config.glob('windows-first-choice-*.json')), [])
        for request in requests:
            result = self.cli('desktop', 'registration', 'mode-result', '--json', '--request-id', request)
            self.assertEqual((result['receipt'], result['code']), (None, 'original_choice_unavailable'))

    def writer_commit_window_is_unavailable_never_lost(self, first):
        """A writer publishes its anchor, then saves its record, under the
        registration lock. A reader in that window waits out the bounded lock
        and refuses; it never reports the window as a lost journal."""
        status = self.cli('desktop', 'registration', 'status', '--json')
        sid = status['task_name'].removeprefix('hgs-desktop-')
        anchor = self.config / f"windows-first-choice-{status['configuration']}.json"
        original = anchor.read_bytes()
        published = json.loads(original)
        published['request'] = str(uuid.uuid4())
        import ctypes as c
        from ctypes import wintypes as w
        dll = c.WinDLL('kernel32', use_last_error=True)
        dll.CreateMutexW.argtypes = (c.c_void_p, w.BOOL, w.LPCWSTR)
        dll.CreateMutexW.restype = w.HANDLE
        dll.WaitForSingleObject.argtypes = (w.HANDLE, w.DWORD)
        dll.WaitForSingleObject.restype = w.DWORD
        dll.ReleaseMutex.argtypes = (w.HANDLE,)
        dll.CloseHandle.argtypes = (w.HANDLE,)
        handle = dll.CreateMutexW(None, False, 'Global\\hgs-desktop-registration-' + sid)
        self.assertTrue(handle, c.get_last_error())
        try:
            self.assertEqual(dll.WaitForSingleObject(handle, 10000), 0)
            try:
                anchor.write_bytes(json.dumps(published).encode('utf-8'))
                for args in (('mode-result', '--json', '--request-id', first['request_id']),
                             ('status', '--json')):
                    result = subprocess.run([str(self.cli_image), 'desktop', 'registration', *args],
                                            env=self.env, capture_output=True, text=True,
                                            encoding='utf-8', timeout=20)
                    self.assertNotEqual(result.returncode, 0, (args, result.stdout))
                    self.assertNotIn('first_choice_lost', result.stdout, args)
                    if args[0] == 'mode-result':  # Status also waits in its generic exchange.
                        self.assertIn('Registration lock not confirmed', result.stderr, args)
            finally:
                anchor.write_bytes(original)
                dll.ReleaseMutex(handle)
        finally:
            dll.CloseHandle(handle)
        self.assertIsNone(self.cli('desktop', 'registration', 'status', '--json')['first_choice_lost'])

    def first_call_started(self, request_id):
        """The first-choice form through the installed-frontend caller, not
        awaited. Debug builds hold exactly this UUID at Held (zerus-yfbd.1)."""
        self.control.write_text(json.dumps(dict(case='mode_switch', desired='background', request_id=request_id,
                                                cli=str(self.cli_image))), encoding='utf-8')
        return subprocess.Popen([str(self.frontend), '--exact',
                                 'instance::tests::evidence::native_frontend_request_fixture', '--nocapture'],
                                env=dict(self.env, HGS_FRONTEND_TEST_CONTROL=str(self.control),
                                         HGS_TEST_MODE_HELD_PAUSE=request_id),
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                creationflags=subprocess.CREATE_NO_WINDOW)

    def first_call_finished(self, child):
        out, err = child.communicate(timeout=90)
        values = [json.loads(line.removeprefix('MODE_CALLER_RESULT ')) for line in out.splitlines()
                  if line.startswith('MODE_CALLER_RESULT ')]
        self.assertEqual(len(values), 1, (out + err)[-8192:])
        return values[0]

    def held_helper(self, request_id, child):
        """The helper's own private marker names its PID. It counts only as
        the helper of THIS call: the selected CLI image, a child of the CLI
        that the frontend fixture child started, with no other owned work."""
        marker = self.config / f'held-pause-{request_id}.json'
        value, deadline = None, time.monotonic() + 30
        while value is None and time.monotonic() < deadline:
            self.assertIsNone(child.poll(), 'the first choice ended before Held')
            try:
                value = json.loads(marker.read_text(encoding='utf-8'))
            except (FileNotFoundError, PermissionError, json.JSONDecodeError):
                time.sleep(0.05)
        self.assertIsNotNone(value, 'the debug helper never reached Held')
        self.assertEqual(value['request'], request_id)
        helper = windows_process_metadata(value['pid'])
        self.assertFalse(helper['exited'], helper)
        parents = windows_process_parents()
        cli = parents[value['pid']][0]
        self.assertEqual(parents[cli][0], child.pid, parents.get(cli))
        for pid in (value['pid'], cli):
            self.assertEqual(Path(windows_process_metadata(pid)['image']).resolve(strict=True), self.cli_image)
        owned = [pid for pid, (parent, exe) in parents.items() if parent == value['pid']
                 and windows_process_metadata(pid).get('birth', 0) >= helper['birth']]
        # A console host is the only child a hidden console helper may own.
        self.assertTrue(all(parents[pid][1].lower() == 'conhost.exe' for pid in owned), owned)
        return dict(pid=value['pid'], birth=helper['birth'], cli=cli, owned=owned, marker=marker)

    def kill_held_helper(self, helper):
        """Terminate only the captured helper (same PID AND birth through one
        handle), then wait for its own console host to leave by itself."""
        import ctypes as c
        from ctypes import wintypes as w
        dll = c.WinDLL('kernel32', use_last_error=True)
        dll.OpenProcess.argtypes = (w.DWORD, w.BOOL, w.DWORD)
        dll.OpenProcess.restype = w.HANDLE
        dll.TerminateProcess.argtypes = (w.HANDLE, w.UINT)
        dll.WaitForSingleObject.argtypes = (w.HANDLE, w.DWORD)
        dll.WaitForSingleObject.restype = w.DWORD
        dll.CloseHandle.argtypes = (w.HANDLE,)
        handle = dll.OpenProcess(0x0001 | 0x00100000 | 0x1000, False, helper['pid'])
        self.assertTrue(handle, c.get_last_error())
        try:
            current = windows_process_metadata(helper['pid'], handle)
            self.assertEqual((current['birth'], current['exited']), (helper['birth'], False), current)
            self.assertTrue(dll.TerminateProcess(handle, 1), c.get_last_error())
            self.assertEqual(dll.WaitForSingleObject(handle, 10000), 0)
        finally:
            dll.CloseHandle(handle)
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline and any(not windows_process_metadata(pid).get('exited', True)
                                                  and 'error' not in windows_process_metadata(pid)
                                                  for pid in helper['owned']):
            time.sleep(0.1)

    def lost_reply_at_held_keeps_its_original(self):
        """Debug held seam (zerus-yfbd.1). While a first choice is held after
        its durable reservation, an ownership change is refused: the Grant
        cannot drift inside a transition (the helper holds the handoff lock
        every Grant/revoke takes). Then a true helper death: the original stays
        Unknown and pending, a duplicate only reads, the old frame refuses, and
        only the supported pre-manager cancellation closes it."""
        path = self.state / 'desktop' / 'windows-registration-mode.json'
        configuration = self.cli('desktop', 'status', '--json')['configuration']
        before = self.observe()['instances']
        service = self.cli('desktop', 'status', '--json')['lifecycle']['pid']
        # "Unchanged" below compares exactly this one running passive engine.
        self.assertEqual([item['engine_pid'] for item in before], [service], before)
        granted = self.cli('desktop', 'ownership-status', '--json')
        database = (self.state / 'desktop' / 'state.sqlite3').as_uri() + '?mode=ro'
        with closing(sqlite3.connect(database, uri=True)) as db:
            commands = db.execute('SELECT count(*) FROM desktop_commands').fetchone()[0]
        request = str(uuid.uuid4())
        child = self.first_call_started(request)
        helper = self.held_helper(request, child)
        record = json.loads(path.read_text(encoding='utf-8'))
        self.assertEqual((record['pending']['request_id'], record['pending']['phase']), (request, 'held'), record)
        revoke = subprocess.run([str(self.cli_image), 'desktop', 'revoke', '--json', '--request-id',
                                 str(uuid.uuid4()), '--reason', 'manual'], env=self.env, capture_output=True,
                                text=True, encoding='utf-8', timeout=30)
        self.assertNotEqual(revoke.returncode, 0, revoke.stdout)
        self.assertIn('handoff', revoke.stdout + revoke.stderr)
        self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), granted)
        with closing(sqlite3.connect(database, uri=True)) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM desktop_commands').fetchone()[0], commands)
        self.kill_held_helper(helper)
        lost = self.first_call_finished(child)
        self.assertNotEqual(lost['code'], 0, lost)
        self.assertIn('retain the original pending UUID', lost['stderr'], lost)
        helper['marker'].unlink()
        status = self.cli('desktop', 'registration', 'status', '--json')
        self.assertEqual((status['pending_transition']['request_id'], status['pending_transition']['phase']),
                         (request, 'held'), status)
        self.assertEqual((status['first_choice']['request'], status['first_choice']['phase']),
                         (request, 'unknown'), status)
        self.assertIsNone(status['first_choice_lost'])
        anchor = self.config / f'windows-first-choice-{configuration}.json'
        self.assertEqual(json.loads(anchor.read_bytes())['request'], request)
        result = self.cli('desktop', 'registration', 'mode-result', '--json', '--request-id', request)
        self.assertEqual((result['outcome'], result['receipt']['phase']), ('unknown', 'unknown'), result)
        self.assertEqual(self.observe()['instances'], before)
        self.assertEqual(self.cli('desktop', 'status', '--json')['lifecycle']['pid'], service)
        arranged = path.read_bytes()
        duplicate = self.first_call('background', request)
        self.assertEqual(duplicate['code'], 0, duplicate)
        self.assertFalse(json.loads(duplicate['stdout'])['ok'], duplicate)
        refused = self.mode('background')
        self.assertFalse(refused['ok'], refused)
        self.assertIn('first mode choice is pending', refused['error'])
        self.assertEqual(path.read_bytes(), arranged)
        self.assertEqual(self.observe()['instances'], before)
        cancelled = self.cancel(request)
        self.assertEqual(cancelled['cancelled_request_id'], request, cancelled)
        self.assertFalse(cancelled['manager_changed'], cancelled)
        result = self.cli('desktop', 'registration', 'mode-result', '--json', '--request-id', request)
        self.assertEqual(result['receipt']['phase'], 'refused', result)
        self.assertEqual(self.observe()['instances'], before)
        self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), granted)

    def expired_hold_refuses_at_held(self):
        """The hold consumes the Stop deadline it started in. Held past it (here
        to the seam's own bound, with no release), the unchanged StopOffered
        save's fence refuses the expired deadline before any manager effect:
        the same original stays pending at Held, and only the supported
        pre-manager cancellation closes it."""
        path = self.state / 'desktop' / 'windows-registration-mode.json'
        before = self.observe()['instances']
        service = self.cli('desktop', 'status', '--json')['lifecycle']['pid']
        self.assertEqual([item['engine_pid'] for item in before], [service], before)
        granted = self.cli('desktop', 'ownership-status', '--json')
        request = str(uuid.uuid4())
        child = self.first_call_started(request)
        helper = self.held_helper(request, child)
        expired = self.first_call_finished(child)
        self.assertEqual(expired['code'], 0, expired)
        refused = json.loads(expired['stdout'])
        self.assertFalse(refused['ok'], refused)
        self.assertIn('deadline reached', refused['error'])
        timing = refused['stop_timing']
        self.assertEqual(timing['abi'], 'desktop-mode-stop-timing-v1')
        self.assertEqual(timing['budget_ms'], 2000)  # Documented Stop contract.
        self.assertLessEqual(timing['elapsed_ms'], 60000)
        self.assertEqual(set(timing['steps']), {'native_rechecks', 'metadata_sync',
                                               'instance_stop', 'physical_exit'})
        self.assertEqual(timing['steps']['instance_stop']['calls'], 0)
        self.assertEqual(timing['steps']['physical_exit']['calls'], 0)
        print('MODE_STOP_TIMING_EXPIRED_HOLD_CONTROL ' + json.dumps(timing), file=sys.stderr)
        helper['marker'].unlink()
        record = json.loads(path.read_text(encoding='utf-8'))
        self.assertEqual((record['pending']['request_id'], record['pending']['phase']), (request, 'held'), record)
        status = self.cli('desktop', 'registration', 'status', '--json')
        self.assertEqual(status['selected_mode'], 'passive')
        self.assertEqual((status['first_choice']['request'], status['first_choice']['phase']),
                         (request, 'unknown'), status)
        result = self.cli('desktop', 'registration', 'mode-result', '--json', '--request-id', request)
        self.assertEqual((result['outcome'], result['receipt']['phase']), ('unknown', 'unknown'), result)
        self.assertEqual(self.observe()['instances'], before)
        self.assertEqual(self.cli('desktop', 'status', '--json')['lifecycle']['pid'], service)
        self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), granted)
        cancelled = self.cancel(request)
        self.assertEqual(cancelled['cancelled_request_id'], request, cancelled)
        self.assertFalse(cancelled['manager_changed'], cancelled)
        self.assertEqual(self.observe()['instances'], before)

    TOAST_KEYS = (r'Software\Classes\AppUserModelId\com.hgdev.hgs-tray',
                  r'Software\Classes\CLSID\{0F0F70A2-1D40-47D8-AC5E-D6A9B789605F}',
                  r'Software\Classes\CLSID\{0F0F70A2-1D40-47D8-AC5E-D6A9B789605F}\LocalServer32')

    def toast_registry(self):
        """Every value of the three owned Toast keys, read-only."""
        import winreg
        values = {}
        for path in self.TOAST_KEYS:
            try:
                with winreg.OpenKey(winreg.HKEY_CURRENT_USER, path, 0, winreg.KEY_READ) as key:
                    index = 0
                    while True:
                        try:
                            name, value, kind = winreg.EnumValue(key, index)
                        except OSError:
                            break
                        values[(path, name)] = (value, kind)
                        index += 1
            except FileNotFoundError:
                values[(path, None)] = None
        return values

    def set_toast_command(self, command):
        import winreg
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, self.TOAST_KEYS[2], 0, winreg.KEY_SET_VALUE) as key:
            winreg.SetValueEx(key, '', 0, winreg.REG_SZ, command)

    def attention(self):
        status = self.cli('desktop', 'status', '--json')['lifecycle']['background']['attention']
        return dict(code=status.get('code'), ready=status.get('ready'),
                    error=(status.get('status') or {}).get('error'))

    def toast_registration_names_another_copy(self):
        """zerus-mgw2v: the explicit Toast registration, then ONE arranged
        change: LocalServer32 names a second copy of the CLI with exactly the
        expected arguments (the "second registered copy")."""
        self.assertFalse(self.cli('desktop', 'notification-registration', 'status', '--json')['registered'],
                         'the disposable runner must have no Toast registration yet')
        installed = self.cli('desktop', 'notification-registration', 'install', '--json')
        self.assertTrue(installed['registered'], installed)
        original = self.toast_registry()
        command = original[(self.TOAST_KEYS[2], '')][0]
        # Keep the product's exact arguments; only the quoted image changes.
        self.assertTrue(command.startswith('"'), command)
        image, arguments = command[1:].split('"', 1)
        self.assertEqual(Path(image).resolve(strict=True), self.cli_image)
        other = self.root / 'other copy ü' / 'hgs.exe'
        other.parent.mkdir()
        shutil.copy2(self.cli_image, other)
        self.set_toast_command(f'"{other}"' + arguments)
        return dict(original=original, command=command, arranged=self.toast_registry())

    def attention_refuses_another_copy_then_is_ready(self, toast):
        """The task-started background service sees the runner's logon HOME,
        not this fixture's private profile (the F1 mismatch). It selects its
        own image: the other copy is the closed diagnostic with the registry
        byte-identical, and the exact registration then makes attention ready."""
        observed, deadline = None, time.monotonic() + 60
        while time.monotonic() < deadline:
            observed = self.attention()
            if observed['error'] == 'notification_registration_other_image':
                break
            time.sleep(1)
        self.assertEqual(observed['error'], 'notification_registration_other_image', observed)
        self.assertFalse(observed['ready'], observed)
        self.assertEqual(self.toast_registry(), toast['arranged'], 'the service never repairs')
        self.set_toast_command(toast['command'])
        self.assertEqual(self.toast_registry(), toast['original'])
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            observed = self.attention()
            if observed['ready'] is True:
                break
            time.sleep(1)
        self.assertTrue(observed['ready'], observed)
        self.assertIsNone(observed['error'], observed)
        self.assertEqual(self.toast_registry(), toast['original'])

    def first_background_choice_from_plain_passive(self):
        """zerus-yfbd.1: the first explicit background choice, with the
        caller's retained UUID, through the actual installed-frontend caller."""
        path = self.state / 'desktop' / 'windows-registration-mode.json'
        status = self.cli('desktop', 'registration', 'status', '--json')
        self.assertIn('first_choice_unavailable', status)
        self.assertIsNone(status['first_choice_unavailable'], 'separate trees keep the anchor')
        self.assertIsNone(status['pending_transition'])
        self.assertIsNone(status['reviewed_transition'])
        self.assertFalse(status['activation_automatic_blocked'])
        granted = self.cli('desktop', 'ownership-status', '--json')
        before, record_before = self.observe()['instances'], path.read_bytes()
        # The closed form: a passive request or a malformed UUID never reaches
        # the mode helper.
        for desired, request in (('passive', str(uuid.uuid4())), ('background', 'not-a-uuid')):
            self.assertNotEqual(self.first_call(desired, request)['code'], 0)
        self.assertEqual(path.read_bytes(), record_before)
        self.assertEqual(self.observe()['instances'], before)
        request = str(uuid.uuid4())
        result = self.first_call('background', request)
        self.assertEqual(result['code'], 0, result)
        first = json.loads(result['stdout'])
        self.assertTrue(first['ok'], first)
        self.assertEqual((first['request_id'], first['running_mode']), (request, 'background'))
        current = self.cli('desktop', 'status', '--json')['lifecycle']
        receipt = self.cli('desktop', 'registration', 'mode-result', '--json',
                           '--request-id', request)['receipt']
        self.assertNotIn('reviewed', receipt)
        self.assertEqual((receipt['request'], receipt['desired'], receipt['phase']),
                         (request, 'background', 'settled'))
        self.assertEqual(receipt['scope'], dict(journal=granted['journal'], owner_epoch=granted['owner_epoch'],
                                               service_epoch=current['epoch'], generation=current['generation']))
        registration = self.cli('desktop', 'registration', 'status', '--json')
        self.assertEqual(registration['first_choice'], receipt)
        self.assertIsNone(registration['mode_choice'])
        # From background the form refuses before any effect.
        self.refused_first_choice(str(uuid.uuid4()), 'from a passive registration only')
        return first

    def lost_journal_refuses_every_mode_operation(self, first):
        """The independent first-choice anchor (config folder) outlives the
        mode journal (state folder): a missing or replaced journal is lost,
        never "never written"."""
        path = self.state / 'desktop' / 'windows-registration-mode.json'
        configuration = self.cli('desktop', 'status', '--json')['configuration']
        anchor = self.config / f'windows-first-choice-{configuration}.json'
        self.assertEqual(json.loads(anchor.read_bytes())['request'], first['request_id'])
        original = path.read_bytes()
        record = json.loads(original)
        empty = {key: record[key] for key in ('schema', 'configuration', 'sid', 'command')}
        empty.update(pending=None, cancelled=None, completed=None)
        for case in ('missing', 'replaced'):
            if case == 'missing':
                path.unlink()
            else:
                path.write_text(json.dumps(empty), encoding='utf-8')
            registration = self.cli('desktop', 'registration', 'status', '--json')
            self.assertEqual(registration['first_choice_lost'], first['request_id'], case)
            self.assertTrue(registration['activation_automatic_blocked'], case)
            self.assertIsNone(registration['first_choice'], case)
            before = self.observe()['instances']
            grant = self.cli('desktop', 'ownership-status', '--json')
            refused = self.mode('passive')
            self.assertFalse(refused['ok'], refused)
            self.assertIn('no longer shows first choice', refused['error'])
            again = self.first_call('background', str(uuid.uuid4()))
            self.assertEqual(again['code'], 0, again)
            self.assertFalse(json.loads(again['stdout'])['ok'], again)
            result = self.cli('desktop', 'registration', 'mode-result', '--json', '--request-id', first['request_id'])
            self.assertEqual((result['code'], result['outcome'], result['receipt']),
                             ('first_choice_lost', 'unknown', None), case)
            self.assertEqual(self.observe()['instances'], before, case)
            self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), grant, case)
            path.write_bytes(original)  # Restore the arranged journal for the rest of the round trip.
        self.assertIsNone(self.cli('desktop', 'registration', 'status', '--json')['first_choice_lost'])

    def pending_first_choice_refuses_the_old_frame(self, current):
        """A reserved FIRST choice whose helper died at Prepared or Held: the
        OLD no-request-ID switch never adopts or continues it."""
        path = self.state / 'desktop' / 'windows-registration-mode.json'
        configuration = self.cli('desktop', 'status', '--json')['configuration']
        anchor = self.config / f'windows-first-choice-{configuration}.json'
        for phase in ('prepared', 'held'):
            record = json.loads(path.read_text(encoding='utf-8'))
            request = str(uuid.uuid4())
            # Arrange lost reply metadata only, around the actual running
            # passive instance; no synthetic native authentication. As the
            # helper does, the anchor names the new request before the record
            # reserves it; otherwise this is a lost journal, not a pending one.
            published = json.loads(anchor.read_bytes())
            published['request'] = request
            anchor.write_bytes(json.dumps(published).encode('utf-8'))
            record['pending'] = dict(request_id=request, desired='background', phase=phase,
                                     original=current, started=None)
            record.setdefault('first_choices', []).append(
                dict(request=request, desired='background', phase='unknown'))
            path.write_text(json.dumps(record), encoding='utf-8')
            self.assertIsNone(self.cli('desktop', 'registration', 'status', '--json')['first_choice_lost'])
            arranged = path.read_bytes()
            before = self.observe()['instances']
            grant = self.cli('desktop', 'ownership-status', '--json')
            service = self.cli('desktop', 'status', '--json')['lifecycle']['pid']
            refused = self.mode('background')
            self.assertFalse(refused['ok'], refused)
            self.assertIn('first mode choice is pending', refused['error'])
            # Zero Stop/Update/RunEx, ownership or record change; no new ID.
            self.assertEqual(path.read_bytes(), arranged)
            self.assertEqual(self.observe()['instances'], before)
            self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), grant)
            self.assertEqual(self.cli('desktop', 'status', '--json')['lifecycle']['pid'], service)
            result = self.cli('desktop', 'registration', 'mode-result', '--json', '--request-id', request)
            self.assertEqual((result['outcome'], result['receipt']['phase']), ('unknown', 'unknown'), result)
            # Closed only through the supported pre-manager cancellation.
            cancelled = self.cancel(request)
            self.assertEqual(cancelled['cancelled_request_id'], request, cancelled)
            self.assertFalse(cancelled['manager_changed'], cancelled)
            self.assertEqual(self.observe()['instances'], before)
            result = self.cli('desktop', 'registration', 'mode-result', '--json', '--request-id', request)
            self.assertEqual(result['receipt']['phase'], 'refused', result)
        self.assertIsNone(self.cli('desktop', 'registration', 'status', '--json')['first_choice_lost'])

    def review_old_stop_without_adopting_the_later_passive(self, original, current):
        path = self.state / 'desktop' / 'windows-registration-mode.json'
        record = json.loads(path.read_text(encoding='utf-8'))
        request = str(uuid.uuid4())
        # Arrange lost Stop metadata only. Both identities came from actual
        # owned RunEx/readiness above; no synthetic native authentication.
        record['pending'] = dict(request_id=request, desired='background', phase='stop_offered',
                                 original=original, started=None)
        path.write_text(json.dumps(record), encoding='utf-8')
        grant = self.cli('desktop', 'ownership-status', '--json')
        before = self.observe()['instances']
        # Historical StopOffered cannot be inferred to mean 'no Stop' or
        # downgraded to Held by this ordering fix.
        saved = path.read_bytes()
        refused = self.cancel(request)
        self.assertFalse(refused['ok'], refused)
        self.assertIn('recorded original start', refused['error'])
        self.assertEqual(path.read_bytes(), saved)
        self.assertEqual(self.observe()['instances'], before)
        self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), grant)
        reply = self.review_mode(request)
        self.assertEqual(reply['code'], 'reviewed_unknown', reply)
        self.assertEqual(reply['outcome'], 'unknown', reply)
        self.assertFalse(reply['manager_changed'])
        self.assertFalse(reply['authority_changed'])
        self.assertTrue(reply['activation_automatic_blocked'])
        self.assertEqual(self.observe()['instances'], before)
        self.assertEqual(self.cli('desktop', 'status', '--json')['lifecycle']['pid'], current['pid'])
        self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), grant)
        archived = json.loads(path.read_bytes())['reviewed'][-1]
        self.assertEqual(archived['original']['request_id'], request)
        self.assertEqual(archived['original']['original'], original)
        self.assertEqual(archived['original']['phase'], 'stop_offered')
        self.assertEqual(archived['outcome'], 'unknown')
        # Review history closes the first-choice form, even with a passive
        # registration; the reviewed route is the only choice left.
        self.refused_first_choice(str(uuid.uuid4()), 'Review it')
        # A fresh explicit Rearm is a separate fixture action, not a capability
        # obtained from Review. With a valid Grant, an OLD automatic switch
        # frame must still refuse specifically at the durable review latch.
        self.grant('ownership_rearm')
        granted = self.cli('desktop', 'ownership-status', '--json')
        refused = self.mode('background')
        self.assertFalse(refused['ok'], refused)
        self.assertIn('--after-review', refused['error'])
        self.assertEqual(self.observe()['instances'], before)
        self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), granted)
        background = self.choice_after_review('background', request)
        self.latest = background['instance']
        # Native original-ID receipt: the post-review choice records the SAME
        # retained actual Grant and authenticated ready endpoint, not readiness
        # inferred later from a different task instance.
        receipt = self.cli('desktop', 'registration', 'mode-result', '--json',
                           '--request-id', background['request_id'])['receipt']
        current = self.cli('desktop', 'status', '--json')['lifecycle']
        self.assertEqual((receipt['request'], receipt['reviewed'], receipt['desired'], receipt['phase']),
                         (background['request_id'], request, 'background', 'settled'))
        self.assertEqual(receipt['scope'], dict(journal=granted['journal'], owner_epoch=granted['owner_epoch'],
                                               service_epoch=current['epoch'], generation=current['generation']))
        registration = self.cli('desktop', 'registration', 'status', '--json')
        self.assertEqual(registration['mode_choice'], receipt)
        self.assertEqual((registration['service_epoch'],registration['service_generation']),
                         (current['epoch'],current['generation']))
        passive = self.choice_after_review('passive', request)
        self.latest = passive['instance']
        # A completed request remains read-only after a later choice overwrites
        # completed_request_id. Re-offering it must not start a new transition.
        before = self.observe()['instances']
        record_before = path.read_bytes()
        result = self.cli('desktop', 'registration', 'mode-result', '--json',
                          '--request-id', background['request_id'])
        self.assertTrue(result['read_only'], result)
        self.assertEqual(result['outcome'], 'settled', result)
        self.assertEqual(result['receipt']['reviewed'], request)
        self.assertEqual(result['receipt']['desired'], 'background')
        repeated = self.frontend_call(dict(case='mode_switch', desired='background',
                                           after_review=request, request_id=background['request_id']),
                                      'MODE_CALLER_RESULT ')
        self.assertEqual(repeated['code'], 0, repeated)
        self.assertFalse(json.loads(repeated['stdout'])['ok'], repeated)
        self.assertEqual(path.read_bytes(), record_before)
        self.assertEqual(self.observe()['instances'], before)

    def live_original_review_requires_a_barrier_and_never_stops_it(self, current):
        path = self.state / 'desktop' / 'windows-registration-mode.json'
        record = json.loads(path.read_text(encoding='utf-8'))
        request = str(uuid.uuid4())
        record['pending'] = dict(request_id=request, desired='background', phase='stop_offered',
                                 original=current, started=None)
        path.write_text(json.dumps(record), encoding='utf-8')
        grant = self.cli('desktop', 'ownership-status', '--json')
        before = self.observe()['instances']
        reply = self.review_mode(request)
        self.assertEqual(reply['code'], 'reboot_required', reply)
        self.assertEqual(reply['outcome'], 'unknown', reply)
        self.assertFalse(reply['manager_changed'])
        self.assertFalse(reply['authority_changed'])
        self.assertEqual(self.observe()['instances'], before)
        self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), grant)
        barrier = path.read_bytes()
        refused = self.review_mode(request)
        self.assertFalse(refused['ok'], refused)
        self.assertEqual(path.read_bytes(), barrier)
        self.assertEqual(self.cli('desktop', 'status', '--json')['lifecycle']['pid'], current['pid'])
        # The fixture's existing exact-original stop qualifies physical absence;
        # Review itself has no stop capability. Never reset the pending record.
        self.assertTrue(self.stop_exact_passive(current)['stopped'])
        finished = self.review_mode(request)
        self.assertEqual(finished['code'], 'reviewed_unknown', finished)
        self.assertEqual(finished['outcome'], 'unknown')
        self.assertFalse(finished['manager_changed'])
        self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), grant)

    def recover_initial_start_and_cancel_background_before_a_grant(self, passive):
        path = self.state / 'desktop' / 'windows-registration-mode.json'
        record = json.loads(path.read_text(encoding='utf-8'))
        # Arrange an actual successful start whose reply was lost. The SAME
        # native instance must settle through the helper; no file reset follows.
        request_id = passive['request_id']
        self.assertEqual(record['completed'], request_id)
        before = self.observe()
        record['completed'] = None  # original completion reply/save was lost
        record['pending'] = dict(request_id=request_id, desired='passive', phase='start_offered',
                                 original=None, started=passive['instance']['guid'])
        path.write_text(json.dumps(record), encoding='utf-8')
        settled = self.mode('passive')
        self.assertTrue(settled['ok'], settled)
        self.assertEqual(settled['code'], 'original_start_observed')
        self.assertEqual(settled['request_id'], request_id)
        self.assertEqual(settled['instance'], passive['instance'])
        self.assertFalse(settled['manager_changed'])
        self.assertEqual(self.observe(), before, 'same GUID/process/start time; no manager replay')
        self.assertIsNone(json.loads(path.read_text(encoding='utf-8'))['pending'])
        self.assertEqual(self.cli('desktop', 'ownership-status', '--json')['state'], 'passive')
        # A reboot/restart may invalidate the original tuple before any manager
        # effect. Cancellation of either background phase requires no grant.
        for phase in ('prepared', 'held'):
            record = json.loads(path.read_text(encoding='utf-8'))
            identity = dict(passive['instance'], birth=passive['instance']['birth'] + 1)
            request_id = str(uuid.uuid4())
            record['pending'] = dict(request_id=request_id, desired='background', phase=phase,
                                     original=identity, started=None)
            path.write_text(json.dumps(record), encoding='utf-8')
            receipt = self.cancel(request_id)
            self.assertTrue(receipt['ok'], receipt)
            self.assertFalse(receipt['manager_changed'])
            self.assertEqual(receipt['cancelled_request_id'], request_id)
        self.assertEqual(self.cli('desktop', 'status', '--json')['lifecycle']['pid'], passive['instance']['pid'])
        return settled

    def metadata_refusals(self, passive):
        # Only fixture metadata is arranged, never native custody. Every wrong
        # original refuses; supported record-only cancellation replaces resets.
        path = self.state / 'desktop' / 'windows-registration-mode.json'
        pid = passive['instance']['pid']
        for field in ('guid', 'pid', 'birth', 'epoch'):
            record = json.loads(path.read_text(encoding='utf-8'))
            identity = dict(passive['instance'])
            identity[field] = str(uuid.uuid4()) if field in ('guid', 'epoch') else identity[field] + 1
            pending = dict(request_id=str(uuid.uuid4()), desired='passive', phase='prepared',
                           original=identity, started=None)
            record['pending'] = pending
            path.write_text(json.dumps(record), encoding='utf-8')
            reply = self.mode('passive')
            self.assertFalse(reply['ok'], reply)
            self.assertIn('Original task or service changed', reply['error'])
            self.assertEqual(reply['pending_transition']['request_id'], pending['request_id'])
            receipt = self.cancel(pending['request_id'])
            self.assertTrue(receipt['ok'], receipt)
            self.assertFalse(receipt['manager_changed'])
            self.assertEqual(self.cli('desktop', 'status', '--json')['lifecycle']['pid'], pid)
        with closing(sqlite3.connect((self.state / 'desktop' / 'state.sqlite3').as_uri() + '?mode=ro', uri=True)) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM desktop_commands').fetchone()[0], 0)

    def busy_exclusion_does_not_reserve(self, passive):
        import msvcrt
        path = self.state / 'desktop' / 'windows-registration-mode.json'
        before = path.read_bytes()
        # The fixture holds byte zero of the SAME private handoff file; fs2's
        # exclusive LockFileEx range includes it. No actor/manager is stopped.
        with (self.state / 'desktop' / 'handoff.lock').open('r+b') as file:
            file.seek(0)
            msvcrt.locking(file.fileno(), msvcrt.LK_NBLCK, 1)
            try:
                reply = self.mode('background')
                self.assertFalse(reply['ok'], reply)
                self.assertIsNone(reply['pending_transition'])
                self.assertEqual(path.read_bytes(), before)
            finally:
                file.seek(0)
                msvcrt.locking(file.fileno(), msvcrt.LK_UNLCK, 1)
        self.assertEqual(self.cli('desktop', 'status', '--json')['lifecycle']['pid'], passive['instance']['pid'])

    def cancel_unsent_private_rollback(self, passive):
        record_path = self.state / 'desktop' / 'windows-registration-mode.json'
        record = json.loads(record_path.read_text(encoding='utf-8'))
        request_id = str(uuid.uuid4())
        record['pending'] = dict(request_id=request_id, desired='passive', phase='revoke_offered',
                                 original=passive['instance'], started=None)
        record_path.write_text(json.dumps(record), encoding='utf-8')
        before = self.cli('desktop', 'ownership-status', '--json')
        result = self.frontend_call(dict(case='mode_cancel', request_id=request_id), 'MODE_CALLER_RESULT ')
        self.assertEqual(result['code'], 0, result)
        receipt = json.loads(result['stdout'])
        self.assertTrue(receipt['ok'], receipt)
        self.assertFalse(receipt['manager_changed'])
        self.assertEqual(receipt['cancelled_request_id'], request_id)
        self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), before)
        self.assertEqual(self.cli('desktop', 'status', '--json')['lifecycle']['pid'], passive['instance']['pid'])
        with closing(sqlite3.connect((self.state / 'desktop' / 'state.sqlite3').as_uri() + '?mode=ro', uri=True)) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM desktop_commands').fetchone()[0], 0)
        return request_id

    def cancel_ready_rollback_after_authority_moves(self, background, rollback, *, later_revoke=False):
        path = self.state / 'desktop' / 'windows-registration-mode.json'
        record = json.loads(path.read_text(encoding='utf-8'))
        # The actual passive RunEx completed, but simulate an overrun before
        # its reply/completed metadata save. Keep the REAL rollback receipt,
        # UUID and started GUID. No manual reset follows: the supported cancel
        # must resolve this record after authority has moved on.
        record['completed'] = background['request_id']
        record['pending'] = dict(request_id=rollback['request_id'], desired='passive',
                                 phase='ready' if later_revoke else 'start_offered', original=background['instance'],
                                 started=rollback['instance']['guid'])
        path.write_text(json.dumps(record), encoding='utf-8')
        if later_revoke:
            # A separately explicit fixture revoke supersedes the old receipt.
            # Cancellation itself must neither send it nor require a new grant.
            receipt = self.cli('desktop', 'revoke', '--json', '--request-id',
                               str(uuid.uuid4()), '--reason', 'rollback')
            self.assertTrue(receipt['accepted'], receipt)
            self.assertTrue(receipt['durable'], receipt)
            self.assertTrue(receipt['checkpointed'], receipt)
        else:
            self.grant('ownership_rearm')
        ownership = self.cli('desktop', 'ownership-status', '--json')
        self.assertEqual(ownership['state'], 'revoked' if later_revoke else 'service')

        def journal():
            with closing(sqlite3.connect((self.state / 'desktop' / 'state.sqlite3').as_uri()
                                         + '?mode=ro', uri=True)) as db:
                receipt = db.execute('SELECT * FROM desktop_commands WHERE request_id=?',
                                     (rollback['request_id'],)).fetchone()
                self.assertIsNotNone(receipt)
                count = db.execute('SELECT count(*) FROM desktop_commands').fetchone()[0]
                return receipt, count

        original_receipt = journal()
        before = self.observe()
        denied = self.mode('background')
        self.assertFalse(denied['ok'], denied)
        self.assertEqual(denied['unknown']['code'], 'different_explicit_choice')
        settlement = self.mode('passive')
        self.assertFalse(settlement['ok'], settlement)
        self.assertEqual(settlement['unknown']['code'], 'original_completion_unverified')
        pending = path.read_bytes()
        wrong = self.cancel(str(uuid.uuid4()))
        self.assertFalse(wrong['ok'], wrong)
        self.assertEqual(path.read_bytes(), pending)
        cancelled = self.cancel(rollback['request_id'])
        self.assertTrue(cancelled['ok'], cancelled)
        self.assertEqual(cancelled['code'], 'observed_start_cancelled')
        self.assertTrue(cancelled['metadata_changed'])
        self.assertFalse(cancelled['manager_changed'])
        self.assertFalse(cancelled['authority_changed'])
        self.assertEqual(cancelled['observed_instance'], rollback['instance'])
        self.assertEqual(cancelled['cancelled_request_id'], rollback['request_id'])
        saved = json.loads(path.read_text(encoding='utf-8'))
        self.assertIsNone(saved['pending'])
        self.assertEqual(saved['cancelled'], rollback['request_id'])
        self.assertEqual(saved['completed'], background['request_id'])
        self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), ownership)
        self.assertEqual(journal(), original_receipt)
        self.assertEqual(self.observe(), before)
        self.assertEqual(self.cli('desktop', 'status', '--json')['lifecycle']['pid'], rollback['instance']['pid'])

    def powershell(self, script, data):
        result = subprocess.run(self.powershell_command(script, data), env=self.env, capture_output=True,
                                text=True, encoding='utf-8', timeout=12)
        self.assertEqual(result.returncode, 0,
                         'fixture PowerShell stdout: ' + result.stdout[-8192:]
                         + '\nstderr: ' + result.stderr[-1024:])
        self.assertLessEqual(len(result.stdout.encode('utf-8')), 8192)
        return json.loads(result.stdout)

    def powershell_command(self, script, data):
        # Fixture-only native COM observation/cleanup. No task-name-wide Stop,
        # schtasks /End, PID kill or executable-pattern cleanup is used.
        request = self.root / ('sdk-' + str(uuid.uuid4()) + '.json')
        request.write_text(json.dumps(data), encoding='utf-8')
        escaped = str(request).replace("'", "''")
        lookup = r'''
function Get-OwnInstances($scheduler,$registered) {
 $path=[string]$registered.Path;
 if($path-cne ('\'+[string]$x.task)) {throw 'fixture registered task path changed'};
 $clock=[Diagnostics.Stopwatch]::StartNew();$all=$scheduler.GetRunningTasks(1);$count=$all.Count;
 if($count-lt 0 -or $count-gt 256) {throw 'fixture running inventory exceeds its bound'};
 $items=@();for($index=1;$index-le $count;$index++) {
  if($clock.ElapsedMilliseconds-ge 2000) {throw 'fixture original scan deadline expired'};
  $candidate=$all.Item($index);$candidatePath=[string]$candidate.Path;
  if([string]::IsNullOrEmpty($candidatePath)-or $candidatePath.Length-gt 32768) {throw 'fixture task path unavailable'};
  if($candidatePath-ceq $path) {
   $items+=@($candidate);if($items.Count-gt 1) {throw 'fixture has multiple exact task instances'}
  }
 };
 if($clock.ElapsedMilliseconds-ge 2000) {throw 'fixture original scan deadline expired'};
 return $items
};
'''
        # Keep the actual failure out of PowerShell's CLIXML error/progress
        # stream, which can hide it behind first-use module messages in CI.
        body = ("$ErrorActionPreference='Stop';$ProgressPreference='SilentlyContinue';"
                "[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);try {"
                "$x=Get-Content -Raw -LiteralPath '" + escaped + "'|ConvertFrom-Json;" + lookup + script + r'''
} catch {
 $record=$_;$exception=$record.Exception;
 for($depth=0;$depth-lt 4 -and $null-ne $exception.InnerException;$depth++) {$exception=$exception.InnerException};
 $message=[string]$exception.Message;if($message.Length-gt 1024) {$message=$message.Substring(0,1024)};
 $id=[string]$record.FullyQualifiedErrorId;if($id.Length-gt 256) {$id=$id.Substring(0,256)};
 @{fixture_error=@{type=$exception.GetType().FullName;message=$message;hresult=[int]$exception.HResult;
    id=$id;line=[int]$record.InvocationInfo.ScriptLineNumber}}|ConvertTo-Json -Compress -Depth 3;
 exit 1
}
''')
        return [str(Path(os.environ['SystemRoot']) / 'System32/WindowsPowerShell/v1.0/powershell.exe'),
                '-NoLogo', '-NoProfile', '-NonInteractive', '-EncodedCommand',
                base64.b64encode(body.encode('utf-16le')).decode('ascii')]

    def observe(self):
        return self.powershell(r'''
$s=New-Object -ComObject Schedule.Service;$s.Connect();$t=$s.GetFolder('\').GetTask($x.task);
$items=@();foreach($i in @(Get-OwnInstances $s $t)) {
 $p=$null;try {$p=[Diagnostics.Process]::GetProcessById($i.EnginePID).ProcessName} catch {}
 $items+=@{guid=$i.InstanceGuid;engine_pid=$i.EnginePID;engine_image=$p;state=[int]$i.State}
};@{instances=$items;registered_instances_count=$t.GetInstances(0).Count;hidden=[bool]$t.Definition.Settings.Hidden;state=[int]$t.State;last_task_result=[int64]$t.LastTaskResult;
last_run_utc=$t.LastRunTime.ToUniversalTime().ToString('o')}|ConvertTo-Json -Compress -Depth 4
''', dict(task=self.task))

    def task_history(self, started):
        # Read only this fixture's task and retain events for the recorded
        # original GUID. A disabled/unavailable history is explicit data,
        # never permission to rerun a task or infer its process has drained.
        return self.powershell(r'''
$task='\'+$x.task;
$xpath="*[EventData[Data[@Name='TaskName']='"+$task+"']]";
try {$events=@(Get-WinEvent -LogName 'Microsoft-Windows-TaskScheduler/Operational' -FilterXPath $xpath -MaxEvents 32 -ErrorAction Stop)}
catch {@{available=$false;error=$_.Exception.GetType().Name;events=@()}|ConvertTo-Json -Compress;exit 0};
$items=@();foreach($e in $events) {
 $xml=[xml]$e.ToXml();$data=@{};foreach($d in $xml.Event.EventData.Data) {
  if($d.Name-in @('InstanceId','TaskInstanceId','ResultCode','ReturnCode','ErrorCode','ErrorValue','ProcessID','ProcessId','EnginePID')) {
   $text=[string]$d.'#text';if($text.Length-gt 64) {$text=$text.Substring(0,64)};$data[[string]$d.Name]=$text
  }
 };
 $matches=$false;foreach($key in @('InstanceId','TaskInstanceId')) {
  try {if([Guid]$data[$key]-eq [Guid]$x.started) {$matches=$true}} catch {}
 };
 if($matches) {
  $items+=@{id=[int]$e.Id;record=[int64]$e.RecordId;data=$data};if($items.Count-ge 8) {break}
 }
};@{available=$true;events=$items}|ConvertTo-Json -Compress -Depth 5
''', dict(task=self.task, started=started))

    def startup_security(self):
        # Native metadata of four fixed fixture-owned entries only. This
        # explains a refused privacy check; it is never an admission result.
        return self.powershell(r'''
$items=@();foreach($p in $x.paths) {
 try {
  $acl=Get-Acl -LiteralPath $p.path -ErrorAction Stop;
  $rules=@($acl.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier]));
  $entries=@();foreach($r in ($rules|Select-Object -First 8)) {
   $sid=[string]$r.IdentityReference.Value;if($sid.Length-gt 64) {$sid=$sid.Substring(0,64)};
   $entries+=@{sid=$sid;type=[string]$r.AccessControlType;
     propagation=[string]$r.PropagationFlags;inherited=[bool]$r.IsInherited}
  };
  $items+=@{name=$p.name;owner=$acl.GetOwner([Security.Principal.SecurityIdentifier]).Value;
    protected=[bool]$acl.AreAccessRulesProtected;count=$rules.Count;entries=$entries}
 } catch {$items+=@{name=$p.name;error=$_.Exception.GetType().Name;code=[int]$_.Exception.HResult}}
};@{objects=$items}|ConvertTo-Json -Compress -Depth 5
''', dict(paths=[dict(name=name, path=str(path)) for name, path in (
            ('state', self.state), ('desktop', self.state / 'desktop'),
            ('owner', self.state / 'desktop' / 'owner.lock'),
            ('diagnostic', self.state / 'desktop' / 'startup-failure.json'))]))

    def assert_exact_user_created_state(self):
        # Check objects actually created by the scheduled service and SQLite,
        # rather than inferring ownership from a successful manager reply.
        observed = self.powershell(r'''
$items=@();foreach($p in $x.paths) {
 $acl=Get-Acl -LiteralPath $p.path -ErrorAction Stop;
 $items+=@{name=$p.name;owner=$acl.GetOwner([Security.Principal.SecurityIdentifier]).Value}
};@{objects=$items}|ConvertTo-Json -Compress -Depth 3
''', dict(paths=[dict(name=name, path=str(self.state / 'desktop' / name))
                  for name in ('owner.lock', 'state.sqlite3', 'state.sqlite3-wal', 'state.sqlite3-shm')]))
        self.assertEqual(len(observed['objects']), 4, observed)
        for item in observed['objects']:
            self.assertEqual(item['owner'], self.sid, item)

    def stop_exact_passive(self, expected, *, wrong_birth=False):
        # The maintained plan carries the exact registered (possibly verbatim)
        # path. Python's resolved process image can have different spelling.
        plan = self.cli('desktop', 'registration', '--json')
        task_image = plan['argv'][0]
        self.assertEqual(Path(task_image).resolve(strict=True), self.cli_image)
        return self.powershell(r'''
$s=New-Object -ComObject Schedule.Service;$s.Connect();$t=$s.GetFolder('\').GetTask($x.task);
$d=$t.Definition;
if($d.Actions.Count-ne 1) {throw 'fixture task action_count changed'};
if($d.Actions.Item(1).Type-ne 0 -or $d.Actions.Item(1).Path-cne $x.task_image) {throw 'fixture task action_path changed'};
$user=[string]$d.Principal.UserId;
if([string]::IsNullOrEmpty($user)-or $user.Length-gt 256) {throw 'fixture task principal unavailable'};
# As in the native adapter, compare the actual SID, not its account-name spelling.
if($user.StartsWith('S-1-',[StringComparison]::Ordinal)) {
 $sid=([Security.Principal.SecurityIdentifier]::new($user)).Value
} else {$sid=([Security.Principal.NTAccount]::new($user)).Translate([Security.Principal.SecurityIdentifier]).Value};
if($sid-cne $x.sid) {throw 'fixture task principal_sid changed'};
$values=@(Get-OwnInstances $s $t);if($values.Count-ne 1) {throw 'fixture task instance count changed'};
$i=$values[0];$i.Refresh();$guid=$i.InstanceGuid;if($guid-cne $x.guid -or $i.EnginePID-ne $x.pid) {throw 'fixture original instance changed'};
$p=[Diagnostics.Process]::GetProcessById($i.EnginePID);$handle=$p.Handle;
if($p.HasExited) {@{stopped=$false;reason='original_exited'}|ConvertTo-Json -Compress;exit 0};
if($p.StartTime.ToFileTimeUtc()-ne [int64]$x.birth) {
 @{stopped=$false;reason='original_birth_changed'}|ConvertTo-Json -Compress;exit 0
};
if($p.MainModule.FileName-cne $x.image) {
 @{stopped=$false;reason='original_image_changed'}|ConvertTo-Json -Compress;exit 0
};
$i.Refresh();if($i.InstanceGuid-cne $x.guid -or $i.EnginePID-ne $p.Id) {throw 'fixture instance changed before stop'};
$i.Stop();if(-not $p.WaitForExit(5000)) {throw 'fixture original process exit unconfirmed'};
if(@(Get-OwnInstances $s $t).Count-ne 0) {throw 'fixture instance remains'};
@{stopped=$true;original_pid=$p.Id;original_guid=$guid}|ConvertTo-Json -Compress
''', dict(task=self.task, task_image=task_image, image=str(self.cli_image), sid=self.sid, guid=expected['guid'],
          pid=expected['pid'], birth=expected['birth'] + (1 if wrong_birth else 0)))

    def armed_instance_restart(self, request_id, original):
        """A COM worker armed BEFORE the call: it pins the exact original
        instance, waits for the helper's own held marker, stops exactly that
        instance, starts the task once, proves the replacement engine (GUID,
        PID, native birth, selected image) and only then releases the hold,
        all within the Stop deadline the hold consumes. Never retried."""
        plan = self.cli('desktop', 'registration', '--json')
        armed = self.root / f'armed-{request_id}'
        worker = subprocess.Popen(self.powershell_command(r"""
$s=New-Object -ComObject Schedule.Service;$s.Connect();$t=$s.GetFolder('\').GetTask($x.task);
$d=$t.Definition;
if($d.Actions.Count-ne 1 -or $d.Actions.Item(1).Path-cne $x.task_image) {throw 'fixture task action changed'};
$values=@(Get-OwnInstances $s $t);if($values.Count-ne 1) {throw 'fixture task instance count changed'};
$i=$values[0];$i.Refresh();if($i.InstanceGuid-cne $x.guid -or $i.EnginePID-ne $x.pid) {throw 'fixture original instance changed'};
$p=[Diagnostics.Process]::GetProcessById($i.EnginePID);$handle=$p.Handle;
if($p.HasExited -or $p.StartTime.ToFileTimeUtc()-ne [int64]$x.birth -or $p.MainModule.FileName-cne $x.image) {throw 'fixture original identity changed'};
[IO.File]::WriteAllText($x.armed,'armed');
$clock=[Diagnostics.Stopwatch]::StartNew();
while(-not [IO.File]::Exists($x.marker)) {if($clock.ElapsedMilliseconds-ge 60000) {throw 'held marker never appeared'};Start-Sleep -Milliseconds 5};
$seen=$clock.ElapsedMilliseconds;
$i.Refresh();if($i.InstanceGuid-cne $x.guid -or $i.EnginePID-ne $p.Id) {throw 'fixture instance changed before stop'};
$i.Stop();if(-not $p.WaitForExit(5000)) {throw 'fixture original process exit unconfirmed'};
if(@(Get-OwnInstances $s $t).Count-ne 0) {throw 'fixture instance remains'};
$r=$t.Run($null);$guid=[string]$r.InstanceGuid;
# The replacement engine, proven BEFORE the release and inside the Stop
# budget the hold consumes; otherwise the arrangement fails unreleased.
$limit=$clock.ElapsedMilliseconds+1000;$q=$null;
while($null-eq $q) {
 if($clock.ElapsedMilliseconds-ge $limit) {throw 'replacement engine not proven within the Stop budget'};
 $values=@(Get-OwnInstances $s $t);
 if($values.Count-eq 1 -and [string]$values[0].InstanceGuid-ceq $guid -and $values[0].EnginePID-gt 0) {
  $q=[Diagnostics.Process]::GetProcessById($values[0].EnginePID)
 } else {Start-Sleep -Milliseconds 10}
};
$qh=$q.Handle;
if($q.HasExited -or $q.MainModule.FileName-cne $x.image) {throw 'replacement engine is not the selected CLI'};
$later=@{guid=$guid;pid=$q.Id;birth=$q.StartTime.ToFileTimeUtc()};
[IO.File]::WriteAllText($x.release,$x.request,[Text.ASCIIEncoding]::new());
@{stopped=$true;later=$later;restart_ms=$clock.ElapsedMilliseconds-$seen}|ConvertTo-Json -Compress
""", dict(task=self.task, task_image=plan['argv'][0], guid=original['guid'], pid=original['pid'],
          birth=original['birth'], image=str(self.cli_image), armed=str(armed),
          marker=str(self.config / f'held-pause-{request_id}.json'),
          release=str(self.config / f'held-release-{request_id}'), request=request_id)),
            env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding='utf-8',
            creationflags=subprocess.CREATE_NO_WINDOW)
        deadline = time.monotonic() + 30
        while not armed.exists() and worker.poll() is None and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertTrue(armed.exists(), worker.communicate(timeout=30) if worker.poll() is not None else 'not armed')
        return worker

    def armed_instance_restarted(self, worker):
        out, err = worker.communicate(timeout=60)
        self.assertEqual(worker.returncode, 0, 'fixture PowerShell stdout: ' + out[-8192:] + '\nstderr: ' + err[-1024:])
        return json.loads(out)

    def exact_later_instance(self, guid, wait=8):
        """Exactly one running instance of the fixture task, the given GUID, on
        a live engine of the selected CLI image. Unobservable fails the
        control: an empty or different set is never success."""
        deadline, instances = time.monotonic() + wait, []
        while True:
            instances = self.observe()['instances']
            if (len(instances) == 1 and instances[0]['guid'] == guid) or time.monotonic() >= deadline:
                break
            time.sleep(0.25)
        self.assertEqual([item['guid'] for item in instances], [guid], instances)
        metadata = windows_process_metadata(instances[0]['engine_pid'])
        self.assertFalse(metadata.get('exited', True), metadata)
        self.assertEqual(Path(metadata['image']).resolve(strict=True), self.cli_image)
        return dict(guid=guid, pid=instances[0]['engine_pid'], birth=metadata['birth'])

    @contextmanager
    def partial_original_pipe(self, original, configuration):
        """One retained partial client of the exact old service. It is a real
        parser/connection loan, never a manager or operation capability."""
        import ctypes as c
        from ctypes import wintypes as w
        dll = c.WinDLL('kernel32', use_last_error=True)
        dll.CreateFileW.argtypes = (w.LPCWSTR, w.DWORD, w.DWORD, c.c_void_p, w.DWORD, w.DWORD, w.HANDLE)
        dll.CreateFileW.restype = w.HANDLE
        dll.WaitNamedPipeW.argtypes = (w.LPCWSTR, w.DWORD)
        dll.GetNamedPipeServerProcessId.argtypes = (w.HANDLE, c.POINTER(w.DWORD))
        dll.WriteFile.argtypes = (w.HANDLE, c.c_void_p, w.DWORD, c.POINTER(w.DWORD), c.c_void_p)
        dll.CloseHandle.argtypes = (w.HANDLE,)
        name = r'\\.\pipe\hgs-desktop-' + self.sid + '-' + configuration
        self.assertTrue(dll.WaitNamedPipeW(name, 5000), c.get_last_error())
        handle = dll.CreateFileW(name, 0xc0000000, 0, None, 3, 0x00110000, None)
        self.assertNotEqual(handle, c.c_void_p(-1).value, c.get_last_error())
        stopping = threading.Event()
        errors = []
        def write():
            byte = c.create_string_buffer(b' ')
            while not stopping.is_set():
                count = w.DWORD()
                if not dll.WriteFile(handle, byte, 1, c.byref(count), None) or count.value != 1:
                    errors.append(c.get_last_error())
                    return
                stopping.wait(0.1)
        worker = None
        try:
            pid = w.DWORD()
            self.assertTrue(dll.GetNamedPipeServerProcessId(handle, c.byref(pid)), c.get_last_error())
            self.assertEqual(pid.value, original['pid'])
            worker = threading.Thread(target=write)
            worker.start()
            yield
            self.assertEqual(errors, [], 'original partial connection disappeared before its release')
        finally:
            stopping.set()
            if worker is not None:
                worker.join(timeout=5)
                self.assertFalse(worker.is_alive(), 'original partial writer cleanup unknown; original handle retained')
            dll.CloseHandle(handle)

    def refresh_call(self, case, request=None, target=None):
        control = dict(case=case)
        if request is not None:
            control['request_id'] = request
        if target is not None:
            control['target'] = target
        started = time.monotonic_ns()
        utc_ms = time.time_ns() // 1_000_000
        result = self.frontend_call(control, 'MODE_CALLER_RESULT ')
        self.assertEqual(result['code'], 0, result)
        reply = json.loads(result['stdout'])
        if case == 'mode_refresh':
            print('REFRESH_CALL_TIMING ' + json.dumps(dict(
                request=request, utc_ms=utc_ms, elapsed_ms=(time.monotonic_ns() - started) // 1_000_000,
                outcome=reply.get('outcome'), phase=reply.get('pending_transition', {}).get('phase')
                    if isinstance(reply.get('pending_transition'), dict) else None,
                timing=reply.get('timing'), original_exchange_error=reply.get('original_exchange_error'))), flush=True)
        return reply

    def refresh_info_drained(self):
        # Observe the required EOF/Permit settlement, including prior status
        # observers; a helper exit alone does not synchronize the remote actor.
        deadline = time.monotonic() + 10
        while True:
            info = self.refresh_call('mode_refresh_info')
            self.assertTrue(info['ok'], info)
            if info['drained']:
                return info
            self.assertLess(time.monotonic(), deadline, info)
            time.sleep(0.05)

    def refresh_started_at_held(self, request, target):
        self.control.write_text(json.dumps(dict(case='mode_refresh', request_id=request, target=target,
                                                cli=str(self.cli_image))), encoding='utf-8')
        return subprocess.Popen([str(self.frontend), '--exact',
                                 'instance::tests::evidence::native_frontend_request_fixture', '--nocapture'],
                                env=dict(self.env, HGS_FRONTEND_TEST_CONTROL=str(self.control),
                                         HGS_TEST_MODE_HELD_PAUSE=request),
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                creationflags=subprocess.CREATE_NO_WINDOW)

    def replace_private_cli_and_wait_for_retirement(self, suffix):
        # Installer-equivalent replacement of ONLY this private stable CLI.
        # Same bytes, different object; no process/image adoption by pathname.
        replacement = self.cli_image.with_name('next-private-cli.exe')
        shutil.copy2(self.cli_image, replacement)
        aside = self.cli_image.with_name('hgs.exe.old-1760371200-' + suffix)
        self.cli_image.rename(aside)
        replacement.rename(self.cli_image)
        deadline = time.monotonic() + 30
        while True:
            status = self.cli('desktop', 'status', '--json')
            if status.get('executable_upgrade', {}).get('state') == 'manual_refresh_required':
                return
            self.assertLess(time.monotonic(), deadline, status)
            time.sleep(0.1)

    def test_original_instance_refresh_keeps_its_grant_mode_and_original_ids(self):
        """Actual private hosted task/caller/loaded image. Held helper death
        is before every manager call, not abrupt in-flight manager cleanup
        qualification. Installed Settings and owner-PC checks remain separate."""
        initial = self.cli('desktop', 'registration', 'status', '--json')
        self.assertFalse(initial['registered'], 'Requires an unused disposable runner')
        self.task = initial['task_name']
        self.sid = self.task.removeprefix('hgs-desktop-')
        clean = False
        try:
            installed = self.cli('desktop', 'registration', 'install', '--json')
            self.installed = True
            self.cli_image = Path(installed['cli']['path']).resolve(strict=True)
            shutil.copy2(self.frontend_source, self.frontend)
            passive = self.mode('passive')
            self.assertTrue(passive['ok'], passive)
            self.latest = passive['instance']
            self.grant()
            granted = self.cli('desktop', 'ownership-status', '--json')
            original = dict(self.latest)
            self.pin_original(original)
            before = self.observe()['instances']
            self.replace_private_cli_and_wait_for_retirement('01234567')
            info = self.refresh_info_drained()
            target = info['target']
            self.assertEqual(target['original'], original)
            self.assertEqual(target['mode'], 'passive')
            self.assertTrue(info['drained'], info)
            self.assertNotEqual(target['original_image'], target['replacement_image'])
            path = self.state / 'desktop' / 'windows-registration-mode.json'
            stable = path.read_bytes()
            # Same configuration with a changed confirmed selection must fail
            # before reserving an intent, and without any native effect.
            for key in ('selection', 'grant_binding'):
                wrong = dict(target, **{key: '0' * 64})
                refused = self.refresh_call('mode_refresh', str(uuid.uuid4()), wrong)
                self.assertFalse(refused['ok'], refused)
                self.assertIn('target changed', refused['error'])
                self.assertEqual(path.read_bytes(), stable)
                self.assertEqual(self.observe()['instances'], before)
                self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), granted)
            # A live original partial pipe blocks the real controller before
            # Held/Stop. Closing it is the actual EOF, not an invented drain.
            with self.partial_original_pipe(original, target['configuration']):
                blocked_info = self.refresh_call('mode_refresh_info')
                self.assertFalse(blocked_info['drained'], blocked_info)
                blocked_id = str(uuid.uuid4())
                blocked = self.refresh_call('mode_refresh', blocked_id, blocked_info['target'])
                self.assertFalse(blocked['ok'], blocked)
                record = json.loads(path.read_bytes())
                self.assertEqual((record['pending']['request_id'], record['pending']['phase']),
                                 (blocked_id, 'prepared'), blocked)
                self.assertEqual(self.observe()['instances'], before)
                self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), granted)
            cancelled = self.refresh_call('mode_refresh_cancel', blocked_id)
            self.assertEqual(cancelled['receipt']['phase'], 'refused', cancelled)
            target = self.refresh_info_drained()['target']
            request = str(uuid.uuid4())
            child = self.refresh_started_at_held(request, target)
            helper = self.held_helper(request, child)
            record = json.loads(path.read_bytes())
            self.assertEqual((record['pending']['request_id'], record['pending']['phase']), (request, 'held'))
            self.assertEqual(record['pending']['refresh']['kind'], 'original_instance_refresh')
            self.kill_held_helper(helper)
            lost = self.first_call_finished(child)
            self.assertNotEqual(lost['code'], 0, lost)
            helper['marker'].unlink()
            pending = path.read_bytes()
            outcome = self.cli('desktop', 'registration', 'refresh-result', '--json', '--request-id', request)
            self.assertEqual((outcome['outcome'], outcome['receipt']['phase']), ('unknown', 'unknown'))
            duplicate = self.refresh_call('mode_refresh', request, target)
            self.assertEqual(duplicate['outcome'], 'unknown', duplicate)
            self.assertFalse(duplicate['manager_changed'], duplicate)
            self.assertFalse(self.mode('background')['ok'])
            self.assertEqual(path.read_bytes(), pending)
            self.assertEqual(self.observe()['instances'], before)
            self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), granted)
            cancelled = self.refresh_call('mode_refresh_cancel', request)
            self.assertEqual(cancelled['receipt']['phase'], 'refused', cancelled)
            self.assertFalse(cancelled['manager_changed'])
            target = self.refresh_info_drained()['target']
            new_request = str(uuid.uuid4())
            refreshed = self.refresh_call('mode_refresh', new_request, target)
            self.assertTrue(refreshed['ok'], refreshed)
            self.assertEqual((refreshed['read_only'], refreshed['changed'], refreshed['manager_changed']),
                             (False, True, True), 'native positive must complete the engine, not reconcile a late receipt: ' + str(refreshed))
            self.assertIsNone(refreshed.get('original_exchange_error'), refreshed)
            receipt = refreshed['receipt']
            self.assertEqual((receipt['request'], receipt['phase']), (new_request, 'settled'))
            self.assertEqual(receipt['scope']['mode'], 'passive')
            self.assertEqual(receipt['original'], original)
            self.assertEqual((receipt['scope']['journal'], receipt['scope']['owner_epoch']),
                             (granted['journal'], granted['owner_epoch']))
            current = receipt['ready']['instance']
            self.latest = current
            self.assertNotEqual(current['guid'], original['guid'])
            self.assertNotEqual((current['pid'], current['birth']), (original['pid'], original['birth']))
            self.assertNotEqual(current['epoch'], original['epoch'])
            observed = self.observe()['instances']
            self.assertEqual([item['guid'] for item in observed], [current['guid']], observed)
            current_grant = self.cli('desktop', 'ownership-status', '--json')
            self.assertEqual((current_grant['state'], current_grant['journal'], current_grant['owner_epoch'],
                              current_grant['identity_current'], current_grant['rearm_required']),
                             ('service', granted['journal'], granted['owner_epoch'], True, False))
            lifecycle = self.cli('desktop', 'status', '--json')['lifecycle']
            self.assertEqual((lifecycle['epoch'], lifecycle['generation'], lifecycle['mode']),
                             (current['epoch'], receipt['ready']['generation'], 'passive'))
            settled = path.read_bytes()
            # Every call has a fresh native helper; loss of the original reply
            # and helper restart can only read this SAME request/receipt.
            duplicate = self.refresh_call('mode_refresh', new_request, target)
            self.assertEqual(duplicate['receipt'], receipt)
            self.assertFalse(duplicate['manager_changed'])
            self.assertEqual(path.read_bytes(), settled)
            self.assertEqual(self.observe()['instances'], observed)
            # Separately choose background with the EXISTING maintained mode
            # route, then exercise same-background refresh. The refresh itself
            # must preserve this mode history and the original Grant incarnation.
            background = self.mode('background')
            self.assertTrue(background['ok'], background)
            self.latest = background['instance']
            self.pin_original(self.latest)
            background_original = dict(self.latest)
            mode_record = json.loads(path.read_bytes())
            self.replace_private_cli_and_wait_for_retirement('89abcdef')
            target = self.refresh_info_drained()['target']
            self.assertEqual((target['mode'], target['original']), ('background', background_original))
            refreshed = self.refresh_call('mode_refresh', str(uuid.uuid4()), target)
            self.assertTrue(refreshed['ok'], refreshed)
            self.assertEqual((refreshed['read_only'], refreshed['changed'], refreshed['manager_changed']),
                             (False, True, True), 'native positive must complete the engine, not reconcile a late receipt: ' + str(refreshed))
            self.assertIsNone(refreshed.get('original_exchange_error'), refreshed)
            receipt = refreshed['receipt']
            self.assertEqual((receipt['phase'], receipt['scope']['mode'], receipt['original']),
                             ('settled', 'background', background_original))
            self.assertEqual((receipt['scope']['journal'], receipt['scope']['owner_epoch']),
                             (granted['journal'], granted['owner_epoch']))
            current = receipt['ready']['instance']
            self.latest = current
            self.assertNotEqual(current['guid'], background_original['guid'])
            self.assertNotEqual((current['pid'], current['birth']),
                                (background_original['pid'], background_original['birth']))
            saved = json.loads(path.read_bytes())
            for key in ('completed', 'cancelled', 'choices', 'first_choices', 'reviewed'):
                self.assertEqual(saved.get(key), mode_record.get(key), key)
            lifecycle = self.cli('desktop', 'status', '--json')['lifecycle']
            self.assertEqual((lifecycle['epoch'], lifecycle['generation'], lifecycle['mode']),
                             (current['epoch'], receipt['ready']['generation'], 'background'))
            settled = path.read_bytes()
            observed = self.observe()['instances']
            # Revoked/blocked refresh is explicitly unsupported in this source
            # slice. It never Rearms or toggles the registration to recover it.
            revoked = self.cli('desktop', 'revoke', '--json', '--request-id', str(uuid.uuid4()), '--reason', 'manual')
            self.assertTrue(revoked['revoked'], revoked)
            blocked = self.refresh_call('mode_refresh_info')
            self.assertFalse(blocked['ok'], blocked)
            self.assertIn('current Service Grant', blocked['error'])
            self.assertEqual(path.read_bytes(), settled)
            self.assertEqual(self.observe()['instances'], observed)
            self.stop_exact_passive(current)
            self.assertFalse(self.cli('desktop', 'status', '--json')['running'])
            self.assertFalse(self.cli('desktop', 'registration', 'remove', '--json')['registered'])
            self.installed = False
            self.close_originals()
            clean = True
        finally:
            if clean:
                shutil.rmtree(self.root)
            else:
                print('REFRESH_PRIVATE_ROOT_RETAINED ' + str(self.root), file=sys.stderr)
                if self.installed:
                    try:
                        print('REFRESH_TASK_OBSERVATION ' + json.dumps(self.observe()), file=sys.stderr)
                    except Exception as error:
                        print('REFRESH_OBSERVATION_UNAVAILABLE ' + type(error).__name__, file=sys.stderr)

    def test_first_choice_instance_drift_refuses_at_held_and_cancels_without_stop(self):
        """The same timely Held arrangement now refuses before StopOffered.
        The original UUID remains Unknown at Held until explicit metadata
        cancellation; neither refusal nor cancellation touches the proven
        replacement. The hold still spends the original Stop deadline, and
        the precise selection error must win rather than an expiry error."""
        initial = self.cli('desktop', 'registration', 'status', '--json')
        self.assertFalse(initial['registered'], 'Task-mode fixture requires an unused disposable runner')
        self.task = initial['task_name']
        self.sid = self.task.removeprefix('hgs-desktop-')
        clean = False
        try:
            installed = self.cli('desktop', 'registration', 'install', '--json')
            self.installed = True
            self.cli_image = Path(installed['cli']['path']).resolve(strict=True)
            shutil.copy2(self.frontend_source, self.frontend)
            passive = self.mode('passive')
            self.assertTrue(passive['ok'], passive)
            self.latest = passive['instance']
            self.grant()
            granted = self.cli('desktop', 'ownership-status', '--json')
            path = self.state / 'desktop' / 'windows-registration-mode.json'
            request = str(uuid.uuid4())
            worker = self.armed_instance_restart(request, self.latest)
            child = self.first_call_started(request)
            drift = self.first_call_finished(child)
            later = self.armed_instance_restarted(worker)
            print('MODE_TASK_DRIFT_RESTART ' + json.dumps(later), file=sys.stderr)
            marker = self.config / f'held-pause-{request}.json'
            self.assertEqual(json.loads(marker.read_text(encoding='utf-8'))['request'], request)
            self.assertEqual(drift['code'], 0, drift)
            refused = json.loads(drift['stdout'])
            self.assertFalse(refused['ok'], refused)
            # The instance check refused, not the deadline the hold consumed.
            self.assertIn('Original task selection changed before stop', refused['error'])
            arranged = later['later']
            self.assertTrue(same_replacement(arranged, self.exact_later_instance(arranged['guid'])),
                            (arranged, self.observe()))
            (self.config / f'held-release-{request}').unlink()
            marker.unlink()
            record = json.loads(path.read_text(encoding='utf-8'))
            self.assertEqual((record['pending']['request_id'], record['pending']['phase']),
                             (request, 'held'), record)
            self.assertEqual(record['pending']['original'], passive['instance'])
            status = self.cli('desktop', 'registration', 'status', '--json')
            self.assertEqual(status['selected_mode'], 'passive', 'no update was sent')
            self.assertEqual((status['first_choice']['request'], status['first_choice']['phase']),
                             (request, 'unknown'), status)
            self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), granted)
            # The one replacement the worker proved before the release was
            # neither stopped nor replaced: same GUID, PID and birth, still live.
            self.assertTrue(same_replacement(arranged, self.exact_later_instance(arranged['guid'], wait=0)))
            result = self.cli('desktop', 'registration', 'mode-result', '--json', '--request-id', request)
            self.assertEqual((result['outcome'], result['receipt']['phase']), ('unknown', 'unknown'), result)
            endpoint = self.cli('desktop', 'status', '--json')
            self.assertTrue(endpoint['running'], endpoint)
            self.assertEqual(endpoint['lifecycle']['pid'], arranged['pid'])
            # This is the maintained explicit pre-manager metadata operation,
            # after the original helper/caller child has physically completed.
            cancelled = self.cancel(request)
            self.assertTrue(cancelled['ok'], cancelled)
            self.assertFalse(cancelled['manager_changed'], cancelled)
            self.assertEqual(cancelled['cancelled_request_id'], request)
            after = self.cli('desktop', 'registration', 'status', '--json')
            self.assertIsNone(after['pending_transition'], after)
            self.assertEqual(after['cancelled_request_id'], request)
            self.assertEqual(after['selected_mode'], 'passive')
            result = self.cli('desktop', 'registration', 'mode-result', '--json', '--request-id', request)
            self.assertEqual((result['outcome'], result['receipt']['phase']), ('refused', 'refused'), result)
            self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), granted)
            self.assertTrue(same_replacement(arranged, self.exact_later_instance(arranged['guid'], wait=0)))
            current = self.cli('desktop', 'status', '--json')
            self.assertEqual((current['epoch'], current['generation'], current['lifecycle']['pid']),
                             (endpoint['epoch'], endpoint['generation'], arranged['pid']))
            # Teardown is a separate fixture operation on that exact proved
            # replacement, never a capability supplied by cancellation.
            stopped = self.stop_exact_passive(dict(guid=arranged['guid'], pid=arranged['pid'],
                                                   birth=arranged['birth']))
            self.assertTrue(stopped['stopped'], stopped)
            self.assertFalse(self.cli('desktop', 'status', '--json')['running'])
            self.assertFalse(self.cli('desktop', 'registration', 'remove', '--json')['registered'])
            self.installed = False
            clean = True
        finally:
            if clean:
                shutil.rmtree(self.root)
            elif self.installed:
                # An uncertain instance is never killed or adopted here; the
                # disposable runner is the final boundary.
                print('MODE_TASK_DRIFT_OBSERVATION ' + json.dumps(self.observe()), file=sys.stderr)

    def test_explicit_mode_round_trip_uses_the_own_task_and_preserves_unrelated_processes(self):
        # A deliberate fixture failure must retain its structured error on
        # stdout, without a task, process mutation or CLIXML progress prefix.
        with self.assertRaisesRegex(AssertionError, r'"fixture_error":'):
            self.powershell("throw 'fixture diagnostic control'", {})
        # Never touch a pre-existing per-user registration on a reused image.
        initial = self.cli('desktop', 'registration', 'status', '--json')
        self.assertFalse(initial['registered'], 'Task-mode fixture requires an unused disposable runner')
        self.task = initial['task_name']
        self.sid = self.task.removeprefix('hgs-desktop-')
        shell = subprocess.Popen([str(Path(os.environ['SystemRoot']) / 'System32/cmd.exe'),
                                  '/D', '/Q', '/C', 'set /p ZERUS_MODE_FIXTURE_WAIT='],
                                 stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                 creationflags=subprocess.CREATE_NO_WINDOW)
        clean = False
        try:
            installed = self.cli('desktop', 'registration', 'install', '--json')
            self.installed = True
            # The maintained registration installer stages the stable CLI
            # without enabling local sessions. Native relays must then use
            # THAT image, matching the actual service's file identity.
            self.cli_image = Path(installed['cli']['path']).resolve(strict=True)
            self.assertEqual(self.cli_image, self.profile / '.local' / 'bin' / 'hgs.exe')
            shutil.copy2(self.frontend_source, self.frontend)
            self.assertFalse((self.config / 'local-sessions').exists())
            self.assertFalse(installed['started'])
            self.assertFalse(self.cli('desktop', 'status', '--json')['running'])
            refused = self.mode('background')
            self.assertFalse(refused['ok'], refused)
            self.assertFalse(self.cli('desktop', 'status', '--json')['running'])
            passive = self.mode('passive')
            self.assertTrue(passive['ok'], passive)
            self.latest = passive['instance']
            self.assertEqual(passive['selected_mode'], 'passive')
            self.assert_exact_user_created_state()
            discovery = self.observe()
            self.assertTrue(discovery['hidden'], discovery)
            self.assertEqual([item['guid'] for item in discovery['instances']],
                             [passive['instance']['guid']], discovery)
            print('MODE_TASK_HIDDEN_DISCOVERY ' + json.dumps(discovery), file=sys.stderr)
            self.assertIsNone(shell.poll())
            passive = self.recover_initial_start_and_cancel_background_before_a_grant(passive)
            self.grant()
            self.requester_bound_ownership_is_a_native_read()
            granted = self.cli('desktop', 'ownership-status', '--json')
            no_op = self.mode('passive')
            self.assertTrue(no_op['ok'], no_op)
            self.assertFalse(no_op['changed'])
            self.assertEqual(self.cli('desktop', 'ownership-status', '--json'), granted)
            self.assertEqual(self.cli('desktop', 'status', '--json')['lifecycle']['pid'], passive['instance']['pid'])
            self.metadata_refusals(passive)
            self.busy_exclusion_does_not_reserve(passive)
            cancelled = self.cancel_unsent_private_rollback(passive)
            # The first passive-to-background switch uses the first-choice
            # form; the ordinary form stays covered by next_background below.
            self.overlapping_layout_refuses_the_first_choice()
            self.lost_reply_at_held_keeps_its_original()
            self.expired_hold_refuses_at_held()
            toast = self.toast_registration_names_another_copy()
            background = self.first_background_choice_from_plain_passive()
            self.assertTrue(background['ok'], background)
            self.attention_refuses_another_copy_then_is_ready(toast)
            self.lost_journal_refuses_every_mode_operation(background)
            self.writer_commit_window_is_unavailable_never_lost(background)
            self.assertNotEqual(background['request_id'], cancelled)
            self.assertEqual(background['running_mode'], 'background')
            self.latest = background['instance']
            self.assert_exact_user_created_state()
            # Ordinary install/refresh preserves selection and the SAME runtime.
            before = self.cli('desktop', 'status', '--json')['lifecycle']['pid']
            refresh = self.cli('desktop', 'registration', 'install', '--json')
            self.assertFalse(refresh['changed'])
            self.assertFalse(refresh['started'])
            self.assertFalse(refresh['stopped'])
            self.assertEqual(refresh['selected_mode'], 'background')
            self.assertEqual(self.cli('desktop', 'status', '--json')['lifecycle']['pid'], before)
            self.assertIsNone(shell.poll())
            rollback = self.mode('passive')
            self.assertTrue(rollback['ok'], rollback)
            # zerus-1an7q: the rollback result above (ok, then ownership
            # 'revoked' below) is the primary control for a clean drain with
            # attention ready. The background instance's Task Scheduler history
            # is a second check only where it exists (zerus-lpz3t): without it
            # nothing is asserted and the subtest reports a skip, never a pass.
            history = self.task_history(background['instance']['guid'])
            print('MODE_TASK_BACKGROUND_EXIT ' + json.dumps(history), file=sys.stderr)
            with self.subTest('background instance crash-code history'):
                if not (history.get('available') and history.get('events')):
                    print('MODE_TASK_CRASH_CODE crash-code history unavailable; relying on the rollback result',
                          file=sys.stderr)
                    self.skipTest('crash-code history unavailable; relying on the rollback result')
                faults = [event for event in history['events']
                          for key in ('ResultCode', 'ReturnCode', 'ErrorCode', 'ErrorValue')
                          if str(event['data'].get(key, '')).lower() in ('3221225477', '0xc0000005', '-1073741819')]
                self.assertEqual(faults, [], history)
            self.latest = rollback['instance']
            self.pin_original(self.latest)  # Keep this OLD PID unavailable for reuse through review.
            self.assert_exact_user_created_state()
            self.assertEqual(self.cli('desktop', 'ownership-status', '--json')['state'], 'revoked')
            status = self.cli('desktop', 'registration', 'status', '--json')
            self.assertEqual(status['selected_mode'], 'passive')
            self.assertEqual(status['running_mode'], 'passive')
            self.assertIsNone(status['pending_transition'])
            self.assertEqual(status['completed_request_id'], rollback['request_id'])
            # Passive again, ownership revoked: the old first-choice UUID only
            # reads, and a NEW one still needs a current Service Grant.
            self.refused_first_choice(background['request_id'], 'already offered')
            self.refused_first_choice(str(uuid.uuid4()), 'Grant')
            self.pending_first_choice_refuses_the_old_frame(self.latest)
            self.cancel_ready_rollback_after_authority_moves(background, rollback)
            # A separately explicit new choice is admitted after cancellation;
            # the original manager effects and revoke are never replayed.
            next_background = self.mode('background')
            self.assertTrue(next_background['ok'], next_background)
            self.assertNotEqual(next_background['request_id'], rollback['request_id'])
            self.latest = next_background['instance']
            final_passive = self.mode('passive')
            self.assertTrue(final_passive['ok'], final_passive)
            self.latest = final_passive['instance']
            self.cancel_ready_rollback_after_authority_moves(next_background, final_passive, later_revoke=True)
            before_refusal = self.observe()['instances']
            refusal = self.stop_exact_passive(self.latest, wrong_birth=True)
            self.assertFalse(refusal['stopped'], refusal)
            self.assertEqual(refusal['reason'], 'original_birth_changed', refusal)
            self.assertEqual(self.observe()['instances'], before_refusal)
            still_running = self.cli('desktop', 'status', '--json')
            self.assertTrue(still_running['running'])
            self.assertEqual(still_running['lifecycle']['pid'], self.latest['pid'])
            self.review_old_stop_without_adopting_the_later_passive(rollback['instance'], self.latest)
            self.live_original_review_requires_a_barrier_and_never_stops_it(self.latest)
            self.assertFalse(self.cli('desktop', 'status', '--json')['running'])
            self.assertFalse(self.cli('desktop', 'registration', 'remove', '--json')['registered'])
            self.installed = False
            self.assertIsNone(shell.poll())
            clean = True
        finally:
            shell.stdin.write(b'done\n'); shell.stdin.close()
            shell.wait(timeout=5)
            if clean:
                shutil.rmtree(self.root)
            else:
                # An uncertain original instance is never killed or adopted.
                # Preserve its exact paths; disposable VM teardown is the final
                # boundary. The failed test remains failed/unqualified.
                if self.installed:
                    try:
                        print('MODE_TASK_OBSERVATION ' + json.dumps(self.observe()), file=sys.stderr)
                        status = self.cli('desktop', 'status', '--json')
                        print('MODE_TASK_SERVICE ' + json.dumps({key: status.get(key) for key in
                              ('configuration', 'running', 'lifecycle')}), file=sys.stderr)
                        # Fixed known leaves only: this read-only footprint
                        # narrows a guard failure even if it cannot log safely.
                        print('MODE_TASK_STARTUP_FILES ' + json.dumps({name: (
                            (self.state / 'desktop' / name).stat().st_size
                            if (self.state / 'desktop' / name).is_file() else None)
                            for name in ('owner.lock', 'state.sqlite3', 'state.sqlite3-wal',
                                         'state.sqlite3-shm', 'startup-failure.json')}), file=sys.stderr)
                        diagnostic = self.state / 'desktop' / 'startup-failure.json'
                        if diagnostic.exists():
                            self.assertLessEqual(diagnostic.stat().st_size, 4096)
                            data = json.loads(diagnostic.read_bytes())
                            self.assertEqual(data['abi'], 'desktop-startup-failure-v1')
                            self.assertEqual(data['configuration'], status['configuration'])
                            print('MODE_TASK_STARTUP_REFUSAL ' + json.dumps(data), file=sys.stderr)
                        try:
                            print('MODE_TASK_STARTUP_SECURITY ' + json.dumps(self.startup_security()), file=sys.stderr)
                        except Exception as error:
                            print('MODE_TASK_STARTUP_SECURITY_UNAVAILABLE ' + type(error).__name__, file=sys.stderr)
                        record = json.loads((self.state / 'desktop' / 'windows-registration-mode.json')
                                            .read_text(encoding='utf-8'))
                        started = (record.get('pending') or {}).get('started')
                        if started:
                            print('MODE_TASK_ORIGINAL_HISTORY ' + json.dumps(self.task_history(started)),
                                  file=sys.stderr)
                    except Exception as error:
                        print('MODE_TASK_OBSERVATION_UNAVAILABLE ' + type(error).__name__, file=sys.stderr)
                print('MODE_TASK_PRIVATE_ROOT_RETAINED ' + str(self.root), file=sys.stderr)


if __name__ == '__main__':
    unittest.main()
