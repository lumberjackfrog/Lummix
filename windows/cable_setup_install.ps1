# Invoked as the current user; ONLY the original signed vendor program may elevate.
param([Parameter(Mandatory=$true)][string]$Folder, [ValidateSet('Verify','Launch')][string]$Mode='Verify')
$ErrorActionPreference = 'Stop'
$exe = Join-Path $Folder 'VBCABLE_Setup_x64.exe'
$cat = Join-Path $Folder 'vbaudio_cable64_win10.cat'
$sig = Get-AuthenticodeSignature -LiteralPath $exe
$driver = Get-AuthenticodeSignature -LiteralPath $cat
if ((Get-FileHash -Algorithm SHA256 -LiteralPath $exe).Hash -ne '734C35DFA6D98F48782A451633CEB471166EC70D60482FD89A1123D0EE3C4F41' -or
    $sig.Status -ne 'Valid' -or $sig.SignerCertificate.Thumbprint -ne 'A77952D93229D0EC36E2543081EEA7D125732B9C' -or
    $driver.Status -ne 'Valid' -or $driver.SignerCertificate.GetNameInfo('SimpleName',$false) -ne 'Microsoft Windows Hardware Compatibility Publisher') {
    throw 'The original VB-Audio setup program or Microsoft driver catalog could not be authenticated.'
}
if ($Mode -eq 'Verify') { exit 0 }
try {
    $proc = Start-Process -FilePath $exe -WorkingDirectory $Folder -ArgumentList '-i','-h' -Verb RunAs -PassThru -Wait
} catch {
    $e=$_.Exception
    while ($e) {
        if ($e -is [System.ComponentModel.Win32Exception] -and $e.NativeErrorCode -eq 1223) { exit 1223 }
        if (($e.HResult -band 65535) -eq 1223) { exit 1223 }
        $e=$e.InnerException
    }
    throw
}
exit $proc.ExitCode
