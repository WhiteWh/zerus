#!/usr/bin/env python3
"""Agent Teams milestone 1 against real agents (package 15).

Each class brings up one team of one agent kind (a lead and one member, launched by
a private `hgs desktop serve --background` from the team store) in a private root,
answers the members' startup cards as the person, and then walks the places where
the Teams service touches a real session, in order:

1. launch and identity: the session binds by its launch ID, its record carries the
   store's ownership nonce and worktree, the native process is the pinned image under
   the production supervisor, the role's model and effort reached its argv, and the
   member's parent link names the lead;
2. delivery and the member's shell: `hgs task team-deliver` puts the task prompt into
   the member exactly once; the member's own tool shell acknowledges with the pinned
   begin-attempt, passes the caller proof (`hgs team current` names it), is refused a
   lead's command, and reports with the pinned report-attempt;
3. the lead's doorbell: one `hgs send` naming the inbox, with none of the item's text;
   the lead's own shell reads the inbox as the lead;
4. the cancel notice: the lead cancels a delivered task from its shell and the member
   receives the existing cancel notice once;
5. stop and terminate: a person's team stop ends no agent; a person's Terminate ends
   the member's process and archives its run.

Further classes turn one of the team's delegations
on: folder trust by the standing confirmation (Codex selected; Claude Code waits for
the owner's decision zerus-63rru.66 and for zerus-63rru.73) and the resume of an
exited member (Claude Code and Codex selected).
The model is a keyless loopback fixture; no account and no model quota are used.
Run through `scripts/ci/agent-compatibility.py --contract teams`, or directly with
HGS_TEST_BIN and HGS_CLAUDE_TEST_BIN or HGS_CODEX_TEST_BIN, outside any agent
session: the Teams caller proof refuses a person command below a live agent.
The Kimi class runs only in the hosted Kimi workflow.
"""
from contextlib import closing, contextmanager, nullcontext
import json
import os
from pathlib import Path
import pwd
import re
import shutil
import shlex
import socket
import subprocess
import tempfile
import time
import unittest
from unittest import mock
import uuid

import teams_native_fixture as fixture


# Cases written against real agents that wait for a product fix before the contract
# selects them: TeamsContract methods named waiting_*, and whole classes by name.
# TeamsWaitingList keeps this list, the selection, the workflow header, the row's
# coverage and the docs naming the same fixes.
WAITING = {
    'waiting_6_a_member_shell_in_a_private_pid_namespace': 'zerus-63rru.76 (the caller proof cannot see the agent\'s '
                                                           'recorded process from a private PID namespace; a named '
                                                           'refusal comes with the other stream\'s next sync to main)',
    # Whole classes the teams contract does not select yet.
    'NativeTeamsTrustClaude': 'zerus-63rru.66 (the owner decides whether the standing confirmation may answer a '
                              'card whose recorded trust is the main checkout; runs tc20, tc21, tp1 to tp3) and '
                              'zerus-63rru.73 (test_2: an ordinary question is taken as a trust candidate)',
    'NativeTeamsQuestionAfterTrustClaude': 'zerus-63rru.73 (an ordinary question after no startup card opens one '
                                           'notice card today)',
}


# Kimi Code's selected steps that no hosted run has proven yet (the review's point H).
# While any are listed, the workflow header and the docs say so; once a hosted run proves
# them, empty this and both sentences must go (TeamsWaitingList checks both ways).
KIMI_UNPROVEN = ('test_2_delivery_and_member_shell', 'test_3_lead_doorbell', 'test_4_cancel_notice')


class ClassFixture:
    """One team per class, brought up once; the ordered steps share it, and a failed
    step fails the later ones with its name."""
    agent = None
    variable = None
    fx = None
    failed = None
    delegations = None
    service_answers = ()
    after_refusal = None
    count = 1
    pinned = None
    pretrust = ()

    @classmethod
    def setUpClass(cls):
        cls.fx = fixture.TeamFixture(cls.agent, os.environ[cls.variable], cls.__name__,
                                     delegations=cls.delegations, service_answers=cls.service_answers,
                                     after_refusal=cls.after_refusal, count=cls.count, pinned=cls.pinned,
                                     pretrust=cls.pretrust)
        try:
            cls.fx.bring_up()
        except BaseException:
            problems = cls.fx.teardown()
            cls.fx.evidence.write('bring-up-teardown.json', problems)
            raise

    @classmethod
    def tearDownClass(cls):
        problems = cls.fx.teardown()
        if problems:
            raise AssertionError('Teams fixture teardown: ' + '; '.join(problems))

    @property
    def root(self):
        """The private root, declared for the hosted Kimi runner's custody records."""
        return self.fx.root if self.fx else None

    def setUp(self):
        if type(self).failed:
            self.fail(f'an earlier step failed: {type(self).failed}')

    def run(self, result=None):
        before = len(result.failures) + len(result.errors) if result is not None else 0
        outcome = super().run(result)
        if result is not None and len(result.failures) + len(result.errors) > before and not type(self).failed:
            type(self).failed = self._testMethodName
        return outcome


def journal_is(case, expected, label):
    """The whole audit journal settles to exactly `expected` (journal_summary) within a
    turn's bound; the last summary read is kept in the evidence either way."""
    fx = case.fx
    last = []

    def settled():
        last[:] = [fx.journal_summary()]
        return last[0] == expected or None
    try:
        fx.wait(label, settled, fixture.BOUND['turn'], 2)
    finally:
        fx.evidence.write('journal-' + re.sub(r'\W+', '-', label).strip('-')[:60] + '.json',
                          dict(expected=expected, last=last[0] if last else None))


# Hand-outs took 0.1 to 0.9 s from the task's observation in tc23; a quiet window ten
# times longer shows that nobody took the task.
QUIET = 15


def ordinary_question_stays_the_persons(case, askers):
    """zerus-63rru.73: only the member agent's own folder-trust prompt is a candidate of
    the standing confirmation. An ordinary question plans nothing (no card, no audit row),
    stays pending for the person, and its member is not free while it is open. The
    positive control: once the person answers one member's question, that member takes
    the task nobody took while every question was open."""
    fx = case.fx
    journal = fx.journal_summary()
    seen = {c.get('seq') for c in fx.cards()}
    for member in askers:
        fx.send(member, 'Ask the fixture questions now.')
    cards = {member: fx.wait(f'{member} asked a question mid-conversation', lambda m=member: next(iter(fx.pending(m)), None),
                             fixture.BOUND['turn'], 2) for member in askers}
    fx.evidence.write('ordinary-questions.json', cards)
    _, task_uuid = fx.add_task('Fixture task while questions are open', 'Fixture task: taken once a question is answered.')
    node = fx.wait('task node in the queue', lambda: fx.node_of(task_uuid), fixture.BOUND['handout'], 2)
    deadline = time.monotonic() + QUIET
    while time.monotonic() < deadline:
        case.assertEqual(fx.handouts(node['id']), [], 'no member with an open question is handed a task')
        time.sleep(1)
    first = askers[0]
    fx.answer_question(first, cards[first])
    taken = fx.wait(f'{first} takes the task once its question is answered', lambda: fx.handouts(node['id']) or None,
                    fixture.BOUND['handout'], 1)
    case.assertEqual(taken, [first])
    for member in askers[1:]:
        case.assertIn(cards[member]['question_id'], [q.get('question_id') for q in fx.pending(member)],
                      f"{member}'s question is still the person's")
    case.assertEqual([c for c in fx.cards() if c.get('seq') not in seen], [], 'no Teams card for an ordinary question')
    journal_is(case, journal, 'the journal is unchanged by the ordinary questions')


