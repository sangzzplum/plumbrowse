#!/usr/bin/env bash
# Mac: сохранить всё и отправить на GitHub.
# Использование:
#   ./scripts/push.sh
#   ./scripts/push.sh "мой комментарий"

set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
MSG="${1:-PlumBrowser v${VERSION}}"

echo "→ git add..."
git add -A
git status --short

if git diff --cached --quiet; then
  echo "Нечего коммитить — только push."
else
  echo "→ git commit..."
  git commit -m "$MSG"
fi

echo "→ git push..."
git push origin main

echo ""
echo "Готово. На Windows: scripts\\pull.cmd"
