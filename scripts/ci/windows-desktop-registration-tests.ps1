# Policy and owned trace-file controls: no real Task Scheduler, registry, service or process.
param([string]$RegistrationSource=(Join-Path $PSScriptRoot 'fixtures/windows-desktop-registration-legacy.ps1'))
$ErrorActionPreference='Stop'
. ([ScriptBlock]::Create([IO.File]::ReadAllText($RegistrationSource)))
$script:sid='S-1-5-21-1000'
$script:configuration='a'*64
$script:count=0
class FixtureItems {
    [object[]]$Values
    [int]$Count
    [string]$Context='DesktopUser'
    FixtureItems([object[]]$values){$this.Values=$values;$this.Count=$values.Count}
    [object]Item([int]$index){return $this.Values[$index-1]}
}
class FixtureTask {
    [object]$Definition
    [bool]$Enabled=$true
    [int]$State=3
    [string]$Security
    [string]GetSecurityDescriptor([int]$flags){if($flags -ne 5){throw 'Unexpected security query'};return $this.Security}
}
class FixtureFolder {
    [object]$Task=$null
    [object]$Legacy=$null
    [bool]$DenyRead=$false
    [bool]$RaceCreate=$false
    [int]$Creates=0
    [int]$Deletes=0
    [int]$Flags=0
    [object]GetTask([string]$name){
        if($this.DenyRead){throw [Runtime.InteropServices.COMException]::new('Denied',-2147024891)}
        if($name -eq 'hgs-desktop' -and $null -ne $this.Legacy){return $this.Legacy}
        if($name -eq ('hgs-desktop-'+$script:sid) -and $null -ne $this.Task){return $this.Task}
        throw [Runtime.InteropServices.COMException]::new('Absent',-2147024894)
    }
    [object]RegisterTask([string]$name,[string]$xml,[int]$flags,[string]$user,[object]$password,[int]$logon,[string]$security){
        if($this.RaceCreate){throw 'A foreign task appeared; create-only refused'}
        if($name -cne ('hgs-desktop-'+$script:sid) -or $user -cne $script:sid -or $null -ne $password -or $logon -ne 3){throw 'Unexpected registration identity'}
        $this.Flags=$flags;$this.Creates++;$this.Task=New-OwnedTask
        return $this.Task
    }
    [void]DeleteTask([string]$name,[int]$flags){
        if($name -cne ('hgs-desktop-'+$script:sid) -or $flags -ne 0){throw 'Unexpected removal'}
        $this.Deletes++;$this.Task=$null
    }
}
class FixtureScheduler {
    [FixtureFolder]$Folder
    FixtureScheduler(){$this.Folder=[FixtureFolder]::new()}
    [object]GetFolder([string]$path){if($path -cne '\'){throw 'Unexpected task folder'};return $this.Folder}
}
function New-Plan {
    return [pscustomobject]@{schema=1;operation='preview';configuration=$configuration;label='hgs-desktop';user_sid=$sid;task_name=('hgs-desktop-'+$sid)
        argv=@('C:\Example\hgs.exe','desktop','serve','--config-dir','C:\Example\config','--state-dir','C:\Example\state')
        definition='<Task xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task"><Actions><Exec><Arguments>desktop serve --config-dir C:\Example\config --state-dir C:\Example\state</Arguments></Exec></Actions></Task>'
        session=[pscustomobject]@{kind='interactive_user';allow_session_zero=$false}}
}
function New-OwnedTask {
    $task=[FixtureTask]::new()
    $task.Security='O:'+ $sid +'D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;'+$sid+')'
    $task.Definition=[pscustomobject]@{
        RegistrationInfo=[pscustomobject]@{Description=('Zerus desktop registration v1; configuration='+$configuration);URI=('\hgs-desktop-'+$sid)}
        Principal=[pscustomobject]@{Id='DesktopUser';UserId=$sid;GroupId='';LogonType=3;RunLevel=0}
        Actions=[FixtureItems]::new(@([pscustomobject]@{Type=0;Path='C:\Example\hgs.exe';Arguments='desktop serve --config-dir C:\Example\config --state-dir C:\Example\state';WorkingDirectory=''}))
        Triggers=[FixtureItems]::new(@([pscustomobject]@{Type=9;UserId=$sid;Enabled=$true;StartBoundary='';EndBoundary='';Delay='';Repetition=[pscustomobject]@{Interval='';Duration=''}}))
        Settings=[pscustomobject]@{Enabled=$true;Hidden=$true;MultipleInstances=2;ExecutionTimeLimit='PT0S';RestartInterval='PT1M';RestartCount=3;DisallowStartIfOnBatteries=$false;StopIfGoingOnBatteries=$false;StartWhenAvailable=$true;RunOnlyIfIdle=$false;RunOnlyIfNetworkAvailable=$false;WakeToRun=$false}
    }
    return $task
}
function Call-Policy($scheduler,[string]$operation,$plan=(New-Plan)) {
    return Invoke-RegistrationOperation ([pscustomobject]@{schema=1;operation=$operation;plan=$plan}) $scheduler $sid
}
function Check([string]$name,[scriptblock]$body){& $body;$script:count++;Write-Output ('PASS '+$name)}
function Assert([bool]$ok){if(-not $ok){throw 'Control assertion failed'}}
function Refuses([scriptblock]$body){$failed=$false;try{& $body|Out-Null}catch{$failed=$true};Assert $failed}

Check 'trace survives a retained read/write file and logging failure is nonfatal' {
    $p=[IO.Path]::GetTempFileName();$held=$null;$previous=$env:HGS_DESKTOP_REGISTRATION_TRACE_FILE
    try {
        $held=[IO.File]::Open($p,[IO.FileMode]::Open,[IO.FileAccess]::ReadWrite,([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete))
        $env:HGS_DESKTOP_REGISTRATION_TRACE_FILE=$p;$script:rc=[Diagnostics.Stopwatch]::StartNew()
        Trace-Registration main
        $bytes=[byte[]]::new(64);$n=$held.Read($bytes,0,$bytes.Length)
        Assert ([Text.Encoding]::UTF8.GetString($bytes,0,$n) -cmatch '^main [0-9]+\n$')
        $env:HGS_DESKTOP_REGISTRATION_TRACE_FILE=Join-Path $p 'invalid'
        Trace-Registration input
    } finally {
        $env:HGS_DESKTOP_REGISTRATION_TRACE_FILE=$previous
        if($null -ne $held){$held.Dispose()}
        Remove-Item -LiteralPath $p
    }
}

function Parse-RequestBytes([byte[]]$data) {
    $stream=[IO.MemoryStream]::new($data,$false)
    try { return Read-RegistrationRequest $stream $data.Length } finally { $stream.Dispose() }
}
Add-Type -TypeDefinition @'
using System; using System.IO;
public sealed class NoEofStream : MemoryStream {
 public NoEofStream(byte[] bytes):base(bytes,false){}
 public override int Read(byte[] b,int o,int n) {
  if(Position==Length)throw new IOException("Reader waited for EOF");
  return base.Read(b,o,Math.Min(n,3));
 }
}
'@
Check 'exact frame completes without EOF even across short reads' {
$data=[Text.Encoding]::UTF8.GetBytes('{"ok":true}')
$stream=[NoEofStream]::new($data)
try { Assert ((Read-RegistrationRequest $stream $data.Length).ok) } finally { $stream.Dispose() }
}
Check 'short extra and invalid declared lengths refuse' {
foreach($length in @(-1,0,65537,1,20)) {
    $stream=[IO.MemoryStream]::new([Text.Encoding]::UTF8.GetBytes('{"ok":true}'),$false)
    try { Refuses {Read-RegistrationRequest $stream $length} } finally {$stream.Dispose()}
}
}
Check 'UTF-8 input retains multibyte text' {
    $json='{"text":"'+[char]0x20ac+'"}'
    Assert ((Parse-RequestBytes ([Text.Encoding]::UTF8.GetBytes($json))).text -ceq [string][char]0x20ac)
}
Check 'exactly 64KiB input is accepted' {
    $json='{"text":"'+('x'*(65536-11))+'"}'
    Assert ((Parse-RequestBytes ([Text.Encoding]::UTF8.GetBytes($json))).text.Length -eq (65536-11))
}
Check 'UTF-8 byte cap refuses multibyte overflow below character cap' {
    $json='{"text":"'+([string][char]0x20ac)*22000+'"}'
    Assert ($json.Length -lt 65536)
    Refuses {Parse-RequestBytes ([Text.Encoding]::UTF8.GetBytes($json))}
}
Check 'invalid UTF-8 is refused without replacement' {Refuses {Parse-RequestBytes ([byte[]]@(255))}}
Check 'empty malformed and non-object input is refused' {
    foreach($json in @('','{','null','[]','true','5')) { Refuses {Parse-RequestBytes ([Text.Encoding]::UTF8.GetBytes($json))} }
}

Check 'absent status is read-only' {$s=[FixtureScheduler]::new();$r=Call-Policy $s status;Assert (-not $r.registered -and $s.Folder.Creates -eq 0 -and $s.Folder.Deletes -eq 0)}
Check 'install is create-only interactive and does not run a task' {$s=[FixtureScheduler]::new();$r=Call-Policy $s install;Assert ($r.registered -and $r.changed -and $s.Folder.Flags -eq 34 -and $s.Folder.Creates -eq 1)}
Check 'matching install is idempotent' {$s=[FixtureScheduler]::new();$s.Folder.Task=New-OwnedTask;$r=Call-Policy $s install;Assert ($r.registered -and -not $r.changed -and $s.Folder.Creates -eq 0)}
Check 'remove deletes only the matching registration' {$s=[FixtureScheduler]::new();$s.Folder.Task=New-OwnedTask;$s.Folder.Legacy='keep';$r=Call-Policy $s remove;Assert (-not $r.registered -and $r.changed -and $s.Folder.Deletes -eq 1 -and $s.Folder.Legacy -ceq 'keep')}
Check 'absent removal is idempotent' {$s=[FixtureScheduler]::new();$r=Call-Policy $s remove;Assert (-not $r.changed -and $s.Folder.Deletes -eq 0)}
Check 'legacy global registration blocks preflight' {$s=[FixtureScheduler]::new();$s.Folder.Legacy='keep';Refuses {Call-Policy $s check};Assert ($s.Folder.Creates -eq 0)}
Check 'access denied is not absence' {$s=[FixtureScheduler]::new();$s.Folder.DenyRead=$true;Refuses {Call-Policy $s install};Assert ($s.Folder.Creates -eq 0)}
Check 'creation race never overwrites' {$s=[FixtureScheduler]::new();$s.Folder.RaceCreate=$true;Refuses {Call-Policy $s install};Assert ($s.Folder.Creates -eq 0)}
foreach($edit in @(
    @{name='foreign description';change={$args[0].Definition.RegistrationInfo.Description='foreign'}}
    @{name='foreign URI';change={$args[0].Definition.RegistrationInfo.URI='\other'}}
    @{name='other user';change={$args[0].Definition.Principal.UserId='S-1-5-21-1001'}}
    @{name='system principal';change={$args[0].Definition.Principal.UserId='S-1-5-18'}}
    @{name='noninteractive principal';change={$args[0].Definition.Principal.LogonType=2}}
    @{name='elevated principal';change={$args[0].Definition.Principal.RunLevel=1}}
    @{name='another executable';change={$args[0].Definition.Actions.Values[0].Path='C:\Other\hgs.exe'}}
    @{name='active background argv';change={$args[0].Definition.Actions.Values[0].Arguments+=' --background'}}
    @{name='extra action';change={$args[0].Definition.Actions.Count=2}}
    @{name='extra trigger';change={$args[0].Definition.Triggers.Count=2}}
    @{name='other trigger user';change={$args[0].Definition.Triggers.Values[0].UserId='S-1-5-21-1001'}}
    @{name='repeated trigger';change={$args[0].Definition.Triggers.Values[0].Repetition.Interval='PT1M'}}
    @{name='disabled task';change={$args[0].Enabled=$false}}
    @{name='modified restart policy';change={$args[0].Definition.Settings.RestartCount=20}}
    @{name='wake machine';change={$args[0].Definition.Settings.WakeToRun=$true}}
    @{name='foreign DACL';change={$args[0].Security='O:'+ $script:sid +'D:(A;;GA;;;WD)'}}
    @{name='foreign owner';change={$args[0].Security='O:SYD:(A;;GA;;;SY)'}}
)) {
    $s=[FixtureScheduler]::new();$s.Folder.Task=New-OwnedTask;& $edit.change $s.Folder.Task
    Check ('keep '+$edit.name) {Refuses {Call-Policy $s remove};Refuses {Call-Policy $s install};Assert ($s.Folder.Creates -eq 0 -and $s.Folder.Deletes -eq 0 -and $null -ne $s.Folder.Task)}
}
Check 'system caller is refused' {$s=[FixtureScheduler]::new();Refuses {Invoke-RegistrationOperation ([pscustomobject]@{schema=1;operation='install';plan=(New-Plan)}) $s 'S-1-5-18'};Assert ($s.Folder.Creates -eq 0)}
Check 'plan cannot select another user' {$s=[FixtureScheduler]::new();$p=New-Plan;$p.user_sid='S-1-5-21-1001';Refuses {Call-Policy $s install $p};Assert ($s.Folder.Creates -eq 0)}
Check 'plan cannot enable background writers' {$s=[FixtureScheduler]::new();$p=New-Plan;$p.argv+='--background';Refuses {Call-Policy $s install $p};Assert ($s.Folder.Creates -eq 0)}
Check 'plan cannot permit Session 0' {$s=[FixtureScheduler]::new();$p=New-Plan;$p.session.allow_session_zero=$true;Refuses {Call-Policy $s install $p};Assert ($s.Folder.Creates -eq 0)}
Write-Output ("Registration policy controls: $script:count passed; explicit scheduler/registry/service mutations=0")
