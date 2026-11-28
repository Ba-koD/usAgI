#!/usr/bin/env bash
# usAgI macOS 설치 (한 줄):
#   curl -fsSL https://raw.githubusercontent.com/Ba-koD/usAgI/main/scripts/install-mac.sh | bash
#
# 처음 설치용이에요. 설치한 뒤 업데이트는 앱의 자동 업데이트(설정 → 정보)를 쓰세요. 앱은 들어 있는 공개 키로
# 서명을 늘 확인하지만, 이 스크립트는 minisign이 있을 때만 서명을 확인해요.
#
# 확인하는 것
#   - SHA-256: 릴리스 본문(이전 릴리스는 첨부)의 값과 비교. 파일과 같은 릴리스에서 온 값이라 릴리스를 고칠 수 있는
#     사람이 함께 바꿀 수 있어요 (내려받다 깨진 파일만 걸러 냄).
#   - 서명: minisign이 있으면(brew install minisign) 아래 고정 공개 키로 서명과 서명 안의 버전·파일 이름을 확인.
#     없으면 서명을 확인하지 못했다고 알리고 설치해요. 그 설치는 지금 받은 파일을 그대로 믿는 설치(trust-on-first-use).
#     USAGI_REQUIRE_SIGNATURE=1이면 서명을 확인하지 못할 때 설치하지 않아요.
#
# 스크립트 전체를 함수로 감싸 마지막 줄에서 불러요. curl | bash가 중간에 끊기면 아무것도 실행하지 않아서,
# 기존 앱을 지운 뒤 새 앱을 복사하기 전에 멈추는 일이 없어요. (heredoc 때문에 함수 안을 들여쓰지 않음)
#
# curl로 받은 파일에는 브라우저와 달리 "인터넷에서 받은 파일" 격리 표시가 붙지 않아서
# "확인되지 않은 개발자" 경고 없이 바로 열려요.
#
# 환경 변수
#   USAGI_REPO                 받을 저장소 (기본 Ba-koD/usAgI)
#   USAGI_DEST                 설치 폴더 (기본 /Applications, 쓸 수 없으면 ~/Applications)
#   USAGI_ZIP                  이미 받아 둔 zip으로 설치 (시험용, 체크섬·서명 확인 없음)
#   USAGI_REQUIRE_SIGNATURE=1  minisign으로 서명을 확인하지 못하면 설치하지 않음

# 업데이트 서명 공개 키 (packaging/usagi-update.pub와 같아야 함, scripts/check-update-key.sh가 확인)
USAGI_PUBKEY='RWQ0BBRb5+kzbsZhlUlizHXXUSqgW3r2epPcN4zPTxHERCgqDw+cdo06'

# 서명을 확인하지 못했을 때: 알리고 계속 (USAGI_REQUIRE_SIGNATURE=1이면 중단)
signature_unverified() {
  if [ "${USAGI_REQUIRE_SIGNATURE:-}" = 1 ]; then
    echo "서명 확인 불가: $1. USAGI_REQUIRE_SIGNATURE=1이라 설치 중단." >&2
    exit 1
  fi
  echo "주의: 서명을 확인하지 못함 ($1)." >&2
  echo "  같은 릴리스의 SHA-256만 확인했으므로 이번 설치는 지금 받은 파일을 그대로 믿는 설치(trust-on-first-use)입니다." >&2
  echo "  설치한 뒤의 업데이트는 앱에 들어 있는 공개 키로 서명을 확인합니다." >&2
}

# minisign으로 서명 확인: 고정 공개 키로 만든 서명이고, 서명 안의 설명이 이 버전·이 파일이어야 함 (앱과 같은 규칙)
verify_signature() {
  local file="$1" signature="$2" version="$3" comment
  if ! command -v minisign >/dev/null 2>&1; then
    signature_unverified "minisign 없음, brew install minisign으로 설치 가능"
    return
  fi
  if ! comment="$(minisign -V -Q -P "$USAGI_PUBKEY" -x "$signature" -m "$file")"; then
    echo "서명 확인 실패. 설치 중단." >&2
    exit 1
  fi
  comment="${comment#Trusted comment: }"
  if [ "$comment" != "usAgI ${version} usAgI-mac.zip" ]; then
    echo "다른 버전이나 다른 파일의 서명 (${comment}). 설치 중단." >&2
    exit 1
  fi
  echo "서명 확인: ${comment}"
}

