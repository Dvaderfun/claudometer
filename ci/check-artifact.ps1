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