class TeamsContract(ClassFixture):
    """Delegations off: the person answers every startup card."""

    # 1 -----------------------------------------------------------------------
    def test_1_launch_and_identity(self):
        fx = self.fx
        members = fx.members()
        for member_id in ('lead', 'm1'):
            stored = members[member_id]
            view = fx.inspect(member_id)
            launch = fx.sessions[member_id]['launch']
            self.assertEqual(stored['session']['session_id'], 'launch_id:' + launch)
            self.assertEqual(view.get('launch_id'), launch)
            self.assertTrue(view.get('tracked'))
            self.assertEqual(view.get('agent'), self.agent)
            self.assertEqual(os.path.realpath(view.get('cwd')), os.path.realpath(stored['worktree']))
            # The launch picks among the signed-in fixture accounts and spreads seats over
            # them (the always-listed native account is a configured fixture account too).
            self.assertIn(stored.get('account'), fx.fixture_accounts)
            record = fx.record(member_id)
            self.assertEqual((record.get('worktree_owner') or {}).get('id'), stored['ownership_nonce'])
            pid = record['pid']
            parent = int(open(f'/proc/{pid}/stat').read().rsplit(')', 1)[1].split()[1])
            self.assertEqual(parent, record['supervisor']['pid'], 'the native agent runs under the production supervisor')
            self.assertEqual(Path(f'/proc/{pid}/exe').resolve(), fx.native, 'the native process is the pinned image')
            argv = open(f'/proc/{pid}/cmdline', 'rb').read().split(b'\0')
            if member_id == 'm1' and self.agent == 'claude':
                self.assertIn(b'--effort', argv)
                self.assertIn(b'high', argv)
                self.assertIn(b'claude-sonnet-4-6', argv)
            if member_id == 'm1' and self.agent == 'codex':
                self.assertIn(b'model_reasoning_effort="high"', argv)
                self.assertIn(b'gpt-6.1-sol', argv)
            if self.agent == 'codex':
                # This class leaves trust to the person, so it retains the
                # native panel before answering. Standing trust can answer
                # before the fixture's first inspection in its own class.
                seen = [v for k, v in fx.cards_seen.items() if k[0] == member_id and v['source'] == 'codex_folder_trust']
                self.assertEqual(len(seen), 1, 'the person saw one native folder-trust panel')
                card = seen[0]['card']
                self.assertEqual(card['questions'][0]['question'], 'Trust this Git repository?')
                self.assertTrue(card.get('can_answer'))
                self.assertEqual(card['questions'][0]['body'].split('\n\n', 1)[0].replace('\n', ''), stored['worktree'])
                self.assertEqual(seen[0]['size'], '80x24')
                key = next(k for k, value in fx.cards_seen.items() if value is seen[0])
                variant = fixture.codex_panel_variant(fx.panes[key], stored['worktree'])
                fits = len(stored['worktree']) <= fixture.CODEX_PANEL_COLUMNS
                fx.evidence.write(f'codex-panel-{member_id}.json', dict(
                    variant=variant, folder_columns=len(stored['worktree']), row_columns=fixture.CODEX_PANEL_COLUMNS,
                    hosted=fixture.hosted_runner()))
                self.assertEqual(variant, 'one-row' if fits else 'wrapped')
                self.assertEqual(variant, 'one-row' if fixture.hosted_runner() else 'wrapped')
        # The service writes the link on a pass after the lead has a conversation (it
        # records identity_pending until then); it never replaces one. Codex starts its
        # conversation at the first message, so a fresh Codex team has no link until the
        # person's first message to each session has started one.
        for member_id in ('lead', 'm1'):
            if not fx.inspect(member_id).get('conversation_id'):
                fx.send(member_id, 'Fixture first message.')
                fx.wait(f'{member_id} conversation started by its first message',
                        lambda m=member_id: fx.inspect(m).get('conversation_id'), fixture.BOUND['turn'], 2)
        def linked():
            member = fx.inspect('m1')
            read = fx.json('parent', fx.sessions['m1']['name'], '--json', ok=False, stdin=json.dumps(dict(
                expected_run_id=member.get('run_id'), expected_conversation_id=member.get('conversation_id') or '',
                action='read')))
            return read if read.get('parent') else None
        parent = fx.wait('member parent link written by the service', linked, fixture.BOUND['parent'], 3)
        fx.evidence.write('member-parent.json', parent)
        self.assertEqual((parent.get('parent') or {}).get('name'), fx.sessions['lead']['name'],
                         'the member is linked to its lead (manual link written after binding)')

    def pinned_form(self, prompt):
        """Which acknowledgement form a delivered prompt pins: report-attempt exactly once
        and begin-attempt at most once (src/tasks/controls.rs pin_dispatch); anything else
        fails."""
        reports = re.findall(fixture.PINNED.format(verb='report-attempt'), prompt)
        begins = re.findall(fixture.PINNED.format(verb='begin-attempt'), prompt)
        self.assertEqual(len(reports), 1, 'report-attempt is pinned exactly once')
        self.assertLessEqual(len(begins), 1, 'begin-attempt is pinned at most once')
        if begins:
            return 'begin and report'
        self.assertIsNotNone(fixture.acknowledgement(prompt), 'the prompt pins an acknowledgement')
        return 'start and report'

    # 2 -----------------------------------------------------------------------
    def test_2_delivery_and_member_shell(self):
        fx = self.fx
        out = fx.root / 'out'
        fx.script('delivered', (
            'set -- "$@"; n=0; for x in ' + str(out) + '/delivered-*.json; do [ -e "$x" ] && n=$((n+1)); done; '
            'f=' + str(out) + '/delivered-$n.json\n'
            'current=$(python3 -c ' + shlex.quote(
                'import sys; sys.path.insert(0, ' + repr(str(Path(__file__).resolve().parent)) + '); '
                'import teams_native_fixture as f; import json; '
                'print(json.dumps(f.wait_for_held_task(sys.argv[1], sys.argv[2], ' + str(fixture.BOUND['turn']) + ')))'
            ) + ' "$H" "$3"); c=$?\n'
            'if [ "$c" -ne 0 ]; then exit "$c"; fi\n'
            'if [ "$5" = begin-attempt ]; then begin=$("$H" task begin-attempt "$1" --attempt "$2" --json 2>&1); b=$?; '
            'elif [ "$5" = start ]; then begin=$("$H" task start "$1" --json 2>&1); b=$?; '
            'else begin="no acknowledgement is pinned"; b=-1; fi\n'
            'inbox=$("$H" team inbox --team ' + fx.team + ' --json 2>&1); i=$?\n'
            'if [ -f ' + str(out) + '/begin-only ]; then report=""; r=-1; else\n'
            '  legacy=$(printf \'%s\' "$begin" | python3 -c \'import json,sys; print(json.load(sys.stdin).get("reporting") == "legacy")\')\n'
            '  if [ "$legacy" = True ]; then report=$(printf "Fixture report: nothing to change.\\n" | "$H" task report "$1" 2>&1); r=$?; '
            'else report=$(printf "Fixture report: nothing to change.\\n" | "$H" task report-attempt "$3" --attempt "$4" 2>&1); r=$?; fi\n'
            'fi\n'
            'python3 -c \'import json,sys; json.dump(dict(begin=sys.argv[1], begin_exit=int(sys.argv[2]), current=sys.argv[3], '
            'current_exit=int(sys.argv[4]), inbox=sys.argv[5], inbox_exit=int(sys.argv[6]), report=sys.argv[7], '
            'report_exit=int(sys.argv[8])), open(sys.argv[9], "w"))\' "$begin" $b "$current" $c "$inbox" $i "$report" $r "$f"\n'
            'echo "fixture step done"\n'))
        ref, task_uuid = fx.add_task('Fixture task one', 'Fixture task one: report that nothing needs to change.')
        node = fx.wait('task node in the queue', lambda: fx.node_of(task_uuid), fixture.BOUND['handout'], 2)
        result = fx.wait('member ran the delivered task in its shell',
                         lambda: (out / 'delivered-0.json').is_file() and json.loads((out / 'delivered-0.json').read_text()),
                         fixture.BOUND['handout'] + fixture.BOUND['turn'], 2)
        fx.evidence.write('delivered-0.json', result)
        # Delivery: the prompt the member's model saw names this task and holds it once.
        prompts = fx.requests_with(lambda r: (r.get('prompt') or '').startswith('[Zerus task ' + ref))
        self.assertTrue(prompts, 'no model request carried the delivered prompt')
        delivered = prompts[0]['prompt']
        # Follow the prompt's managed acknowledgement or legacy Start, then the
        # report origin from that acknowledgement; keep the form in the evidence.
        form = self.pinned_form(delivered)
        fx.evidence.write('delivered-0-form.json', dict(form=form))
        for request in prompts:
            self.assertEqual(sum(text == delivered for text in request['users']), 1,
                             'the delivered prompt appears once in the member conversation')
        # The member's own shell: its pinned acknowledgement, caller proof,
        # refusal, report.
        self.assertEqual(result['begin_exit'], 0, result['begin'])
        current = json.loads(result['current'])
        self.assertEqual(result['current_exit'], 0, result['current'])
        self.assertEqual(current['actor'], dict(kind='session', member='m1', person='local', team=fx.team))
        self.assertIn(node['id'], [n['id'] for n in current.get('nodes') or []])
        refused = json.loads(result['inbox'])
        self.assertIs(refused.get('ok'), False, result['inbox'])
        self.assertEqual(refused['error']['code'], 'not_allowed', 'a member is refused the lead inbox')
        self.assertEqual(result['report_exit'], 0, result['report'])
        task = fx.json('task', 'get', ref, '--json')
        task = task.get('task') or task
        self.assertEqual(task.get('stage'), 'review', 'the report moved the task to In review')
        type(self).first = dict(ref=ref, uuid=task_uuid, node=node['id'])

    # 3 -----------------------------------------------------------------------
    def test_3_lead_doorbell(self):
        fx = self.fx
        bell = re.compile(r'(1 item waits|\d+ items wait) in your team inbox\. Read them with: hgs team inbox --team '
                          + re.escape(fx.team))
        rang = fx.wait('doorbell reached the lead', lambda: fx.requests_with(lambda r: bell.fullmatch(r.get('prompt') or '')),
                       fixture.BOUND['doorbell'], 2)
        text = rang[0]['prompt']
        latest = fx.requests_with(lambda r: text in r['users'])[-1]
        self.assertEqual(latest['users'].count(text), 1, 'one doorbell, not a repeat')
        self.assertNotIn('Fixture report', text, 'the doorbell carries none of the item')
        out = fx.root / 'out/lead-inbox.json'
        fx.script('lead-inbox', '"$H" team inbox --team ' + fx.team + ' --json > ' + str(out) + ' 2>&1; echo done\n')
        fx.send('lead', 'ZT-RUN lead-inbox')
        inbox = fx.wait('lead read its inbox in its own shell', lambda: out.is_file() and json.loads(out.read_text() or 'null'),
                        fixture.BOUND['turn'], 2)
        fx.evidence.write('lead-inbox.json', inbox)
        self.assertEqual(inbox['actor'].get('kind'), 'session')
        self.assertEqual(inbox['actor'].get('member'), 'lead')
        self.assertTrue(any(row.get('kind') == 'report' and row.get('node') == self.first['node'] for row in inbox['rows']))

    # 4 -----------------------------------------------------------------------
    def test_4_cancel_notice(self):
        fx = self.fx
        out = fx.root / 'out'
        (out / 'begin-only').write_text('the member keeps working without a report\n')
        ref, task_uuid = fx.add_task('Fixture task two', 'Fixture task two: keep working until cancelled.')
        result = fx.wait('member ran the second task in its shell and kept working',
                         lambda: (out / 'delivered-1.json').is_file() and json.loads((out / 'delivered-1.json').read_text()),
                         fixture.BOUND['handout'] + fixture.BOUND['turn'], 2)
        second = fx.requests_with(lambda r: (r.get('prompt') or '').startswith('[Zerus task ' + ref))
        self.assertTrue(second, 'no model request carried the second delivered prompt')
        form = self.pinned_form(second[0]['prompt'])
        fx.evidence.write('delivered-1-form.json', dict(form=form))
        self.assertEqual((result['begin_exit'], result['report_exit']), (0, -1), result)
        node = fx.node_of(task_uuid)
        cancel = out / 'lead-cancel.json'
        fx.script('lead-cancel', (
            'rev=$("$H" team status --team ' + fx.team + ' --json | python3 -c "import json,sys; print(json.load(sys.stdin)[\'team\'][\'revision\'])")\n'
            'printf \'{"request_id":"%s","team":"' + fx.team + '","expected_revision":%s,"node":' + str(node['id'])
            + ',"reason":"Fixture cancel"}\' "$(python3 -c "import uuid; print(uuid.uuid4())")" "$rev" | '
            '"$H" team cancel --json > ' + str(cancel) + ' 2>&1; echo done\n'))
        fx.send('lead', 'ZT-RUN lead-cancel')
        reply = fx.wait('lead cancelled the task in its own shell',
                        lambda: cancel.is_file() and json.loads(cancel.read_text() or 'null'), fixture.BOUND['turn'], 2)
        fx.evidence.write('lead-cancel.json', reply)
        self.assertTrue(reply.get('ok'), reply)
        self.assertEqual(reply['actor'].get('member'), 'lead')
        notice = re.compile(r'\[Zerus task ' + re.escape(ref) + r' cancelled\]')
        seen = fx.wait('member received the cancel notice',
                       lambda: fx.requests_with(lambda r: notice.match(r.get('prompt') or '')), fixture.BOUND['cancel'], 2)
        latest = fx.requests_with(lambda r: seen[0]['prompt'] in r['users'])[-1]
        self.assertEqual(latest['users'].count(seen[0]['prompt']), 1, 'the notice arrives once')
        task = fx.json('task', 'get', ref, '--json')
        task = task.get('task') or task
        self.assertTrue(task.get('cancelled_at'), 'the task is cancelled')

    # 6 (waiting) --------------------------------------------------------------
    def waiting_6_a_member_shell_in_a_private_pid_namespace(self):
        """Written down, not built (zerus-63rru.76). A member whose agent runs its shell
        commands inside a private PID namespace (the agent's own sandbox, when a person
        turned one on with a private process namespace) is handed a task. Expected, once
        the other stream's helper is synced: the member's own report-attempt and every team
        command in its shell are refused with the refusal that names the separate process
        namespace and its remedy; no report is recorded for the node; and the lead is told
        (its inbox holds the refusal, with one doorbell)."""
        raise unittest.SkipTest('waits for zerus-63rru.76; not built')

    # 5 -----------------------------------------------------------------------
    def test_5_stop_and_terminate(self):
        fx = self.fx
        rows = fx.json('team', 'queue', '--team', fx.team, '--limit', '200', '--json').get('rows') or []
        if not any(row.get('state') in ('waiting', 'pending', 'in_progress') for row in rows):
            # The stop must have an unfinished node to cancel; the earlier steps' tasks are
            # reported and cancelled, and a free member may take this one at once.
            _, task_uuid = fx.add_task('Fixture task for the stop', 'Fixture task: unfinished when the team stops.')
            fx.wait('task node in the queue', lambda: fx.node_of(task_uuid), fixture.BOUND['handout'], 2)
        before = {m: fx.process(m) for m in ('lead', 'm1')}
        refusals = []

        def stop():
            # The service may change the team between the person's read and the stop (a
            # hand-out of the task above): a stale revision changes nothing, so the person
            # refreshes and asks again with a new request.
            revision = fx.status()['team']['revision']
            reply = fx.json('team', 'stop', '--json', ok=False, stdin=json.dumps(dict(
                request_id=str(uuid.uuid4()), team=fx.team, expected_revision=revision)))
            if (reply.get('error') or {}).get('code') == 'stale_revision':
                refusals.append(revision)
                return None
            return reply
        reply = fx.wait("the person's team stop at a fresh revision", stop, fixture.BOUND['stop'], 1)
        fx.evidence.write('stop-reply.json', dict(reply=reply, stale_revisions=refusals))
        self.assertTrue(reply.get('ok'), reply)
        fx.wait('team stopped', lambda: fx.status()['team'].get('status') == 'stopped', fixture.BOUND['stop'], 2)
        # The service acts on the stop (it records the cancel of the unfinished node) and
        # ends no agent. The node's own state is the store's rule (package 10), not agent
        # behaviour, so this step reads the service's journal entry, not the queue.
        def acted():
            rows = fx.json('team', 'journal', '--team', fx.team, '--limit', '200', '--json').get('rows') or []
            stops = [r['seq'] for r in rows if r.get('kind') == 'stop']
            after = {r.get('kind') for r in rows if stops and r['seq'] > stops[-1]
                     and (r.get('actor') or {}).get('kind') == 'service'}
            return {'record_cancel', 'record_key_clear'} <= after or None
        fx.wait('service recorded the stopped team\'s cancel and key clear', acted, fixture.BOUND['stop'], 2)
        for member_id, (pid, ticks) in before.items():
            self.assertEqual(fx.start_ticks(pid), ticks, f'{member_id} still runs the same process after the stop')
        view = fx.inspect('m1')
        pid, _ = before['m1']
        fx.hgs('terminate', fx.sessions['m1']['name'], '--expected-run-id', view['run_id'], timeout=120)
        fx.wait('member process exited after Terminate', lambda: fx.start_ticks(pid) is None or None,
                fixture.BOUND['exit'], 1)
        archives = [json.loads(p.read_text()) for p in (Path(fx.env['HGS_STATE_DIR']) / 'archives').glob('*.json')]
        self.assertTrue(any(a.get('run_id') == view['run_id'] for a in archives), 'Terminate archived the run')


