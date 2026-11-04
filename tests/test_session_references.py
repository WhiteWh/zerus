"""Portable references resolve exact saved identities on a private tmux socket."""
import hashlib
import json
import subprocess
import unittest
import uuid
from urllib.parse import parse_qs, urlencode, urlsplit
import test_input as fixtures


class SessionReferences(unittest.TestCase):
    setUp = fixtures.InputTransport.setUp
    tearDown = fixtures.InputTransport.tearDown
    script = fixtures.InputTransport.script
    tmux = fixtures.InputTransport.tmux
    wait_for = fixtures.InputTransport.wait_for
    write_record = fixtures.InputTransport.write_record

    def cli(self, *args, success=True):
        result = subprocess.run([str(fixtures.HGS), *args], env=self.env, text=True, capture_output=True, timeout=15)
        if success:
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(result.stdout)
        self.assertNotEqual(result.returncode, 0)
        return result.stderr

    def reference(self, *args):
        return self.cli('reference', self.name, '--json', *args)

    def test_copy_inspect_and_renamed_session_keep_the_conversation(self):
        reference = self.reference('--run', self.run_id, '--conversation', self.conversation_id)
        uri = reference['uri']
        self.assertEqual(reference['host'], 'test')
        self.assertEqual(reference['conversation_id'], self.conversation_id)
        self.assertEqual(self.reference()['machine_id'], reference['machine_id'])
        self.assertEqual(self.cli('inspect', uri)['name'], self.name)
        original_owner = self.name
        raw = dict(conversation_id=self.conversation_id, reply_id='original-reply', attention_signature=None, at=1.0)
        marks = self.state / 'read-marks.json'
        marks.write_text(json.dumps(dict(version=1, sessions={original_owner: {self.conversation_id: raw}})))
        marks.chmod(0o600)
        self.record['journal_name'] = original_owner
        old_path = self.record_path
        old_name = self.name
        self.name = 'codex/renamed/new-tag'
        self.tmux('rename-session', '-t', '=' + old_name, self.name)
        self.record['name'] = self.name
        self.record_path = self.state / (hashlib.sha256(self.name.encode()).hexdigest() + '.json')
        self.write_record()
        self.record_path.chmod(0o600)
        old_path.unlink()
        resolved = self.cli('inspect', uri)
        self.assertEqual(resolved['name'], self.name)
        self.assertEqual(resolved['read'], raw)
        self.assertTrue(resolved['read_observation']['complete'])
        self.assertEqual(resolved['session_reference']['original_name'], old_name)

    def test_clear_and_wrong_computer_never_silently_retarget(self):
        reference = self.reference()
        fields = {key: value[0] for key, value in parse_qs(urlsplit(reference['uri']).query).items()}
        fields['machine'] = str(uuid.uuid4())
        self.cli('inspect', 'zerus://session?' + urlencode(fields), success=False)
        self.record['conversation_id'] = str(uuid.uuid4())
        self.write_record()
        self.assertIn('no longer bound', self.cli('inspect', reference['uri'], success=False))
        self.assertIn('changed', self.cli('reference', self.name, '--json', '--conversation', self.conversation_id, success=False))

    def test_archive_and_child_are_explicit(self):
        child = 'external:codex:fixture-child'
        self.record['subagents'] = {child: {'name': 'Fixture review', 'state': 'finished'}}
        self.write_record()
        reference = self.reference('--agent', child)
        self.assertEqual(self.cli('inspect', reference['uri'])['agent_id'], child)
        archive = str(uuid.uuid4())
        directory = self.state / 'archives'
        directory.mkdir()
        archived = {**self.record, 'archive_id': archive, 'completion_reason': 'user_archived'}
        (directory / (archive + '.json')).write_text(json.dumps(archived))
        uri = self.reference('--archive', archive)['uri']
        self.record['conversation_id'] = str(uuid.uuid4())
        self.write_record()
        resolved = self.cli('inspect', uri)
        self.assertNotIn('read_observation', resolved)
        self.assertEqual(resolved['archive_id'], archive)
        self.assertEqual(resolved['conversation_id'], self.conversation_id)

    def test_ambiguous_and_mutating_uses_fail_closed(self):
        uri = self.reference()['uri']
        other = {**self.record, 'name': 'codex/project/duplicate'}
        path = self.state / (hashlib.sha256(other['name'].encode()).hexdigest() + '.json')
        path.write_text(json.dumps(other))
        self.assertIn('Ambiguous', self.cli('inspect', uri, success=False))
        self.assertIn('read-only', self.cli('send', uri, '--json', success=False))
        self.cli('inspect', uri + '&name=other', success=False)

    def test_remote_copy_and_inspection_use_one_verified_hop(self):
        self.script('ssh', """#!/usr/bin/env python3
import json, os, pathlib, sys
pathlib.Path(os.environ['INPUT_FIXTURE'], 'ssh-args').write_text(json.dumps(sys.argv[1:]))
os.execv('/bin/sh', ['sh', '-c', sys.argv[-1]])
""")
        reference = self.cli('@remote', 'reference', self.name, '--json')
        uri = reference['uri']
        raw = dict(conversation_id=self.conversation_id, reply_id='host-reply', attention_signature=None, at=1.0)
        marks = self.state / 'read-marks.json'
        marks.write_text(json.dumps(dict(version=1, sessions={self.name: {self.conversation_id: raw}})))
        marks.chmod(0o600)
        self.record_path.chmod(0o600)
        remote = self.cli('@remote', 'inspect', uri)
        self.assertEqual(remote['conversation_id'], self.conversation_id)
        self.assertEqual(remote['read'], raw)
        self.assertTrue(remote['read_observation']['complete'])
        args = json.loads((self.root / 'ssh-args').read_text())
        self.assertIn('BatchMode=yes', args)
        self.assertTrue(args[-1].endswith(' --local'))
        fields = {key: value[0] for key, value in parse_qs(urlsplit(uri).query).items()}
        fields['machine'] = str(uuid.uuid4()); fields['host'] = 'remote'
        wrong = 'zerus://session?' + urlencode(fields)
        self.assertIn('selected SSH alias', self.cli('inspect', wrong, success=False))
        self.assertEqual(json.loads((self.root / 'ssh-args').read_text())[-1].count(' --local'), 1)
