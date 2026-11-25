$ErrorActionPreference = 'Stop'
$drivers = @(Get-CimInstance Win32_PnPSignedDriver -Filter "DeviceName='VB-Audio Virtual Cable'" |
    Where-Object { $_.DriverProviderName -eq 'VB-Audio Software' -and $_.HardWareID -eq 'VBAudioVACWDM' })
$signed = @($drivers | Where-Object { $_.IsSigned -and $_.Signer -eq 'Microsoft Windows Hardware Compatibility Publisher' })
$seen = $false
$endpointIds = @{ Render = @(); Capture = @() }
$captureName = $null
$captureShort = $null
$ports = @{ Render = $false; Capture = $false }
$base = 'SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio'
foreach ($flow in 'Render','Capture') {
    $bank = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey("$base\$flow")
    if (-not $bank) { throw "Windows endpoint inventory unavailable" }
    foreach ($key in $bank.GetSubKeyNames()) {
        $device = $bank.OpenSubKey($key)
        $props = $device.OpenSubKey('Properties')
        if ($props) {
            $hw = $props.GetValue('{a8b865dd-2e3d-4094-ad97-e593a70c75d6},8')
            $inf = $props.GetValue('{a8b865dd-2e3d-4094-ad97-e593a70c75d6},5')
            $desc = $props.GetValue('{b3f8fa53-0004-438e-9003-51a46e139bfc},6')
            if ($hw -eq 'VBAudioVACWDM' -and $desc -eq 'VB-Audio Virtual Cable' -and $device.GetValue('DeviceState') -eq 1) {
                $seen = $true
                if ($signed.InfName -contains $inf) {
                    $ports[$flow] = $true
                    $prefix = if ($flow -eq 'Render') { '{0.0.0.00000000}.' } else { '{0.0.1.00000000}.' }
                    $endpointIds[$flow] += $prefix + $key
                    if ($flow -eq 'Capture') {
                        $captureName = $props.GetValue('{a45c254e-df1c-4efd-8020-67d146a850e0},14')
                        $captureShort = $props.GetValue('{a45c254e-df1c-4efd-8020-67d146a850e0},2')
                    }
                }
            }
            $props.Close()
        }
        $device.Close()
    }
    $bank.Close()
}
$state = if ($ports.Render -and $ports.Capture) { 'ready' }
         elseif ($drivers.Count -gt 0 -or $seen) { 'partial' }
         else { 'missing' }
if ($endpointIds.Capture.Count -ne 1) { $captureName = $null; $captureShort = $null }
$captureAlias = if ($captureShort -ceq 'Lummix') { 'ready' } elseif ($captureName) { 'original' } else { 'unavailable' }
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
$bootInfo = @{ event=$bootId; ticks=$bootTicks }
[pscustomobject]@{ captureName=$captureName; captureAlias=$captureAlias; state=$state; render=[bool]$ports.Render; capture=[bool]$ports.Capture; signed=[bool]$signed.Count; endpointIds=$endpointIds; boot=$bootInfo } | ConvertTo-Json -Compress
