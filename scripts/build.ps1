[CmdletBinding()]
param([string] $Python, [string] $MakeNsis, [switch] $MsiOnly,
      [ValidatePattern('^[0-9A-Fa-f]{40}$')][string] $SigningThumbprint,
      [uri] $TimestampUrl)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$windows = Join-Path $root 'windows'
if ($SigningThumbprint) {
    if (-not $TimestampUrl -or $TimestampUrl.Scheme -ne 'https') {
        throw 'Supply the HTTPS RFC 3161 timestamp URL for your release certificate.'
    }
    $signTool = (Get-Command signtool.exe -ErrorAction SilentlyContinue).Source
    if (-not $signTool) {
        $kit = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
        $signTool = Get-ChildItem -Path "$kit\*\x64\signtool.exe" -File |
            Sort-Object { [version] $_.Directory.Parent.Name } -Descending |
            Select-Object -First 1 -ExpandProperty FullName
    }
    if (-not $signTool) { throw 'Install Windows SDK Signing Tools or put signtool.exe on PATH.' }
}
function Sign-ReleaseFile([string] $File) {
    if (-not $SigningThumbprint) { return }
    & $signTool sign /s My /sha1 $SigningThumbprint /fd SHA256 /tr $TimestampUrl.AbsoluteUri /td SHA256 $File
    if ($LASTEXITCODE -ne 0) { throw "Signing failed for $File" }
    & $signTool verify /pa $File
    if ($LASTEXITCODE -ne 0) { throw "The Windows trust check failed for $File" }
}
if (-not $Python) {
    $Python = Join-Path $root '.venv\Scripts\python.exe'
    if (-not (Test-Path -LiteralPath $Python)) {
        & py -3.12 -m venv (Join-Path $root '.venv')
        if ($LASTEXITCODE -ne 0) { throw 'Install CPython 3.12 x64 and its Windows py launcher.' }
    }
}
& $Python -c "import sys, struct; assert sys.version_info[:2] == (3,12) and sys.prefix != sys.base_prefix and struct.calcsize('P') == 8, 'Use a CPython 3.12 x64 virtual environment.'"
if ($LASTEXITCODE -ne 0) { throw 'Unsupported Python environment.' }
& $Python -m pip --disable-pip-version-check install --only-binary=:all: -r (Join-Path $windows 'requirements-build.txt')
if ($LASTEXITCODE -ne 0) { throw 'Pinned package restore failed.' }
& $Python (Join-Path $windows 'runtime\build_runtime.py')
if ($LASTEXITCODE -ne 0) { throw 'Runtime build failed.' }
Sign-ReleaseFile (Join-Path $root '.build\frozen\runtime\Lummix Helper.exe')
Sign-ReleaseFile (Join-Path $root '.build\frozen\runtime\_internal\lummix_dsp.dll')
& $Python (Join-Path $windows 'packaging\package.py')
if ($LASTEXITCODE -ne 0) { throw 'Runtime archive packaging failed.' }
$dist = Join-Path $root 'dist'
& $Python (Join-Path $windows 'packaging\installer.py') (Join-Path $dist 'Lummix for Windows.zip') (Join-Path $dist 'Lummix Setup.msi')
if ($LASTEXITCODE -ne 0) { throw 'Windows Installer packaging failed.' }
Sign-ReleaseFile (Join-Path $dist 'Lummix Setup.msi')
if (-not $MsiOnly) {
    & (Join-Path $PSScriptRoot 'build-setup.ps1') -MakeNsis $MakeNsis
    Sign-ReleaseFile (Join-Path $dist 'Lummix Setup.exe')
}
