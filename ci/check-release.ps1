[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^v[0-9]+\.[0-9]+\.[0-9]+$')]
    [string] $Tag,

    [Parameter(Mandatory)]
    [string] $AssetsPath,

    [switch] $RequireEvidence,

    [ValidatePattern('^[0-9a-f]{64}$')]
    [string] $PublicKeyHex,

    [ValidateRange(1, [long]::MaxValue)]
    [long] $Sequence,

    [ValidatePattern('^[0-9]+\.[0-9]+\.[0-9]+$')]
    [string] $MinimumUpdaterVersion
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'release-crypto.ps1')
$cargo = Get-Content -LiteralPath 'Cargo.toml' -Raw
$versionMatch = [regex]::Match($cargo, '(?ms)^\[package\].*?^version\s*=\s*"([^"]+)"')
if (-not $versionMatch.Success) {
    throw 'Could not read the package version from Cargo.toml.'
}
$version = $versionMatch.Groups[1].Value
if (-not (Test-CanonicalReleaseVersion $version)) {
    throw "Cargo.toml version '$version' is not canonical major.minor.patch."
}
if ($Tag -ne "v$version") {
    throw "Tag '$Tag' does not match Cargo.toml version '$version'."
}

$changelog = Get-Content -LiteralPath 'CHANGELOG.md' -Raw
if ($changelog -notmatch "(?m)^## \[$([regex]::Escape($version))\]") {
    throw "CHANGELOG.md has no heading for version '$version'."
}

