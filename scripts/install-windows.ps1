#Requires -Version 5.1
# usAgI Windows installer (first install). Keep this file ASCII: Windows PowerShell 5.1 reads it in the system codepage.
#
# After installing, update from the app (Settings > About, automatic updates). The app always verifies the update
# signature with its built-in public key; this script can only do so when minisign is installed.
#
# What is checked
#   - SHA-256 from the release notes (older releases: the .sha256 attachment). It comes from the same release as the
#     file, so whoever can edit the release can change both. It only catches broken downloads.
#   - Signature: if minisign is on PATH, the pinned public key below must verify the signature, and the signed
#     comment must name this version and file. Without minisign the install trusts the downloaded file on first use
#     (TOFU) and says so. -RequireSignature refuses to install when the signature cannot be verified.

param([switch]$NoLaunch, [switch]$RequireSignature)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$repo = 'Ba-koD/usAgI'
$exeName = 'usagi.exe'
# Update signing public key. Must match packaging/usagi-update.pub (scripts/check-update-key.sh checks it).
$publicKey = 'RWQ0BBRb5+kzbsZhlUlizHXXUSqgW3r2epPcN4zPTxHERCgqDw+cdo06'
$release = Invoke-RestMethod -UseBasicParsing -TimeoutSec 30 `
    -Uri "https://api.github.com/repos/$repo/releases/latest" `
    -Headers @{ Accept = 'application/vnd.github+json' }
$tag = $release.tag_name
if ($tag -cnotmatch '^v?[0-9]+\.[0-9]+\.[0-9]+$') { throw 'Invalid release version' }
$version = $tag.TrimStart('v')

function Get-ReleaseAssetUrl([string]$Name) {
    $url = "https://github.com/$repo/releases/download/$tag/$Name"
    $assets = @($release.assets | Where-Object { $_.name -ceq $Name })
    if ($assets.Count -ne 1 -or $assets[0].browser_download_url -ne $url) {
        throw "Missing or invalid release asset: $Name"
    }
    return $url
}

function Test-ReleaseAsset([string]$Name) {
    return @($release.assets | Where-Object { $_.name -ceq $Name }).Count -gt 0
}

function Get-ReleaseAssetText([string]$Name) {
    $response = Invoke-WebRequest -UseBasicParsing -TimeoutSec 30 -Uri (Get-ReleaseAssetUrl $Name)
    return [Text.Encoding]::UTF8.GetString($response.RawContentStream.ToArray())
}

# Verification block in the release notes, or $null for older releases that attach .sha256/.minisig files
function Get-VerificationContent {
    $body = [string]$release.body
    $start = '<!-- usagi-verification:start -->'
    $end = '<!-- usagi-verification:end -->'
    $markers = [regex]::Matches($body, "(?m)^(?:$start|$end)\r?$")
    if ($markers.Count -eq 0) { return $null }
    $block = [regex]::Match($body, "(?ms)^$start\r?\n(?<content>.*?)^$end\r?$")
    if ($markers.Count -ne 2 -or -not $block.Success) { throw 'Invalid release verification block' }
    return $block.Groups['content'].Value
}

function Get-ReleaseChecksum($Content) {
    if ($null -eq $Content) {
        $checksums = Get-ReleaseAssetText "$exeName.sha256"
    } else {
        $section = [regex]::Match($Content, '(?ms)^### SHA-256\r?\n\s*^```text\r?\n(?<checksums>.*?)^```\r?$')
        if (-not $section.Success) { throw 'Missing release checksums' }
        $checksums = $section.Groups['checksums'].Value
    }

    $entries = [regex]::Matches($checksums, '(?im)^[ \t]*([0-9a-f]{64})[ \t]+usagi\.exe[ \t]*\r?$')
    if ($entries.Count -ne 1) { throw 'Expected one SHA-256 checksum for usagi.exe' }
    return $entries[0].Groups[1].Value
}

function Get-ReleaseSignature($Content) {
    if ($null -eq $Content) {
        if (-not (Test-ReleaseAsset "$exeName.minisig")) { return $null }
        return Get-ReleaseAssetText "$exeName.minisig"
    }
    $section = [regex]::Match($Content, '(?ms)^\*\*`usagi\.exe\.minisig`\*\*\r?\n\s*^```text\r?\n(?<signature>.*?)^```\r?$')
    if (-not $section.Success -or -not $section.Groups['signature'].Value.Trim()) { return $null }
    return $section.Groups['signature'].Value -replace "`r", ''
}

# Verify with minisign when available: the pinned key must verify it and the signed comment must be "usAgI <version> usagi.exe"
function Test-ReleaseSignature([string]$File, [string]$Signature) {
    $minisign = Get-Command minisign -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $minisign) {
        $reason = 'minisign is not installed (https://jedisct1.github.io/minisign/)'
        if ($RequireSignature) { throw "Cannot verify the update signature: $reason. Not installing (-RequireSignature)" }
        Write-Warning "Signature not verified: $reason."
        Write-Warning 'Only the SHA-256 from the same release was checked, so this install trusts the downloaded file on first use (TOFU).'
        Write-Warning 'Once installed, the app verifies the signature of every update with its built-in public key.'
        return
    }
    $signatureFile = "$File.minisig"
    [IO.File]::WriteAllText($signatureFile, $Signature)
    try {
        $output = & $minisign.Source -V -Q -P $publicKey -x $signatureFile -m $File
        if ($LASTEXITCODE -ne 0) { throw 'Signature verification failed for usagi.exe. Not installing' }
        $comment = ([string]($output | Select-Object -Last 1)).Trim() -replace '^Trusted comment: ', ''
        if ($comment -cne "usAgI $version $exeName") { throw "The signature belongs to another version or file ($comment). Not installing" }
        Write-Host "Signature verified: $comment"
    } finally {
        Remove-Item -LiteralPath $signatureFile -Force -ErrorAction SilentlyContinue
    }
}

$url = Get-ReleaseAssetUrl $exeName
$verification = Get-VerificationContent
$checksum = Get-ReleaseChecksum $verification
$signature = Get-ReleaseSignature $verification
# The app refuses unsigned releases too
if (-not $signature) { throw 'The release has no signature for usagi.exe. Not installing' }
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
        Test-ReleaseSignature $download $signature
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
Write-Host 'Updates: the app installs signed updates itself (Settings > About). No need to run this script again.'
if (-not $NoLaunch) {
    # The app registers its Start menu shortcut and notification identity on launch.
    Start-Process -FilePath $exe -WorkingDirectory $installDir
}
