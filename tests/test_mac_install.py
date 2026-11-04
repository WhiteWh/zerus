"""Installer configuration checks with fake transports and an isolated home."""
import json
import os
import shlex
from pathlib import Path
import plistlib
import re
import subprocess
import tempfile
import tomllib
import unittest

REPO = Path(__file__).resolve().parents[1]


def desktop_preflight_fixture(root, tools):
    """Build-only fixtures stop before signing; the desktop installer has its own contracts."""
    scripts = root / "scripts"; scripts.mkdir(exist_ok=True)
    (scripts / "install-macos-desktop-service.py").write_text(
        "import sys\nassert sys.argv[-1] == '--check-only'\n")
    (tools / "hgs").write_text("#!/bin/sh\nexit 0\n")
    (tools / "hgs").chmod(0o755)


class MacInstall(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='zerus-install-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.home = self.root / 'home'
        (self.home / '.config/hgs').mkdir(parents=True)
        (self.home / '.local/bin').mkdir(parents=True)
        self.script(self.home / '.local/bin/hgs', '#!/bin/sh\necho fixture\n')
        self.script(self.bin / 'ssh', '''#!/usr/bin/env python3
import os, pathlib, shlex, subprocess, sys
assert sys.argv[1:3] == ['-o', 'BatchMode=yes'], sys.argv
data = sys.stdin.buffer.read()
with open(os.environ['TRANSPORT_LOG'], 'a') as log:
    log.write('ssh\\n')
command = sys.argv[-1]
# Native builds, the desktop installer and the live tmux server stay out of scope.
if command.endswith('bash -s') or 'tray/install.sh' in command or 'tmux source-file' in command:
    with open(os.environ['TRANSPORT_LOG'], 'a') as log:
        log.write('remote ' + command + '\\n')
    sys.exit(0)
if command.startswith('bash -s -- '):
    sys.exit(subprocess.run(shlex.split(command), input=data).returncode)
sys.exit(subprocess.run(['sh', '-c', command], input=data).returncode)
''')
        self.script(self.bin / 'scp', '#!/bin/sh\n[ "$1" = -o ] && [ "$2" = BatchMode=yes ]\n')
        self.script(self.bin / 'tar', '#!/bin/sh\nprintf "%s\\n" "$*" >> "$TRANSPORT_LOG.tar"\nexit 0\n')
        self.script(self.bin / 'tmux', '#!/bin/sh\necho fixture\n')
        self.script(self.bin / 'hostname', '#!/bin/sh\necho fixture-mac\n')
        self.env = {k: v for k, v in os.environ.items() if not k.startswith('HGS_MAC_')}
        self.env.update(HOME=str(self.home), PATH=str(self.bin) + os.pathsep + os.environ['PATH'],
                        TRANSPORT_LOG=str(self.root / 'transport.log'))

    def script(self, path, text):
        path.write_text(text)
        path.chmod(0o755)

    def run_install(self, *args, **settings):
        return subprocess.run(['bash', str(REPO / 'mac-install.sh'), *args],
                              env=dict(self.env, **settings), input='', text=True,
                              capture_output=True, timeout=15)

    def config(self):
        return (self.home / '.config/hgs/config').read_text()

    def test_mobile_connector_sources_travel_and_its_build_stays_opt_in(self):
        for value in ("", "1", "0"):
            with self.subTest(HGS_MOBILE_CONNECTOR=value):
                (self.root / 'transport.log.tar').unlink(missing_ok=True)
                (self.root / 'transport.log').unlink(missing_ok=True)
                result = self.run_install('fixture-mac', '--no-tray', HGS_MOBILE_CONNECTOR=value)
                self.assertEqual(result.returncode, 0, result.stderr)
                sent = (self.root / 'transport.log.tar').read_text().splitlines()[0].split()
                for path in ('services/mobile/connector/Cargo.toml', 'services/mobile/connector/Cargo.lock',
                             'services/mobile/connector/.cargo', 'services/mobile/connector/src',
                             'services/mobile/relay/Cargo.toml', 'services/mobile/relay/src',
                             'services/mobile/crypto/Cargo.toml', 'services/mobile/crypto/src',
                             'crates/replica/Cargo.toml', 'crates/replica/Cargo.lock',
                             'crates/replica/.cargo', 'crates/replica/src',
                             'crates/watch-protocol/Cargo.toml', 'crates/watch-protocol/Cargo.lock',
                             'crates/watch-protocol/.cargo', 'crates/watch-protocol/src'):
                    self.assertIn(path, sent)
                self.assertFalse(any('target' in path.split('/') for path in sent), sent)
                builds = [line for line in (self.root / 'transport.log').read_text().splitlines()
                          if line.startswith('remote ') and line.endswith('bash -s')]
                self.assertEqual(builds, [f"remote HGS_MOBILE_CONNECTOR='{value}' bash -s"])

    def test_destination_required_before_any_connection(self):
        result = self.run_install()
        self.assertEqual(result.returncode, 2)
        self.assertFalse((self.root / 'transport.log').exists())

    def test_local_signing_selector_travels_as_one_literal_value(self):
        selector = "Developer ID Application: Example's $(touch unexpected) `id` (ABCDE12345)"
        result = self.run_install('fixture-mac', HGS_MACOS_SIGN_IDENTITY=selector)
        self.assertEqual(result.returncode, 0, result.stderr)
        commands = [line[len('remote '):] for line in (self.root / 'transport.log').read_text().splitlines()
                    if 'tray/install.sh' in line]
        self.assertEqual(len(commands), 1)
        self.assertEqual(shlex.split(commands[0]), ['HGS_MAC_CLI_BUILD=~/.local/src/hgs/.rust-build/target/release/hgs', 'HGS_MACOS_SIGN_IDENTITY=' + selector, 'HGS_MAC_GUI=client',
                                                  'bash', '~/.local/src/hgs/tray/install.sh'])
        self.assertFalse((self.root / 'unexpected').exists())

    def test_next_is_forwarded_by_default_and_qt_requires_an_explicit_choice(self):
        for choice in (None, '', 'client', 'qt'):
            with self.subTest(choice=choice):
                (self.root / 'transport.log').unlink(missing_ok=True)
                settings = {} if choice is None else {'HGS_MAC_GUI': choice}
                result = self.run_install('fixture-mac', **settings)
                self.assertEqual(result.returncode, 0, result.stderr)
                commands = [line[len('remote '):] for line in (self.root / 'transport.log').read_text().splitlines()
                            if 'tray/install.sh' in line]
                self.assertEqual(len(commands), 1)
                self.assertEqual(shlex.split(commands[0]), ['HGS_MAC_CLI_BUILD=~/.local/src/hgs/.rust-build/target/release/hgs', 'HGS_MACOS_SIGN_IDENTITY=',
                                                          'HGS_MAC_GUI=' + (choice or 'client'),
                                                          'bash', '~/.local/src/hgs/tray/install.sh'])

    def test_the_client_sources_travel_and_the_gui_choice_is_forwarded(self):
        # Source folders are replaced, so a file deleted upstream is not built in.
        stale = self.home / '.local/src/hgs/client/src-tauri/capabilities/removed.json'
        stale.parent.mkdir(parents=True)
        stale.write_text('{}')
        stale_native = self.home / '.local/src/hgs/crates/freedesktop-notify/src/removed.rs'
        stale_native.parent.mkdir(parents=True)
        stale_native.write_text('previous source')
        cache = self.home / '.local/src/hgs/client/src-tauri/target/kept'
        cache.parent.mkdir(parents=True)
        cache.write_text('build cache')
        native_cache = self.home / '.local/src/hgs/crates/freedesktop-notify/target/kept'
        native_cache.parent.mkdir(parents=True)
        native_cache.write_text('build cache')
        result = self.run_install('fixture-mac', HGS_MAC_GUI='client')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(stale.exists())
        self.assertFalse(stale_native.exists())
        self.assertTrue(cache.exists())
        self.assertTrue(native_cache.exists())
        tray = [line.split() for line in (self.root / 'transport.log.tar').read_text().splitlines()
                if 'tray' in line.split()]
        self.assertEqual(len(tray), 1)
        for expected in ('client', 'VERSION', 'DEV_VERSION', '--exclude=client/node_modules',
                         '--exclude=client/*/target', '--exclude=client/dist',
                         # client/core and client/src-tauri take it by path (../../crates).
                         'crates/watch-protocol/Cargo.toml', 'crates/watch-protocol/src',
                         'crates/freedesktop-notify/Cargo.toml', 'crates/freedesktop-notify/src'):
            self.assertIn(expected, tray[0])
        commands = [line for line in (self.root / 'transport.log').read_text().splitlines() if 'tray/install.sh' in line]
        self.assertIn("HGS_MAC_GUI='client'", commands[0])

    def test_every_crate_taken_by_path_travels(self):
        # A crate hgs or the client takes by path, directly or through another crate,
        # must reach the Mac, or its build fails there while Linux still builds.
        def closure(manifest):
            found, pending = set(), [manifest]
            while pending:
                current = pending.pop()
                manifest = tomllib.loads(current.read_text())
                owners = [manifest, *manifest.get('target', {}).values()]
                for owner in owners:
                    for group in ('dependencies', 'build-dependencies'):
                        for spec in owner.get(group, {}).values():
                            if not isinstance(spec, dict) or 'path' not in spec:
                                continue
                            crate = (current.parent / spec['path']).resolve().relative_to(REPO)
                            if crate not in found:
                                found.add(crate)
                                pending.append(REPO / crate / 'Cargo.toml')
            return found
        cli = closure(REPO / 'Cargo.toml')
        client = closure(REPO / 'client/core/Cargo.toml') | closure(REPO / 'client/src-tauri/Cargo.toml')
        self.assertIn(Path('crates/desktop-read-policy'), cli & client)
        self.assertIn(Path('client/core'), cli)
        result = self.run_install('fixture-mac', HGS_MAC_GUI='client')
        self.assertEqual(result.returncode, 0, result.stderr)
        sent = [line.split() for line in (self.root / 'transport.log.tar').read_text().splitlines()]
        cli_tar = next(line for line in sent if 'hgs_state.py' in line)
        tray_tar = next(line for line in sent if 'tray' in line)
        for crates, tar in ((cli, cli_tar), (client, tray_tar)):
            for crate in sorted(crates):
                with self.subTest(crate=str(crate), tray=tar is tray_tar):
                    for suffix in ('Cargo.toml', 'src'):
                        required = crate / suffix
                        # The GUI ships its whole client tree; the CLI names
                        # only the core manifest/src, never GUI caches.
                        self.assertTrue(any(Path(entry) == required or Path(entry) in required.parents
                                            for entry in tar), (required, tar))
        # The retained checkout serves old launch paths after a successful build.
        # It needs the same transitive source closure as the private build tree.
        script = (REPO / 'mac-install.sh').read_text()
        copy_line = next(line for line in script.splitlines() if line.startswith('copy="'))
        retained = shlex.split(copy_line.removeprefix('copy='))[0].split()
        for crate in cli:
            for suffix in ('Cargo.toml', 'src'):
                required = crate / suffix
                self.assertTrue(any(Path(entry) == required or Path(entry) in required.parents
                                    for entry in retained), (required, retained))
        (self.root / 'transport.log.tar').unlink()
        result = self.run_install('fixture-mac', '--no-tray')
        self.assertEqual(result.returncode, 0, result.stderr)
        cli_only = (self.root / 'transport.log.tar').read_text().split()
        self.assertIn('client/core/Cargo.toml', cli_only)
        self.assertIn('client/core/src', cli_only)
        self.assertNotIn('client', cli_only)
        self.assertFalse(any('target' in Path(entry).parts for entry in cli_only))

    def test_default_does_not_create_ssh_or_reverse_peer(self):
        result = self.run_install('fixture-mac', '--no-tray')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('HGS_PEERS=""', self.config())
        self.assertIn('HGS_TAB_COLORS=""', self.config())
        self.assertFalse((self.home / '.ssh').exists())

    def test_existing_alias_does_not_rewrite_ssh(self):
        ssh = self.home / '.ssh/config'
        ssh.parent.mkdir()
        original = 'Host workstation\n  HostName workstation.example\n'
        ssh.write_text(original)
        result = self.run_install('fixture-mac', '--no-tray', HGS_MAC_PEER='workstation')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(ssh.read_text(), original)
        self.assertIn('HGS_PEERS="workstation"', self.config())

    def test_explicit_new_peer_is_idempotent(self):
        settings = dict(HGS_MAC_PEER='workstation', HGS_MAC_PEER_HOST='workstation.example',
                        HGS_MAC_PEER_USER='example', HGS_MAC_PEER_KEY='~/.ssh/example_key')
        for _ in range(2):
            result = self.run_install('fixture-mac', '--no-tray', **settings)
            self.assertEqual(result.returncode, 0, result.stderr)
        ssh = self.home / '.ssh/config'
        self.assertEqual(ssh.read_text().count('Host workstation\n'), 1)
        self.assertIn('  IdentityFile ~/.ssh/example_key\n', ssh.read_text())
        self.assertEqual(ssh.stat().st_mode & 0o777, 0o600)

    def test_existing_application_config_is_preserved(self):
        original = 'HGS_SELF="custom"\nHGS_PEERS="existing"\nHGS_TAB=0\nHGS_TAB_COLORS="custom=#123456"\n'
        (self.home / '.config/hgs/config').write_text(original)
        result = self.run_install('fixture-mac', '--no-tray')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.config(), original)

    def test_moved_sources_do_not_survive_on_the_mac(self):
        (self.bin / 'tar').unlink()  # pack and unpack for real
        build = self.home / '.local/src/hgs/.rust-build'
        for stale in ('src/platform.rs', 'target/release/keep'):
            (build / stale).parent.mkdir(parents=True, exist_ok=True)
            (build / stale).write_text('previous')
        result = self.run_install('fixture-mac')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((build / 'src/platform.rs').exists())
        self.assertTrue((build / 'src/platform/mod.rs').is_file())
        self.assertEqual((build / 'target/release/keep').read_text(), 'previous')
        # The desktop build reads its Dev base version beside tray/.
        self.assertEqual((self.home / '.local/src/hgs/DEV_VERSION').read_text(),
                         (REPO / 'DEV_VERSION').read_text())
        self.assertTrue((self.home / '.local/src/hgs/tray/CMakeLists.txt').is_file())

    def test_incomplete_or_injected_peer_config_never_connects(self):
        for settings in [dict(HGS_MAC_PEER_HOST='workstation.example'),
                         dict(HGS_MAC_PEER='workstation', HGS_MAC_PEER_USER='example'),
                         dict(HGS_MAC_PEER='workstation\nHost injected')]:
            with self.subTest(settings=settings):
                self.assertEqual(self.run_install('fixture-mac', '--no-tray', **settings).returncode, 2)
        self.assertFalse((self.root / 'transport.log').exists())


