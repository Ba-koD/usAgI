# Windows 실행 파일의 DLL 검색 보호 확인 (CI)
#   사용: pwsh scripts/check-windows-pe.ps1 target/debug/usagi.exe target/debug/usagi-settings.exe
# - dumpbin /dependents /loadconfig 결과에서 가져다 쓰는 DLL 목록을 출력 (PR·릴리스 기록용)
# - DependentLoadFlags가 0x800(LOAD_LIBRARY_SEARCH_SYSTEM32)이 아니면 실패: exe 폴더의 같은 이름 DLL을 먼저 읽게 됨
# - 동적 C 런타임(VCRUNTIME*.dll, MSVCP*.dll, api-ms-win-crt-*, ucrtbase.dll)을 쓰면 실패: .cargo/config.toml의 정적 링크가 빠짐

param([Parameter(Mandatory = $true, ValueFromRemainingArguments = $true)][string[]]$Exe)

$ErrorActionPreference = 'Stop'

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) { throw 'vswhere.exe not found' }
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vs) { throw 'Visual Studio C++ tools not found' }
$dumpbin = Get-ChildItem -LiteralPath (Join-Path $vs 'VC\Tools\MSVC') -Recurse -Filter dumpbin.exe |
    Where-Object { $_.FullName -match '\\bin\\Hostx64\\x64\\dumpbin\.exe$' } |
    Sort-Object FullName -Descending | Select-Object -First 1
if (-not $dumpbin) { throw 'dumpbin.exe not found' }

# "Image has the following (delay load) dependencies:" 아래 들여 쓴 줄들
function Get-Dependencies([string[]]$Lines) {
    $deps = @()
    $inside = $false
    $started = $false
    foreach ($line in $Lines) {
        if ($line -match '^\s*Image has the following (delay load )?dependencies:\s*$') { $inside = $true; $started = $false; continue }
        if (-not $inside) { continue }
        if ($line.Trim() -eq '') {
            if ($started) { $inside = $false }
            continue
        }
        $started = $true
        $deps += $line.Trim()
    }
    return $deps
}

$failed = @()
foreach ($file in $Exe) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "Missing executable: $file" }
    $lines = & $dumpbin.FullName /nologo /dependents /loadconfig $file
    if ($LASTEXITCODE -ne 0) { throw "dumpbin failed: $file" }
    $deps = Get-Dependencies $lines
    Write-Host "== $file"
    Write-Host 'Imports:'
    foreach ($dep in $deps) { Write-Host "  $dep" }

    $flag = $lines | Select-String -Pattern '^\s*([0-9A-Fa-f]+)\s+Dependent\s*Load\s*Flags?\s*$' | Select-Object -First 1
    if (-not $flag) {
        Write-Host ($lines -join "`n")
        $failed += "${file}: DependentLoadFlags not found in the load config"
    } else {
        $value = [Convert]::ToUInt32($flag.Matches[0].Groups[1].Value, 16)
        Write-Host ('DependentLoadFlags: 0x{0:X}' -f $value)
        if ($value -ne 0x800) { $failed += ('{0}: DependentLoadFlags is 0x{1:X}, expected 0x800' -f $file, $value) }
    }

    $crt = @($deps | Where-Object { $_ -match '^(vcruntime|msvcp|ucrtbase|api-ms-win-crt-)' })
    if ($crt.Count -gt 0) { $failed += "${file}: dynamic C runtime imports: $($crt -join ', ')" }
}

if ($failed.Count -gt 0) {
    foreach ($message in $failed) { Write-Host "::error::$message" }
    exit 1
}
Write-Host 'DLL search protection OK'
