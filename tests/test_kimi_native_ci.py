"""Hosted Kimi helper controls: data, mocked HTTP/processes, no native agent."""
from email.message import Message
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tarfile
import sys
from types import SimpleNamespace
import unittest
from unittest import mock
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('kimi_native_ci', ROOT / 'scripts/ci/kimi-native.py')
ci = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ci)


def headers(**values):
    result = Message()
    for name, value in values.items():
        result[name.replace('_', '-')] = str(value)
    return result


class Response:
    def __init__(self, body, declared=None):
        self.body, self.status, self.url, self.reads = body, 200, 'https://cdn.kimi.com/kimi-code/binaries/2.2.0/fixture', 0
        self.headers = headers(Content_Length=len(body) if declared is None else declared)

    def __enter__(self):
        return self

    def __exit__(self, *args):
        pass

    def read(self, bound):
        self.reads += 1
        value, self.body = self.body[:bound], self.body[bound:]
        return value


class KimiNativeCI(unittest.TestCase):
    def root(self):
        temporary = tempfile.TemporaryDirectory(prefix='hgs-kimi-ci-control-')
        self.addCleanup(temporary.cleanup)
        return Path(temporary.name)

    def test_official_manifests_match_all_four_frozen_pins(self):
        for platform in ('linux-x64', 'darwin-arm64'):
            pins = ci.checked_pins(platform)
            self.assertEqual(tuple(pins), ci.HISTORICAL_VERSIONS)
            self.assertTrue(all(pins[v][platform]['size'] < ci.IMAGE_LIMIT for v in ci.HISTORICAL_VERSIONS))
        for content in ('{"a":1,"a":2}', '{"object":{"a":1,"a":2}}'):
            with self.assertRaisesRegex(ValueError, 'duplicate'):
                ci.strict(content)

    def test_latest_resolution_is_once_and_manifest_target_and_size_are_bound(self):
        sha = hashlib.sha256(b'image').hexdigest()
        manifest = dict(version='2.3.1', tag='@moonshot-ai/kimi-code@2.3.1',
                        platforms={'linux-x64': dict(filename='kimi-code-linux-x64', checksum=sha)})
        raw = json.dumps(manifest).encode()
        with mock.patch.object(ci, 'metadata', side_effect=[(b'2.3.1\n', {}), (raw, {}), (b'', {'size': 5})]) as get:
            record, observed, _ = ci.resolve_latest('linux-x64')
        self.assertEqual(record, dict(schema=1, version='2.3.1', target='linux-x64', image_size=5,
                                     manifest_sha256=hashlib.sha256(raw).hexdigest(), image_sha256=sha))
        self.assertEqual(observed, raw)
        self.assertEqual(get.call_args_list, [mock.call('https://code.kimi.com/kimi-code/latest', 64),
            mock.call('https://code.kimi.com/kimi-code/binaries/2.3.1/manifest.json', 65536),
            mock.call('https://code.kimi.com/kimi-code/binaries/2.3.1/kimi-code-linux-x64', ci.IMAGE_LIMIT, method='HEAD')])
        for body in (b'', b'latest', b'2.3.1-beta', b'2.3.1/../2.2.0', b'2.3.1\n2.2.0'):
            with self.subTest(body=body), mock.patch.object(ci, 'metadata', return_value=(body, {})) as get:
                with self.assertRaises(ValueError):
                    ci.resolve_latest('linux-x64')
                get.assert_called_once()
        for key, value in (('version', '2.2.0'), ('tag', '@other/code@2.3.1')):
            changed = json.dumps(dict(manifest, **{key: value})).encode()
            with mock.patch.object(ci, 'metadata', side_effect=[(b'2.3.1', {}), (changed, {})]) as get:
                with self.assertRaises(ValueError):
                    ci.resolve_latest('linux-x64')
                self.assertEqual(get.call_count, 2)

    def test_latest_metadata_refuses_bad_lengths_encoding_late_reads_and_redirects(self):
        for body, declared in ((b'2.3.1', 5), (b'2.3.1', 4), (b'2.3', 5)):
            response = Response(body, declared)
            with mock.patch.object(ci.urllib.request, 'build_opener') as build:
                build.return_value.open.return_value = response
                if len(body) == declared:
                    self.assertEqual(ci.metadata('https://code.kimi.com/kimi-code/latest', 64)[0], body)
                else:
                    with self.assertRaises(ValueError):
                        ci.metadata('https://code.kimi.com/kimi-code/latest', 64)
        response = Response(b'2.3.1')
        response.headers['Content-Encoding'] = 'gzip'
        with mock.patch.object(ci.urllib.request, 'build_opener') as build:
            build.return_value.open.return_value = response
            with self.assertRaises(ValueError):
                ci.metadata('https://code.kimi.com/kimi-code/latest', 64)
            self.assertEqual(response.reads, 0)
        response = Response(b'2.3.1')
        with mock.patch.object(ci.urllib.request, 'build_opener') as build, \
                mock.patch.object(ci.time, 'monotonic', side_effect=[0, 1, 61]):
            build.return_value.open.return_value = response
            with self.assertRaises(ValueError):
                ci.metadata('https://code.kimi.com/kimi-code/latest', 64)
        source = 'https://code.kimi.com/kimi-code/latest'
        request = ci.OfficialRedirect().redirect_request(urllib.request.Request(source), None, 302, '',
                        headers(Location=source.replace('code.', 'cdn.')), source.replace('code.', 'cdn.'))
        self.assertEqual(request.full_url, 'https://cdn.kimi.com/kimi-code/latest')
        with self.assertRaises(ValueError):
            ci.OfficialRedirect().redirect_request(urllib.request.Request(source, method='HEAD'), None, 302, '',
                        headers(Location=source.replace('code.', 'cdn.')), source.replace('code.', 'cdn.'))

    def test_head_size_is_bounded_without_reading_binary_bytes(self):
        for declared, duplicate, accepted in ((5, False, True), (0, False, False),
                                              (ci.IMAGE_LIMIT + 1, False, False), (5, True, False)):
            response = Response(b'', declared)
            if duplicate:
                response.headers['Content-Length'] = str(declared)
            with mock.patch.object(ci.urllib.request, 'build_opener') as build:
                build.return_value.open.return_value = response
                if accepted:
                    self.assertEqual(ci.metadata('https://code.kimi.com/kimi-code/binaries/2.3.1/kimi-code-linux-x64',
                                    ci.IMAGE_LIMIT, method='HEAD')[1]['size'], 5)
                else:
                    with self.assertRaises(ValueError):
                        ci.metadata('https://code.kimi.com/kimi-code/binaries/2.3.1/kimi-code-linux-x64',
                                    ci.IMAGE_LIMIT, method='HEAD')
                self.assertEqual(response.reads, 0)
        source = 'https://code.kimi.com/kimi-code/binaries/2.3.1/kimi-code-linux-x64'
        redirected = ci.OfficialRedirect('HEAD').redirect_request(urllib.request.Request(source, method='HEAD'),
                      None, 302, '', headers(Location=source.replace('code.', 'cdn.')), source.replace('code.', 'cdn.'))
        self.assertEqual(redirected.get_method(), 'HEAD')

    def test_invalid_target_manifest_never_probes_or_downloads_an_image(self):
        good = dict(filename='kimi-code-linux-x64', checksum='a' * 64)
        for entry in (dict(good, filename='../../image'), dict(good, checksum='A' * 64),
                      dict(good, checksum=True), dict(good, checksum='')):
            raw = json.dumps(dict(version='2.3.1', tag='@moonshot-ai/kimi-code@2.3.1',
                                  platforms={'linux-x64': entry})).encode()
            with mock.patch.object(ci, 'metadata', side_effect=[(b'2.3.1', {}), (raw, {})]) as get:
                with self.assertRaises(ValueError):
                    ci.resolve_latest('linux-x64')
                self.assertEqual(get.call_count, 2)

    def test_one_release_is_staged_nonexecutable_before_its_record_is_admitted(self):
        root = self.root(); images, evidence = root / 'images', root / 'evidence'
        images.mkdir(); evidence.mkdir()
        record = dict(schema=1, version='2.3.1', target='linux-x64', image_size=5,
                      image_sha256=hashlib.sha256(b'image').hexdigest(), manifest_sha256=hashlib.sha256(b'{}').hexdigest())
        def fetch(url, path, size, sha):
            self.assertFalse((images / 'selected-release.json').exists())
            self.assertEqual((images / 'manifest.json').read_bytes(), b'{}')
            self.assertEqual((size, sha), (5, record['image_sha256']))
            path.write_bytes(b'image'); path.chmod(0o400)
            return {'sha256': sha}
        with mock.patch.object(ci, 'resolve_latest', return_value=(record, b'{}', {})) as resolve, \
                mock.patch.object(ci, 'fetch', side_effect=fetch) as download:
            selected, image, admission = ci.stage_latest(images, evidence, 'linux-x64')
        resolve.assert_called_once_with('linux-x64'); download.assert_called_once()
        self.assertEqual((selected, image.name, json.loads(admission.read_text())),
                         (record, 'kimi-code-linux-x64', record))
        self.assertEqual(admission.stat().st_mode & 0o777, 0o400)
        self.assertEqual((images / 'manifest.json').stat().st_mode & 0o777, 0o400)

    def test_failed_image_download_cannot_publish_a_selected_record(self):
        root = self.root(); images, evidence = root / 'images', root / 'evidence'
        images.mkdir(); evidence.mkdir()
        record = dict(version='2.3.1', image_size=5, image_sha256='a' * 64)
        with mock.patch.object(ci, 'resolve_latest', return_value=(record, b'{}', {})), \
                mock.patch.object(ci, 'fetch', side_effect=ValueError('hash mismatch')) as fetch:
            with self.assertRaises(ValueError):
                ci.stage_latest(images, evidence, 'linux-x64')
            fetch.assert_called_once()
        self.assertFalse((images / 'selected-release.json').exists())

    def test_native_orchestration_runs_one_version_and_never_falls_back(self):
        for failure in (False, True):
            root = self.root(); output = root / 'out'; runtime = root / 'runtime'; runtime.mkdir()
            hgs, tmux = root / 'hgs', root / 'tmux'
            hgs.write_bytes(b'hgs'); tmux.write_bytes(b'tmux')
            sha = 'a' * 40
            record = dict(version='2.3.1', image_sha256='b' * 64)
            def stage(images, evidence, platform):
                if failure:
                    raise ValueError('unknown latest metadata')
                image, admission = images / 'kimi-code-linux-x64', images / 'selected-release.json'
                for path in (image, admission, images / 'manifest.json'):
                    path.write_bytes(b'synthetic'); path.chmod(0o400)
                return record, image, admission
            def launch(argv, **kwargs):
                reservation = json.loads((output / 'evidence/reservation.json').read_text())
                self.assertEqual((reservation['state'], reservation['versions'], reservation['native_cases']), ('SPENT', ['2.3.1'], len(ci.METHODS)))
                self.assertIn('HGS_KIMI_TEST_RECORD', kwargs['env'])
                ci.write(Path(argv[5]), dict(tests=len(ci.METHODS), started=list(ci.METHODS), completed=list(ci.METHODS),
                          failures=0, errors=0, skipped=0, expected_failures=0, unexpected_successes=0))
                return object()
            with mock.patch.object(sys, 'argv', [str(ci.__file__), '--source-sha', sha, '--hgs', str(hgs),
                       '--tmux', str(tmux), '--output', str(output)]), \
                    mock.patch.object(ci, 'hosted_paths'), mock.patch.object(ci, 'checked_methods'), \
                    mock.patch.object(ci, 'target', return_value='linux-x64'), \
                    mock.patch.object(ci.tempfile, 'mkdtemp', return_value=str(runtime)), \
                    mock.patch.object(ci.subprocess, 'check_output', return_value=sha), \
                    mock.patch.object(ci, 'source_map', return_value={}), mock.patch.object(ci, 'digest', return_value='b' * 64), \
                    mock.patch.object(ci, 'stage_latest', side_effect=stage) as selected, \
                    mock.patch.object(ci.subprocess, 'Popen', side_effect=launch) as spawned, \
                    mock.patch.object(ci, 'await_child', return_value=0), mock.patch.object(ci, 'retain', return_value={}):
                if failure:
                    with self.assertRaises(ValueError):
                        ci.main()
                    spawned.assert_not_called()
                else:
                    self.assertEqual(ci.main(), 0); spawned.assert_called_once()
                selected.assert_called_once()
            receipt = json.loads((output / 'evidence/receipt.json').read_text())
            self.assertEqual((receipt['outcome'], receipt['completed_versions'], receipt['native_cases']),
                             ('FAIL', 0, None) if failure else ('PASS', 1, len(ci.METHODS)))

    def test_tmux_installer_keeps_linux_defaults_and_explicitly_enables_macos_unicode(self):
        # Only configure argv is under test. All network, archive, hash, make
        # and tmux commands are stand-ins; no compiler or native image runs.
        base = self.root()
        tools, runtime, prefix = base / 'bin', base / 'tmp', base / 'prefix'
        tools.mkdir(); runtime.mkdir(); (prefix / 'bin').mkdir(parents=True)
        arguments = base / 'arguments'
        configure = base / 'configure'
        configure.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$FIXTURE_ARGUMENTS"\n')
        configure.chmod(0o700)
        scripts = dict(curl='exit 0', sha256sum='cat >/dev/null', make='exit 0',
                       realpath='printf "%s\\n" "$2"',
                       tar='mkdir tmux-3.7c; cp "$FIXTURE_CONFIGURE" tmux-3.7c/configure')
        for name, body in scripts.items():
            path = tools / name
            path.write_text('#!/bin/sh\n' + body + '\n'); path.chmod(0o700)
        tmux = prefix / 'bin/tmux'
        tmux.write_text('#!/bin/sh\nprintf "tmux 3.7c\\n"\n'); tmux.chmod(0o700)
        for system in ('Linux', 'Darwin'):
            (tools / 'uname').write_text('#!/bin/sh\nprintf "' + system + '\\n"\n')
            (tools / 'uname').chmod(0o700)
            env = dict(PATH=str(tools) + ':/usr/bin:/bin', RUNNER_TEMP=str(runtime),
                       FIXTURE_CONFIGURE=str(configure), FIXTURE_ARGUMENTS=str(arguments))
            result = subprocess.run([shutil.which('bash'), str(ROOT / 'scripts/ci/install-tmux.sh'), str(prefix)],
                                    env=env, text=True, capture_output=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(arguments.read_text().splitlines(), ['--prefix=' + str(prefix)] +
                             (['--enable-utf8proc', '--enable-jemalloc'] if system == 'Darwin' else []))
            self.assertEqual(list(runtime.iterdir()), [])

    def test_only_reviewed_architectures_and_exact_eight_methods_are_selected(self):
        self.assertEqual(ci.target('Linux', 'x86_64'), 'linux-x64')
        self.assertEqual(ci.target('Darwin', 'arm64'), 'darwin-arm64')
        for pair in (('Linux', 'aarch64'), ('Darwin', 'x86_64'), ('Windows', 'AMD64')):
            with self.assertRaises(ValueError):
                ci.target(*pair)
        self.assertEqual(ci.checked_methods(), ci.METHODS)
        self.assertEqual(len(ci.METHODS), len(set(ci.METHODS)))

    def test_the_only_redirect_is_one_exact_302_to_the_cdn(self):
        source = 'https://code.kimi.com/kimi-code/binaries/2.2.0/manifest.json'
        target = source.replace('code.kimi.com', 'cdn.kimi.com')
        redirect = ci.OfficialRedirect()
        accepted = redirect.redirect_request(urllib.request.Request(source), None, 302, '', headers(Location=target), target)
        self.assertEqual((accepted.full_url, accepted.get_method()), (target, 'GET'))
        with self.assertRaises(ValueError):
            redirect.redirect_request(accepted, None, 302, '', headers(Location=target), target)
        for changed in (target.replace('https:', 'http:'), target + '?query', target + '#fragment',
                        target.replace('cdn.kimi.com', 'cdn.kimi.com:443'), target.replace('https://', 'https://user@'),
                        target.replace('/2.2.0/', '/2.1.1/'), target.replace('cdn.kimi.com', 'other.invalid')):
            with self.subTest(target=changed), self.assertRaises(ValueError):
                ci.OfficialRedirect().redirect_request(urllib.request.Request(source), None, 302, '', headers(Location=changed), changed)
        for code in (301, 303, 307, 308):
            with self.assertRaises(ValueError):
                ci.OfficialRedirect().redirect_request(urllib.request.Request(source), None, code, '', headers(Location=target), target)
        repeated = headers(Location=target)
        repeated['Location'] = target
        with self.assertRaises(ValueError):
            ci.OfficialRedirect().redirect_request(urllib.request.Request(source), None, 302, '', repeated, target)

    def fetch(self, response, expected=b'image', sha=None):
        output = self.root() / 'image'
        opener = mock.Mock()
        opener.open.return_value = response
        record = ci.fetch('https://code.kimi.com/kimi-code/binaries/2.2.0/fixture', output,
                          len(expected), sha or hashlib.sha256(expected).hexdigest(), opener=opener)
        return output, record, opener

    def test_download_is_one_exclusive_nonexecutable_exact_file(self):
        output, record, opener = self.fetch(Response(b'image'))
        self.assertEqual(output.read_bytes(), b'image')
        self.assertEqual(output.stat().st_mode & 0o777, 0o400)
        self.assertEqual(record['sha256'], hashlib.sha256(b'image').hexdigest())
        opener.open.assert_called_once()
        with self.assertRaises(FileExistsError):
            ci.fetch(record['url'], output, 5, record['sha256'], opener=opener)

    def test_status_length_duplicates_and_encoding_refuse_before_body(self):
        for change in ('status', 'short', 'oversized', 'duplicate', 'compressed'):
            response = Response(b'image')
            if change == 'status':
                response.status = 206
            elif change == 'duplicate':
                response.headers['Content-Length'] = '5'
            elif change == 'compressed':
                response.headers['Content-Encoding'] = 'gzip'
            else:
                response.headers.replace_header('Content-Length', '4' if change == 'short' else '6')
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.fetch(response)
            self.assertEqual(response.reads, 0)

    def test_short_extra_and_wrong_hash_never_become_executable(self):
        for body, checksum in ((b'ima', None), (b'images', None), (b'image', '0' * 64)):
            output = self.root() / 'image'
            opener = mock.Mock()
            opener.open.return_value = Response(body, declared=5)
            with self.subTest(body=body), self.assertRaises(ValueError):
                ci.fetch('https://code.kimi.com/kimi-code/binaries/2.2.0/fixture', output, 5,
                         checksum or hashlib.sha256(b'image').hexdigest(), opener=opener)
            self.assertEqual(output.stat().st_mode & 0o111, 0)

    def test_a_late_read_is_refused(self):
        with mock.patch.object(ci.time, 'monotonic', side_effect=[0, 1, 181]):
            with self.assertRaisesRegex(TimeoutError, 'late'):
                self.fetch(Response(b'image'))

    def test_native_failure_summary_names_the_actual_case_and_inventory_size(self):
        case = ci.METHODS[4]
        result = dict(tests=5, failures=1, errors=0, skipped=0,
                      failed_cases=[case], error_cases=[], skipped_cases=[])
        summary = ci.failure_summary(result)
        self.assertIn('all %d exact cases' % len(ci.METHODS), summary)
        self.assertNotIn('all eight', summary)
        self.assertIn(case, summary)
        self.assertIn('"tests": 5', summary)
        self.assertIn('"failures": 1', summary)

    def test_exit_zero_without_exact_cases_and_clean_result_is_not_a_pass(self):
        good = dict(tests=len(ci.METHODS), started=list(ci.METHODS), completed=list(ci.METHODS), failures=0,
                    errors=0, skipped=0, expected_failures=0, unexpected_successes=0)
        self.assertTrue(ci.outcome(good, 0))
        self.assertFalse(ci.outcome(good, 1))
        for name in ('failures', 'errors', 'skipped', 'expected_failures', 'unexpected_successes'):
            for value in (None, False, 0.0, 1):
                with self.subTest(name=name, value=value):
                    self.assertFalse(ci.outcome(dict(good, **{name: value}), 0))
        for value in (0, len(ci.METHODS) - 1, len(ci.METHODS) + 1, float(len(ci.METHODS)), str(len(ci.METHODS))):
            self.assertFalse(ci.outcome(dict(good, tests=value), 0))
        for name in ('started', 'completed'):
            self.assertFalse(ci.outcome(dict(good, **{name: list(ci.METHODS[:-1])}), 0))
            self.assertFalse(ci.outcome(dict(good, **{name: list(reversed(ci.METHODS))}), 0))

    def test_child_environment_is_private_and_does_not_inherit_credentials(self):
        root = self.root()
        with mock.patch.dict(os.environ, {'OPENAI_API_KEY': 'synthetic', 'GITHUB_TOKEN': 'synthetic',
                                         'KIMI_CODE_BASE_URL': 'https://other.invalid', 'HGS_SESSION': 'old'}):
            env = ci.environment(root, root / 'helpers', root / 'hgs')
        self.assertFalse(set(env) & {'OPENAI_API_KEY', 'GITHUB_TOKEN', 'KIMI_CODE_BASE_URL', 'HGS_SESSION'})
        self.assertEqual(env['PATH'], str(root / 'helpers') + ':/usr/bin:/bin')
        self.assertTrue(all(Path(env[key]).is_dir() for key in ('HOME', 'TMPDIR', 'XDG_STATE_HOME')))

    def test_owner_machine_and_nonprivate_build_paths_refuse_before_allocation(self):
        temporary = self.root()
        args = SimpleNamespace(hgs=temporary / 'hgs', tmux=temporary / 'tmux',
                               output=ci.ROOT / 'artifacts/test-results/kimi-native')
        env = dict(GITHUB_ACTIONS='true', RUNNER_ENVIRONMENT='github-hosted',
                   RUNNER_TEMP=str(temporary), GITHUB_WORKSPACE=str(ci.ROOT))
        ci.hosted_paths(args, env)
        for changes in (dict(GITHUB_ACTIONS=''), dict(RUNNER_ENVIRONMENT='self-hosted'),
                        dict(GITHUB_WORKSPACE=str(temporary))):
            with self.assertRaises(ValueError):
                ci.hosted_paths(args, dict(env, **changes))
        args.hgs = Path('/usr/bin/hgs')
        with self.assertRaisesRegex(ValueError, 'private'):
            ci.hosted_paths(args, env)
        self.assertFalse(args.output.exists())

    def test_timeout_stops_only_the_owned_child_and_reports_unknown(self):
        log = self.root() / 'log'
        log.write_bytes(b'')
        child = mock.Mock()
        child.poll.return_value = None
        child.wait.side_effect = [subprocess.TimeoutExpired('suite', 10), 0]
        with mock.patch.object(ci.time, 'monotonic', side_effect=[0, 2]):
            with self.assertRaisesRegex(TimeoutError, 'UNKNOWN'):
                ci.await_child(child, log, seconds=1)
        child.terminate.assert_called_once_with()
        child.kill.assert_called_once_with()
        self.assertEqual(child.wait.call_count, 2)

    def test_custody_uses_only_declared_private_root_attributes(self):
        base = self.root()
        root = base / 'hgs-kimi-native-example'
        root.mkdir(mode=0o700)
        case = mock.Mock(root=root, images=None, image=None, kimi=None)
        case.id.return_value = 'fixture.case'
        output = base / 'custody.jsonl'
        ci.record_roots(case, output)
        row = json.loads(output.read_text())
        self.assertEqual((row['path'], row['inode'], row['case']), (str(root.resolve()), root.stat().st_ino, 'fixture.case'))
        # macOS's temporary directory also has a /var -> /private/var spelling.
        alias = base / 'alias'
        alias.symlink_to(base, target_is_directory=True)
        case.root = alias / root.name
        ci.record_roots(case, output)
        alias_row = json.loads(output.read_text().splitlines()[-1])
        self.assertEqual((alias_row['path'], alias_row['inode']), (row['path'], row['inode']))
        case.root = base
        with self.assertRaisesRegex(ValueError, 'declared'):
            ci.record_roots(case, output)

    def test_retention_never_follows_a_runtime_symlink(self):
        base = self.root()
        runtime = base / 'runtime'
        runtime.mkdir(mode=0o700)
        outside = base / 'outside'
        outside.write_text('outside bytes')
        (runtime / 'link').symlink_to(outside)
        (runtime / 'inside').write_text('fixture bytes')
        archive = base / 'retained.tar.gz'
        result = ci.retain([], runtime, archive)
        self.assertEqual(result['members'], 3)
        with tarfile.open(archive) as data:
            self.assertEqual(data.getmember('root-0/link').type, tarfile.SYMTYPE)
            self.assertEqual(data.extractfile('root-0/inside').read(), b'fixture bytes')
            self.assertNotIn(b'outside bytes', b''.join(data.extractfile(row).read() for row in data.getmembers() if row.isfile()))

    def test_retention_refuses_a_changed_root_and_special_file(self):
        base = self.root()
        runtime = base / 'runtime'
        runtime.mkdir(mode=0o700)
        custody = base / 'custody'
        custody.write_text(json.dumps(dict(path=str(runtime), device=runtime.stat().st_dev,
                                          inode=runtime.stat().st_ino + 1, uid=os.getuid())) + '\n')
        with self.assertRaisesRegex(ValueError, 'identity'):
            ci.retain([custody], runtime, base / 'wrong.tar.gz')
        os.mkfifo(runtime / 'fifo', mode=0o600)
        with self.assertRaisesRegex(ValueError, 'special'):
            ci.retain([], runtime, base / 'special.tar.gz')

    def test_workflow_is_readonly_optional_and_uses_standard_ephemeral_runners(self):
        workflow = (ROOT / '.github/workflows/kimi-native.yml').read_text()
        self.assertIn('  pull_request:', workflow)
        self.assertIn('  workflow_dispatch:', workflow)
        self.assertNotIn('pull_request_target', workflow)
        self.assertIn('permissions:\n  contents: read\ncache-mode: none', workflow)
        for forbidden in ('secrets.', 'secrets: inherit', 'environment:', 'actions/cache', 'self-hosted', 'contents: write'):
            self.assertNotIn(forbidden, workflow)
        self.assertIn('os: ["ubuntu-24.04", "macos-15"]', workflow)
        self.assertIn('CARGO_BUILD_JOBS: 2', workflow)
        for variable, directory in (('CARGO_TARGET_DIR', 'target'), ('CARGO_HOME', 'cargo'), ('RUSTUP_HOME', 'rustup')):
            self.assertIn('echo "' + variable + '=$RUNNER_TEMP/kimi-' + directory + '"', workflow)
        self.assertIn('} >> "$GITHUB_ENV"', workflow)
        self.assertIn('brew install coreutils libevent ncurses utf8proc jemalloc bison pkg-config', workflow)
        self.assertIn('$(brew --prefix utf8proc)/lib/pkgconfig', workflow)
        self.assertIn('$(brew --prefix jemalloc)/lib/pkgconfig', workflow)
        installer = (ROOT / 'scripts/ci/install-tmux.sh').read_text()
        self.assertIn('if [ "$(uname -s)" = Darwin ]; then', installer)
        self.assertIn('persist-credentials: false', workflow)
        exact_source = '${{ github.event.pull_request.head.sha || github.sha }}'
        self.assertIn('SOURCE_SHA: ' + exact_source, workflow)
        self.assertIn('ref: ' + exact_source, workflow)
        for family in ('results', 'retained'):
            self.assertIn('name: kimi-native-' + family + '-${{ matrix.os }}-${{ env.SOURCE_SHA }}', workflow)


if __name__ == '__main__':
    unittest.main()
