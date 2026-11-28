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

# 검증 정보 안의 `<이름>.minisig` 서명 글자 (앱의 update.rs fenced_text와 같은 규칙). 없으면 $null, 형식이 틀리면 오류
function Get-ReleaseVerificationSignature([string]$Content, [string]$Name) {
    $lines = [string[]]($Content -split '\r?\n')
    $heading = [array]::IndexOf($lines, ('**`{0}.minisig`**' -f $Name))
    if ($heading -lt 0) { return $null }
    $i = $heading + 1
    while ($i -lt $lines.Count -and -not $lines[$i].Trim()) { $i++ }
    if ($i -ge $lines.Count -or $lines[$i] -cne '```text') { throw "$Name.minisig 서명 형식 오류" }
    $value = [System.Collections.Generic.List[string]]::new()
    for ($i++; $i -lt $lines.Count; $i++) {
        if ($lines[$i] -ceq '```') {
            if (-not ($value -join '').Trim()) { throw "$Name.minisig 서명 내용 없음" }
            return ($value -join "`n") + "`n"
        }
        $value.Add($lines[$i])
    }
    throw "$Name.minisig 서명 형식 오류"
}

function Set-ReleaseVerificationBlock([string]$Body, [string]$Block) {
    $previous = Find-ReleaseVerificationBlock $Body
    if ($previous) { return $Body.Remove($previous.Index, $previous.Length).Insert($previous.Index, $Block) }
    if (-not $Body.Trim()) { return $Block }
    $Body.TrimEnd() + "`n`n" + $Block
}
