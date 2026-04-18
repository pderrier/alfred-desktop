#!/usr/bin/env bash
# bump-version.sh — Single source of truth for version bumping.
# Usage: ./scripts/bump-version.sh 0.2.6
#
# Updates version in all 5 manifest files + README download links.
# Source of truth: the version argument. No file is "primary".

set -euo pipefail

if [ $# -ne 1 ]; then
  echo "Usage: $0 <new-version>"
  echo "Example: $0 0.2.6"
  exit 1
fi

NEW="$1"
DIR="$(cd "$(dirname "$0")/.." && pwd)"

# Validate format
if ! echo "$NEW" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "Error: version must be semver (e.g. 0.2.6), got: $NEW"
  exit 1
fi

# Read current version from package.json
OLD=$(grep -oP '"version":\s*"\K[^"]+' "$DIR/package.json")
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
sed -i "s/\"version\": \"$OLD\"/\"version\": \"$NEW\"/" "$DIR/package.json"

# 2. Cargo.toml (only the package version line)
sed -i "s/^version = \"$OLD\"/version = \"$NEW\"/" "$DIR/src-tauri/Cargo.toml"

# 3-5. Tauri config files
for conf in tauri.conf.json tauri.windows.conf.json tauri.macos.conf.json; do
  sed -i "s/\"version\": \"$OLD\"/\"version\": \"$NEW\"/" "$DIR/src-tauri/$conf"
done

# 6. README download links
sed -i "s/v$OLD/v$NEW/g" "$DIR/README.md"

# 7. Rebuild Cargo.lock
echo "Rebuilding Cargo.lock..."
(cd "$DIR/src-tauri" && cargo check --quiet 2>/dev/null) || true

# Verify
echo ""
echo "Verification:"
grep -n "\"$NEW\"" "$DIR/package.json" "$DIR/src-tauri/tauri.conf.json" "$DIR/src-tauri/tauri.windows.conf.json" "$DIR/src-tauri/tauri.macos.conf.json" | head -5
grep -n "version = \"$NEW\"" "$DIR/src-tauri/Cargo.toml"
grep -c "v$NEW" "$DIR/README.md" | xargs -I{} echo "README.md: {} references to v$NEW"

echo ""
echo "Done. Don't forget to add a '## What's new in v$NEW' section in README.md"
