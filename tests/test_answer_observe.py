#!/usr/bin/env python3
"""The deferred answer actor's effect-free native question read.

`hgs answer <session> --observe --json` must give, for each source the
deferred answer kind takes, exactly the consent identity `hgs inspect` shows
for that question, and must leave the state tree byte-identical: no lock
file, cache, SQLite -wal/-shm, directory or record change, and no terminal
resize. Every case runs on a private tmux socket with a fake agent.
"""
import hashlib
import json
import os
import signal
import stat
import subprocess
import time
import unittest
import uuid
from pathlib import Path

import test_claude_permission
import test_claude_trust
import test_codex_hooks_trust
import test_codex_rate_limit
import test_codex_trust
import test_codex_update
import receipt_locator
import test_input as fixtures
import test_kimi_cache_hint
import test_kimi_trust
import test_questions

HGS = fixtures.HGS
FIXTURES = Path(__file__).parent / 'fixtures'
# `questions::CONSENT_FIELDS` in client/core.
CONSENT_FIELDS = ('question_id', 'question_hash', 'run_id', 'conversation_id', 'questions', 'source',
                  'tool', 'tool_name', 'tool_call_id', 'native_question_id', 'agent_id', 'answer_transport',
                  'approval', 'approval_choices', 'trust_request', 'optional')
# `deferred_answer::SOURCES` in client/core.
SOURCES = {'claude_permission_mode', 'claude_resume', 'claude_usage_limit', 'claude_folder_trust',
           'codex_hooks_trust', 'codex_update', 'codex_rate_limit', 'codex_folder_trust', 'kimi_folder_trust',
           'kimi_cache_hint', 'kimi_plan_review', 'kimi_tool_approval', 'kimi_wire'}


def identity(card):
    return {field: card[field] for field in CONSENT_FIELDS if field in card}


def tree(root):
    """Every entry under `root`, the whole fixture root (HOME, configuration,
    tmux socket, state, agent homes and wire files): its kind, mode, size,
    content digest and modification time, so that any write, lock file or
    directory shows."""
    entries = {}
    for directory, folders, files in os.walk(root):
        for name in folders + files:
            path = Path(directory) / name
            info = os.lstat(path)
            digest = hashlib.sha256(path.read_bytes()).hexdigest() if stat.S_ISREG(info.st_mode) else None
            entries[str(path.relative_to(root))] = (stat.S_IFMT(info.st_mode), info.st_mode & 0o7777,
                                                    info.st_size, digest, info.st_mtime_ns)
    return entries


def observe(case, question_id, **update):
    payload = dict(question_id=question_id, expected_run_id=case.run_id,
                   expected_conversation_id=case.record.get('conversation_id') or '')
    payload.update(update)
    result = subprocess.run([str(HGS), 'answer', case.name, '--observe', '--json'], input=json.dumps(payload),
                            text=True, capture_output=True, env=case.env, timeout=30)
    case.assertEqual(result.returncode, 0, result.stderr)
    return json.loads(result.stdout)


def window_height(case):
    return case.tmux('display-message', '-p', '-t', case.pane, '#{window_height}').strip()


def check(case, source):
    """`hgs inspect` (which may fit a panel) first, as the client reads it;
    then the observation, which must agree and change nothing."""
    cards = [card for card in case.inspect()['pending_questions'] if card.get('source') == source]
    case.assertEqual(len(cards), 1, f'{source}: {cards}')
    card = cards[0]
    # The inspection may start the receipt locator's background worker, which
    # writes after it returns; only the observation itself is measured.
    receipt_locator.settle(HGS, case.env)
    before, height = tree(case.root), window_height(case)
    seen = observe(case, card['question_id'])
    case.assertEqual(tree(case.root), before, f'{source}: the observation changed the fixture tree')
    case.assertEqual(window_height(case), height, f'{source}: the observation resized the terminal')
    case.assertEqual(seen, dict(found=True, consent_identity=identity(card)), source)
    # Another question, run or conversation is never found.
    absent = dict(found=False, consent_identity=None)
    case.assertEqual(observe(case, 'question-elsewhere'), absent)
    case.assertEqual(observe(case, card['question_id'], expected_run_id=str(uuid.uuid4())), absent)
    case.assertEqual(observe(case, card['question_id'], expected_conversation_id='conversation-elsewhere'), absent)
    case.assertEqual(tree(case.root), before)


