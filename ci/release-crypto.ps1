Set-StrictMode -Version Latest

function Test-CanonicalReleaseVersion {
    param([Parameter(Mandatory)][string] $Value)

    if ($Value -cnotmatch '^[0-9]+\.[0-9]+\.[0-9]+$') {
        return $false
    }
    $parts = $Value.Split('.')
    $parsed = [System.Collections.Generic.List[uint16]]::new()
    foreach ($part in $parts) {
        [uint16] $field = 0
        if (-not [uint16]::TryParse($part, [ref] $field)) {
            return $false
        }
        $parsed.Add($field)
    }
    return $Value -ceq ($parsed -join '.')
}

function Resolve-ReleaseOpenSsl {
    $command = Get-Command openssl.exe -ErrorAction SilentlyContinue
    if ($null -ne $command) {
        return $command.Source
    }

    $git = Get-Command git.exe -ErrorAction SilentlyContinue
    if ($null -ne $git) {
        $gitRoot = Split-Path -Parent (Split-Path -Parent $git.Source)
        foreach ($relative in @('usr\bin\openssl.exe', 'mingw64\bin\openssl.exe')) {
            $candidate = Join-Path $gitRoot $relative
            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                return $candidate
            }
        }
    }

    throw 'OpenSSL was not found. Install OpenSSL or Git for Windows.'
}

function Invoke-ReleaseOpenSsl {
    param(
        [Parameter(Mandatory)]
        [string] $OpenSsl,

        [Parameter(Mandatory)]
        [string[]] $Arguments
    )

    & $OpenSsl @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "OpenSSL failed with exit code $LASTEXITCODE."
    }
}

function Get-ReleasePublicKeyHex {
    param(
        [Parameter(Mandatory)]
        [string] $OpenSsl,

        [Parameter(Mandatory)]
        [string] $PrivateKeyPath,

        [Parameter(Mandatory)]
        [string] $PublicDerPath
    )

    Invoke-ReleaseOpenSsl $OpenSsl @(
        'pkey', '-in', $PrivateKeyPath, '-inform', 'DER', '-pubout',
        '-outform', 'DER', '-out', $PublicDerPath
    )
    $publicDer = [IO.File]::ReadAllBytes($PublicDerPath)
    $prefix = [Convert]::FromHexString('302a300506032b6570032100')
    if ($publicDer.Length -ne $prefix.Length + 32) {
        throw 'The release private key is not an Ed25519 PKCS#8 key.'
    }
    for ($index = 0; $index -lt $prefix.Length; $index++) {
        if ($publicDer[$index] -ne $prefix[$index]) {
            throw 'The release private key has an unexpected public-key encoding.'
        }
    }

    return [Convert]::ToHexString($publicDer[$prefix.Length..($publicDer.Length - 1)]).
        ToLowerInvariant()
}

function Test-ReleaseEd25519Signature {
    param(
        [Parameter(Mandatory)]
        [string] $OpenSsl,

        [Parameter(Mandatory)]
        [string] $PublicKeyHex,

        [Parameter(Mandatory)]
        [string] $MessagePath,

        [Parameter(Mandatory)]
        [string] $SignatureHex
    )

    if ($PublicKeyHex -cnotmatch '^[0-9a-f]{64}$') {
        throw 'Release public key must be 64 lowercase hexadecimal characters.'
    }
    if ($SignatureHex -cnotmatch '^[0-9a-f]{128}$') {
        throw 'Release signature must be 128 lowercase hexadecimal characters.'
    }

    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    $temp = Join-Path $tempRoot ('claudometer-release-verify-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $temp | Out-Null
    try {
        $publicDerPath = Join-Path $temp 'public.der'
        $signaturePath = Join-Path $temp 'signature.bin'
        $publicDer = [Convert]::FromHexString('302a300506032b6570032100' + $PublicKeyHex)
        [IO.File]::WriteAllBytes($publicDerPath, $publicDer)
        [IO.File]::WriteAllBytes($signaturePath, [Convert]::FromHexString($SignatureHex))
        Invoke-ReleaseOpenSsl $OpenSsl @(
            'pkeyutl', '-verify', '-pubin', '-inkey', $publicDerPath,
            '-keyform', 'DER', '-rawin', '-in', $MessagePath, '-sigfile', $signaturePath
        )
    } finally {
        $resolved = [IO.Path]::GetFullPath($temp)
        if (-not $resolved.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -or
            -not ([IO.Path]::GetFileName($resolved)).StartsWith('claudometer-release-verify-')) {
            throw "Refusing to remove unexpected verification directory '$resolved'."
        }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
