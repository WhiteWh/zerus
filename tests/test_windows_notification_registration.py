"""Disposable Windows metadata only: no Toast Show, service Run or agent action."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import uuid

HGS = Path(sys.argv[sys.argv.index('--hgs') + 1]).resolve() if '--hgs' in sys.argv else None
if '--hgs' in sys.argv:
    index = sys.argv.index('--hgs')
    del sys.argv[index:index + 2]


@unittest.skipUnless(os.name == 'nt' and HGS is not None, 'explicit disposable Windows CLI required')
class WindowsToastRegistration(unittest.TestCase):
    def test_owned_registration_round_trip_without_starting_a_runtime(self):
        root = Path(tempfile.mkdtemp(prefix='zerus-toast-registration-'))
        env = {key: os.environ[key] for key in ('SystemRoot', 'WINDIR', 'COMSPEC') if key in os.environ}
        env.update(HOME=str(root), USERPROFILE=str(root), APPDATA=str(root / 'roaming'),
                   LOCALAPPDATA=str(root / 'local'), TEMP=str(root / 'tmp'), TMP=str(root / 'tmp'),
                   HGS_CONFIG_DIR=str(root / 'config'), HGS_STATE_DIR=str(root / 'state'),
                   HGS_SELF='toast-registration-fixture', HGS_PEERS='')
        for name in ('roaming', 'local', 'tmp', 'config'):
            (root / name).mkdir()
        request = str(uuid.uuid4())  # Retain for every cleanup attempt; never rotate it.
        task_touched = toast_touched = False
        task_absent = toast_absent = False

        def cli(*args):
            result = subprocess.run([str(HGS), *args], env=env, capture_output=True,
                                    text=True, encoding='utf-8', timeout=75)
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
            self.assertLessEqual(len(result.stdout.encode('utf-8')), 65536)
            return json.loads(result.stdout)

        def toast(operation):
            args = ['desktop', 'notification-registration', operation, '--json']
            if operation == 'remove':
                args += ['--request-id', request]
            return cli(*args)

        try:
            cold = subprocess.run([str(HGS), '--desktop-notification-activation', '-Embedding'],
                                  env=env, capture_output=True, timeout=15)
            self.assertEqual(cold.returncode, 78, cold.stderr)
            task_absent = not cli('desktop', 'registration', 'status', '--json')['registered']
            self.assertTrue(task_absent, 'Keep the existing task; this runner is not disposable')
            toast_absent = not toast('status')['registered']
            self.assertTrue(toast_absent, 'Keep the existing Toast registration')
            task_touched = True
            task_absent = False
            installed = cli('desktop', 'registration', 'install', '--json')
            self.assertTrue(installed['registered'])
            self.assertEqual(installed['task_state'], 3)
            toast_touched = True
            toast_absent = False
            first = toast('install')
            self.assertTrue(first['registered'])
            self.assertTrue(first['changed'])
            self.assertFalse(first['started'])
            self.assertFalse(first['stopped'])
            self.assertTrue(toast('status')['registered'])
            self.assertFalse(toast('install')['changed'])
            self.assertFalse(cli('desktop', 'status', '--json')['running'])
            self.assertFalse((root / 'state' / 'desktop' / 'state.sqlite3').exists())
            removed = toast('remove')  # Verified no-service/no-state, not a live drain.
            self.assertTrue(removed['changed'])
            self.assertFalse(removed['registered'])
            toast_absent = not toast('status')['registered']
            self.assertTrue(toast_absent)
            self.assertFalse(toast('remove')['changed'])
            task_absent = not cli('desktop', 'registration', 'remove', '--json')['registered']
            self.assertTrue(task_absent)
        finally:
            # Only the exact config's maintained removals; uncertainty retains
            # the selected CLI and private paths rather than a dangling server.
            if toast_touched and not toast_absent:
                try:
                    toast_absent = not toast('status')['registered']
                    if not toast_absent:
                        toast_absent = not toast('remove')['registered']
                except Exception:
                    toast_absent = False
            if task_touched and not task_absent:
                try:
                    task_absent = not cli('desktop', 'registration', 'status', '--json')['registered']
                    if not task_absent:
                        task_absent = not cli('desktop', 'registration', 'remove', '--json')['registered']
                except Exception:
                    task_absent = False
            if task_absent and toast_absent:
                shutil.rmtree(root)
            else:
                print('Registration cleanup uncertain; private root retained:', root, file=sys.stderr)


if __name__ == '__main__':
    unittest.main()
