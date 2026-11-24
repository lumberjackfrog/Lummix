[CmdletBinding()]
param(
    [string] $Msi,
    [string] $Output,
    [string] $MakeNsis
)
$ErrorActionPreference = 'Stop'
$project = Split-Path $PSScriptRoot -Parent
if (-not $Msi) { $Msi = Join-Path $project 'dist\Lummix Setup.msi' }
if (-not $Output) { $Output = Join-Path $project 'dist\Lummix Setup.exe' }
if (-not $MakeNsis) {
    $compiler = Get-Command makensis.exe -ErrorAction SilentlyContinue
    if ($compiler) { $MakeNsis = $compiler.Source }
    else { $MakeNsis = Join-Path ${env:ProgramFiles(x86)} 'NSIS\makensis.exe' }
}
if (-not (Test-Path -LiteralPath $MakeNsis -PathType Leaf)) {
    throw 'Install NSIS 3 from nsis.sourceforge.io or provide -MakeNsis with the path to makensis.exe.'
}
$payload = (Resolve-Path -LiteralPath $Msi).Path
$target = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Output)
if ([IO.Path]::GetExtension($target) -ne '.exe') { throw 'Output must be an .exe file.' }
$null = [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($target))
$engine = New-Object -ComObject WindowsInstaller.Installer
$database = $engine.OpenDatabase($payload, 0)
function Read-MsiProperty([string] $Name) {
    $view = $database.OpenView("SELECT Value FROM Property WHERE Property = '$Name'")
    $null = $view.Execute()
    try { return $view.Fetch().StringData(1) } finally { $null = $view.Close() }
}
if ((Read-MsiProperty 'ProductName') -ne 'Lummix') { throw 'Expected the Lummix application MSI.' }
$code = Read-MsiProperty 'ProductCode'
$version = Read-MsiProperty 'ProductVersion'
if ($code -notmatch '^\{[0-9a-fA-F-]{36}\}$' -or $version -notmatch '^\d+\.\d+\.\d+$') { throw 'Invalid MSI product metadata.' }
$scriptFile = Join-Path $project 'setup\Lummix.nsi'
& $MakeNsis /INPUTCHARSET UTF8 /V2 "/DSETUP_EXE=$target" "/DPAYLOAD_MSI=$payload" "/DPRODUCT_CODE=$code" "/DVERSION=$version" $scriptFile
if ($LASTEXITCODE -ne 0) { throw "Setup compiler exited with $LASTEXITCODE." }
Get-Item -LiteralPath $target
