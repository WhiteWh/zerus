#!/usr/bin/env python3
"""Post-publication cask lifecycle qualification, only on a disposable Mac runner.

No signing key, publication credential, cached artifact or deployment environment
is used. The ephemeral read-only job token is confined to allowlisted public REST
metadata GETs. This does not qualify interactive Terminal/TCC consent.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
import urllib.error
import zipfile
import posixpath
import stat

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
from macos_release import (cask_fields, cask_for, cask_version_key, render_cask,
                           reviewed_cask_templates, validate_macos_info)

SHA = re.compile(r"[0-9a-f]{40}")
TEAM = "XQA263U346"
APP = Path("/Applications/Zerus.app")
DOMAIN = "com.hgdev.hgs-tray"
SETTINGS = {"workspace/activityWidth": "736", "workspace/theme": "dark"}
ALLOWED_HOSTS = {"api.github.com", "raw.githubusercontent.com", "github.com",
                 "release-assets.githubusercontent.com", "objects.githubusercontent.com"}
METADATA_TOKEN_ENV = "GH_PUBLIC_METADATA_TOKEN"
METADATA_URL = re.compile(
    r"https://api\.github\.com/repos/ufna/(?:homebrew-zerus/commits/[0-9a-f]{40}"
    r"|zerus/releases/tags/(?:v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
    r"|nightly-[1-9][0-9]*))")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def exact_commit(value):
    require(isinstance(value, str) and SHA.fullmatch(value), "Expected a complete lowercase tap commit SHA.")
    return value


def safe_url(url):
    parsed = urllib.parse.urlsplit(url)
    require(parsed.scheme == "https" and parsed.hostname in ALLOWED_HOSTS
            and parsed.port in (None, 443) and not parsed.username and not parsed.password,
            "Unexpected public download origin.")
    return url


class PublicRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, new_url):
        return super().redirect_request(request, fp, code, message, headers, safe_url(new_url))


def fetch(url, limit=1024 * 1024):
    request = urllib.request.Request(safe_url(url), headers={"User-Agent": "Zerus-public-cask-qualification"})
    with urllib.request.build_opener(PublicRedirects()).open(request, timeout=60) as response:
        safe_url(response.url)
        data = response.read(limit + 1)
    require(len(data) <= limit, "Public response exceeds its byte limit.")
    return data


class MetadataRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, new_url):
        raise ValueError("Public REST metadata redirects are refused.")


class PublicMetadata:
    def __init__(self, token):
        require(isinstance(token, str) and len(token) <= 1024
                and all(33 <= ord(char) <= 126 for char in token), "Invalid metadata token.")
        self._token = token

    def fetch(self, url, limit=1024 * 1024, *, method="GET"):
        require(method == "GET" and isinstance(url, str) and METADATA_URL.fullmatch(url),
                "Unexpected public REST metadata route or method.")
        headers = {"User-Agent": "Zerus-public-cask-qualification", "Accept": "application/vnd.github+json"}
        if self._token:
            headers["Authorization"] = "Bearer " + self._token
        request = urllib.request.Request(url, headers=headers, method="GET")
        try:
            with urllib.request.build_opener(MetadataRedirects()).open(request, timeout=60) as response:
                require(response.url == url, "Public REST metadata origin changed.")
                data = response.read(limit + 1)
        except urllib.error.HTTPError as error:
            raise ValueError(f"Public REST metadata GET failed with HTTP {error.code}.") from None
        except urllib.error.URLError:
            raise ValueError("Public REST metadata GET failed.") from None
        require(len(data) <= limit, "Public REST metadata exceeds its byte limit.")
        return data


def consume_metadata_token():
    # Never leave the job token in the environment inherited by brew, the app,
    # native fixtures or their subprocesses. It is not written into evidence.
    return PublicMetadata(os.environ.pop(METADATA_TOKEN_ENV, ""))


def accepted_cask(text, channel):
    """Exact current or retained templates; never normalize input Ruby."""
    fields = cask_fields(text)
    tag = None
    if channel == "nightly":
        tags = re.findall(r"releases/download/(nightly-[1-9][0-9]*)/", text)
        require(len(tags) == 1, "Expected one exact immutable Nightly tag.")
        tag = tags[0]
    reviewed = reviewed_cask_templates(fields["version"], fields["sha256"], channel, tag)
    # Literal reviewed 0.43/0.44 template difference. Input text is compared
    # byte-for-byte, including all hooks, rather than parsed as arbitrary Ruby.
    legacy = tuple(template.replace('        "com.hgdev.zerus-mobile-connector",\n', "").replace(
        '        "~/Library/LaunchAgents/com.hgdev.zerus-mobile-connector.plist",\n', "") for template in reviewed)
    require(text in (*reviewed, *legacy), "Cask differs from the current and retained reviewed templates.")
    return dict(fields, channel=channel, tag=tag or "v" + fields["version"])


def release_proof(cask, release, metadata_bytes, sums_bytes):
    """Check independently downloaded metadata and seals before executing cask Ruby."""
    require(release.get("tag_name") == cask["tag"] and release.get("draft") is False,
            "Public release tag or draft status differs.")
    require(type(release.get("prerelease")) is bool
            and release["prerelease"] == (cask["channel"] == "nightly"), "Wrong release channel.")
    info = json.loads(metadata_bytes)
    version = info.get("release_version", info.get("version"))
    require(version == cask["version"], "Public metadata version differs from the tap.")
    commit = exact_commit(info.get("source_commit"))
    require(release.get("target_commitish") == commit, "Release source differs from public metadata.")
    validate_macos_info(info, cask["version"], cask["channel"])
    require(info["macos"]["team_id"] == TEAM, "Unexpected Developer ID team.")
    sums = {}
    for line in sums_bytes.decode("utf-8").splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]+)", line)
        require(match and match[2] not in sums, "Malformed or duplicate public SHA256SUMS row.")
        sums[match[2]] = match[1]
    require(sums.get("release-info.json") == hashlib.sha256(metadata_bytes).hexdigest(),
            "Public metadata does not match its seal.")
    asset = info["macos"]["asset"]
    require(sums.get(asset) == cask["sha256"], "Public ZIP seal differs from the exact tap recipe.")
    origin = "https://github.com/ufna/zerus/releases/download/" + cask["tag"] + "/"
    assets = {row["name"]: row for row in release.get("assets", [])}
    require(len(assets) == len(release.get("assets", [])), "Duplicate public release asset names.")
    for name in ("release-info.json", "SHA256SUMS", asset):
        require(name in assets and assets[name].get("browser_download_url") == origin + name,
                "Release asset is missing or has a different origin.")
    require(type(assets[asset].get("size")) is int and 0 < assets[asset]["size"] <= 512 * 1024**2,
            "Invalid public ZIP size.")
    return dict(cask, source_commit=commit, product_version=info.get("product_version", info["version"]), asset=asset,
                size=assets[asset]["size"], url=origin + asset, metadata=info)


def safe_archive(path):
    """Bound extraction and reject traversal/special entries before invoking ditto."""
    seen, links, total = set(), {}, 0
    with zipfile.ZipFile(path) as archive:
        for member in archive.infolist():
            original = member.filename.rstrip("/")
            # Qualification bundles use ASCII paths. Refuse Unicode rather than
            # assume Python folding exactly models every APFS/HFS+ normalization
            # rule; canonical ASCII keys account for normal case-insensitive Macs.
            require(original.isascii() and member.orig_filename == member.filename and "\0" not in original,
                    "Non-ASCII or truncated ZIP member path requires separate review.")
            name = original.casefold()
            require(name not in seen and (original == "Zerus.app" or original.startswith("Zerus.app/"))
                    and not any(part in ("", ".", "..") for part in name.split("/")) and "\\" not in name,
                    "Unexpected ZIP member path.")
            seen.add(name)
            mode = member.external_attr >> 16
            require(stat.S_IFMT(mode) in (0, stat.S_IFREG, stat.S_IFDIR, stat.S_IFLNK), "Special ZIP member.")
            total += member.file_size
            require(total <= 2 * 1024**3 and len(seen) <= 50000, "Expanded bundle exceeds its bound.")
            if stat.S_ISLNK(mode):
                require(member.file_size <= 4096, "Oversized ZIP symlink.")
                target = archive.read(member).decode("utf-8")
                require(target and target.isascii() and not target.startswith("/") and "\\" not in target and "\0" not in target,
                        "Escaping ZIP symlink.")
                links[name] = target.casefold()
    # ditto must never write a member through an earlier (or later) stored
    # symlink. Qt frameworks store their real Versions/A tree separately.
    require("zerus.app" not in links, "Bundle root cannot be a symlink.")
    for name in seen:
        parent = posixpath.dirname(name)
        while parent:
            require(parent not in links, "ZIP member has a stored symlink ancestor.")
            parent = posixpath.dirname(parent)
    for name, target in links.items():
        pending = posixpath.dirname(name).split("/") + target.split("/")
        resolved, followed = [], set()
        steps = 0
        while pending:
            steps += 1
            require(steps <= 512, "ZIP link resolution exceeds its bound.")
            component, pending = pending[0], pending[1:]
            if component in ("", "."):
                continue
            if component == "..":
                require(len(resolved) > 1, "ZIP link escapes the bundle root.")
                resolved.pop()
                continue
            resolved.append(component)
            require(resolved[0] == "zerus.app", "ZIP link escapes the bundle root.")
            prefix = "/".join(resolved)
            if prefix in links:
                require(prefix not in followed, "Cyclic ZIP symlink graph.")
                followed.add(prefix)
                resolved.pop()
                pending = links[prefix].split("/") + pending
        require(resolved and resolved[0] == "zerus.app", "ZIP link escapes the bundle root.")


def account_identity():
    # HOME/CI environment is not proof of the OS account used by CFPreferences
    # or Homebrew. This is invoked only after the macOS platform guard.
    import pwd
    uid, euid = os.getuid(), os.geteuid()
    return uid, euid, pwd.getpwuid(uid)


def prepare_public(commit, channel, output, metadata_fetch=fetch):
    commit = exact_commit(commit)
    output.mkdir(parents=True)
    # Only the real public tap repository supplies executable cask code.
    tap_api = json.loads(metadata_fetch("https://api.github.com/repos/ufna/homebrew-zerus/commits/" + commit))
    require(tap_api.get("sha") == commit, "Tap commit lookup differs.")
    name = cask_for(channel)
    raw = fetch(f"https://raw.githubusercontent.com/ufna/homebrew-zerus/{commit}/Casks/{name}.rb")
    cask = accepted_cask(raw.decode("utf-8"), channel)
    release = json.loads(metadata_fetch("https://api.github.com/repos/ufna/zerus/releases/tags/" + cask["tag"]))
    origin = "https://github.com/ufna/zerus/releases/download/" + cask["tag"] + "/"
    info = fetch(origin + "release-info.json")
    sums = fetch(origin + "SHA256SUMS")
    proof = release_proof(cask, release, info, sums)
    (output / "public.rb").write_bytes(raw)
    (output / "release-info.json").write_bytes(info)
    (output / "SHA256SUMS").write_bytes(sums)
    (output / "github-release.json").write_text(json.dumps(release, indent=2) + "\n")
    archive = fetch(proof["url"], 512 * 1024**2)
    require(len(archive) == proof["size"] and hashlib.sha256(archive).hexdigest() == cask["sha256"],
            "Public ZIP bytes differ from their immutable seals.")
    (output / proof["asset"]).write_bytes(archive)
    proof.update(tap_commit=commit, cask=name, directory=str(output))
    (output / "proof.json").write_text(json.dumps(proof, indent=2) + "\n")
    return proof


def disposable_mac(env, home):
    require(platform.system() == "Darwin" and platform.machine() == "arm64", "Apple Silicon macOS is required.")
    require(env.get("GITHUB_ACTIONS") == "true" and env.get("RUNNER_ENVIRONMENT") == "github-hosted"
            and env.get("RUNNER_OS") == "macOS" and env.get("RUNNER_TEMP"),
            "This lifecycle test only runs on a disposable standard GitHub-hosted Mac.")
    resolved = home.resolve()
    uid, euid, account = account_identity()
    require(uid > 0 and uid == euid == account.pw_uid and account.pw_name in ("runner", "runneradmin")
            and resolved.parent == Path("/Users") and resolved.name == account.pw_name
            and Path(account.pw_dir).resolve() == resolved,
            "Refusing a non-runner account.")
    require(not APP.exists() and not APP.is_symlink(), "A Zerus installation already exists.")
    require(not (home / "Library/Preferences" / (DOMAIN + ".plist")).exists(),
            "The disposable runner already has Zerus preferences.")


class Qualification:
    def __init__(self, output):
        self.output = output
        self.commands = 0
        self.gui = None
        self.gui_runtime = None
        self.native = None
        self.installed = None
        self.phases = []

    def run(self, args, *, env=None, timeout=180):
        self.commands += 1
        result = subprocess.run([str(x) for x in args], env=env, stdin=subprocess.DEVNULL,
                                capture_output=True, text=True, timeout=timeout)
        (self.output / f"command-{self.commands:03}.json").write_text(json.dumps(
            {"argv": [str(x) for x in args], "returncode": result.returncode,
             "stdout": result.stdout, "stderr": result.stderr}, indent=2) + "\n")
        require(result.returncode == 0, "Command failed: " + " ".join(str(x) for x in args))
        return result.stdout

    def native_signature(self, app, proof):
        self.run(["codesign", "--verify", "--deep", "--strict", app])
        self.run(["spctl", "--assess", "--type", "execute", "--verbose=2", app])
        self.run(["xcrun", "stapler", "validate", app])
        identity = subprocess.run(["codesign", "-d", "--verbose=4", str(app)],
                                  capture_output=True, text=True, timeout=30)
        require(identity.returncode == 0 and "TeamIdentifier=" + TEAM in identity.stderr
                and "Authority=Developer ID Application:" in identity.stderr,
                "Unexpected native application signing identity.")
        plist = plistlib.loads((app / "Contents/Info.plist").read_bytes())
        require(plist.get("CFBundleIdentifier") == DOMAIN and plist.get("ZerusReleaseVersion") == proof["version"]
                and plist.get("CFBundleShortVersionString") == proof["product_version"],
                "Application bundle identity differs from the public release.")

    def select_tap(self, proof):
        tap = Path(self.run(["brew", "--repository", "ufna/zerus"]).strip())
        self.run(["git", "-C", tap, "fetch", "--quiet", "origin", proof["tap_commit"]])
        self.run(["git", "-C", tap, "checkout", "--detach", proof["tap_commit"]])
        require((tap / "Casks" / (proof["cask"] + ".rb")).read_bytes()
                == (Path(proof["directory"]) / "public.rb").read_bytes(), "Local public tap bytes differ.")

    def install(self, proof, upgrade=False):
        self.select_tap(proof)
        self.run(["brew", "upgrade" if upgrade else "install", "--cask", "ufna/zerus/" + proof["cask"]], timeout=600)
        self.installed = proof["cask"]
        self.native_signature(APP, proof)  # Assess quarantine before execution/attribute changes.
        # Only this verified installation in a disposable hosted account loses
        # quarantine, as in the existing clean-cask test. No TCC permission changes.
        self.run(["xattr", "-dr", "com.apple.quarantine", APP])
        info = json.loads(self.run([APP / "Contents/MacOS/hgs-tray", "--build-info"], env=self.env))
        require(info.get("sourceCommit") == proof["source_commit"] and info.get("sourceDirty") is False,
                "Installed GUI source differs from the sealed public source.")
        self.run([APP / "Contents/MacOS/hgs-tray", "--selftest"], env=self.env)

    def seed_state(self):
        # Native-format preferences belong to the disposable runner account;
        # HOME alone is not claimed to isolate macOS CFPreferences.
        for key, value in SETTINGS.items():
            self.run(["defaults", "write", DOMAIN, key, "-int" if value.isdigit() else "-string", value])
        self.drafts = Path.home() / "Library/Preferences" / (DOMAIN + ".plist.drafts")
        key = "\n" + self.native.name
        folder = self.drafts / hashlib.sha256(key.encode()).hexdigest()
        folder.mkdir(parents=True, mode=0o700)
        payload = b"Private synthetic immutable attachment.\n"
        digest = hashlib.sha256(payload).hexdigest()
        (folder / (digest + ".bin")).write_bytes(payload)
        self.draft_file = folder / "draft.json"
        self.draft_file.write_text(json.dumps({"version": 1, "key": key, "text": "Retain this private cask upgrade draft.",
            "position": 7, "anchor": 3, "label": "Cask fixture", "form_state": {"fixture_answer": "Retain this staged answer"}, "next_attachment_number": 2,
            "sending": False, "uncertain": False, "attachments": [{"name": "fixture.txt", "mime": "text/plain",
            "reference": "fixture.txt", "sha256": digest, "bytes": len(payload)}]}))
        self.protected_draft = self.payload_state()

    def payload_state(self):
        draft = json.loads(self.draft_file.read_text())
        # No durable field is volatile: preserve selection, form, label,
        # attachment numbering and any future retained fields as well.
        return draft, {
            p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in self.draft_file.parent.glob("*.bin")}

    def native_state(self):
        n = self.native
        pane_pid = n.tmux("display-message", "-p", "-t", n.pane, "#{pane_pid}").strip()
        server_pid = n.tmux("display-message", "-p", "#{pid}").strip()
        start = self.run(["ps", "-p", pane_pid, "-o", "lstart="], env=dict(self.env, LC_ALL="C", TZ="UTC")).strip()
        server_start = self.run(["ps", "-p", server_pid, "-o", "lstart="], env=dict(self.env, LC_ALL="C", TZ="UTC")).strip()
        require("Fixture complete." in n.screen(), "Native TUI no longer shows its completed private turn.")
        rows = list((n.root / ".claude/projects").glob("*/*.jsonl"))
        require(len(rows) == 1 and rows[0].stem == n.conversation_id, "Native conversation changed.")
        record = json.loads(n.record_path.read_text())
        live_run = n.tmux("show-options", "-v", "-t", "=" + n.name + ":", "@hgs_run").strip()
        live_name = n.tmux("display-message", "-p", "-t", n.pane, "#{session_name}").strip()
        live_pane = n.tmux("display-message", "-p", "-t", n.pane, "#{pane_id}").strip()
        require(record.get("name") == live_name == n.name and record.get("run_id") == live_run == n.run_id
                and record.get("conversation_id") == rows[0].stem == n.conversation_id
                and record.get("pane") == live_pane == n.pane and record.get("pid") == int(pane_pid)
                and record.get("process_start") == start, "Durable/live native target binding changed.")
        return {"pane_pid": pane_pid, "server_pid": server_pid, "start": start, "run": record["run_id"],
                "server_start": server_start, "conversation": record["conversation_id"], "pane": live_pane,
                "api_requests": len(n.native_requests)}

    def prepare_gui_runtime(self):
        require(self.gui_runtime is None, "A private GUI runtime already exists.")
        # A resolved native fixture below /private/var/folders can put hgs-tray
        # at Darwin's 104-byte sockaddr_un limit. Own a short separate directory.
        self.gui_runtime = tempfile.TemporaryDirectory(prefix="zerus-cask-ipc-", dir="/tmp")
        self.env["TMPDIR"] = str(Path(self.gui_runtime.name).resolve())

    def gui_runtime_info(self):
        runtime = Path(self.env["TMPDIR"])
        canonical = runtime.resolve(strict=True)
        require(self.gui_runtime is not None and runtime == canonical
                == Path(self.gui_runtime.name).resolve(strict=True), "GUI runtime is not this fixture's directory.")
        attributes = canonical.stat()
        require(attributes.st_uid == os.geteuid() and stat.S_IMODE(attributes.st_mode) == 0o700,
                "GUI runtime must be private to this fixture account.")
        socket_path = runtime / "hgs-tray"
        canonical_socket = canonical / "hgs-tray"
        paths = {"runtime": runtime, "canonical_runtime": canonical,
                 "socket": socket_path, "canonical_socket": canonical_socket}
        proof = {name: str(path) for name, path in paths.items()}
        proof.update({name + "_bytes": len(os.fsencode(path)) for name, path in paths.items()})
        require(proof["socket_bytes"] <= 90 and proof["canonical_socket_bytes"] <= 90,
                "Private GUI IPC path exceeds its short-path budget.")
        return proof

    def start_gui(self, phase):
        (self.output / (phase + "-ipc.json")).write_text(json.dumps(self.gui_runtime_info(), indent=2) + "\n")
        log = (self.output / (phase + "-gui.log")).open("w")
        self.gui = subprocess.Popen([str(APP / "Contents/MacOS/hgs-tray"), "--sessions", "--hgs",
                                     str(APP / "Contents/MacOS/hgs")], env=self.env,
                                    stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
        log.close()
        # Public app has no automation API for its editor. An owned private IPC
        # socket proves startup; it is not an interactive keystroke/TCC test.
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            require(self.gui.poll() is None, "Private GUI exited during startup.")
            try:
                with socket.socket(socket.AF_UNIX) as probe:
                    probe.settimeout(.2)
                    probe.connect(str(Path(self.env["TMPDIR"]) / "hgs-tray"))
                return
            except OSError:
                time.sleep(.05)
        raise ValueError("The private GUI did not publish its owned IPC socket.")

    def stop_gui(self):
        if self.gui is not None and self.gui.poll() is None:
            self.gui.send_signal(signal.SIGTERM)
            self.gui.wait(timeout=20)
        self.gui = None

    def checkpoint(self, phase, old_gui_pid=None):
        require(self.native_state() == self.native_identity, "Native process or conversation changed during cask work.")
        require(self.payload_state() == self.protected_draft, "Draft payload/attachment bytes changed during cask work.")
        for key, value in SETTINGS.items():
            require(self.run(["defaults", "read", DOMAIN, key]).strip() == value, "Saved preference changed.")
        if old_gui_pid is not None:
            require(self.gui is not None and self.gui.pid == old_gui_pid and self.gui.poll() is None,
                    "Homebrew stopped/replaced the running GUI without explicit fixture restart.")
        self.phases.append({"phase": phase, "native_retained": True, "draft_retained": True,
                            "settings_retained": True, "running_gui_retained": old_gui_pid is not None})

    def open_native(self, directory):
        spec = importlib.util.spec_from_file_location("zerus_agent_compatibility", ROOT / "scripts/ci/agent-compatibility.py")
        compatibility = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(compatibility)
        pins = json.loads(compatibility.MANIFEST.read_text())["claude"]
        package = next(row for row in reversed(pins) if row["target"] == "darwin-arm64")
        native_bin = compatibility.install_native("claude", package, directory / "native")
        require(compatibility.probe(native_bin, ["--version"], compatibility.fixture_environment(directory / "probe-home")).strip()
                == compatibility.AGENTS["claude"]["version_output"].format(package["version"]), "Native pin version differs.")
        os.environ["HGS_CLAUDE_TEST_BIN"] = str(native_bin)
        os.environ["HGS_TEST_BIN"] = str(APP / "Contents/MacOS/hgs")
        sys.path.insert(0, str(ROOT / "tests"))
        import test_claude_questions_native as native
        self.native = native.NativeClaudeQuestions("runTest")
        self.native.message_fixture = True
        try:
            self.native.setUp()
        except BaseException:
            self.native.doCleanups()
            raise
        self.env = self.native.env
        self.env["HGS_PEERS"] = ""
        self.prepare_gui_runtime()
        self.native_identity = self.native_state()
        (self.output / "native-package-pin.json").write_text(json.dumps(package, indent=2) + "\n")

    def lifecycle(self, old, new, nightly):
        require(cask_version_key(old["version"]) < cask_version_key(new["version"]), "Stable upgrade must increase version.")
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(("HGS_", "ANTHROPIC_", "OPENAI_", "CODEX_", "CLAUDE", "GH_", "GITHUB_TOKEN"))}
        self.env.update(HOME=str(self.output / "selftest-home"), TMUX_TMPDIR=str(self.output / "selftest-tmux"))
        Path(self.env["HOME"]).mkdir()
        Path(self.env["TMUX_TMPDIR"]).mkdir()
        # Every cask and payload is checked before brew can evaluate Ruby.
        for proof in (old, new, nightly):
            directory = Path(proof["directory"]) / "unpacked"
            safe_archive(Path(proof["directory"]) / proof["asset"])
            self.run(["ditto", "-x", "-k", Path(proof["directory"]) / proof["asset"], directory])
            self.native_signature(directory / "Zerus.app", proof)
        self.run(["brew", "tap", "ufna/zerus"])
        self.install(old)  # A real clean install from the public tap, not a file URL.
        self.open_native(self.output)
        self.seed_state()
        self.start_gui("old-stable")
        self.checkpoint("old-stable-clean-install")
        previous = self.gui.pid
        self.install(new, upgrade=True)  # Real Homebrew upgrade between published versions.
        self.checkpoint("stable-version-upgrade", previous)
        self.stop_gui()
        self.start_gui("new-stable")
        self.checkpoint("new-stable-restart")
        for proof, phase in ((nightly, "stable-to-nightly"), (new, "nightly-to-stable")):
            previous = self.gui.pid
            self.run(["brew", "uninstall", "--cask", self.installed])  # Never --zap or setup --remove.
            self.installed = None
            self.checkpoint(phase + "-uninstalled", previous)
            self.install(proof)
            self.checkpoint(phase + "-installed", previous)
            self.stop_gui()
            self.start_gui(phase)
            self.checkpoint(phase + "-restart")

    def cleanup(self):
        errors = []
        try:
            try:
                self.stop_gui()
            except subprocess.TimeoutExpired:
                # Only the exact Popen child owned by this disposable fixture.
                self.gui.kill()
                self.gui.wait(timeout=10)
                self.gui = None
                errors.append("The private GUI did not finish its normal durable shutdown.")
            if self.native:
                if not self.native.doCleanups():
                    errors.append("Private native fixture cleanup failed.")
            if self.installed:
                try:
                    self.run(["brew", "uninstall", "--cask", self.installed])
                    self.installed = None
                except (ValueError, subprocess.TimeoutExpired) as error:
                    errors.append(str(error))
        finally:
            if self.gui_runtime is not None:
                self.gui_runtime.cleanup()
                self.gui_runtime = None
        require(not errors, "; ".join(errors))


def main():
    metadata = consume_metadata_token()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    require(sys.version_info >= (3, 11), "The existing native fixture requires Python 3.11 or newer.")
    disposable_mac(os.environ, Path.home())
    output = args.output.resolve()
    require(not output.exists(), "Evidence output already exists; keep each run immutable.")
    output.mkdir(parents=True, mode=0o700)
    q = Qualification(output)
    receipt = {"public_tap_install": False, "cask_upgrade": False, "channel_switch": False,
               "interactive_terminal": "NOT_QUALIFIED", "owner_account_install": False,
               "cleanup_passed": False, "completed": False}
    try:
        old = prepare_public(os.environ["OLD_STABLE_TAP_COMMIT"], "stable", output / "old-stable", metadata.fetch)
        new = prepare_public(os.environ["NEW_STABLE_TAP_COMMIT"], "stable", output / "new-stable", metadata.fetch)
        nightly = prepare_public(os.environ["NIGHTLY_TAP_COMMIT"], "nightly", output / "nightly", metadata.fetch)
        q.lifecycle(old, new, nightly)
        receipt.update(public_tap_install=True, cask_upgrade=True, channel_switch=True,
                       phases=q.phases, sources=[p["source_commit"] for p in (old, new, nightly)])
    finally:
        try:
            q.cleanup()
            receipt["cleanup_passed"] = True
            receipt["completed"] = all(receipt[key] for key in ("public_tap_install", "cask_upgrade", "channel_switch"))
        finally:
            # Evidence artifacts contain small receipts/logs, never a second
            # distribution of downloaded bundles or the official native agent.
            for name in ("old-stable", "new-stable", "nightly"):
                shutil.rmtree(output / name / "unpacked", ignore_errors=True)
                for archive in (output / name).glob("*.zip"):
                    archive.unlink()
            shutil.rmtree(output / "native", ignore_errors=True)
            (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")


if __name__ == "__main__":
    main()
