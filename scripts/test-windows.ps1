[CmdletBinding()]
param([string] $Python, [switch] $Frozen)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$windows = Join-Path $root 'windows'
if (-not $Python) { $Python = Join-Path $root '.venv\Scripts\python.exe' }
if (-not (Test-Path -LiteralPath $Python)) { throw 'Run scripts/build.ps1 first or supply -Python with the x64 CPython 3.12 venv interpreter.' }
& $Python -c "import sys; assert sys.prefix != sys.base_prefix, 'Pass a virtual environment interpreter.'"
if ($LASTEXITCODE -ne 0) { throw 'A virtual environment is required.' }
& $Python -m pip --disable-pip-version-check install --only-binary=:all: -r (Join-Path $windows 'requirements-test.txt')
if ($LASTEXITCODE -ne 0) { throw 'Test package restore failed.' }
& $Python (Join-Path $windows 'native_build.py') --test
if ($LASTEXITCODE -ne 0) { throw 'Native module build or tests failed.' }
$previous = $env:PYTHONPATH
$previousHost = $env:LUMMIX_HOST_DLL
try {
    $env:PYTHONPATH = "$windows;$windows\runtime;$windows\packaging"
    $env:LUMMIX_HOST_DLL = Join-Path $root '.build\host\release\lummix_host.dll'
    $tests = @('update_test.py','spotify_setup_test.py','profile_recovery_test.py','cable_setup_test.py','cable_gain_test.py','cable_alias_test.py','spotify_cable_guard_test.py',
               'mic_route_test.py','mix_duck_test.py','native_mix_test.py','media_native_test.py','spotify-test.py','mix_gains_test.py','spotify_volume_test.py')
    if ($Frozen) { $tests += 'test_runtime.py' }
    foreach ($test in $tests) {
        & $Python -B (Join-Path "$windows\tests" $test)
        if ($LASTEXITCODE -ne 0) { throw "$test failed." }
    }
} finally { $env:PYTHONPATH = $previous; $env:LUMMIX_HOST_DLL = $previousHost }
