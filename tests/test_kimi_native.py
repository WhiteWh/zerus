#!/usr/bin/env python3
"""Real Kimi Code against a localhost-only model, never a user's account.
Run with HGS_KIMI_TEST_BIN, HGS_KIMI_TEST_SHA256 and the runner's sealed
HGS_KIMI_TEST_RECORD; a missing native image skips this opt-in suite.

Kimi's updater replaces the executable it runs from, whatever its home. Each
case therefore runs its own copy, with automatic updates off, never the given
file or a shared installation, and fails if either executable's bytes change.
Kimi's platform endpoint is the fixture's too, so no remote banner or client
config reaches the screen or the session.

On macOS no file holds a process's exec-time environment, and the kernel's
argument area has no boundary between it and Darwin's apple strings, while
Kimi's process.title rewrites the argv bytes before it. There the copy is
started through a fixture-owned exec shim: it writes one private capture of
the exact argv and envp it was started with, its PID and start, and becomes
the copy in the same process with exactly those. A child's environment is then the recorded envp,
proven byte for byte by that process's argument area, read between two
identical samples of its PID, start, image and supervisor; anything else fails.
"""
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import shutil
import shlex
import stat
import subprocess
import sys
import tempfile
import threading
import time
import tomllib
import unittest
from unittest import mock
import urllib.error
import urllib.request
import uuid
import test_pause as lifecycle
from kimi_release import admitted_version

def settled_turn(state, receipt):
    """One native observation, after this exact submitted turn has replied."""
    return (state.get('conversation_id') and state.get('activity') == 'idle' and state.get('phase') == 'idle'
            and float(state.get('reply_at') or 0) > receipt['submitted_at']
            and 'pause' in state.get('allowed_actions', []) and state)


def settled_recovery(state):
    job = state.get('recovery', {})
    # A durable recovery result and the next live projection are separate
    # observations. Do not accept success until the original receipt has settled.
    return (job.get('state') == 'succeeded' and isinstance(job.get('receipt'), dict)
            and settled_turn(state, job['receipt']))


def settled_recorded_turn(state, receipt, text):
    """Wait for both callback observations in the same settled snapshot."""
    if not settled_turn(state, receipt):
        return None
    events = [event for event in state.get('events', [])
              if event.get('run_id') == state.get('run_id')
              and event.get('kimi_conversation_id') == state.get('conversation_id')
              and event.get('agent_id', '') in ('', 'main')]
    starts = [event for event in events if event.get('type') == 'TurnStarted' and event.get('detail') == text]
    if len(starts) != 1 or type(starts[0].get('kimi_turn_id')) is not int or starts[0]['kimi_turn_id'] < 0:
        return None
    return state if any(event.get('type') == 'Stop'
                        and type(event.get('kimi_turn_id')) is int
                        and event.get('kimi_turn_id') == starts[0]['kimi_turn_id']
                        and event.get('turn_identity_source') in ('kimi_wire', 'kimi_hook')
                        for event in events) else None


KIMI = os.environ.get('HGS_KIMI_TEST_BIN')
GUARDS = [Path(path).resolve() for path in os.environ.get('ZERUS_KIMI_GUARD', '').split(os.pathsep) if path]
# Where KIMI_CODE_BASE_URL points: Kimi asks its platform there for a banner
# and client configs, and receives none.
PLATFORM = '/kimi-platform'
# Kimi's tmux keyboard warning; hgs's panes keep extended-keys off.
TMUX_KEYS = 'tmux extended-keys is off.'
# Kimi 2.1.1's PROCESS_NAME (src/constant/app.ts), set first in main().
TITLE = b'kimi-code'
PADDING_LIMIT = 1024  # NULs allowed between the exec path and argv[0]
# One exec capture: at most MAX_ARGC and MAX_ENVC strings of at most MAX_STRING
# bytes, MAX_PAYLOAD bytes together, in a capture file of at most CAPTURE_BYTES.
MAX_ARGC, MAX_ENVC, MAX_STRING, MAX_PAYLOAD = 1024, 1024, 256 << 10, 1 << 20
CAPTURE_BYTES = MAX_PAYLOAD + 256
CAPTURE = re.compile(rb'ZKSHIM2 abi=(\d+(?::\d+){6}) pid=(\d+) start=(\d+)\.(\d{6}) argc=(\d+) envc=(\d+) '
                     rb'payload=(\d+)\n')
# struct proc_bsdinfo of the macOS SDK (sys/proc_info.h), field by field; the
# guard reads its status, pid, ppid, uid and start, at these offsets of 136 bytes.
BSDINFO = (('flags', 'I'), ('status', 'I'), ('xstatus', 'I'), ('pid', 'I'), ('ppid', 'I'), ('uid', 'I'),
           ('gid', 'I'), ('ruid', 'I'), ('rgid', 'I'), ('svuid', 'I'), ('svgid', 'I'), ('rfu', 'I'),
           ('comm', '16s'), ('name', '32s'), ('nfiles', 'I'), ('pgid', 'I'), ('pjobc', 'I'), ('tdev', 'I'),
           ('tpgid', 'I'), ('nice', 'i'), ('start_sec', 'Q'), ('start_usec', 'Q'))
ABI = dict(size=136, status=4, pid=12, ppid=16, uid=20, start_sec=120, start_usec=128)
ABI_TEXT = b':'.join(b'%d' % value for value in ABI.values())
KEY = re.compile(rb'[A-Za-z_][A-Za-z0-9_]*')
PLAIN_PATH = re.compile(r'(/[A-Za-z0-9._-]+)+')
DAYS = ('Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun')
MONTHS = ('Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec')
# The macOS exec shim. Every bound and admission is checked before it
# allocates, writes or execs; any refusal exits 127 without starting the copy.
SHIM_C = r'''#include <fcntl.h>
#include <libproc.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/proc_info.h>
#include <sys/stat.h>
#include <unistd.h>

_Static_assert(sizeof(struct proc_bsdinfo) == 136, "proc_bsdinfo size");
_Static_assert(offsetof(struct proc_bsdinfo, pbi_status) == 4, "pbi_status offset");
_Static_assert(offsetof(struct proc_bsdinfo, pbi_pid) == 12, "pbi_pid offset");
_Static_assert(offsetof(struct proc_bsdinfo, pbi_ppid) == 16, "pbi_ppid offset");
_Static_assert(offsetof(struct proc_bsdinfo, pbi_uid) == 20, "pbi_uid offset");
_Static_assert(offsetof(struct proc_bsdinfo, pbi_start_tvsec) == 120, "pbi_start_tvsec offset");
_Static_assert(offsetof(struct proc_bsdinfo, pbi_start_tvusec) == 128, "pbi_start_tvusec offset");

#define MAX_ARGC @MAX_ARGC@
#define MAX_ENVC @MAX_ENVC@
#define MAX_STRING @MAX_STRING@
#define MAX_PAYLOAD @MAX_PAYLOAD@
#define CAPTURE_BYTES @CAPTURE_BYTES@

static const char CAPTURES[] = "@CAPTURES@";
static const char IMAGE[] = "@IMAGE@";

static void refuse(void) {
    _exit(127);
}

/* A string's length, refusing one longer than MAX_STRING before any copy. */
static size_t bounded(const char *text) {
    size_t length = strnlen(text, (size_t)MAX_STRING + 1);
    if (length > MAX_STRING) refuse();
    return length;
}

static int private_to_me(const struct stat *info, mode_t kind) {
    return (info->st_mode & S_IFMT) == kind && info->st_uid == geteuid() && (info->st_mode & 077) == 0;
}

static void copy(char *record, size_t *used, size_t total, const char *text) {
    size_t length = bounded(text) + 1;
    if (length > total - *used) refuse();
    memcpy(record + *used, text, length);
    *used += length;
}

int main(int argc, char **argv, char **envp) {
    if (argc < 1 || argc > MAX_ARGC) refuse();
    size_t envc = 0, payload = 0;
    while (envp[envc]) {
        if (++envc > MAX_ENVC) refuse();
    }
    for (int i = 0; i < argc; i++) {
        payload += bounded(argv[i]) + 1;
        if (payload > MAX_PAYLOAD) refuse();
    }
    for (size_t i = 0; i < envc; i++) {
        payload += bounded(envp[i]) + 1;
        if (payload > MAX_PAYLOAD) refuse();
    }
    struct proc_bsdinfo info;
    pid_t pid = getpid();
    if (proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, (int)sizeof info) != (int)sizeof info ||
        (pid_t)info.pbi_pid != pid || info.pbi_start_tvusec >= 1000000)
        refuse();
    char header[256], name[64];
    int head = snprintf(header, sizeof header,
                        "ZKSHIM2 abi=%zu:%zu:%zu:%zu:%zu:%zu:%zu pid=%d start=%llu.%06llu "
                        "argc=%d envc=%zu payload=%zu\n",
                        sizeof info, offsetof(struct proc_bsdinfo, pbi_status), offsetof(struct proc_bsdinfo, pbi_pid),
                        offsetof(struct proc_bsdinfo, pbi_ppid), offsetof(struct proc_bsdinfo, pbi_uid),
                        offsetof(struct proc_bsdinfo, pbi_start_tvsec), offsetof(struct proc_bsdinfo, pbi_start_tvusec),
                        (int)pid, (unsigned long long)info.pbi_start_tvsec, (unsigned long long)info.pbi_start_tvusec,
                        argc, envc, payload);
    int named = snprintf(name, sizeof name, "%d.%llu.%06llu", (int)pid, (unsigned long long)info.pbi_start_tvsec,
                         (unsigned long long)info.pbi_start_tvusec);
    if (head <= 0 || (size_t)head >= sizeof header || named <= 0 || (size_t)named >= sizeof name) refuse();
    size_t total = (size_t)head + payload + 1;
    if (total > CAPTURE_BYTES) refuse();
    char *record = malloc(total);
    if (!record) refuse();
    memcpy(record, header, (size_t)head);
    size_t used = (size_t)head;
    for (int i = 0; i < argc; i++) copy(record, &used, total, argv[i]);
    for (size_t i = 0; i < envc; i++) copy(record, &used, total, envp[i]);
    if (used != total - 1) refuse();
    record[used++] = '\n';
    struct stat state;
    int folder = open(CAPTURES, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    if (folder < 0 || fstat(folder, &state) != 0 || !private_to_me(&state, S_IFDIR)) refuse();
    int fd = openat(folder, name, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600);
    if (fd < 0 || fstat(fd, &state) != 0 || !private_to_me(&state, S_IFREG) || state.st_nlink != 1 ||
        state.st_size != 0)
        refuse();
    for (size_t done = 0; done < total;) {
        ssize_t written = write(fd, record + done, total - done);
        if (written <= 0) refuse();
        done += (size_t)written;
    }
    if (fsync(fd) != 0 || close(fd) != 0 || fsync(folder) != 0 || close(folder) != 0) refuse();
    execve(IMAGE, argv, envp);
    refuse();
}
'''


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def executable(pid):
    """The running image, not its command line or the PATH that started it."""
    if sys.platform.startswith('linux'):
        return Path(os.readlink(f'/proc/{pid}/exe')).resolve()
    if sys.platform == 'darwin':
        import ctypes
        library = ctypes.CDLL('/usr/lib/libproc.dylib', use_errno=True)
        path = library.proc_pidpath
        path.argtypes = (ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32)
        path.restype = ctypes.c_int
        buffer = ctypes.create_string_buffer(4096)  # PROC_PIDPATHINFO_MAXSIZE
        if path(pid, buffer, len(buffer)) <= 0:
            raise OSError(ctypes.get_errno(), 'proc_pidpath failed')
        return Path(os.fsdecode(buffer.value)).resolve()
    raise RuntimeError('Native Kimi image proof is supported only on Linux and macOS')


