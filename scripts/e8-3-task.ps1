<#
.SYNOPSIS
  Registers or removes the scheduled task of E8-3's weekly speed-regression run
  (docs/notes/e8-3-design.md).

.DESCRIPTION
  -Register creates (or replaces) the task \LitePack\E8-3 weekly speed for the current user:
    * runs only when the user is logged on (interactive logon, no stored password), RunLevel Limited;
    * two weekly triggers, -Day and -FallbackDay, both at -At (local time);
    * wakes the computer, starts only after 15 idle minutes (waiting up to 2 hours for idle), does
      not stop when the computer is no longer idle, ignores a new start while one runs, stops
      after 8 hours, and is hidden;
    * runs Git for Windows' bash.exe (found from git's own location, never the WSL bash.exe on
      PATH) on the main checkout's scripts/e8-3-weekly.sh.
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
$WeeklyScript = Join-Path $MainRoot 'scripts\e8-3-weekly.sh'
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
if (-not (Test-Path -LiteralPath $WeeklyScript -PathType Leaf)) {
    throw "the weekly script $WeeklyScript does not exist (is the main checkout on a branch that has it?)"
}

# Git for Windows' bash.exe: git.exe sits in <Git>\cmd (or <Git>\bin, <Git>\mingw64\bin); bash.exe
# in <Git>\bin. Never the WSL bash.exe on PATH (System32 or WindowsApps).
$gitDir = Split-Path -Parent $git
$bash = $null
foreach ($candidate in @(
        (Join-Path (Split-Path -Parent $gitDir) 'bin\bash.exe'),
        (Join-Path $gitDir 'bash.exe'),
        (Join-Path (Split-Path -Parent (Split-Path -Parent $gitDir)) 'bin\bash.exe'))) {
    if (Test-Path -LiteralPath $candidate -PathType Leaf) {
        $bash = [System.IO.Path]::GetFullPath($candidate)
        break
    }
}
if ($null -eq $bash) {
    throw "Git for Windows' bash.exe was not found next to $git"
}
if ($bash -match '\\(System32|WindowsApps)\\') {
    throw "refusing $bash (the WSL bash, not Git for Windows')"
}

$time = [datetime]::ParseExact($At, 'HH:mm', [System.Globalization.CultureInfo]::InvariantCulture)
$user = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name

# Forward slashes: bash reads the path as it is, and the quotes keep the spaces in it.
$scriptArg = '"' + ($WeeklyScript -replace '\\', '/') + '"'
$action = New-ScheduledTaskAction -Execute $bash -Argument $scriptArg -WorkingDirectory $MainRoot
$triggers = @(
    (New-ScheduledTaskTrigger -Weekly -DaysOfWeek $Day -At $time),
    (New-ScheduledTaskTrigger -Weekly -DaysOfWeek $FallbackDay -At $time)
)
$principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet `
    -WakeToRun `
    -RunOnlyIfIdle `
    -IdleDuration (New-TimeSpan -Minutes 15) `
    -IdleWaitTimeout (New-TimeSpan -Hours 2) `
    -DontStopOnIdleEnd `
    -MultipleInstances IgnoreNew `
    -ExecutionTimeLimit (New-TimeSpan -Hours 8) `
    -Hidden

$description = 'LitePack E8-3: the weekly speed-regression run (scripts/e8-3-weekly.sh, ' +
    'docs/notes/e8-3-design.md). At most one complete run per ISO week; remove with ' +
    'scripts/e8-3-task.ps1 -Remove.'

Register-ScheduledTask -TaskPath $TaskPath -TaskName $TaskName -Action $action -Trigger $triggers `
    -Principal $principal -Settings $settings -Description $description -Force | Out-Null

Write-Output "Registered $TaskPath$TaskName for $user"
Write-Output "  triggers: weekly on $Day and on $FallbackDay at $At (local time)"
Write-Output "  action:   `"$bash`" $scriptArg"
Write-Output "  start the first run by hand: Start-ScheduledTask -TaskPath '$TaskPath' -TaskName '$TaskName'"
