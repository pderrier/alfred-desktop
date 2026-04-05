<#
.SYNOPSIS
    Downloads portable Node.js, installs @openai/codex, then extracts only the
    native binary + rg into src-tauri/codex-runtime/.  The JS wrapper / Node
    runtime are NOT shipped — Alfred invokes the native codex.exe directly.
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
$StageDir    = JP $env:TEMP "codex-stage"
$NodeDirName = "node-$NodeVersion-win-$Arch"
$NodeUrl     = "https://nodejs.org/dist/$NodeVersion/$NodeDirName.zip"
$ZipPath     = JP $env:TEMP "$NodeDirName.zip"

Write-Host "=== Prepare Codex Bundle ===" -ForegroundColor Cyan
Write-Host "Node version : $NodeVersion ($Arch)"
Write-Host "Output dir   : $OutDir"

# Clean previous bundle and staging area
foreach ($d in @($OutDir, $StageDir)) {
    if (Test-Path $d) { Remove-Item -Recurse -Force $d }
}
New-Item -ItemType Directory -Force $StageDir | Out-Null
New-Item -ItemType Directory -Force $OutDir | Out-Null

# ── 1. Download Node.js portable (needed to run npm) ─────────────
if (-not (Test-Path $ZipPath)) {
    Write-Host "Downloading Node.js from $NodeUrl ..."
    Invoke-WebRequest -Uri $NodeUrl -OutFile $ZipPath -UseBasicParsing
} else {
    Write-Host "Using cached Node.js zip at $ZipPath"
}

# ── 2. Extract Node.js to staging dir ────────────────────────────
Write-Host "Extracting Node.js to staging dir..."
Expand-Archive -Path $ZipPath -DestinationPath $StageDir -Force

$Nested = JP $StageDir $NodeDirName
if (Test-Path $Nested) {
    Get-ChildItem -Path $Nested | Move-Item -Destination $StageDir -Force
    Remove-Item -Recurse -Force $Nested
}

$NodeExe = JP $StageDir "node.exe"
if (-not (Test-Path $NodeExe)) {
    throw "node.exe not found at $NodeExe after extraction"
}

# ── 3. Install @openai/codex via npm (in staging dir) ────────────
$NpmCmd = JP $StageDir "npm.cmd"
Write-Host "Installing @openai/codex via portable npm..."
& $NpmCmd install -g "@openai/codex" --prefix="$StageDir" 2>&1 | Write-Host

# ── 4. Locate native binary and copy to output ──────────────────
$VendorDir = JP (JP (JP (JP (JP (JP (JP (JP $StageDir "node_modules") "@openai") "codex") "node_modules") "@openai") "codex-win32-x64") "vendor") "x86_64-pc-windows-msvc"
$NativeBin = JP (JP $VendorDir "codex") "codex.exe"
if (-not (Test-Path $NativeBin)) {
    throw "Native codex.exe not found at $NativeBin"
}

# Copy native binary
Copy-Item $NativeBin -Destination $OutDir
Write-Host "codex.exe OK" -ForegroundColor Green

# Copy rg.exe from vendor path/ dir
$RgBin = JP (JP $VendorDir "path") "rg.exe"
if (Test-Path $RgBin) {
    # Put rg in a path/ subdir matching the vendor layout
    $PathDir = JP $OutDir "path"
    New-Item -ItemType Directory -Force $PathDir | Out-Null
    Copy-Item $RgBin -Destination $PathDir
    Write-Host "rg.exe OK" -ForegroundColor Green
} else {
    Write-Host "WARNING: rg.exe not found at $RgBin" -ForegroundColor Yellow
}

# Verify version
$ver = & (JP $OutDir "codex.exe") --version 2>&1
Write-Host "codex version: $ver" -ForegroundColor Green

# ── 5. Clean up staging dir ──────────────────────────────────────
Remove-Item -Recurse -Force $StageDir

$Size = (Get-ChildItem -Recurse $OutDir | Measure-Object -Property Length -Sum).Sum / 1MB
Write-Host ("Bundle size: {0:N1} MB" -f $Size) -ForegroundColor Cyan
Write-Host "=== Done. Run 'cargo tauri build' to create the installer. ===" -ForegroundColor Green
