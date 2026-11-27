<#
Multi-person executor setup on one Windows host (README, "Operator setup,
migration and rollback"). Run once from an elevated PowerShell. It creates
exactly the objects the README lists and nothing else:

  1. the non-administrator service account (default zerus-exec);
  2. the executor folder (default C:\Program Files\Zerus Executor) with the
     binary and its qualification record: Administrators, SYSTEM and
     TrustedInstaller write, the service and Users read, no
     ALL APPLICATION PACKAGES entry;
  3. the executor folder for state, identity and configuration (default
     C:\ProgramData\ZerusExecutor): the service owns its state and identity;
     the configuration is installer-owned and only readable by the service;
  4. the operator folder (default C:\ProgramData\ZerusOperator) with the first
     policy revision: the named operator, Administrators and SYSTEM write,
     the service reads;
  5. the volume-root listing for the installation's jobs capability;
  6. the scheduled task "Zerus Shared Executor" that runs
     `hgs-executor executor-serve-shared` as the service account at startup.

-Uninstall removes them again. The script never edits a policy after the
first revision; use access-admit-device and access-revoke for that.
#>
param(
    [string]$Package = '',
    [string]$Operator = 'zerus-operator',
    [string]$ServiceAccount = 'zerus-exec',
    [string]$InstallDir = "$env:ProgramFiles\Zerus Executor",
    [string]$DataDir = "$env:ProgramData\ZerusExecutor",
    [string]$OperatorDir = "$env:ProgramData\ZerusOperator",
    [string]$ListenIP = '',
    [int]$Port = 7443,
    [string]$Log = '',
    [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'
if ($Log) { Start-Transcript -LiteralPath $Log -Force | Out-Null }
try {
$task = 'Zerus Shared Executor'
$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'run this from an elevated PowerShell' }

function Set-Protected([string]$Path, [string[]]$Grants, [string]$Owner = '*S-1-5-32-544') {
    & icacls $Path /inheritance:r /grant:r @Grants | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "icacls failed on $Path" }
    & icacls $Path /setowner $Owner | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "owner change failed on $Path" }
}

if ($Uninstall) {
    Unregister-ScheduledTask -TaskName $task -Confirm:$false -ErrorAction SilentlyContinue
    Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -like "$InstallDir\*" } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
    $cfg = Join-Path $DataDir 'config\executor-shared.json'
    $exe = Join-Path $InstallDir 'hgs-executor.exe'
    if ((Test-Path $cfg) -and (Test-Path $exe)) {
        $sid = (& $exe appcontainer-capability $cfg | Select-Object -First 1).Trim()
        if ($sid -like 'S-1-15-3-*') {
            & icacls "$($DataDir.Substring(0,3))" /remove:g "*$sid" | Out-Null
            $ancestor = Split-Path $DataDir
            while ($ancestor -and $ancestor.Length -gt 3) { & icacls $ancestor /remove:g "*$sid" | Out-Null; $ancestor = Split-Path $ancestor }
        }
    }
    foreach ($p in @($InstallDir, $DataDir, $OperatorDir)) { if (Test-Path $p) { & takeown /f $p /r /d y | Out-Null; & icacls $p /reset /t /q | Out-Null; Remove-Item -LiteralPath $p -Recurse -Force } }
    foreach ($u in @($ServiceAccount, $Operator)) { if (Get-LocalUser -Name $u -ErrorAction SilentlyContinue) { Remove-LocalUser -Name $u } }
    Write-Output 'Uninstalled.'
    return
}

