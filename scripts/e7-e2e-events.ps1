# E7-7 test 6 (CI only, the updater-e2e job): what Windows recorded about the update's processes since -Since
# (an ISO 8601 time). Process creation and termination (Security 4688 and 4689, which the job turns on with
# auditpol, command lines included), the Application log, and Defender's operational log. Reads only.
param(
    [Parameter(Mandatory = $true)][string]$Since
)
$ErrorActionPreference = 'Continue'
$t0 = [DateTime]::Parse($Since, [Globalization.CultureInfo]::InvariantCulture,
    [Globalization.DateTimeStyles]::AdjustToUniversal -bor [Globalization.DateTimeStyles]::AssumeUniversal).ToLocalTime()
$pattern = 'updater|setup|installer|lpk|Au_|LitePack'
$keep = 'New Process ID|New Process Name|Process Command Line|Creator Process ID|Creator Process Name|Process ID|Process Name|Exit Status|Token Elevation Type'

"=== Security 4688/4689 since $Since (matching $pattern)"
Get-WinEvent -FilterHashtable @{ LogName = 'Security'; Id = 4688, 4689; StartTime = $t0 } -ErrorAction SilentlyContinue |
    Sort-Object TimeCreated |
    Where-Object { $_.Message -match $pattern } |
    ForEach-Object {
        $fields = ($_.Message -split "`r?`n" | ForEach-Object { $_.Trim() } | Where-Object { $_ -match "^($keep):" }) -join ' | '
        '{0:HH:mm:ss.fff} {1} {2}' -f $_.TimeCreated.ToUniversalTime(), $_.Id, $fields
    }

"=== Application log since $Since"
Get-WinEvent -FilterHashtable @{ LogName = 'Application'; StartTime = $t0 } -MaxEvents 200 -ErrorAction SilentlyContinue |
    Sort-Object TimeCreated |
    ForEach-Object {
        $msg = ($_.Message -replace "`r?`n", ' ')
        if ($msg.Length -gt 400) { $msg = $msg.Substring(0, 400) }
        '{0:HH:mm:ss.fff} {1} {2} {3}: {4}' -f $_.TimeCreated.ToUniversalTime(), $_.LevelDisplayName, $_.ProviderName, $_.Id, $msg
    }

"=== Defender operational log since $Since"
Get-WinEvent -FilterHashtable @{ LogName = 'Microsoft-Windows-Windows Defender/Operational'; StartTime = $t0 } -MaxEvents 50 -ErrorAction SilentlyContinue |
    Sort-Object TimeCreated |
    ForEach-Object {
        $msg = ($_.Message -replace "`r?`n", ' ')
        if ($msg.Length -gt 400) { $msg = $msg.Substring(0, 400) }
        '{0:HH:mm:ss.fff} {1} {2}: {3}' -f $_.TimeCreated.ToUniversalTime(), $_.LevelDisplayName, $_.Id, $msg
    }

"=== SmartScreen and AppLocker since $Since"
foreach ($log in 'Microsoft-Windows-SmartScreen/Debug', 'Microsoft-Windows-AppLocker/EXE and DLL') {
    Get-WinEvent -FilterHashtable @{ LogName = $log; StartTime = $t0 } -MaxEvents 20 -ErrorAction SilentlyContinue |
        ForEach-Object { '{0:HH:mm:ss.fff} {1} {2}: {3}' -f $_.TimeCreated.ToUniversalTime(), $log, $_.Id, ($_.Message -replace "`r?`n", ' ') }
}
"=== end"
