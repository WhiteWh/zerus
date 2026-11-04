#!/usr/bin/env python3
"""The seven Kimi Bash, eight CreateGoal and one ExitPlanMode approval contracts in a private Windows ConPTY host.

Only the sealed selected official latest win32-x64 image qualifies. No tmux, shared Kimi,
user profile, real provider or relaxed screen predicate is used. A separate
known-environment child must first qualify kimi_windows_process on this exact
hosted Windows image. That reader uses an internal x64 ABI and refuses unknown
layouts; its results are runtime-environment proof, not exec-time capture.
"""
import ctypes
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
import uuid

import kimi_windows_process as process
import test_kimi_approvals_native as approvals

ROOT = Path(__file__).resolve().parents[1]
WAIT = 120


def child_ticks(child):
    k, _, _, w = process.api()
    times = [w.FILETIME() for _ in range(4)]
    if not k.GetProcessTimes(child._handle, *(ctypes.byref(item) for item in times)):
        raise OSError('cannot bind the exact unreaped child handle')
    return str((times[0].dwHighDateTime << 32) | times[0].dwLowDateTime)


@unittest.skipUnless(os.name == 'nt', 'the pinned ConPTY approval fixture requires Windows')
class WindowsEnvironmentOracle(unittest.TestCase):
    def test_a_known_private_child_environment_matches_the_bounded_reader(self):
        """Qualify the internal environment layout before any Kimi process."""
        with tempfile.TemporaryDirectory(prefix='ke-') as directory:
            env = dict(SystemRoot=os.environ['SystemRoot'], HOME=directory, USERPROFILE=directory,
                       TEMP=directory, TMP=directory, KIMI_ORACLE='private-fixture-only')
            child = subprocess.Popen([sys.executable, '-I', '-c', 'import sys; sys.stdin.buffer.read()'],
                                     env=env, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                                     stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NO_WINDOW)
            pin = None
            try:
                pin = process.Process(child.pid, child_ticks(child))
                actual, sample = pin.environment(Path(sys.executable).resolve(), os.getpid())
                self.assertEqual({key: value for key, value in actual.items() if not key.startswith('=')},
                                 {key.upper(): value for key, value in env.items()})
                self.assertEqual(sample['pid'], child.pid)
            finally:
                child.stdin.close()
                try:
                    child.wait(5)
                except subprocess.TimeoutExpired:
                    child.terminate()  # Popen's original Windows handle, no PID lookup.
                    child.wait(5)
                child.stderr.close()
                if pin:
                    pin.close()