class TrustContract(ClassFixture):
    """Delegation confirm_trust on: the standing confirmation answers the members' startup
    folder-trust cards (zerus-63rru.48), and a prompt that is not a startup prompt stays
    the person's. The agent's own trust record proves the answer reached it."""
    delegations = {'confirm_trust': True}
    trust_source = None

    def test_1_the_service_answers_the_startup_folder_trust(self):
        fx = self.fx
        answered = [json.loads(line) for line in (fx.evidence.folder / 'person-answers.jsonl').read_text().splitlines()]
        for member_id in self.member_ids():
            with self.subTest(member=member_id):
                self.assertFalse([a for a in answered if a['member'] == member_id and a['source'] == self.trust_source],
                                 'the person did not answer it')
                name = fx.sessions[member_id]['name']
                rows = fx.audit_rows(name)
                fx.evidence.write(f'audit-{member_id}.json', rows)
                grants = [r for r in rows if r['source'] == self.trust_source]
                self.assertEqual(len(grants), 1, 'one audited grant for the card')
                self.assertEqual(grants[0]['status'], 'approved', grants[0])
                # The card the service answered, as its grant saved it: the real startup card
                # with its null conversation. The service may answer before the fixture's
                # first look; a card the fixture did see is the granted one.
                card = grants[0]['saved_question']
                self.assertEqual(card['questions'][0]['question'], 'Trust this folder?')
                self.assertIn('conversation_id', card)
                self.assertIsNone(card['conversation_id'], 'the real card carries a null conversation')
                seen = [v['card'] for k, v in fx.cards_seen.items() if k[0] == member_id and v['source'] == self.trust_source]
                self.assertLessEqual(len(seen), 1, 'the startup trust card appeared at most once')
                if seen:
                    self.assertEqual(seen[0].get('question_id'), card.get('question_id'), 'the card seen is the card granted')
                pending = [q for q in fx.pending(member_id) if q.get('source') == self.trust_source]
                self.assertFalse(pending, 'no second startup trust card for the member')
                self.assert_agent_trusted(member_id)
        refusals = [c for c in fx.cards() if 'needs your attention' in c.get('text', '')]
        fx.evidence.write('cards-after-trust.json', refusals)
        self.assertFalse(refusals, 'no member card was left for the person')
        journal_is(self, self.startup_grants(), 'the journal holds exactly the two startup grants')

    def member_ids(self):
        return ('lead',) + tuple(f'm{number}' for number in range(1, self.count + 1))

    def startup_grants(self):
        return sorted((member, self.trust_source, ('team_folder_trust',), 'approved') for member in self.member_ids())

    def test_2_an_ordinary_question_stays_the_persons(self):
        """After the startup grants, every member asks an ordinary question (zerus-63rru.73)."""
        ordinary_question_stays_the_persons(self, self.member_ids()[1:])

    def assert_agent_trusted(self, member_id):
        raise NotImplementedError


class ClaudeTrust:
    """Claude Code 2.1.322 records a folder-trust answer in a linked worktree as trust for
    the repository's main checkout, on the member's account, and none for the worktree;
    a second worktree on the same account still shows its card (zerus-63rru.66: the same
    record a person's own answer leaves). Pinned as observed, so a release that stores it
    elsewhere or stops asking fails here. Two members share one account for that."""
    trust_source = 'claude_folder_trust'
    service_answers = ('claude_folder_trust',)
    count = 2
    pinned = 'zt-claude'

    # A rule, not a race (probes tp1, tp3, tp4): after an answer given on a worktree card,
    # a later worktree on the same account asks again, with or without another session of
    # the account running; only an answer given in the main checkout itself stops it.
    def test_3_a_second_worktree_on_the_same_account_still_asked(self):
        fx = self.fx
        launches = {}
        for row in fx.listed(fx.json('team', 'journal', '--team', fx.team, '--limit', '200', '--json'), 'rows', 'team journal'):
            if row.get('kind') == 'set_member_session':
                member = next((ref[len('member:'):] for ref in row.get('refs') or [] if ref.startswith('member:')), None)
                launches.setdefault(member, row['at_ms'])
        shared = sorted((launches[m], m) for m in self.member_ids() if fx.members()[m].get('account') == self.pinned)
        fx.evidence.write('shared-account-launches.json', dict(launches=launches, shared=shared))
        self.assertEqual(len(shared), 2, 'two members on the pinned account')
        (_, first), (later_at, later) = shared
        first_grant = [r for r in fx.audit_rows(fx.sessions[first]['name']) if r['source'] == self.trust_source]
        self.assertEqual(len(first_grant), 1)
        self.assertGreater(later_at, first_grant[0]['at'], 'the later member launched after the first grant on its account')
        later_grant = [r for r in fx.audit_rows(fx.sessions[later]['name']) if r['source'] == self.trust_source]
        self.assertEqual(len(later_grant), 1, 'the later worktree on the same account still showed its own card')

    def assert_agent_trusted(self, member_id):
        fx = self.fx
        home = fx.agent_home(member_id)
        # Where Claude keeps trust, recorded before asserting it (zerus-63rru.60: the grant
        # must be no wider than the card's folder).
        records = fx.claude_trust_records()
        fx.evidence.write(f'claude-trust-records-{member_id}.json', dict(home=str(home), records=records))
        mine = records.get(str(home / '.claude.json'))
        self.assertIsInstance(mine, dict, "the member account's own .claude.json is readable")
        main = str((fx.root / 'work/proj').resolve())
        self.assertEqual(mine, {main: True}, "the account holds exactly the main checkout as trusted, no worktree key")


def codex_config(fx, member):
    """The config.toml Codex reads for a member: the native account's in HOME/.codex,
    a managed account's in its own home."""
    account = fx.members()[member].get('account')
    if not account:
        raise AssertionError(f'{member} has no account')
    if account.startswith('native-'):
        return Path(fx.env['HOME']) / '.codex/config.toml'
    return Path(fx.env['HGS_CONFIG_DIR']) / 'accounts' / account / 'config.toml'


def codex_trust_problems(records, accounts, main):
    """How Codex's recorded folder trust differs from the card the person answered: every
    members' account configuration holds exactly {main: trusted}, and every other
    config.toml in the root holds no project at all. A wider, narrower or extra record is a
    problem."""
    problems = [f'{path}: not a readable configuration' for path, value in records.items() if not isinstance(value, dict)]
    for path in sorted(accounts):
        if records.get(path) != {main: 'trusted'}:
            problems.append(f'{path}: {records.get(path)!r} instead of {{{main!r}: trusted}}')
    for path, value in records.items():
        if path not in accounts and value:
            problems.append(f'{path}: unexpected projects {value!r}')
    return problems


