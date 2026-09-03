[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$')]
    [string] $Repository
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Invoke-GitHubApi {
    param([Parameter(Mandatory)][string] $Path)

    $json = gh api -H 'X-GitHub-Api-Version: 2026-03-10' $Path
    if ($LASTEXITCODE -ne 0) {
        throw "GitHub API request failed for '$Path'."
    }
    return $json | ConvertFrom-Json
}

function Test-RefCondition {
    param(
        [Parameter(Mandatory)] $Ruleset,
        [Parameter(Mandatory)][string] $Target,
        [Parameter(Mandatory)][string] $Reference
    )

    if ($Ruleset.enforcement -cne 'active' -or $Ruleset.target -cne $Target) {
        return $false
    }
    $includes = @($Ruleset.conditions.ref_name.include)
    $excludes = @($Ruleset.conditions.ref_name.exclude)
    return $Reference -cin $includes -and $Reference -cnotin $excludes
}

$immutable = Invoke-GitHubApi "repos/$Repository/immutable-releases"
if ($immutable.enabled -ne $true) {
    throw 'GitHub immutable releases are not enabled.'
}

$summaries = @(Invoke-GitHubApi "repos/$Repository/rulesets?per_page=100")
$rulesets = @(
    foreach ($summary in $summaries) {
        Invoke-GitHubApi "repos/$Repository/rulesets/$($summary.id)"
    }
)

$requiredChecks = @(
    'Source gate',
    'RustSec and OSV',
    'Dependency review',
    'Release build (x64)',
    'Release build (arm64)'
)
$mainProtected = $false
foreach ($ruleset in $rulesets) {
    if (-not (Test-RefCondition $ruleset 'branch' 'refs/heads/main') -or
        @($ruleset.bypass_actors).Count -ne 0) {
        continue
    }
    $rules = @($ruleset.rules)
    $types = @($rules.type)
    $pullRequest = $rules | Where-Object type -CEQ 'pull_request' | Select-Object -First 1
    $statusChecks = $rules |
        Where-Object type -CEQ 'required_status_checks' |
        Select-Object -First 1
    if ('deletion' -cnotin $types -or 'non_fast_forward' -cnotin $types -or
        $null -eq $pullRequest -or $null -eq $statusChecks) {
        continue
    }
    $contexts = @($statusChecks.parameters.required_status_checks.context)
    $missingChecks = @($requiredChecks | Where-Object { $_ -cnotin $contexts })
    if ($pullRequest.parameters.required_approving_review_count -eq 0 -and
        $pullRequest.parameters.dismiss_stale_reviews_on_push -eq $true -and
        $pullRequest.parameters.require_last_push_approval -eq $false -and
        $pullRequest.parameters.required_review_thread_resolution -eq $true -and
        $statusChecks.parameters.strict_required_status_checks_policy -eq $true -and
        $missingChecks.Count -eq 0) {
        $mainProtected = $true
        break
    }
}
if (-not $mainProtected) {
    throw 'No active no-bypass main ruleset enforces pull requests, current checks, deletion, and force-push protection.'
}

$tagCreationProtected = $false
$tagMutationProtected = $false
foreach ($ruleset in $rulesets) {
    if (-not (Test-RefCondition $ruleset 'tag' 'refs/tags/v*')) {
        continue
    }
    $types = @($ruleset.rules.type)
    if ('creation' -cin $types -and @($ruleset.bypass_actors).Count -gt 0) {
        $tagCreationProtected = $true
    }
    if ('update' -cin $types -and 'deletion' -cin $types -and
        @($ruleset.bypass_actors).Count -eq 0) {
        $tagMutationProtected = $true
    }
}
if (-not $tagCreationProtected -or -not $tagMutationProtected) {
    throw 'Release tags need a release-role creation ruleset and a separate no-bypass update/deletion ruleset.'
}

$environment = Invoke-GitHubApi "repos/$Repository/environments/release"
$reviewRule = @($environment.protection_rules) |
    Where-Object type -CEQ 'required_reviewers' |
    Select-Object -First 1
if ($null -eq $reviewRule -or @($reviewRule.reviewers).Count -eq 0) {
    throw "The 'release' environment has no required reviewer."
}
if ($reviewRule.prevent_self_review -ne $false) {
    throw "The solo-maintainer 'release' environment must permit its owner to approve."
}
if ($environment.can_admins_bypass -ne $false) {
    throw "The 'release' environment permits administrator bypass."
}
if ($environment.deployment_branch_policy.custom_branch_policies -ne $true) {
    throw "The 'release' environment is not restricted to selected refs."
}
$deploymentPolicies = Invoke-GitHubApi `
    "repos/$Repository/environments/release/deployment-branch-policies?per_page=100"
$releaseTagPolicy = @($deploymentPolicies.branch_policies) |
    Where-Object { $_.name -ceq 'v*' -and $_.type -ceq 'tag' }
if (@($releaseTagPolicy).Count -ne 1) {
    throw "The 'release' environment must allow exactly the 'v*' tag pattern."
}

Write-Output 'Release infrastructure is protected: main, v* tags, release environment, and immutable releases.'
