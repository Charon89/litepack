# E6-4's acceptance (docs/notes/e6-design.md section 6), CI only: `lpk-gui.exe --register-shell --open-with` and
# `--unregister-shell` leave the registry as found, `.lpk` opens in LitePack after registering, and no default handler
# changes.
#   pwsh scripts/e6-registry-diff.ps1 <path to lpk-gui.exe>     (lpk_shell.dll beside it)
# This script WRITES the current user's registry (HKCU\Software\Classes, Software\LitePack, RegisteredApplications),
# so it refuses to run anywhere but a CI runner: it needs CI=true in the environment.
param([Parameter(Mandatory = $true)][string]$Gui)
$ErrorActionPreference = 'Stop'
if ($env:CI -ne 'true') { throw 'e6-registry-diff.ps1 writes the registry of the user running it: it runs on CI only (CI=true)' }
$Gui = (Resolve-Path -LiteralPath $Gui).Path
$exts = 'lpk', 'zip', '7z', 'rar', 'tar', 'tgz'
$classes = 'HKCU\Software\Classes'
$explorer = 'HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts'

# Every key the registration touches, and each extension's UserChoice.
$keys = @()
foreach ($e in $exts) { $keys += "$classes\.$e"; $keys += "$classes\LitePack.$e" }
# Whole subtrees, so that a parent registration created (`*\shell`, `Directory\Background\shell`, `CLSID`) and left
# empty shows too; `Directory` covers `Directory\Background`.
foreach ($t in '*', 'Directory', 'CLSID') { $keys += "$classes\$t" }
$keys += 'HKCU\Software\LitePack', 'HKCU\Software\RegisteredApplications'
$choices = $exts | ForEach-Object { "$explorer\.$_\UserChoice" }

# `reg export` of each key (its whole subtree) as text; an absent key is the text ABSENT.
function Get-Snapshot([string[]]$Paths) {
    $out = @{}
    $tmp = Join-Path $env:RUNNER_TEMP ("e6-export-" + [guid]::NewGuid().ToString('N') + '.reg')
    foreach ($k in $Paths) {
        cmd /c "reg query `"$k`" >nul 2>nul"
        if ($LASTEXITCODE -ne 0) { $out[$k] = 'ABSENT'; continue }
        cmd /c "reg export `"$k`" `"$tmp`" /y >nul 2>nul"
        if ($LASTEXITCODE -ne 0) { throw "reg export failed for $k" }
        $out[$k] = Get-Content -LiteralPath $tmp -Raw
        Remove-Item -LiteralPath $tmp -Force
    }
    return $out
}

function Compare-Snapshots($Before, $After, [string]$What) {
    $bad = 0
    foreach ($k in $Before.Keys) {
        if ($Before[$k] -ne $After[$k]) {
            $bad++
            Write-Host "DIFFERENT after ${What}: $k"
            Write-Host "--- before"; Write-Host $Before[$k]
            Write-Host "--- after"; Write-Host $After[$k]
        }
    }
    return $bad
}

function Invoke-Gui([string[]]$Arguments) {
    # A GUI-subsystem exe: only Start-Process -Wait waits for it. Its stderr goes to a file, shown when not empty.
    $err = Join-Path $env:RUNNER_TEMP ("e6-gui-" + [guid]::NewGuid().ToString('N') + '.txt')
    $p = Start-Process -FilePath $Gui -ArgumentList $Arguments -Wait -PassThru -RedirectStandardError $err
    $text = Get-Content -LiteralPath $err -Raw -ErrorAction SilentlyContinue
    if ($text) { Write-Host "lpk-gui $($Arguments -join ' ') stderr: $text" }
    Remove-Item -LiteralPath $err -Force -ErrorAction SilentlyContinue
    return $p.ExitCode
}

