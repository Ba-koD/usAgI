# GitHub Actions 게시와 수동 재서명에서 같은 형식의 릴리스 본문을 사용.
$releaseVerificationStart = '<!-- usagi-verification:start -->'
$releaseVerificationEnd = '<!-- usagi-verification:end -->'

function Find-ReleaseVerificationBlock([string]$Body) {
    $start = [regex]::Escape($releaseVerificationStart)
    $end = [regex]::Escape($releaseVerificationEnd)
    $markers = [regex]::Matches($Body, "(?m)^(?:$start|$end)\r?$")
    if ($markers.Count -eq 0) { return }

    $block = [regex]::Match($Body, "(?ms)^$start\r?\n(?<content>.*?)^$end\r?$")
    if ($markers.Count -ne 2 -or -not $block.Success) { throw '릴리스 본문의 파일 검증 정보 경계 오류' }
    $block
}

function New-ReleaseVerificationBlock([string]$Directory, [string[]]$Names, [switch]$Signed) {
    $lines = @(
        $releaseVerificationStart
        '<details>'
        '<summary>파일 검증 정보 (SHA-256 · Minisign)</summary>'
        ''
        '### SHA-256'
        ''
        '```text'
        foreach ($name in $Names) {
            $hash = (Get-FileHash -LiteralPath (Join-Path $Directory $name) -Algorithm SHA256).Hash.ToLowerInvariant()
            "$hash  $name"
        }
        '```'
        if ($Signed) {
            ''
            '### Minisign'
            foreach ($name in $Names) {
                $signature = Get-Content -LiteralPath (Join-Path $Directory "$name.minisig") -Raw -Encoding utf8
                if ([string]::IsNullOrWhiteSpace($signature)) { throw "$name.minisig 내용 없음" }
                ''
                ('**`{0}.minisig`**' -f $name)
                ''
                '```text'
                $signature.TrimEnd("`r", "`n")
                '```'
            }
        }
        ''
        '</details>'
        $releaseVerificationEnd
    )
    $lines -join "`n"
}

function Set-ReleaseVerificationBlock([string]$Body, [string]$Block) {
    $previous = Find-ReleaseVerificationBlock $Body
    if ($previous) { return $Body.Remove($previous.Index, $previous.Length).Insert($previous.Index, $Block) }
    if (-not $Body.Trim()) { return $Block }
    $Body.TrimEnd() + "`n`n" + $Block
}
