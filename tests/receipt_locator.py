"""The private, rebuildable receipt locator in a fixture's state directory.

`delivery-receipts-v1.sqlite3` is built by one coalesced background worker
(`hgs __state receipt-index-worker`) that an inspection or listing may start
(zerus-a4mub.11). Until it finishes, coverage is reported as unknown, and the
detached worker keeps writing after the command that started it has returned.
A fixture that compares two reads, the tree around an observation, or removes
its directory builds the locator first and waits for any worker, so the
measured command neither starts nor overlaps that background discovery.
"""
import os
from pathlib import Path
import subprocess
import time

try:
    import fcntl
except ImportError:  # Windows: these fixtures need tmux and never run there.
    fcntl = None

LOCATOR = 'delivery-receipts-v1.sqlite3'


def _held(lock):
    try:
        handle = open(lock, 'rb')
    except FileNotFoundError:
        return False
    with handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return True
        fcntl.flock(handle, fcntl.LOCK_UN)
        return False


def idle(state, timeout=30.0):
    """Wait until no locator worker runs, nor is being launched, in `state`.
    A just-spawned worker takes its lock moments later: two free checks apart."""
    if fcntl is None:
        return
    state = Path(state)
    until = time.monotonic() + timeout
    free = 0
    while free < 2:
        busy = _held(state / 'receipt-index-worker.lock') or _held(state / 'receipt-index-launch.lock')
        free = 0 if busy else free + 1
        if busy and time.monotonic() > until:
            raise AssertionError(f'the receipt locator worker in {state} did not finish')
        time.sleep(0.1)


def settle(hgs, env, timeout=60.0):
    """Build the locator now with the worker in the foreground: a later read
    sees complete coverage and starts no background discovery of its own."""
    state = Path(env['HGS_STATE_DIR'])
    idle(state, timeout)
    result = subprocess.run([str(hgs), '__state', 'receipt-index-worker'], env=env, text=True,
                            stdin=subprocess.DEVNULL, capture_output=True, timeout=timeout)
    if result.returncode != 0:
        raise AssertionError(f'the receipt locator worker failed: {result.stderr}')
    idle(state, timeout)


def reset(state, timeout=30.0):
    """Drop the rebuildable locator after a fixture deleted receipt originals.
    Production never deletes originals; a locator that still lists them can
    never verify its coverage again, so the next read rebuilds it instead."""
    state = Path(state)
    idle(state, timeout)
    for suffix in ('', '-wal', '-shm', '-journal'):
        try:
            os.unlink(state / (LOCATOR + suffix))
        except FileNotFoundError:
            pass