def observing(name, base, source, prepare=None):
    """`base`'s fixture with one test: its own tests are not run again."""
    attributes = {name: None for name in dir(base) if name.startswith('test')}

    def test_observation_matches_inspection_and_writes_nothing(self):
        if prepare:
            prepare(self)
        check(self, source)

    attributes['test_observation_matches_inspection_and_writes_nothing'] = \
        test_observation_matches_inspection_and_writes_nothing
    return type(name, (base,), attributes)


ClaudeFolderTrust = observing('ClaudeFolderTrust', test_claude_trust.FolderTrust, 'claude_folder_trust')
ClaudePermissionMode = observing('ClaudePermissionMode', test_claude_permission.PermissionMode, 'claude_permission_mode')
CodexHooksTrust = observing('CodexHooksTrust', test_codex_hooks_trust.HooksTrust, 'codex_hooks_trust')
CodexUpdate = observing('CodexUpdate', test_codex_update.CodexUpdate, 'codex_update')
CodexRateLimit = observing('CodexRateLimit', test_codex_rate_limit.CodexRateLimit, 'codex_rate_limit')
CodexFolderTrust = observing('CodexFolderTrust', test_codex_trust.FolderTrust, 'codex_folder_trust')
CodexFolderTrustOverview = observing('CodexFolderTrustOverview', test_codex_trust.FolderTrust,
                                    'codex_folder_trust', lambda case: case.overview())
KimiFolderTrust = observing('KimiFolderTrust', test_kimi_trust.FolderTrust, 'kimi_folder_trust')
KimiCacheHint = observing('KimiCacheHint', test_kimi_cache_hint.CacheHint, 'kimi_cache_hint')
KimiWire = observing('KimiWire', test_questions.Questions, 'kimi_wire')
KimiPlanReview = observing('KimiPlanReview', test_questions.PlanReview, 'kimi_plan_review')


# A fake Claude that shows the panel in the fixture root's `panel` file.
FAKE_PANEL = fixtures.SIGNAL_SAFE_READ + r'''
import os,pathlib,signal,sys,termios,tty
root=pathlib.Path(os.environ['INPUT_FIXTURE']);fd=sys.stdin.fileno();original=termios.tcgetattr(fd);tty.setraw(fd)
os.write(1,b'\x1b[?2004h')
def render(*args):
 screen=(root/'panel').read_text().strip() if (root/'panel').exists() else ''
 os.write(1,b'\x1b[2J\x1b[H'+screen.replace('\n','\r\n').encode())
signal.signal(signal.SIGUSR1,render);render();(root/'agent-pid').write_text(str(os.getpid()))
try:
 while True:
  value=read_input(fd)
  if not value:break
finally:termios.tcsetattr(fd,termios.TCSANOW,original)
'''


class ClaudePanels(unittest.TestCase):
    """Claude's resume and usage-limit panels, which no other suite renders."""
    script = fixtures.InputTransport.script
    tmux = fixtures.InputTransport.tmux
    wait_for = fixtures.InputTransport.wait_for
    write_record = fixtures.InputTransport.write_record
    tearDown = fixtures.InputTransport.tearDown
    inspect = test_questions.Questions.inspect

    def setUp(self):
        original = fixtures.FAKE_AGENT
        try:
            fixtures.FAKE_AGENT = FAKE_PANEL
            fixtures.InputTransport.setUp(self)
        finally:
            fixtures.FAKE_AGENT = original
        self.record.update(agent='claude', run_identity_version=1, supervisor={},
                           launch_dir='/work/example', phase='idle', activity='idle', active_tools={})
        self.write_record()

    def show(self, fixture):
        text = (FIXTURES / fixture).read_text()
        (self.root / 'panel').write_text(text)
        os.kill(self.record['pid'], signal.SIGUSR1)
        first = next(line.strip() for line in text.splitlines() if line.strip())
        self.wait_for(lambda: first in self.tmux('capture-pane', '-p', '-t', self.pane))

    def test_resume_observation_matches_inspection_and_writes_nothing(self):
        self.show('claude-resume.txt')
        check(self, 'claude_resume')

    def test_usage_limit_observation_matches_inspection_and_writes_nothing(self):
        self.show('claude-usage-limit.txt')
        check(self, 'claude_usage_limit')


