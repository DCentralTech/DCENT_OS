param(
    [string]$TargetDir = "C:\bt\xtensa-esp32s3-espidf\release",
    [string]$BoardTarget,
    [string]$Version,
    [string]$OutDir,
    [string]$ElfPath,
    [string]$PartitionsCsv,
    [string]$OtaAppPartition = $env:DCENT_OTA_APP_PARTITION,
    [string]$DeviceModel = $env:DCENT_DEVICE_MODEL,
    [string]$Esptool = "python",
    [string]$SigningKeyPem = $env:DCENT_OTA_PRIVATE_KEY_PEM,
    [string]$SigningKeyId = $env:DCENT_OTA_KEY_ID,
    [string]$PublicKeyHex = $env:DCENT_OTA_PUBLIC_KEY_HEX,
    [string]$EnforceSignedOta = $env:DCENT_ENFORCE_SIGNED_OTA
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Require-Path {
    param([string]$Path, [string]$Label)
    if (-not (Test-Path -LiteralPath $Path)) {
        throw "$Label not found: $Path"
    }
}

function Get-WorkspaceVersion {
    $cargoToml = Join-Path $PSScriptRoot "..\Cargo.toml"
    $match = Select-String -Path $cargoToml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
    if (-not $match) {
        throw "Unable to determine workspace version from $cargoToml"
    }
    return $match.Matches[0].Groups[1].Value
}

function Get-OtaDataPath {
    param([string]$Dir)
    $candidate = Get-ChildItem -Path (Join-Path $Dir "build") -Recurse -Filter "ota_data_initial.bin" | Select-Object -First 1
    if (-not $candidate) {
        throw "ota_data_initial.bin not found under $Dir\build"
    }
    return $candidate.FullName
}

function Convert-PartitionSizeToBytes {
    param([string]$Value)
    $trimmed = $Value.Trim()
    if ($trimmed.StartsWith("0x", [System.StringComparison]::OrdinalIgnoreCase)) {
        return [Convert]::ToInt64($trimmed.Substring(2), 16)
    }
    return [Int64]$trimmed
}

function Get-OtaSlotSizeBytes {
    param([string]$Path, [string]$PartitionName)

    foreach ($line in Get-Content -LiteralPath $Path) {
        $trimmed = $line.Trim()
        if ([string]::IsNullOrWhiteSpace($trimmed) -or $trimmed.StartsWith("#")) {
            continue
        }

        $columns = $trimmed.Split(",") | ForEach-Object { $_.Trim() }
        if ($columns.Count -ge 5 -and $columns[0] -eq $PartitionName -and $columns[1] -eq "app") {
            return Convert-PartitionSizeToBytes -Value $columns[4]
        }
    }

    throw "$PartitionName app partition not found in $Path"
}

function Get-PartitionOffsetBytes {
    param([string]$Path, [string]$PartitionName)

    foreach ($line in Get-Content -LiteralPath $Path) {
        $trimmed = $line.Trim()
        if ([string]::IsNullOrWhiteSpace($trimmed) -or $trimmed.StartsWith("#")) {
            continue
        }

        $columns = $trimmed.Split(",") | ForEach-Object { $_.Trim() }
        if ($columns.Count -ge 5 -and $columns[0] -eq $PartitionName) {
            return Convert-PartitionSizeToBytes -Value $columns[3]
        }
    }

    throw "$PartitionName partition not found in $Path"
}

function Get-BuildArtifactPath {
    param([string]$Dir, [string]$Filter)
    $candidate = Get-ChildItem -Path (Join-Path $Dir "build") -Recurse -Filter $Filter | Select-Object -First 1
    if (-not $candidate) {
        throw "$Filter not found under $Dir\build"
    }
    return $candidate.FullName
}

function Get-HexString {
    param([string]$Path)
    return ([System.BitConverter]::ToString([System.IO.File]::ReadAllBytes($Path))).Replace("-", "").ToLowerInvariant()
}

function Get-PublicKeyHexFromPem {
    param([string]$Path)
    $pubPath = [System.IO.Path]::GetTempFileName()
    try {
        & openssl pkey -in $Path -pubout -outform DER -out $pubPath | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "OpenSSL public key export failed"
        }
        $bytes = [System.IO.File]::ReadAllBytes($pubPath)
        return ([System.BitConverter]::ToString($bytes[($bytes.Length-32)..($bytes.Length-1)])).Replace("-", "").ToLowerInvariant()
    }
    finally {
        Remove-Item -LiteralPath $pubPath -Force -ErrorAction SilentlyContinue
    }
}