class TrayInstallGui(unittest.TestCase):
    """The macOS desktop build chooses the Tauri client, stopped before signing."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='zerus-tray-gui-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for name in ('tray/install.sh', 'tray/update-installed.py', 'tray/prepare-local-macos.py', 'tray/sign-local-macos.py', 'client/scripts/switch-config.py',
                     'client/src-tauri/tauri.switch.conf.json', 'client/src-tauri/Info.switch.plist',
                     'VERSION', 'DEV_VERSION'):
            (self.root / name).parent.mkdir(parents=True, exist_ok=True)
            (self.root / name).write_bytes((REPO / name).read_bytes())
        tools = self.root / 'bin'
        tools.mkdir()
        desktop_preflight_fixture(self.root, tools)
        self.log = self.root / 'tools.log'
        for name, text in {
            'uname': '#!/bin/sh\necho Darwin\n',
            'brew': '#!/bin/sh\necho /opt/fixture/qt\n',
            'cmake': f'#!/bin/sh\necho "cmake $*" >> "{self.log}"\nexit 3\n',
            # npm ci succeeds; the build records its Cargo settings and stops the
            # installer before signing.
            'npm': f'#!/bin/sh\necho "npm $*" >> "{self.log}"\n[ "$1" = ci ] && exit 0\n'
                   f'echo "$CARGO_TARGET_DIR $CARGO_BUILD_JOBS" > "{self.root}/cargo.env"\nexit 3\n',
        }.items():
            (tools / name).write_text(text)
            (tools / name).chmod(0o755)
        self.home = self.root / 'home'
        # The caller's GUI choice and restart mode never reach these runs.
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith('HGS_MAC_') and k not in ('HGS_TRAY_RESTART', 'CARGO_TARGET_DIR', 'CARGO_BUILD_JOBS')}
        self.env.update(HOME=str(self.home), PATH=str(tools) + os.pathsep + os.environ['PATH'])
        raw_cli = self.root / 'target/release/hgs'
        raw_cli.parent.mkdir(parents=True)
        raw_cli.write_bytes(b'\xcf\xfa\xed\xfe synthetic CLI'); raw_cli.chmod(0o755)
        stable = tools / 'hgs'; stable.write_bytes(raw_cli.read_bytes()); stable.chmod(0o755)

    def install(self, **settings):
        return subprocess.run(['bash', str(self.root / 'tray/install.sh')], env=dict(self.env, **settings),
                              text=True, capture_output=True, timeout=30)

    def test_the_client_is_built_with_the_switch_configuration(self):
        result = self.install(HGS_MAC_GUI='client')
        self.assertEqual(result.returncode, 3, result.stderr)
        log = self.log.read_text().splitlines()
        config = self.root / 'client/src-tauri/target/switch.conf.json'
        self.assertEqual(log, ['npm ci --no-audit --no-fund',
                               f'npm exec -- tauri build --config {config} --bundles app'])
        self.assertEqual(json.loads(config.read_text())['identifier'], 'com.hgdev.hgs-tray.local')
        jobs = subprocess.run(['getconf', '_NPROCESSORS_ONLN'], capture_output=True, text=True).stdout.strip() or '4'
        self.assertEqual((self.root / 'cargo.env').read_text().split(),
                         [str(self.root / 'client/src-tauri/target'), jobs])

    def test_the_bundle_is_built_where_the_installer_takes_it_from(self):
        # A caller's CARGO_TARGET_DIR would leave an older Zerus.app to be installed.
        result = self.install(HGS_MAC_GUI='client', CARGO_TARGET_DIR=str(self.root / 'elsewhere'),
                              HGS_MAC_CLI_BUILD=str(self.root / 'target/release/hgs'), CARGO_BUILD_JOBS='2')
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertEqual((self.root / 'cargo.env').read_text().split(),
                         [str(self.root / 'client/src-tauri/target'), '2'])

    def installed_gui(self, kind):
        if kind is None:
            return
        app = self.home / 'Applications/hgs-tray.app/Contents'
        app.mkdir(parents=True, exist_ok=True)
        info = {'CFBundleIdentifier': 'com.hgdev.hgs-tray'}
        if kind == 'tauri':
            info['ZerusGUI'] = kind
        (app / 'Info.plist').write_bytes(plistlib.dumps(info))

    def test_fresh_and_upgrade_installs_default_to_the_client(self):
        for kind in (None, 'qt', 'tauri'):
            for choice in (None, ''):
                with self.subTest(installed=kind, choice=choice):
                    self.installed_gui(kind)
                    self.log.unlink(missing_ok=True)
                    settings = {} if choice is None else {'HGS_MAC_GUI': choice}
                    result = self.install(**settings)
                    self.assertEqual(result.returncode, 3, result.stderr)
                    self.assertTrue(self.log.read_text().startswith('npm ci'))

    def test_qt_remains_an_explicit_fallback_for_fresh_and_upgrade_installs(self):
        for kind in (None, 'qt', 'tauri'):
            with self.subTest(installed=kind):
                self.installed_gui(kind)
                self.log.unlink(missing_ok=True)
                result = self.install(HGS_MAC_GUI='qt')
                self.assertEqual(result.returncode, 3, result.stderr)
                self.assertTrue(self.log.read_text().startswith('cmake'))

    def test_an_invalid_gui_choice_refuses_before_building(self):
        result = self.install(HGS_MAC_GUI='other')
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertFalse(self.log.exists())

    def test_cli_build_skew_stops_before_building_or_replacing_the_app(self):
        self.installed_gui('qt')
        info = self.home / 'Applications/hgs-tray.app/Contents/Info.plist'
        before = info.read_bytes()
        raw = self.root / 'target/release/hgs'
        raw.write_bytes(b'\xcf\xfa\xed\xfe newer CLI')
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Run ./install.sh (or mac-install.sh)', result.stderr)
        self.assertFalse(self.log.exists())
        self.assertEqual(info.read_bytes(), before)
        self.assertFalse((self.home / 'Library/LaunchAgents').exists())

    def test_pair_swap_precedes_service_selection_and_preserves_the_stable_cli(self):
        # Real staging/signing Python paths, with modelled codesign/launchd and
        # updater only. No Mach-O fixture, GUI or notification is executed.
        tools = self.root / 'bin'
        app = self.root / 'client/src-tauri/target/release/bundle/macos/Zerus.app'
        code = app / 'Contents/MacOS/hgs-tray'
        code.parent.mkdir(parents=True)
        code.write_bytes(b'\xcf\xfa\xed\xfe synthetic GUI'); code.chmod(0o755)
        info = app / 'Contents/Info.plist'
        info.write_bytes(plistlib.dumps({'CFBundleIdentifier': 'com.hgdev.hgs-tray.local',
            'CFBundleExecutable': 'hgs-tray', 'ZerusGUI': 'tauri'}))
        original = info.read_bytes()
        installed = self.home / 'Applications/hgs-tray.app'
        stable = tools / 'hgs'; stable_bytes = stable.read_bytes()
        (tools / 'npm').write_text('#!/bin/sh\nexit 0\n')
        calls = self.root / 'install-order.log'
        updater = self.root / 'tray/update-installed.py'
        updater.write_text('''import sys, shutil
from pathlib import Path
if '--check' not in sys.argv:
    source, dest = map(Path, sys.argv[1:])
    assert (source / 'Contents/MacOS/hgs').is_file()
    if dest.exists(): shutil.rmtree(dest)
    shutil.copytree(source, dest)
    with Path(''' + repr(str(calls)) + ''').open('a') as log: log.write('swap\\n')
''')
        desktop = self.root / 'scripts/install-macos-desktop-service.py'
        desktop.write_text('''import sys
from pathlib import Path
if '--check-only' not in sys.argv:
    selected = Path(sys.argv[1])
    assert selected == Path(''' + repr(str(installed / 'Contents/MacOS/hgs')) + ''')
    assert selected.is_file()
    assert selected.parents[2].stat().st_mode & 0o777 == 0o755
    assert sys.argv[2:] == ['--restart', '--runner-hgs', ''' + repr(str(stable)) + ''']
    with Path(''' + repr(str(calls)) + ''').open('a') as log: log.write('desktop\\n')
''')
        for name, body in {
            'codesign': '''#!/usr/bin/python3
import sys, hashlib, plistlib
from pathlib import Path
args = sys.argv[1:]; path = Path(args[-1])
if not path.exists(): sys.exit(1)
if '--sign' in args and path.is_file():
    with path.open('ab') as stream: stream.write(b' synthetic signature')
if '-d' in args:
    identifier = 'hgs' if path.is_file() else 'com.hgdev.hgs-tray'
    if '-r-' in args: print('designated => identifier "'+identifier+'"')
    else:
        data = path.read_bytes() if path.is_file() else (path/'Contents/Info.plist').read_bytes()
        print('Identifier='+identifier+'\\nAuthority=Developer ID Application: Example (ABCDE12345)'
              +'\\nTeamIdentifier=ABCDE12345\\nCDHash='+hashlib.sha256(data).hexdigest()[:40])
''',
            'security': '#!/bin/sh\necho \'1) ' + 'A'*40 + ' "Developer ID Application: Example (ABCDE12345)"\'\n',
            'launchctl': '#!/bin/sh\nexit 0\n',
        }.items():
            path = tools / name; path.write_text(body); path.chmod(0o755)
        template = self.root / 'tray/packaging/com.hgdev.hgs-tray.plist'
        template.parent.mkdir()
        template.write_bytes((REPO / 'tray/packaging/com.hgdev.hgs-tray.plist').read_bytes())
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(calls.read_text().splitlines(), ['swap', 'desktop'])
        self.assertEqual(stable.read_bytes(), stable_bytes)
        self.assertEqual((self.root / 'target/release/hgs').read_bytes(), stable_bytes)
        self.assertNotEqual((installed / 'Contents/MacOS/hgs').read_bytes(), stable_bytes)
        self.assertEqual(info.read_bytes(), original)
        self.assertNotIn('HGSDesktopNotificationHostABI', plistlib.loads((installed / 'Contents/Info.plist').read_bytes()))
        gui_text = (self.home / 'Library/LaunchAgents/com.hgdev.hgs-tray.plist').read_text()
        gui = plistlib.loads(re.sub(r'<!--.*?-->', '', gui_text, flags=re.S).encode())
        self.assertEqual(gui['ProgramArguments'][-2:], ['--hgs', str(stable)])


class TrayInstallCache(unittest.TestCase):
    """The macOS desktop build, stopped after configuring with fake tools."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='zerus-tray-install-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.tray = self.root / 'tray'
        self.build = self.tray / 'build'
        (self.build / 'CMakeFiles').mkdir(parents=True)
        (self.tray / 'install.sh').write_text((REPO / 'tray/install.sh').read_text())
        tools = self.root / 'bin'
        tools.mkdir()
        log = self.root / 'cmake.log'
        desktop_preflight_fixture(self.root, tools)
        for name, text in {
            'uname': '#!/bin/sh\necho Darwin\n',
            'brew': '#!/bin/sh\necho /opt/fixture/qt\n',
            # Record whether configuration starts from the old cache, then stop
            # the installer before it touches any application or LaunchAgent.
            'cmake': f'#!/bin/sh\nif [ "$1" = --build ]; then exit 3; fi\n'
                     f'if [ -e "{self.build}/CMakeCache.txt" ]; then echo kept >> "{log}"; '
                     f'else echo fresh >> "{log}"; fi\n',
        }.items():
            (tools / name).write_text(text)
            (tools / name).chmod(0o755)
        self.log = log
        self.env = dict(os.environ, HOME=str(self.root / 'home'), HGS_MAC_GUI='qt',
                        PATH=str(tools) + os.pathsep + os.environ['PATH'])

    def configure(self, sdk):
        (self.build / 'CMakeCache.txt').write_text(
            f'CMAKE_OSX_SYSROOT:PATH={sdk}\nCUPS_LIBRARIES:FILEPATH={sdk}/usr/lib/libcups.tbd\n'
            f'FIND_PACKAGE_MESSAGE_DETAILS_Cups:INTERNAL=[{sdk}/usr/lib/libcups.tbd][{sdk}/usr/include][v2.3]\n')
        result = subprocess.run(['bash', str(self.tray / 'install.sh')], env=self.env, text=True,
                                capture_output=True, timeout=15)
        self.assertEqual(result.returncode, 3, result.stderr)
        return self.log.read_text().split()[-1], result.stdout

    def test_cache_for_a_removed_sdk_is_reconfigured_from_scratch(self):
        state, output = self.configure(self.root / 'Xcode.app/SDKs/MacOSX26.0.sdk')
        self.assertEqual(state, 'fresh')
        self.assertIn('reconfiguring', output)
        self.assertFalse((self.build / 'CMakeFiles').exists())

    def test_cache_for_an_installed_sdk_is_kept(self):
        sdk = self.root / 'Xcode.app/SDKs/MacOSX27.0.sdk'
        sdk.mkdir(parents=True)
        self.assertEqual(self.configure(sdk)[0], 'kept')
        self.assertTrue((self.build / 'CMakeFiles').exists())


if __name__ == '__main__':
    unittest.main()
