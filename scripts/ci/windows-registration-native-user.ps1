param(
    [ValidateSet('Controller', 'Child')][string]$Mode = 'Controller',
    [Parameter(Mandatory=$true)][string]$Stage,
    [string]$Hgs
)
# Disposable hosted Windows only. The candidate builds its own release image.
# Register/remove only this new ordinary user's passive task; never Run/Stop/End.
# PowerShell is a test controller only. hgs's OS-registration child is native.
$Layout = 'native-release'
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
if ($Mode -eq 'Controller' -and ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted')) {
    throw 'This account-creation fixture requires a disposable GitHub-hosted runner'
}
if ($Mode -ne 'Controller') {
    $script:PSModuleAutoLoadingPreference = 'None'
    $phasePath = [IO.Path]::Combine($Stage, 'child-phases.txt')
    [IO.File]::WriteAllText($phasePath, "script_start`n")
}

$captureSource = @'
using System;
using System.Diagnostics;
using System.IO;
using System.ServiceProcess;
using System.Text;
using System.Threading.Tasks;
public static class RegistrationCapture {
    public static Task<string> Read(Stream stream) {
        return Task.Run(() => {
            var bytes = new byte[65537]; int count = 0;
            while (count < bytes.Length) {
                int n = stream.Read(bytes, count, bytes.Length - count);
                if (n == 0) return new UTF8Encoding(false, true).GetString(bytes, 0, count);
                count += n;
            }
            throw new InvalidDataException("capture exceeded 64 KiB");
        });
    }
    public static Task<bool> ReadyLogonService(string phases) {
        return Task.Run(() => {
            var watch = Stopwatch.StartNew();
            using (var service = new ServiceController("seclogon")) {
                var before = service.Status;
                File.AppendAllText(phases, "before " + before + "\n");
                if (before == ServiceControllerStatus.Stopped) service.Start();
                else if (before != ServiceControllerStatus.Running && before != ServiceControllerStatus.StartPending)
                    throw new InvalidOperationException("Unexpected Secondary Logon state");
                service.WaitForStatus(ServiceControllerStatus.Running, TimeSpan.FromSeconds(20));
                File.AppendAllText(phases, "running " + watch.ElapsedMilliseconds + "\n");
                return true;
            }
        });
    }
    public static Task<bool> Start(Process child, string phases) {
        return Task.Run(() => {
            File.AppendAllText(phases, "start_enter\n");
            bool started = child.Start();
            File.AppendAllText(phases, "start_return\n");
            return started;
        });
    }
    public static Task Stop(Process child) {
        // A native termination request can itself stall. Keep it off the
        // caller deadline and retain the original Process until a real wait.
        return Task.Run(() => { child.Kill(); child.WaitForExit(); });
    }
}
'@

$serializerAssembly = [Reflection.Assembly]::Load('System.Web.Extensions, Version=4.0.0.0, Culture=neutral, PublicKeyToken=31bf3856ad364e35')
$reportJson = [Activator]::CreateInstance($serializerAssembly.GetType('System.Web.Script.Serialization.JavaScriptSerializer', $true))
$reportJson.MaxJsonLength = 262144

function Write-Report($Path, $Value) {
    [IO.File]::WriteAllText($Path, $reportJson.Serialize($Value), [Text.UTF8Encoding]::new($false))
}

