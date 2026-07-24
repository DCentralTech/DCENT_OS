#Requires -Version 5.1
<#
.SYNOPSIS
  Export BoardDesc install matrix TSV/JSON for docs, CI, and Toolbox.

.DESCRIPTION
  Runs a tiny host-side Rust one-liner via cargo test filter is not ideal;
  instead prints guidance + regenerates from unit-test golden if cargo missing.

  Preferred: cargo test -p dcentrald-common install_matrix -- --nocapture
  and copy TSV from install_matrix_tsv() in a dedicated export binary later.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File DCENT_OS_Antminer/scripts/export_install_matrix.ps1
#>
$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..\..\..")
$dcentrald = Join-Path $root "projects\dcentos\dcentrald"
$tsvOut = Join-Path $root "projects\dcentos\docs\architecture\install_matrix.tsv"
$jsonOut = Join-Path $root "projects\dcentos\docs\architecture\hardware_enablement_matrix.json"
$producerJsonOut = Join-Path $root "projects\dcentos\docs\architecture\artifact_producers.json"
$toolboxJsonOut = Join-Path $root "projects\dcent-toolbox\src\dcent_toolbox\data\hardware_enablement_matrix.json"

function Write-Utf8NoBomAtomic([string]$Path, [string]$Content) {
    $directory = Split-Path -Parent $Path
    New-Item -ItemType Directory -Force -Path $directory | Out-Null
    $temporary = Join-Path $directory (".{0}.tmp" -f [System.IO.Path]::GetRandomFileName())
    try {
        $encoding = New-Object System.Text.UTF8Encoding($false)
        # PowerShell 5.1's Out-String emits host-native CRLF even when the Rust
        # generator emits canonical LF. Normalize before the atomic write so
        # running this exporter is byte-idempotent on Windows and Linux.
        $normalized = $Content.Replace("`r`n", "`n").Replace("`r", "`n")
        [System.IO.File]::WriteAllText(
            $temporary,
            $normalized.TrimEnd([char[]]"`n") + "`n",
            $encoding
        )
        Move-Item -LiteralPath $temporary -Destination $Path -Force
    } finally {
        if (Test-Path -LiteralPath $temporary) {
            Remove-Item -LiteralPath $temporary -Force
        }
    }
}

Push-Location $dcentrald
try {
    # Embed TSV generation in a one-shot cargo script via existing unit test surface:
    # We run a small rustc-free path: cargo test prints nothing useful, so invoke
    # `cargo test` that writes via an env flag is future work. For now, call
    # the library through `cargo test` helper binary pattern is unavailable.
    # Fallback: use `cargo run` if we had a bin — write static export by re-running
    # the pure function via `cargo test` + include_str golden.
    $code = @'
fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("json") => print!("{}", dcentrald_common::install_matrix_json()),
        Some("producers") => print!("{}", dcentrald_common::artifact_producer::artifact_producer_manifest_json()),
        Some("tsv") => print!("{}", dcentrald_common::install_matrix_tsv()),
        _ => panic!("usage: dcent_export_matrix <tsv|json|producers>"),
    }
}
'@
    $tmpRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
    $tmp = Join-Path $tmpRoot ("dcent_export_matrix-{0}" -f [System.Guid]::NewGuid().ToString("N"))
    $tmp = [System.IO.Path]::GetFullPath($tmp)
    if (-not $tmp.StartsWith($tmpRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "temporary export path escaped the host temp directory: $tmp"
    }
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    $src = Join-Path $tmp "src"
    New-Item -ItemType Directory -Force -Path $src | Out-Null
    Set-Content -Path (Join-Path $tmp "Cargo.toml") -Encoding utf8 -Value @"
[package]
name = "dcent_export_matrix"
version = "0.0.0"
edition = "2021"
[dependencies]
dcentrald-common = { path = "$($dcentrald -replace '\\','/')/dcentrald-common" }
"@
    Set-Content -Path (Join-Path $src "main.rs") -Encoding utf8 -Value $code
    Push-Location $tmp
    try {
        $tsv = cargo run -q --release -- tsv 2>$null | Out-String
        if ($LASTEXITCODE -ne 0) { throw "TSV export failed" }
        $json = cargo run -q --release -- json 2>$null | Out-String
        if ($LASTEXITCODE -ne 0) { throw "JSON export failed" }
        $producerJson = cargo run -q --release -- producers 2>$null | Out-String
        if ($LASTEXITCODE -ne 0) { throw "producer JSON export failed" }
        Write-Utf8NoBomAtomic $tsvOut $tsv
        Write-Utf8NoBomAtomic $jsonOut $json
        Write-Utf8NoBomAtomic $producerJsonOut $producerJson
        Write-Utf8NoBomAtomic $toolboxJsonOut $json
        if (
            (Get-Item $tsvOut).Length -lt 10 -or
            (Get-Item $jsonOut).Length -lt 10 -or
            (Get-Item $producerJsonOut).Length -lt 10
        ) {
            throw "export produced an empty matrix"
        }
        Write-Output "wrote=$tsvOut"
        Write-Output "wrote=$jsonOut"
        Write-Output "wrote=$producerJsonOut"
        Write-Output "wrote=$toolboxJsonOut"
        # Show first line with visible tab markers for operators.
        $first = (Get-Content -Raw $tsvOut).Split("`n")[0]
        Write-Output ("header_tabs={0}" -f (($first.ToCharArray() | Where-Object { $_ -eq [char]9 }).Count))
    } finally {
        Pop-Location
        if (Test-Path -LiteralPath $tmp) {
            Remove-Item -LiteralPath $tmp -Recurse -Force
        }
    }
} finally {
    Pop-Location
}
