#!/usr/bin/env python3
"""Task 157: the Windows half of the release candidate.

Runs the fork-windows steps after the fork tests: the Tauri build with the
fork channel, the package and the portable self-test. Task #388: no Qt step and
no Qt in the zip. The paths are the workflow's; --dry-run prints them; each
step prints how long it took.
"""
from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def run(command: list[str], cwd: Path = ROOT, env: dict[str, str] | None = None, dry: bool = False) -> None:
    print("+", " ".join(command), flush=True)
    if dry:
        return
    merged = dict(os.environ)
    merged.update(env or {})
    # On Windows npm is npm.cmd: CreateProcess finds only .exe by a bare
    # name, so the program is resolved on PATH first (task 169).
    program = shutil.which(command[0], path=merged.get("PATH")) or command[0]
    started = time.monotonic()
    code = subprocess.run([program, *command[1:]], cwd=cwd, env=merged).returncode
    print(f"  took {time.monotonic() - started:.0f} s", flush=True)
    if code != 0:
        raise SystemExit(f"release candidate step failed: {' '.join(command)}")


def fork_identity(build: str, feed: str) -> tuple[str, str]:
    """Task 213: the fork identity this leg embeds and packages.

    The workflow takes it from the tag and run number (fork-windows.yml); a
    local leg has no tag, so the caller passes the values explicitly (template
    `release-candidate`, `fork_build` and `fork_feed`, or ZERUS_FORK_BUILD and
    ZERUS_FORK_FEED). Both are given together or neither is (an experimental
    build); the packaged body refuses the same, so this fails early with the
    reason.
    """
    build = build.strip()
    feed = feed.strip()
    if bool(build) != bool(feed):
        raise SystemExit("A fork build has both a build number and a feed, or neither (experimental).")
    if build and (not build.isdigit() or int(build) <= 0):
        raise SystemExit(f"The fork build is a positive number or empty (got {build!r}).")
    if feed and not feed.startswith("https://github.com/WhiteWh/zerus/releases/"):
        raise SystemExit(f"The fork feed is a release asset of WhiteWh/zerus (got {feed!r}).")
    return build, feed


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--commit", default="HEAD")
    parser.add_argument("--dry-run", action="store_true")
    # Task 213: the local leg takes the fork identity the workflow gets from its
    # tag and run number. The environment names fork-windows.yml sets are read
    # too, so the same values serve both legs.
    parser.add_argument("--fork-build", default=os.environ.get("ZERUS_FORK_BUILD", ""),
                        help="the fork build number the Tauri app embeds and build-info.json names "
                             "[default: $ZERUS_FORK_BUILD; empty for an experimental build]")
    parser.add_argument("--fork-feed", default=os.environ.get("ZERUS_FORK_FEED") or os.environ.get("ZERUS_FORK_FEED_URL", ""),
                        help="the fork update feed [default: $ZERUS_FORK_FEED, else $ZERUS_FORK_FEED_URL; empty for an experimental build]")
    args = parser.parse_args()
    fork_build, fork_feed = fork_identity(args.fork_build, args.fork_feed)
    print(f"fork identity: build {fork_build or 'none (experimental)'}, feed {fork_feed or 'none (experimental)'}", flush=True)
    run(["cargo", "build", "--locked", "--release", "--bin", "hgs"], dry=args.dry_run)
    run(["cargo", "build", "--locked", "--release", "--manifest-path", "crates/lore-bin/Cargo.toml", "--target-dir", "target/lore"], dry=args.dry_run)
    run(["cargo", "build", "--locked", "--release", "--manifest-path", "services/mobile/connector/Cargo.toml"], dry=args.dry_run)
    run(["cargo", "build", "--locked", "--release", "--manifest-path", "services/mobile/relay/Cargo.toml"], dry=args.dry_run)
    conpty = Path(os.environ.get("RUNNER_TEMP", ROOT / ".zerus-team/tmp")) / "conpty"
    if not conpty.exists():
        run(["python", "scripts/fetch-conpty.py", "--output", str(conpty)], dry=args.dry_run)
    results = ROOT / "artifacts/test-results"
    results.mkdir(parents=True, exist_ok=True)
    client = ROOT / "client"
    target = ROOT / "target/windows-next"
    run(["npm", "ci"], cwd=client, dry=args.dry_run)
    print(f"tauri fork channel: ZERUS_FORK_BUILD={fork_build or '(unset)'} ZERUS_FORK_FEED_URL={fork_feed or '(unset)'}", flush=True)
    run(
        ["npm", "run", "tauri", "--", "build", "--ci", "--no-bundle",
         "--config", "src-tauri/tauri.windows.switch.conf.json", "--features", "video"],
        cwd=client,
        env={"CARGO_TARGET_DIR": str(target), "CARGO_BUILD_JOBS": "4",
             "ZERUS_FORK_BUILD": fork_build,
             "ZERUS_FORK_FEED_URL": fork_feed},
        dry=args.dry_run,
    )
    run(["python", "client/scripts/collect-notices.py", "artifacts/windows-frontend-licenses"], dry=args.dry_run)
    # WhiteWh fork (zerus-y0cj): the model-free artifact executor from pinned Go
    # and desync, with its linked-license notices, shipped beside hgs.exe and as
    # the standalone consumer package (scripts/build-artifact-executor.py).
    run(["python", "scripts/build-artifact-executor.py", "--output", "artifacts/hgs-executor-windows-x86_64.zip"],
        dry=args.dry_run)
    run(["python", "scripts/package-windows.py", "--fork",
         "--fork-build", fork_build, "--fork-feed", fork_feed,
         "--cli", "target/release/hgs.exe", "--lore", "target/lore/release/lore.exe",
         "--gui", "target/windows-next/release/hgs-tray.exe",
         "--connector", "services/mobile/connector/target/release/zerus-mobile-connector.exe",
         "--mobile-relay", "services/mobile/relay/target/release/zerus-relay.exe",
         "--frontend-licenses", "artifacts/windows-frontend-licenses",
         "--executor", "artifacts/hgs-executor-windows-x86_64.zip",
         "--conpty", str(Path(os.environ.get("RUNNER_TEMP", ROOT / ".zerus-team/tmp")) / "conpty"),
         "--output", "artifacts/zerus-windows-x86_64.zip"], dry=args.dry_run)
    run(["python", "scripts/check-windows-portable.py", "artifacts/zerus-windows-x86_64.zip",
         "--run", "--selftest"], dry=args.dry_run)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
