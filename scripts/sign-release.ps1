# 이미 게시된 릴리스의 파일에 서명을 다시 붙임 (업데이트 서명 검증용).
#
# 보통은 GitHub Actions가 게시할 때 서명까지 올려요 (비밀값 UPDATE_SIGNING_KEY).
# 이 스크립트는 서명이 빠졌거나 다시 올려야 할 때 쓰는 수동 방법: 릴리스 파일을 내려받아 이 PC의 비밀 키로 서명하고 .minisig 를 올려요.
#
# 사용:  scripts\sign-release.ps1 -Version 0.2.0
#
#   -Key <파일>   비밀 키 위치 (기본: %USERPROFILE%\.usagi\update-signing.key)
#   -NoUpload     서명만 만들고 올리지 않음 (dist\sign-<버전> 에 남김)
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$Key,
    [switch]$NoUpload
)

$ErrorActionPreference = 'Stop'
$ver = $Version.TrimStart('v', 'V')
if ($ver -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$') { throw "버전 형식 오류: $Version (예: 0.2.0)" }
$tag = "v$ver"
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    $work = Join-Path $root "dist\sign-$ver"
    if (Test-Path $work) { Remove-Item -Recurse -Force $work }
    New-Item -ItemType Directory -Force $work | Out-Null

    # 1) 릴리스 파일 내려받기 (자동 업데이트용 2개 + 직접 내려받는 묶음 2개 + 체크섬 목록)
    $names = @('usagi.exe', 'usAgI-mac.zip', "usAgI-portable-$ver.zip", "usAgI-mac-$ver.zip")
    $patterns = @()
    foreach ($n in $names + 'SHA256SUMS.txt') { $patterns += @('--pattern', $n) }
    gh release download $tag --dir $work @patterns
    if ($LASTEXITCODE -ne 0) { throw "릴리스 $tag 내려받기 실패" }

    # 2) 받은 파일이 릴리스의 체크섬 목록과 같은지 확인
    $sums = @{}
    foreach ($line in Get-Content (Join-Path $work 'SHA256SUMS.txt')) {
        $parts = $line -split '\s+', 2
        if ($parts.Count -eq 2) { $sums[$parts[1].Trim().TrimStart('*')] = $parts[0].ToLower() }
    }
    $files = @()
    foreach ($n in $names) {
        $path = Join-Path $work $n
        if (-not (Test-Path $path)) { throw "$n 없음 (릴리스에 올라오지 않음)" }
        $hash = (Get-FileHash -Algorithm SHA256 $path).Hash.ToLower()
        if ($sums[$n] -ne $hash) { throw "$n 체크섬 불일치 (SHA256SUMS.txt와 다름)" }
        $files += $path
    }
    Write-Host "내려받은 파일 $($files.Count)개 체크섬 확인 완료"

    # 3) 서명 (비밀번호는 서명 도구가 직접 물어봄)
    $keyArgs = @()
    if ($Key) { $keyArgs = @('--key', $Key) }
    cargo run --quiet --release -p usagi-sign -- sign --version $ver @keyArgs @files
    if ($LASTEXITCODE -ne 0) { throw "서명 실패" }

    # 4) 서명 올리기
    $sigs = Get-ChildItem $work -Filter *.minisig | ForEach-Object { $_.FullName }
    if ($NoUpload) {
        Write-Host "서명 파일: $work  (올리지 않음)"
    } else {
        gh release upload $tag @sigs --clobber
        if ($LASTEXITCODE -ne 0) { throw "서명 올리기 실패" }
        Write-Host "릴리스 $tag 에 서명 $($sigs.Count)개 올림. 설치된 앱은 다음 확인 때부터 이 버전을 받음."
    }
}
finally {
    Pop-Location
}
