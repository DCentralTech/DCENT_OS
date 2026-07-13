#Requires -Version 5.1
<#
.SYNOPSIS
  Inventory unique DCENT_* env tokens in dcentrald and optionally fail if count grows.

.DESCRIPTION
  ADR-0012 growth control. Regenerates the unique-token list and compares against
  a baseline count. Does not require Rust toolchain.

.PARAMETER FailIfAbove
  If set, exit 1 when unique token count exceeds this number (default: 260, soft ceiling).

.PARAMETER WriteRawList
  If set, write DCENT_OS_Antminer/docs/architecture/_env_vars_raw.txt

.EXAMPLE
  pwsh -File DCENT_OS_Antminer/scripts/check_dcent_env_surface.ps1 -WriteRawList
#>
param(
    [int]$FailIfAbove = 260,
    [switch]$WriteRawList
)

$ErrorActionPreference = "Stop"
$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..\..\..")
$dcentrald = Join-Path $repoRoot "projects\dcentos\dcentrald"
if (-not (Test-Path $dcentrald)) {
    Write-Error "dcentrald path not found: $dcentrald"
}

$rg = Get-Command rg -ErrorAction SilentlyContinue
if (-not $rg) {
    Write-Error "ripgrep (rg) is required on PATH"
}

$matches = & rg -o "DCENT_[A-Z0-9_]+" $dcentrald -g "*.rs" 2>$null
$tokens = @(
    $matches | ForEach-Object {
        if ($_ -match 'DCENT_[A-Z0-9_]+') { $Matches[0] }
    } | Sort-Object -Unique
)
$count = $tokens.Count
Write-Output "unique_DCENT_env_tokens=$count"
Write-Output "fail_if_above=$FailIfAbove"

$am2 = @($tokens | Where-Object { $_ -like 'DCENT_AM2_*' }).Count
$am3 = @($tokens | Where-Object { $_ -like 'DCENT_AM3_*' }).Count
Write-Output "DCENT_AM2_count=$am2"
Write-Output "DCENT_AM3_count=$am3"

if ($WriteRawList) {
    $outDir = Join-Path $repoRoot "projects\dcentos\docs\architecture"
    New-Item -ItemType Directory -Force -Path $outDir | Out-Null
    $outFile = Join-Path $outDir "_env_vars_raw.txt"
    $tokens | Set-Content -Encoding utf8 $outFile
    Write-Output "wrote=$outFile"
}

if ($count -gt $FailIfAbove) {
    Write-Error "Env surface grew past ceiling: $count > $FailIfAbove (ADR-0012). Prefer BoardDesc/TOML over new DCENT_* product flags."
    exit 1
}

Write-Output "OK"
exit 0