$expectedAssets = @(
    [pscustomobject]@{
        Name = "claudometer-$Tag-windows-x64.exe"
        Architecture = 'x64'
        Target = 'x86_64-pc-windows-msvc'
    },
    [pscustomobject]@{
        Name = "claudometer-$Tag-windows-arm64.exe"
        Architecture = 'arm64'
        Target = 'aarch64-pc-windows-msvc'
    }
)
$allowedNames = [System.Collections.Generic.List[string]]::new()
foreach ($expectedAsset in $expectedAssets) {
    $asset = $expectedAsset.Name
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

    ./ci/check-artifact.ps1 -Path $assetPath -Target $expectedAsset.Target
    $allowedNames.Add($asset)
    $allowedNames.Add("$asset.sha256")

    if (-not $RequireEvidence) {
        continue
    }
    if (-not $PSBoundParameters.ContainsKey('PublicKeyHex') -or
        -not $PSBoundParameters.ContainsKey('Sequence') -or
        -not $PSBoundParameters.ContainsKey('MinimumUpdaterVersion')) {
        throw 'Evidence verification requires public key, sequence, and minimum updater version.'
    }
    if (-not (Test-CanonicalReleaseVersion $MinimumUpdaterVersion) -or
        [version] $MinimumUpdaterVersion -gt [version] $version) {
        throw 'Minimum updater version is non-canonical or newer than the release.'
    }

    $stem = $asset.Substring(0, $asset.Length - 4)
    $manifestName = "$stem.manifest.json"
    $signatureName = "$stem.manifest.signatures.json"
    $manifestPath = Join-Path $AssetsPath $manifestName
    $signaturePath = Join-Path $AssetsPath $signatureName
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw "Missing release manifest '$manifestName'."
    }
    if (-not (Test-Path -LiteralPath $signaturePath -PathType Leaf)) {
        throw "Missing release signature '$signatureName'."
    }

    $manifestText = Get-Content -LiteralPath $manifestPath -Raw
    $manifest = $manifestText | ConvertFrom-Json
    $expectedManifestFields = @(
        'schema', 'channel', 'sequence', 'version', 'tag', 'issued_at',
        'policy_expires_at', 'architecture', 'asset', 'size', 'sha256',
        'minimum_updater_version'
    )
    $actualManifestFields = @($manifest.PSObject.Properties.Name)
    if (@(Compare-Object $expectedManifestFields $actualManifestFields).Count -ne 0) {
        throw "Manifest '$manifestName' has an unexpected field set."
    }
    $expectedSize = (Get-Item -LiteralPath $assetPath).Length
    if ($manifest.schema -cne 'claudometer-release-manifest-v1' -or
        $manifest.channel -cne 'stable' -or
        [long] $manifest.sequence -ne $Sequence -or
        $manifest.version -cne $version -or
        $manifest.tag -cne $Tag -or
        $manifest.architecture -cne $expectedAsset.Architecture -or
        $manifest.asset -cne $asset -or
        [long] $manifest.size -ne $expectedSize -or
        $manifest.sha256 -cne $actualHash -or
        $manifest.minimum_updater_version -cne $MinimumUpdaterVersion) {
        throw "Manifest '$manifestName' does not describe the exact release artifact and policy."
    }
    $issuedMatch = [regex]::Match(
        $manifestText,
        '"issued_at"\s*:\s*"(?<value>\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z)"'
    )
    $expiresMatch = [regex]::Match(
        $manifestText,
        '"policy_expires_at"\s*:\s*"(?<value>\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z)"'
    )
    if (-not $issuedMatch.Success -or -not $expiresMatch.Success) {
        throw "Manifest '$manifestName' has a non-canonical timestamp."
    }
    $issued = [DateTimeOffset]::ParseExact(
        $issuedMatch.Groups['value'].Value,
        "yyyy-MM-dd'T'HH:mm:ss'Z'",
        [Globalization.CultureInfo]::InvariantCulture,
        [Globalization.DateTimeStyles]::AssumeUniversal
    )
    $expires = [DateTimeOffset]::ParseExact(
        $expiresMatch.Groups['value'].Value,
        "yyyy-MM-dd'T'HH:mm:ss'Z'",
        [Globalization.CultureInfo]::InvariantCulture,
        [Globalization.DateTimeStyles]::AssumeUniversal
    )
    $now = [DateTimeOffset]::UtcNow
    if ($issued -gt $now.AddMinutes(5) -or $expires -le $now -or $expires -le $issued) {
        throw "Manifest '$manifestName' has an invalid policy window."
    }

    $envelope = Get-Content -LiteralPath $signaturePath -Raw | ConvertFrom-Json
    $envelopeFields = @($envelope.PSObject.Properties.Name)
    if (@(Compare-Object @('schema', 'signatures') $envelopeFields).Count -ne 0 -or
        $envelope.schema -cne 'claudometer-release-signatures-v1' -or
        @($envelope.signatures).Count -ne 1) {
        throw "Signature envelope '$signatureName' is invalid."
    }
    $signature = @($envelope.signatures)[0]
    if (@(Compare-Object @('public_key', 'signature') @($signature.PSObject.Properties.Name)).Count -ne 0 -or
        $signature.public_key -cne $PublicKeyHex) {
        throw "Signature envelope '$signatureName' uses an unexpected key or field set."
    }
    $openSsl = Resolve-ReleaseOpenSsl
    Test-ReleaseEd25519Signature `
        -OpenSsl $openSsl `
        -PublicKeyHex $PublicKeyHex `
        -MessagePath $manifestPath `
        -SignatureHex $signature.signature
    $allowedNames.Add($manifestName)
    $allowedNames.Add($signatureName)
}

$sbomName = "claudometer-$Tag.sbom.spdx.json"
if ($RequireEvidence) {
    $sbomPath = Join-Path $AssetsPath $sbomName
    if (-not (Test-Path -LiteralPath $sbomPath -PathType Leaf)) {
        throw "Missing release SBOM '$sbomName'."
    }
    $sbom = Get-Content -LiteralPath $sbomPath -Raw | ConvertFrom-Json
    if ($sbom.spdxVersion -cne 'SPDX-2.3' -or @($sbom.packages).Count -eq 0) {
        throw "Release SBOM '$sbomName' is not a populated SPDX 2.3 document."
    }
    $rootPackage = @($sbom.packages) |
        Where-Object { $_.name -ceq 'claudometer' -and $_.versionInfo -ceq $version }
    if (@($rootPackage).Count -ne 1) {
        throw "Release SBOM '$sbomName' does not identify claudometer $version exactly once."
    }
    $allowedNames.Add($sbomName)
}

$unexpected = Get-ChildItem -LiteralPath $AssetsPath -File |
    Where-Object { $_.Name -notin @($allowedNames) }
if ($unexpected) {
    throw "Unexpected release assets: $($unexpected.Name -join ', ')"
}

$evidence = if ($RequireEvidence) { ' and verifiable evidence' } else { '' }
Write-Output "Release inputs$evidence match $Tag for x64 and ARM64."
