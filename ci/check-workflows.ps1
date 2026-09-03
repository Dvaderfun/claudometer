[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$workflowFiles = Get-ChildItem -LiteralPath '.github/workflows' -Filter '*.yml' -File
$violations = [System.Collections.Generic.List[string]]::new()

foreach ($file in $workflowFiles) {
    $text = Get-Content -LiteralPath $file.FullName -Raw
    $uses = [regex]::Matches($text, '(?m)^\s*uses:\s*(?<value>\S+)')
    foreach ($use in $uses) {
        $value = $use.Groups['value'].Value
        if ($value.StartsWith('./')) {
            continue
        }
        if ($value -notmatch '@[0-9a-f]{40}$') {
            $violations.Add("$($file.Name): action is not pinned to a full commit SHA: $value")
        }
    }
}

$build = Get-Content -LiteralPath '.github/workflows/build.yml' -Raw
$requiredCommands = @(
    'cargo fmt --all -- --check',
    'cargo clippy --locked --workspace --all-targets --all-features -- -D warnings',
    'cargo test --locked --workspace --all-targets --all-features',
    './ci/check-privacy.ps1',
    'cargo build --locked --release --target ${{ matrix.target }}'
)
foreach ($command in $requiredCommands) {
    if (-not $build.Contains($command)) {
        $violations.Add("build.yml is missing required command: $command")
    }
}
$releaseBuildCommand = 'cargo build --locked --release --target ${{ matrix.target }}'
if ([regex]::Matches($build, [regex]::Escape($releaseBuildCommand)).Count -ne 1) {
    $violations.Add('build.yml must build each immutable release artifact exactly once.')
}

$release = Get-Content -LiteralPath '.github/workflows/release.yml' -Raw
if (-not $release.Contains('uses: ./.github/workflows/build.yml')) {
    $violations.Add('release.yml does not call the unified source gate.')
}
if ($release.Contains('cargo build')) {
    $violations.Add('release.yml must reuse tested artifacts instead of rebuilding them.')
}
$releaseRequirements = @(
    'gh release create $env:GITHUB_REF_NAME @assets',
    '--draft',
    'actions/attest@',
    'anchore/sbom-action@',
    './ci/new-release-evidence.ps1',
    './ci/check-release-infrastructure.ps1',
    'gh release download $env:GITHUB_REF_NAME',
    'gh attestation verify $exe.FullName',
    './ci/verify-demo.ps1',
    'gh release edit $env:GITHUB_REF_NAME',
    '--json isDraft,isImmutable'
)
foreach ($requirement in $releaseRequirements) {
    if (-not $release.Contains($requirement)) {
        $violations.Add("release.yml is missing release evidence policy: $requirement")
    }
}
if ([regex]::Matches(
        $release,
        [regex]::Escape('./ci/check-release-infrastructure.ps1')
    ).Count -ne 2) {
    $violations.Add('release.yml must check protected infrastructure before signing and before publishing.')
}
$infrastructure = Get-Content -LiteralPath 'ci/check-release-infrastructure.ps1' -Raw
foreach ($policy in @('immutable-releases', 'refs/heads/main', 'refs/tags/v*', 'required_reviewers')) {
    if (-not $infrastructure.Contains($policy)) {
        $violations.Add("release infrastructure check is missing policy: $policy")
    }
}

if ($violations.Count -ne 0) {
    throw "Workflow policy violations:`n$($violations -join "`n")"
}

Write-Output "Workflow policy passed for $($workflowFiles.Count) files."
