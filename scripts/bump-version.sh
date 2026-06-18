#!/usr/bin/env bash
# bump-version.sh — Single source of truth for version bumping.
# Usage: ./scripts/bump-version.sh 0.2.6
#
# Updates version in all 6 manifest files + README download links.
# Source of truth: the version argument. No file is "primary".
# Bumps: package.json, src-tauri/Cargo.toml, tauri.conf.json,
#        tauri.windows.conf.json, tauri.macos.conf.json, alfred-cli/Cargo.toml.

set -euo pipefail

if [ $# -ne 1 ]; then
  echo "Usage: $0 <new-version>"
  echo "Example: $0 0.2.6"
  exit 1
fi

NEW="$1"
DIR="$(cd "$(dirname "$0")/.." && pwd)"

# Cross-platform in-place sed (GNU/BSD)
sed_in_place() {
  local expr="$1"
  local file="$2"

  if sed --version >/dev/null 2>&1; then
    sed -i -e "$expr" "$file"
  else
    sed -i '' -e "$expr" "$file"
  fi
}

# Validate format
if ! echo "$NEW" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "Error: version must be semver (e.g. 0.2.6), got: $NEW"
  exit 1
fi

# Read current version from package.json
OLD=$(sed -nE 's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' "$DIR/package.json" | head -1)
if [ -z "$OLD" ]; then
  echo "Error: could not read current version from package.json"
  exit 1
fi

if [ "$OLD" = "$NEW" ]; then
  echo "Already at version $NEW"
  exit 0
fi

echo "Bumping $OLD → $NEW"

# 1. package.json
sed_in_place "s/\"version\": \"$OLD\"/\"version\": \"$NEW\"/" "$DIR/package.json"

# 2. Cargo.toml (only the package version line)
sed_in_place "s/^version = \"$OLD\"/version = \"$NEW\"/" "$DIR/src-tauri/Cargo.toml"

# 3-5. Tauri config files
for conf in tauri.conf.json tauri.windows.conf.json tauri.macos.conf.json; do
  sed_in_place "s/\"version\": \"$OLD\"/\"version\": \"$NEW\"/" "$DIR/src-tauri/$conf"
done

# 6. alfred-cli/Cargo.toml — match the first `version = ...` line regardless
# of its current value, so the CLI stays aligned even if it lagged behind.
sed_in_place "0,/^version = \"[^\"]*\"/{s/^version = \"[^\"]*\"/version = \"$NEW\"/}" "$DIR/alfred-cli/Cargo.toml"

# 7. README download links — the download-link lines must ALWAYS advertise the
# new version, even if their text drifted away from $OLD (e.g. it was frozen at
# v0.3.2 across several releases because a plain `s/v$OLD/.../` only rewrites the
# *current* version). Scope to the release-link lines (matched by the stable
# `/alfred/release/` URL marker so the changelog history is never touched) and
# replace whatever `vX.Y.Z` token they carry, independent of its prior value.
# `[0-9]\{1,\}` is portable BRE (works under both GNU and BSD sed).
sed_in_place "/alfred\/release\//s/v[0-9]\{1,\}\.[0-9]\{1,\}\.[0-9]\{1,\}/v$NEW/g" "$DIR/README.md"

# 8. Rebuild Cargo.lock files
echo "Rebuilding Cargo.lock files..."
(cd "$DIR/src-tauri" && cargo check --quiet 2>/dev/null) || true
(cd "$DIR/alfred-cli" && cargo check --quiet 2>/dev/null) || true

# Verify
echo ""
echo "Verification:"
grep -n "\"$NEW\"" "$DIR/package.json" "$DIR/src-tauri/tauri.conf.json" "$DIR/src-tauri/tauri.windows.conf.json" "$DIR/src-tauri/tauri.macos.conf.json" | head -5
grep -n "version = \"$NEW\"" "$DIR/src-tauri/Cargo.toml" "$DIR/alfred-cli/Cargo.toml"
grep -c "v$NEW" "$DIR/README.md" | xargs -I{} echo "README.md: {} references to v$NEW"

echo ""
echo "Done. Don't forget to add a '## What's new in v$NEW' section in README.md"