if (-not $Package) { throw '-Package: the release hgs-executor-windows-x86_64.zip, or its folder' }
# The named operator must be an individual non-administrator account. A
# dedicated one is created if missing; its password goes to a file only the
# person running this setup can read, for running access-admit-device as it.
if (-not (Get-LocalUser -Name $Operator -ErrorAction SilentlyContinue)) {
    $ob = New-Object byte[] 24
    [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($ob)
    $opw = [Convert]::ToBase64String($ob) + 'a1!'
    New-LocalUser -Name $Operator -Password (ConvertTo-SecureString $opw -AsPlainText -Force) -PasswordNeverExpires -UserMayNotChangePassword -Description 'Zerus executor access operator' | Out-Null
    $secret = Join-Path $env:USERPROFILE ".zerus\$Operator.password"
    New-Item -ItemType Directory -Force -Path (Split-Path $secret) | Out-Null
    Set-Content -LiteralPath $secret -Value $opw -Encoding ascii -NoNewline
    & icacls $secret /inheritance:r /grant:r "${env:USERDOMAIN}\${env:USERNAME}:F" | Out-Null
}
if ((Get-LocalGroupMember -SID 'S-1-5-32-544').Name -contains "$env:COMPUTERNAME\$Operator") { throw 'the operator must not be an administrator' }
$operatorSid = (New-Object Security.Principal.NTAccount($Operator)).Translate([Security.Principal.SecurityIdentifier]).Value

# 1. service account
$bytes = New-Object byte[] 24
[Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
$password = [Convert]::ToBase64String($bytes) + 'a1!'
$secure = ConvertTo-SecureString $password -AsPlainText -Force
if (Get-LocalUser -Name $ServiceAccount -ErrorAction SilentlyContinue) {
    Set-LocalUser -Name $ServiceAccount -Password $secure
} else {
    New-LocalUser -Name $ServiceAccount -Password $secure -PasswordNeverExpires -UserMayNotChangePassword -Description 'Zerus multi-person executor service' | Out-Null
}
if ((Get-LocalGroupMember -SID 'S-1-5-32-544').Name -contains "$env:COMPUTERNAME\$ServiceAccount") { throw 'the service account must not be an administrator' }
$serviceSid = (Get-LocalUser -Name $ServiceAccount).SID.Value
# The task runs as a batch logon; Task Scheduler does not grant that right on
# every Windows edition, so it is granted here explicitly.
if (-not ('Zerus.Lsa' -as [type])) {
    Add-Type -Namespace Zerus -Name Lsa -MemberDefinition @'
[StructLayout(LayoutKind.Sequential)] public struct LSA_UNICODE_STRING { public ushort Length; public ushort MaximumLength; public IntPtr Buffer; }
[StructLayout(LayoutKind.Sequential)] public struct LSA_OBJECT_ATTRIBUTES { public int Length; public IntPtr RootDirectory; public IntPtr ObjectName; public int Attributes; public IntPtr SecurityDescriptor; public IntPtr SecurityQualityOfService; }
[DllImport("advapi32.dll")] public static extern uint LsaOpenPolicy(IntPtr system, ref LSA_OBJECT_ATTRIBUTES attributes, int access, out IntPtr handle);
[DllImport("advapi32.dll")] public static extern uint LsaAddAccountRights(IntPtr handle, byte[] sid, LSA_UNICODE_STRING[] rights, int count);
[DllImport("advapi32.dll")] public static extern uint LsaClose(IntPtr handle);
[DllImport("advapi32.dll")] public static extern int LsaNtStatusToWinError(uint status);
public static int Grant(byte[] sid, string right) {
    LSA_OBJECT_ATTRIBUTES a = new LSA_OBJECT_ATTRIBUTES(); IntPtr h;
    uint st = LsaOpenPolicy(IntPtr.Zero, ref a, 0x00000800 | 0x00000010, out h);
    if (st != 0) return LsaNtStatusToWinError(st);
    LSA_UNICODE_STRING[] r = new LSA_UNICODE_STRING[1];
    r[0].Buffer = Marshal.StringToHGlobalUni(right); r[0].Length = (ushort)(right.Length * 2); r[0].MaximumLength = (ushort)(right.Length * 2 + 2);
    try { st = LsaAddAccountRights(h, sid, r, 1); } finally { Marshal.FreeHGlobal(r[0].Buffer); LsaClose(h); }
    return st == 0 ? 0 : LsaNtStatusToWinError(st);
}
'@
}
$sidObject = New-Object Security.Principal.SecurityIdentifier($serviceSid)
$sidBytes = New-Object byte[] ($sidObject.BinaryLength)
$sidObject.GetBinaryForm($sidBytes, 0)
$rightError = [Zerus.Lsa]::Grant($sidBytes, 'SeBatchLogonRight')
if ($rightError -ne 0) { throw "granting the batch logon right failed ($rightError)" }

# 2. executor folder: binary and qualification record
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$source = $Package
if ($Package -like '*.zip') {
    $source = Join-Path $env:TEMP ('zerus-executor-' + [guid]::NewGuid())
    Expand-Archive -LiteralPath $Package -DestinationPath $source
    $source = Join-Path $source 'hgs-executor-windows-x86_64'
}
foreach ($name in 'hgs-executor.exe', 'appcontainer-qualification.json') {
    if (-not (Test-Path (Join-Path $source $name))) { throw "$name is missing from the package" }
    Copy-Item -LiteralPath (Join-Path $source $name) -Destination $InstallDir -Force
}
Copy-Item -LiteralPath (Join-Path $source 'licenses') -Destination $InstallDir -Recurse -Force -ErrorAction SilentlyContinue
Set-Protected $InstallDir @('*S-1-5-32-544:(OI)(CI)F', '*S-1-5-18:(OI)(CI)F', '*S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464:(OI)(CI)F', "*${serviceSid}:(OI)(CI)RX", '*S-1-5-32-545:(OI)(CI)RX')
$exe = Join-Path $InstallDir 'hgs-executor.exe'

# 3. data folder: state and identity owned by the service, configuration installer-owned
$state = Join-Path $DataDir 'state'; $identity = Join-Path $DataDir 'identity'; $configDir = Join-Path $DataDir 'config'
foreach ($p in @($DataDir, $state, $configDir)) { New-Item -ItemType Directory -Force -Path $p | Out-Null }
Set-Protected $DataDir @('*S-1-5-32-544:(OI)(CI)F', '*S-1-5-18:(OI)(CI)F', "*${serviceSid}:RX")
Set-Protected $state @('*S-1-5-32-544:(OI)(CI)F', '*S-1-5-18:(OI)(CI)F', "*${serviceSid}:(OI)(CI)F") "*$serviceSid"
# The configuration folder also receives the executor's offer file: the service
# may add files and owns what it creates, but may not delete the configuration.
Set-Protected $configDir @('*S-1-5-32-544:(OI)(CI)F', '*S-1-5-18:(OI)(CI)F', "*${serviceSid}:(RX,WD)", '*S-1-3-0:(OI)(IO)F')

# The executor identity is created by this elevated setup, then handed to the
# service account: a logon as another user (Start-Process -Credential) is not
# available from every elevated context, a hosted runner's session 0 included.
if (-not (Test-Path (Join-Path $identity 'public.json'))) {
    $seed = Join-Path $DataDir ('identity-seed-' + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $seed | Out-Null
    $init = Join-Path $seed 'identity-init.json'
    '{}' | Out-File -LiteralPath $init -Encoding ascii
    $text = & $exe identity-init $init 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) { throw "hgs-executor identity-init failed ($LASTEXITCODE): $text" }
    if (Test-Path $identity) { Remove-Item -LiteralPath $identity -Recurse -Force }
    Move-Item -LiteralPath (Join-Path $seed 'identity') -Destination $identity
    Remove-Item -LiteralPath $seed -Recurse -Force
    Set-Protected $identity @('*S-1-5-32-544:(OI)(CI)F', '*S-1-5-18:(OI)(CI)F', "*${serviceSid}:(OI)(CI)F") "*$serviceSid"
    & icacls "$identity\*" /reset /q | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'identity file permissions failed' }
    & icacls "$identity\*" /setowner "*$serviceSid" /q | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'identity file owner change failed' }
}
$hostFingerprint = (Get-Content -LiteralPath (Join-Path $identity 'public.json') -Raw | ConvertFrom-Json).fingerprint

