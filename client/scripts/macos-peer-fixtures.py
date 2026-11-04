#!/usr/bin/env python3
"""Qualify peer observations headlessly under the project's owned Mac heavy lock.

Use digest-pinned Node/Deno inputs as separately signed synthetic socket peers.
C fixtures use ad-hoc signing. This changes no keychains, installed apps, services
or notification state, and never selects an installed signing identity.
"""
import argparse
import hashlib
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import subprocess
import sys


def sha256(path):
    value = hashlib.sha256()
    with path.open("rb") as source:
        while True:
            chunk = source.read(1024 * 1024)
            if not chunk:
                return value.hexdigest()
            value.update(chunk)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["node", "deno", "test-bin", "scratch"]:
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--node-sha256", required=True)
    parser.add_argument("--deno-sha256", required=True)
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("native macOS qualification only")
    test = args.test_bin.resolve()
    if not test.is_file():
        parser.error("the built test executable must exist")
    inputs = [("node", args.node.resolve(), args.node_sha256),
              ("deno", args.deno.resolve(), args.deno_sha256)]
    for name, path, expected in inputs:
        if not re.fullmatch("[0-9a-f]{64}", expected) or not path.is_file() or sha256(path) != expected:
            parser.error(name + " does not match its pinned binary digest")
    root = args.scratch.resolve()
    if len(str(root)) > 65:
        parser.error("use a short private scratch path for Unix socket limits")
    os.umask(0o077)
    root.mkdir(mode=0o700)
    env = {"PATH": "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "C.UTF-8",
           "DENO_NO_UPDATE_CHECK": "1"}
    for name, folder in [("HOME", "home"), ("TMPDIR", "tmp"),
                         ("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"),
                         ("XDG_CACHE_HOME", "cache"), ("XDG_STATE_HOME", "state"),
                         ("XDG_RUNTIME_DIR", "runtime"), ("TMUX_TMPDIR", "tmux"),
                         ("DENO_DIR", "deno-cache")]:
        path = root / folder
        path.mkdir(mode=0o700)
        env[name] = str(path)
    env["ZERUS_MAC_PEER_FIXTURES"] = str(root)

    def run(command, capture=False):
        command = [str(x) for x in command]
        print("EXECUTED", repr(command), flush=True)
        return subprocess.run(command, env=env, cwd=root, check=True, timeout=90,
                              capture_output=capture, text=capture)

    teams = []
    for name, original, expected in inputs:
        destination = root / name
        shutil.copy2(original, destination)
        if sha256(destination) != expected:
            raise ValueError("copied runtime changed")
        run(["codesign", "--verify", "--strict", destination])
        result = run(["codesign", "--display", "--verbose=4", destination], capture=True)
        metadata = result.stdout + result.stderr
        print(metadata, flush=True)
        match = re.search(r"^TeamIdentifier=([A-Z0-9]{10})$", metadata, re.MULTILINE)
        if not match or "Signature=adhoc" in metadata:
            raise ValueError("runtime lacks a genuine signing team")
        teams.append(match.group(1))
    if teams[0] == teams[1]:
        raise ValueError("the two pinned runtimes must have different signing teams")
    # Pinned input-file metadata, never a requirement received from a live peer.
    env["ZERUS_MAC_PEER_TEAM_REQUIREMENT"] = (
        'anchor apple generic and certificate leaf[subject.OU] = "' + teams[0] + '"')
    source = Path(__file__).resolve().parent.parent / "core/tests/support/macos_peer.c"
    run(["cc", "-Wall", "-Wextra", "-Werror", source, "-o", root / "peer-base"])
    for name in ["a", "b", "adhoc"]:
        destination = root / ("peer-" + name)
        shutil.copy2(root / "peer-base", destination)
        run(["codesign", "--force", "--sign", "-", "--identifier",
             "com.example.zerus-peer-" + name, destination])
    script = root / "server.cjs"
    script.write_text('''const net = require("node:net");
const server = net.createServer(socket => {
  socket.on("data", bytes => {
    for (const byte of bytes) {
      if (byte === 113) process.exit(0);
      socket.write(Buffer.from([byte]));
    }
  });
  socket.on("error", () => process.exit(0));
  socket.on("close", () => process.exit(0));
});
server.on("error", () => process.exit(3));
server.listen(process.argv[2]);
''', encoding="utf-8")
    for name in ["node", "deno"]:
        wrapper = root / ("peer-" + name)
        command = [str(root / name)]
        if name == "deno":
            command.extend(["run", "--cached-only", "--no-config", "--no-remote", "--allow-all"])
        command.append(str(script))
        wrapper.write_text("#!/bin/sh\nexec " + " ".join(shlex.quote(x) for x in command) + ' "$1"\n', encoding="utf-8")
        wrapper.chmod(0o700)
    test_copy = root / "core-test"
    shutil.copy2(test, test_copy)
    command = [str(test_copy), "macos_notify::peer::tests::", "--ignored", "--test-threads=1", "--nocapture"]
    print("EXECUTED", repr(command), flush=True)
    process = subprocess.Popen(command, env=env, cwd=root, start_new_session=True)
    try:
        code = process.wait(timeout=120)
    except subprocess.TimeoutExpired:
        # Only this fresh test session and its synthetic children.
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        raise
    if code:
        raise subprocess.CalledProcessError(code, command)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
