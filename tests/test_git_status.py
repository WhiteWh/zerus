#!/usr/bin/env python3
"""Read-only publication evidence from isolated real Git checkouts and remotes."""
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

REPO = Path(__file__).resolve().parents[1]
HGS = Path(os.environ.get('HGS_TEST_BIN', REPO / 'target/debug/hgs')).resolve()


class GitStatus(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='hgs-git-status-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.repo = self.root / 'checkout with spaces'
        self.remote = self.root / 'remote.git'
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(('HGS_', 'GIT_'))}
        self.env.update(HOME=str(self.root / 'home'), HGS_STATE_DIR=str(self.root / 'state'),
                        HGS_SELF='fixture', HGS_PEERS='', GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null',
                        GIT_CEILING_DIRECTORIES=str(self.root))
        self.git('init', '-q', '-b', 'main', str(self.repo))
        self.git('init', '--bare', '-q', str(self.remote))
        (self.repo / 'tracked').write_text('initial\n')
        (self.repo / 'rename').write_text('rename me\n')
        (self.repo / '.gitignore').write_text('ignored\n')
        self.git('-C', str(self.repo), 'add', '.')
        self.commit('initial')
        self.git('-C', str(self.repo), 'remote', 'add', 'origin', str(self.remote))
        self.git('-C', str(self.repo), 'push', '-qu', 'origin', 'main')

    def git(self, *args):
        return subprocess.check_output(['git', *args], env=self.env, stderr=subprocess.PIPE).decode().strip()

    def commit(self, message, repo=None):
        self.git('-C', str(repo or self.repo), '-c', 'user.name=Fixture',
                 '-c', 'user.email=fixture@example.test', 'commit', '--allow-empty', '-qm', message)

    def status(self, path=None):
        proc = subprocess.run([str(HGS), 'git-status', '--path', str(path or self.repo), '--json'],
                              env=self.env, capture_output=True, text=True, timeout=6)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        return json.loads(proc.stdout)

    def clear_verification(self):
        shutil.rmtree(self.root / 'state' / 'git-status-cache', ignore_errors=True)

    def test_verified_clean_read_does_not_mutate_git(self):
        index = (self.repo / '.git/index').read_bytes()
        refs = self.git('-C', str(self.repo), 'show-ref')
        data = self.status()
        self.assertEqual((data['state'], data['remote_state']), ('ok', 'verified'))
        self.assertEqual((data['changed_files'], data['ahead'], data['behind']), (0, 0, 0))
        self.assertEqual(data['root'], str(self.repo))
        self.assertEqual((self.repo / '.git/index').read_bytes(), index)
        self.assertEqual(self.git('-C', str(self.repo), 'show-ref'), refs)
        self.assertFalse((self.repo / '.git/FETCH_HEAD').exists())

    def test_counts_paths_once_including_staged_unstaged_untracked_and_rename(self):
        (self.repo / 'tracked').write_text('staged\n')
        self.git('-C', str(self.repo), 'add', 'tracked')
        (self.repo / 'tracked').write_text('unstaged too\n')
        self.git('-C', str(self.repo), 'mv', 'rename', 'renamed')
        (self.repo / 'new\nname').write_text('untracked')
        (self.repo / 'ignored').write_text('ignored')
        data = self.status()
        self.assertEqual(data['changed_files'], 3)
        self.assertEqual(data['remote_state'], 'verified')

    def test_unpublished_commits_and_push_invalidate_verification(self):
        original = self.status()
        self.commit('not pushed')
        local = self.status()
        self.assertEqual((local['ahead'], local['behind'], local['remote_state']), (1, 0, 'verified'))
        self.git('-C', str(self.repo), 'push', '-q')
        published = self.status()
        self.assertEqual((published['ahead'], published['behind'], published['remote_state']), (0, 0, 'verified'))
        self.assertNotEqual(original['head'], published['head'])
        self.assertGreater(published['remote_checked_at'], original['remote_checked_at'])

    def test_changed_remote_is_not_certified_from_stale_tracking_ref(self):
        peer = self.root / 'peer'
        self.git('clone', '-q', '-b', 'main', str(self.remote), str(peer))
        self.commit('peer advance', peer)
        self.git('-C', str(peer), 'push', '-q')
        data = self.status()
        self.assertEqual((data['ahead'], data['behind']), (0, 0))
        self.assertEqual(data['remote_state'], 'changed')
        self.assertFalse((self.repo / '.git/FETCH_HEAD').exists())
        self.git('-C', str(self.repo), 'fetch', '-q')
        data = self.status()
        self.assertEqual((data['ahead'], data['behind'], data['remote_state']), (0, 1, 'verified'))
        self.commit('independent advance')
        data = self.status()
        self.assertEqual((data['ahead'], data['behind'], data['remote_state']), (1, 1, 'verified'))

    def test_no_upstream_detached_unborn_and_plain_folders(self):
        self.git('-C', str(self.repo), 'branch', '--unset-upstream')
        data = self.status()
        self.assertEqual(data['remote_state'], 'no_upstream')
        self.assertIsNone(data['ahead'])
        self.git('-C', str(self.repo), 'branch', '-q', 'base')
        self.git('-C', str(self.repo), 'branch', '-q', '--set-upstream-to', 'base')
        self.assertEqual(self.status()['remote_state'], 'local_upstream')
        self.git('-C', str(self.repo), 'checkout', '--detach', '-q')
        data = self.status()
        self.assertTrue(data['detached'])
        self.assertEqual(data['remote_state'], 'detached')
        unborn = self.root / 'unborn'
        self.git('init', '-q', '-b', 'main', str(unborn))
        (unborn / 'draft').write_text('new')
        data = self.status(unborn)
        self.assertTrue(data['unborn'])
        self.assertEqual((data['changed_files'], data['remote_state']), (1, 'no_upstream'))
        plain = self.root / 'plain'
        plain.mkdir()
        self.assertEqual(self.status(plain)['state'], 'not_repo')
        self.assertFalse((plain / '.git').exists())

    def test_worktrees_subfolders_and_inherited_git_context(self):
        linked = self.root / 'linked'
        self.git('-C', str(self.repo), 'worktree', 'add', '-qb', 'feature', str(linked))
        nested = linked / 'nested'
        nested.mkdir()
        (nested / 'draft').write_text('local work')
        self.env.update(GIT_DIR=str(self.repo / '.git'), GIT_WORK_TREE=str(self.repo))
        data = self.status(nested)
        self.assertEqual((data['root'], data['branch'], data['changed_files']), (str(linked), 'feature', 1))
        self.assertEqual(data['common_dir'], str(self.repo / '.git'))

    def test_missing_offline_and_reconfigured_remotes_invalidate_old_success(self):
        self.assertEqual(self.status()['remote_state'], 'verified')
        self.git('-C', str(self.repo), 'remote', 'set-url', 'origin', str(self.root / 'missing.git'))
        self.assertEqual(self.status()['remote_state'], 'unavailable')
        self.git('-C', str(self.repo), 'remote', 'set-url', 'origin', str(self.remote))
        self.git('-C', str(self.repo), 'push', '-q', 'origin', '--delete', 'main')
        self.clear_verification()
        self.assertEqual(self.status()['remote_state'], 'missing')

    def test_large_status_is_bounded_and_never_claims_clean(self):
        # hgs prepends HOME/.local/bin ahead of system/Homebrew programs.
        fake = Path(self.env['HOME']) / '.local/bin'
        fake.mkdir(parents=True)
        program = fake / 'git'
        program.write_text('#!/bin/sh\nexec yes "oversized status"\n')
        program.chmod(0o755)
        self.env['PATH'] = str(fake) + os.pathsep + self.env['PATH']
        start = time.monotonic()
        self.assertEqual(self.status()['state'], 'too_large')
        self.assertLess(time.monotonic() - start, 4)

    def test_checkout_changes_during_remote_verification_are_not_certified(self):
        # The remote side of ls-remote changes the checkout, so the change lands
        # after the first status read and before the recheck. Local Git reads stay
        # unwrapped: a slow PATH wrapper could spend their deadline, and a failed
        # upstream lookup skips remote verification altogether.
        hook = self.root / 'upload-pack'
        hook.write_text('#!/bin/sh\nprintf changed > ' + shlex.quote(str(self.repo / 'new-during-check'))
                        + '\nexec git upload-pack "$@"\n')
        hook.chmod(0o755)
        self.git('-C', str(self.repo), 'config', 'remote.origin.uploadpack', shlex.quote(str(hook)))
        data = self.status()
        self.assertEqual((data['state'], data['remote_state']), ('changed_during_check', 'verified'))
        refreshed = self.status()
        self.assertEqual((refreshed['state'], refreshed['changed_files']), ('ok', 1))

    def test_failed_or_stalled_upstream_lookup_is_not_reported_as_no_upstream(self):
        # Only the upstream lookup fails or outlives its deadline; every other
        # call reaches the real Git, so the checkout itself is still read.
        fake = Path(self.env['HOME']) / '.local/bin'
        fake.mkdir(parents=True)
        program = fake / 'git'
        real = shutil.which('git')
        self.env['PATH'] = str(fake) + os.pathsep + self.env['PATH']
        for lookup in ('sys.exit(128)', 'time.sleep(30)'):
            program.write_text('#!' + sys.executable + '\nimport os,sys,time\n'
                               'if "for-each-ref" in sys.argv and any("upstream" in arg for arg in sys.argv):\n ' + lookup + '\n'
                               'os.execv(' + repr(real) + ', [' + repr(real) + '] + sys.argv[1:])\n')
            program.chmod(0o755)
            start = time.monotonic()
            data = self.status()
            self.assertLess(time.monotonic() - start, 4)
            self.assertEqual((data['state'], data['remote_state']), ('ok', 'unknown'), lookup)
            self.assertEqual((data['branch'], data['upstream'], data['changed_files']), ('main', 'origin/main', 0))
            self.assertNotIn('remote_checked_at', data)

    def test_failed_or_stalled_tracking_lookup_is_not_reported_as_changed(self):
        # Only the local tracking ref lookup fails, is killed or outlives its
        # deadline. A shell shim keeps the other local reads within theirs.
        fake = Path(self.env['HOME']) / '.local/bin'
        fake.mkdir(parents=True)
        program = fake / 'git'
        self.env['PATH'] = str(fake) + os.pathsep + self.env['PATH']
        cache = self.root / 'state/git-status-cache'
        for lookup in ('echo "fatal: unable to read refs/remotes/origin/main: Connection timed out" >&2; exit 128',
                       'echo "error: unable to read refs/remotes/origin/main" >&2; exit 1',
                       'kill -KILL $$', 'exec sleep 30'):
            program.write_text("#!/bin/sh\ncase \" $* \" in *' @{upstream} '*) " + lookup + ';; esac\n'
                               'exec ' + shlex.quote(shutil.which('git')) + ' "$@"\n')
            program.chmod(0o755)
            start = time.monotonic()
            data = self.status()
            self.assertLess(time.monotonic() - start, 4)
            self.assertEqual((data['state'], data['remote_state']), ('ok', 'unknown'), lookup)
            self.assertEqual((data['remote'], data['remote_ref'], data['changed_files']), ('origin', 'refs/heads/main', 0))
            self.assertNotIn('remote_checked_at', data)
            self.assertFalse(cache.exists(), lookup)
        # A completed lookup that finds no tracking ref still compares the remote
        # branch with nothing, so a fetch is the remedy.
        program.unlink()
        self.git('-C', str(self.repo), 'update-ref', '-d', 'refs/remotes/origin/main')
        data = self.status()
        self.assertEqual((data['state'], data['remote_state']), ('ok', 'changed'))
        self.assertIn('remote_checked_at', data)
        self.git('-C', str(self.repo), 'fetch', '-q')
        self.assertEqual(self.status()['remote_state'], 'verified')

    def test_expired_remote_verification_cannot_hide_remote_changes(self):
        self.assertEqual(self.status()['remote_state'], 'verified')
        peer = self.root / 'peer'
        self.git('clone', '-q', '-b', 'main', str(self.remote), str(peer))
        self.commit('new remote work', peer)
        self.git('-C', str(peer), 'push', '-q')
        for path in (self.root / 'state/git-status-cache').glob('*.json'):
            data = json.loads(path.read_text())
            data['at'] = time.time() - 61
            path.write_text(json.dumps(data))
        self.assertEqual(self.status()['remote_state'], 'changed')


if __name__ == '__main__':
    unittest.main()
