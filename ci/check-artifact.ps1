[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string] $Path,

    [Parameter(Mandatory)]
    [string] $Target
)

$ErrorActionPreference = 'Stop'
$budgets = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'release-budgets.json') -Raw |
    ConvertFrom-Json
$targetBudget = $budgets.targets.$Target
if ($null -eq $targetBudget) {
    throw "No release budget is recorded for target '$Target'."
}

$artifact = Get-Item -LiteralPath $Path
$size = $artifact.Length
$bytes = [IO.File]::ReadAllBytes($artifact.FullName)
if ($bytes.Length -lt 64 -or $bytes[0] -ne 0x4d -or $bytes[1] -ne 0x5a) {
    throw "Artifact '$Path' is not a PE executable."
}
$peOffset = [BitConverter]::ToInt32($bytes, 0x3c)
if ($peOffset -lt 0 -or $peOffset + 6 -gt $bytes.Length -or
    $bytes[$peOffset] -ne 0x50 -or $bytes[$peOffset + 1] -ne 0x45 -or
    $bytes[$peOffset + 2] -ne 0 -or $bytes[$peOffset + 3] -ne 0) {
    throw "Artifact '$Path' has an invalid PE header."
}
$expectedMachine = switch ($Target) {
    'x86_64-pc-windows-msvc' { 0x8664 }
    'aarch64-pc-windows-msvc' { 0xaa64 }
    default { throw "No PE machine policy is recorded for target '$Target'." }
}
$actualMachine = [BitConverter]::ToUInt16($bytes, $peOffset + 4)
if ($actualMachine -ne $expectedMachine) {
    throw "Artifact PE machine 0x$($actualMachine.ToString('x4')) does not match '$Target'."
}

$cargo = Get-Content -LiteralPath 'Cargo.toml' -Raw
$versionMatch = [regex]::Match($cargo, '(?ms)^\[package\].*?^version\s*=\s*"([^"]+)"')
if (-not $versionMatch.Success) {
    throw 'Could not read the package version from Cargo.toml.'
}
$expectedVersion = $versionMatch.Groups[1].Value
$versionInfo = [Diagnostics.FileVersionInfo]::GetVersionInfo($artifact.FullName)
if ($versionInfo.FileVersion -cne $expectedVersion -or
    $versionInfo.ProductVersion -cne $expectedVersion) {
    throw "Artifact VERSIONINFO does not match Cargo.toml version '$expectedVersion'."
}

$hardCeiling = [int64] $budgets.hard_ceiling_bytes
if ($size -gt $hardCeiling) {
    throw "Artifact is $size bytes; hard ceiling is $hardCeiling bytes."
}

if ($null -ne $targetBudget.baseline_bytes) {
    $baseline = [int64] $targetBudget.baseline_bytes
    $tolerance = [double] $budgets.regression_tolerance_percent
    $regressionLimit = [int64] [Math]::Floor($baseline * (1.0 + $tolerance / 100.0))
    if ($size -gt $regressionLimit) {
        throw "Artifact is $size bytes; 10% regression limit is $regressionLimit bytes (baseline $baseline)."
    }
}

Write-Output "$Target artifact: $size bytes (hard ceiling: $hardCeiling)."
