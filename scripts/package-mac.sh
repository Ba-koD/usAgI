#!/usr/bin/env bash
# usAgI macOS 앱 묶기
#   결과: dist/usAgI.app, dist/release/usAgI-mac.zip (+ .sha256)
#   직접 설치, Homebrew, 설치 스크립트와 자동 업데이트가 같은 zip을 사용.
#   x86_64 타깃도 설치돼 있으면 Intel+Apple Silicon 유니버설로 만듦
#   (rustup target add x86_64-apple-darwin)
#   ARM·Intel 바이너리 경로를 인자로 주면 옆의 usagi-settings와 함께 묶음.
set -euo pipefail
cd "$(dirname "$0")/.."

VER=$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')
SETTINGS_BINS=()
if [ "$#" -eq 0 ]; then
  TARGETS=(aarch64-apple-darwin)
  if rustup target list --installed 2>/dev/null | grep -q '^x86_64-apple-darwin$'; then
    TARGETS+=(x86_64-apple-darwin)
  fi
  ARGS=()
  [ -n "${CI:-}" ] && ARGS+=(--locked)
  BINS=()
  for t in "${TARGETS[@]}"; do
    ARGS+=(--target "$t")
    BINS+=("${CARGO_TARGET_DIR:-target}/$t/dist/usagi-mac")
    SETTINGS_BINS+=("${CARGO_TARGET_DIR:-target}/$t/settings-release/usagi-settings")
  done
  cargo build --profile dist -p usagi-mac "${ARGS[@]}"
  cargo build --profile settings-release -p usagi-settings --features external-fonts "${ARGS[@]}"
elif [ "$#" -eq 2 ]; then
  BINS=("$@")
  for bin in "${BINS[@]}"; do
    SETTINGS_BINS+=("$(dirname "$bin")/usagi-settings")
  done
else
  echo "사용: bash scripts/package-mac.sh [<ARM 바이너리> <Intel 바이너리>]" >&2
  exit 1
fi

for bin in "${BINS[@]}" "${SETTINGS_BINS[@]}"; do
  [ -s "$bin" ] || { echo "macOS 바이너리 없음: $bin" >&2; exit 1; }
done

APP=dist/usAgI.app
EXE="$APP/Contents/MacOS/usAgI"
SETTINGS="$APP/Contents/MacOS/usagi-settings"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
mkdir -p "$APP/Contents/Resources/Fonts"
cp crates/usagi-settings/ui/fonts/UsagiUI-*.otf "$APP/Contents/Resources/Fonts/"
if [ "${#BINS[@]}" -gt 1 ]; then
  lipo -create "${BINS[@]}" -output "$EXE"
  lipo -create "${SETTINGS_BINS[@]}" -output "$SETTINGS"
  for bin in "$EXE" "$SETTINGS"; do
    archs=" $(lipo -archs "$bin") "
    [[ "$archs" == *" arm64 "* && "$archs" == *" x86_64 "* ]] || {
      echo "macOS 유니버설 바이너리 아님: $bin ($archs)" >&2
      exit 1
    }
  done
else
  cp "${BINS[0]}" "$EXE"
  cp "${SETTINGS_BINS[0]}" "$SETTINGS"
fi
# Actions 아티팩트를 내려받으면 실행 권한이 빠짐.
chmod +x "$EXE" "$SETTINGS"

# 앱 아이콘: 앱과 같은 토끼 실루엣으로 .icns
ICONSET="$(mktemp -d)/usAgI.iconset"
"$EXE" --iconset "$ICONSET"
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
codesign --force -s "${MAC_SIGN_IDENTITY:--}" "$SETTINGS"
codesign --force -s "${MAC_SIGN_IDENTITY:--}" "$APP"
codesign --verify --deep --strict "$APP"

REL=dist/release
mkdir -p "$REL"
ZIP="$REL/usAgI-mac.zip"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"
(cd "$REL" && shasum -a 256 usAgI-mac.zip | cut -d' ' -f1 > usAgI-mac.zip.sha256)

echo "macOS 앱: $APP"
echo "압축 파일: $ZIP"