@unittest.skipUnless(os.name == 'nt', 'native Kimi approvals require Windows')
class NativeWindowsKimiApprovals(approvals.ApprovalCases, approvals.GoalCases, approvals.PlanCases, unittest.TestCase):
    command = approvals.MARKER
    rows = None
    card = approvals.NativeKimiApprovals.card
    payload = approvals.NativeKimiApprovals.payload
    resolved = approvals.NativeKimiApprovals.resolved
    answerable = approvals.NativeKimiApprovals.answerable
    decide = approvals.NativeKimiApprovals.decide
    tool_results = approvals.NativeKimiApprovals.tool_results

    def setUp(self):
        self.version = approvals.admitted_version(approvals.KIMI, approvals.KIMI_SHA256)
        self.source = Path(approvals.KIMI).resolve(strict=True)
        self.guards = approvals.seals([self.source, *approvals.GUARDED])
        self.assertEqual(self.guards[str(self.source)], approvals.KIMI_SHA256)
        self.root = Path(tempfile.mkdtemp(prefix='ka-', dir=os.environ['RUNNER_TEMP'])).resolve()
        self.images, self.client, self.client_start = None, None, None
        self.pinned, self.names, self.server = {}, [], None
        self.addCleanup(self.custody)
        self.images = Path(tempfile.mkdtemp(prefix='ki-', dir=os.environ['RUNNER_TEMP'])).resolve()
        print('ZERUS_KIMI_FIXTURE=' + json.dumps(dict(case=self.id(), root=str(self.root),
              images=str(self.images))), flush=True)
        if '_danger' in self._testMethodName:
            self.command = approvals.DANGER
        self.rows = 12 if '_clipped_' in self._testMethodName else None
        self.profile, self.state = self.root / 'home', self.root / 'state'
        self.bin, self.project, self.home = self.profile / '.local/bin', self.root / 'p', self.profile / '.kimi-code'
        for folder in (self.bin, self.state, self.project, self.home):
            folder.mkdir(parents=True)
        (self.project / 'scratch-fixture').mkdir()
        self.image = self.images / 'kimi.exe'
        shutil.copyfile(self.source, self.image)  # Never hard-link an updater to the pin.
        self.assertEqual(approvals.digest(self.image), approvals.KIMI_SHA256)
        shutil.copyfile(self.image, self.bin / 'kimi.exe')
        # The actual image that hgs finds is a separate, equally pinned copy.
        self.image = self.bin / 'kimi.exe'
        self.goal_mode = approvals.goal_mode(self._testMethodName)
        self.plan_scenario = self._testMethodName.startswith('test_plan_')
        arguments = dict(objective=approvals.GOAL_OBJECTIVE,
                         completionCriterion=approvals.GOAL_CRITERION) if self.goal_mode else None
        self.server = approvals.serve(self.command, 'CreateGoal' if self.goal_mode else 'Bash', arguments,
                                      plan=self.plan_scenario)
        self.env = approvals.private_environment(self.profile, self.home, self.bin,
            'http://127.0.0.1:%d%s' % (self.server.server_port, approvals.PLATFORM),
            dict(HGS_CONFIG_DIR=str(self.profile / '.config/hgs'), HGS_STATE_DIR=str(self.state),
                 HGS_SELF='test', HGS_PEERS='', HGS_TAB='0', HGS_TRACKING='1'))
        system = Path(os.environ['SystemRoot'])
        bash = Path(os.environ['HGS_KIMI_GIT_BASH']).resolve(strict=True)
        self.assertEqual(approvals.digest(bash), os.environ['HGS_KIMI_GIT_BASH_SHA256'])
        self.bash, self.bash_sha = bash, approvals.digest(bash)
        self.env.update(SystemRoot=str(system), WINDIR=str(system), COMSPEC=str(system / 'System32/cmd.exe'),
                        PATHEXT='.COM;.EXE;.BAT;.CMD', TEMP=self.env['TMPDIR'], TMP=self.env['TMPDIR'],
                        PATH=os.pathsep.join(map(str, (self.bin, bash.parent, system / 'System32',
                             system / 'System32/WindowsPowerShell/v1.0'))),
                        HTTP_PROXY='http://127.0.0.1:9', HTTPS_PROXY='http://127.0.0.1:9',
                        ALL_PROXY='http://127.0.0.1:9', NO_PROXY='127.0.0.1,localhost')
        self.assertEqual(os.path.normcase(os.path.realpath(shutil.which('bash', path=self.env['PATH']))),
                         os.path.normcase(str(bash)), 'only the exact installed Git Bash may resolve')
        approvals.make_private_folders(self.env)
        (self.home / 'config.toml').write_text(
            approvals.model_config(self.server.server_port, self.goal_mode or 'default'))
        (self.home / 'tui.toml').write_text(approvals.tui_config(self._testMethodName))
        portable = self.root / 'portable'
        portable.mkdir()
        self.hgs_exe = portable / 'hgs.exe'
        shutil.copyfile(Path(os.environ['HGS_TEST_BIN']).resolve(strict=True), self.hgs_exe)
        self.hgs_sha = approvals.digest(self.hgs_exe)
        conpty = json.loads((ROOT / 'packaging/windows/conpty.json').read_text(encoding='utf-8'))['files']
        for name, pin in conpty.items():
            path = Path(os.environ['HGS_TEST_CONPTY_DIR']) / name
            self.assertEqual((path.stat().st_size, approvals.digest(path)), (pin['size'], pin['sha256']))
            shutil.copyfile(path, portable / name)
        self.hgs('session-host', 'install', '--json')
        private_images = [path for path in self.root.rglob('*') if path.suffix.lower() in ('.exe', '.dll')]
        self.assertLessEqual(len(private_images), 128)
        self.private_guards = approvals.seals(private_images)
        self.name = 'kimi/p/approval'
        self.names.append(self.name)  # Custody is reserved even if launch times out.
        self.launch_acknowledged = False
        self.hgs('kimi', str(self.project), '--new', '-n', 'approval', '-d')
        self.launch_acknowledged = True
        self.until(lambda: self.binding(self.name).get('pid'))
        self.pin_host()
        self.assert_process('initial')
        self.trust()
        ready = self.until(lambda: self.inspect().get('first_message_can_send') and self.inspect())
        text = 'Start the fixture goal.' if self.goal_mode else 'Run the fixture command.'
        marker = '1. Switch to Never Ask and start' if self.goal_mode else '1. Approve once'
        if self.plan_scenario:
            ready = self.plan_mode_on()
            text, marker = approvals.PLAN_PROMPT, 'Ready to build with this plan?'
        payload = dict(request_id=str(uuid.uuid4()), text=text,
                       expected_run_id=ready['run_id'], expected_conversation_id=ready.get('conversation_id') or '')
        self.assertEqual(self.call('send', payload)['status'], 'submitted')

        def asked():
            files = list(self.home.glob('sessions/*/*/agents/main/wire.jsonl'))
            return files[0] if len(files) == 1 and '"kind":"approval"' in files[0].read_text(encoding='utf-8') \
                and marker in self.screen() else None
        self.wire = self.until(asked)
        self.assert_process('approval')
        if self.rows:
            self.attach_once(self.rows)
            self.assertEqual(self.height(), self.rows)

    def hgs(self, *args):
        result = self.run_hgs(*args)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result.stdout

    def run_hgs(self, *args, input=None):
        return subprocess.run([str(self.hgs_exe), *args], env=self.env, cwd=self.project,
                              input=input, stdin=None if input is not None else subprocess.DEVNULL,
                              capture_output=True, text=True, encoding='utf-8', errors='strict',
                              timeout=WAIT, creationflags=subprocess.CREATE_NO_WINDOW)

    def rpc(self, operation, **fields):
        result = self.run_hgs('__host', 'call', json.dumps(dict(op=operation, **fields)))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return json.loads(result.stdout)['ok']

    def binding(self, name):
        path = self.state / (hashlib.sha256(name.encode()).hexdigest() + '.json')
        return json.loads(path.read_text(encoding='utf-8')) if path.exists() else {}

    def inspect(self):
        return json.loads(self.hgs('inspect', self.name))

    def screen(self):
        return self.rpc('capture', target=dict(name=self.name), styled=False)['text']

    def height(self):
        return self.rpc('pane_info', target=dict(name=self.name))['height']

    def press(self, key):
        """One key into the private pseudoconsole, written as tmux send-keys writes it."""
        self.rpc('send_keys', target=dict(name=self.name), keys=[dict(named=key)])

    def until(self, predicate, seconds=WAIT):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            value = predicate()
            if value:
                return value
            time.sleep(.2)
        self.fail('Native Windows Kimi timed out: ' + self.screen())

    def call(self, command, payload, success=True):
        result = self.run_hgs(command, self.name, '--json', input=json.dumps(payload))
        if not success:
            self.assertNotEqual(result.returncode, 0, result.stdout)
            return result.stderr
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return json.loads(result.stdout)

    def trust(self):
        def card():
            cards = [item for item in self.inspect().get('pending_questions', [])
                     if item.get('source') == 'kimi_folder_trust' and item.get('can_answer')]
            return cards[0] if len(cards) == 1 else None
        item = self.until(card)
        self.assertIn('Trust this folder?', self.screen())
        binding = self.binding(self.name)
        self.assertIn(binding['conversation_id'], (None, ''))
        self.assertIn(item['conversation_id'], (None, ''))
        self.assertEqual(item['run_id'], binding['run_id'])
        # Startup is deliberately unbound, unlike a normal approval. Keep the
        # existing Unix trust envelope; never coerce a generic approval's ID.
        payload = dict(request_id=str(uuid.uuid4()), expected_run_id=binding['run_id'],
                       expected_conversation_id='', question_id=item['question_id'],
                       expected_question_hash=item['question_hash'],
                       answers=[dict(question_id='trust', selected_option_ids=['trust_0'], text='')])
        self.assertEqual(self.call('answer', payload)['status'], 'answered')

    def pin(self, pid, started):
        key = pid, str(started)
        if key not in self.pinned:
            self.pinned[key] = process.Process(pid, started)
        return self.pinned[key]

    def pin_host(self):
        status = json.loads(self.hgs('__host', 'status'))
        self.assertTrue(status.get('running'), status)
        self.assertEqual(status['status']['conpty'], 'bundled')
        item = status['host']
        self.pin(item['pid'], item['created'])

    def assert_process(self, stage):
        record = self.binding(self.name)
        supervisor = record['supervisor']
        self.pin(supervisor['pid'], supervisor['start'])
        pinned = self.pin(record['pid'], record['process_start'])
        actual, sample = pinned.environment(self.image, supervisor['pid'])
        self.assertEqual(approvals.digest(self.image), approvals.KIMI_SHA256)
        findings = approvals.environment_findings(actual, self.env)
        print('ZERUS_KIMI_PROCESS=' + json.dumps(dict(case=self.id(), stage=stage, version=self.version,
              sha256=approvals.KIMI_SHA256, sample=sample, **findings)), flush=True)
        self.assertEqual(findings, dict(keys={key: 'match' for key in approvals.REQUIRED},
                                       forbidden=[], unexpected_kimi=[]))

    def attach_request(self, rows):
        return dict(target=dict(name=self.name), size=dict(cols=80, rows=rows),
                    run_id=self.binding(self.name)['run_id'])

    def attach_once(self, rows):
        self.hgs('__host', 'attach', json.dumps(self.attach_request(rows)),
                 json.dumps([dict(wait_ms=300), dict(detach=True)]))
        self.assertEqual(self.rpc('list_clients', target=dict(name=self.name))['clients'], [])

    def attach_client(self):
        self.client = subprocess.Popen([str(self.hgs_exe), '__host', 'attach',
            json.dumps(self.attach_request(self.rows)), json.dumps([dict(wait_ms=60000), dict(detach=True)])],
            env=self.env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            creationflags=subprocess.CREATE_NO_WINDOW)
        self.client_start = child_ticks(self.client)
        self.until(lambda: len(self.rpc('list_clients', target=dict(name=self.name))['clients']) == 1)

    def test_an_unviewed_clipped_panel_grows_until_answerable(self):
        self.assertEqual(self.height(), 12)
        self.assertEqual(self.rpc('list_clients', target=dict(name=self.name))['clients'], [])
        card = self.answerable()
        self.assertGreater(self.height(), 12)
        self.assertEqual(self.decide(card, option='3'), {'decision': 'rejected'})
        self.assertEqual(self.rpc('list_clients', target=dict(name=self.name))['clients'], [])

    def retain(self):
        if not approvals.EVIDENCE:
            raise AssertionError('exact private native evidence directory is required')
        out = Path(approvals.EVIDENCE) / self._testMethodName
        out.mkdir(parents=True, exist_ok=True)
        if getattr(self, 'name', None):
            record = self.binding(self.name)
            captured = self.run_hgs('__host', 'call', json.dumps(dict(op='capture',
                target=dict(name=self.name), styled=False)))
            response = json.loads(captured.stdout)
            if captured.returncode:
                # A definitive current absence after a failed launch is useful
                # evidence. It never turns the primary launch failure into PASS.
                self.assertFalse(getattr(self, 'launch_acknowledged', False), captured.stdout + captured.stderr)
                self.assertEqual(record, {}, record)
                self.assertEqual((response.get('error') or {}).get('code'), 'not_found', response)
                (out / 'capture-absent.json').write_text(json.dumps(dict(
                    status='absent_before_launch_acknowledgement', response=response,
                    exit=captured.returncode)), encoding='utf-8')
            else:
                (out / 'screen.txt').write_text(response['ok']['text'], encoding='utf-8')
            (out / 'record.json').write_text(json.dumps(record), encoding='utf-8')
        if getattr(self, 'home', None) and self.home.exists():
            for source in [self.home / 'logs', *self.home.glob('sessions/*/*/agents')]:
                if source.is_dir():
                    shutil.copytree(source, out / source.relative_to(self.home), dirs_exist_ok=True)
        if self.server:
            (out / 'model.json').write_text(json.dumps(dict(requests=self.server.requests,
                 platform=self.server.platform, completed=self.server.platform_completed)), encoding='utf-8')

    def stop_client(self):
        if self.client:
            # The original Popen handle pins this exact child, even after exit.
            if self.client.poll() is None:
                self.client.terminate()
            self.client.wait(10)

    def stop_owned(self):
        failures = []
        if getattr(self, 'hgs_exe', None):
            if self.names:
                try:
                    self.pin_host()
                except BaseException as error:
                    failures.append('final owned host binding: ' + repr(error))
            for name in self.names:
                try:
                    record = self.binding(name)
                    if record.get('pid') and record.get('process_start'):
                        self.pin(record['pid'], record['process_start'])
                except BaseException as error:
                    failures.append('final process binding: ' + repr(error))
                try:
                    result = self.run_hgs('__host', 'call', json.dumps(dict(op='kill', target=dict(name=name))))
                    if result.returncode:
                        # A typed absent session is harmless; malformed/unknown is not.
                        try:
                            value = json.loads(result.stdout)
                        except ValueError:
                            value = {}
                        if (value.get('error') or {}).get('code') != 'not_found':
                            failures.append('owned session stop was not acknowledged')
                except BaseException as error:
                    failures.append('owned session stop: ' + repr(error))
            if self.pinned:
                try:
                    end = time.monotonic() + 15
                    while self.rpc('list')['sessions'] and time.monotonic() < end:
                        time.sleep(.2)
                    if self.rpc('list')['sessions']:
                        raise AssertionError('owned sessions did not drain after kill')
                    self.rpc('stop_host')
                except BaseException as error:
                    failures.append('host stop: ' + repr(error))
        for pinned in self.pinned.values():
            try:
                if not pinned.wait(15):
                    pinned.terminate()
                    if not pinned.wait(10):
                        failures.append('owned process did not exit after handle termination')
                    failures.append('owned process exceeded graceful cleanup bound')
            except BaseException as error:
                failures.append('owned handle cleanup: ' + repr(error))
            finally:
                try:
                    pinned.close()
                except BaseException as error:
                    failures.append('owned handle close: ' + repr(error))
        self.pinned.clear()
        if getattr(self, 'hgs_exe', None):
            files = [path for path in self.root.rglob('*') if path.suffix.lower() in ('.exe', '.dll')]
            if len(files) > 128:
                failures.append('owned executable inventory exceeded its bound')
            else:
                def held(path):
                    try:
                        with path.open('r+b'):
                            return False
                    except FileNotFoundError:
                        return False
                    except OSError:
                        return True
                end = time.monotonic() + 15
                while any(held(path) for path in files) and time.monotonic() < end:
                    time.sleep(.2)
                if any(held(path) for path in files):
                    failures.append('owned executable/library custody remains unknown')
        if failures:
            raise AssertionError('; '.join(failures))

    def seal(self):
        after = approvals.seals([self.source, *approvals.GUARDED])
        private = [path for path in (getattr(self, 'image', None),
                   self.images / 'kimi.exe' if self.images else None) if path]
        problems = approvals.seal_problems(self.guards, after)
        for path in private:
            if approvals.digest(path) != approvals.KIMI_SHA256:
                problems.append('private Kimi copy changed')
        if getattr(self, 'bash', None) and approvals.digest(self.bash) != self.bash_sha:
            problems.append('the reviewed Git Bash executable changed')
        if getattr(self, 'hgs_exe', None) and approvals.digest(self.hgs_exe) != self.hgs_sha:
            problems.append('private hgs copy changed')
        private_before = getattr(self, 'private_guards', {})
        problems.extend(approvals.seal_problems(private_before, approvals.seals(map(Path, private_before))))
        print('ZERUS_KIMI_IMAGE_GUARDS=' + json.dumps(dict(case=self.id(), before=self.guards, after=after,
              problems=problems)), flush=True)
        if problems:
            raise AssertionError('; '.join(problems))

    def custody(self):
        errors = []
        def attempt(label, step):
            try:
                step()
                return True
            except BaseException as error:
                errors.append(label + ': ' + repr(error))
                return False
        attempt('evidence', self.retain)
        attempt('attached client', self.stop_client)
        attempt('owned processes', self.stop_owned)
        if self.server:
            attempt('model stop', self.server.shutdown)
            attempt('model close', self.server.server_close)
        attempt('after-shutdown-attempt seals', self.seal)
        removed = []
        if not errors:
            for path in (self.root, self.images):
                if path:
                    try:
                        shutil.rmtree(path)
                        removed.append(str(path))
                    except BaseException as error:
                        errors.append('removal: ' + repr(error))
                        break
        if errors:
            print('ZERUS_KIMI_PRESERVED=' + json.dumps(dict(case=self.id(), removed=removed,
                  kept=[str(path) for path in (self.root, self.images) if path and path.exists()],
                  problems=errors)), flush=True)
            raise AssertionError('; '.join(errors))


if __name__ == '__main__':
    unittest.main()
