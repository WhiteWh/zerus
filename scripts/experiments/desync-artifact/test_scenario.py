"""Model-free scenario execution on synthetic installed trees only."""
import json
import concurrent.futures
from pathlib import Path
import tempfile
import time
import unittest
import uuid

from qualify import child,MIB
from test_transfer import BINARY,read
from test_tree import TreePublisher


@unittest.skipUnless(BINARY and BINARY.is_file(),"set ZERUS_DESYNC_TEST_BINARY to the isolated helper")
class ScenarioExecutionTests(unittest.TestCase):
    def fixture(self,script=None,timeout=15,guided=False):
        temporary=tempfile.TemporaryDirectory();self.addCleanup(temporary.cleanup);root=Path(temporary.name)
        executor=root/"executor";executor.mkdir();init=executor/"init.json";init.write_text("{}",encoding="utf-8")
        self.assertEqual(child(BINARY,"identity-init",init,"identity",memory_bytes=128*MIB,deadline=15)["exit_code"],0)
        sources=[];slots=[];descriptors=[]
        for label in ("artifact","scenario"):
            base=root/label;base.mkdir();source=base/"source";source.mkdir()
            if label=="artifact":(source/"opaque.container").write_bytes(b"synthetic build payload"*4096)
            else:
                spec={"protocol":1,"instructions":"Run the explicit synthetic owner scenario and retain its measurement.","runtime":"powershell","entry":"run.ps1","arguments":[],"timeout_seconds":timeout,"memory_bytes":256*MIB,"guided":guided}
                (source/"scenario.json").write_text(json.dumps(spec),encoding="utf-8")
                body=script or "[IO.File]::AppendAllText((Join-Path $env:ZERUS_RESULT_DIR 'runs.txt'), 'executed-once'); [IO.File]::WriteAllText((Join-Path $env:ZERUS_RESULT_DIR 'measurement.json'), '{\"synthetic\":true}'); exit 0"
                (source/"run.ps1").write_text(body,encoding="utf-8")
            publisher=TreePublisher(base,source);self.addCleanup(publisher.stop);offer=publisher.start()
            slot=base/"slot";slot.mkdir();config=slot/"config.json";delivery=uuid.uuid4().hex
            config.write_text(json.dumps({"offer":offer,"delivery_id":delivery,"stash_bytes":MIB,"memory_bytes":128*MIB}),encoding="utf-8")
            self.assertEqual(child(BINARY,"tree-receive",config,"install",memory_bytes=256*MIB,deadline=20)["exit_code"],0)
            publisher.stop();slots.append(slot);descriptors.append((offer["artifact_id"],delivery))
        config=root/"job.json"
        value={"executor_root":str(executor),"identity":str(executor/"identity"),"job_id":uuid.uuid4().hex,
               "artifact_slot":str(slots[0]),"artifact_id":descriptors[0][0],"artifact_delivery_id":descriptors[0][1],
               "scenario_slot":str(slots[1]),"scenario_id":descriptors[1][0],"scenario_delivery_id":descriptors[1][1],
               "device_id":"host","parameters":{"sample":"synthetic"}}
        config.write_text(json.dumps(value),encoding="utf-8")
        return root,config,value

    def invoke(self,config,command="scenario-run",label="scenario"):
        return child(BINARY,command,config,label,memory_bytes=512*MIB,deadline=35)["exit_code"]

    def state(self,value):
        return read(Path(value["executor_root"])/"jobs"/value["job_id"]/"job-state.json")

    def test_runs_once_without_models_and_keeps_exact_artifact_identity(self):
        root,config,value=self.fixture()
        self.assertEqual(self.invoke(config),0)
        state=self.state(value);self.assertEqual(state["status"],"passed")
        self.assertTrue(state["artifact_id"]==value["artifact_id"])
        work=Path(value["executor_root"])/"jobs"/value["job_id"]
        self.assertTrue((work/"outputs/runs.txt").read_text()=="executed-once")
        self.assertEqual(self.invoke(config,label="duplicate"),0)
        self.assertTrue((work/"outputs/runs.txt").read_text()=="executed-once")
        value["parameters"]["sample"]="different";config.write_text(json.dumps(value),encoding="utf-8")
        self.assertNotEqual(self.invoke(config,label="conflicting-request"),0)

    def test_timeout_stops_only_owned_scenario_processes(self):
        root,config,value=self.fixture(script="Start-Sleep -Seconds 30; exit 0",timeout=1)
        self.assertEqual(self.invoke(config),0)
        self.assertEqual(self.state(value)["status"],"timeout")
        self.assertTrue((Path(value["artifact_slot"])/"build/opaque.container").is_file())

    def test_crash_while_waiting_for_quiet_never_launches_on_resume(self):
        root,config,value=self.fixture()
        value.update(performance={"cpu_percent":100,"disk_percent":100,"disk_busy_percent":100,"wait_seconds":0,"on_busy":"not_run"},crash_phase="scenario_quiet_wait")
        config.write_text(json.dumps(value),encoding="utf-8");self.assertEqual(self.invoke(config),86)
        value.pop("crash_phase");config.write_text(json.dumps(value),encoding="utf-8")
        self.assertEqual(self.invoke(config),0);self.assertEqual(self.state(value)["status"],"unknown")
        self.assertFalse((Path(value["executor_root"])/"jobs"/value["job_id"]/"outputs/runs.txt").exists())

    def test_abrupt_exit_never_replays_the_same_job(self):
        for phase in ("scenario_admitted","scenario_created","scenario_running"):
            with self.subTest(phase=phase):
                root,config,value=self.fixture()
                value["crash_phase"]=phase;config.write_text(json.dumps(value),encoding="utf-8")
                self.assertEqual(self.invoke(config,label="crash"),86)
                value.pop("crash_phase");config.write_text(json.dumps(value),encoding="utf-8")
                self.assertEqual(self.invoke(config,label="reconcile"),0)
                self.assertEqual(self.state(value)["status"],"unknown")
                output=Path(value["executor_root"])/"jobs"/value["job_id"]/"outputs/runs.txt"
                before=output.read_bytes() if output.exists() else b""
                self.assertEqual(self.invoke(config,label="repeat-unknown"),0)
                self.assertTrue((output.read_bytes() if output.exists() else b"")==before)

    def test_result_acknowledgement_releases_only_unchanged_generated_outputs(self):
        root,config,value=self.fixture()
        self.assertEqual(self.invoke(config),0);state=self.state(value)
        value["acknowledgement"]={"request_id":uuid.uuid4().hex,"result_sha256":state["result_sha256"]}
        config.write_text(json.dumps(value),encoding="utf-8")
        self.assertEqual(self.invoke(config,"scenario-ack","acknowledge"),0)
        self.assertTrue(self.state(value)["collected"])
        work=Path(value["executor_root"])/"jobs"/value["job_id"]
        self.assertFalse((work/"outputs").exists())
        self.assertTrue((Path(value["artifact_slot"])/"build/opaque.container").is_file())
        self.assertEqual(self.invoke(config,"scenario-ack","repeat-ack"),0)
        self.assertEqual(self.invoke(config,label="repeat-collected-job"),0)
        self.assertFalse((work/"outputs").exists())

    def test_changed_results_are_preserved_instead_of_pruned(self):
        root,config,value=self.fixture()
        self.assertEqual(self.invoke(config),0);state=self.state(value)
        output=Path(value["executor_root"])/"jobs"/value["job_id"] /"outputs/measurement.json"
        output.write_text("user changed this synthetic output",encoding="utf-8")
        value["acknowledgement"]={"request_id":uuid.uuid4().hex,"result_sha256":state["result_sha256"]}
        config.write_text(json.dumps(value),encoding="utf-8")
        self.assertNotEqual(self.invoke(config,"scenario-ack","refused-ack"),0)
        self.assertTrue(output.is_file())

    def test_guided_scenario_waits_for_a_bound_observation(self):
        script="while (-not (Test-Path $env:ZERUS_OBSERVATION_FILE)) { Start-Sleep -Milliseconds 50 }; [IO.File]::WriteAllText((Join-Path $env:ZERUS_RESULT_DIR 'observation.txt'),'synthetic observation received'); exit 0"
        root,config,value=self.fixture(script=script,timeout=15,guided=True)
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
            running=pool.submit(self.invoke,config)
            deadline=time.monotonic()+10
            state=None
            while time.monotonic()<deadline:
                try:
                    state=self.state(value)
                    if state["status"]=="awaiting_observation":break
                except FileNotFoundError:pass
                if running.done():break
                time.sleep(0.02)
            self.assertIsNotNone(state)
            self.assertEqual(state["status"],"awaiting_observation")
            value["observation"]={"request_id":uuid.uuid4().hex,"job_id":value["job_id"],"request_sha256":state["request_sha256"],"verdict":"passed","notes":"Synthetic test observer response"}
            config.write_text(json.dumps(value),encoding="utf-8")
            self.assertEqual(self.invoke(config,"scenario-observe","observe"),0)
            self.assertEqual(running.result(timeout=20),0)
        self.assertEqual(self.state(value)["status"],"passed")
        self.assertTrue(self.state(value)["observation"]["notes"]=="Synthetic test observer response")


if __name__=="__main__":unittest.main()
