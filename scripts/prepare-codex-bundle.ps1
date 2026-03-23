<#
.SYNOPSIS
    Downloads portable Node.js and installs @openai/codex into src-tauri/codex-runtime/.
    Run this BEFORE `cargo tauri build` so the installer bundles codex + node.
.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/prepare-codex-bundle.ps1
#>
param(
    [string]$NodeVersion = "v22.15.0",
    [string]$Arch = "x64"
)

$ErrorActionPreference = "Stop"

# PS 5.1 compat: Join-Path only accepts 2 args, so chain calls for deeper paths.
function JP { param([string]$a, [string]$b) Join-Path $a $b }

$ScriptDir   = Split-Path -Parent $MyInvocation.MyCommand.Path
$TauriDir    = JP (JP $ScriptDir "..") "src-tauri"
$OutDir      = JP $TauriDir "codex-runtime"
$NodeDirName = "node-$NodeVersion-win-$Arch"
$NodeUrl     = "https://nodejs.org/dist/$NodeVersion/$NodeDirName.zip"
$ZipPath     = JP $env:TEMP "$NodeDirName.zip"

Write-Host "=== Prepare Codex Bundle ===" -ForegroundColor Cyan
Write-Host "Node version : $NodeVersion ($Arch)"
Write-Host "Output dir   : $OutDir"

# Clean previous bundle
if (Test-Path $OutDir) {
    Write-Host "Removing previous codex-runtime..."
    Remove-Item -Recurse -Force $OutDir
}
New-Item -ItemType Directory -Force $OutDir | Out-Null

# ── 1. Download Node.js portable ──────────────────────────────────
if (-not (Test-Path $ZipPath)) {
    Write-Host "Downloading Node.js from $NodeUrl ..."
    Invoke-WebRequest -Uri $NodeUrl -OutFile $ZipPath -UseBasicParsing
} else {
    Write-Host "Using cached Node.js zip at $ZipPath"
}

# ── 2. Extract ────────────────────────────────────────────────────
Write-Host "Extracting Node.js..."
Expand-Archive -Path $ZipPath -DestinationPath $OutDir -Force

# The zip extracts to codex-runtime/node-vXX-win-x64/. Flatten one level.
$Nested = JP $OutDir $NodeDirName
if (Test-Path $Nested) {
    Get-ChildItem -Path $Nested | Move-Item -Destination $OutDir -Force
    Remove-Item -Recurse -Force $Nested
}

# Verify node.exe
$NodeExe = JP $OutDir "node.exe"
if (-not (Test-Path $NodeExe)) {
    throw "node.exe not found at $NodeExe after extraction"
}
Write-Host "node.exe OK: $NodeExe"

# ── 3. Install @openai/codex using the portable npm ──────────────
$NpmCmd = JP $OutDir "npm.cmd"
Write-Host "Installing @openai/codex via portable npm..."

# Set prefix to the codex-runtime dir so codex.cmd lands there
& $NpmCmd install -g "@openai/codex" --prefix="$OutDir" 2>&1 | Write-Host

$CodexCmd = JP $OutDir "codex.cmd"
if (-not (Test-Path $CodexCmd)) {
    throw "codex.cmd not found at $CodexCmd after npm install"
}

# Verify version
$CodexJs = JP (JP (JP (JP (JP $OutDir "node_modules") "@openai") "codex") "bin") "codex.js"
$ver = & $NodeExe $CodexJs --version 2>&1
Write-Host "codex version: $ver" -ForegroundColor Green

# ── 4. Trim unnecessary files to reduce bundle size ───────────────
Write-Host "Trimming unnecessary files..."
$Removals = @(
    "include",          # C headers
    "share",            # docs
    "CHANGELOG.md",
    "README.md",
    "LICENSE"
)
foreach ($item in $Removals) {
    $p = JP $OutDir $item
    if (Test-Path $p) { Remove-Item -Recurse -Force $p }
}

# Remove npm cache
$NpmCache = JP (JP $OutDir "node_modules") ".cache"
if (Test-Path $NpmCache) { Remove-Item -Recurse -Force $NpmCache }

$Size = (Get-ChildItem -Recurse $OutDir | Measure-Object -Property Length -Sum).Sum / 1MB
Write-Host ("Bundle size: {0:N1} MB" -f $Size) -ForegroundColor Cyan
Write-Host "=== Done. Run 'cargo tauri build' to create the installer. ===" -ForegroundColor Green
