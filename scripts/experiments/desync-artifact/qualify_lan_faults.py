"""Private synthetic LAN fault qualification between two Windows hosts.

Runs independent fault cases in parallel, each in its own folder of the
qualify_lan fresh root with generated data only:
  F1 receiver killed mid-transfer resumes without fetching the done part again
  F2 publisher lost mid-transfer; a new publication of the same tree resumes
  F3 a second receiver on the same slot is refused while the first runs
  F5 a receiver pinned to the wrong peer fingerprint is refused, writes nothing
"""
import argparse
import concurrent.futures
import json
import sys
import time
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import qualify_lan as q  # noqa: E402

MIB = 1024**2
GIB = 1024**3
RUN = time.strftime("%H%M%S")


def ps(host, script, timeout=1800):
    return host.shell("$root='" + host.root + "'\n" + script, timeout=timeout)


def make_source(host, base, size):
    return ps(host, rf"""
$dir=Join-Path $root '{base}\src'
[IO.Directory]::CreateDirectory($dir)|Out-Null
$path=Join-Path $dir 'payload.bin'
if(-not (Test-Path -LiteralPath $path)){{
  $rng=New-Object Random 4242
  $block=New-Object byte[] (4MB)
  $fs=[IO.File]::Open($path,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write)
  try{{ for($i=0;$i -lt {size // (4 * MIB)};$i++){{ $rng.NextBytes($block); $fs.Write($block,0,$block.Length) }} }} finally {{ $fs.Close() }}
}}
@{{sha=(Get-FileHash -LiteralPath $path).Hash.ToLowerInvariant();bytes=(Get-Item -LiteralPath $path).Length}}|ConvertTo-Json -Compress
""")


def publish(dev, peer_fp, base, tag):
    info = STATE["hosts"][dev.alias]
    pub = f"{base}/pub-{tag}"
    dev.write(pub + "/publish.json", {"source_root": dev.path(base + "/src"), "average": 1 * MIB, "serve_seconds": 3600,
                                      "stash_bytes": 0, "memory_bytes": 256 * MIB, "delivery_id": "0" * 32,
                                      "peer": {"identity": dev.path("identity-init/identity"), "fingerprint": peer_fp,
                                               "listen": info["ip"] + ":0"}})
    label = f"{base.replace('/', '-')}-pub-{tag}-{RUN}"
    dev.start(label, "tree-serve", pub + "/publish.json")
    for _ in range(180):
        try:
            return label, dev.call("read", {"path": pub + "/tree-offer.json"})
        except RuntimeError:
            if dev.call("status", {"label": label}).get("state") in ("failed", "finished"):
                raise RuntimeError(base + ": publication ended before offering")
            time.sleep(1)
    raise RuntimeError(base + ": no offer")


def receive_config(build, dev, base, offer, delivery, fingerprint=None):
    build.write(base + "/slot/receive.json", {"offer": offer, "delivery_id": delivery, "stash_bytes": 16 * MIB,
                                             "memory_bytes": 256 * MIB,
                                             "peer": {"identity": build.path("identity-init/identity"),
                                                      "fingerprint": fingerprint or STATE["hosts"][dev.alias]["fingerprint"]}})


def receiver_progress(build, base):
    return ps(build, rf"""
$slot=Join-Path $root '{base}\slot'
[long]$part=0
foreach($f in @(Get-ChildItem -LiteralPath $slot -Recurse -File -ErrorAction SilentlyContinue)){{ if($f.Length -ge 1MB){{ $part=[Math]::Max($part,$f.Length) }} }}
$results=@();foreach($f in @(Get-ChildItem -LiteralPath (Join-Path $slot 'files') -Filter result.json -Recurse -File -ErrorAction SilentlyContinue)){{ $results+=(Get-Content -LiteralPath $f.FullName -Raw|ConvertFrom-Json) }}
$installed=$null; $p=Join-Path $slot 'tree-installed.json'; if(Test-Path -LiteralPath $p){{ $installed=(Get-Content -LiteralPath $p -Raw|ConvertFrom-Json).state }}
$err=$null; $e=Join-Path $slot 'error.json'; if(Test-Path -LiteralPath $e){{ $err=(Get-Content -LiteralPath $e -Raw|ConvertFrom-Json).error }}
$sha=$null; $b=Join-Path $slot 'build\payload.bin'; if($installed -eq 'ready' -and (Test-Path -LiteralPath $b)){{ $sha=(Get-FileHash -LiteralPath $b).Hash.ToLowerInvariant() }}
@{{largest=$part;results=$results;installed=$installed;error=$err;sha=$sha}}|ConvertTo-Json -Compress -Depth 5
""", timeout=300)


