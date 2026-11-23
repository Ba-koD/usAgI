# 이미 게시된 릴리스의 파일에 서명을 다시 붙임 (업데이트 서명 검증용).
#
# 보통은 GitHub Actions가 게시할 때 서명까지 올려요 (비밀값 UPDATE_SIGNING_KEY).
# 이 스크립트는 릴리스 파일을 내려받아 이 PC의 비밀 키로 서명하고 본문의 서명과 업데이트용 첨부를 갱신해요.
#
# 사용:  scripts\sign-release.ps1 -Version 0.2.0
#
#   -Key <파일>   비밀 키 위치 (기본: %USERPROFILE%\.usagi\update-signing.key)
#   -NoUpload     서명과 갱신할 본문을 dist\sign-<버전> 에만 저장
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$Key,
    [switch]$NoUpload
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'release-verification.ps1')
$ver = $Version.TrimStart('v', 'V')
if ($ver -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$') { throw "버전 형식 오류: $Version (예: 0.2.0)" }
$tag = "v$ver"
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    $work = Join-Path $root "dist\sign-$ver"
    New-Item -ItemType Directory -Force $work | Out-Null

    # 1) 본문의 체크섬 목록을 읽음. 이전 릴리스에만 SHA256SUMS.txt를 사용.
    $metadata = gh release view $tag --json body,assets
    if ($LASTEXITCODE -ne 0) { throw "릴리스 $tag 정보 조회 실패" }
    $release = $metadata | ConvertFrom-Json
    $body = [string]$release.body
    $verification = Find-ReleaseVerificationBlock $body
    $attached = $release.assets.name
    $names = @('usagi.exe', 'usAgI-mac.zip')
    foreach ($archive in @("usAgI-portable-$ver.zip", "usAgI-mac-$ver.zip")) {
        if ($archive -cin $attached) { $names += $archive }
    }
    $patterns = @()
    foreach ($n in $names) { $patterns += @('--pattern', $n) }
    if (-not $verification) { $patterns += @('--pattern', 'SHA256SUMS.txt') }
    gh release download $tag --dir $work --clobber @patterns
    if ($LASTEXITCODE -ne 0) { throw "릴리스 $tag 내려받기 실패" }

    # 2) 받은 파일이 릴리스의 체크섬 목록과 같은지 확인
    $checksums = if ($verification) {
        $verification.Groups['content'].Value
    } else {
        Get-Content -LiteralPath (Join-Path $work 'SHA256SUMS.txt') -Raw -Encoding utf8
    }
    $sums = @{}
    foreach ($line in ($checksums -split '\r?\n')) {
        if ($line -match '^([0-9a-fA-F]{64})[ \t]+\*?(\S+)[ \t]*$' -and $Matches[2] -cin $names) {
            $name = $Matches[2]
            if ($sums.ContainsKey($name)) { throw "$name 체크섬 중복" }
            $sums[$name] = $Matches[1].ToLowerInvariant()
        }
    }
    $files = @()
    foreach ($n in $names) {
        $path = Join-Path $work $n
        if (-not (Test-Path $path)) { throw "$n 없음 (릴리스에 올라오지 않음)" }
        if (-not $sums.ContainsKey($n)) { throw "$n 체크섬 없음" }
        $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
        if ($sums[$n] -ne $hash) { throw "$n 체크섬 불일치" }
        $files += $path
    }
    Write-Host "내려받은 파일 $($files.Count)개 체크섬 확인 완료"

    # 3) 서명 (비밀번호는 서명 도구가 직접 물어봄)
    $keyArgs = @()
    if ($Key) { $keyArgs = @('--key', $Key) }
    cargo run --quiet --release -p usagi-sign -- sign --version $ver @keyArgs @files
    if ($LASTEXITCODE -ne 0) { throw "서명 실패" }

    # 4) 본문과 호환용 서명 첨부 갱신. 이전 릴리스에 이미 붙은 서명도 갱신.
    $block = New-ReleaseVerificationBlock -Directory $work -Names $names -Signed
    $notes = Join-Path $work 'notes.md'
    [System.IO.File]::WriteAllText($notes, (Set-ReleaseVerificationBlock $body $block) + "`n")
    if ($NoUpload) {
        Write-Host "서명 파일과 릴리스 본문: $work  (올리지 않음)"
    } else {
        $sigs = @(foreach ($n in $names) {
            if ("$n.minisig" -cin $attached -or "$n.sha256" -cin $attached) {
                Join-Path $work "$n.minisig"
            }
        })
        if ($sigs.Count -gt 0) {
            gh release upload $tag @sigs --clobber
            if ($LASTEXITCODE -ne 0) { throw "서명 올리기 실패" }
        }
        gh release edit $tag --notes-file $notes
        if ($LASTEXITCODE -ne 0) { throw "릴리스 본문 갱신 실패" }
        Write-Host "릴리스 $tag 본문과 서명 첨부 $($sigs.Count)개 갱신"
    }
}
finally {
    Pop-Location
}
