"""Isolation that every GUI smoke uses before it starts an app (zerus-5a99f).

A switch-identity build (com.hgdev.hgs-tray) finds the canonical
single-instance guard at /tmp/com.hgdev.hgs-tray-<uid> (instance_guard.rs)
whatever HOME, TMPDIR or XDG say, so a start outside isolation reaches the
person's running GUI. `sandboxed()` wraps a smoke's outer command in bwrap
with a private /tmp, network, IPC and /run/user/<uid>; `refuse_reachable_guard()`
stops a worker before it starts an app while a canonical guard is visible.
A start with ZERUS_CLIENT_PERF set also never forwards to a GUI it did not
start (instance.rs). There is no unsandboxed mode.
"""
import os
import pathlib
import shutil
import sys


def canonical_guard() -> pathlib.Path:
    """instance_guard::directory(): a fixed /tmp path, never HOME or TMPDIR."""
    return pathlib.Path(f'/tmp/com.hgdev.hgs-tray-{os.geteuid()}')


def refuse_reachable_guard() -> None:
    """Inside the sandbox no canonical guard exists before the app makes one."""
    guard = canonical_guard()
    if guard.exists():
        raise SystemExit(f'refusing to start a GUI: the canonical GUI guard {guard} is reachable, '
                         'so this start would reach the running GUI. Start GUI smokes only through '
                         'gui_sandbox.sandboxed().')


def sandboxed(command, keep=(), read_only=()) -> list:
    """`command` inside bwrap: the host file system, but a private /tmp,
    /run/user/<uid>, network and IPC. Each existing path in `read_only` is
    bound read-only, so an absolute or passwd-derived write there fails loudly
    instead of reaching the person's files (zerus-2ldeb.4). Each existing path
    in `keep` stays shared and writable, including one below /tmp or below a
    read-only path, so the outer process sees its output."""
    if sys.platform != 'linux':
        raise SystemExit('GUI smokes start apps only on Linux, inside bwrap')
    bwrap = shutil.which('bwrap')
    if not bwrap:
        raise SystemExit('bubblewrap (bwrap) is required: there is no unsandboxed mode')
    wrapped = [bwrap, '--dev-bind', '/', '/', '--tmpfs', '/tmp']
    runtime = pathlib.Path(f'/run/user/{os.geteuid()}')
    if runtime.is_dir():
        wrapped += ['--tmpfs', str(runtime)]
    # Read-only first: a later `keep` bind below one stays writable.
    for path in read_only:
        path = pathlib.Path(path).resolve(strict=True)
        wrapped += ['--ro-bind', str(path), str(path)]
    for path in keep:
        path = pathlib.Path(path).resolve(strict=True)
        wrapped += ['--bind', str(path), str(path)]
    return wrapped + ['--unshare-net', '--unshare-ipc', '--die-with-parent', '--', *map(str, command)]
