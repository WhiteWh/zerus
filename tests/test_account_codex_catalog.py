#!/usr/bin/env python3
"""Opt-in integration checks using two real Codex binaries and loopback OAuth/models."""
import base64
from contextlib import contextmanager
import hashlib
import http.server
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import tempfile
import threading
import time
import unittest
from urllib.parse import urlsplit


HGS = Path(os.environ.get("HGS_TEST_BIN", "target/debug/hgs")).resolve()
CODEX = os.environ.get("HGS_TEST_CODEX_CATALOG")
OLD_CODEX = os.environ.get("HGS_TEST_CODEX_CATALOG_OLD")
CURRENT_MODEL = "fixture-catalog-current"
PREVIOUS_MODEL = "fixture-catalog-previous"
ACCOUNT = "fixture-catalog-account"


def jwt(expires):
    def segment(value):
        return base64.urlsafe_b64encode(json.dumps(value).encode()).decode().rstrip("=")

    claims = {
        "exp": expires,
        "email": "codex@example.invalid",
        "https://api.openai.com/auth": {
            "chatgpt_account_id": ACCOUNT,
            "chatgpt_plan_type": "pro",
            "chatgpt_user_id": "fixture-user",
        },
    }
    return f'{segment({"alg": "RS256"})}.{segment(claims)}.fixture'


def model_row(slug):
    # This is a fixture backend response. Only Codex serializes the cache.
    return {
        "slug": slug,
        "display_name": slug,
        "supported_reasoning_levels": [{"effort": "xhigh", "description": "Fixture"}],
        "default_reasoning_level": "xhigh",
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": True,
        "priority": 0,
        "base_instructions": "Fixture catalog without inference.",
        "support_verbosity": False,
        "truncation_policy": {"mode": "bytes", "limit": 10000},
        "context_window": 16000,
        "experimental_supported_tools": [],
    }


