#!/usr/bin/env bash
# usAgI macOS 앱 묶기
#   결과: dist/usAgI.app, dist/usAgI-mac-<버전>.zip
#         dist/release/  GitHub 릴리스에 그대로 올릴 파일
#           usAgI-mac-<버전>.zip         Homebrew Cask가 받는 파일
#           usAgI-mac.zip (+ .sha256)    설치 스크립트·앱 자동 업데이트가 latest/download 로 받는 고정 이름
#   x86_64 타깃도 설치돼 있으면 Intel+Apple Silicon 유니버설로 만듦
#   (rustup target add x86_64-apple-darwin)
set -euo pipefail
cd "$(dirname "$0")/.."

VER=$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')
TARGETS=(aarch64-apple-darwin)
if rustup target list --installed 2>/dev/null | grep -q '^x86_64-apple-darwin$'; then
  TARGETS+=(x86_64-apple-darwin)
fi

# 여러 타깃을 한 번의 cargo 호출로 (cargo가 동시에 컴파일). CI에서는 Cargo.lock 그대로.
ARGS=(--release -p usagi-mac)
[ -n "${CI:-}" ] && ARGS+=(--locked)
BINS=()
for t in "${TARGETS[@]}"; do
  ARGS+=(--target "$t")
  BINS+=("target/$t/release/usagi-mac")
done
cargo build "${ARGS[@]}"

APP=dist/usAgI.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
if [ "${#BINS[@]}" -gt 1 ]; then
  lipo -create "${BINS[@]}" -output "$APP/Contents/MacOS/usAgI"
else
  cp "${BINS[0]}" "$APP/Contents/MacOS/usAgI"
fi

# 앱 아이콘: 앱과 같은 토끼 실루엣으로 .icns
ICONSET="$(mktemp -d)/usAgI.iconset"
"$APP/Contents/MacOS/usAgI" --iconset "$ICONSET"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/usAgI.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>usAgI</string>
  <key>CFBundleDisplayName</key><string>usAgI</string>
  <key>CFBundleIdentifier</key><string>dev.usagi.usagi</string>
  <key>CFBundleExecutable</key><string>usAgI</string>
  <key>CFBundleIconFile</key><string>usAgI</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VER}</string>
  <key>CFBundleVersion</key><string>${VER}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# 서명 (개발자 인증서가 없으면 임시 서명. 처음 열 때 우클릭 → 열기 필요)
codesign --force --deep -s "${MAC_SIGN_IDENTITY:--}" "$APP"

ZIP="usAgI-mac-${VER}.zip"
(cd dist && rm -f "$ZIP" && ditto -c -k --keepParent usAgI.app "$ZIP")
REL=dist/release
rm -rf "$REL"
mkdir -p "$REL"
cp "dist/$ZIP" "$REL/$ZIP"
cp "dist/$ZIP" "$REL/usAgI-mac.zip"
(cd "$REL" && shasum -a 256 usAgI-mac.zip | cut -d' ' -f1 > usAgI-mac.zip.sha256)

echo "macOS 앱: $APP"
echo "압축 파일: dist/$ZIP"
echo "릴리스 파일: $REL"