def private_environment(env, root, home, platform):
    """Keep only the lifecycle fixture's own routing and executable selection.
    No inherited account, provider, proxy, key, agent socket or profile override."""
    keep = ('PATH', 'SHELL', 'HGS_CONFIG_DIR', 'HGS_STATE_DIR', 'HGS_SELF', 'HGS_PEERS', 'HGS_TAB', 'HGS_TRACKING')
    clean = {key: env[key] for key in keep if key in env}
    clean.update(HOME=str(root), KIMI_CODE_HOME=str(home), KIMI_CODE_BASE_URL=platform, KIMI_CODE_NO_AUTO_UPDATE='1',
                 KIMI_DISABLE_TELEMETRY='1', DISABLE_AUTOUPDATER='1', TERM='xterm-256color',
                 LANG='en_US.UTF-8' if sys.platform == 'darwin' else 'C.UTF-8', PYTHONUTF8='1')
    for key, folder in (('XDG_CONFIG_HOME', '.config'), ('XDG_DATA_HOME', '.local/share'),
                        ('XDG_CACHE_HOME', '.cache'), ('XDG_STATE_HOME', '.local/state'),
                        ('USERPROFILE', '.'), ('APPDATA', 'AppData/Roaming'),
                        ('LOCALAPPDATA', 'AppData/Local'), ('TMPDIR', 'tmp')):
        path = root / folder
        path.mkdir(parents=True, exist_ok=True)
        clean[key] = str(path)
    return clean


def shim_source(captures, image):
    """The exec shim for two fixture paths, which must be plain absolute paths."""
    values = dict(CAPTURES=str(captures), IMAGE=str(image), MAX_ARGC=MAX_ARGC, MAX_ENVC=MAX_ENVC,
                  MAX_STRING=MAX_STRING, MAX_PAYLOAD=MAX_PAYLOAD, CAPTURE_BYTES=CAPTURE_BYTES)
    if not all(PLAIN_PATH.fullmatch(values[key]) for key in ('CAPTURES', 'IMAGE')):
        raise ValueError('a shim path is not a plain absolute path')
    source = SHIM_C
    for key, value in values.items():
        source = source.replace('@%s@' % key, str(value))
    if '@' in source:
        raise ValueError('a shim placeholder was not filled')
    return source


def capture_name(pid, birth):
    return '%d.%d.%06d' % (pid, birth[0], birth[1])


def capture_record(data, pid, birth):
    """One exec's capture, parsed strictly against this PID, start and the guard's ABI."""
    if len(data) > CAPTURE_BYTES:
        raise ValueError('the capture is larger than its bound')
    end = data.find(b'\n')
    match = CAPTURE.fullmatch(data[:end + 1]) if end >= 0 else None
    if not match:
        raise ValueError('a malformed capture header')
    abi, number, seconds, micros, argc, envc, length = match.groups()
    argc, envc, length = int(argc), int(envc), int(length)
    if abi != ABI_TEXT:
        raise ValueError("the shim's proc_bsdinfo layout is not the guard's")
    if (int(number), int(seconds), int(micros)) != (pid, birth[0], birth[1]):
        raise ValueError('the capture is not of this process start')
    payload = data[end + 1:end + 1 + length]
    strings = payload.split(b'\0')
    if len(payload) != length or data[end + 1 + length:] != b'\n' or not 1 <= argc <= MAX_ARGC or \
            envc > MAX_ENVC or length > MAX_PAYLOAD or len(strings) != argc + envc + 1 or strings[-1] != b'' or \
            any(len(string) > MAX_STRING for string in strings):
        raise ValueError('the capture payload does not match its header')
    return dict(pid=pid, birth=(birth[0], birth[1]), abi=abi.decode(), argv=strings[:argc], env=strings[argc:-1])


def private_to_me(info, kind):
    return stat.S_IFMT(info.st_mode) == kind and info.st_uid == os.geteuid() and not info.st_mode & 0o077


def read_capture(folder, pid, birth):
    """One exec's capture, named by its PID and start: the folder and then the
    file are opened without following a link or blocking, admitted only as
    this user's private folder and single-link regular file within the bound,
    and read at most one byte past it. Only fixed reasons are raised."""
    if not (isinstance(birth, tuple) and len(birth) == 2):
        raise ValueError('no process start names a capture')
    directory = os.open(folder, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        if not private_to_me(os.fstat(directory), stat.S_IFDIR):
            raise ValueError("the capture folder is not this user's private folder")
        fd = os.open(capture_name(pid, birth), os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC,
                     dir_fd=directory)
        try:
            info = os.fstat(fd)
            if not private_to_me(info, stat.S_IFREG) or info.st_nlink != 1:
                raise ValueError("the capture is not this user's private single-link regular file")
            if info.st_size > CAPTURE_BYTES:
                raise ValueError('the capture is larger than its bound')
            data = b''
            while len(data) <= CAPTURE_BYTES:
                chunk = os.read(fd, min(1 << 16, CAPTURE_BYTES + 1 - len(data)))
                if not chunk:
                    break
                data += chunk
            if len(data) != info.st_size:
                raise ValueError('the capture changed while it was read')
        finally:
            os.close(fd)
    finally:
        os.close(directory)
    return capture_record(data, pid, birth)


def layout(fields):
    """Natural-alignment offsets and padded size of a C struct of these fields."""
    offsets, offset, widest = {}, 0, 1
    for name, code in fields:
        size, align = dict(I=(4, 4), i=(4, 4), Q=(8, 8)).get(code) or (int(code[:-1]), 1)
        offset = -(-offset // align) * align
        offsets[name] = offset
        offset, widest = offset + size, max(widest, align)
    return dict(size=-(-offset // widest) * widest, **offsets)


def sysctl_length(returned, limit, exact=None):
    """A returned sysctl length the guard may use, or OSError; never clamped."""
    if not 0 < returned <= limit or (exact is not None and returned != exact):
        raise OSError('sysctl returned an impossible length')
    return returned


def row_problems(rows):
    """Recorded envp rows must be strict UTF-8 KEY=VALUE without duplicate keys."""
    problems, keys = [], []
    for row in rows:
        key, separator, _ = row.partition(b'=')
        if not separator or not KEY.fullmatch(key):
            problems.append('a malformed environment row of %d bytes' % len(row))
            continue
        try:
            row.decode('utf-8', 'strict')
        except UnicodeDecodeError:
            problems.append('an environment row that is not strict UTF-8')
            continue
        keys.append(key)
    if len(set(keys)) != len(keys):
        problems.append('duplicate environment keys')
    return problems


def launch_oracle(raw, argv, env, image, title=TITLE):
    """KERN_PROCARGS2 bytes against the shim's exact execve: argc, the copy's
    exec path, an argv area holding either the launched bytes or Kimi's title
    then NULs, and exactly the recorded envp after it. What follows is Darwin's
    opaque apple area, counted but never read."""
    launched = b''.join(argument + b'\0' for argument in argv)
    expected = b''.join(row + b'\0' for row in env)
    result = dict(valid=False, reasons=row_problems(env), argc=None, area=None, tail_bytes=None, environment=None)
    if len(raw) < 4 or not argv:
        result['reasons'].append('no argument area or no launched argv')
        return result
    result['argc'] = argc = int.from_bytes(raw[:4], 'little')
    path_end = raw.find(b'\0', 4)
    if path_end < 0:
        result['reasons'].append('no NUL after the exec path')
        return result
    start = path_end
    while start < len(raw) and raw[start] == 0 and start - path_end <= PADDING_LIMIT:
        start += 1
    area, env_start = raw[start:start + len(launched)], start + len(launched)
    written = title[:len(launched) - 1]
    titled = written + b'\0' * (len(launched) - len(written))
    result['area'] = 'launched' if area == launched else 'title' if area == titled else 'other'
    if argc != len(argv):
        result['reasons'].append('argc %d is not the launched %d' % (argc, len(argv)))
    if raw[4:path_end] != os.fsencode(str(image)):
        result['reasons'].append('the exec path is not the copy')
    if start - path_end > PADDING_LIMIT:
        result['reasons'].append('more than %d padding bytes' % PADDING_LIMIT)
    if result['area'] == 'other':
        result['reasons'].append('the argv area is neither the launched bytes nor the title')
    if raw[env_start:env_start + len(expected)] != expected:
        result['reasons'].append('the environment is not exactly the recorded envp')
    if not result['reasons']:
        result.update(valid=True, tail_bytes=len(raw) - env_start - len(expected),
                      environment=dict(row.decode().split('=', 1) for row in env))
    return result


def lstart_text(seconds):
    """`%a %b %e %H:%M:%S %Y` in UTC and the C locale, as hgs records a start."""
    moment = time.gmtime(seconds)
    return '%s %s %2d %02d:%02d:%02d %d' % (DAYS[moment.tm_wday], MONTHS[moment.tm_mon - 1], moment.tm_mday,
                                           moment.tm_hour, moment.tm_min, moment.tm_sec, moment.tm_year)


def identity_problems(before, after, record, image, binding):
    """The kernel sample belongs to the shim-recorded process hgs tracks: the
    same PID, owner, microsecond start, copy image and supervisor before and
    after it, matching the shim's record and hgs's binding."""
    normal = lambda text: ' '.join(str(text or '').split())
    problems = []
    if before != after:
        problems.append('the process changed while its environment was read')
    if before.get('error') or before.get('pid') != record['pid'] or not before.get('own') or before.get('zombie'):
        problems.append('the process is not a live process of this user with the recorded PID')
    if before.get('image') != image:
        problems.append('the process is not running the copied image')
    if before.get('birth') != record['birth']:
        problems.append('the process did not start as the shim recorded')
    if before.get('birth') and normal(lstart_text(before['birth'][0])) != normal(binding.get('process_start')):
        problems.append('hgs tracks another process start')
    supervisor = binding.get('supervisor') or {}
    if before.get('ppid') != supervisor.get('pid') or not supervisor.get('start') or \
            normal(before.get('parent_start')) != normal(supervisor.get('start')):
        problems.append('the process is not the recorded supervisor\'s child')
    return problems


class MacProcess:
    """The macOS process reads of the environment guard; libraries load per call."""

    @staticmethod
    def structure():
        import ctypes
        codes = dict(I=ctypes.c_uint32, i=ctypes.c_int32, Q=ctypes.c_uint64)

        class BsdInfo(ctypes.Structure):
            _fields_ = [(name, codes.get(code) or ctypes.c_char * int(code[:-1])) for name, code in BSDINFO]
        found = {name: getattr(BsdInfo, name).offset for name in ABI if name != 'size'}
        found['size'] = ctypes.sizeof(BsdInfo)
        if found != ABI:
            raise OSError("the ctypes proc_bsdinfo layout is not the SDK's")
        return BsdInfo

    @classmethod
    def info(cls, pid):
        import ctypes
        info = cls.structure()()
        library = ctypes.CDLL('/usr/lib/libproc.dylib', use_errno=True)
        library.proc_pidinfo.argtypes = (ctypes.c_int, ctypes.c_int, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_int)
        library.proc_pidinfo.restype = ctypes.c_int
        # PROC_PIDTBSDINFO: the whole structure or nothing.
        if library.proc_pidinfo(pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info)) != ctypes.sizeof(info) or \
                info.pid != pid:
            raise OSError(ctypes.get_errno(), 'proc_pidinfo failed')
        return info

    @classmethod
    def identity(cls, pid):
        try:
            info = cls.info(pid)
            parent = cls.info(info.ppid)
            return dict(pid=info.pid, own=info.uid == os.geteuid(), zombie=info.status == 5, ppid=info.ppid,
                        birth=(info.start_sec, info.start_usec), image=executable(pid),
                        parent_start=lstart_text(parent.start_sec))
        except OSError as error:
            return dict(pid=pid, error=type(error).__name__)

    @staticmethod
    def procargs(pid):
        import ctypes
        library = ctypes.CDLL('/usr/lib/libSystem.B.dylib', use_errno=True)
        library.sysctl.argtypes = (ctypes.POINTER(ctypes.c_int), ctypes.c_uint, ctypes.c_void_p,
                                   ctypes.POINTER(ctypes.c_size_t), ctypes.c_void_p, ctypes.c_size_t)
        library.sysctl.restype = ctypes.c_int

        def sysctl(mib, limit, exact=None):
            name = (ctypes.c_int * len(mib))(*mib)
            buffer, length = ctypes.create_string_buffer(limit), ctypes.c_size_t(limit)
            if library.sysctl(name, len(mib), buffer, ctypes.byref(length), None, 0) != 0:
                raise OSError(ctypes.get_errno(), 'sysctl %r failed' % (mib,))
            return buffer.raw[:sysctl_length(length.value, limit, exact)]
        size = ctypes.sizeof(ctypes.c_int)
        argmax = int.from_bytes(sysctl([1, 8], size, exact=size), sys.byteorder)  # KERN_ARGMAX
        if not 4096 <= argmax <= 16 << 20:
            raise OSError('KERN_ARGMAX is out of range')
        return sysctl([1, 49, pid], argmax)  # KERN_PROCARGS2


class Model(http.server.BaseHTTPRequestHandler):
    """An OpenAI-compatible chat completion stream that finishes every turn,
    and Kimi's platform, which has no banner or client config to offer."""

    def log_message(self, *args):
        pass

    def platform(self, body):
        observed = dict(method=self.command, path=self.path[len(PLATFORM):],
                        name=body.get('name') if isinstance(body, dict) else None)
        self.server.platform.append(observed)
        self.send_error(404)
        self.wfile.flush()
        # The request log alone does not prove that its response was written.
        self.server.platform_completed.append(dict(observed))

    def do_GET(self):
        if self.path.startswith(PLATFORM + '/'):
            return self.platform({})
        body = json.dumps({'object': 'list', 'data': [{'id': 'fixture', 'object': 'model'}]}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))) or b'{}')
        if self.path.startswith(PLATFORM + '/'):
            return self.platform(body)
        self.server.requests.append(body)
        if getattr(self.server, 'failure', None):
            error = json.dumps(dict(error=dict(type='rate_limit_error', code='rate_limit_exceeded',
                                              message=self.server.failure))).encode()
            self.send_response(429)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(error)))
            self.send_header('x-should-retry', 'false')
            self.end_headers()
            self.wfile.write(error)
            self.wfile.flush()
            return
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        for delta, finish in (({'role': 'assistant', 'content': 'Fixture complete.'}, None), ({}, 'stop')):
            chunk = {'id': 'chat_fixture', 'object': 'chat.completion.chunk', 'model': 'fixture',
                     'choices': [{'index': 0, 'delta': delta, 'finish_reason': finish}]}
            if finish:
                chunk['usage'] = {'prompt_tokens': 10, 'completion_tokens': 3, 'total_tokens': 13}
            self.wfile.write(b'data: ' + json.dumps(chunk).encode() + b'\n\n')
        self.wfile.write(b'data: [DONE]\n\n')
        self.wfile.flush()


