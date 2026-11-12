"""Frame-gated receipt durability in one original private hgs sender."""
from contextlib import contextmanager
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import time


@contextmanager
def held_sender(root, state, workspace, env, executable, binding, payload):
    folder = root / 'fsync-control'
    folder.mkdir()
    library = folder / ('gate.dylib' if sys.platform == 'darwin' else 'gate.so')
    compiler = ['cc', '-std=c11', '-O2']
    compiler += ['-dynamiclib'] if sys.platform == 'darwin' else ['-shared', '-fPIC']
    compiler += [str(Path(__file__).parent / 'fixtures/opencode-fsync-gate.c'), '-o', str(library)]
    if sys.platform != 'darwin':
        compiler.append('-ldl')
    subprocess.run(compiler, env=env, capture_output=True, check=True, timeout=60)
    witness_read, witness_write = os.pipe()
    release_read, release_write = os.pipe()
    descriptors = {witness_read, witness_write, release_read, release_write}
    sender = None
    deadline = time.monotonic() + 60  # The maintained hgs send allowance.
    try:
        request = folder / 'request.json'
        request.write_text(json.dumps(payload))
        send_env = dict(env, ZERUS_FSYNC_PREFIX=str(state.resolve() / 'opencode/receipt-index') + '/',
                        ZERUS_FSYNC_WITNESS_FD=str(witness_write),
                        ZERUS_FSYNC_RELEASE_FD=str(release_read))
        send_env['DYLD_INSERT_LIBRARIES' if sys.platform == 'darwin' else 'LD_PRELOAD'] = str(library)
        # Set PID before exec, preserving the original sender identity. A fork
        # child's inherited interposer must never enter the parent's pipe gate.
        entry = ('import os,sys; env=dict(os.environ,ZERUS_FSYNC_OWNER_PID=str(os.getpid()));'
                 'os.execve(sys.argv[1],sys.argv[1:],env)')
        stdout_path, stderr_path = folder / 'stdout', folder / 'stderr'
        with request.open() as stdin, stdout_path.open('w') as stdout, stderr_path.open('w') as stderr:
            sender = subprocess.Popen([sys.executable, '-c', entry, str(executable), 'send',
                                       binding['name'], '--json'], env=send_env, cwd=workspace,
                                      stdin=stdin, stdout=stdout, stderr=stderr,
                                      pass_fds=(witness_write, release_read))
        for fd in (witness_write, release_read):
            os.close(fd)
            descriptors.remove(fd)
        ready, _, _ = select.select([witness_read], [], [], max(0, deadline - time.monotonic()))
        if not ready or os.read(witness_read, 32) != (str(sender.pid) + '\n').encode():
            raise AssertionError('The original sender did not reach its receipt fsync')
        if sender.poll() is not None:
            raise AssertionError('The gated sender exited before the helper observation')
        sender.fixture_deadline = deadline
        yield sender
    finally:
        try:
            if sender is not None:
                try:
                    os.write(release_write, b'r')
                except BrokenPipeError:
                    pass
                try:
                    sender.wait(timeout=max(0, deadline - time.monotonic()))
                except subprocess.TimeoutExpired:
                    sender.kill()  # Only this original test-owned CLI, never the service.
                    sender.wait(timeout=5)
                    raise
        finally:
            for fd in descriptors:
                os.close(fd)
    if stdout_path.stat().st_size > 65536 or stderr_path.stat().st_size > 65536:
        raise AssertionError('The private sender exceeded its bounded output')
    sender.receipt = json.loads(stdout_path.read_text()) if sender.returncode == 0 else None
    sender.stderr_text = stderr_path.read_text()
