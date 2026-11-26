"""Private synthetic remote-build qualification between two Windows hosts.

Scenario: the developer host owns generated sources; it sends them to the
build host with tree-serve/tree-receive, the build host runs a deterministic
synthetic build, publishes the packages back to the developer host's executor
with controller-run, and the developer host runs the test scenario. A second
round changes one source file: only its chunks travel, one package changes and
only its chunks come back. Runs in both directions.

Generated data only. It reuses qualify_lan's hosts, fresh root and identities;
private offers and pairing stay in that root.
"""
import argparse
import json
import sys
import time
import uuid
from pathlib import Path, PureWindowsPath

sys.path.insert(0, str(Path(__file__).resolve().parent))
import qualify_lan as q  # noqa: E402

MIB = 1024**2
SOURCE_FILES = 48
SOURCE_BYTES = 4 * MIB
PACKAGES = 4
PACKAGE_BYTES = 768 * MIB
RUN = time.strftime("%H%M%S")


def hosts_for(state, direction):
    local, peer = q.load_hosts(state)
    return (local, peer) if direction == "forward" else (peer, local)


def run_ps(host, script, timeout=1800):
    return host.shell("$root='" + host.root + "'\n" + script, timeout=timeout)


def generate_sources(dev, base, generation):
    """Deterministic text-like sources; generation 2 rewrites exactly one file."""
    return run_ps(dev, rf"""
$dir=Join-Path $root '{base}\sources'
[IO.Directory]::CreateDirectory($dir)|Out-Null
$changed=@()
for($i=0;$i -lt {SOURCE_FILES};$i++){{
  $name=('module{{0:D2}}.src' -f $i)
  $path=Join-Path $dir $name
  $seed=$i; if({generation} -ge 2 -and $i -eq 7){{ $seed=1000+$i }}
  if((Test-Path -LiteralPath $path) -and $seed -eq $i){{ continue }}
  $sb=New-Object Text.StringBuilder
  $line=0
  while($sb.Length -lt {SOURCE_BYTES}){{ [void]$sb.AppendFormat('// unit {{0}} line {{1}} value {{2}}`n',$seed,$line,($seed*7919+$line*104729)%1000003); $line++ }}
  [IO.File]::WriteAllText($path,$sb.ToString().Substring(0,{SOURCE_BYTES}))
  $changed+=$name
}}
@{{files={SOURCE_FILES};changed=$changed}}|ConvertTo-Json -Compress
""")


def tree_send(dev, build, base, round_, previous=None):
    """dev publishes its sources; build receives them. Returns receiver facts."""
    dev_info = STATE["hosts"][dev.alias]
    build_info = STATE["hosts"][build.alias]
    pub_dir = f"{base}/send-{round_}"
    dev.write(pub_dir + "/publish.json", {
        "source_root": dev.path(base + "/sources"), "average": 1 * MIB, "serve_seconds": 1800, "stash_bytes": 0,
        "memory_bytes": 256 * MIB, "delivery_id": "0" * 32,
        "peer": {"identity": dev.path("identity-init/identity"), "fingerprint": build_info["fingerprint"],
                 "listen": dev_info["ip"] + ":0"}})
    label = f"{base.replace('/', '-')}-send-{round_}-{RUN}"
    dev.start(label, "tree-serve", pub_dir + "/publish.json")
    offer = None
    for _ in range(120):
        try:
            offer = dev.call("read", {"path": pub_dir + "/tree-offer.json"})
            break
        except RuntimeError:
            if dev.call("status", {"label": label}).get("state") in ("failed", "finished"):
                raise RuntimeError("source publication ended before offering")
            time.sleep(1)
    if offer is None:
        raise RuntimeError("source publication offered nothing")
    receive = {"offer": offer, "delivery_id": uuid.uuid4().hex, "stash_bytes": 16 * MIB, "memory_bytes": 256 * MIB,
               "peer": {"identity": build.path("identity-init/identity"), "fingerprint": dev_info["fingerprint"]}}
    if previous:
        receive["previous_artifact_id"] = previous
    build.write(base + "/src-slot/receive.json", receive)
    rlabel = f"{base.replace('/', '-')}-recv-{round_}-{RUN}"
    started = time.monotonic()
    build.start(rlabel, "tree-receive", base + "/src-slot/receive.json")
    build.wait(rlabel, 3600)
    seconds = time.monotonic() - started
    dev.call("stop", {"label": label})
    facts = run_ps(build, rf"""
$slot=Join-Path $root '{base}\src-slot'
$installed=Get-Content -LiteralPath (Join-Path $slot 'tree-installed.json') -Raw|ConvertFrom-Json
[long]$down=0
foreach($f in @(Get-ChildItem -LiteralPath (Join-Path $slot 'files') -Filter result.json -Recurse -File)){{ $v=Get-Content -LiteralPath $f.FullName -Raw|ConvertFrom-Json; if(-not $v.ok){{throw 'source reconstruction failed'}}; if($f.LastWriteTime -gt (Get-Date).AddSeconds(-{int(seconds) + 30})){{ $down+=[long]$v.downloaded_payload_bytes }} }}
$files=@(Get-ChildItem -LiteralPath (Join-Path $slot 'build') -File -Recurse)
@{{state=$installed.state;artifact=$installed.artifact_id;files=$files.Count;bytes=($files|Measure-Object Length -Sum).Sum;downloaded=$down}}|ConvertTo-Json -Compress
""")
    if facts["state"] != "ready" or facts["files"] != SOURCE_FILES or facts["bytes"] != SOURCE_FILES * SOURCE_BYTES:
        raise RuntimeError("sources did not arrive exactly: " + json.dumps(facts))
    dev_hashes = run_ps(dev, rf"""
$dir=Join-Path $root '{base}\sources'
$h=@{{}};foreach($f in Get-ChildItem -LiteralPath $dir -File){{ $h[$f.Name]=(Get-FileHash -LiteralPath $f.FullName).Hash }};$h|ConvertTo-Json -Compress
""")
    build_hashes = run_ps(build, rf"""
$dir=Join-Path $root '{base}\src-slot\build'
$h=@{{}};foreach($f in Get-ChildItem -LiteralPath $dir -File){{ $h[$f.Name]=(Get-FileHash -LiteralPath $f.FullName).Hash }};$h|ConvertTo-Json -Compress
""")
    if dev_hashes != build_hashes:
        raise RuntimeError("received sources differ from the developer's")
    facts["seconds"] = round(seconds, 1)
    return facts


