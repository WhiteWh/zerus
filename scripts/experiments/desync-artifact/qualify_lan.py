"""Private, synthetic Windows LAN qualification; never accepts existing build paths.

SSH is reached only through hgs.exe's overlapped byte transport. PowerShell is
the only fixture runtime required on the consumer; no Python/model is installed
there. Runtime roots, pairing keys, offers and reports stay outside the checkout.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path, PureWindowsPath
import subprocess
import socket
import sys
import time
import uuid

GIB = 1024**3
MIB = 1024**2
HERE = Path(__file__).resolve().parent
PS = str(Path(os.environ.get("SystemRoot", "C:/Windows")) / "System32/WindowsPowerShell/v1.0/powershell.exe")
# Read the inherited Windows SSH handle in its actual async mode.
NATIVE_INPUT = r"""
Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class ZerusQualificationInput {
 [DllImport("kernel32.dll")] static extern IntPtr GetStdHandle(int n);
 [DllImport("ntdll.dll")] static extern int NtQueryInformationFile(IntPtr h,[Out] IntPtr[] status,out uint mode,uint size,int info);
 static int Read(FileStream s,byte[] b,int at,int count) {
  var task=s.ReadAsync(b,at,count);if(!task.Wait(15000))throw new IOException("Qualification input lease expired");return task.GetAwaiter().GetResult();
 }
 static void Exact(FileStream s,byte[] b,int count) {
  int at=0;while(at<count){int n=Read(s,b,at,count-at);if(n==0)throw new IOException("Truncated qualification input");at+=n;}
 }
 public static byte[] Receive(bool framed) {
  var handle=GetStdHandle(-10);uint mode;
  if(NtQueryInformationFile(handle,new IntPtr[2],out mode,4,16)!=0)throw new IOException("Input mode unavailable");
  using(var s=new FileStream(new SafeFileHandle(handle,false),FileAccess.Read,1,(mode&48)==0))
  using(var memory=new MemoryStream()){
   byte[] b=new byte[16384];
   while(true){
    int count;
    if(framed){Exact(s,b,4);uint size=BitConverter.ToUInt32(b,0);if(size==uint.MaxValue)continue;if(size==0)break;if(size>b.Length)throw new IOException("Oversized frame");count=(int)size;Exact(s,b,count);}
    else{count=Read(s,b,0,b.Length);if(count==0)break;}
    if(memory.Length+count>134217728)throw new IOException("Oversized input");memory.Write(b,0,count);
   }
   return memory.ToArray();
  }
 }
}
'@ | Out-Null
"""


def b64(data):
    return base64.b64encode(data).decode("ascii")


class Host:
    def __init__(self, alias, root, profile=None):
        self.alias, self.root, self.profile = alias, root, profile

    def path(self, relative):
        part = PureWindowsPath(relative)
        if part.is_absolute() or ".." in part.parts:
            raise ValueError("qualification paths must stay below their fresh root")
        return str(PureWindowsPath(self.root) / part)

    def shell(self, script, payload=b"", timeout=60):
        script = "$ErrorActionPreference='Stop';$ProgressPreference='SilentlyContinue';$env:PSModulePath=Join-Path $PSHOME 'Modules';[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false);\n" + script
        encoded = b64(script.encode("utf-16-le"))
        if self.profile:
            profile = self.profile
            argv = ["hgs", "--ssh-bytes-client", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5",
                    "-o", "HostName="+profile["hostname"], "-l", profile["user"], "-p", str(profile["port"])]
            # Resolve a configured identity only if it was explicitly set.
            if profile.get("identity_file"):
                argv += ["-i", os.path.expanduser(profile["identity_file"])]
            remote="powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command \"& ([ScriptBlock]::Create([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('"+b64(script.encode())+"'))))\""
            if len(remote)>8000:raise ValueError("qualification command exceeds remote shell limit")
            argv += [self.alias, "__hgs_ssh_bytes_v1__ " + remote]
        else:
            argv = [PS, "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", encoded]
        try:
            result = subprocess.run(argv, input=payload, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    timeout=timeout, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        except subprocess.TimeoutExpired:
            raise TimeoutError(f"{self.alias}: qualification RPC timed out; inspect the saved job before retry") from None
        if result.returncode:
            # Scripts operate only within generated roots. Do not print input,
            # command lines, offers or arbitrary environment/configuration.
            detail = result.stderr.decode("utf-8", errors="replace")[-1600:]
            raise RuntimeError(f"{self.alias}: qualification operation failed ({result.returncode}): {detail}")
        output = result.stdout.decode("utf-8-sig").strip()
        return json.loads(output) if output else None

    def input_script(self):
        return NATIVE_INPUT + "$raw=[ZerusQualificationInput]::Receive($" + ("true" if self.profile else "false") + ")\n"

    def bootstrap(self):
        nonce = PureWindowsPath(self.root).name
        uuid.UUID(nonce)
        script = rf"""