class CodexTrustContract(ClassFixture):
    """Standing trust records the team repository's main checkout and subfolders,
    as owner decision D9 permits. The product's saved native question proves
    which card was answered even when it precedes the fixture's first look.
    The ordinary Codex startup case retains the physical 80x24 panel variants."""
    delegations = {'confirm_trust': True}
    service_answers = ('codex_folder_trust',)

    def test_1_the_service_answers_the_repository_scope_and_records_the_grant(self):
        fx = self.fx
        answered = [json.loads(line) for line in (fx.evidence.folder / 'person-answers.jsonl').read_text().splitlines()]
        main = str((fx.root / 'work/proj').resolve())
        for member_id in ('lead', 'm1'):
            with self.subTest(member=member_id):
                rows = fx.audit_rows(fx.sessions[member_id]['name'])
                fx.evidence.write(f'audit-{member_id}.json', rows)
                grants = [r for r in rows if r['source'] == 'codex_folder_trust']
                self.assertEqual(len(grants), 1, 'one product-written grant for the native card')
                grant = grants[0]
                self.assertEqual((grant['status'], grant['categories'], grant['scope']),
                                 ('approved', ['team_folder_trust'], main))
                card = grant['saved_question']
                self.assertEqual(card['run_id'], fx.sessions[member_id]['run'])
                self.assertIsNone(card['conversation_id'], 'the grant answered the startup card')
                self.assertTrue(card.get('can_answer'))
                item = card['questions'][0]
                self.assertEqual(item['question'], 'Trust this Git repository?')
                self.assertEqual(item['body'].split('\n\n', 1)[0].replace('\n', ''),
                                 fx.members()[member_id]['worktree'])
                self.assertIn(main, item['options'][0]['description'])
                self.assertFalse([a for a in answered if a['member'] == member_id and a['source'] == 'codex_folder_trust'],
                                 'the person did not answer the delegated trust card')
                self.assertFalse([q for q in fx.pending(member_id) if q.get('source') == 'codex_folder_trust'])
                seen = [v['card'] for k, v in fx.cards_seen.items() if k[0] == member_id and v['source'] == 'codex_folder_trust']
                self.assertLessEqual(len(seen), 1)
                if seen:
                    self.assertEqual(seen[0]['question_id'], card['question_id'])
        fx.evidence.write('codex-trust-cards.json', fx.cards())
        self.assertFalse([c for c in fx.cards() if 'needs your attention' in c.get('text', '')])
        journal_is(self, [(m, 'codex_folder_trust', ('team_folder_trust',), 'approved') for m in ('lead', 'm1')],
                   'the journal holds exactly the two startup grants')
        records = fx.codex_trust_records()
        fx.evidence.write('codex-trust-records-asserted.json', records)
        accounts = {str(codex_config(fx, member)) for member in ('lead', 'm1')}
        self.assertEqual(codex_trust_problems(records, accounts, main), [])


class ResumeContract(ClassFixture):
    """Delegation resume_exited on (folder trust stays the person's): a member that has
    worked (the person's message and its answer) and whose agent then exits by itself is
    resumed with its brief, in the same launch and conversation and a new run; a member
    the person terminated is not resumed."""
    delegations = {'resume_exited': True}
    exit_keys = ('/exit', 'Enter')

    def during_exit(self, before):
        return nullcontext()

    def brief_text(self):
        return (f'You are a member of team {self.fx.team} in role zt-dev. '
                'Read `hgs team current` for your current team and work.')

    def test_1_a_member_that_exits_by_itself_is_resumed_with_its_brief(self):
        fx = self.fx
        # A resume continues a persisted conversation: the member works once first (Codex
        # also starts its conversation at this message).
        fx.worked('m1', 'Fixture message before the exit.')
        before = fx.inspect('m1')
        pid, _ = fx.process('m1')
        target = '=' + fx.sessions['m1']['name'] + ':'
        # Evidence for a refused resume: whether the member's folder exists over time.
        watch = fixture.FolderWatch(fx.members()['m1']['worktree'])
        watch.start()
        try:
            with self.during_exit(before):
                for key in self.exit_keys:
                    subprocess.run(['tmux', 'send-keys', '-t', target, key], env=fx.env, check=True)
                fx.wait('m1 agent exited by itself', lambda: fx.start_ticks(pid) is None or None, fixture.BOUND['exit'], 1)
            resumed = fx.wait('the service resumed m1 in a new run', lambda: (lambda v: v if v.get('run_id') not in (None, before['run_id'])
                              and v.get('runtime_state') == 'live' else None)(fx.inspect('m1')), fixture.BOUND['doorbell'], 3)
        except AssertionError:
            fx.evidence.write('m1-folder-watch.json', watch.finish())
            self.diagnose_refused_resume()
            raise
        fx.evidence.write('m1-folder-watch.json', watch.finish())
        fx.evidence.write('resumed-view.json', {k: resumed.get(k) for k in ('run_id', 'conversation_id', 'launch_id', 'activity')})
        self.assertEqual(resumed.get('launch_id'), fx.sessions['m1']['launch'], 'the same launch')
        self.assertTrue(before.get('conversation_id'), 'the exited run had a conversation')
        self.assertEqual(resumed.get('conversation_id'), before.get('conversation_id'), 'the same conversation')
        brief = self.brief_text()
        seen = fx.wait('the brief reached the resumed member', lambda: fx.requests_with(lambda r: r.get('prompt') == brief),
                       fixture.BOUND['turn'], 2)
        latest = fx.requests_with(lambda r: brief in r['users'])[-1]
        self.assertEqual(latest['users'].count(brief), 1, 'the brief arrives once')
        rows = [r for r in fx.audit_rows(fx.sessions['m1']['name']) if r['source'] == 'team_resume']
        fx.evidence.write('resume-audit.json', rows)
        # One reservation for the resume of the exited run and one for the brief in the new run.
        self.assertEqual(sorted((r['question_id'].split(':', 1)[0], r['run']) for r in rows),
                         [('brief', resumed['run_id']), ('resume', before['run_id'])], 'the resume and its brief are audited')
        journal_is(self, self.resume_journal(), 'the journal holds exactly the resume and its brief')
        type(self).resumed_run = resumed['run_id']

    def resume_journal(self):
        return [('m1', 'team_resume', ('team_resume',), 'approved')] * 2

    def diagnose_refused_resume(self):
        """A refused resume: what the service reads at that moment, then the person's own
        Restore of the member's archive, which tells a fault of Teams from a refused
        Restore."""
        fx = self.fx
        try:
            seen = fx.observation()
            fx.evidence.write('observation-at-refusal.json', seen)
            archives = (seen.get('m1') or {}).get('archives') or []
            if archives:
                fx.evidence.write('person-restore.json', fx.person_restore('m1', archives[-1]))
        except Exception as error:
            fx.evidence.write('person-restore.json', dict(error=str(error)[:500]))

    def test_2_a_member_the_person_terminates_is_not_resumed(self):
        fx = self.fx
        view = fx.inspect('m1')
        pid, _ = fx.process('m1')
        # The launch from the team store's binding (fails if absent), and the view agrees.
        launch = fx.sessions['m1']['launch']
        self.assertEqual(view.get('launch_id'), launch)
        last = max([c.get('seq', 0) for c in fx.cards()] or [0])
        fx.hgs('terminate', fx.sessions['m1']['name'], '--expected-run-id', view['run_id'], timeout=120)
        fx.wait('m1 process exited after Terminate', lambda: fx.start_ticks(pid) is None or None, fixture.BOUND['exit'], 1)
        refused = fx.wait('the service refused to resume the terminated member, with a card for the person',
                          lambda: [c for c in fx.cards() if c.get('seq', 0) > last
                                   and 'Member m1 needs your attention' in c.get('text', '')], fixture.BOUND['doorbell'], 3)
        fx.evidence.write('terminate-refusal.json', refused)
        rows = [r for r in fx.audit_rows(fx.sessions['m1']['name'])
                if r['source'] == 'team_resume' and r['run'] == view['run_id'] and r['question_id'].startswith('resume:')]
        self.assertFalse(rows, 'no resume was reserved for the terminated run')
        # The positive control: the same reader still finds the first resume and its brief.
        journal_is(self, self.resume_journal(), 'the journal still holds only the first resume and its brief')
        live = fx.live_runs(launch, control=fx.sessions['lead']['launch'])
        self.assertFalse(live, 'no session of the member launch runs again')


class UnworkedResumeContract(ClassFixture):
    """Delegation resume_exited on: a member that exits before any message (launched and
    closed before its first hand-out) has no persisted conversation, so its restore is
    refused before launch. What zerus-63rru.62's fix makes true: nothing starts, and the
    person's card carries the command's own reason. The person's own Restore of that
    archive is recorded beside it."""
    delegations = {'resume_exited': True}
    exit_keys = ('/exit', 'Enter')

    def test_1_a_member_that_never_worked_is_not_restored_and_the_card_says_why(self):
        fx = self.fx
        before = fx.inspect('m1')
        pid, _ = fx.process('m1')
        last = max([c.get('seq', 0) for c in fx.cards()] or [0])
        target = '=' + fx.sessions['m1']['name'] + ':'
        for key in self.exit_keys:
            subprocess.run(['tmux', 'send-keys', '-t', target, key], env=fx.env, check=True)
        fx.wait('m1 agent exited by itself', lambda: fx.start_ticks(pid) is None or None, fixture.BOUND['exit'], 1)
        cards = fx.wait('the service told the person about m1', lambda: [c for c in fx.cards() if c.get('seq', 0) > last
                        and 'Member m1 needs your attention' in c.get('text', '')], fixture.BOUND['doorbell'], 3)
        fx.evidence.write('unworked-cards.json', cards)
        live = fx.live_runs(fx.sessions['m1']['launch'], control=fx.sessions['lead']['launch'])
        seen = fx.observation()
        fx.evidence.write('observation-at-refusal.json', seen)
        archives = (seen.get('m1') or {}).get('archives') or []
        self.assertTrue(archives, 'the exited member has an archive inspected by the product')
        archive = archives[-1]
        view = archive['view']
        self.assertNotIn('restore', view['allowed_actions'])
        reason = view['action_reasons']['restore']
        self.assertTrue(reason, 'the archive inspection explains why it offers no restore')
        restored = fx.person_restore('m1', archive)
        fx.evidence.write('person-restore.json', restored)
        self.assertEqual(restored['receipt']['status'], 'failed', restored)
        self.assertFalse(fx.live_runs(fx.sessions['m1']['launch'], control=fx.sessions['lead']['launch']))
        fx.evidence.write('journal-after-refusal.json', fx.journal_summary())
        self.assertFalse(live, 'nothing of the member launch started')
        self.assertTrue(any(reason in c.get('text', '') for c in cards), "the card carries the command's own reason")


class QuestionAfterTrustContract(ClassFixture):
    """Delegation confirm_trust on, and the person has trusted the main checkout on both
    Claude accounts before the team starts, so no member shows a startup card. An ordinary
    question then plans nothing: the mirror of the trust case's test_2 (zerus-63rru.73:
    today it opens one notice card)."""
    delegations = {'confirm_trust': True}
    pinned = 'zt-claude'
    pretrust = ('native-claude', 'zt-claude')

    def test_1_no_startup_card_and_an_ordinary_question_stays_the_persons(self):
        fx = self.fx
        # The positive control: the same capture saw the person's two plain sessions' cards.
        plain = sorted(k[0] for k, v in fx.cards_seen.items() if k[0].startswith('plain-') and v['source'] == 'claude_folder_trust')
        self.assertEqual(plain, ['plain-native-claude', 'plain-zt-claude'], 'the capture saw the plain sessions\' trust cards')
        startup = [v['source'] for k, v in fx.cards_seen.items() if not k[0].startswith('plain-')]
        self.assertEqual(startup, [], 'no member showed a startup card')
        journal_is(self, [], 'the journal holds no grant: there was no startup card')
        ordinary_question_stays_the_persons(self, ['m1'])