def tool_approval(case):
    """A Kimi tool approval on the wire, beside the plan review fixture's: a
    Shell tool, which Zerus answers only in Terminal, so both reads leave it
    without a transport."""
    call = 'tool_shell'
    event_id = 'approval_' + str(uuid.uuid4())
    case.record['active_tools'][call] = {'name': 'Shell'}
    case.write_record()
    event = dict(type='interaction.request', id=event_id, kind='approval', agentId='main', toolCallId=call,
                 request=dict(id=event_id, toolCallId=call, toolName='Shell', action='Run a command',
                              display=dict(kind='shell', command='ls -la')),
                 time=int(time.time() * 1000))
    with case.wire.open('a') as wire:
        wire.write(json.dumps(event) + '\n')


KimiOtherToolApproval = observing('KimiOtherToolApproval', test_questions.PlanReview, 'kimi_tool_approval',
                                  tool_approval)

# A fake Kimi that draws its Bash command approval for the fixture root's
# `command`, as `kimi_approval` reads it.
FAKE_APPROVAL = fixtures.SIGNAL_SAFE_READ + r'''
import os,pathlib,signal,sys,termios,tty
root=pathlib.Path(os.environ['INPUT_FIXTURE']);fd=sys.stdin.fileno();original=termios.tcgetattr(fd);tty.setraw(fd)
os.write(1,b'\x1b[?2004h')
def render(*args):
 if not (root/'command').exists():return
 rule=' '+'\u2500'*75;labels=['Approve once','Approve for this session','Reject','Reject with feedback']
 lines=[rule,'   \u25b6 Run this command?','','   $ '+(root/'command').read_text(),'','   \u25b6 1. '+labels[0]]
 lines+=['     '+str(index+1)+'. '+label for index,label in enumerate(labels) if index]
 lines+=['','   \u2191/\u2193 select \u00b7 1/2/3/4 choose \u00b7 \u21b5 confirm',rule]
 os.write(1,b'\x1b[2J\x1b[H'+'\r\n'.join(lines).encode())
signal.signal(signal.SIGUSR1,render);(root/'agent-pid').write_text(str(os.getpid()))
try:
 while True:
  value=read_input(fd)
  if not value:break
finally:termios.tcsetattr(fd,termios.TCSANOW,original)
'''


class KimiToolApproval(unittest.TestCase):
    """A Kimi Bash command approval the TUI shows: answerable, so its
    transport is part of the identity both reads must agree on."""
    script = fixtures.InputTransport.script
    tmux = fixtures.InputTransport.tmux
    wait_for = fixtures.InputTransport.wait_for
    write_record = fixtures.InputTransport.write_record
    tearDown = fixtures.InputTransport.tearDown
    inspect = test_questions.Questions.inspect

    def setUp(self):
        original = fixtures.FAKE_AGENT
        try:
            fixtures.FAKE_AGENT = FAKE_APPROVAL
            fixtures.InputTransport.setUp(self)
        finally:
            fixtures.FAKE_AGENT = original
        old_path = self.record_path
        self.tmux('rename-session', '-t', '=' + self.name, 'kimi/approval-test')
        self.name = 'kimi/approval-test'
        self.conversation_id = 'session_' + str(uuid.uuid4())
        self.record_path = self.state / (hashlib.sha256(self.name.encode()).hexdigest() + '.json')
        old_path.unlink()
        self.record.update(name=self.name, agent='kimi', conversation_id=self.conversation_id,
                           agent_home=str(self.root / '.kimi-code'), activity='busy', phase='approval',
                           active_tools={'tool_bash': {'name': 'Bash'}})
        self.write_record()
        wire = self.root / '.kimi-code/sessions/wd_fixture' / self.conversation_id / 'agents/main/wire.jsonl'
        wire.parent.mkdir(parents=True)
        event_id = 'approval_' + str(uuid.uuid4())
        event = dict(type='interaction.request', id=event_id, kind='approval', agentId='main', toolCallId='tool_bash',
                     request=dict(id=event_id, toolName='Bash', toolCallId='tool_bash', action='Run a command',
                                  display=dict(kind='command', command='ls -la')),
                     time=int(time.time() * 1000))
        wire.write_text(json.dumps(event) + '\n')
        (self.root / 'command').write_text('ls -la')
        os.kill(self.record['pid'], signal.SIGUSR1)
        self.wait_for(lambda: 'Run this command?' in self.tmux('capture-pane', '-p', '-t', self.pane))

    def test_observation_matches_inspection_and_writes_nothing(self):
        cards = [card for card in self.inspect()['pending_questions'] if card['source'] == 'kimi_tool_approval']
        self.assertEqual(len(cards), 1, cards)
        self.assertEqual((cards[0]['can_answer'], cards[0]['answer_transport']), (True, 'kimi_tui'),
                         cards[0]['answer_unavailable_reason'])
        check(self, 'kimi_tool_approval')