$root='{self.root}';$parent=Split-Path -Parent $root
if ($root -notmatch '^D:\\ZerusArtifactQualification\\[a-f0-9-]{{36}}$') {{ throw 'Invalid synthetic root' }}
if (Test-Path -LiteralPath $root) {{ throw 'Fresh synthetic root already exists' }}
if ((Test-Path -LiteralPath $parent) -and ((Get-Item -LiteralPath $parent).Attributes -band [IO.FileAttributes]::ReparsePoint)) {{ throw 'Reparse root parent' }}
[IO.Directory]::CreateDirectory($root)|Out-Null
[IO.File]::WriteAllText((Join-Path $root 'synthetic-root.txt'),'{nonce}')
'{{"ok":true}}'
"""
        self.shell(script)
        self.upload(HERE / "qualify_lan_host.ps1", "qualify_lan_host.ps1")

    def upload(self, source, relative):
        source = Path(source)
        if source.stat().st_size > 128*MIB or source.suffix.lower() == ".key":
            raise ValueError("upload is limited to scripts/public tool packages")
        target = self.path(relative)
        expected = hashlib.sha256(source.read_bytes()).hexdigest()
        script = self.input_script() + f"""
$root='{self.root}';$target='{target}'
if (([IO.File]::ReadAllText((Join-Path $root 'synthetic-root.txt'))).Trim() -ne '{PureWindowsPath(self.root).name}') {{ throw 'Root marker changed' }}
if (-not ([IO.Path]::GetFullPath($target)).StartsWith($root+'\',[StringComparison]::OrdinalIgnoreCase)) {{ throw 'Upload escapes root' }}
if (Test-Path -LiteralPath $target) {{ throw 'Upload output must be new' }}
[IO.Directory]::CreateDirectory((Split-Path -Parent $target))|Out-Null
[IO.File]::WriteAllBytes($target,$raw)
if ((Get-FileHash -LiteralPath $target).Hash.ToLowerInvariant() -ne '{expected}') {{ throw 'Upload hash differs' }}
'{{"ok":true}}'
"""
        return self.shell(script, source.read_bytes(), timeout=180)

    def call(self, action, value=None, timeout=60):
        if not action.replace("-", "").isalnum():
            raise ValueError("invalid action")
        script = self.input_script() + f"""
$text=[Text.Encoding]::UTF8.GetString($raw)
& '{self.path('qualify_lan_host.ps1')}' -Root '{self.root}' -Action '{action}' -InputJson $text
"""
        try:
            return self.shell(script, json.dumps(value or {}).encode(), timeout)
        except RuntimeError as error:
            # A job just started may not have written its first status yet.
            if action == "status" and "status.json" in str(error):
                return {"state": "starting"}
            raise

    def write(self, path, value, text=False):
        return self.call("write", {"path":path,"value":value,"kind":"text" if text else "json"})

    def start(self, label, command=None, config=None, kind="command", **extra):
        return self.call("start", dict(label=label, command=command, config=config, kind=kind,
                                       exe="package/hgs-executor-windows-x86_64/hgs-executor.exe", **extra))

    def wait(self, label, seconds=7200):
        end = time.monotonic()+seconds
        last = None
        while time.monotonic()<end:
            status = self.call("status", {"label":label})
            if status.get("state") in ("finished", "failed"):
                if status.get("exit_code") != 0:
                    raise RuntimeError(f"{self.alias}/{label}: failed; inspect its private job report")
                return status
            progress = (int(status.get("seconds",0))//30, status.get("completed_files"))
            if progress != last:
                print(json.dumps({"host":self.alias,"job":label,"state":status.get("state"),
                                  "completed_files":status.get("completed_files"),"elapsed_seconds":round(status.get("seconds",0))}),flush=True)
                last = progress
            time.sleep(5)
        raise TimeoutError(f"{self.alias}/{label}: deadline; inspect private exact child receipt before stopping")

    def install(self, package):
        self.upload(package,"candidate.zip")
        script=rf"""
Add-Type -AssemblyName System.IO.Compression.FileSystem
$root='{self.root}';$destination=Join-Path $root 'package'
if (Test-Path -LiteralPath $destination) {{ throw 'Package destination exists' }}
$archive=[IO.Compression.ZipFile]::OpenRead((Join-Path $root 'candidate.zip'))
try {{
  [long]$total=0
  foreach ($entry in $archive.Entries) {{
    $path=[IO.Path]::GetFullPath((Join-Path $destination $entry.FullName))
    if (-not $path.StartsWith($destination+'\',[StringComparison]::OrdinalIgnoreCase) -or $entry.FullName -match '\.key$') {{ throw 'Unsafe candidate entry' }}
    $total += $entry.Length
  }}
  if ($archive.Entries.Count -gt 1000 -or $total -gt 134217728) {{ throw 'Unexpected consumer package' }}
}} finally {{ $archive.Dispose() }}
[IO.Compression.ZipFile]::ExtractToDirectory((Join-Path $root 'candidate.zip'),$destination)
$exe=Join-Path $destination 'hgs-executor-windows-x86_64\hgs-executor.exe'
$manifest=Get-Content -LiteralPath (Join-Path $destination 'hgs-executor-windows-x86_64\licenses\build.json') -Raw | ConvertFrom-Json
if ((Get-FileHash -LiteralPath $exe).Hash.ToLowerInvariant() -ne $manifest.binary_sha256) {{ throw 'Candidate binary differs' }}
& $exe --version
"""
        version=self.shell(script,timeout=120)
        if version.get("model_runtime") is not False:raise RuntimeError("candidate must be model-free")
        self.write("identity-init/init.json",{})
        self.start("identity-init","identity-init","identity-init/init.json")
        self.wait("identity-init",60)
        public=self.call("read",{"path":"identity-init/identity/public.json"})
        return {"version":version,"fingerprint":public["fingerprint"]}


def save_state(path,state):
    temporary=path.with_suffix('.next')
    temporary.write_text(json.dumps(state,indent=2))
    os.replace(temporary,path)


def configure_direction(state,builder,consumer,name,source):
    builder_info=state["hosts"][builder.alias];consumer_info=state["hosts"][consumer.alias]
    consumer.write("state/synthetic.json",{"nonce":PureWindowsPath(consumer.root).name})
    config={"root":consumer.path("state"),"peer":{"identity":consumer.path("identity-init/identity"),
            "fingerprint":builder_info["fingerprint"],"listen":consumer_info["ip"]+":34443"},
            "serve_seconds":14400,"memory_bytes":1024*MIB,"scenario_memory_bytes":256*MIB,
            "scenario_seconds":600,"stash_bytes":16*MIB}
    consumer.write("executor.json",config)
    consumer.start(name+"-service","executor-serve","executor.json")
    # Never print the offer or pairing configuration; they stay in private state.
    deadline=time.monotonic()+45
    while True:
        try:
            offer=consumer.call("read",{"path":"executor-offer.json"})
            break
        except RuntimeError:
            status=consumer.call("status",{"label":name+"-service"})
            if status.get("state") in ("failed","finished") or time.monotonic()>deadline:raise
            time.sleep(1)
    spec={"protocol":1,"instructions":"Synthetic qualification: verify the four opaque files and collect one execution marker.",
          "runtime":"powershell","entry":"run.ps1","arguments":[],"timeout_seconds":120,"memory_bytes":128*MIB,"guided":False}
    script="""$ErrorActionPreference='Stop'
$files=[IO.Directory]::GetFiles($env:ZERUS_ARTIFACT_ROOT,'*',[IO.SearchOption]::AllDirectories)
[long]$total=0
foreach($file in $files){$total+=(New-Object IO.FileInfo($file)).Length}
if($files.Count -ne 4 -or $total -ne 21474836480){exit 3}
[IO.File]::AppendAllText((Join-Path $env:ZERUS_RESULT_DIR 'executions.txt'),'executed-once')
[IO.File]::WriteAllText((Join-Path $env:ZERUS_RESULT_DIR 'inventory.json'),'{"files":4,"bytes":21474836480}')
exit 0
"""
    builder.write("scenario/scenario.json",spec);builder.write("scenario/run.ps1",script,text=True)
    job=uuid.uuid4().hex
    controller={"endpoint":offer["endpoint"],"peer":{"identity":builder.path("identity-init/identity"),"fingerprint":consumer_info["fingerprint"]},
                "job_id":job,"slot":"synthetic-20g","device_id":"host","artifact_root":builder.path(source),
                "scenario_root":builder.path("scenario"),"listen":builder_info["ip"]+":0","average":4*MIB,
                "memory_bytes":512*MIB,"timeout_seconds":3600,"parameters":{"case":"synthetic-20g","direction":name}}
    relative="controller/"+name+"/config.json"
    builder.write(relative,controller)
    return {"job":job,"config":relative,"builder":builder.alias,"consumer":consumer.alias,"source":source}


def verify_delivery(state,builder,consumer,name,label,config,performance=False):
    process=builder.wait(label)
    receiver=consumer.call("inspect",{"slot":"state/slots/synthetic-20g"})
    expected=builder.call("read",{"path":"fixture.json"})
    wanted={entry["path"]:entry["sha256"] for entry in expected["entries"]}
    actual={entry["path"]:entry["sha256"] for entry in receiver["files"]}
    if receiver["state"]!="ready" or receiver["total_bytes"]!=20*GIB or wanted!=actual or receiver["large_outside"]:
        raise RuntimeError("physical delivery failed exact-tree/single-copy checks")
    result_path=str(PureWindowsPath(config).parent/"controller-result.json")
    result=builder.call("read",{"path":result_path})["result"]
    receipt=builder.call("read",{"path":str(PureWindowsPath(config).parent/"controller-state.json")})
    if result["status"]!="passed" or not receipt.get("collected"):
        raise RuntimeError("scenario result was not passed and collected")
    if performance:
        report=result.get("performance",{})
        if report.get("origin")!="executor-host" or not report.get("valid") or report.get("during_samples",0)<1:
            raise RuntimeError("remote native performance evidence is missing")
    return {"process":process,"receiver":receiver,"result":result}


def direction_hosts(state,name):
    local,peer=load_hosts(state)
    return (local,peer) if name=="forward" else (peer,local)


def run_delta(state_path,state,name):
    builder,consumer=direction_hosts(state,name);direction=state[name]
    if not direction.get("initial"):raise ValueError("initial generation must be verified first")
    if direction.get("delta_config"):raise ValueError("delta already prepared; inspect saved job rather than creating another execution")
    before=direction["initial"]["receiver"]
    mutation=builder.call("mutate",{"source":direction["source"]},timeout=600)
    spec=builder.call("read",{"path":"scenario/scenario.json"});spec["guided"]=True
    spec["instructions"]="Synthetic instruction: inspect the generated inventory evidence, then submit an explicitly automated synthetic observation."
    builder.write("scenario/scenario.json",spec)
    script="""$ErrorActionPreference='Stop'
$files=[IO.Directory]::GetFiles($env:ZERUS_ARTIFACT_ROOT,'*',[IO.SearchOption]::AllDirectories)
[long]$total=0
foreach($file in $files){$total+=(New-Object IO.FileInfo($file)).Length}
if($files.Count -ne 4 -or $total -ne 21474836480){exit 3}
[IO.File]::AppendAllText((Join-Path $env:ZERUS_RESULT_DIR 'executions.txt'),'executed-once')
[IO.File]::WriteAllText((Join-Path $env:ZERUS_RESULT_DIR 'inventory.json'),'{"files":4,"bytes":21474836480}')
[Threading.Thread]::Sleep(3000)
while(-not [IO.File]::Exists($env:ZERUS_OBSERVATION_FILE)){[Threading.Thread]::Sleep(50)}
exit 0
"""
    builder.write("scenario/run.ps1",script,text=True)
    cfg=builder.call("read",{"path":direction["config"]});cfg["job_id"]=uuid.uuid4().hex
    cfg["parameters"]["case"]="synthetic-delta-guided"
    cfg["performance"]={"cpu_percent":100,"disk_percent":100,"disk_busy_percent":100,"wait_seconds":0,"on_busy":"not_run"}
    config="controller/"+name+"-delta/config.json"
    builder.write(config,cfg)
    direction.update(delta_config=config,delta_job=cfg["job_id"],mutation=mutation)
    save_state(state_path,state)
    builder.start(name+"-delta","controller-run",config)
    deadline=time.monotonic()+3600
    observed=False
    while time.monotonic()<deadline:
        status=builder.call("status",{"label":name+"-delta"})
        if status.get("state") in ("finished","failed"):break
        try:
            job=consumer.call("read",{"path":"state/jobs/"+cfg["job_id"]+"/job-state.json"})
        except RuntimeError:
            time.sleep(5);continue
        if job.get("status")=="awaiting_observation":
            evidence=consumer.call("read",{"path":"state/jobs/"+cfg["job_id"]+"/outputs/inventory.json"})
            if evidence!={"files":4,"bytes":20*GIB}:raise RuntimeError("synthetic observation evidence differs")
            cfg["observation"]={"request_id":uuid.uuid4().hex,"job_id":cfg["job_id"],"request_sha256":job["request_sha256"],
                                "verdict":"passed","notes":"Automated synthetic observation of generated inventory; no physical human/device claim."}
            builder.write(config,cfg)
            builder.start(name+"-observe","controller-observe",config)
            builder.wait(name+"-observe",60);observed=True
            break
        time.sleep(5)
    if not observed:raise RuntimeError("guided synthetic observation was not requested")
    value=verify_delivery(state,builder,consumer,name,name+"-delta",config,performance=True)
    if value["receiver"]["downloaded_payload_bytes"]>=512*MIB:raise RuntimeError("delta fetched too much of the 20 GiB fixture")
    old_renamed=next(item["identity"] for item in before["files"] if item["path"].startswith("packs/") and item["path"] not in ("packs/alpha.pak","packs/beta.ucas"))
    new_renamed=next(item["identity"] for item in value["receiver"]["files"] if item["path"].startswith("packs/") and item["path"] not in ("packs/alpha.pak","packs/beta.ucas"))
    if old_renamed!=new_renamed:raise RuntimeError("rename replaced its native receiver instead of moving it")
    prior=consumer.call("read",{"path":"state/jobs/"+cfg["job_id"]+"/job-state.json"})
    builder.start(name+"-reconnect","controller-run",config);builder.wait(name+"-reconnect",120)
    after=consumer.call("read",{"path":"state/jobs/"+cfg["job_id"]+"/job-state.json"})
    if prior!=after:raise RuntimeError("reconnect changed the completed scenario state")
    direction["delta"]=value;direction["no_replay"]=True;save_state(state_path,state)
    print(json.dumps({"direction":name,"delta":"passed","fixture_bytes":20*GIB,"changed_bytes":64*MIB,
                      "downloaded_payload_bytes":value["receiver"]["downloaded_payload_bytes"],"rename_native_identity_kept":True,
                      "guided_observation":"synthetic","remote_performance_valid":True,"no_replay":True}),flush=True)


def load_hosts(state):
    profile = json.loads(subprocess.check_output(["hgs","machine","resolve",state["peer"]]))["effective"]
    config = json.loads(subprocess.check_output(["hgs","machine","ls"]))
    configured = next(row for row in config["machines"] if row["alias"]==state["peer"])
    profile["identity_file"] = configured.get("connection",{}).get("identity_file","")
    return Host("msi",state["root"]), Host(state["peer"],state["root"],profile)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation",choices=["prepare","status","install","forward","forward-wait","delta","reverse","reverse-wait"])
    parser.add_argument("--state",type=Path,required=True,help="private local state file outside the repository")
    parser.add_argument("--peer",required=True,help="the configured SSH alias of the second private host")
    parser.add_argument("--package",type=Path)
    parser.add_argument("--direction",choices=["forward","reverse"],default="forward")
    args=parser.parse_args()
    if args.operation=="prepare":
        if args.state.exists():raise ValueError("qualification state must be new")
        state={"protocol":1,"peer":args.peer,"root":str(PureWindowsPath('D:/ZerusArtifactQualification')/str(uuid.uuid4())),"bytes":20*GIB}
        args.state.parent.mkdir(parents=True,exist_ok=True)
        args.state.write_text(json.dumps(state,indent=2))
        for host in load_hosts(state):
            host.bootstrap()
            info=host.call("preflight")
            if info["free_bytes"]<25*GIB:raise RuntimeError(f"{host.alias}: insufficient space for synthetic fixture")
            print(json.dumps({"host":host.alias,"preflight":"passed","enough_disk":True}),flush=True)
        local,_=load_hosts(state)
        local.start("generate",kind="generate",bytes=state["bytes"])
        print("Synthetic generation started; state and reports remain private.",flush=True)
    elif args.operation=="status":
        state=json.loads(args.state.read_text())
        local,_=load_hosts(state)
        print(json.dumps(local.call("status",{"label":state.get("generation_label","generate")})))
    elif args.operation=="install":
        state=json.loads(args.state.read_text());hosts=load_hosts(state)
        if not args.package:raise ValueError("--package required")
        peer_ip=hosts[1].profile["hostname"]
        with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as probe:
            probe.connect((peer_ip,22));local_ip=probe.getsockname()[0]
        state.setdefault("hosts",{})
        for host,ip in zip(hosts,(local_ip,peer_ip)):
            state["hosts"][host.alias]=dict(host.install(args.package),ip=ip)
            save_state(args.state,state)
            print(json.dumps({"host":host.alias,"candidate":"installed","model_runtime":False}),flush=True)
    elif args.operation=="forward":
        state=json.loads(args.state.read_text());builder,consumer=load_hosts(state)
        builder.wait(state.get("generation_label","generate"),60)
        direction=configure_direction(state,builder,consumer,"forward","source")
        state["forward"]=direction;save_state(args.state,state)
        builder.start("forward-initial","controller-run",direction["config"])
        print("Forward synthetic initial delivery started.",flush=True)
    elif args.operation=="forward-wait":
        state=json.loads(args.state.read_text());builder,consumer=load_hosts(state)
        value=verify_delivery(state,builder,consumer,"forward",state["forward"].get("initial_label","forward-initial"),state["forward"]["config"])
        state["forward"]["initial"]=value;save_state(args.state,state)
        print(json.dumps({"direction":"forward","initial":"passed","bytes":value["receiver"]["total_bytes"],"downloaded_payload_bytes":value["receiver"]["downloaded_payload_bytes"]}),flush=True)
    elif args.operation=="delta":
        state=json.loads(args.state.read_text());run_delta(args.state,state,args.direction)
    elif args.operation=="reverse":
        state=json.loads(args.state.read_text());local,peer=load_hosts(state)
        if not state.get("forward",{}).get("delta"):raise ValueError("forward initial and delta must finish before reversing")
        peer.call("stop",{"label":"forward-service"})
        manifest=local.call("read",{"path":"fixture.json"});manifest["source"]="state/slots/synthetic-20g/build"
        peer.write("fixture.json",manifest)
        local.call("retire-source",timeout=600)
        state["reverse"]=configure_direction(state,peer,local,"reverse","state/slots/synthetic-20g/build")
        save_state(args.state,state)
        peer.start("reverse-initial","controller-run",state["reverse"]["config"])
        print("Reversed roles using the existing consumer tree as publisher; the original generated source was retired before receiving.",flush=True)
    elif args.operation=="reverse-wait":
        state=json.loads(args.state.read_text());builder,consumer=direction_hosts(state,"reverse")
        value=verify_delivery(state,builder,consumer,"reverse","reverse-initial",state["reverse"]["config"])
        state["reverse"]["initial"]=value;save_state(args.state,state)
        print(json.dumps({"direction":"reverse","initial":"passed","bytes":value["receiver"]["total_bytes"],"downloaded_payload_bytes":value["receiver"]["downloaded_payload_bytes"]}),flush=True)


if __name__=="__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    main()
