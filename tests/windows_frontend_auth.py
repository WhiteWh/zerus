"""Explicit Windows native suite, with exact CI-built CLI and frontend fixture.

No WebView, native agent, logon task or production service is started. The
foreground service has private state and stays passive: authenticated disabling
commits, while enabling approvals remains unavailable.
"""
from contextlib import closing
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import unittest
import uuid
import test_desktop as kernel


class WindowsFrontend(unittest.TestCase):
    cli = kernel.Desktop.cli
    serve = kernel.Desktop.serve
    stop = kernel.Desktop.stop

    def setUp(self):
        self.assertEqual(os.name, 'nt', 'run this native suite on Windows')
        fixture = Path(os.environ['HGS_WINDOWS_FRONTEND_FIXTURE']).resolve(strict=True)
        original_cli = kernel.HGS
        kernel.Desktop.setUp(self)
        stage = self.root / 'native'
        stage.mkdir()
        selected = stage / 'hgs.exe'
        self.frontend = stage / 'hgs-tray.exe'
        shutil.copy2(original_cli, selected)
        shutil.copy2(fixture, self.frontend)
        kernel.HGS = selected
        self.addCleanup(setattr, kernel, 'HGS', original_cli)
        self.env.update(LOCALAPPDATA=str(self.root / 'AppData' / 'Local'))
        self.control = self.root / 'control.json'

    def invoke(self, case='positive'):
        _, status = self.serve()
        request_id = str(uuid.uuid4())
        self.control.write_text(json.dumps(dict(case=case, configuration=status['configuration'],
                                               cli=str(kernel.HGS), request_id=request_id)), encoding='utf-8')
        env = dict(self.env, HGS_FRONTEND_TEST_CONTROL=str(self.control))
        if case == 'huge_env':
            for index in range(3):
                env[f'ZERUS_FRONTEND_FIXTURE_PAD{index}'] = 'λ' * 20_000
        output = subprocess.run([str(self.frontend), '--exact',
                                 'instance::tests::evidence::native_frontend_request_fixture', '--nocapture'],
                                env=env, capture_output=True, text=True, timeout=15,
                                creationflags=subprocess.CREATE_NO_WINDOW)
        self.assertEqual(output.returncode, 0, (output.stdout + output.stderr)[-8000:])
        results = [json.loads(line.removeprefix('FRONTEND_RESULT ')) for line in output.stdout.splitlines()
                   if line.startswith('FRONTEND_RESULT ')]
        self.assertEqual(len(results), 1, output.stdout[-4096:])
        chains = [json.loads(line.removeprefix('FRONTEND_CHAIN ')) for line in output.stdout.splitlines()
                  if line.startswith('FRONTEND_CHAIN ')]
        self.assertEqual(len(chains), 1, output.stdout[-4096:])
        self.assertLessEqual(len(chains[0]), 32)
        print('FRONTEND_OBSERVED_CHAIN ' + json.dumps(dict(case=case, chain=chains[0])), file=sys.stderr)
        self.assertEqual(results[0]['request_id'], request_id)
        if case != 'positive':
            self.assertFalse(results[0]['accepted'])
            self.assertFalse(results[0]['durable'])
        # Safe passive OFF may commit; neither positive nor negative case can
        # enable approvals or grant writers in this privately launched service.
        after = self.cli('desktop', 'status', '--json')
        self.assertFalse(after['writers_enabled'])
        self.assertFalse(after['automatic_approvals'])
        self.assertFalse((self.root / 'unexpected-tool').exists())
        return results[0]

    def mode_check(self, image=None):
        self.control.write_text(json.dumps(dict(case='mode_check', cli=str(kernel.HGS))), encoding='utf-8')
        output = subprocess.run([str(image or self.frontend), '--exact',
                                 'instance::tests::evidence::native_frontend_request_fixture', '--nocapture'],
                                env=dict(self.env, HGS_FRONTEND_TEST_CONTROL=str(self.control)),
                                capture_output=True, text=True, timeout=15,
                                creationflags=subprocess.CREATE_NO_WINDOW)
        self.assertEqual(output.returncode, 0, (output.stdout + output.stderr)[-8000:])
        results = [json.loads(line.removeprefix('MODE_CALLER_RESULT ')) for line in output.stdout.splitlines()
                   if line.startswith('MODE_CALLER_RESULT ')]
        self.assertEqual(len(results), 1, output.stdout[-4096:])
        self.assertFalse(self.desktop.exists(), 'mode admission is read-only')
        return results[0]

    def test_mode_caller_accepts_the_actual_retained_installed_frontend_without_a_console(self):
        result = self.mode_check()
        self.assertEqual(result['code'], 0, result)
        value = json.loads(result['stdout'])
        self.assertTrue(value['admitted'])
        self.assertEqual(value['source'], 'installed_frontend')

    def test_mode_caller_still_refuses_the_real_marked_ancestor_when_the_child_is_clean(self):
        key = 'HGS_RUN_ID'
        previous = os.environ.get(key)
        os.environ[key] = 'native-mode-parent-fixture'
        try:
            result = self.mode_check()
            self.assertNotEqual(result['code'], 0)
            self.assertIn('agent-descended', result['stderr'])
        finally:
            if previous is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = previous

    def test_mode_caller_refuses_an_agent_named_frontend_image(self):
        agent = self.frontend.with_name('codex.exe')
        shutil.copy2(self.frontend, agent)
        result = self.mode_check(agent)
        self.assertNotEqual(result['code'], 0)
        self.assertIn('agent-descended', result['stderr'])

    def test_checking_cli_cannot_be_its_own_frontend_and_cannot_manufacture_a_console(self):
        shutil.copy2(kernel.HGS, self.frontend)
        result = subprocess.run([str(self.frontend), 'desktop', 'registration', 'mode-check', '--json'],
                                env=self.env, capture_output=True, text=True, timeout=8,
                                creationflags=subprocess.CREATE_NO_WINDOW)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Settings', result.stderr)
        self.assertFalse(self.desktop.exists())

    def test_real_canonical_frontend_and_original_child_disable_commits_in_passive_service(self):
        receipt = self.invoke()
        self.assertIs(receipt['accepted'], True)
        self.assertIs(receipt['durable'], True)
        self.assertIs(receipt['enabled'], False)
        self.assertEqual(receipt, dict(request_id=receipt['request_id'], accepted=True, durable=True,
                                      op='automatic_approvals', enabled=False, revision=1))
        database = self.desktop / 'state.sqlite3'
        self.assertTrue(database.is_file())
        with closing(sqlite3.connect(database.as_uri() + '?mode=ro', uri=True)) as db:
            recorded = db.execute('SELECT receipt FROM desktop_delivery_requests WHERE request_id=?',
                                  (receipt['request_id'],)).fetchone()
            self.assertIsNotNone(recorded)
            self.assertEqual(json.loads(recorded[0]), receipt)
            self.assertEqual(db.execute('SELECT enabled,revision FROM desktop_delivery_policy WHERE id=1')
                             .fetchone(), (0, 1))

    def test_authenticated_enable_still_refuses_in_passive_service(self):
        self.assertEqual(self.invoke('enable')['code'], 'desktop_background_unavailable')

    def test_wrong_native_child_birth_refuses_before_actor_admission(self):
        self.assertEqual(self.invoke('wrong_birth')['code'], 'frontend_authentication_refused')

    def test_wrong_cached_configuration_is_not_native_authority(self):
        self.assertEqual(self.invoke('wrong_config')['code'], 'frontend_authentication_refused')

    def test_wrong_native_request_fingerprint_refuses_before_actor_admission(self):
        self.assertEqual(self.invoke('wrong_fingerprint')['code'], 'frontend_authentication_refused')

    def test_oversize_unicode_environment_remains_unavailable(self):
        self.assertEqual(self.invoke('huge_env')['code'], 'frontend_authentication_refused')

    def test_child_stripping_markers_cannot_hide_its_real_tagged_ancestor(self):
        # Only this disposable test driver is tagged. The explicit child env
        # remains clean; the service must inspect the original parent chain.
        key = 'HGS_RUN_ID'
        previous = os.environ.get(key)
        os.environ[key] = 'native-fixture-parent'
        try:
            self.assertEqual(self.invoke('agent_parent')['code'], 'frontend_authentication_refused')
        finally:
            if previous is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = previous


if __name__ == '__main__':
    unittest.main()
