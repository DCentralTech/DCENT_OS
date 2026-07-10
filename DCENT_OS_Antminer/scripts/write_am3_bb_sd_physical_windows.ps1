param(
    [Parameter(Mandatory = $true)]
    [string]$ImagePath,

    [Parameter(Mandatory = $true)]
    [int]$DiskNumber,

    [string]$ExpectedImageHash = '',

    [string]$LogPath = ''
)

$ErrorActionPreference = 'Stop'

if (-not $LogPath) {
    $LogPath = Join-Path (Split-Path -Parent $ImagePath) 'am3-bb-sd-physical-write.log'
}

function Log([string]$Message) {
    $line = "$(Get-Date -Format s) $Message"
    Add-Content -LiteralPath $LogPath -Value $line
    Write-Host $line
}

function Get-RawPrefixHash([string]$Path, [long]$Bytes) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $stream = [System.IO.FileStream]::new(
        $Path,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::ReadWrite,
        4194304,
        [System.IO.FileOptions]::SequentialScan
    )
    try {
        $buffer = [byte[]]::new(4194304)
        $remaining = $Bytes
        while ($remaining -gt 0) {
            $want = [int][Math]::Min($buffer.Length, $remaining)
            $read = $stream.Read($buffer, 0, $want)
            if ($read -le 0) {
                throw 'Unexpected EOF while hashing raw disk'
            }
            [void]$sha.TransformBlock($buffer, 0, $read, $null, 0)
            $remaining -= $read
        }
        [void]$sha.TransformFinalBlock([byte[]]::new(0), 0, 0)
        return ([BitConverter]::ToString($sha.Hash)).Replace('-', '')
    }
    finally {
        $stream.Close()
        $sha.Dispose()
    }
}

try {
    Set-Content -LiteralPath $LogPath -Value "$(Get-Date -Format s) START DCENT_OS AM3-BB physical SD write"

    $image = Get-Item -LiteralPath $ImagePath
    $imageHash = (Get-FileHash -LiteralPath $ImagePath -Algorithm SHA256).Hash
    Log "Image=$ImagePath bytes=$($image.Length) sha256=$imageHash"
    if ($ExpectedImageHash -and $imageHash -ne $ExpectedImageHash.ToUpperInvariant()) {
        throw "Image hash mismatch: expected $ExpectedImageHash got $imageHash"
    }

    $disk = Get-Disk -Number $DiskNumber
    Log "Disk=$($disk.Number) name=$($disk.FriendlyName) bus=$($disk.BusType) size=$($disk.Size) system=$($disk.IsSystem) boot=$($disk.IsBoot) readonly=$($disk.IsReadOnly) status=$($disk.OperationalStatus)"
    if ($disk.IsSystem -or $disk.IsBoot) {
        throw 'Refusing to write system/boot disk'
    }
    if ($disk.BusType -ne 'USB') {
        throw "Refusing non-USB disk bus $($disk.BusType)"
    }
    if ($disk.Size -lt $image.Length) {
        throw "Disk is smaller than image: $($disk.Size) < $($image.Length)"
    }
    if ($disk.IsReadOnly) {
        Set-Disk -Number $DiskNumber -IsReadOnly $false
    }

    $parts = Get-Partition -DiskNumber $DiskNumber -ErrorAction SilentlyContinue
    foreach ($part in $parts) {
        if ($part.DriveLetter) {
            try {
                Log "Dismounting $($part.DriveLetter):"
                & fsutil.exe volume dismount "$($part.DriveLetter):" | ForEach-Object { Log "fsutil: $_" }
            }
            catch {
                Log "fsutil dismount warning for $($part.DriveLetter): $($_.Exception.Message)"
            }
            try {
                Log "Removing access path $($part.DriveLetter): from partition $($part.PartitionNumber)"
                Remove-PartitionAccessPath -DiskNumber $DiskNumber -PartitionNumber $part.PartitionNumber -AccessPath "$($part.DriveLetter):\" -ErrorAction Stop
            }
            catch {
                Log "Remove-PartitionAccessPath warning for $($part.DriveLetter): $($_.Exception.Message)"
            }
        }
    }

    try {
        Log "Clearing existing partition table on Disk $DiskNumber"
        Clear-Disk -Number $DiskNumber -RemoveData -RemoveOEM -Confirm:$false -ErrorAction Stop
        Start-Sleep -Seconds 2
    }
    catch {
        Log "Clear-Disk warning: $($_.Exception.Message)"
    }

    try {
        Log "Taking Disk $DiskNumber offline for raw write"
        Set-Disk -Number $DiskNumber -IsOffline $true -ErrorAction Stop
        Start-Sleep -Seconds 1
    }
    catch {
        Log "Set-Disk offline warning: $($_.Exception.Message)"
    }

    $rawPath = "\\.\PhysicalDrive$DiskNumber"
    $inputStream = [System.IO.FileStream]::new(
        $ImagePath,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::Read,
        4194304,
        [System.IO.FileOptions]::SequentialScan
    )
    $outputStream = [System.IO.FileStream]::new(
        $rawPath,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Write,
        [System.IO.FileShare]::ReadWrite,
        4194304,
        [System.IO.FileOptions]::WriteThrough
    )

    try {
        $outputStream.Position = 0
        $buffer = [byte[]]::new(4194304)
        $total = 0L
        while (($read = $inputStream.Read($buffer, 0, $buffer.Length)) -gt 0) {
            $outputStream.Write($buffer, 0, $read)
            $total += $read
            if (($total % 33554432) -eq 0) {
                Log "wrote $total bytes"
            }
        }
        $outputStream.Flush($true)
        Log "WRITE_DONE bytes=$total"
    }
    finally {
        $outputStream.Close()
        $inputStream.Close()
    }

    Start-Sleep -Seconds 2
    $verifyHash = Get-RawPrefixHash $rawPath $image.Length
    Log "VERIFY sha256=$verifyHash"
    if ($verifyHash -ne $imageHash) {
        throw "Raw readback hash mismatch: expected $imageHash got $verifyHash"
    }

    try {
        Update-HostStorageCache | Out-Null
    }
    catch {
        Log "Update-HostStorageCache warning: $($_.Exception.Message)"
    }
    try {
        Log "Bringing Disk $DiskNumber online after raw write"
        Set-Disk -Number $DiskNumber -IsOffline $false -ErrorAction Stop
        Update-HostStorageCache | Out-Null
    }
    catch {
        Log "Set-Disk online warning: $($_.Exception.Message)"
    }

    Log 'SUCCESS physical SD image written and verified'
}
catch {
    try {
        Set-Disk -Number $DiskNumber -IsOffline $false -ErrorAction SilentlyContinue
    }
    catch {
    }
    Log "ERROR $($_.Exception.Message)"
    exit 1
}
