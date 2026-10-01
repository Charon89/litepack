#Requires -Version 5.1
<#
.SYNOPSIS
    Prepare a Windows machine for LitePack development. Idempotent: installs only what is missing.

.DESCRIPTION
    Installs, via winget and rustup:
      - Git for Windows and GitHub CLI
      - Visual Studio 2022 Build Tools with the C++ workload (MSVC linker + Windows SDK; one UAC prompt)
      - rustup and the toolchain named in rust-toolchain.toml, moved to the current stable release
      - cargo-nextest and cargo-deny
    then prints what it found. Exit code 0 means every command in CLAUDE.md can run on this machine.

    Running this script accepts the licence terms of the packages it installs (notably Microsoft's
    Visual Studio Build Tools licence).

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\setup-windows.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$vcTools = 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64'

function Test-Command([string]$Name) {
    [bool](Get-Command $Name -ErrorAction SilentlyContinue)
}

# Installers edit the PATH stored in the registry, not this process's copy: pick up what they added.
function Update-SessionPath {
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
    $fresh = @(Join-Path $cargoHome 'bin') +
        ([Environment]::GetEnvironmentVariable('Path', 'User') -split ';') +
        ([Environment]::GetEnvironmentVariable('Path', 'Machine') -split ';')
    $current = $env:Path -split ';'
    $missing = @($fresh | Where-Object { $_ -and ($current -notcontains $_) })
    if ($missing.Count -gt 0) { $env:Path = (@($env:Path) + $missing) -join ';' }
}

# winget's exit code varies by installer (reboot required, already installed), so callers check
# for the tool afterwards instead of trusting it.
function Install-WingetPackage([string]$Id, [string[]]$ExtraArgs = @()) {
    Write-Host "==> winget install $Id"
    winget install --id $Id --exact --source winget --accept-package-agreements `
        --accept-source-agreements --disable-interactivity @ExtraArgs
    Update-SessionPath
}

function Get-MsvcInstallPath {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) { return $null }
    & $vswhere -latest -products * -requires $vcTools -property installationPath | Select-Object -First 1
}

if (-not (Test-Command 'winget')) {
    throw 'winget is required (install "App Installer" from the Microsoft Store), then re-run.'
}
Update-SessionPath

if (-not (Test-Command 'git')) { Install-WingetPackage 'Git.Git' }
if (-not (Test-Command 'gh')) { Install-WingetPackage 'GitHub.cli' }

# First of the slow steps so its UAC prompt appears while someone is still at the keyboard.
if (-not (Get-MsvcInstallPath)) {
    Write-Host 'Visual Studio Build Tools: installing the C++ workload (several GB) - approve the UAC prompt.'
    $vsArgs = '--passive --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
    Install-WingetPackage 'Microsoft.VisualStudio.2022.BuildTools' @('--override', $vsArgs)
}

if (-not (Test-Command 'rustup')) { Install-WingetPackage 'Rustlang.Rustup' }
if (Test-Command 'rustup') {
    Push-Location $repoRoot
    try {
        Write-Host '==> rustup: toolchain from rust-toolchain.toml'
        rustup toolchain install
        # CI always builds with the newest stable; an install from months ago would lint differently.
        rustup update stable
    }
    finally { Pop-Location }
}

# Built from source, so they need the MSVC linker installed above.
if ((Test-Command 'cargo') -and (Get-MsvcInstallPath)) {
    foreach ($tool in 'cargo-nextest', 'cargo-deny') {
        if (-not (Test-Command $tool)) {
            Write-Host "==> cargo install $tool"
            cargo +stable install --locked $tool
        }
    }
}

$checks = [ordered]@{
    'git'            = { git --version }
    'gh'             = { gh --version }
    'MSVC C++ tools' = { Get-MsvcInstallPath }
    'rustc'          = { rustc --version }
    'cargo'          = { cargo --version }
    'rustfmt'        = { cargo fmt --version }
    'clippy'         = { cargo clippy --version }
    'cargo-nextest'  = { cargo nextest --version }
    'cargo-deny'     = { cargo deny --version }
}
$missing = @()
Write-Host ''
Push-Location $repoRoot
try {
    foreach ($name in $checks.Keys) {
        $found = $null
        try { $found = & $checks[$name] 2>$null | Select-Object -First 1 } catch { }
        if ($found) { Write-Host ('{0,-16} {1}' -f $name, $found) }
        else { Write-Host ('{0,-16} MISSING' -f $name); $missing += $name }
    }
}
finally { Pop-Location }

if ($missing.Count -gt 0) {
    Write-Host ''
    Write-Host "Not ready - missing: $($missing -join ', ')."
    if ($missing -contains 'MSVC C++ tools') {
        Write-Host 'If Visual Studio is already installed, add the "Desktop development with C++" workload in Visual Studio Installer.'
    }
    Write-Host 'Fix the above and run this script again.'
    exit 1
}
Write-Host ''
Write-Host 'Ready. Open a new terminal so the updated PATH is picked up.'
