"""Authenticated asynchronous executor workflow; every payload is synthetic."""
import hashlib
import http.client
import json
import os
from pathlib import Path
import ssl
import subprocess
import tempfile
import time
import unittest
import urllib.parse
import uuid

from qualify import child,MIB
from test_transfer import BINARY,Publisher,read
from test_tree import TreePublisher
from network_faults import FaultRelay, through


@unittest.skipUnless(BINARY and BINARY.is_file(),"set ZERUS_DESYNC_TEST_BINARY to the isolated helper")
class ExecutorServiceTests(unittest.TestCase):
    def identity(self,base,name):
        directory=base/name;directory.mkdir();config=directory/"init.json";config.write_text("{}",encoding="utf-8")
        self.assertEqual(child(BINARY,"identity-init",config,"identity",memory_bytes=128*MIB,deadline=15)["exit_code"],0)
        return directory/"identity",read(directory/"identity/public.json")["fingerprint"]

    def fixture(self,script=None,guided=False,runtime="powershell"):
        temporary=tempfile.TemporaryDirectory();self.addCleanup(temporary.cleanup);root=Path(temporary.name)
        controller,controller_pin=self.identity(root,"controller")
        executor,executor_pin=self.identity(root,"executor-identity")
        offers=[]
        for name in ("artifact","scenario"):
            base=root/name;base.mkdir();source=base/"source";source.mkdir()
            if name=="artifact":(source/"large.other").write_bytes(b"synthetic payload"*65536)
            else:
                spec={"protocol":1,"instructions":"Execute the explicit synthetic scenario.","runtime":runtime,"entry":"run.ps1","arguments":[],"timeout_seconds":15,"memory_bytes":256*MIB,"guided":guided}
                (source/"scenario.json").write_text(json.dumps(spec),encoding="utf-8")
                (source/"run.ps1").write_text(script or "[IO.File]::AppendAllText((Join-Path $env:ZERUS_RESULT_DIR 'count.txt'),'one-execution'); Write-Output 'synthetic log'; exit 0",encoding="utf-8")
            producer=TreePublisher(base,source);self.addCleanup(producer.stop)
            producer.value["peer"]={"identity":str(controller),"fingerprint":executor_pin,"listen":"127.0.0.1:0"}
            offers.append(producer.start())
        host=root/"host";host.mkdir();state=host/"state";state.mkdir()
        service=Publisher(host,state);self.addCleanup(service.stop)
        service.command="executor-serve";service.offer_name="executor-offer.json"
        service.value={"root":str(state),"peer":{"identity":str(executor),"fingerprint":controller_pin,"listen":"127.0.0.1:0"},
                       "serve_seconds":60,"memory_bytes":512*MIB,"scenario_memory_bytes":256*MIB,"scenario_seconds":60,"stash_bytes":MIB}
        service.start()
        context=ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT);context.check_hostname=False;context.verify_mode=ssl.CERT_NONE
        context.minimum_version=ssl.TLSVersion.TLSv1_3
        context.load_cert_chain(str(controller/"identity.crt"),str(controller/"identity.key"))
        request={"protocol":1,"job_id":uuid.uuid4().hex,"slot":"synthetic-project","device_id":"host","artifact":offers[0],"scenario":offers[1],"parameters":{"case":"synthetic"}}
        return service,context,executor_pin,request,state

    def call(self,service,context,pin,method,path,body=None,headers=None,lost_reply=False):
        endpoint=urllib.parse.urlparse(service.offer["endpoint"])
        self.assertEqual(endpoint.hostname,"127.0.0.1")
        connection=http.client.HTTPSConnection(endpoint.hostname,endpoint.port,context=context,timeout=10)
        connection.connect()
        self.assertTrue(hashlib.sha256(connection.sock.getpeercert(binary_form=True)).hexdigest()==pin)
        payload=json.dumps(body).encode() if body is not None else None
        connection.request(method,path,body=payload,headers=headers or {})
        if lost_reply:connection.close();return None,None,None
        response=connection.getresponse();data=response.read(4*MIB+1);status=response.status;returned=dict(response.getheaders());connection.close()
        return status,data,returned

    def wait_result(self,service,context,pin,job):
        deadline=time.monotonic()+25
        while time.monotonic()<deadline:
            status,data,_=self.call(service,context,pin,"GET","/executor/v1/jobs/"+job)
            if status==200:
                value=json.loads(data)
                if value["submission"]["phase"] not in ("queued","deploying","executing"):return value
            time.sleep(0.05)
        self.fail("synthetic executor did not finish")

    def test_submission_survives_lost_reply_and_collects_once(self):
        service,context,pin,request,state=self.fixture()
        status,data,_=self.call(service,context,pin,"GET","/executor/v1/capabilities")
        self.assertEqual(status,200);self.assertFalse(json.loads(data)["model_runtime"])
        self.call(service,context,pin,"POST","/executor/v1/jobs",request,lost_reply=True)
        status,_,_=self.call(service,context,pin,"POST","/executor/v1/jobs",request)
        self.assertEqual(status,200)
        result=self.wait_result(service,context,pin,request["job_id"])
        self.assertEqual(result["submission"]["phase"],"passed")
        evidence=result["result"]["evidence"][0]
        key=hashlib.sha256(evidence["path"].encode()).hexdigest()
        status,data,_=self.call(service,context,pin,"GET","/executor/v1/jobs/"+request["job_id"]+"/evidence/"+key,headers={"X-Zerus-Offset":"0"})
        self.assertEqual(status,200);self.assertTrue(hashlib.sha256(data).hexdigest()==evidence["sha256"])
        self.assertTrue(data==b"one-execution")
        ack={"request_id":uuid.uuid4().hex,"result_sha256":result["result"]["result_sha256"]}
        status,_,_=self.call(service,context,pin,"POST","/executor/v1/jobs/"+request["job_id"]+"/acknowledgement",ack)
        self.assertEqual(status,200)
        status,_,_=self.call(service,context,pin,"POST","/executor/v1/jobs",request)
        self.assertEqual(status,200)
        self.assertFalse((state/"jobs"/request["job_id"]/"outputs").exists())

    def test_changed_job_identity_is_refused(self):
        service,context,pin,request,state=self.fixture()
        status,_,_=self.call(service,context,pin,"POST","/executor/v1/jobs",request);self.assertEqual(status,200)
        altered=json.loads(json.dumps(request));altered["parameters"]["case"]="different"
        status,_,_=self.call(service,context,pin,"POST","/executor/v1/jobs",altered);self.assertEqual(status,409)
        self.assertEqual(self.wait_result(service,context,pin,request["job_id"])["submission"]["phase"],"passed")

    def test_local_read_idle_bounds_executor_download_before_scenario(self):
        service,context,pin,request,state=self.fixture()
        service.stop();service.value["peer"]["read_idle_seconds"]=1;service.start()
        port=int(request["artifact"]["endpoint"].rsplit(":",1)[1])
        relay=FaultRelay(port,cut_response=3,cut_offset=512,stall=True)
        self.addCleanup(relay.close)
        request["artifact"]=through(request["artifact"],relay)
        start=time.monotonic()
        status,_,_=self.call(service,context,pin,"POST","/executor/v1/jobs",request)
        self.assertEqual(status,200)
        result=None
        while time.monotonic()-start<10:
            status,data,_=self.call(service,context,pin,"GET","/executor/v1/jobs/"+request["job_id"])
            self.assertEqual(status,200);result=json.loads(data)
            if result["submission"]["phase"]=="held":break
            time.sleep(0.05)
        self.assertTrue(relay.stats()["fired"],"synthetic download stall did not fire")
        self.assertEqual(result["submission"]["phase"],"held","local receiver inactivity bound was not applied")
        self.assertLess(time.monotonic()-start,10)
        slot=state/"slots"/request["slot"]
        self.assertFalse((slot/"tree-installed.json").exists())
        self.assertFalse((state/"jobs"/request["job_id"]/"job-state.json").exists())
        self.assertFalse((state/"jobs"/request["job_id"]/"outputs").exists())

    def test_host_performance_is_measured_at_executor_and_pinned(self):
        service,context,pin,request,state=self.fixture("Start-Sleep -Seconds 3; exit 0")
        request["performance"]={"cpu_percent":100,"disk_percent":100,"disk_busy_percent":100,"wait_seconds":0,"on_busy":"not_run"}
        status,_,_=self.call(service,context,pin,"POST","/executor/v1/jobs",request);self.assertEqual(status,200)
        changed=json.loads(json.dumps(request));changed["performance"]["disk_busy_percent"]=99
        status,_,_=self.call(service,context,pin,"POST","/executor/v1/jobs",changed);self.assertEqual(status,409)
        result=self.wait_result(service,context,pin,request["job_id"]);self.assertEqual(result["submission"]["phase"],"passed")
        load=result["result"]["performance"]
        self.assertEqual(load["origin"],"executor-host");self.assertTrue(load["valid"] and not load["loaded"])
        self.assertTrue(load["samples"]>=3 and load["during_samples"]>=1 and load["execution_ms"]>=1000)
        for sample in (load["before"],load["during"],load["after"]):
            self.assertTrue(all(0<=sample[key]<=100 for key in ("cpu_percent","disk_percent","disk_busy_percent")),"executor sample outside supported range")

    def test_performance_refused_before_execution_is_collectable(self):
        service,context,pin,request,state=self.fixture();service.stop();service.value["scenario_seconds"]=15;service.start()
        request["performance"]={"cpu_percent":100,"disk_percent":100,"disk_busy_percent":100,"wait_seconds":0,"on_busy":"not_run"}
        status,_,_=self.call(service,context,pin,"POST","/executor/v1/jobs",request);self.assertEqual(status,200)
        result=self.wait_result(service,context,pin,request["job_id"]);self.assertEqual(result["submission"]["phase"],"not_run")
        self.assertFalse(result["result"]["process"]["creation_issued"])
        self.assertFalse((state/"jobs"/request["job_id"]/"outputs/count.txt").exists())
        ack={"request_id":uuid.uuid4().hex,"result_sha256":result["result"]["result_sha256"]}
        status,_,_=self.call(service,context,pin,"POST","/executor/v1/jobs/"+request["job_id"]+"/acknowledgement",ack);self.assertEqual(status,200)

    def test_refused_admission_can_release_capacity_without_forgetting_job(self):
        service,context,pin,request,state=self.fixture(runtime="unsupported")
        status,_,_=self.call(service,context,pin,"POST","/executor/v1/jobs",request);self.assertEqual(status,200)
        result=self.wait_result(service,context,pin,request["job_id"]);self.assertEqual(result["submission"]["phase"],"infrastructure_error")
        self.assertFalse((state/"jobs"/request["job_id"]/"job-state.json").exists())
        release={"request_id":uuid.uuid4().hex,"request_sha256":result["submission"]["request_sha256"]}
        for _ in range(2):
            status,_,_=self.call(service,context,pin,"POST","/executor/v1/jobs/"+request["job_id"]+"/release",release);self.assertEqual(status,200)
        status,data,_=self.call(service,context,pin,"GET","/executor/v1/jobs");self.assertEqual(json.loads(data),[])
        status,_,_=self.call(service,context,pin,"POST","/executor/v1/jobs",request);self.assertEqual(status,200)
        self.assertEqual(self.wait_result(service,context,pin,request["job_id"])["submission"]["phase"],"infrastructure_error")

    def test_builder_controller_deploys_collects_and_reconnects(self):
        service,context,pin,request,state=self.fixture()
        root=state.parent.parent
        work=root/"controller-job";work.mkdir();config=work/"controller.json"
        value={"endpoint":service.offer["endpoint"],"peer":{"identity":str(root/"controller/identity"),"fingerprint":pin},
               "job_id":request["job_id"],"slot":request["slot"],"device_id":"host","artifact_root":str(root/"artifact/source"),
               "scenario_root":str(root/"scenario/source"),"listen":"127.0.0.1:0","average":65536,"memory_bytes":512*MIB,
               "timeout_seconds":30,"parameters":request["parameters"],"performance":{"cpu_percent":100,"disk_percent":100,"disk_busy_percent":100,"wait_seconds":0,"on_busy":"not_run"}}
        config.write_text(json.dumps(value),encoding="utf-8")
        self.assertEqual(child(BINARY,"controller-deploy",config,"deploy",memory_bytes=512*MIB,deadline=40)["exit_code"],0)
        self.assertNotEqual(read(work/"controller-state.json")["phase"],"deploying")
        self.assertEqual(child(BINARY,"controller-run",config,"collect",memory_bytes=512*MIB,deadline=40)["exit_code"],0)
        result=read(work/"controller-result.json")["result"]
        self.assertEqual(result["status"],"passed")
        self.assertTrue(read(work/"controller-state.json")["performance"]["valid"])
        for entry in result["evidence"]:
            evidence=work/"evidence"/hashlib.sha256(entry["path"].encode()).hexdigest()
            with evidence.open("rb") as stream:
                self.assertTrue(hashlib.file_digest(stream,"sha256").hexdigest()==entry["sha256"])
        self.assertFalse((state/"jobs"/request["job_id"]/"outputs").exists())
        self.assertEqual(child(BINARY,"controller-run",config,"reconnect",memory_bytes=512*MIB,deadline=20)["exit_code"],0)

    def test_service_restart_never_replays_admitted_script(self):
        script="[IO.File]::AppendAllText((Join-Path $env:ZERUS_RESULT_DIR 'count.txt'),'once'); Start-Sleep -Seconds 12; exit 0"
        service,context,pin,request,state=self.fixture(script)
        status,_,_=self.call(service,context,pin,"POST","/executor/v1/jobs",request);self.assertEqual(status,200)
        count=state/"jobs"/request["job_id"]/"outputs/count.txt"
        deadline=time.monotonic()+15
        while not count.exists() and time.monotonic()<deadline:time.sleep(0.05)
        self.assertTrue(count.exists());service.stop();service.start()
        result=self.wait_result(service,context,pin,request["job_id"])
        self.assertEqual(result["submission"]["phase"],"unknown")
        self.call(service,context,pin,"POST","/executor/v1/jobs",request)
        self.assertTrue(count.read_bytes()==b"once")

    def test_guided_observation_uses_exact_remote_request(self):
        script="while (-not (Test-Path -LiteralPath $env:ZERUS_OBSERVATION_FILE)) { Start-Sleep -Milliseconds 50 }; exit 0"
        service,context,pin,request,state=self.fixture(script,guided=True)
        status,_,_=self.call(service,context,pin,"POST","/executor/v1/jobs",request);self.assertEqual(status,200)
        deadline=time.monotonic()+15;result=None
        while time.monotonic()<deadline:
            status,data,_=self.call(service,context,pin,"GET","/executor/v1/jobs/"+request["job_id"])
            result=json.loads(data)
            if result["result"].get("status")=="awaiting_observation":break
            time.sleep(0.05)
        self.assertEqual(result["result"].get("status"),"awaiting_observation")
        observation={"request_id":uuid.uuid4().hex,"job_id":request["job_id"],"request_sha256":result["result"]["request_sha256"],"verdict":"passed","notes":"Synthetic explicit observation."}
        altered=dict(observation,request_sha256="0"*64)
        status,_,_=self.call(service,context,pin,"POST","/executor/v1/jobs/"+request["job_id"]+"/observation",altered);self.assertEqual(status,409)
        status,_,_=self.call(service,context,pin,"POST","/executor/v1/jobs/"+request["job_id"]+"/observation",observation);self.assertEqual(status,200)
        self.assertEqual(self.wait_result(service,context,pin,request["job_id"])["submission"]["phase"],"passed")

    @unittest.skipUnless(os.environ.get("ZERUS_ARTIFACT_TEAM_TEST_BINARY"),"set the isolated Rust unit-test executable for broker integration")
    def test_native_teams_broker_runs_the_paired_pipeline(self):
        self.native_broker(False)

    @unittest.skipUnless(os.environ.get("ZERUS_ARTIFACT_TEAM_TEST_BINARY"),"set the isolated Rust unit-test executable for broker integration")
    def test_native_teams_performance_uses_remote_receipt(self):
        self.native_broker(True)

    def native_broker(self,performance):
        service,context,pin,request,state=self.fixture()
        root=state.parent.parent;base=root/"zerus-state/artifact-executors";(base/"profiles").mkdir(parents=True)
        profile={"helper":str(BINARY),"source":"shared","artifact_relative":"artifact/source","controller":{
            "endpoint":service.offer["endpoint"],"peer":{"identity":str(root/"controller/identity"),"fingerprint":pin},
            "slot":"native-broker","device_id":"host","scenario_root":str(root/"scenario/source"),"listen":"127.0.0.1:0",
            "average":65536,"memory_bytes":512*MIB,"timeout_seconds":30,"parameters":{"case":"synthetic"}}}
        (base/"profiles/synthetic.json").write_text(json.dumps(profile),encoding="utf-8")
        if performance:
            profile["controller"]["performance"]={"cpu_percent":100,"disk_percent":100,"disk_busy_percent":100,"wait_seconds":0,"on_busy":"not_run"}
            (base/"profiles/synthetic.json").write_text(json.dumps(profile),encoding="utf-8")
        fixture=root/"native-fixture.json";fixture.write_text(json.dumps({"base":str(base),"project":str(root),"performance":performance}),encoding="utf-8")
        environment={key:os.environ[key] for key in ("SystemRoot","WINDIR","COMSPEC","PATH") if key in os.environ}
        environment["ZERUS_ARTIFACT_TEAM_FIXTURE"]=str(fixture)
        result=subprocess.run([os.environ["ZERUS_ARTIFACT_TEAM_TEST_BINARY"],"state::teams::artifacts::tests::artifact_native_broker_pipeline","--exact","--ignored","--nocapture"],
                              env=environment,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=50,creationflags=subprocess.CREATE_NO_WINDOW)
        self.assertEqual(result.returncode,0,"synthetic Rust broker integration failed: "+result.stdout.decode("utf-8",errors="replace")[-4000:])


if __name__=="__main__":unittest.main()
