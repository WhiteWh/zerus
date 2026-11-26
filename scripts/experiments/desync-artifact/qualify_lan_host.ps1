param([Parameter(Mandatory=$true)][string]$Root,
      [Parameter(Mandatory=$true)][string]$Action,
      [string]$InputJson = '{}', [string]$Job = '')
# Synthetic qualification only. Never point this script at an existing build.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
$utf8 = New-Object Text.UTF8Encoding($false)
$Root = [IO.Path]::GetFullPath($Root).TrimEnd('\')
if ($Root -notmatch '^D:\\ZerusArtifactQualification\\[a-f0-9-]{36}$') { throw 'Invalid synthetic root' }
$nonce = Split-Path -Leaf $Root
if (([IO.File]::ReadAllText((Join-Path $Root 'synthetic-root.txt'))).Trim() -ne $nonce) { throw 'Synthetic ownership marker differs' }
$inputValue = $InputJson | ConvertFrom-Json
if ($Job) { $inputValue = [pscustomobject]@{label=$Job} }
function Path-InRoot([string]$Relative) {
    $path = [IO.Path]::GetFullPath((Join-Path $Root $Relative))
    if (-not $path.StartsWith($Root + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Path escapes synthetic root' }
    # Refuse traversal through any existing reparse point, including junctions.
    $check = $path
    while ($check.Length -gt $Root.Length) {
        if (Test-Path -LiteralPath $check) {
            if ((Get-Item -LiteralPath $check -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Reparse point in synthetic path' }
        }
        $check = Split-Path -Parent $check
    }
    return $path
}
function Save-Json([string]$Path, $Value) {
    [IO.Directory]::CreateDirectory((Split-Path -Parent $Path)) | Out-Null
    $next = $Path + '.next'
    [IO.File]::WriteAllText($next, ($Value | ConvertTo-Json -Depth 30), $utf8)
    if ([IO.File]::Exists($Path)) { [IO.File]::Replace($next, $Path, [System.Management.Automation.Language.NullString]::Value) }
    else { [IO.File]::Move($next, $Path) }
}
function Read-Json([string]$Path) {
    $stream = New-Object IO.FileStream($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, ([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete))
    try { $reader = New-Object IO.StreamReader($stream); return ($reader.ReadToEnd() | ConvertFrom-Json) }
    finally { $stream.Dispose() }
}
function Emit($Value) { $Value | ConvertTo-Json -Depth 30 -Compress }
function Add-FixtureType {
    if ('ZerusSyntheticFiles' -as [type]) { return }
    Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Security.Cryptography;
public static class ZerusSyntheticFiles {
    public static string Generate(string path, long length) {
        byte[] buffer = new byte[8 * 1024 * 1024];
        using (var random = RandomNumberGenerator.Create())
        using (var hash = SHA256.Create())
        using (var output = new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.Read, buffer.Length, FileOptions.SequentialScan)) {
            long left = length;
            while (left > 0) {
                random.GetBytes(buffer); int count = (int)Math.Min(left, buffer.Length);
                output.Write(buffer, 0, count); hash.TransformBlock(buffer, 0, count, buffer, 0); left -= count;
            }
            output.Flush(true); hash.TransformFinalBlock(new byte[0], 0, 0);
            return BitConverter.ToString(hash.Hash).Replace("-", "").ToLowerInvariant();
        }
    }
    public static void Patch(string path, long offset, int length) {
        byte[] buffer = new byte[length];
        using (var random = RandomNumberGenerator.Create()) random.GetBytes(buffer);
        using (var output = new FileStream(path, FileMode.Open, FileAccess.Write, FileShare.None)) {
            if (offset < 0 || offset + length > output.Length) throw new InvalidOperationException("Patch outside fixture");
            output.Position = offset; output.Write(buffer, 0, buffer.Length); output.Flush(true);
        }
    }
}
'@ | Out-Null
}
function Fixture-Inventory([string]$Source) {
    $items = @()
    foreach ($file in @(Get-ChildItem -LiteralPath $Source -Recurse -File)) {
        if ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Reparse fixture file' }
        $relative = $file.FullName.Substring($Source.Length + 1).Replace('\','/')
        $items += [pscustomobject]@{path=$relative;bytes=$file.Length;sha256=(Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()}
    }
    return $items
}
switch ($Action) {
    'preflight' {
        $drive = New-Object IO.DriveInfo('D:')
        Emit @{ok=$true;free_bytes=$drive.AvailableFreeSpace;os=[Environment]::OSVersion.VersionString;power_shell=$PSVersionTable.PSVersion.ToString()}
    }
    'write' {
        $path = Path-InRoot $inputValue.path
        [IO.Directory]::CreateDirectory((Split-Path -Parent $path)) | Out-Null
        if ($inputValue.kind -eq 'text') { [IO.File]::WriteAllText($path, [string]$inputValue.value, $utf8) }
        else { Save-Json $path $inputValue.value }
        Emit @{ok=$true}
    }
    'read' {
        if ([string]$inputValue.path -match '\.key$') { throw 'Private keys cannot be read by the harness' }
        Emit (Read-Json (Path-InRoot $inputValue.path))
    }
    'start' {
        $label = [string]$inputValue.label
        if ($label -notmatch '^[a-z0-9-]{1,64}$') { throw 'Invalid job label' }
        $job = Path-InRoot ('jobs\' + $label)
        if (Test-Path -LiteralPath $job) { throw 'Job label already exists; inspect its receipt' }
        [IO.Directory]::CreateDirectory($job) | Out-Null
        Save-Json (Join-Path $job 'request.json') $inputValue
        $args = '-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' + (Join-Path $Root 'qualify_lan_host.ps1') + '" -Root "' + $Root + '" -Action run -Job ' + $label
        # A Windows SSH login owns a kill-on-close process job. Use the user's
        # interactive scheduler token so a test service outlives that SSH RPC;
        # no password, elevation or native agent/session is involved.
        $scheduler=New-Object -ComObject Schedule.Service; $scheduler.Connect()
        $folder=$scheduler.GetFolder('\'); $definition=$scheduler.NewTask(0)
        $definition.RegistrationInfo.Description='Zerus synthetic '+$nonce
        $definition.Principal.UserId=[Security.Principal.WindowsIdentity]::GetCurrent().Name
        $definition.Principal.LogonType=3; $definition.Principal.RunLevel=0
        $definition.Settings.ExecutionTimeLimit='PT4H'
        $definition.Settings.DisallowStartIfOnBatteries=$false
        $definition.Settings.StopIfGoingOnBatteries=$false
        $operation=$definition.Actions.Create(0)
        $operation.Path="$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
        $operation.Arguments=$args; $operation.WorkingDirectory=$Root
        $taskName='ZerusSynthetic-'+$nonce+'-'+$label
        $task=$folder.RegisterTaskDefinition($taskName,$definition,2,$null,$null,3)
        Save-Json (Join-Path $job 'scheduler.json') @{task=$taskName;root=$nonce}
        $task.Run($null) | Out-Null
        Emit @{ok=$true;label=$label}
    }
    'run' {
        $job = Path-InRoot ('jobs\' + [string]$inputValue.label)
        $request = Read-Json (Join-Path $job 'request.json')
        $statusPath = Join-Path $job 'status.json'
        $self=Get-Process -Id $PID
        Save-Json (Join-Path $job 'wrapper.json') @{pid=$PID;birth=$self.StartTime.ToUniversalTime().Ticks}
        $timer = [Diagnostics.Stopwatch]::StartNew()
        Save-Json $statusPath @{state='running';seconds=0}
        try {
            if ($request.kind -eq 'generate') {
                $source = Path-InRoot 'source'
                if (Test-Path -LiteralPath $source) {
                    $allowed=@('packs/alpha.pak','packs/beta.ucas','packs/gamma.bundle','bin/game.dll')
                    foreach($file in @(Get-ChildItem -LiteralPath $source -Recurse -File)) {
                        $relative=$file.FullName.Substring($source.Length+1).Replace('\','/')
                        if ($relative -notin $allowed -or $file.Length -ne ([long]$request.bytes/4) -or ($file.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Partial fixture is not a complete planned generated file' }
                    }
                }
                [IO.Directory]::CreateDirectory((Join-Path $source 'packs')) | Out-Null
                [IO.Directory]::CreateDirectory((Join-Path $source 'bin')) | Out-Null
                Add-FixtureType
                $entries = @()
                foreach ($relative in @('packs/alpha.pak','packs/beta.ucas','packs/gamma.bundle','bin/game.dll')) {
                    $path = Join-Path $source $relative
                    $length = [long]$request.bytes / 4
                    if (Test-Path -LiteralPath $path) { $hash=(Get-FileHash -LiteralPath $path).Hash.ToLowerInvariant() }
                    else { $hash = [ZerusSyntheticFiles]::Generate($path,$length) }
                    $entries += @{path=$relative;bytes=$length;sha256=$hash}
                    Save-Json $statusPath @{state='running';completed_files=$entries.Count;seconds=$timer.Elapsed.TotalSeconds}
                }
                Save-Json (Path-InRoot 'fixture.json') @{generation=1;entries=$entries;total_bytes=[long]$request.bytes;source='source'}
                Save-Json $statusPath @{state='finished';exit_code=0;seconds=$timer.Elapsed.TotalSeconds;total_bytes=[long]$request.bytes}
            } else {
                $exe = Path-InRoot ([string]$request.exe)
                $config = Path-InRoot ([string]$request.config)
                $command = [string]$request.command
                if ($command -notmatch '^(identity-init|executor-serve|controller-(run|deploy|status|collect|observe)|tree-(receive|serve|status))$') { throw 'Unsupported qualification command' }
                $info = New-Object Diagnostics.ProcessStartInfo
                $info.FileName = $exe
                $info.Arguments = $command + ' "' + $config + '"'
                $info.UseShellExecute = $false; $info.CreateNoWindow = $true
                $info.RedirectStandardOutput = $true; $info.RedirectStandardError = $true
                $keep = @{}
                foreach ($key in @('SystemRoot','WINDIR','COMSPEC')) { $keep[$key] = [Environment]::GetEnvironmentVariable($key) }
                $info.EnvironmentVariables.Clear()
                foreach ($key in $keep.Keys) { if ($keep[$key]) { $info.EnvironmentVariables[$key] = $keep[$key] } }
                $info.EnvironmentVariables['PATH'] = "$env:SystemRoot\System32;$env:SystemRoot\System32\WindowsPowerShell\v1.0"
                [IO.Directory]::CreateDirectory((Join-Path $job 'temp')) | Out-Null
                $info.EnvironmentVariables['TEMP'] = Join-Path $job 'temp'; $info.EnvironmentVariables['TMP'] = Join-Path $job 'temp'
                $process = New-Object Diagnostics.Process; $process.StartInfo = $info
                if (-not $process.Start()) { throw 'Cannot start qualification child' }
                $stdout = $process.StandardOutput.ReadToEndAsync(); $stderr = $process.StandardError.ReadToEndAsync()
                $birth = $process.StartTime.ToUniversalTime().Ticks
                Save-Json (Join-Path $job 'child.json') @{pid=$process.Id;birth=$birth;exe=$exe;command=$command}
                $peak = [long]0
                while (-not $process.WaitForExit(1000)) {
                    $process.Refresh(); $peak = [Math]::Max($peak,$process.PeakWorkingSet64)
                    Save-Json $statusPath @{state='running';seconds=$timer.Elapsed.TotalSeconds;peak_rss_bytes=$peak}
                }
                $process.WaitForExit(); $exit = $process.ExitCode
                [IO.File]::WriteAllText((Join-Path $job 'stdout.log'),$stdout.Result,$utf8)
                [IO.File]::WriteAllText((Join-Path $job 'stderr.log'),$stderr.Result,$utf8)
                Save-Json $statusPath @{state='finished';exit_code=$exit;seconds=$timer.Elapsed.TotalSeconds;peak_rss_bytes=$peak}
            }
        } catch {
            Save-Json $statusPath @{state='failed';error=$_.Exception.Message;seconds=$timer.Elapsed.TotalSeconds}
            throw
        }
    }
    'status' {
        $job = Path-InRoot ('jobs\' + [string]$inputValue.label)
        if (Test-Path -LiteralPath (Join-Path $job 'status.json')) {
            $status=Read-Json (Join-Path $job 'status.json')
            if ($status.state -notin @('finished','failed')) {
                if (-not (Test-Path -LiteralPath (Join-Path $job 'wrapper.json'))) { Emit @{state='starting'}; break }
                $wrapper=Read-Json (Join-Path $job 'wrapper.json')
                $process=Get-Process -Id $wrapper.pid -ErrorAction SilentlyContinue
                if (-not $process -or $process.StartTime.ToUniversalTime().Ticks -ne [long]$wrapper.birth) { Emit @{state='failed';error='Wrapper exited without a final receipt'}; break }
            }
            Emit $status
        }
        else { Emit @{state='starting'} }
    }
    'stop' {
        $job = Path-InRoot ('jobs\' + [string]$inputValue.label)
        $child = Read-Json (Join-Path $job 'child.json')
        $process = Get-Process -Id $child.pid -ErrorAction SilentlyContinue
        if ($process) {
            if ($process.StartTime.ToUniversalTime().Ticks -ne [long]$child.birth -or $process.Path -ne $child.exe) { throw 'Child process identity changed' }
            # Only the exact synthetic helper is stopped. Never native agents.
            Stop-Process -Id $process.Id
        }
        Emit @{ok=$true}
    }
    'inspect' {
        $slot = Path-InRoot ([string]$inputValue.slot)
        $installed = Read-Json (Join-Path $slot 'tree-installed.json')
        $total = [long]0; $downloaded = [long]0; $largeOutside = @(); $outside = [long]0
        foreach ($entry in $installed.catalog.entries) { $total += [long]$entry.artifact.bytes }
        foreach ($file in @(Get-ChildItem -LiteralPath (Join-Path $slot 'files') -Filter result.json -Recurse -File)) {
            $value = Read-Json $file.FullName
            if (-not $value.ok) { throw 'Unsuccessful file reconstruction report' }
            if ($value.transport -ne 'paired-tls-private-network' -or $value.external_network -ne $true) { throw 'Result scope does not identify the physical private network' }
            $downloaded += [long]$value.downloaded_payload_bytes
        }
        foreach ($file in @(Get-ChildItem -LiteralPath $Root -Recurse -File)) {
            if ($file.FullName.StartsWith((Join-Path $slot 'build')+'\',[StringComparison]::OrdinalIgnoreCase)) { continue }
            # Packages/executables are tooling, not a second payload copy.
            if ($file.FullName.StartsWith((Join-Path $Root 'package')+'\',[StringComparison]::OrdinalIgnoreCase)) { continue }
            if ($file.Name -eq 'candidate.zip') { continue }
            $outside += $file.Length
            if ($file.Length -ge 128MB) { $largeOutside += $file.FullName.Substring($Root.Length+1) }
        }
        Emit @{state=$installed.state;total_bytes=$total;downloaded_payload_bytes=$downloaded;outside_bytes=$outside;large_outside=$largeOutside;files=$installed.files;artifact=$installed.artifact_id;delivery=$installed.delivery_id}
    }
    'mutate' {
        $source = Path-InRoot ([string]$inputValue.source)
        $manifest = Read-Json (Path-InRoot 'fixture.json')
        Add-FixtureType
        foreach ($entry in $manifest.entries) {
            $path = Join-Path $source $entry.path
            if ((Get-Item -LiteralPath $path).Length -ne [long]$entry.bytes) { throw 'Fixture length differs before mutation' }
        }
        # Four 16 MiB replacements in one large opaque file; no full rewrite.
        foreach ($offset in @([long]64MB,[long]1024MB,[long]2048MB,[long]3072MB)) {
            [ZerusSyntheticFiles]::Patch((Join-Path $source 'packs/alpha.pak'),$offset,16MB)
        }
        if (Test-Path -LiteralPath (Join-Path $source 'packs/gamma.bundle')) {
            Move-Item -LiteralPath (Join-Path $source 'packs/gamma.bundle') -Destination (Join-Path $source 'packs/renamed.container')
        } else {
            Move-Item -LiteralPath (Join-Path $source 'packs/renamed.container') -Destination (Join-Path $source 'packs/renamed-again.assetpack')
        }
        $entries = Fixture-Inventory $source
        Save-Json (Path-InRoot 'fixture.json') @{generation=([int]$manifest.generation+1);entries=$entries;total_bytes=($entries | Measure-Object -Property bytes -Sum).Sum;source=[string]$inputValue.source}
        Emit @{ok=$true;changed_bytes=64MB;renamed_bytes=5GB}
    }
    'verify' {
        $source = Path-InRoot ([string]$inputValue.source)
        $manifest = Read-Json (Path-InRoot 'fixture.json')
        $actual = @(Fixture-Inventory $source)
        if ($actual.Count -ne @($manifest.entries).Count) { throw 'Unexpected fixture file set' }
        foreach ($entry in $manifest.entries) {
            $found = @($actual | Where-Object {$_.path -eq $entry.path})
            if ($found.Count -ne 1 -or $found[0].bytes -ne $entry.bytes -or $found[0].sha256 -ne $entry.sha256) { throw 'Synthetic fixture bytes differ' }
        }
        Emit @{ok=$true;files=$actual.Count}
    }
    'retire-source' {
        $source = Path-InRoot 'source'
        $manifest = Read-Json (Path-InRoot 'fixture.json')
        if ($manifest.source -ne 'source') { throw 'Only the originally generated source may be retired' }
        $actual = @(Get-ChildItem -LiteralPath $source -Recurse -File)
        if ($actual.Count -ne @($manifest.entries).Count) { throw 'Unexpected source entries; refusing retirement' }
        foreach ($entry in $manifest.entries) {
            $path = Path-InRoot ('source\' + $entry.path.Replace('/','\'))
            if ((Get-Item -LiteralPath $path).Length -ne [long]$entry.bytes -or (Get-FileHash -LiteralPath $path).Hash.ToLowerInvariant() -ne $entry.sha256) { throw 'Changed synthetic source; refusing retirement' }
        }
        # Exact verified generated files only, never a recursive tree removal.
        foreach ($entry in $manifest.entries) { Remove-Item -LiteralPath (Path-InRoot ('source\' + $entry.path.Replace('/','\'))) }
        Emit @{ok=$true;retired_bytes=$manifest.total_bytes}
    }
    default { throw 'Unsupported synthetic qualification action' }
}
