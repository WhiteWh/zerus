"""Disposable Windows CI: actual own-user task, never start/stop a service."""
import base64
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


HGS = Path(sys.argv[sys.argv.index('--hgs') + 1]).resolve() if '--hgs' in sys.argv else None
if '--hgs' in sys.argv:
    index = sys.argv.index('--hgs')
    del sys.argv[index:index + 2]


@unittest.skipUnless(os.name == 'nt' and HGS is not None, 'explicit disposable Windows CLI required')
class WindowsDesktopRegistration(unittest.TestCase):
    def test_native_worker_refuses_bad_frames_without_loading_configuration(self):
        with tempfile.TemporaryDirectory(prefix='zerus-registration-frame-') as directory:
            root=Path(directory)
            env={key: os.environ[key] for key in ('SystemRoot','WINDIR') if key in os.environ}
            env.update(HGS_CONFIG_DIR=str(root/'unread-config'), HGS_STATE_DIR=str(root/'unread-state'))
            for argument, body in [('0',b''),('65537',b''),('03',b'{} '),('3',b'{}'),('3',b'[1]'),('3',b'{x}')]:
                with self.subTest(length=argument, body=body):
                    result=subprocess.run([str(HGS),'__desktop-registration-native',argument],input=body,env=env,capture_output=True,timeout=20)
                    self.assertEqual(result.returncode,1,result.stderr)
                    reply=json.loads(result.stdout)
                    self.assertEqual(reply['abi'],'desktop-native-registration-v1')
                    self.assertIsInstance(reply['error'],str)
                    self.assertNotIn('reply',reply)
            self.assertEqual(list(root.iterdir()),[])

    def test_cold_notification_entry_refuses_without_loading_configuration(self):
        with tempfile.TemporaryDirectory(prefix='zerus-toast-cold-') as directory:
            root = Path(directory)
            env = {key: os.environ[key] for key in ('SystemRoot', 'WINDIR', 'COMSPEC') if key in os.environ}
            env.update(HGS_CONFIG_DIR=str(root / 'unread-config'), HGS_STATE_DIR=str(root / 'unread-state'))
            result = subprocess.run([str(HGS), '--desktop-notification-activation', '-Embedding'],
                                    env=env, capture_output=True, timeout=15)
            self.assertEqual(result.returncode, 78, result.stderr)
            self.assertEqual(result.stdout, b'')
            self.assertEqual(list(root.iterdir()), [])

    def test_fixed_input_frame_finishes_while_the_writer_stays_open(self):
        body = json.dumps({'schema': 1, 'operation': 'invalid'}).encode('utf-8')
        process = subprocess.Popen([str(HGS), '__desktop-registration-native', str(len(body))],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   creationflags=subprocess.CREATE_NO_WINDOW)
        try:
            process.stdin.write(body)
            process.stdin.flush()
            # Deliberately keep the ORIGINAL writer open, with no EOF. A timed
            # out reader fails this test; the 15s product limit is unchanged.
            self.assertEqual(process.wait(timeout=15), 1)
            self.assertFalse(process.stdin.closed)
            output = process.stdout.read(65537)
            error = process.stderr.read(65537)
            self.assertLessEqual(len(output), 65536)
            self.assertEqual(error, b'')
            self.assertEqual(json.loads(output)['abi'], 'desktop-native-registration-v1')
            self.assertIn('error',json.loads(output))
        finally:
            process.stdin.close()
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)
            process.stdout.close()
            process.stderr.close()

    def test_own_interactive_logon_task_without_starting_service_or_enabling_sessions(self):
        root = Path(tempfile.mkdtemp(prefix='zerus-desktop-logon-'))
        env = {key: os.environ[key] for key in ('SystemRoot', 'WINDIR', 'COMSPEC') if key in os.environ}
        env.update(HOME=str(root), USERPROFILE=str(root),
                   APPDATA=str(root / 'roaming'), LOCALAPPDATA=str(root / 'local'),
                   TEMP=str(root / 'tmp'), TMP=str(root / 'tmp'),
                   HGS_CONFIG_DIR=str(root / 'config'), HGS_STATE_DIR=str(root / 'state'),
                   HGS_SELF='windows-registration-fixture', HGS_PEERS='')
        if os.environ.get('HGS_DESKTOP_REGISTRATION_TRACE') == '1':
            env['HGS_DESKTOP_REGISTRATION_TRACE'] = '1'
        for name in ('roaming', 'local', 'tmp', 'config'):
            (root / name).mkdir()

        def cli(*args):
            result = subprocess.run([str(HGS), *args], env=env, capture_output=True,
                                    text=True, encoding='utf-8', timeout=75)
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
            self.assertLessEqual(len(result.stdout.encode('utf-8')), 65536)
            return json.loads(result.stdout)

        def legacy(operation, plan):
            # This frozen oracle is test-only: hgs never loads a script or
            # interpreter. It proves upgrade compatibility in both directions.
            source = Path(__file__).resolve().parents[1] / 'scripts/ci/fixtures/windows-desktop-registration-legacy.ps1'
            body=json.dumps({'schema':1,'operation':operation,'plan':plan}).encode('utf-8')
            escaped=str(source).replace("'","''")
            script=(". ([ScriptBlock]::Create([IO.File]::ReadAllText('"+escaped+"')));"
                    "Initialize-RegistrationEnvironment;Invoke-RegistrationMain "+str(len(body)))
            command=[str(Path(os.environ['SystemRoot'])/'System32/WindowsPowerShell/v1.0/powershell.exe'),
                     '-NoLogo','-NoProfile','-NonInteractive','-EncodedCommand',
                     base64.b64encode(script.encode('utf-16le')).decode('ascii')]
            result=subprocess.run(command,input=body,env=env,capture_output=True,timeout=20)
            self.assertEqual(result.returncode,0,result.stderr+result.stdout)
            self.assertLessEqual(len(result.stdout),65536)
            return json.loads(result.stdout)

        def digest(path):
            self.assertLessEqual(path.stat().st_size, 256 * 1024 * 1024)
            with path.open('rb') as stream:
                return hashlib.file_digest(stream, 'sha256').digest()

        absent = False
        installed = False
        try:
            status = cli('desktop', 'registration', 'status', '--json')
            self.assertFalse(status['registered'], 'Keep pre-existing task; this runner is not disposable')
            absent = True
            installed = True
            absent = False
            first = cli('desktop', 'registration', 'install', '--json')
            self.assertTrue(first['registered'])
            self.assertTrue(first['changed'])
            self.assertFalse(first['started'])
            self.assertFalse(first['stopped'])
            self.assertEqual(first['task_state'], 3)  # TASK_STATE_READY, not queued/running.
            staged = root / '.local' / 'bin' / 'hgs.exe'
            self.assertEqual(digest(staged), digest(HGS))
            self.assertFalse((root / 'config' / 'local-sessions').exists())
            self.assertFalse(cli('desktop', 'status', '--json')['running'])
            again = cli('desktop', 'registration', 'install', '--json')
            self.assertFalse(again['changed'])
            self.assertFalse(again['cli']['changed'])
            self.assertEqual(again['task_state'], 3)
            selected_env = dict(env, HGS_EXECUTABLE=str(staged))
            selected=subprocess.run([str(HGS),'desktop','registration','--json'],env=selected_env,
                                    capture_output=True,timeout=20)
            self.assertEqual(selected.returncode,0,selected.stderr)
            plan=json.loads(selected.stdout)
            self.assertEqual(plan['configuration'],first['configuration'])
            # Native-created task must satisfy the previous helper's exact
            # ownership/property checks. Legacy removal never Run/Stop/End.
            self.assertTrue(legacy('status',plan)['registered'])
            self.assertFalse(legacy('install',plan)['changed'])
            self.assertFalse(legacy('remove',plan)['registered'])
            # An existing pre-native task must remain an idempotent install
            # and be removable by the new native reader without replacement.
            self.assertTrue(legacy('install',plan)['registered'])
            compatible=cli('desktop','registration','install','--json')
            self.assertTrue(compatible['registered'])
            self.assertFalse(compatible['changed'])
            self.assertEqual(compatible['task_state'],3)
            removed = cli('desktop', 'registration', 'remove', '--json')
            self.assertTrue(removed['changed'])
            self.assertFalse(removed['registered'])
            self.assertFalse(removed['stopped'])
            self.assertFalse(cli('desktop', 'registration', 'status', '--json')['registered'])
            installed = False
            absent = True
            self.assertFalse(cli('desktop', 'registration', 'remove', '--json')['changed'])
            self.assertTrue(staged.exists())
        finally:
            # Only our exact native-validated task can be removed. Uncertainty
            # retains its source paths rather than leaving a dangling task.
            if installed:
                try:
                    current = cli('desktop', 'registration', 'status', '--json')
                    absent = not current['registered']
                    if not absent:
                        absent = not cli('desktop', 'registration', 'remove', '--json')['registered']
                except Exception:
                    absent = False
            if absent:
                shutil.rmtree(root)
            else:
                print('Registration cleanup uncertain; private root retained:', root, file=sys.stderr)


if __name__ == '__main__':
    unittest.main()