# 4. operator folder and the first policy revision
New-Item -ItemType Directory -Force -Path $OperatorDir, (Join-Path $OperatorDir 'incoming') | Out-Null
Set-Protected $OperatorDir @('*S-1-5-32-544:(OI)(CI)F', '*S-1-5-18:(OI)(CI)F', "*${operatorSid}:(OI)(CI)M", "*${serviceSid}:(OI)(CI)RX")
$policy = Join-Path $OperatorDir 'access-policy.json'
if (-not (Test-Path $policy)) {
    [ordered]@{ contract = 'zerus-host-access-v1'; host = $hostFingerprint; revision = 1; persons = @(); devices = @(); projects = @(); operators = @(); contexts = @(); revoked_devices = @(); revoked_persons = @() } |
        ConvertTo-Json -Depth 4 | Out-File -LiteralPath $policy -Encoding utf8NoBOM
}

if (-not $ListenIP) {
    $ListenIP = (Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -notlike '127.*' -and $_.PrefixOrigin -ne 'WellKnown' -and $_.IPAddress -match '^(10|192\.168|172\.(1[6-9]|2[0-9]|3[01]))\.' } | Select-Object -First 1).IPAddress
}
if (-not $ListenIP) { throw 'no private IPv4 address; pass -ListenIP' }
$config = Join-Path $configDir 'executor-shared.json'
[ordered]@{
    root = $state
    peer = [ordered]@{ identity = $identity; listen = "${ListenIP}:$Port"; read_idle_seconds = 30 }
    serve_seconds = 604800; memory_bytes = 1073741824; scenario_memory_bytes = 536870912; scenario_seconds = 3600; stash_bytes = 16777216
    access = [ordered]@{ policy = $policy; operator = $operatorSid; isolation_backend = 'windows-appcontainer-v1'; per_person_jobs = 3 }
} | ConvertTo-Json -Depth 4 | Out-File -LiteralPath $config -Encoding utf8NoBOM
& icacls $config /inheritance:r /grant:r '*S-1-5-32-544:F' '*S-1-5-18:F' "*${serviceSid}:R" | Out-Null
& icacls $config /setowner '*S-1-5-32-544' | Out-Null

