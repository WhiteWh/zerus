"""Additive paged archive read contract; exact identity admission stays bounded."""
import asyncio
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
import uuid
from unittest.mock import AsyncMock, patch
from zerus_mobile import archives
from zerus_mobile.connector import Connector
from zerus_mobile.context import READ_OPERATIONS, READ_SQL
from zerus_mobile.server import validate_request

class ArchiveValidation(unittest.TestCase):
    def test_preview_retains_exact_notification_preferences_and_unknown(self):
        preference=dict(conversation_id='conversation',muted=True,revision=7,
                        suppressed=dict(reply_id=None,attention_signature=None,optional_question_ids=[],main_attention=None,child_attention={}))
        rows=[dict(name='codex/example/archived',archive_id='00000000-0000-4000-8000-000000000010',state='archived',archived_at=20,notification_preferences=preference),
              dict(name='codex/example/other',archive_id='00000000-0000-4000-8000-000000000020',state='archived',archived_at=10,notification_preferences=None)]
        page=dict(version=1,sessions=rows,total=2,next_cursor='',**{'from':0,'until':253402300800,'query':''})
        preview,metadata=archives.preview(page)
        self.assertEqual(preview[0].get('notification_preferences'),preference)
        self.assertIn('notification_preferences',preview[1])
        self.assertIsNone(preview[1]['notification_preferences'])
        self.assertEqual(metadata['loaded'],2)
        rows[0]['notification_preferences']={'padding':'x'*(128*1024)}
        self.assertEqual(archives.preview(page)[0],[],'An oversized row stays outside the bounded preview')
    def test_read_class_strict_bounds_and_sessionless_envelope(self):
        self.assertIn('archive_list',READ_OPERATIONS);self.assertNotIn('archive_list',READ_SQL)
        for payload in ({},{'from':10,'until':11,'limit':1},{'query':'Русский %_'}):
            archives.validate(payload)
            validate_request(dict(request_id=str(uuid.uuid4()),computer_id=str(uuid.uuid4()),operation='archive_list',session='',payload=payload))
        for payload in ({'limit':51},{'limit':True},{'from':5,'until':5},{'until':253402300801},{'cursor':'#'},{'query':'x\nx'},{'offset':1000}):
            with self.assertRaises(ValueError):archives.validate(payload)
        with self.assertRaises(ValueError):validate_request(dict(request_id=str(uuid.uuid4()),computer_id=str(uuid.uuid4()),operation='archive_list',session='codex/example/x',payload={}))

    def test_preview_keeps_notification_preferences_unchanged(self):
        # The relay's preference ledger refuses a known conversation without them.
        preferences={'conversation_id':'c','muted':True,'revision':3,'suppressed':{'reply_id':'r','attention_signature':None,
            'optional_question_ids':[],'main_attention':None,'child_attention':{}}}
        rows=[dict(name='claude/example/a',conversation_id='c',archive_id='10000000-0000-4000-8000-000000000002',state='archived',
                   archived_at=200,notification_preferences=preferences),
              dict(name='claude/example/b',conversation_id='d',archive_id='10000000-0000-4000-8000-000000000001',state='archived',
                   archived_at=100,notification_preferences=None)]
        value=dict(version=1,sessions=rows,next_cursor='',total=2,**{'from':0,'until':253402300800,'query':''})
        preview,_=archives.preview(value)
        self.assertEqual(preview[0]['notification_preferences'],preferences)
        self.assertIn('notification_preferences',preview[1]);self.assertIsNone(preview[1]['notification_preferences'])

