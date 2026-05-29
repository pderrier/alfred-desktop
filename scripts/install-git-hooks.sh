#!/usr/bin/env bash
# install-git-hooks.sh — install repo git hooks into the active hooks dir.
# Run once after clone / submodule init. Idempotent.
set -euo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)"
HOOKS_DIR="$(git -C "$DIR" rev-parse --git-path hooks)"
mkdir -p "$HOOKS_DIR"

for hook in "$DIR"/scripts/git-hooks/*; do
  name="$(basename "$hook")"
  cp "$hook" "$HOOKS_DIR/$name"
  chmod +x "$HOOKS_DIR/$name"
  echo "installed $name → $HOOKS_DIR/$name"
done
