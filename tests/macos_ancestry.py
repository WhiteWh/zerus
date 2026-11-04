"""Bounded read-only topology for the Darwin caller fixture, never admission."""
import ctypes as c
import json
import os
import sys
import time


class Short(c.Structure):
    _fields_ = ([(name, c.c_uint32) for name in ('pid', 'ppid', 'pgid', 'status')]
                + [('comm', c.c_char * 16)]
                + [(name, c.c_uint32) for name in ('flags', 'uid', 'gid', 'ruid', 'rgid', 'svuid', 'svgid', 'reserved')])


def topology():
    if sys.platform != 'darwin':
        raise RuntimeError('Darwin metadata only')
    lib = c.CDLL('/usr/lib/libproc.dylib', use_errno=True)
    lib.proc_pidinfo.argtypes = [c.c_int, c.c_int, c.c_uint64, c.c_void_p, c.c_int]
    lib.proc_pidinfo.restype = c.c_int
    lib.proc_pidpath.argtypes = [c.c_int, c.c_void_p, c.c_uint32]
    lib.proc_pidpath.restype = c.c_int
    if c.sizeof(Short) != 64:
        raise RuntimeError('Unexpected short-BSD layout')
    pid, seen, records = os.getpid(), set(), []
    until = time.monotonic() + 2
    while pid:
        if pid in seen or len(seen) >= 32 or time.monotonic() >= until:
            raise RuntimeError('Bounded topology could not complete')
        seen.add(pid)
        value = Short()
        c.set_errno(0)
        used = lib.proc_pidinfo(pid, 13, 0, c.byref(value), c.sizeof(value))
        error = c.get_errno()
        if used != c.sizeof(value) or value.pid != pid:
            raise RuntimeError(f'Short BSD unavailable: pid={pid}, bytes={used}, errno={error}')
        path = c.create_string_buffer(4096)
        used = lib.proc_pidpath(pid, path, len(path))
        if not 0 < used < len(path):
            raise RuntimeError(f'Native path unavailable: pid={pid}, errno={c.get_errno()}')
        records.append(dict(pid=pid, ppid=value.ppid, uid=value.uid, ruid=value.ruid,
                            path=path.value.decode('utf-8', 'strict'), sid=os.getsid(pid)))
        pid = value.ppid
    if not records or records[-1]['pid'] != 1:
        raise RuntimeError('Topology ended without system PID 1')
    if len(json.dumps(records).encode()) > 65536:
        raise RuntimeError('Topology exceeded its output bound')
    return records


def unsupported_hosted_foreign(records, owner, environment):
    # This affects only one fixture's hosted eligibility. Native permission
    # never reads these flags or this diagnostic. Recognized system paths still
    # run the full test, so bad credentials/session/image checks cannot be hidden.
    hosted = (environment.get('GITHUB_ACTIONS') == 'true'
              and environment.get('RUNNER_ENVIRONMENT') == 'github-hosted'
              and environment.get('RUNNER_OS') == 'macOS')
    if not hosted:
        return None
    return next((v for v in records if v['uid'] != owner
                 and v['path'] not in ('/usr/bin/login', '/sbin/launchd')), None)