class DirectSource(test_questions.Questions):
    """A question of a source the deferred kind leaves on the direct path,
    or of a session that cannot answer: `hgs inspect` may show it, the
    observation never finds it."""

    def test_a_direct_source_is_never_observed(self):
        self.wire.unlink()
        self.hook(dict(hook_event_name='PreToolUse', tool_name='AskUserQuestion', tool_call_id='tool_test',
                       tool_input=dict(questions=self.items)))
        card = self.inspect()['pending_questions'][0]
        self.assertEqual(card['source'], 'hook')
        before = tree(self.root)
        self.assertEqual(observe(self, card['question_id']), dict(found=False, consent_identity=None))
        self.assertEqual(tree(self.root), before)


    def test_a_session_that_cannot_answer_is_never_observed(self):
        # A start that has not confirmed its conversation answers only its
        # own startup questions; a failed, paused or exited agent answers
        # none.
        question_id = self.card['question_id']
        self.assertEqual(observe(self, question_id)['found'], True)
        for update in (dict(expected_id='session_expected'), dict(error='The agent exited.'),
                       dict(pausing=dict(pid=self.record['pid'], start=self.record['process_start'])),
                       dict(process_start='0'), dict(pane='%999')):
            original = dict(self.record)
            self.record.update(update)
            self.write_record()
            try:
                before = tree(self.root)
                self.assertEqual(observe(self, question_id), dict(found=False, consent_identity=None), update)
                self.assertEqual(tree(self.root), before)
            finally:
                self.record.clear()
                self.record.update(original)
                self.write_record()


for _name in dir(test_questions.Questions):
    if _name.startswith('test'):
        setattr(DirectSource, _name, None)


class KimiClippedPanel(unittest.TestCase):
    """A Kimi trust panel clipped by a short window: `hgs inspect` would fit
    it, the observation never does. It finds no card and resizes nothing,
    and once the panel is fitted it agrees with the inspection."""
    script = fixtures.InputTransport.script
    tmux = fixtures.InputTransport.tmux
    wait_for = fixtures.InputTransport.wait_for
    write_record = fixtures.InputTransport.write_record
    tearDown = fixtures.InputTransport.tearDown
    inspect = test_questions.Questions.inspect

    def setUp(self):
        original = fixtures.FAKE_AGENT
        try:
            fixtures.FAKE_AGENT = test_kimi_trust.FAKE
            fixtures.InputTransport.setUp(self)
        finally:
            fixtures.FAKE_AGENT = original
        self.record.update(agent='kimi', run_identity_version=1, supervisor={},
                           conversation_id=None, launch_dir='/work/example',
                           phase='unknown', activity='unknown', last_event_at=0)
        self.write_record()

    def test_a_clipped_panel_is_never_fitted_or_found(self):
        self.tmux('resize-window', '-t', '=' + self.name + ':', '-y', '10')
        os.kill(self.record['pid'], signal.SIGUSR1)
        self.wait_for(lambda: window_height(self) == '10')
        before, height = tree(self.root), window_height(self)
        seen = observe(self, 'kimi-trust:' + 'a' * 64)
        self.assertEqual(seen, dict(found=False, consent_identity=None))
        self.assertEqual(window_height(self), height, 'the observation fitted the panel')
        self.assertEqual(tree(self.root), before)
        # A complete panel again, as a fit leaves it: both agree, and
        # observing still writes nothing.
        self.tmux('resize-window', '-t', '=' + self.name + ':', '-y', '30')
        os.kill(self.record['pid'], signal.SIGUSR1)
        self.wait_for(lambda: window_height(self) == '30')
        check(self, 'kimi_folder_trust')


if __name__ == '__main__':
    unittest.main()
