#!/usr/bin/env bash
# sync-version-from-tag.sh — Align project version fields from a git tag.
# Usage: ./scripts/sync-version-from-tag.sh [v0.2.8|refs/tags/v0.2.8]

set -euo pipefail

TAG_INPUT="${1:-${GITHUB_REF_NAME:-}}"

# Non-tag contexts (e.g. workflow_dispatch on a branch) should be a no-op.
if [ -z "$TAG_INPUT" ]; then
  echo "No tag provided; skipping version sync."
  exit 0
fi

# Allow either vX.Y.Z or refs/tags/vX.Y.Z inputs.
TAG="$TAG_INPUT"
if echo "$TAG" | grep -qE '^refs/tags/v[0-9]+\.[0-9]+\.[0-9]+$'; then
  TAG="${TAG#refs/tags/}"
fi

if ! echo "$TAG" | grep -qE '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "Error: invalid release tag format. Expected v<semver> or refs/tags/v<semver>, got: $TAG_INPUT"
  exit 1
fi

NEW="${TAG#v}"
DIR="$(cd "$(dirname "$0")/.." && pwd)"

"$DIR/scripts/bump-version.sh" "$NEW"

echo "Synchronized versions from tag $TAG"