def wait_share(build, base, label, size, share, rate=12e6):
    """Wait until about `share` of `size` has crossed the link. The receiver
    preallocates its file, so its length says nothing about progress."""
    end = time.monotonic() + share * size / rate
    while time.monotonic() < end:
        if build.call("status", {"label": label}).get("state") in ("failed", "finished"):
            raise RuntimeError(base + ": receiver ended before the interruption point")
        time.sleep(2)


def wait_bytes(build, base, label, threshold, seconds=900):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        prog = receiver_progress(build, base)
        if prog["largest"] >= threshold:
            return prog
        if build.call("status", {"label": label}).get("state") in ("failed", "finished"):
            raise RuntimeError(base + ": receiver ended before the interruption point: " + json.dumps(prog)[:300])
        time.sleep(2)
    raise TimeoutError(base + ": no progress")


def f1_receiver_killed(dev, build, size):
    base = f"faults/f1-{RUN}"
    src = make_source(dev, base, size)
    label, offer = publish(dev, STATE["hosts"][build.alias]["fingerprint"], base, "a")
    delivery = uuid.uuid4().hex
    receive_config(build, dev, base, offer, delivery)
    first = f"{base.replace('/', '-')}-recv-a"
    build.start(first, "tree-receive", base + "/slot/receive.json")
    wait_share(build, base, first, size, 0.4)
    build.call("stop", {"label": first})
    time.sleep(3)
    second = f"{base.replace('/', '-')}-recv-b"
    build.start(second, "tree-receive", base + "/slot/receive.json")
    build.wait(second, 1800)
    dev.call("stop", {"label": label})
    prog = receiver_progress(build, base)
    fetched = sum(int(r.get("downloaded_payload_bytes", 0)) for r in prog["results"])
    stats = prog["results"][0].get("statistics", {}) if prog["results"] else {}
    reused = stats.get("chunks-in-place", 0) + stats.get("chunks-from-seeds", 0)
    ok = prog["installed"] == "ready" and prog["sha"] == src["sha"] and fetched < size * 0.8
    return {"case": "F1 receiver killed at 40% resumes", "passed": ok, "fetched_after_resume": fetched, "size": size,
            "chunks_reused": reused, "chunks_total": stats.get("chunks-total"),
            "installed": prog["installed"], "exact": prog["sha"] == src["sha"]}


def f2_publisher_lost(dev, build, size):
    base = f"faults/f2-{RUN}"
    src = make_source(dev, base, size)
    label, offer = publish(dev, STATE["hosts"][build.alias]["fingerprint"], base, "a")
    delivery = uuid.uuid4().hex
    receive_config(build, dev, base, offer, delivery)
    first = f"{base.replace('/', '-')}-recv-a"
    build.start(first, "tree-receive", base + "/slot/receive.json")
    wait_share(build, base, first, size, 0.4)
    dev.call("stop", {"label": label})
    end = time.monotonic() + 300
    while build.call("status", {"label": first}).get("state") not in ("failed", "finished") and time.monotonic() < end:
        time.sleep(2)
    lost = build.call("status", {"label": first})
    label2, offer2 = publish(dev, STATE["hosts"][build.alias]["fingerprint"], base, "b")
    same_tree = offer2["artifact_id"] == offer["artifact_id"]
    receive_config(build, dev, base, offer2, delivery)
    second = f"{base.replace('/', '-')}-recv-b"
    build.start(second, "tree-receive", base + "/slot/receive.json")
    build.wait(second, 1800)
    dev.call("stop", {"label": label2})
    prog = receiver_progress(build, base)
    fetched = sum(int(r.get("downloaded_payload_bytes", 0)) for r in prog["results"])
    stats = prog["results"][0].get("statistics", {}) if prog["results"] else {}
    reused = stats.get("chunks-in-place", 0) + stats.get("chunks-from-seeds", 0)
    ok = lost.get("exit_code") not in (0, None) and same_tree and prog["installed"] == "ready" and prog["sha"] == src["sha"] and fetched < size * 0.8
    return {"case": "F2 publisher lost at 40%, new publication resumes", "passed": ok, "first_receiver_exit": lost.get("exit_code"),
            "chunks_reused": reused, "chunks_total": stats.get("chunks-total"),
            "same_artifact": same_tree, "fetched_after_resume": fetched, "exact": prog["sha"] == src["sha"]}


