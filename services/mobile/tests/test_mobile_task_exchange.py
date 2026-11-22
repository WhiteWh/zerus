"""End to end: a phone exchanges a project's tasks with a computer through the
Rust relay, the Rust connector, the real hgs and the apps' replica bindings
(docs/superpowers/specs/2026-10-18-task-replication-loro-design.md, "Phone").

hgs runs with a private home, configuration and state; its tmux is a stub that
fails, and the connector's session listing is answered without it. The phone
opens a private projection store through the same bindings as the apps.
"""
import asyncio
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
import uuid

from aiohttp import ClientSession

from e2e_connector import COMMAND, REQUIRED, running
from relay_fixture import start_relay

NATIVE = "ZERUS_TASK_TEST_HGS"
PHONE = "ZERUS_TASK_TEST_PHONE"
CATALOG = {"projects": [{"id": "project-1", "name": "orchard", "color": "#123456", "folders": [], "sessions": []}]}


class TaskExchangeEndToEnd(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        for variable, what in ((NATIVE, "an isolated native CLI"), (COMMAND, "a built Rust connector")):
            if not os.environ.get(variable):
                if os.environ.get(REQUIRED) == "1":
                    self.fail(f"{REQUIRED}=1: this suite needs {variable} naming {what}")
                self.skipTest(f"{variable} names no {what}")
        self.native = str(Path(os.environ[NATIVE]).resolve())
        directory = tempfile.TemporaryDirectory(prefix="zerus-task-exchange-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.computer = self.replica("computer")
        relay = await start_relay(rust_only=True)
        self.addAsyncCleanup(relay.cleanup)
        self.relay = relay
        self.phone = await relay.pair("Example phone", relay.invitation["pair_code"])
        self.client = ClientSession(base_url=relay.url)
        self.addAsyncCleanup(self.client.close)

    def replica(self, name: str) -> dict:
        """The environment of an isolated hgs whose catalog has project orchard."""
        home = self.root / name / "home"
        (home / ".local/bin").mkdir(parents=True)
        tmux = home / ".local/bin/tmux"
        tmux.write_text("#!/bin/sh\nexit 1\n")
        tmux.chmod(0o755)
        (self.root / name / "tmux").mkdir(mode=0o700)
        env = {"HOME": str(home), "PATH": "/usr/bin:/bin", "TMUX_TMPDIR": str(self.root / name / "tmux"),
               "HGS_CONFIG_DIR": str(self.root / name / "config"), "HGS_STATE_DIR": str(self.root / name / "state")}
        self.hgs(env, "swarm", "initialize", data=CATALOG)
        return env

    def hgs(self, env: dict, *args: str, data=None, text: str | None = None):
        stdin = text if data is None else json.dumps(data)
        result = subprocess.run([self.native, *args], input=stdin, env=env, text=True, capture_output=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    async def call(self, method: str, route: str, body=None, expected: int = 200):
        headers = {"Authorization": "Bearer " + self.phone["device_token"]}
        async with self.client.request(method, route, json=body, headers=headers) as response:
            self.assertEqual(response.status, expected, await response.text())
            return await response.json()

    async def computer_snapshot(self, ready, what: str) -> dict:
        for _ in range(400):
            computers = (await self.call("GET", "/v1/computers"))["computers"]
            snapshot = computers[0].get("snapshot") or {}
            if computers[0]["online"] and ready(snapshot):
                return snapshot
            await asyncio.sleep(0.05)
        self.fail(f"the computer's heartbeat never showed {what}")

    async def submit(self, operation: str, payload: dict) -> dict:
        request_id = str(uuid.uuid4())
        body = {"request_id": request_id, "computer_id": self.relay.node["node_id"], "operation": operation,
                "session": "", "payload": payload}
        receipt = await self.call("POST", "/v1/requests", body, expected=202)
        for _ in range(400):
            if receipt["state"] not in ("queued", "claimed"):
                break
            await asyncio.sleep(0.05)
            receipt = await self.call("GET", "/v1/requests/" + request_id)
        return receipt

    async def exchange(self, payload: dict) -> dict:
        receipt = await self.submit("task_exchange", payload)
        self.assertEqual(receipt["state"], "completed", receipt)
        return receipt["result"]

    async def phone_exchange(self, program, *, add=None):
        args = (add,) if add is not None else ()
        process = await asyncio.create_subprocess_exec(
            program, str(self.root / "phone"), "project-1", "orchard", *args,
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE, cwd=self.root, limit=1 << 20,
        )
        answers = []
        try:
            while line := await asyncio.wait_for(process.stdout.readline(), timeout=30):
                message = json.loads(line)
                if "request" not in message:
                    self.assertEqual(await asyncio.wait_for(process.wait(), timeout=5), 0)
                    return message, answers
                answer = await self.exchange(message["request"])
                answers.append(answer)
                process.stdin.write(json.dumps(answer).encode() + b"\n")
                await process.stdin.drain()
            error = await asyncio.wait_for(process.stderr.read(), timeout=5)
            self.fail(f"the phone replica stopped before reporting its digest: {error.decode()}")
        finally:
            if process.returncode is None:
                process.kill()
                await process.wait()

    def connector(self):
        """The Rust connector of this computer; its session listing comes from a
        wrapper, everything else from the real hgs."""
        listing = self.root / "bin/hgs"
        if not listing.exists():
            listing.parent.mkdir()
            listing.write_text("#!/bin/sh\nif [ \"$1\" = ls ]; then\n"
                               "  echo '{\"host\": \"Synthetic computer\", \"sessions\": []}'\n  exit 0\nfi\n"
                               f"exec '{self.native}' \"$@\"\n")
            listing.chmod(0o755)
        config = {"server_url": self.relay.url, "node_token": self.relay.node["node_token"],
                  "hgs_path": str(listing), "state_dir": str(self.root / "journal"), "poll_interval": 1}
        return running("rust", config, self.root, self.computer)

    async def test_a_phone_exchanges_tasks_through_the_relay_and_the_rust_connector(self):
        phone_program = os.environ.get(PHONE)
        if not phone_program:
            if os.environ.get(REQUIRED) == "1":
                self.fail(f"{REQUIRED}=1: this suite needs {PHONE} naming the phone replica fixture")
            self.skipTest(f"{PHONE} names no built phone replica fixture")
        phone_program = str(Path(phone_program).resolve())
        self.assertIn("task_exchange", (await self.call("GET", "/v1/capabilities"))["operations"])
        self.hgs(self.computer, "task", "add", "--project", "orchard", text="Add rate limiting")
        async with self.connector() as connector:
            def ready(snapshot):
                projects = (snapshot.get("mobile_projects") or {}).get("projects") or []
                operations = (snapshot.get("mobile_capabilities") or {}).get("operations") or []
                return "task_exchange" in operations and projects and "tasks_digest" in projects[0]
            snapshot = await self.computer_snapshot(ready, "the task exchange and a tasks digest")
            connector.check()
            here = self.hgs(self.computer, "task", "digests", "--json")["projects"]["project-1"]["digest"]
            self.assertEqual(snapshot["mobile_projects"]["projects"][0]["tasks_digest"], here)
            # An empty phone gets the live task (the project document was never saved).
            [task] = self.hgs(self.computer, "task", "list", "--project", "orchard")["tasks"]
            received, answers = await self.phone_exchange(phone_program)
            answer = answers[0]
            self.assertEqual((answer["more"], answer["digest"], answer["refused"]), (False, here, []))
            self.assertEqual([update["task"] for update in answer["updates"]], [task["uuid"]])
            self.assertEqual(received["changed"], 1)
            self.assertEqual(received["tasks"], [{
                "uuid": task["uuid"], "text": "Add rate limiting", "writable": False, "schema": task["schema"],
            }])
            self.assertEqual(received["digest"], here)
            # File through the app's local writer, then send what the computer lacks.
            after, answers = await self.phone_exchange(phone_program, add="Filed on the phone")
            self.assertEqual(sum(answer["imported"] for answer in answers), 1)
            filed = next(task for task in after["tasks"] if task["text"] == "Filed on the phone")
            tasks = self.hgs(self.computer, "task", "list", "--project", "orchard")["tasks"]
            self.assertEqual({row["uuid"]: row["text"] for row in tasks},
                             {task["uuid"]: "Add rate limiting", filed["uuid"]: "Filed on the phone"})
            # The computer's next heartbeat names the new digest.
            changed = self.hgs(self.computer, "task", "digests", "--json")["projects"]["project-1"]["digest"]
            self.assertNotEqual(changed, here)
            self.assertEqual(after["digest"], changed)
            await self.computer_snapshot(
                lambda snapshot: snapshot["mobile_projects"]["projects"][0].get("tasks_digest") == changed,
                "the changed tasks digest")
            connector.check()


    async def test_send_now_cancel_and_done_answer_through_the_relay(self):
        for operation in ("task_send", "task_cancel", "task_done"):
            self.assertIn(operation, (await self.call("GET", "/v1/capabilities"))["operations"])
        task = self.hgs(self.computer, "task", "add", "--project", "orchard", text="Add rate limiting")["task"]
        async with self.connector() as connector:
            def ready(snapshot):
                operations = (snapshot.get("mobile_capabilities") or {}).get("operations") or []
                return {"task_send", "task_cancel", "task_done"} <= set(operations)
            await self.computer_snapshot(ready, "the phone's task actions")
            connector.check()
            done = await self.submit("task_done", {"schema": 1, "project": "project-1", "limit": 50})
            self.assertEqual(done["state"], "completed", done)
            self.assertEqual((done["result"]["tasks"], done["result"]["total"], done["result"]["next"]), ([], 0, None))
            # Only this session: the Done tasks sent to one session.
            mine = await self.submit("task_done", {"schema": 1, "project": "project-1", "limit": 50,
                                                   "session": {"machine": "node-1", "session": "codex/api"}})
            self.assertEqual((mine["state"], mine["result"]["total"]), ("completed", 0), mine)
            # The computer's own hgs refuses before anything happens: the receipt says why.
            sent = await self.submit("task_send", {"schema": 1, "project": "project-1", "task": task["uuid"],
                                                    "session": "codex/nowhere"})
            self.assertEqual(sent["state"], "failed", sent)
            self.assertTrue(sent["error"], sent)
            self.assertNotEqual(sent["error"], "native command failed")
            cancelled = await self.submit("task_cancel", {"schema": 1, "project": "project-1", "task": task["uuid"]})
            self.assertEqual((cancelled["state"], cancelled["error"]),
                             ("failed", "only a task In progress can be cancelled"))
            self.assertEqual(self.hgs(self.computer, "task", "get", task["ref"])["task"]["stage"], "todo")
            connector.check()


if __name__ == "__main__":
    unittest.main()
