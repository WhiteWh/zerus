"""Own tmux socket/home and fake agents; optional real Fish/native Codex controls."""
import json
import os
from pathlib import Path
import unittest
import uuid

if os.name != 'posix':
    raise unittest.SkipTest('tmux login-shell bootstrap is Unix-only')

import test_pause
import test_codex_input_native

FISH = os.environ.get('HGS_TEST_FISH')


@unittest.skipUnless(test_pause.TMUX, 'tmux required')
class ShellBootstrap(test_pause.Harness):
    def test_an_unknown_login_shell_is_never_given_guessed_syntax(self):
        marker = self.root / 'unknown-shell-was-run'
        self.script('unknown-shell', '#!/bin/sh\ntouch "' + str(marker) + '"\nexit 93\n')
        self.env['SHELL'] = str(self.bin / 'unknown-shell')
        name = self.start('codex')
        conversation = self.binding(name)['conversation_id']
        self.hgs('pause', name)
        self.resume(name)
        self.assertEqual(self.binding(name)['conversation_id'], conversation)
        self.assertFalse(marker.exists(), 'neither tmux nor the launcher may interpret unknown syntax')

    def test_the_agent_gets_literal_arguments_including_empty_and_final_semicolons(self):
        project = self.root / 'каталог;'
        project.mkdir()
        (self.root / 'cfg/projects.local').write_text('p=' + str(project) + '\n')
        profile = self.root / 'profile;'
        profile.mkdir()
        self.env['CODEX_HOME'] = str(profile)
        values = [';', '\\;', 'tail;', 'quote\'value', '$(not-a-command)', '', 'строка с пробелом']
        self.start('codex', 'literal', '--', '--fixture-literal-argv', *values)
        calls = [json.loads(line) for line in (self.root / 'argv.jsonl').read_text().splitlines()]
        received = calls[-1]['argv']
        start = received.index('--fixture-literal-argv') + 1
        self.assertEqual(received[start:start + len(values)], values)
        self.assertEqual(calls[-1]['codex_home'], str(profile))

    @unittest.skipUnless(FISH, 'Set HGS_TEST_FISH for the actual Fish interpreter')
    def test_real_fish_preserves_literal_arguments(self):
        self.env['SHELL'] = str(Path(FISH).resolve())
        self.test_the_agent_gets_literal_arguments_including_empty_and_final_semicolons()

    @unittest.skipUnless(FISH, 'Set HGS_TEST_FISH for the actual Fish interpreter')
    def test_real_fish_selected_account_paths_keep_backslashes_quotes_and_login_overrides(self):
        self.env['SHELL'] = str(Path(FISH).resolve())
        config = self.root / '.config/fish'
        config.mkdir(parents=True)
        self.env['XDG_CONFIG_HOME'] = str(config.parent)
        # The selected account must win AFTER the real login startup runs.
        (config / 'config.fish').write_text('set -gx CODEX_HOME /tmp/not-the-selected-profile\n')
        for tag, leaf in [('trailing-bs', 'profile with space\\'),
                          ('double-bs', "profile\\\\quoted' $value;")]:
            with self.subTest(profile=leaf):
                profile = self.root / leaf
                profile.mkdir()
                self.env['CODEX_HOME'] = str(profile)
                name = self.start('codex', tag, '--new', '--account', 'native-codex')
                conversation = self.binding(name)['conversation_id']
                self.assertEqual(self.binding(name)['agent_home'], str(profile))
                self.hgs('pause', name)
                self.resume(name)
                self.assertEqual(self.binding(name)['conversation_id'], conversation)
                calls = [json.loads(line) for line in (self.root / 'argv.jsonl').read_text().splitlines()]
                self.assertEqual(calls[-1]['codex_home'], str(profile))

    def test_pre_supervisor_failure_keeps_an_honest_diagnostic_on_the_original_launch(self):
        # A real shell-startup failure before hgs --run can record an exit.
        self.script('bash', '#!/bin/sh\necho synthetic-login-startup-failure >&2\nexit 47\n')
        self.env['SHELL'] = str(self.bin / 'bash')
        launch = str(uuid.uuid4())
        self.hgs('codex', 'p', '--new', '-n', 'failed', '--launch-id', launch, '-d')
        def ended():
            # The listing omits this optional field until an exit is observed.
            rows = json.loads(self.hgs('ls', '--local', '--json', '--no-archives')).get('ended_launches', [])
            return next((row for row in rows if row['launch_id'] == launch), None)
        self.wait(ended)
        row = ended()
        self.assertEqual((row['code'], row['output']), (None, ''))
        self.assertEqual(row['diagnostic_code'], 'launch_supervisor_report_missing')
        evidence = json.loads((self.root / 'state/launches' / (launch + '.json')).read_text())
        self.assertEqual(evidence['unreported_diagnostic'], row['diagnostic_code'])
        self.assertFalse((self.root / 'argv.jsonl').exists(), 'no native agent started')

    @unittest.skipUnless(FISH, 'Set HGS_TEST_FISH for the actual Fish interpreter')
    def test_real_fish_new_and_resume_keep_the_same_conversation_in_a_cyrillic_folder(self):
        self.env['SHELL'] = str(Path(FISH).resolve())
        project = self.root / 'каталог'
        project.mkdir()
        (self.root / 'cfg/projects.local').write_text('p=' + str(project) + '\n')
        name = self.start('codex')
        conversation = self.binding(name)['conversation_id']
        self.hgs('pause', name)
        self.resume(name)
        self.assertEqual(self.binding(name)['conversation_id'], conversation)


@unittest.skipUnless(FISH and os.environ.get('HGS_CODEX_TEST_BIN'),
                     'Set HGS_TEST_FISH and HGS_CODEX_TEST_BIN for actual native Fish/Codex startup')
class NativeFishCodexStartup(test_codex_input_native.NativeCodexStartup):
    def setUp(self):
        super().setUp()
        self.env['SHELL'] = str(Path(FISH).resolve())
