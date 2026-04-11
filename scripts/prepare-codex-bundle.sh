#!/bin/bash
#
# Downloads portable Node.js, installs @openai/codex, then extracts only the
# native binary + rg into src-tauri/codex-runtime/.  The JS wrapper / Node
# runtime are NOT shipped — Alfred invokes the native codex binary directly.
#
# Usage:
#   bash scripts/prepare-codex-bundle.sh
#
set -euo pipefail

NODE_VERSION="${NODE_VERSION:-v22.15.0}"

# Detect architecture
ARCH=$(uname -m)
if [ "$ARCH" = "arm64" ] || [ "$ARCH" = "aarch64" ]; then
    NODE_ARCH="arm64"
    CODEX_PKG="@openai/codex-darwin-arm64"
    VENDOR_TRIPLE="aarch64-apple-darwin"
else
    NODE_ARCH="x64"
    CODEX_PKG="@openai/codex-darwin-x64"
    VENDOR_TRIPLE="x86_64-apple-darwin"
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TAURI_DIR="$SCRIPT_DIR/../src-tauri"
OUT_DIR="$TAURI_DIR/codex-runtime"
STAGE_DIR="$(mktemp -d)"
NODE_DIR_NAME="node-${NODE_VERSION}-darwin-${NODE_ARCH}"
NODE_URL="https://nodejs.org/dist/${NODE_VERSION}/${NODE_DIR_NAME}.tar.gz"
TAR_PATH="/tmp/${NODE_DIR_NAME}.tar.gz"

echo "=== Prepare Codex Bundle (macOS ${NODE_ARCH}) ==="
echo "Node version : ${NODE_VERSION} (${NODE_ARCH})"
echo "Output dir   : ${OUT_DIR}"

# Clean previous bundle
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"

# ── 1. Download Node.js portable (needed to run npm) ─────────────
if [ ! -f "$TAR_PATH" ]; then
    echo "Downloading Node.js from $NODE_URL ..."
    curl -fsSL "$NODE_URL" -o "$TAR_PATH"
else
    echo "Using cached Node.js tarball at $TAR_PATH"
fi

# ── 2. Extract Node.js to staging dir ────────────────────────────
echo "Extracting Node.js to staging dir..."
tar xzf "$TAR_PATH" -C "$STAGE_DIR" --strip-components=1

NODE_BIN="$STAGE_DIR/bin/node"
if [ ! -f "$NODE_BIN" ]; then
    echo "ERROR: node not found at $NODE_BIN after extraction"
    exit 1
fi

# ── 3. Install @openai/codex via npm (in staging dir) ────────────
NPM_CMD="$STAGE_DIR/bin/npm"
echo "Installing @openai/codex via portable npm..."
"$NPM_CMD" install -g "@openai/codex" --prefix="$STAGE_DIR" 2>&1

# ── 4. Locate native binary and copy to output ──────────────────
# npm global install on macOS puts packages in lib/node_modules/
VENDOR_DIR="$STAGE_DIR/lib/node_modules/@openai/codex/node_modules/${CODEX_PKG}/vendor/${VENDOR_TRIPLE}"

# Fallback: some npm versions put globals directly in node_modules/
if [ ! -d "$VENDOR_DIR" ]; then
    VENDOR_DIR="$STAGE_DIR/node_modules/@openai/codex/node_modules/${CODEX_PKG}/vendor/${VENDOR_TRIPLE}"
fi

NATIVE_BIN="$VENDOR_DIR/codex/codex"
if [ ! -f "$NATIVE_BIN" ]; then
    echo "ERROR: Native codex binary not found at $NATIVE_BIN"
    echo "Searching for codex binary in staging dir..."
    find "$STAGE_DIR" -name "codex" -type f 2>/dev/null || true
    rm -rf "$STAGE_DIR"
    exit 1
fi

# Copy native binary
cp "$NATIVE_BIN" "$OUT_DIR/codex"
chmod +x "$OUT_DIR/codex"
echo "codex binary OK"

# Copy rg from vendor path/ dir
RG_BIN="$VENDOR_DIR/path/rg"
if [ -f "$RG_BIN" ]; then
    mkdir -p "$OUT_DIR/path"
    cp "$RG_BIN" "$OUT_DIR/path/rg"
    chmod +x "$OUT_DIR/path/rg"
    echo "rg binary OK"
else
    echo "WARNING: rg not found at $RG_BIN"
fi

# Verify version
"$OUT_DIR/codex" --version 2>&1 || echo "WARNING: codex --version failed (may need signing on macOS)"

# ── 5. Clean up staging dir ──────────────────────────────────────
rm -rf "$STAGE_DIR"

SIZE=$(du -sh "$OUT_DIR" | cut -f1)
echo "Bundle size: $SIZE"
echo "=== Done. Run 'npm run build:macos' to create the DMG. ==="
