#!/usr/bin/env bash
# 업데이트 서명 공개 키 고정 확인 (CI·릴리스 워크플로가 실행)
#
#   packaging/usagi-update.pub의 키가 아래 고정 값과 같은지, 설치 스크립트에 넣어 둔 키도 같은지 확인해요.
#   공개 키를 바꾸면 그 뒤로 설치·업데이트되는 앱은 새 키의 서명만 믿어요. 그래서 키 교체는 공개 키 파일 한 줄만이
#   아니라 이 파일과 설치 스크립트까지 함께 고쳐야 통과하게 해서, 리뷰 없이 키가 바뀌는 일을 눈에 띄게 만들어요.
#   빈 공개 키 파일도 실패예요 (서명 없는 릴리스는 게시하지 않음).
#
#   키 교체 절차: docs/DEVELOPMENT.md의 "서명 키 교체"
set -euo pipefail
cd "$(dirname "$0")/.."

PINNED='RWQ0BBRb5+kzbsZhlUlizHXXUSqgW3r2epPcN4zPTxHERCgqDw+cdo06'

fail() {
  echo "::error::$1" >&2
  exit 1
}

# 앱(update.rs의 key_line)과 같은 규칙: 설명 줄·주석·빈 줄을 뺀 나머지가 키
keys="$(tr -d '\r' < packaging/usagi-update.pub | grep -vE '^[[:space:]]*(untrusted comment:.*|#.*)?$' | sed -E 's/^[[:space:]]+|[[:space:]]+$//g' || true)"
[ -n "$keys" ] || fail "packaging/usagi-update.pub에 공개 키 없음 (서명 없는 릴리스는 게시하지 않음)"
[ "$(printf '%s\n' "$keys" | wc -l | tr -d ' ')" = 1 ] || fail "packaging/usagi-update.pub에 키 줄이 여러 개"
[ "$keys" = "$PINNED" ] || fail "packaging/usagi-update.pub의 키($keys)가 scripts/check-update-key.sh의 고정 값과 다름"

grep -qF "USAGI_PUBKEY='$PINNED'" scripts/install-mac.sh || fail "scripts/install-mac.sh의 USAGI_PUBKEY가 고정 값과 다름"
tr -d '\r' < scripts/install-windows.ps1 | grep -qF "\$publicKey = '$PINNED'" \
  || fail "scripts/install-windows.ps1의 \$publicKey가 고정 값과 다름"

echo "업데이트 공개 키 확인: $PINNED"
