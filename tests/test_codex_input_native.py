#!/usr/bin/env python3
"""Literal Codex message delivery against a keyless localhost Responses provider."""
import http.server
import io
import json
import os
from pathlib import Path
import subprocess
import threading
import time
import tomllib
import uuid
import unittest
from unittest import mock

import test_clear_context_native as native
import test_codex_async_questions_native as questions
import test_codex_update
import test_input
import test_pause


@unittest.skipUnless(os.environ.get('HGS_CODEX_TEST_BIN'), 'Set HGS_CODEX_TEST_BIN for isolated native Codex input')
class NativeCodexInput(unittest.TestCase):
    script = test_input.InputTransport.script
    write_record = test_input.InputTransport.write_record
    tmux = test_input.InputTransport.tmux
    tearDown = test_input.InputTransport.tearDown
    payload = test_input.InputTransport.payload
    send = test_input.InputTransport.send
    fail = native.NativeCodexClear.fail
    wait_for = test_codex_update.NativeCodexUpdate.wait_for
    native_features = 'mentions_v2=false\n'

    def setUp(self):
        class Provider(questions.Provider):
            def do_POST(self):
                raw = self.rfile.read(int(self.headers.get('Content-Length', '0')))
                self.server.raw_requests.append(json.loads(raw))
                self.rfile = io.BytesIO(raw)
                super().do_POST()
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
        self.server.raw_requests = []
        self.server.requests, self.server.release, self.server.questions = [], threading.Event(), []
        self.server.collaboration = False
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.addCleanup(self.server.release.set)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.provider_url = f'http://127.0.0.1:{self.server.server_port}/v1'
        native.NativeCodexClear.setUp(self)
        (self.root / 'literalreviewer.log').write_text('Synthetic file-completion target.\n')

    def screen(self):
        return self.tmux('capture-pane', '-p', '-t', self.pane)

    def test_id_pending_settings_then_message_makes_one_model_call_and_keeps_defaults(self):
        self.pending_message(named=True)

    def test_id_unnamed_pending_settings_then_message_counts_the_auxiliary_title_request(self):
        self.pending_message(named=False)

    def pending_message(self, *, named):
        import native_effort_id as identity
        if named:
            # An existing explicitly named conversation has no automatic title
            # request after the message. Use the native action, not a DB edit
            # or product config change. /rename can generate its own suggestion
            # before this operation; that request is measured separately.
            self.tmux('send-keys', '-t', self.pane, '-l', '/rename')
            self.wait_for(lambda: '/rename' in self.screen())
            self.tmux('send-keys', '-t', self.pane, 'Enter')
            self.wait_for(lambda: 'Type a name and press Enter' in self.screen())
            self.wait_for(lambda: self.server.raw_requests)
            self.tmux('send-keys', '-t', self.pane, '-l', 'Native pending send fixture')
            self.wait_for(lambda: 'Native pending send fixture' in self.screen())
            time.sleep(.2)  # Let Codex finish its native paste-burst handling.
            self.tmux('send-keys', '-t', self.pane, 'Enter')
            self.wait_for(lambda: 'Native pending send fixture' in self.screen()
                          and 'Name thread' not in self.screen() and 'Rename thread' not in self.screen())
            print('ZERUS_NATIVE_CODEX_RENAMED=' + json.dumps(dict(
                screen=self.screen(), cursor=self.tmux('display-message', '-p', '-t', self.pane,
                                                       '#{cursor_x}:#{cursor_y}').strip())), flush=True)
        models = [dict(slug='gpt-6.1-sol', display_name='GPT-6.1-Sol', default_reasoning_level='medium',
                       supported_reasoning_levels=[dict(effort=value) for value in
                                                  ['low', 'medium', 'high', 'xhigh', 'max', 'ultra']])]
        (self.native_home / 'models_cache.json').write_text(json.dumps(dict(models=models)))
        self.record.update(model='gpt-6.1-sol', phase='idle', activity='idle', active_tools={})
        self.write_record()
        defaults = self.native_home / 'config.toml'
        before, requests = defaults.read_bytes(), len(self.server.requests)
        record, path, _ = identity.moved_record(self, self.name, self.record, existing_machine=True)
        self.record, self.record_path = record, path
        attempts = identity.key_log(self)
        text = 'Continue after pending session reasoning @literalreviewer'
        identity.pending_then_message(self, self.name, record, path,
                                      'gpt-6.1-sol', 'high', text, attempts)
        expected = 1 if named else 2
        self.wait_for(lambda: len(self.server.requests) >= requests + expected)
        self.assertEqual(len(self.server.requests), requests + expected, self.server.raw_requests)
        offered = self.server.raw_requests[requests:]
        def title_request(body):
            schema = body.get('text', {}).get('format', {}).get('schema', {})
            return schema.get('required') == ['title'] and 'title' in schema.get('properties', {})
        titles = sum(title_request(body) for body in offered)
        self.assertEqual(titles, 0 if named else 1, offered)
        self.assertEqual(len(offered) - titles, 1, offered)
        print('ZERUS_NATIVE_CODEX_CALLS=' + json.dumps(dict(
            case=self.id(), named=named, before=requests, message_calls=len(offered)-titles,
            title_calls=titles, total_calls=len(offered))), flush=True)
        rows = map(json.loads, Path(record['transcript']).read_text().splitlines())
        messages = [row['payload']['message'] for row in rows if row.get('type') == 'event_msg'
                    and row.get('payload', {}).get('type') == 'user_message']
        self.assertEqual(messages.count(text), 1)
        self.assertEqual(defaults.read_bytes(), before)
        self.assertNotIn('Select Model', self.screen())

    def test_leading_absolute_path_is_literal_message_text(self):
        text = '/help/fixture/capture.png\nWhat is shown in this screenshot?'
        payload = self.payload(text=text)
        receipt = self.send(payload)
        self.assertEqual(receipt['submitted_text'], text)
        self.wait_for(lambda: any(request['texts'][-1:] == [text] for request in self.server.requests))
        self.wait_for(lambda: 'Fixture turn complete.' in self.screen())
        self.assertEqual(self.send(payload), receipt)
        rows = map(json.loads, Path(self.record['transcript']).read_text().splitlines())
        messages = [row['payload']['message'] for row in rows if row.get('type') == 'event_msg'
                    and row.get('payload', {}).get('type') == 'user_message']
        self.assertEqual(messages, ['Previous fixture request', text])

    def test_literal_at_mentions_reach_the_model_once_without_file_completion(self):
        # Positive control: the native completion menu really has a matching file.
        # Warm the exact query before hgs sends, so its fixed paste delay cannot
        # accidentally avoid the regression through a slow first file search.
        text = 'Send a fixture test to @literalreviewer'
        self.tmux('send-keys', '-t', self.pane, '-l', text)
        self.wait_for(lambda: 'literalreviewer.log' in self.screen())
        self.tmux('send-keys', '-t', self.pane, 'Enter')
        self.wait_for(lambda: 'Send a fixture test to literalreviewer.log' in self.screen())
        self.assertEqual(self.server.requests, [], 'Completion must not submit a native turn')
        self.tmux('send-keys', '-t', self.pane, 'C-a', 'C-k')
        self.wait_for(lambda: 'Send a fixture test to' not in self.screen())

        payload = self.payload(text=text)
        receipt = self.send(payload)
        self.assertEqual(receipt['submitted_text'], text)
        self.wait_for(lambda: self.server.requests)
        self.assertEqual(self.server.requests[0]['texts'][-1], text)
        self.wait_for(lambda: 'Fixture turn complete.' in self.screen())
        self.assertEqual(self.send(payload), receipt)
        # A later native request exposes any replayed/queued duplicate too. The
        # reused fixture has no progress hooks, so send this control in Terminal
        # rather than fabricate an acknowledgement to unlock another hgs send.
        control = 'After the literal mention.'
        self.tmux('set-buffer', '-b', 'literal-control', control)
        self.tmux('paste-buffer', '-p', '-r', '-d', '-b', 'literal-control', '-t', self.pane)
        self.wait_for(lambda: control in self.screen())
        self.tmux('send-keys', '-t', self.pane, 'Enter')
        self.wait_for(lambda: any(request['texts'][-1:] == [control] for request in self.server.requests))
        # Codex may also ask this provider for an automatic task title. Check
        # typed user-message events in this exact native conversation, so
        # auxiliary model requests and injected environment context cannot
        # impersonate an accepted or duplicate turn.
        rows = map(json.loads, Path(self.record['transcript']).read_text().splitlines())
        messages = [row['payload']['message'] for row in rows if row.get('type') == 'event_msg'
                    and row.get('payload', {}).get('type') == 'user_message']
        self.assertEqual(messages, ['Previous fixture request', text, control])


