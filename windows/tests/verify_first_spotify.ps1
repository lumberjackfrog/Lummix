[CmdletBinding()]
param([Parameter(Mandatory)][string]$Setup,[Parameter(Mandatory)][string]$Version)
$ErrorActionPreference='Stop'
if ($env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or $env:RUNNER_OS -ne 'Windows') { throw 'A disposable Windows runner is required.' }
$name='LumSpotifyQA'
$scratch=Join-Path $env:ProgramData ('Lummix-Spotify-QA-'+[guid]::NewGuid().ToString('N'))
$password=ConvertTo-SecureString ('Lt9!'+[guid]::NewGuid().ToString('N')) -AsPlainText -Force
$user=New-LocalUser -Name $name -Password $password
$product=$null
try {
    $users=Get-LocalGroup -SID 'S-1-5-32-545'
    if (!(Get-LocalGroupMember -Group $users | Where-Object SID -eq $user.SID)) { Add-LocalGroupMember -Group $users -Member $user }
    $scratch=New-Item -ItemType Directory -Path $scratch
    & icacls.exe $scratch.FullName /grant ('*'+$user.SID.Value+':(OI)(CI)M') | Out-Null
    if ($LASTEXITCODE) { throw 'Could not prepare the standard user test directory.' }
    # The hosted server restricts unmanaged MSI installs for standard users. Its runner
    # installs Lummix normally. The separate account runs the exact installed files read-only.
    $installed=Start-Process -FilePath $Setup -ArgumentList '/S' -WindowStyle Hidden -Wait -PassThru
    if ($installed.ExitCode -ne 0) { throw "Lummix Setup returned $($installed.ExitCode)." }
    $cache=@(Get-ChildItem -LiteralPath (Join-Path $env:LOCALAPPDATA 'Lummix/Installer') -Filter Lummix.msi -Recurse)
    if ($cache.Count -ne 1 -or $cache[0].Directory.Name -notmatch '^\{[\da-fA-F-]{36}\}$') { throw 'Cannot identify the QA installation.' }
    $product=$cache[0].Directory.Name
    $program=Get-Item -LiteralPath (Join-Path $env:LOCALAPPDATA "Programs/Lummix/$Version/Lummix.exe")
    & icacls.exe $program.Directory.FullName /grant ('*'+$user.SID.Value+':(OI)(CI)RX') /T /Q | Out-Null
    if ($LASTEXITCODE) { throw 'Could not grant the test account read access to the installed program.' }
    foreach ($test in 'verify_sign_in.py','verify_first_spotify.py') { Copy-Item -LiteralPath (Join-Path $PSScriptRoot $test) -Destination $scratch }
    Start-Service seclogon
    $credentials=[pscredential]::new(($env:COMPUTERNAME+'\'+$name),$password)
    $python=(Get-Command python.exe -ErrorAction Stop).Source
    $proc=Start-Process -FilePath $python -Credential $credentials -LoadUserProfile -WorkingDirectory $scratch -ArgumentList @('-B','verify_first_spotify.py','.',('"'+$program.FullName+'"')) -WindowStyle Hidden -PassThru
    $finished=$proc.WaitForExit(320000)
    $log=Join-Path $scratch 'first-spotify.log'
    if (Test-Path -LiteralPath $log) { Get-Content -LiteralPath $log -Tail 25 }
    if (!$finished -or $proc.ExitCode -ne 0) { throw 'The clean standard-user Spotify installation did not pass.' }
} finally {
    if ($product) {
        $removed=Start-Process msiexec.exe -ArgumentList @('/x',$product,'/qn','/norestart') -WindowStyle Hidden -Wait -PassThru
        if ($removed.ExitCode -ne 0) { Write-Warning "QA uninstall returned $($removed.ExitCode)." }
    }
    Remove-LocalUser -SID $user.SID
}
