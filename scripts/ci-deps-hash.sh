#!/usr/bin/env bash
# CI 의존성 지문: 외부 의존성·기능·빌드 프로필·Rust 버전이 그대로면 같은 값 (우리 버전 번호는 뺌)
#   사용: bash scripts/ci-deps-hash.sh [구분자(예: 빌드 대상)]
# 릴리스 워크플로가 이 값으로 "의존성 캐시가 이미 최신인지" 확인하고, 그대로면 큰 캐시를 다시 올리지 않음
set -euo pipefail
cd "$(dirname "$0")/.."
# macOS 기본 도구는 shasum
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi; }
{
  # Cargo.lock: 출처(source)가 없는 항목 = 이 저장소의 크레이트 → 버전 줄만 빼고 의존 목록은 남김
  # (Windows 체크아웃의 CRLF는 먼저 지움: 빈 줄로 항목을 나누므로)
  tr -d '\r' < Cargo.lock | awk 'BEGIN { RS = ""; ORS = "\n\n" } !/\nsource = / { gsub(/\nversion = "[^"]*"/, "") } { print }'
  # 모든 Cargo.toml: 의존성·기능·프로필 (워크스페이스 버전 줄만 뺌)
  find . -name Cargo.toml -not -path './target/*' | LC_ALL=C sort | while read -r f; do
    echo "== $f"
    tr -d '\r' < "$f" | grep -v '^version = "' || true
  done
  # 빌드 플래그(rustflags)가 바뀌면 모든 의존성을 다시 빌드하므로 지문에 포함
  if [ -f .cargo/config.toml ]; then
    echo "== .cargo/config.toml"
    tr -d '\r' < .cargo/config.toml
  fi
  rustc -V
  echo "${1:-}"
} | sha | cut -c1-16
