param(
    [Parameter(Mandatory = $true)]
    [string]$ImagePath,

    [string]$DriveLetter = 'D',

    [int]$DiskNumber = -1,

    [string]$ExpectedImageHash = '',

    [string]$LogPath = ''
)

$ErrorActionPreference = 'Stop'

if (-not $LogPath) {
    $LogPath = Join-Path (Split-Path -Parent $ImagePath) 'am3-bb-sd-volume-write.log'
}

function Log([string]$Message) {
    $line = "$(Get-Date -Format s) $Message"
    Add-Content -LiteralPath $LogPath -Value $line
    Write-Host $line
}

function Read-MbrPartition([string]$Path) {
    $stream = [System.IO.FileStream]::new(
        $Path,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::ReadWrite,
        512,
        [System.IO.FileOptions]::SequentialScan
    )
    try {
        $mbr = [byte[]]::new(512)
        $read = $stream.Read($mbr, 0, $mbr.Length)
        if ($read -ne 512 -or $mbr[510] -ne 0x55 -or $mbr[511] -ne 0xAA) {
            throw 'Image does not have a valid DOS MBR signature'
        }
        $entry = 446
        $bootable = $mbr[$entry] -eq 0x80
        $type = $mbr[$entry + 4]
        $startLba = [BitConverter]::ToUInt32($mbr, $entry + 8)
        $sectorCount = [BitConverter]::ToUInt32($mbr, $entry + 12)
        if ($startLba -eq 0 -or $sectorCount -eq 0) {
            throw 'Image partition table has no first partition'
        }
        [pscustomobject]@{
            Bootable = $bootable
            Type = $type
            StartLba = [uint64]$startLba
            SectorCount = [uint64]$sectorCount
            OffsetBytes = [uint64]$startLba * 512
            SizeBytes = [uint64]$sectorCount * 512
        }
    }
    finally {
        $stream.Close()
    }
}

function New-Sha256() {
    [System.Security.Cryptography.SHA256]::Create()
}

function Finish-Hash([System.Security.Cryptography.HashAlgorithm]$Hash) {
    [void]$Hash.TransformFinalBlock([byte[]]::new(0), 0, 0)
    ([BitConverter]::ToString($Hash.Hash)).Replace('-', '')
}