class TeamsFixtureIsolation(unittest.TestCase):
    """The fixture's own isolation, without an agent: the tmux wrapper judges resolved
    paths, and the preflight refuses the runner's account name in a child's environment."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='hgs-teams-isolation-')
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name).resolve()
        self.marker = self.base / 'real-tmux-called'
        stand_in = self.base / 'real-tmux'
        stand_in.write_text('#!/bin/sh\necho "$@" >> ' + str(self.marker) + '\n')
        stand_in.chmod(0o755)
        native = self.base / 'agent'
        native.write_text('#!/bin/sh\n')
        native.chmod(0o755)
        patches = [mock.patch.object(fixture, 'REAL_TMUX', str(stand_in)), mock.patch.object(fixture, 'HGS', str(native)),
                   mock.patch.dict(os.environ, ZERUS_TEAMS_EVIDENCE=str(self.base / 'evidence'))]
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)
        # The root goes straight into TMPDIR, as in a run, so its socket paths stay short.
        self.fx = fixture.TeamFixture('claude', native, 'Isolation')
        self.addCleanup(shutil.rmtree, self.fx.root, True)
        self.fx.build()
        self.outside = self.base / 'outside'
        self.outside.mkdir()
        server = socket.socket(socket.AF_UNIX)
        self.addCleanup(server.close)
        server.bind(str(self.outside / 'default'))

    def tmux(self, *argv, **env):
        self.marker.unlink(missing_ok=True)
        run = subprocess.run([str(self.fx.root / 'bin/tmux'), *argv], env=dict(self.fx.env, **env),
                             capture_output=True, text=True, timeout=30)
        return run.returncode, self.marker.exists()

    def test_tmux_wrapper_refuses_a_link_out_of_the_root_before_tmux_runs(self):
        private = self.fx.root / 'tmux'
        self.assertEqual(self.tmux('-S', str(private / 'own.sock'), 'ls'), (0, True), 'a private socket passes')
        self.assertEqual(self.tmux('ls'), (0, True), 'a fresh private server passes before tmux-<uid> exists')
        (private / 'link.sock').symlink_to(self.outside / 'default')
        (private / 'dangling.sock').symlink_to(self.outside / 'missing')
        for name, argv, env in (
                ('-S link', ('-S', str(private / 'link.sock'), 'ls'), {}),
                ('$TMUX link', ('ls',), dict(TMUX=str(private / 'link.sock') + ',1,0')),
                ('dangling link', ('-S', str(private / 'dangling.sock'), 'ls'), {})):
            with self.subTest(case=name):
                self.assertEqual(self.tmux(*argv, **env), (1, False), name + ' never reaches tmux')
        (private / ('tmux-%d' % os.getuid())).symlink_to(self.outside, target_is_directory=True)
        for name, argv in (('TMUX_TMPDIR route', ('ls',)), ('labelled server', ('-L', 'other', 'ls'))):
            with self.subTest(case=name):
                self.assertEqual(self.tmux(*argv), (1, False), name + ' through a linked tmux-<uid> never reaches tmux')
        log = (self.fx.evidence.folder / 'tmux-calls.log').read_text().splitlines()
        self.assertEqual([line.split('\t')[1] for line in log], ['private', 'private'] + ['refused'] * 5)

    def test_preflight_refuses_the_runner_account_name(self):
        account = pwd.getpwuid(os.getuid()).pw_name
        self.assertEqual((self.fx.env['USER'], self.fx.env['LOGNAME']), (fixture.FIXTURE_USER, fixture.FIXTURE_USER))
        self.fx.preflight()
        for key, value, refusal in (('USER', account, 'account name'), ('LOGNAME', account, 'account name'),
                                    ('HGS_PEERS', account, 'runner account name is the value of HGS_PEERS'),
                                    ('USER', 'someone-else', 'not the fixture account name'),
                                    ('LOGNAME', 'someone-else', 'not the fixture account name')):
            with self.subTest(variable=key, value=value):
                saved = self.fx.env[key]
                self.fx.env[key] = value
                with self.assertRaisesRegex(AssertionError, refusal):
                    self.fx.preflight()
                self.fx.env[key] = saved

    def test_kimi_stand_in_and_executable_image_survive_hgs_path_preparation(self):
        import hashlib
        import stat
        source = self.base / 'sealed-kimi'
        source.write_text('#!/bin/sh\nexit 0\n')
        source.chmod(0o400)
        fx = fixture.TeamFixture('kimi', source, 'KimiIsolation')
        self.addCleanup(shutil.rmtree, fx.root, True)
        fx.build()
        fx.preflight()
        home_bin = fx.root / 'home/.local/bin'
        self.assertEqual(Path(shutil.which('node', path=fx.env['PATH'])), home_bin / 'node')
        self.assertEqual(Path(shutil.which('kimi', path=fx.env['PATH'])).resolve(), fx.native)
        self.assertNotEqual(fx.native, source.resolve())
        self.assertEqual(stat.S_IMODE(fx.native.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(source.stat().st_mode), 0o400)
        def digest(path):
            with path.open('rb') as stream:
                return hashlib.file_digest(stream, 'sha256').hexdigest()
        self.assertEqual(digest(source), fx.native_sha256)
        self.assertEqual(digest(fx.native), fx.native_sha256)
        run = subprocess.run([str(home_bin / 'node'), '--input-type=module', '-e', 'inspection'],
                             env=dict(fx.env, KIMI_CODE_HOME=str(fx.root / 'home/.kimi-code')),
                             capture_output=True, text=True, timeout=30)
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertEqual(json.loads(run.stdout), fixture.kimi_inspection())
        # The production rule that put the hosted runner's /usr/local/bin/node ahead
        # of the old stand-in is part of this fixture's placement contract.
        production = Path(__file__).resolve().parents[1] / 'src/platform/unix.rs'
        self.assertIn('"{home}/.local/bin:/opt/homebrew/bin:/usr/local/bin:{path}:', production.read_text())

    def test_kimi_fd_stand_in_prevents_managed_download(self):
        source = self.base / 'sealed-kimi-fd'
        source.write_text('#!/bin/sh\nexit 0\n')
        source.chmod(0o400)
        fx = fixture.TeamFixture('kimi', source, 'KimiFdIsolation')
        self.addCleanup(shutil.rmtree, fx.root, True)
        fx.build()
        fd = Path(fx.env['HOME']) / '.local/bin/fd'
        self.assertEqual(shutil.which('fd', path=fx.env['PATH']), str(fd))
        version = subprocess.run([str(fd), '--version'], env=fx.env, capture_output=True, text=True, timeout=30)
        self.assertEqual((version.returncode, version.stdout), (0, 'fd fixture\n'))
        refused = subprocess.run([str(fd), 'unexpected-operation'], env=fx.env, capture_output=True, text=True, timeout=30)
        self.assertEqual(refused.returncode, 97)
        log = fx.evidence.folder / 'fd-stand-in.log'
        self.assertEqual(len(log.read_text().splitlines()), 2)
        fx.preflight()
        fd.unlink()
        with self.assertRaisesRegex(AssertionError, 'resolves fd'):
            fx.preflight()

    def test_kimi_terminal_trust_answer_uses_the_exact_pane(self):
        from types import SimpleNamespace
        from test_kimi_approvals_native import FixtureControls
        short = '/tmp/hgs-pause-x/project with spaces'
        folder = short + '/' + 'x' * 80
        card = dict(source='startup_terminal_choice', question_id='startup-choice:fixture', can_answer=False,
                    questions=[dict(id='terminal', question='Trust this folder?', options=[])])
        pending = dict(pending_questions=[card], runtime_state='live', process_state='running', activity='busy',
                       conversation_id=None, first_message_can_send=False, run_id='run', cwd=folder)
        ready = dict(pending, pending_questions=[], activity='idle', conversation_id='conversation')

        def answer(pane, succeeds):
            fx = fixture.TeamFixture.__new__(fixture.TeamFixture)
            fx.agent, fx.env = 'kimi', {}
            fx.after_refusal, fx.service_answers = {}, ()
            fx.panes, fx.cards_seen = {}, {}
            fx.sessions = {'lead': {'name': 'kimi/example/team'}}
            fx.evidence = SimpleNamespace(append=mock.Mock())
            fx.capture_card = lambda member, item: fx.panes.__setitem__((member, item['question_id']), pane)
            fx.inspect = mock.Mock(side_effect=[pending, ready if succeeds else pending])
            def wait(label, predicate, bound, step):
                for _ in range(2):
                    value = predicate()
                    if value:
                        return value
                raise AssertionError('not ready')
            fx.wait = wait
            with mock.patch.object(fixture.subprocess, 'run') as run:
                if succeeds:
                    self.assertEqual(fx.answer_startup('lead'), ready)
                else:
                    with self.assertRaisesRegex(AssertionError, 'not ready'):
                        fx.answer_startup('lead')
            return run

        chooser = FixtureControls.CHOOSER.replace('  ' + short, '  ' + folder[:77] + '\n  ' + folder[77:])
        answered = answer(chooser, True)
        answered.assert_called_once_with(['tmux', 'send-keys', '-t', '=kimi/example/team:', 'Enter'], env={}, check=True)
        changed = chooser.replace('❯ Trust and continue', '  Trust and continue').replace('     Exit', '   ❯ Exit')
        self.assertFalse(answer(changed, False).call_count, 'a changed selection must stay in Terminal for the person')


class TeamsWaitingList(unittest.TestCase):
    """Every written class is selected or waiting, and the waiting cases' fixes are named
    alike wherever the contract lists them."""

    ISSUE = re.compile(r'zerus-63rru\.\d+')
    ROOT = Path(__file__).resolve().parent.parent

    def script(self, name):
        import importlib.util
        spec = importlib.util.spec_from_file_location('teams_' + name.removesuffix('.py').replace('-', '_'),
                                                      self.ROOT / 'scripts/ci' / name)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def test_every_class_is_selected_or_waiting_and_each_list_names_the_same_fixes(self):
        compat, kimi = self.script('agent-compatibility.py'), self.script('kimi-native.py')
        selected = {name.split('.')[1] for names in compat.TEAMS_CLASSES.values() for name in names}
        selected |= {name.split('.')[1] for name in kimi.TEAMS_METHODS}
        written = {name for name, value in globals().items() if name.startswith('NativeTeams') and isinstance(value, type)}
        waiting = {name for name in WAITING if name in written}
        self.assertFalse(selected & waiting, 'a waiting class is not selected')
        self.assertEqual(written, selected | waiting, 'every written class is selected or waiting')
        self.assertEqual(set(WAITING) - waiting, {name for name in dir(TeamsContract) if name.startswith('waiting_')})
        self.assertEqual(self.mismatches(*self.places(compat), WAITING), [])

    def places(self, compat):
        header = (self.ROOT / '.github/workflows/teams-native.yml').read_text().split('\non:', 1)[0]
        docs = (self.ROOT / 'docs/ci-and-aur.md').read_text()
        section = docs.split('\n### Agent Teams real agents\n', 1)[1].split('\n## ', 1)[0]
        return header, section, compat.TEAMS_COVERAGE

    def mismatches(self, header, section, coverage, waiting):
        """The places whose fixes differ from the waiting list's."""
        fixes = set(self.ISSUE.findall(' '.join(waiting.values())))
        return [place for place, text in (('workflow header', header), ('docs section', section), ('row coverage', coverage))
                if set(self.ISSUE.findall(text)) != fixes]

    def test_kimi_steps_not_yet_proven_are_said_so_in_the_header_and_the_docs(self):
        kimi = self.script('kimi-native.py')
        selected = {name.rsplit('.', 1)[1] for name in kimi.TEAMS_METHODS}
        self.assertLessEqual(set(KIMI_UNPROVEN), selected, 'the unproven Kimi steps are selected')
        header, section, _ = self.places(self.script('agent-compatibility.py'))
        phrase = re.compile(r"Kimi Code's steps 2 to 4[^.]*not yet proven")
        for place, text in (('workflow header', header), ('docs section', section)):
            with self.subTest(place=place):
                flat = ' '.join(line.lstrip('#').strip() for line in text.splitlines())
                self.assertEqual(bool(phrase.search(flat)), bool(KIMI_UNPROVEN))

    def test_each_place_is_compared(self):
        # Each place drifting alone is found (the review's mutant m7 removed one comparison).
        header, section, coverage = self.places(self.script('agent-compatibility.py'))
        extra = ' zerus-63rru.999'
        for place, args in (('workflow header', (header + extra, section, coverage)),
                            ('docs section', (header, section + extra, coverage)),
                            ('row coverage', (header, section, coverage + extra))):
            with self.subTest(place=place):
                self.assertEqual(self.mismatches(*args, WAITING), [place])


