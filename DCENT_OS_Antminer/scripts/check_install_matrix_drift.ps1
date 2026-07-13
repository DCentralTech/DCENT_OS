#Requires -Version 5.1
<#
.SYNOPSIS
  Fail if docs/architecture/install_matrix.tsv drifts from BoardDesc generator.

.DESCRIPTION
  Thin wrapper: cargo test -p dcentrald-common committed_install_matrix_tsv_matches_generator

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File DCENT_OS_Antminer/scripts/check_install_matrix_drift.ps1
#>
$ErrorActionPreference = "Stop"
$dcentrald = Resolve-Path (Join-Path $PSScriptRoot "..\dcentrald")
Push-Location $dcentrald
try {
    cargo test -p dcentrald-common committed_install_matrix_tsv_matches_generator --lib -- --test-threads=1
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Install matrix drift detected. Run: powershell -ExecutionPolicy Bypass -File DCENT_OS_Antminer/scripts/export_install_matrix.ps1"
        exit 1
    }
    Write-Output "install_matrix_tsv_OK"
    exit 0
} finally {
    Pop-Location
}
