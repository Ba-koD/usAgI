#!/usr/bin/env bash
# usAgI macOS 설치 (한 줄):
#   curl -fsSL https://raw.githubusercontent.com/Ba-koD/usAgI/main/scripts/install-mac.sh | bash
#
# curl로 받은 파일에는 브라우저와 달리 "인터넷에서 받은 파일" 격리 표시가 붙지 않아서
# "확인되지 않은 개발자" 경고 없이 바로 열려요.
#
# 환경 변수
#   USAGI_REPO   받을 저장소 (기본 Ba-koD/usAgI)
#   USAGI_DEST   설치 폴더 (기본 /Applications, 쓸 수 없으면 ~/Applications)
#   USAGI_ZIP    이미 받아 둔 zip으로 설치 (시험용)
set -euo pipefail

REPO="${USAGI_REPO:-Ba-koD/usAgI}"
URL="https://github.com/${REPO}/releases/latest/download/usAgI-mac.zip"
DEST="${USAGI_DEST:-/Applications}"
mkdir -p "$DEST" 2>/dev/null || true
if [ ! -w "$DEST" ]; then
  # /Applications에 쓸 수 없는 계정이면 내 폴더의 Applications로
  DEST="$HOME/Applications"
  mkdir -p "$DEST"
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

if [ -n "${USAGI_ZIP:-}" ]; then
  cp "$USAGI_ZIP" "$TMP/usAgI-mac.zip"
else
  echo "usAgI 내려받는 중… ($URL)"
  # https만 (리다이렉트도), 받은 뒤 릴리스의 체크섬과 비교
  CURL=(curl -fL --proto '=https' --proto-redir '=https' --tlsv1.2)
  "${CURL[@]}" --progress-bar "$URL" -o "$TMP/usAgI-mac.zip"
  WANT="$("${CURL[@]}" -sS "$URL.sha256" | awk '{print tolower($1)}')"
  GOT="$(shasum -a 256 "$TMP/usAgI-mac.zip" | awk '{print $1}')"
  if [ "${#WANT}" -ne 64 ] || [ "$WANT" != "$GOT" ]; then
    echo "체크섬 불일치. 잠시 뒤 다시 시도 필요." >&2
    exit 1
  fi
fi
ditto -x -k "$TMP/usAgI-mac.zip" "$TMP"
[ -d "$TMP/usAgI.app" ] || { echo "압축 안에 usAgI.app 없음" >&2; exit 1; }

# 실행 중이면 끄고 바꿈
pkill -f "usAgI.app/Contents/MacOS/usAgI" 2>/dev/null || true
sleep 1
rm -rf "$DEST/usAgI.app"
ditto "$TMP/usAgI.app" "$DEST/usAgI.app"
# curl은 격리 표시를 붙이지 않지만, 다른 경로로 받은 zip일 때를 위해 한 번 더 정리
xattr -dr com.apple.quarantine "$DEST/usAgI.app" 2>/dev/null || true

open "$DEST/usAgI.app"
echo "설치 완료: $DEST/usAgI.app"
echo "메뉴 막대 오른쪽 위의 사용량 숫자 클릭. 업데이트: 이 명령 다시 실행."
