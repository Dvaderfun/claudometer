[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$metadataText = cargo metadata --locked --format-version 1
if ($LASTEXITCODE -ne 0) {
    throw 'cargo metadata failed.'
}
$metadata = $metadataText | ConvertFrom-Json

$allowedSource = 'registry+https://github.com/rust-lang/crates.io-index'
$allowedLicenses = @(
    '(MIT OR Apache-2.0) AND Unicode-3.0',
    'Apache-2.0',
    'Apache-2.0 OR MIT',
    'Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT',
    'BSD-3-Clause',
    'MIT',
    'MIT OR Apache-2.0',
    'MIT OR Apache-2.0 OR BSD-1-Clause',
    'MIT OR Apache-2.0 OR LGPL-2.1-or-later',
    'MIT/Apache-2.0',
    'Unicode-3.0',
    'Unlicense OR MIT'
)

$violations = [System.Collections.Generic.List[string]]::new()
foreach ($package in $metadata.packages) {
    if ($metadata.workspace_members -contains $package.id) {
        continue
    }
    if ($package.source -ne $allowedSource) {
        $violations.Add("$($package.name) $($package.version): unapproved source '$($package.source)'")
    }
    if ($allowedLicenses -notcontains $package.license) {
        $violations.Add("$($package.name) $($package.version): unreviewed license '$($package.license)'")
    }
}

if ($violations.Count -ne 0) {
    throw "Dependency policy violations:`n$($violations -join "`n")"
}

$dependencyCount = ($metadata.packages |
    Where-Object { $metadata.workspace_members -notcontains $_.id }).Count
Write-Output "Dependency policy passed for $dependencyCount registry packages."
