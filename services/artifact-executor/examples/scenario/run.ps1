$ErrorActionPreference = 'Stop'
$parameters = Get-Content -LiteralPath $env:ZERUS_PARAMETERS_FILE -Raw | ConvertFrom-Json
# Replace this synthetic check with the actual packaged test or SDK invocation.
# The artifact root is a managed verified build; write only into RESULT_DIR.
if (-not (Test-Path -LiteralPath $env:ZERUS_ARTIFACT_ROOT -PathType Container)) { exit 1 }
@{ scenario = 'synthetic-smoke'; passed = $true } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $env:ZERUS_RESULT_DIR 'result.json')
exit 0
