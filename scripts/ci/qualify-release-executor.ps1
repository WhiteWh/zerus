# Qualify the exact fork release executor (.github/workflows/fork-windows.yml):
# appcontainer-qualify as a throwaway non-administrator account on the
# disposable free runner, synthetic data only. On success the all-pass record
# is in artifacts/test-results for scripts/add-qualification-record.py.
$ErrorActionPreference = 'Stop'
Expand-Archive -LiteralPath 'artifacts/hgs-executor-windows-x86_64.zip' -DestinationPath "$env:RUNNER_TEMP\aq-pkg"
$exe = "$env:RUNNER_TEMP\aq-pkg\hgs-executor-windows-x86_64\hgs-executor.exe"
$binary = (Get-FileHash -Algorithm SHA256 -LiteralPath $exe).Hash.ToLower()
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$msbuild = & $vswhere -latest -requires Microsoft.Component.MSBuild -find 'MSBuild\**\Bin\MSBuild.exe' | Select-Object -First 1
$vsPath = & $vswhere -latest -property installationPath
$vsVersion = & $vswhere -latest -property installationVersion
if (-not $msbuild -or -not $vsPath -or -not $vsVersion) { throw 'Visual Studio tools unavailable' }
$cmake = (Get-Command cmake -ErrorAction Stop).Source
$ip = (Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -notlike '127.*' -and $_.PrefixOrigin -ne 'WellKnown' } | Select-Object -First 1).IPAddress
if (-not $ip) { throw 'no non-loopback IPv4 address for the network case' }
$vcvars = Join-Path $vsPath 'VC\Auxiliary\Build\vcvars64.bat'
$toolchain = @{}
foreach ($l in (& cmd.exe /d /c "call `"$vcvars`" >nul && set")) {
  $k, $v = $l -split '=', 2
  if ($k -in @('INCLUDE', 'LIB', 'LIBPATH', 'PATH')) { $toolchain[$k] = $v }
}
if (-not $toolchain.INCLUDE) { throw 'vcvars64 produced no INCLUDE' }
$bytes = New-Object byte[] 24
[System.Security.Cryptography.RandomNumberGenerator]::Fill($bytes)
$password = [Convert]::ToBase64String($bytes) + 'a1!'
echo "::add-mask::$password"
net user zerus-qsvc $password /add /y | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'account creation failed' }
if ((Get-LocalGroupMember -SID 'S-1-5-32-544').Name -match 'zerus-qsvc') { throw 'service account must not be an administrator' }
New-Item -ItemType Directory -Force -Path 'C:\ZerusQ\root' | Out-Null
icacls 'C:\ZerusQ' /grant 'zerus-qsvc:(OI)(CI)F' | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'folder grant failed' }
$record = 'C:\ZerusQ\appcontainer-qualification.json'
$config = 'C:\ZerusQ\qualify.json'
@{ root = 'C:\ZerusQ\root'; record = $record; listen_ip = $ip; cmake = $cmake; msbuild = $msbuild; vs_instance = "$vsPath,version=$vsVersion"; toolchain = $toolchain } |
  ConvertTo-Json | Out-File -FilePath $config -Encoding utf8NoBOM
# Operator step, as on a real host: the jobs capability may list the volume root.
$sid = (& $exe appcontainer-capability $config | Select-Object -First 1).Trim()
if ($sid -notlike 'S-1-15-3-*') { throw "no jobs capability: $sid" }
icacls 'C:\' /grant "*${sid}:(RD,X,RA)" | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'volume root grant failed' }
$credential = New-Object System.Management.Automation.PSCredential('zerus-qsvc', (ConvertTo-SecureString $password -AsPlainText -Force))
$line = "set ZERUS_APPCONTAINER_QUALIFICATION=1&& set GITHUB_ACTIONS=true&& set RUNNER_ENVIRONMENT=github-hosted&& set ImageOS=$env:ImageOS&& set ImageVersion=$env:ImageVersion&& `"$exe`" appcontainer-qualify `"$config`" > C:\ZerusQ\qualify.log 2>&1"
$p = Start-Process -FilePath "$env:SystemRoot\System32\cmd.exe" -ArgumentList '/d', '/c', $line -Credential $credential -LoadUserProfile -WorkingDirectory 'C:\ZerusQ' -Wait -PassThru
Get-Content 'C:\ZerusQ\qualify.log' -ErrorAction SilentlyContinue | Select-Object -Last 40
if (-not (Test-Path -LiteralPath $record)) { throw 'no qualification record' }
$r = Get-Content $record | ConvertFrom-Json
$r.cases | ForEach-Object { "$($_.case) $($_.verdict)" }
if ($r.binary_sha256 -ne $binary) { throw 'record names another binary' }
if ($p.ExitCode -ne 0) { throw "qualification failed or incomplete (exit $($p.ExitCode))" }
Copy-Item -LiteralPath $record -Destination 'artifacts/test-results/appcontainer-qualification.json'
Copy-Item -LiteralPath 'C:\ZerusQ\qualify.log' -Destination 'artifacts/test-results/appcontainer-qualify.log' -ErrorAction SilentlyContinue
