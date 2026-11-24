# 새 버전 준비: 버전 올리기 → 테스트·컴파일 확인 → CHANGELOG 확인 → (선택) 커밋
# push는 하지 않아요. 끝에 올릴 명령을 알려 줘요.
# main에 올리면 GitHub Actions(release.yml)가 빌드하고, 게시할 때 태그(v<버전>)를 그 커밋에 만들어요.
#
# 사용:
#   scripts\release.ps1 -Version 0.2.0            버전만 올리고 확인
#   scripts\release.ps1 -Version 0.2.0 -Commit    + 버전 커밋까지 (로컬만, 예전 -Tag도 같은 뜻)
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [Alias('Tag')][switch]$Commit
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

    # 2) 테스트 + 컴파일 확인 (Cargo.lock도 갱신). 릴리스용 최적화 빌드는 CI가 함
    cargo test -p usagi-core
    if ($LASTEXITCODE -ne 0) { throw "테스트 실패" }
    cargo check -p usagi-win -p usagi-settings
    if ($LASTEXITCODE -ne 0) { throw "컴파일 확인 실패" }

    # 3) CHANGELOG에 이 버전 섹션이 있어야 함
    $changelog = Get-Content -Raw -Encoding utf8 (Join-Path $root 'CHANGELOG.md')
    if ($changelog -notmatch "(?m)^## \[$([regex]::Escape($ver))\]") {
        throw "CHANGELOG.md에 '## [$ver] - 날짜' 섹션 필요"
    }

    # 4) 커밋 (선택, 로컬만). 태그는 CI가 게시할 때 만듦
    if ($Commit) {
        # 버전 파일만 커밋 (작업 폴더의 다른 변경을 함께 쓸어 담지 않음)
        git add Cargo.toml Cargo.lock CHANGELOG.md
        $other = git status --porcelain | Where-Object { $_ -notmatch '^[MA]  ' }
        if ($other) { Write-Host "커밋하지 않은 다른 변경 있음 (이 커밋에는 넣지 않음):"; $other | ForEach-Object { Write-Host "  $_" } }
        git commit -m "Chore: Prepare release version $ver" -m "- 버전 $ver 릴리스 준비"
        if ($LASTEXITCODE -ne 0) { throw "커밋 실패" }
        Write-Host ""
        Write-Host "버전 커밋 완료. main에 올리면 GitHub Actions가 빌드·게시하고 태그 v$($ver)를 만듦:"
        Write-Host "  git push origin main"
        Write-Host "서명(.minisig)은 게시 단계에서 자동 생성 (비밀값 UPDATE_SIGNING_KEY 필요)"
    } else {
        Write-Host ""
        Write-Host "준비 완료. 커밋까지:  scripts\release.ps1 -Version $ver -Commit"
    }
}
finally {
    Pop-Location
}