class NativeCodexMentionsV2Input(NativeCodexInput):
    native_features = 'mentions_v2=true\n'


@unittest.skipUnless(os.environ.get('HGS_CODEX_TEST_BIN'), 'Set HGS_CODEX_TEST_BIN for isolated native Codex startup')
class NativeCodexStartup(test_pause.Harness):
    """Production launch/hooks/first send, with a keyless localhost model only."""
    def setUp(self):
        super().setUp()
        (self.bin / 'codex').unlink()
        (self.bin / 'codex').symlink_to(Path(os.environ['HGS_CODEX_TEST_BIN']).resolve())
        for key in list(self.env):
            if key.startswith(('OPENAI_', 'CODEX_', 'ANTHROPIC_', 'CLAUDE_')):
                self.env.pop(key, None)
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), questions.Provider)
        self.server.requests, self.server.release, self.server.questions = [], threading.Event(), []
        self.server.collaboration = False
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.addCleanup(self.server.release.set)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.native_home = self.root / '.codex'
        self.native_home.mkdir()
        (self.native_home / 'config.toml').write_text(
            'model="gpt-6.1-sol"\nmodel_provider="fixture"\ncheck_for_update_on_startup=false\n'
            '[features]\nplugins=false\nhooks=true\n'
            '[model_providers.fixture]\nname="Fixture"\nwire_api="responses"\n'
            f'base_url="http://127.0.0.1:{self.server.server_port}/v1"\n'
            '[projects.' + json.dumps(str(self.project)) + ']\ntrust_level="trusted"\n')
        self.env['CODEX_HOME'] = str(self.native_home)
        for key in ('XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_STATE_HOME'):
            directory = self.root / key.lower()
            directory.mkdir()
            self.env[key] = str(directory)

    def screen(self):
        return self.t('capture-pane', '-p', '-t', '=' + self.name + ':')

    def until(self, label, predicate):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            result = predicate()
            if result:
                return result
            time.sleep(.1)
        self.fail(label + '\n' + self.screen())

    def turn_completed(self, text):
        # SessionStart or a previous Stop can still describe idle after Enter.
        # Require this exact provider request too; never resend to make it true.
        record = self.binding(self.name)
        return (any(row['texts'][-1:] == [text] for row in self.server.requests)
                and record.get('activity') == 'idle' and not record.get('expected_id'))

    def pause_settled_original(self):
        # SessionStart can publish idle before this turn's UserPromptSubmit.
        # Only these preflight refusals prove that pause sent no signal.
        name = self.name
        original = self.binding(name)
        fields = ('run_id', 'conversation_id', 'pid', 'process_start')
        identity = tuple(original.get(key) for key in fields)
        self.assertTrue(all(identity), 'The fixture must retain its original native binding')
        refusals = {
            f'hgs: {name}: activity changed while checking; retry pause',
            f'hgs: {name}: agent is busy or unconfirmed; wait for its answer before pausing',
        }
        deadline = time.monotonic() + 20  # Existing fixture command budget, not per retry.
        last = ''
        while True:
            current = self.binding(name)
            self.assertEqual(tuple(current.get(key) for key in fields), identity,
                             'The original changed while waiting to pause; never adopt another run')
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                self.fail('The original did not settle for pause within 20 s: ' + last)
            result = subprocess.run([str(test_pause.HGS), 'pause', name], env=self.env,
                                    capture_output=True, text=True, timeout=remaining)
            if result.returncode == 0:
                return result.stdout
            last = result.stdout + result.stderr
            if result.returncode != 1 or result.stdout or result.stderr.strip() not in refusals:
                self.fail(last)  # A possible signal, timeout or another refusal is never retried.
            time.sleep(min(.1, max(0, deadline - time.monotonic())))

    def launch(self, trust=False):
        self.name = 'codex/p/startup'
        args = [] if trust else ['--dangerously-bypass-hook-trust']
        self.hgs('codex', 'p', '--new', '-n', 'startup', '-d', '--',
                 '--dangerously-bypass-approvals-and-sandbox', *args)
        self.run_id = self.until('The original supervisor must publish its binding',
                                 lambda: self.binding(self.name).get('run_id'))

    def inspect(self):
        return json.loads(self.hgs('inspect', self.name))

    def deliver_first(self):
        self.until('Fresh native composer must accept its first message',
                   lambda: self.inspect().get('first_message_can_send'))
        text = 'Synthetic first native message.'
        request = dict(request_id=str(uuid.uuid4()), expected_run_id=self.run_id,
                       expected_conversation_id='', text=text)
        argv = [str(test_pause.HGS), 'send', self.name, '--json']
        first = subprocess.run(argv, input=json.dumps(request), env=self.env,
                               text=True, capture_output=True, timeout=20)
        self.assertEqual(first.returncode, 0, first.stderr + '\n' + self.screen())
        self.until('The real hooks must confirm the first conversation and completed turn',
                   lambda: self.binding(self.name).get('conversation_id')
                   and self.binding(self.name).get('activity') == 'idle')
        record = self.binding(self.name)
        self.assertEqual(record['run_id'], self.run_id)
        self.assertTrue(record.get('transcript'))
        events = self.inspect()['events']
        self.assertTrue({'SessionStart', 'UserPromptSubmit', 'Stop'} <= {event['type'] for event in events})
        replay = subprocess.run(argv, input=json.dumps(request), env=self.env,
                                text=True, capture_output=True, timeout=20)
        self.assertEqual(replay.returncode, 0, replay.stderr)
        self.assertEqual(json.loads(first.stdout), json.loads(replay.stdout))
        self.assertEqual(sum(row['texts'][-1:] == [text] for row in self.server.requests), 1)

    def test_fresh_native_launch_reaches_its_first_turn_through_the_real_hooks(self):
        self.launch()
        self.deliver_first()

    def test_explicit_native_hook_trust_then_first_turn(self):
        self.launch(trust=True)
        card = self.until('The native hook trust choice must stay explicit', lambda: next(
            (q for q in self.inspect().get('pending_questions', []) if q.get('source') == 'codex_hooks_trust'), None))
        self.assertFalse(self.inspect()['first_message_can_send'])
        self.assertEqual(self.server.requests, [])
        request = dict(request_id=str(uuid.uuid4()), expected_run_id=self.run_id,
                       expected_conversation_id='', question_id=card['question_id'],
                       expected_question_hash=card['question_hash'],
                       answers=[dict(question_id='hooks_trust', selected_option_ids=['trust'], text='')])
        result = subprocess.run([str(test_pause.HGS), 'answer', self.name, '--json'],
                                input=json.dumps(request), env=self.env, text=True,
                                capture_output=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr + '\n' + self.screen())
        self.assertEqual(json.loads(result.stdout)['status'], 'answered')
        self.deliver_first()

    def test_resumed_and_restored_hook_review_blocks_input_until_the_person_answers(self):
        def card():
            return next((q for q in self.inspect().get('pending_questions', [])
                         if q.get('source') == 'codex_hooks_trust'), None)

        def answer(question):
            record = self.binding(self.name)
            payload = dict(request_id=str(uuid.uuid4()), expected_run_id=record['run_id'],
                           expected_conversation_id=record.get('conversation_id') or '',
                           question_id=question['question_id'], expected_question_hash=question['question_hash'],
                           answers=[dict(question_id='hooks_trust', selected_option_ids=['trust'], text='')])
            result = subprocess.run([str(test_pause.HGS), 'answer', self.name, '--json'],
                                    input=json.dumps(payload), env=self.env, text=True,
                                    capture_output=True, timeout=20)
            self.assertEqual(result.returncode, 0, result.stderr + '\n' + self.screen())

        def message(text):
            record = self.binding(self.name)
            payload = dict(request_id=str(uuid.uuid4()), expected_run_id=record['run_id'],
                           expected_conversation_id=record['conversation_id'], text=text)
            return subprocess.run([str(test_pause.HGS), 'send', self.name, '--json'],
                                  input=json.dumps(payload), env=self.env, text=True,
                                  capture_output=True, timeout=20)

        self.launch(trust=True)
        initial = self.until('The initial native hook review must be explicit', card)
        answer(initial)
        self.deliver_first()
        conversation = self.binding(self.name)['conversation_id']
        hooks = self.native_home / 'hooks.json'
        original = hooks.read_bytes()
        stamp = hooks.stat().st_mtime_ns
        previous = self.binding(self.name)['run_id']
        self.pause_settled_original()
        self.hgs('resume', self.name, '-d')
        self.until('The unchanged resume must reach its native composer', lambda:
                   self.binding(self.name).get('run_id') != previous and '›' in self.screen()
                   and 'Hooks need review' not in self.screen())
        self.assertEqual((hooks.read_bytes(), hooks.stat().st_mtime_ns), (original, stamp))
        self.assertIsNone(card(), 'An unchanged registration must not request trust again')
        unchanged = 'Synthetic unchanged-hook resume.'
        result = message(unchanged)
        self.assertEqual(result.returncode, 0, result.stderr + '\n' + self.screen())
        self.until('The resumed turn must finish', lambda: self.turn_completed(unchanged))

        for mode in ('resume', 'restore'):
            with self.subTest(mode=mode):
                previous = self.binding(self.name)['run_id']
                self.pause_settled_original()
                # Change one of our fixture's existing definitions, not its
                # executable or trust state. Codex must request new consent.
                document = json.loads(hooks.read_text())
                document['hooks']['SessionStart'][0]['hooks'][0]['timeout'] += 1
                hooks.write_text(json.dumps(document, indent=2))
                options = []
                if mode == 'restore':
                    self.hgs('archive', self.name)
                    archived = next(json.loads(path.read_text())
                                    for path in (self.root / 'state/archives').glob('*.json')
                                    if json.loads(path.read_text()).get('name') == self.name)
                    options = ['--archive', archived['archive_id']]
                self.hgs('resume', self.name, *options, '-d')
                self.until('The new original run must be bound', lambda:
                           self.binding(self.name).get('run_id') not in (None, previous))
                review = self.until('The saved conversation must expose its real hook review', card)
                self.assertEqual(review['conversation_id'], conversation)
                self.assertEqual(review['run_id'], self.binding(self.name)['run_id'])
                self.assertNotEqual(review['question_hash'], initial['question_hash'])
                result = message('MUST NOT ENTER THE HOOK TRUST DIALOG')
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('hook trust review', result.stderr)
                self.assertIn('› 1. Review hooks', self.screen())
                self.assertFalse(any('MUST NOT ENTER' in text
                                     for request in self.server.requests for text in request['texts']))
                answer(review)  # This fixture's explicit decision; never automatic trust.
                text = 'Synthetic after ' + mode + ' hook review.'
                result = message(text)
                self.assertEqual(result.returncode, 0, result.stderr + '\n' + self.screen())
                self.until('The original resumed conversation must complete its turn',
                           lambda: self.turn_completed(text))
                self.assertEqual(self.binding(self.name)['conversation_id'], conversation)
                self.assertEqual(sum(row['texts'][-1:] == [text] for row in self.server.requests), 1)

    def test_hook_review_pause_rechecks_each_original_after_preflight_refusals(self):
        real_run = subprocess.run
        refused, paused = {}, []
        errors = ('agent is busy or unconfirmed; wait for its answer before pausing',
                  'activity changed while checking; retry pause')

        def run(argv, *args, **kwargs):
            if argv == [str(test_pause.HGS), 'pause', getattr(self, 'name', None)]:
                run_id = self.binding(self.name)['run_id']
                count = refused.get(run_id, 0)
                if count < len(errors):
                    refused[run_id] = count + 1
                    # A fixture preflight refusal: no native pause call occurred.
                    return subprocess.CompletedProcess(argv, 1, '', f'hgs: {self.name}: {errors[count]}\n')
                result = real_run(argv, *args, **kwargs)
                if result.returncode == 0:
                    paused.append(run_id)
                return result
            return real_run(argv, *args, **kwargs)

        with mock.patch.object(subprocess, 'run', side_effect=run):
            # Delegate every trust, send, resume, archive and scope assertion to
            # the original native contract. A missing retry at either call site fails.
            self.test_resumed_and_restored_hook_review_blocks_input_until_the_person_answers()
        self.assertEqual(len(refused), 3)
        self.assertEqual(list(refused.values()), [2, 2, 2])
        self.assertEqual(paused, list(refused))

    def untrusted(self, git=False):
        self.server.collaboration = False
        # A project config makes the ordinary folder a project in 0.167 too:
        # genuinely projectless directories no longer require folder consent.
        config = self.native_home / 'config.toml'
        config.write_text(config.read_text().split('[projects.', 1)[0])
        target = self.project
        (target / '.codex').mkdir()
        (target / '.codex/config.toml').write_text('# Synthetic project, no hooks or commands.\n')
        if git:
            subprocess.run(['git', 'init', '-q', str(target)], check=True, env=self.env)
            self.project = target / 'subdir'
            self.project.mkdir()
            (self.root / 'cfg/projects.local').write_text('p=' + str(self.project) + '\n')
        self.launch()
        card = self.until('The real folder trust choice must be answerable', lambda: next(
            (q for q in self.inspect().get('pending_questions', [])
             if q.get('source') == 'codex_folder_trust' and q.get('can_answer')), None))
        self.assertFalse(self.inspect()['first_message_can_send'])
        self.assertEqual(self.server.requests, [])
        self.assertNotIn('projects', tomllib.loads(config.read_text()))
        return card, target

    def folder_answer(self, card, choice):
        request = dict(request_id=str(uuid.uuid4()), expected_run_id=self.run_id,
                       expected_conversation_id='', question_id=card['question_id'],
                       expected_question_hash=card['question_hash'],
                       answers=[dict(question_id='trust', selected_option_ids=[f'trust_{choice}'], text='')])
        args = ['answer', self.name, '--json']
        # Harness.hgs intentionally accepts only argv; keep the real JSON
        # submission and its replay on subprocess stdin.
        def submit():
            result = subprocess.run([str(test_pause.HGS), *args], input=json.dumps(request), env=self.env,
                                    text=True, capture_output=True, timeout=20)
            # Quit can already have removed the pane; do not capture it while
            # evaluating the assertion's diagnostic on a successful answer.
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            return json.loads(result.stdout)
        receipt = submit()
        self.assertEqual(receipt['status'], 'answered')
        self.assertEqual(submit(), receipt)
        return receipt

    def test_explicit_native_folder_trust_then_first_turn(self):
        card, target = self.untrusted()
        self.assertEqual(card['questions'][0]['question'], 'Trust this folder?')
        self.folder_answer(card, 0)
        self.assertEqual(tomllib.loads((self.native_home / 'config.toml').read_text())['projects'][str(target)]['trust_level'], 'trusted')
        self.deliver_first()

    def test_explicit_native_git_subdirectory_trust_names_the_root(self):
        card, target = self.untrusted(git=True)
        self.assertEqual(card['questions'][0]['question'], 'Trust this Git repository?')
        self.assertIn(str(target), card['questions'][0]['options'][0]['description'])
        self.folder_answer(card, 0)
        projects = tomllib.loads((self.native_home / 'config.toml').read_text())['projects']
        self.assertEqual(projects[str(target)]['trust_level'], 'trusted')
        self.assertNotIn(str(self.project), projects)
        self.deliver_first()

    def test_explicit_native_folder_cancel_does_not_trust(self):
        card, _ = self.untrusted()
        cancel = card['questions'][0]['options'][1]['label']
        self.assertIn(cancel, ('Quit', 'Back to Agent Command Center'))
        self.folder_answer(card, 1)
        if cancel == 'Back to Agent Command Center':
            self.until('Back must show the live command center', lambda: 'Agent command center' in self.screen())
            record = self.binding(self.name)
            self.assertFalse(record.get('conversation_id'))
            os.kill(record['pid'], 0)
        self.assertNotIn('projects', tomllib.loads((self.native_home / 'config.toml').read_text()))
        self.assertEqual(self.server.requests, [])
