"""Mutually pinned TLS qualification over loopback with synthetic data."""
import json
from pathlib import Path
import tempfile
import unittest

from qualify import child,MIB
from test_transfer import BINARY,read
from test_tree import TreePublisher


@unittest.skipUnless(BINARY and BINARY.is_file(),"set ZERUS_DESYNC_TEST_BINARY to the isolated helper")
class PairedPeerTests(unittest.TestCase):
    def identity(self,root,name):
        folder=root/name;folder.mkdir();config=folder/"init.json";config.write_text("{}",encoding="utf-8")
        result=child(BINARY,"identity-init",config,"identity",memory_bytes=128*MIB,deadline=15)
        self.assertEqual(result["exit_code"],0)
        return folder/"identity",read(folder/"identity/public.json")["fingerprint"]

    def fixture(self):
        temporary=tempfile.TemporaryDirectory();self.addCleanup(temporary.cleanup)
        root=Path(temporary.name)
        server,server_pin=self.identity(root,"server-identity")
        client,client_pin=self.identity(root,"client-identity")
        source=root/"source";source.mkdir();(source/"example.bundle").write_bytes(b"synthetic encrypted-channel data"*1024)
        publisher=TreePublisher(root,source);self.addCleanup(publisher.stop)
        publisher.value["peer"]={"identity":str(server),"fingerprint":client_pin,"listen":"127.0.0.1:0"}
        offer=publisher.start()
        slot=root/"slot";slot.mkdir();config=slot/"config.json"
        value={"offer":offer,"delivery_id":"1234567890abcdef1234567890abcdef","stash_bytes":MIB,"memory_bytes":128*MIB,
               "peer":{"identity":str(client),"fingerprint":server_pin}}
        config.write_text(json.dumps(value),encoding="utf-8")
        return root,publisher,source,config,value

    def invoke(self,config,label="tls"):
        return child(BINARY,"tree-receive",config,label,memory_bytes=256*MIB,deadline=20)["exit_code"]

    def test_paired_peer_delivers_and_private_keys_stay_local(self):
        root,publisher,source,config,value=self.fixture()
        self.assertTrue(value["offer"]["endpoint"].startswith("https://127.0.0.1:"))
        self.assertNotIn("identity",json.dumps(value["offer"]))
        self.assertEqual(self.invoke(config),0)
        self.assertTrue((config.parent/"build/example.bundle").read_bytes()==(source/"example.bundle").read_bytes())
        publisher.stop()
        self.assertEqual(self.invoke(config,"offline"),0)

    def test_unknown_server_client_and_plaintext_downgrade_are_refused(self):
        root,publisher,_,config,value=self.fixture()
        stranger,stranger_pin=self.identity(root,"stranger")
        variants=[]
        wrong_server=json.loads(json.dumps(value));wrong_server["peer"]["fingerprint"]="0"*64;variants.append(wrong_server)
        wrong_client=json.loads(json.dumps(value));wrong_client["peer"]["identity"]=str(stranger);variants.append(wrong_client)
        downgrade=json.loads(json.dumps(value));downgrade["offer"]["endpoint"]=downgrade["offer"]["endpoint"].replace("https:","http:");variants.append(downgrade)
        public=json.loads(json.dumps(value));public["offer"]["endpoint"]="https://192.0.2.1:443";variants.append(public)
        unpaired=json.loads(json.dumps(value));unpaired.pop("peer");variants.append(unpaired)
        for variant in variants:
            config.write_text(json.dumps(variant),encoding="utf-8")
            self.assertNotEqual(self.invoke(config,"refused"),0)
            self.assertFalse((config.parent/"tree-installed.json").exists())
        config.write_text(json.dumps(value),encoding="utf-8")
        self.assertEqual(self.invoke(config,"authorized"),0)


if __name__=="__main__":unittest.main()