def synthetic_build(build, base):
    """Each package derives from a quarter of the sources; one changed source changes one package."""
    return run_ps(build, rf"""
$src=Join-Path $root '{base}\src-slot\build'
$out=Join-Path $root '{base}\out'
[IO.Directory]::CreateDirectory((Join-Path $out 'packs'))|Out-Null
$sources=@(Get-ChildItem -LiteralPath $src -File|Sort-Object Name)
$per=[int]($sources.Count/{PACKAGES})
$sha=[Security.Cryptography.SHA256]::Create()
$stamps=Join-Path $root '{base}\stamps'
[IO.Directory]::CreateDirectory($stamps)|Out-Null
$rebuilt=@()
for($p=0;$p -lt {PACKAGES};$p++){{
  $group=$sources[($p*$per)..(($p+1)*$per-1)]
  $ms=New-Object IO.MemoryStream
  foreach($s in $group){{ $b=$sha.ComputeHash([IO.File]::ReadAllBytes($s.FullName)); $ms.Write($b,0,$b.Length) }}
  $digest=$sha.ComputeHash($ms.ToArray())
  $key=[BitConverter]::ToString($digest).Replace('-','')
  $target=Join-Path $out ('packs\package{{0}}.pak' -f $p)
  $stamp=Join-Path $stamps ('package{{0}}.key' -f $p)
  if((Test-Path -LiteralPath $target) -and (Test-Path -LiteralPath $stamp) -and ((Get-Content -LiteralPath $stamp -Raw).Trim() -eq $key)){{ continue }}
  $rng=New-Object Random ([BitConverter]::ToInt32($digest,0))
  $block=New-Object byte[] (4MB)
  $fs=[IO.File]::Open($target,[IO.FileMode]::Create,[IO.FileAccess]::Write)
  try{{ for($m=0;$m -lt {PACKAGE_BYTES // (4 * MIB)};$m++){{ $rng.NextBytes($block); $fs.Write($block,0,$block.Length) }} }} finally {{ $fs.Close() }}
  [IO.File]::WriteAllText($stamp,$key)
  $rebuilt+=('package{{0}}.pak' -f $p)
}}
$entries=@();foreach($f in Get-ChildItem -LiteralPath (Join-Path $out 'packs') -File){{ $entries+=@{{path='packs/'+$f.Name;sha256=(Get-FileHash -LiteralPath $f.FullName).Hash.ToLowerInvariant();bytes=$f.Length}} }}
@{{rebuilt=$rebuilt;entries=$entries}}|ConvertTo-Json -Compress -Depth 4
""", timeout=3600)


