#!/usr/bin/env python3
"""Resolve one latest official DeepSeek release and seal its private CI install.

The lock-only phase executes no package scripts. The following npm ci retains
the existing fixture's lifecycle-script policy, after every source/integrity is
recorded. No global installation, old lock fallback or native version probe.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import signal
import subprocess
import sys
import time
import unittest
import urllib.request

PACKAGE = '@deepseek-ai/dsh'
SECURITY_OVERRIDES = {'@modelcontextprotocol/client': '2.3.0', 'fflate': '0.8.3'}
SECURITY_MINIMUMS = {'@modelcontextprotocol/client': (2, 3, 0), 'fflate': (0, 8, 3)}
REGISTRY = 'https://registry.npmjs.org'
LATEST = REGISTRY + '/' + PACKAGE + '/latest'
RECEIPT_ENV = 'ZERUS_TEST_DSH_RECEIPT'
MAX_METADATA = 2 * 1024**2
MAX_LOCK = 16 * 1024**2
MAX_ENTRY = 64 * 1024**2
VERSION = r'(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?'
NAME = r'(?:@[a-z0-9._-]+/)?[a-z0-9._-]+'
SOURCE_KEYS = ('allow-git', 'allow-file', 'allow-directory', 'allow-remote')
SOURCE_FLAGS = tuple('--' + name + '=none' for name in SOURCE_KEYS)
CLEANUP_SECONDS = 15


def require(condition, message):
    if not condition:
        raise ValueError(message)


def before(end):
    require(end is None or time.monotonic() < end, 'Original DeepSeek fixture installation deadline expired')


def read_bytes(source, limit, end):
    data = bytearray()
    while len(data) <= limit:
        before(end)
        part = source.read(min(65536, limit + 1 - len(data)))
        before(end)
        if not part:
            break
        data.extend(part)
    return bytes(data)


def bounded(path, limit, end=None):
    before(end)
    with Path(path).open('rb') as source:
        before(end)
        data = read_bytes(source, limit, end)
    before(end)
    require(len(data) <= limit, 'Fixture input exceeds its byte bound: ' + Path(path).name)
    return data


def digest(data):
    return hashlib.sha256(data).hexdigest()


def write_new(path, data, end=None):
    before(end)
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'wb') as out:
        before(end)
        out.write(data)
        before(end)
        out.flush()
        before(end)
        os.fsync(out.fileno())
        before(end)
    before(end)


def sync_directory(path, end=None):
    before(end)
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        before(end)
        os.fsync(descriptor)
        before(end)
    finally:
        os.close(descriptor)
    before(end)


def json_new(path, value, end=None):
    write_new(path, (json.dumps(value, indent=2, sort_keys=True) + '\n').encode(), end)
    sync_directory(path.parent, end)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError('Official npm discovery must not redirect')


def discover(end):
    before(end)
    request = urllib.request.Request(LATEST, headers={'Accept': 'application/json', 'User-Agent': 'Zerus-DeepSeek-CI'})
    with urllib.request.build_opener(NoRedirect).open(request, timeout=min(30, end - time.monotonic())) as response:
        before(end)
        raw = read_bytes(response, MAX_METADATA, end)
    before(end)
    require(len(raw) <= MAX_METADATA, 'Latest npm metadata exceeds its byte bound')
    return raw


def integrity(value):
    require(isinstance(value, str) and value.startswith('sha512-'), 'Official package needs SHA-512 integrity')
    try:
        decoded = base64.b64decode(value[7:], validate=True)
    except ValueError:
        raise ValueError('Invalid package SHA-512 integrity') from None
    require(len(decoded) == 64 and base64.b64encode(decoded).decode() == value[7:], 'Invalid package SHA-512 integrity')


def package_source(name, version, url, seal):
    require(isinstance(name, str) and re.fullmatch(NAME, name), 'Invalid registry package name')
    require(isinstance(version, str) and len(version) <= 128 and re.fullmatch(VERSION, version), 'Invalid exact package version')
    expected = f'{REGISTRY}/{name}/-/{name.rsplit("/", 1)[-1]}-{version}.tgz'
    require(url == expected, 'Package must use its exact official npm tarball')
    integrity(seal)


def selection(raw):
    data = json.loads(raw)
    require(data.get('name') == PACKAGE, 'Latest metadata names another package')
    dist = data.get('dist', {})
    package_source(PACKAGE, data.get('version'), dist.get('tarball'), dist.get('integrity'))
    for field in ('dependencies', 'optionalDependencies', 'peerDependencies'):
        dependencies = data.get(field, {})
        require(isinstance(dependencies, dict) and len(dependencies) <= 1000, 'Unknown direct dependency inventory')
        for name, spec in dependencies.items():
            require(isinstance(name, str) and re.fullmatch(NAME, name) and isinstance(spec, str)
                    and re.fullmatch(r'[0-9A-Za-z.*~^<>=| +_-]{1,256}', spec),
                    'Direct dependencies must use npm registry version ranges or tags')
    entry = data.get('bin', {}).get('dsh')
    require(isinstance(entry, str) and 0 < len(entry) <= 4096 and not entry.startswith('/')
            and all(p not in ('', '.', '..') for p in entry.split('/')) and '\\' not in entry,
            'Latest package must name one contained dsh entry')
    return dict(name=PACKAGE, version=data['version'], url=dist['tarball'], integrity=dist['integrity'], entry=entry)


def validate_security_pins(raw):
    packages = json.loads(raw)['packages']
    for path, item in packages.items():
        for package, minimum in SECURITY_MINIMUMS.items():
            if not path.endswith('node_modules/' + package):
                continue
            version = item.get('version')
            require(isinstance(version, str) and re.fullmatch(r'\d+\.\d+\.\d+', version) is not None,
                    f'{package} security version is not a reviewed release')
            require(tuple(map(int, version.split('.'))) >= minimum,
                    f'{package} security fix regressed: {version}')


def validate_lock(raw, chosen):
    require(len(raw) <= MAX_LOCK, 'Transitive lock exceeds its byte bound')
    lock = json.loads(raw)
    require(lock.get('lockfileVersion') == 3, 'An exact npm v3 lock is required')
    packages = lock.get('packages')
    require(isinstance(packages, dict) and 1 < len(packages) <= 10000, 'Unknown bounded transitive package inventory')
    require(packages.get('', {}).get('dependencies') == {PACKAGE: chosen['version']}, 'Lock root differs from selected latest')
    root = packages.get('node_modules/' + PACKAGE, {})
    require((root.get('version'), root.get('resolved'), root.get('integrity')) ==
            (chosen['version'], chosen['url'], chosen['integrity']), 'Lock replaced the selected latest package')
    for path, item in packages.items():
        if path == '':
            continue
        require(isinstance(path, str) and path.startswith('node_modules/') and
                all(p not in ('', '.', '..') for p in path.split('/')) and '\\' not in path and
                isinstance(item, dict) and not item.get('link'), 'Linked or escaping lock entry')
        name = path.rsplit('node_modules/', 1)[-1]
        package_source(name, item.get('version'), item.get('resolved'), item.get('integrity'))
    return len(packages) - 1


def npm_environment(prefix, end=None):
    # Do not forward npm configuration, account tokens, model keys or HGS state.
    private = prefix / '.private'
    before(end)
    private.mkdir(mode=0o700)
    for name in ('home', 'cache', 'config'):
        before(end)
        (private / name).mkdir(mode=0o700)
    for name in ('user.npmrc', 'global.npmrc'):
        write_new(private / name, b'', end)
    return dict(PATH=os.environ['PATH'], HOME=str(private / 'home'), LANG='C.UTF-8', CI='true',
                XDG_CONFIG_HOME=str(private / 'config'), npm_config_cache=str(private / 'cache'),
                npm_config_userconfig=str(private / 'user.npmrc'),
                npm_config_globalconfig=str(private / 'global.npmrc'), npm_config_registry=REGISTRY,
                npm_config_fetch_retries='0')


def process_stat(pid, end):
    raw = bounded(Path('/proc') / str(pid) / 'stat', 8192, end).decode()
    fields = raw[raw.rfind(')') + 2:].split()
    require(len(fields) >= 20, 'Unknown npm process identity')
    return fields[0], int(fields[2]), int(fields[3]), int(fields[19])


def group_live(pid, end):
    live = []
    before(end)
    with os.scandir('/proc') as entries:
        for count, entry in enumerate(entries):
            before(end)
            require(count < 65536, 'Unknown bounded process inventory during npm cleanup')
            if not entry.name.isdecimal():
                continue
            try:
                state, group, session, _ = process_stat(int(entry.name), end)
            except (FileNotFoundError, ProcessLookupError):
                continue
            if group == pid:
                require(session == pid, 'npm process group has an unknown session')
                if state not in ('Z', 'X'):
                    live.append(int(entry.name))
    before(end)
    return live


def owned_command(command, *, cwd, env, stdin, stdout, stderr, work_end, cleanup_end):
    """Linux only: retain the session leader unreaped until its group is quiet.

    This controls the owned session's original process group. It is not a sandbox
    or a proof about a package deliberately escaping with setsid/setpgid.
    """
    require(sys.platform == 'linux' and hasattr(os, 'WNOWAIT'), 'Linux unreaped-child custody is required')
    before(work_end)
    proc = subprocess.Popen(command, cwd=cwd, env=env, stdin=stdin, stdout=stdout, stderr=stderr,
                            start_new_session=True)
    pid = proc.pid  # Reserve ownership before any later fallible observation.
    identity = None
    failure = None
    try:
        state, group, session, start = process_stat(pid, cleanup_end)
        require(group == session == pid, 'npm did not enter its owned session/group')
        identity = group, session, start
        while True:
            before(work_end)
            if os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT) is not None:
                break
            time.sleep(min(.02, max(0, work_end - time.monotonic())))
    except BaseException as error:
        failure = error
    finally:
        # WNOWAIT keeps this exact leader/PID/PGID reserved. No poll()/wait(),
        # numeric signals or group signals may occur after the final reap.
        try:
            require(identity is not None, 'npm cleanup identity is unknown')
            def signal_group(value):
                before(cleanup_end)
                os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
                _, group, session, start = process_stat(pid, cleanup_end)
                require((group, session, start) == identity, 'npm cleanup leader identity changed')
                os.killpg(pid, value)
            signal_group(signal.SIGTERM)
            grace = min(cleanup_end, time.monotonic() + .5)
            while group_live(pid, cleanup_end) and time.monotonic() < grace:
                time.sleep(.02)
            if group_live(pid, cleanup_end):
                signal_group(signal.SIGKILL)
            while group_live(pid, cleanup_end):
                before(cleanup_end)
                time.sleep(.02)
            before(cleanup_end)
            code = proc.wait(timeout=max(.001, cleanup_end - time.monotonic()))
            before(cleanup_end)
        except BaseException as cleanup_error:
            # Retain all paths and refuse qualification. Do not signal after
            # an unknown/reaped leader, or guess at an unrelated process group.
            raise ValueError('npm group cleanup is unknown; private prefix retained') from cleanup_error
    if failure is not None:
        raise failure
    before(work_end)
    return subprocess.CompletedProcess(command, code)


def run_npm(npm, args, prefix, env, evidence, name, end, *, source_policy=True):
    work_end = end - CLEANUP_SECONDS
    before(work_end)
    # Output stays in the retained CI evidence, not a potentially unbounded pipe.
    with (evidence / (name + '.log')).open('xb') as log:
        result = owned_command([npm, *args, *(SOURCE_FLAGS if source_policy else ()),
                                 '--prefix', str(prefix), '--registry=' + REGISTRY,
                                 '--no-audit', '--no-fund'], cwd=prefix, env=env,
                                stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                                work_end=work_end, cleanup_end=end)
    before(work_end)
    require(result.returncode == 0, 'npm ' + name + ' failed; see retained log, no fallback attempted')
    before(work_end)


def npm_version(value):
    match = re.fullmatch(r'(11)\.([0-9]+)\.([0-9]+)', value)
    require(match is not None and int(match[2]) >= 16,
            'DeepSeek CI requires npm >=11.16.0,<12 with registry source-policy support')


def npm_policy(npm, prefix, env, evidence, end):
    work_end = end - CLEANUP_SECONDS
    run_npm(npm, ['--version'], prefix, env, evidence, 'npm-version', end, source_policy=False)
    version_raw = bounded(evidence / 'npm-version.log', 256, work_end)
    version = version_raw.decode().strip()
    npm_version(version)
    run_npm(npm, ['config', 'list', '--json', '--long'], prefix, env, evidence,
            'npm-config-defaults', end, source_policy=False)
    defaults = bounded(evidence / 'npm-config-defaults.log', MAX_METADATA, work_end)
    known = json.loads(defaults)
    require(isinstance(known, dict) and all(known.get(key) in ('all', 'none', 'root') for key in SOURCE_KEYS),
            'npm does not expose all four required dependency source policies')
    run_npm(npm, ['config', 'list', '--json', '--long'], prefix, env, evidence, 'npm-config-denied', end)
    applied = bounded(evidence / 'npm-config-denied.log', MAX_METADATA, work_end)
    configured = json.loads(applied)
    require(isinstance(configured, dict) and all(configured.get(key) == 'none' for key in SOURCE_KEYS),
            'npm did not enforce all four dependency source policies')
    proof = dict(version=version, executable=str(Path(npm).resolve()),
                 version_sha256=digest(version_raw), defaults_sha256=digest(defaults), configured_sha256=digest(applied),
                 denied_sources=list(SOURCE_FLAGS))
    json_new(evidence / 'npm-policy.json', proof, work_end)
    return proof


def install(prefix, evidence, *, end=None):
    prefix, evidence = Path(prefix), Path(evidence)
    require(prefix.is_absolute() and evidence.is_absolute(), 'Absolute private prefix and evidence paths required')
    require(prefix.resolve() == prefix, 'Canonical private fixture prefix required')
    require(not prefix.exists() and not evidence.exists(), 'Fresh DeepSeek prefix and evidence paths required')
    npm = shutil.which('npm')
    require(npm is not None, 'npm is required for the private official fixture')
    end = time.monotonic() + 600 if end is None else end
    work_end = end - CLEANUP_SECONDS
    before(work_end)
    evidence.mkdir(mode=0o700, parents=True)
    before(work_end)
    prefix.mkdir(mode=0o700)
    env = npm_environment(prefix, work_end)
    npm_policy(npm, prefix, env, evidence, end)
    raw = discover(work_end)  # One latest lookup; a failure never consults the old lock.
    write_new(evidence / 'latest.json', raw, work_end)
    chosen = selection(raw)
    before(work_end)
    manifest = (json.dumps(dict(name='zerus-dsh-latest-fixture', private=True,
                               dependencies={PACKAGE: chosen['version']}, overrides=SECURITY_OVERRIDES), indent=2) + '\n').encode()
    write_new(prefix / 'package.json', manifest, work_end)
    run_npm(npm, ['install', '--package-lock-only', '--ignore-scripts'], prefix, env, evidence, 'resolve', end)
    lock = bounded(prefix / 'package-lock.json', MAX_LOCK, work_end)
    count = validate_lock(lock, chosen)
    validate_security_pins(lock)
    require(bounded(prefix / 'package.json', MAX_METADATA, work_end) == manifest, 'Resolution changed the exact manifest')
    write_new(evidence / 'package.json', manifest, work_end)
    write_new(evidence / 'package-lock.json', lock, work_end)
    record = dict(schema=1, source=LATEST, package=chosen, packages=count, prefix=str(prefix),
                  metadata_sha256=digest(raw), manifest_sha256=digest(manifest), lock_sha256=digest(lock),
                  npm_policy_sha256=digest(bounded(evidence / 'npm-policy.json', MAX_METADATA, work_end)))
    json_new(evidence / 'selection.json', record, work_end)  # Durable before package scripts or tests.
    run_npm(npm, ['ci'], prefix, env, evidence, 'install', end)
    require(bounded(prefix / 'package.json', MAX_METADATA, work_end) == manifest and
            bounded(prefix / 'package-lock.json', MAX_LOCK, work_end) == lock, 'Install changed the selected source lock')
    package = prefix / 'node_modules' / PACKAGE
    require(not package.is_symlink() and package.resolve().is_relative_to(prefix), 'Installed package escapes its prefix')
    installed_raw = bounded(package / 'package.json', MAX_METADATA, work_end)
    installed = json.loads(installed_raw)
    require(installed.get('name') == PACKAGE and installed.get('version') == chosen['version'] and
            installed.get('bin', {}).get('dsh') == chosen['entry'], 'Installed package differs from latest selection')
    executable = package / PurePosixPath(chosen['entry'])
    require(executable.resolve().is_relative_to(package.resolve()) and
            (prefix / 'node_modules/.bin/dsh').resolve() == executable.resolve(), 'Installed dsh escapes its package')
    result = dict(schema=1, status='installed', selection_sha256=digest(bounded(evidence / 'selection.json', MAX_METADATA, work_end)),
                  package_manifest_sha256=digest(installed_raw), entry_sha256=digest(bounded(executable, MAX_ENTRY, work_end)))
    pending, receipt = evidence / 'installed-pending.json', evidence / 'installed.json'
    json_new(pending, result, work_end)
    verify_install(pending, str(prefix / 'node_modules/.bin/dsh'), work_end)
    before(work_end)
    info = pending.stat()
    renamed = False
    try:
        before(work_end)
        os.rename(pending, receipt)
        renamed = True
        sync_directory(evidence, work_end)
        before(work_end)
        return receipt
    except BaseException:
        if renamed and receipt.exists() and (receipt.stat().st_dev, receipt.stat().st_ino) == (info.st_dev, info.st_ino):
            receipt.unlink()
        raise


def verify_install(receipt, executable, end=None):
    before(end)
    receipt = Path(receipt)
    installed = json.loads(bounded(receipt, MAX_METADATA, end))
    record_raw = bounded(receipt.parent / 'selection.json', MAX_METADATA, end)
    record = json.loads(record_raw)
    require(installed.get('schema') == 1 and installed.get('status') == 'installed' and record.get('schema') == 1
            and record.get('source') == LATEST and installed.get('selection_sha256') == digest(record_raw),
            'Unconfirmed latest DeepSeek install receipt')
    policy_raw = bounded(receipt.parent / 'npm-policy.json', MAX_METADATA, end)
    policy = json.loads(policy_raw)
    require(digest(policy_raw) == record.get('npm_policy_sha256') and
            policy.get('denied_sources') == list(SOURCE_FLAGS), 'npm dependency source policy seal changed')
    npm_version(policy.get('version', ''))
    for name, key in [('npm-version.log', 'version_sha256'), ('npm-config-defaults.log', 'defaults_sha256'),
                      ('npm-config-denied.log', 'configured_sha256')]:
        require(digest(bounded(receipt.parent / name, MAX_METADATA, end)) == policy.get(key), 'npm source-policy proof changed')
    raw = bounded(receipt.parent / 'latest.json', MAX_METADATA, end)
    chosen = selection(raw)
    require(chosen == record.get('package') and digest(raw) == record.get('metadata_sha256'), 'Latest selection seal changed')
    prefix = Path(record['prefix'])
    require(prefix.is_absolute(), 'Unknown selected fixture prefix')
    for name, limit, key in [('package.json', MAX_METADATA, 'manifest_sha256'), ('package-lock.json', MAX_LOCK, 'lock_sha256')]:
        sealed = bounded(receipt.parent / name, limit, end)
        require(digest(sealed) == record.get(key) and bounded(prefix / name, limit, end) == sealed, 'Selected fixture source lock changed')
    validate_lock(bounded(prefix / 'package-lock.json', MAX_LOCK, end), chosen)
    package = prefix / 'node_modules' / PACKAGE
    require(not package.is_symlink() and package.resolve().is_relative_to(prefix), 'Installed package escapes its prefix')
    entry = package / PurePosixPath(chosen['entry'])
    require(Path(executable).resolve() == entry.resolve() and entry.resolve().is_relative_to(package.resolve()),
            'Native test would run a different DeepSeek executable')
    require(digest(bounded(package / 'package.json', MAX_METADATA, end)) == installed.get('package_manifest_sha256') and
            digest(bounded(entry, MAX_ENTRY, end)) == installed.get('entry_sha256'), 'Installed DeepSeek bytes changed')
    before(end)
    return chosen['version']


def package_root(executable, env=os.environ):
    receipt = env.get(RECEIPT_ENV)
    if receipt:
        verify_install(receipt, executable)
        record = json.loads(bounded(Path(receipt).parent / 'selection.json', MAX_METADATA))
        return Path(record['prefix']) / 'node_modules' / PACKAGE
    return Path(executable).resolve().parent.parent


def required(env=os.environ):
    return bool(env.get(RECEIPT_ENV)) or env.get('GITHUB_ACTIONS') == 'true' and sys.platform == 'linux'


def admit_fixture(executable, hgs, env=os.environ):
    """Mandatory Linux CI must execute the selected latest, never skip it.

    Optional developer runs retain their existing reviewed-version admission;
    they neither discover upstream versions nor update an installed agent.
    """
    require(executable is not None and Path(hgs).is_file(), 'Official dsh and built hgs are required')
    receipt = env.get(RECEIPT_ENV)
    probe_env = None
    if required(env):
        require(receipt, 'Mandatory DeepSeek CI requires the sealed latest-install receipt')
        expected = verify_install(receipt, executable)
        selected = json.loads(bounded(Path(receipt).parent / 'selection.json', MAX_METADATA))
        private = Path(selected['prefix']) / '.private'
        probe_env = dict(PATH=env.get('PATH', os.environ['PATH']), HOME=str(private / 'home'),
                         DSH_HOME=str(private / 'version-home'), DSH_TELEMETRY_DISABLED='1', LANG='C.UTF-8')
    else:
        expected = '0.2.1-rc.2'
    actual = subprocess.check_output([executable, '--version'], text=True, stdin=subprocess.DEVNULL,
                                     env=probe_env, timeout=20).strip()
    if actual != expected:
        if required(env):
            raise ValueError('Selected latest DeepSeek version differs from the actual fixture: ' + actual)
        raise unittest.SkipTest('Optional contract fixture needs the reviewed version or a sealed latest-install receipt')
    return actual


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prefix', type=Path, required=True)
    parser.add_argument('--evidence', type=Path, required=True)
    args = parser.parse_args()
    end = time.monotonic() + 600
    before(end - CLEANUP_SECONDS)
    print('Preparing one latest official DeepSeek fixture; receipt: ' + str(args.evidence / 'installed.json'), flush=True)
    before(end - CLEANUP_SECONDS)
    # No stdout, stat or other fallible IO after install's final guarded return.
    return install(args.prefix, args.evidence, end=end)


if __name__ == '__main__':
    main()
