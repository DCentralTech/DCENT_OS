param(
    [Parameter(Mandatory = $true)]
    [string]$Ip,
    [switch]$SkipBuild,
    [string]$Config,
    [switch]$Passthrough,
    [switch]$Verify,
    [switch]$RollbackOnFail,
    [string]$User = 'root',
    [string]$Password = $env:DCENT_PASSWORD
)

$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($Password)) {
    throw 'Set DCENT_PASSWORD or pass -Password for Windows deploys.'
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptDir '..\..\..')).Path
$workspaceDir = Join-Path $repoRoot 'projects\dcentos\dcentrald'
$sshCmdJs = Join-Path $repoRoot 'tools\ssh_cmd.js'
$sftpPutJs = Join-Path $repoRoot 'tools\sftp_put.js'

function Step([string]$Message) {
    Write-Output ""
    Write-Output "=== $Message ==="
}

function Invoke-SshTry {
    param([string]$RemoteCmd)

    $output = & node $sshCmdJs $Ip $User $Password $RemoteCmd 2>&1
    $exitCode = $LASTEXITCODE
    return @{ Ok = ($exitCode -eq 0); Output = (($output | ForEach-Object { "$_" }) -join "`n") }
}

function Invoke-SshOutput {
    param([string]$RemoteCmd)

    $result = Invoke-SshTry $RemoteCmd
    if (-not $result.Ok) {
        throw "SSH command failed: $($result.Output)"
    }
    return $result.Output
}

function Invoke-Sftp {
    param(
        [string]$LocalPath,
        [string]$RemotePath
    )

    & node $sftpPutJs $Ip $User $Password $LocalPath $RemotePath
    if ($LASTEXITCODE -ne 0) {
        throw "Upload failed: $LocalPath -> $RemotePath"
    }
}

function Parse-KeyValueOutput {
    param([string]$Text)

    $map = @{}
    foreach ($line in ($Text -split "`n")) {
        if ($line -match '^(?<k>[A-Z0-9_]+)=(?<v>.*)$') {
            $map[$matches['k']] = $matches['v']
        }
    }
    return $map
}

function Get-MapValue {
    param(
        [hashtable]$Map,
        [string]$Key,
        $Default
    )

    if ($Map.ContainsKey($Key) -and $null -ne $Map[$Key] -and "$($Map[$Key])" -ne '') {
        return $Map[$Key]
    }

    return $Default
}

function Get-FirstToken {
    param([string]$Text)

    if ([string]::IsNullOrWhiteSpace($Text)) {
        return ''
    }

    return (($Text.Trim() -split '\s+')[0]).Trim()
}

