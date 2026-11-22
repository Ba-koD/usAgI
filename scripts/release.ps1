# 새 버전 준비: 버전 올리기 → 테스트·빌드 → CHANGELOG 확인 → (선택) 커밋·태그
# push는 하지 않아요. 끝에 올릴 명령을 알려 줘요.
#
# 사용:
#   scripts\release.ps1 -Version 0.2.0          버전만 올리고 확인
#   scripts\release.ps1 -Version 0.2.0 -Tag     + 커밋과 태그(v0.2.0)까지 (로컬만)
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [switch]$Tag
)

$ErrorActionPreference = 'Stop'
$ver = $Version.TrimStart('v', 'V')
if ($ver -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$') { throw "버전 형식 오류: $Version (예: 0.2.0)" }
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    # 1) Cargo.toml 워크스페이스 버전
    $toml = Join-Path $root 'Cargo.toml'
    $text = [IO.File]::ReadAllText($toml)
    $new = [regex]::Replace($text, '(?m)^version\s*=\s*"[^"]+"', "version = `"$ver`"", 1)
    [IO.File]::WriteAllText($toml, $new)
    Write-Host "Cargo.toml 버전: $ver"

    # 2) 테스트 + 빌드 (Cargo.lock도 갱신)
    cargo test -p usagi-core
    if ($LASTEXITCODE -ne 0) { throw "테스트 실패" }
    cargo build --release -p usagi-win
    if ($LASTEXITCODE -ne 0) { throw "빌드 실패" }

    # 3) CHANGELOG에 이 버전 섹션이 있어야 함
    $changelog = Get-Content -Raw -Encoding utf8 (Join-Path $root 'CHANGELOG.md')
    if ($changelog -notmatch "(?m)^## \[$([regex]::Escape($ver))\]") {
        throw "CHANGELOG.md에 '## [$ver] - 날짜' 섹션 필요"
    }

    # 4) 커밋·태그 (선택, 로컬만)
    if ($Tag) {
        # 버전 파일만 커밋 (작업 폴더의 다른 변경을 함께 쓸어 담지 않음)
        git add Cargo.toml Cargo.lock CHANGELOG.md
        $other = git status --porcelain | Where-Object { $_ -notmatch '^[MA]  ' }
        if ($other) { Write-Host "커밋하지 않은 다른 변경 있음 (이 커밋에는 넣지 않음):"; $other | ForEach-Object { Write-Host "  $_" } }
        git commit -m "Chore: Prepare release version $ver" -m "- 버전 $ver 릴리스 준비"
        if ($LASTEXITCODE -ne 0) { throw "커밋 실패" }
        git tag -a "v$ver" -m "usAgI v$ver"
        if ($LASTEXITCODE -ne 0) { throw "태그 실패" }
        Write-Host ""
        Write-Host "커밋과 태그 v$ver 생성 완료. 푸시하면 GitHub Actions가 릴리스 생성:"
        Write-Host "  git push origin main"
        Write-Host "  git push origin v$ver"
        Write-Host "서명(.minisig)은 게시 단계에서 자동 생성 (비밀값 UPDATE_SIGNING_KEY 필요)"
    } else {
        Write-Host ""
        Write-Host "준비 완료. 커밋·태그까지:  scripts\release.ps1 -Version $ver -Tag"
    }
}
finally {
    Pop-Location
}