function Sign-OtaMetadata {
    param(
        [string]$Message
    )

    if ([string]::IsNullOrWhiteSpace($SigningKeyPem) -or -not (Test-Path -LiteralPath $SigningKeyPem)) {
        return $null
    }

    $msgPath = [System.IO.Path]::GetTempFileName()
    $sigPath = [System.IO.Path]::GetTempFileName()
    try {
        [System.IO.File]::WriteAllBytes($msgPath, [System.Text.Encoding]::ASCII.GetBytes($Message))
        & openssl pkeyutl -sign -rawin -inkey $SigningKeyPem -in $msgPath -out $sigPath | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "OpenSSL signing failed"
        }
        return Get-HexString -Path $sigPath
    }
    finally {
        Remove-Item -LiteralPath $msgPath,$sigPath -Force -ErrorAction SilentlyContinue
    }
}

function Resolve-DeviceModel {
    param([string]$BoardTarget)

    switch ($BoardTarget) {
        "bitaxe-max" { return "max" }
        "bitaxe-ultra" { return "ultra" }
        "bitaxe-supra" { return "supra" }
        "bitaxe-gamma" { return "gamma" }
        "bitaxe-gamma-duo" { return "gammaduo" }
        "bitaxe-gt" { return "gammaturbo" }
        "bitaxe-touch" { return "touch" }
        "bitaxe-gt-touch" { return "gt_touch" }
        "bitaxe-hex-ultra" { return "hexultra" }
        "bitaxe-hex-supra" { return "suprahex" }
        "nerdnos" { return "nerdnos" }
        "nerdaxe" { return "nerdaxe" }
        "nerdqaxe-plus" { return "nerdqaxeplus" }
        "nerdqaxe-pp" { return "nerdqaxepp" }
        "dcent-axe-bm1397" { return "dcentaxe_bm1397" }
        "dcent-axe-quad-bm1397" { return "dcentaxe_quad_bm1397" }
        "dcent-axe-hex-bm1397" { return "dcentaxe_hex_bm1397" }
        default { throw "Unknown -BoardTarget '$BoardTarget'" }
    }
}

if (-not [string]::IsNullOrWhiteSpace($EnforceSignedOta)) {
    if ([string]::IsNullOrWhiteSpace($SigningKeyPem) -or -not (Test-Path -LiteralPath $SigningKeyPem)) {
        throw "Signed OTA packaging requires DCENT_OTA_PRIVATE_KEY_PEM"
    }
    if ([string]::IsNullOrWhiteSpace($PublicKeyHex)) {
        throw "Signed OTA packaging requires DCENT_OTA_PUBLIC_KEY_HEX"
    }
    if ([string]::IsNullOrWhiteSpace($SigningKeyId)) {
        throw "Signed OTA packaging requires DCENT_OTA_KEY_ID"
    }
}

if ([string]::IsNullOrWhiteSpace($BoardTarget)) {
    throw "-BoardTarget is required"
}

$defaultDeviceModel = Resolve-DeviceModel -BoardTarget $BoardTarget

if ([string]::IsNullOrWhiteSpace($Version)) {
    $Version = Get-WorkspaceVersion
}

if ([string]::IsNullOrWhiteSpace($OutDir)) {
    $OutDir = Join-Path $PSScriptRoot "..\dist\$BoardTarget"
}

if ([string]::IsNullOrWhiteSpace($PartitionsCsv)) {
    $PartitionsCsv = Join-Path $PSScriptRoot "..\partitions.csv"
}

if ([string]::IsNullOrWhiteSpace($OtaAppPartition)) {
    $OtaAppPartition = "ota_0"
}

if ([string]::IsNullOrWhiteSpace($DeviceModel)) {
    $DeviceModel = $defaultDeviceModel
}

if ([string]::IsNullOrWhiteSpace($ElfPath)) {
    $ElfPath = Join-Path $TargetDir "dcentaxe"
}

$bootloader = Get-BuildArtifactPath -Dir $TargetDir -Filter "bootloader.bin"
$partitionTable = Get-BuildArtifactPath -Dir $TargetDir -Filter "partition-table.bin"
$otaData = Get-OtaDataPath -Dir $TargetDir