function Get-RemoteApiPort {
    param([string]$ConfigPath)

    if ([string]::IsNullOrWhiteSpace($ConfigPath) -or $ConfigPath -in @('builtin', 'unknown')) {
        return 80
    }

    $cmd = @'
awk '
    BEGIN { in_api = 0 }
    /^\[api\]/ { in_api = 1; next }
    /^\[/ { in_api = 0 }
    in_api && $1 == "http_port" {
        gsub(/[^0-9]/, "", $3)
        print $3
        exit
    }
' '__CONFIG_PATH__' 2>/dev/null
'@
    $cmd = $cmd.Replace('__CONFIG_PATH__', $ConfigPath)
    $result = Invoke-SshTry $cmd
    if (-not $result.Ok) {
        return 80
    }

    $port = 80
    if ([int]::TryParse($result.Output.Trim(), [ref]$port)) {
        return $port
    }

    return 80
}

function Restore-PersistentBackup {
    param(
        [string]$BackupPath,
        [string]$DeployPath
    )

    [void](Invoke-SshTry ("[ -f '{0}' ] && rm -f '{1}' && cp '{0}' '{1}' && chmod +x '{1}'" -f $BackupPath, $DeployPath))
    [void](Invoke-SshTry "if [ -x /etc/init.d/S82dcentrald ]; then /etc/init.d/S82dcentrald start; fi")
    Start-Sleep -Seconds 2
    return (Invoke-SshTry 'pidof dcentrald 2>/dev/null || echo NONE').Output.Trim()
}

Step "Connecting to $Ip"
$probe = Invoke-SshTry 'echo OK'
if (-not $probe.Ok -or $probe.Output.Trim() -ne 'OK') {
    throw ("Cannot SSH to root@{0}: {1}" -f $Ip, $probe.Output)
}
Write-Output '  SSH OK'

Step 'Detecting miner platform'
$minerInfoCmd = @'
echo "BOSMINER_PID=$(pidof bosminer 2>/dev/null || echo NONE)"
echo "BOSTOOLS_PID=$(pidof bos-tools 2>/dev/null || echo NONE)"
echo "BOSER_PID=$(pidof boser 2>/dev/null || echo NONE)"
echo "DCENTRALD_PID=$(pidof dcentrald 2>/dev/null || echo NONE)"
echo "OS_VER=$(cat /etc/dcentos-version 2>/dev/null || echo NONE)"
echo "BOS_VER=$(cat /etc/bos_version 2>/dev/null | head -1 || echo NONE)"
echo "BOS_PLATFORM=$(cat /etc/bos_platform 2>/dev/null | head -1 || echo NONE)"
echo "ARCH=$(uname -m 2>/dev/null || echo unknown)"
if [ -f /sys/devices/soc0/soc_id ]; then
    echo "SOC=$(cat /sys/devices/soc0/soc_id 2>/dev/null)"
elif grep -q zynq /proc/cpuinfo 2>/dev/null; then
    echo "SOC=zynq"
else
    echo "SOC=unknown"
fi
MODEL="$(cat /config/CONF_MINER_TYPE 2>/dev/null || cat /proc/device-tree/model 2>/dev/null || echo)"
echo "MODEL=$MODEL"
HWID="$(cat /config/CONF_HARDWARE_ID 2>/dev/null || echo)"
echo "HWID=$HWID"
echo "UIO_COUNT=$(find /sys/class/uio -maxdepth 1 -name "uio*" 2>/dev/null | wc -l)"
'@
$minerInfo = Parse-KeyValueOutput (Invoke-SshOutput $minerInfoCmd)

$arch = Get-MapValue $minerInfo 'ARCH' 'unknown'
$soc = Get-MapValue $minerInfo 'SOC' 'unknown'
$model = Get-MapValue $minerInfo 'MODEL' ''
$hwid = Get-MapValue $minerInfo 'HWID' ''
$bosPlatform = Get-MapValue $minerInfo 'BOS_PLATFORM' ''
$uioCount = 0
[void][int]::TryParse((Get-MapValue $minerInfo 'UIO_COUNT' '0').Trim(), [ref]$uioCount)

$platformFamily = ''
$platformDesc = ''
$target = ''
$deployMode = ''
$deployPath = ''
$backupPath = ''
$stagingPath = ''
$configRemote = ''
$verifyTimeout = 15
$hasPersistentSupervisor = $false

$bosPlatformLc = $bosPlatform.ToLowerInvariant()
$socLc = $soc.ToLowerInvariant()
$modelLc = $model.ToLowerInvariant()
$hwidLc = $hwid.ToLowerInvariant()

if ($bosPlatformLc -eq 'zynq-bm3-am2') {
    $platformFamily = 'am2'
    $platformDesc = 'AM2 runtime-only (detected via /etc/bos_platform)'
    $target = 'armv7-unknown-linux-musleabihf'
    $deployMode = 'runtime-only'
    $deployPath = '/tmp/dcentrald_runtime'
    $stagingPath = '/tmp/dcentrald_runtime.new'
    $configRemote = '/tmp/dcentrald.runtime.toml'
    $verifyTimeout = 30
} elseif ($bosPlatformLc -eq 'zynq-am1-s9') {
    $platformFamily = 'am1'
    $platformDesc = 'AM1 persistent (detected via /etc/bos_platform)'
    $target = 'armv7-unknown-linux-musleabihf'
    $deployMode = 'persistent'
    $deployPath = '/data/dcentrald'
    $backupPath = '/tmp/dcentrald_backup'
    $stagingPath = '/tmp/dcentrald_new'
    $configRemote = '/data/dcentrald.toml'
    $hasPersistentSupervisor = $true
} elseif ($arch -eq 'aarch64' -or $socLc.Contains('amlogic') -or $modelLc.Contains('amlogic')) {
    $platformFamily = 'amlogic'
    $platformDesc = 'Amlogic runtime-only'
    $target = 'aarch64-unknown-linux-musl'
    $deployMode = 'runtime-only'
    $deployPath = '/tmp/dcentrald_runtime'
    $stagingPath = '/tmp/dcentrald_runtime.new'
    $configRemote = '/tmp/dcentrald.runtime.toml'
    $verifyTimeout = 30
} elseif ($uioCount -ge 19 -or $hwidLc.Contains('am2') -or $modelLc.Contains('s17') -or $modelLc.Contains('s19') -or $modelLc.Contains('t17') -or $modelLc.Contains('t19')) {
    $platformFamily = 'am2'
    $platformDesc = 'AM2 runtime-only (UIO/HWID heuristic)'
    $target = 'armv7-unknown-linux-musleabihf'
    $deployMode = 'runtime-only'
    $deployPath = '/tmp/dcentrald_runtime'
    $stagingPath = '/tmp/dcentrald_runtime.new'
    $configRemote = '/tmp/dcentrald.runtime.toml'
    $verifyTimeout = 30
} else {
    $platformFamily = 'am1'
    $platformDesc = 'AM1 persistent'
    $target = 'armv7-unknown-linux-musleabihf'
    $deployMode = 'persistent'
    $deployPath = '/data/dcentrald'
    $backupPath = '/tmp/dcentrald_backup'
    $stagingPath = '/tmp/dcentrald_new'
    $configRemote = '/data/dcentrald.toml'
    $hasPersistentSupervisor = $true
}

if ($Passthrough -and $platformFamily -ne 'am1') {
    throw "--passthrough is only supported on AM1/S9 targets. Detected $platformFamily."
}

Write-Output "  Platform:    $platformDesc"
Write-Output "  Arch:        $arch"
Write-Output "  SoC:         $soc"
Write-Output "  bos_platform: $bosPlatform"
Write-Output "  Model:       $model"
Write-Output "  HWID:        $hwid"
Write-Output "  UIO count:   $uioCount"

$binary = Join-Path $workspaceDir ("target\{0}\release\dcentrald" -f $target)

if (-not $SkipBuild) {
    Step "Building dcentrald ($target)"
    Push-Location $workspaceDir
    try {
        if ($target -eq 'armv7-unknown-linux-musleabihf') {
            $env:CC_armv7_unknown_linux_musleabihf = (Join-Path $workspaceDir 'zig-cc-arm.bat')
            $env:AR_armv7_unknown_linux_musleabihf = (Join-Path $workspaceDir 'zig-ar-arm.bat')
        } elseif ($target -eq 'aarch64-unknown-linux-musl') {
            $env:CC_aarch64_unknown_linux_musl = (Join-Path $workspaceDir 'zig-cc-aarch64.bat')
            $env:AR_aarch64_unknown_linux_musl = (Join-Path $workspaceDir 'zig-ar-aarch64.bat')
        }

        & cargo build --release --target $target
        if ($LASTEXITCODE -ne 0) {
            throw 'cargo build failed'
        }
    } finally {
        Pop-Location
    }
} else {
    Step 'Skipping build (--skip-build)'
}

if (-not (Test-Path -LiteralPath $binary)) {
    throw "Binary not found: $binary"
}

$binaryInfo = Get-Item -LiteralPath $binary
$binarySize = [int64]$binaryInfo.Length
$binaryHash = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant()
Write-Output ("  Binary: {0} bytes" -f $binarySize)
Write-Output ("  SHA256: {0}" -f $binaryHash)

if ($Config) {
    Step "Uploading config: $Config"
    if (-not (Test-Path -LiteralPath $Config)) {
        throw "Config file not found: $Config"
    }
    Invoke-Sftp -LocalPath $Config -RemotePath $configRemote
}

if ($deployMode -eq 'persistent' -and -not $Force -and -not $Config) {
    $currentHashResult = Invoke-SshTry ('sha256sum ''{0}'' 2>/dev/null' -f $deployPath)
    if ($currentHashResult.Ok) {
        $remoteCurrentHash = (Get-FirstToken $currentHashResult.Output).ToLowerInvariant()
        Write-Output ("  Remote SHA256: {0}" -f $remoteCurrentHash)
        if ($remoteCurrentHash -eq $binaryHash) {
            Write-Output '  Installed binary already matches requested hash; probing API before deciding whether to redeploy'

            $configUsed = if ((Invoke-SshTry "[ -f '$configRemote' ] && echo yes || echo no").Output.Trim() -eq 'yes') {
                $configRemote
            } elseif ((Invoke-SshTry '[ -f /data/dcentrald.toml ] && echo yes || echo no').Output.Trim() -eq 'yes') {
                '/data/dcentrald.toml'
            } elseif ((Invoke-SshTry '[ -f /etc/dcentrald.toml ] && echo yes || echo no').Output.Trim() -eq 'yes') {
                '/etc/dcentrald.toml'
            } else {
                'unknown'
            }

            $apiPort = Get-RemoteApiPort $configUsed
            $apiHealthy = $false
            $runningPid = (Invoke-SshTry 'pidof dcentrald 2>/dev/null || echo NONE').Output.Trim()

            Step 'Probing API health for no-op path'
            $noOpProbeSeconds = if ($Verify) { $verifyTimeout } else { 5 }
            $deadline = (Get-Date).AddSeconds($noOpProbeSeconds)
            while ((Get-Date) -lt $deadline) {
                $httpCode = (Get-FirstToken (Invoke-SshTry ('wget -q -O /dev/null -S http://127.0.0.1:{0}/api/status 2>&1 | grep ''HTTP/'' | tail -1 | awk ''{{print $2}}''' -f $apiPort)).Output)
                if ($httpCode -eq '200') {
                    $apiHealthy = $true
                    break
                }
                Start-Sleep -Seconds 2
            }

            if ($apiHealthy) {
                Step 'Deploy complete'
                Write-Output ("  Target:      root@{0}" -f $Ip)
                Write-Output "  Platform:    $platformDesc"
                Write-Output "  Mode:        $deployMode (no-op)"
                Write-Output "  Binary:      $deployPath"
                Write-Output "  SHA256:      $binaryHash"
                Write-Output "  Config:      $configUsed"
                Write-Output "  PID:         $runningPid"
                Write-Output "  API port:    $apiPort"
                Write-Output "  API healthy: $apiHealthy"
                exit 0
            }

            Write-Output '  Installed binary matches, but API is not healthy; continuing with full deploy'
        }
    }
}

if ($deployMode -eq 'persistent') {
    Step 'Preflight space check'
    $deployDir = ($deployPath -replace '/[^/]+$', '')
    $spaceCmd = @'
TMP_FREE_KB=$(df -k /tmp 2>/dev/null | awk 'NR==2 {print $4}')
DEPLOY_FREE_KB=$(df -k '__DEPLOY_DIR__' 2>/dev/null | awk 'NR==2 {print $4}')
EXISTING_SIZE=0
if [ -f '__DEPLOY_PATH__' ]; then
    EXISTING_SIZE=$(wc -c < '__DEPLOY_PATH__' 2>/dev/null || echo 0)
fi
echo TMP_FREE_BYTES=$(( ${TMP_FREE_KB:-0} * 1024 ))
echo DEPLOY_FREE_BYTES=$(( ${DEPLOY_FREE_KB:-0} * 1024 ))
echo DEPLOY_EXISTING_SIZE=${EXISTING_SIZE:-0}
'@
    $spaceCmd = $spaceCmd.Replace('__DEPLOY_DIR__', $deployDir).Replace('__DEPLOY_PATH__', $deployPath)
    $spaceInfo = Parse-KeyValueOutput (Invoke-SshOutput $spaceCmd)
    $tmpFree = [int64](Get-MapValue $spaceInfo 'TMP_FREE_BYTES' '0')
    $deployFree = [int64](Get-MapValue $spaceInfo 'DEPLOY_FREE_BYTES' '0')
    $existingSize = [int64](Get-MapValue $spaceInfo 'DEPLOY_EXISTING_SIZE' '0')
    $tmpRequired = $binarySize + $existingSize
    $deployAvailableAfterReplace = $deployFree + $existingSize

    Write-Output ("  /tmp free:            {0} bytes" -f $tmpFree)
    Write-Output ("  {0} free:     {1} bytes" -f $deployDir, $deployFree)
    Write-Output ("  Existing binary size: {0} bytes" -f $existingSize)

    if ($tmpFree -lt $tmpRequired) {
        throw "Not enough /tmp space for staging + backup (need $tmpRequired bytes)"
    }
    if ($deployAvailableAfterReplace -lt $binarySize) {
        throw "Not enough $deployDir space after replacing existing binary"
    }
}

Step 'Stopping daemons'
if ((Get-MapValue $minerInfo 'DCENTRALD_PID' 'NONE') -ne 'NONE') {
    [void](Invoke-SshTry (('echo {0} > /tmp/dcentrald.expected_exit.pid 2>/dev/null; kill -TERM {0} 2>/dev/null; for i in $(seq 1 30); do kill -0 {0} 2>/dev/null || break; sleep 1; done; if kill -0 {0} 2>/dev/null; then rm -f /tmp/dcentrald.expected_exit.pid; kill -9 {0} 2>/dev/null; else rm -f /tmp/dcentrald.expected_exit.pid; fi; true' -f (Get-MapValue $minerInfo 'DCENTRALD_PID' 'NONE'))))
}
if ((Get-MapValue $minerInfo 'BOSMINER_PID' 'NONE') -ne 'NONE') {
    [void](Invoke-SshTry ("kill -TERM {0} 2>/dev/null; sleep 10; kill -9 {0} 2>/dev/null; true" -f (Get-MapValue $minerInfo 'BOSMINER_PID' 'NONE')))
}

Step 'Deploying binary'
if ($deployMode -eq 'persistent') {
    [void](Invoke-SshTry ("[ -f '{0}' ] && rm -f '{1}' && cp '{0}' '{1}' || true" -f $deployPath, $backupPath))
}

Invoke-Sftp -LocalPath $binary -RemotePath $stagingPath
$stageHash = (Get-FirstToken (Invoke-SshOutput ('sha256sum ''{0}'' 2>/dev/null' -f $stagingPath))).ToLowerInvariant()
if ($stageHash -ne $binaryHash) {
    throw "Staging hash mismatch: $stageHash != $binaryHash"
}

if ($deployMode -eq 'persistent') {
    [void](Invoke-SshOutput ("chmod +x '{0}' && rm -f '{1}' && cp '{0}' '{1}' && chmod +x '{1}' && rm -f '{0}'" -f $stagingPath, $deployPath))
} else {
    [void](Invoke-SshOutput ("chmod +x '{0}' && mv '{0}' '{1}'" -f $stagingPath, $deployPath))
}

$installedHash = (Get-FirstToken (Invoke-SshOutput ('sha256sum ''{0}'' 2>/dev/null' -f $deployPath))).ToLowerInvariant()
if ($installedHash -ne $binaryHash) {
    if ($deployMode -eq 'persistent' -and $RollbackOnFail) {
        [void](Restore-PersistentBackup -BackupPath $backupPath -DeployPath $deployPath)
    }
    throw "Installed hash mismatch: $installedHash != $binaryHash"
}

Step 'Starting dcentrald'
$startCmd = @'
CONFIG_USED=builtin
CONFIG_ARG=''
if [ -f '__CONFIG_REMOTE__' ]; then
    CONFIG_USED='__CONFIG_REMOTE__'
    CONFIG_ARG='--config __CONFIG_REMOTE__'
elif [ -f /data/dcentrald.toml ]; then
    CONFIG_USED=/data/dcentrald.toml
    CONFIG_ARG='--config /data/dcentrald.toml'
elif [ -f /etc/dcentrald.toml ]; then
    CONFIG_USED=/etc/dcentrald.toml
    CONFIG_ARG='--config /etc/dcentrald.toml'
fi
EXTRA_ARGS=''
if [ '__PLATFORM_FAMILY__' = 'amlogic' ]; then
    EXTRA_ARGS='--serial-mining'
elif [ '__PLATFORM_FAMILY__' = 'am2' ]; then
    EXTRA_ARGS='--s19j-hybrid'
fi
if [ '__PASSTHROUGH__' = 'true' ]; then
    EXTRA_ARGS="$EXTRA_ARGS --passthrough"
fi
echo CONFIG_USED=$CONFIG_USED
if [ '__DEPLOY_MODE__' = 'persistent' ] && [ '__PASSTHROUGH__' != 'true' ] && [ -x /etc/init.d/S82dcentrald ]; then
    /etc/init.d/S82dcentrald start
    sleep 1
    echo NEW_PID=$(pidof dcentrald 2>/dev/null | awk '{print $1}')
else
    nohup __DEPLOY_PATH__ $CONFIG_ARG $EXTRA_ARGS >/tmp/dcentrald.log 2>&1 &
    echo NEW_PID=$!
fi
'@
    $startCmd = $startCmd.Replace('__CONFIG_REMOTE__', $configRemote)
    $startCmd = $startCmd.Replace('__PLATFORM_FAMILY__', $platformFamily)
    $startCmd = $startCmd.Replace('__PASSTHROUGH__', $Passthrough.IsPresent.ToString().ToLowerInvariant())
    $startCmd = $startCmd.Replace('__DEPLOY_MODE__', $deployMode)
    $startCmd = $startCmd.Replace('__DEPLOY_PATH__', $deployPath)
$startInfo = Parse-KeyValueOutput (Invoke-SshOutput $startCmd)
$configUsed = Get-MapValue $startInfo 'CONFIG_USED' 'unknown'
$apiPort = Get-RemoteApiPort $configUsed
Start-Sleep -Seconds 1
$runningPid = (Invoke-SshOutput 'pidof dcentrald 2>/dev/null || echo NONE').Trim()

if ($runningPid -eq 'NONE') {
    if ($RollbackOnFail -and $deployMode -eq 'persistent') {
        $rollbackPid = Restore-PersistentBackup -BackupPath $backupPath -DeployPath $deployPath
        throw "dcentrald exited immediately; rolled back to backup (PID=$rollbackPid)"
    }
    throw 'dcentrald exited immediately'
}

$apiHealthy = $false
if ($Verify) {
    Step 'Verifying API health'
    $deadline = (Get-Date).AddSeconds($verifyTimeout)
    while ((Get-Date) -lt $deadline) {
        $httpCode = (Get-FirstToken (Invoke-SshTry ('wget -q -O /dev/null -S http://127.0.0.1:{0}/api/status 2>&1 | grep ''HTTP/'' | tail -1 | awk ''{{print $2}}''' -f $apiPort)).Output)
        if ($httpCode -eq '200') {
            $apiHealthy = $true
            break
        }
        Start-Sleep -Seconds 2
    }

    if (-not $apiHealthy -and $RollbackOnFail -and $deployMode -eq 'persistent') {
        [void](Invoke-SshTry (('echo {0} > /tmp/dcentrald.expected_exit.pid 2>/dev/null; kill -TERM {0} 2>/dev/null; for i in $(seq 1 30); do kill -0 {0} 2>/dev/null || break; sleep 1; done; if kill -0 {0} 2>/dev/null; then rm -f /tmp/dcentrald.expected_exit.pid; kill -9 {0} 2>/dev/null; else rm -f /tmp/dcentrald.expected_exit.pid; fi; true' -f $runningPid)))
        $rollbackPid = Restore-PersistentBackup -BackupPath $backupPath -DeployPath $deployPath
        throw "API verify failed; rolled back to backup (PID=$rollbackPid)"
    }
}

Step 'Deploy complete'
Write-Output ("  Target:      root@{0}" -f $Ip)
Write-Output "  Platform:    $platformDesc"
Write-Output "  Mode:        $deployMode"
Write-Output "  Binary:      $deployPath"
Write-Output "  SHA256:      $binaryHash"
Write-Output "  Config:      $configUsed"
Write-Output "  PID:         $runningPid"
Write-Output "  API port:    $apiPort"
Write-Output "  API healthy: $apiHealthy"