# 5. volume-root listing for the installation's jobs capability
$sid = (& $exe appcontainer-capability $config | Select-Object -First 1).Trim()
if ($sid -notlike 'S-1-15-3-*') { throw "no jobs capability: $sid" }
& icacls "$($state.Substring(0,3))" /grant "*${sid}:(RD,X,RA)" | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'volume root grant failed' }
# Every folder between the volume root and the executor root lists names for
# the same capability, as the executor's own entry (not inherited); the
# service account may not change these folders' permissions itself.
$ancestor = Split-Path $state
$ancestors = @()
while ($ancestor -and $ancestor.Length -gt 3) { $ancestors = @($ancestor) + $ancestors; $ancestor = Split-Path $ancestor }
foreach ($folder in $ancestors) {
    & icacls $folder /grant "*${sid}:(RD,X,RA,S)" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "folder listing grant failed on $folder" }
}

# 6. the service task
$action = New-ScheduledTaskAction -Execute $exe -Argument "executor-serve-shared `"$config`"" -WorkingDirectory $state
$trigger = New-ScheduledTaskTrigger -AtStartup
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
Register-ScheduledTask -TaskName $task -Action $action -Trigger $trigger -Settings $settings -User $ServiceAccount -Password $password -RunLevel Limited -Force | Out-Null
Start-ScheduledTask -TaskName $task
Start-Sleep -Seconds 8
$offer = Join-Path $configDir 'executor-offer.json'
$errorFile = Join-Path $configDir 'error.json'
[ordered]@{
    host = $hostFingerprint; endpoint = "https://${ListenIP}:$Port"; operator = $Operator; service = $ServiceAccount
    config = $config; policy = $policy; task = (Get-ScheduledTask -TaskName $task).State.ToString()
    offer = (Test-Path $offer); error = if (Test-Path $errorFile) { (Get-Content $errorFile -Raw | ConvertFrom-Json).error } else { $null }
} | ConvertTo-Json
} finally {
    if ($Log) { Stop-Transcript | Out-Null }
}
