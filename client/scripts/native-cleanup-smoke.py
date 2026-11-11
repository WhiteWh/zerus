#!/usr/bin/python3
"""Built production GUI -> real native program-stop/cleanup, Linux only.

Uses prebuilt, clean canonical GUI/CLI images, real hgs launch/ownership and a
hook-speaking fake Claude. XTest drives the ordinary GUI; there is no replay,
PerfHost bypass, fabricated receipt, ptrace or replacement native authority.
Every process which can send a signal runs below a private PID-1 reaper in
gui_sandbox plus a user/PID namespace. See native-cleanup-smoke.md for usage.
"""
import argparse
import csv
import ctypes
import hashlib
import io
import itertools
import json
import os
from pathlib import Path
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / 'client/scripts'))
import gui_sandbox


def dump(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def file_hash(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def owner_install():
    """Read-only outer witness; never launch, attach to or message the owner GUI."""
    home = Path.home()
    folder = home / '.local/bin'
    paths = list(itertools.islice(folder.rglob('*'),4097)) if folder.exists() else []
    assert len(paths) <= 4096, 'owner bin inventory exceeds snapshot bound'
    paths.sort()
    records = []
    for path in [folder, *paths, home / '.local/share/hgs/build-provenance.json']:
        if not path.exists() and not path.is_symlink():
            records.append(dict(path=str(path), absent=True))
            continue
        metadata = path.lstat()
        row = dict(path=str(path), mode=metadata.st_mode, device=metadata.st_dev,
                   inode=metadata.st_ino, changed_ns=metadata.st_ctime_ns,
                   modified_ns=metadata.st_mtime_ns)
        if path.is_symlink():
            row['link'] = os.readlink(path)
        if path.is_file():
            row['sha256'] = file_hash(path)
        records.append(row)
    gui = []
    installed = folder / 'hgs-tray'
    if installed.exists():
        selected = installed.stat()
        for path in Path('/proc').iterdir():
            if not path.name.isdigit():
                continue
            try:
                metadata = (path / 'exe').stat()
                if (metadata.st_dev, metadata.st_ino) == (selected.st_dev, selected.st_ino):
                    row = proc(int(path.name))
                    gui.append(dict(pid=row['pid'], birth=row['birth'], exe=row['exe'],
                                    pid_namespace=row['pid_namespace'], sha256=file_hash(path / 'exe')))
            except (FileNotFoundError, PermissionError, ProcessLookupError):
                continue
    return dict(home=str(home), entries=records, running_gui=sorted(gui,key=lambda row:row['pid']))


def namespace():
    return {key: os.readlink('/proc/self/ns/' + key) for key in ('pid', 'user')}


def proc(pid):
    path = Path('/proc') / str(pid)
    raw = (path / 'stat').read_text()
    fields = raw[raw.rfind(')') + 2:].split()
    row = dict(pid=int(pid), parent=int(fields[1]), birth=fields[19], state=fields[0],
               group=int(fields[2]), session=int(fields[3]))
    for key in ('exe', 'cwd'):
        try:
            row[key] = os.readlink(path / key)
        except OSError:
            row[key] = None
    row['pid_namespace'] = os.readlink(path / 'ns/pid')
    row['pid_for_children'] = os.readlink(path / 'ns/pid_for_children')
    row['argv'] = (path / 'cmdline').read_bytes()[:4096].decode(errors='replace').split('\0')[:-1]
    return row


def census():
    pids = [int(p.name) for p in Path('/proc').iterdir() if p.name.isdigit()]
    assert len(pids) <= 256, 'private process census exceeded fixture bound'
    rows = []
    for pid in sorted(pids):
        try:
            rows.append(proc(pid))
        except (FileNotFoundError, ProcessLookupError):
            pass
    expected=namespace()['pid']
    assert all(p['pid_namespace']==p['pid_for_children']==expected for p in rows), rows
    return rows


def admission(root):
    outer = json.loads((root / 'outer.json').read_text())
    current = namespace()
    assert all(current[key] != outer['namespaces'][key] for key in current)
    init = (Path('/proc/1/cmdline').read_bytes()).split(b'\0')
    assert b'--init' in init and str(root).encode() in init, 'PID 1 is not this fixture reaper'
    assert proc(1)['group'] == proc(1)['session'] == 1
    assert os.environ['HOME'] == str(root / 'home')
    assert os.environ['HGS_STATE_DIR'] == str(root / 'state')
    assert os.environ['GDK_BACKEND'] == 'x11'
    assert os.environ['WAYLAND_DISPLAY'] == 'zerus-private-no-wayland'
    assert os.environ['XDG_RUNTIME_DIR'] == str(root / 'runtime')
    assert not list((root / 'runtime').glob('wayland*'))
    dump(root / 'admission.json', dict(namespaces=current, init=proc(1), processes=census(),
         display=os.environ.get('DISPLAY'), runtime=os.environ['XDG_RUNTIME_DIR']))


class NamespaceMonitor:
    """Bounded observations throughout the walk, also checking pid_for_children."""
    def __init__(self, root):
        self.done = threading.Event()
        self.errors = []
        self.root = root
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        try:
            with (self.root / 'namespace-monitor.jsonl').open('w') as log:
                for _ in range(6000):  # Ten minutes at 100 ms; outer watchdog is shorter.
                    rows = census()
                    log.write(json.dumps(dict(at=time.monotonic(), processes=rows)) + '\n')
                    log.flush()
                    if self.done.wait(.1):
                        return
                raise AssertionError('namespace monitor exhausted its bound')
        except BaseException as error:
            self.errors.append(repr(error))

    def check(self):
        assert not self.errors, self.errors

    def close(self):
        self.done.set()
        self.thread.join(timeout=5)
        assert not self.thread.is_alive()
        self.check()


def reaper(root, mode):
    assert os.getpid() == 1
    os.setsid()
    os.umask(0o077)
    dump(root / 'init.json', dict(namespaces=namespace(), identity=proc(1)))
    command = [
        '/usr/bin/dbus-run-session', '--config-file', str(root / 'bus.conf'), '--',
        '/usr/bin/xvfb-run', '-a', '-s', '-screen 0 1440x900x24',
        '/usr/bin/python3', str(Path(__file__).resolve()), '--worker', str(root), mode]
    child = subprocess.Popen(command)
    result = None
    with (root / 'reaper.jsonl').open('w') as log:
        while result is None:
            pid, status = os.waitpid(-1, 0)
            log.write(json.dumps(dict(pid=pid, status=status, at=time.monotonic())) + '\n');log.flush()
            if pid == child.pid:
                result = os.waitstatus_to_exitcode(status)
        until = time.monotonic() + 5
        while time.monotonic() < until:
            try:
                pid, status = os.waitpid(-1, os.WNOHANG)
            except ChildProcessError:
                break
            if pid:
                log.write(json.dumps(dict(pid=pid, status=status, at=time.monotonic())) + '\n');log.flush()
            else:
                time.sleep(.01)
    raise SystemExit(result)


class Screen:
    def __init__(self, root):
        self.root = root
        self.number = 0
        self.x = ctypes.CDLL('libX11.so.6')
        self.t = ctypes.CDLL('libXtst.so.6')
        self.x.XOpenDisplay.argtypes = [ctypes.c_char_p]
        self.x.XOpenDisplay.restype = ctypes.c_void_p
        self.x.XFlush.argtypes = [ctypes.c_void_p]
        self.x.XSetInputFocus.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
        self.x.XStringToKeysym.argtypes = [ctypes.c_char_p]
        self.x.XStringToKeysym.restype = ctypes.c_ulong
        self.x.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
        self.x.XKeysymToKeycode.restype = ctypes.c_uint
        self.t.XTestFakeMotionEvent.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_ulong]
        self.t.XTestFakeButtonEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
        self.t.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
        self.display = self.x.XOpenDisplay(None)
        assert self.display

    def capture(self, label):
        self.number += 1
        path = self.root / f'{self.number:03}-{label}.png'
        subprocess.run(['/usr/bin/import', '-window', 'root', str(path)], check=True, timeout=5)
        result = subprocess.run(['/usr/bin/tesseract', str(path), 'stdout', '--tessdata-dir', str(self.root / 'tessdata'), '-l', 'eng', '--psm', '11', '-c', 'tessedit_create_tsv=1'],
                                capture_output=True, text=True, check=True, timeout=10)
        path.with_suffix('.tsv').write_text(result.stdout)
        inverted=path.with_name(path.stem+'-inverted.png')
        subprocess.run(['/usr/bin/magick',str(path),'-colorspace','Gray','-threshold','70%','-negate',str(inverted)],check=True,timeout=5)
        second=subprocess.run(['/usr/bin/tesseract',str(inverted),'stdout','--tessdata-dir',str(self.root/'tessdata'),'-l','eng','--psm','6','-c','tessedit_create_tsv=1'],capture_output=True,text=True,check=True,timeout=10)
        inverted.with_suffix('.tsv').write_text(second.stdout)
        # Tesseract TSV has literal quote characters, never CSV quoting.
        rows = list(csv.DictReader(io.StringIO(result.stdout), delimiter='\t', quoting=csv.QUOTE_NONE))
        more=list(csv.DictReader(io.StringIO(second.stdout), delimiter='\t', quoting=csv.QUOTE_NONE))
        for row in more:row['page_num']='inverted'
        rows+=more
        assert not rows or 'text' in rows[0], result.stderr
        lines = {}
        for row in rows:
            if not row['text'].strip():
                continue
            key = tuple(row[k] for k in ('page_num', 'block_num', 'par_num', 'line_num'))
            lines.setdefault(key, []).append(row)
        self.lines = []
        for words in lines.values():
            left = min(int(w['left']) for w in words);top = min(int(w['top']) for w in words)
            right = max(int(w['left']) + int(w['width']) for w in words)
            bottom = max(int(w['top']) + int(w['height']) for w in words)
            if any(abs(line['left']-left)<8 and abs(line['top']-top)<8 for line in self.lines):
                continue
            self.lines.append(dict(text=' '.join(w['text'] for w in words), x=(left + right)//2,
                                  y=(top + bottom)//2, left=left, top=top, right=right, bottom=bottom))
        dump(path.with_suffix('.json'), self.lines)
        return self.lines

    def click(self, text, button=1, exact=False):
        matches = [line for line in self.lines if line['text'] == text or (not exact and text in line['text'])]
        clusters=[]
        for match in matches:
            if not any(abs(match['y']-old['y'])<8 and match['left']<=old['right'] and old['left']<=match['right'] for old in clusters):
                clusters.append(match)
        matches=clusters
        assert len(matches) == 1, (text, matches, self.lines)
        line = matches[0]
        self.at(line['x'], line['y'], button)

    def at(self, x, y, button=1):
        self.t.XTestFakeMotionEvent(self.display, -1, x, y, 0)
        self.t.XTestFakeButtonEvent(self.display, button, 1, 0)
        self.t.XTestFakeButtonEvent(self.display, button, 0, 0)
        self.x.XFlush(self.display)

    def key(self, name):
        key = self.x.XKeysymToKeycode(self.display, self.x.XStringToKeysym(name.encode()))
        assert key
        self.t.XTestFakeKeyEvent(self.display, key, 1, 0)
        self.t.XTestFakeKeyEvent(self.display, key, 0, 0)
        self.x.XFlush(self.display)

    def wait(self, label, predicate, seconds=25):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            self.capture(label)
            if predicate(self.lines):
                return
            time.sleep(.25)
        raise AssertionError((label, self.lines))

    def row(self, tag):
        matches = [line for line in self.lines if re.fullmatch(re.escape(tag) + r'( Ready)?', line['text'])]
        assert len(matches) == 1, (tag, matches)
        self.at(matches[0]['x'], matches[0]['y'], 3)


def originals(root, tag, cleanup=False):
    folder = root / 'state/worktree-program-stops'
    files = list(folder.glob('*.original.json'))
    assert len(files) <= 16
    rows = []
    for path in files:
        value = json.loads(path.read_text())
        if path.name.startswith('cleanup-') != cleanup:
            continue
        data = value['input']['stop'] if cleanup else value['input']
        if data['manifest']['capture']['root'] == str(root / 'worktrees' / tag):
            rows.append((path, value))
    assert len(rows) <= 1, (tag, rows)
    return rows


def terminal(root, tag, cleanup=False):
    rows = originals(root, tag, cleanup)
    if not rows:
        return None
    path, original = rows[0]
    result = path.with_name(path.name.replace('.original.json', '.result.json'))
    return (original, json.loads(result.read_text())) if result.exists() else None


def wait_terminal(root, tag, cleanup=False):
    until = time.monotonic() + 35
    while time.monotonic() < until:
        found = terminal(root, tag, cleanup)
        if found:
            return found
        time.sleep(.1)
    raise AssertionError(('missing native journal result', tag, cleanup))


def identity(row):
    return row['pid'], row['birth']


def proof(root, tag, before, after, source, result):
    """Journal targets are checkout-bound; protected process identities survive."""
    original, report = result
    data = original['input']
    manifest = data['manifest']
    checkout = str(root / 'worktrees' / tag)
    assert data['source'] == dict(mode='native', request_id=source['request_id'],
                                 journal=source['journal'], revision=source['revision'])
    assert data['stop'] == dict(kind='normal')  # No Force or escalation in this smoke.
    assert manifest['capture']['root'] == checkout
    captured=json.loads((root/'sessions.json').read_text())[tag]
    assert manifest['capture']['owner_id']==captured['worktree_owner_id']
    assert manifest['capture']['target']['run_id']==captured['run_id']
    assert manifest['capture']['target']['conversation_id']==captured['conversation_id']
    assert manifest['configuration'] == source['configuration']
    targets = {identity(row) for row in manifest['programs']}
    before_ids = {identity(row): row for row in before}
    after_ids = {identity(row): row for row in after}
    assert targets and targets <= before_ids.keys(), (targets, before_ids)
    assert all(before_ids[target]['cwd'] == checkout for target in targets)
    # Program-holder owns a retained root FD, but is never a signal target.
    # It and short-lived read helpers may exit naturally after settlement.
    helpers = {identity(row) for row in before if any(arg in ('program-holder', 'frontend-request') for arg in row['argv'])
               and row['cwd'] != checkout}
    protected = before_ids.keys() - targets - helpers
    assert protected <= after_ids.keys(), ('protected process disappeared', protected - after_ids.keys())
    assert not targets & protected
    missing = before_ids.keys() - after_ids.keys()
    assert missing <= targets | helpers
    if report['state'] == 'complete':
        assert all(row['observation'] == 'exited' for row in report['members'])
        assert not targets & after_ids.keys()
    dump(root / (tag + '-signal-proof.json'), dict(
        namespaces=namespace(), checkout=checkout, original=original, result=report,
        targets=[before_ids[target] for target in sorted(targets)],
        protected=[before_ids[target] for target in sorted(protected)],
        natural_helpers=[before_ids[target] for target in sorted(helpers)],
        disappeared=[before_ids[target] for target in sorted(missing)],
        protected_survived=True, other_pid_namespaces_observed=False))


def worker(root, mode):
    admission(root)
    gui_sandbox.refuse_reachable_guard()
    monitor = NamespaceMonitor(root)
    env = dict(os.environ)
    hgs = root / 'bin/hgs'
    app = root / 'home/.local/bin/hgs-tray'
    counter = 0
    def call(*args, json_result=False, stdin=None):
        nonlocal counter
        counter += 1
        result = subprocess.run([str(hgs), *map(str, args)], env=env, capture_output=True,
                                text=True, input=stdin, timeout=40, cwd=root)
        dump(root / f'cli-{counter:03}.json', dict(args=list(map(str,args)),returncode=result.returncode,
             stdout=result.stdout,stderr=result.stderr))
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout) if json_result else result.stdout
    sys.path.insert(0, str(REPO / 'tests'))
    import test_pause
    local = root / 'home/.local/bin'
    local.mkdir(parents=True)
    # Log only signals the synthetic agents receive; native authority still
    # comes from production startup and its original journals.
    received = '''
    raw = pathlib.Path('/proc/self/stat').read_text()
    birth = raw[raw.rfind(')') + 2:].split()[19]
    fd = os.open(home.parent / 'received-signals.jsonl', os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    try:
        os.write(fd, (json.dumps(dict(role='agent', pid=os.getpid(), birth=birth, signal=args[0], checkout=os.getcwd())) + '\\n').encode())
    finally:
        os.close(fd)
'''
    agent = test_pause.AGENT.replace('def stop(*args):\n', 'def stop(*args):\n' + received)
    (local / 'claude').write_text(agent);(local / 'claude').chmod(0o700)
    (local / 'hgs').symlink_to(hgs)
    staging_census=census()
    assert not any((row['exe'] or '').endswith('/hgs-tray') for row in staging_census)
    dump(root/'updater-scope.json',dict(home=str(root/'home'), namespaces=namespace(),
         processes=staging_census, owner_gui_visible=False, owner_pid_namespace=json.loads((root/'outer.json').read_text())['namespaces']['pid']))
    staged=subprocess.run(['/usr/bin/python3',str(REPO/'tray/update-installed.py'),str(root/'bin/hgs-tray'),str(app)],env=env,cwd=root,capture_output=True,text=True,timeout=30)
    (root/'stage.log').write_text(staged.stdout+staged.stderr)
    assert staged.returncode==0,staged.stderr
    # A sealed own-helper has no sibling installation path. The maintained
    # updater stages the GUI in this PRIVATE HOME's native selected location.
    info = json.loads(subprocess.check_output([str(app), '--build-info'], env=env, timeout=10))
    assert info['CFBundleIdentifier'] == 'com.hgdev.hgs-tray' and info['sourceDirty'] is False, info
    dump(root / 'build-info.json', info)
    (local / 'tmux').write_text('#!/bin/sh\nexec /usr/bin/tmux -S '+str(root / 'tmux/socket')+' -f /dev/null "$@"\n')
    (local / 'tmux').chmod(0o700)
    (local / 'shell').write_text('#!/bin/sh\nshift\nexec /bin/bash -c "$@"\n');(local / 'shell').chmod(0o700)
    # The Unknown control delegates the REAL deletion, then loses its success
    # result. It changes neither the selected hgs nor any native permit/check.
    git = '''#!/usr/bin/python3
import json, os, pathlib, subprocess, sys
root = pathlib.Path(os.environ['HOME']).parent
args = sys.argv[1:]
removal = any(args[i:i+2] == ['worktree', 'remove'] for i in range(len(args)-1))
if not removal:
    os.execv('/usr/bin/git', ['/usr/bin/git', *args])
checkout = pathlib.Path(args[-1])
unknown = checkout == root / 'worktrees/unknown'
# Preserve the native inherited close-only custody descriptor through Git.
result = subprocess.run(['/usr/bin/git', *args], close_fds=False)
row = dict(pid=os.getpid(), args=args, git_returncode=result.returncode,
           lost_success=unknown and result.returncode == 0,
           checkout=str(checkout), checkout_exists=checkout.exists())
with (root / 'git-removals.jsonl').open('a') as log:
    log.write(json.dumps(row) + '\\n')
sys.exit(1 if row['lost_success'] else result.returncode)
'''
    (local / 'git').write_text(git);(local / 'git').chmod(0o700)
    repo = root / 'repo'
    subprocess.run(['/usr/bin/git','init','-q','-b','main',str(repo)], env=env,check=True)
    (repo / 'tracked.txt').write_text('Synthetic original only.\n')
    (repo / 'Cargo.toml').write_text('[package]\nname="fixture"\nversion="0.1.0"\n')
    (repo / '.gitignore').write_text('target/\n')
    for argv in [['add','.'],['-c','user.name=Smoke Fixture','-c','user.email=smoke@example.invalid','commit','-qm','initial']]:
        subprocess.run(['/usr/bin/git','-C',str(repo),*argv],env=env,check=True)
    projects = []
    for tag in ('clean','kept','unknown','changed'):
        checkout = root / 'worktrees' / tag
        call('worktrees','create','--path',repo,'--branch','smoke-'+tag,'--destination',checkout,'--json')
        projects.append(tag+'='+str(checkout))
    (root / 'config/projects.local').write_text('\n'.join(projects)+'\n')
    sessions = {}
    for tag in ('clean','kept','unknown','changed'):
        call('claude',tag,'-n',tag,'-d')
        until = time.monotonic()+15
        while time.monotonic()<until:
            value=call('inspect','claude/'+tag+'/'+tag,'--after',str(2**63-1),json_result=True)
            if value.get('conversation_id'):
                break
            time.sleep(.1)
        assert value.get('conversation_id') and value.get('git_worktree') is True,value
        assert value['git_root'] == str(root / 'worktrees' / tag),value
        sessions[tag]=value
    dump(root / 'sessions.json',sessions)
    occupants=[]
    children={}
    sleeper = '''import json, os, pathlib, signal, sys, time
root = pathlib.Path(sys.argv[1])
def stop(number, frame):
    raw = pathlib.Path('/proc/self/stat').read_text()
    row = dict(role='occupant', pid=os.getpid(), birth=raw[raw.rfind(')')+2:].split()[19],
               signal=number, checkout=os.getcwd())
    with (root/'received-signals.jsonl').open('a') as log:
        log.write(json.dumps(row)+'\\n')
    sys.exit(0)
signal.signal(signal.SIGTERM, stop)
print('ready', flush=True)
time.sleep(400)
'''
    for tag in sessions:
        checkout=root/'worktrees'/tag
        (checkout/'target').mkdir()
        (checkout/'target/CACHEDIR.TAG').write_text('Signature: 8a477f597d28d172789f06886806bc55\n')
        (checkout/'target/generated.bin').write_bytes(b'Synthetic generated cache\n')
        child=subprocess.Popen(['/usr/bin/python3','-c',sleeper,str(root)],cwd=checkout,env=env,
                               stdout=subprocess.PIPE,text=True)
        assert child.stdout.readline() == 'ready\n'
        child.stdout.close()
        children[tag]=child
        occupants.append(dict(tag=tag,identity=proc(child.pid)))
    dump(root/'occupants.json',occupants)
    service=subprocess.Popen([str(hgs),'desktop','serve','--background'],env=env,cwd=root,
        stdout=(root/'service.log').open('wb'),stderr=subprocess.STDOUT,stdin=subprocess.DEVNULL)
    for _ in range(50):
        assert service.poll() is None, (root/'service.log').read_text()
        if call('desktop','status','--json',json_result=True)['running']:break
        time.sleep(.1)
    dump(root/'service-process.json',proc(service.pid))
    appenv=dict(env,ZERUS_CLIENT_HGS=str(hgs),ZERUS_CLIENT_DESKTOP_SERVICE='0',
                ZERUS_CLIENT_QUERY='page=sessions&skin=zerus-light',ZERUS_CLIENT_PERF='smoke')
    assert 'ZERUS_CLIENT_REPLAY' not in appenv
    trace = subprocess.Popen([str(app), '--sessions'], env=appenv, cwd=root,
        stdout=(root / 'app.log').open('wb'), stderr=subprocess.STDOUT)
    dump(root / 'app-process.json', proc(trace.pid))
    try:
        screen=Screen(root)
        screen.wait('startup', lambda lines: any(line['text'].split()[0:1] == ['clean'] for line in lines))
        source=call('desktop','ownership-status','--json',json_result=True)
        assert source['state']=='native' and source['identity_current'] and source['journal'] and source['revision']>0,source
        dump(root / 'ownership-status.json',source)
        dump(root / 'before-offer-census.json',census())
        outcomes={}
        for tag in ('clean','kept','unknown','changed'):
            screen.wait(tag+'-row', lambda lines: any(re.fullmatch(tag+r'( Ready)?', line['text']) for line in lines))
            screen.row(tag);time.sleep(.25);screen.capture(tag+'-menu')
            screen.click('Terminate session')
            # Let native discovery finish before starting short-lived OCR
            # children, which deliberately add process-census churn.
            time.sleep(3)
            screen.wait(tag+'-offer',lambda lines:any('Remove the working folder' in line['text'] for line in lines))
            # The preference is initially false; later confirmations remember it.
            if not any('1 other program' in line['text'] for line in screen.lines):
                screen.click('Remove the working folder')
                screen.wait(tag+'-checked',lambda lines:any('1 other program' in line['text'] for line in lines))
            occupant=next(value['identity'] for value in occupants if value['tag']==tag)
            screen.click('1 other program')
            screen.wait(tag+'-expanded',lambda lines:any('PID '+str(occupant['pid']) in line['text'] for line in lines))
            checkout=root/'worktrees'/tag
            if tag=='kept':
                (checkout/'notes.local').write_text('Keep this newly introduced local data.\n')
            if tag=='changed':
                subprocess.run([str(local/'tmux'),'send-keys','-t','=claude/changed/changed:','switch','Enter'],env=env,check=True,timeout=5)
                until=time.monotonic()+10
                while time.monotonic()<until:
                    fresh=call('inspect','claude/changed/changed','--after',str(2**63-1),json_result=True)
                    if fresh['conversation_id']!=sessions[tag]['conversation_id']:break
                    time.sleep(.1)
                assert fresh['conversation_id']!=sessions[tag]['conversation_id']
                dump(root/'changed-new-source.json',fresh)
            before=census();dump(root/(tag+'-before-stop.json'),before)
            monitor.check()
            screen.click('Stop and remove folder')
            if tag=='changed':
                time.sleep(6)
                screen.capture('changed-refusal')
                fresh=call('inspect','claude/changed/changed','--after',str(2**63-1),json_result=True)
                assert fresh['runtime_state']=='live' and fresh['conversation_id']!=sessions[tag]['conversation_id']
                assert not originals(root,tag) and checkout.exists() and children[tag].poll() is None
                assert not any(json.loads(path.read_text())['name']=='claude/changed/changed'
                               for path in (root/'state/archives').glob('*.json')), 'changed source was archived'
                after=census();dump(root/(tag+'-after-stop.json'),after)
                protected={identity(row) for row in before if 'program-holder' not in row['argv'] and 'frontend-request' not in row['argv']}
                assert protected <= {identity(row) for row in after}
                dump(root/'changed-signal-proof.json',dict(captured=sessions[tag],current=fresh,
                     native_stop_admitted=False, native_stop_journal_count=0,
                     protected=sorted(protected), protected_survived=True, namespaces=namespace()))
                outcomes[tag]='refused changed conversation; no stop or archive'
            else:
                stopped=wait_terminal(root,tag)
                assert stopped[1]['state']=='complete',stopped
                removed=wait_terminal(root,tag,cleanup=True)
                if tag!='kept':
                    assert removed[1]['state']==('unknown' if tag=='unknown' else 'removed'),removed
                    assert not checkout.exists()
                    outcomes[tag]=removed[1]['state']
                else:
                    assert removed[1]['state']=='kept',removed
                    assert (checkout/'notes.local').read_text()=='Keep this newly introduced local data.\n'
                    outcomes[tag]='kept new local data after confirmed program stop'
                children[tag].wait(timeout=5)  # Reap our original child before the after census.
                after=census();dump(root/(tag+'-after-stop.json'),after)
                proof(root,tag,before,after,source,stopped)
                data=stopped[0]['input']
                archive=json.loads((root/'state/archives'/(data['archive_id']+'.json')).read_text())
                assert archive['worktree_owner']['id']==data['manifest']['capture']['owner_id']
                assert archive['conversation_id']==sessions[tag]['conversation_id']
                branch=subprocess.check_output(['/usr/bin/git','-C',str(repo),'rev-parse','refs/heads/smoke-'+tag],env=env,text=True).strip()
                assert branch==archive['worktree_owner']['head']
            monitor.check()
            screen.capture(tag+'-result')
            print(json.dumps(dict(stage=tag,result=outcomes[tag])),flush=True)
        # Read-only original-ID lookups and review reopen cannot replay Unknown.
        unknown=terminal(root,'unknown',cleanup=True)
        original_input=unknown[0]['input']
        files={str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in (root/'state/worktree-program-stops').glob('*.json')}
        for _ in range(2):
            result=call('worktrees','program-cleanup-status','--json',stdin=json.dumps(original_input),json_result=True)
            assert result['found'] and result['report']==unknown[1],result
            # Read-only result chip at the fixed 1440x900 viewport. Wait for
            # the dialog rather than treating delivery of XTest as rendering.
            screen.at(210,887)
            screen.wait('review-unknown',lambda lines:any('Remove working folders' in row['text'] for row in lines))
            screen.key('Escape');time.sleep(.3)
        time.sleep(2)
        assert files=={str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in (root/'state/worktree-program-stops').glob('*.json')}
        removals=[json.loads(line) for line in (root/'git-removals.jsonl').read_text().splitlines()]
        lost=[row for row in removals if row['lost_success']]
        assert len(removals)==2 and len(lost)==1 and lost[0]['checkout_exists'] is False,removals
        received=[json.loads(line) for line in (root/'received-signals.jsonl').read_text().splitlines()]
        allowed={}
        for tag in ('clean','kept','unknown'):
            allowed.update({identity(row):str(root/'worktrees'/tag) for row in terminal(root,tag)[0]['input']['manifest']['programs']})
        assert received and all(allowed.get(identity(row))==row['checkout'] and row['signal']==signal.SIGTERM for row in received),received
        assert {row['checkout'] for row in received if row['role']=='occupant'}=={str(root/'worktrees'/tag) for tag in ('clean','kept','unknown')},received
        monitor.close()
        dump(root/'summary.json',dict(outcomes=outcomes, ownership=source, received_signals=received,
             removals=removals, namespaces=namespace(), other_pid_namespaces_observed=False,
             no_unknown_replay=True, force_attempts=0))
    finally:
        monitor.close()
        if trace.poll() is None:
            app_identity=proc(trace.pid)
            assert app_identity['exe'] == str(app),app_identity
            fd=os.pidfd_open(trace.pid)
            try:signal.pidfd_send_signal(fd,signal.SIGTERM)
            finally:os.close(fd)
        trace.wait(timeout=20)


def outer(args):
    output=args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    shutil.copy2(Path(__file__),output/'fixture-source.py')
    owner_before=owner_install()
    dump(output/'owner-install-before.json',owner_before)
    with tempfile.TemporaryDirectory(prefix='nc-',dir=f'/run/user/{os.getuid()}') as scratch:
        root=Path(scratch)
        for name in ('home','bin','config','data','cache','state','runtime','tmp','tmux','worktrees','tessdata'):
            (root/name).mkdir(mode=0o700)
        for name,source in (('hgs',args.hgs),('hgs-tray',args.app)):
            shutil.copy2(source,root/'bin'/name)
        shutil.copy2(args.tessdata/'eng.traineddata',root/'tessdata/eng.traineddata')
        dump(output/'inputs.json',{name:dict(path=str(source.resolve()),sha256=file_hash(source))
             for name,source in (('hgs',args.hgs),('hgs-tray',args.app),('eng.traineddata',args.tessdata/'eng.traineddata'))})
        dump(root/'outer.json',dict(namespaces=namespace(),root=str(root)))
        env={'PATH':str(root/'home/.local/bin')+':'+str(root/'bin')+':/usr/bin:/bin','LANG':'C.UTF-8',
             'HOME':str(root/'home'),'HGS_CONFIG_DIR':str(root/'config'),'HGS_STATE_DIR':str(root/'state'),
             'HGS_SELF':'fixture','HGS_PEERS':'','HGS_TRACKING':'1','HGS_TAB':'0','SHELL':str(root/'home/.local/bin/shell'),
             'TMPDIR':str(root/'tmp'),'TMUX_TMPDIR':str(root/'tmux'),'XDG_CONFIG_HOME':str(root/'config'),
             'XDG_DATA_HOME':str(root/'data'),'XDG_CACHE_HOME':str(root/'cache'),'XDG_STATE_HOME':str(root/'state'),
             'XDG_RUNTIME_DIR':str(root/'runtime'),'GDK_BACKEND':'x11','WAYLAND_DISPLAY':'zerus-private-no-wayland',
             'NO_AT_BRIDGE':'1','WEBKIT_DISABLE_DMABUF_RENDERER':'1','GSETTINGS_BACKEND':'memory'}
        dump(root/'environment.json',env)
        (root/'bus.conf').write_text('<busconfig><type>session</type><listen>unix:tmpdir='+str(root/'runtime')+'</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
        command=gui_sandbox.sandboxed(['/usr/bin/unshare','--user','--map-current-user','--pid','--fork','--mount-proc',
                 '/usr/bin/env','-i',*[key+'='+value for key,value in env.items()],
                 '/usr/bin/python3',str(Path(__file__).resolve()),'--init',str(root),'full'],keep=[root])
        dump(output/'launch.json',dict(command=command,environment=env))
        result=None
        try:
            with (output/'run.log').open('wb') as log:
                result=subprocess.run(command,stdout=log,stderr=subprocess.STDOUT,timeout=420)
        finally:
            for source in root.rglob('*'):
                if any(part in ('bin','tmp','tmux','runtime','tessdata') for part in source.relative_to(root).parts):
                    continue
                try:
                    regular=stat.S_ISREG(source.lstat().st_mode)
                except FileNotFoundError:
                    continue
                if regular:
                    target=output/'fixture'/source.relative_to(root);target.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(source,target)
            owner_after=owner_install()
            dump(output/'owner-install-after.json',owner_after)
            dump(output/'result.json',dict(returncode=result.returncode if result else None,root=str(root),mode='full',
                 owner_install_unchanged=owner_before==owner_after))
    assert owner_before==owner_after, 'owner installation or GUI changed during this walk; inspect witnesses'
    hashes=[]
    for path in sorted(output.rglob('*')):
        if path.is_file():
            hashes.append(file_hash(path)+'  '+str(path.relative_to(output)))
    (output/'SHA256SUMS').write_text('\n'.join(hashes)+'\n')
    print('Evidence:',output,flush=True)
    print((output/'run.log').read_text()[-2500:],flush=True)
    raise SystemExit(result.returncode)


if __name__ == '__main__':
    if len(sys.argv)>1 and sys.argv[1]=='--init':
        reaper(Path(sys.argv[2]),sys.argv[3])
    elif len(sys.argv)>1 and sys.argv[1]=='--worker':
        worker(Path(sys.argv[2]),sys.argv[3])
    else:
        parser=argparse.ArgumentParser(description=__doc__)
        parser.add_argument('--app',type=Path,required=True,help='clean canonical com.hgdev.hgs-tray build')
        parser.add_argument('--hgs',type=Path,required=True,help='prebuilt real same-source CLI; never an installed owner CLI')
        parser.add_argument('--tessdata',type=Path,required=True,help='directory containing English eng.traineddata')
        parser.add_argument('--output',type=Path,required=True,help='new retained evidence directory')
        args=parser.parse_args()
        args.app=args.app.resolve(strict=True);args.hgs=args.hgs.resolve(strict=True);args.tessdata=args.tessdata.resolve(strict=True)
        for binary in ('bwrap','unshare','dbus-run-session','xvfb-run','Xvfb','tmux','git','magick','import','tesseract'):
            assert shutil.which(binary), 'Missing required smoke dependency: '+binary
        outer(args)
