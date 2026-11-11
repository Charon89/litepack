# E7-7 test 6 (CI only, the updater-e2e job): watch the processes the update starts. Every second for
# -Seconds, list through Win32_Process anything running from the temporary folder, named like an installer, or
# LitePack's own executables; log each process when first seen (time, pid, parent, path, command line) and when it
# is gone. Never run on a developer's PC: it only reads, but it belongs to the job's diagnostics.
param(
    [Parameter(Mandatory = $true)][string]$Out,
    [int]$Seconds = 240
)
$ErrorActionPreference = 'Continue'
$temps = @([IO.Path]::GetTempPath().TrimEnd('\'), $env:TEMP, $env:TMP) | Where-Object { $_ } |
    ForEach-Object { (Resolve-Path -LiteralPath $_ -ErrorAction SilentlyContinue).ProviderPath } |
    Where-Object { $_ } | Select-Object -Unique
"watch: temp folders $($temps -join '; ')" | Out-File -FilePath $Out -Encoding utf8
$seen = @{}
$start = Get-Date
while (((Get-Date) - $start).TotalSeconds -lt $Seconds) {
    $now = Get-Date
    $t = '{0:HH:mm:ss.fff}' -f $now.ToUniversalTime()
    $procs = @(Get-CimInstance Win32_Process | Where-Object {
            $p = $_.ExecutablePath
            ($p -and ($temps | Where-Object { $p.StartsWith($_, [StringComparison]::OrdinalIgnoreCase) })) -or
            $_.Name -like '*setup*.exe' -or $_.Name -like '*installer*' -or $_.Name -like 'lpk*.exe' -or
            $_.Name -like 'Au_*.exe' -or $_.Name -like 'un*.exe'
        })
    $live = @{}
    foreach ($p in $procs) {
        $key = "$($p.ProcessId)/$($p.CreationDate.Ticks)"
        $live[$key] = $true
        if (-not $seen.ContainsKey($key)) {
            $seen[$key] = "$($p.ProcessId) $($p.Name)"
            $created = '{0:HH:mm:ss.fff}' -f $p.CreationDate.ToUniversalTime()
            "$t start pid $($p.ProcessId) parent $($p.ParentProcessId) created $created name $($p.Name) path $($p.ExecutablePath) cmd $($p.CommandLine)" |
                Out-File -FilePath $Out -Append -Encoding utf8
        }
    }
    foreach ($key in @($seen.Keys)) {
        if ($seen[$key] -and -not $live.ContainsKey($key)) {
            "$t gone pid $($seen[$key])" | Out-File -FilePath $Out -Append -Encoding utf8
            $seen[$key] = $null
        }
    }
    Start-Sleep -Milliseconds 500
}
"watch: done after $Seconds s" | Out-File -FilePath $Out -Append -Encoding utf8
