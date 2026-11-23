#!/usr/bin/env bash
# 릴리스된 macOS zip으로 Homebrew 탭(<owner>/homebrew-usagi)의 Casks/usagi.rb 갱신
#   사용: TAP_TOKEN=... scripts/update-homebrew-tap.sh v0.3.0 dist/release/usAgI-mac.zip
#   GitHub Actions에서는 저장소 비밀값 HOMEBREW_TAP_TOKEN(탭 저장소에 쓰기 권한)이 있을 때만 실행돼요.
set -euo pipefail

TAG="$1"
ZIP="$2"
VER="${TAG#v}"
REPO="${GITHUB_REPOSITORY:-Ba-koD/usAgI}"
OWNER="${REPO%%/*}"
TAP="${OWNER}/homebrew-usagi"
if command -v sha256sum >/dev/null 2>&1; then
  SHA="$(sha256sum "$ZIP" | cut -d' ' -f1)"
else
  SHA="$(shasum -a 256 "$ZIP" | cut -d' ' -f1)"
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
# 토큰은 주소에 넣지 않고 요청 헤더로만 보냄 (주소는 오류 메시지·.git/config에 남을 수 있음)
B64="$(printf 'x-access-token:%s' "$TAP_TOKEN" | base64 | tr -d '\n')"
# GitHub Actions는 비밀 값 자체만 가려 주므로, base64로 바꾼 값도 로그에서 가리게 등록
if [ -n "${GITHUB_ACTIONS:-}" ]; then echo "::add-mask::${B64}"; fi
AUTH="AUTHORIZATION: basic ${B64}"
git -c "http.https://github.com/.extraheader=${AUTH}" clone --depth 1 "https://github.com/${TAP}.git" "$WORK/tap"
mkdir -p "$WORK/tap/Casks"
sed -e "s|{{VERSION}}|${VER}|g" -e "s|{{SHA256}}|${SHA}|g" -e "s|{{REPO}}|${REPO}|g" \
  packaging/homebrew/usagi.rb.template > "$WORK/tap/Casks/usagi.rb"

cd "$WORK/tap"
git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
git add Casks/usagi.rb
if git diff --cached --quiet; then
  echo "탭이 이미 최신"
  exit 0
fi
git commit -m "Chore: Update usagi cask version" -m "- usagi ${VER} 반영"
git -c "http.https://github.com/.extraheader=${AUTH}" push origin HEAD
echo "탭 갱신: ${TAP} → ${VER}"
