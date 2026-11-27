<#
End-to-end check of multi-person mode on one Windows host, with synthetic
people and payloads only. Run elevated: it installs the shared executor with
services/artifact-executor/install-shared-executor.ps1, then

  1. creates two synthetic people (a person key and a device identity each),
     signs a device request for each and admits both into one project;
  2. runs one job for each person at the same time through controller-run
     against the shared executor, each published from the person's device;
  3. checks that both jobs passed inside their containers, that each person's
     capabilities name only that person, that neither can read the other's
     job, and that a container cannot read the operator policy;
  4. uninstalls again unless -Keep is given.

Writes shared-e2e.json beside -Work and exits nonzero on any failure.
#>
param(
    [Parameter(Mandatory = $true)][string]$Package,
    [string]$Work = (Join-Path ([IO.Path]::GetTempPath()) ('zerus-shared-e2e-' + [guid]::NewGuid().ToString('N').Substring(0, 8))),
    [string]$ListenIP = '127.0.0.1',
    [int]$Port = 7443,
    [switch]$Keep
)
$ErrorActionPreference = 'Stop'
$Package = (Resolve-Path -LiteralPath $Package).Path
$installer = Join-Path $PSScriptRoot '..\..\services\artifact-executor\install-shared-executor.ps1'
$installDir = "$env:ProgramFiles\Zerus Executor"
$operatorDir = "$env:ProgramData\ZerusOperator"
$configDir = "$env:ProgramData\ZerusExecutor\config"
$exe = Join-Path $installDir 'hgs-executor.exe'
$project = 'proj-e2e'
$checks = [ordered]@{}
$failed = $false

