[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^v[0-9]+\.[0-9]+\.[0-9]+$')]
    [string] $Tag,

    [Parameter(Mandatory)]
    [string] $AssetsPath,

    [Parameter(Mandatory)]
    [ValidateRange(1, [long]::MaxValue)]
    [long] $Sequence,

    [Parameter(Mandatory)]
    [ValidatePattern('^[0-9]+\.[0-9]+\.[0-9]+$')]
    [string] $MinimumUpdaterVersion,

    [Parameter(Mandatory)]
    [ValidatePattern('^[0-9a-f]{64}$')]
    [string] $PublicKeyHex,

    [Parameter(Mandatory)]
    [string] $PrivateKeyPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'release-crypto.ps1')

$assets = (Resolve-Path -LiteralPath $AssetsPath).Path
$privateKey = (Resolve-Path -LiteralPath $PrivateKeyPath).Path
$openSsl = Resolve-ReleaseOpenSsl
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$temp = Join-Path $tempRoot ('claudometer-release-sign-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $temp | Out-Null

try {
    $derivedKey = Get-ReleasePublicKeyHex `
        -OpenSsl $openSsl `
        -PrivateKeyPath $privateKey `
        -PublicDerPath (Join-Path $temp 'public.der')
    if ($derivedKey -cne $PublicKeyHex) {
        throw 'The release private key does not match the configured public key.'
    }

    $version = $Tag.Substring(1)
    if (-not (Test-CanonicalReleaseVersion $version) -or
        -not (Test-CanonicalReleaseVersion $MinimumUpdaterVersion)) {
        throw 'Release and minimum-updater versions must be canonical u16 major.minor.patch values.'
    }
    if ([version] $MinimumUpdaterVersion -gt [version] $version) {
        throw 'Minimum updater version cannot be newer than the released application.'
    }
    $issued = [DateTimeOffset]::UtcNow
    $expires = $issued.AddDays(30)
    $timestampFormat = "yyyy-MM-dd'T'HH:mm:ss'Z'"
    $utf8 = [Text.UTF8Encoding]::new($false)

    foreach ($architecture in @('x64', 'arm64')) {
        $stem = "claudometer-$Tag-windows-$architecture"
        $assetName = "$stem.exe"
        $assetPath = Join-Path $assets $assetName
        $checksumPath = "$assetPath.sha256"
        if (-not (Test-Path -LiteralPath $assetPath -PathType Leaf)) {
            throw "Missing release artifact '$assetName'."
        }
        if (-not (Test-Path -LiteralPath $checksumPath -PathType Leaf)) {
            throw "Missing release checksum '$assetName.sha256'."
        }

        $hash = (Get-FileHash -LiteralPath $assetPath -Algorithm SHA256).Hash.ToLowerInvariant()
        $expectedChecksum = "$hash *$assetName"
        if ((Get-Content -LiteralPath $checksumPath -Raw).Trim() -cne $expectedChecksum) {
            throw "Checksum mismatch for '$assetName'."
        }

        $manifestPath = Join-Path $assets "$stem.manifest.json"
        $envelopePath = Join-Path $assets "$stem.manifest.signatures.json"
        if ((Test-Path -LiteralPath $manifestPath) -or (Test-Path -LiteralPath $envelopePath)) {
            throw "Refusing to replace release evidence for '$assetName'."
        }

        $manifest = [ordered]@{
            schema = 'claudometer-release-manifest-v1'
            channel = 'stable'
            sequence = $Sequence
            version = $version
            tag = $Tag
            issued_at = $issued.ToString($timestampFormat, [Globalization.CultureInfo]::InvariantCulture)
            policy_expires_at = $expires.ToString($timestampFormat, [Globalization.CultureInfo]::InvariantCulture)
            architecture = $architecture
            asset = $assetName
            size = (Get-Item -LiteralPath $assetPath).Length
            sha256 = $hash
            minimum_updater_version = $MinimumUpdaterVersion
        }
        [IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json), $utf8)

        $rawSignaturePath = Join-Path $temp "$architecture.signature"
        Invoke-ReleaseOpenSsl $openSsl @(
            'pkeyutl', '-sign', '-inkey', $privateKey, '-keyform', 'DER',
            '-rawin', '-in', $manifestPath, '-out', $rawSignaturePath
        )
        $signatureBytes = [IO.File]::ReadAllBytes($rawSignaturePath)
        if ($signatureBytes.Length -ne 64) {
            throw "OpenSSL produced an invalid Ed25519 signature for '$assetName'."
        }
        $signatureHex = [Convert]::ToHexString($signatureBytes).ToLowerInvariant()
        $envelope = [ordered]@{
            schema = 'claudometer-release-signatures-v1'
            signatures = @(
                [ordered]@{
                    public_key = $PublicKeyHex
                    signature = $signatureHex
                }
            )
        }
        [IO.File]::WriteAllText($envelopePath, ($envelope | ConvertTo-Json -Depth 4), $utf8)
        Test-ReleaseEd25519Signature `
            -OpenSsl $openSsl `
            -PublicKeyHex $PublicKeyHex `
            -MessagePath $manifestPath `
            -SignatureHex $signatureHex
    }

    Write-Output "Generated signed manifests for $Tag sequence $Sequence."
} finally {
    $resolved = [IO.Path]::GetFullPath($temp)
    if (-not $resolved.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -or
        -not ([IO.Path]::GetFileName($resolved)).StartsWith('claudometer-release-sign-')) {
        throw "Refusing to remove unexpected signing directory '$resolved'."
    }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
