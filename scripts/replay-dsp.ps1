[CmdletBinding()]
param([Parameter(Mandatory)][string] $Python, [Parameter(Mandatory)][string] $Original)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$pythonExe = (Resolve-Path -LiteralPath $Python).Path
$env:LUMMIX_QA_ORIGINAL = (Resolve-Path -LiteralPath $Original).Path
$test = Join-Path $root 'windows/tests/native_mix_test.py'
$qa = New-Item -ItemType Directory -Force -Path (Join-Path $root '.build/qa')
$replay = New-Item -ItemType Directory -Force -Path (Join-Path $qa.FullName 'dsp-replay')
& $pythonExe -B $test
if ($LASTEXITCODE) { throw 'Audio output diverged from the pre-port source.' }
& $pythonExe -B $test benchmark (Join-Path $qa.FullName 'dsp-benchmark.json')
if ($LASTEXITCODE) { throw 'Callback benchmark failed.' }

$zip = Join-Path $qa.FullName 'rust-anything-v5.0.1.zip'
$asset = 'rust-anything-v5.0.1-x86_64-pc-windows-msvc.zip'
Invoke-WebRequest "https://github.com/erkamyaman/rust-anything/releases/download/v5.0.1/$asset" -OutFile $zip
if ((Get-FileHash -LiteralPath $zip).Hash -ne '74e55f10af699551dc4b30372ac22b75f3bea7d603afc1ce194cfa12e31e7593') {
    throw 'Unexpected rust-anything verification tool checksum.'
}
$toolsDir = Join-Path $qa.FullName 'rust-anything-tool'
Expand-Archive -LiteralPath $zip -DestinationPath $toolsDir -Force
$tool = @(Get-ChildItem -LiteralPath $toolsDir -Filter rust-anything.exe -Recurse -File)
if ($tool.Count -ne 1) { throw 'Expected one rust-anything executable.' }
$cases = @('impulse','normal','limiter','talkover','faders','nonfinite')
$inputs = Join-Path $qa.FullName 'dsp-cases.jsonl'
$cases | ForEach-Object { @{name=$_;args=@($_)} | ConvertTo-Json -Compress } | Set-Content -LiteralPath $inputs -Encoding utf8NoBOM
$toolExe = $tool[0].FullName
# Pass the exact argv to CreateProcess on Windows, including paths with spaces.
$runTool = 'import subprocess, sys; sys.exit(subprocess.run(sys.argv[1:]).returncode)'
& $pythonExe -c $runTool $toolExe app record $replay.FullName --inputs $inputs -- $pythonExe -B $test fingerprint numpy
if ($LASTEXITCODE) { throw 'Recording the original mixer failed.' }
$records = @(Get-ChildItem -Path (Join-Path $replay.FullName 'rust/app-cases/*.stdout') |
    ForEach-Object { (Get-Content -Raw -LiteralPath $_.FullName).Trim() })
foreach ($case in $cases) {
    $digest = & $pythonExe -B $test fingerprint numpy $case
    if ($LASTEXITCODE -or $digest -notmatch '^[a-f0-9]{64}$' -or $digest -notin $records) { throw "Missing recorded PCM for $case." }
}
$command = '"{0}" -B "{1}" fingerprint rust' -f $pythonExe.Replace('\','/'),$test.Replace('\','/')
& $pythonExe -c $runTool $toolExe app replay $replay.FullName --build $command
if ($LASTEXITCODE) { throw 'rust-anything found a replay difference.' }
