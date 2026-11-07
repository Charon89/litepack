<#
.SYNOPSIS
  Starts E8-3's weekly speed-regression run (scripts/e8-3-weekly.sh) when the PC is unattended.

.DESCRIPTION
  The scheduled task \LitePack\E8-3 weekly speed (scripts/e8-3-task.ps1) runs this launcher as
    pwsh.exe -NoProfile -NonInteractive -WindowStyle Hidden -File <main checkout>\scripts\e8-3-launch.ps1
  It replaces the task's idle condition, which cannot work after an unattended wake (Windows goes
  back to sleep after its unattended-sleep timeout, long before 15 idle minutes). In order:
    1. finds the main checkout from this script's own location;
    2. if scripts/e8-3-weekly.sh is missing there, appends a REFUSED line to the status log and
       exits 0;
    3. reads the time since the last keyboard or mouse input (GetLastInputInfo); under 15 minutes
       it appends `REFUSED user input <n> min ago` and exits 0;
    4. asks Windows to keep the system awake while this process lives
       (SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)): a transient request that ends
       with the process, not a setting;
    5. runs Git for Windows' bash.exe (found from git's own location, never the WSL bash.exe on
       PATH) on scripts/e8-3-weekly.sh with any passed-through arguments, waits, and exits with
       bash's exit code.
  -DryRun passes --dry-run through and prints refusals instead of writing them to the status log.
  The launcher changes no setting (no power plan, antivirus or other system setting).

.EXAMPLE
  pwsh -NoProfile -File scripts/e8-3-launch.ps1 -DryRun
#>
[CmdletBinding()]
param(
    [switch] $DryRun,

    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $PassThru = @()
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

$IdleMinutes = 15

# 1. The main checkout: git's common directory is <main>\.git, whichever checkout holds this copy.
$git = (Get-Command git -CommandType Application | Select-Object -First 1).Source
$common = & $git -C $PSScriptRoot rev-parse --path-format=absolute --git-common-dir
if ($LASTEXITCODE -ne 0 -or -not $common) {
    throw "$PSScriptRoot is not inside a git checkout"
}
$MainRoot = Split-Path -Parent ([System.IO.Path]::GetFullPath($common.Trim()))
$Acceptance = Join-Path $MainRoot 'target\acceptance'
$StatusLog = Join-Path $Acceptance 'e8-3-weekly-status.log'
$WeeklyScript = Join-Path $MainRoot 'scripts\e8-3-weekly.sh'

# A refusal: one `REFUSED <reason> <UTC>` line in the status log (as e8-3-weekly.sh writes them),
# or printed under -DryRun; then exit 0.
function Exit-Refused([string] $Reason) {
    if ($DryRun) {
        Write-Output "[dry-run] REFUSED $Reason"
    } else {
        New-Item -ItemType Directory -Force -Path $Acceptance | Out-Null
        $stamp = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
        [System.IO.File]::AppendAllText($StatusLog, "REFUSED $Reason $stamp`n")
    }
    exit 0
}

# 2. The weekly script must exist in the main checkout (its branch may predate E8-3).
if (-not (Test-Path -LiteralPath $WeeklyScript -PathType Leaf)) {
    $head = & $git -C $MainRoot symbolic-ref --short -q HEAD
    if ($LASTEXITCODE -ne 0 -or -not $head) {
        $head = & $git -C $MainRoot rev-parse --short HEAD
    }
    Exit-Refused "the weekly script is missing from the main checkout (HEAD $head)"
}

Add-Type -Namespace LitePackE83 -Name Native -MemberDefinition @'
[StructLayout(LayoutKind.Sequential)]
public struct LASTINPUTINFO {
    public uint cbSize;
    public uint dwTime;
}
[DllImport("user32.dll", SetLastError = true)]
public static extern bool GetLastInputInfo(ref LASTINPUTINFO plii);
[DllImport("kernel32.dll", SetLastError = true)]
public static extern uint SetThreadExecutionState(uint esFlags);
'@

# 3. The time since the last keyboard or mouse input of this session. dwTime is the 32-bit tick
#    count of that input, so the difference is taken modulo 2^32.
$info = New-Object 'LitePackE83.Native+LASTINPUTINFO'
$info.cbSize = [uint32][System.Runtime.InteropServices.Marshal]::SizeOf($info)
if (-not [LitePackE83.Native]::GetLastInputInfo([ref] $info)) {
    Exit-Refused 'GetLastInputInfo failed'
}
$wrap = [long]4294967296
$nowTicks = [Environment]::TickCount64 % $wrap
$idleMs = ($nowTicks - [long]$info.dwTime + $wrap) % $wrap
$idleMin = [math]::Floor($idleMs / 60000)
if ($idleMs -lt ($IdleMinutes * 60000)) {
    Exit-Refused "user input $idleMin min ago"
}
if ($DryRun) {
    Write-Output "[dry-run] last user input $idleMin min ago (needs $IdleMinutes)"
}

# 4. Keep the system awake while this process runs (ES_CONTINUOUS | ES_SYSTEM_REQUIRED). The
#    request ends with the process; nothing is changed.
$esContinuousSystemRequired = [uint32]2147483649
if (-not $DryRun) {
    if ([LitePackE83.Native]::SetThreadExecutionState($esContinuousSystemRequired) -eq 0) {
        Exit-Refused 'SetThreadExecutionState failed'
    }
}

# 5. Git for Windows' bash.exe: git.exe sits in <Git>\cmd (or <Git>\bin, <Git>\mingw64\bin); bash.exe
#    in <Git>\bin. Never the WSL bash.exe on PATH (System32 or WindowsApps).
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
if ($null -eq $bash -or $bash -match '\\(System32|WindowsApps)\\') {
    Exit-Refused "Git for Windows' bash.exe was not found next to $git"
}

$bashArgs = @($WeeklyScript -replace '\\', '/')
if ($DryRun) {
    $bashArgs += '--dry-run'
}
$bashArgs += $PassThru
& $bash @bashArgs
exit $LASTEXITCODE