@unittest.skipUnless(KIMI, 'Set HGS_KIMI_TEST_BIN for isolated native Kimi coverage')
class NativeKimi(lifecycle.Harness):
    def setUp(self):
        self.version = admitted_version(KIMI, os.environ.get('HGS_KIMI_TEST_SHA256', ''))
        # Kimi's trust panel wraps a folder wider than its frame, and only a
        # path on one row is answered through hgs: keep the fixture's short.
        with mock.patch.object(tempfile, 'tempdir', '/tmp'):
            super().setUp()
        source = Path(KIMI).resolve()
        before = digest(source)
        guards = {str(path): digest(path) for path in [source, *GUARDS]}
        # The copy lives outside the fixture's home, which tearDown removes, so
        # its bytes are checked once every process of the private socket ended.
        images = Path(tempfile.mkdtemp(prefix='hgs-kimi-image-'))
        self.addCleanup(self.discard_images, images)
        image = images / 'kimi'
        shutil.copyfile(source, image)
        image.chmod(0o755)
        self.image, self.image_digest = image.resolve(), before
        self.assertEqual(digest(image), before)
        (self.bin / 'kimi').unlink()
        if sys.platform == 'darwin':
            # One private capture per exec stays inside the fixture root, never in exported logs.
            self.captures = self.root.resolve() / 'kimi-exec'
            self.captures.mkdir(mode=0o700)
            shim = self.root / 'kimi-shim.c'
            shim.write_text(shim_source(self.captures, image.resolve()))
            build = subprocess.run(['/usr/bin/cc', '-std=c11', '-O1', '-Wall', '-Werror', '-o', str(self.bin / 'kimi'),
                                    str(shim)], env=dict(PATH='/usr/bin:/bin', HOME=str(self.root)),
                                   capture_output=True, text=True, timeout=120)
            self.assertEqual(build.returncode, 0, 'the Kimi exec shim did not build: ' + build.stderr[-2000:])
        else:
            (self.bin / 'kimi').symlink_to(image)

        def unchanged():
            after = {str(path): digest(path) for path in [source, *GUARDS]}
            self.assertEqual(digest(image), before, 'the fixture changed its own Kimi executable')
            self.assertEqual(after, guards, 'a given or guarded Kimi executable changed')
            print('ZERUS_KIMI_IMAGE_GUARDS=' + json.dumps(dict(case=self.id(), before=guards, after=after)))
        self.addCleanup(unchanged)
        # Only the copy is reachable: hgs puts this home's folders first, and no
        # other folder on the PATH may offer a kimi of its own.
        folders = [folder for folder in os.environ['PATH'].split(os.pathsep)
                   if folder and not (Path(folder) / 'kimi').exists()]
        self.env['PATH'] = os.pathsep.join([str(self.bin), *folders])
        self.assertEqual(Path(shutil.which('kimi', path=self.env['PATH'])).resolve(),
                         (self.bin / 'kimi').resolve() if sys.platform == 'darwin' else image.resolve())
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
        self.model_server = server
        server.requests, server.platform, server.platform_completed = [], [], []
        self.requests, self.platform, self.platform_completed = server.requests, server.platform, server.platform_completed
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        home = self.root / '.kimi-code'
        home.mkdir()
        (home / 'config.toml').write_text(
            'default_model = "fixture"\ntelemetry = false\n'
            '[providers.fixture]\ntype = "openai"\nbase_url = "http://127.0.0.1:%d/v1"\napi_key = "fixture-key"\n'
            '[models.fixture]\nprovider = "fixture"\nmodel = "fixture"\nmax_context_size = 32768\n'
            '[loop_control]\nmax_attempts_per_step = 1\n' % server.server_port)
        (home / 'tui.toml').write_text('[upgrade]\nauto_install = false\n')
        self.home = home
        self.env = private_environment(self.env, self.root, home,
                                       'http://127.0.0.1:%d%s' % (server.server_port, PLATFORM))

    def discard_images(self, images):
        """tearDown keeps the fixture root while a private process may still run,
        and then the copy it may run stays too: keeping it is a reported failure,
        and so is any error removing it."""
        if self.root.exists():
            raise AssertionError('kept the Kimi image copy %s with the preserved fixture root %s' % (images, self.root))
        shutil.rmtree(images)

    def screen(self, name):
        return self.t('capture-pane', '-p', '-J', '-t', '=' + name + ':')

    def terminal(self, name):
        """Independent before/after diagnostics, themselves two tmux calls.
        These are not the actual cursor and styled samples rejected inside hgs."""
        target = '=' + name + ':'
        return dict(cursor=self.t('display-message', '-p', '-t', target,
                                  '#{cursor_x},#{cursor_y} #{pane_width}x#{pane_height} #{pane_in_mode}').strip(),
                    styled=self.t('capture-pane', '-p', '-e', '-t', target), platform=list(self.platform),
                    platform_completed=list(self.platform_completed))

    def until(self, predicate, name, seconds=60):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            result = predicate()
            if result:
                return result
            time.sleep(0.2)
        self.fail('Native Kimi timed out:\n' + self.screen(name) + '\n' + self.hgs('inspect', name) +
                  '\nLast model messages: ' + json.dumps([request.get('messages', [])[-2:] for request in self.requests[-2:]],
                                                         ensure_ascii=False)[:4000])

    def inspect(self, name):
        return json.loads(self.hgs('inspect', name))

    def answer(self, name, card, choice):
        payload = dict(request_id=str(uuid.uuid4()), expected_run_id=self.binding(name)['run_id'],
                       expected_conversation_id='', question_id=card['question_id'],
                       expected_question_hash=card['question_hash'],
                       answers=[dict(question_id='trust', selected_option_ids=['trust_' + str(choice)], text='')])
        p = subprocess.run([str(lifecycle.HGS), 'answer', name, '--json'], env=self.env, input=json.dumps(payload),
                           capture_output=True, text=True, timeout=60)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        return json.loads(p.stdout)

    def launch(self):
        """A tracked Kimi start: its folder trust panel is answered once through
        hgs, then the new session takes its first message. Kimi creates its
        conversation only with that message."""
        name = 'kimi/p/native'
        self.hgs('kimi', 'p', '-n', 'native', '-d')
        def agent_started():
            record = self.binding(name)
            supervisor = (record.get('supervisor') or {}).get('pid')
            # Lifecycle first publishes the supervisor itself as a placeholder.
            # Pin the native identity only after spawn publishes its actual child.
            return record.get('pid') and supervisor and record['pid'] != supervisor
        self.until(agent_started, name)
        self.assert_process(name, 'initial')
        answered = []

        def ready():
            state = self.inspect(name)
            cards = [card for card in state.get('pending_questions', []) if card.get('can_answer')]
            if cards and not answered:
                self.assertIn('Trust this folder?', self.screen(name))
                answered.append(self.answer(name, cards[0], 0))
                self.assertEqual(answered[0]['status'], 'answered')
                return None
            return state if state.get('first_message_can_send') else None
        self.until(ready, name)
        self.assertEqual(len(answered), 1)
        # Kimi 2.1.1 renders more startup output after its composer first takes
        # input, without awaiting it: the banner it asks its platform for (here
        # none) and its tmux keyboard warning. These preconditions reduce late
        # redraws; they do not classify a refusal, and hgs's guard stays strict.
        self.assertEqual(self.t('show-options', '-gv', 'extended-keys').strip(), 'off')
        state = self.until(lambda: self.started(name), name)
        self.assertFalse(state.get('conversation_id'))
        return name

    def started(self, name):
        """The platform response was written, the warning is visible and hgs admits input.
        The response marker does not prove that Kimi has consumed or rendered it."""
        if not any(request['name'] == 'client_banner' for request in self.platform_completed) or \
                TMUX_KEYS not in self.screen(name):
            return None
        state = self.inspect(name)
        return state if state.get('first_message_can_send') else None

    def process_sample(self, pid):
        """Diagnostic metadata, not a PID-reuse proof: ps lstart has second precision.
        Its time is local, and this query is separate from the environment sample."""
        sample = dict(image='unavailable', start_lstart=None)
        try:
            sample['image'] = 'expected' if executable(pid) == self.image else 'different'
        except OSError as error:
            sample['image_error'] = type(error).__name__
        try:
            result = subprocess.run(['/bin/ps', '-p', str(pid), '-o', 'lstart='],
                                    capture_output=True, text=True, timeout=2)
            sample['start_exit_code'] = result.returncode
            start = result.stdout.strip()
            if result.returncode == 0 and re.fullmatch(
                    r'[A-Z][a-z]{2} [A-Z][a-z]{2}\s+\d{1,2} \d{2}:\d{2}:\d{2} \d{4}', start):
                sample['start_lstart'] = start
        except (OSError, subprocess.SubprocessError, UnicodeError) as error:
            sample['start_error'] = type(error).__name__
        return sample

    def environment(self, pid, sample, binding):
        if sys.platform.startswith('linux'):
            data = Path(f'/proc/{pid}/environ').read_bytes()
            sample.update(source='proc', output_bytes=len(data))
            rows = data.decode(errors='replace').split('\0')
            return dict(row.split('=', 1) for row in rows if '=' in row)
        return self.launched_environment(pid, sample, binding)

    def launched_environment(self, pid, sample, binding):
        """The exact envp the shim exec'd this process with, from its admitted
        capture, proven by its own argument area between two identical identity
        samples, or a failure. Diagnostics carry counts and reasons, never values."""
        sample.update(source='kernel', capture=None, abi=None, area=None, argc=None, env_rows=None, tail_bytes=None,
                      problems=[])
        before = MacProcess.identity(pid)
        try:
            record = read_capture(self.captures, pid, before.get('birth'))
        except ValueError as error:
            sample['problems'] = ['the exec capture cannot be admitted: %s' % error]
        except OSError as error:
            sample['problems'] = ['the exec capture cannot be admitted: %s' % type(error).__name__]
        if sample['problems']:
            raise AssertionError('the Kimi environment cannot be proven: ' + sample['problems'][0])
        sample.update(capture='admitted', abi=record['abi'])
        raw = MacProcess.procargs(pid)
        after = MacProcess.identity(pid)
        problems = identity_problems(before, after, record, self.image, binding)
        result = launch_oracle(raw, record['argv'], record['env'], self.image)
        sample.update(area=result['area'], argc=result['argc'], env_rows=len(record['env']),
                      tail_bytes=result['tail_bytes'], problems=problems + result['reasons'])
        if sample['problems']:
            raise AssertionError('the Kimi environment cannot be proven: ' + '; '.join(sample['problems']))
        return result['environment']

    def assert_process(self, name, stage):
        binding = self.binding(name)
        process = binding['pid']

        def running():
            try:
                return executable(process) == self.image
            except OSError:
                return False  # The supervisor may not have exec'd its image yet.
        self.until(running, name)
        self.assertEqual(digest(executable(process)), self.image_digest)
        keys = ('HOME', 'KIMI_CODE_HOME', 'KIMI_CODE_BASE_URL', 'KIMI_CODE_NO_AUTO_UPDATE',
                'KIMI_DISABLE_TELEMETRY', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_STATE_HOME',
                'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'TMPDIR')
        forbidden = ('OPENAI_', 'ANTHROPIC_', 'CODEX_', 'CLAUDE', 'DEEPSEEK_', 'OPENCODE_',
                     'AWS_', 'AZURE_', 'GOOGLE_', 'GCP_', 'GITHUB_', 'GH_', 'NPM_', 'npm_', 'SSH_', 'SOPS_', 'AGE_')
        before, sample, actual = self.process_sample(process), {}, None
        try:
            actual = self.environment(process, sample, binding)
        finally:
            after = self.process_sample(process)
            identity = 'unknown'
            if before['start_lstart'] and after['start_lstart'] and \
                    before['image'] != 'unavailable' and after['image'] != 'unavailable':
                if before['start_lstart'] != after['start_lstart'] or before['image'] != after['image']:
                    identity = 'changed'
                elif before['image'] == after['image'] == 'expected':
                    identity = 'same_reported_start_and_image'
            # No environment values or complete ps output enter the retained log.
            print('ZERUS_KIMI_ENVIRONMENT=' + json.dumps(dict(
                case=self.id(), stage=stage, pid=process, expected_image=str(self.image),
                before=before, after=after, identity=identity, sample=sample,
                keys=None if actual is None else {
                    key: 'missing' if key not in actual else 'match' if actual[key] == self.env[key] else 'different'
                    for key in keys},
                forbidden_keys=None if actual is None else [key for key in actual if actual[key] and key.startswith(forbidden)],
                unexpected_kimi_keys=None if actual is None else [
                    key for key in actual if actual[key] and key.startswith('KIMI_') and key not in keys])), flush=True)
        for key in keys:
            self.assertTrue(actual.get(key) == self.env[key], 'unexpected private child environment key: ' + key)
        # hgs explicitly clears the other agents' homes with empty values.
        self.assertFalse([key for key in actual if actual[key] and key.startswith(forbidden)],
                         'a child inherited credential keys')
        self.assertFalse([key for key in actual if actual[key] and key.startswith('KIMI_') and key not in keys],
                         'a child inherited an unexpected Kimi override')
        print('ZERUS_KIMI_PROCESS=' + json.dumps(dict(case=self.id(), stage=stage, pid=process,
              image=str(self.image), sha256=self.image_digest, private_environment_keys=list(keys))))

    def payload(self, name, text):
        state = self.inspect(name)
        return dict(request_id=str(uuid.uuid4()), text=text, expected_run_id=state['run_id'],
                    expected_conversation_id=state.get('conversation_id') or '')

    def send(self, name, payload):
        before = self.terminal(name)
        p = subprocess.run([str(lifecycle.HGS), 'send', name, '--json'], env=self.env, input=json.dumps(payload),
                           capture_output=True, text=True, timeout=60)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr + '\n' + self.screen(name) + '\nBracketing pane samples: ' +
                         json.dumps(dict(before=before, after=self.terminal(name))))
        receipt = json.loads(p.stdout)
        self.assertEqual((receipt['request_id'], receipt['status']), (payload['request_id'], 'submitted'))
        return receipt

    def settled(self, name, receipt):
        """The submitted turn has ended and the session is Ready again: Kimi's
        reply came after this submission, and hgs would pause the session now.
        SessionStart confirms the conversation idle before the turn starts."""
        return settled_turn(self.inspect(name), receipt)

    def brief(self, state):
        keys = ('activity', 'phase', 'conversation_id', 'expected_id', 'active_tools', 'subagents',
                'subagent_groups', 'reply_id', 'reply_at', 'allowed_actions', 'action_reasons')
        return json.dumps({key: state.get(key) for key in keys}, ensure_ascii=False)[:2000]

    def held(self, text):
        """How often the newest model request's conversation holds `text` as a
        user message of its own. Kimi adds its reminders as separate messages."""
        found = []
        for message in (self.requests[-1].get('messages', []) if self.requests else []):
            content = message.get('content')
            if isinstance(content, list):
                content = ''.join(part.get('text', '') for part in content if isinstance(part, dict))
            if message.get('role') == 'user':
                found.append(content)
        return found.count(text)

    def test_launch_prompt_waits_for_trust_and_is_not_replayed_on_resume(self):
        name, text = 'kimi/p/initial', 'Initial launch message\\'
        launch = str(uuid.uuid4())
        self.hgs('kimi', 'p', '--new', '-n', 'initial', '-d', '--launch-id', launch, '--', text)
        state = self.until(lambda: (s if any(q.get('can_answer') for q in s.get('pending_questions', [])) else None)
                           if (s := self.inspect(name)) else None, name)
        self.assertEqual(self.binding(name)['launch_id'], launch)
        self.assertIn('Trust this folder?', self.screen(name))
        self.assertFalse(self.requests, 'the launch message bypassed folder trust')
        self.assertNotIn(text, self.binding(name)['base'], 'resume recipe contains the launch prompt')
        card = next(q for q in state['pending_questions'] if q.get('can_answer'))
        self.assertEqual(self.answer(name, card, 0)['status'], 'answered')
        self.until(lambda: self.held(text) == 1, name)
        record = self.until(lambda: (r if r.get('launch_prompt', {}).get('status') == 'submitted' else None)
                            if (r := self.binding(name)) else None, name)
        request = record['launch_prompt']['request_id']
        receipt = json.loads((self.root / 'state/input_receipts' / (request + '.json')).read_text())
        self.assertEqual((receipt['status'], receipt['first_message'], receipt['text']), ('submitted', True, text))
        state = self.until(lambda: self.settled(name, receipt), name)
        conversation = state['conversation_id']
        self.assert_process(name, 'initial')
        calls = len(self.requests)
        self.hgs('pause', name)
        self.hgs('resume', name, '-d')
        self.until(lambda: self.inspect(name).get('activity') == 'idle', name)
        self.assertEqual(self.binding(name)['conversation_id'], conversation)
        self.assertEqual(len(self.requests), calls, 'resume replayed the initial message')
        followup = self.send(name, self.payload(name, 'Follow-up after initial launch'))
        self.until(lambda: settled_recorded_turn(self.inspect(name), followup,
                                                'Follow-up after initial launch'), name)
        self.assertEqual(self.held(text), 1)
        self.assert_process(name, 'resumed')

    def test_declined_initial_prompt_is_cancelled_and_inspection_is_bounded(self):
        name, text = 'kimi/p/declined', 'Private initial message ' * 100
        self.hgs('kimi', 'p', '--new', '-n', 'declined', '-d', '--', text)
        state = self.until(lambda: (s if any(q.get('can_answer') for q in s.get('pending_questions', [])) else None)
                           if (s := self.inspect(name)) else None, name)
        pending = state['launch_prompt']
        self.assertEqual(pending['status'], 'pending')
        self.assertNotIn('text', pending)
        self.assertEqual(pending['preview'], text[:200] + '…')
        self.assertLess(len(json.dumps(pending)), 1024)
        self.assertFalse(self.requests)
        self.assert_process(name, 'initial')
        card = next(q for q in state['pending_questions'] if q.get('can_answer'))
        self.assertEqual(self.answer(name, card, 1)['status'], 'answered')
        self.until(lambda: self.binding(name).get('launch_prompt', {}).get('status') == 'cancelled', name)
        self.assertFalse(self.requests, 'declining trust delivered the initial message')
        self.assertFalse((self.root / 'state/input_receipts' / (pending['request_id'] + '.json')).exists())

    def test_exact_input_reaches_the_model_once(self):
        # Existing account-wide tracking entries predate the managed TOML
        # block. A supervised launch must retain them without adding another
        # handler that executes the same HGS callback for every native event.
        tracked = ('SessionStart', 'SessionEnd', 'UserPromptSubmit', 'PreToolUse', 'PostToolUse',
                   'PermissionRequest', 'SubagentStart', 'SubagentStop', 'PreCompact', 'PostCompact', 'Stop',
                   'PostToolUseFailure', 'StopFailure', 'Notification', 'Interrupt', 'TurnStarted',
                   'UserPromptQueued', 'PermissionResult', 'TaskStarted', 'SessionHeartbeat')
        command = shlex.join([str(lifecycle.HGS), '__state', 'hook'])
        config = self.home / 'config.toml'
        with config.open('a') as output:
            for event in tracked:
                output.write('\n[[hooks]]\nevent = %s\ncommand = %s\ntimeout = 5\n' %
                             (json.dumps(event), json.dumps(command)))
        name = self.launch()
        registered = tomllib.loads(config.read_text())['hooks']
        for event in tracked:
            self.assertEqual(sum(row['event'] == event and row['command'] == command for row in registered), 1)
        conversation = None
        native_turns = set()
        for text in ('Continue the fixture\\', 'Literal paths:\nfixture\\\\', 'Проверка ввода',
                     '/help/fixture/capture.png\nWhat is shown in this screenshot?'):
            payload = self.payload(name, text)
            receipt = self.send(name, payload)
            self.assertEqual(receipt['status'], 'submitted')
            self.assertEqual(receipt['text'], text)
            self.until(lambda: self.held(text), name)
            state = self.until(lambda: self.settled(name, receipt), name)
            # The first message created the conversation; later ones continue it.
            conversation = conversation or state['conversation_id']
            self.assertEqual(state['conversation_id'], conversation)
            self.assertEqual(self.held(text), 1)
            hooks = [event for event in state['events'] if event['type'] == 'TurnStarted' and event.get('detail') == text]
            self.assertEqual(len(hooks), 1)
            turn = hooks[0].get('kimi_turn_id')
            self.assertIs(type(turn), int)
            self.assertEqual(hooks[0]['kimi_conversation_id'], conversation)
            self.assertNotIn(turn, native_turns)
            native_turns.add(turn)
            matching = [row for row in state.get('kimi_turns', []) if row['turn_id'] == turn]
            self.assertEqual(len(matching), 1)
            self.assertIn(hashlib.sha256(text.strip().encode()).hexdigest(), matching[0]['text_sha256'])
            # The same request identity returns its receipt and never types again.
            requests = len(self.requests)
            self.assertEqual(self.send(name, payload), receipt)
            time.sleep(1)
            self.assertEqual(len(self.requests), requests)
            self.assertEqual(self.held(text), 1)

    def test_pause_and_resume_keep_the_conversation(self):
        name = self.launch()
        text = 'Start the fixture conversation'
        receipt = self.send(name, self.payload(name, text))
        self.until(lambda: self.held(text), name)
        accepted = self.until(lambda: self.settled(name, receipt), name)
        self.assertEqual(self.held(text), 1)
        before = self.binding(name)
        try:
            self.hgs('pause', name)
        except AssertionError as error:
            raise AssertionError('%s\nAccepted as settled: %s\nAfter the refusal: %s'
                                 % (error, self.brief(accepted), self.brief(self.inspect(name)))) from error
        self.until(lambda: subprocess.run([str(self.bin / 'tmux'), 'has-session', '-t', '=' + name], env=self.env,
                                          capture_output=True).returncode != 0, name)
        self.hgs('resume', name, '-d')
        self.until(lambda: self.binding(name).get('run_id') != before['run_id'] and
                   self.inspect(name).get('activity') == 'idle', name)
        self.assert_process(name, 'resumed')
        self.assertEqual(self.binding(name)['conversation_id'], before['conversation_id'])
        resumed = self.binding(name)
        self.hgs('terminate', name, '--expected-run-id', resumed['run_id'])
        archives = [json.loads(path.read_text()) for path in (self.root / 'state/archives').glob('*.json')]
        self.assertTrue(any(row['conversation_id'] == before['conversation_id'] and row['run_id'] == resumed['run_id'] for row in archives))

    def test_rolling_window_429_recovers_in_the_same_conversation(self):
        name = self.launch()
        self.model_server.failure = ("You've reached your 5-hour usage limit. Your quota will reset when the current "
                                     "5-hour window ends. To continue now, purchase extra usage or upgrade your plan: "
                                     "https://example.invalid/subscription?tab=quota")
        text = 'Finish the synthetic recovery fixture'
        self.send(name, self.payload(name, text))

        def failed():
            state = self.inspect(name)
            return state if state.get('phase') == 'error' and state.get('provider_error') else None
        failure = self.until(failed, name)
        self.assertEqual(failure['provider_error']['error_kind'], 'rate_limit')
        self.assertIn('429', failure['provider_error']['detail'])
        self.assertIn(self.model_server.failure, failure['provider_error']['detail'])
        self.assertFalse(failure['provider_error'].get('reset_at'))
        self.assertEqual(len(self.requests), 1)  # Native attempts finished first.
        self.assertEqual(self.held(text), 1)
        binding = self.binding(name)
        self.assert_process(name, 'rate-limit-stopped')
        directory = self.root / 'state/recovery'
        directory.mkdir(exist_ok=True)
        policy = dict(version=2, revision=1, enabled=True, enabled_at=1,
                      rate_limit=True, service=True, network=True, schedules=dict(rate_limit=[5,0]))
        path = directory / 'policy.json'
        path.write_text(json.dumps(policy))
        self.hgs('recovery', 'tick')
        waiting = self.inspect(name)['recovery']
        self.assertEqual((waiting['state'], waiting['class'], waiting['attempt']), ('waiting', 'rate_limit', 0))
        self.assertEqual(waiting['not_before'], 0.0)
        self.model_server.failure = None

        def recovered():
            self.hgs('recovery', 'tick')
            return settled_recovery(self.inspect(name))
        finished = self.until(recovered, name, seconds=60)
        job = finished['recovery']
        self.assertEqual((job['attempt'], job['attempts']), (1, dict(rate_limit=1)))
        self.assertIn('[HGS automatic recovery]', job['receipt']['text'])
        self.assertEqual(len(self.requests), 2)
        self.assertEqual(self.held(text), 1)  # No replay of the original request.
        self.assertEqual(self.held(job['receipt']['text']), 1)
        self.assertEqual((finished['run_id'], finished['conversation_id']), (binding['run_id'], binding['conversation_id']))
        self.assertEqual(self.binding(name)['pid'], binding['pid'])
        self.assertEqual(finished['phase'], 'idle')
        self.assertEqual(json.loads(path.read_text()), policy)
        self.assert_process(name, 'recovered')
        self.hgs('recovery', 'tick')
        self.assertEqual(len(self.requests), 2)


