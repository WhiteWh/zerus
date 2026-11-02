#!/usr/bin/env python3
"""Desktop GUI live updates (tray/update-installed.py, tray/install.sh) with a fake GUI.

The fake GUI runs from the installed path inside an isolated home, so the
updater never matches the developer's own Zerus. systemctl, D-Bus tools and
cmake are fixtures; nothing reaches the real service manager or session bus.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

from tray_update_fixture import OwnedChildren, StartControl, cleanup
from desktop_revoke_fixture import REVOCATION_CLI

REPO = Path(__file__).resolve().parents[1]
UPDATER = REPO / 'tray/update-installed.py'

# A GUI build: answers --build-info, holds the single-instance socket, records
# activations, and (unless 'old') saves its queued draft when SIGTERM quits it.
FAKE_GUI = r'''#!{python}
import json, os, pathlib, signal, socket, sys
VARIANT = {variant!r}
root = pathlib.Path(os.environ['TRAY_FIXTURE'])
if sys.argv[1:] == ['--build-info']:
    # 'unverifiable' reports itself as a build, never once installed.
    if VARIANT == 'unverifiable' and pathlib.Path(sys.argv[0]).parent.name == 'bin':
        sys.exit(1)
    if VARIANT == 'legacy-broken':
        sys.exit(1)
    info = {{'version': '0.42.0', 'versionName': VARIANT, 'devBuildNumber': int(VARIANT[4:]) if VARIANT.startswith('dev-') else 0,
             'sourceCommit': VARIANT.ljust(40, '0'), 'sourceDirty': False}}
    if VARIANT.startswith('client-'):
        info.update(CFBundleIdentifier='com.hgdev.hgs-tray', ZerusGUI='tauri')
    print(json.dumps(info))
    sys.exit(0)
if VARIANT == 'broken':
    sys.exit(1)
drafts = pathlib.Path(os.environ['XDG_CONFIG_HOME']) / 'hgdev/hgs-tray.conf.drafts'
if VARIANT == 'eraser':
    for draft in drafts.rglob('draft.json'):
        draft.unlink()
def flush(*_):
    # The draft write that aboutToQuit flushes before the process ends.
    queued = drafts / 'queued'
    queued.mkdir(parents=True, exist_ok=True)
    (queued / 'draft.json').write_text(json.dumps({{'key': 'queued', 'text': 'saved on quit'}}))
    sys.exit(0)
if VARIANT != 'old':
    signal.signal(signal.SIGTERM, flush)
path = os.path.join(os.environ['TMPDIR'], 'hgs-tray')
server = socket.socket(socket.AF_UNIX)
try:
    os.unlink(path)
except FileNotFoundError:
    pass
server.bind(path)
server.listen()
with (root / 'started').open('a') as log:
    log.write(f'{{VARIANT}} {{os.getpid()}} {{os.environ["TRAY_LAUNCH_TOKEN"]}}\n')
while True:
    connection, _ = server.accept()
    with (root / 'activations').open('a') as log:
        log.write(connection.recv(64).decode())
    connection.close()
'''
SYSTEMCTL = r'''#!{python}
import os, pathlib, socket, sys, time
with (pathlib.Path(os.environ['TRAY_FIXTURE']) / 'systemctl').open('a') as log:
    log.write(' '.join(sys.argv[1:]) + '\n')
if sys.argv[1:] == ['--user', 'start', 'hgs-tray.service']:
    end = time.monotonic() + 3
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(3)
        client.connect(str(pathlib.Path(os.environ['TRAY_FIXTURE']) / 'start.sock'))
        client.sendall(b'start\n')
        client.shutdown(socket.SHUT_WR)
        reply = b''
        while len(reply) < 3:
            remaining = end - time.monotonic()
            if remaining <= 0:
                raise TimeoutError('fake service reply expired')
            client.settimeout(remaining)
            part = client.recv(3 - len(reply))
            if not part:
                raise RuntimeError('fake service reply is incomplete')
            reply += part
        if reply != b'ok\n':
            raise RuntimeError('fake service start refused')
elif sys.argv[1:] in (['--user', 'is-active', 'hgs-tray.service'],
                      ['--user', 'is-enabled', 'hgs-tray.service']):
    print('active' if sys.argv[2] == 'is-active' else 'enabled')
'''
NPM = r'''#!{python}
import json, os, pathlib, shutil, sys
root = pathlib.Path(os.environ['TRAY_FIXTURE'])
with (root / 'npm.json').open('a') as log:
    log.write(json.dumps(sys.argv[1:]) + '\n')
if sys.argv[1:4] == ['exec', '--', 'tauri']:
    output = root / 'client/src-tauri/target/release/hgs-tray'
    output.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(os.environ['TRAY_CLIENT_BUILD'], output)
'''


@unittest.skipUnless(sys.platform.startswith('linux'), 'the fake GUI is identified through /proc')
class TrayUpdate(unittest.TestCase):
    def setUp(self):
        # Short even with macOS's canonical /private/tmp Unix socket prefix.
        self.root = Path(tempfile.mkdtemp(prefix='ztu-', dir='/tmp'))
        self.owned = self.control = None
        self.addCleanup(self.stop_all)
        self.root = self.root.resolve()
        self.home = self.root / 'home'
        self.bin = self.root / 'bin'
        self.builds = self.root / 'builds'
        for path in (self.home / '.local/bin', self.bin, self.builds, self.root / 'tmp'):
            path.mkdir(parents=True)
        self.installed = self.home / '.local/bin/hgs-tray'
        self.drafts = self.home / '.config/hgdev/hgs-tray.conf.drafts'
        self.script(self.home / '.local/bin/hgs', REVOCATION_CLI.format(python=sys.executable))
        self.script(self.bin / 'systemctl', SYSTEMCTL.format(python=sys.executable))
        for tool in ('qdbus6', 'busctl'):
            self.script(self.bin / tool, '#!/bin/sh\nexit 1\n')
        self.env = dict(os.environ, HOME=str(self.home), TMPDIR=str(self.root / 'tmp'),
                        XDG_CONFIG_HOME=str(self.home / '.config'), TRAY_FIXTURE=str(self.root),
                        PATH=f'{self.bin}{os.pathsep}{os.environ["PATH"]}',
                        HGS_TRAY_QUIT_TIMEOUT='10', HGS_TRAY_START_TIMEOUT='4', HGS_DESKTOP_REVOKE_TIMEOUT='1')
        self.owned = OwnedChildren(self.env)
        self.control = StartControl(self.root / 'start.sock', lambda: self.owned.start(self.installed))
        self.control.open()

    def script(self, path, text):
        path.write_text(text)
        path.chmod(0o755)

    def build(self, variant, path=None):
        path = path or self.builds / variant / 'hgs-tray'
        path.parent.mkdir(parents=True, exist_ok=True)
        self.script(path, FAKE_GUI.format(python=sys.executable, variant=variant))
        return path

    def started(self):
        log = self.root / 'started'
        return [line.split() for line in log.read_text().splitlines()] if log.exists() else []

    def running(self):
        """The variant and pid of the live fake GUI, if any."""
        return self.owned.running(self.installed, self.started())

    def launch(self, variant):
        self.build(variant, self.installed)
        # The fake service's children remain owned and reapable by this test.
        self.owned.start(self.installed)
        self.wait(lambda: self.running() and (Path(self.env['TMPDIR']) / 'hgs-tray').exists())
        return self.running()[0][1]

    def wait(self, condition, seconds=10):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if condition():
                return
            time.sleep(0.1)
        self.fail('condition not reached')

    def stop_all(self):
        cleanup(self.root, self.control, self.owned, shutil.rmtree)

    def draft(self, name, text):
        folder = self.drafts / name
        folder.mkdir(parents=True)
        (folder / 'draft.json').write_text(json.dumps({'key': name, 'text': text}))

    def update(self, built):
        return subprocess.run([sys.executable, str(UPDATER), str(built), str(self.installed)], env=self.env,
                              capture_output=True, text=True, timeout=60)

    def leftovers(self):
        return [p.name for p in self.installed.parent.iterdir() if p.name.startswith('.hgs-tray-update-')]

    def test_running_gui_quits_like_its_menu_and_comes_back(self):
        old = self.launch('first')
        self.draft('kept', 'unsent text')
        result = self.update(self.build('second'))
        self.assertEqual(result.returncode, 0, result.stderr)
        # SIGTERM reached the handler: the queued draft was written before replacement.
        self.assertEqual(json.loads((self.drafts / 'queued/draft.json').read_text())['text'], 'saved on quit')
        self.assertEqual(json.loads((self.drafts / 'kept/draft.json').read_text())['text'], 'unsent text')
        [(variant, pid)] = self.running()
        self.assertEqual(variant, 'second')
        self.assertNotEqual(pid, old)
        self.assertIn('--user start hgs-tray.service', (self.root / 'systemctl').read_text())
        self.assertEqual((self.root / 'activations').read_text(), 'sessions\n')
        self.assertIn("'second'", self.installed.read_text())
        previous = self.home / '.local/share/hgs/desktop-previous/hgs-tray'
        self.assertIn("'first'", previous.read_text())
        record = json.loads((self.home / '.local/share/hgs/build-provenance.json').read_text())
        self.assertEqual((record['guiVersionName'], record['preservedDrafts']), ('second', 2))
        self.assertEqual(self.leftovers(), [])
        self.assertEqual(list((self.home / '.local/state/hgs/desktop-update').iterdir()), [])
        self.assertIn('2 nonempty drafts kept', result.stdout)
        # The next update keeps exactly one rollback copy: the GUI it replaced.
        result = self.update(self.build('third'))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual([p.name for p in previous.parent.iterdir()], ['hgs-tray'])
        self.assertIn("'second'", previous.read_text())
        self.assertEqual([variant for variant, _ in self.running()], ['third'])

    def test_gui_without_a_signal_handler_is_never_killed(self):
        old = self.launch('old')
        result = self.update(self.build('second'))
        self.assertEqual(result.returncode, 1)
        self.assertIn('predates graceful service stops', result.stderr)
        self.assertEqual(self.running(), [('old', old)])
        self.assertIn("'old'", self.installed.read_text())
        self.assertFalse((self.root / 'systemctl').exists())
        self.assertEqual(self.leftovers(), [])

    def test_new_gui_that_does_not_start_is_rolled_back(self):
        self.launch('first')
        result = self.update(self.build('broken'))
        self.assertEqual(result.returncode, 1)
        self.assertIn('previous GUI was restored', result.stderr)
        self.assertIn("'first'", self.installed.read_text())
        self.wait(lambda: [variant for variant, _ in self.running()] == ['first'])
        self.assertEqual(self.leftovers(), [])

    def test_stopped_gui_is_replaced_without_starting_it(self):
        self.build('first', self.installed)
        result = self.update(self.build('second'))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("'second'", self.installed.read_text())
        self.assertIn("'first'", (self.home / '.local/share/hgs/desktop-previous/hgs-tray').read_text())
        self.assertFalse((self.root / 'systemctl').exists())
        self.assertEqual(self.running(), [])
        self.assertEqual(self.leftovers(), [])

    def test_a_stopped_gui_that_cannot_report_itself_is_not_installed(self):
        # Without a start, the new GUI's own --build-info verifies it.
        self.build('first', self.installed)
        result = self.update(self.build('unverifiable'))
        self.assertEqual(result.returncode, 1)
        self.assertIn('the new GUI was not verified', result.stderr)
        self.assertIn('the previous GUI was restored', result.stderr)
        self.assertIn(f'{self.installed} keeps the previous GUI', result.stdout)
        self.assertIn("'first'", self.installed.read_text())
        self.assertFalse((self.home / '.local/share/hgs/desktop-previous').exists())

    def test_update_removes_only_old_unused_leftover_folders(self):
        folder = self.installed.parent

        def leftover(name, files=(), age=7200):
            path = folder / name
            path.mkdir()
            for file in files:
                (path / file).parent.mkdir(parents=True, exist_ok=True)
                (path / file).write_text('copy')
            then = time.time() - age
            for item in [*path.rglob('*'), path]:
                os.utime(item, (then, then))
            return path

        leftover('.zerus-main-500e966')
        leftover('.zerus-previous-500e966', ['hgs-tray'])
        leftover('.zerus-0.41.Abm', ['previous/hgs-tray', 'previous/hgs_state.py'])
        leftover('.hgs-tray-update-interrupted', ['hgs-tray'])
        kept = [leftover('.zerus-notes', ['notes.txt']), leftover('.zerus-recent', age=0),
                leftover('.zerus-deep', ['a/b/hgs-tray']), leftover('.other-stage'),
                # An interrupted update's set-aside GUI may be the only copy.
                leftover('.hgs-tray-update-set-aside', ['previous/hgs-tray'])]
        running = leftover('.zerus-running', ['hgs-tray'])
        self.script(running / 'hgs-tray', f'#!{sys.executable}\nimport time\ntime.sleep(60)\n')
        os.utime(running, (time.time() - 7200,) * 2)
        # One direct child keeps the folder in use without an unowned sleep descendant.
        self.owned.start(running / 'hgs-tray')
        (folder / '.zerus-link').symlink_to(self.root)
        self.build('first', self.installed)
        result = self.update(self.build('second'))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(sorted(p.name for p in folder.iterdir() if p.name.startswith('.')),
                         sorted([p.name for p in kept] + ['.zerus-running', '.zerus-link']))
        self.assertIn('removed 4 leftover folders', result.stdout)
        self.assertIn('.zerus-notes: it holds files other than installer copies', result.stdout)
        self.assertIn('.hgs-tray-update-set-aside: it holds files other than installer copies', result.stdout)
        self.assertIn('.zerus-running: a process still uses it', result.stdout)
        self.assertTrue((self.root / 'builds').exists())

    def test_lost_drafts_are_reported_with_their_snapshot(self):
        self.launch('first')
        self.draft('kept', 'unsent text')
        result = self.update(self.build('eraser'))
        self.assertEqual(result.returncode, 1)
        self.assertIn('kept: missing', result.stderr)
        [snapshot] = (self.home / '.local/state/hgs/desktop-update').iterdir()
        self.assertIn(str(snapshot), result.stdout)
        self.assertEqual(json.loads((snapshot / 'qt/kept/draft.json').read_text())['text'], 'unsent text')
        self.assertEqual(snapshot.stat().st_mode & 0o777, 0o700)

    def installer(self, build, plugin=None):
        """A copy of tray/install.sh whose cmake builds `build` and, given bytes, the native UI plugin."""
        tray = self.root / 'tray'
        if not tray.exists():
            for name in ('install.sh', 'update-installed.py', 'linux-source-update.sh',
                         'packaging/hgs-tray.service', 'packaging/hgs-tray.desktop'):
                (tray / name).parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(REPO / 'tray' / name, tray / name)
            shutil.copytree(REPO / 'tray/resources/icons', tray / 'resources/icons')
            self.script(self.bin / 'uname', '#!/bin/sh\necho Linux\n')
            for tool in ('gtk-update-icon-cache', 'kbuildsycoca6'):
                self.script(self.bin / tool, '#!/bin/sh\nexit 0\n')
        source = self.root / 'plugin.so'
        # Like CMakeLists.txt: a configuration without the plugin removes an old one.
        step = f'cp "{source}" "$2/plugins/libhgs-tray-native-ui.so"' if plugin else 'rm -f "$2/plugins/libhgs-tray-native-ui.so"'
        if plugin:
            source.write_bytes(plugin)
        self.script(self.bin / 'cmake', f'#!/bin/sh\nif [ "$1" = --build ]; then mkdir -p "$2/plugins" && cp "{build}" "$2/hgs-tray" && {step}; fi\n')
        return tray / 'install.sh'

    def client_installer(self, build, gstreamer=True):
        install = self.installer(build)
        tauri = self.root / 'client/src-tauri'
        tauri.mkdir(parents=True, exist_ok=True)
        (tauri / 'Cargo.toml').write_text('[features]\nmedia = []\nvideo = []\n')
        shutil.copy2(REPO / 'client/src-tauri/tauri.switch.linux.conf.json',
                     tauri / 'tauri.switch.linux.conf.json')
        self.script(self.bin / 'npm', NPM.format(python=sys.executable))
        self.script(self.bin / 'pkg-config', '#!/bin/sh\nexit "${TRAY_GSTREAMER_OK:-1}"\n')
        self.env['TRAY_CLIENT_BUILD'] = str(build)
        self.env['TRAY_GSTREAMER_OK'] = '0' if gstreamer else '1'
        return install

    def test_client_install_is_prefix_private_and_stays_selected(self):
        install = self.client_installer(self.build('client-1'))
        prefix = self.root / 'prefix%dev&review\\path'
        plugin = prefix / 'lib/hgs-tray/plugins/libhgs-tray-native-ui.so'
        plugin.parent.mkdir(parents=True)
        plugin.write_bytes(b'for the Qt rollback')
        env = dict(self.env, HGS_LINUX_GUI='client', HGS_INSTALL_PREFIX=str(prefix))
        result = subprocess.run(['bash', str(install)], env=env, capture_output=True, text=True, timeout=90)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        binary = prefix / 'bin/hgs-tray'
        self.assertTrue(binary.is_file())
        self.assertIn("'client-1'", binary.read_text())
        unit = prefix / 'lib/systemd/user/hgs-tray.service'
        escaped = str(binary).replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%')
        self.assertIn(f'ExecStart="{escaped}"', unit.read_text())
        self.assertNotIn('--sessions', unit.read_text())
        desktop = (prefix / 'share/applications/hgs-tray.desktop').read_text()
        self.assertIn(f'Exec={binary} --sessions', desktop)
        self.assertEqual(plugin.read_bytes(), b'for the Qt rollback')
        self.assertFalse((self.root / 'systemctl').exists(), 'a scratch prefix never reaches systemd')
        self.assertIn('no service-manager action', result.stdout)
        env.pop('HGS_LINUX_GUI')
        unit_before = unit.read_bytes()
        result = subprocess.run(['bash', str(install)], env=env, capture_output=True, text=True, timeout=90)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(unit.read_bytes(), unit_before)
        self.assertFalse(unit_before.endswith(b'\n\n'))
        calls = [json.loads(line) for line in (self.root / 'npm.json').read_text().splitlines()]
        self.assertEqual(len(calls), 4, 'npm ci and the Tauri build on each install')
        for call in calls[1::2]:
            self.assertEqual(call[-2:], ['--features', 'video'])

    def test_client_install_without_gstreamer_keeps_media_only(self):
        install = self.client_installer(self.build('client-1'), gstreamer=False)
        env = dict(self.env, HGS_LINUX_GUI='client', HGS_INSTALL_PREFIX=str(self.root / 'prefix'))
        result = subprocess.run(['bash', str(install)], env=env, capture_output=True, text=True, timeout=90)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('media only, without round video', result.stdout)
        calls = [json.loads(line) for line in (self.root / 'npm.json').read_text().splitlines()]
        self.assertEqual(calls[-1][-2:], ['--features', 'media'])

    def test_installer_places_and_removes_the_native_ui_plugin(self):
        installed = self.home / '.local/lib/hgs-tray/plugins/libhgs-tray-native-ui.so'
        build = self.build('first', self.installed)
        for plugin, expected in ((b'first plugin', b'first plugin'), (b'second plugin', b'second plugin'), (None, None)):
            result = subprocess.run(['bash', str(self.installer(build, plugin))], env=self.env, capture_output=True,
                                    text=True, timeout=90)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn('Installed hgs-tray.', result.stdout)
            self.assertEqual(installed.read_bytes() if installed.exists() else None, expected)
            self.assertFalse(installed.with_name(installed.name + '.new').exists())

    def test_default_install_keeps_the_packaged_unit_byte_identical(self):
        install = self.installer(self.build('first'))
        result = subprocess.run(['bash', str(install)], env=self.env,
                                capture_output=True, text=True, timeout=90)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        installed = self.home / '.config/systemd/user/hgs-tray.service'
        self.assertEqual(installed.read_bytes(), (REPO / 'tray/packaging/hgs-tray.service').read_bytes())
        desktop = self.home / '.local/share/applications/hgs-tray.desktop'
        before = desktop.read_bytes()
        result = subprocess.run(['bash', str(install)], env=self.env,
                                capture_output=True, text=True, timeout=90)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('already installed and unchanged', result.stdout)
        self.assertEqual(desktop.read_bytes(), before)
        self.assertTrue(before.endswith(b'\n') and not before.endswith(b'\n\n'))

    def test_installer_replaces_a_running_gui_through_the_updater(self):
        install = self.installer(self.build('second'))
        self.launch('first')
        result = subprocess.run(['bash', str(install)], env=self.env, capture_output=True,
                                text=True, timeout=90)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('Installed hgs-tray.', result.stdout)
        self.assertEqual([variant for variant, _ in self.running()], ['second'])
        self.assertEqual(json.loads((self.drafts / 'queued/draft.json').read_text())['text'], 'saved on quit')
        calls = (self.root / 'systemctl').read_text()
        self.assertNotIn('restart', calls)
        self.assertIn('--user enable --now hgs-tray.service', calls)


    def rollback(self):
        return subprocess.run([sys.executable, str(UPDATER), '--rollback', str(self.installed)], env=self.env,
                              capture_output=True, text=True, timeout=60)

    def test_rollback_reinstalls_the_kept_copy(self):
        self.launch('first')
        self.draft('kept', 'unsent text')
        result = self.update(self.build('second'))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        result = self.rollback()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('reinstalling the rollback copy', result.stdout)
        self.assertEqual([variant for variant, _ in self.running()], ['first'])
        self.assertIn("'first'", self.installed.read_text())
        self.assertIn("'second'", (self.home / '.local/share/hgs/desktop-previous/hgs-tray').read_text())
        self.assertEqual(json.loads((self.drafts / 'kept/draft.json').read_text())['text'], 'unsent text')
        self.assertEqual((self.root / 'activations').read_text(), 'sessions\nsessions\n')

    def test_client_updates_keep_qt_as_the_rollback_copy(self):
        prefix = self.root / 'prefix'
        self.env['HGS_INSTALL_PREFIX'] = str(prefix)
        self.installed = prefix / 'bin/hgs-tray'
        (prefix / 'bin').mkdir(parents=True)
        self.script(prefix / 'bin/hgs', REVOCATION_CLI.format(python=sys.executable))
        self.launch('first')
        self.draft('kept', 'from Qt')
        result = self.update(self.build('client-1'))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual([variant for variant, _ in self.running()], ['client-1'])
        slot = prefix / 'share/hgs/desktop-previous/hgs-tray'
        self.assertIn("'first'", slot.read_text())
        client_drafts = prefix / 'share/com.hgdev.hgs-tray/drafts'
        (client_drafts / 'kept').mkdir(parents=True)
        (client_drafts / 'kept/draft.json').write_text(json.dumps({'key': 'kept', 'text': 'from the client'}))
        result = self.update(self.build('client-2'))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual([variant for variant, _ in self.running()], ['client-2'])
        self.assertIn("'first'", slot.read_text(), 'later client updates retain the original Qt')
        self.assertEqual(json.loads((client_drafts / 'kept/draft.json').read_text())['text'], 'from the client')
        self.assertEqual(list(self.installed.parent.glob('.hgs-tray-kept-*')), [])
        kind = subprocess.run([sys.executable, str(UPDATER), '--installed-gui', str(self.installed)],
                              env=self.env, capture_output=True, text=True, timeout=30)
        self.assertEqual((kind.returncode, kind.stdout.strip()), (0, 'tauri'), kind.stderr)
        result = self.rollback()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual([variant for variant, _ in self.running()], ['first'])
        self.assertIn("'client-2'", slot.read_text())
        self.assertEqual(json.loads((client_drafts / 'kept/draft.json').read_text())['text'], 'from the client')
        kind = subprocess.run([sys.executable, str(UPDATER), '--installed-gui', str(self.installed)],
                              env=self.env, capture_output=True, text=True, timeout=30)
        self.assertEqual((kind.returncode, kind.stdout.strip()), (0, 'qt'), kind.stderr)

    def test_an_unreadable_legacy_gui_is_replaced_and_keeps_the_existing_slot(self):
        for variant in ('second', 'client-1'):
            with self.subTest(variant=variant):
                slot = self.home / '.local/share/hgs/desktop-previous/hgs-tray'
                self.build('previous-good', slot)
                self.build('legacy-broken', self.installed)
                (self.drafts / 'kept').mkdir(parents=True, exist_ok=True)
                (self.drafts / 'kept/draft.json').write_text(json.dumps({'key': 'kept', 'text': 'legacy draft'}))
                result = self.update(self.build(variant))
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn(f"'{variant}'", self.installed.read_text())
                self.assertIn("'previous-good'", slot.read_text())
                self.assertEqual(json.loads((self.drafts / 'kept/draft.json').read_text())['text'], 'legacy draft')

    def test_an_unknown_installed_kind_allows_an_on_disk_reinstall(self):
        self.build('unverifiable', self.installed)
        for variant in ('second', 'client-1'):
            with self.subTest(variant=variant):
                result = subprocess.run([sys.executable, str(UPDATER), '--check', str(self.build(variant)),
                                         str(self.installed)], env=self.env, capture_output=True, text=True, timeout=30)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_an_on_disk_client_switch_needs_the_live_update(self):
        self.build('first', self.installed)
        result = subprocess.run([sys.executable, str(UPDATER), '--check', str(self.build('client-1')),
                                 str(self.installed)], env=self.env, capture_output=True, text=True, timeout=30)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('needs the live update', result.stderr)
        self.assertIn("'first'", self.installed.read_text())

    def revocation_calls(self):
        path = self.root / 'revocation.jsonl'
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def test_no_handoff_revocation_allows_qt_to_start(self):
        self.env['REVOCATION_MODE'] = 'noop'
        self.build('first', self.installed)
        result = self.update(self.build('second'))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("'second'", self.installed.read_text())
        slot = self.home / '.local/share/hgs/desktop-previous/hgs-tray'
        self.assertIn("'first'", slot.read_text())

    def test_revocation_recovers_the_same_request_after_disconnect(self):
        self.env['REVOCATION_MODE'] = 'disconnect'
        self.launch('first')
        result = self.update(self.build('second'))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        calls = [call for call in self.revocation_calls()
                 if call['args'][:2] in (['desktop', 'revoke'], ['desktop', 'request'])]
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[0]['args'][:2], ['desktop', 'revoke'])
        self.assertEqual(calls[1]['args'][:2], ['desktop', 'request'])
        self.assertEqual(calls[0]['request_id'], calls[1]['request_id'])
        self.assertEqual([variant for variant, _ in self.running()], ['second'])

    def test_legacy_cli_without_desktop_state_keeps_qt_update_and_rollback(self):
        self.env['REVOCATION_MODE'] = 'unsupported'
        self.env.pop('HGS_STATE_DIR', None)
        self.env.pop('XDG_STATE_HOME', None)
        self.build('first', self.installed)
        result = self.update(self.build('second'))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("'second'", self.installed.read_text())
        self.assertIn('no desktop state; keeping the plain Qt path', result.stdout)

        result = self.rollback()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("'first'", self.installed.read_text())
        self.assertFalse(any(call['args'][:2] in (['desktop', 'status'], ['desktop', 'revoke'],
                                                  ['desktop', 'request']) for call in self.revocation_calls()))
        self.assertFalse((self.home / '.local/state/hgs/sessions').exists())

    def test_missing_cli_without_desktop_state_keeps_qt_update_and_rollback(self):
        # A GUI installed before hgs (no zerus-setup yet) has no writers to revoke.
        (self.home / '.local/bin/hgs').unlink()
        self.env.pop('HGS_STATE_DIR', None)
        self.env.pop('XDG_STATE_HOME', None)
        self.build('first', self.installed)
        result = self.update(self.build('second'))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("'second'", self.installed.read_text())
        self.assertIn('no hgs is installed and there is no desktop state', result.stdout)
        result = self.rollback()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("'first'", self.installed.read_text())
        self.assertFalse((self.home / '.local/state/hgs/sessions').exists())

    def test_missing_cli_with_desktop_state_keeps_qt_stopped(self):
        (self.home / '.local/bin/hgs').unlink()
        self.env.pop('HGS_STATE_DIR', None)
        self.env.pop('XDG_STATE_HOME', None)
        (self.home / '.local/state/hgs/sessions/desktop').mkdir(parents=True)
        self.build('first', self.installed)
        result = self.update(self.build('second'))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('cannot check desktop revocation', result.stderr)
        self.assertIn("'first'", self.installed.read_text())
        self.assertEqual(self.running(), [])

    def test_legacy_cli_with_desktop_state_keeps_qt_stopped(self):
        self.env['REVOCATION_MODE'] = 'unsupported'
        self.env.pop('HGS_STATE_DIR', None)
        self.env.pop('XDG_STATE_HOME', None)
        desktop = self.home / '.local/state/hgs/sessions/desktop'
        desktop.mkdir(parents=True)
        self.build('first', self.installed)
        result = self.update(self.build('second'))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('cannot check desktop revocation', result.stderr)
        self.assertIn(str(desktop), result.stderr)
        self.assertIn("'first'", self.installed.read_text())
        self.assertEqual(self.running(), [])
        self.assertFalse(any(call['args'][:2] == ['desktop', 'revoke'] for call in self.revocation_calls()))

    def test_failed_revocation_keeps_qt_stopped(self):
        for mode in ('held', 'pending', 'malformed', 'wrong_config', 'timeout'):
            with self.subTest(mode=mode):
                self.env['REVOCATION_MODE'] = mode
                self.build('first', self.installed)
                result = self.update(self.build('second'))
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("'first'", self.installed.read_text())
                self.assertEqual(self.running(), [])
                self.assertFalse((self.root / 'systemctl').exists())

    def test_undrained_operation_names_exact_attempt_and_manual_remediation(self):
        self.env['REVOCATION_MODE'] = 'resources'
        self.build('first', self.installed)
        result = self.update(self.build('second'))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('33333333-3333-4333-8333-333333333333', result.stderr)
        self.assertIn('helper 7654321 is running', result.stderr)
        calls = [call for call in self.revocation_calls() if call['args'][:2] == ['desktop', 'revoke']]
        self.assertEqual(len(calls), 1)
        self.assertIn(calls[0]['request_id'], result.stderr)
        self.assertIn('stop only that helper yourself', result.stderr)
        self.assertIn('restart only hgs-desktop', result.stderr)
        self.assertLess(result.stderr.index('Wait for'), result.stderr.index('restart only'))
        self.assertLess(result.stderr.index('restart only'), result.stderr.index('Reboot only'))
        self.assertEqual(self.running(), [])
        self.assertFalse((self.root / 'systemctl').exists())

    def test_rollback_without_a_copy_changes_nothing(self):
        self.build('first', self.installed)
        result = self.rollback()
        self.assertEqual(result.returncode, 1)
        self.assertIn('there is no rollback copy', result.stderr)
        self.assertIn("'first'", self.installed.read_text())

    def test_an_unnumbered_build_never_replaces_a_numbered_dev_install(self):
        # AGENTS.md and docs/reference.md: the live update and HGS_TRAY_RESTART=0 refuse alike.
        self.build('dev-1413', self.installed)
        result = self.update(self.build('second'))
        self.assertEqual(result.returncode, 1)
        self.assertIn('Dev build 1413', result.stderr)
        self.assertIn('ZERUS_LOCAL_PREVIEW_LABEL', result.stderr)
        install = self.installer(self.build('third'))
        result = subprocess.run(['bash', str(install)], env=dict(self.env, HGS_TRAY_RESTART='0'),
                                capture_output=True, text=True, timeout=90)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Dev build 1413', result.stderr)
        self.assertIn("'dev-1413'", self.installed.read_text())
        self.assertFalse((self.root / 'systemctl').exists(), 'nothing was started or enabled')
        result = self.update(self.build('dev-1414'))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("'dev-1414'", self.installed.read_text())
    def test_a_refused_install_changes_nothing_in_either_mode(self):
        # Important: the guard runs right after the build, before the plugin, the
        # icons, the launcher and the unit, with and without the live update.
        plugin = self.home / '.local/lib/hgs-tray/plugins/libhgs-tray-native-ui.so'
        written = [plugin, self.home / '.config/systemd/user/hgs-tray.service',
                   self.home / '.local/share/applications/hgs-tray.desktop', self.home / '.local/share/icons']
        for restart in ('1', '0'):
            with self.subTest(restart=restart):
                for path in written:
                    shutil.rmtree(path) if path.is_dir() else path.unlink(missing_ok=True)
                (self.root / 'systemctl').unlink(missing_ok=True)
                self.build('dev-1413', self.installed)
                pid = self.launch('dev-1413') if restart == '1' else None
                install = self.installer(self.build('third'), b'new plugin')
                result = subprocess.run(['bash', str(install)], env=dict(self.env, HGS_TRAY_RESTART=restart),
                                        capture_output=True, text=True, timeout=90)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('Dev build 1413', result.stderr)
                self.assertIn('Nothing was replaced', result.stderr)
                self.assertIn("'dev-1413'", self.installed.read_text())
                self.assertEqual([path for path in written if path.exists()], [])
                self.assertFalse((self.root / 'systemctl').exists(), 'nothing was started or enabled')
                if pid:
                    self.assertEqual(self.running(), [('dev-1413', pid)])


class InstallerOrder(unittest.TestCase):
    def test_the_guard_runs_before_the_first_change_on_macos_too(self):
        script = (REPO / 'tray/install.sh').read_text()
        mac = script[:script.index('# --- 3. LaunchAgent')]
        check = mac.index('update-installed.py" --check')
        self.assertLess(check, mac.index('mkdir -p "${HOME}/Applications"'))
        self.assertLess(check, mac.index('rm -rf "${APP_DEST}"'))
        self.assertLess(check, script.index('printf \'%s\\n\' "${NEW_PLIST}" > "${PLIST_DEST}"'))
        linux = script[script.index('# --- 2. binary'):]
        self.assertLess(linux.index('update-installed.py" "${CHECK[@]}"'), linux.index('install -Dm755'))


if __name__ == '__main__':
    unittest.main()