if ($Mode -ne 'Controller') {
    [IO.File]::AppendAllText($phasePath, "serializer_ready`n")
    # CreateProcessWithLogonW supplies a new standard-user token, not an unelevated admin.
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $administratorSidPresent = $false
    foreach ($group in $identity.Groups) {
        if ($group.Value -eq 'S-1-5-32-544') { $administratorSidPresent = $true }
    }
    $report = [ordered]@{
        layout = $Layout
        sid = $identity.User.Value
        administrator_sid_present = $administratorSidPresent
        session = [Diagnostics.Process]::GetCurrentProcess().SessionId
        calls = @()
        complete = $false
    }
    $reportPath = [IO.Path]::Combine($Stage, 'result.json')
    Write-Report $reportPath $report
    if ($administratorSidPresent -ne $false) { throw 'Unexpected account privilege class' }
    # Keep Windows' actual account profile. Only Zerus roots and temporary files
    # are private, matching the cold installer acceptance environment.
    foreach ($name in @('home', 'config', 'state', 'tmp')) {
        [void][IO.Directory]::CreateDirectory(([IO.Path]::Combine($Stage, $name)))
    }
    $env:HOME = [IO.Path]::Combine($Stage, 'home')
    $env:HGS_CONFIG_DIR = [IO.Path]::Combine($Stage, 'config')
    $env:HGS_STATE_DIR = [IO.Path]::Combine($Stage, 'state')
    $env:TEMP = [IO.Path]::Combine($Stage, 'tmp')
    $env:TMP = $env:TEMP
    $env:HGS_SELF = 'windows-registration-standard-fixture'
    $env:HGS_PEERS = ''
    $env:HGS_DESKTOP_REGISTRATION_TRACE = '1'
    # Read loops retain at most 64 KiB each, even if a failed child keeps writing.
    [void][Reflection.Assembly]::Load([IO.File]::ReadAllBytes([IO.Path]::Combine($Stage, 'capture.dll')))
    foreach ($arguments in @('--version', 'desktop registration status --json', 'desktop registration install --json', 'desktop registration install --json', 'desktop registration remove --json', 'desktop registration status --json')) {
        $child = [Diagnostics.Process]::new()
        $child.StartInfo.FileName = [IO.Path]::Combine($Stage, 'hgs.exe')
        $child.StartInfo.Arguments = $arguments
        $child.StartInfo.UseShellExecute = $false
        $child.StartInfo.CreateNoWindow = $true
        $child.StartInfo.RedirectStandardInput = $true
        $child.StartInfo.RedirectStandardOutput = $true
        $child.StartInfo.RedirectStandardError = $true
        $call = [ordered]@{ arguments=$arguments; phase='spawn'; physical_wait=$false }
        $report.calls += $call
        Write-Report $reportPath $report
        $watch = [Diagnostics.Stopwatch]::StartNew()
        [IO.File]::AppendAllText($phasePath, "hgs_spawn`n")
        [void]$child.Start()
        # Opening the ORIGINAL Process handle fixes PID reuse before recording.
        $originalHandle = $child.Handle
        $call.pid = $child.Id
        $call.birth = $child.StartTime.ToUniversalTime().ToFileTimeUtc().ToString()
        $call.phase = 'started'
        $child.StandardInput.Close() # Real EOF, never the runner's synchronous pipe.
        $stdout = [RegistrationCapture]::Read($child.StandardOutput.BaseStream)
        $stderr = [RegistrationCapture]::Read($child.StandardError.BaseStream)
        Write-Report $reportPath $report
        if (-not $child.WaitForExit(20000)) {
            $stop = [RegistrationCapture]::Stop($child)
            try { [void]$stop.Wait(2000) } catch { }
            $call.phase = 'timeout-or-termination-pending'
            Write-Report $reportPath $report
            throw 'Original release child exceeded the fixed test deadline; preserve fixture'
        }
        [IO.File]::AppendAllText($phasePath, "hgs_exit`n")
        $call.physical_wait = $true
        $call.exit = $child.ExitCode
        $call.elapsed_ms = $watch.ElapsedMilliseconds
        if (-not [Threading.Tasks.Task]::WaitAll(@($stdout, $stderr), 2000)) {
            $call.phase = 'capture-pending'
            Write-Report $reportPath $report
            throw 'Original release output is incomplete; preserve fixture'
        }
        $call.stdout = $stdout.GetAwaiter().GetResult()
        $call.stderr = $stderr.GetAwaiter().GetResult()
        $call.phase = 'exited'
        Write-Report $reportPath $report
        $child.Dispose()
        if ($call.exit -ne 0) { throw 'Original release command failed; preserve fixture' }
        if ($arguments -ne '--version') {
            $value = $reportJson.DeserializeObject($call.stdout)
            $installed = $arguments -eq 'desktop registration install --json'
            if ($value.registered -ne $installed) { throw 'Unexpected exact own-task registration state' }
            if ($installed -and ($value.started -ne $false -or $value.stopped -ne $false -or $value.task_state -ne 3)) { throw 'Passive task must remain ready, never started' }
            if ($installed -and $report.calls.Count -eq 3 -and $value.changed -ne $true) { throw 'Fresh native installation must change its absent task' }
            if ($installed -and $report.calls.Count -eq 4 -and $value.changed -ne $false) { throw 'Repeated install must not replace its task' }
        }
    }
    $report.complete = $true
    Write-Report $reportPath $report
    return
}

