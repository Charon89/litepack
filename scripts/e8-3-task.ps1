<#
.SYNOPSIS
  Registers or removes the scheduled task of E8-3's weekly speed-regression run
  (docs/notes/e8-3-design.md).

.DESCRIPTION
  -Register creates (or replaces) the task \LitePack\E8-3 weekly speed for the current user:
    * runs only when the user is logged on (interactive logon, no stored password), RunLevel Limited;
    * two weekly triggers, -Day and -FallbackDay, both at -At (local time);
    * wakes the computer, ignores a new start while one runs, stops after 8 hours, runs at
      priority 4 (NORMAL_PRIORITY_CLASS, like every earlier measurement), has no idle condition
      (the launcher checks the time since the last input itself) and no catch-up start at logon;
      it is visible under \LitePack\ in Task Scheduler;
    * runs PowerShell 7 (pwsh.exe, found with Get-Command at registration time) on the main
      checkout's scripts/e8-3-launch.ps1, through the classic console host started directly:
        conhost.exe "<pwsh.exe>" -NoProfile -NonInteractive -WindowStyle Hidden -File <main>\scripts\e8-3-launch.ps1
      so that Windows Terminal, the default terminal on Windows 11, never takes the console and
      -WindowStyle Hidden hides the real window after a moment's flash (no -ExecutionPolicy:
      PowerShell 7's default allows local scripts).
  -Remove unregisters exactly that task and prints, without running it, the command that removes
  the weekly worktree.
  The script changes nothing else: no power plan, antivirus or other system setting.

.EXAMPLE
  pwsh -File scripts/e8-3-task.ps1 -Register
  pwsh -File scripts/e8-3-task.ps1 -Register -Day Sunday -At 03:00 -FallbackDay Monday
  pwsh -File scripts/e8-3-task.ps1 -Remove
#>
[CmdletBinding(DefaultParameterSetName = 'Register')]
param(
    [Parameter(Mandatory = $true, ParameterSetName = 'Register')]
    [switch] $Register,

    [Parameter(Mandatory = $true, ParameterSetName = 'Remove')]
    [switch] $Remove,

    [Parameter(ParameterSetName = 'Register')]
    [ValidateSet('Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday')]
    [string] $Day = 'Sunday',

    [Parameter(ParameterSetName = 'Register')]
    [ValidatePattern('^([01][0-9]|2[0-3]):[0-5][0-9]$')]
    [string] $At = '03:00',

    [Parameter(ParameterSetName = 'Register')]
    [ValidateSet('Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday')]
    [string] $FallbackDay = 'Monday'
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

$TaskPath = '\LitePack\'
$TaskName = 'E8-3 weekly speed'

# The main checkout, from this script's own location: git's common directory is <main>\.git,
# whichever checkout (the main one or a worktree) holds this copy of the script.
$git = (Get-Command git -CommandType Application | Select-Object -First 1).Source
$common = & $git -C $PSScriptRoot rev-parse --path-format=absolute --git-common-dir
if ($LASTEXITCODE -ne 0 -or -not $common) {
    throw "$PSScriptRoot is not inside a git checkout"
}
$MainRoot = Split-Path -Parent ([System.IO.Path]::GetFullPath($common.Trim()))
$Launcher = Join-Path $MainRoot 'scripts\e8-3-launch.ps1'
$WeeklyWorktree = Join-Path $MainRoot '.claude\worktrees\e8-3-weekly'

if ($Remove) {
    $task = Get-ScheduledTask -TaskPath $TaskPath -TaskName $TaskName -ErrorAction SilentlyContinue
    if ($null -ne $task) {
        Unregister-ScheduledTask -TaskPath $TaskPath -TaskName $TaskName -Confirm:$false
        Write-Output "Removed the scheduled task $TaskPath$TaskName."
    } else {
        Write-Output "No scheduled task $TaskPath$TaskName is registered."
    }
    Write-Output 'To remove the weekly worktree as well (not run by this script):'
    Write-Output "  git -C `"$MainRoot`" worktree remove --force `"$WeeklyWorktree`""
    Write-Output "  git -C `"$MainRoot`" worktree prune"
    return
}

if ($Day -eq $FallbackDay) {
    throw "-Day and -FallbackDay must differ (both are $Day)"
}
if (-not (Test-Path -LiteralPath $Launcher -PathType Leaf)) {
    throw "the launcher $Launcher does not exist (is the main checkout on a branch that has it?)"
}

# PowerShell 7, found now; the task keeps this absolute path.
$pwshCommand = Get-Command pwsh -CommandType Application -ErrorAction SilentlyContinue |
    Select-Object -First 1
if ($null -eq $pwshCommand) {
    throw 'pwsh.exe (PowerShell 7) is not on PATH'
}
$pwsh = $pwshCommand.Source

$time = [datetime]::ParseExact($At, 'HH:mm', [System.Globalization.CultureInfo]::InvariantCulture)
$user = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name

# conhost.exe started directly hosts the console itself, so Windows Terminal (the default terminal
# on Windows 11, "Let Windows decide") never takes it over, and pwsh's -WindowStyle Hidden hides the
# real window after a moment's flash. Through Windows Terminal, -WindowStyle Hidden would hide only
# the pseudo-console and leave the terminal window open for the whole run, and closing that window
# would stop the run (review finding N1; tested on 2026-11-07: no Windows Terminal process, no
# visible window after four seconds).
$conhost = Join-Path $env:SystemRoot 'System32\conhost.exe'
if (-not (Test-Path -LiteralPath $conhost -PathType Leaf)) {
    throw "$conhost does not exist"
}
$launchArgs = "`"$pwsh`" -NoProfile -NonInteractive -WindowStyle Hidden -File `"$Launcher`""
$action = New-ScheduledTaskAction -Execute $conhost -Argument $launchArgs -WorkingDirectory $MainRoot
$triggers = @(
    (New-ScheduledTaskTrigger -Weekly -DaysOfWeek $Day -At $time),
    (New-ScheduledTaskTrigger -Weekly -DaysOfWeek $FallbackDay -At $time)
)
$principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
# No -RunOnlyIfIdle (the launcher checks the last input itself), no -Hidden (the task stays
# visible), no -StartWhenAvailable (no catch-up start at logon).
$settings = New-ScheduledTaskSettingsSet `
    -WakeToRun `
    -MultipleInstances IgnoreNew `
    -ExecutionTimeLimit (New-TimeSpan -Hours 8) `
    -Priority 4

$description = 'LitePack E8-3: the weekly speed-regression run (scripts/e8-3-launch.ps1, ' +
    'scripts/e8-3-weekly.sh, docs/notes/e8-3-design.md). At most one complete run per ' +
    'measurement week; remove with scripts/e8-3-task.ps1 -Remove.'

Register-ScheduledTask -TaskPath $TaskPath -TaskName $TaskName -Action $action -Trigger $triggers `
    -Principal $principal -Settings $settings -Description $description -Force | Out-Null

Write-Output "Registered $TaskPath$TaskName for $user"
Write-Output "  triggers: weekly on $Day and on $FallbackDay at $At (local time)"
Write-Output "  action:   `"$conhost`" $launchArgs"
