"""Whole-tree and generation qualification with synthetic local payloads."""
import hashlib
import json
import os
from pathlib import Path
import random
import tempfile
import threading
import unittest
import uuid

from qualify import child, MIB
from test_transfer import BINARY, Publisher, identity, read


class TreePublisher(Publisher):
    def __init__(self,base,source):
        super().__init__(base,source)
        self.command="tree-serve"
        self.offer_name="tree-offer.json"
        self.value={"source_root":str(source),"average":65536,"serve_seconds":60,
                    "stash_bytes":MIB,"memory_bytes":128*MIB}


@unittest.skipUnless(BINARY and BINARY.is_file(),"set ZERUS_DESYNC_TEST_BINARY to the isolated helper")
class TreeDeliveryTests(unittest.TestCase):
    def test_publication_restart_with_concurrent_metadata_observer(self):
        publisher,source,config,value=self.fixture()
        stop=threading.Event();failures=[]
        def observe():
            while not stop.is_set():
                try:read(publisher.work/publisher.offer_name)
                except FileNotFoundError:pass
                except Exception:failures.append("synthetic metadata observation failed");return
                stop.wait(0.001)
        thread=threading.Thread(target=observe,daemon=True);thread.start()
        try:
            for generation in range(12):
                publisher.stop()
                with (source/"app.dll").open("r+b") as stream:stream.seek(100);stream.write(bytes([generation])*16)
                old=source/("toggle-a.txt" if generation%2 else "toggle-b.txt")
                if old.exists():old.unlink()
                (source/("toggle-b.txt" if generation%2 else "toggle-a.txt")).write_text("synthetic metadata",encoding="utf-8")
                publisher.start()
        finally:
            stop.set();thread.join(timeout=5);publisher.stop()
        self.assertFalse(thread.is_alive() or failures,"synthetic observer did not settle cleanly")
    def fixture(self):
        temporary=tempfile.TemporaryDirectory();self.addCleanup(temporary.cleanup)
        base=Path(temporary.name);source=base/"source";source.mkdir();(source/"packs").mkdir()
        (source/"packs/large.container").write_bytes(random.Random(301).randbytes(8*MIB))
        (source/"app.dll").write_bytes(random.Random(12).randbytes(65536))
        (source/"obsolete.txt").write_text("old synthetic content",encoding="utf-8")
        (source/"empty.dat").touch()
        publisher=TreePublisher(base,source);self.addCleanup(publisher.stop);offer=publisher.start()
        slot=base/"slot";slot.mkdir();config=slot/"config.json"
        value={"offer":offer,"delivery_id":uuid.uuid4().hex,"stash_bytes":MIB,"memory_bytes":128*MIB}
        config.write_text(json.dumps(value),encoding="utf-8")
        return publisher,source,config,value

    def invoke(self,config,command="tree-receive",label="tree"):
        return child(BINARY,command,config,label,memory_bytes=256*MIB,deadline=30)["exit_code"]

    def assert_ready(self,source,config):
        self.assertEqual(self.invoke(config,"tree-status","status"),0)
        wanted={str(p.relative_to(source)) for p in source.rglob("*") if p.is_file()}
        actual={str(p.relative_to(config.parent/"build")) for p in (config.parent/"build").rglob("*") if p.is_file()}
        self.assertTrue(wanted==actual,"synthetic tree file set differs")
        for relative in wanted:
            with (source/relative).open("rb") as a,(config.parent/"build"/relative).open("rb") as b:
                self.assertTrue(hashlib.file_digest(a,"sha256").digest()==hashlib.file_digest(b,"sha256").digest(),"synthetic tree contents differ")

    def test_tree_provision_and_second_generation_reuses_renamed_container(self):
        publisher,source,config,value=self.fixture()
        self.assertEqual(self.invoke(config),0);self.assert_ready(source,config)
        old=value["offer"]["artifact_id"]
        renamed_identity=identity(config.parent/"build/packs/large.container")
        publisher.stop()
        (source/"packs/large.container").rename(source/"packs/renamed.other")
        (source/"obsolete.txt").unlink()
        (source/"new-dir").mkdir();(source/"new-dir/config.ini").write_text("synthetic replacement",encoding="utf-8")
        with (source/"app.dll").open("r+b") as stream: stream.seek(1024);stream.write(b"new synthetic code")
        value["offer"]=publisher.start();value["previous_artifact_id"]=old;value["delivery_id"]=uuid.uuid4().hex
        config.write_text(json.dumps(value),encoding="utf-8")
        self.assertEqual(self.invoke(config,label="generation"),0);self.assert_ready(source,config)
        self.assertTrue(identity(config.parent/"build/packs/renamed.other")==renamed_identity)
        publisher.stop()
        self.assertEqual(self.invoke(config,label="offline-no-op"),0)

    def results(self,config):
        found={}
        for path in (config.parent/"files").glob("*/result.json"):
            manifest=json.loads((path.parent/"manifest.json").read_text(encoding="utf-8"))
            found[manifest.get("tree_path") or manifest.get("path")]=json.loads(path.read_text(encoding="utf-8"))
        return found

    def test_next_generation_seeds_unchanged_files_from_the_cached_index(self):
        publisher,source,config,value=self.fixture()
        self.assertEqual(self.invoke(config),0);self.assert_ready(source,config)
        first=self.results(config)
        self.assertTrue(first and not any(r.get("receiver_index_cached") for r in first.values()))
        old=value["offer"]["artifact_id"];publisher.stop()
        with (source/"app.dll").open("r+b") as stream: stream.seek(1024);stream.write(b"new synthetic code")
        value["offer"]=publisher.start();value["previous_artifact_id"]=old;value["delivery_id"]=uuid.uuid4().hex
        config.write_text(json.dumps(value),encoding="utf-8")
        self.assertEqual(self.invoke(config,label="generation"),0);self.assert_ready(source,config)
        second=self.results(config)
        changed=[r for r in second.values() if r.get("receiver_index_cached")]
        self.assertTrue(changed,second)
        self.assertTrue(all(r["ok"] and r["exact_hash_match"] for r in second.values()))
        publisher.stop()

    def test_corruption_after_install_redoes_a_cached_seed(self):
        publisher,source,config,value=self.fixture()
        self.assertEqual(self.invoke(config),0)
        old=value["offer"]["artifact_id"];publisher.stop()
        path=config.parent/"build/packs/large.container"
        os.chmod(path,0o666)
        with path.open("r+b") as stream:stream.seek(8192);stream.write(b"synthetic corruption")
        with (source/"app.dll").open("r+b") as stream: stream.seek(1024);stream.write(b"new synthetic code")
        value["offer"]=publisher.start();value["previous_artifact_id"]=old;value["delivery_id"]=uuid.uuid4().hex
        config.write_text(json.dumps(value),encoding="utf-8")
        self.assertEqual(self.invoke(config,label="generation"),0);self.assert_ready(source,config)
        results=self.results(config)
        self.assertTrue(any(r.get("receiver_index_redone") for r in results.values()),results)
        publisher.stop()

    def test_crash_boundaries_repair_the_whole_tree(self):
        for phase in ("tree_intent","tree_file","tree_ready"):
            with self.subTest(phase=phase):
                publisher,source,config,value=self.fixture()
                value["crash_phase"]=phase;config.write_text(json.dumps(value),encoding="utf-8")
                self.assertEqual(self.invoke(config,label="crash"),86)
                self.assertNotEqual(self.invoke(config,"tree-status","unavailable"),0)
                value.pop("crash_phase");config.write_text(json.dumps(value),encoding="utf-8")
                self.assertEqual(self.invoke(config,label="repair"),0);self.assert_ready(source,config)
                publisher.stop()

    def test_owned_corruption_repairs_in_place(self):
        publisher,source,config,value=self.fixture()
        self.assertEqual(self.invoke(config),0)
        path=config.parent/"build/packs/large.container";before=identity(path)
        with path.open("r+b") as stream:stream.seek(8192);stream.write(b"synthetic corruption")
        self.assertNotEqual(self.invoke(config,"tree-status"),0)
        self.assertEqual(self.invoke(config),0);self.assert_ready(source,config)
        self.assertTrue(before==identity(path))
        publisher.stop()

    def test_readonly_files_can_change_move_and_retire(self):
        publisher,source,config,value=self.fixture();publisher.stop()
        for name in ("app.dll","obsolete.txt","packs/large.container"):os.chmod(source/name,0o444)
        value["offer"]=publisher.start();config.write_text(json.dumps(value),encoding="utf-8")
        self.assertEqual(self.invoke(config),0);self.assert_ready(source,config)
        publisher.stop();previous=value["offer"]["artifact_id"]
        os.chmod(source/"app.dll",0o600)
        with (source/"app.dll").open("r+b") as stream:stream.seek(100);stream.write(b"synthetic replacement")
        os.chmod(source/"app.dll",0o444)
        os.chmod(source/"obsolete.txt",0o600);(source/"obsolete.txt").unlink()
        (source/"packs/large.container").rename(source/"packs/renamed.readonly")
        value.update(offer=publisher.start(),previous_artifact_id=previous,delivery_id=uuid.uuid4().hex)
        config.write_text(json.dumps(value),encoding="utf-8")
        self.assertEqual(self.invoke(config),0);self.assert_ready(source,config);publisher.stop()
        # The fixture owns its generated files; restore permissions for cleanup.
        for root in (source,config.parent/"build"):
            for path in root.rglob("*"):
                if path.is_file():os.chmod(path,0o600)

    def test_repeated_generations_readd_and_file_directory_changes(self):
        publisher,source,config,value=self.fixture()
        self.assertEqual(self.invoke(config),0)
        original_pack=identity(config.parent/"build/packs/large.container")
        for generation in range(1,4):
            publisher.stop();previous=value["offer"]["artifact_id"]
            if generation==1:
                (source/"obsolete.txt").unlink()
                (source/"empty.dat").unlink();(source/"empty.dat").mkdir()
                (source/"empty.dat/nested.txt").write_text("new synthetic directory",encoding="utf-8")
            elif generation==2:
                (source/"obsolete.txt").write_text("re-added synthetic data",encoding="utf-8")
                (source/"empty.dat/nested.txt").rename(source/"nested-stage")
                (source/"empty.dat").rmdir();(source/"nested-stage").rename(source/"empty.dat")
            else:
                (source/"packs/large.container").rename(source/"pack-stage")
                (source/"packs").rmdir();(source/"pack-stage").rename(source/"packs")
                payload=(source/"empty.dat").read_bytes();(source/"empty.dat").unlink();(source/"empty.dat").mkdir()
                (source/"empty.dat/final.txt").write_bytes(payload)
            value.update(offer=publisher.start(),previous_artifact_id=previous,delivery_id=uuid.uuid4().hex)
            config.write_text(json.dumps(value),encoding="utf-8")
            self.assertEqual(self.invoke(config,label="generation"),0)
            self.assert_ready(source,config)
        self.assertTrue(identity(config.parent/"build/packs")==original_pack)
        self.assertEqual(list((config.parent/"relocations").glob("*.bin")),[])

    def test_rename_and_removal_crashes_keep_native_custody(self):
        for phase in ("tree_move_stage","tree_move","tree_remove"):
            with self.subTest(phase=phase):
                publisher,source,config,value=self.fixture()
                self.assertEqual(self.invoke(config),0)
                publisher.stop();previous=value["offer"]["artifact_id"]
                (source/"packs/large.container").rename(source/"packs/renamed.bundle")
                (source/"obsolete.txt").unlink()
                value.update(offer=publisher.start(),previous_artifact_id=previous,delivery_id=uuid.uuid4().hex,crash_phase=phase)
                config.write_text(json.dumps(value),encoding="utf-8")
                self.assertEqual(self.invoke(config,label="crash"),86)
                value.pop("crash_phase");config.write_text(json.dumps(value),encoding="utf-8")
                self.assertEqual(self.invoke(config,label="repair"),0);self.assert_ready(source,config)
                publisher.stop()

    def test_unmanaged_and_replaced_files_never_become_ready(self):
        publisher,source,config,value=self.fixture()
        self.assertEqual(self.invoke(config),0)
        unrelated=config.parent/"build/user-notes.txt";unrelated.write_text("unmanaged local data",encoding="utf-8")
        self.assertNotEqual(self.invoke(config,"tree-status","unknown"),0)
        self.assertTrue(unrelated.is_file())
        unrelated.unlink()
        target=config.parent/"build/app.dll";target.rename(config.parent/"prior-app.dll")
        target.write_bytes(b"unrelated replacement")
        self.assertNotEqual(self.invoke(config,"tree-status","replaced"),0)
        self.assertTrue(target.read_bytes()==b"unrelated replacement")

    def test_explicit_single_file_generation_transition_recovers_metadata_boundaries(self):
        for phase in ("generation_intent","generation_index","generation_manifest","generation_receipt"):
            with self.subTest(phase=phase):
                temporary=tempfile.TemporaryDirectory();self.addCleanup(temporary.cleanup)
                base=Path(temporary.name);source=base/"source.bin";source.write_bytes(random.Random(14).randbytes(4*MIB))
                publisher=Publisher(base,source,"insert");self.addCleanup(publisher.stop);first=publisher.start()
                slot=base/"slot";slot.mkdir();config=slot/"config.json"
                value={"offer":first,"delivery_id":uuid.uuid4().hex,"stash_bytes":MIB,"memory_bytes":128*MIB}
                config.write_text(json.dumps(value),encoding="utf-8")
                self.assertEqual(self.invoke(config,"receive","first"),0)
                old_identity=identity(slot/"receiver.bin");publisher.stop();publisher.value["scenario"]="swap"
                value["offer"]=publisher.start();value["previous_artifact_id"]=first["artifact_id"];value["delivery_id"]=uuid.uuid4().hex;value["crash_phase"]=phase
                config.write_text(json.dumps(value),encoding="utf-8")
                self.assertEqual(self.invoke(config,"receive","generation-crash"),86)
                self.assertNotEqual(self.invoke(config,"receive-status","unavailable"),0)
                value.pop("crash_phase");config.write_text(json.dumps(value),encoding="utf-8")
                self.assertEqual(self.invoke(config,"receive","repair"),0)
                self.assertEqual(self.invoke(config,"receive-status","ready"),0)
                self.assertTrue(identity(slot/"receiver.bin")==old_identity)
                publisher.stop()


if __name__=="__main__":unittest.main()
