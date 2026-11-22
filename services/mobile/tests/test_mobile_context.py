"""Context controls execute only against synthetic native ABIs."""
import copy
import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import AsyncMock, patch
import uuid

from aiohttp.test_utils import TestClient, TestServer

from zerus_mobile.connector import Connector, ConnectorError
from zerus_mobile.context import OPERATIONS, validate_core
from zerus_mobile.send_outcome import OPERATIONS as SEND_OUTCOME_OPERATIONS
from zerus_mobile.speech import OPERATIONS as SPEECH_OPERATIONS
from zerus_mobile.speech_config import OPERATIONS as SPEECH_CONFIG_OPERATIONS
from zerus_mobile.voice import OPERATIONS as VOICE_OPERATIONS
from zerus_mobile.server import Config, create_app
from zerus_mobile.store import Store

NAME = "codex/example/mobile"


def command(operation="compact_context", **extra):
    request_id = str(uuid.uuid4())
    payload = {"request_id": request_id, "expected_run_id": "fixture-run", "expected_conversation_id": "fixture-conversation", **extra}
    return {"request_id": request_id, "operation": operation, "session": NAME, "payload": payload}


def detail():
    return {"name": NAME, "run_id": "fixture-run", "conversation_id": "fixture-conversation",
            "phase": "idle", "activity": "idle", "compact_context_supported": True, "clear_context_supported": True}


def receipt(request):
    return {"request_id": request["request_id"], "name": NAME, "run_id": "fixture-run",
            "conversation_id": "fixture-conversation", "status": "submitted"}