class TeamsCodexPanel(unittest.TestCase):
    """Reading which Folder access panel Codex showed (synthetic folders)."""

    def pane(self, folder, columns=fixture.CODEX_PANEL_COLUMNS):
        rows = [folder[i:i + columns] for i in range(0, len(folder), columns)]
        return '\n'.join(['', '  Folder access', *('  ' + row for row in rows), '',
                          '  Note: You\u2019re in a subdirectory of a Git project. Trusting will apply to the',
                          '  repository root:', '  /tmp/example/work/proj', ''])

    def test_the_variant_is_what_the_pane_shows(self):
        short = '/tmp/hgs-tn-abcdefgh/work/zerus-team-' + str(uuid.UUID(int=1))
        long = '/srv/example/target/run/tmp/hgs-tn-abcdefgh/work/zerus-team-' + str(uuid.UUID(int=2))
        self.assertLessEqual(len(short), fixture.CODEX_PANEL_COLUMNS)
        self.assertGreater(len(long), fixture.CODEX_PANEL_COLUMNS)
        self.assertEqual(fixture.codex_panel_variant(self.pane(short), short), 'one-row')
        self.assertEqual(fixture.codex_panel_variant(self.pane(long), long), 'wrapped')
        # Another folder, a cut folder, or no panel title is no variant.
        self.assertIsNone(fixture.codex_panel_variant(self.pane(short), long))
        self.assertIsNone(fixture.codex_panel_variant(self.pane(long), short))
        cut = '\n'.join(['', '  Folder access', '  ' + long[:fixture.CODEX_PANEL_COLUMNS], '', '  Note: example'])
        self.assertIsNone(fixture.codex_panel_variant(cut, long))
        # The folder's rows follow each other: a blank row between its halves is not the panel.
        rest = long[fixture.CODEX_PANEL_COLUMNS:]
        gap = self.pane(long).replace('\n  ' + rest + '\n', '\n\n  ' + rest + '\n')
        self.assertNotEqual(gap, self.pane(long))
        self.assertIsNone(fixture.codex_panel_variant(gap, long))
        self.assertIsNone(fixture.codex_panel_variant(self.pane(short).replace('Folder access', 'Folder'), short))

    def test_a_hosted_root_in_tmp_fits_one_row_and_a_worktree_root_wraps(self):
        # mkdtemp adds eight characters; the service names the member worktree after its launch.
        member = 'work/zerus-team-' + str(uuid.uuid4())
        hosted = Path('/tmp') / (fixture.ROOT_PREFIX + 'x' * 8) / member
        self.assertLessEqual(len(str(hosted)), fixture.CODEX_PANEL_COLUMNS)
        local = Path(__file__).resolve().parents[1] / 'target/run/tmp' / (fixture.ROOT_PREFIX + 'x' * 8) / member
        self.assertGreater(len(str(local)), fixture.CODEX_PANEL_COLUMNS)
        self.assertTrue(fixture.hosted_runner({'GITHUB_ACTIONS': 'true', 'RUNNER_ENVIRONMENT': 'github-hosted'}))
        for env in ({}, {'GITHUB_ACTIONS': 'true'}, {'GITHUB_ACTIONS': 'true', 'RUNNER_ENVIRONMENT': 'self-hosted'}):
            self.assertFalse(fixture.hosted_runner(env), env)


