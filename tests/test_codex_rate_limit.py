#!/usr/bin/env python3
"""Screenshot-derived Codex rate-limit picker; private raw terminal, never owner input."""
import os
import signal
import unittest
import uuid

import test_input as fixtures
from test_questions import Questions

FAKE = fixtures.SIGNAL_SAFE_READ + r'''
import os,pathlib,signal,sys,termios,tty
root=pathlib.Path(os.environ['INPUT_FIXTURE']);fd=sys.stdin.fileno();original=termios.tcgetattr(fd);tty.setraw(fd)
os.write(1,b'\x1b[?2004h');selected=0;buf=b''
labels=['Switch to gpt-6-luna','Keep current model','Keep current model (never show again)']
def render(*args):
 global selected
 if (root/'selection').exists():selected=int((root/'selection').read_text());(root/'selection').unlink()
 lines=['  Fixture turn complete.','','  Approaching rate limits','  Switch to gpt-6-luna for lower credit usage?','']
 desc=['Fast and affordable model for easier tasks.','','Hide future rate limit reminders about switching models']
 for i in range(3):lines += [('› ' if selected==i else '  ')+str(i+1)+'. '+labels[i]+('  '+desc[i] if desc[i] else '')]
 lines += ['','  enter select · esc back']
 screen='\r\n'.join(lines)
 if (root/'future').exists():screen=screen.replace('gpt-6-luna','future-model')
 if (root/'partial').exists():screen=screen.replace('  enter select · esc back','')
 if (root/'quoted').exists():screen+='\r\n› Ask Codex to do anything\r\n  ? for shortcuts'
 os.write(1,b'\x1b[2J\x1b[H'+screen.encode())
signal.signal(signal.SIGUSR1,render);render();(root/'agent-pid').write_text(str(os.getpid()))
try:
 while True:
  value=read_input(fd)
  if not value:break
  with (root/'received').open('ab') as f:f.write(value)
  buf+=value
  while buf:
   if buf.startswith((b'\x1b[A',b'\x1bOA')):selected=(selected-1)%3;buf=buf[3:];render()
   elif buf.startswith((b'\x1b[B',b'\x1bOB')):selected=(selected+1)%3;buf=buf[3:];render()
   elif buf.startswith(b'\r'):
    buf=buf[1:];(root/'submitted').write_text(labels[selected])
    if not (root/'hold-dialog').exists():os.write(1,'\x1b[2J\x1b[H› Ask Codex to do anything\r\n  ? for shortcuts'.encode())
   elif buf[0]==27 and len(buf)<3:break
   else:buf=buf[1:]
finally:termios.tcsetattr(fd,termios.TCSANOW,original)
'''

