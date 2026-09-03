[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$violations = [System.Collections.Generic.List[string]]::new()
$privacy = Get-Content -LiteralPath 'PRIVACY.md' -Raw
$networkSource = Get-Content -LiteralPath 'src/network.rs' -Raw

function Get-RuntimeSource([string]$path) {
    $source = Get-Content -LiteralPath $path -Raw
    $testStart = $source.IndexOf('#[cfg(test)]', [StringComparison]::Ordinal)
    if ($testStart -ge 0) {
        return $source.Substring(0, $testStart)
    }
    return $source
}

$destinationPattern = '(?ms)^(?:pub )?const (?<name>[A-Z0-9_]+): &str =\s*"(?<url>https?://[^"\r\n]+)";'
$destinations = [regex]::Matches($networkSource, $destinationPattern)
$networkUrls = [regex]::Matches($networkSource, 'https?://[^"\s]+')
if ($networkUrls.Count -ne $destinations.Count) {
    $violations.Add('every URL in src/network.rs must be declared as a named string constant')
}
foreach ($destination in $destinations) {
    $name = $destination.Groups['name'].Value
    $url = $destination.Groups['url'].Value
    if (-not $url.StartsWith('https://', [StringComparison]::Ordinal)) {
        $violations.Add("network destination is not HTTPS: $url")
    }
    if (-not $privacy.Contains("``$name``")) {
        $violations.Add("PRIVACY.md does not name network destination constant $name")
    }
    if (-not $privacy.Contains("``$url``")) {
        $violations.Add("PRIVACY.md does not document network destination $url")
    }
}

$sourceFiles = Get-ChildItem -LiteralPath 'src' -Filter '*.rs' -File
foreach ($file in $sourceFiles) {
    if ($file.Name -eq 'network.rs') {
        continue
    }
    $runtimeSource = Get-RuntimeSource $file.FullName
    foreach ($url in [regex]::Matches($runtimeSource, 'https?://[^"\s]+')) {
        $violations.Add("$($file.Name) contains a runtime URL outside src/network.rs: $($url.Value)")
    }
}

$actualSites = [System.Collections.Generic.List[string]]::new()
$requestExecutionCount = 0
$requestPattern = 'crate::network::get\s*\(\s*[^,]+,\s*(?<target>[^)\r\n]+)\s*\)'
$executionPattern = '\.(?:call|send|send_json|send_bytes|send_string|send_form)\s*\('
foreach ($file in $sourceFiles) {
    $runtimeSource = Get-RuntimeSource $file.FullName
    $requestExecutionCount += [regex]::Matches($runtimeSource, $executionPattern).Count
    foreach ($request in [regex]::Matches($runtimeSource, $requestPattern)) {
        $target = $request.Groups['target'].Value.Trim() -replace '^crate::network::', ''
        $actualSites.Add("src/$($file.Name)|$target")
    }
}

if ($requestExecutionCount -ne $actualSites.Count) {
    $violations.Add("found $requestExecutionCount direct HTTP executions but $($actualSites.Count) allowlisted network::get sites")
}

$documentedSites = [System.Collections.Generic.List[string]]::new()
$sitePattern = '(?m)^<!-- PRIVACY_REQUEST (?<site>[^>]+) -->\s*$'
foreach ($site in [regex]::Matches($privacy, $sitePattern)) {
    $documentedSites.Add($site.Groups['site'].Value.Trim())
}
$actual = ($actualSites | Sort-Object) -join "`n"
$documented = ($documentedSites | Sort-Object) -join "`n"
if ($actual -cne $documented) {
    $violations.Add("request-site allowlist differs from PRIVACY.md`nActual:`n$actual`nDocumented:`n$documented")
}

if ($violations.Count -ne 0) {
    throw "Privacy network allowlist violations:`n$($violations -join "`n")"
}

Write-Output "Privacy network allowlist passed for $($destinations.Count) destinations and $($actualSites.Count) request sites."
