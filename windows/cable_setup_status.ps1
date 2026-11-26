$ErrorActionPreference = 'Stop'
# Collect Windows evidence only. The native host qualifies driver and endpoint identity.
$drivers = @(Get-CimInstance Win32_PnPSignedDriver -Filter "DeviceName='VB-Audio Virtual Cable'" | ForEach-Object {
    @{ name=[string]$_.DeviceName; provider=[string]$_.DriverProviderName; hardware=[string]$_.HardWareID
       signed=[bool]$_.IsSigned; signer=[string]$_.Signer; inf=[string]$_.InfName }
})
# The recorded boot event is stable even when the clock is corrected later.
try {
    $boot = Get-WinEvent -FilterHashtable @{LogName='System';ProviderName='Microsoft-Windows-Kernel-General';Id=12} -MaxEvents 1 -ErrorAction Stop
} catch {
    $boot = $null
}
$bootId = if ($boot) { "$($boot.RecordId):$($boot.TimeCreated.ToUniversalTime().Ticks)" } else { $null }
try {
    $os = Get-CimInstance Win32_OperatingSystem -Property LastBootUpTime -ErrorAction Stop
} catch {
    $os = $null
}
$bootTicks = if ($os.LastBootUpTime) { $os.LastBootUpTime.ToUniversalTime().Ticks } else { $null }
[pscustomobject]@{ drivers=$drivers; boot=@{ event=$bootId; ticks=$bootTicks } } | ConvertTo-Json -Compress -Depth 6
