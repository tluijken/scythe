# Windows' mobile hotspot for Scythe (src/remote.rs runs this hidden, with the variables below):
# it starts the hotspot as SCYTHE_HOTSPOT_SSID with SCYTHE_HOTSPOT_PASSWORD, starts it again when
# Windows turns it off (it does so after minutes without a device), and turns it off once the
# Scythe of SCYTHE_PID has quit. Offline, a saved Wi-Fi network stands in for the connection
# Windows wants to share. Exit codes: 0 when Scythe quit, 2 when the hotspot did not start.
$ErrorActionPreference = 'Stop'
$ssid = $env:SCYTHE_HOTSPOT_SSID
$passphrase = $env:SCYTHE_HOTSPOT_PASSWORD
$scythe = [int]$env:SCYTHE_PID

Add-Type -AssemblyName System.Runtime.WindowsRuntime
$null = [Windows.Networking.Connectivity.NetworkInformation, Windows.Networking.Connectivity, ContentType = WindowsRuntime]
$null = [Windows.Networking.NetworkOperators.NetworkOperatorTetheringManager, Windows.Networking.NetworkOperators, ContentType = WindowsRuntime]
$resultType = [Windows.Networking.NetworkOperators.NetworkOperatorTetheringOperationResult, Windows.Networking.NetworkOperators, ContentType = WindowsRuntime]

$asTask = [System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object { $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 }
$operation = $asTask | Where-Object { $_.IsGenericMethod -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' }
$action = $asTask | Where-Object { -not $_.IsGenericMethod -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncAction' }
function Wait-Result($op) { $t = $operation.MakeGenericMethod($resultType).Invoke($null, @($op)); $t.Wait() | Out-Null; $t.Result }
function Wait-Done($op) { $action.Invoke($null, @($op)).Wait() | Out-Null }

function Start-Hotspot {
    $info = [Windows.Networking.Connectivity.NetworkInformation]
    $profiles = @($info::GetInternetConnectionProfile()) + @($info::GetConnectionProfiles() | Where-Object { $_.IsWlanConnectionProfile }) |
        Where-Object { $_ }
    foreach ($profile in $profiles) {
        try {
            $manager = [Windows.Networking.NetworkOperators.NetworkOperatorTetheringManager]::CreateFromConnectionProfile($profile)
            $config = $manager.GetCurrentAccessPointConfiguration()
            if ($config.Ssid -ne $ssid -or $config.Passphrase -ne $passphrase) {
                if ("$($manager.TetheringOperationalState)" -eq 'On') { Wait-Result $manager.StopTetheringAsync() | Out-Null }
                $config.Ssid = $ssid
                $config.Passphrase = $passphrase
                Wait-Done $manager.ConfigureAccessPointAsync($config)
            }
            if ("$($manager.TetheringOperationalState)" -eq 'On') { return $manager }
            if ("$((Wait-Result $manager.StartTetheringAsync()).Status)" -eq 'Success') { return $manager }
        } catch { }
    }
    $null
}

$manager = Start-Hotspot
if (-not $manager) { exit 2 }
while (Get-Process -Id $scythe -ErrorAction SilentlyContinue) {
    Start-Sleep -Seconds 15
    if ("$($manager.TetheringOperationalState)" -ne 'On') {
        $again = Start-Hotspot
        if ($again) { $manager = $again }
    }
}
Wait-Result $manager.StopTetheringAsync() | Out-Null
exit 0