class CodexRateLimit(unittest.TestCase):
    script = fixtures.InputTransport.script
    tmux = fixtures.InputTransport.tmux
    wait_for = fixtures.InputTransport.wait_for
    received = fixtures.InputTransport.received
    write_record = fixtures.InputTransport.write_record
    tearDown = fixtures.InputTransport.tearDown
    inspect = Questions.inspect
    answer = Questions.answer

    def setUp(self):
        original=fixtures.FAKE_AGENT
        try:
            fixtures.FAKE_AGENT=FAKE
            fixtures.InputTransport.setUp(self)
        finally:
            fixtures.FAKE_AGENT=original
        self.record.update(run_identity_version=1,supervisor={},launch_dir='/work/example')
        self.write_record()
        self.card=self.inspect()['pending_questions'][0]

    def payload(self,choice='keep_model',**updates):
        p=dict(request_id=str(uuid.uuid4()),expected_run_id=self.run_id,
               expected_conversation_id=self.conversation_id,question_id=self.card['question_id'],
               expected_question_hash=self.card['question_hash'],
               answers=[dict(question_id='codex_rate_limit',selected_option_ids=[choice],text='')])
        p.update(updates)
        return p

    def redraw(self,file,value,expected):
        (self.root/file).write_text(value)
        os.kill(self.record['pid'],signal.SIGUSR1)
        self.wait_for(lambda: expected in self.tmux('capture-pane','-p','-t',self.pane))

    def test_idle_native_stop_becomes_attention_without_autoanswer(self):
        state=self.inspect()
        self.assertEqual(state['phase'],'input')
        self.assertEqual(state['attention_id'],self.card['question_id'])
        self.assertEqual(self.card['source'],'codex_rate_limit')
        self.assertTrue(self.card['can_answer'])
        self.assertEqual(self.card['answer_transport'],'codex_tui')
        self.assertEqual([o['label'] for o in self.card['questions'][0]['options']],
                         ['Switch to gpt-6-luna','Keep current model','Keep current model (never show again)'])
        self.assertIn('rate-limit model choice',fixtures.InputTransport.send(self,fixtures.InputTransport.payload(self),success=False))
        self.assertEqual(self.received(),b'')

    def test_all_three_explicit_choices_use_guarded_single_enter(self):
        for choice,label in [('switch_model','Switch to gpt-6-luna'),('keep_model','Keep current model'),
                             ('keep_model_never','Keep current model (never show again)')]:
            with self.subTest(choice=choice):
                self.redraw('selection','0','› 1.')
                self.card=self.inspect()['pending_questions'][0]
                before=self.received();request=self.payload(choice)
                receipt=self.answer(request)
                self.assertEqual(receipt['status'],'answered')
                self.assertEqual((self.root/'submitted').read_text(),label)
                delivered=self.received()[len(before):];self.assertEqual(delivered.count(b'\r'),1)
                self.assertEqual(self.answer(request),receipt)
                self.assertEqual(self.received()[len(before):],delivered)
                self.assertEqual(self.inspect()['pending_questions'],[])

    def test_highlight_change_keeps_identity_and_navigates_from_current_row(self):
        original=self.card['question_hash'];self.redraw('selection','2','› 3.')
        self.assertEqual(self.inspect()['pending_questions'][0]['question_hash'],original)
        self.answer(self.payload('keep_model'))
        self.assertEqual((self.root/'submitted').read_text(),'Keep current model')
        self.assertEqual(self.received().count(b'\r'),1)

    def test_future_model_is_terminal_only_and_stale_answer_sends_no_keys(self):
        request=self.payload();self.redraw('future','1','future-model')
        state=self.inspect();self.assertEqual(state['phase'],'input')
        self.assertFalse(state['pending_questions'][0]['can_answer'])
        self.assertIn('Open Terminal',state['pending_questions'][0]['answer_unavailable_reason'])
        self.answer(request,success=False);self.assertEqual(self.received(),b'')

    def test_clipped_panel_is_attention_with_terminal_hint(self):
        self.redraw('partial','1','Approaching rate limits')
        state=self.inspect();self.assertEqual(state['phase'],'input')
        self.assertFalse(state['pending_questions'][0]['can_answer'])
        self.assertIn('Terminal',state['pending_questions'][0]['answer_unavailable_reason'])
        self.assertEqual(self.received(),b'')

    def test_conversation_guard_and_invalid_option_send_nothing(self):
        self.answer(self.payload(expected_conversation_id=str(uuid.uuid4())),success=False)
        self.answer(self.payload('not-an-option'),success=False)
        self.assertEqual(self.received(),b'')

    def test_quoted_panel_above_composer_is_not_a_question(self):
        self.redraw('quoted','1','? for shortcuts')
        self.assertEqual(self.inspect()['pending_questions'],[])
        self.assertEqual(self.received(),b'')

    def test_held_panel_delivery_is_uncertain_and_not_replayed(self):
        (self.root/'hold-dialog').touch();request=self.payload()
        self.assertIn('uncertain',self.answer(request,success=False))
        before=self.received();self.assertEqual(before.count(b'\r'),1)
        self.assertIn('uncertain',self.answer(request,success=False));self.assertEqual(self.received(),before)

