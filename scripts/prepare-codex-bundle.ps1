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
# 2026-05-24 : @openai/codex@0.133+ ships the native binaries via npm-alias
# optional dependencies (`@openai/codex-win32-x64`) at the FLAT path
# `node_modules/@openai/codex-win32-x64/...`, not nested under `codex/`.
# Layout inside vendor/{triple}/ also changed : `codex/codex.exe` → `bin/codex.exe`
# and `path/rg.exe` → `codex-path/rg.exe`. Search-based resolution avoids
# brittleness if the upstream re-shuffles further.
$VendorDir = JP (JP (JP (JP $StageDir "node_modules") "@openai") "codex-win32-x64") "vendor"
$TripleDir = JP $VendorDir "x86_64-pc-windows-msvc"
if (-not (Test-Path $TripleDir)) {
    Write-Host "Expected triple dir not found at $TripleDir — searching staging tree..." -ForegroundColor Yellow
    $found = Get-ChildItem -Path $StageDir -Recurse -Filter "codex.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $found) { throw "codex.exe not found anywhere under $StageDir after npm install" }
    $NativeBin = $found.FullName
} else {
    $NativeBin = JP (JP $TripleDir "bin") "codex.exe"
    if (-not (Test-Path $NativeBin)) {
        # Pre-0.133 fallback for older pinned versions
        $LegacyBin = JP (JP $TripleDir "codex") "codex.exe"
        if (Test-Path $LegacyBin) { $NativeBin = $LegacyBin } else {
            throw "Native codex.exe not found at $NativeBin or $LegacyBin"
        }
    }
}

# Copy native binary
Copy-Item $NativeBin -Destination $OutDir
Write-Host "codex.exe OK ($NativeBin)" -ForegroundColor Green

# Copy rg.exe — new path is `codex-path/rg.exe`, legacy is `path/rg.exe`
$RgBin = JP (JP $TripleDir "codex-path") "rg.exe"
if (-not (Test-Path $RgBin)) {
    $RgLegacy = JP (JP $TripleDir "path") "rg.exe"
    if (Test-Path $RgLegacy) { $RgBin = $RgLegacy }
}
if (Test-Path $RgBin) {
    # Put rg in a path/ subdir matching the vendor layout
    $PathDir = JP $OutDir "path"
    New-Item -ItemType Directory -Force $PathDir | Out-Null
    Copy-Item $RgBin -Destination $PathDir
    Write-Host "rg.exe OK ($RgBin)" -ForegroundColor Green
} else {
    Write-Host "WARNING: rg.exe not found at $RgBin or legacy path" -ForegroundColor Yellow
}

# Verify version
$ver = & (JP $OutDir "codex.exe") --version 2>&1
Write-Host "codex version: $ver" -ForegroundColor Green

# ── 5. Clean up staging dir ──────────────────────────────────────
Remove-Item -Recurse -Force $StageDir

$Size = (Get-ChildItem -Recurse $OutDir | Measure-Object -Property Length -Sum).Sum / 1MB
Write-Host ("Bundle size: {0:N1} MB" -f $Size) -ForegroundColor Cyan
Write-Host "=== Done. Run 'cargo tauri build' to create the installer. ===" -ForegroundColor Green
