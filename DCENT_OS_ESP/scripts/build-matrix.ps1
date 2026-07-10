param(
    [string]$BuildRoot,
    [string]$DistRoot,
    [switch]$IncludeInternalTargets
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$root = Join-Path $PSScriptRoot ".."
if ([string]::IsNullOrWhiteSpace($BuildRoot)) {
    $BuildRoot = Join-Path $root "build-matrix"
}
if ([string]::IsNullOrWhiteSpace($DistRoot)) {
    $DistRoot = Join-Path $root "dist"
}

if ([string]::IsNullOrWhiteSpace($env:CC_xtensa_esp32s3_espidf)) {
    $env:CC_xtensa_esp32s3_espidf = "xtensa-esp32s3-elf-gcc"
}

Push-Location $root

$publicTargets = @(
    @{ Feature = "bitaxe-max"; BoardTarget = "bitaxe-max" },
    @{ Feature = "bitaxe-ultra"; BoardTarget = "bitaxe-ultra" },
    @{ Feature = "bitaxe-supra"; BoardTarget = "bitaxe-supra" },
    @{ Feature = "bitaxe-gamma"; BoardTarget = "bitaxe-gamma" },
    @{ Feature = "bitaxe-hex-ultra"; BoardTarget = "bitaxe-hex-ultra" },
    @{ Feature = "bitaxe-hex-supra"; BoardTarget = "bitaxe-hex-supra" }
)

$internalTargets = @(
    @{ Feature = "bitaxe-gamma-duo"; BoardTarget = "bitaxe-gamma-duo" },
    @{ Feature = "bitaxe-gt"; BoardTarget = "bitaxe-gt" },
    @{ Feature = "bitaxe-touch"; BoardTarget = "bitaxe-touch" },
    @{ Feature = "bitaxe-gt-touch"; BoardTarget = "bitaxe-gt-touch" },
    @{ Feature = "nerdnos"; BoardTarget = "nerdnos" },
    @{ Feature = "nerdaxe"; BoardTarget = "nerdaxe" },
    @{ Feature = "nerdqaxe-plus"; BoardTarget = "nerdqaxe-plus" },
    @{ Feature = "nerdqaxe-pp"; BoardTarget = "nerdqaxe-pp" }
)

$targets = $publicTargets
if ($IncludeInternalTargets) {
    $targets += $internalTargets
}

foreach ($target in $targets) {
    $cargoTargetDir = Join-Path $BuildRoot $target.BoardTarget
    $releaseDir = Join-Path $cargoTargetDir "xtensa-esp32s3-espidf\release"

    Write-Host "==> Building $($target.BoardTarget) ($($target.Feature))"
    $env:CARGO_TARGET_DIR = $cargoTargetDir
    cargo build --locked --release -p dcentaxe --no-default-features --features $target.Feature

    Write-Host "==> Packaging $($target.BoardTarget)"
    & (Join-Path $PSScriptRoot "package-firmware.ps1") `
        -TargetDir $releaseDir `
        -BoardTarget $target.BoardTarget `
        -OutDir (Join-Path $DistRoot $target.BoardTarget)
}

Pop-Location
