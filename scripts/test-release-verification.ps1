# release-verification.ps1 확인 (CI의 Linux 줄에서 pwsh로 실행)
#   본문 만들기 → 찾기 → 서명 꺼내기가 서로 맞는지, 서명이 없거나 형식이 틀린 경우를 확인해요.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'release-verification.ps1')

function Assert([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "실패: $Message" }
}

$dir = Join-Path ([IO.Path]::GetTempPath()) ('usagi-rv-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $dir | Out-Null
try {
    $names = @('usagi.exe', 'usAgI-mac.zip')
    foreach ($n in $names) {
        [IO.File]::WriteAllText((Join-Path $dir $n), "fixture $n")
        [IO.File]::WriteAllText((Join-Path $dir "$n.minisig"), "untrusted comment: test`nRWQsig-$n`ntrusted comment: usAgI 1.0.0 $n`nglobal-$n`n")
    }

    $block = New-ReleaseVerificationBlock -Directory $dir -Names $names -Signed
    # GitHub이 돌려주는 본문은 CRLF일 수 있음
    $body = (Set-ReleaseVerificationBlock "### 변경 내용`n`n- 항목" $block) -replace "`n", "`r`n"
    $found = Find-ReleaseVerificationBlock $body
    Assert ($null -ne $found) '검증 정보 찾기'
    $content = $found.Groups['content'].Value
    foreach ($n in $names) {
        $want = [IO.File]::ReadAllText((Join-Path $dir "$n.minisig"))
        $got = Get-ReleaseVerificationSignature $content $n
        Assert ($got -ceq $want) "$n 서명 꺼내기"
    }
    Assert ($null -eq (Get-ReleaseVerificationSignature $content 'other.zip')) '없는 파일의 서명은 $null'

    $unsigned = New-ReleaseVerificationBlock -Directory $dir -Names $names
    Assert ($null -eq (Get-ReleaseVerificationSignature $unsigned 'usagi.exe')) '서명 없는 본문은 $null'

    foreach ($bad in @(
        ('**`usagi.exe.minisig`**' + "`n`nnot a fence"),
        ('**`usagi.exe.minisig`**' + "`n`n" + '```text' + "`nunfinished"),
        ('**`usagi.exe.minisig`**' + "`n`n" + '```text' + "`n  `n" + '```')
    )) {
        $threw = $false
        try { Get-ReleaseVerificationSignature $bad 'usagi.exe' | Out-Null } catch { $threw = $true }
        Assert $threw "형식이 틀린 서명은 오류: $bad"
    }
    Write-Host 'release-verification.ps1 확인 완료'
}
finally {
    Remove-Item -LiteralPath $dir -Recurse -Force
}