class Backend(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def reply(self, value, status=200):
        data = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        try:
            self.wfile.write(data)
        except BrokenPipeError:
            # Codex can close a background request during normal app-server shutdown.
            pass

    def do_GET(self):
        path = urlsplit(self.path)
        if path.path != "/fixture/v1/models":
            self.reply({}, 404)
            return
        self.server.model_queries.append(path.query)
        print("loopback models GET:", path.query, flush=True)
        token = self.headers.get("Authorization", "").removeprefix("Bearer ")
        try:
            claims = json.loads(base64.urlsafe_b64decode(token.split(".")[1] + "=="))
        except (ValueError, IndexError):
            claims = {}
        if claims.get("exp", 0) <= time.time():
            self.reply({"error": {"message": "Fixture credential expired"}}, 401)
            return
        model = PREVIOUS_MODEL if path.query == "client_version=0.167.0" else self.server.model
        self.reply({"models": [model_row(model)]})

    def do_POST(self):
        path = urlsplit(self.path).path
        if path != "/oauth/token":
            self.reply({}, 404)
            return
        self.rfile.read(int(self.headers["Content-Length"]))
        self.server.renewals.append(path)
        print("loopback OAuth POST:", path, flush=True)
        if self.server.reject_renewal:
            self.reply({"error": "invalid_grant"}, 400)
            return
        self.reply({"access_token": jwt(int(time.time()) + 3600), "refresh_token": "fixture-renewed"})


@unittest.skipUnless(CODEX and OLD_CODEX, "set HGS_TEST_CODEX_CATALOG and HGS_TEST_CODEX_CATALOG_OLD")
class CodexCatalog(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="codex-catalog-")))
        self.home = self.root / "home"
        config = self.root / "hgs-config"
        tmux = self.root / "tmux"
        bin_dir = self.home / ".local/bin"
        bin_dir.mkdir(parents=True)
        tmux.mkdir(mode=0o700)
        (bin_dir / "codex").symlink_to(Path(CODEX).resolve())
        self.env = {
            "HOME": str(self.home),
            "PATH": os.pathsep.join((str(bin_dir), os.defpath)),
            "HGS_CONFIG_DIR": str(config),
            "HGS_STATE_DIR": str(self.root / "hgs-state"),
            "TMUX_TMPDIR": str(tmux),
            "TMPDIR": str(self.root),
        }
        server = self.enterContext(http.server.ThreadingHTTPServer(("127.0.0.1", 0), Backend))
        self.server = server
        self.server.model_queries = []
        self.server.renewals = []
        self.server.reject_renewal = False
        self.server.model = PREVIOUS_MODEL
        worker = threading.Thread(target=server.serve_forever)
        worker.start()
        self.addCleanup(worker.join)
        self.addCleanup(server.shutdown)
        base = f"http://127.0.0.1:{server.server_port}"
        self.env["CODEX_REFRESH_TOKEN_URL_OVERRIDE"] = base + "/oauth/token"
        listing = self.run_hgs("add", "fixture", "--provider", "codex", "--label", "Catalog fixture")
        self.host = listing["host"]
        self.account = config / "accounts/fixture"
        self.catalog = self.account / "models_cache.json"
        self.auth = self.account / "auth.json"
        (self.account / "config.toml").write_text(
            f'chatgpt_base_url = "{base}/backend-api/"\n'
            'cli_auth_credentials_store = "file"\n'
            'model_provider = "fixture"\n'
            '[model_providers.fixture]\n'
            'name = "Fixture"\n'
            f'base_url = "{base}/fixture/v1"\n'
            f'model_catalog_url = "{base}/fixture/v1/models"\n'
            'requires_openai_auth = true\n'
            "[analytics]\nenabled = false\n"
            "[features]\nplugins = false\nremote_plugin = false\n"
        )
        self.write_auth(int(time.time()) + 3600)
        with self.codex(OLD_CODEX) as call:
            self.assertIn("data", call("model/list", {"limit": 100}))
            self.assertEqual(call("thread/list", {"limit": 1})["data"], [])
        self.before = self.read_cache()
        self.assertEqual([row["slug"] for row in self.before["models"]], [PREVIOUS_MODEL])
        version = subprocess.check_output(
            [CODEX, "--version"], env=self.env, text=True, stderr=subprocess.DEVNULL,
        )
        self.version = version.strip().split()[1]
        self.assertNotEqual(self.version, self.before["client_version"])
        self.settings = self.run_hgs("model-settings", "fixture")
        self.server.model = CURRENT_MODEL
        self.server.model_queries.clear()

    def write_auth(self, expires):
        staged = self.auth.with_name("fixture-auth-temp.json")
        staged.write_text(json.dumps({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": jwt(int(time.time()) + 3600),
                "access_token": jwt(expires),
                "refresh_token": "fixture-initial",
                "account_id": ACCOUNT,
            },
            "last_refresh": "2026-01-01T00:00:00Z",
        }))
        staged.replace(self.auth)

    def read_cache(self):
        return json.loads(self.catalog.read_bytes())

    def run_hgs(self, *args, ok=True, prefix=(), env=None):
        result = subprocess.run(
            [HGS, *prefix, "account", *args], cwd=self.root, env=env or self.env,
            capture_output=True, text=True, timeout=23,
        )
        self.assertEqual(result.returncode == 0, ok, result.stderr)
        return json.loads(result.stdout) if ok else result.stderr

    @contextmanager
    def codex(self, binary, account=None):
        with subprocess.Popen(
            [binary, "app-server", "--listen", "stdio://"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            cwd=self.root, env=dict(self.env, CODEX_HOME=str(account or self.account)), text=True,
        ) as process:
            replies = queue.Queue()

            def receive():
                for line in process.stdout:
                    replies.put(json.loads(line))

            reader = threading.Thread(target=receive)
            reader.start()
            next_id = 0

            def call(method, params=None, notify=False):
                nonlocal next_id
                request = {"method": method}
                if params is not None:
                    request["params"] = params
                if not notify:
                    next_id += 1
                    request["id"] = next_id
                process.stdin.write(json.dumps(request) + "\n")
                process.stdin.flush()
                if notify:
                    return None
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    reply = replies.get(timeout=max(0, deadline - time.monotonic()))
                    if reply.get("id") == next_id:
                        self.assertIn("result", reply, reply)
                        return reply["result"]
                self.fail(f"Codex did not answer {method}")

            try:
                call("initialize", {"clientInfo": {"name": "hgs_zerus", "version": "0.1.0"}})
                call("initialized", notify=True)
                yield call
            finally:
                process.stdin.close()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)
                reader.join(timeout=3)

    def set_default(self, model, ok=True):
        return self.run_hgs(
            "model-defaults", "fixture", "--model", model, "--effort", "xhigh",
            "--revision", self.settings["revision"], ok=ok,
        )

    def assert_refreshed(self):
        result = self.read_cache()
        self.assertEqual(result["client_version"], self.version)
        self.assertGreater(result["fetched_at"], self.before["fetched_at"])
        self.assertEqual([row["slug"] for row in result["models"]], [CURRENT_MODEL])
        self.assertTrue(self.server.model_queries)
        print(f"native cache {self.before['client_version']} -> {self.version}, "
              f"{self.before['fetched_at']} -> {result['fetched_at']}; "
              f"models HTTP requests={len(self.server.model_queries)}", flush=True)
        return result

    def test_old_cache_refreshes_and_saves(self):
        auth = self.auth.read_bytes()
        profiles = self.set_default(CURRENT_MODEL)["profiles"]
        account = next(row for row in profiles if row["id"] == "fixture")
        self.assertEqual(account["model_defaults"], {"model": CURRENT_MODEL, "effort": "xhigh"})
        self.assert_refreshed()
        self.assertEqual(self.auth.read_bytes(), auth)
        self.assertEqual(self.server.renewals, [])
        self.assertFalse((self.account / "sessions").exists())

    def test_refuses_model_absent_after_refresh_and_preserves_defaults(self):
        error = self.set_default("fixture-catalog-foreign", ok=False)
        cache = self.assert_refreshed()
        for field in ("Catalog fixture", "fixture", self.host, self.version,
                      cache["fetched_at"][:10]):
            self.assertIn(field, error)
        state = self.run_hgs("model-settings", "fixture")
        self.assertEqual(state["defaults"], self.settings["defaults"])
        self.assertEqual(state["revision"], self.settings["revision"])

    def test_signed_out_does_not_list_models(self):
        self.auth.unlink()
        error = self.set_default(CURRENT_MODEL, ok=False)
        self.assertIn("sign in again", error.lower())
        self.assertEqual(self.read_cache(), self.before)
        self.assertEqual(self.server.model_queries, [])
        self.assertFalse(self.auth.exists())
        self.assertFalse((self.account / ".zerus-model-catalog.lock").exists())

    def test_refresh_renews_with_an_existing_native_thread_in_same_home(self):
        with self.codex(OLD_CODEX) as call:
            original = call("thread/start", {
                "ephemeral": True, "model": PREVIOUS_MODEL, "cwd": str(self.home),
            })["thread"]["id"]
            self.write_auth(int(time.time()) - 60)
            before = self.auth.read_bytes()
            self.set_default(CURRENT_MODEL)
            after = json.loads(self.auth.read_bytes())
            self.assertNotEqual(hashlib.sha256(self.auth.read_bytes()), hashlib.sha256(before))
            self.assertEqual(after["tokens"]["refresh_token"], "fixture-renewed")
            self.assertEqual(after["tokens"]["account_id"], ACCOUNT)
            self.assertEqual(self.server.renewals, ["/oauth/token"])
            running = call("thread/read", {"threadId": original, "includeTurns": False})["thread"]
            self.assertEqual(running["id"], original)
            print("original native thread still readable; Codex persisted one OAuth renewal", flush=True)
        self.assert_refreshed()

    def test_rejected_renewal_refuses_with_sign_in_guidance(self):
        self.write_auth(int(time.time()) - 60)
        self.server.reject_renewal = True
        before = self.auth.read_bytes()
        error = self.set_default(CURRENT_MODEL, ok=False)
        self.assertIn("sign in again", error.lower())
        self.assertIn(self.version, error)
        self.assertTrue(self.server.renewals)
        self.assertEqual(self.auth.read_bytes(), before)
        self.assertNotIn(CURRENT_MODEL, [row["slug"] for row in self.read_cache()["models"]])

    def test_held_home_lock_prevents_overlapping_refresh(self):
        import fcntl
        lock_path = self.account / ".zerus-model-catalog.lock"
        with lock_path.open("w") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            error = self.set_default(CURRENT_MODEL, ok=False)
            self.assertIn("already in progress", error)
            self.assertEqual(self.server.model_queries, [])
        self.set_default(CURRENT_MODEL)
        self.assert_refreshed()

    def test_use_everywhere_target_command_refreshes_only_the_peer_catalog(self):
        # Run the same guarded @peer command the desktop sends after the
        # source catalog validates the selection. The SSH fixture uses the
        # actual CLI on both ends with separate homes and state.
        source = self.root / "source"
        source_home = source / "home"
        source_tmux = source / "tmux"
        source_bin = source_home / ".local/bin"
        source_bin.mkdir(parents=True)
        source_tmux.mkdir(mode=0o700)
        source_env = dict(
            self.env, HOME=str(source_home), HGS_CONFIG_DIR=str(source / "config"),
            HGS_STATE_DIR=str(source / "state"), HGS_PEERS="catalog-peer",
            TMUX_TMPDIR=str(source_tmux),
            PATH=os.pathsep.join((str(source_bin), os.defpath)),
        )
        (source_bin / "codex").symlink_to(Path(CODEX).resolve())
        (self.home / ".local/bin/hgs").symlink_to(HGS)
        self.run_hgs("add", "source", "--provider", "codex", "--label", "Source", env=source_env)
        source_account = source / "config/accounts/source"
        for name in ("config.toml", "auth.json"):
            shutil.copyfile(self.account / name, source_account / name)
        with self.codex(CODEX, source_account) as call:
            call("model/list", {"limit": 100})
        source_cache = source_account / "models_cache.json"
        source_bytes = source_cache.read_bytes()
        source_settings = self.run_hgs("model-settings", "source", env=source_env)
        self.assertIn(CURRENT_MODEL, [row["id"] for row in source_settings["models"]])
        self.run_hgs(
            "model-defaults", "source", "--model", CURRENT_MODEL, "--effort", "xhigh",
            "--revision", source_settings["revision"], env=source_env,
        )
        self.server.model_queries.clear()
        machine = subprocess.check_output(
            [HGS, "swarm", "get"], cwd=self.root, env=self.env, text=True, timeout=10,
        )
        machine = json.loads(machine)["node_id"]
        ssh = source_bin / "ssh"
        ssh.write_text(
            "#!/usr/bin/env python3\n"
            "import os, shlex, sys\n"
            'if sys.argv[-1].startswith(\'echo "hgs-remote-v2|\'):\n'
            '    print("hgs-remote-v2|%OS%|%SystemRoot%|:OS|:SystemRoot")\n'
            "    sys.exit(0)\n"
            "args = shlex.split(sys.argv[-1])\n"
            'assert args[:2] == ["sh", "-c"] and len(args) == 3\n'
            f"os.execve('/bin/sh', args, {self.env!r})\n"
        )
        ssh.chmod(0o700)
        saved = self.run_hgs(
            "model-defaults", "fixture", "--model", CURRENT_MODEL, "--effort", "xhigh",
            "--revision", self.settings["revision"],
            prefix=("@catalog-peer", "--expect-machine", machine), env=source_env,
        )
        target = next(row for row in saved["profiles"] if row["id"] == "fixture")
        self.assertEqual(target["model_defaults"], {"model": CURRENT_MODEL, "effort": "xhigh"})
        self.assertEqual(source_cache.read_bytes(), source_bytes)
        self.assert_refreshed()


if __name__ == "__main__":
    unittest.main()