class TeamsFixtureDiagnostics(unittest.TestCase):
    """The read-only evidence of a refused resume, on a synthetic private state."""

    def product_reader(self, entries, details, pages=None):
        from types import SimpleNamespace
        calls = []

        def json_(*argv, stdin=None, **kw):
            calls.append((argv, json.loads(stdin)))
            if argv[:2] == ('approval-log', 'list'):
                if pages is not None:
                    return pages[json.loads(stdin).get('before')]
                return dict(entries=entries, next=None, previous=None)
            if argv[:2] == ('approval-log', 'detail'):
                return details[json.loads(stdin)['id']]
            raise AssertionError(argv)
        import types
        reader = SimpleNamespace(json=json_, listed=fixture.TeamFixture.listed, JOURNAL_BOUND=fixture.TeamFixture.JOURNAL_BOUND)
        reader.journal = types.MethodType(fixture.TeamFixture.journal, reader)
        return reader, calls

    def test_the_journal_is_read_through_the_products_own_reader(self):
        # The reader's shapes come from their producer (src/approval_log.rs list and detail).
        producer = (Path(__file__).resolve().parents[1] / 'src/approval_log.rs').read_text()
        self.assertIn('"categories":saved_categories(row.get::<_,Option<String>>(7)?)', producer)
        self.assertIn('json!({"id":id,"question":saved["question"],"answers":saved["answers"],"status":status,"detail":detail,', producer)
        self.assertIn('"usage: hgs approval-log begin|finish|list|detail (JSON stdin)"', producer)
        question = {'source': 'team_resume', 'question_id': 'resume:run-1', 'run_id': 'run-1'}
        entries = [dict(id=1, at=1, host='h', session='member', provider='claude', prompt='', status='approved',
                        categories=['team_resume']),
                   dict(id=2, at=2, host='h', session='other', provider='claude', prompt='', status='approved',
                        categories=None)]
        details = {1: dict(id=1, question=question, answers=[], status='approved', detail='', scope='/work/repository'),
                   2: dict(id=2, question=dict(question, source='claude_tool_approval'), answers=[], status='approved',
                           detail='')}
        reader, calls = self.product_reader(entries, details)
        rows = fixture.TeamFixture.audit_rows(reader, 'member')
        self.assertEqual(rows[0]['scope'], '/work/repository')
        self.assertEqual([(r['id'], r['source'], r['question_id'], r['run'], r['categories']) for r in rows],
                         [(1, 'team_resume', 'resume:run-1', 'run-1', ['team_resume'])])
        self.assertEqual([c[0][:2] for c in calls], [('approval-log', 'list'), ('approval-log', 'detail'), ('approval-log', 'detail')])
        # Pages are followed by the product's own cursor.
        pages = {None: dict(entries=entries[1:], next=2, previous=None), 2: dict(entries=entries[:1], next=None, previous=2)}
        reader, _ = self.product_reader(None, details, pages)
        self.assertEqual([r['id'] for r in reader.journal()], [1, 2])
        # A reply without its entries list fails the reader.
        reader, _ = self.product_reader(None, details, {None: dict(rows=[])})
        with self.assertRaisesRegex(AssertionError, 'reply has no entries list'):
            reader.journal()

    def test_the_journal_reader_refuses_a_shape_it_does_not_know(self):
        entry = dict(id=1, at=1, host='h', session='member', provider='claude', prompt='', status='approved',
                     categories=['team_resume'])
        good = dict(id=1, question={'source': 'team_resume', 'question_id': 'q', 'run_id': 'r'}, answers=[],
                    status='approved', detail='')
        for name, entry_, detail in (('no question', entry, dict(good, question=None)),
                                     ('question without run', entry, dict(good, question={'source': 's', 'question_id': 'q'})),
                                     ('no answers', entry, {k: v for k, v in good.items() if k != 'answers'}),
                                     ('another id', entry, dict(good, id=2)),
                                     ('categories not a list', dict(entry, categories='team_resume'), good),
                                     ('no session', {k: v for k, v in entry.items() if k != 'session'}, good)):
            with self.subTest(case=name):
                reader, _ = self.product_reader([entry_], {1: detail})
                with self.assertRaisesRegex(AssertionError, 'shape the reader does not know'):
                    reader.journal()

    def test_codex_trust_problems_name_a_wider_or_extra_record(self):
        main, account, native = '/tmp/example/work/proj', '/tmp/example/cfg/accounts/zt-codex/config.toml', \
            '/tmp/example/native/.codex/config.toml'
        problems = codex_trust_problems
        self.assertEqual(problems({account: {main: 'trusted'}, native: {}}, {account}, main), [])
        for name, records in (('a parent', {account: {'/tmp/example/work': 'trusted'}, native: {}}),
                              ('the worktree', {account: {'/tmp/example/work/zerus-team-1': 'trusted'}, native: {}}),
                              ('an extra project', {account: {main: 'trusted', '/tmp/example': 'trusted'}, native: {}}),
                              ('another account', {account: {main: 'trusted'}, native: {main: 'trusted'}}),
                              ('not trusted', {account: {main: 'untrusted'}, native: {}}),
                              ('no record', {native: {}}),
                              ('unreadable', {account: {main: 'trusted'}, native: 'TOMLDecodeError'})):
            with self.subTest(case=name):
                self.assertNotEqual(problems(records, {account}, main), [])

    def test_readers_refuse_a_reply_without_the_keys_a_check_relies_on(self):
        from types import SimpleNamespace
        listed = fixture.TeamFixture.listed
        self.assertEqual(listed({'rows': []}, 'rows', 'team cards'), [])
        for reply in ({}, {'rows': None}, {'rows': {}}, {'items': []}, []):
            with self.subTest(reply=reply), self.assertRaisesRegex(AssertionError, 'reply has no rows list'):
                listed(reply, 'rows', 'team cards')

        def reader(rows):
            return SimpleNamespace(rows=lambda: rows)
        live = fixture.TeamFixture.live_runs
        control = dict(name='lead', launch_id='C', runtime_state='live', run_id='r0')
        self.assertEqual(live(reader([control, dict(name='m1', launch_id='L', runtime_state='stopped', run_id='r1')]), 'L', 'C'), [])
        self.assertEqual(live(reader([control, dict(name='m1', launch_id='L', runtime_state='live', run_id='r1')]), 'L', 'C'), ['m1'])
        with self.assertRaisesRegex(AssertionError, 'lacks its runtime state'):
            live(reader([control, dict(name='m1', launch_id='L', state='live', run_id='r1')]), 'L', 'C')
        with self.assertRaisesRegex(AssertionError, 'control launch C is not found live'):
            live(reader([dict(control, runtime_state='stopped')]), 'L', 'C')
        with self.assertRaisesRegex(AssertionError, 'control launch C is not found live'):
            live(reader([dict(name='lead', launchId='C', runtime_state='live', run_id='r0')]), 'L', 'C')
        for launch in (None, '', 7):
            with self.subTest(launch=launch), self.assertRaisesRegex(AssertionError, 'not a launch'):
                live(reader([control]), launch, 'C')
        with self.assertRaisesRegex(AssertionError, 'not a launch'):
            live(reader([control]), 'L', None)
        gone = SimpleNamespace(record=lambda member: dict(pid=2 ** 22 + 12345), start_ticks=fixture.TeamFixture.start_ticks)
        with self.assertRaisesRegex(AssertionError, 'not running when its identity is taken'):
            fixture.TeamFixture.process(gone, 'm1')

    def test_the_session_record_is_the_one_live_record_of_the_run(self):
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as base:
            state = Path(base)
            (state / 'sessions').mkdir()
            (state / 'archives').mkdir()
            (state / 'sessions/a.json').write_text(json.dumps(dict(run_id='r', pid=11, name='m1')))
            (state / 'archives/b.json').write_text(json.dumps(dict(run_id='r', pid=11, state='archived')))
            (state / 'sessions/receipt.json').write_text(json.dumps(dict(run_id='r', status='completed')))
            reader = SimpleNamespace(env={'HGS_STATE_DIR': str(state)}, inspect=lambda member: {'run_id': 'r'})
            self.assertEqual(fixture.TeamFixture.record(reader, 'm1')['name'], 'm1')
            (state / 'sessions/c.json').write_text(json.dumps(dict(run_id='r', pid=12)))
            with self.assertRaisesRegex(AssertionError, '2 live session records'):
                fixture.TeamFixture.record(reader, 'm1')
            reader.inspect = lambda member: {}
            with self.assertRaisesRegex(AssertionError, 'has no current run'):
                fixture.TeamFixture.record(reader, 'm1')

    def test_account_snapshots_name_what_changed_without_content(self):
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as base:
            home = Path(base)
            (home / '.claude.json').write_text(json.dumps({'projects': {}, 'numStartups': 1}))
            (home / 'keep.txt').write_text('same')
            (home / 'gone.txt').write_text('x')
            before = fixture.TeamFixture.account_snapshot(SimpleNamespace(), home)
            time.sleep(0.01)
            (home / '.claude.json').write_text(json.dumps({'projects': {'/tmp/example/work/proj': {
                'hasTrustDialogAccepted': True, 'allowedTools': []}}, 'numStartups': 2, 'secretish': 'value'}))
            (home / 'gone.txt').unlink()
            (home / 'new.txt').write_text('y')
            after = fixture.TeamFixture.account_snapshot(SimpleNamespace(), home)
        diff = fixture.TeamFixture.snapshot_diff(before, after)
        self.assertEqual((diff['added'], diff['removed']), (['new.txt'], ['gone.txt']))
        change = diff['changed']['.claude.json']
        self.assertEqual((change['keys_added'], change['keys_changed']), (['secretish'], ['numStartups', 'projects']))
        self.assertEqual(change['projects_added'], ['/tmp/example/work/proj'])
        self.assertEqual(change['project_keys'], {'/tmp/example/work/proj': dict(
            added=['allowedTools', 'hasTrustDialogAccepted'], removed=[], changed=[])})
        self.assertNotIn('keep.txt', diff['changed'])
        self.assertNotIn('value', json.dumps(after), 'names and hashes only, no content')

    def test_the_pinned_commands_of_a_delivered_prompt(self):
        attempt = str(uuid.UUID(int=7))
        prompt = ('[Zerus task Example project/T-abc]\nWritten by the person.\n\n'
                  f"Report this execution with `hgs task report-attempt 'Example project/T-abc' --attempt {attempt}` "
                  'instead of any unscoped report instruction above.')
        self.assertEqual(fixture.pinned(prompt, 'report-attempt'), ('Example project/T-abc', attempt))
        self.assertIsNone(fixture.pinned(prompt, 'begin-attempt'))
        plain = f'Acknowledge with `hgs task begin-attempt T-abc --attempt {attempt}` first.'
        self.assertEqual(fixture.pinned(plain, 'begin-attempt'), ('T-abc', attempt))
        self.assertIsNone(fixture.pinned(plain.replace(attempt, 'not-a-uuid'), 'begin-attempt'))

    def test_acknowledgement_follows_the_original_legacy_or_managed_pin(self):
        attempt = str(uuid.UUID(int=7))
        legacy = "[Zerus task start]\nAcknowledge this task with `hgs task start 'Example project/T-abc'` before working."
        managed = f"Acknowledge with `hgs task begin-attempt T-abc --attempt {attempt}`."
        self.assertEqual(fixture.acknowledgement(legacy), ('start', 'Example project/T-abc', ''))
        self.assertEqual(fixture.acknowledgement(legacy + managed), ('begin-attempt', 'T-abc', attempt))
        self.assertIsNone(fixture.acknowledgement(legacy.replace('[Zerus task start]', 'Mentioned in prose')))
        self.assertIsNone(fixture.acknowledgement('Report with `hgs task report T-abc`.'))

    def test_member_waits_for_the_original_tasks_product_delivery_outcome(self):
        # Synthetic reader control; native classes read every row from hgs itself.
        task = json.dumps(dict(task=dict(uuid='original-task')))
        other = json.dumps(dict(nodes=[dict(task=dict(uuid='another-task'))]))
        held = json.dumps(dict(nodes=[dict(task=dict(uuid='original-task'))]))
        with mock.patch.object(fixture.subprocess, 'check_output', side_effect=[task, other, held]) as read, \
                mock.patch.object(fixture.time, 'sleep'):
            self.assertEqual(fixture.wait_for_held_task('/fixture/hgs', 'Example/T-abc', 1), json.loads(held))
        self.assertEqual(read.call_args_list[0].args[0], ['/fixture/hgs', 'task', 'get', 'Example/T-abc', '--json'])
        self.assertEqual([c.args[0] for c in read.call_args_list[1:]], [['/fixture/hgs', 'team', 'current', '--json']] * 2)
        with mock.patch.object(fixture.subprocess, 'check_output', side_effect=[task, other]):
            with self.assertRaisesRegex(AssertionError, 'did not record the original task delivery'):
                fixture.wait_for_held_task('/fixture/hgs', 'Example/T-abc', 0)
        with mock.patch.object(fixture.subprocess, 'check_output', side_effect=[task, '{}']):
            with self.assertRaisesRegex(AssertionError, 'no nodes list'):
                fixture.wait_for_held_task('/fixture/hgs', 'Example/T-abc', 1)

    def test_worked_waits_for_the_products_empty_composer_after_stop(self):
        from types import SimpleNamespace
        # Synthetic reader control: Stop alone must not submit a quit key.
        fx = fixture.TeamFixture.__new__(fixture.TeamFixture)
        fx.sessions = {'m1': {'name': 'codex/example/member'}}
        view = dict(run_id='run', conversation_id='conversation', activity='idle', events=[dict(type='Stop', seq=2)])
        fx.inspect = mock.Mock(side_effect=[dict(events=[]), dict(view, events=[]), view, view])
        fx.send = mock.Mock()
        fx.requests_with = mock.Mock(return_value=[dict(prompt='message')])
        fx.hgs = mock.Mock(side_effect=[SimpleNamespace(returncode=0, stdout=json.dumps(dict(composer=dict(state=state))))
                                        for state in ('unknown', 'empty')])
        fx.evidence = SimpleNamespace(write=mock.Mock())
        def wait(label, predicate, *args):
            if 'answered' in label:
                self.assertIsNone(predicate(), 'no Stop yet')
                self.assertIsNone(predicate(), 'Stop precedes the native composer')
            return predicate()
        fx.wait = wait
        self.assertEqual(fx.worked('m1', 'message'), view)
        self.assertEqual(fx.hgs.call_count, 2)
        for call in fx.hgs.call_args_list:
            self.assertEqual(call.args, ('terminal', 'codex/example/member', '--json'))
            request = json.loads(call.kwargs['stdin'])
            self.assertEqual((request['action'], request['expected_run_id'], request['expected_conversation_id']),
                             ('composer', 'run', 'conversation'))
        fx.evidence.write.assert_called_once_with('before-native-exit-composer.json', dict(composer=dict(state='empty')))

    def test_handouts_name_the_members_a_node_went_to(self):
        from types import SimpleNamespace
        rows = [dict(kind='handout_intent', refs=['team:t', 'node:1', 'member:m1', 'effect:e']),
                dict(kind='handout_intent', refs=['team:t', 'node:2', 'member:m2', 'effect:f']),
                dict(kind='observe_task', refs=['team:t', 'node:1'])]
        reader = SimpleNamespace(team='t', listed=fixture.TeamFixture.listed, json=lambda *argv, **kw: dict(rows=rows))
        self.assertEqual(fixture.TeamFixture.handouts(reader, 1), ['m1'])
        self.assertEqual(fixture.TeamFixture.handouts(reader, 3), [])
        reader.json = lambda *argv, **kw: dict(items=rows)
        with self.assertRaisesRegex(AssertionError, 'reply has no rows list'):
            fixture.TeamFixture.handouts(reader, 1)

    def test_folder_watch_times_a_folder_that_goes_and_comes_back(self):
        with tempfile.TemporaryDirectory() as base:
            folder = Path(base) / 'member'
            folder.mkdir()
            watch = fixture.FolderWatch(folder)
            watch.start()
            time.sleep(0.5)
            folder.rmdir()
            time.sleep(0.5)
            folder.mkdir()
            time.sleep(0.5)
            result = watch.finish()
        self.assertIsNotNone(result['gone_at'])
        self.assertIsNotNone(result['back_at'])
        self.assertLess(result['started'], result['gone_at'])
        self.assertLess(result['gone_at'], result['back_at'])
        self.assertGreater(result['samples'], 3)

    def test_maintenance_reads_only_the_rows_and_receipts_of_that_checkout(self):
        import sqlite3
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as base:
            state = Path(base) / 'state'
            (state / 'worktree-maintenance').mkdir(parents=True)
            (state / 'worktree-cache').mkdir()
            member, other = '/tmp/example/work/zerus-team-1', '/tmp/example/work/zerus-team-2'
            db = sqlite3.connect(state / 'worktree-maintenance/queue.sqlite3')
            db.execute('CREATE TABLE checkouts (path TEXT PRIMARY KEY, common_dir TEXT, owner TEXT, next_check REAL, '
                       'status TEXT, detail TEXT)')
            db.execute('CREATE TABLE removals (path TEXT PRIMARY KEY, common_dir TEXT, branch TEXT, head TEXT, '
                       'fingerprint TEXT, request_id TEXT, reserved_at REAL, status TEXT, detail TEXT)')
            for path in (member, other):
                db.execute('INSERT INTO checkouts VALUES (?, ?, ?, 0, ?, ?)', (path, '/tmp/example/.git', '{}', 'pending', ''))
            db.execute('INSERT INTO removals VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?)',
                       (other, '/tmp/example/.git', 'b', 'h', 'f', 'r', 'pending', ''))
            db.commit()
            db.close()
            (state / 'worktree-maintenance/policy.json').write_text('{"enabled": true}')
            (state / 'worktree-cache/owned-a.json').write_text(json.dumps({'path': member}))
            (state / 'worktree-cache/owned-b.json').write_text(json.dumps({'path': other}))
            out = fixture.TeamFixture.maintenance(SimpleNamespace(env={'HGS_STATE_DIR': str(state)}), member)
        self.assertEqual([row['path'] for row in out['checkouts']], [member])
        self.assertEqual(out['removals'], [])
        self.assertEqual(out['policy_file'], {'enabled': True})
        self.assertIsNone(out['last_result'])
        self.assertEqual([r['file'] for r in out['owned_receipts']], ['owned-a.json'])


