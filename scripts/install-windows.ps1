#Requires -Version 5.1

param([switch]$NoLaunch)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$repo = 'Ba-koD/usAgI'
$exeName = 'usagi.exe'
$release = Invoke-RestMethod -UseBasicParsing -TimeoutSec 30 `
    -Uri "https://api.github.com/repos/$repo/releases/latest" `
    -Headers @{ Accept = 'application/vnd.github+json' }
$tag = $release.tag_name
if ($tag -cnotmatch '^v?[0-9]+\.[0-9]+\.[0-9]+$') { throw 'Invalid release version' }

function Get-ReleaseAssetUrl([string]$Name) {
    $url = "https://github.com/$repo/releases/download/$tag/$Name"
    $assets = @($release.assets | Where-Object { $_.name -ceq $Name })
    if ($assets.Count -ne 1 -or $assets[0].browser_download_url -ne $url) {
        throw "Missing or invalid release asset: $Name"
    }
    return $url
}

function Get-ReleaseChecksum {
    $body = [string]$release.body
    $start = '<!-- usagi-verification:start -->'
    $end = '<!-- usagi-verification:end -->'
    $markers = [regex]::Matches($body, "(?m)^(?:$start|$end)\r?$")

    if ($markers.Count -eq 0) {
        $response = Invoke-WebRequest -UseBasicParsing -TimeoutSec 30 -Uri (Get-ReleaseAssetUrl "$exeName.sha256")
        $checksums = [Text.Encoding]::UTF8.GetString($response.RawContentStream.ToArray())
    } else {
        $block = [regex]::Match($body, "(?ms)^$start\r?\n(?<content>.*?)^$end\r?$")
        if ($markers.Count -ne 2 -or -not $block.Success) { throw 'Invalid release verification block' }
        $section = [regex]::Match($block.Groups['content'].Value, '(?ms)^### SHA-256\r?\n\s*^```text\r?\n(?<checksums>.*?)^```\r?$')
        if (-not $section.Success) { throw 'Missing release checksums' }
        $checksums = $section.Groups['checksums'].Value
    }

    $entries = [regex]::Matches($checksums, '(?im)^[ \t]*([0-9a-f]{64})[ \t]+usagi\.exe[ \t]*\r?$')
    if ($entries.Count -ne 1) { throw 'Expected one SHA-256 checksum for usagi.exe' }
    return $entries[0].Groups[1].Value
}

$url = Get-ReleaseAssetUrl $exeName
$checksum = Get-ReleaseChecksum
$installDir = Join-Path $env:LOCALAPPDATA 'Programs\usAgI'
$exe = Join-Path $installDir $exeName
$installed = Test-Path -LiteralPath $exe -PathType Leaf

if (-not $installed -or (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash -ne $checksum) {
    if (Get-Process -Name usagi -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe }) {
        throw 'Quit usAgI from the notification area and run the installer again'
    }
    [IO.Directory]::CreateDirectory($installDir) | Out-Null
    $download = Join-Path $installDir ('.install-' + [guid]::NewGuid().ToString('N') + '.tmp')
    try {
        Write-Host "Downloading usAgI $tag"
        Invoke-WebRequest -UseBasicParsing -TimeoutSec 120 -Uri $url -OutFile $download
        if ((Get-FileHash -LiteralPath $download -Algorithm SHA256).Hash -ne $checksum) {
            throw 'SHA-256 mismatch for usagi.exe'
        }
        if ($installed) {
            [IO.File]::Replace($download, $exe, [NullString]::Value)
        } else {
            [IO.File]::Move($download, $exe)
        }
    } finally {
        if (Test-Path -LiteralPath $download -PathType Leaf) { Remove-Item -LiteralPath $download -Force }
    }
}

Write-Host "usAgI $tag installed at $exe"
if (-not $NoLaunch) {
    # The app registers its Start menu shortcut and notification identity on launch.
    Start-Process -FilePath $exe -WorkingDirectory $installDir
}