function Write-Json([string]$Path, $Value) {
    $Value | ConvertTo-Json -Depth 8 | Out-File -LiteralPath $Path -Encoding utf8NoBOM
}
function Invoke-Executor([string]$Command, [string]$Config) {
    $text = & $exe $Command $Config 2>&1 | Out-String
    [pscustomobject]@{ code = $LASTEXITCODE; text = $text.Trim() }
}
function Test-Check([string]$Name, [bool]$Ok, [string]$Detail = '') {
    $script:checks[$Name] = [ordered]@{ ok = $Ok; detail = $Detail }
    if (-not $Ok) { $script:failed = $true }
    Write-Output ("{0} {1} {2}" -f ($(if ($Ok) { 'PASS' } else { 'FAIL' })), $Name, $Detail)
}
function New-Seed {
    $bytes = New-Object byte[] 32
    [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
    [Convert]::ToBase64String($bytes).TrimEnd('=').Replace('+', '-').Replace('/', '_')
}
function New-JobId { [guid]::NewGuid().ToString('N') }

New-Item -ItemType Directory -Force -Path $Work | Out-Null
try {
    # Installation
    & wevtutil sl Microsoft-Windows-TaskScheduler/Operational /e:true 2>$null
    $installed = & $installer -Package $Package -ListenIP $ListenIP -Port $Port -Log (Join-Path $Work 'install.log') | Out-String | ConvertFrom-Json
    $offer = Join-Path $configDir 'executor-offer.json'
    $errorFile = Join-Path $configDir 'error.json'
    $deadline = (Get-Date).AddSeconds(90)
    while (-not (Test-Path $offer) -and -not (Test-Path $errorFile) -and (Get-Date) -lt $deadline) { Start-Sleep -Seconds 2 }
    $taskInfo = Get-ScheduledTaskInfo -TaskName 'Zerus Shared Executor'
    $serviceError = if (Test-Path $errorFile) { Get-Content $errorFile -Raw } else { '' }
    Test-Check 'service-started' ((Test-Path $offer) -and -not $serviceError) ("task result $($taskInfo.LastTaskResult) $serviceError")
    if ($failed) {
        $events = Get-WinEvent -LogName 'Microsoft-Windows-TaskScheduler/Operational' -MaxEvents 30 -ErrorAction SilentlyContinue |
            Where-Object { $_.Message -like '*Zerus Shared Executor*' } | ForEach-Object { "$($_.Id) $($_.Message)" }
        $logons = Get-WinEvent -FilterHashtable @{ LogName = 'Security'; Id = 4625 } -MaxEvents 5 -ErrorAction SilentlyContinue | ForEach-Object { $_.Message -replace '\s+', ' ' }
        $state = (Get-ScheduledTask -TaskName 'Zerus Shared Executor').State
        Write-Output "task state $state"
        $events | ForEach-Object { Write-Output "task event $_" }
        $logons | ForEach-Object { Write-Output "logon failure $_" }
        Get-ChildItem $configDir -ErrorAction SilentlyContinue | ForEach-Object { Write-Output "config file $($_.Name)" }
        throw 'the shared executor did not start'
    }
    $hostId = $installed.host
    $endpoint = $installed.endpoint
    $policy = $installed.policy

    # Two synthetic people, admitted into one project
    $people = [ordered]@{}
    $revision = 1
    foreach ($name in 'alice', 'bob') {
        $dir = Join-Path $Work $name
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        '{}' | Out-File -LiteralPath (Join-Path $dir 'init.json') -Encoding ascii
        $r = Invoke-Executor 'identity-init' (Join-Path $dir 'init.json')
        if ($r.code -ne 0) { throw "identity-init for ${name}: $($r.text)" }
        New-Seed | Out-File -LiteralPath (Join-Path $dir 'person.key') -Encoding ascii -NoNewline
        Write-Json (Join-Path $dir 'request-config.json') ([ordered]@{
                person_seed_file = (Join-Path $dir 'person.key'); host = $hostId
                device_certificate = (Join-Path $dir 'identity\identity.crt'); label = "Synthetic $name"; lifetime_days = 7
                output = (Join-Path $dir 'request.json') })
        $r = Invoke-Executor 'access-request-device' (Join-Path $dir 'request-config.json')
        Test-Check "request-$name" ($r.code -eq 0) $r.text
        $next = Join-Path $operatorDir ("access-policy-next-{0}.json" -f ($revision + 1))
        Write-Json (Join-Path $dir 'admit-config.json') ([ordered]@{
                policy_in = $policy; policy_out = $next; request = (Join-Path $dir 'request.json')
                ledger = (Join-Path $operatorDir 'enrollment-ledger.json')
                grants = @([ordered]@{ project = $project; role = 'contributor' })
                context = [ordered]@{ context = 'builds'; backend = 'windows-appcontainer-v1' } })
        $r = Invoke-Executor 'access-admit-device' (Join-Path $dir 'admit-config.json')
        Test-Check "admit-$name" ($r.code -eq 0) $r.text
        Move-Item -LiteralPath $next -Destination $policy -Force
        $revision++
        $people[$name] = $dir
    }
    Write-Json (Join-Path $Work 'show-config.json') @{ policy = $policy }
    $r = Invoke-Executor 'access-show' (Join-Path $Work 'show-config.json')
    Test-Check 'policy-shows-both' ($r.code -eq 0 -and $r.text -match 'Synthetic alice' -and $r.text -match 'Synthetic bob') $r.text

    # One job per person, at the same time
    $jobs = [ordered]@{}
    $processes = @()
    foreach ($name in $people.Keys) {
        $dir = $people[$name]
        $artifact = Join-Path $dir 'artifact'; $scenario = Join-Path $dir 'scenario'; $controller = Join-Path $dir 'controller'
        New-Item -ItemType Directory -Force -Path $artifact, $scenario, $controller | Out-Null
        $payload = New-Object byte[] (3MB)
        [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($payload)
        [IO.File]::WriteAllBytes((Join-Path $artifact 'payload.bin'), $payload)
        "synthetic build of $name" | Out-File -LiteralPath (Join-Path $artifact 'owner.txt') -Encoding ascii
        Write-Json (Join-Path $scenario 'scenario.json') ([ordered]@{
                protocol = 1; instructions = 'Synthetic multi-person check.'; runtime = 'powershell'; entry = 'run.ps1'
                arguments = @(); timeout_seconds = 120; memory_bytes = 268435456; guided = $false })
        @(
            '$ErrorActionPreference = ''Stop'''
            '$owner = (Get-Content -LiteralPath (Join-Path $env:ZERUS_ARTIFACT_ROOT ''owner.txt'') -Raw).Trim()'
            'try { [void][IO.File]::ReadAllBytes(''' + $policy + '''); $policyReadable = $true } catch { $policyReadable = $false }'
            "`$otherReadable = `$false; try { [void](Get-ChildItem -LiteralPath '$Work' -ErrorAction Stop); `$otherReadable = `$true } catch { }"
            '@{ owner = $owner; policy_readable = $policyReadable; other_readable = $otherReadable } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $env:ZERUS_RESULT_DIR ''result.json'')'
            'if ($policyReadable -or $otherReadable) { exit 3 }'
            'exit 0'
        ) | Out-File -LiteralPath (Join-Path $scenario 'run.ps1') -Encoding utf8NoBOM
        $job = New-JobId
        $jobs[$name] = $job
        Write-Json (Join-Path $controller 'controller.json') ([ordered]@{
                endpoint = $endpoint; peer = [ordered]@{ identity = (Join-Path $dir 'identity'); fingerprint = $hostId }
                job_id = $job; slot = 'e2e'; device_id = 'host'; artifact_root = $artifact; scenario_root = $scenario
                listen = "${ListenIP}:0"; average = 65536; memory_bytes = 536870912; timeout_seconds = 900
                parameters = [ordered]@{ case = 'shared-e2e' }; project = $project })
        $processes += Start-Process -FilePath $exe -ArgumentList 'controller-run', ('"' + (Join-Path $controller 'controller.json') + '"') -PassThru -NoNewWindow `
            -RedirectStandardOutput (Join-Path $controller 'stdout.txt') -RedirectStandardError (Join-Path $controller 'stderr.txt')
    }
    $processes | Wait-Process -Timeout 1200 -ErrorAction SilentlyContinue
    $i = 0
    foreach ($name in $people.Keys) {
        $p = $processes[$i]; $i++
        $controller = Join-Path $people[$name] 'controller'
        $stderr = (Get-Content (Join-Path $controller 'stderr.txt') -Raw -ErrorAction SilentlyContinue)
        $resultFile = Join-Path $controller 'controller-result.json'
        $status = if (Test-Path $resultFile) { (Get-Content $resultFile -Raw | ConvertFrom-Json).result.status } else { 'missing' }
        Test-Check "job-$name" ($p.HasExited -and $p.ExitCode -eq 0 -and $status -eq 'passed') ("exit $($p.ExitCode) status $status $stderr")
    }

    # Each person's capabilities name only that person
    $persons = @{}
    foreach ($name in $people.Keys) {
        $controller = Join-Path $people[$name] 'controller'
        $r = Invoke-Executor 'controller-capabilities' (Join-Path $controller 'controller.json')
        $caps = Get-Content (Join-Path $controller 'capabilities.json') -Raw -ErrorAction SilentlyContinue | ConvertFrom-Json
        $persons[$name] = $caps.access.person
        Test-Check "capabilities-$name" ($r.code -eq 0 -and $caps.access.mode -eq 'multi-person' -and $caps.access.person) ($r.text + ' ' + $caps.access.person)
    }
    Test-Check 'distinct-persons' ($persons['alice'] -and $persons['alice'] -ne $persons['bob']) ''

    # Neither person can read the other's job
    foreach ($pair in @(@('bob', 'alice'), @('alice', 'bob'))) {
        $reader, $owner = $pair
        $probe = Join-Path $people[$reader] ("probe-$owner")
        New-Item -ItemType Directory -Force -Path $probe | Out-Null
        $config = Get-Content (Join-Path $people[$owner] 'controller\controller.json') -Raw | ConvertFrom-Json
        $config.peer.identity = Join-Path $people[$reader] 'identity'
        Write-Json (Join-Path $probe 'controller.json') $config
        $r = Invoke-Executor 'controller-status' (Join-Path $probe 'controller.json')
        # The command prints a generic failure; the refusal itself is in error.json.
        $refusal = if (Test-Path (Join-Path $probe 'error.json')) { Get-Content (Join-Path $probe 'error.json') -Raw } else { '' }
        Test-Check "$reader-cannot-read-$owner" ($r.code -ne 0 -and "$($r.text) $refusal" -match '\(404\)' -and -not (Test-Path (Join-Path $probe 'controller-status.json'))) ($refusal.Trim() -replace '\s+', ' ')
    }
} catch {
    Test-Check 'run' $false ($_ | Out-String)
} finally {
    $report = Join-Path (Split-Path $Work) 'shared-e2e.json'
    Write-Json $report ([ordered]@{ ok = -not $failed; checks = $checks; work = $Work })
    if (-not $Keep) {
        & $installer -Uninstall | Out-Null
    }
}
if ($failed) { exit 1 }
exit 0
