"""Exercise the local source launcher in private prefixes without a display or service manager."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'tray/linux-launcher.py'
spec = importlib.util.spec_from_file_location('linux_launcher', SOURCE)
launcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(launcher)


class LinuxLauncher(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='zerus-local-webkit-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.install = self.root / 'install with spaces'
        self.stack = self.root / 'private stack'
        self.binary = self.install / 'bin/hgs-tray'
        self.binary.parent.mkdir(parents=True)
        self.gui('tauri')
        for name in (*launcher.LIBRARIES, *launcher.PROCESSES):
            path = self.stack / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('fixture')
            path.chmod(0o755)
        self.env = {k: v for k, v in os.environ.items()
                    if k not in ('HGS_LINUX_WEBKIT_PREFIX', 'LD_LIBRARY_PATH', 'WEBKIT_EXEC_PATH')}

    def gui(self, kind, capability=1):
        self.binary.write_text(f'''#!{sys.executable}
import json, os, sys
if sys.argv[1:] == ['--build-info']:
    print(json.dumps({{'ZerusGUI': {kind!r}, 'localWebKitChildEnvironment': {capability}}}))
else:
    print(json.dumps({{'args': sys.argv[1:], 'libraries': os.environ.get('LD_LIBRARY_PATH'),
                      'processes': os.environ.get('WEBKIT_EXEC_PATH'),
                      'original': os.environ.get({launcher.ENV_MARKER!r})}}))
''')
        self.binary.chmod(0o755)

    def configure(self, **env):
        return subprocess.run([sys.executable, str(SOURCE), '--install', str(self.install)],
                              env=dict(self.env, **env), capture_output=True, text=True, timeout=10)

    def launch(self, **env):
        result = subprocess.run([str(self.install / launcher.LAUNCHER), '--sessions', 'two words'],
                                env=dict(self.env, **env), capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout), result.stderr

    def select(self):
        result = self.configure(HGS_LINUX_WEBKIT_PREFIX=str(self.stack))
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_unset_is_stock_and_explicit_selection_persists_across_reinstalls(self):
        self.assertEqual(self.configure().returncode, 0)
        self.assertFalse((self.install / launcher.SETTING).exists())
        self.assertFalse((self.install / launcher.LAUNCHER).exists())
        self.select()
        setting = self.install / launcher.SETTING
        before = setting.read_bytes()
        self.assertEqual(setting.stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.configure().stdout, '')
        self.assertEqual(setting.read_bytes(), before)
        result, _ = self.launch(LD_LIBRARY_PATH='/other/libraries')
        marker = result.pop('original')
        self.assertEqual(json.loads(marker), dict(version=1, LD_LIBRARY_PATH=b'/other/libraries'.hex(), WEBKIT_EXEC_PATH=None))
        self.assertEqual(result, dict(args=['--sessions', 'two words'],
                                     libraries=f'{self.stack}/gtk/lib:{self.stack}/lib:/other/libraries',
                                     processes=f'{self.stack}/libexec/webkit2gtk-4.1'))
        self.assertEqual(self.configure(HGS_LINUX_WEBKIT_PREFIX='').returncode, 0)
        self.assertFalse(setting.exists())
        self.assertFalse((self.install / launcher.LAUNCHER).exists())

    def test_marker_preserves_unset_empty_and_existing_webkit_process_path(self):
        self.select()
        for env, libraries, processes in (({}, None, None),
                                          ({'LD_LIBRARY_PATH': '', 'WEBKIT_EXEC_PATH': ''}, '', ''),
                                          ({'WEBKIT_EXEC_PATH': '/original/processes'}, None, '/original/processes')):
            original = json.loads(self.launch(**env)[0]['original'])
            decode = lambda value: None if value is None else bytes.fromhex(value).decode()
            self.assertEqual((decode(original['LD_LIBRARY_PATH']), decode(original['WEBKIT_EXEC_PATH'])), (libraries, processes))

    def test_missing_or_partial_stack_falls_back_and_returns_without_reconfiguration(self):
        self.select()
        removed = self.stack / launcher.LIBRARIES[0]
        removed.unlink()
        self.assertIn('using system libraries', self.launch()[1])
        self.assertIsNone(self.launch()[0]['libraries'])
        removed.write_text('fixture')
        absent = self.root / 'unmounted'
        self.stack.rename(absent)
        self.assertEqual(self.configure().returncode, 0, 'retain a temporarily absent prefix')
        self.assertIsNone(self.launch()[0]['processes'])
        absent.rename(self.stack)
        self.assertIsNotNone(self.launch()[0]['libraries'])

    def test_qt_rollback_and_malformed_settings_use_stock(self):
        self.select()
        self.gui('qt')
        self.assertIsNone(self.launch()[0]['libraries'])
        self.assertIsNone(self.launch()[0]['original'])
        self.gui('tauri', capability=0)
        self.assertIsNone(self.launch()[0]['libraries'], 'older client rollback cannot sanitize children')
        self.gui('tauri')
        for value in ('{', '[]', '{"version":1,"prefix":"relative"}', 'x' * 5000):
            (self.install / launcher.SETTING).write_text(value)
            self.assertIsNone(self.launch()[0]['processes'])

    def test_invalid_explicit_opt_in_preserves_the_selection(self):
        self.select()
        setting = self.install / launcher.SETTING
        before = setting.read_bytes()
        for value in ('relative', '/missing', f'{self.stack}:other'):
            result = self.configure(HGS_LINUX_WEBKIT_PREFIX=value)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(setting.read_bytes(), before)


if __name__ == '__main__':
    unittest.main()