# Official native Codex, loopback Responses only. The usage headers ask Codex
# itself to draw its real lower-credit chooser after a completed fixture turn.
import http.server
import json
import threading
import test_clear_context_native as native
import test_codex_update
import test_external_agents_native

class RateLimitResponse(test_external_agents_native.Response):
    def send_header(self,key,value):
        super().send_header(key,value)
        if key=='Content-Type':
            super().send_header('x-codex-primary-used-percent','97')
            super().send_header('x-codex-primary-window-minutes','300')
            super().send_header('x-codex-secondary-used-percent','97')
            super().send_header('x-codex-secondary-window-minutes','10080')

@unittest.skipUnless(os.environ.get('HGS_CODEX_TEST_BIN'),'Set HGS_CODEX_TEST_BIN for isolated real Codex rate-limit choices')
class NativeCodexRateLimit(unittest.TestCase):
    script=fixtures.InputTransport.script
    tmux=fixtures.InputTransport.tmux
    write_record=fixtures.InputTransport.write_record
    tearDown=fixtures.InputTransport.tearDown
    wait_for=test_codex_update.NativeCodexUpdate.wait_for
    fail=native.NativeCodexClear.fail
    inspect=Questions.inspect
    answer=Questions.answer
    payload=CodexRateLimit.payload

    def setUp(self):
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),RateLimitResponse)
        self.addCleanup(server.server_close);self.addCleanup(server.shutdown)
        threading.Thread(target=server.serve_forever,daemon=True).start()
        self.provider_url=f'http://127.0.0.1:{server.server_port}/v1'
        native.NativeCodexClear.setUp(self)
        self.tmux('send-keys','-t',self.pane,'-l','Finish this isolated fixture turn.')
        self.wait_for(lambda: 'Finish this isolated fixture turn.' in self.tmux('capture-pane','-p','-t',self.pane))
        import time
        time.sleep(.5)  # Native Codex's paste-burst guard must expire before Enter.
        self.tmux('send-keys','-t',self.pane,'Enter')
        self.wait_for(lambda: 'Approaching rate limits' in self.tmux('capture-pane','-p','-t',self.pane))
        self.screen=self.tmux('capture-pane','-p','-t',self.pane)
        # Retain only synthetic, credential-free native evidence when requested.
        if os.environ.get('ZERUS_NATIVE_RATE_SCREENS'):
            from pathlib import Path
            p=Path(os.environ['ZERUS_NATIVE_RATE_SCREENS']);p.mkdir(parents=True,exist_ok=True)
            (p/(os.environ.get('HGS_CODEX_TEST_SIZE','default')+'-'+os.environ.get('HGS_CODEX_TEST_ANIMATIONS','true')+'.txt')).write_text(self.screen)
        self.record.update(run_identity_version=1,supervisor={},phase='idle',activity='idle',active_tools={})
        self.write_record()
        self.card=next(q for q in self.inspect()['pending_questions'] if q['source']=='codex_rate_limit')

    def choose(self,choice):
        state=self.inspect();self.assertEqual(state['phase'],'input')
        self.assertTrue(self.card['can_answer'],self.screen)
        request=self.payload(choice)
        receipt=self.answer(request)
        self.assertEqual(receipt['status'],'answered')
        self.assertEqual(self.answer(request),receipt)
        self.assertEqual(self.inspect()['pending_questions'],[])

    def test_native_keep_choice_resolves_attention_without_switching(self):
        self.choose('keep_model')
        screen=self.tmux('capture-pane','-p','-t',self.pane)
        self.assertIn('GPT-6.1-Sol',screen)

    def test_native_switch_choice_changes_only_the_fixture_model(self):
        self.choose('switch_model')
        self.wait_for(lambda: 'GPT-6-Luna' in self.tmux('capture-pane','-p','-t',self.pane))

    def test_native_never_show_choice_resolves_attention(self):
        self.choose('keep_model_never')
