#!/usr/bin/env bash
# sync-version-from-tag.sh — Align project version fields from a git tag.
# Usage: ./scripts/sync-version-from-tag.sh v0.2.8

set -euo pipefail

if [ $# -ne 1 ]; then
  echo "Usage: $0 <tag>"
  echo "Example: $0 v0.2.8"
  exit 1
fi

TAG="$1"
if ! echo "$TAG" | grep -qE '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "Error: tag must be in form v<semver> (e.g. v0.2.8), got: $TAG"
  exit 1
fi

NEW="${TAG#v}"
DIR="$(cd "$(dirname "$0")/.." && pwd)"

"$DIR/scripts/bump-version.sh" "$NEW"

echo "Synchronized versions from tag $TAG"