class FixtureControls(unittest.TestCase):
    """The fixture's own checks, without a native Kimi, against the states Kimi
    2.1.1 and hgs's hooks produce: none may pass an unfinished start or turn."""

    IMAGE = Path('/fixture/private/kimi')
    CAPTURES = '/fixture/private/root/kimi-exec'
    BIRTH = (1792368000, 123456)
    KEYS = ('HOME', 'KIMI_CODE_HOME', 'KIMI_CODE_BASE_URL', 'KIMI_CODE_NO_AUTO_UPDATE',
            'KIMI_DISABLE_TELEMETRY', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_STATE_HOME',
            'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'TMPDIR')
    # Darwin's apple strings after 0 to 7 alignment bytes; their value is never read.
    APPLE = b'\0\0\0executable_path=/fixture/private/kimi\0ptr_munge=synthetic-apple-value\0'

    class Files:
        """os.open, fstat, read and close over one capture folder; no real file is touched."""

        def __init__(self, folder, files, order=None, folder_mode=stat.S_IFDIR | 0o700, folder_uid=None):
            self.folder, self.files, self.order = folder, files, order if order is not None else []
            self.folder_stat = (folder_mode, os.geteuid() if folder_uid is None else folder_uid)
            self.fds, self.closed, self.calls, self.read_bytes = {}, [], [], 0

        def open(self, path, flags, mode=0o777, *, dir_fd=None):
            self.calls.append(('open', path, flags, dir_fd))
            if dir_fd is None:
                if path != self.folder:
                    raise FileNotFoundError(2, 'no such folder')
                self.order.append('capture')
                self.fds[100] = None
                return 100
            if dir_fd not in self.fds:
                raise OSError(9, 'not an open folder')
            entry = self.files.get(path)
            if isinstance(entry, BaseException):
                raise entry
            if entry is None:
                raise FileNotFoundError(2, 'no such capture')
            fd = 101 + len(self.fds)
            self.fds[fd] = dict(entry, offset=0)
            return fd

        def fstat(self, fd):
            entry = self.fds[fd]
            if entry is None:
                return os.stat_result((self.folder_stat[0], 1, 1, 2, self.folder_stat[1], 0, 64, 0, 0, 0))
            return os.stat_result((entry.get('mode', stat.S_IFREG | 0o600), 1, 1, entry.get('links', 1),
                                   entry.get('uid', os.geteuid()), 0, entry.get('size', len(entry['data'])), 0, 0, 0))

        def read(self, fd, count):
            entry = self.fds[fd]
            chunk = entry['data'][entry['offset']:entry['offset'] + count]
            entry['offset'] += len(chunk)
            self.read_bytes += len(chunk)
            self.calls.append(('read', count))
            return chunk

        def close(self, fd):
            self.closed.append(fd)

        def patched(self):
            return mock.patch.multiple(os, **{name: mock.Mock(side_effect=getattr(self, name))
                                              for name in ('open', 'fstat', 'read', 'close')})

    @staticmethod
    def capture_bytes(pid, birth, argv, env, abi=ABI_TEXT):
        payload = b''.join(item + b'\0' for item in argv + env)
        return (b'ZKSHIM2 abi=%s pid=%d start=%d.%06d argc=%d envc=%d payload=%d\n' %
                (abi, pid, birth[0], birth[1], len(argv), len(env), len(payload)) + payload + b'\n')

    def procargs_bytes(self, argv, env, area=None, argc=None, exec_path=None, tail=None):
        launched = b''.join(item + b'\0' for item in argv)
        return ((len(argv) if argc is None else argc).to_bytes(4, 'little') +
                os.fsencode(str(exec_path or self.IMAGE)) + b'\0' * 7 + (launched if area is None else area) +
                b''.join(row + b'\0' for row in env) + (self.APPLE if tail is None else tail))

    def kernel_diagnostic(self, values=None, files=None, raw=None, identities=None, binding=None,
                          procargs_error=None, argv=None):
        """One kernel sample of a shim-started child: the real capture reader over
        mocked file calls, mocked identity and argument-area reads; nothing real."""
        case = NativeKimi('test_pause_and_resume_keep_the_conversation')
        case.image, case.image_digest = self.IMAGE, 'fixture-image-hash'
        case.env = {key: '/fixture/private/' + key.lower() for key in self.KEYS}
        case.captures = self.CAPTURES
        launched = dict(PATH='/fixture/private/bin', TMUX='/fixture/private/tmux,1,0', **case.env)
        launched.update(values or {})
        env = [os.fsencode(key + '=' + value) for key, value in launched.items() if value is not None]
        argv = argv or [b'kimi', b'--session', b'fixture-session']
        binding = binding or dict(pid=12345, process_start=lstart_text(self.BIRTH[0]),
                                  supervisor=dict(pid=4242, start='Sun Oct 18 00:00:00 2026'))
        identity = dict(pid=12345, own=True, zombie=False, ppid=4242, birth=self.BIRTH, image=self.IMAGE,
                        parent_start='Sun Oct 18 00:00:00 2026')
        identities = list(identities or [identity, identity])
        order = []
        if files is None:
            files = {capture_name(12345, self.BIRTH): dict(data=self.capture_bytes(12345, self.BIRTH, argv, env))}
        captures = self.Files(self.CAPTURES, files, order)
        case.binding = lambda name: binding
        case.until = lambda predicate, name: self.assertTrue(predicate())

        def process_sample(pid):
            order.append('sample')
            return dict(image='expected', start_lstart='Sun Oct 18 00:00:01 2026', start_exit_code=0)

        def kernel_identity(pid):
            order.append('identity')
            return identities.pop(0)

        def procargs(pid):
            order.append('procargs')
            if procargs_error:
                raise procargs_error
            return self.procargs_bytes(argv, env) if raw is None else raw

        error = None
        with mock.patch.object(case, 'process_sample', side_effect=process_sample), \
                mock.patch.object(MacProcess, 'identity', side_effect=kernel_identity), \
                mock.patch.object(MacProcess, 'procargs', side_effect=procargs), \
                mock.patch(__name__ + '.executable', return_value=case.image), \
                mock.patch(__name__ + '.digest', return_value=case.image_digest), \
                mock.patch.object(sys, 'platform', 'darwin'), \
                mock.patch.object(subprocess, 'run', side_effect=AssertionError('no subprocess may run')), \
                mock.patch('builtins.print') as printed, captures.patched():
            try:
                case.assert_process('kimi/p/private', 'resumed')
            except (AssertionError, OSError) as failure:
                error = failure
        logs = [call.args[0] for call in printed.call_args_list if call.args[0].startswith('ZERUS_KIMI_ENVIRONMENT=')]
        self.assertEqual(len(logs), 1)
        self.assertEqual(sorted(captures.closed), sorted(captures.fds))
        return error, json.loads(logs[0].split('=', 1)[1]), logs[0], order, (argv, env)

    def test_the_proven_environment_distinguishes_match_missing_and_different(self):
        for value, classification in (('/fixture/private/home', 'match'), (None, 'missing'),
                                      ('/fixture/another-home', 'different')):
            with self.subTest(classification=classification):
                error, row, retained, order, _ = self.kernel_diagnostic(dict(HOME=value))
                self.assertEqual(order, ['sample', 'identity', 'capture', 'procargs', 'identity', 'sample'])
                self.assertEqual((row['pid'], row['stage'], row['sample']['source']), (12345, 'resumed', 'kernel'))
                self.assertEqual((row['sample']['capture'], row['sample']['abi'], row['sample']['area'],
                                  row['sample']['problems']), ('admitted', ABI_TEXT.decode(), 'launched', []))
                self.assertEqual(row['keys']['HOME'], classification)
                for value_text in ('/fixture/private/home', '/fixture/another-home', 'synthetic-apple-value'):
                    self.assertNotIn(value_text, retained)
                if classification == 'match':
                    self.assertIsNone(error)
                else:
                    self.assertIsInstance(error, AssertionError)
                    self.assertIn('unexpected private child environment key: HOME', str(error))

    def test_kimis_title_and_a_zero_padded_apple_area_are_accepted(self):
        argv = [b'kimi', b'--session', b'fixture-session']
        launched = b''.join(item + b'\0' for item in argv)
        titled = TITLE + b'\0' * (len(launched) - len(TITLE))
        _, _, _, _, (argv, env) = self.kernel_diagnostic(argv=argv)
        for raw, area in ((self.procargs_bytes(argv, env, area=titled), 'title'),
                          (self.procargs_bytes(argv, env, tail=self.APPLE.lstrip(b'\0')), 'launched')):
            error, row, _, _, _ = self.kernel_diagnostic(raw=raw, argv=argv)
            self.assertIsNone(error)
            self.assertEqual((row['sample']['area'], row['sample']['problems']), (area, []))
        # At cap 5 the truncated title 'kimi' is the launched argv itself: both states are one.
        short = launch_oracle(self.procargs_bytes([b'kimi'], env, area=b'kimi\0'), [b'kimi'], env, self.IMAGE)
        self.assertEqual((short['valid'], short['area']), (True, 'launched'))

    def test_an_environment_that_cannot_be_proven_fails_closed(self):
        argv = [b'kimi', b'--session', b'fixture-session']
        _, _, _, _, (argv, env) = self.kernel_diagnostic(argv=argv)
        name = capture_name(12345, self.BIRTH)
        good = self.capture_bytes(12345, self.BIRTH, argv, env)
        identity = dict(pid=12345, own=True, zombie=False, ppid=4242, birth=self.BIRTH, image=self.IMAGE,
                        parent_start='Sun Oct 18 00:00:00 2026')
        start = dict(pid=12345, process_start=lstart_text(self.BIRTH[0]),
                     supervisor=dict(pid=4242, start='Sun Oct 18 00:00:00 2026'))
        failures = {
            'no capture': dict(files={}),
            'only another process': dict(files={capture_name(999, self.BIRTH): dict(data=good)}),
            'a capture of another start': dict(files={name: dict(data=self.capture_bytes(12345, (1, 1), argv, env))}),
            'a malformed capture': dict(files={name: dict(data=b'ZKSHIM2 abi=136 pid=12345\n')}),
            'a truncated capture': dict(files={name: dict(data=good[:-3])}),
            'another ABI': dict(files={name: dict(data=self.capture_bytes(12345, self.BIRTH, argv, env,
                                                                          abi=b'136:4:12:16:20:120:136'))}),
            'missing row': dict(raw=self.procargs_bytes(argv, env[:-1], tail=b'')),
            'injected row': dict(raw=self.procargs_bytes(argv, [b'HOME=/forged'] + env)),
            'reordered rows': dict(raw=self.procargs_bytes(argv, env[1:] + env[:1])),
            'unrelated bytes': dict(raw=self.procargs_bytes(argv, [b'\xff\0garbage'])),
            'other argv area': dict(raw=self.procargs_bytes(argv, env, area=b'x' * sum(len(a) + 1 for a in argv))),
            'other argc': dict(raw=self.procargs_bytes(argv, env, argc=9)),
            'other exec path': dict(raw=self.procargs_bytes(argv, env, exec_path='/fixture/other/kimi')),
            'short buffer': dict(raw=b'\x03\0'),
            'changed between samples': dict(identities=[identity, dict(identity, birth=(self.BIRTH[0], 1))]),
            'another start than captured': dict(identities=[dict(identity, birth=(1, 1))] * 2),
            'another image': dict(identities=[dict(identity, image=Path('/fixture/other/kimi'))] * 2),
            'zombie': dict(identities=[dict(identity, zombie=True)] * 2),
            'another owner': dict(identities=[dict(identity, own=False)] * 2),
            'identity unavailable': dict(identities=[dict(pid=12345, error='OSError')] * 2),
            'another parent': dict(identities=[dict(identity, ppid=7)] * 2),
            'another supervisor start': dict(binding=dict(start, supervisor=dict(pid=4242,
                                                                                 start='Mon Oct 19 00:00:00 2026'))),
            'hgs tracks another start': dict(binding=dict(start, process_start='Sat Oct 17 00:00:00 2026')),
            # Each of these differs only where its one check looks.
            'a duplicate key in the exact envp': dict(
                files={name: dict(data=self.capture_bytes(12345, self.BIRTH, argv, env + env[:1]))},
                raw=self.procargs_bytes(argv, env + env[:1])),
            'a malformed row in the exact envp': dict(
                files={name: dict(data=self.capture_bytes(12345, self.BIRTH, argv, env + [b'NOEQUALS']))},
                raw=self.procargs_bytes(argv, env + [b'NOEQUALS'])),
            'a PID reused within the same second': dict(identities=[dict(identity, birth=(self.BIRTH[0], 7))] * 2),
        }
        for label, options in failures.items():
            with self.subTest(failure=label):
                error, row, retained, _, _ = self.kernel_diagnostic(argv=argv, **options)
                self.assertIsInstance(error, AssertionError)
                self.assertIn('cannot be proven', str(error))
                self.assertTrue(row['sample']['problems'])
                self.assertIsNone(row['keys'])
                self.assertNotIn('/fixture/private/home', retained)
                self.assertNotIn('synthetic-apple-value', retained)

    def test_each_identity_mismatch_is_named(self):
        """identity_problems itself, condition by condition, including those the
        capture's name and header already refuse before it is asked."""
        record = dict(pid=12345, birth=self.BIRTH)
        identity = dict(pid=12345, own=True, zombie=False, ppid=4242, birth=self.BIRTH, image=self.IMAGE,
                        parent_start='Sun Oct 18 00:00:00 2026')
        binding = dict(pid=12345, process_start=lstart_text(self.BIRTH[0]),
                       supervisor=dict(pid=4242, start='Sun Oct 18 00:00:00 2026'))
        self.assertEqual(identity_problems(identity, identity, record, self.IMAGE, binding), [])
        for label, change, problem in (
                ('another record start', dict(record=dict(record, birth=(self.BIRTH[0], 7))),
                 'did not start as the shim'),
                ('changed', dict(after=dict(identity, ppid=7)), 'changed while'),
                ('another image', dict(both=dict(identity, image=Path('/fixture/other'))), 'copied image'),
                ('another owner', dict(both=dict(identity, own=False)), 'live process of this user'),
                ('a zombie', dict(both=dict(identity, zombie=True)), 'live process of this user'),
                ('unavailable', dict(both=dict(pid=12345, error='OSError')), 'live process of this user'),
                ('another hgs start', dict(binding=dict(binding, process_start='Sat Oct 17 00:00:00 2026')),
                 'hgs tracks another'),
                ('another supervisor', dict(binding=dict(binding, supervisor=dict(pid=7, start='x'))), 'supervisor')):
            with self.subTest(mismatch=label):
                before = change.get('both', identity)
                problems = identity_problems(before, change.get('after', change.get('both', identity)),
                                             change.get('record', record), self.IMAGE, change.get('binding', binding))
                self.assertTrue(any(problem in text for text in problems), problems)

    def test_argv_words_never_supply_the_environment(self):
        argv = [b'kimi', b'HOME=/fixture/private/home', b'KIMI_CODE_HOME=/fixture/private/kimi_code_home']
        error, row, _, _, _ = self.kernel_diagnostic(dict(HOME=None, KIMI_CODE_HOME=None), argv=argv)
        self.assertIsInstance(error, AssertionError)
        self.assertEqual((row['keys']['HOME'], row['keys']['KIMI_CODE_HOME']), ('missing', 'missing'))

    def test_credential_and_kimi_overrides_are_still_refused_without_values(self):
        for key, field in (('OPENAI_API_KEY', 'forbidden_keys'), ('KIMI_EXTRA', 'unexpected_kimi_keys')):
            with self.subTest(key=key):
                error, row, retained, _, _ = self.kernel_diagnostic({key: 'synthetic-sensitive-value'})
                self.assertIsInstance(error, AssertionError)
                self.assertEqual(row[field], [key])
                self.assertNotIn('synthetic-sensitive-value', retained)

    def test_a_kernel_read_failure_is_preserved_and_unusable_metadata_redacted(self):
        error, row, retained, _, _ = self.kernel_diagnostic({'OPENAI_API_KEY': 'synthetic-sensitive-value'},
                                                            procargs_error=OSError(1, 'sysctl failed'))
        self.assertIsInstance(error, OSError)
        self.assertIsNone(row['keys'])
        self.assertNotIn('synthetic-sensitive-value', retained)
        case = NativeKimi('test_pause_and_resume_keep_the_conversation')
        case.image = Path('/fixture/private/kimi')
        with mock.patch(__name__ + '.executable', side_effect=ProcessLookupError()), \
                mock.patch.object(subprocess, 'run', return_value=subprocess.CompletedProcess(
                    [], 0, stdout='synthetic-untrusted-output')):
            metadata = case.process_sample(12345)
        self.assertEqual(metadata['image'], 'unavailable')
        self.assertIsNone(metadata['start_lstart'])
        self.assertNotIn('synthetic-untrusted-output', json.dumps(metadata))

    def test_a_capture_file_is_admitted_before_it_is_read(self):
        name = capture_name(12345, self.BIRTH)
        good = self.capture_bytes(12345, self.BIRTH, [b'kimi'], [b'HOME=/fixture/private/home'])

        def read(files, **folder):
            captures = self.Files(self.CAPTURES, files, **folder)
            with captures.patched():
                try:
                    return read_capture(self.CAPTURES, 12345, self.BIRTH), captures
                except (OSError, ValueError) as error:
                    return error, captures
                finally:
                    self.assertEqual(sorted(captures.closed), sorted(captures.fds))
        record, captures = read({name: dict(data=good)})
        self.assertEqual((record['argv'], record['env']), ([b'kimi'], [b'HOME=/fixture/private/home']))
        (_, _, folder_flags, folder_fd), (_, opened, file_flags, file_fd) = captures.calls[:2]
        for flag in (os.O_DIRECTORY, os.O_NOFOLLOW, os.O_CLOEXEC):
            self.assertTrue(folder_flags & flag)
        for flag in (os.O_NOFOLLOW, os.O_NONBLOCK, os.O_CLOEXEC):
            self.assertTrue(file_flags & flag)
        self.assertEqual((folder_fd, opened, file_fd), (None, name, 100))
        unread = {
            'a folder that is a file': dict(folder_mode=stat.S_IFREG | 0o700),
            "another user's folder": dict(folder_uid=os.geteuid() + 1),
            'a folder others may use': dict(folder_mode=stat.S_IFDIR | 0o750),
            'a symlink': dict(files={name: OSError(62, 'too many levels of symbolic links')}),
            'a FIFO': dict(files={name: dict(data=good, mode=stat.S_IFIFO | 0o600)}),
            "another user's file": dict(files={name: dict(data=good, uid=os.geteuid() + 1)}),
            'a file others may read': dict(files={name: dict(data=good, mode=stat.S_IFREG | 0o640)}),
            'a second link': dict(files={name: dict(data=good, links=2)}),
            'a file over the bound': dict(files={name: dict(data=good, size=CAPTURE_BYTES + 1)}),
        }
        for label, options in unread.items():
            with self.subTest(refusal=label):
                error, captures = read(options.pop('files', {name: dict(data=good)}), **options)
                self.assertIsInstance(error, (OSError, ValueError))
                self.assertEqual((captures.read_bytes, [call for call in captures.calls if call[0] == 'read']), (0, []))
        grown = dict(data=good + b'x' * CAPTURE_BYTES, size=len(good))
        for label, entry in (('a file that grew past the bound', grown),
                             ('a file that shrank', dict(data=good, size=len(good) + 5))):
            with self.subTest(refusal=label):
                error, captures = read({name: entry})
                self.assertIsInstance(error, ValueError)
                self.assertIn('changed while it was read', str(error))
                self.assertLessEqual(captures.read_bytes, CAPTURE_BYTES + 1)
        with self.assertRaises(ValueError):
            read_capture(self.CAPTURES, 12345, None)

    def test_capture_records_are_strict(self):
        argv, env = [b'kimi', b'--session', b's'], [b'HOME=/fixture/private/home']
        good = self.capture_bytes(12345, self.BIRTH, argv, env)
        self.assertEqual(capture_record(good, 12345, self.BIRTH),
                         dict(pid=12345, birth=self.BIRTH, abi=ABI_TEXT.decode(), argv=argv, env=env))
        long = self.capture_bytes(12345, self.BIRTH, [b'kimi', b'x' * (MAX_STRING + 1)], [])
        other_abi = self.capture_bytes(12345, self.BIRTH, argv, env, abi=b'136:4:12:16:20:120:0')
        later = self.capture_bytes(12345, (self.BIRTH[0] + 1, self.BIRTH[1]), argv, env)
        for label, data in (('another ABI', other_abi),
                            ('another PID', self.capture_bytes(12346, self.BIRTH, argv, env)),
                            ('another second', later),
                            ('another microsecond', self.capture_bytes(12345, (self.BIRTH[0], 1), argv, env)),
                            ('a short payload', good[:-2] + b'\n'), ('trailing bytes', good + b'x'),
                            ('no argv', self.capture_bytes(12345, self.BIRTH, [], env)),
                            ('no header end', good.split(b'\n')[0]), ('over the bound', b'x' * (CAPTURE_BYTES + 1)),
                            ('a string over its bound', long),
                            ('counts that disagree', good.replace(b'envc=1', b'envc=2'))):
            with self.subTest(record=label):
                with self.assertRaises(ValueError):
                    capture_record(data, 12345, self.BIRTH)

    def test_the_shim_checks_every_bound_before_it_allocates_writes_or_execs(self):
        """Static structure of the generated C source: no compiler ran, so no behaviour is claimed."""
        source = shim_source(self.CAPTURES, '/fixture/private/kimi')
        self.assertNotIn('@', source)
        self.assertNotIn('realloc', source)
        for define in ('MAX_ARGC %d' % MAX_ARGC, 'MAX_ENVC %d' % MAX_ENVC, 'MAX_STRING %d' % MAX_STRING,
                       'MAX_PAYLOAD %d' % MAX_PAYLOAD, 'CAPTURE_BYTES %d' % CAPTURE_BYTES):
            self.assertIn('#define ' + define + '\n', source)
        position = source.index
        allocate, write, execute = position('malloc('), position('write(fd'), position('execve(')
        self.assertEqual((source.count('malloc('), source.count('execve(')), (1, 1))
        for bound in ('argc > MAX_ARGC', '++envc > MAX_ENVC', 'length > MAX_STRING', 'payload > MAX_PAYLOAD',
                      'total > CAPTURE_BYTES', 'pbi_start_tvusec >= 1000000'):
            self.assertLess(position(bound), allocate, bound)
        for admission in ('O_DIRECTORY | O_NOFOLLOW', 'private_to_me(&state, S_IFDIR)', 'O_CREAT | O_EXCL | O_NOFOLLOW',
                          'private_to_me(&state, S_IFREG)', 'state.st_nlink != 1', 'state.st_size != 0'):
            self.assertLess(position(admission), write, admission)
        for durable in ('fsync(fd)', 'close(fd)', 'fsync(folder)', 'close(folder)'):
            self.assertLess(write, position(durable))
            self.assertLess(position(durable), execute, durable)
        self.assertTrue(source[execute:].split('\n')[1].strip() == 'refuse();')
        self.assertNotIn('return 0', source)
        self.assertIn('"ZKSHIM2 abi=%zu:%zu:%zu:%zu:%zu:%zu:%zu pid=%d start=%llu.%06llu "\n'
                      '                        "argc=%d envc=%zu payload=%zu\\n",', source)

    def test_the_bsdinfo_layout_and_sysctl_lengths_are_exact(self):
        source = shim_source(self.CAPTURES, '/fixture/private/kimi')
        offsets = layout(BSDINFO)
        self.assertEqual({key: offsets[key] for key in ABI}, ABI)
        self.assertEqual(ABI_TEXT, b'136:4:12:16:20:120:128')
        self.assertIn('_Static_assert(sizeof(struct proc_bsdinfo) == 136,', source)
        for field, name in (('status', 'pbi_status'), ('pid', 'pbi_pid'), ('ppid', 'pbi_ppid'), ('uid', 'pbi_uid'),
                            ('start_sec', 'pbi_start_tvsec'), ('start_usec', 'pbi_start_tvusec')):
            self.assertIn('_Static_assert(offsetof(struct proc_bsdinfo, %s) == %d,' % (name, ABI[field]), source)
        self.assertEqual((sysctl_length(4, 4, exact=4), sysctl_length(100, 4096)), (4, 100))
        for returned, limit, exact in ((0, 4096, None), (4097, 4096, None), (8, 8, 4), (3, 4, 4)):
            with self.subTest(returned=returned, limit=limit, exact=exact):
                with self.assertRaises(OSError):
                    sysctl_length(returned, limit, exact)

    def test_a_kept_fixture_root_keeps_and_reports_the_image_copy(self):
        case = NativeKimi('test_pause_and_resume_keep_the_conversation')
        case.root, images = Path('/fixture/private/root'), Path('/fixture/private/images')
        with mock.patch.object(Path, 'exists', return_value=True), mock.patch.object(shutil, 'rmtree') as rmtree:
            with self.assertRaises(AssertionError) as kept:
                case.discard_images(images)
            rmtree.assert_not_called()
        self.assertIn('kept the Kimi image copy', str(kept.exception))
        with mock.patch.object(Path, 'exists', return_value=False), mock.patch.object(shutil, 'rmtree') as rmtree:
            case.discard_images(images)
            rmtree.assert_called_once_with(images)
        with mock.patch.object(Path, 'exists', return_value=False), \
                mock.patch.object(shutil, 'rmtree', side_effect=PermissionError(13, 'denied')):
            with self.assertRaises(PermissionError):
                case.discard_images(images)

    def test_shim_paths_and_starts_are_plain(self):
        self.assertIn(self.CAPTURES, shim_source(self.CAPTURES, '/fixture/private/kimi'))
        for path in ('relative/kimi', '/fixture/with space/kimi', '/fixture/"quote"', '/fixture/back\\slash',
                     '/fixture/ünicode', '/', '/fixture/@IMAGE@'):
            with self.subTest(path=path):
                with self.assertRaises(ValueError):
                    shim_source(path, '/fixture/private/kimi')
        self.assertEqual(capture_name(12345, (1792368000, 7)), '12345.1792368000.000007')
        self.assertEqual(lstart_text(0), 'Thu Jan  1 00:00:00 1970')
        self.assertEqual(lstart_text(1792281600), 'Sun Oct 18 00:00:00 2026')

    def test_kimis_platform_offers_nothing_and_never_reaches_the_model(self):
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
        server.requests, server.platform, server.platform_completed = [], [], []
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

        def post(path, body):
            request = urllib.request.Request('http://127.0.0.1:%d%s' % (server.server_port, path),
                                             data=json.dumps(body).encode(), method='POST')
            try:
                with opener.open(request, timeout=10) as response:
                    response.read()
                    return response.status
            except urllib.error.HTTPError as error:
                return error.code
        self.assertEqual(post(PLATFORM + '/resource_configs', {'name': 'client_banner'}), 404)
        self.assertEqual(post('/v1/chat/completions', {'messages': [{'role': 'user', 'content': 'x'}]}), 200)
        self.assertEqual(server.platform, [dict(method='POST', path='/resource_configs', name='client_banner')])
        self.assertEqual([request['messages'] for request in server.requests], [[{'role': 'user', 'content': 'x'}]])

    def test_a_start_waits_until_the_platform_response_is_written(self):
        entered, release, completed = threading.Event(), threading.Event(), threading.Event()

        class HeldResponse(Model):
            def send_error(self, code, *args, **options):
                entered.set()
                if not release.wait(5):
                    raise AssertionError('The private response was not released')
                return super().send_error(code, *args, **options)

            def platform(self, body):
                super().platform(body)
                completed.set()

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), HeldResponse)
        server.requests, server.platform, server.platform_completed = [], [], []
        serving = threading.Thread(target=server.serve_forever, daemon=True)
        serving.start()
        statuses, errors = [], []

        def request():
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            message = urllib.request.Request('http://127.0.0.1:%d%s/resource_configs' % (server.server_port, PLATFORM),
                                             data=b'{"name":"client_banner"}', method='POST')
            try:
                with opener.open(message, timeout=10) as response:
                    statuses.append(response.status)
            except urllib.error.HTTPError as error:
                statuses.append(error.code)
                try:
                    error.read()
                except Exception as failure:
                    errors.append(str(failure))
                finally:
                    error.close()
            except Exception as error:
                errors.append(str(error))

        class Stub:
            platform = server.platform
            platform_completed = server.platform_completed

            def screen(self, name):
                return TMUX_KEYS

            def inspect(self, name):
                return dict(first_message_can_send=True, conversation_id=None)

        requesting = threading.Thread(target=request, daemon=True)
        requesting.start()
        try:
            self.assertTrue(entered.wait(3), 'The private response was not held')
            self.assertEqual([row['name'] for row in server.platform], ['client_banner'])
            self.assertEqual(server.platform_completed, [])
            self.assertFalse(NativeKimi.started(Stub(), 'kimi/p/private'), 'A request log is not a completed response')
            release.set()
            self.assertTrue(completed.wait(3), 'The private response was not completed')
            requesting.join(3)
            self.assertEqual(errors, [])
            self.assertEqual(statuses, [404])
            self.assertEqual(server.platform_completed, server.platform)
            self.assertTrue(NativeKimi.started(Stub(), 'kimi/p/private'))
            self.assertEqual(server.requests, [])
        finally:
            release.set()
            requesting.join(10)
            server.shutdown()
            server.server_close()
            serving.join(3)
            self.assertFalse(requesting.is_alive(), 'The private HTTP client did not stop')
            self.assertFalse(serving.is_alive(), 'The private HTTP server did not stop')

    def test_a_start_waits_for_kimis_late_banner_request_and_tmux_warning(self):
        trust = 'Trust this folder?\n ❯ Trust this folder'
        composer = ' ╭──╮\n │ > │\n ╰──╯'
        warning = '   ' + TMUX_KEYS + ' Modified Enter keys may not work.'

        class Scripted(NativeKimi):
            """Kimi's composer takes input before its banner request and warning."""
            ticks = 0
            binding_reads = 0
            requests = []

            def tick(self):
                self.ticks += 1
                return self.ticks

            @property
            def platform(self):
                return [dict(method='POST', path='/resource_configs', name='client_banner')] if self.tick() >= 4 else []

            @property
            def platform_completed(self):
                return [dict(method='POST', path='/resource_configs', name='client_banner')] if self.tick() >= 5 else []

            def shown(self):
                return trust if self.ticks <= 2 else composer + ('\n' + warning if self.ticks >= 6 else '')

            def screen(self, name):
                self.tick()
                return self.shown()

            def inspect(self, name):
                if self.tick() == 1:
                    return dict(pending_questions=[dict(can_answer=True, question_id='q', question_hash='h')])
                return dict(first_message_can_send=True, conversation_id=None)

            def hgs(self, *args, **options):
                return ''

            def binding(self, name):
                self.binding_reads += 1
                return dict(pid=1 if self.binding_reads == 1 else 2, run_id='run', supervisor=dict(pid=1))

            def assert_process(self, name, stage):
                pass

            def answer(self, name, card, choice):
                return dict(status='answered')

            def t(self, *args):
                self.assertEqual(args, ('show-options', '-gv', 'extended-keys'))
                return 'off\n'
        kimi = Scripted()
        kimi.launch()
        self.assertGreaterEqual(kimi.binding_reads, 2, 'the initial supervisor PID is not a native agent identity')
        self.assertIn(TMUX_KEYS, kimi.shown(), 'the start was ready before Kimi drew its tmux warning')
        self.assertGreaterEqual(kimi.ticks, 6)

    def test_recovery_success_waits_for_the_same_native_settlement_as_an_ordinary_turn(self):
        receipt = dict(request_id=str(uuid.uuid4()), status='submitted', submitted_at=100.0)
        ready = dict(conversation_id='conversation', activity='idle', phase='idle', reply_at=101.0,
                     allowed_actions=['pause'], recovery=dict(state='succeeded', receipt=receipt))
        for label, state in {
            'persisted success while the live projection is working': dict(ready, activity='busy', phase='working'),
            'an old reply': dict(ready, reply_at=99.0),
            'no reply yet': dict(ready, reply_at=None),
            'native work still blocks pause': dict(ready, allowed_actions=[]),
            'no original receipt': dict(ready, recovery=dict(state='succeeded')),
            'recovery still retrying': dict(ready, recovery=dict(state='retrying', receipt=receipt)),
        }.items():
            with self.subTest(label=label):
                self.assertFalse(settled_recovery(state))
        self.assertIs(settled_recovery(ready), ready)

    def test_recorded_turn_waits_for_its_async_start_and_bound_stop_without_weakening_settlement(self):
        receipt = dict(submitted_at=100.0)
        start = dict(type='TurnStarted', run_id='run', kimi_conversation_id='conversation',
                     kimi_turn_id=1, detail='Follow-up')
        stop = dict(start, type='Stop', detail='', turn_identity_source='kimi_wire')
        ready = dict(run_id='run', conversation_id='conversation', activity='idle', phase='idle',
                     reply_at=101.0, allowed_actions=['pause'], events=[stop, start])
        self.assertIs(settled_recorded_turn(ready, receipt, 'Follow-up'), ready)
        for events in ([stop], [start], [stop, start, start],
                       [dict(stop, kimi_turn_id=2), start],
                       [dict(stop, kimi_turn_id=True), start],
                       [dict(stop, run_id='previous'), start],
                       [dict(stop, kimi_conversation_id='other'), start],
                       [dict(stop, agent_id='child'), start],
                       [dict(stop, turn_identity_source=None), start]):
            self.assertFalse(settled_recorded_turn(dict(ready, events=events), receipt, 'Follow-up'))
        self.assertFalse(settled_recorded_turn(dict(ready, phase='working'), receipt, 'Follow-up'))

    def test_a_turn_settles_only_with_a_reply_after_its_submission_while_pause_is_allowed(self):
        receipt = dict(request_id=str(uuid.uuid4()), status='submitted', submitted_at=100.0)
        ready = dict(conversation_id='conversation', activity='idle', phase='idle', reply_id='7:101.0',
                     reply_at=101.0, allowed_actions=['pause', 'rename', 'terminate', 'forget'])
        unfinished = {
            # Kimi creates its conversation with the first message: SessionStart
            # confirms it idle before TurnStarted arrives.
            'SessionStart before the turn': dict(ready, reply_id=None, reply_at=None),
            'working': dict(ready, activity='busy', phase='working', reply_id=None, reply_at=None),
            'an earlier reply': dict(ready, reply_id='3:99.0', reply_at=99.0),
            'idle with work hgs still sees': dict(ready, active_tools={'call': {'name': 'Shell'}},
                                                  allowed_actions=['rename', 'terminate', 'forget']),
        }

        class Stub:
            state = None

            def inspect(self, name):
                return self.state
        stub = Stub()
        for shape, state in unfinished.items():
            stub.state = state
            self.assertFalse(NativeKimi.settled(stub, 'kimi/p/native', receipt), shape)
        stub.state = ready
        self.assertEqual(NativeKimi.settled(stub, 'kimi/p/native', receipt), ready)


if __name__ == '__main__':
    unittest.main()
