[CmdletBinding()]
param([Parameter(Mandatory)][string]$Setup, [Parameter(Mandatory)][string]$Version)
$ErrorActionPreference = 'Stop'
if ($Version -notmatch '^(0|[1-9][0-9]{0,3})(\.(0|[1-9][0-9]{0,3})){2}$') {throw 'Invalid release version.'}
$file = Get-Item -LiteralPath $Setup
if ($file.VersionInfo.FileDescription -ne 'Lummix Setup' -or $file.VersionInfo.FileVersion -ne $Version) {throw 'Installer metadata does not match the requested Lummix version.'}
if ($file.Length -le 0 -or $file.Length -gt 128MB) {throw 'Installer size is outside the update limit.'}
$source = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot '../windows/release_identity.py')
$expected = [regex]::Match($source,'(?m)^PUBLIC_KEY = "([A-Za-z0-9+/=]+)"').Groups[1].Value
$key = [Security.Cryptography.CngKey]::Open('Lummix GitHub release manifests v1',[Security.Cryptography.CngProvider]::MicrosoftSoftwareKeyStorageProvider)
try {
    $public = [Convert]::ToBase64String($key.Export([Security.Cryptography.CngKeyBlobFormat]::EccPublicBlob))
    if ($public -cne $expected) {throw 'The Windows key does not match the key trusted by Lummix.'}
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $file.FullName).Hash.ToLowerInvariant()
    $text = "Lummix Windows x64 update v1`nv$Version`n$($file.Length)`n$hash`n"
    $signer = [Security.Cryptography.ECDsaCng]::new($key)
    try { $signature = $signer.SignData([Text.Encoding]::ASCII.GetBytes($text),[Security.Cryptography.HashAlgorithmName]::SHA256) }
    finally { $signer.Dispose() }
    $manifest = [ordered]@{format=1;tag="v$Version";size=$file.Length;sha256=$hash;signature=[Convert]::ToBase64String($signature)}
    $target = Join-Path $file.DirectoryName 'Lummix.Update.json'
    [IO.File]::WriteAllText($target,($manifest | ConvertTo-Json),[Text.UTF8Encoding]::new($false))
    Get-Item -LiteralPath $target
} finally { $key.Dispose() }
