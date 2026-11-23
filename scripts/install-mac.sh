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
  [[ "$REPO" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || { echo "저장소 이름 형식 오류" >&2; exit 1; }
  CURL=(curl -fL --proto '=https' --proto-redir '=https' --tlsv1.2)
  # ZIP과 체크섬은 같은 릴리스 응답에서 고름. 설치 중에 최신 버전이 바뀌어도 섞이지 않음.
  "${CURL[@]}" -sS -H 'Accept: application/vnd.github+json' \
    "https://api.github.com/repos/${REPO}/releases/latest" -o "$TMP/release.json"
  VERIFICATION="$(/usr/bin/osascript -l JavaScript - "$TMP/release.json" "$REPO" <<'JXA'
ObjC.import('Foundation');

function run(argv) {
    const text = $.NSString.stringWithContentsOfFileEncodingError(argv[0], $.NSUTF8StringEncoding, null);
    const release = JSON.parse(ObjC.unwrap(text));
    const name = 'usAgI-mac.zip';
    if (!/^v?\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?$/.test(release.tag_name)) {
        throw new Error('릴리스 버전 형식 오류');
    }
    const prefix = 'https://github.com/' + argv[1] + '/releases/download/' + release.tag_name + '/';
    const assetUrl = (filename) => {
        const asset = release.assets.find(item => item.name === filename);
        if (!asset || typeof asset.browser_download_url !== 'string' ||
            asset.browser_download_url.toLowerCase() !== (prefix + filename).toLowerCase()) {
            throw new Error('릴리스 파일 확인 실패: ' + filename);
        }
        return asset.browser_download_url;
    };

    const lines = (release.body || '').split(/\r?\n/);
    const startMarker = '<!-- usagi-verification:start -->';
    const endMarker = '<!-- usagi-verification:end -->';
    const start = lines.indexOf(startMarker);
    const end = lines.indexOf(endMarker);
    const url = assetUrl(name);
    if (start === -1 && end === -1) {
        return url + '\n' + assetUrl(name + '.sha256');
    }
    if (start === -1 || end <= start || start !== lines.lastIndexOf(startMarker) || end !== lines.lastIndexOf(endMarker)) {
        throw new Error('릴리스 본문의 검증 정보 형식 오류');
    }
    const block = lines.slice(start + 1, end);
    const heading = block.indexOf('### SHA-256');
    const opening = block.findIndex((line, index) => index > heading && line.trim() !== '');
    const closing = block.indexOf('```', opening + 1);
    if (heading < 0 || block[opening] !== '```text' || closing < 0) {
        throw new Error('릴리스 본문의 체크섬 형식 오류');
    }
    const matches = block.slice(opening + 1, closing)
        .map(line => line.trim().split(/\s+/))
        .filter(fields => fields[1] === name);
    if (matches.length !== 1 || matches[0].length !== 2 || !/^[0-9a-f]{64}$/i.test(matches[0][0])) {
        throw new Error('릴리스 본문의 Mac 체크섬 오류');
    }
    return url + '\n' + matches[0][0].toLowerCase();
}
JXA
)"
  URL="${VERIFICATION%%$'\n'*}"
  WANT="${VERIFICATION#*$'\n'}"
  if [[ "$WANT" == https://* ]]; then
    WANT="$("${CURL[@]}" -sS "$WANT" | awk 'NR == 1 {print tolower($1)}')"
  fi
  echo "usAgI 내려받는 중… ($URL)"
  "${CURL[@]}" --progress-bar "$URL" -o "$TMP/usAgI-mac.zip"
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
