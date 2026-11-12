"""Indexed archive pages on private synthetic records; no agent or live mark is touched."""
import json
import os
import shlex
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import unittest
import uuid

REPO=Path(__file__).resolve().parents[1]
HGS=Path(os.environ.get('HGS_TEST_BIN',REPO/'target/debug/hgs')).resolve()

class ArchivePages(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='zerus-archive-pages-');self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name);self.state=self.root/'state';self.archives=self.state/'archives';self.archives.mkdir(parents=True)
        socket=self.root/'tmux-private';socket.mkdir(mode=0o700)
        self.env={k:v for k,v in os.environ.items() if not k.startswith('HGS_') and k not in ('TMUX','TMUX_PANE','TMUX_TMPDIR')}
        self.env.update(HOME=str(self.root),HGS_STATE_DIR=str(self.state),HGS_CONFIG_DIR=str(self.root/'config'),HGS_SELF='archive-fixture',HGS_PEERS='',TMUX_TMPDIR=str(socket))
    def record(self,index,at=100,**extra):
        ident=str(uuid.UUID(int=index+1));value=dict(version=1,name=f'codex/example/archived-{index}',agent='codex',run_id=f'run-{index}',conversation_id=f'conversation-{index}',state='archived',archive_id=ident,archived_at=at,created=1,cwd='/synthetic/project',**extra)
        path=self.archives/(ident+'.json');temp=path.with_suffix('.tmp');temp.write_text(json.dumps(value),encoding='utf-8');temp.replace(path)
        return value
    def run_query(self,query=None,fail=False,bounded=False):
        r=subprocess.run([str(HGS),'archives','--json']+(['--bounded'] if bounded else []),input=json.dumps(query or {}),capture_output=True,text=True,env=self.env,timeout=30)
        if fail:self.assertNotEqual(r.returncode,0,r.stdout);return r
        self.assertEqual(r.returncode,0,r.stderr);return json.loads(r.stdout)
    def test_thousands_bounded_pages_ties_and_complete_no_duplicates(self):
        for i in range(2043):self.record(i,100+i//30)
        first=self.run_query({'limit':50});self.assertEqual(first['total'],2043);self.assertEqual(len(first['sessions']),50)
        index=self.state/'archive-catalog-v1.sqlite3';before=index.stat().st_mtime_ns
        got=[];page=first
        while True:
            self.assertLessEqual(len(page['sessions']),50);self.assertLess(len(json.dumps(page)),1048576)
            got += [(v['archived_at'],v['archive_id']) for v in page['sessions']]
            if not page['next_cursor']:break
            page=self.run_query({'limit':50,'cursor':page['next_cursor']})
        self.assertEqual(len(got),2043);self.assertEqual(len(set(got)),2043);self.assertEqual(got,sorted(got,reverse=True))
        self.assertEqual(index.stat().st_mtime_ns,before,'Warm queries must not rebuild or rewrite the metadata index')
    def test_exclusive_upper_bound_search_literal_unicode_and_cursor_binding(self):
        self.record(0,100);self.record(1,101);self.record(2,102,title='Русский %_needle')
        page=self.run_query({'from':101,'until':102,'limit':1});self.assertEqual([r['run_id'] for r in page['sessions']],['run-1'])
        matched=self.run_query({'query':'РУССКИЙ %_'});self.assertEqual(matched['total'],1);self.assertEqual(matched['sessions'][0]['title'],'Русский %_needle')
        first=self.run_query({'limit':1})
        for change in ({'from':1},{'until':103},{'query':'other'}):self.run_query(dict(limit=1,cursor=first['next_cursor'],**change),True)
        for invalid in ({'from':3,'until':3},{'from':-1},{'until':253402300801},{'limit':51},{'limit':True},{'cursor':'bogus'},{'query':'a\nb'},{'offset':10000}):self.run_query(invalid,True)
    def test_long_unicode_query_keeps_cursor_bounded(self):
        needle='🧭'*256
        self.record(0,title=needle);self.record(1,title=needle)
        page=self.run_query({'query':needle,'limit':1});self.assertTrue(page['next_cursor']);self.assertLessEqual(len(page['next_cursor']),1024)
        self.assertEqual(len(self.run_query({'query':needle,'limit':1,'cursor':page['next_cursor']})['sessions']),1)
    def test_keyset_survives_insert_delete_and_rename(self):
        for i in range(8):self.record(i,100+i)
        first=self.run_query({'limit':3});seen={r['archive_id'] for r in first['sessions']}
        self.record(9,1000)
        (self.archives/(str(uuid.UUID(int=4))+'.json')).unlink()
        self.record(2,102,title='new title')
        rest=self.run_query({'limit':50,'cursor':first['next_cursor']})
        self.assertFalse(seen & {r['archive_id'] for r in rest['sessions']});self.assertNotIn('run-9',[r['run_id'] for r in rest['sessions']])
        self.assertEqual([r['run_id'] for r in rest['sessions']],['run-4','run-2','run-1','run-0'])
        self.assertEqual(self.run_query({'query':'new title'})['total'],1)
        self.assertEqual(self.run_query()['total'],8)
    def test_restore_is_offered_only_for_a_saved_conversation(self):
        # zerus-wyrqg: a Claude or Codex session that exited before its first
        # message reported a conversation ID but wrote no transcript. Its
        # archive does not offer Restore and says why; a saved one does.
        launch=self.root/'project';launch.mkdir()
        transcripts=self.root/'transcripts';transcripts.mkdir()
        (transcripts/'empty.jsonl').write_text('');(transcripts/'saved.jsonl').write_text('{}\n')
        cases={}
        for index,(agent,kind) in enumerate((a,k) for a in ('claude','codex') for k in ('missing','empty','saved')):
            ident=str(uuid.uuid4())
            value=dict(version=1,name=f'{agent}/example/{kind}',agent=agent,run_id=f'run-{index}',conversation_id=f'conversation-{index}',
                       state='archived',archive_id=ident,archived_at=100+index,created=1,cwd=str(launch),launch_dir=str(launch),
                       base=[agent],transcript=str(transcripts/f'{kind}.jsonl'))
            (self.archives/(ident+'.json')).write_text(json.dumps(value),encoding='utf-8')
            cases[ident]=(agent,kind)
        rows={row['archive_id']:row for row in self.run_query({'limit':50})['sessions']}
        for ident,(agent,kind) in cases.items():
            with self.subTest(agent=agent,kind=kind):
                row=rows[ident]
                if kind=='saved':
                    self.assertIn('restore',row['allowed_actions'])
                else:
                    self.assertNotIn('restore',row['allowed_actions'])
                    self.assertIn('ended before its first message',row['action_reasons']['restore'])
    def test_cursor_rejects_other_catalog_and_database_rebuild(self):
        self.record(0);self.record(1);first=self.run_query({'limit':1})
        (self.state/'archive-catalog-v1.sqlite3').unlink()
        self.run_query({'cursor':first['next_cursor']},True)
    def test_live_poll_excludes_archives_without_opening_their_journals(self):
        for i in range(150):self.record(i,100+i)
        first=self.run_query({'limit':1});self.assertEqual(first['total'],150)
        result=subprocess.run([str(HGS),'ls','--json','--local','--no-archives'],env=self.env,capture_output=True,text=True,timeout=20)
        self.assertEqual(result.returncode,0,result.stderr);snapshot=json.loads(result.stdout)
        self.assertTrue(snapshot['archive_paging']);self.assertEqual(snapshot['sessions'],[])
    def test_archived_preferences_follow_the_original_journal_and_current_store(self):
        owner='session:00000000-0000-4000-8000-000000000050'
        row=self.record(0,journal_name=owner)
        preference=dict(conversation_id=row['conversation_id'],muted=True,revision=3,
                        suppressed=dict(reply_id=None,attention_signature=None,optional_question_ids=[],main_attention=None,child_attention={}))
        store=self.state/'notifications.json'
        store.write_text(json.dumps(dict(version=1,sessions={owner:{row['conversation_id']:preference}})))
        for bounded in (False,True):
            result=self.run_query(bounded=bounded)['sessions'][0]
            self.assertEqual(result.get('notification_preferences'),preference)
            self.assertEqual(result['conversation_id'],row['conversation_id'])
        # A warm metadata index must not retain an earlier preference revision.
        preference.update(muted=False,revision=4)
        store.write_text(json.dumps(dict(version=1,sessions={owner:{row['conversation_id']:preference}})))
        self.assertEqual(self.run_query()['sessions'][0].get('notification_preferences'),preference)
        store.write_text('{broken')
        result=self.run_query()['sessions'][0]
        self.assertIn('notification_preferences',result)
        self.assertIsNone(result['notification_preferences'],'Unreadable preferences remain unknown')
    def test_a_valid_large_preference_fits_an_archive_page_and_the_response_bound(self):
        row=self.record(0)
        children={f'child-{i}':dict(phase='input',request_id='q'*1200,started_at=None) for i in range(220)}
        preference=dict(conversation_id=row['conversation_id'],muted=True,revision=1,
                        suppressed=dict(reply_id=None,attention_signature=None,optional_question_ids=[],main_attention=None,child_attention=children))
        self.assertGreater(len(json.dumps(preference)),250000)
        (self.state/'notifications.json').write_text(json.dumps(dict(version=1,sessions={row['name']:{row['conversation_id']:preference}})))
        page=self.run_query(bounded=True)
        self.assertEqual(page['sessions'][0].get('notification_preferences'),preference)
        self.assertLess(len(json.dumps(page)),1048576)
    def test_non_regular_archive_entries_never_block_catalog(self):
        self.record(0)
        if hasattr(os,'mkfifo'):os.mkfifo(self.archives/(str(uuid.UUID(int=2))+'.json'))
        self.assertEqual(self.run_query()['total'],1)
    def test_exact_lookup_needs_no_catalog_or_transcript_and_rejects_wrong_identity(self):
        row=self.record(0)
        def lookup(value):
            return subprocess.run([str(HGS),'archives','lookup','--json'],input=json.dumps(value),capture_output=True,text=True,env=self.env,timeout=5)
        value={key:row[key] for key in ('name','archive_id')}
        result=lookup(value);self.assertEqual(result.returncode,0,result.stderr)
        self.assertEqual(json.loads(result.stdout),dict(version=1,**value,run_id=row['run_id'],conversation_id=row['conversation_id']))
        self.assertFalse((self.state/'archive-catalog-v1.sqlite3').exists())
        for bad in ({**value,'name':'codex/example/other'},{**value,'archive_id':str(uuid.UUID(int=99))},{**value,'extra':True},{'name':value['name']},{**value,'name':'@peer'}):
            self.assertNotEqual(lookup(bad).returncode,0,bad)
        (self.archives/(row['archive_id']+'.json')).unlink()
        if hasattr(os,'mkfifo'):
            os.mkfifo(self.archives/(row['archive_id']+'.json'))
            self.assertNotEqual(lookup(value).returncode,0)
    def test_bidirectional_keysets_reach_all_rows_with_only_current_window_anchors(self):
        for i in range(257):self.record(i,100+i//4)
        expected=sorted([(100+i//4,str(uuid.UUID(int=i+1))) for i in range(257)],reverse=True)
        page=self.run_query();position=0
        while True:
            self.assertEqual(page['position'],position)
            self.assertEqual([(r['archived_at'],r['archive_id']) for r in page['sessions']],expected[position:position+50])
            self.assertLessEqual(len(page['sessions']),50)
            self.assertEqual(bool(page['previous_cursor']),position>0)
            if not page['next_cursor']:break
            page=self.run_query({'cursor':page['next_cursor']});position+=50
        self.assertEqual(position,250)
        while page['previous_cursor']:
            page=self.run_query({'cursor':page['previous_cursor']});position=max(0,position-50)
            self.assertEqual(page['position'],position)
            self.assertEqual([(r['archived_at'],r['archive_id']) for r in page['sessions']],expected[position:position+50])
        self.assertEqual(position,0)
        next_page=self.run_query({'cursor':page['next_cursor']})
        for change in ({'query':'changed'},{'from':1},{'until':200}):self.run_query(dict(cursor=next_page['previous_cursor'],**change),True)

    # zerus-080n8: bounded pages answer only requests that opt in with --bounded.
    EXACT_KEYS={'version','sessions','next_cursor','previous_cursor','position','total','from','until','query'}
    def test_bounded_pages_are_opt_in_and_the_exact_page_keeps_its_payload(self):
        for i in range(60):self.record(i,100+i)
        exact=self.run_query({'limit':50})
        self.assertEqual(set(exact),self.EXACT_KEYS)
        bounded=self.run_query({'limit':50},bounded=True)
        self.assertEqual(set(bounded),self.EXACT_KEYS|{'total_capped','position_capped','partial','searched'})
        self.assertEqual((bounded['total'],bounded['total_capped'],bounded['partial'],bounded['position'],bounded['position_capped']),(60,False,False,0,False))
        self.assertEqual([r['archive_id'] for r in bounded['sessions']],[r['archive_id'] for r in exact['sessions']])
        self.assertEqual(bounded['searched'],51)
        rest=self.run_query({'limit':50,'cursor':bounded['next_cursor']},bounded=True)
        self.assertEqual((len(rest['sessions']),rest['position'],rest['next_cursor'],rest['partial']),(10,50,'',False))
        # The exact reader also continues a bounded page's cursor: it is the same (at, id) keyset.
        self.assertEqual(len(self.run_query({'limit':50,'cursor':bounded['next_cursor']})['sessions']),10)
        for args in (['archives','lookup','--json','--bounded'],['archives','--bounded','--json'],['archives','--json','--bounded','--bounded']):
            r=subprocess.run([str(HGS),*args],input='{}',capture_output=True,text=True,env=self.env,timeout=30)
            self.assertNotEqual(r.returncode,0,args)
        helped=subprocess.run([str(HGS),'--help'],capture_output=True,text=True,env=self.env,timeout=30)
        self.assertIn('archive-bounded-pages-v1',[line.strip() for line in helped.stdout.splitlines()])
    def test_a_peer_gets_bounded_pages_only_when_its_cli_lists_the_marker(self):
        dry=subprocess.run([str(HGS),'--dry-run','@peer','archives','--json','--bounded'],capture_output=True,text=True,env=dict(self.env,HGS_PEERS='peer'),timeout=30)
        self.assertEqual(dry.returncode,0,dry.stderr)
        words=shlex.split(dry.stdout.strip().splitlines()[-1]);script=words[-1].replace(' --dry-run','')
        self.assertEqual(words[words.index('peer')+1:words.index('peer')+3],['sh','-c'])
        for marker,expected in ((True,['archives','--json','--bounded']),(False,['archives','--json'])):
            home=self.root/('peer-new' if marker else 'peer-old');bin_dir=home/'.local'/'bin';bin_dir.mkdir(parents=True)
            stub=bin_dir/'hgs';stub.write_text('#!/bin/sh\nif [ "$1" = --help ]; then echo usage; %s exit 0; fi\nprintf "%%s\\n" "$@" > "$HOME/called"\n' % ('echo "       archive-bounded-pages-v1";' if marker else ''),encoding='utf-8');stub.chmod(0o700)
            ran=subprocess.run(['sh','-c',script],env={'HOME':str(home),'PATH':'/usr/bin:/bin'},capture_output=True,text=True,timeout=30)
            self.assertEqual(ran.returncode,0,ran.stderr)
            self.assertEqual((home/'called').read_text(encoding='utf-8').split(),expected)

    def test_bounded_pages_cut_by_bytes_reach_every_row_in_both_directions(self):
        # zerus-m0boz: about 9.5 kB of metadata per row, so 250 kB holds 26 of a 50-row page.
        for i in range(80):
            value=self.record(i,100+i,title='\U0001f9ed'*1024);value['cwd']='/synthetic/'+'p'*3000
            path=self.archives/(value['archive_id']+'.json');path.write_text(json.dumps(value),encoding='utf-8')
        expected=[str(uuid.UUID(int=i+1)) for i in reversed(range(80))]
        page=self.run_query({'limit':50},bounded=True);older=[];sizes=[]
        while True:
            older+=[r['archive_id'] for r in page['sessions']];sizes.append(len(page['sessions']))
            if not page['next_cursor']:break
            page=self.run_query({'limit':50,'cursor':page['next_cursor']},bounded=True)
        self.assertEqual(older,expected);self.assertTrue(all(n<50 for n in sizes) and len(sizes)>2,sizes)
        newer=[];pages=[]
        while page['previous_cursor']:
            page=self.run_query({'limit':50,'cursor':page['previous_cursor']},bounded=True)
            ids=[r['archive_id'] for r in page['sessions']];pages.append(ids);newer+=ids
            # Each page's position is the count of rows newer than its first one.
            self.assertEqual(page['position'],expected.index(ids[0]))
        self.assertEqual(sorted(newer),sorted(expected[:80-sizes[-1]]))
        self.assertEqual(len(newer),len(set(newer)))
        for ids in pages:self.assertEqual(ids,sorted(ids,key=expected.index))

if __name__=='__main__':unittest.main()
