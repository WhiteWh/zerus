#!/usr/bin/env python3
"""Compare actual receipt I/O and alternating inspection latency, in private state.

Usage: receipt-index-control.py --baseline-bin PATH --candidate-bin PATH --output DIR
Both executables must be locally built hgs images. Requires Unix tmux and strace.
No native provider or owner state is used. The retained traces contain fixtures only.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import statistics
import subprocess
import sys
import time
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline-bin", type=Path, required=True)
    parser.add_argument("--candidate-bin", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    baseline, candidate = args.baseline_bin.resolve(), args.candidate_bin.resolve()
    if not shutil.which("strace") or not shutil.which("tmux"):
        parser.error("tmux and strace are required")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    repo = Path(__file__).resolve().parents[2]
    os.environ["HGS_TEST_BIN"] = str(candidate)
    sys.path.insert(0, str(repo / "tests"))
    import test_input
    fixture = test_input.InputTransport()
    fixture.setUp()
    try:
        for key in ("CLAUDE_CONFIG_DIR", "CODEX_HOME", "KIMI_CODE_HOME", "DSH_HOME", "OPENCODE_HOME"):
            fixture.env.pop(key, None)
        directory = fixture.state / "input_receipts"
        directory.mkdir()
        expected = []
        for n in range(12_000):
            request = str(uuid.UUID(int=n + 1))
            if n < 2:
                expected.append(request)
            value = dict(request_id=request, status="submitted", name=fixture.name,
                         run_id=fixture.run_id,
                         conversation_id=fixture.conversation_id if n < 2 else "foreign-conversation",
                         digest=hashlib.sha256(str(n).encode()).hexdigest(),
                         submitted_at=float(n), text=f"fixture {n}", submitted_text=f"fixture {n}",
                         attachments=[dict(name="image.png", mime="image/png", bytes=1,
                                           reference="[Image #1]", path="/private/fixture/image.png")])
            (directory / f"{request}.json").write_text(json.dumps(value))

        def fingerprints():
            return {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in directory.glob("*.json")}

        before = fingerprints()
        migration = subprocess.run([str(candidate), "__state", "receipt-index-worker"],
                                   env=fixture.env, text=True, capture_output=True, timeout=30)
        assert migration.returncode == 0, migration.stderr
        # Keep discovery settled during both image samples, including a mutant
        # that erroneously marks the migrated namespace as incomplete.
        with (fixture.state / "receipt-index-worker.lock").open("a+b") as lease:
            fcntl.flock(lease, fcntl.LOCK_EX)
            binaries = dict(baseline=baseline, candidate=candidate)
            results = {}

            def command(binary):
                return [str(binary), "__state", "inspect", fixture.name, "--skip-processes"]

            for label, binary in binaries.items():
                trace = output / f"{label}.strace"
                result = subprocess.run(["strace", "-yy", "-e", "trace=%file,read,getdents64,fstat",
                                         "-o", str(trace), *command(binary)],
                                        env=fixture.env, text=True, capture_output=True, timeout=30)
                assert result.returncode == 0, result.stderr
                (output / f"{label}.json").write_text(result.stdout)
                value = json.loads(result.stdout)
                lines = trace.read_text().splitlines()
                prefix = str(directory)
                results[label] = dict(
                    image_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                    receipt_stats=sum(bool(re.match(r"\w*stat\w*\(", line)) and prefix in line
                                      and ".json" in line for line in lines),
                    receipt_opens=sum(line.startswith(("openat(", "openat2(")) and prefix in line
                                      and ".json" in line for line in lines),
                    receipt_reads=sum(line.startswith("read(") and prefix in line and ".json" in line
                                      for line in lines),
                    receipt_directory_reads=sum(line.startswith("getdents64(") and prefix in line
                                                for line in lines),
                    projected_request_ids=[v["message_id"] for v in value["attachment_messages"]],
                    latency_seconds=[])
            # Alternate order to avoid assigning a warm-up/load trend to one image.
            loads = []
            for sample in range(12):
                loads.append(Path("/proc/loadavg").read_text().strip())
                for label in (("baseline", "candidate") if sample % 2 == 0 else ("candidate", "baseline")):
                    start = time.monotonic()
                    result = subprocess.run(command(binaries[label]), env=fixture.env, text=True,
                                            capture_output=True, timeout=30)
                    elapsed = time.monotonic() - start
                    assert result.returncode == 0, result.stderr
                    results[label]["latency_seconds"].append(elapsed)
            for result in results.values():
                result["median_seconds"] = statistics.median(result["latency_seconds"])
            summary = dict(receipts=12_000, expected_request_ids=expected,
                           source_head=subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo,
                                                               text=True).strip(),
                           source_diff_sha256=hashlib.sha256(subprocess.check_output(["git", "diff", "HEAD"],
                                                                                   cwd=repo)).hexdigest(),
                           loads=loads, results=results, original_bytes_unchanged=before == fingerprints())
            (output / "results.json").write_text(json.dumps(summary, indent=2))
            print(json.dumps(summary), flush=True)
            assert summary["original_bytes_unchanged"]
            old, new = results["baseline"], results["candidate"]
            assert old["receipt_stats"] >= 12_000 and old["receipt_opens"] == 512, old
            assert not old["projected_request_ids"], old
            assert new["receipt_stats"] <= 4 and new["receipt_opens"] == 2 and new["receipt_reads"] <= 32, new
            assert new["receipt_directory_reads"] == 0, new
            assert set(new["projected_request_ids"]) == set(expected), new
            # A performance failure remains a failure; this control changes no deadline.
            assert new["median_seconds"] < old["median_seconds"], summary
    finally:
        fixture.tearDown()
        fixture.doCleanups()


if __name__ == "__main__":
    main()
