"""Public cask qualification boundaries; no Homebrew side effects or real tokens."""
from email.message import Message
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("macos_public_upgrade", ROOT / "scripts/ci/macos-public-upgrade.py")
upgrade = importlib.util.module_from_spec(spec)
spec.loader.exec_module(upgrade)


class PublicCaskBoundaries(unittest.TestCase):
    def cask(self, channel="stable", legacy=False, previous=False):
        version = "0.44.0" if channel == "stable" else "0.44.0.r1558.gacbbee6.n19"
        tag = "nightly-39002487144" if channel == "nightly" else None
        if previous:
            text = (ROOT / "packaging/homebrew/previous" / f"{upgrade.cask_for(channel)}.rb.in").read_text().replace(
                "@VERSION@", version).replace("@SHA256@", "a" * 64).replace("@RELEASE_TAG@", tag or "")
        else:
            text = upgrade.render_cask(version, "a" * 64, channel, tag)
        if legacy:
            text = text.replace('        "com.hgdev.zerus-mobile-connector",\n', "").replace(
                '        "~/Library/LaunchAgents/com.hgdev.zerus-mobile-connector.plist",\n', "")
        return text

    def proof_inputs(self):
        cask = upgrade.accepted_cask(self.cask(), "stable")
        prefix = "https://github.com/ufna/zerus/releases/download/v0.44.0/"
        metadata = json.dumps({"version": "0.44.0", "source_commit": "b" * 40,
            "macos": {"asset": "zerus-0.44.0-macos-arm64.zip", "qt_version": "6.11.2",
            "team_id": upgrade.TEAM, "notarization_id": "11111111-1111-4111-8111-111111111111",
            "minimum_system_version": "14.0"}, "homebrew_casks": ["zerus"]}).encode()
        names = ["release-info.json", "SHA256SUMS", "zerus-0.44.0-macos-arm64.zip"]
        release = {"tag_name": "v0.44.0", "target_commitish": "b" * 40, "draft": False, "prerelease": False,
                   "assets": [{"name": name, "browser_download_url": prefix + name, "size": 12} for name in names]}
        sums = (hashlib.sha256(metadata).hexdigest() + "  release-info.json\n" + "a" * 64
                + "  zerus-0.44.0-macos-arm64.zip\n").encode()
        return cask, release, metadata, sums

    def test_full_commit_and_https_public_origins_only(self):
        self.assertEqual(upgrade.exact_commit("a" * 40), "a" * 40)
        for value in ["main", "a" * 7, "A" * 40, "a" * 40 + "\n"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                upgrade.exact_commit(value)
        credential_url = "https://" + "fixture-user" + "@" + "github.com/asset"
        for value in ["http://github.com/ufna/zerus", "https://example.org/asset", "file:///tmp/asset",
                      credential_url, "https://github.com:8443/asset"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                upgrade.safe_url(value)

    def test_metadata_token_is_confined_to_exact_official_get_routes(self):
        token = "synthetic_metadata_fixture"
        metadata = upgrade.PublicMetadata(token)
        urls = ["https://api.github.com/repos/ufna/homebrew-zerus/commits/" + "a" * 40,
                "https://api.github.com/repos/ufna/zerus/releases/tags/v0.44.0",
                "https://api.github.com/repos/ufna/zerus/releases/tags/nightly-39002487144"]
        requests = []
        def open_request(request, timeout):
            requests.append(request)
            response = io.BytesIO(b"{}")
            response.url = request.full_url
            return response
        with patch.object(upgrade.urllib.request, "build_opener",
                          return_value=types.SimpleNamespace(open=open_request)) as opener:
            for url in urls:
                self.assertEqual(metadata.fetch(url), b"{}")
            self.assertTrue(all(isinstance(call.args[0], upgrade.MetadataRedirects) for call in opener.call_args_list))
        self.assertEqual([request.full_url for request in requests], urls)
        for request in requests:
            self.assertEqual(request.get_method(), "GET")
            self.assertEqual(request.get_header("Authorization"), "Bearer " + token)
        bad_urls = ["http://api.github.com/repos/ufna/zerus/releases/tags/v0.44.0",
                    "https://api.github.com:443/repos/ufna/zerus/releases/tags/v0.44.0",
                    "https://api.github.com/repos/other/zerus/releases/tags/v0.44.0",
                    "https://api.github.com/repos/ufna/zerus/contents/VERSION",
                    "https://api.github.com/repos/ufna/zerus/releases/tags/v0.44.0?fixture=1",
                    "https://api.github.com/repos/ufna/zerus/releases/tags/v0.44.0#fixture",
                    "https://api.github.com/repos/ufna/zerus/releases/tags/../latest",
                    "https://api.github.com/repos/ufna/homebrew-zerus/commits/main",
                    "https://raw.githubusercontent.com/ufna/zerus/main/VERSION",
                    "https://github.com/ufna/zerus/releases/download/v0.44.0/release-info.json",
                    "https://release-assets.githubusercontent.com/fixture",
                    "https://example.org/repos/ufna/zerus/releases/tags/v0.44.0"]
        with patch.object(upgrade.urllib.request, "build_opener") as opener:
            for url in bad_urls:
                with self.subTest(url=url), self.assertRaises(ValueError):
                    metadata.fetch(url)
            for method in ("POST", "PUT", "PATCH", "DELETE", "HEAD", "get"):
                with self.subTest(method=method), self.assertRaises(ValueError):
                    metadata.fetch(urls[0], method=method)
            opener.assert_not_called()

    def test_authenticated_metadata_redirects_never_open_another_request(self):
        url = "https://api.github.com/repos/ufna/zerus/releases/tags/v0.44.0"
        request = upgrade.urllib.request.Request(url, headers={"Authorization": "Bearer synthetic_fixture"})
        handler = upgrade.MetadataRedirects()
        with patch.object(upgrade.urllib.request, "urlopen") as urlopen:
            for destination in [url, "https://api.github.com/repos/ufna/zerus/releases/tags/v0.45.0",
                                "https://raw.githubusercontent.com/ufna/zerus/main/VERSION",
                                "https://github.com/ufna/zerus/releases/download/v0.44.0/release-info.json",
                                "https://release-assets.githubusercontent.com/fixture", "https://example.org/fixture"]:
                for code in (301, 302, 303, 307, 308):
                    headers = Message()
                    headers["Location"] = destination
                    with self.subTest(destination=destination, code=code), self.assertRaises(ValueError):
                        handler.http_error_302(request, io.BytesIO(), code, "Fixture redirect", headers)
            urlopen.assert_not_called()

    def test_metadata_errors_do_not_expose_token_and_responses_are_bounded(self):
        token = "synthetic_metadata_fixture"
        metadata = upgrade.PublicMetadata(token)
        url = "https://api.github.com/repos/ufna/zerus/releases/tags/v0.44.0"
        for error in (upgrade.urllib.error.URLError(token),
                      upgrade.urllib.error.HTTPError(url, 403, token, {}, io.BytesIO())):
            with patch.object(upgrade.urllib.request, "build_opener",
                              return_value=types.SimpleNamespace(open=lambda *args, **kwargs: (_ for _ in ()).throw(error))):
                with self.assertRaises(ValueError) as raised:
                    metadata.fetch(url)
                self.assertNotIn(token, str(raised.exception))
                self.assertTrue(raised.exception.__suppress_context__)
            if isinstance(error, upgrade.urllib.error.HTTPError):
                error.close()
        def response(*args, **kwargs):
            body = io.BytesIO(b"12345")
            body.url = url
            return body
        with patch.object(upgrade.urllib.request, "build_opener", return_value=types.SimpleNamespace(open=response)):
            with self.assertRaises(ValueError):
                metadata.fetch(url, limit=4)

    def test_raw_and_payload_downloads_remain_anonymous(self):
        requests = []
        def open_request(request, timeout):
            requests.append(request)
            response = io.BytesIO(b"Public fixture bytes")
            response.url = request.full_url
            return response
        with patch.dict(os.environ, {upgrade.METADATA_TOKEN_ENV: "synthetic_metadata_fixture"}), \
                patch.object(upgrade.urllib.request, "build_opener", return_value=types.SimpleNamespace(open=open_request)):
            for url in ("https://raw.githubusercontent.com/ufna/homebrew-zerus/" + "a" * 40 + "/Casks/zerus.rb",
                        "https://github.com/ufna/zerus/releases/download/v0.44.0/zerus-0.44.0-macos-arm64.zip"):
                self.assertEqual(upgrade.fetch(url), b"Public fixture bytes")
        self.assertTrue(all(request.get_header("Authorization") is None for request in requests))

    def test_prepare_public_authenticates_only_the_two_metadata_reads(self):
        with tempfile.TemporaryDirectory() as tmp:
            cask, release, info, sums = self.proof_inputs()
            payload = b"Public synthetic ZIP"
            digest = hashlib.sha256(payload).hexdigest()
            raw = self.cask().replace("a" * 64, digest).encode()
            sums = sums.replace(b"a" * 64, digest.encode())
            release["assets"][-1]["size"] = len(payload)
            prefix = "https://github.com/ufna/zerus/releases/download/v0.44.0/"
            raw_url = "https://raw.githubusercontent.com/ufna/homebrew-zerus/" + "a" * 40 + "/Casks/zerus.rb"
            public = {raw_url: raw, prefix + "release-info.json": info, prefix + "SHA256SUMS": sums,
                      prefix + "zerus-0.44.0-macos-arm64.zip": payload}
            api = {"https://api.github.com/repos/ufna/homebrew-zerus/commits/" + "a" * 40: json.dumps({"sha": "a" * 40}).encode(),
                   "https://api.github.com/repos/ufna/zerus/releases/tags/v0.44.0": json.dumps(release).encode()}
            with patch.object(upgrade, "fetch", side_effect=lambda url, *args: public[url]) as anonymous:
                calls = []
                def metadata_fetch(url):
                    calls.append(url)
                    return api[url]
                upgrade.prepare_public("a" * 40, "stable", Path(tmp) / "public", metadata_fetch)
            self.assertEqual(calls, list(api))
            self.assertEqual([call.args[0] for call in anonymous.call_args_list], list(public))

    def test_main_removes_metadata_token_before_guards_and_child_processes(self):
        token = "synthetic_metadata_fixture"
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ, {upgrade.METADATA_TOKEN_ENV: token}), \
                patch.object(sys, "argv", ["macos-public-upgrade.py", "--output", tmp]), \
                patch.object(upgrade.sys, "version_info", (3, 11)), \
                patch.object(upgrade, "disposable_mac", side_effect=RuntimeError("Private fixture boundary")):
            with self.assertRaisesRegex(RuntimeError, "Private fixture boundary"):
                upgrade.main()
            self.assertNotIn(upgrade.METADATA_TOKEN_ENV, os.environ)
            q = upgrade.Qualification(Path(tmp))
            args = [sys.executable, "-c", "import os; print(os.getenv('GH_PUBLIC_METADATA_TOKEN', 'ABSENT'))"]
            self.assertEqual(q.run(args).strip(), "ABSENT")
            with subprocess.Popen(args, stdout=subprocess.PIPE, text=True) as child:
                stdout, _ = child.communicate(timeout=10)
                self.assertEqual(child.returncode, 0)
                self.assertEqual(stdout.strip(), "ABSENT")
            self.assertTrue(all(token not in path.read_text() for path in Path(tmp).glob("*.json")))

    def test_short_gui_runtime_is_owned_and_cleanup_never_follows_external_links(self):
        with tempfile.TemporaryDirectory() as tmp:
            outside = Path(tmp) / "outside"
            outside.mkdir()
            retained = outside / "retain.txt"
            retained.write_text("Unrelated private fixture data")
            q = upgrade.Qualification(Path(tmp))
            q.env = {"TMPDIR": str(outside)}
            q.prepare_gui_runtime()
            runtime = Path(q.env["TMPDIR"])
            try:
                self.assertEqual(runtime.parent, Path("/tmp").resolve())
                self.assertEqual(stat.S_IMODE(runtime.stat().st_mode), 0o700)
                self.assertNotEqual(runtime, outside)
                self.assertLessEqual(q.gui_runtime_info()["canonical_socket_bytes"], 90)
                (runtime / "external").symlink_to(outside, target_is_directory=True)
                (runtime / "hgs-tray").touch()
                with self.assertRaises(ValueError):
                    q.prepare_gui_runtime()
            finally:
                q.cleanup()
            self.assertFalse(runtime.exists())
            self.assertIsNone(q.gui_runtime)
            self.assertEqual(retained.read_text(), "Unrelated private fixture data")

    def test_gui_runtime_cleanup_also_runs_after_native_cleanup_failure(self):
        for throws in (False, True):
            with self.subTest(throws=throws), tempfile.TemporaryDirectory() as tmp:
                q = upgrade.Qualification(Path(tmp))
                q.env = {}
                q.prepare_gui_runtime()
                runtime = Path(q.env["TMPDIR"])
                def cleanup():
                    if throws:
                        raise RuntimeError("Private native fixture cleanup failure")
                    return False
                q.native = types.SimpleNamespace(doCleanups=cleanup)
                with self.assertRaisesRegex(RuntimeError if throws else ValueError, "native fixture cleanup fail"):
                    q.cleanup()
                self.assertFalse(runtime.exists())
                self.assertIsNone(q.gui_runtime)

    def test_start_gui_records_actual_ipc_paths_before_owned_process(self):
        with tempfile.TemporaryDirectory() as tmp:
            q = upgrade.Qualification(Path(tmp))
            q.env = {}
            q.prepare_gui_runtime()
            runtime = Path(q.env["TMPDIR"])
            def start(*args, **kwargs):
                proof = json.loads((Path(tmp) / "fixture-ipc.json").read_text())
                self.assertEqual(proof["runtime"], str(runtime))
                self.assertEqual(proof["canonical_runtime"], str(runtime.resolve()))
                self.assertEqual(proof["canonical_socket_bytes"], len(os.fsencode(runtime / "hgs-tray")))
                self.assertEqual(kwargs["env"]["TMPDIR"], str(runtime))
                return types.SimpleNamespace(poll=lambda: None, send_signal=lambda _: None, wait=lambda **_: 0)
            try:
                with patch.object(upgrade.subprocess, "Popen", side_effect=start) as popen, \
                        patch.object(upgrade.socket, "socket") as socket:
                    q.start_gui("fixture")
                    popen.assert_called_once()
                    socket.return_value.__enter__.return_value.connect.assert_called_once_with(str(runtime / "hgs-tray"))
            finally:
                q.cleanup()
            self.assertFalse(runtime.exists())

    def test_foreign_runtime_is_refused_before_gui_launch_and_not_removed(self):
        with tempfile.TemporaryDirectory() as tmp:
            outside = Path(tmp) / "outside"
            outside.mkdir()
            marker = outside / "hgs-tray"
            marker.write_text("Never touch another runtime")
            q = upgrade.Qualification(Path(tmp))
            q.env = {}
            q.prepare_gui_runtime()
            owned = Path(q.env["TMPDIR"])
            q.env["TMPDIR"] = str(outside)
            try:
                with patch.object(upgrade.subprocess, "Popen") as popen, self.assertRaises(ValueError):
                    q.start_gui("refused")
                popen.assert_not_called()
            finally:
                q.cleanup()
            self.assertFalse(owned.exists())
            self.assertEqual(marker.read_text(), "Never touch another runtime")

    def test_only_literal_current_or_retained_casks_are_accepted(self):
        for channel in ("stable", "nightly"):
            self.assertNotEqual(self.cask(channel), self.cask(channel, previous=True))
            for previous, legacy in ((False, False), (True, False), (True, True)):
                text = self.cask(channel, legacy=legacy, previous=previous)
                self.assertEqual(upgrade.accepted_cask(text, channel)["channel"], channel)
                for injected in [text + "system 'touch /tmp/unreviewed'\n",
                                 text.replace('  app "Zerus.app"', '  uninstall quit: "com.hgdev.hgs-tray"'),
                                 text.replace("https://github.com/ufna/zerus/", "https://example.org/")]:
                    with self.subTest(channel=channel, previous=previous, legacy=legacy), self.assertRaises(ValueError):
                        upgrade.accepted_cask(injected, channel)

    def test_release_metadata_seal_source_and_asset_identity(self):
        args = self.proof_inputs()
        self.assertEqual(upgrade.release_proof(*args)["source_commit"], "b" * 40)
        for change in ({"target_commitish": "main"}, {"draft": True}, {"prerelease": True}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                upgrade.release_proof(args[0], dict(args[1], **change), *args[2:])
        bad_assets = [dict(row, browser_download_url="https://example.org/" + row["name"])
                      for row in args[1]["assets"]]
        with self.assertRaises(ValueError):
            upgrade.release_proof(args[0], dict(args[1], assets=bad_assets), *args[2:])
        with self.assertRaises(ValueError):
            upgrade.release_proof(*args[:2], args[2] + b" ", args[3])
        with self.assertRaises(ValueError):
            upgrade.release_proof(*args[:3], args[3].replace(b"a" * 64, b"c" * 64))

    def test_nightly_retains_product_and_release_versions_separately(self):
        cask, release, metadata, sums = self.proof_inputs()
        cask = upgrade.accepted_cask(self.cask("nightly"), "nightly")
        info = json.loads(metadata)
        info.update(version=cask["version"], product_version="0.44.0", homebrew_casks=["zerus@nightly"])
        info["macos"]["asset"] = "zerus-" + cask["version"] + "-macos-arm64.zip"
        metadata = json.dumps(info).encode()
        release.update(tag_name=cask["tag"], prerelease=True)
        prefix = "https://github.com/ufna/zerus/releases/download/" + cask["tag"] + "/"
        release["assets"] = [{"name": name, "browser_download_url": prefix + name, "size": 12}
                             for name in ["release-info.json", "SHA256SUMS", info["macos"]["asset"]]]
        sums = (hashlib.sha256(metadata).hexdigest() + "  release-info.json\n" + "a" * 64
                + "  " + info["macos"]["asset"] + "\n").encode()
        proof = upgrade.release_proof(cask, release, metadata, sums)
        self.assertEqual(proof["product_version"], "0.44.0")
        self.assertEqual(proof["version"], cask["version"])

    def test_archive_rejects_traversal_and_escaping_symlinks(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "app.zip"
            for name, target in [("../outside", None), ("Other.app/file", None),
                                 ("Zerus.app/link", "../../outside")]:
                with zipfile.ZipFile(path, "w") as archive:
                    member = zipfile.ZipInfo(name)
                    if target:
                        member.external_attr = (stat.S_IFLNK | 0o777) << 16
                    archive.writestr(member, target or "bytes")
                with self.subTest(name=name), self.assertRaises(ValueError):
                    upgrade.safe_archive(path)
            with zipfile.ZipFile(path, "w") as archive:
                archive.writestr("Zerus.app/Contents/Info.plist", "bytes")
                member = zipfile.ZipInfo("Zerus.app/Contents/Frameworks/QtCore.framework/Versions/Current")
                member.external_attr = (stat.S_IFLNK | 0o777) << 16
                archive.writestr(member, "A")
            upgrade.safe_archive(path)

    def test_archive_rejects_symlink_ancestor_pivot(self):
        # Every target alone is lexically inside the bundle; following earlier
        # symlink ancestors changes the meaning of later '..' components.
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "pivot.zip"
            with zipfile.ZipFile(path, "w") as archive:
                for name, target in [("Zerus.app/a", "."), ("Zerus.app/a/b", "../outside"),
                                     ("Zerus.app/a/b/c", ".."), ("Zerus.app/a/b/c/d", "..")]:
                    member = zipfile.ZipInfo(name)
                    member.external_attr = (stat.S_IFLNK | 0o777) << 16
                    archive.writestr(member, target)
                archive.writestr("Zerus.app/a/b/c/d/probe", "Never extract this fixture.")
            with self.assertRaises(ValueError):
                upgrade.safe_archive(path)

    def test_archive_rejects_link_cycles_but_retains_framework_chains(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "links.zip"
            for cyclic in (True, False):
                with zipfile.ZipFile(path, "w") as archive:
                    if cyclic:
                        links = [("Zerus.app/a", "b"), ("Zerus.app/b", "a")]
                    else:
                        base = "Zerus.app/Contents/Frameworks/QtCore.framework/"
                        archive.writestr(base + "Versions/A/QtCore", "Synthetic framework binary")
                        links = [(base + "Versions/Current", "A"), (base + "QtCore", "Versions/Current/QtCore")]
                    for name, target in links:
                        member = zipfile.ZipInfo(name)
                        member.external_attr = (stat.S_IFLNK | 0o777) << 16
                        archive.writestr(member, target)
                if cyclic:
                    with self.assertRaises(ValueError):
                        upgrade.safe_archive(path)
                else:
                    upgrade.safe_archive(path)

    def test_archive_rejects_case_and_normalization_alias_pivots(self):
        cases = [
            [("Zerus.app/A", "."), ("Zerus.app/B", "a/.."),
             ("Zerus.app/C", "b/.."), ("Zerus.app/c/probe", None)],
            [("Zerus.app/\u00c5", "."), ("Zerus.app/A\u030a/probe", None)],
            [("Zerus.app/Case", None), ("Zerus.app/case", None)],
            [("Zerus.app/UnicodeTarget", "\u00c5")],
        ]
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "aliases.zip"
            for entries in cases:
                with zipfile.ZipFile(path, "w") as archive:
                    for name, target in entries:
                        member = zipfile.ZipInfo(name)
                        if target is not None:
                            member.external_attr = (stat.S_IFLNK | 0o777) << 16
                        archive.writestr(member, "Never extract this fixture." if target is None else target)
                with self.subTest(paths=[name for name, _ in entries]), self.assertRaises(ValueError):
                    upgrade.safe_archive(path)

    def test_owner_accounts_and_nonhosted_macs_are_refused(self):
        env = dict(GITHUB_ACTIONS="true", RUNNER_ENVIRONMENT="github-hosted", RUNNER_OS="macOS", RUNNER_TEMP="/tmp/fixture")
        with patch.object(upgrade.platform, "system", return_value="Darwin"), \
                patch.object(upgrade.platform, "machine", return_value="arm64"), \
                patch.object(upgrade, "account_identity", return_value=(501, 501,
                    types.SimpleNamespace(pw_uid=501, pw_name="example", pw_dir=str(Path("/Users") / "example")))):
            with self.assertRaises(ValueError):
                upgrade.disposable_mac(env, Path("/Users") / "example")
            with self.assertRaises(ValueError):
                upgrade.disposable_mac(dict(env, RUNNER_ENVIRONMENT="self-hosted"), Path("/Users") / "runner")

    def test_runner_home_claim_cannot_override_actual_uid_account(self):
        env = dict(GITHUB_ACTIONS="true", RUNNER_ENVIRONMENT="github-hosted", RUNNER_OS="macOS", RUNNER_TEMP="/tmp/fixture")
        runner_home = Path("/Users") / "runner"
        actual = types.SimpleNamespace(pw_uid=501, pw_name="example", pw_dir=str(Path("/Users") / "example"))
        with patch.object(upgrade.platform, "system", return_value="Darwin"), \
                patch.object(upgrade.platform, "machine", return_value="arm64"), \
                patch.object(upgrade, "account_identity", return_value=(501, 501, actual), create=True), \
                patch.object(upgrade.Path, "resolve", lambda path: path), \
                patch.object(upgrade, "APP", Path("/nonexistent-qualification-fixture-app")):
            with self.assertRaises(ValueError):
                upgrade.disposable_mac(env, runner_home)
            actual.pw_name, actual.pw_dir = "runner", str(runner_home)
            upgrade.disposable_mac(env, runner_home)
            actual.pw_dir = str(Path("/Users") / "example")
            with self.assertRaises(ValueError):
                upgrade.disposable_mac(env, runner_home)
            actual.pw_dir = str(runner_home)
            with patch.object(upgrade, "account_identity", return_value=(501, 0, actual)), self.assertRaises(ValueError):
                upgrade.disposable_mac(env, runner_home)

    def test_draft_snapshot_retains_all_durable_fields(self):
        with tempfile.TemporaryDirectory() as tmp:
            q = upgrade.Qualification(Path(tmp))
            q.draft_file = Path(tmp) / "draft.json"
            original = dict(version=1, key="fixture", text="Retained draft", position=7, anchor=3,
                            label="Fixture label", form_state={"answer": "Keep this"}, next_attachment_number=2,
                            sending=False, uncertain=False, attachments=[])
            q.draft_file.write_text(json.dumps(original))
            before = q.payload_state()
            for field, lost in dict(position=0, anchor=0, label="", form_state={}, next_attachment_number=1).items():
                q.draft_file.write_text(json.dumps(dict(original, **{field: lost})))
                with self.subTest(field=field):
                    self.assertNotEqual(before, q.payload_state())

    def test_native_snapshot_samples_durable_and_live_target_bindings(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            n = types.SimpleNamespace(root=root, name="claude/cask-fixture", pane="%7", run_id="run-original",
                                      conversation_id="conversation-original", native_requests=[{}])
            transcript = root / ".claude/projects/fixture" / (n.conversation_id + ".jsonl")
            transcript.parent.mkdir(parents=True)
            transcript.write_text("Synthetic native transcript\n")
            n.record_path = root / "record.json"
            original = dict(name=n.name, run_id=n.run_id, conversation_id=n.conversation_id,
                            pane=n.pane, pid=321, process_start="fixture-start")
            n.record_path.write_text(json.dumps(original))
            live = dict(run=n.run_id, name=n.name, pane=n.pane)
            def tmux(*args):
                if args[0] == "show-options":
                    return live["run"]
                return {"#{pane_pid}": "321", "#{pid}": "123", "#{session_name}": live["name"],
                        "#{pane_id}": live["pane"]}[args[-1]]
            n.tmux, n.screen = tmux, lambda: "Fixture complete."
            q = upgrade.Qualification(root)
            q.native, q.env = n, {}
            q.run = lambda *args, **kwargs: "fixture-start"
            self.assertEqual(q.native_state()["run"], n.run_id)
            for field, changed in dict(run_id="run-changed", conversation_id="conversation-changed", pane="%8",
                                       pid=322, process_start="changed-start", name="claude/other-fixture").items():
                n.record_path.write_text(json.dumps(dict(original, **{field: changed})))
                with self.subTest(durable_field=field), self.assertRaises(ValueError):
                    q.native_state()
            n.record_path.write_text(json.dumps(original))
            for field, changed in dict(run="run-changed", name="claude/other-fixture", pane="%8").items():
                previous, live[field] = live[field], changed
                with self.subTest(live_field=field), self.assertRaises(ValueError):
                    q.native_state()
                live[field] = previous

    def test_install_native_signature_precedes_app_execution(self):
        with tempfile.TemporaryDirectory() as tmp:
            q = upgrade.Qualification(Path(tmp))
            q.env = {}
            calls = []
            q.select_tap = lambda _: calls.append("tap")
            q.native_signature = lambda *_: calls.append("signature")
            def run(args, **kwargs):
                calls.append(str(args[0]) + " " + str(args[1]))
                return json.dumps(dict(sourceCommit="b" * 40, sourceDirty=False)) if args[-1] == "--build-info" else ""
            q.run = run
            q.install(dict(cask="zerus", source_commit="b" * 40), upgrade=True)
            self.assertEqual(calls[:3], ["tap", "brew upgrade", "signature"])
            self.assertLess(calls.index("signature"), next(i for i, x in enumerate(calls) if "--build-info" in x))
            self.assertFalse(any("zap" in call or "bootout" in call for call in calls))

    def test_lifecycle_uses_real_upgrade_and_both_explicit_channel_switches(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            q = upgrade.Qualification(root)
            commands, installs, checkpoints = [], [], []
            q.run = lambda args, **kwargs: commands.append([str(x) for x in args]) or ""
            q.native_signature = lambda *_: None
            def install(proof, upgrade=False):
                installs.append((proof["version"], proof["cask"], upgrade))
                q.installed = proof["cask"]
            q.install = install
            q.open_native = lambda _: None
            q.seed_state = lambda: None
            q.start_gui = lambda _: setattr(q, "gui", types.SimpleNamespace(pid=123))
            q.stop_gui = lambda: setattr(q, "gui", None)
            q.checkpoint = lambda phase, old_gui_pid=None: checkpoints.append((phase, old_gui_pid))
            old = dict(version="0.43.0", cask="zerus", directory=str(root / "old"), asset="old.zip")
            new = dict(version="0.44.0", cask="zerus", directory=str(root / "new"), asset="new.zip")
            nightly = dict(version="0.44.0.r1.gabcdef0.n1", cask="zerus@nightly", directory=str(root / "nightly"), asset="nightly.zip")
            with patch.object(upgrade, "safe_archive"):
                q.lifecycle(old, new, nightly)
            self.assertEqual(installs, [("0.43.0", "zerus", False), ("0.44.0", "zerus", True),
                                       (nightly["version"], "zerus@nightly", False), ("0.44.0", "zerus", False)])
            self.assertEqual([x for x in commands if x[:3] == ["brew", "uninstall", "--cask"]],
                             [["brew", "uninstall", "--cask", "zerus"], ["brew", "uninstall", "--cask", "zerus@nightly"]])
            self.assertIn(("stable-version-upgrade", 123), checkpoints)
            self.assertIn(("nightly-to-stable-restart", None), checkpoints)


if __name__ == "__main__":
    unittest.main()