def deliver_back(build, dev, base, round_, entries):
    """build publishes its packages to dev's executor; dev runs the test scenario."""
    dev_info = STATE["hosts"][dev.alias]
    build_info = STATE["hosts"][build.alias]
    service = f"{base.replace('/', '-')}-svc-{RUN}"
    if round_ == 1:
        dev.write(base + "/consumer/synthetic.json", {"nonce": PureWindowsPath(dev.root).name})
        dev.write(base + "/executor.json", {"root": dev.path(base + "/consumer"), "peer": {
            "identity": dev.path("identity-init/identity"), "fingerprint": build_info["fingerprint"],
            "listen": dev_info["ip"] + ":34445"}, "serve_seconds": 14400, "memory_bytes": 1024 * MIB,
            "scenario_memory_bytes": 256 * MIB, "scenario_seconds": 600, "stash_bytes": 16 * MIB})
        dev.start(service, "executor-serve", base + "/executor.json")
    offer = None
    for _ in range(60):
        try:
            offer = dev.call("read", {"path": base + "/executor-offer.json"})
            break
        except RuntimeError:
            time.sleep(1)
    if offer is None:
        raise RuntimeError("developer executor offered nothing")
    total = sum(e["bytes"] for e in entries)
    script = f"""$ErrorActionPreference='Stop'
$files=[IO.Directory]::GetFiles($env:ZERUS_ARTIFACT_ROOT,'*.pak',[IO.SearchOption]::AllDirectories)
[long]$total=0
foreach($file in $files){{$total+=(New-Object IO.FileInfo($file)).Length}}
if($files.Count -ne {PACKAGES} -or $total -ne {total}){{exit 3}}
[IO.File]::AppendAllText((Join-Path $env:ZERUS_RESULT_DIR 'executions.txt'),'executed-once')
[IO.File]::WriteAllText((Join-Path $env:ZERUS_RESULT_DIR 'inventory.json'),'{{"files":{PACKAGES},"bytes":{total}}}')
exit 0
"""
    build.write(base + "/scenario/scenario.json", {"protocol": 1, "instructions": "Synthetic remote build: verify the packages built on the build host.",
                                                     "runtime": "powershell", "entry": "run.ps1", "arguments": [], "timeout_seconds": 120,
                                                     "memory_bytes": 128 * MIB, "guided": False})
    build.write(base + "/scenario/run.ps1", script, text=True)
    job = uuid.uuid4().hex
    config = f"{base}/controller-{round_}/config.json"
    build.write(config, {"endpoint": offer["endpoint"], "peer": {"identity": build.path("identity-init/identity"),
                         "fingerprint": dev_info["fingerprint"]}, "job_id": job, "slot": "rbuild", "device_id": "host",
                         "artifact_root": build.path(base + "/out"), "scenario_root": build.path(base + "/scenario"),
                         "listen": build_info["ip"] + ":0", "average": 4 * MIB, "memory_bytes": 512 * MIB, "timeout_seconds": 3600,
                         "parameters": {"case": "synthetic-remote-build", "round": str(round_)}})
    label = f"{base.replace('/', '-')}-ctl-{round_}-{RUN}"
    started = time.monotonic()
    build.start(label, "controller-run", config)
    build.wait(label, 3600)
    seconds = time.monotonic() - started
    receiver = dev.call("inspect", {"slot": base + "/consumer/slots/rbuild"})
    wanted = {e["path"]: e["sha256"] for e in entries}
    actual = {e["path"]: e["sha256"] for e in receiver["files"]}
    if receiver["state"] != "ready" or wanted != actual:
        raise RuntimeError("packages did not arrive exactly on the developer host")
    result = build.call("read", {"path": str(PureWindowsPath(config).parent / "controller-result.json")})["result"]
    if result["status"] != "passed":
        raise RuntimeError("developer-host scenario did not pass: " + json.dumps(result)[:400])
    return {"seconds": round(seconds, 1), "downloaded_payload_bytes": receiver["downloaded_payload_bytes"],
            "total_bytes": receiver["total_bytes"], "scenario": result["status"]}


def run(direction):
    dev, build = hosts_for(STATE, direction)
    base = "rbuild/" + direction
    report = {"direction": direction, "developer": dev.alias, "build_host": build.alias}
    report["sources_1"] = generate_sources(dev, base, 1)
    report["send_1"] = tree_send(dev, build, base, 1)
    print(json.dumps({"step": "sources sent", **report["send_1"]}), flush=True)
    built = synthetic_build(build, base)
    report["build_1"] = built["rebuilt"]
    report["back_1"] = deliver_back(build, dev, base, 1, built["entries"])
    print(json.dumps({"step": "packages delivered and tested", **report["back_1"]}), flush=True)
    report["sources_2"] = generate_sources(dev, base, 2)
    report["send_2"] = tree_send(dev, build, base, 2, previous=report["send_1"]["artifact"])
    if report["send_2"]["downloaded"] > 2 * SOURCE_BYTES:
        raise RuntimeError("source delta moved too much: " + json.dumps(report["send_2"]))
    built = synthetic_build(build, base)
    report["build_2"] = built["rebuilt"]
    if len(built["rebuilt"]) != 1:
        raise RuntimeError("one changed source must rebuild exactly one package: " + json.dumps(built["rebuilt"]))
    report["back_2"] = deliver_back(build, dev, base, 2, built["entries"])
    # The receiver reports the payload fetched for the files of its current tree.
    if report["back_2"]["downloaded_payload_bytes"] > PACKAGE_BYTES + 64 * MIB:
        raise RuntimeError("package delta moved too much: " + json.dumps(report["back_2"]))
    print(json.dumps({"direction": direction, "remote_build": "passed", **{k: report[k] for k in ("send_1", "back_1", "send_2", "build_2", "back_2")}}), flush=True)
    STATE.setdefault("rbuild", {})[direction] = report
    q.save_state(STATE_PATH, STATE)


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state", type=Path, required=True)
    parser.add_argument("--direction", choices=["forward", "reverse"], default="forward")
    args = parser.parse_args()
    STATE_PATH = args.state
    STATE = json.loads(args.state.read_text())
    run(args.direction)