try {
    Set-Content -LiteralPath $LogPath -Value "$(Get-Date -Format s) START DCENT_OS AM3-BB SD volume write"

    $image = Get-Item -LiteralPath $ImagePath
    $imageHash = (Get-FileHash -LiteralPath $ImagePath -Algorithm SHA256).Hash
    Log "Image=$ImagePath bytes=$($image.Length) sha256=$imageHash"
    if ($ExpectedImageHash -and $imageHash -ne $ExpectedImageHash.ToUpperInvariant()) {
        throw "Image hash mismatch: expected $ExpectedImageHash got $imageHash"
    }

    $partitionImage = Read-MbrPartition $ImagePath
    Log "Image partition bootable=$($partitionImage.Bootable) type=0x$('{0:X2}' -f $partitionImage.Type) start_lba=$($partitionImage.StartLba) bytes=$($partitionImage.SizeBytes)"
    if (-not $partitionImage.Bootable) {
        throw 'Image first partition is not marked bootable'
    }
    if ($partitionImage.Type -ne 0x0B -and $partitionImage.Type -ne 0x0C) {
        throw "Image first partition type 0x$('{0:X2}' -f $partitionImage.Type) is not FAT32"
    }

    $partition = Get-Partition -DriveLetter $DriveLetter
    if ($DiskNumber -ge 0 -and $partition.DiskNumber -ne $DiskNumber) {
        throw "$DriveLetter`: maps to Disk $($partition.DiskNumber), expected Disk $DiskNumber"
    }
    $disk = Get-Disk -Number $partition.DiskNumber
    Log "$DriveLetter`: Disk=$($disk.Number) Partition=$($partition.PartitionNumber) bus=$($disk.BusType) size=$($disk.Size) offset=$($partition.Offset) part_bytes=$($partition.Size) active=$($partition.IsActive)"
    if ($disk.IsSystem -or $disk.IsBoot) {
        throw 'Refusing to write a system/boot disk'
    }
    if ($disk.BusType -ne 'USB') {
        throw "Refusing non-USB disk bus $($disk.BusType)"
    }
    if ($partition.Size -lt $partitionImage.SizeBytes) {
        throw "Destination partition is smaller than image partition: $($partition.Size) < $($partitionImage.SizeBytes)"
    }

    try {
        & fsutil.exe volume dismount "$DriveLetter`:" | ForEach-Object { Log "fsutil: $_" }
    }
    catch {
        Log "fsutil dismount warning: $($_.Exception.Message)"
    }

    $volumePath = "\\.\$DriveLetter`:"
    $inputStream = [System.IO.FileStream]::new(
        $ImagePath,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::ReadWrite,
        4194304,
        [System.IO.FileOptions]::SequentialScan
    )
    $volumeStream = [System.IO.FileStream]::new(
        $volumePath,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::ReadWrite,
        [System.IO.FileShare]::ReadWrite,
        4194304,
        [System.IO.FileOptions]::WriteThrough
    )

    $imageRegionHash = New-Sha256
    try {
        $inputStream.Position = [int64]$partitionImage.OffsetBytes
        $volumeStream.Position = 0
        $buffer = [byte[]]::new(4194304)
        $remaining = [int64]$partitionImage.SizeBytes
        $written = 0L
        while ($remaining -gt 0) {
            $want = [int][Math]::Min($buffer.Length, $remaining)
            $read = $inputStream.Read($buffer, 0, $want)
            if ($read -le 0) {
                throw 'Unexpected EOF while reading image partition'
            }
            [void]$imageRegionHash.TransformBlock($buffer, 0, $read, $null, 0)
            $volumeStream.Write($buffer, 0, $read)
            $written += $read
            $remaining -= $read
            if (($written % 33554432) -eq 0) {
                Log "wrote $written bytes"
            }
        }
        $volumeStream.Flush($true)
        $expectedRegionHash = Finish-Hash $imageRegionHash
        Log "WRITE_DONE bytes=$written partition_sha256=$expectedRegionHash"
    }
    finally {
        $inputStream.Close()
        $volumeStream.Close()
        $imageRegionHash.Dispose()
    }

    $verifyHash = New-Sha256
    $verifyStream = [System.IO.FileStream]::new(
        $volumePath,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::ReadWrite,
        4194304,
        [System.IO.FileOptions]::SequentialScan
    )
    try {
        $verifyStream.Position = 0
        $buffer = [byte[]]::new(4194304)
        $remaining = [int64]$partitionImage.SizeBytes
        while ($remaining -gt 0) {
            $want = [int][Math]::Min($buffer.Length, $remaining)
            $read = $verifyStream.Read($buffer, 0, $want)
            if ($read -le 0) {
                throw 'Unexpected EOF while verifying destination partition'
            }
            [void]$verifyHash.TransformBlock($buffer, 0, $read, $null, 0)
            $remaining -= $read
        }
        $actualRegionHash = Finish-Hash $verifyHash
        Log "VERIFY partition_sha256=$actualRegionHash"
        if ($actualRegionHash -ne $expectedRegionHash) {
            throw "Destination readback hash mismatch: expected $expectedRegionHash got $actualRegionHash"
        }
    }
    finally {
        $verifyStream.Close()
        $verifyHash.Dispose()
    }

    try {
        & fsutil.exe volume dismount "$DriveLetter`:" | ForEach-Object { Log "fsutil-post: $_" }
    }
    catch {
        Log "post-write dismount warning: $($_.Exception.Message)"
    }

    Log 'SUCCESS AM3-BB SD FAT partition image written and verified'
}
catch {
    Log "ERROR $($_.Exception.Message)"
    exit 1
}