class ArchiveConnector(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.node=Connector({'server_url':'https://gateway.example','node_token':'synthetic','hgs_path':'/synthetic/hgs','state_dir':self.temp.name})
        self.addCleanup(self.node.journal.close);self.node.archive_supported=True
    def request(self,**payload):return dict(request_id=str(uuid.uuid4()),operation='archive_list',session='',payload=payload)
    def page(self):return dict(version=1,sessions=[dict(name='codex/example/a',archive_id='10000000-0000-4000-8000-000000000001',state='archived',archived_at=100)],next_cursor='',total=1,**{'from':0,'until':253402300800,'query':''})
    async def test_page_provenance_survives_active_snapshot_and_native_recheck_remains(self):
        req=self.request();self.node.fleet.resolve=AsyncMock(return_value=(self.node,None))
        with patch.object(self.node,'native',AsyncMock(return_value=self.page())) as native:
            result=await self.node.execute(req)
        self.assertEqual(result['state'],'completed',result);self.assertEqual(native.call_args.args,(['archives','--json'],{}));self.assertEqual(native.call_args.kwargs['timeout'],30)
        self.node.archives=set();self.node.snapshot_seen_at=time.monotonic()
        self.assertTrue(self.node.archive_allowed('codex/example/a','10000000-0000-4000-8000-000000000001'))
        self.assertFalse(self.node.archive_allowed('codex/example/other','10000000-0000-4000-8000-000000000001'))
        target=('codex/example/a','10000000-0000-4000-8000-000000000001');self.node.paged_archives[target]-=3601
        self.assertFalse(self.node.archive_allowed(*target))
    async def test_bad_native_pages_never_grant_archive_admission(self):
        self.node.fleet.resolve=AsyncMock(return_value=(self.node,None))
        for edit in ({'version':2},{'total':0},{'from':5},{'next_cursor':'#'},{'sessions':[]}, {'previous_cursor':'#','position':0},{'previous_cursor':'','position':True},{'previous_cursor':'','position':1},{'position':0}):
            page={**self.page(),**edit}
            if edit=={'sessions':[]}:page['next_cursor']='loop'
            with patch.object(self.node,'native',AsyncMock(return_value=page)):
                self.assertEqual((await self.node.execute(self.request()))['state'],'failed')
            self.assertFalse(self.node.paged_archives)
    async def test_uncached_archive_requires_exact_native_lookup_before_any_action(self):
        self.node.fleet.resolve=AsyncMock(return_value=(self.node,None))
        archive='10000000-0000-4000-8000-000000000001';name='codex/example/a'
        detail=dict(version=1,name=name,archive_id=archive,run_id='old-run',conversation_id='old-conversation')
        def inspect():return dict(request_id=str(uuid.uuid4()),operation='inspect',session=name,payload={'archive_id':archive})
        for wrong in ({**detail,'archive_id':str(uuid.uuid4())},{**detail,'name':name+'other'},{**detail,'version':True}):
            with patch.object(self.node,'native',AsyncMock(return_value=wrong)) as native:
                self.assertEqual((await self.node.execute(inspect()))['state'],'failed')
                self.assertEqual(native.call_count,1)
            self.assertFalse(self.node.paged_archives)
        req=dict(request_id=str(uuid.uuid4()),operation='forget',session=name)
        req['payload']=dict(request_id=req['request_id'],archive_id=archive,expected_run_id='replacement-run',expected_conversation_id='old-conversation')
        with patch.object(self.node,'native',AsyncMock(return_value=detail)) as native:
            self.assertEqual((await self.node.execute(req))['state'],'failed');self.assertEqual(native.call_count,1)
        self.assertFalse(self.node.paged_archives,'Wrong-run metadata must not admit a mutation')
        with patch.object(self.node,'native',AsyncMock(side_effect=[detail,{'name':name,'archive_id':archive}])) as native:
            self.assertEqual((await self.node.execute(inspect()))['state'],'completed')
            self.assertEqual(native.call_args_list[0].args,(['archives','lookup','--json'],{'name':name,'archive_id':archive}))
        self.assertTrue(self.node.archive_allowed(name,archive))
    async def test_only_successful_deliverable_archive_reads_extend_admission(self):
        self.node.fleet.resolve=AsyncMock(return_value=(self.node,None))
        name='codex/example/a';archive='10000000-0000-4000-8000-000000000001'
        key=(name,archive);prior=time.monotonic()-30;self.node.paged_archives[key]=prior
        def request():return dict(request_id=str(uuid.uuid4()),operation='inspect',session=name,payload={'archive_id':archive})
        with patch.object(self.node,'native',AsyncMock(return_value={'text':'x'*(1024*1024)})):
            self.assertEqual((await self.node.execute(request()))['state'],'failed')
        self.assertEqual(self.node.paged_archives[key],prior)
        with patch.object(self.node,'native',AsyncMock(return_value={'name':name,'archive_id':archive})):
            self.assertEqual((await self.node.execute(request()))['state'],'completed')
        self.assertGreater(self.node.paged_archives[key],prior)

from bb_operations import real_hgs, OperationCase
from connector_harness import BlackBoxCase

class NativeArchivePaging(OperationCase,BlackBoxCase):
    def produce_archives(self, native):
        tests = Path(__file__).resolve().parents[3] / "tests"
        with patch.object(sys, "path", [str(tests), *sys.path]):
            import test_pause as pause

        self.assertTrue(pause.TMUX, "tmux is required for the native archive writer fixture")
        producer = pause.Harness()
        producer.setUp()
        self.addCleanup(producer.tearDown)
        producer.env["TMUX_TMPDIR"] = str(producer.root)
        pending = "hgs: Archive catalog coverage is incomplete; metadata is updating; retry the archive"

        def catalog_pending(result):
            return result.returncode == 1 and not result.stdout and result.stderr.strip() == pending

        def ready(expected):
            result = subprocess.run([native, "archives", "--json", "--bounded"], input="{}",
                                    env=producer.env, capture_output=True, text=True, timeout=10)
            if catalog_pending(result):
                return False
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            page = json.loads(result.stdout)
            return page.get("catalog_complete") is not False and page["total"] == expected

        def settle_catalog(expected):
            producer.hgs("__state", "archive-catalog-worker")
            producer.wait(lambda: ready(expected))

        with patch.object(pause, "HGS", Path(native)):
            producer.hgs("listing-index", "disable", "--json")
            for index in range(121):
                name = producer.start(tag=f"archive-{index}")
                producer.hgs("pause", name)
                binding = producer.binding(name)
                result = subprocess.run([native, "archive", name], env=producer.env,
                                        capture_output=True, text=True, timeout=20)
                if catalog_pending(result):
                    # The archive writer can refuse before publication. Retry only
                    # once after confirming the same session and the prior count.
                    current = producer.binding(name)
                    for key in ("run_id", "conversation_id"):
                        self.assertEqual(current[key], binding[key])
                    settle_catalog(index)
                    producer.hgs("archive", name)
                else:
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            settle_catalog(121)
        env = {key: producer.env[key] for key in
               ("HGS_CONFIG_DIR", "HGS_STATE_DIR", "HGS_SELF", "HGS_PEERS", "TMUX_TMPDIR")}
        env["PATH"] = os.pathsep.join([str(producer.bin), self.environment()["PATH"]])
        return env

    async def test_real_native_pages_stay_lazy_and_admit_exact_archived_inspection(self):
        native=real_hgs()
        if native is None:self.fail('Build the native hgs before running archive transport contracts')
        env = await asyncio.to_thread(self.produce_archives, native)
        self.hgs.prepend({'argv':['ls','--json','--local','--no-archives'],'exec':native},{'argv':['--help'],'exec':native},{'prefix':['archives'],'exec':native},{'prefix':['inspect'],'exec':native})
        process=await self.launch(env=env);mark=self.mark()
        snapshot=self.relay.heartbeats()[-1].body['snapshot']
        self.assertIn('archive_list',snapshot['mobile_capabilities']['operations'])
        legacy=[row for row in snapshot['sessions'] if row.get('state')=='archived']
        self.assertEqual(len(legacy),50);self.assertEqual(snapshot['mobile_archive']['total'],121);self.assertTrue(snapshot['mobile_archive']['partial'])
        self.assertEqual(self.since(mark,'archives'),[])
        def request(cursor=''):return dict(request_id=str(uuid.uuid4()),computer_id=self.relay.computer_id,operation='archive_list',session='',payload={'from':0,'until':253402300800,'limit':50,'cursor':cursor,'query':''})
        first=await self.submit(request(),process);self.assertEqual(first['state'],'completed',first)
        page=first['result'];self.assertEqual(page['total'],121);self.assertEqual(len(page['sessions']),50)
        self.assertEqual(len(self.since(mark,'archives')),1)
        second=await self.submit(request(page['next_cursor']),process);self.assertEqual(second['state'],'completed',second)
        self.assertEqual(len(second['result']['sessions']),50)
        self.assertFalse({r['archive_id']for r in page['sessions']} & {r['archive_id']for r in second['result']['sessions']})
        row=second['result']['sessions'][0]
        self.assertNotIn(row['archive_id'],{entry['archive_id'] for entry in legacy},'The inspection must exercise a target outside the legacy window')
        inspect=dict(request_id=str(uuid.uuid4()),computer_id=self.relay.computer_id,operation='inspect',session=row['name'],payload={'archive_id':row['archive_id']})
        result=await self.submit(inspect,process);self.assertEqual(result['state'],'completed',result)
        self.assertEqual(result['result']['archive_id'],row['archive_id']);self.assertEqual(result['result']['run_id'],row['run_id'])
        self.assertEqual(len(self.since(mark,'archives')),2,'Inspect must not load the entire archive again')
        back=await self.submit(request(second['result']['previous_cursor']),process)
        self.assertEqual(back['state'],'completed',back);self.assertEqual(back['result']['position'],0)
        self.assertEqual([r['archive_id'] for r in back['result']['sessions']],[r['archive_id'] for r in page['sessions']])
        process.stop()
        process=await self.launch(env=env)
        mark=self.mark()
        # The phone can retain this exact target while the connector restarts.
        # It is deliberately absent from the bounded recent snapshot window.
        inspect['request_id']=str(uuid.uuid4())
        result=await self.submit(inspect,process);self.assertEqual(result['state'],'completed',result)
        self.assertEqual(result['result']['archive_id'],row['archive_id'])
        self.assertEqual(result['result']['run_id'],row['run_id'])
        self.assertEqual([call['argv'] for call in self.since(mark,'archives')],[['archives','lookup','--json']])