$source = (Resolve-Path $Hgs).Path
if ([IO.Path]::GetFileName($source) -ne 'hgs.exe' -or [IO.Path]::GetFileName([IO.Path]::GetDirectoryName($source)) -ne 'release') {
    throw 'Exact release hgs.exe required'
}
$nonce = [Guid]::NewGuid().ToString('N')
$userName = 'zr' + $nonce.Substring(0, 12)
$Stage = [IO.Path]::GetFullPath($Stage)
if (Test-Path $Stage) { throw 'Fixture stage must not exist' }
[void][IO.Directory]::CreateDirectory($Stage)
$digest = (Get-FileHash $source -Algorithm SHA256).Hash.ToLowerInvariant()
Write-Output "Native registration RELEASE image SHA256 $digest"
$passwordBytes = New-Object byte[] 32
[Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($passwordBytes)
$password = [Convert]::ToBase64String($passwordBytes) + '!aA1'
$secure = ConvertTo-SecureString $password -AsPlainText -Force
$ownedUser = $null
$outer = $null
$complete = $false
try {
    # Compile the bounded reader once under the warm controller. The fresh
    # account loads bytes only; no Add-Type/JSON/path cmdlet autoloading.
    $capturePath = Join-Path $Stage 'capture.dll'
    $serviceAssembly=[Reflection.Assembly]::Load('System.ServiceProcess, Version=4.0.0.0, Culture=neutral, PublicKeyToken=b03f5f7f11d50a3a')
    Add-Type -TypeDefinition $captureSource -ReferencedAssemblies @('System.dll','System.Core.dll',$serviceAssembly.Location) -OutputAssembly $capturePath
    $ownedUser = New-LocalUser -Name $userName -Password $secure -Description 'Disposable Zerus CI fixture' -AccountNeverExpires
    # Explicit ordinary Users membership; never add an administrator membership.
    Add-LocalGroupMember -SID 'S-1-5-32-545' -Member $ownedUser
    & icacls.exe $Stage /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' "*$($ownedUser.SID.Value):(OI)(CI)M" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Private account ACL setup failed' }
    Copy-Item $source (Join-Path $Stage 'hgs.exe')
    Copy-Item $PSCommandPath (Join-Path $Stage 'fixture.ps1')
    if ((Get-FileHash (Join-Path $Stage 'hgs.exe') -Algorithm SHA256).Hash.ToLowerInvariant() -ne $digest) {
        throw 'Release image copy differs'
    }
    $program = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $scriptPath = (Join-Path $Stage 'fixture.ps1').Replace("'", "''")
    $stageLiteral = $Stage.Replace("'", "''")
    # Fixed owned source loader, like the maintained policy corpus. This does
    # not change any execution-policy setting for the new account or machine.
    $entry = "& ([ScriptBlock]::Create([IO.File]::ReadAllText('$scriptPath'))) -Mode Child -Stage '$stageLiteral'"
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($entry))
    if ($encoded.Length -ge 30000) { throw 'Fixture entry exceeds command-line bound' }
    $arguments = '-NoLogo -NoProfile -NonInteractive -EncodedCommand ' + $encoded
    # .NET uses CreateProcessWithLogonW for this actual new local account.
    # Leave EnvironmentVariables untouched: the OS builds its profile environment.
    if (($program.Length + $arguments.Length + 3) -ge 1024) { throw 'Credentialed launch exceeds its native bound' }
    $outer = [Diagnostics.Process]::new()
    $outer.StartInfo.FileName = $program
    $outer.StartInfo.Arguments = $arguments
    $outer.StartInfo.WorkingDirectory = $Stage
    $outer.StartInfo.UseShellExecute = $false
    $outer.StartInfo.CreateNoWindow = $true
    $outer.StartInfo.UserName = $userName
    $outer.StartInfo.Domain = $env:COMPUTERNAME
    $outer.StartInfo.Password = $secure
    $outer.StartInfo.LoadUserProfile = $true
    $outer.StartInfo.RedirectStandardInput = $true
    $outer.StartInfo.RedirectStandardOutput = $true
    $outer.StartInfo.RedirectStandardError = $true
    [void][Reflection.Assembly]::Load([IO.File]::ReadAllBytes($capturePath))
    # This is fixture setup on the disposable VM, never a production service
    # dependency repair. Bound SCM start/read as well as the subsequent wait.
    $serviceReport=[ordered]@{service='seclogon';ready=$false;elapsed_ms=0}
    $serviceWatch=[Diagnostics.Stopwatch]::StartNew()
    $serviceReportPath=Join-Path $env:GITHUB_WORKSPACE 'artifacts/test-results/registration-logon-service.json'
    Write-Report $serviceReportPath $serviceReport
    $serviceReady=[RegistrationCapture]::ReadyLogonService((Join-Path $Stage 'logon-service-phases.txt'))
    try {
        if (-not $serviceReady.Wait(30000)) { throw 'Secondary Logon readiness remains pending; preserve fixture' }
        $serviceReport.ready=$serviceReady.Result
        if (-not $serviceReport.ready) { throw 'Secondary Logon readiness was not confirmed' }
    } finally {
        $serviceReport.elapsed_ms=$serviceWatch.ElapsedMilliseconds
        Write-Report $serviceReportPath $serviceReport
    }
    $launch = [ordered]@{ phase='spawn'; physical_wait=$false }
    $launchPath = Join-Path $env:GITHUB_WORKSPACE ('artifacts/test-results/registration-' + $Layout + '-launch.json')
    Write-Report $launchPath $launch
    $starting = [RegistrationCapture]::Start($outer, (Join-Path $Stage 'launch-native-phases.txt'))
    if (-not $starting.Wait(20000)) {
        $launch.phase = 'spawn-pending'
        Write-Report $launchPath $launch
        throw 'Credentialed original launch is pending; preserve fixture'
    }
    if (-not $starting.Result) { throw 'Credentialed original launch refused' }
    $originalHandle = $outer.Handle
    $launch.pid = $outer.Id
    $launch.birth = $outer.StartTime.ToUniversalTime().ToFileTimeUtc().ToString()
    $launch.phase = 'started'
    Write-Report $launchPath $launch
    $password = $null
    $secure = $null
    $outer.StandardInput.Close()
    $outerOut = [RegistrationCapture]::Read($outer.StandardOutput.BaseStream)
    $outerErr = [RegistrationCapture]::Read($outer.StandardError.BaseStream)
    if (-not $outer.WaitForExit(150000)) {
        $stop = [RegistrationCapture]::Stop($outer)
        try { [void]$stop.Wait(2000) } catch { }
        $launch.phase = 'timeout-or-termination-pending'
        Write-Report $launchPath $launch
        throw 'Original wrapper deadline reached; preserve fixture'
    }
    $launch.physical_wait = $true
    $launch.exit = $outer.ExitCode
    $launch.phase = 'exited'
    if (-not [Threading.Tasks.Task]::WaitAll(@($outerOut, $outerErr), 2000)) {
        Write-Report $launchPath $launch
        throw 'Original wrapper capture remains pending'
    }
    $launch.stdout = $outerOut.GetAwaiter().GetResult()
    $launch.stderr = $outerErr.GetAwaiter().GetResult()
    $outer.Dispose()
    $launch.result_present = [IO.File]::Exists((Join-Path $Stage 'result.json'))
    $phasePath = Join-Path $Stage 'child-phases.txt'
    if ([IO.File]::Exists($phasePath)) {
        if ((Get-Item $phasePath).Length -gt 1024) { throw 'Fixture phase bound exceeded' }
        $launch.phases = [IO.File]::ReadAllText($phasePath)
    }
    Write-Report (Join-Path $env:GITHUB_WORKSPACE ('artifacts/test-results/registration-' + $Layout + '-launch.json')) $launch
    Write-Output ($reportJson.Serialize($launch))
    $path = Join-Path $Stage 'result.json'
    if (Test-Path $path) {
        $text = [IO.File]::ReadAllText($path)
        if ($text.Length -gt 200000) { throw 'Fixture result exceeds bound' }
        [IO.File]::WriteAllText((Join-Path $env:GITHUB_WORKSPACE ('artifacts/test-results/registration-' + $Layout + '-standard-user.json')), $text)
        Write-Output $text
        $result = $text | ConvertFrom-Json
        $complete = $result.complete -eq $true -and $result.sid -eq $ownedUser.SID.Value -and $result.administrator_sid_present -eq $false -and $result.calls.Count -eq 6 -and $launch.physical_wait -and $launch.exit -eq 0
    }
    if (-not $complete) { throw 'Standard-user native release lifecycle failed; retained only on disposable VM' }
} finally {
    # Retain the ORIGINAL child evidence even when credentialed Start itself
    # never returned. Its absence is not evidence that the child never ran.
    foreach ($entry in @(@('logon-service-phases.txt',1024), @('launch-native-phases.txt',1024), @('child-phases.txt',1024), @('result.json',200000))) {
        $path = Join-Path $Stage $entry[0]
        if ([IO.File]::Exists($path)) {
            $file=[IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
            try {
                $length=$file.Length
                if ($length -gt $entry[1]) { throw 'Original fixture evidence exceeds its bound' }
                $buffer=[byte[]]::new([int]$length)
                $offset=0
                while ($offset -lt $buffer.Length) {
                    $read=$file.Read($buffer,$offset,$buffer.Length-$offset)
                    if ($read -le 0) { throw 'Original fixture evidence is incomplete' }
                    $offset+=$read
                }
                $destination=Join-Path $env:GITHUB_WORKSPACE ('artifacts/test-results/registration-' + $Layout + '-' + $entry[0])
                [IO.File]::WriteAllBytes($destination,$buffer)
            } finally { $file.Dispose() }
        }
    }
    if ($complete) {
        $profileDeadline = [DateTime]::UtcNow.AddSeconds(5)
        do {
            $profile = Get-CimInstance Win32_UserProfile -Filter "SID='$($ownedUser.SID.Value)'"
            if (-not $profile.Loaded) { break }
            Start-Sleep -Milliseconds 100
        } while ([DateTime]::UtcNow -lt $profileDeadline)
        if ($profile.Loaded) { throw 'Original fixture profile remains loaded' }
        if ($profile) { $profile | Remove-CimInstance }
        Remove-LocalUser -SID $ownedUser.SID
        Remove-Item -LiteralPath $Stage -Recurse
        Write-Output 'Standard-user account, profile and stage removed; owned passive task absent'
    } else {
        Write-Output "Incomplete fixture preserved on disposable VM only: $Stage; no product effects replayed"
    }
}
