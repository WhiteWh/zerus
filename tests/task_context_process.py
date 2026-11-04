"""Private Linux subreaper for the keyless Codex task-context fixture only.

No process lookup or group signal is used. The kernel assigns
orphaned descendants of this fresh wrapper to it; actual waits settle them.
Unknown settlement never writes a receipt authorizing fixture removal.
"""
import argparse
import ctypes
import json
import os
from pathlib import Path
import subprocess
import sys
import time

MAX_DESCENDANTS = 64


def run(command, receipt, nonce, timeout, drain_seconds):
    if sys.platform != "linux":
        raise RuntimeError("Codex task-context custody requires a private Linux subreaper")
    if ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "PR_SET_CHILD_SUBREAPER")
    native = subprocess.Popen(command, stdin=subprocess.DEVNULL)
    expired = False
    try:
        code = native.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        expired = True
        # Only this original Popen child may be stopped, never a numeric PID
        # from an inventory or a process group shared with another program.
        native.kill()
        code = native.wait(timeout=2)
    end = time.monotonic() + drain_seconds
    reaped = 0
    while True:
        if time.monotonic() >= end:
            raise TimeoutError("native descendants did not settle; private fixture retained")
        try:
            pid, _ = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            break  # The kernel confirms that this private wrapper has no children.
        if pid:
            reaped += 1
            if reaped > MAX_DESCENDANTS:
                raise RuntimeError("native descendant count exceeded its bound; private fixture retained")
        else:
            time.sleep(min(.01, max(0, end - time.monotonic())))
    with Path(receipt).open("x") as output:
        json.dump(dict(settled=True, wrapper_pid=os.getpid(), nonce=nonce,
                       native_pid=native.pid, native_returncode=code, reaped_descendants=reaped), output)
    return 124 if expired else code


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--receipt", required=True)
    parser.add_argument("--nonce", required=True)
    parser.add_argument("--timeout", type=float, required=True)
    parser.add_argument("--drain-seconds", type=float, required=True)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if not (0 < args.timeout <= 120 and 0 < args.drain_seconds <= 6):
        parser.error("unexpected fixture deadline")
    if not args.command or args.command[0] != "--" or len(args.command) < 2:
        parser.error("expected one explicit native command after --")
    sys.exit(run(args.command[1:], args.receipt, args.nonce, args.timeout, args.drain_seconds))
