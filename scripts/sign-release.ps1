# 이미 게시된 릴리스의 파일에 서명을 다시 붙임 (업데이트 서명 검증용).
#
# 보통은 GitHub Actions가 게시할 때 서명해요 (release.yml의 "서명" 작업, release-signing 환경 승인 필요).
# 이 스크립트는 릴리스 파일을 내려받아 이 PC의 비밀 키로 서명하고 본문의 서명과 업데이트용 첨부를 갱신해요.
#
# 주의: 서명하는 것은 "지금 릴리스에 올라와 있는 파일"이에요. 릴리스 본문의 SHA-256(이전 릴리스는 SHA256SUMS.txt)과
# 맞는지 확인하지만, 그 값도 같은 릴리스에서 오므로 릴리스를 고칠 수 있는 사람이 파일과 함께 바꿀 수 있어요.
# 릴리스 안의 체크섬은 독립된 증거가 아니에요. 그래서
#   - 릴리스에 이미 서명이 있으면(본문 또는 .minisig 첨부) 먼저 확인하고, 맞지 않으면 서명하지 않아요
#     (서명 뒤에 파일이 바뀌었을 수 있음).
#   - 파일마다 SHA-256을 보여 주고, 릴리스 밖의 값(직접 빌드한 파일, 그 버전을 빌드한 Actions 실행의 산출물이나
#     서명 작업 로그, 게시 때 따로 적어 둔 값)과 비교한 뒤 태그 이름을 직접 입력해야 서명해요.
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
    $attachedSigs = @(foreach ($n in $names) { if ("$n.minisig" -cin $attached) { $n } })
    $patterns = @()
    foreach ($n in $names) { $patterns += @('--pattern', $n) }
    foreach ($n in $attachedSigs) { $patterns += @('--pattern', "$n.minisig") }
    if (-not $verification) { $patterns += @('--pattern', 'SHA256SUMS.txt') }
    gh release download $tag --dir $work --clobber @patterns
    if ($LASTEXITCODE -ne 0) { throw "릴리스 $tag 내려받기 실패" }

    # 2) 받은 파일이 릴리스의 체크섬 목록과 같은지 확인 (같은 릴리스 안의 일관성만 확인, 독립된 증거 아님)
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
    Write-Host "내려받은 파일 $($files.Count)개가 릴리스의 체크섬 목록과 같음"

    # 3) 이미 있는 서명 확인 (저장소의 공개 키로). 하나라도 맞지 않으면 서명하지 않음
    $checked = 0
    if ($attachedSigs.Count -gt 0) {
        $targets = @(foreach ($n in $attachedSigs) { Join-Path $work $n })
        cargo run --quiet --release -p usagi-sign -- verify --version $ver @targets
        if ($LASTEXITCODE -ne 0) { throw "첨부된 기존 서명이 맞지 않음. 서명 뒤에 파일이 바뀌었을 수 있어 서명하지 않음" }
        $checked += $targets.Count
    }
    if ($verification) {
        $targets = @()
        foreach ($n in $names) {
            $signature = Get-ReleaseVerificationSignature $verification.Groups['content'].Value $n
            if ($signature) {
                [System.IO.File]::WriteAllText((Join-Path $work "$n.minisig"), $signature)
                $targets += (Join-Path $work $n)
            }
        }
        if ($targets.Count -gt 0) {
            cargo run --quiet --release -p usagi-sign -- verify --version $ver @targets
            if ($LASTEXITCODE -ne 0) { throw "본문의 기존 서명이 맞지 않음. 서명 뒤에 파일이 바뀌었을 수 있어 서명하지 않음" }
            $checked += $targets.Count
        }
    }
    if ($checked -eq 0) {
        Write-Warning "확인할 기존 서명 없음. 아래 SHA-256만이 확인 수단이므로 릴리스 밖의 값과 반드시 비교"
    } else {
        Write-Host "기존 서명 $checked개 확인 완료"
    }

    # 4) 사람이 확인: 릴리스 밖의 값과 SHA-256 비교 후 태그 이름 입력
    Write-Host ""
    Write-Host "서명할 파일 (릴리스 $tag):"
    foreach ($path in $files) {
        $item = Get-Item -LiteralPath $path
        Write-Host ("  {0}  {1,12:N0} bytes  {2}" -f $sums[$item.Name], $item.Length, $item.Name)
    }
    Write-Host ""
    Write-Host "위 SHA-256을 릴리스 밖의 값과 비교하세요: 직접 빌드한 파일, 이 버전을 빌드한 Actions 실행의 산출물이나"
    Write-Host "서명 작업 로그, 게시 때 따로 적어 둔 값. 릴리스 본문·첨부의 체크섬은 파일과 함께 바뀔 수 있어 증거가 아님."
    $answer = Read-Host "모두 같으면 태그 이름($tag)을 그대로 입력 (다르면 Enter로 중단)"
    if ($answer -cne $tag) { throw "확인 입력이 '$tag'와 달라 서명하지 않음" }

    # 5) 서명 (비밀번호는 서명 도구가 직접 물어봄)
    $keyArgs = @()
    if ($Key) { $keyArgs = @('--key', $Key) }
    cargo run --quiet --release -p usagi-sign -- sign --version $ver @keyArgs @files
    if ($LASTEXITCODE -ne 0) { throw "서명 실패" }

    # 6) 본문과 호환용 서명 첨부 갱신. 이전 릴리스에 이미 붙은 서명도 갱신.
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
            # 변경 불가 릴리스(immutable)는 첨부를 바꿀 수 없음. 본문은 바꿀 수 있음
            gh release upload $tag @sigs --clobber
            if ($LASTEXITCODE -ne 0) { throw "서명 올리기 실패 (변경 불가 릴리스는 첨부를 바꿀 수 없음)" }
        }
        gh release edit $tag --notes-file $notes
        if ($LASTEXITCODE -ne 0) { throw "릴리스 본문 갱신 실패" }
        Write-Host "릴리스 $tag 본문과 서명 첨부 $($sigs.Count)개 갱신"
    }
}
finally {
    Pop-Location
}
