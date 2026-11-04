"""Linux-only fixture supervisor: bounded descendants, no process-name cleanup.

Run a *compiled* test binary, not cargo, so compiler jobs do not share its
fixture budget. The separate subreaper owns only its original child subtree.
"""
import argparse
import ctypes
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def children(pid):
    result = set()
    try:
        for thread in (Path('/proc') / str(pid) / 'task').iterdir():
            try:
                result.update(map(int, (thread / 'children').read_text().split()))
            except (FileNotFoundError, PermissionError):
                pass
    except (FileNotFoundError, PermissionError):
        pass
    return result


def identity(pid):
    fields = (Path('/proc') / str(pid) / 'stat').read_text().rsplit(') ', 1)[1].split()
    return int(fields[1]), fields[19]


class Tree:
    def __init__(self, limit):
        self.owned = {}
        self.limit = limit

    def collect(self, freeze=False, deadline=None):
        pending = [os.getpid()]
        seen = set()
        while pending:
            if deadline is not None and time.monotonic() >= deadline:
                return
            parent = pending.pop()
            if parent in seen:
                continue
            seen.add(parent)
            if parent != os.getpid():
                entry = self.owned.get(parent)
                try:
                    if entry is None or identity(parent)[1] != entry[0]:
                        continue
                    signal.pidfd_send_signal(entry[1], 0)
                except (FileNotFoundError, ProcessLookupError, PermissionError):
                    continue
            for pid in children(parent):
                if deadline is not None and time.monotonic() >= deadline:
                    return
                try:
                    before = identity(pid)
                    if before[0] not in (parent, os.getpid()):
                        continue
                    fd = os.pidfd_open(pid)
                    try:
                        after = identity(pid)
                    except BaseException:
                        os.close(fd)
                        raise
                    if after != before:
                        os.close(fd)
                        continue
                    # The numeric parent must still be our retained original,
                    # rather than a reused PID whose children we just observed.
                    if before[0] == parent and parent != os.getpid():
                        try:
                            signal.pidfd_send_signal(self.owned[parent][1], 0)
                        except (ProcessLookupError, PermissionError):
                            os.close(fd)
                            continue
                    previous = self.owned.get(pid)
                    if previous:
                        os.close(previous[1])
                    self.owned[pid] = (before[1], fd)
                    if freeze:
                        signal.pidfd_send_signal(fd, signal.SIGSTOP)
                    pending.append(pid)
                    if not freeze and len(self.owned) > self.limit:
                        return
                except (FileNotFoundError, ProcessLookupError, PermissionError):
                    pass

    def prune(self):
        for pid, (_, fd) in list(self.owned.items()):
            try:
                signal.pidfd_send_signal(fd, 0)
            except ProcessLookupError:
                os.close(fd)
                del self.owned[pid]
            except PermissionError:
                # A sandbox transition may temporarily change signal rights.
                # Keep the original pidfd; unreadable never means exited.
                pass

    def cleanup(self):
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            # Freeze parents before discovering children; no PID/name/group
            # heuristic may add a process from outside this subreaper's tree.
            self.collect(freeze=True, deadline=deadline)
            for _, fd in self.owned.values():
                try:
                    signal.pidfd_send_signal(fd, signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass
            while True:
                try:
                    pid, _ = os.waitpid(-1, os.WNOHANG)
                    if not pid:
                        break
                except ChildProcessError:
                    # Only the kernel's no-children result proves cleanup.
                    # /proc discovery can be empty because access was denied.
                    return True
            time.sleep(.005)
        return False

    def close(self):
        for _, fd in self.owned.values():
            os.close(fd)


def supervise(command, seconds, limit):
    if not sys.platform.startswith('linux') or not hasattr(os, 'pidfd_open') or not hasattr(signal, 'pidfd_send_signal'):
        raise RuntimeError('bounded fixture requires Linux pidfds')
    if ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), 'cannot establish private fixture subreaper')
    tree = Tree(limit)
    child = None
    def interrupted(signum, _frame):
        raise SystemExit(128 + signum)
    previous = {sig: signal.signal(sig, interrupted) for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP)}
    try:
        child = subprocess.Popen(command, start_new_session=True)
        deadline = time.monotonic() + seconds
        while True:
            tree.prune()
            tree.collect()
            if len(tree.owned) > limit:
                print('bounded fixture: process budget exceeded', file=sys.stderr)
                return 125
            code = child.poll()
            if code is not None:
                return code if code >= 0 else 128 - code
            if time.monotonic() >= deadline:
                print('bounded fixture: deadline exceeded', file=sys.stderr)
                return 124
            time.sleep(.005)
    finally:
        # A cancelled fixture must still finish its original descendant cleanup.
        for sig in previous:
            signal.signal(sig, signal.SIG_IGN)
        clean = tree.cleanup()
        if not clean:
            # Content-free original identities for an explicit failed cleanup;
            # the cap is diagnostic only and never changes the cleanup result.
            identities = ','.join(f'{pid}:{entry[0]}' for pid, entry in list(tree.owned.items())[:128])
            print(f'bounded fixture: original identities {identities}; retained={len(tree.owned)}', file=sys.stderr, flush=True)
        tree.close()
        for sig, handler in previous.items():
            signal.signal(sig, handler)
        if child is not None:
            # cleanup already reaped only this supervisor's descendants.
            child.returncode = child.returncode if child.returncode is not None else -signal.SIGKILL
        if not clean:
            print('bounded fixture: original descendants remain', file=sys.stderr)
            os._exit(126)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--seconds', type=float, default=30)
    parser.add_argument('--max-processes', type=int, default=32)
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ['--'] else args.command
    if not command or not 0 < args.seconds <= 300 or not 1 <= args.max_processes <= 128:
        parser.error('positive bounded deadline/process limit and a command are required')
    sys.exit(supervise(command, args.seconds, args.max_processes))
