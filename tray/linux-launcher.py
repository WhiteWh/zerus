#!/usr/bin/env python3
"""Local source-install launcher; published packages keep the stock launch path."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

SETTING = Path('share/hgs/local-webkit.json')
LAUNCHER = Path('libexec/hgs-tray-launch')
ENV_MARKER = 'HGS_LINUX_WEBKIT_ORIGINAL_ENV'
LIBRARIES = ('gtk/lib/libgtk-3.so.0', 'gtk/lib/libgdk-3.so.0',
             'lib/libwebkit2gtk-4.1.so.0', 'lib/libjavascriptcoregtk-4.1.so.0')
PROCESSES = tuple(f'libexec/webkit2gtk-4.1/{name}' for name in
                  ('WebKitWebProcess', 'WebKitNetworkProcess', 'WebKitGPUProcess'))


def available(prefix):
    return (all((prefix / name).is_file() for name in LIBRARIES)
            and all((prefix / name).is_file() and os.access(prefix / name, os.X_OK) for name in PROCESSES))


def read_prefix(install):
    try:
        with (install / SETTING).open() as setting:
            value = json.loads(setting.read(4097))
        prefix = value['prefix']
        if value.get('version') == 1 and isinstance(prefix, str) and valid_path(prefix):
            return Path(prefix)
    except (OSError, ValueError, KeyError, TypeError):
        pass
    return None


def valid_path(value):
    # LD_LIBRARY_PATH cannot encode a colon inside one directory.
    return Path(value).is_absolute() and not any(c in value for c in ':\n\r\0')


def replace(path, content, mode):
    if path.is_file() and path.read_bytes() == content and path.stat().st_mode & 0o777 == mode:
        return False
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as out:
        temporary = Path(out.name)
        try:
            out.write(content)
            out.flush()
            os.fchmod(out.fileno(), mode)
            os.replace(temporary, path)
        finally:
            temporary.unlink(missing_ok=True)
    return True


def install_launcher(install):
    changed = False
    setting = install / SETTING
    if 'HGS_LINUX_WEBKIT_PREFIX' in os.environ:
        value = os.environ['HGS_LINUX_WEBKIT_PREFIX']
        if value:
            if not valid_path(value) or not available(Path(value)):
                raise ValueError('HGS_LINUX_WEBKIT_PREFIX must name an existing absolute WebKitGTK/GTK3 prefix '
                                 'with libraries and WebKit processes (no colon or newline)')
            content = (json.dumps({'version': 1, 'prefix': str(Path(value))}) + '\n').encode()
            changed |= replace(setting, content, 0o600)
        else:
            for path in (setting, install / LAUNCHER):
                if path.exists():
                    path.unlink()
                    changed = True
    # Unset retains the local selection, including when its private disk is absent.
    if setting.exists():
        changed |= replace(install / LAUNCHER, Path(__file__).read_bytes(), 0o755)
    if changed:
        print('changed')


def environment(install, binary):
    env = os.environ.copy()
    prefix = read_prefix(install)
    if prefix is None:
        return env
    if not available(prefix):
        print('hgs-tray: local WebKitGTK prefix unavailable; using system libraries', file=sys.stderr)
        return env
    # Qt rollback and older, unidentifiable GUIs keep their stock environment.
    try:
        info = subprocess.run([str(binary), '--build-info'], capture_output=True,
                              text=True, timeout=5, check=True)
        identity = json.loads(info.stdout)
        if (not isinstance(identity, dict) or identity.get('ZerusGUI') != 'tauri'
                or identity.get('localWebKitChildEnvironment') != 1):
            return env
    except (OSError, ValueError, subprocess.SubprocessError):
        return env
    libraries = f'{prefix}/gtk/lib:{prefix}/lib'
    if env.get('LD_LIBRARY_PATH'):
        libraries += ':' + env['LD_LIBRARY_PATH']
    # Zerus restores this environment at each non-WebKit child boundary. Encode
    # bytes so an unset, empty or non-UTF-8 original value survives exactly.
    original = {key: (os.fsencode(env[key]).hex() if key in env else None)
                for key in ('LD_LIBRARY_PATH', 'WEBKIT_EXEC_PATH')}
    env[ENV_MARKER] = json.dumps({'version': 1, **original})
    env['LD_LIBRARY_PATH'] = libraries
    env['WEBKIT_EXEC_PATH'] = str(prefix / 'libexec/webkit2gtk-4.1')
    return env


def main():
    if len(sys.argv) == 3 and sys.argv[1] == '--install':
        try:
            install_launcher(Path(sys.argv[2]).absolute())
        except (OSError, ValueError) as error:
            print(f'hgs-tray: {error}', file=sys.stderr)
            return 1
    else:
        install = Path(__file__).resolve().parent.parent
        binary = install / 'bin/hgs-tray'
        os.execve(binary, [str(binary), *sys.argv[1:]], environment(install, binary))
    return 0


if __name__ == '__main__':
    sys.exit(main())
