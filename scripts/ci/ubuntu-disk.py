#!/usr/bin/env python3
"""Reclaim unused build space only on the two disposable hosted Ubuntu jobs."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess


# Neither job uses Android, .NET, Haskell or CodeQL. Never prune Docker here:
# the relay's PostgreSQL service is already running when checkout starts.
UNUSED_TOOLS = (
    "/usr/local/lib/android",
    "/usr/share/dotnet",
    "/opt/ghc",
    "/opt/hostedtoolcache/CodeQL",
)
RELAY_TARGETS = ("target", "services/mobile/relay/target", "services/mobile/crypto/target")


def hosted_workspace():
    required = {
        "GITHUB_ACTIONS": "true",
        "RUNNER_ENVIRONMENT": "github-hosted",
        "RUNNER_OS": "Linux",
    }
    if any(os.environ.get(key) != value for key, value in required.items()):
        raise ValueError("disk cleanup is restricted to disposable hosted Linux jobs")
    if os.environ.get("GITHUB_JOB") not in ("quick", "relay"):
        raise ValueError("unexpected hosted job for disk cleanup")
    workspace = Path(os.environ["GITHUB_WORKSPACE"])
    if not workspace.is_absolute() or workspace.resolve() != Path.cwd().resolve():
        raise ValueError("run from the original checked-out workspace")
    return workspace.resolve()


def report(workspace, phase):
    usage = shutil.disk_usage(workspace)
    print(json.dumps({"phase": phase, "disk_bytes": {
        "total": usage.total, "used": usage.used, "free": usage.free,
    }}), flush=True)
    paths = [workspace / path for path in RELAY_TARGETS]
    paths.append(Path.home() / ".cargo/registry")
    existing = [str(path) for path in paths if path.exists()]
    if existing:
        try:
            measured = subprocess.run(
                ["du", "-sh", "--", *existing], capture_output=True, text=True, timeout=15,
            )
            print(measured.stdout[:8192], end="", flush=True)
            if measured.returncode:
                print("directory sizes unavailable: " + measured.stderr[:1024], flush=True)
        except subprocess.TimeoutExpired:
            print("directory size measurement exceeded 15 seconds", flush=True)


def prepare(workspace):
    report(workspace, "before-unused-tools")
    subprocess.run(["sudo", "rm", "-rf", "--one-file-system", "--", *UNUSED_TOOLS], check=True)
    report(workspace, "after-unused-tools")


def relay_before_image(workspace):
    if os.environ.get("GITHUB_JOB") != "relay":
        raise ValueError("only relay may retire its completed host builds")
    report(workspace, "before-retiring-host-builds")
    # HTTP/connector contracts have finished. The remaining memory-envelope
    # tests use the newly built Docker image, not any host Cargo executable.
    for relative in RELAY_TARGETS:
        target = workspace / relative
        if target.is_symlink() or target.resolve() != target:
            raise ValueError("changed Cargo target path: " + relative)
        if target.exists():
            shutil.rmtree(target)
    report(workspace, "before-deployment-image")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("prepare", "report", "relay-before-image"))
    parser.add_argument("phase", nargs="?", default="checkpoint")
    args = parser.parse_args()
    workspace = hosted_workspace()
    if args.operation == "prepare":
        prepare(workspace)
    elif args.operation == "relay-before-image":
        relay_before_image(workspace)
    else:
        report(workspace, args.phase)


if __name__ == "__main__":
    main()
