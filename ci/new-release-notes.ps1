[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^v[0-9]+\.[0-9]+\.[0-9]+$')]
    [string] $Tag,

    [Parameter(Mandatory)]
    [string] $AssetsPath,

    [Parameter(Mandatory)]
    [ValidatePattern('^[0-9a-f]{64}$')]
    [string] $PublicKeyHex,

    [Parameter(Mandatory)]
    [ValidateRange(1, [long]::MaxValue)]
    [long] $Sequence,

    [Parameter(Mandatory)]
    [string] $OutputPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$version = $Tag.Substring(1)
$templatePath = Join-Path 'docs/release-notes' "$version.md"
$notes = Get-Content -LiteralPath $templatePath -Raw
$tokens = [ordered]@{
    PUBLIC_KEY = $PublicKeyHex
    PUBLIC_KEY_SHA256 = [Convert]::ToHexString(
        [Security.Cryptography.SHA256]::HashData([Convert]::FromHexString($PublicKeyHex))
    ).ToLowerInvariant()
    SEQUENCE = $Sequence.ToString([Globalization.CultureInfo]::InvariantCulture)
}
foreach ($architecture in @('x64', 'arm64')) {
    $assetName = "claudometer-$Tag-windows-$architecture.exe"
    $assetPath = Join-Path $AssetsPath $assetName
    $asset = Get-Item -LiteralPath $assetPath
    $tokens[$architecture.ToUpperInvariant() + '_SHA256'] =
        (Get-FileHash -LiteralPath $assetPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $tokens[$architecture.ToUpperInvariant() + '_BYTES'] =
        $asset.Length.ToString([Globalization.CultureInfo]::InvariantCulture)
}
foreach ($entry in $tokens.GetEnumerator()) {
    $notes = $notes.Replace('{{' + $entry.Key + '}}', [string] $entry.Value)
}
if ($notes -match '\{\{[^}]+\}\}|(?i)pending|draft for|proposed trust-root') {
    throw 'Release notes contain unresolved placeholders or draft instructions.'
}
[IO.File]::WriteAllText(
    [IO.Path]::GetFullPath($OutputPath), $notes, [Text.UTF8Encoding]::new($false)
)
Write-Output "Release notes describe exact $Tag artifacts and public policy."
