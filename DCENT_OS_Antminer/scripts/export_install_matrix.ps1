#Requires -Version 5.1
<#
.SYNOPSIS
  Export BoardDesc install matrix TSV for docs/CI (requires cargo).

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
$out = Join-Path $root "projects\dcentos\docs\architecture\install_matrix.tsv"

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
    print!("{}", dcentrald_common::install_matrix_tsv());
}
'@
    $tmp = Join-Path $env:TEMP "dcent_export_matrix"
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
        $tsv = cargo run -q --release 2>$null | Out-String
        # Preserve tabs: do not use Format-* / Write-Host on the body.
        [System.IO.File]::WriteAllText($out, $tsv.TrimEnd() + "`n")
        if (-not (Test-Path $out) -or (Get-Item $out).Length -lt 10) {
            throw "export failed or empty"
        }
        Write-Output "wrote=$out"
        # Show first line with visible tab markers for operators.
        $first = (Get-Content -Raw $out).Split("`n")[0]
        Write-Output ("header_tabs={0}" -f (($first.ToCharArray() | Where-Object { $_ -eq [char]9 }).Count))
    } finally {
        Pop-Location
    }
} finally {
    Pop-Location
}