class TeamsFixtureProducers(unittest.TestCase):
    """Values another component writes come from that component (plan, 2026-11-12):
    the Kimi inspection stand-in carries only what src/accounts/kimi-read.mjs writes."""

    def test_kimi_inspection_stand_in_is_a_value_the_producer_writes(self):
        producer = (Path(__file__).resolve().parents[1] / 'src/accounts/kimi-read.mjs').read_text()
        base = re.search(r"const output = (\{.*?\});", producer)[1]
        self.assertEqual(base, "{identity:{},windows:[],source:'Kimi Code',status:'unavailable'}")
        self.assertEqual(fixture.KIMI_PRODUCER_BASE, dict(identity={}, windows=[], source='Kimi Code', status='unavailable'))
        identity = re.search(r"output\.identity=\{(.*?)\};", producer)[1]
        written = set(re.findall(r"(\w+):", identity))
        value = fixture.kimi_inspection()
        self.assertEqual(set(value), set(fixture.KIMI_PRODUCER_BASE), 'no key the producer does not write')
        self.assertEqual(set(value['identity']), written)
        self.assertIn("auth_method:'Kimi Code'", identity)
        self.assertEqual(value['identity']['auth_method'], 'Kimi Code')
        # The producer sets status ok only when it has windows; with none it stays unavailable.
        self.assertIn("if(output.windows.length)output.status='ok';", producer)
        self.assertEqual((value['windows'], value['status']), ([], 'unavailable'))


@unittest.skipUnless(os.environ.get('HGS_CLAUDE_TEST_BIN') and fixture.HGS, 'Set HGS_TEST_BIN and HGS_CLAUDE_TEST_BIN')
class NativeTeamsClaude(TeamsContract, unittest.TestCase):
    agent = 'claude'
    variable = 'HGS_CLAUDE_TEST_BIN'


@unittest.skipUnless(os.environ.get('HGS_CLAUDE_TEST_BIN') and fixture.HGS, 'Set HGS_TEST_BIN and HGS_CLAUDE_TEST_BIN')
class NativeTeamsTrustClaude(ClaudeTrust, TrustContract, unittest.TestCase):
    agent = 'claude'
    variable = 'HGS_CLAUDE_TEST_BIN'


@unittest.skipUnless(os.environ.get('HGS_CLAUDE_TEST_BIN') and fixture.HGS, 'Set HGS_TEST_BIN and HGS_CLAUDE_TEST_BIN')
class NativeTeamsResumeClaude(ResumeContract, unittest.TestCase):
    agent = 'claude'
    variable = 'HGS_CLAUDE_TEST_BIN'


@unittest.skipUnless(os.environ.get('HGS_CLAUDE_TEST_BIN') and fixture.HGS, 'Set HGS_TEST_BIN and HGS_CLAUDE_TEST_BIN')
class NativeTeamsResumeUnworkedClaude(UnworkedResumeContract, unittest.TestCase):
    """Selected since zerus-63rru.62: the card of the refused restore carries the command's reason."""
    agent = 'claude'
    variable = 'HGS_CLAUDE_TEST_BIN'


@unittest.skipUnless(os.environ.get('HGS_CLAUDE_TEST_BIN') and fixture.HGS, 'Set HGS_TEST_BIN and HGS_CLAUDE_TEST_BIN')
class NativeTeamsQuestionAfterTrustClaude(QuestionAfterTrustContract, unittest.TestCase):
    """Waiting (WAITING): not selected by the teams contract until zerus-63rru.73."""
    agent = 'claude'
    variable = 'HGS_CLAUDE_TEST_BIN'


@unittest.skipUnless(os.environ.get('HGS_CODEX_TEST_BIN') and fixture.HGS, 'Set HGS_TEST_BIN and HGS_CODEX_TEST_BIN')
class NativeTeamsResumeCodex(ResumeContract, unittest.TestCase):
    """Resume through a real archive publication whose metadata transaction rolls back."""
    agent = 'codex'
    variable = 'HGS_CODEX_TEST_BIN'
    # Native 0.169 buffers rapid characters plus Enter as a paste/newline. End
    # flushes that burst without changing the command; Enter then dispatches it.
    exit_keys = ('/quit', 'End', 'Enter')

    @contextmanager
    def during_exit(self, before):
        import fcntl
        import sqlite3
        import threading

        fx = self.fx
        state = Path(fx.env['HGS_STATE_DIR'])
        fx.hgs('__state', 'archive-catalog-worker')
        aux = state / 'archive-catalog-v2'
        catalog = state / 'archive-catalog-v1.sqlite3'
        original_run = before['run_id']
        quit_cooldown = threading.Event()
        stamp = aux / 'teams-attempt.json'
        self.assertFalse(stamp.exists(), 'the clean fixture has no previous Teams repair')

        def suppress_generic_worker():
            # Keep ordinary archive listing's five-second request cooldown
            # current. The Teams request has its own durable admission.
            pending = aux / 'fixture-attempt.json'
            pending.write_text(json.dumps({'at': time.time()}))
            pending.replace(aux / 'attempt.json')

        def sustain():
            while not quit_cooldown.wait(1):
                suppress_generic_worker()

        with (aux / 'worker.lock').open('r+b') as exclusion, closing(sqlite3.connect(catalog, timeout=5)) as db:
            fcntl.flock(exclusion, fcntl.LOCK_EX)
            quoted_run, = db.execute('SELECT quote(?)', (original_run,)).fetchone()
            db.execute(f"""CREATE TRIGGER fixture_lost_publication BEFORE INSERT ON archive_rows
                           WHEN NEW.run = {quoted_run}
                           BEGIN SELECT RAISE(ABORT, 'fixture lost publication'); END""")
            try:
                yield

                def original_archive():
                    for path in sorted((state / 'archives').glob('*.json'))[:10]:
                        raw = json.loads(path.read_text())
                        if raw['run_id'] == original_run:
                            return {key: raw[key] for key in (
                                'archive_id', 'run_id', 'session_id', 'launch_id', 'conversation_id',
                                'state', 'completion_source', 'completion_reason')}
                    return None

                raw = fx.wait('supervisor published the member archive', original_archive, fixture.BOUND['exit'])
                self.assertEqual(raw['completion_reason'], 'clean_exit')
                for key in ('run_id', 'session_id', 'launch_id', 'conversation_id'):
                    self.assertEqual(raw[key], before[key])
                fx.wait('the exited member is untracked', lambda: fx.inspect('m1').get('tracked') is False or None,
                        fixture.BOUND['exit'])
                rows = db.execute('SELECT id FROM archive_identities WHERE run=?', (original_run,)).fetchall()
                self.assertEqual(rows, [], 'the native archive exists and its metadata transaction rolled back')
                fx.evidence.write('catalog-gap.json', dict(native=raw, indexed=rows))
            finally:
                db.execute('DROP TRIGGER fixture_lost_publication')
                suppress_generic_worker()
                fcntl.flock(exclusion, fcntl.LOCK_UN)
            thread = threading.Thread(target=sustain, daemon=True)
            thread.start()
            try:
                fx.wait('Teams requested bounded archive repair', lambda: stamp.is_file() or None,
                        fixture.BOUND['doorbell'], 1)
                indexed = fx.wait('the repair indexed the original run', lambda: db.execute(
                    'SELECT id FROM archive_identities WHERE run=?', (original_run,)).fetchone(),
                    fixture.BOUND['exit'], 1)
                self.assertEqual(indexed[0], raw['archive_id'])
                fx.evidence.write('catalog-recovered.json', dict(repair=json.loads(stamp.read_text()), id=indexed[0]))
            finally:
                quit_cooldown.set()
                thread.join()


@unittest.skipUnless(os.environ.get('HGS_CODEX_TEST_BIN') and fixture.HGS, 'Set HGS_TEST_BIN and HGS_CODEX_TEST_BIN')
class NativeTeamsTrustCodex(CodexTrustContract, unittest.TestCase):
    agent = 'codex'
    variable = 'HGS_CODEX_TEST_BIN'


@unittest.skipUnless(os.environ.get('HGS_CODEX_TEST_BIN') and fixture.HGS, 'Set HGS_TEST_BIN and HGS_CODEX_TEST_BIN')
class NativeTeamsCodex(TeamsContract, unittest.TestCase):
    agent = 'codex'
    variable = 'HGS_CODEX_TEST_BIN'


# Set only by scripts/ci/kimi-native.py --teams, which itself refuses to run outside
# a GitHub-hosted runner.
HOSTED = os.environ.get('ZERUS_TEAMS_KIMI_HOSTED') == '1'


@unittest.skipUnless(HOSTED and os.environ.get('HGS_KIMI_TEST_BIN') and fixture.HGS,
                     'Kimi Teams classes run only in the hosted Kimi workflow (no local Kimi staging); a skip is no evidence')
class NativeTeamsKimi(TeamsContract, unittest.TestCase):
    agent = 'kimi'
    variable = 'HGS_KIMI_TEST_BIN'


if __name__ == '__main__':
    unittest.main()
