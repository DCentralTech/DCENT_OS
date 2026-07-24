#Requires -Version 5.1
<#
.SYNOPSIS
  Fail if generated install-matrix projections drift from BoardDesc.

.DESCRIPTION
  Runs both Rust-generated TSV/JSON drift tests and verifies that Toolbox
  bundles the exact canonical JSON bytes.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File DCENT_OS_Antminer/scripts/check_install_matrix_drift.ps1
#>
$ErrorActionPreference = "Stop"
$dcentrald = Resolve-Path (Join-Path $PSScriptRoot "..\dcentrald")
Push-Location $dcentrald
try {
    $expectedTests = @(
        "install_matrix::tests::committed_install_matrix_json_matches_generator",
        "install_matrix::tests::committed_install_matrix_tsv_matches_generator"
    ) | Sort-Object
    $testPrefix = "install_matrix::tests::committed_install_matrix_"
    $listStdoutPath = $null
    $listStderrPath = $null
    try {
        $listStdoutPath = [IO.Path]::GetTempFileName()
        $listStderrPath = [IO.Path]::GetTempFileName()
        $savedErrorActionPreference = $ErrorActionPreference
        try {
            $ErrorActionPreference = "Continue"
            & cargo test --locked -p dcentrald-common --lib -- --list `
                1> $listStdoutPath 2> $listStderrPath
            $listExitCode = $LASTEXITCODE
        } finally {
            $ErrorActionPreference = $savedErrorActionPreference
        }
        $listOutput = @(Get-Content -LiteralPath $listStdoutPath)
        $listDiagnostics = @(Get-Content -LiteralPath $listStderrPath)
    } finally {
        $listCapturePaths = @($listStdoutPath, $listStderrPath) |
            Where-Object { -not [String]::IsNullOrEmpty($_) }
        if ($listCapturePaths.Count -ne 0) {
            Remove-Item -Force -LiteralPath $listCapturePaths
        }
    }
    if ($listExitCode -ne 0) {
        Write-Error ("Install matrix test inventory failed:`n" +
            (($listOutput + $listDiagnostics) -join [Environment]::NewLine))
        exit 1
    }
    $listedTests = @(
        $listOutput |
            ForEach-Object {
                $line = $_.Trim()
                if ($line.StartsWith($testPrefix, [StringComparison]::Ordinal) -and
                    $line.EndsWith(": test", [StringComparison]::Ordinal)) {
                    $line.Substring(0, $line.Length - ": test".Length)
                }
            } |
            Sort-Object
    )
    $selectorDrift = $listedTests.Count -ne $expectedTests.Count
    if (-not $selectorDrift) {
        for ($index = 0; $index -lt $expectedTests.Count; $index++) {
            if (-not [String]::Equals(
                $expectedTests[$index],
                $listedTests[$index],
                [StringComparison]::Ordinal
            )) {
                $selectorDrift = $true
                break
            }
        }
    }
    if ($selectorDrift) {
        Write-Error ("Install matrix test inventory drifted; expected exactly:`n  " +
            ($expectedTests -join "`n  ") + "`nObserved:`n  " +
            ($listedTests -join "`n  "))
        exit 1
    }
    foreach ($testName in $expectedTests) {
        & cargo test --locked -p dcentrald-common --lib $testName -- `
            --exact --include-ignored --test-threads=1
        if ($LASTEXITCODE -ne 0) {
            Write-Error "Install matrix drift detected in $testName. Run: powershell -ExecutionPolicy Bypass -File DCENT_OS_Antminer/scripts/export_install_matrix.ps1"
            exit 1
        }
    }
    $canonical = Resolve-Path (Join-Path $PSScriptRoot "..\docs\architecture\hardware_enablement_matrix.json")
    $toolbox = Resolve-Path (Join-Path $PSScriptRoot "..\..\dcent-toolbox\src\dcent_toolbox\data\hardware_enablement_matrix.json")
    $canonicalSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $canonical).Hash
    $toolboxSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $toolbox).Hash
    if ($canonicalSha256 -ne $toolboxSha256) {
        Write-Error "Toolbox bundled hardware enablement matrix drifted. Run scripts/export_install_matrix.ps1"
        exit 1
    }
    Write-Output "install_matrix_tsv_json_toolbox_OK"
    exit 0
} finally {
    Pop-Location
}
