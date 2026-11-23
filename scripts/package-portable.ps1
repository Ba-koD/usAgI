# usAgI 포터블 패키지 만들기
#   결과: dist\usAgI-portable\  (usagi.exe + data\ + 읽어보기.txt)
#         dist\usAgI-portable-<버전>.zip
#         dist\release\       릴리스 파일 (usagi.exe, usagi.exe.sha256)
# 사용: powershell -ExecutionPolicy Bypass -File scripts\package-portable.ps1
# -ReleaseOnly: 릴리스 파일만 준비하고 로컬 포터블 묶음은 생략

param([switch]$ReleaseOnly)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    # CI에서는 Cargo.lock 그대로 (바뀌면 실패)
    $extra = @()
    if ($env:CI) { $extra += @('--locked', '--timings') }
    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
    $timings = Join-Path $target 'cargo-timings'
    cargo build --release -p usagi-win @extra
    if ($LASTEXITCODE -ne 0) { throw "빌드 실패" }
    if ($env:CI) { Copy-Item (Join-Path $timings 'cargo-timing.html') (Join-Path $timings 'windows-host.html') }
    cargo build --profile settings-release -p usagi-settings @extra
    if ($LASTEXITCODE -ne 0) { throw "설정 빌드 실패" }
    if ($env:CI) { Copy-Item (Join-Path $timings 'cargo-timing.html') (Join-Path $timings 'windows-settings.html') }

    $dist = Join-Path $root 'dist'
    $hostExe = Join-Path $target 'release/usagi.exe'
    $settingsExe = Join-Path $target 'settings-release/usagi-settings.exe'
    $rel = Join-Path $dist 'release'
    New-Item -ItemType Directory -Force $rel | Out-Null
    $exe = Join-Path $rel 'usagi.exe'
    Copy-Item -LiteralPath $hostExe -Destination $exe -Force
    # Overlay outside the PE image: the settings code is mapped only when launched.
    $output = [IO.File]::Open($exe, [IO.FileMode]::Append)
    try {
        $start = $output.Position
        $inputStream = [IO.File]::OpenRead($settingsExe)
        try {
            if ($inputStream.Length -eq 0 -or $inputStream.Length -gt 64MB) { throw '설정 실행 파일 크기 오류' }
            $level = [IO.Compression.CompressionLevel]::Optimal
            if ([Enum]::IsDefined([IO.Compression.CompressionLevel], 'SmallestSize')) {
                $level = [IO.Compression.CompressionLevel]::SmallestSize
            }
            $gzip = [IO.Compression.GZipStream]::new($output, $level, $true)
            try { $inputStream.CopyTo($gzip) } finally { $gzip.Dispose() }
        } finally {
            $inputStream.Dispose()
        }
        $length = [BitConverter]::GetBytes([uint64]($output.Position - $start))
        $marker = [Text.Encoding]::ASCII.GetBytes('USAGISETTINGS001')
        $output.Write($length, 0, $length.Length)
        $output.Write($marker, 0, $marker.Length)
    } finally {
        $output.Dispose()
    }
    if ((Get-Item -LiteralPath $exe).Length -gt 64MB) { throw 'Windows 업데이트 다운로드 크기 초과' }
    $hash = (Get-FileHash $exe -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -NoNewline -Encoding ascii (Join-Path $rel 'usagi.exe.sha256') "$hash  usagi.exe"
    Write-Host "릴리스 파일:   $rel"
    if ($ReleaseOnly) { return }

    $ver = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
    $out = Join-Path $dist 'usAgI-portable'
    if (Test-Path -LiteralPath $out) {
        $resolved = (Resolve-Path -LiteralPath $out).ProviderPath
        $item = Get-Item -LiteralPath $out -Force
        if ($resolved -ne $out -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw '포터블 출력 폴더 경로 확인 실패'
        }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
    New-Item -ItemType Directory -Force $out | Out-Null

    Copy-Item -LiteralPath $exe -Destination $out
    # exe 옆의 data\usagi.portable 표시 파일이 포터블 모드를 켬.
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
  usagi.exe 옆 data\usagi.portable 표시 파일로 포터블 모드 실행. 모든 설정과 받은 사용량을 data 폴더에 저장.
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
    Write-Host "포터블 패키지: $out"
    Write-Host "압축 파일:     $zip"
}
finally {
    Pop-Location
}
