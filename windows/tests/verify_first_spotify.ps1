[CmdletBinding()]
param([Parameter(Mandatory)][string]$Setup,[Parameter(Mandatory)][string]$Version)
$ErrorActionPreference='Stop'
if ($env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or $env:RUNNER_OS -ne 'Windows') { throw 'A disposable Windows runner is required.' }
$name='LumSpotifyQA'
$scratch=Join-Path $env:ProgramData ('Lummix-Spotify-QA-'+[guid]::NewGuid().ToString('N'))
$password=ConvertTo-SecureString ('Lt9!'+[guid]::NewGuid().ToString('N')) -AsPlainText -Force
$user=New-LocalUser -Name $name -Password $password
try {
    $users=Get-LocalGroup -SID 'S-1-5-32-545'
    if (!(Get-LocalGroupMember -Group $users | Where-Object SID -eq $user.SID)) { Add-LocalGroupMember -Group $users -Member $user }
    $scratch=New-Item -ItemType Directory -Path $scratch
    & icacls.exe $scratch.FullName /grant ('*'+$user.SID.Value+':(OI)(CI)M') | Out-Null
    if ($LASTEXITCODE) { throw 'Could not prepare the standard user test directory.' }
    Copy-Item -LiteralPath $Setup -Destination (Join-Path $scratch 'Lummix.Setup.exe')
    foreach ($test in 'verify_sign_in.py','verify_first_spotify.py') { Copy-Item -LiteralPath (Join-Path $PSScriptRoot $test) -Destination $scratch }
    Start-Service seclogon
    $credentials=[pscredential]::new(($env:COMPUTERNAME+'\'+$name),$password)
    $python=(Get-Command python.exe -ErrorAction Stop).Source
    $proc=Start-Process -FilePath $python -Credential $credentials -LoadUserProfile -WorkingDirectory $scratch -ArgumentList @('-B','verify_first_spotify.py','.',$Version) -WindowStyle Hidden -PassThru
    $finished=$proc.WaitForExit(320000)
    $log=Join-Path $scratch 'first-spotify.log'
    if (Test-Path -LiteralPath $log) { Get-Content -LiteralPath $log -Tail 25 }
    if (!$finished -or $proc.ExitCode -ne 0) { throw 'The clean standard-user Spotify installation did not pass.' }
} finally {
    Remove-LocalUser -SID $user.SID
}
