[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^v[0-9]+\.[0-9]+\.[0-9]+$')]
    [string] $Tag,

    [Parameter(Mandatory)]
    [string] $AssetsPath
)

$ErrorActionPreference = 'Stop'
$cargo = Get-Content -LiteralPath 'Cargo.toml' -Raw
$versionMatch = [regex]::Match($cargo, '(?ms)^\[package\].*?^version\s*=\s*"([^"]+)"')
if (-not $versionMatch.Success) {
    throw 'Could not read the package version from Cargo.toml.'
}
$version = $versionMatch.Groups[1].Value
if ($Tag -ne "v$version") {
    throw "Tag '$Tag' does not match Cargo.toml version '$version'."
}

$changelog = Get-Content -LiteralPath 'CHANGELOG.md' -Raw
if ($changelog -notmatch "(?m)^## \[$([regex]::Escape($version))\]") {
    throw "CHANGELOG.md has no heading for version '$version'."
}

$expected = @(
    "claudometer-$Tag-windows-x64.exe",
    "claudometer-$Tag-windows-arm64.exe"
)
foreach ($asset in $expected) {
    $assetPath = Join-Path $AssetsPath $asset
    $hashPath = "$assetPath.sha256"
    if (-not (Test-Path -LiteralPath $assetPath -PathType Leaf)) {
        throw "Missing release asset '$asset'."
    }
    if (-not (Test-Path -LiteralPath $hashPath -PathType Leaf)) {
        throw "Missing checksum for '$asset'."
    }
    $line = (Get-Content -LiteralPath $hashPath -Raw).Trim()
    $actualHash = (Get-FileHash -LiteralPath $assetPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $expectedLine = "$actualHash *$asset"
    if ($line -cne $expectedLine) {
        throw "Checksum mismatch for '$asset'."
    }
}

$allowedNames = $expected + ($expected | ForEach-Object { "$_.sha256" })
$unexpected = Get-ChildItem -LiteralPath $AssetsPath -File |
    Where-Object { $_.Name -notin $allowedNames }
if ($unexpected) {
    throw "Unexpected release assets: $($unexpected.Name -join ', ')"
}

Write-Output "Release inputs match $Tag for x64 and ARM64."