Require-Path -Path $ElfPath -Label "ELF"
Require-Path -Path $bootloader -Label "Bootloader"
Require-Path -Path $partitionTable -Label "Partition table"
Require-Path -Path $PartitionsCsv -Label "Partition CSV"
$otaAppOffset = Get-PartitionOffsetBytes -Path $PartitionsCsv -PartitionName $OtaAppPartition
$otaDataOffset = Get-PartitionOffsetBytes -Path $PartitionsCsv -PartitionName "otadata"

if (-not [string]::IsNullOrWhiteSpace($SigningKeyPem) -and -not [string]::IsNullOrWhiteSpace($PublicKeyHex)) {
    $derivedPublic = Get-PublicKeyHexFromPem -Path $SigningKeyPem
    if ($derivedPublic -ne $PublicKeyHex.ToLowerInvariant()) {
        throw "Signing private key does not match DCENT_OTA_PUBLIC_KEY_HEX"
    }
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$prefix = "dcentaxe-$BoardTarget-$Version"
$updateBin = Join-Path $OutDir "$prefix-update.bin"
$factoryBin = Join-Path $OutDir "$prefix-factory.bin"
$manifestPath = Join-Path $OutDir "$prefix-manifest.json"
$checksumsPath = Join-Path $OutDir "$prefix-SHA256SUMS.txt"

& $Esptool -m esptool --chip esp32s3 elf2image --output $updateBin $ElfPath
& $Esptool -m esptool --chip esp32s3 merge_bin --flash_mode dio --flash_size 16MB --flash_freq 80m `
    0x0 $bootloader `
    0x8000 $partitionTable `
    $otaDataOffset $otaData `
    $otaAppOffset $updateBin `
    -o $factoryBin

$updateSha = (Get-FileHash -LiteralPath $updateBin -Algorithm SHA256).Hash.ToLowerInvariant()
$factorySha = (Get-FileHash -LiteralPath $factoryBin -Algorithm SHA256).Hash.ToLowerInvariant()
$bootloaderSha = (Get-FileHash -LiteralPath $bootloader -Algorithm SHA256).Hash.ToLowerInvariant()
$partitionTableSha = (Get-FileHash -LiteralPath $partitionTable -Algorithm SHA256).Hash.ToLowerInvariant()
$otaDataSha = (Get-FileHash -LiteralPath $otaData -Algorithm SHA256).Hash.ToLowerInvariant()
$updateSize = (Get-Item -LiteralPath $updateBin).Length
$factorySize = (Get-Item -LiteralPath $factoryBin).Length
$bootloaderSize = (Get-Item -LiteralPath $bootloader).Length
$partitionTableSize = (Get-Item -LiteralPath $partitionTable).Length
$otaDataSize = (Get-Item -LiteralPath $otaData).Length
$otaSlotSize = Get-OtaSlotSizeBytes -Path $PartitionsCsv -PartitionName $OtaAppPartition
# XPH-7: compute the slot-fit verdict instead of hard-coding `$true` in the
# manifest. The throw below stays load-bearing (an over-slot image must NEVER
# ship), but `$updateFitsSlot` is derived from the same size comparison so a
# future refactor that removes the throw can't leave a stale, dishonest
# `updateFitsSlot = $true` for a truncated image.
if ($updateSize -gt $otaSlotSize) {
    $updateFitsSlot = $false
    throw "Update image exceeds $OtaAppPartition slot: $updateSize > $otaSlotSize bytes"
} else {
    $updateFitsSlot = $true
}
$normalizedDeviceModel = $DeviceModel.Trim().ToLowerInvariant()
$otaMessage = "schema=2`nboard_target=$BoardTarget`ndevice_model=$normalizedDeviceModel`nversion=$Version`nsize=$updateSize`nsha256=$updateSha`n"
$bundleMessage = "schema=2`nboard_target=$BoardTarget`ndevice_model=$normalizedDeviceModel`nversion=$Version`nupdate_size=$updateSize`nupdate_sha256=$updateSha`nfactory_size=$factorySize`nfactory_sha256=$factorySha`n"
$otaSignature = Sign-OtaMetadata -Message $otaMessage
$bundleSignature = Sign-OtaMetadata -Message $bundleMessage
if (-not [string]::IsNullOrWhiteSpace($EnforceSignedOta) -and [string]::IsNullOrWhiteSpace($otaSignature)) {
    throw "Signed OTA packaging required but no signature was produced"
}
if ((-not [string]::IsNullOrWhiteSpace($otaSignature) -or -not [string]::IsNullOrWhiteSpace($bundleSignature)) -and [string]::IsNullOrWhiteSpace($SigningKeyId)) {
    throw "Signed OTA packaging requires DCENT_OTA_KEY_ID"
}

@(
    "$factorySha  $([System.IO.Path]::GetFileName($factoryBin))",
    "$updateSha  $([System.IO.Path]::GetFileName($updateBin))"
) | Set-Content -LiteralPath $checksumsPath -Encoding ASCII

$manifest = [ordered]@{
    schema = 1
    product = "DCENT_axe"
    family = "bitaxe"
    packageType = "esp32-factory-and-ota-bundle"
    boardTarget = $BoardTarget
    deviceModel = $DeviceModel
    version = $Version
    createdAtUtc = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    ota = [ordered]@{
        appPartition = $OtaAppPartition
        slotSize = $otaSlotSize
        updateFitsSlot = $updateFitsSlot
    }
    signatureAlgorithm = if ($bundleSignature) { "ed25519" } else { $null }
    keyId = if ($bundleSignature -and -not [string]::IsNullOrWhiteSpace($SigningKeyId)) { $SigningKeyId } else { $null }
    signature = $bundleSignature
    _signatureNote = "AOTA-5: the top-level 'signature' covers factory_size+factory_sha256 (the full factory image) and is NOT verified by on-device firmware - DCENT_axe only enforces 'otaSignature' over the schema-2 OTA update message (size+sha of the update payload) at the /api/system/OTA handler. The bundle 'signature' is for a serial-flash verifier (DCENT Toolbox) to check factory_sha256 against the compiled key before flashing factory.bin. Do not present a factory install as device-verified on the strength of this field alone."
    bundleSignatureVerifiedOnDevice = $false
    deviceEnforcedSignature = "otaSignature"
    factorySha256 = $factorySha
    factoryFlashMap = @(
        [ordered]@{
            name = "bootloader"
            offset = 0
            size = $bootloaderSize
            sha256 = $bootloaderSha
        },
        [ordered]@{
            name = "partition-table"
            offset = 32768
            size = $partitionTableSize
            sha256 = $partitionTableSha
        },
        [ordered]@{
            name = "ota-data-initial"
            offset = $otaDataOffset
            size = $otaDataSize
            sha256 = $otaDataSha
        },
        [ordered]@{
            name = "update"
            offset = $otaAppOffset
            size = $updateSize
            sha256 = $updateSha
        }
    )
    otaSignatureAlgorithm = if ($otaSignature) { "ed25519" } else { $null }
    otaKeyId = if ($otaSignature -and -not [string]::IsNullOrWhiteSpace($SigningKeyId)) { $SigningKeyId } else { $null }
    otaSignature = $otaSignature
    payloads = @(
        [ordered]@{
            name = "factory"
            path = [System.IO.Path]::GetFileName($factoryBin)
            size = $factorySize
            sha256 = $factorySha
        },
        [ordered]@{
            name = "update"
            path = [System.IO.Path]::GetFileName($updateBin)
            size = $updateSize
            sha256 = $updateSha
        },
        [ordered]@{
            name = "manifest"
            path = [System.IO.Path]::GetFileName($manifestPath)
            size = $null
            sha256 = $null
        }
    )
    toolbox = [ordered]@{
        installCommand = "dcent flash --serial <port> -f $($factoryBin | Split-Path -Leaf)"
        updateCommand = "dcent ota update <ip> -f $($updateBin | Split-Path -Leaf)"
        uploadEndpoint = "/api/system/OTA"
        boardTargetHeader = "X-DCENT-Board-Target"
        deviceModelHeader = "X-DCENT-Device-Model"
        requiresInactiveSlot = $true
    }
}

$manifest | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $manifestPath -Encoding ASCII
$manifestSha = (Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
Add-Content -LiteralPath $checksumsPath -Value "$manifestSha  $([System.IO.Path]::GetFileName($manifestPath))" -Encoding ASCII

Write-Host "Factory package: $factoryBin"
Write-Host "Update package:  $updateBin"
Write-Host "Manifest:        $manifestPath"
Write-Host "Checksums:       $checksumsPath"
