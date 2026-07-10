param(
    [Parameter(Mandatory = $true)]
    [int]$DiskNumber,

    [Parameter(Mandatory = $true)]
    [string]$ArtifactDir,

    [Parameter(Mandatory = $true)]
    [string]$PayloadStageDir,

    [string]$LogPath = ''
)

$ErrorActionPreference = 'Stop'

if (-not $LogPath) {
    $LogPath = Join-Path (Split-Path -Parent $PayloadStageDir) 'am3-bb-sd-p2-finalize.log'
}

function Log([string]$Message) {
    $line = "$(Get-Date -Format s) $Message"
    Add-Content -LiteralPath $LogPath -Value $line
    Write-Host $line
}

function Require-Admin() {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'This script must run from an elevated Administrator shell'
    }
}

function Get-HashString([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToUpperInvariant()
}

function Assert-Hash([string]$ActualPath, [string]$ExpectedPath) {
    $actual = Get-HashString $ActualPath
    $expected = Get-HashString $ExpectedPath
    if ($actual -ne $expected) {
        throw "Hash mismatch for $ActualPath expected $expected got $actual"
    }
    Log "VERIFY $ActualPath sha256=$actual"
}

function Select-DriveLetter([char]$Preferred) {
    $used = @{}
    Get-Volume | Where-Object DriveLetter | ForEach-Object {
        $used[[char]$_.DriveLetter] = $true
    }
    if (-not $used.ContainsKey($Preferred)) {
        return $Preferred
    }
    foreach ($letter in ([char[]]'QRSTUVWXYZ')) {
        if (-not $used.ContainsKey($letter)) {
            return $letter
        }
    }
    throw 'No free drive letter available'
}

function Ensure-DriveLetter([uint32]$PartitionNumber, [char]$Preferred) {
    $part = Get-Partition -DiskNumber $DiskNumber -PartitionNumber $PartitionNumber
    if ($part.DriveLetter) {
        return [char]$part.DriveLetter
    }
    $letter = Select-DriveLetter $Preferred
    Set-Partition -DiskNumber $DiskNumber -PartitionNumber $PartitionNumber -NewDriveLetter $letter
    Start-Sleep -Seconds 1
    return $letter
}

function Write-CorrectUEnv([string]$Path) {
    $content = @(
        'dcent_bootfile=uImage',
        'dcent_fdtfile=devicetree.dtb',
        'dcent_ramdiskfile=ramdisk.gz',
        'mmcpart=2',
        '#optargs=quiet',
        'kloadaddr=0x80007fc0',
        'rdaddr=0x81000000',
        'fdtaddr=0x80F80000',
        'bootargs_dcent=setenv bootargs console=${console} ${optargs} rdinit=/init',
        'loaduimage_dcent=load mmc ${bootpart} ${kloadaddr} ${dcent_bootfile}',
        'loadfdt_dcent=load mmc ${bootpart} ${fdtaddr} ${dcent_fdtfile}',
        'loadramdisk_dcent=load mmc ${bootpart} ${rdaddr} ${dcent_ramdiskfile}',
        'uenvcmd=echo DCENT_OS SD p2 initramfs boot; setenv bootpart ${mmcdev}:${mmcpart}; if run loaduimage_dcent loadfdt_dcent loadramdisk_dcent bootargs_dcent; then bootm ${kloadaddr} ${rdaddr} ${fdtaddr}; else echo DCENT_OS SD load failed, falling back to NAND; run nandboot; fi'
    )
    [System.IO.File]::WriteAllText($Path, (($content -join "`n") + "`n"), [System.Text.Encoding]::ASCII)
}

try {
    Require-Admin
    Set-Content -LiteralPath $LogPath -Value "$(Get-Date -Format s) START DCENT_OS AM3-BB p2 finalizer"

    $disk = Get-Disk -Number $DiskNumber
    Log "Disk=$($disk.Number) name=$($disk.FriendlyName) bus=$($disk.BusType) size=$($disk.Size) system=$($disk.IsSystem) boot=$($disk.IsBoot) readonly=$($disk.IsReadOnly) status=$($disk.OperationalStatus)"
    if ($disk.IsSystem -or $disk.IsBoot) {
        throw 'Refusing to touch system/boot disk'
    }
    if ($disk.BusType -ne 'USB') {
        throw "Refusing non-USB disk bus $($disk.BusType)"
    }
    if ($disk.IsReadOnly) {
        Set-Disk -Number $DiskNumber -IsReadOnly $false
    }

    $p1 = Get-Partition -DiskNumber $DiskNumber -PartitionNumber 1
    $p2 = Get-Partition -DiskNumber $DiskNumber -PartitionNumber 2
    Log "P1 type=$($p1.MbrType) active=$($p1.IsActive) offset=$($p1.Offset) size=$($p1.Size) letter=$($p1.DriveLetter)"
    Log "P2 type=$($p2.MbrType) active=$($p2.IsActive) offset=$($p2.Offset) size=$($p2.Size) letter=$($p2.DriveLetter)"

    if ($p1.Offset -ne 1048576 -or $p1.Size -ne 33554432 -or -not $p1.IsActive) {
        throw 'Partition 1 does not match the expected AM335x boot partition layout'
    }
    if ($p2.Offset -ne 34603008 -or $p2.Size -lt 160000000) {
        throw 'Partition 2 does not match the expected AM3-BB payload partition layout'
    }

    $bootLetter = Ensure-DriveLetter 1 'D'
    $bootRoot = "$bootLetter`:\"
    $payloadRoot = Join-Path $PayloadStageDir 'dcentos-am3-bb-s19jpro-sdcard'
    $uenvTmp = Join-Path $PayloadStageDir 'uEnv.am3-bb-s19jpro.generated.txt'
    Write-CorrectUEnv $uenvTmp

    $required = @(
        (Join-Path $ArtifactDir 'MLO'),
        (Join-Path $ArtifactDir 'u-boot.img'),
        (Join-Path $ArtifactDir 'uImage'),
        (Join-Path $ArtifactDir 'devicetree.dtb'),
        (Join-Path $payloadRoot 'ramdisk.gz'),
        (Join-Path $payloadRoot 'uramdisk.image.gz')
    )
    foreach ($path in $required) {
        if (-not (Test-Path -LiteralPath $path)) {
            throw "Required source file missing: $path"
        }
    }

    Assert-Hash (Join-Path $bootRoot 'MLO') (Join-Path $ArtifactDir 'MLO')
    Assert-Hash (Join-Path $bootRoot 'u-boot.img') (Join-Path $ArtifactDir 'u-boot.img')
    Assert-Hash (Join-Path $bootRoot 'uImage') (Join-Path $ArtifactDir 'uImage')
    Assert-Hash (Join-Path $bootRoot 'devicetree.dtb') (Join-Path $ArtifactDir 'devicetree.dtb')

    Log 'Updating partition 1 uEnv.txt in place'
    Copy-Item -LiteralPath $uenvTmp -Destination (Join-Path $bootRoot 'uEnv.txt') -Force
    Assert-Hash (Join-Path $bootRoot 'uEnv.txt') $uenvTmp

    Log 'Formatting partition 2 as FAT32 label DCENTBB'
    Format-Volume -Partition $p2 -FileSystem FAT32 -NewFileSystemLabel 'DCENTBB' -Force -Confirm:$false | Out-Null
    Start-Sleep -Seconds 2
    $payloadLetter = Ensure-DriveLetter 2 'F'
    $payloadDest = "$payloadLetter`:\"
    Log "Payload destination=$payloadDest"

    Copy-Item -LiteralPath $uenvTmp -Destination (Join-Path $payloadDest 'uEnv.txt') -Force
    Copy-Item -LiteralPath (Join-Path $ArtifactDir 'uImage') -Destination (Join-Path $payloadDest 'uImage') -Force
    Copy-Item -LiteralPath (Join-Path $ArtifactDir 'devicetree.dtb') -Destination (Join-Path $payloadDest 'devicetree.dtb') -Force
    Copy-Item -LiteralPath (Join-Path $payloadRoot 'ramdisk.gz') -Destination (Join-Path $payloadDest 'ramdisk.gz') -Force
    Copy-Item -LiteralPath (Join-Path $payloadRoot 'uramdisk.image.gz') -Destination (Join-Path $payloadDest 'uramdisk.image.gz') -Force
    if (Test-Path -LiteralPath (Join-Path $payloadRoot 'README.txt')) {
        Copy-Item -LiteralPath (Join-Path $payloadRoot 'README.txt') -Destination (Join-Path $payloadDest 'README.txt') -Force
    }

    Assert-Hash (Join-Path $payloadDest 'uEnv.txt') $uenvTmp
    Assert-Hash (Join-Path $payloadDest 'uImage') (Join-Path $ArtifactDir 'uImage')
    Assert-Hash (Join-Path $payloadDest 'devicetree.dtb') (Join-Path $ArtifactDir 'devicetree.dtb')
    Assert-Hash (Join-Path $payloadDest 'ramdisk.gz') (Join-Path $payloadRoot 'ramdisk.gz')
    Assert-Hash (Join-Path $payloadDest 'uramdisk.image.gz') (Join-Path $payloadRoot 'uramdisk.image.gz')

    Log 'SUCCESS AM3-BB SD p2 finalized and verified'
}
catch {
    Log "ERROR $($_.Exception.Message)"
    exit 1
}