def f3_second_receiver(dev, build, size):
    base = f"faults/f3-{RUN}"
    make_source(dev, base, size)
    label, offer = publish(dev, STATE["hosts"][build.alias]["fingerprint"], base, "a")
    receive_config(build, dev, base, offer, uuid.uuid4().hex)
    first = f"{base.replace('/', '-')}-recv-a"
    build.start(first, "tree-receive", base + "/slot/receive.json")
    wait_bytes(build, base, first, 64 * MIB)
    second = f"{base.replace('/', '-')}-recv-b"
    build.start(second, "tree-receive", base + "/slot/receive.json")
    end = time.monotonic() + 120
    while build.call("status", {"label": second}).get("state") not in ("failed", "finished") and time.monotonic() < end:
        time.sleep(1)
    st2 = build.call("status", {"label": second})
    still = build.call("status", {"label": first}).get("state")
    build.wait(first, 1800)
    dev.call("stop", {"label": label})
    prog = receiver_progress(build, base)
    ok = st2.get("state") == "finished" and st2.get("exit_code") not in (0, None) and still == "running" and prog["installed"] == "ready"
    return {"case": "F3 second receiver on a busy slot is refused", "passed": ok, "second_exit": st2.get("exit_code"),
            "first_kept_running": still == "running", "first_installed": prog["installed"]}


def f5_wrong_peer(dev, build, size):
    base = f"faults/f5-{RUN}"
    make_source(dev, base, 64 * MIB)
    label, offer = publish(dev, STATE["hosts"][build.alias]["fingerprint"], base, "a")
    receive_config(build, dev, base, offer, uuid.uuid4().hex, fingerprint="0" * 64)
    rl = f"{base.replace('/', '-')}-recv-a"
    build.start(rl, "tree-receive", base + "/slot/receive.json")
    end = time.monotonic() + 120
    while build.call("status", {"label": rl}).get("state") not in ("failed", "finished") and time.monotonic() < end:
        time.sleep(1)
    st = build.call("status", {"label": rl})
    dev.call("stop", {"label": label})
    prog = receiver_progress(build, base)
    ok = st.get("exit_code") not in (0, None) and prog["largest"] == 0 and prog["installed"] is None
    return {"case": "F5 wrong peer fingerprint refused, nothing written", "passed": ok, "exit": st.get("exit_code"),
            "bytes_written": prog["largest"], "error": prog["error"]}


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state", type=Path, required=True)
    parser.add_argument("--direction", choices=["forward", "reverse"], default="forward")
    parser.add_argument("--size-mib", type=int, default=1024)
    parser.add_argument("--cases", default="f1,f2,f3,f5")
    args = parser.parse_args()
    STATE = json.loads(args.state.read_text())
    local, peer = q.load_hosts(STATE)
    dev, build = (local, peer) if args.direction == "forward" else (peer, local)
    size = args.size_mib * MIB
    cases = {"f1": f1_receiver_killed, "f2": f2_publisher_lost, "f3": f3_second_receiver, "f5": f5_wrong_peer}
    chosen = [cases[c] for c in args.cases.split(",")]
    with concurrent.futures.ThreadPoolExecutor(max_workers=len(chosen)) as pool:
        futures = {pool.submit(case, dev, build, size): case.__name__ for case in chosen}
        results = []
        for future in concurrent.futures.as_completed(futures):
            try:
                result = future.result()
            except Exception as error:  # report every case, keep the others running
                result = {"case": futures[future], "passed": False, "error": str(error)[-600:]}
            results.append(result)
            print(json.dumps(result), flush=True)
    print(json.dumps({"direction": args.direction, "passed": sum(r["passed"] for r in results), "total": len(results)}), flush=True)