# On a failed check: the association chain as the per-user and merged views hold it, what AssocQueryStringW gives for
# the command and the executable, and the session's UAC setting and integrity level.
function Show-Chain {
    foreach ($k in "$classes\.lpk", "$classes\LitePack.lpk", 'HKCR\.lpk', 'HKCR\LitePack.lpk', 'HKCU\Software\LitePack') {
        Write-Host "--- reg query $k /s"
        cmd /c "reg query `"$k`" /s 2>&1"
    }
    foreach ($s in 1, 2) {
        $b = New-Object System.Text.StringBuilder 1024
        [uint32]$l = 1024
        $r = [LpkAssoc]::AssocQueryStringW(0, $s, '.lpk', $null, $b, [ref]$l)
        Write-Host ('AssocQueryStringW(.lpk, ASSOCSTR {0}) = 0x{1:X8} "{2}"' -f $s, $r, $b.ToString())
    }
    cmd /c 'reg query HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System /v EnableLUA 2>&1'
    whoami /groups | Select-String 'Mandatory Label'
}

Add-Type -TypeDefinition @'
using System.Text;
using System.Runtime.InteropServices;
public static class LpkAssoc {
    [DllImport("shlwapi.dll", CharSet = CharSet.Unicode)]
    public static extern int AssocQueryStringW(uint flags, int str, string assoc, string extra, StringBuilder buf, ref uint len);
}
'@

$before = Get-Snapshot ($keys + $choices)

$code = Invoke-Gui @('--register-shell', '--open-with')
if ($code -ne 0) { throw "--register-shell --open-with exited $code" }

# `.lpk` opens in LitePack. First the chain as the merged view holds it, which is what Windows reads: `.lpk` names
# LitePack.lpk, whose open command starts the exe.
$progid = (Get-ItemProperty -LiteralPath 'Registry::HKEY_CLASSES_ROOT\.lpk').'(default)'
if ($progid -ne 'LitePack.lpk') { Show-Chain; throw "HKCR\.lpk names '$progid', not LitePack.lpk" }
$command = (Get-ItemProperty -LiteralPath 'Registry::HKEY_CLASSES_ROOT\LitePack.lpk\shell\open\command').'(default)'
$want = '"{0}" --open "%1"' -f $Gui
if ($command -ne $want) { Show-Chain; throw "HKCR\LitePack.lpk's open command is '$command', not '$want'" }

# Then AssocQueryStringW(ASSOCSTR_EXECUTABLE), as Explorer resolves a double-click. Asked from this session, which runs
# at high integrity, it answers 0x80070483 (no association) although HKCR holds the chain: the shell gives an elevated
# caller no per-user association. Explorer runs at medium integrity, so the question is asked from a basic-user child
# at medium integrity (`runas /trustlevel:0x20000`), which writes its answer and its integrity level to a file.
$elevated = New-Object System.Text.StringBuilder 1024
[uint32]$len = 1024
$hr = [LpkAssoc]::AssocQueryStringW(0, 2, '.lpk', $null, $elevated, [ref]$len)
Write-Host ('elevated AssocQueryStringW(.lpk) = 0x{0:X8} "{1}" (for the record; not the check)' -f $hr, $elevated.ToString())
$childPs1 = Join-Path $env:RUNNER_TEMP 'e6-assoc-child.ps1'
$childOut = Join-Path $env:RUNNER_TEMP 'e6-assoc-child.txt'
$winps = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
foreach ($p in $childPs1, $childOut, $winps) {
    if ($p.Contains(' ')) { throw "runas takes one command string: '$p' holds a space" }
}
Set-Content -LiteralPath $childPs1 -Encoding ascii -Value @'
param([string]$Out)
Add-Type -TypeDefinition @"
using System.Text;
using System.Runtime.InteropServices;
public static class LpkAssocChild {
    [DllImport("shlwapi.dll", CharSet = CharSet.Unicode)]
    public static extern int AssocQueryStringW(uint flags, int str, string assoc, string extra, StringBuilder buf, ref uint len);
}
"@
$sb = New-Object System.Text.StringBuilder 1024
[uint32]$len = 1024
$hr = [LpkAssocChild]::AssocQueryStringW(0, 2, '.lpk', $null, $sb, [ref]$len)
$label = (whoami /groups | Select-String 'Mandatory Label' | Out-String).Trim()
Set-Content -LiteralPath "$Out.part" -Value ('{0:X8}|{1}|{2}' -f $hr, $sb.ToString(), $label)
Move-Item -LiteralPath "$Out.part" -Destination $Out -Force
'@
Remove-Item -LiteralPath $childOut -Force -ErrorAction SilentlyContinue
& runas.exe /trustlevel:0x20000 "$winps -NoProfile -ExecutionPolicy Bypass -File $childPs1 -Out $childOut"
$deadline = (Get-Date).AddSeconds(90)
while (-not (Test-Path -LiteralPath $childOut) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 500 }
if (-not (Test-Path -LiteralPath $childOut)) { Show-Chain; throw 'the medium-integrity child (runas /trustlevel:0x20000) wrote no answer within 90 s' }
$answer = (Get-Content -LiteralPath $childOut -Raw).Trim()
Write-Host "medium-integrity AssocQueryStringW(.lpk): $answer"
$childHr, $childExe, $childLabel = $answer -split '\|', 3
if ($childLabel -notmatch 'Medium Mandatory Level') { Show-Chain; throw "the child did not run at medium integrity: $childLabel" }
if ($childHr -ne '00000000') { Show-Chain; throw "AssocQueryStringW(.lpk) at medium integrity failed: 0x$childHr" }
if ($childExe -ne $Gui) { Show-Chain; throw "AssocQueryStringW(.lpk) at medium integrity gave '$childExe', expected '$Gui'" }

# Each opted-in extension lists its ProgID in OpenWithProgids.
$assoc = Get-Item 'HKCU:\Software\LitePack\Capabilities\FileAssociations'
$opted = $assoc.GetValueNames() | Where-Object { $_ -ne '' }
foreach ($need in '.lpk', '.zip', '.7z', '.tar', '.tgz') {
    if ($opted -notcontains $need) { throw "Capabilities lists no $need" }
}
foreach ($name in $opted) {
    $progid = $assoc.GetValue($name)
    $with = Get-Item "HKCU:\Software\Classes\$name\OpenWithProgids"
    if ($with.GetValueNames() -notcontains $progid) { throw "$name\OpenWithProgids does not list $progid" }
}

# No default handler changed: every UserChoice as it was.
$afterRegister = Get-Snapshot $choices
$bad = 0
foreach ($k in $choices) {
    if ($before[$k] -ne $afterRegister[$k]) { $bad++; Write-Host "UserChoice changed by registering: $k" }
}
if ($bad -ne 0) { throw 'UserChoice changed' }

$code = Invoke-Gui @('--unregister-shell')
if ($code -ne 0) { throw "--unregister-shell exited $code" }

$after = Get-Snapshot ($keys + $choices)
$bad = Compare-Snapshots $before $after 'unregistering'
if ($bad -ne 0) { Write-Host "$bad key(s) differ from before"; exit 1 }
Write-Host "registry as found after register and unregister: $($before.Count) keys compared"
