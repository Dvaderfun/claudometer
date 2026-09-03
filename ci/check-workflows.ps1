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

$release = Get-Content -LiteralPath '.github/workflows/release.yml' -Raw
if (-not $release.Contains('uses: ./.github/workflows/build.yml')) {
    $violations.Add('release.yml does not call the unified source gate.')
}
if (-not $release.Contains('draft: true')) {
    $violations.Add('release.yml must create a draft before publication.')
}

if ($violations.Count -ne 0) {
    throw "Workflow policy violations:`n$($violations -join "`n")"
}

Write-Output "Workflow policy passed for $($workflowFiles.Count) files."