class ContextConnectorTests(unittest.IsolatedAsyncioTestCase):
    def test_model_settings_use_the_same_utf8_bound_as_native_launch(self):
        for model in ('ollama/' + 'a' * 249, 'ollama/' + '界' * 83):
            request = command('settings', model=model)
            validate_core('settings', request['payload'], request['request_id'])
        for model in ('ollama/' + 'a' * 250, 'ollama/' + '界' * 84):
            request = command('settings', model=model)
            with self.assertRaises(ValueError):
                validate_core('settings', request['payload'], request['request_id'])

    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.connector = Connector({"server_url": "https://relay.example.invalid", "node_token": "fixture-token", "state_dir": self.temp.name})
        self.connector.sessions = {NAME}

    async def asyncTearDown(self):
        self.connector.journal.close()
        self.temp.cleanup()

    async def test_fixed_argv_and_strict_native_acknowledgement(self):
        for operation, native_name in (("compact_context", "compact-context"), ("clear_context", "clear-context")):
            request = command(operation)
            with patch.object(self.connector, "native", AsyncMock(side_effect=[detail(), receipt(request)])) as native:
                response = await self.connector.execute(request)
                self.assertEqual(response["state"], "completed")
                self.assertEqual(response["result"]["status"], "submitted")
                self.assertEqual(native.call_args_list[0].args, (["inspect", NAME],))
                self.assertEqual(native.call_args_list[1].args, ([native_name, NAME, "--json"], request["payload"]))

    async def test_invalid_payload_archive_and_unknown_operation_never_spawn(self):
        values = [command(text="/clear"), command(archive_id=str(uuid.uuid4())), command(expected_run_id=""),
                  command(expected_conversation_id=""), command(expected_conversation_id=1), command("compact-context")]
        wrong_id = command()
        wrong_id["payload"]["request_id"] = str(uuid.uuid4())
        values.append(wrong_id)
        with patch.object(self.connector, "native", AsyncMock()) as native:
            for value in values:
                self.assertEqual((await self.connector.execute(value))["state"], "failed")
            native.assert_not_awaited()

    async def test_fresh_identity_and_native_support_rejection_before_mutation(self):
        for fields in ({"name": "other"}, {"run_id": "other"}, {"conversation_id": "other"},
                       {"compact_context_supported": False}, {"compact_context_supported": 1},
                       {"archive_id": str(uuid.uuid4())}):
            with patch.object(self.connector, "native", AsyncMock(return_value={**detail(), **fields})) as native:
                response = await self.connector.execute(command())
                self.assertEqual(response["state"], "failed")
                self.assertEqual(native.await_count, 1)
                self.assertEqual(native.call_args.args[0], ["inspect", NAME])
        with patch.object(self.connector, "native", AsyncMock(side_effect=ConnectorError("inspect failed"))):
            self.assertEqual((await self.connector.execute(command()))["state"], "failed")

    async def test_mismatched_or_unknown_ack_is_uncertain_and_never_replayed(self):
        for fields in ({"status": "completed"}, {"request_id": str(uuid.uuid4())}, {"name": "other"},
                       {"run_id": "other"}, {"conversation_id": "other"}):
            request = command()
            with patch.object(self.connector, "native", AsyncMock(side_effect=[detail(), {**receipt(request), **fields}])) as native:
                first = await self.connector.execute(request)
                self.assertEqual(first["state"], "uncertain")
                self.assertEqual(await self.connector.execute(request), first)
                self.assertEqual(native.await_count, 2)

    async def test_timeout_after_invocation_and_duplicate_claim_do_not_replay(self):
        request = command("clear_context")
        with patch.object(self.connector, "native", AsyncMock(side_effect=[detail(), ConnectorError("native command timed out")])) as native:
            response = await self.connector.execute(request)
            self.assertEqual(response["state"], "uncertain")
            self.assertEqual(await self.connector.execute(request), response)
            self.assertEqual(native.await_count, 2)
        claimed = command()
        request_hash = hashlib.sha256(json.dumps(claimed, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()).hexdigest()
        self.connector.journal.claim(claimed["request_id"], "compact_context", request_hash)
        with patch.object(self.connector, "native", AsyncMock()) as native:
            self.assertEqual((await self.connector.execute(claimed))["state"], "uncertain")
            native.assert_not_awaited()

    async def test_continuation_requires_exact_native_completion_and_idle(self):
        compact_id = str(uuid.uuid4())
        compact = {"request_id": compact_id, "run_id": "fixture-run", "conversation_id": "fixture-conversation", "status": "completed"}
        for fields in ({"status": "submitted"}, {"status": "uncertain"}, {"request_id": str(uuid.uuid4())},
                       {"run_id": "other"}, {"conversation_id": "other"}):
            with patch.object(self.connector, "native", AsyncMock(return_value={**detail(), "compact_context_request": {**compact, **fields}})) as native:
                response = await self.connector.execute(command("send", text="Kept draft", expected_compaction_id=compact_id))
                self.assertEqual(response["state"], "failed")
                self.assertEqual(native.await_count, 1)
        with patch.object(self.connector, "native", AsyncMock(return_value={**detail(), "phase": "working", "compact_context_request": compact})):
            self.assertEqual((await self.connector.execute(command("send", text="Kept draft", expected_compaction_id=compact_id)))["state"], "failed")
        request = command("send", text="Kept draft", expected_compaction_id=compact_id)
        with patch.object(self.connector, "native", AsyncMock(side_effect=[{**detail(), "compact_context_request": compact}, {"status": "submitted"}])) as native:
            self.assertEqual((await self.connector.execute(request))["state"], "completed")
            self.assertEqual(native.call_args.args, (["send", NAME, "--json"], request["payload"]))

    async def test_ordinary_old_send_needs_no_context_preflight(self):
        with patch.object(self.connector, "native", AsyncMock(return_value={"status": "submitted"})) as native:
            self.assertEqual((await self.connector.execute(command("send", text="Ordinary")))["state"], "completed")
            self.assertEqual(native.await_count, 1)

    async def test_archived_only_name_never_allows_a_context_command(self):
        self.connector.sessions.clear()
        self.connector.archives = {(NAME, str(uuid.uuid4()))}
        with patch.object(self.connector, "native", AsyncMock()) as native:
            for operation in ("compact_context", "clear_context"):
                self.assertEqual((await self.connector.execute(command(operation)))["state"], "failed")
            native.assert_not_awaited()


class ContextRelayTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.store = Store(Path(self.temp.name) / "relay.sqlite")
        workspace = self.store.workspace("Synthetic")
        self.node = self.store.node(workspace, "Example")
        self.phone = self.store.pair(self.store.invite(workspace)["pair_code"], "Test phone")
        self.client = TestClient(TestServer(create_app(self.store, Config(background=False))))
        await self.client.start_server()

    async def asyncTearDown(self):
        await self.client.close()
        self.store.close()
        self.temp.cleanup()

    async def call(self, method, path, body=None, node=False):
        token = self.node["node_token"] if node else self.phone["device_token"]
        return await self.client.request(method, path, json=body, headers={"Authorization": "Bearer " + token})

    async def advertise(self, operations=None):
        snapshot = {"sessions": []}
        if operations is not None:
            snapshot["mobile_capabilities"] = {"protocol_version": 1, "operations": operations}
        self.assertEqual((await self.call("POST", "/v1/node/heartbeat", {"snapshot": snapshot}, True)).status, 200)

    def request(self, operation="compact_context", **extra):
        return {"computer_id": self.node["node_id"], **command(operation, **extra)}

    async def test_relay_declares_ops_old_nodes_reject_controls_but_accept_ordinary_send(self):
        caps = await (await self.call("GET", "/v1/capabilities")).json()
        self.assertEqual(set(caps["operations"]), OPERATIONS)
        for operations in (None, [], ["inspect", "send"]):
            await self.advertise(operations)
            for op in ("compact_context", "clear_context"):
                self.assertEqual((await self.call("POST", "/v1/requests", self.request(op))).status, 409)
        self.assertEqual((await self.call("POST", "/v1/requests", self.request("send", text="Legacy send"))).status, 202)

    async def test_valid_request_duplicate_receipt_survives_capability_change(self):
        await self.advertise(sorted(OPERATIONS))
        request = self.request()
        first = await self.call("POST", "/v1/requests", request)
        self.assertEqual(first.status, 202)
        claimed = await (await self.call("GET", "/v1/node/requests", node=True)).json()
        self.assertEqual(claimed["requests"][0]["payload"], request["payload"])
        await self.advertise([])
        retry = await self.call("POST", "/v1/requests", request)
        self.assertEqual(retry.status, 202)
        self.assertEqual((await retry.json())["state"], "claimed")
        different = copy.deepcopy(request)
        different["operation"] = "clear_context"
        self.assertEqual((await self.call("POST", "/v1/requests", different)).status, 409)
        self.assertEqual((await (await self.call("GET", "/v1/node/requests", node=True)).json())["requests"], [])

    async def test_node_declares_each_operation_and_protocol_explicitly(self):
        await self.advertise(["compact_context"])
        self.assertEqual((await self.call("POST", "/v1/requests", self.request())).status, 202)
        self.assertEqual((await self.call("POST", "/v1/requests", self.request("clear_context"))).status, 409)
        for version in (True, 1.0, 2, "1"):
            snapshot = {"sessions": [], "mobile_capabilities": {"protocol_version": version, "operations": sorted(OPERATIONS)}}
            self.assertEqual((await self.call("POST", "/v1/node/heartbeat", {"snapshot": snapshot}, True)).status, 200)
            self.assertEqual((await self.call("POST", "/v1/requests", self.request())).status, 409)

    async def test_relay_rejects_extra_archive_invalid_identity_and_compaction_id(self):
        await self.advertise(sorted(OPERATIONS))
        for request in (self.request(text="/compact"), self.request(archive_id=str(uuid.uuid4())),
                        self.request(expected_run_id=""), self.request(expected_conversation_id=""),
                        self.request(expected_conversation_id=3), self.request("compact-context"),
                        self.request("send", text="Hello", expected_compaction_id="../../file"),
                        self.request("send", text="Hello", expected_compaction_id=3)):
            self.assertEqual((await self.call("POST", "/v1/requests", request)).status, 400)


class ContextFixtureTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.env = patch.dict(os.environ, {"ZERUS_MOBILE_FIXTURE_DIR": str(self.root)})
        self.env.start()
        self.connector = Connector({"server_url": "https://relay.example.invalid", "node_token": "fixture-token",
            "hgs_path": str(Path(__file__).with_name("fixture_hgs.py")), "state_dir": str(self.root / "journal")})
        await self.connector.snapshot()

    async def asyncTearDown(self):
        self.connector.journal.close()
        self.env.stop()
        self.temp.cleanup()

    def config(self, **values):
        (self.root / "fixture-config.json").write_text(json.dumps(values))

    def mutations(self):
        return [json.loads(line) for line in (self.root / "mutations.jsonl").read_text().splitlines()]

    async def test_fixture_confirmed_compact_continue_exactly_one_compact_and_send(self):
        self.config(compact_delay=0)
        request = command()
        first = await self.connector.execute(request)
        self.assertEqual(first["result"]["status"], "submitted")
        self.assertEqual(await self.connector.execute(request), first)
        inspect = await self.connector.native(["inspect", NAME])
        self.assertEqual(inspect["compact_context_request"]["status"], "completed")
        self.assertEqual(inspect["compact_context_request"]["request_id"], request["request_id"])
        send = command("send", text="Kept synthetic draft", expected_compaction_id=request["request_id"])
        self.assertEqual((await self.connector.execute(send))["state"], "completed")
        await self.connector.execute(send)
        self.assertEqual([m["operation"] for m in self.mutations()], ["compact-context", "send"])

    async def test_fixture_pending_and_failed_compaction_do_not_send(self):
        self.config(compact_delay=30)
        request = command()
        await self.connector.execute(request)
        inspect = await self.connector.native(["inspect", NAME])
        self.assertEqual(inspect["compact_context_request"]["status"], "compacting")
        response = await self.connector.execute(command("send", text="Kept draft", expected_compaction_id=request["request_id"]))
        self.assertEqual(response["state"], "failed")
        self.config(compact_delay=0, compact_status="unchanged")
        inspect = await self.connector.native(["inspect", NAME])
        self.assertEqual(inspect["compact_context_request"]["status"], "unchanged")
        self.assertEqual(len(self.mutations()), 1)

    async def test_clear_confirmed_receipt_old_identity_and_fresh_new_conversation(self):
        request = command("clear_context")
        response = await self.connector.execute(request)
        self.assertEqual(response["state"], "completed")
        self.assertEqual(response["result"]["status"], "confirmed")
        self.assertEqual(response["result"]["conversation_id"], "fixture-conversation")
        inspect = await self.connector.native(["inspect", NAME])
        self.assertEqual(inspect["run_id"], "fixture-run")
        self.assertNotEqual(inspect["conversation_id"], response["result"]["conversation_id"])
        self.assertEqual(inspect["events"], [])
        self.assertEqual(inspect["session_usage"]["context"]["used"], 0)
        await self.connector.execute(request)
        self.assertEqual(len(self.mutations()), 1)

    async def test_fixture_telemetry_passthrough_capabilities_and_all_states(self):
        usage = {"status": "ok", "context": {"used": 60000, "limit": 200000}, "prompt_cache": {"status": "warm", "expires_at": 1}}
        hint = {"status": "cold", "source": "native_footer", "tokens": 60000}
        self.config(all_states=True, session_usage=usage, cache_hint=hint)
        snapshot = await self.connector.snapshot()
        # The synthetic CLI lacks the send-outcome markers and these optional ABIs.
        self.assertFalse(self.connector.history_projection_supported)
        self.assertNotIn("history_projection", snapshot["mobile_capabilities"]["operations"])
        self.assertSetEqual(
            set(snapshot["mobile_capabilities"]["operations"]),
            OPERATIONS - SEND_OUTCOME_OPERATIONS - SPEECH_OPERATIONS - SPEECH_CONFIG_OPERATIONS
            - VOICE_OPERATIONS - {"history_projection", "session_notifications", "directory_create",
                                  "file_read", "archive_list", "hooks_status", "analytics_read",
                                  "project_assignment", "project_assign"},
        )
        self.assertEqual(len(snapshot["sessions"]), 7)
        inspect = await self.connector.native(["inspect", NAME])
        self.assertEqual(inspect["session_usage"], usage)
        self.assertEqual(inspect["cache_hint"], hint)

    async def test_fixture_uncertain_receipt_records_only_one_mutation(self):
        self.config(context_failure_after_submit=True)
        request = command()
        first = await self.connector.execute(request)
        self.assertEqual(first["state"], "uncertain")
        self.assertEqual(await self.connector.execute(request), first)
        self.assertEqual(len(self.mutations()), 1)
        inspect = await self.connector.native(["inspect", NAME])
        self.assertEqual(inspect["compact_context_request"]["request_id"], request["request_id"])

    async def test_fixture_submitted_clear_does_not_change_identity(self):
        self.config(clear_status="submitted")
        request = command("clear_context")
        first = await self.connector.execute(request)
        self.assertEqual(first["state"], "completed")
        self.assertEqual(first["result"]["status"], "submitted")
        inspect = await self.connector.native(["inspect", NAME])
        self.assertEqual(inspect["conversation_id"], first["result"]["conversation_id"])
