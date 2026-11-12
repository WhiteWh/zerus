"""The hgs DeepSeek bridge registers the agent hint as one system prompt section.

Node loads src/dsh/bridge.mjs with a stand-in context; no harness, host or model
runs. Native qualification against the official harness is separate.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import unittest

REPO = Path(__file__).resolve().parents[1]
NODE = shutil.which('node')
HINT = ('This agent runs in a Zerus session: the hgs CLI reads, messages, launches and stops other agent '
        'sessions on this and other computers. Run hgs guide before using it.')
SCRIPT = r'''
const { registerAgentHint, AGENT_HINT_SECTION } = await import(process.argv[1]);
const cases = JSON.parse(process.argv[2]);
const results = cases.map(({ hint, service, fail }) => {
  const sections = [], effects = [];
  const prompts = { section: s => {
    if (fail) throw new Error('duplicate section');
    sections.push(s); return () => sections.splice(sections.indexOf(s), 1);
  } };
  const ctx = { get: name => (name === 'systemPrompt' && service ? prompts : undefined),
                effect: run => { effects.push(run()); } };
  const owned = new Map([['main', {}]]);
  const registered = registerAgentHint(ctx, hint, owned);
  const describe = id => sections.map(section => ({
    ...section, text: section.text({ agent: { session: { id } } })
  })).filter(section => section.text);
  const shown = { main: describe('main'), ui: describe('ui'), unknown: describe('unknown') };
  effects.forEach(dispose => dispose());
  return { registered, shown, remaining: sections.length, name: AGENT_HINT_SECTION };
});
console.log(JSON.stringify(results));
'''


@unittest.skipUnless(NODE, 'node runs the DeepSeek bridge module')
class BridgeAgentHint(unittest.TestCase):
    def run_cases(self, cases):
        result = subprocess.run([NODE, '--input-type=module', '-e', SCRIPT, str(REPO / 'src/dsh/bridge.mjs'), json.dumps(cases)],
                                capture_output=True, text=True, timeout=30, env={'PATH': os.environ['PATH']})
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_the_hint_is_one_literal_section_for_owned_main_agents_only(self):
        (on,) = self.run_cases([{'hint': HINT, 'service': True}])
        self.assertTrue(on['registered'])
        self.assertEqual(on['shown']['main'], [{'name': 'hgs:agent-hint', 'order': 10300, 'text': HINT, 'interpolate': False}])
        self.assertEqual(on['shown']['ui'], [])
        self.assertEqual(on['shown']['unknown'], [])
        self.assertEqual(on['remaining'], 0)

    def test_an_opted_out_or_older_harness_gets_nothing(self):
        for case in ({'hint': None, 'service': True}, {'hint': '', 'service': True}, {'hint': HINT, 'service': False}):
            (result,) = self.run_cases([case])
            self.assertEqual((result['registered'], result['shown']['main']), (False, []), case)

    def test_registration_failure_is_fail_soft(self):
        (result,) = self.run_cases([{'hint': HINT, 'service': True, 'fail': True}])
        self.assertFalse(result['registered'])
        self.assertEqual(result['shown']['main'], [])
        self.assertEqual(result['remaining'], 0)

    def test_hgs_passes_the_same_sentence_as_its_hook(self):
        source = (REPO / 'src/state/agent_hint.rs').read_text()
        self.assertIn('pub(super) const HINT: &str = "' + HINT[:60], source.replace('\\\n', ''))


if __name__ == '__main__':
    unittest.main()
