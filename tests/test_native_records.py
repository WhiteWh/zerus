#!/usr/bin/env python3
"""The JSON Lines reader the native agent suites poll transcripts with.

It lives outside those suites: without a native executable every one of their
tests must skip, which the compatibility gate checks."""
import json
import tempfile
import unittest
from pathlib import Path

from test_claude_questions_native import failure_evidence, records
from types import SimpleNamespace


class Records(unittest.TestCase):
    def test_failure_evidence_retains_native_tool_errors_and_bounds_output(self):
        result=dict(type='tool_result',tool_use_id='toolu_fixture',is_error=True,content='shell error '+('x'*10000))
        request=dict(messages=[dict(content='prompt'),dict(content=[result])])
        for name in ('native_requests','requests'):
            fixture=SimpleNamespace(root=Path('/fixture'),**{name:[request]*20})
            evidence=json.loads(failure_evidence(fixture))
            self.assertEqual(evidence['work'],'/fixture/work')
            self.assertEqual(len(evidence['tool_results']),1)
            self.assertTrue(evidence['tool_results'][0]['is_error'])
            self.assertTrue(evidence['tool_results'][0]['content'].startswith('shell error '))
            self.assertEqual(len(evidence['tool_results'][0]['content']),4096)

    def test_only_complete_records_are_read_and_malformed_ones_still_fail(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'transcript.jsonl'
            first = json.dumps(dict(type='user', text='a b\x85c'), ensure_ascii=False).encode() + b'\n'
            partial = json.dumps(dict(type='assistant', text='Привет'), ensure_ascii=False).encode()
            # An append cut inside a multibyte character waits for the next read.
            path.write_bytes(first + partial[:partial.index('Привет'.encode()) + 3])
            self.assertEqual(records(path), [dict(type='user', text='a b\x85c')])
            path.write_bytes(first + partial + b'\n')
            self.assertEqual([row['type'] for row in records(path)], ['user', 'assistant'])
            path.write_bytes(first + b'{"type":\n' + partial)
            with self.assertRaises(json.JSONDecodeError):
                records(path)


if __name__ == '__main__':
    unittest.main()
