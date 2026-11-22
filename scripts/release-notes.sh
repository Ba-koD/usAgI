#!/usr/bin/env bash
# CHANGELOG.md에서 한 버전의 섹션만 꺼냄 (GitHub 릴리스 노트·앱의 업데이트 안내에 쓰임)
#   사용: scripts/release-notes.sh 0.1.0     (v0.1.0도 돼요)
#   섹션이 없으면 "usAgI v<버전>" 한 줄
set -euo pipefail
cd "$(dirname "$0")/.."

VER="${1:?버전 필요 (예: 0.1.0)}"
VER="${VER#v}"

notes="$(tr -d '\r' < CHANGELOG.md | awk -v head="## [${VER}]" '
  /^## \[/ { if (inside) exit; inside = (index($0, head) == 1); next }
  inside { print }
' | sed -e '/./,$!d')"

if [ -z "$notes" ]; then
  echo "usAgI v${VER}"
else
  printf '%s\n' "$notes"
fi