main() {
set -euo pipefail

REPO="${USAGI_REPO:-Ba-koD/usAgI}"
DEST="${USAGI_DEST:-/Applications}"
mkdir -p "$DEST" 2>/dev/null || true
if [ ! -w "$DEST" ]; then
  # /Applications에 쓸 수 없는 계정이면 내 폴더의 Applications로
  DEST="$HOME/Applications"
  mkdir -p "$DEST"
fi
DEST="$(cd "$DEST" && pwd -P)"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

if [ -n "${USAGI_ZIP:-}" ]; then
  cp "$USAGI_ZIP" "$TMP/usAgI-mac.zip"
else
  [[ "$REPO" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || { echo "저장소 이름 형식 오류" >&2; exit 1; }
  CURL=(curl -fL --proto '=https' --proto-redir '=https' --tlsv1.2)
  # ZIP·체크섬·서명은 같은 릴리스 응답에서 고름. 설치 중에 최신 버전이 바뀌어도 섞이지 않음.
  "${CURL[@]}" -sS --max-filesize 1048576 -H 'Accept: application/vnd.github+json' \
    "https://api.github.com/repos/${REPO}/releases/latest" -o "$TMP/release.json"
  # 출력: 1줄 ZIP 주소, 2줄 SHA-256(또는 체크섬 첨부 주소), 3줄 버전, 4줄 서명(본문에서 파일로 꺼냈으면 file, 첨부면 주소)
  VERIFICATION="$(/usr/bin/osascript -l JavaScript - "$TMP/release.json" "$REPO" "$TMP/usAgI-mac.zip.minisig" <<'JXA'
ObjC.import('Foundation');

function run(argv) {
    const text = $.NSString.stringWithContentsOfFileEncodingError(argv[0], $.NSUTF8StringEncoding, null);
    const release = JSON.parse(ObjC.unwrap(text));
    const name = 'usAgI-mac.zip';
    if (!/^v?\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?$/.test(release.tag_name)) {
        throw new Error('릴리스 버전 형식 오류');
    }
    const version = release.tag_name.replace(/^v/, '');
    const prefix = 'https://github.com/' + argv[1] + '/releases/download/' + release.tag_name + '/';
    const findAsset = (filename) => {
        const asset = release.assets.find(item => item.name === filename);
        if (!asset) {
            return null;
        }
        if (typeof asset.browser_download_url !== 'string' ||
            asset.browser_download_url.toLowerCase() !== (prefix + filename).toLowerCase()) {
            throw new Error('릴리스 파일 확인 실패: ' + filename);
        }
        return asset.browser_download_url;
    };
    const assetUrl = (filename) => {
        const url = findAsset(filename);
        if (!url) {
            throw new Error('릴리스 파일 확인 실패: ' + filename);
        }
        return url;
    };
    // 앱의 update.rs fenced_text와 같은 규칙: 제목 줄 다음의 빈 줄이 아닌 첫 줄이 ```text, ```까지가 내용
    const fenced = (block, heading) => {
        const at = block.indexOf(heading);
        if (at < 0) {
            return null;
        }
        const opening = block.findIndex((line, index) => index > at && line.trim() !== '');
        const closing = opening < 0 ? -1 : block.indexOf('```', opening + 1);
        if (block[opening] !== '```text' || closing < 0) {
            throw new Error('릴리스 본문의 검증 정보 형식 오류: ' + heading);
        }
        return block.slice(opening + 1, closing);
    };

    const lines = (release.body || '').split(/\r?\n/);
    const startMarker = '<!-- usagi-verification:start -->';
    const endMarker = '<!-- usagi-verification:end -->';
    const start = lines.indexOf(startMarker);
    const end = lines.indexOf(endMarker);
    const url = assetUrl(name);
    if (start === -1 && end === -1) {
        return [url, assetUrl(name + '.sha256'), version, findAsset(name + '.minisig') || 'none'].join('\n');
    }
    if (start === -1 || end <= start || start !== lines.lastIndexOf(startMarker) || end !== lines.lastIndexOf(endMarker)) {
        throw new Error('릴리스 본문의 검증 정보 형식 오류');
    }
    const block = lines.slice(start + 1, end);
    const checksums = fenced(block, '### SHA-256');
    if (!checksums) {
        throw new Error('릴리스 본문의 체크섬 형식 오류');
    }
    const matches = checksums
        .map(line => line.trim().split(/\s+/))
        .filter(fields => fields[1] === name);
    if (matches.length !== 1 || matches[0].length !== 2 || !/^[0-9a-f]{64}$/i.test(matches[0][0])) {
        throw new Error('릴리스 본문의 Mac 체크섬 오류');
    }
    let signature = 'none';
    const signatureLines = fenced(block, '**`' + name + '.minisig`**');
    if (signatureLines) {
        const value = signatureLines.join('\n') + '\n';
        if (!value.trim()) {
            throw new Error('릴리스 본문의 서명 형식 오류');
        }
        const saved = $.NSString.alloc.initWithUTF8String(value)
            .writeToFileAtomicallyEncodingError(argv[2], true, $.NSUTF8StringEncoding, null);
        if (!saved) {
            throw new Error('서명 저장 실패');
        }
        signature = 'file';
    }
    return [url, matches[0][0].toLowerCase(), version, signature].join('\n');
}
JXA
)"
  URL="$(sed -n 1p <<<"$VERIFICATION")"
  WANT="$(sed -n 2p <<<"$VERIFICATION")"
  VERSION="$(sed -n 3p <<<"$VERIFICATION")"
  SIGNATURE="$(sed -n 4p <<<"$VERIFICATION")"
  if [[ "$WANT" == https://* ]]; then
    WANT="$("${CURL[@]}" -sS --max-filesize 65536 "$WANT" | awk 'NR == 1 {print tolower($1)}')"
  fi
  echo "usAgI ${VERSION} 내려받는 중… ($URL)"
  "${CURL[@]}" --progress-bar "$URL" -o "$TMP/usAgI-mac.zip"
  GOT="$(shasum -a 256 "$TMP/usAgI-mac.zip" | awk '{print $1}')"
  if [ "${#WANT}" -ne 64 ] || [ "$WANT" != "$GOT" ]; then
    echo "체크섬 불일치. 잠시 뒤 다시 시도 필요." >&2
    exit 1
  fi
  if [[ "$SIGNATURE" == https://* ]]; then
    "${CURL[@]}" -sS --max-filesize 65536 "$SIGNATURE" -o "$TMP/usAgI-mac.zip.minisig"
  elif [ "$SIGNATURE" != file ]; then
    # 앱도 서명 없는 릴리스는 받지 않음
    echo "릴리스에 서명 없음. 설치 중단." >&2
    exit 1
  fi
  verify_signature "$TMP/usAgI-mac.zip" "$TMP/usAgI-mac.zip.minisig" "$VERSION"
fi
ditto -x -k "$TMP/usAgI-mac.zip" "$TMP"
[ -d "$TMP/usAgI.app" ] || { echo "압축 안에 usAgI.app 없음" >&2; exit 1; }
APP="$TMP/usAgI.app"
NATIVE_APP="$TMP/native/usAgI.app"
if /usr/bin/ditto --arch "$(uname -m)" "$APP" "$NATIVE_APP" 2>/dev/null \
    && /usr/bin/codesign --verify --deep --strict "$NATIVE_APP" 2>/dev/null; then
  APP="$NATIVE_APP"
fi

# 이 설치 경로에서 실행 중인 앱만 끄고 바꿈.
for pid in $(pgrep -x usAgI || true); do
  if [ "$(ps -p "$pid" -o comm=)" = "$DEST/usAgI.app/Contents/MacOS/usAgI" ]; then
    kill "$pid" 2>/dev/null || true
  fi
done
sleep 1
# A legacy portable directory inside the bundle would be replaced with it.
legacy="$DEST/usAgI.app/Contents/MacOS"
legacy_data="$legacy/data"
if [ -d "$legacy_data" ] && { [ -f "$legacy/usagi.portable" ] || [ -f "$legacy_data/usagi.portable" ] || [ -f "$legacy_data/config.toml" ] || [ -f "$legacy_data/state.json" ]; }; then
  if [ -f "$APP/Contents/MacOS/usagi-settings" ]; then
    portable="$DEST/usAgI.data"
    if [ -e "$portable" ] || [ -L "$portable" ]; then
      echo "기존 포터블 데이터를 옮길 폴더가 이미 있습니다: $portable" >&2
      exit 1
    fi
    touch "$legacy_data/usagi.portable"
    mv -n "$legacy_data" "$portable"
    [ ! -d "$legacy_data" ] || { echo "포터블 데이터 이동 실패: $legacy_data" >&2; exit 1; }
  else
    # Older bundles look for data inside Contents/MacOS.
    ditto "$legacy_data" "$APP/Contents/MacOS/data"
    touch "$APP/Contents/MacOS/data/usagi.portable"
  fi
fi
rm -rf "$DEST/usAgI.app"
ditto "$APP" "$DEST/usAgI.app"
# curl은 격리 표시를 붙이지 않지만, 다른 경로로 받은 zip일 때를 위해 한 번 더 정리
xattr -dr com.apple.quarantine "$DEST/usAgI.app" 2>/dev/null || true

open "$DEST/usAgI.app"
echo "설치 완료: $DEST/usAgI.app"
echo "메뉴 막대 오른쪽 위의 사용량 숫자 클릭. 업데이트는 앱이 서명을 확인해 자동으로 설치 (설정 → 정보)."
}

main "$@"
