# usAgI 포터블 패키지 만들기
#   결과: dist\usAgI-portable\  (usagi.exe + data\ + 읽어보기.txt)
#         dist\usAgI-portable-<버전>.zip
#         dist\release\       GitHub 릴리스에 그대로 올릴 파일 (usagi.exe, usagi.exe.sha256, 포터블 zip)
# 사용: powershell -ExecutionPolicy Bypass -File scripts\package-portable.ps1

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    # CI에서는 Cargo.lock 그대로 (바뀌면 실패)
    $extra = @()
    if ($env:CI) { $extra += '--locked' }
    cargo build --release -p usagi-win $extra
    if ($LASTEXITCODE -ne 0) { throw "빌드 실패" }

    $ver = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
    $dist = Join-Path $root 'dist'
    $out = Join-Path $dist 'usAgI-portable'
    if (Test-Path $out) { Remove-Item $out -Recurse -Force }
    New-Item -ItemType Directory -Force $out | Out-Null

    Copy-Item (Join-Path $root 'target\release\usagi.exe') $out
    # exe 옆에 data 폴더가 있으면 포터블 모드 (zip은 빈 폴더를 빼므로 안내 파일을 하나 넣어 둠)
    $data = Join-Path $out 'data'
    New-Item -ItemType Directory -Force $data | Out-Null
    Set-Content -Encoding utf8 (Join-Path $data 'usagi.portable') 'usAgI portable data folder'
    Set-Content -Encoding utf8 (Join-Path $data '읽어보기.txt') 'usAgI 포터블 데이터 폴더. 설정과 받은 사용량 저장 위치. 계정을 추가하면 profiles 폴더에 공식 CLI의 로그인 파일이 생기므로 이 폴더를 다른 사람에게 주거나 공유 폴더에 올리지 말 것. 이 폴더를 지우면 일반 모드(%APPDATA%\usAgI)로 전환.'
    Set-Content -Encoding utf8 (Join-Path $out '읽어보기.txt') @"
usAgI $ver (포터블)

실행
  usagi.exe 더블클릭: 작업 표시줄 위젯과 알림 영역 아이콘 표시. 설치 불필요.
  위젯 더블클릭: 설정 창. 위젯 우클릭: 퍼센트 / 남은 시간 전환.

저장 위치
  usagi.exe 옆에 data 폴더가 있으면 포터블 모드. 모든 설정과 받은 사용량을 data 폴더에 저장.
  폴더째 이동 가능. data 폴더를 지우면 일반 모드로 전환, %APPDATA%\usAgI 사용.
  usAgI에서 계정을 추가하면 data\profiles 폴더에 공식 CLI의 로그인 파일이 생김.
  data 폴더를 압축해 다른 사람에게 주거나 공유·동기화 폴더에 두면 로그인 정보가 함께 넘어가므로 주의.

필요한 것
  - Windows 10 / 11
  - 공식 Claude Code CLI / Codex CLI (로그인과 사용량 확인용. usAgI는 토큰 저장 없음)

참고
  - Windows 알림용으로 앱 이름만 사용자 레지스트리에 등록
    (HKCU\Software\Classes\AppUserModelId\usAgI.UsageRabbit). 포터블 모드에서는 시작 메뉴 바로 가기 미생성.
  - "Windows 시작 시 실행"을 켜면 작업 스케줄러에 로그온 작업(usAgI)으로 이 exe 경로 등록.
    폴더를 옮긴 뒤에는 새 위치에서 한 번 실행하면 등록 경로도 갱신.
  - Claude Code 실시간 연결(상태 표시줄)은 자동. 폴더를 옮기면 다음 실행 때 새 경로로 다시 연결.
"@

    $zip = Join-Path $dist "usAgI-portable-$ver.zip"
    if (Test-Path $zip) { Remove-Item $zip -Force }
    Compress-Archive -Path (Join-Path $out '*') -DestinationPath $zip
    # 릴리스용 파일: 앱 자동 업데이트가 받는 exe + 체크섬, 포터블 zip
    $rel = Join-Path $dist 'release'
    if (Test-Path $rel) { Remove-Item $rel -Recurse -Force }
    New-Item -ItemType Directory -Force $rel | Out-Null
    Copy-Item (Join-Path $root 'target\release\usagi.exe') $rel
    Copy-Item $zip $rel
    $hash = (Get-FileHash (Join-Path $rel 'usagi.exe') -Algorithm SHA256).Hash.ToLower()
    Set-Content -NoNewline -Encoding ascii (Join-Path $rel 'usagi.exe.sha256') "$hash  usagi.exe"

    Write-Host "포터블 패키지: $out"
    Write-Host "압축 파일:     $zip"
    Write-Host "릴리스 파일:   $rel"
}
finally {
    Pop-Location
}
