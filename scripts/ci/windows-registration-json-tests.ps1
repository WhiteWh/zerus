param([string]$RegistrationSource)
$ErrorActionPreference='Stop'
. ([ScriptBlock]::Create([IO.File]::ReadAllText($RegistrationSource)))
function Assert([bool]$ok){if(-not $ok){throw 'JSON control assertion failed'}}
function Refuses([scriptblock]$body){$failed=$false;try{& $body|Out-Null}catch{$failed=$true};Assert $failed}
$codec=Get-RegistrationJson
$text=$codec.Serialize([ordered]@{schema=1;changed=$false;task_state=$null;error=('quote"'+[char]10+[char]0x20ac)})
$data=[Text.Encoding]::UTF8.GetBytes($text);$stream=[IO.MemoryStream]::new($data,$false)
try { $value=Read-RegistrationRequest $stream $data.Length } finally {$stream.Dispose()}
Assert ($value.schema -eq 1 -and $value.changed -eq $false -and $null -eq $value.task_state -and $value.error -ceq ('quote"'+[char]10+[char]0x20ac))
Refuses {$codec.DeserializeObject('{"__type":"System.IO.FileInfo, mscorlib"}')}
Refuses {$codec.DeserializeObject(('{'+'"x":')*33+'null'+('}'*33))}
Write-Output 'Framework JSON controls: 3 passed; explicit scheduler/registry/service mutations=0'
