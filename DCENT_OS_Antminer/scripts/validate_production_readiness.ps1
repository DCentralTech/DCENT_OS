param(
    [switch]$RunRustChecks,
    [string]$TargetTriple = 'armv7-unknown-linux-musleabihf',
    [string]$ManifestPubkeyHex = $env:DCENT_MANIFEST_PUBLIC_KEY_HEX,
    [string]$DcentraldBinaryPath = ''
)

$ErrorActionPreference = 'Stop'

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptDir '..')).Path

$results = New-Object System.Collections.Generic.List[object]
$rustCheckCommands = New-Object System.Collections.Generic.List[string]

function Add-Result {
    param(
        [ValidateSet('PASS', 'WARN', 'FAIL')]
        [string]$Status,
        [string]$Check,
        [string]$Detail,
        [string]$Path = ''
    )

    $results.Add([PSCustomObject]@{
        Status = $Status
        Check = $Check
        Detail = $Detail
        Path = $Path
    })

    $suffix = if ([string]::IsNullOrWhiteSpace($Path)) { '' } else { " [$Path]" }
    Write-Output ("{0}: {1} - {2}{3}" -f $Status, $Check, $Detail, $suffix)
}

function Join-RepoPath {
    param([string]$RelativePath)
    return (Join-Path $repoRoot $RelativePath)
}

function Get-RepoText {
    param([string]$RelativePath)

    $path = Join-RepoPath $RelativePath
    if (-not (Test-Path -LiteralPath $path)) {
        Add-Result FAIL 'required file' 'file is missing' $RelativePath
        return $null
    }

    return [System.IO.File]::ReadAllText($path)
}

function Test-Pattern {
    param(
        [string]$Text,
        [string]$Pattern,
        [string]$Check,
        [string]$PassDetail,
        [string]$FailDetail,
        [string]$Path
    )

    if ($null -eq $Text) {
        return
    }

    if ([regex]::IsMatch($Text, $Pattern)) {
        Add-Result PASS $Check $PassDetail $Path
    } else {
        Add-Result FAIL $Check $FailDetail $Path
    }
}

function Test-NoPromptStyleCommandLines {
    param(
        [string]$Text,
        [string]$Path,
        [string]$Check,
        [string]$PassDetail,
        [string]$FailDetail
    )

    if ($null -eq $Text) {
        return
    }

    $commandHeadPattern = '(?:ssh|scp|curl|wget|devmem|i2c(?:set|get|detect|dump)|fw_(?:setenv|printenv)|sysupgrade|reboot|poweroff|halt|service|systemctl|/etc/init\.d/|ubiupdatevol|nandwrite|flashcp|dd|mount|umount|modprobe|insmod|rmmod|killall|pkill|pidof)'
    $promptCommandPattern = '^(?:[$>#]\s*)' + $commandHeadPattern + '(?=$|\s|/)'
    $bareCommandPattern = '^' + $commandHeadPattern + '(?=$|\s|/)'
    $negativeProsePattern = '^(?:no|not|never|do\s+not|does\s+not|must\s+not|should\s+not|without|forbidden)\b'
    $proseAfterCommandPattern = '^' + $commandHeadPattern + '\s+(?:is|are|was|were|remains|must|should|will|can|cannot|does|do|not|requires|proof|path|behavior|state|command|commands)\b'

    $hits = New-Object System.Collections.Generic.List[string]
    $lineNumber = 0
    foreach ($line in ($Text -split '\r?\n')) {
        $lineNumber++
        $trimmed = $line.Trim()
        if ([string]::IsNullOrWhiteSpace($trimmed) -or $trimmed.StartsWith('```')) {
            continue
        }

        $candidate = ($trimmed -replace '^(?:[-*+]\s+|\d+\.\s+|>\s+)+', '').Trim()
        if ($candidate -match $negativeProsePattern) {
            continue
        }

        $isPromptCommand = $candidate -match $promptCommandPattern
        $isBareCommand = ($candidate -match $bareCommandPattern) -and ($candidate -notmatch $proseAfterCommandPattern)
        if ($isPromptCommand -or $isBareCommand) {
            $hits.Add(('{0}: {1}' -f $lineNumber, $trimmed))
        }
    }

    if ($hits.Count -gt 0) {
        Add-Result FAIL $Check ("$FailDetail Lines: {0}" -f (($hits | Select-Object -First 5) -join '; ')) $Path
    } else {
        Add-Result PASS $Check $PassDetail $Path
    }
}

function Get-RustFunctionText {
    param(
        [string]$Text,
        [string]$FunctionName
    )

    if ($null -eq $Text) {
        return $null
    }

    $match = [regex]::Match(
        $Text,
        "(?m)^\s*(?:(?:pub|pub\([^)]*\))\s+)?(?:async\s+)?fn\s+$([regex]::Escape($FunctionName))\s*\("
    )
    if (-not $match.Success) {
        return $null
    }
    $start = $match.Index

    $next = $Text.Length
    foreach ($marker in @("`nfn ", "`nasync fn ", "`npub fn ", "`npub async fn ", "`npub(crate) fn ", "`npub(crate) async fn ", "`nstruct ", "`npub struct ", "`nimpl ")) {
        $idx = $Text.IndexOf($marker, $start + 1, [System.StringComparison]::Ordinal)
        if ($idx -ge 0 -and $idx -lt $next) {
            $next = $idx
        }
    }

    return $Text.Substring($start, $next - $start)
}

function Get-TomlApiHttpPort {
    param([string]$RelativePath)

    $path = Join-RepoPath $RelativePath
    if (-not (Test-Path -LiteralPath $path)) {
        Add-Result FAIL 'api http_port' 'config file is missing' $RelativePath
        return $null
    }

    $inApi = $false
    foreach ($line in (Get-Content -LiteralPath $path)) {
        $trimmed = $line.Trim()
        if ($trimmed -match '^\[(?<section>[^\]]+)\]') {
            $inApi = ($matches['section'] -eq 'api')
            continue
        }

        if ($inApi -and $trimmed -match '^http_port\s*=\s*(?<port>\d+)') {
            return [int]$matches['port']
        }
    }

    return $null
}

function Get-TomlSectionText {
    param(
        [string]$RelativePath,
        [string]$SectionName
    )

    $path = Join-RepoPath $RelativePath
    if (-not (Test-Path -LiteralPath $path)) {
        Add-Result FAIL 'toml section' 'config file is missing' $RelativePath
        return $null
    }

    $inSection = $false
    $lines = New-Object System.Collections.Generic.List[string]
    foreach ($line in (Get-Content -LiteralPath $path)) {
        if ($line -match '^\s*\[(?<section>[^\]]+)\]\s*$') {
            if ($inSection) {
                break
            }
            $inSection = ($matches['section'] -eq $SectionName)
            continue
        }

        if ($inSection) {
            $lines.Add($line)
        }
    }

    if ($lines.Count -eq 0) {
        Add-Result FAIL ("toml [{0}]" -f $SectionName) 'section is missing or empty' $RelativePath
        return $null
    }

    return ($lines -join "`n")
}

function Test-TomlApiPort {
    param([string]$RelativePath)

    $port = Get-TomlApiHttpPort $RelativePath
    if ($null -eq $port) {
        Add-Result FAIL 'api http_port' 'no [api].http_port entry found' $RelativePath
    } elseif ($port -eq 8080) {
        Add-Result PASS 'api http_port' 'configured for dcentrald :8080' $RelativePath
    } else {
        Add-Result FAIL 'api http_port' "expected 8080, found $port" $RelativePath
    }
}

function Get-TextFileCandidates {
    param([string[]]$RelativeRoots)

    $files = New-Object System.Collections.Generic.List[System.IO.FileInfo]
    foreach ($root in $RelativeRoots) {
        $path = Join-RepoPath $root
        if (-not (Test-Path -LiteralPath $path)) {
            continue
        }

        Get-ChildItem -LiteralPath $path -Recurse -File -Force | ForEach-Object {
            $ext = $_.Extension.ToLowerInvariant()
            if ($ext -in @('.bin', '.img', '.gz', '.xz', '.zip', '.png', '.jpg', '.jpeg', '.gif', '.ico', '.woff', '.woff2')) {
                return
            }
            $files.Add($_)
        }
    }

    return $files
}

function ConvertTo-RelativePath {
    param([string]$Path)

    $full = (Resolve-Path -LiteralPath $Path).Path
    if ($full.StartsWith($repoRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $full.Substring($repoRoot.Length).TrimStart('\', '/')
    }

    return $full
}

function Test-DashboardOwnsPort80 {
    Write-Output ''
    Write-Output '=== Dashboard Port Ownership ==='

    $initPath = 'br2_external_dcentos\board\zynq\rootfs-overlay\etc\init.d\S80dashboard'
    $serverPath = 'br2_external_dcentos\board\zynq\rootfs-overlay\root\web\server.py'
    $initText = Get-RepoText $initPath
    $serverText = Get-RepoText $serverPath

    Test-Pattern $initText '(?m)^PORT=80\s*$' 'dashboard :80' 'S80dashboard declares PORT=80' 'S80dashboard does not declare PORT=80' $initPath
    Test-Pattern $initText 'server\.py' 'dashboard server' 'S80dashboard starts server.py' 'S80dashboard does not reference server.py' $initPath
    Test-Pattern $initText '--port\s+["'']?\$PORT["'']?' 'dashboard server port arg' 'S80dashboard passes --port $PORT' 'S80dashboard does not pass --port $PORT' $initPath
    Test-Pattern $initText 'MAX_CRASH_RESTARTS=\d+' 'dashboard bounded supervisor' 'S80dashboard has a bounded restart supervisor' 'S80dashboard lacks a bounded restart supervisor' $initPath
    Test-Pattern $initText 'CHILD_PIDFILE=' 'dashboard child pidfile' 'S80dashboard tracks the child server PID' 'S80dashboard does not track the child server PID' $initPath
    Test-Pattern $initText 'wait\s+\\?\$CHILD_PID' 'dashboard child wait' 'S80dashboard supervisor waits on the child process' 'S80dashboard does not visibly wait on the child process' $initPath
    Test-Pattern $serverText '(?m)^DCENTRALD_PORT\s*=\s*8080\s*$' 'dashboard proxy target' 'server.py proxies dcentrald API to localhost:8080' 'server.py does not set DCENTRALD_PORT = 8080' $serverPath
    Test-Pattern $serverText 'add_argument\(["'']--port["''][^\r\n]*default=80' 'dashboard default port' 'server.py default HTTP port is 80' 'server.py default HTTP port is not 80' $serverPath
    Test-Pattern $serverText '/api/dashboard/health' 'dashboard health endpoint' 'server.py serves local dashboard health' 'server.py lacks /api/dashboard/health' $serverPath
}

function Test-DcentraldPort8080 {
    Write-Output ''
    Write-Output '=== dcentrald API Port ==='

    Test-TomlApiPort 'br2_external_dcentos\board\zynq\rootfs-overlay\etc\dcentrald.toml'
    Test-TomlApiPort 'dcentrald\dcentrald.toml'

    $configRsPath = 'dcentrald\dcentrald\src\config.rs'
    $configRs = Get-RepoText $configRsPath
    Test-Pattern $configRs '(?s)fn\s+default_http_port\(\)\s*->\s*u16\s*\{\s*8080\s*\}' 'rust default_http_port' 'daemon config default is 8080' 'daemon config default_http_port is not 8080' $configRsPath

    $apiLibPath = 'dcentrald\dcentrald-api\src\lib.rs'
    $apiLib = Get-RepoText $apiLibPath
    Test-Pattern $apiLib 'http_port:\s*8080' 'api crate default port' 'dcentrald-api default is 8080' 'dcentrald-api default port is not visibly 8080' $apiLibPath

    $initScripts = @(
        'br2_external_dcentos\board\zynq\rootfs-overlay\etc\init.d\S82dcentrald',
        'br2_external_dcentos\board\zynq\am2-s19jpro\rootfs-overlay\etc\init.d\S82dcentrald',
        'br2_external_dcentos\board\amlogic\rootfs-overlay\etc\init.d\S82dcentrald'
    )

    foreach ($initPath in $initScripts) {
        $initText = Get-RepoText $initPath
        Test-Pattern $initText 'migrate_legacy_api_port' 'legacy :80 migration' 'init script defines legacy port migration' 'init script lacks legacy port migration' $initPath
        Test-Pattern $initText 'sub\(/80/,\s*["'']8080["'']\)|80 to 8080' 'legacy :80 rewrite' 'init script rewrites http_port 80 to 8080' 'init script does not visibly rewrite 80 to 8080' $initPath

        if ($null -ne $initText) {
            $migrationRefs = ([regex]::Matches($initText, 'migrate_legacy_api_port')).Count
            if ($migrationRefs -ge 2) {
                Add-Result PASS 'legacy migration called' 'migration function is defined and called during start' $initPath
            } else {
                Add-Result FAIL 'legacy migration called' 'migration function is not called after definition' $initPath
            }
        }

        Test-Pattern $initText ':8080|port 8080|API_PORT=8080|:\$API_PORT' 'dcentrald start advertises :8080' 'init script documents/prints REST API on :8080' 'init script does not visibly advertise :8080' $initPath
    }

    $amlogicInit = 'br2_external_dcentos\board\amlogic\rootfs-overlay\etc\init.d\S82dcentrald'
    $amlogicText = Get-RepoText $amlogicInit
    Test-Pattern $amlogicText 'REST/WebSocket\s+:\$API_PORT|REST/WebSocket\s+:8080|API:\s+REST/WebSocket\s+:\$API_PORT' 'amlogic dcentrald port note' 'Amlogic init script visibly keeps dcentrald on :8080' 'Amlogic init script does not visibly mention :8080' $amlogicInit
}

function Test-S99UpgradeHealthGate {
    Write-Output ''
    Write-Output '=== S99upgrade Boot Commit Gate ==='

    $upgradePath = 'br2_external_dcentos\board\zynq\rootfs-overlay\etc\init.d\S99upgrade'
    $text = Get-RepoText $upgradePath

    Test-Pattern $text 'wait_for_http' 'http health helper' 'S99upgrade has bounded HTTP wait helper' 'S99upgrade lacks bounded HTTP wait helper' $upgradePath
    Test-Pattern $text 'ip\s+addr\s+show\s+eth0' 'network boot health' 'S99upgrade verifies eth0 has an IP before commit' 'S99upgrade does not verify eth0 IP before commit' $upgradePath
    Test-Pattern $text 'netstat\s+-tln[\s\S]*:22' 'ssh boot health' 'S99upgrade verifies SSH is listening before commit' 'S99upgrade does not verify SSH listener before commit' $upgradePath
    Test-Pattern $text 'authorized_keys' 'ssh access health' 'S99upgrade detects key-only SSH lockout before commit' 'S99upgrade does not check authorized_keys lockout risk' $upgradePath
    Test-Pattern $text 'http://127\.0\.0\.1/api/dashboard/health' 'dashboard boot health' 'S99upgrade probes dashboard health on :80' 'S99upgrade does not probe dashboard health' $upgradePath
    Test-Pattern $text 'pidof\s+dcentrald' 'daemon process health' 'S99upgrade verifies dcentrald process before commit' 'S99upgrade does not verify dcentrald process before commit' $upgradePath
    Test-Pattern $text 'http://127\.0\.0\.1:8080/api/status' 'daemon api boot health' 'S99upgrade probes dcentrald API on :8080' 'S99upgrade does not probe dcentrald API on :8080' $upgradePath
    Test-Pattern $text 'HEALTH_OK=false' 'health failure latch' 'S99upgrade preserves failure latch before commit' 'S99upgrade lacks HEALTH_OK=false failure path' $upgradePath
    Test-Pattern $text 'fw_setenv\s+upgrade_stage' 'upgrade stage clear' 'S99upgrade clears upgrade_stage only in commit path' 'S99upgrade does not visibly clear upgrade_stage' $upgradePath

    if ($null -ne $text) {
        $commitSectionIndex = $text.IndexOf('Upgrade detected', [System.StringComparison]::Ordinal)
        $dashboardIndex = $text.IndexOf('http://127.0.0.1/api/dashboard/health', [System.StringComparison]::Ordinal)
        $apiIndex = $text.IndexOf('http://127.0.0.1:8080/api/status', [System.StringComparison]::Ordinal)
        $healthCallIndex = if ($commitSectionIndex -ge 0) { $text.IndexOf('check_health', $commitSectionIndex, [System.StringComparison]::Ordinal) } else { -1 }
        $healthFailIndex = if ($healthCallIndex -ge 0) { $text.IndexOf('HEALTH CHECK FAILED', $healthCallIndex, [System.StringComparison]::Ordinal) } else { -1 }
        $clearIndex = if ($healthCallIndex -ge 0) { $text.IndexOf('fw_setenv upgrade_stage', $healthCallIndex, [System.StringComparison]::Ordinal) } else { -1 }

        if ($dashboardIndex -ge 0 -and $apiIndex -ge 0 -and $healthCallIndex -ge 0 -and $healthFailIndex -gt $healthCallIndex -and $clearIndex -gt $healthFailIndex) {
            Add-Result PASS 'boot commit order' 'dashboard/API checks and failure branch precede upgrade_stage clearing' $upgradePath
        } else {
            Add-Result FAIL 'boot commit order' 'could not prove dashboard/API checks precede upgrade_stage clearing' $upgradePath
        }
    }
}

function Test-ServiceSupervision {
    Write-Output ''
    Write-Output '=== Service Supervision ==='

    $services = @(
        [PSCustomObject]@{
            Path = 'br2_external_dcentos\board\zynq\rootfs-overlay\etc\init.d\S80dashboard'
            Name = 'dashboard'
            NeedsFanSafety = $false
        },
        [PSCustomObject]@{
            Path = 'br2_external_dcentos\board\zynq\rootfs-overlay\etc\init.d\S82dcentrald'
            Name = 'zynq dcentrald'
            NeedsFanSafety = $true
        },
        [PSCustomObject]@{
            Path = 'br2_external_dcentos\board\zynq\am2-s19jpro\rootfs-overlay\etc\init.d\S82dcentrald'
            Name = 'am2 dcentrald'
            NeedsFanSafety = $true
        },
        [PSCustomObject]@{
            Path = 'br2_external_dcentos\board\amlogic\rootfs-overlay\etc\init.d\S82dcentrald'
            Name = 'amlogic dcentrald'
            NeedsFanSafety = $true
        }
    )

    foreach ($service in $services) {
        $text = Get-RepoText $service.Path
        Test-Pattern $text 'start-stop-daemon\s+-S\s+-b\s+-m\s+-p\s+"\$PIDFILE"' ("{0} wrapper start" -f $service.Name) 'service starts under a tracked wrapper process' 'service does not visibly start under a tracked wrapper process' $service.Path
        Test-Pattern $text 'CHILD_PIDFILE=' ("{0} child pidfile" -f $service.Name) 'service tracks the child process PID' 'service does not track the child process PID' $service.Path
        Test-Pattern $text 'EXPECTFILE=' ("{0} expected exit marker" -f $service.Name) 'service marks expected exits before stop/replacement' 'service does not mark expected exits' $service.Path
        Test-Pattern $text 'MAX_CRASH_RESTARTS=\d+' ("{0} restart bound" -f $service.Name) 'service has a bounded crash restart limit' 'service lacks a bounded crash restart limit' $service.Path
        Test-Pattern $text 'wait\s+\\?\$CHILD_PID' ("{0} child wait" -f $service.Name) 'service supervisor waits on the child process' 'service does not visibly wait on the child process' $service.Path
        Test-Pattern $text 'restart limit reached' ("{0} restart cutoff" -f $service.Name) 'service logs and stops after restart limit' 'service lacks a visible restart cutoff' $service.Path

        if ($service.NeedsFanSafety) {
            Test-Pattern $text 'fan_safety_override' ("{0} fan safety" -f $service.Name) 'dcentrald crash/timeout path leaves fans in safety mode' 'dcentrald service lacks fan safety override' $service.Path
            Test-Pattern $text '(?m)^\s+safety\)' ("{0} safety action" -f $service.Name) 'dcentrald service exposes a safety action for supervisor calls' 'dcentrald service lacks a safety action for supervisor calls' $service.Path
        }
    }
}

function Test-FanPwmScale {
    Write-Output ''
    Write-Output '=== Fan PWM Scale ==='

    $halFanPath = 'dcentrald\dcentrald-hal\src\fan.rs'
    $configPath = 'dcentrald\dcentrald\src\config.rs'
    $thermalControllerPath = 'dcentrald\dcentrald-thermal\src\controller.rs'
    $thermalFanPath = $thermalControllerPath
    $autotunerPath = 'dcentrald\dcentrald-autotuner\src\tuner.rs'
    $powerBudgetPath = 'dcentrald\dcentrald-autotuner\src\power_budget.rs'
    $profitabilityPath = 'dcentrald\dcentrald-autotuner\src\profitability.rs'
    $daemonPath = 'dcentrald\dcentrald\src\daemon.rs'
    $serialMiningPath = 'dcentrald\dcentrald\src\serial_mining.rs'

    $halFanText = Get-RepoText $halFanPath
    $configText = Get-RepoText $configPath
    $thermalControllerText = Get-RepoText $thermalControllerPath
    $thermalFanText = Get-RepoText $thermalFanPath
    $autotunerText = Get-RepoText $autotunerPath
    $powerBudgetText = Get-RepoText $powerBudgetPath
    $profitabilityText = Get-RepoText $profitabilityPath
    $daemonText = Get-RepoText $daemonPath
    $serialMiningText = Get-RepoText $serialMiningPath

    Test-Pattern $halFanText 'pub const PWM_MAX:\s*u8\s*=\s*100;' 'HAL fan PWM ceiling' 'HAL exposes Braiins fan-control ceiling as 100' 'HAL fan PWM ceiling is not visibly 100' $halFanPath
    Test-Pattern $configText 'thermal\.fan_max_pwm[\s\S]*dcentrald_hal::fan::PWM_MAX' 'config fan max validation' 'config validation uses HAL PWM_MAX instead of legacy 127' 'config validation does not visibly use HAL PWM_MAX' $configPath
    Test-Pattern $thermalControllerText 'const FAN_PWM_MAX:\s*u8\s*=\s*100;[\s\S]*clamp\(0\.0,\s*FAN_PWM_MAX as f32\)' 'thermal PID PWM ceiling' 'thermal PID output is clamped to 0-100' 'thermal PID still appears to use a non-100 ceiling' $thermalControllerPath
    Test-Pattern $thermalFanText '(?s)(?=.*const FAN_PWM_MAX:\s*u8\s*=\s*100;)(?=.*profile\.fan_max_pwm\s*>\s*FAN_PWM_MAX[\s\S]*profile\.fan_max_pwm\s*=\s*FAN_PWM_MAX)(?=.*fn\s+safety_capped_pwm[\s\S]*safe_fan_pwm\([\s\S]*\.min\(profile_max_pwm\))' 'thermal fan manager ceiling' 'thermal fan manager clamps profile and safety overrides to the 0-100 ceiling' 'thermal fan manager does not visibly clamp profile and safety overrides to the 0-100 ceiling' $thermalFanPath
    Test-Pattern $autotunerText 'PWM 100[\s\S]*fan_pwm\.min\(100\)[\s\S]*\(64\.0,\s*0\.85,\s*100\.0,\s*1\.00\)' 'autotuner fan factor scale' 'autotuner fan thermal factor uses 0-100 PWM scale' 'autotuner fan thermal factor does not visibly use 0-100 scale' $autotunerPath
    Test-Pattern $powerBudgetText 'fan_pwm as f64\s*/\s*100\.0' 'fan power estimate scale' 'fan power estimate uses 100 as PWM full-scale fallback' 'fan power estimate still appears to use legacy full-scale' $powerBudgetPath
    Test-Pattern $profitabilityText 'PWM 100[\s\S]*noise_to_fan_pwm[\s\S]*clamp\(10,\s*100\)' 'noise PWM scale' 'noise-to-fan mapping returns PWM 10-100' 'noise-to-fan mapping does not visibly return 10-100' $profitabilityPath

    if ($null -ne $daemonText -and $daemonText -match 'PWM \{\}/127|\*\s*100\s*/\s*127') {
        Add-Result FAIL 'daemon legacy fan percentage' 'daemon still contains legacy 127 PWM percentage math' $daemonPath
    } else {
        Add-Result PASS 'daemon legacy fan percentage' 'daemon fan logs/percentages use the 0-100 PWM scale' $daemonPath
    }

    if ($null -ne $serialMiningText -and $serialMiningText -match 'latest_fan_pwm\s+as\s+u16\s*\*\s*100\)\s*/\s*127') {
        Add-Result FAIL 'serial mining fan percentage' 'serial mining API state still scales fan PWM by 127' $serialMiningPath
    } else {
        Add-Result PASS 'serial mining fan percentage' 'serial mining API state does not use legacy 127 fan scaling' $serialMiningPath
    }
}

function Test-TomlDonationConfig {
    param([string]$RelativePath)

    $section = Get-TomlSectionText $RelativePath 'donation'
    if ($null -eq $section) {
        return
    }

    if ($section -match '(?m)^\s*enabled\s*=\s*true\s*$') {
        Add-Result PASS 'donation shipped default enabled' 'donation is enabled by default in shipped config' $RelativePath
    } else {
        Add-Result FAIL 'donation shipped default enabled' 'expected [donation].enabled = true for voluntary 2% default' $RelativePath
    }

    if ($section -match '(?m)^\s*percent\s*=\s*2(?:\.0+)?\s*$') {
        Add-Result PASS 'donation shipped default percent' 'donation percent defaults to 2%' $RelativePath
    } else {
        Add-Result FAIL 'donation shipped default percent' 'expected [donation].percent = 2.0' $RelativePath
    }

    if ($section -match '(?m)^\s*pool_url\s*=\s*"(stratum\+tcp|stratum2\+tcp)://[^"]+"\s*$') {
        Add-Result PASS 'donation shipped pool visible' 'donation pool URL is explicit in config' $RelativePath
    } else {
        Add-Result FAIL 'donation shipped pool visible' 'expected a non-empty donation pool URL in shipped config' $RelativePath
    }

    if ($section -match '(?m)^\s*worker\s*=\s*"[^"]+"\s*$') {
        Add-Result PASS 'donation shipped worker visible' 'donation worker is explicit in config' $RelativePath
    } else {
        Add-Result FAIL 'donation shipped worker visible' 'expected a non-empty donation worker in shipped config' $RelativePath
    }

    if ($section -match '(?m)^\s*cycle_duration_s\s*=\s*3600\s*$') {
        Add-Result PASS 'donation shipped cycle' 'donation cycle defaults to 3600 seconds' $RelativePath
    } else {
        Add-Result FAIL 'donation shipped cycle' 'expected [donation].cycle_duration_s = 3600' $RelativePath
    }

    if ($section -match '(?i)(voluntary|optional)' -and $section -match '(?i)(disable|enabled\s*=\s*false)') {
        Add-Result PASS 'donation shipped transparency copy' 'config comments explain the donation is voluntary and disableable' $RelativePath
    } else {
        Add-Result FAIL 'donation shipped transparency copy' 'config comments must state the donation is voluntary/optional and can be disabled' $RelativePath
    }
}

function Test-DonationFeeReadiness {
    Write-Output ''
    Write-Output '=== Donation Fee Readiness ==='

    $daemonConfigPath = 'dcentrald\dcentrald\src\config.rs'
    $stratumTypesPath = 'dcentrald\dcentrald-stratum\src\types.rs'
    $stratumClientPath = 'dcentrald\dcentrald-stratum\src\v1\client.rs'
    $daemonPath = 'dcentrald\dcentrald\src\daemon.rs'
    $apiLibPath = 'dcentrald\dcentrald-api\src\lib.rs'
    $apiWsPath = 'dcentrald\dcentrald-api\src\websocket.rs'
    $apiRestPath = 'dcentrald\dcentrald-api\src\rest.rs'
    $apiTypesPath = 'dashboard\src\api\types.ts'
    $donationCardPath = 'dashboard\src\components\common\DonationFeeCard.tsx'
    $donatingIndicatorPath = 'dashboard\src\components\common\DonatingIndicator.tsx'
    $aboutPath = 'dashboard\src\components\common\AboutPage.tsx'
    $settingsPath = 'dashboard\src\components\standard\SettingsPage.tsx'
    $wizardPath = 'dashboard\src\components\wizard\SetupWizard.tsx'
    $wizardStepPath = 'dashboard\src\components\wizard\DonationStep.tsx'

    $daemonConfigText = Get-RepoText $daemonConfigPath
    $stratumTypesText = Get-RepoText $stratumTypesPath
    $stratumClientText = Get-RepoText $stratumClientPath
    $daemonText = Get-RepoText $daemonPath
    $apiLibText = Get-RepoText $apiLibPath
    $apiWsText = Get-RepoText $apiWsPath
    $apiRestText = Get-RepoText $apiRestPath
    $apiTypesText = Get-RepoText $apiTypesPath
    $donationCardText = Get-RepoText $donationCardPath
    $donatingIndicatorText = Get-RepoText $donatingIndicatorPath
    $aboutText = Get-RepoText $aboutPath
    $settingsText = Get-RepoText $settingsPath
    $wizardText = Get-RepoText $wizardPath
    $wizardStepText = Get-RepoText $wizardStepPath

    Test-Pattern $daemonConfigText 'impl\s+Default\s+for\s+DonationConfig[\s\S]*enabled:\s*true[\s\S]*percent:\s*2\.0' 'daemon donation default' 'daemon DonationConfig defaults to enabled 2%' 'daemon DonationConfig does not visibly default to enabled 2%' $daemonConfigPath
    Test-Pattern $daemonConfigText 'fn\s+default_donation_enabled\(\)\s*->\s*bool\s*\{\s*true\s*\}' 'daemon donation serde default' 'missing TOML donation.enabled defaults to true' 'missing TOML donation.enabled does not visibly default to true' $daemonConfigPath
    Test-Pattern $daemonConfigText 'fn\s+default_donation_pool\(\)\s*->\s*String\s*\{\s*("[^"]+"\.(to_string|into)\(\)|String::from\("[^"]+"\))\s*\}' 'daemon donation pool default' 'daemon has a non-empty donation pool default' 'daemon donation pool default is empty or not visible' $daemonConfigPath
    Test-Pattern $daemonConfigText 'fn\s+default_donation_worker\(\)\s*->\s*String\s*\{\s*("[^"]+"\.(to_string|into)\(\)|String::from\("[^"]+"\))\s*\}' 'daemon donation worker default' 'daemon has a non-empty donation worker default' 'daemon donation worker default is empty or not visible' $daemonConfigPath
    Test-Pattern $daemonConfigText 'donation\.percent[\s\S]*0\.0[\s\S]*5\.0' 'donation percent bounds' 'daemon validates donation.percent in the 0-5% range' 'daemon does not visibly validate donation.percent in the 0-5% range' $daemonConfigPath
    Test-Pattern $daemonConfigText 'donation\.enabled[\s\S]*donation\.pool_url[\s\S]*donation\.worker' 'donation pool required only when enabled' 'daemon requires donation pool/worker only when donation is enabled' 'daemon does not visibly gate donation pool/worker validation on enabled' $daemonConfigPath

    Test-Pattern $stratumTypesText 'impl\s+Default\s+for\s+DonationConfig[\s\S]*enabled:\s*true[\s\S]*percent:\s*2\.0[\s\S]*cycle_duration_s:\s*3600' 'stratum donation default' 'stratum DonationConfig defaults to enabled 2% with a 3600s cycle' 'stratum DonationConfig does not visibly default to enabled 2%' $stratumTypesPath
    Test-Pattern $stratumTypesText 'Donation configuration[\s\S]*voluntary[\s\S]*transparent[\s\S]*disableable' 'stratum donation transparency docs' 'stratum types document donation as voluntary, transparent, and disableable' 'stratum donation docs do not visibly state voluntary/transparent/disableable' $stratumTypesPath
    Test-Pattern $stratumClientText 'config\.donation\.enabled\s*&&\s*config\.donation\.percent\s*>\s*0\.0' 'donation runtime disable path' 'runtime disables donation when enabled=false or percent=0' 'runtime does not visibly disable donation for enabled=false or percent=0' $stratumClientPath
    Test-Pattern $stratumClientText 'let\s+don_frac\s*=\s*\(\(?config\.donation\.percent\s+as\s+f64\)?\s*/\s*100\.0\)\.clamp\(0\.0,\s*1\.0\)' 'donation timing math' 'runtime computes donation time as percent / 100 and clamps the fraction defensively' 'runtime donation timing math is not visibly percent based' $stratumClientPath
    Test-Pattern $stratumClientText 'phase_remaining\.is_zero\(\)[\s\S]*tokio::time::sleep[\s\S]*if\s+!phase_remaining\.is_zero\(\)' 'donation timer disabled cleanly' 'donation timer is inert when donation is disabled' 'donation timer is not visibly disabled when donation is off' $stratumClientPath
    Test-Pattern $stratumClientText '(?s)(?=.*fn\s+flush_dispatcher_for_pool_switch[\s\S]*JobTemplate::flush_only)(?=.*Ok\(SessionEndReason::DonationSwitch\)[\s\S]{0,1800}flush_dispatcher_for_pool_switch)' 'donation switch clean job flush' 'donation pool switches force a clean-job refresh before more work is dispatched' 'could not prove donation switches flush stale user/donation work' $stratumClientPath
    Test-Pattern $stratumClientText 'clear_orphaned_pending_submits\(if\s+is_donation[\s\S]*donation_session_end' 'donation pending submit cleanup' 'donation session changes clear orphaned pending submits' 'donation session changes do not visibly clear orphaned pending submits' $stratumClientPath
    Test-Pattern $stratumClientText 'let\s+submit_worker\s*=\s*if\s+is_donation[\s\S]*self\.config\.donation\.worker\.clone\(\)' 'donation worker submit identity' 'share submits use the donation worker during donation windows' 'share submits do not visibly switch to donation worker identity' $stratumClientPath
    Test-Pattern $stratumClientText 'Donation pool rejected credentials[\s\S]*disabling donation for this session[\s\S]*Donation pool handshake timed out[\s\S]*resuming user pool mining[\s\S]*Donation pool session error[\s\S]*resuming user pool mining' 'donation failure fallback' 'donation pool failures fall back to user mining instead of forcing downtime' 'donation pool failure handling is not visibly fail-open to user mining' $stratumClientPath

    Test-TomlDonationConfig 'dcentrald\dcentrald.toml'
    Test-TomlDonationConfig 'br2_external_dcentos\board\zynq\rootfs-overlay\etc\dcentrald.toml'

    Test-Pattern $apiRestText 'CONFIG_ALLOWED_KEYS:\s*&\[\&str\]\s*=\s*&\[[^\]]*"donation"' 'API donation config write' 'POST /api/config allows the donation section to be changed' 'POST /api/config whitelist does not visibly allow donation updates' $apiRestPath
    Test-Pattern $apiRestText '\.route\("/api/config",\s*get\(get_config\)\.post\(post_config\)\)' 'API config route' 'dashboard can read/write config through /api/config' 'API config route is not visibly wired' $apiRestPath
    Test-Pattern $apiLibText 'pub\s+donating:\s*bool' 'REST pool donating field' 'REST pool status exposes pool.donating' 'REST pool status does not visibly expose pool.donating' $apiLibPath
    Test-Pattern $apiWsText 'pub\s+donating:\s*Option<bool>[\s\S]*donating:\s*Some\(state\.pool\.donating\)' 'WebSocket donating field' 'WebSocket status exposes pool.donating' 'WebSocket status does not visibly expose pool.donating' $apiWsPath
    Test-Pattern $daemonText 'DonationStateChanged[\s\S]*s\.pool\.donating\s*=\s*active' 'daemon donating state propagation' 'daemon propagates donation active state to API status' 'daemon does not visibly propagate donation state to API status' $daemonPath
    Test-Pattern $daemonText 'Donation mining active[\s\S]*supporting open-source development[\s\S]*(Disable|change|changed)[\s\S]*Settings' 'daemon donation status copy' 'daemon status message explains active donation and Settings control' 'daemon status copy does not visibly explain active donation and Settings control' $daemonPath
    Test-Pattern $apiTypesText 'export\s+interface\s+DonationConfig[\s\S]*enabled:\s*boolean[\s\S]*percent:\s*number[\s\S]*cycle_duration_s:\s*number' 'dashboard donation config type' 'dashboard API types include donation config fields' 'dashboard API types do not visibly model donation config' $apiTypesPath
    Test-Pattern $apiTypesText 'donating\?:\s*boolean' 'dashboard donating status type' 'dashboard API types include pool.donating' 'dashboard API types do not visibly include pool.donating' $apiTypesPath

    Test-Pattern $donationCardText 'const\s+DEFAULT_PERCENT\s*=\s*2(?:\.0)?;' 'dashboard donation percent default' 'DonationFeeCard defaults the suggested percent to 2%' 'DonationFeeCard does not visibly default percent to 2%' $donationCardPath
    Test-Pattern $donationCardText 'DEFAULT_STATE:\s*DonationState\s*=\s*\{[\s\S]*enabled:\s*true[\s\S]*percent:\s*DEFAULT_PERCENT' 'dashboard donation enabled fallback' 'DonationFeeCard fallback state is enabled at 2%' 'DonationFeeCard fallback state is not visibly enabled at 2%' $donationCardPath
    Test-Pattern $donationCardText '(api\.updateConfig\(\{\s*donation:\s*nextDonation|api\.updateDonationConfig\(\s*nextDonation)' 'dashboard donation config write' 'DonationFeeCard persists donation settings through a donation-only write path' 'DonationFeeCard does not visibly persist donation settings' $donationCardPath
    Test-Pattern $donationCardText 'role="switch"[\s\S]*aria-checked=\{state\.enabled\}[\s\S]*Disable donation[\s\S]*Enable donation' 'dashboard donation disable control' 'DonationFeeCard exposes an accessible enable/disable switch' 'DonationFeeCard does not visibly expose a disable control' $donationCardPath
    Test-Pattern $donationCardText '(?i)(voluntary|optional)[\s\S]*(disable|choose|change)[\s\S]*(open-source|open source)' 'dashboard donation transparency copy' 'DonationFeeCard explains voluntary donation and open-source funding' 'DonationFeeCard transparency copy is missing or too weak' $donationCardPath
    Test-Pattern $donationCardText 'poolDonating[\s\S]*Currently donating|poolDonating[\s\S]*LIVE' 'dashboard donation live state' 'DonationFeeCard shows when the live donation window is active' 'DonationFeeCard does not visibly show live donation state' $donationCardPath
    Test-Pattern $donatingIndicatorText 'status\?\.pool\?\.donating[\s\S]*Currently mining on the donation pool[\s\S]*DONATING' 'dashboard donating chip' 'DonatingIndicator is driven by pool.donating and labels the live donation window' 'DonatingIndicator does not visibly expose pool.donating state' $donatingIndicatorPath
    Test-Pattern $settingsText 'DonationFeeCard\s+variant="full"' 'settings donation surface' 'Settings page includes the full donation controls' 'Settings page does not visibly include donation controls' $settingsPath
    Test-Pattern $aboutText 'DonationFeeCard' 'about donation surface' 'About page includes donation transparency card' 'About page does not visibly include donation transparency' $aboutPath
    Test-Pattern $wizardText 'donation:\s*\{\s*enabled:\s*true,\s*percent:\s*2\s*\}' 'wizard donation default' 'setup wizard defaults donation to enabled 2%' 'setup wizard does not visibly default donation to enabled 2%' $wizardPath
    Test-Pattern $wizardText '(api\.updateConfig\(\{[\s\S]*donation:\s*\{|api\.updateDonationConfig\(\{)[\s\S]*enabled:\s*Boolean\(config\.donation\?\.enabled\)[\s\S]*percent:\s*Math\.max\(0,\s*Math\.min\(5' 'wizard donation config write' 'setup wizard persists donation enabled/percent with 0-5 clamp' 'setup wizard does not visibly persist donation settings safely' $wizardPath
    Test-Pattern $wizardText '\{\s*id:\s*''donation''[\s\S]*skippable:\s*true' 'wizard donation step skippable' 'setup wizard donation step is visible and skippable' 'setup wizard donation step is not visibly skippable' $wizardPath
    Test-Pattern $wizardStepText 'optional[\s\S]*mandatory[\s\S]*cannot disable[\s\S]*choose' 'wizard donation transparency copy' 'wizard explains the donation is optional and user-controlled' 'wizard donation copy does not visibly explain optional control' $wizardStepPath
    Test-Pattern $wizardStepText 'role="switch"[\s\S]*aria-checked=\{enabled\}[\s\S]*Disable donation[\s\S]*Enable donation' 'wizard donation disable control' 'wizard exposes an accessible enable/disable switch' 'wizard does not visibly expose a disable control' $wizardStepPath

    $topbarFiles = @(
        [PSCustomObject]@{ Path = 'dashboard\src\components\basic\BasicDashboard.tsx'; Name = 'basic topbar' },
        [PSCustomObject]@{ Path = 'dashboard\src\components\standard\KitTopBar.tsx'; Name = 'standard topbar' },
        [PSCustomObject]@{ Path = 'dashboard\src\components\advanced\AdvancedDashboard.tsx'; Name = 'advanced topbar' }
    )

    foreach ($topbar in $topbarFiles) {
        $text = Get-RepoText $topbar.Path
        Test-Pattern $text 'DonatingIndicator[\s\S]*<DonatingIndicator\s*/>' ("{0} donation indicator" -f $topbar.Name) 'topbar renders the live DONATING indicator' 'topbar does not visibly render the live DONATING indicator' $topbar.Path
    }
}

function Test-NoVendorSshKeys {
    Write-Output ''
    Write-Output '=== Buildroot Overlay SSH Material ==='

    $overlayRoots = @(
        'br2_external_dcentos\board\zynq\rootfs-overlay',
        'br2_external_dcentos\board\zynq\am2-s19jpro\rootfs-overlay',
        'br2_external_dcentos\board\amlogic\rootfs-overlay'
    )

    $badNamePattern = '(?i)(^|[\\/])authorized_keys$|(^|[\\/])id_(rsa|dsa|ecdsa|ed25519)(\.pub)?$|dropbear_(rsa|dss|ecdsa|ed25519)_host_key$|ssh_host_.*_key$'
    $keyMaterialPattern = '(?im)-----BEGIN (OPENSSH|RSA|DSA|EC|PRIVATE) PRIVATE KEY-----|^\s*(ssh-rsa|ssh-ed25519|ssh-dss|ecdsa-sha2-[^ ]+)\s+'
    $findings = New-Object System.Collections.Generic.List[string]

    foreach ($file in (Get-TextFileCandidates $overlayRoots)) {
        $rel = ConvertTo-RelativePath $file.FullName
        if ($rel -match $badNamePattern) {
            $findings.Add("${rel}: suspicious SSH key filename")
            continue
        }

        try {
            $text = [System.IO.File]::ReadAllText($file.FullName)
        } catch {
            continue
        }

        if ([regex]::IsMatch($text, $keyMaterialPattern)) {
            $findings.Add("${rel}: SSH public/private key material")
        }
    }

    if ($findings.Count -eq 0) {
        Add-Result PASS 'vendor SSH key material' 'no SSH keys or authorized_keys files found in Buildroot overlays'
    } else {
        foreach ($finding in $findings) {
            Add-Result FAIL 'vendor SSH key material' $finding
        }
    }
}

function Test-NoVendorPhoneHomeEndpoints {
    Write-Output ''
    Write-Output '=== Vendor Phone-Home Endpoint Scan ==='

    $scanRoots = @(
        'br2_external_dcentos\board\zynq\rootfs-overlay',
        'br2_external_dcentos\board\zynq\am2-s19jpro\rootfs-overlay',
        'br2_external_dcentos\board\amlogic\rootfs-overlay',
        'scripts'
    )

    $endpointPattern = '(?i)(https?://|wss?://|mqtt://|tcp://|stratum2?\+tcp://|[a-z0-9][a-z0-9.-]*\.(com|net|org|io|xyz|work|tech|farm|cn))'
    $vendorPattern = '(?i)(vnish|anthill|asic\.to|hiveos\.farm|minerlink|awesome[-_]?miner|minerstat|foreman\.mn|lincoin|api\.bitmain|cloud\.bitmain|monitor\.bitmain|telemetry\.[a-z0-9.-]+|phone[-_ ]?home)'
    $allowPattern = '(?i)(127\.0\.0\.1|localhost|this-miner|example\.com|d-central\.tech|github\.com|releases\.linaro\.org|modelcontextprotocol\.io|fonts\.googleapis\.com|fonts\.gstatic\.com)'
    $findings = New-Object System.Collections.Generic.List[string]

    foreach ($file in (Get-TextFileCandidates $scanRoots)) {
        $rel = ConvertTo-RelativePath $file.FullName
        if ($rel -match '(?i)rootfs-overlay[\\/]root[\\/]web[\\/]static[\\/]index\.html$') {
            continue
        }

        try {
            $lines = Get-Content -LiteralPath $file.FullName
        } catch {
            continue
        }

        for ($i = 0; $i -lt $lines.Count; $i++) {
            $line = $lines[$i]
            if ($line -match $endpointPattern -and $line -match $vendorPattern -and $line -notmatch $allowPattern) {
                $findings.Add(("{0}:{1}: {2}" -f $rel, ($i + 1), $line.Trim()))
            }
        }
    }

    if ($findings.Count -eq 0) {
        Add-Result PASS 'vendor phone-home endpoints' 'no suspicious vendor callback/update/telemetry endpoints found in overlays or scripts'
    } else {
        foreach ($finding in $findings) {
            Add-Result FAIL 'vendor phone-home endpoints' $finding
        }
    }
}

function Test-FirmwareIntakePackagingGates {
    Write-Output ''
    Write-Output '=== Firmware Intake / Packaging Gates ==='

    $sysupgradePath = 'br2_external_dcentos\board\zynq\rootfs-overlay\usr\sbin\sysupgrade'
    $packagePath = 'scripts\package_sysupgrade.sh'
    $flashUniversalPath = 'scripts\flash_universal.sh'
    $flashBraiinsosPath = 'scripts\flash_braiinsos.sh'
    $apiRestPath = 'dcentrald\dcentrald-api\src\rest.rs'
    $cargoConfigPath = 'dcentrald\.cargo\config.toml'

    $sysupgradeText = Get-RepoText $sysupgradePath
    $packageText = Get-RepoText $packagePath
    $flashUniversalText = Get-RepoText $flashUniversalPath
    $flashBraiinsosText = Get-RepoText $flashBraiinsosPath
    $apiRestText = Get-RepoText $apiRestPath
    $cargoConfigText = Get-RepoText $cargoConfigPath

    Test-Pattern $sysupgradeText 'tar\s+tf\s+"\$ROOTFS"[\s\S]*sysupgrade-\*[\s\S]*MANIFEST\.json[\s\S]*kernel[\s\S]*root' 'packaged sysupgrade tar' 'sysupgrade accepts tar packages with manifest, kernel, and root payloads' 'sysupgrade does not visibly require packaged tar payload structure' $sysupgradePath
    Test-Pattern $sysupgradeText 'Raw squashfs sysupgrade is blocked by default[\s\S]*DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1' 'raw squashfs default rejection' 'raw squashfs is rejected unless the lab override is set' 'raw squashfs is not visibly rejected by default' $sysupgradePath
    Test-Pattern $sysupgradeText 'RELEASE_PUBKEY="/etc/dcentos/release_ed25519\.pub"[\s\S]*MANIFEST\.sig[\s\S]*release_ed25519\.pub[\s\S]*openssl\s+pkeyutl\s+-verify[\s\S]*-inkey\s+"\$RELEASE_PUBKEY"' 'release signature verification' 'sysupgrade verifies MANIFEST.sig against the embedded release public key' 'sysupgrade does not visibly verify package signatures against the release key' $sysupgradePath
    Test-Pattern $sysupgradeText 'Package release_ed25519\.pub does not match embedded release key[\s\S]*Package release_ed25519\.pub does not match signed manifest' 'release key hash checks' 'sysupgrade checks package key identity and manifest key hash' 'sysupgrade does not visibly check release key identity and manifest hash' $sysupgradePath
    Test-Pattern $sysupgradeText 'CURRENT_MTD[\s\S]*INACTIVE_MTD[\s\S]*ubiattach\s+-m\s+"\$INACTIVE_MTD"[\s\S]*ubiupdatevol\s+/dev/ubi1_1\s+"\$ROOTFS"' 'inactive-slot write target' 'sysupgrade derives inactive slot and writes rootfs to /dev/ubi1_1' 'sysupgrade does not visibly write the inactive UBI rootfs volume' $sysupgradePath

    if ($null -ne $sysupgradeText) {
        if ($sysupgradeText -match 'ubiupdatevol\s+/dev/ubi0_(0|1)\b') {
            Add-Result FAIL 'active-slot write refusal' 'sysupgrade contains an active-slot ubi0 update command' $sysupgradePath
        } else {
            Add-Result PASS 'active-slot write refusal' 'sysupgrade contains no direct /dev/ubi0 kernel/rootfs update command' $sysupgradePath
        }
    }

    Test-Pattern $packageText 'MANIFEST\.sig[\s\S]*release_ed25519\.pub[\s\S]*openssl\s+pkeyutl\s+-sign[\s\S]*openssl\s+pkeyutl\s+-verify' 'packager signing artifacts' 'package_sysupgrade signs MANIFEST.json and embeds release_ed25519.pub' 'package_sysupgrade does not visibly produce signed release-key artifacts' $packagePath
    Test-Pattern $packageText 'Refusing live upload of an unsigned package' 'unsigned live upload refusal' 'package_sysupgrade refuses unsigned live upload unless explicitly allowed' 'package_sysupgrade does not visibly refuse unsigned live upload' $packagePath
    Test-Pattern $packageText 'requires_inactive_slot"\s*:\s*true' 'package inactive-slot metadata' 'sysupgrade manifest declares inactive-slot requirement' 'sysupgrade manifest does not visibly declare inactive-slot requirement' $packagePath

    Test-Pattern $flashUniversalText 'Unsafe BraiinsOS active-rootfs flashing is disabled[\s\S]*Use package_sysupgrade\.sh' 'legacy flash_braiinsos promotion' 'universal installer refuses BraiinsOS active-rootfs flash and points to sysupgrade' 'universal installer may still promote the legacy BraiinsOS active-rootfs flash path' $flashUniversalPath
    Test-Pattern $flashBraiinsosText 'Legacy BraiinsOS UBI Path[\s\S]*active rootfs volume and is disabled[\s\S]*exit 1' 'flash_braiinsos disabled' 'flash_braiinsos exits before active-rootfs flash steps' 'flash_braiinsos is not visibly disabled before active-rootfs flash steps' $flashBraiinsosPath

    Test-Pattern $apiRestText '"/api/v1/firmware/update"[\s\S]*post\(post_system_upgrade\)' 'firmware update alias route' '/api/v1/firmware/update routes to the signed sysupgrade handler' '/api/v1/firmware/update route is missing or not wired to post_system_upgrade' $apiRestPath
    Test-Pattern $apiRestText 'Only signed sysupgrade \.tar packages are accepted by the browser updater[\s\S]*Command::new\("sysupgrade"\)[\s\S]*\.args\(\["--test",\s*&staged_path\]\)' 'browser updater package gate' 'browser updater accepts .tar packages and verifies them with sysupgrade --test' 'browser updater does not visibly gate uploads through sysupgrade --test' $apiRestPath
    Test-Pattern $apiRestText 'for path in \["/api/v1/system/upgrade",\s*"/api/v1/firmware/update"\]' 'firmware update alias test' 'API tests cover the /api/v1/firmware/update alias' 'API tests do not visibly cover the firmware/update alias' $apiRestPath
    Test-Pattern $apiRestText 'read_upgrade_stage_entries_returns_empty_for_missing_root[\s\S]*read_upgrade_stage_entries_marks_tar_case_insensitively_and_preserves_metadata[\s\S]*read_upgrade_stage_entries_limits_depth[\s\S]*read_upgrade_stage_entries_limits_entry_count' 'upgrade staging scanner tests' 'API tests cover upgrade staging scanner missing-root, metadata, depth, and count cases' 'API tests do not visibly cover upgrade staging scanner edge cases' $apiRestPath
    Test-Pattern $apiRestText 'entries\.len\(\)\s*>=\s*32' 'upgrade staging scanner implementation cap' 'upgrade staging scanner caps reported files' 'upgrade staging scanner cap is missing' $apiRestPath
    Test-Pattern $apiRestText 'read_upgrade_stage_entries_limits_entry_count[\s\S]*entries\.len\(\)\s*<=\s*32' 'upgrade staging scanner cap test' 'upgrade staging scanner has test coverage for the report cap' 'upgrade staging scanner cap test is missing' $apiRestPath
    Test-Pattern $apiRestText 'build_system_upgrade_status_reports_idle[\s\S]*build_system_upgrade_status_reports_staged_tar_only[\s\S]*build_system_upgrade_status_reports_null_fwenv_when_unavailable[\s\S]*build_system_upgrade_status_reports_pending_boot_commit_for_upgrade_stage_zero[\s\S]*build_system_upgrade_status_reports_pending_boot_commit_for_upgrade_stage_one[\s\S]*build_system_upgrade_status_uses_boot_slot_fallback_order' 'upgrade status builder tests' 'API tests cover idle, staged, unavailable-fwenv, rollback-armed, and boot-slot fallback status states' 'API tests do not visibly cover upgrade status builder states' $apiRestPath

    Test-Pattern $cargoConfigText 'absolute path contains spaces[\s\S]*set CC_armv7_unknown_linux_musleabihf=%CD%\\zig-cc-arm\.bat' 'Zig absolute path guidance' 'cargo config documents explicit wrapper paths for Windows Zig builds' 'cargo config lacks visible guidance for explicit Zig wrapper paths' $cargoConfigPath
    Test-Pattern $cargoConfigText 'We do NOT put a default in a `\[env\]` block here' 'Zig wrapper env default avoidance' 'cargo config avoids cargo-resolved env defaults that break paths with spaces' 'cargo config may still rely on cargo-resolved Zig wrapper env defaults' $cargoConfigPath

    $harnessText = Get-RepoText 'scripts\validate_production_readiness.ps1'
    Test-Pattern $harnessText 'Join-Path\s+\$dcentraldDir\s+''zig-cc-arm\.bat''' 'Rust check wrapper absolute path' 'harness builds Rust check env vars from repo-root absolute paths' 'harness does not visibly build Zig wrapper env vars from absolute paths' 'scripts\validate_production_readiness.ps1'
    Test-Pattern $harnessText 'if\s+\(-not\s+\$RunRustChecks\)[\s\S]*pass -RunRustChecks' 'RunRustChecks remains optional' 'Rust cargo checks remain opt-in behind -RunRustChecks' 'Rust cargo checks may run by default' 'scripts\validate_production_readiness.ps1'
}

function Test-HonestTelemetryContracts {
    Write-Output ''
    Write-Output '=== Honest Telemetry Contracts ==='

    $apiRestPath = 'dcentrald\dcentrald-api\src\rest.rs'
    $prometheusMetricsPath = 'dcentrald\dcentrald-api-types\src\prometheus_metrics.rs'
    $apiClientPath = 'dashboard\src\api\client.ts'
    $apiTypesPath = 'dashboard\src\api\types.ts'
    $systemDebugPath = 'dashboard\src\components\advanced\SystemDebug.tsx'

    $apiRestText = Get-RepoText $apiRestPath
    $prometheusMetricsText = Get-RepoText $prometheusMetricsPath
    $apiClientText = Get-RepoText $apiClientPath
    $apiTypesText = Get-RepoText $apiTypesPath
    $systemDebugText = Get-RepoText $systemDebugPath

    Test-Pattern $apiRestText '\.route\("/api/system/health",\s*get\(get_system_health\)\)' 'system health route' '/api/system/health is wired to the honest health handler' '/api/system/health is not visibly wired' $apiRestPath
    Test-Pattern $apiRestText '\.route\("/api/system/stats",\s*get\(get_system_stats\)\)' 'system stats route' '/api/system/stats is wired to live host telemetry' '/api/system/stats is not visibly wired' $apiRestPath
    Test-Pattern $apiRestText 'struct\s+SystemStatsResponse[\s\S]*uptime_s[\s\S]*load_percent_1m[\s\S]*mem_used_percent[\s\S]*soc_temp_c' 'system stats response shape' 'system stats response exposes uptime/load/memory/SoC temperature fields' 'system stats response shape is incomplete' $apiRestPath
    Test-Pattern $apiRestText '/proc/uptime[\s\S]*/proc/loadavg[\s\S]*/proc/meminfo[\s\S]*thermal_zone' 'system stats live sources' 'system stats reads Linux procfs/sysfs sources instead of dashboard estimates' 'system stats live procfs/sysfs sources are missing' $apiRestPath
    Test-Pattern $apiRestText 'field_sources[\s\S]*unsupported_metrics[\s\S]*bestDiff[\s\S]*vrTemp' 'system info provenance' '/api/system/info marks compatibility-only fields with provenance and unsupported_metrics' '/api/system/info does not visibly mark unsupported compatibility fields' $apiRestPath
    Test-Pattern ($apiRestText + "`n" + $prometheusMetricsText) 'chip_model:\s*hardware\.chip_type\.clone\(\)[\s\S]*chip_model_source:[\s\S]*hardware_info\.chip_type[\s\S]*dcentrald_info\{\{version=\\"\{\}\\",model=\\"\{\}\\",model_source=\\"\{\}\\"' 'prometheus model provenance' '/metrics dcentrald_info uses hardware_info model provenance instead of a hardcoded S9 label' '/metrics dcentrald_info still lacks hardware model provenance' $prometheusMetricsPath
    Test-Pattern $apiRestText '\.route\("/api/system/upgrade/status",\s*get\(get_system_upgrade_status\)\)[\s\S]*\.route\("/api/system/update/status",\s*get\(get_system_upgrade_status\)\)' 'system upgrade status routes' 'read-only upgrade/update status routes are wired' 'read-only upgrade/update status routes are missing' $apiRestPath
    Test-Pattern $apiRestText '\.route\(\s*"/api/config/backup/manifest",\s*get\(get_config_backup_manifest\),?\s*\)' 'config backup manifest route' 'read-only config backup manifest route is wired' 'config backup manifest route is missing' $apiRestPath
    Test-Pattern $apiRestText 'struct\s+ConfigBackupManifestResponse[\s\S]*read_only:\s*bool[\s\S]*content_collected:\s*bool[\s\S]*restore_supported:\s*bool[\s\S]*daemon_config_export_supported:\s*bool[\s\S]*CONFIG_BACKUP_SOURCE_SPECS[\s\S]*/data/dcentrald\.toml[\s\S]*/etc/dcentrald\.toml[\s\S]*CONFIG_BACKUP_SECRET_KEY_PATTERNS[\s\S]*password[\s\S]*pool\.password' 'config backup manifest response shape' 'config backup manifest reports source metadata and redaction policy without restore support' 'config backup manifest shape, sources, or redaction policy is incomplete' $apiRestPath
    Test-Pattern $apiRestText 'build_config_backup_manifest_response_reports_metadata_only_contract[\s\S]*content_collected[\s\S]*restore_supported[\s\S]*daemon_config_export_supported[\s\S]*dashboard_preferences_export_supported[\s\S]*/data/dcentrald\.toml[\s\S]*/etc/dcentrald\.toml' 'config backup manifest tests' 'API tests cover metadata-only config backup manifest behavior' 'config backup manifest test coverage is missing' $apiRestPath
    Test-Pattern $apiRestText '\.route\(\s*"/api/system/api-compatibility/manifest",\s*get\(get_api_compatibility_manifest\),?\s*\)[\s\S]*\.route\(\s*"/api/compatibility/manifest",\s*get\(get_api_compatibility_manifest\),?\s*\)' 'api compatibility manifest routes' 'read-only API compatibility manifest primary route and alias are wired' 'API compatibility manifest routes are missing' $apiRestPath
    Test-Pattern $apiRestText 'struct\s+ApiCompatibilityManifestResponse[\s\S]*read_only:\s*bool[\s\S]*content_collected:\s*bool[\s\S]*probe_performed:\s*bool[\s\S]*handlers_executed:\s*bool[\s\S]*surfaces:\s*&''static\s*\[ApiCompatibilitySurface\][\s\S]*omissions:\s*&''static\s*\[ApiCompatibilityOmission\]' 'api compatibility manifest response shape' 'API compatibility manifest declares no-probe/read-only fields and explicit omissions' 'API compatibility manifest shape is incomplete' $apiRestPath
    Test-Pattern $apiRestText 'API_COMPATIBILITY_PYASIC_ROUTES[\s\S]*/api/system/info[\s\S]*bestDiff[\s\S]*/api/system/asic' 'api compatibility pyasic declarations' 'manifest declares pyasic/AxeOS discovery routes and unsupported placeholders' 'API compatibility manifest is missing pyasic/AxeOS route declarations' $apiRestPath
    Test-Pattern $apiRestText 'API_COMPATIBILITY_V1_ROUTES[\s\S]*/api/v1/firmware/update/status[\s\S]*/api/v1/firmware/update' 'api compatibility v1 alias declarations' 'manifest declares mounted /api/v1 firmware aliases' 'API compatibility manifest is missing mounted /api/v1 aliases' $apiRestPath
    Test-Pattern $apiRestText 'API_COMPATIBILITY_CGMINER_COMMANDS[\s\S]*summary[\s\S]*switchpool[\s\S]*recognized_unsupported' 'api compatibility cgminer declarations' 'manifest declares CGMiner implemented and recognized-unsupported commands' 'API compatibility manifest is missing CGMiner command declarations' $apiRestPath
    Test-Pattern $apiRestText 'API_COMPATIBILITY_OMISSIONS[\s\S]*/api/config/donation[\s\S]*full VNish OpenAPI /api/v1' 'api compatibility explicit omissions' 'manifest declares explicit omissions for unmounted/documented-only surfaces' 'API compatibility manifest is missing explicit omissions' $apiRestPath
    Test-Pattern $apiRestText 'build_api_compatibility_manifest_response_reports_declared_no_probe_contract[\s\S]*read_only[\s\S]*content_collected[\s\S]*probe_performed[\s\S]*handlers_executed' 'api compatibility no-probe tests' 'API tests cover read-only/no-probe manifest booleans' 'API compatibility manifest no-probe test coverage is missing' $apiRestPath
    Test-Pattern $apiRestText 'build_api_compatibility_manifest_response_reports_declared_no_probe_contract[\s\S]*/api/system/info[\s\S]*/api/system/asic[\s\S]*/api/v1/firmware/update[\s\S]*/api/system/api-compatibility/manifest' 'api compatibility route tests' 'API tests cover declared compatibility routes' 'API compatibility manifest route test coverage is missing' $apiRestPath
    Test-Pattern $apiRestText 'build_api_compatibility_manifest_response_reports_declared_no_probe_contract[\s\S]*summary[\s\S]*switchpool[\s\S]*/api/config/donation' 'api compatibility command omission tests' 'API tests cover CGMiner support states and explicit omissions' 'API compatibility manifest command/omission test coverage is missing' $apiRestPath
    Test-Pattern $apiRestText '\.route\(\s*"/api/diagnostics/logs/manifest",\s*get\(get_diagnostics_log_manifest\),?\s*\)' 'log manifest route' 'read-only diagnostics log manifest route is wired' 'diagnostics log manifest route is missing' $apiRestPath
    Test-Pattern $apiRestText 'struct\s+LogManifestResponse[\s\S]*read_only:\s*bool[\s\S]*content_collected:\s*bool[\s\S]*sources:\s*Vec<LogSourceManifestEntry>[\s\S]*LOG_SOURCE_SPECS[\s\S]*dcentrald-runtime[\s\S]*/tmp/dcentrald\.log[\s\S]*dashboard-server[\s\S]*/tmp/dashboard\.log[\s\S]*not_exposed_metadata_only' 'log manifest response shape' 'log manifest reports known real log sources and access policy' 'log manifest response shape or source list is incomplete' $apiRestPath
    Test-Pattern $apiRestText 'build_log_manifest_response_reports_metadata_only_sources[\s\S]*content_collected[\s\S]*dcentrald-runtime[\s\S]*mode_gated_content_endpoint[\s\S]*dashboard-server[\s\S]*not_exposed_metadata_only' 'log manifest tests' 'API tests cover metadata-only log manifest behavior' 'log manifest test coverage is missing' $apiRestPath
    Test-Pattern $apiRestText 'async\s+fn\s+get_system_upgrade_status\(\)\s*->\s*impl IntoResponse\s*\{[\s\S]*Path::new\(SYSTEM_UPGRADE_STAGE_ROOT\)[\s\S]*read_upgrade_stage_entries\(stage_root\)[\s\S]*read_upgrade_fwenv_snapshot\(\)[\s\S]*Json\(build_system_upgrade_status_payload\(' 'system upgrade status thin wrapper' 'upgrade status endpoint is a thin read-only wrapper over the fixture-testable builder' 'upgrade status endpoint is not visibly a thin read-only wrapper' $apiRestPath
    Test-Pattern $apiRestText 'struct\s+UpgradeFwEnvSnapshot[\s\S]*upgrade_stage:\s*Option<String>[\s\S]*bootcount:\s*Option<String>[\s\S]*bootlimit:\s*Option<String>[\s\S]*boot_slot:\s*Option<String>[\s\S]*dcent_boot_slot:\s*Option<String>[\s\S]*active_slot:\s*Option<String>' 'system upgrade fwenv snapshot' 'upgrade status captures U-Boot env in an explicit snapshot' 'upgrade status fwenv snapshot is missing or incomplete' $apiRestPath
    Test-Pattern $apiRestText 'fn\s+build_system_upgrade_status_payload\([\s\S]*stage_root:\s*&str[\s\S]*stage_root_present:\s*bool[\s\S]*stage_entries:\s*&\[UpgradeStageEntry\][\s\S]*fwenv:\s*&UpgradeFwEnvSnapshot[\s\S]*"pending_boot_commit"[\s\S]*"validated_or_staged"[\s\S]*"idle"[\s\S]*"read_only":\s*true[\s\S]*"staged_packages"[\s\S]*"upgrade_stage"[\s\S]*"boot_slot"[\s\S]*"limitations"' 'system upgrade status builder shape' 'upgrade status builder reports read-only staged/fwenv state without shelling directly' 'upgrade status builder shape is missing read-only staged/fwenv fields' $apiRestPath
    Test-Pattern $apiRestText 'watchdog":\s*read_kernel_watchdog_state\(\)' 'system health watchdog field' '/api/system/health includes passive watchdog state' '/api/system/health does not visibly include passive watchdog state' $apiRestPath
    Test-Pattern $apiRestText 'KERNEL_WATCHDOG0_SYSFS:\s*&str\s*=\s*"/sys/class/watchdog/watchdog0"' 'system health watchdog source' 'watchdog status is sourced from kernel sysfs' 'watchdog status source is not visibly kernel sysfs' $apiRestPath
    Test-Pattern $apiTypesText 'export\s+interface\s+SystemStatsResponse[\s\S]*uptime_s:\s*number[\s\S]*mem_used_percent\??:\s*number\s*\|\s*null[\s\S]*soc_temp_c\??:\s*number\s*\|\s*null' 'dashboard system stats type' 'dashboard models nullable live system telemetry fields' 'dashboard does not visibly model system stats response' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+SystemUpgradeStatusResponse[\s\S]*read_only:\s*boolean[\s\S]*staged_packages[\s\S]*upgrade_stage:\s*string\s*\|\s*null' 'dashboard upgrade status type' 'dashboard models read-only system upgrade status' 'dashboard does not visibly model system upgrade status' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+ConfigBackupManifestResponse[\s\S]*read_only:\s*boolean[\s\S]*content_collected:\s*boolean[\s\S]*restore_supported:\s*boolean[\s\S]*sources:\s*ConfigBackupSourceEntry\[\]' 'dashboard config backup manifest type' 'dashboard models metadata-only config backup manifest response' 'dashboard config backup manifest type is missing' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+ApiCompatibilityManifestResponse[\s\S]*read_only:\s*boolean[\s\S]*content_collected:\s*boolean[\s\S]*probe_performed:\s*boolean[\s\S]*handlers_executed:\s*boolean[\s\S]*surfaces:\s*ApiCompatibilitySurface\[\][\s\S]*omissions:\s*ApiCompatibilityOmission\[\]' 'dashboard API compatibility manifest type' 'dashboard models read-only no-probe API compatibility manifest response' 'dashboard API compatibility manifest type is missing' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+LogManifestResponse[\s\S]*read_only:\s*boolean[\s\S]*content_collected:\s*boolean[\s\S]*sources:\s*LogSourceManifestEntry\[\]' 'dashboard log manifest type' 'dashboard models metadata-only log manifest response' 'dashboard log manifest type is missing' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+KernelWatchdogState[\s\S]*available:\s*boolean[\s\S]*read_only\??:\s*boolean' 'dashboard watchdog status type' 'dashboard models passive watchdog state' 'dashboard does not visibly model passive watchdog state' $apiTypesPath
    Test-Pattern $apiClientText 'getSystemStats:\s*\(\)\s*=>\s*get<SystemStatsResponse>\(''\/api\/system\/stats''\)' 'dashboard system stats client' 'dashboard has a typed /api/system/stats client' 'dashboard client is missing getSystemStats' $apiClientPath
    Test-Pattern $apiClientText 'getSystemUpgradeStatus:\s*\(\)\s*=>\s*get<SystemUpgradeStatusResponse>\(''\/api\/system\/upgrade\/status''\)' 'dashboard system upgrade status client' 'dashboard has a typed /api/system/upgrade/status client' 'dashboard client is missing getSystemUpgradeStatus' $apiClientPath
    Test-Pattern $apiClientText 'getConfigBackupManifest:\s*\(\)\s*=>\s*get<ConfigBackupManifestResponse>\(''\/api\/config\/backup\/manifest''\)' 'dashboard config backup manifest client' 'dashboard has a typed /api/config/backup/manifest client' 'dashboard client is missing getConfigBackupManifest' $apiClientPath
    Test-Pattern $apiClientText 'getApiCompatibilityManifest:\s*\(\)\s*=>\s*get<ApiCompatibilityManifestResponse>\(''\/api\/system\/api-compatibility\/manifest''\)' 'dashboard API compatibility manifest client' 'dashboard has a typed /api/system/api-compatibility/manifest client' 'dashboard client is missing getApiCompatibilityManifest' $apiClientPath
    Test-Pattern $apiClientText 'getLogManifest:\s*\(\)\s*=>\s*get<LogManifestResponse>\(''\/api\/diagnostics\/logs\/manifest''\)' 'dashboard log manifest client' 'dashboard has a typed /api/diagnostics/logs/manifest client' 'dashboard client is missing getLogManifest' $apiClientPath
    Test-Pattern $systemDebugText 'api\.getSystemStats\(\)' 'SystemDebug live stats fetch' 'SystemDebug fetches live /api/system/stats telemetry' 'SystemDebug does not visibly fetch live system stats' $systemDebugPath
    Test-Pattern $systemDebugText 'BOSMINER OWNER' 'SystemDebug proxy I2C owner state' 'SystemDebug marks I2C ownership as BOSMINER OWNER in proxy/hybrid mode' 'SystemDebug does not visibly mark proxy I2C ownership' $systemDebugPath
    Test-Pattern $systemDebugText 'Blocked:\s*bosminer owns I2C in proxy mode' 'SystemDebug proxy I2C reset gate' 'SystemDebug blocks I2C reset in proxy/hybrid mode' 'SystemDebug does not visibly block I2C reset in proxy/hybrid mode' $systemDebugPath

    $upgradeStatusHandler = Get-RustFunctionText $apiRestText 'get_system_upgrade_status'
    if ($null -ne $upgradeStatusHandler -and $upgradeStatusHandler -match 'Command::new\("sysupgrade"\)|Command::new\("reboot"\)|Command::new\("fw_setenv"\)|ubiupdatevol|fw_setenv\s+') {
        Add-Result FAIL 'upgrade status no side effects' 'read-only upgrade status handler contains update/reboot side-effect calls' $apiRestPath
    } else {
        Add-Result PASS 'upgrade status no side effects' 'read-only upgrade status handler does not invoke sysupgrade/fw_setenv/ubiupdatevol/reboot' $apiRestPath
    }

    $watchdogStatusHandler = Get-RustFunctionText $apiRestText 'read_kernel_watchdog_state'
    if ($null -ne $watchdogStatusHandler -and $watchdogStatusHandler -match '/dev/watchdog|enable_watchdog|disable_watchdog|feed_watchdog|Command::new|write\(') {
        Add-Result FAIL 'watchdog status no arming' 'watchdog status handler may open/arm/control watchdog' $apiRestPath
    } else {
        Add-Result PASS 'watchdog status no arming' 'watchdog status handler only reads watchdog sysfs state' $apiRestPath
    }

    $upgradeStatusBuilder = Get-RustFunctionText $apiRestText 'build_system_upgrade_status_payload'
    if ($null -ne $upgradeStatusBuilder -and $upgradeStatusBuilder -match 'Command::new|tokio::process|std::fs|tokio::fs|Path::new|\.exists\(\)|\.await|tokio::spawn|File::create|write_all|remove_file|create_dir|fw_setenv\s+|ubiupdatevol|Command::new\("sysupgrade"\)|Command::new\("reboot"\)') {
        Add-Result FAIL 'upgrade status builder pure' 'upgrade status builder contains filesystem, process, async, or upgrade side-effect calls' $apiRestPath
    } else {
        Add-Result PASS 'upgrade status builder pure' 'upgrade status builder is fixture-driven and side-effect free' $apiRestPath
    }

    $configBackupManifestText = @(
        (Get-RustFunctionText $apiRestText 'config_backup_source_entry'),
        (Get-RustFunctionText $apiRestText 'build_config_backup_manifest_response'),
        (Get-RustFunctionText $apiRestText 'get_config_backup_manifest')
    ) -join "`n"
    if ($null -ne $configBackupManifestText -and $configBackupManifestText -match 'std::fs::read|read_to_string|toml::from_str|atomic_write|write_toml_section|post_config|Command::new|tokio::fs|File::open|File::create|OpenOptions|remove_file|create_dir|write_all|set_len\(|rename\(') {
        Add-Result FAIL 'config backup manifest no content collection' 'config backup manifest appears to read/parse/write config contents or mutate files' $apiRestPath
    } else {
        Add-Result PASS 'config backup manifest no content collection' 'config backup manifest reports metadata without reading, parsing, exporting, restoring, or writing config values' $apiRestPath
    }

    $apiCompatibilityManifestText = @(
        (Get-RustFunctionText $apiRestText 'build_api_compatibility_manifest_response'),
        (Get-RustFunctionText $apiRestText 'get_api_compatibility_manifest')
    ) -join "`n"
    if ($null -ne $apiCompatibilityManifestText -and $apiCompatibilityManifestText -match 'Command::new|TcpStream|reqwest::|std::fs::read|read_to_string|tokio::fs|File::open|File::create|OpenOptions|post_[A-Za-z0-9_]+\(|atomic_write|write_toml_section|tokio::spawn|Multipart|remove_file|create_dir|write_all|set_len\(|rename\(') {
        Add-Result FAIL 'api compatibility manifest no probing' 'API compatibility manifest appears to call handlers, probe endpoints, read content, or mutate state' $apiRestPath
    } else {
        Add-Result PASS 'api compatibility manifest no probing' 'API compatibility manifest is a declared read-only contract without handler execution or runtime probes' $apiRestPath
    }

    $logManifestText = @(
        (Get-RustFunctionText $apiRestText 'log_source_manifest_entry'),
        (Get-RustFunctionText $apiRestText 'build_log_manifest_response'),
        (Get-RustFunctionText $apiRestText 'get_diagnostics_log_manifest')
    ) -join "`n"
    if ($null -ne $logManifestText -and $logManifestText -match 'std::fs::read|read_to_string|read_dir|Command::new|tokio::fs|File::open|File::create|OpenOptions|\.lines\(\)|tail\s+-|grep\s+|remove_file|create_dir|write_all|set_len\(|rename\(') {
        Add-Result FAIL 'log manifest no content collection' 'log manifest appears to read/collect/mutate log contents' $apiRestPath
    } else {
        Add-Result PASS 'log manifest no content collection' 'log manifest reports metadata without reading, collecting, or mutating log contents' $apiRestPath
    }
}

function Test-DashboardReadOnlyUpgradeStatusUi {
    Write-Output ''
    Write-Output '=== Dashboard Read-Only Upgrade Status UI ==='

    $upgradeUiPath = 'dashboard\src\components\common\UpgradeStatusPanel.tsx'
    $settingsPath = 'dashboard\src\components\standard\SettingsPage.tsx'
    $maintenancePath = 'dashboard\src\components\advanced\MaintenanceMode.tsx'

    $upgradeUiText = Get-RepoText $upgradeUiPath
    $settingsText = Get-RepoText $settingsPath
    $maintenanceText = Get-RepoText $maintenancePath

    Test-Pattern $upgradeUiText 'api\.getSystemUpgradeStatus\(\)' 'upgrade status UI fetch' 'UI fetches read-only upgrade status from the typed API client' 'UI does not call api.getSystemUpgradeStatus()' $upgradeUiPath
    Test-Pattern $upgradeUiText 'read_only|readOnly|Read-only|read-only|status only|view only' 'upgrade status read-only copy' 'UI visibly labels upgrade status as read-only/status-only' 'UI lacks read-only/status-only language' $upgradeUiPath
    Test-Pattern $upgradeUiText 'unavailable|not available|not exposed|not supported' 'upgrade status unavailable copy' 'UI has unavailable-state copy for missing upgrade status data' 'UI lacks unavailable-state language' $upgradeUiPath
    Test-Pattern $upgradeUiText 'limitations\??\.|limitations\??\s*\|\||Limitations|limitations' 'upgrade status limitations copy' 'UI renders endpoint limitations/provenance' 'UI does not visibly render limitations' $upgradeUiPath
    Test-Pattern $upgradeUiText 'staged_package_count|staged_packages|upgrade_stage|boot_slot|bootcount|bootlimit' 'upgrade status fields' 'UI renders real upgrade status fields instead of local staged state only' 'UI does not visibly render upgrade status fields' $upgradeUiPath
    Test-Pattern $upgradeUiText 'No staged upgrade[\s\S]*Package staged[\s\S]*Boot commit pending[\s\S]*Backend-reported state' 'upgrade status state copy' 'UI maps backend upgrade states to clear read-only operator copy' 'UI lacks explicit read-only copy for backend upgrade states' $upgradeUiPath
    Test-Pattern $settingsText 'UpgradeStatusPanel' 'settings upgrade status surface' 'Settings firmware section renders the read-only upgrade status panel' 'Settings firmware section does not render the upgrade status panel' $settingsPath
    Test-Pattern $maintenanceText 'UpgradeStatusPanel' 'maintenance upgrade status surface' 'Maintenance firmware section renders the read-only upgrade status panel' 'Maintenance firmware section does not render the upgrade status panel' $maintenancePath

    if ($null -ne $upgradeUiText -and $upgradeUiText -match 'api\.uploadFirmware|FormData|type="file"|onProgress|progressbar|Uploading package|Flashing\.\.\.|Flash Staged|Validate \+ Flash|flash the inactive firmware slot') {
        Add-Result FAIL 'upgrade status no flashing UI' 'read-only upgrade status panel exposes upload/flash/progress affordances' $upgradeUiPath
    } else {
        Add-Result PASS 'upgrade status no flashing UI' 'read-only upgrade status panel does not imply flashing progress or expose flash actions' $upgradeUiPath
    }
}

function Test-DashboardApiCompatibilityManifestUi {
    Write-Output ''
    Write-Output '=== Dashboard API Compatibility Manifest UI ==='

    $aboutPath = 'dashboard\src\components\common\AboutPage.tsx'
    $manifestCardPath = 'dashboard\src\components\common\ApiCompatibilityManifestCard.tsx'
    $apiExplorerPath = 'dashboard\src\components\advanced\ApiExplorer.tsx'

    $aboutText = Get-RepoText $aboutPath
    $manifestCardText = Get-RepoText $manifestCardPath
    $apiExplorerText = Get-RepoText $apiExplorerPath

    Test-Pattern $aboutText 'import\s+\{\s*ApiCompatibilityManifestCard\s*\}[\s\S]*<AboutCard title="Firmware">[\s\S]*<ApiCompatibilityManifestCard\s*/>[\s\S]*<AboutCard title="Hardware">' 'about API compatibility card placement' 'About page renders passive API compatibility manifest between firmware and hardware identity' 'About page does not render API compatibility manifest in the expected passive location' $aboutPath
    Test-Pattern $manifestCardText 'api\.getApiCompatibilityManifest\(\)[\s\S]*Firmware-declared compatibility manifest[\s\S]*does not call, probe, or test' 'api compatibility card honest copy' 'API compatibility card fetches only the manifest and states no probing' 'API compatibility card copy/client behavior does not prove no-probe honesty' $manifestCardPath
    Test-Pattern $manifestCardText 'UNAVAILABLE_COPY\s*=\s*''API compatibility manifest unavailable\. No endpoint status was inferred\.''' 'api compatibility card unavailable copy' 'API compatibility card unavailable state refuses inferred endpoint status' 'API compatibility card unavailable state does not refuse inferred endpoint status' $manifestCardPath
    Test-Pattern $manifestCardText 'READ-ONLY[\s\S]*DECLARED BY FIRMWARE[\s\S]*NO PROBING[\s\S]*manifest\.surfaces\.map[\s\S]*manifest\.limitations' 'api compatibility card declared manifest render' 'API compatibility card renders declared surfaces and limitations from the manifest' 'API compatibility card does not visibly render declared surfaces and limitations' $manifestCardPath
    Test-Pattern $manifestCardText 'downloadCompatibilityManifest[\s\S]*JSON\.stringify\(manifest,\s*null,\s*2\)[\s\S]*dcentos-api-compatibility-manifest[\s\S]*Browser-local JSON export; no endpoints are probed' 'api compatibility manifest export' 'API compatibility card exports the already-declared manifest as browser-local JSON without probes' 'API compatibility card does not expose a safe declared-manifest export' $manifestCardPath

    if ($null -ne $manifestCardText -and $manifestCardText -match 'fetch\(|XMLHttpRequest|ActionButton|setInterval|api\.getStatus|api\.read[A-Za-z0-9_]*\(|api\.write[A-Za-z0-9_]*\(|api\.restart\(|api\.reboot\(|api\.uploadFirmware\(') {
        Add-Result FAIL 'api compatibility card no active probing controls' 'API compatibility card appears to probe endpoints or expose action controls' $manifestCardPath
    } else {
        Add-Result PASS 'api compatibility card no active probing controls' 'API compatibility card uses only the typed manifest client and no active endpoint runner' $manifestCardPath
    }

    if ($null -ne $manifestCardText -and $manifestCardText -match 'reachable|online|healthy|latency_ms|status_code|last_checked|\bPASS\b|\bFAIL\b') {
        Add-Result FAIL 'api compatibility card no fake endpoint status' 'API compatibility card appears to infer runtime endpoint status' $manifestCardPath
    } else {
        Add-Result PASS 'api compatibility card no fake endpoint status' 'API compatibility card avoids runtime reachability/status claims' $manifestCardPath
    }

    if ($null -ne $apiExplorerText -and $apiExplorerText -match 'ApiCompatibilityManifestCard|/api/system/api-compatibility/manifest|/api/compatibility/manifest') {
        Add-Result FAIL 'api compatibility manifest separate from explorer' 'ApiExplorer includes the passive compatibility manifest or adds it to the active request list' $apiExplorerPath
    } else {
        Add-Result PASS 'api compatibility manifest separate from explorer' 'ApiExplorer remains an active request runner separate from passive compatibility metadata' $apiExplorerPath
    }
}

function Test-CompatibilityApiHonesty {
    Write-Output ''
    Write-Output '=== Compatibility API Honesty ==='

    $cgminerPath = 'dcentrald\dcentrald-api\src\cgminer.rs'
    $cgminerText = Get-RepoText $cgminerPath

    Test-Pattern $cgminerText 'let\s+hardware_errors:\s*u64\s*=\s*miner\.chains\.iter\(\)\.map\(\|chain\|\s*chain\.errors\s+as\s+u64\)\.sum\(\)' 'cgminer hardware error source' 'CGMiner summary uses real chain error totals for Hardware Errors' 'CGMiner summary does not visibly source Hardware Errors from chain errors' $cgminerPath
    Test-Pattern $cgminerText '_DCENTUnsupported[\s\S]*Last getwork[\s\S]*_DCENTFieldSources[\s\S]*Best Share[\s\S]*recent_share_history' 'cgminer unsupported summary fields' 'CGMiner summary labels unsupported compatibility counters and sources Best Share from real share history' 'CGMiner summary does not visibly label unsupported compatibility counters or Best Share provenance' $cgminerPath
    Test-Pattern $cgminerText '_DCENTFieldSources[\s\S]*miner_state\.accepted[\s\S]*miner_state\.rejected' 'cgminer field provenance' 'CGMiner compatibility responses include field provenance' 'CGMiner compatibility responses do not visibly include field provenance' $cgminerPath
}

function Test-DashboardNoFabricatedOperatorData {
    Write-Output ''
    Write-Output '=== Dashboard No Fabricated Operator Data ==='

    $dataExportPath = 'dashboard\src\components\features\DataExport.tsx'
    $demandResponsePath = 'dashboard\src\components\features\DemandResponse.tsx'
    $enLocalePath = 'dashboard\src\i18n\locales\en.ts'
    $maintenancePath = 'dashboard\src\components\advanced\MaintenanceMode.tsx'
    $logsPagePath = 'dashboard\src\components\standard\LogsPage.tsx'
    $settingsPath = 'dashboard\src\components\standard\SettingsPage.tsx'
    $sharesPagePath = 'dashboard\src\components\standard\SharesPage.tsx'
    $standardDashboardPath = 'dashboard\src\components\standard\StandardDashboard.tsx'
    $statsGridPath = 'dashboard\src\components\standard\KitDashboardPage.tsx'
    $kpiGridPath = 'dashboard\src\components\standard\KitStatsKpiGrid.tsx'
    $currentBlockPath = 'dashboard\src\components\standard\CurrentBlockCard.tsx'
    $autotunerEvidencePath = 'dashboard\src\components\standard\AutotunerEvidencePanel.tsx'
    $thermalPosturePath = 'dashboard\src\components\standard\ThermalPowerPostureCard.tsx'
    $miningWorkPosturePath = 'dashboard\src\components\standard\MiningWorkPostureCard.tsx'
    $miningPipelineManifestPath = 'dashboard\src\components\standard\MiningPipelineManifestCard.tsx'
    $tempFansPath = 'dashboard\src\components\standard\TempFansPage.tsx'
    $standardCssPath = 'dashboard\src\styles\standard.css'
    $apiClientPath = 'dashboard\src\api\client.ts'
    $apiTypesPath = 'dashboard\src\api\types.ts'
    $apiRestPath = 'dcentrald\dcentrald-api\src\rest.rs'
    $apiLibPath = 'dcentrald\dcentrald-api\src\lib.rs'
    $apiTypesRustPath = 'dcentrald\dcentrald-api-types\src\lib.rs'
    $daemonConfigPath = 'dcentrald\dcentrald\src\config.rs'
    $daemonPath = 'dcentrald\dcentrald\src\daemon.rs'
    $serialMiningPath = 'dcentrald\dcentrald\src\serial_mining.rs'
    $pipelineSmokePlanPath = 'docs\dev\2026-04-28-ralph-loop-30\hardware-smoke-test-plan.md'
    $pipelineRollbackProofPath = 'docs\dev\2026-04-28-ralph-loop-30\rollback-proof-plan.md'
    $pipelineEvidenceTemplatePath = 'docs\dev\2026-04-28-ralph-loop-30\evidence-template.md'
    $dataExportText = Get-RepoText $dataExportPath
    $demandResponseText = Get-RepoText $demandResponsePath
    $enLocaleText = Get-RepoText $enLocalePath
    $maintenanceText = Get-RepoText $maintenancePath
    $logsPageText = Get-RepoText $logsPagePath
    $settingsText = Get-RepoText $settingsPath
    $sharesPageText = Get-RepoText $sharesPagePath
    $standardDashboardText = Get-RepoText $standardDashboardPath
    $statsGridText = Get-RepoText $statsGridPath
    $kpiGridText = Get-RepoText $kpiGridPath
    $currentBlockText = Get-RepoText $currentBlockPath
    $autotunerEvidenceText = Get-RepoText $autotunerEvidencePath
    $thermalPostureText = Get-RepoText $thermalPosturePath
    $miningWorkPostureText = Get-RepoText $miningWorkPosturePath
    $miningPipelineManifestText = Get-RepoText $miningPipelineManifestPath
    $tempFansText = Get-RepoText $tempFansPath
    $standardCssText = Get-RepoText $standardCssPath
    $apiClientText = Get-RepoText $apiClientPath
    $apiTypesText = Get-RepoText $apiTypesPath
    $apiRestText = Get-RepoText $apiRestPath
    $apiLibText = Get-RepoText $apiLibPath
    $apiTypesRustText = Get-RepoText $apiTypesRustPath
    $daemonConfigText = Get-RepoText $daemonConfigPath
    $daemonText = Get-RepoText $daemonPath
    $serialMiningText = Get-RepoText $serialMiningPath
    $pipelineSmokePlanText = Get-RepoText $pipelineSmokePlanPath
    $pipelineRollbackProofText = Get-RepoText $pipelineRollbackProofPath
    $pipelineEvidenceTemplateText = Get-RepoText $pipelineEvidenceTemplatePath

    if ($null -ne $dataExportText -and $dataExportText -match '0\.000005|1100\s*/\s*1000|estimatedBtcDaily|TaxReportEntry') {
        Add-Result FAIL 'tax export placeholder removal' 'DataExport still contains placeholder BTC/power tax values' $dataExportPath
    } else {
        Add-Result PASS 'tax export placeholder removal' 'DataExport refuses tax reports until real share/revenue history exists' $dataExportPath
    }

    Test-Pattern $dataExportText 'Tax report is unavailable[\s\S]*real share/revenue history[\s\S]*No estimated BTC' 'tax export honest unavailable state' 'DataExport explains tax export is unavailable instead of fabricating revenue' 'DataExport does not visibly explain unavailable tax export state' $dataExportPath

    Test-Pattern $demandResponseText 'Runtime Status[\s\S]*Unavailable[\s\S]*READINESS ONLY[\s\S]*No curtailment command is sent from this page[\s\S]*Runtime control[\s\S]*In development[\s\S]*Revenue impact[\s\S]*Not calculated' 'demand response readiness-only status' 'DemandResponse reports DPS/grid-price runtime status as unavailable instead of fabricated telemetry' 'DemandResponse does not visibly report DPS/grid-price runtime status as unavailable' $demandResponsePath
    Test-Pattern $demandResponseText 'planning surface only[\s\S]*does not send curtailment[\s\S]*fan[\s\S]*voltage[\s\S]*frequency[\s\S]*pool commands' 'demand response no-control copy' 'DemandResponse tells operators the page does not send runtime hardware or pool commands' 'DemandResponse lacks visible no-control copy' $demandResponsePath
    Test-Pattern $demandResponseText 'Demand response draft updated locally\. No miner state changed\.' 'demand response save honesty' 'DemandResponse save action reports local draft only' 'DemandResponse save action does not clearly say no miner state changed' $demandResponsePath
    Test-Pattern $demandResponseText 'No live price source[\s\S]*Draft threshold for future policy design[\s\S]*not applied to mining, fan, voltage, or pool state[\s\S]*Planning flag only[\s\S]*not fetching grid prices or changing power[\s\S]*Acknowledge Draft' 'demand response draft-only controls' 'DemandResponse labels settings as local draft policy, not applied runtime control' 'DemandResponse still appears to apply grid-price controls to runtime mining' $demandResponsePath
    Test-Pattern $enLocaleText '''demand\.subtitle'':\s*''Plan demand-response policy\.[\s\S]*Runtime DPS control is unavailable' 'demand response subtitle honesty' 'DemandResponse subtitle states runtime DPS control is unavailable until backend status exists' 'DemandResponse subtitle still implies active automatic curtailment' $enLocalePath
    if ($null -ne $demandResponseText -and $demandResponseText -match 'api\.|fetch\(|XMLHttpRequest|FormData|/api/action|/api/debug|/api/fan|/api/pools|/api/system/upgrade|/api/tou/schedule') {
        Add-Result FAIL 'demand response no active control calls' 'DemandResponse appears to call sleep/wake or hardware-control APIs' $demandResponsePath
    } else {
        Add-Result PASS 'demand response no active control calls' 'DemandResponse contains no sleep/wake or hardware-control API calls' $demandResponsePath
    }
    if ($null -ne $demandResponseText -and $demandResponseText -match 'DemandResponseStatus|currentPriceCentsKwh|priceSignal|revenueToday|negativePriceHoursToday|priceColor|signalLabel|Simulated status') {
        Add-Result FAIL 'demand response no fabricated telemetry' 'DemandResponse still contains hardcoded live price, revenue, or runtime curtailment claims' $demandResponsePath
    } else {
        Add-Result PASS 'demand response no fabricated telemetry' 'DemandResponse avoids hardcoded live price, revenue, and runtime curtailment claims' $demandResponsePath
    }
    if ($null -ne $demandResponseText -and $demandResponseText -match 'Automatically curtail|Auto-curtailment|real-time grid pricing|Mining pauses|Grid pays|Maximum power|Mining curtailed') {
        Add-Result FAIL 'demand response no active-control claims' 'DemandResponse still claims active grid/mining control' $demandResponsePath
    } else {
        Add-Result PASS 'demand response no active-control claims' 'DemandResponse avoids active grid/mining control claims' $demandResponsePath
    }
    $frLocaleText = Get-RepoText 'dashboard\src\i18n\locales\fr.ts'
    if ($null -ne $enLocaleText -and $null -ne $frLocaleText -and (($enLocaleText + $frLocaleText) -match 'Automatically curtail|R.duisez automatiquement|Mine during negative prices|Miner pendant les prix n.gatifs|get paid|.tre pay.')) {
        Add-Result FAIL 'demand response locale honesty' 'DemandResponse locale strings still imply live control or earnings' 'dashboard\src\i18n\locales'
    } else {
        Add-Result PASS 'demand response locale honesty' 'DemandResponse locale strings match draft/readiness behavior' 'dashboard\src\i18n\locales'
    }
    if ($null -ne $enLocaleText -and $null -ne $frLocaleText -and (($enLocaleText + $frLocaleText) -match 'demand\.revenueToday|demand\.priceSignal')) {
        Add-Result FAIL 'demand response no stale live locale keys' 'DemandResponse locale files still contain unused live revenue/price-signal keys' 'dashboard\src\i18n\locales'
    } else {
        Add-Result PASS 'demand response no stale live locale keys' 'DemandResponse locale files do not retain unused live revenue/price-signal keys' 'dashboard\src\i18n\locales'
    }

    if ($null -ne $logsPageText -and $logsPageText -match "fetch\('/api/status'\)|const\s+synth|Firmware:\s*v\$\{|Hashrate:\s*\$\{|Shares:\s*\$\{st\.accepted") {
        Add-Result FAIL 'logs page synthetic fallback removal' 'LogsPage still fabricates log entries from status data' $logsPagePath
    } else {
        Add-Result PASS 'logs page synthetic fallback removal' 'LogsPage does not fabricate status-derived log entries when logs are unavailable' $logsPagePath
    }
    Test-Pattern $logsPageText 'No synthetic logs were generated[\s\S]*will not create status-derived log rows' 'logs page honest unavailable state' 'LogsPage explains unavailable logs instead of inventing entries' 'LogsPage lacks honest unavailable copy for missing logs' $logsPagePath
    Test-Pattern $logsPageText 'api\.getLogManifest\(\)[\s\S]*Read-only metadata manifest[\s\S]*No log content was collected' 'logs page manifest surface' 'LogsPage surfaces the metadata-only log manifest without implying collected content' 'LogsPage does not visibly surface the metadata-only log manifest' $logsPagePath
    if ($null -ne $logsPageText -and $logsPageText -match 'setRestLogs\([^)]*manifest|manifest\.sources\.map\([^)]*parseLogLine|content_collected:\s*true') {
        Add-Result FAIL 'logs page manifest no content synthesis' 'LogsPage appears to turn manifest metadata into log entries' $logsPagePath
    } else {
        Add-Result PASS 'logs page manifest no content synthesis' 'LogsPage keeps manifest metadata separate from log entries' $logsPagePath
    }

    Test-Pattern $settingsText 'api\.getConfigBackupManifest\(\)[\s\S]*Dashboard preference export is browser-local[\s\S]*No TOML content was collected' 'settings config backup manifest surface' 'Settings surfaces metadata-only config backup readiness without implying daemon backup content' 'Settings does not visibly surface config backup manifest honesty' $settingsPath
    Test-Pattern $settingsText 'dcentos-dashboard-preferences[\s\S]*Export Dashboard Preferences[\s\S]*Import Dashboard Preferences' 'settings dashboard-only preference export' 'Settings labels browser-local export/import as dashboard preferences' 'Settings still labels browser-local preferences as full config backup' $settingsPath
    if ($null -ne $settingsText -and $settingsText -match 'api\.post<[^>]*>\(''\/api\/config\/backup|restoreConfig|Import Daemon Config|Export Daemon Config|setBackupManifest\([^)]*settings|content_collected:\s*true') {
        Add-Result FAIL 'settings config backup no daemon restore/export' 'Settings appears to expose daemon config import/export or synthesize manifest content' $settingsPath
    } else {
        Add-Result PASS 'settings config backup no daemon restore/export' 'Settings keeps daemon config backup as read-only metadata and browser preferences as local-only export' $settingsPath
    }

    Test-Pattern $apiClientText 'getShareHistory:\s*async\s*\(\)\s*=>\s*normalizeShareHistory\(await get<[^>]+>\(''\/api\/history\/shares''\)\)' 'share history API client' 'typed client fetches real /api/history/shares' 'typed client does not expose real /api/history/shares' $apiClientPath
    Test-Pattern $apiTypesText 'export\s+interface\s+RecentShareEvent[\s\S]*timestamp_ms:\s*number[\s\S]*result:[\s\S]*job_id:\s*string[\s\S]*difficulty\??:\s*number\s*\|\s*null' 'share history event contract' 'dashboard models real recent share events' 'dashboard share event type is missing required fields' $apiTypesPath
    Test-Pattern $apiRestText '\.route\("/api/history/shares",\s*get\(get_share_history\)\)[\s\S]*recent_share_history' 'share history backend contract' 'backend routes /api/history/shares to recent_share_history' 'backend share history route/storage is not visible' $apiRestPath
    Test-Pattern $apiRestText '\.route\("/api/network/block",\s*get\(get_network_block\)\)' 'network block backend route' 'backend mounts read-only /api/network/block' 'backend does not visibly mount /api/network/block' $apiRestPath
    Test-Pattern $apiRestText 'async\s+fn\s+get_network_block[\s\S]*"read_only":\s*true[\s\S]*"internet_dependency":\s*false[\s\S]*"block_height":\s*null[\s\S]*"block_hash":\s*null[\s\S]*"Public blockchain API fallback is disabled by default' 'network block unavailable contract' 'backend returns honest nullable block fields with public fallback disabled' 'backend network block contract does not visibly fail unavailable without fabricated data' $apiRestPath
    Test-Pattern $apiRestText '"source_manifest"[\s\S]*"local_node"[\s\S]*"enabled"[\s\S]*"configured"[\s\S]*"available":\s*false[\s\S]*"live_rpc":\s*false[\s\S]*"request_timeout_ms"[\s\S]*"public_fallback"[\s\S]*"enabled":\s*false[\s\S]*"cache"[\s\S]*"ttl_ms"' 'network block source manifest' 'backend reports local-node/public/cache source capability without live RPC' 'backend network block source manifest is missing or not manifest-only' $apiRestPath
    Test-Pattern $daemonConfigText 'pub\s+network_block:\s*NetworkBlockConfig[\s\S]*self\.network_block\.validate\(\)' 'network block daemon config wiring' 'daemon config schema includes validated network_block section' 'daemon config schema does not visibly validate network_block' $daemonConfigPath
    Test-Pattern $apiLibText 'pub\s+struct\s+NetworkBlockConfig[\s\S]*impl\s+Default\s+for\s+NetworkBlockConfig[\s\S]*enabled:\s*false[\s\S]*request_timeout_ms:\s*1200[\s\S]*cache_ttl_ms:\s*30000[\s\S]*public_fallback_enabled:\s*false[\s\S]*must not embed credentials[\s\S]*250\.\.=3000[\s\S]*5000\.\.=300000' 'network block config safety defaults' 'network block config defaults disabled, public fallback off, timeout/cache bounded, URL credentials rejected' 'network block config defaults or validation are not visibly safe' $apiLibPath
    if ($null -ne $apiRestText -and $apiRestText -match 'CONFIG_ALLOWED_KEYS:\s*&\[\&str\]\s*=\s*&\[[^\]]*"network_block"') {
        Add-Result FAIL 'network block no generic config write' 'POST /api/config appears to allow network_block credential-bearing updates' $apiRestPath
    } else {
        Add-Result PASS 'network block no generic config write' 'POST /api/config does not whitelist network_block credential-bearing updates' $apiRestPath
    }
    Test-Pattern $apiTypesText 'export\s+interface\s+NetworkBlockResponse[\s\S]*read_only:\s*boolean[\s\S]*internet_dependency:\s*false[\s\S]*block_height\??:\s*number\s*\|\s*null[\s\S]*block_hash\??:\s*string\s*\|\s*null[\s\S]*source_manifest\??:\s*NetworkBlockSourceManifest[\s\S]*limitations:\s*string\[\]' 'network block TS contract' 'dashboard models read-only network block response with nullable real-data fields and source manifest' 'dashboard API types do not visibly model the network block honesty contract' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+NetworkBlockSourceManifest[\s\S]*local_node:[\s\S]*enabled:\s*boolean[\s\S]*configured:\s*boolean[\s\S]*live_rpc:\s*boolean[\s\S]*public_fallback:[\s\S]*enabled:\s*boolean[\s\S]*cache:[\s\S]*ttl_ms' 'network block source manifest TS contract' 'dashboard models local-node/public/cache source manifest' 'dashboard API types do not visibly model network block source manifest details' $apiTypesPath
    Test-Pattern $apiClientText 'getNetworkBlock:\s*\(\)\s*=>\s*get<NetworkBlockResponse>\(''\/api\/network\/block''\)' 'network block API client' 'typed client fetches /api/network/block' 'typed client does not expose /api/network/block' $apiClientPath
    Test-Pattern $statsGridText 'CurrentBlockCard[\s\S]*<CurrentBlockCard\s+compact\s*/>' 'network block hero placement' 'Standard overview renders the current Bitcoin block card in the hero row' 'Standard overview does not visibly render CurrentBlockCard in the hero area' $statsGridPath
    Test-Pattern $currentBlockText 'api\.getNetworkBlock\(\)[\s\S]*Unavailable' 'network block card typed fetch' 'CurrentBlockCard fetches the typed endpoint and renders unavailable states' 'CurrentBlockCard does not visibly fetch/render the network block endpoint' $currentBlockPath
    Test-Pattern $currentBlockText 'Local node not configured[\s\S]*Public fallback disabled by default' 'network block card honest unavailable copy' 'CurrentBlockCard explains missing local-node and public-fallback sources' 'CurrentBlockCard lacks clear unavailable copy for missing block sources' $currentBlockPath
    Test-Pattern $currentBlockText 'localNodeLabel[\s\S]*publicFallbackLabel[\s\S]*cacheLabel[\s\S]*timeoutLabel[\s\S]*Node:[\s\S]*Public:[\s\S]*Cache:[\s\S]*Timeout:[\s\S]*Source manifest' 'network block card source manifest UI' 'CurrentBlockCard surfaces local-node/public/cache/timeout source capability without fake block data' 'CurrentBlockCard does not visibly surface source manifest details' $currentBlockPath
    Test-Pattern $currentBlockText 'OverlayDialog[\s\S]*aria-haspopup="dialog"[\s\S]*aria-expanded=\{modalOpen\}[\s\S]*Close block details' 'network block modal accessibility' 'CurrentBlockCard uses the shared overlay dialog and accessible open/close controls' 'CurrentBlockCard lacks visible shared-modal accessibility hooks' $currentBlockPath
    Test-Pattern $standardCssText '\.mode-standard \.current-block-card[\s\S]*#FAA500[\s\S]*\.current-block-modal[\s\S]*prefers-reduced-motion' 'network block UI kit styling' 'CurrentBlockCard has DCENT_OS orange/glass styling and reduced-motion coverage' 'CurrentBlockCard styling or reduced-motion coverage is missing' $standardCssPath
    Test-Pattern $standardCssText '\.mode-standard \.current-block-source-strip[\s\S]*\.current-block-source-pill[\s\S]*\.current-block-source-pill-info[\s\S]*\.current-block-source-pill-warning[\s\S]*\.current-block-source-pill-muted' 'network block source manifest styling' 'CurrentBlockCard source manifest pills wrap in the DCENT_OS Standard UI kit' 'CurrentBlockCard source manifest pill styling is missing' $standardCssPath
    $getNetworkBlockFn = Get-RustFunctionText $apiRestText 'get_network_block'
    if ($null -ne $getNetworkBlockFn -and $getNetworkBlockFn -match 'std::fs::write|tokio::fs::write|atomic_write|Command::new|state_tx|action_tx|enter_sleep|wake\(|post_|PsuController|sysupgrade|fw_setenv|ubiupdatevol|reqwest|\.send\(|getblockchaininfo|getblockheader|getbestblockhash|getmempoolinfo|estimatesmartfee|https://|mempool\.space|blockstream|blockchain\.info') {
        Add-Result FAIL 'network block endpoint read-only/offline' 'get_network_block appears to mutate state, run commands, or call public internet providers' $apiRestPath
    } else {
        Add-Result PASS 'network block endpoint read-only/offline' 'get_network_block is metadata-only and does not visibly call public providers or control paths' $apiRestPath
    }
    if ($null -ne $getNetworkBlockFn -and $getNetworkBlockFn -match 'local_node_rpc_password|local_node_rpc_cookie|Authorization|COOKIE|Basic|Bearer') {
        Add-Result FAIL 'network block no credential exposure' 'get_network_block appears to expose RPC credentials or authorization headers' $apiRestPath
    } else {
        Add-Result PASS 'network block no credential exposure' 'get_network_block exposes only redacted source capability metadata' $apiRestPath
    }
    if ($null -ne $currentBlockText -and $currentBlockText -match 'fetch\(|XMLHttpRequest|mempool\.space|blockstream|blockchain\.info|api\.(restart|reboot|uploadFirmware|setFan|configurePools|sleep|wake)\(|block_height:\s*\d|height:\s*\d|000000000000000000') {
        Add-Result FAIL 'network block card no fake/public data' 'CurrentBlockCard appears to fetch public data, call control APIs, or hardcode block values' $currentBlockPath
    } else {
        Add-Result PASS 'network block card no fake/public data' 'CurrentBlockCard uses only the typed read-only API and does not hardcode block values' $currentBlockPath
    }

    Test-Pattern $apiRestText '\.route\(\s*"/api/thermal/posture",\s*get\(get_thermal_posture\)' 'thermal posture backend route' 'backend mounts GET-only /api/thermal/posture' 'backend does not visibly mount /api/thermal/posture as a GET route' $apiRestPath
    if ($null -ne $apiRestText -and $apiRestText -match '\.route\(\s*"/api/thermal/posture"[\s\S]{0,120}\.(post|put|patch|delete)\(') {
        Add-Result FAIL 'thermal posture no write route' '/api/thermal/posture appears to expose a write method' $apiRestPath
    } else {
        Add-Result PASS 'thermal posture no write route' '/api/thermal/posture is not visibly mounted with write methods' $apiRestPath
    }
    Test-Pattern $apiRestText 'async\s+fn\s+get_thermal_posture[\s\S]*"schema":\s*"dcentos\.thermal\.posture\.v1"[\s\S]*"read_only":\s*true[\s\S]*"control_actions":\s*false[\s\S]*"hardware_writes":\s*false[\s\S]*"filesystem_mutation":\s*false[\s\S]*"telemetry_source"[\s\S]*"thermal"[\s\S]*"fans"[\s\S]*"power"[\s\S]*"curtailment"[\s\S]*"hardware_support"[\s\S]*"runtime_ownership"[\s\S]*"limitations"' 'thermal posture read-only contract' 'backend exposes source-labeled read-only thermal/power posture contract' 'backend thermal posture contract is missing read-only or provenance fields' $apiRestPath
    $getThermalPostureFn = Get-RustFunctionText $apiRestText 'get_thermal_posture'
    if ($null -ne $getThermalPostureFn -and $getThermalPostureFn -match 'std::fs::write|tokio::fs::write|atomic_write|Command::new|tokio::process|state_tx|action_tx|FreqCommand|VoltageCommand|set_voltage|set_speed|enable_watchdog|disable_watchdog|feed_watchdog|enter_sleep\(|wake\(|sysupgrade|fw_setenv|ubiupdatevol|PsuController|post_') {
        Add-Result FAIL 'thermal posture mutation-free' 'get_thermal_posture appears to call hardware-control, command, config-write, or destructive paths' $apiRestPath
    } else {
        Add-Result PASS 'thermal posture mutation-free' 'get_thermal_posture reads existing state without visible hardware/control writes' $apiRestPath
    }
    if ($null -ne $getThermalPostureFn -and $getThermalPostureFn -match 'mock|fake|synthetic|simulated|placeholder|demo|sampleData|Math\.random|rand::|temp_c":\s*(5[0-9]|6[0-9]|7[0-9])|"rpm":\s*[1-9][0-9]{3}|"wall_watts":\s*[1-9][0-9]{2,}') {
        Add-Result FAIL 'thermal posture no fake telemetry' 'get_thermal_posture appears to contain fabricated thermal, fan, or power values' $apiRestPath
    } else {
        Add-Result PASS 'thermal posture no fake telemetry' 'get_thermal_posture uses nullable/state-derived telemetry instead of fabricated readings' $apiRestPath
    }
    Test-Pattern $apiTypesText 'export\s+interface\s+ThermalPowerPostureResponse[\s\S]*read_only:\s*true[\s\S]*control_actions:\s*false[\s\S]*hardware_writes:\s*false[\s\S]*filesystem_mutation:\s*false[\s\S]*telemetry_source:\s*string[\s\S]*thermal:[\s\S]*fans:[\s\S]*power:[\s\S]*curtailment:[\s\S]*hardware_support:[\s\S]*runtime_ownership:[\s\S]*limitations:\s*string\[\]' 'thermal posture TS contract' 'dashboard models read-only thermal/power posture response with provenance fields' 'dashboard API types do not visibly model thermal/power posture' $apiTypesPath
    Test-Pattern $apiClientText 'getThermalPowerPosture:\s*\(\)\s*=>\s*get<ThermalPowerPostureResponse>\(''\/api\/thermal\/posture''\)' 'thermal posture API client' 'typed client fetches /api/thermal/posture' 'typed client does not expose /api/thermal/posture' $apiClientPath
    Test-Pattern $thermalPostureText 'api\.getThermalPowerPosture\(\)[\s\S]*Read-only[\s\S]*No fan/voltage/frequency/PSU writes[\s\S]*Unavailable[\s\S]*telemetry_source[\s\S]*curtailment' 'thermal posture panel honesty' 'ThermalPowerPostureCard uses typed read-only endpoint and honest unavailable/source labels' 'ThermalPowerPostureCard lacks typed fetch or honest read-only/source copy' $thermalPosturePath
    if ($null -ne $thermalPostureText -and $thermalPostureText -match 'fetch\(|XMLHttpRequest|api\.(setFan|saveProfile|setChipFrequency|restart|reboot|uploadFirmware|configurePools|sleep|wake|controlPsu|troubleshootPsu)\(|/api/debug|set_voltage|set_speed|frequency.*POST|profile apply|applyProfile') {
        Add-Result FAIL 'thermal posture panel no controls' 'ThermalPowerPostureCard appears to call raw fetch or control/debug APIs' $thermalPosturePath
    } else {
        Add-Result PASS 'thermal posture panel no controls' 'ThermalPowerPostureCard contains no raw fetch or hardware/control API calls' $thermalPosturePath
    }
    Test-Pattern $tempFansText 'ThermalPowerPostureCard[\s\S]*<ThermalPowerPostureCard\s*/>[\s\S]*Temperature Gauges' 'thermal posture temperature page placement' 'TempFansPage renders posture evidence before thermal/fan controls' 'TempFansPage does not visibly render ThermalPowerPostureCard before thermal controls' $tempFansPath
    Test-Pattern $statsGridText 'ThermalPowerPostureCard[\s\S]*<ThermalPowerPostureCard\s+variant="compact"\s*/>' 'thermal posture standard compact chip' 'StatsGrid renders compact thermal/power posture in Standard dashboard' 'StatsGrid does not visibly render compact thermal/power posture' $statsGridPath
    Test-Pattern $tempFansText 'maximum allowed fan output[\s\S]*active profile or configured ceiling' 'thermal safety copy avoids unconditional 100 percent claim' 'TempFansPage explains fan safety request is bounded by active profile/config ceiling' 'TempFansPage still implies unconditional 100 percent fan output' $tempFansPath
    Test-Pattern $standardCssText '\.mode-standard \.thermal-posture-card[\s\S]*\.thermal-posture-grid[\s\S]*\.thermal-posture-compact[\s\S]*@media \(max-width: (1024|1023\.98)px\)[\s\S]*@media \(max-width: (768|767\.98|640)px\)' 'thermal posture UI kit styling' 'ThermalPowerPostureCard has scoped Standard UI kit styling and responsive layout' 'ThermalPowerPostureCard styling or responsive layout is missing' $standardCssPath

    Test-Pattern $apiRestText '\.route\(\s*"/api/mining/work/posture",\s*get\(get_mining_work_posture\)' 'mining work posture backend route' 'backend mounts GET-only /api/mining/work/posture' 'backend does not visibly mount /api/mining/work/posture as a GET route' $apiRestPath
    if ($null -ne $apiRestText -and $apiRestText -match '\.route\(\s*"/api/mining/work/posture"[\s\S]{0,120}\.(post|put|patch|delete)\(') {
        Add-Result FAIL 'mining work posture no write route' '/api/mining/work/posture appears to expose a write method' $apiRestPath
    } else {
        Add-Result PASS 'mining work posture no write route' '/api/mining/work/posture is not visibly mounted with write methods' $apiRestPath
    }
    Test-Pattern $apiRestText 'async\s+fn\s+get_mining_work_posture[\s\S]*"schema":\s*"dcentos\.mining\.work\.posture\.v1"[\s\S]*"read_only":\s*true[\s\S]*"control_actions":\s*false[\s\S]*"hardware_writes":\s*false[\s\S]*"filesystem_mutation":\s*false[\s\S]*"telemetry_source"[\s\S]*"pool"[\s\S]*"protocol"[\s\S]*"donation"[\s\S]*"sv2"[\s\S]*"job_declaration"[\s\S]*"jobs"[\s\S]*"work"[\s\S]*"shares"[\s\S]*"limitations"' 'mining work posture read-only contract' 'backend exposes source-labeled read-only pool/job/work/share posture contract' 'backend mining work posture contract is missing read-only or provenance fields' $apiRestPath
    $getMiningWorkPostureFn = Get-RustFunctionText $apiRestText 'get_mining_work_posture'
    if ($null -ne $getMiningWorkPostureFn -and $getMiningWorkPostureFn -match 'std::fs::write|tokio::fs::write|atomic_write|Command::new|tokio::process|state_tx|action_tx|post_pools|configure_pool|switch_pool|reconnect\(|disconnect\(|enter_sleep\(|wake\(|set_voltage|set_speed|FreqCommand|VoltageCommand|PsuController|enable_watchdog|disable_watchdog|feed_watchdog|sysupgrade|fw_setenv|ubiupdatevol|reboot|restart\s*\(') {
        Add-Result FAIL 'mining work posture mutation-free' 'get_mining_work_posture appears to call pool, hardware, watchdog, command, upgrade, or filesystem mutation paths' $apiRestPath
    } else {
        Add-Result PASS 'mining work posture mutation-free' 'get_mining_work_posture composes existing state without visible control or hardware writes' $apiRestPath
    }
    if ($null -ne $getMiningWorkPostureFn -and $getMiningWorkPostureFn -match 'mock|fake|synthetic|simulated|placeholder|demo|sampleData|Math\.random|rand::|"job_id":\s*"[0-9a-f]{4,}"|"accepted_total":\s*[1-9][0-9]*|"rejected_total":\s*[1-9][0-9]*') {
        Add-Result FAIL 'mining work posture no fake telemetry' 'get_mining_work_posture appears to contain fabricated job/share values' $apiRestPath
    } else {
        Add-Result PASS 'mining work posture no fake telemetry' 'get_mining_work_posture uses nullable/state-derived telemetry and does not fabricate job/share rows' $apiRestPath
    }
    Test-Pattern $apiTypesText 'export\s+interface\s+MiningWorkPostureResponse[\s\S]*read_only:\s*true[\s\S]*control_actions:\s*false[\s\S]*hardware_writes:\s*false[\s\S]*filesystem_mutation:\s*false[\s\S]*telemetry_source:\s*string[\s\S]*pool:[\s\S]*protocol:[\s\S]*donation:[\s\S]*sv2:[\s\S]*job_declaration:[\s\S]*jobs:[\s\S]*work:[\s\S]*shares:[\s\S]*limitations:\s*string\[\]' 'mining work posture TS contract' 'dashboard models read-only mining work posture response with provenance fields' 'dashboard API types do not visibly model mining work posture' $apiTypesPath
    Test-Pattern $apiClientText 'getMiningWorkPosture:\s*\(\)\s*=>\s*get<MiningWorkPostureResponse>\(''\/api\/mining\/work\/posture''\)' 'mining work posture API client' 'typed client fetches /api/mining/work/posture' 'typed client does not expose /api/mining/work/posture' $apiClientPath
    Test-Pattern $miningWorkPostureText 'api\.getMiningWorkPosture\(\)[\s\S]*Read-only[\s\S]*No pool switching[\s\S]*Unavailable[\s\S]*telemetry_source[\s\S]*notify age: unavailable' 'mining work posture panel honesty' 'MiningWorkPostureCard uses typed read-only endpoint and honest unavailable/source labels' 'MiningWorkPostureCard lacks typed fetch or honest read-only/source copy' $miningWorkPosturePath
    if ($null -ne $miningWorkPostureText -and $miningWorkPostureText -match 'fetch\(|XMLHttpRequest|api\.(configurePools|testPoolConnection|setChipFrequency|restart|reboot|uploadFirmware|setFan|sleep|wake|controlPsu|troubleshootPsu|saveProfile)\(|/api/debug|set_voltage|set_speed|frequency.*POST|profile apply|applyProfile') {
        Add-Result FAIL 'mining work posture panel no controls' 'MiningWorkPostureCard appears to call raw fetch, debug, pool, mining, fan, voltage, reboot, or profile control APIs' $miningWorkPosturePath
    } else {
        Add-Result PASS 'mining work posture panel no controls' 'MiningWorkPostureCard contains no raw fetch or mining/hardware-control API calls' $miningWorkPosturePath
    }
    if ($null -ne $miningWorkPostureText -and $miningWorkPostureText -match 'mock|fake|synthetic|simulated|placeholder|demo|Math\.random|setShareEvents|accepted_total:\s*\d|job_id:\s*''[0-9a-f]+''') {
        Add-Result FAIL 'mining work posture panel no fake rows' 'MiningWorkPostureCard appears to fabricate job or share rows' $miningWorkPosturePath
    } else {
        Add-Result PASS 'mining work posture panel no fake rows' 'MiningWorkPostureCard renders only endpoint-provided rows and unavailable states' $miningWorkPosturePath
    }
    Test-Pattern $statsGridText 'MiningWorkPostureCard[\s\S]*<MiningWorkPostureCard\s+variant="compact"\s*/>' 'mining work posture standard compact chip' 'StatsGrid renders compact mining work posture in Standard dashboard' 'StatsGrid does not visibly render compact mining work posture' $statsGridPath
    Test-Pattern $sharesPageText 'MiningWorkPostureCard[\s\S]*<MiningWorkPostureCard\s*/>[\s\S]*Recent Share Events[\s\S]*/api/history/shares' 'mining work posture shares placement' 'SharesPage renders MiningWorkPostureCard while keeping detailed share history sourced from /api/history/shares' 'SharesPage does not visibly render MiningWorkPostureCard near share history' $sharesPagePath
    Test-Pattern $standardCssText '\.mode-standard \.mining-work-posture-card[\s\S]*#FAA500[\s\S]*\.mining-work-posture-grid[\s\S]*\.mining-work-posture-compact[\s\S]*@media \(max-width: (1024|1023\.98)px\)[\s\S]*@media \(max-width: (768|767\.98|640)px\)' 'mining work posture UI kit styling' 'MiningWorkPostureCard has scoped Standard UI kit styling and responsive layout' 'MiningWorkPostureCard styling or responsive layout is missing' $standardCssPath

    Test-Pattern $apiRestText '\.route\(\s*"/api/mining/pipeline/manifest",\s*get\(get_mining_pipeline_manifest\)' 'mining pipeline manifest backend route' 'backend mounts GET-only /api/mining/pipeline/manifest' 'backend does not visibly mount /api/mining/pipeline/manifest as a GET route' $apiRestPath
    if ($null -ne $apiRestText -and $apiRestText -match '\.route\(\s*"/api/mining/pipeline/manifest"[\s\S]{0,160}\.(post|put|patch|delete)\(') {
        Add-Result FAIL 'mining pipeline manifest no write route' '/api/mining/pipeline/manifest appears to expose a write method' $apiRestPath
    } else {
        Add-Result PASS 'mining pipeline manifest no write route' '/api/mining/pipeline/manifest is not visibly mounted with write methods' $apiRestPath
    }
    Test-Pattern $apiRestText 'build_mining_pipeline_manifest_response[\s\S]*"schema":\s*"dcentos\.mining\.pipeline\.manifest\.v1"[\s\S]*"read_only":\s*true[\s\S]*"control_actions":\s*false[\s\S]*"hardware_writes":\s*false[\s\S]*"filesystem_mutation":\s*false[\s\S]*"content_collected":\s*false[\s\S]*"probe_performed":\s*false[\s\S]*"handlers_executed":\s*false[\s\S]*"live_publisher"[\s\S]*"existing_surfaces"[\s\S]*"candidate_snapshot_fields"[\s\S]*"publisher_contract"[\s\S]*"validation_plan"[\s\S]*"limitations"' 'mining pipeline manifest read-only contract' 'backend exposes a manifest-only mining pipeline evidence contract' 'backend mining pipeline manifest contract is missing read-only or provenance fields' $apiRestPath
    Test-Pattern $apiRestText 'build_mining_pipeline_manifest_response[\s\S]*"publisher_live":\s*snapshot_receiver_configured[\s\S]*"snapshot_available":\s*false[\s\S]*"publisher_gate"[\s\S]*"app_state_field":\s*"mining_pipeline_snapshot_rx"[\s\S]*"receiver_default":\s*"None"[\s\S]*"config_toml_path":\s*"mining\.pipeline_snapshot\.enabled"[\s\S]*"config_default_enabled":\s*false[\s\S]*"enabled_configs_rejected":\s*false[\s\S]*"live_snapshot_endpoint":\s*"/api/mining/pipeline/snapshot"' 'mining pipeline manifest publisher gate' 'manifest reports the default-off receiver/config gate and read-only snapshot route without promoting live telemetry' 'manifest does not visibly declare the default-off receiver/config gate and read-only snapshot route' $apiRestPath
    Test-Pattern $apiRestText 'fn\s+mining_pipeline_snapshot_freshness_contract\(\)[\s\S]*"default_stale_after_ms":\s*crate::MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS[\s\S]*"snapshot_available_only_when":\s*"status == live"[\s\S]*"does_not_populate":[\s\S]*"current_job_id"[\s\S]*"nonce_bursts_total"[\s\S]*build_mining_pipeline_manifest_response[\s\S]*"freshness_contract":\s*mining_pipeline_snapshot_freshness_contract\(\)' 'mining pipeline manifest freshness contract' 'manifest exposes explicit freshness/stale-state contract without live pipeline data' 'manifest does not visibly expose the freshness/stale-state contract' $apiRestPath
    Test-Pattern $apiRestText 'fn\s+mining_pipeline_freshness_classifier_contract\(\)[\s\S]*"schema":\s*crate::MINING_PIPELINE_FRESHNESS_CLASSIFIER_SCHEMA[\s\S]*"status":\s*"design_only"[\s\S]*"runtime_wired":\s*false[\s\S]*"publisher_enabled":\s*false[\s\S]*"snapshot_available":\s*false[\s\S]*"live_route_mounted":\s*false[\s\S]*"outputs":[\s\S]*"future_clock_skew"[\s\S]*"invalid"[\s\S]*"fail_closed_when":[\s\S]*domain_last_update_ms is null[\s\S]*age would be negative[\s\S]*"snapshot_status_mapping":[\s\S]*"future_clock_skew":\s*"unavailable"[\s\S]*"invalid":\s*"unavailable"[\s\S]*"does_not_read":[\s\S]*"mining_sync"[\s\S]*"hardware registers"' 'mining pipeline freshness classifier contract' 'backend exposes design-only pure freshness classifier with future-skew/invalid fail-closed states' 'backend freshness classifier contract is missing fail-closed future-skew/invalid guardrails' $apiRestPath
    Test-Pattern $apiRestText 'build_mining_pipeline_manifest_response[\s\S]*"freshness_classifier_schema":\s*crate::MINING_PIPELINE_FRESHNESS_CLASSIFIER_SCHEMA[\s\S]*"freshness_classifier":\s*mining_pipeline_freshness_classifier_contract\(\)' 'mining pipeline manifest freshness classifier surface' 'manifest exposes pure freshness classifier contract without live telemetry' 'manifest does not visibly expose the freshness classifier contract' $apiRestPath
    Test-Pattern $apiRestText 'fn\s+mining_pipeline_snapshot_publisher_design_contract\(\)[\s\S]*"status":\s*"implemented_default_off"[\s\S]*"implemented":\s*true[\s\S]*"publisher_enabled":\s*false[\s\S]*"live_route_mounted":\s*true[\s\S]*"config_gate":\s*"mining\.pipeline_snapshot\.enabled"[\s\S]*"bounded_publish_cadence":[\s\S]*"max_hz":\s*1[\s\S]*"publish_per_nonce":\s*false[\s\S]*"forbidden":[\s\S]*REST handlers subscribing to mining_sync[\s\S]*per-nonce watch publications[\s\S]*"hardware_smoke_required":[\s\S]*Antminer S9[\s\S]*Antminer S19 Pro[\s\S]*Antminer S21[\s\S]*build_mining_pipeline_manifest_response[\s\S]*"publisher_design":\s*mining_pipeline_snapshot_publisher_design_contract\(\)' 'mining pipeline publisher design contract' 'manifest exposes implemented-default-off bounded publisher gate with hardware smoke requirements' 'manifest does not visibly expose the implemented-default-off bounded publisher gate' $apiRestPath
    Test-Pattern $apiRestText 'fn\s+mining_pipeline_snapshot_domain_freshness_contract[\s\S]*"last_update_ms":\s*serde_json::Value::Null[\s\S]*"age_ms":\s*serde_json::Value::Null[\s\S]*"stale_after_ms":\s*serde_json::Value::Null[\s\S]*"source":\s*serde_json::Value::Null[\s\S]*fn\s+mining_pipeline_snapshot_design_v2_contract\(\)[\s\S]*"schema":\s*"dcentos\.mining\.pipeline\.snapshot\.design\.v2"[\s\S]*"status":\s*"implemented_default_off"[\s\S]*"implemented":\s*true[\s\S]*"publisher_enabled":\s*false[\s\S]*"snapshot_available":\s*false[\s\S]*"live_route_mounted":\s*true[\s\S]*"blocks":[\s\S]*"job_freshness"[\s\S]*"work_freshness"[\s\S]*"nonce_freshness"[\s\S]*"share_freshness"[\s\S]*"forbidden":[\s\S]*REST handlers subscribing to mining_sync[\s\S]*per-nonce watch publications[\s\S]*hardware register polling from REST[\s\S]*"hardware_smoke_required":[\s\S]*Antminer S9[\s\S]*Antminer S19 Pro[\s\S]*Antminer S21' 'mining pipeline snapshot design v2 contract' 'backend exposes implemented-default-off job/work/nonce/share freshness blocks with null future fields and smoke blockers' 'backend snapshot design v2 contract is missing implemented-default-off/null/smoke guardrails' $apiRestPath
    Test-Pattern $apiRestText 'build_mining_pipeline_manifest_response[\s\S]*"snapshot_design_schema":\s*"dcentos\.mining\.pipeline\.snapshot\.design\.v2"[\s\S]*"snapshot_design":\s*mining_pipeline_snapshot_design_v2_contract\(\)' 'mining pipeline manifest snapshot design v2 surface' 'manifest exposes snapshot design v2 without promoting a live snapshot route' 'manifest does not visibly expose snapshot design v2' $apiRestPath
    Test-Pattern $apiRestText 'fn\s+mining_pipeline_publisher_promotion_checklist_contract\(\)[\s\S]*"schema":\s*"dcentos\.mining\.pipeline\.publisher\.promotion\.checklist\.v1"[\s\S]*"status":\s*"implemented_default_off"[\s\S]*"promotion_state":\s*"blocked"[\s\S]*"implemented":\s*true[\s\S]*"route_required":\s*true[\s\S]*"dispatcher_reads":\s*false[\s\S]*"hardware_reads":\s*false[\s\S]*"pool_socket_reads":\s*false[\s\S]*"publisher_enabled":\s*false[\s\S]*"snapshot_available":\s*false[\s\S]*"live_route_mounted":\s*true[\s\S]*"requirements":[\s\S]*"design_v2_fields"[\s\S]*"publisher_source_owner"[\s\S]*"bounded_cadence"[\s\S]*"forbidden_rest_reconstruction"[\s\S]*"hardware_smoke_s9"[\s\S]*"hardware_smoke_s19pro"[\s\S]*"hardware_smoke_s21"[\s\S]*"rollback_disable_path"[\s\S]*"forbidden":[\s\S]*subscribing REST handlers to mining_sync[\s\S]*publishing per nonce[\s\S]*reading hardware registers from REST' 'mining pipeline publisher promotion checklist contract' 'backend exposes implemented-default-off blocked publisher promotion checklist with forbidden paths and smoke blockers' 'backend publisher promotion checklist is missing implemented-default-off blocked guardrails' $apiRestPath
    Test-Pattern $apiRestText 'fn\s+mining_pipeline_publisher_promotion_checklist_contract\(\)[\s\S]*"blockers_schema":\s*"dcentos\.mining\.pipeline\.publisher\.promotion\.blocker\.v1"[\s\S]*"blocker_count":\s*7[\s\S]*"active_blocker_count":\s*4[\s\S]*"all_blockers_active":\s*false[\s\S]*"blockers":[\s\S]*"publisher_not_wired"[\s\S]*"active":\s*false[\s\S]*"live_route_absent"[\s\S]*"active":\s*false[\s\S]*"domain_freshness_unavailable"[\s\S]*"active":\s*false[\s\S]*"hardware_smoke_s9_not_run"[\s\S]*"active":\s*true[\s\S]*"hardware_smoke_s19pro_not_run"[\s\S]*"active":\s*true[\s\S]*"hardware_smoke_s21_not_run"[\s\S]*"active":\s*true[\s\S]*"rollback_not_tested"[\s\S]*"active":\s*true' 'mining pipeline publisher promotion blocker list' 'backend exposes route/publisher blockers cleared while hardware/rollback blockers remain active' 'backend publisher promotion blockers do not reflect the mounted default-off route contract' $apiRestPath
    Test-Pattern $apiRestText 'fn\s+mining_pipeline_publisher_promotion_checklist_contract\(\)[\s\S]*"active_blocker_ids":\s*\[[\s\S]*"hardware_smoke_s9_not_run"[\s\S]*"hardware_smoke_s19pro_not_run"[\s\S]*"hardware_smoke_s21_not_run"[\s\S]*"rollback_not_tested"[\s\S]*\][\s\S]*"blockers":[\s\S]*"publisher_not_wired"[\s\S]*"active":\s*false[\s\S]*"rollback_not_tested"[\s\S]*"active":\s*true' 'mining pipeline publisher promotion active blocker ids alias' 'backend exposes active_blocker_ids alias for the remaining hardware/rollback blockers' 'backend active_blocker_ids alias still includes cleared route/publisher blockers or is missing active blockers' $apiRestPath
    Test-Pattern $apiRestText 'fn\s+mining_pipeline_fleet_parser_notes_contract\(\)[\s\S]*"schema":\s*"dcentos\.mining\.pipeline\.fleet_parser_notes\.v1"[\s\S]*"status":\s*"schema_only"[\s\S]*"read_only":\s*true[\s\S]*"live_telemetry":\s*false[\s\S]*"telemetry_source":\s*"none"[\s\S]*"readiness_evidence":\s*false[\s\S]*"active_blocker_ids":[\s\S]*"source_path":\s*"publisher_promotion_checklist\.active_blocker_ids"[\s\S]*"source":\s*"static_manifest"[\s\S]*"mirrors":\s*"publisher_promotion_checklist\.blockers where active == true"[\s\S]*"missing_means":\s*"treat promotion_state as blocked"[\s\S]*"not_authoritative_for":[\s\S]*"blocker reason"[\s\S]*"freshness_classifier_example_fixtures":[\s\S]*"source_path":\s*"freshness_classifier\.example_fixtures"[\s\S]*"source":\s*"static_design_fixture"[\s\S]*"live_telemetry":\s*false[\s\S]*"must_not_display_as_miner_state":\s*true[\s\S]*"live_promotion_requires":[\s\S]*"S9 hardware smoke"[\s\S]*"S19 Pro hardware smoke"[\s\S]*"S21 hardware smoke"[\s\S]*"does_not_read":[\s\S]*"mining_sync"[\s\S]*"hardware registers"[\s\S]*"does_not_clear":[\s\S]*"publisher_not_wired"[\s\S]*"rollback_not_tested"' 'mining pipeline fleet parser notes contract' 'backend exposes schema-only fleet parser notes without telemetry or readiness evidence' 'backend fleet parser notes are missing static/no-telemetry parser guardrails' $apiRestPath
    Test-Pattern $apiRestText 'build_mining_pipeline_manifest_response[\s\S]*"promotion_checklist_schema":\s*"dcentos\.mining\.pipeline\.publisher\.promotion\.checklist\.v1"[\s\S]*"publisher_promotion_checklist":\s*mining_pipeline_publisher_promotion_checklist_contract\(\)' 'mining pipeline manifest promotion checklist surface' 'manifest exposes publisher promotion checklist without adding a route' 'manifest does not visibly expose publisher promotion checklist' $apiRestPath
    Test-Pattern $apiRestText 'build_mining_pipeline_manifest_response[\s\S]*"fleet_parser_notes_schema":\s*"dcentos\.mining\.pipeline\.fleet_parser_notes\.v1"[\s\S]*"fleet_parser_notes":\s*mining_pipeline_fleet_parser_notes_contract\(\)' 'mining pipeline manifest fleet parser notes surface' 'manifest exposes schema-only fleet parser notes without adding live telemetry' 'manifest does not visibly expose fleet parser notes' $apiRestPath
    Test-Pattern $apiRestText 'ApiCompatibilityRouteEntry[\s\S]*path:\s*"/api/mining/pipeline/manifest"[\s\S]*support:\s*"implemented_manifest_only"[\s\S]*mutates:\s*false[\s\S]*unsupported_fields:[\s\S]*current_job_id[\s\S]*dispatch_queue_depth' 'mining pipeline manifest compatibility registry' 'API compatibility manifest declares mining pipeline manifest as read-only metadata' 'API compatibility manifest does not visibly include /api/mining/pipeline/manifest' $apiRestPath
    $getMiningPipelineManifestFn = Get-RustFunctionText $apiRestText 'get_mining_pipeline_manifest'
    $buildMiningPipelineManifestFn = Get-RustFunctionText $apiRestText 'build_mining_pipeline_manifest_response'
    $miningPipelineManifestBackendText = "$getMiningPipelineManifestFn`n$buildMiningPipelineManifestFn"
    if ($null -ne $miningPipelineManifestBackendText -and $miningPipelineManifestBackendText -match 'std::fs::write|tokio::fs::write|OpenOptions|atomic_write|Command::new|tokio::process|state_tx|action_tx|job_tx|share_tx|subscribe\(|recv\(|post_pools|configure_pool|switch_pool|reconnect\(|disconnect\(|enter_sleep\(|wake\(|set_voltage|set_speed|FreqCommand|VoltageCommand|PsuController|enable_watchdog|disable_watchdog|feed_watchdog|sysupgrade|fw_setenv|ubiupdatevol|reboot|restart\s*\(|/dev/mem|/dev/uio|devmem|I2cBus::open|read_register|read_nonce|flush_work_rx') {
        Add-Result FAIL 'mining pipeline manifest mutation-free' 'get_mining_pipeline_manifest appears to call channels, control paths, commands, hardware reads, or filesystem writes' $apiRestPath
    } else {
        Add-Result PASS 'mining pipeline manifest mutation-free' 'get_mining_pipeline_manifest is manifest-only and does not visibly read dispatcher or hardware paths' $apiRestPath
    }
    if ($null -ne $buildMiningPipelineManifestFn -and $buildMiningPipelineManifestFn -match 'mock|fake|synthetic|simulated|placeholder|demo|sampleData|Math\.random|rand::|"current_job_id":\s*"[0-9a-f]{4,}"|"dispatch_bursts_total":\s*[1-9][0-9]*|"nonce_bursts_total":\s*[1-9][0-9]*') {
        Add-Result FAIL 'mining pipeline manifest no fabricated counters' 'build_mining_pipeline_manifest_response appears to contain fabricated job or pipeline counter values' $apiRestPath
    } else {
        Add-Result PASS 'mining pipeline manifest no fabricated counters' 'build_mining_pipeline_manifest_response leaves live pipeline counters unavailable until a publisher exists' $apiRestPath
    }
    if ($null -ne $miningPipelineManifestBackendText -and $miningPipelineManifestBackendText -match 'state\.mining_sync_tx|mining_sync_tx\.subscribe|\.mining_sync_tx\.subscribe|broadcast::Receiver|state_rx\.borrow|work_dispatcher::|WorkDispatcher|job_rx|nonce_rx|share_rx|subscribe\(|resubscribe\(|recv\(|try_recv\(') {
        Add-Result FAIL 'mining pipeline manifest no dispatcher subscriptions' 'manifest appears to subscribe/read live mining dispatcher or event channels' $apiRestPath
    } else {
        Add-Result PASS 'mining pipeline manifest no dispatcher subscriptions' 'manifest does not visibly subscribe/read live dispatcher or mining event channels' $apiRestPath
    }
    Test-Pattern $apiTypesText 'export\s+interface\s+MiningPipelineManifestResponse[\s\S]*read_only:\s*true[\s\S]*control_actions:\s*false[\s\S]*hardware_writes:\s*false[\s\S]*filesystem_mutation:\s*false[\s\S]*content_collected:\s*false[\s\S]*probe_performed:\s*false[\s\S]*handlers_executed:\s*false[\s\S]*live_publisher:[\s\S]*existing_surfaces:\s*MiningPipelineManifestSurface\[\][\s\S]*candidate_snapshot_fields:\s*MiningPipelineManifestField\[\][\s\S]*publisher_contract:[\s\S]*limitations:\s*string\[\]' 'mining pipeline manifest TS contract' 'dashboard models read-only mining pipeline manifest with unavailable publisher fields' 'dashboard API types do not visibly model mining pipeline manifest' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+MiningPipelineManifestResponse[\s\S]*publisher_gate:[\s\S]*app_state_field:\s*string[\s\S]*receiver_configured:\s*boolean[\s\S]*receiver_default:\s*string[\s\S]*config_toml_path:\s*string[\s\S]*config_default_enabled:\s*false[\s\S]*enabled_configs_rejected:\s*boolean[\s\S]*publisher_default_enabled:\s*false[\s\S]*live_snapshot_endpoint:\s*string\s*\|\s*null' 'mining pipeline manifest publisher gate TS contract' 'dashboard models the default-off receiver/config publisher gate' 'dashboard API types do not visibly model the publisher gate' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+MiningPipelineDomainFreshnessDesignBlock[\s\S]*status:\s*''unavailable''\s*\|\s*string[\s\S]*last_update_ms:\s*number\s*\|\s*null[\s\S]*age_ms:\s*number\s*\|\s*null[\s\S]*stale_after_ms:\s*number\s*\|\s*null[\s\S]*source:\s*string\s*\|\s*null[\s\S]*null_reason:\s*string[\s\S]*export\s+interface\s+MiningPipelineSnapshotDesignV2Contract[\s\S]*schema:[\s\S]*dcentos\.mining\.pipeline\.snapshot\.design\.v2[\s\S]*blocks:[\s\S]*job_freshness:\s*MiningPipelineDomainFreshnessDesignBlock[\s\S]*work_freshness:\s*MiningPipelineDomainFreshnessDesignBlock[\s\S]*nonce_freshness:\s*MiningPipelineDomainFreshnessDesignBlock[\s\S]*share_freshness:\s*MiningPipelineDomainFreshnessDesignBlock' 'mining pipeline snapshot design v2 TS contract' 'dashboard models design-only domain freshness blocks with nullable future fields' 'dashboard API types do not visibly model snapshot design v2 domain freshness blocks' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+type\s+MiningPipelineFreshnessClassifierStatus[\s\S]*''future_clock_skew''[\s\S]*''invalid''[\s\S]*export\s+interface\s+MiningPipelineFreshnessClassifierContract[\s\S]*schema:[\s\S]*dcentos\.mining\.pipeline\.freshness\.classifier\.v1[\s\S]*runtime_wired:\s*false[\s\S]*outputs:\s*MiningPipelineFreshnessClassifierStatus\[\][\s\S]*snapshot_status_mapping:\s*Record<string,\s*MiningPipelineSnapshotStatus\s*\|\s*string>[\s\S]*export\s+interface\s+MiningPipelineManifestResponse[\s\S]*freshness_classifier:\s*MiningPipelineFreshnessClassifierContract' 'mining pipeline freshness classifier TS contract' 'dashboard models design-only freshness classifier with future-skew/invalid states' 'dashboard API types do not visibly model the freshness classifier contract' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+MiningPipelineSnapshotSchemaResponse[\s\S]*snapshot_design_schema:\s*''dcentos\.mining\.pipeline\.snapshot\.design\.v2''\s*\|\s*string[\s\S]*snapshot_design:\s*MiningPipelineSnapshotDesignV2Contract[\s\S]*export\s+interface\s+MiningPipelineManifestResponse[\s\S]*snapshot_design_schema:\s*''dcentos\.mining\.pipeline\.snapshot\.design\.v2''\s*\|\s*string[\s\S]*snapshot_design:\s*MiningPipelineSnapshotDesignV2Contract' 'mining pipeline snapshot design v2 TS response surfaces' 'dashboard models snapshot design v2 on schema and manifest responses' 'dashboard response types do not visibly expose snapshot design v2' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+MiningPipelinePublisherPromotionChecklistRequirement[\s\S]*status:\s*''blocked''\s*\|\s*''not_run''\s*\|\s*''pass''\s*\|\s*''fail''\s*\|\s*string[\s\S]*export\s+interface\s+MiningPipelinePublisherPromotionChecklistContract[\s\S]*schema:[\s\S]*dcentos\.mining\.pipeline\.publisher\.promotion\.checklist\.v1[\s\S]*promotion_state:\s*''blocked''\s*\|\s*''ready''\s*\|\s*string[\s\S]*route_required:\s*boolean[\s\S]*dispatcher_reads:\s*false[\s\S]*hardware_reads:\s*false[\s\S]*pool_socket_reads:\s*false[\s\S]*requirements:\s*MiningPipelinePublisherPromotionChecklistRequirement\[\]' 'mining pipeline publisher promotion checklist TS contract' 'dashboard models blocked implemented-default-off publisher promotion checklist' 'dashboard API types do not visibly model publisher promotion checklist' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+type\s+MiningPipelinePublisherPromotionBlockerId[\s\S]*publisher_not_wired[\s\S]*live_route_absent[\s\S]*domain_freshness_unavailable[\s\S]*hardware_smoke_s9_not_run[\s\S]*hardware_smoke_s19pro_not_run[\s\S]*hardware_smoke_s21_not_run[\s\S]*rollback_not_tested[\s\S]*export\s+interface\s+MiningPipelinePublisherPromotionBlocker[\s\S]*id:\s*MiningPipelinePublisherPromotionBlockerId[\s\S]*export\s+interface\s+MiningPipelinePublisherPromotionChecklistContract[\s\S]*blockers_schema:[\s\S]*dcentos\.mining\.pipeline\.publisher\.promotion\.blocker\.v1[\s\S]*active_blocker_count:\s*number[\s\S]*all_blockers_active:\s*boolean[\s\S]*active_blocker_ids:\s*MiningPipelinePublisherPromotionBlockerId\[\][\s\S]*blockers:\s*MiningPipelinePublisherPromotionBlocker\[\]' 'mining pipeline publisher promotion blocker TS contract' 'dashboard models machine-readable active promotion blockers and active_blocker_ids alias' 'dashboard API types do not visibly model publisher promotion blockers or active_blocker_ids' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+MiningPipelineFleetParserAliasContract[\s\S]*readiness_evidence:\s*false[\s\S]*live_telemetry\?:\s*false[\s\S]*telemetry_source:[\s\S]*none[\s\S]*must_not_display_as_miner_state\?:\s*boolean[\s\S]*export\s+interface\s+MiningPipelineFleetParserNotesContract[\s\S]*schema:[\s\S]*dcentos\.mining\.pipeline\.fleet_parser_notes\.v1[\s\S]*status:\s*''schema_only''[\s\S]*read_only:\s*true[\s\S]*live_telemetry:\s*false[\s\S]*readiness_evidence:\s*false[\s\S]*static_aliases:[\s\S]*active_blocker_ids:\s*MiningPipelineFleetParserAliasContract[\s\S]*freshness_classifier_example_fixtures:\s*MiningPipelineFleetParserAliasContract[\s\S]*does_not_clear:\s*MiningPipelinePublisherPromotionBlockerId\[\]' 'mining pipeline fleet parser notes TS contract' 'dashboard models schema-only fleet parser notes as non-telemetry metadata' 'dashboard API types do not visibly model fleet parser notes' $apiTypesPath
    Test-Pattern $apiClientText 'getMiningPipelineManifest:\s*\(\)\s*=>\s*get<MiningPipelineManifestResponse>\(''\/api\/mining\/pipeline\/manifest''\)' 'mining pipeline manifest API client' 'typed client fetches /api/mining/pipeline/manifest' 'typed client does not expose /api/mining/pipeline/manifest' $apiClientPath
    Test-Pattern $miningPipelineManifestText 'api\.getMiningPipelineManifest\(\)[\s\S]*Read-only[\s\S]*Live publisher unavailable[\s\S]*No dispatcher or hardware reads[\s\S]*No readiness was inferred[\s\S]*telemetry_source' 'mining pipeline manifest panel honesty' 'MiningPipelineManifestCard uses typed read-only endpoint and honest unavailable/source labels' 'MiningPipelineManifestCard lacks typed fetch or honest read-only/source copy' $miningPipelineManifestPath
    if ($null -ne $miningPipelineManifestText -and $miningPipelineManifestText -match 'fetch\(|XMLHttpRequest|api\.(configurePools|testPoolConnection|setChipFrequency|restart|reboot|uploadFirmware|setFan|sleep|wake|controlPsu|troubleshootPsu|saveProfile)\(|/api/debug|set_voltage|set_speed|frequency.*POST|profile apply|applyProfile') {
        Add-Result FAIL 'mining pipeline manifest panel no controls' 'MiningPipelineManifestCard appears to call raw fetch, debug, pool, mining, fan, voltage, reboot, or profile control APIs' $miningPipelineManifestPath
    } else {
        Add-Result PASS 'mining pipeline manifest panel no controls' 'MiningPipelineManifestCard contains no raw fetch or mining/hardware-control API calls' $miningPipelineManifestPath
    }
    if ($null -ne $miningPipelineManifestText -and $miningPipelineManifestText -match 'mock|fake|synthetic|simulated|placeholder|demo|Math\.random|current_job_id:\s*''[0-9a-f]+''|dispatch_bursts_total:\s*\d|nonce_bursts_total:\s*\d') {
        Add-Result FAIL 'mining pipeline manifest panel no fabricated counters' 'MiningPipelineManifestCard appears to fabricate job or pipeline counter values' $miningPipelineManifestPath
    } else {
        Add-Result PASS 'mining pipeline manifest panel no fabricated counters' 'MiningPipelineManifestCard renders only endpoint-provided manifest fields and unavailable states' $miningPipelineManifestPath
    }
    Test-Pattern $statsGridText '<KitHashBoardStrip\s*/>[\s\S]*<MiningPipelineManifestCard\s+compact\s*/>' 'mining pipeline manifest overview placement' 'Standard overview renders compact mining pipeline manifest after hashboards' 'Standard overview does not visibly render compact mining pipeline manifest in the operations flow' $statsGridPath
    Test-Pattern $sharesPageText 'MiningWorkPostureCard[\s\S]*<MiningPipelineManifestCard\s*/>[\s\S]*Recent Share Events[\s\S]*/api/history/shares' 'mining pipeline manifest shares placement' 'SharesPage renders MiningPipelineManifestCard near real share history without replacing /api/history/shares' 'SharesPage does not visibly render MiningPipelineManifestCard near share history' $sharesPagePath
    Test-Pattern $standardCssText '\.mode-standard \.mining-pipeline-manifest-card[\s\S]*box-shadow:\s*none[\s\S]*\.mining-pipeline-manifest-grid[\s\S]*\.mining-pipeline-manifest-summary-grid[\s\S]*@media \(max-width: (1024|1023\.98)px\)[\s\S]*@media \(max-width: (768|767\.98|640)px\)[\s\S]*prefers-reduced-motion[\s\S]*\.mining-pipeline-manifest-card' 'mining pipeline manifest UI kit styling' 'MiningPipelineManifestCard has scoped Standard UI kit styling, responsive layout, and reduced-motion coverage without over-promoting a disabled contract' 'MiningPipelineManifestCard styling, responsive layout, or reduced-motion coverage is missing' $standardCssPath

    Test-Pattern $apiTypesRustText 'MINING_PIPELINE_SNAPSHOT_SCHEMA:\s*&str\s*=\s*"dcentos\.mining\.pipeline\.snapshot\.v1"[\s\S]*pub\s+enum\s+MiningPipelineSnapshotStatus[\s\S]*Unavailable[\s\S]*Live[\s\S]*Stale[\s\S]*pub\s+struct\s+MiningPipelineSnapshot[\s\S]*publisher_enabled:\s*bool[\s\S]*snapshot_available:\s*bool[\s\S]*current_job_id:\s*Option<String>[\s\S]*dispatch_queue_depth:\s*Option<u32>[\s\S]*impl\s+Default\s+for\s+MiningPipelineSnapshot[\s\S]*publisher_enabled:\s*false[\s\S]*snapshot_available:\s*false[\s\S]*current_job_id:\s*None[\s\S]*nonce_bursts_total:\s*None' 'mining pipeline snapshot Rust type default-off' 'dcentrald-api-types exposes a passive default-off MiningPipelineSnapshot contract' 'dcentrald-api-types does not visibly expose a default-off MiningPipelineSnapshot contract' $apiTypesRustPath
    Test-Pattern $apiTypesRustText 'MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS:\s*u64\s*=\s*5_000[\s\S]*classify_freshness[\s\S]*publisher_last_update_ms\s*>\s*generated_at_ms[\s\S]*MiningPipelineSnapshotStatus::Unavailable[\s\S]*normalize_freshness[\s\S]*snapshot_available\s*=\s*matches!\(self\.status,\s*MiningPipelineSnapshotStatus::Live\)[\s\S]*freshness_fixture[\s\S]*current_job_id\.is_none\(\)[\s\S]*mining_pipeline_snapshot_future_timestamp_fails_closed' 'mining pipeline snapshot freshness classifier' 'Rust snapshot contract classifies unavailable/live/stale and fails future timestamps closed without populating pipeline values' 'Rust snapshot contract freshness classifier or future-timestamp fail-closed test is missing' $apiTypesRustPath
    Test-Pattern $apiTypesRustText 'MINING_PIPELINE_FRESHNESS_CLASSIFIER_SCHEMA:\s*&str\s*=[\s\S]*dcentos\.mining\.pipeline\.freshness\.classifier\.v1[\s\S]*pub\s+enum\s+MiningPipelineFreshnessClassifierStatus[\s\S]*FutureClockSkew[\s\S]*Invalid[\s\S]*classify_domain_timestamp[\s\S]*stale_after_ms == 0[\s\S]*Self::Invalid[\s\S]*return Self::Unavailable[\s\S]*future_skew_ms > max_future_skew_ms[\s\S]*Self::FutureClockSkew[\s\S]*return Self::Invalid[\s\S]*as_snapshot_status[\s\S]*Self::FutureClockSkew \| Self::Invalid[\s\S]*MiningPipelineSnapshotStatus::Unavailable[\s\S]*mining_pipeline_freshness_classifier_covers_fail_closed_states' 'mining pipeline freshness classifier rich states' 'Rust pure freshness classifier models unavailable/live/stale/future_clock_skew/invalid and maps fail-closed states to unavailable snapshots' 'Rust pure freshness classifier is missing future_clock_skew/invalid fail-closed coverage' $apiTypesRustPath
    Test-Pattern $apiLibText 'REST must not reconstruct this[\s\S]*mining_sync[\s\S]*pub\s+mining_pipeline_snapshot_rx:\s*Option<watch::Receiver<MiningPipelineSnapshot>>' 'mining pipeline snapshot receiver optional' 'AppState exposes only an optional future snapshot receiver with reconstruction forbidden' 'AppState does not visibly expose the optional default-off snapshot receiver' $apiLibPath
    Test-Pattern $apiTypesRustText 'mining_pipeline_snapshot_default_is_disabled_and_unavailable[\s\S]*MiningPipelineSnapshot::unavailable[\s\S]*publisher_enabled[\s\S]*snapshot_available[\s\S]*current_job_id\.is_none\(\)[\s\S]*dispatch_queue_depth\.is_none\(\)[\s\S]*nonce_bursts_total\.is_none\(\)' 'mining pipeline snapshot Rust default test' 'Rust unit test asserts unavailable/default-off snapshot serialization contract' 'Rust unit test for default-off MiningPipelineSnapshot is missing' $apiTypesRustPath
    Test-Pattern $apiLibText 'minimal_app_state_keeps_mining_pipeline_snapshot_receiver_absent[\s\S]*build_minimal_app_state[\s\S]*mining_pipeline_snapshot_rx\.is_none\(\)' 'mining pipeline snapshot receiver default test' 'Rust unit test asserts minimal AppState keeps snapshot receiver absent' 'Rust unit test for absent AppState snapshot receiver is missing' $apiLibPath
    Test-Pattern $apiLibText 'build_minimal_app_state[\s\S]*mining_pipeline_snapshot_rx:\s*None' 'mining pipeline snapshot minimal builder absent' 'minimal AppState builder defaults snapshot receiver to None' 'minimal AppState builder does not visibly default snapshot receiver to None' $apiLibPath
    if ($null -ne $daemonText -and ([regex]::Matches($daemonText, 'let\s+mining_pipeline_snapshot_rx\s*=\s*if\s+self\.config\.mining\.pipeline_snapshot\.enabled').Count -ge 2) -and ([regex]::Matches($daemonText, 'mining_pipeline_snapshot_rx,').Count -ge 2)) {
        Add-Result PASS 'mining pipeline snapshot daemon builders absent' 'daemon AppState constructors gate snapshot receiver behind default-off config' $daemonPath
    } else {
        Add-Result FAIL 'mining pipeline snapshot daemon builders absent' 'daemon AppState constructors do not visibly gate the snapshot receiver behind default-off config' $daemonPath
    }
    if ($null -ne $serialMiningText -and $serialMiningText -match 'let\s+mining_pipeline_snapshot_rx\s*=\s*if\s+self\.config\.mining\.pipeline_snapshot\.enabled[\s\S]*mining_pipeline_snapshot_rx,') {
        Add-Result PASS 'mining pipeline snapshot serial builder absent' 'serial mining AppState constructor gates snapshot receiver behind default-off config' $serialMiningPath
    } else {
        Add-Result FAIL 'mining pipeline snapshot serial builder absent' 'serial mining AppState constructor does not visibly gate the snapshot receiver behind default-off config' $serialMiningPath
    }
    Test-Pattern $daemonConfigText 'pub\s+pipeline_snapshot:\s*MiningPipelineSnapshotConfig[\s\S]*deny_unknown_fields[\s\S]*struct\s+MiningPipelineSnapshotConfig[\s\S]*Default\s+for\s+MiningPipelineSnapshotConfig[\s\S]*stale_after_ms:\s*5_000' 'mining pipeline snapshot config default-off' 'daemon config includes default-off denied-unknown mining pipeline snapshot gate' 'daemon config snapshot gate is missing or not default-off' $daemonConfigPath
    Test-Pattern $daemonConfigText 'if\s+self\.mining\.pipeline_snapshot\.enabled[\s\S]*self\.mining\.pipeline_snapshot\.stale_after_ms == 0[\s\S]*mining\.pipeline_snapshot\.stale_after_ms must be > 0 when mining\.pipeline_snapshot\.enabled is true' 'mining pipeline snapshot config enabled guard' 'daemon validation accepts enabled publisher config only with a positive stale window' 'daemon validation does not visibly guard enabled pipeline snapshot config with positive stale window' $daemonConfigPath
    Test-Pattern $daemonConfigText 'mining_pipeline_snapshot_config_defaults_disabled[\s\S]*MiningConfig::default\(\)[\s\S]*pipeline_snapshot\.enabled[\s\S]*stale_after_ms[\s\S]*mining_pipeline_snapshot_config_false_is_accepted[\s\S]*\[mining\.pipeline_snapshot\][\s\S]*enabled = false[\s\S]*mining_pipeline_snapshot_config_enabled_is_accepted_with_positive_stale_window[\s\S]*enabled = true[\s\S]*stale_after_ms = 5000[\s\S]*mining_pipeline_snapshot_config_enabled_rejects_zero_stale_window' 'mining pipeline snapshot config tests' 'daemon config tests cover default, disabled TOML, enabled positive stale window, and zero-window rejection' 'daemon config tests for pipeline snapshot gate are missing or stale' $daemonConfigPath
    Test-Pattern $apiRestText '\.route\(\s*"/api/mining/pipeline/snapshot/schema",\s*get\(get_mining_pipeline_snapshot_schema\)' 'mining pipeline snapshot schema backend route' 'backend mounts GET-only /api/mining/pipeline/snapshot/schema' 'backend does not visibly mount /api/mining/pipeline/snapshot/schema as a GET route' $apiRestPath
    if ($null -ne $apiRestText -and $apiRestText -match '\.route\(\s*"/api/mining/pipeline/snapshot/schema"[\s\S]{0,160}\.(post|put|patch|delete)\(') {
        Add-Result FAIL 'mining pipeline snapshot schema no write route' '/api/mining/pipeline/snapshot/schema appears to expose a write method' $apiRestPath
    } else {
        Add-Result PASS 'mining pipeline snapshot schema no write route' '/api/mining/pipeline/snapshot/schema is not visibly mounted with write methods' $apiRestPath
    }
    Test-Pattern $apiRestText '\.route\(\s*"/api/mining/pipeline/snapshot",\s*get\(get_mining_pipeline_snapshot\)' 'mining pipeline live snapshot backend route' 'backend mounts GET-only /api/mining/pipeline/snapshot as a read-only default-off clone endpoint' 'backend does not visibly mount /api/mining/pipeline/snapshot as a GET route' $apiRestPath
    if ($null -ne $apiRestText -and $apiRestText -match '\.route\(\s*"/api/mining/pipeline/snapshot"[\s\S]{0,160}\.(post|put|patch|delete)\(') {
        Add-Result FAIL 'mining pipeline live snapshot no write route' '/api/mining/pipeline/snapshot appears to expose a write method' $apiRestPath
    } else {
        Add-Result PASS 'mining pipeline live snapshot no write route' '/api/mining/pipeline/snapshot is not visibly mounted with write methods' $apiRestPath
    }
    $getMiningPipelineSnapshotFn = Get-RustFunctionText $apiRestText 'get_mining_pipeline_snapshot'
    $buildMiningPipelineSnapshotFn = Get-RustFunctionText $apiRestText 'build_mining_pipeline_snapshot_response'
    $miningPipelineSnapshotBackendText = "$getMiningPipelineSnapshotFn`n$buildMiningPipelineSnapshotFn"
    if ($null -ne $miningPipelineSnapshotBackendText -and $miningPipelineSnapshotBackendText -match 'std::fs::write|tokio::fs::write|OpenOptions|atomic_write|Command::new|tokio::process|state_tx|job_tx|share_tx|post_pools|configure_pool|switch_pool|reconnect\(|disconnect\(|enter_sleep\(|wake\(|set_voltage|set_speed|FreqCommand|VoltageCommand|PsuController|enable_watchdog|disable_watchdog|feed_watchdog|sysupgrade|fw_setenv|ubiupdatevol|reboot|restart\s*\(|/dev/mem|/dev/uio|devmem|I2cBus::open|read_register|read_nonce|flush_work_rx') {
        Add-Result FAIL 'mining pipeline live snapshot mutation-free' 'get_mining_pipeline_snapshot appears to call control paths, commands, hardware reads, or filesystem writes' $apiRestPath
    } else {
        Add-Result PASS 'mining pipeline live snapshot mutation-free' 'get_mining_pipeline_snapshot clones only the optional watch snapshot and normalizes freshness' $apiRestPath
    }
    if ($null -ne $miningPipelineSnapshotBackendText -and $miningPipelineSnapshotBackendText -match '/dev/mem|/dev/uio|/dev/i2c|devmem|i2cget|i2cset|I2cBus::open|Uio|mmap|read_register|write_register|read_nonce|flush_work_rx|PsuController|FanController|set_voltage|set_speed|set_frequency') {
        Add-Result FAIL 'mining pipeline live snapshot no hardware access' 'live snapshot endpoint appears to touch hardware/HAL paths' $apiRestPath
    } else {
        Add-Result PASS 'mining pipeline live snapshot no hardware access' 'live snapshot endpoint has no visible hardware/HAL reads or writes' $apiRestPath
    }
    Test-Pattern $apiRestText 'build_mining_pipeline_snapshot_schema_response[\s\S]*"schema":\s*"dcentos\.mining\.pipeline\.snapshot\.schema\.v1"[\s\S]*"snapshot_schema":\s*crate::MINING_PIPELINE_SNAPSHOT_SCHEMA[\s\S]*"status":\s*"default_off"[\s\S]*"read_only":\s*true[\s\S]*"control_actions":\s*false[\s\S]*"hardware_writes":\s*false[\s\S]*"filesystem_mutation":\s*false[\s\S]*"content_collected":\s*false[\s\S]*"probe_performed":\s*false[\s\S]*"handlers_executed":\s*false[\s\S]*"publisher_default_enabled":\s*false[\s\S]*"live_snapshot_endpoint":\s*"/api/mining/pipeline/snapshot"[\s\S]*"default_snapshot":\s*crate::MiningPipelineSnapshot::unavailable' 'mining pipeline snapshot schema default-off contract' 'backend exposes default-off mining pipeline snapshot schema with read-only live clone route' 'backend mining pipeline snapshot schema contract is missing default-off markers or read-only route' $apiRestPath
    Test-Pattern $apiRestText 'build_mining_pipeline_snapshot_schema_response[\s\S]*"snapshot_design_schema":\s*"dcentos\.mining\.pipeline\.snapshot\.design\.v2"[\s\S]*"snapshot_design":\s*mining_pipeline_snapshot_design_v2_contract\(\)' 'mining pipeline snapshot schema design v2 surface' 'schema endpoint exposes snapshot design v2 without mounting live telemetry' 'schema endpoint does not visibly expose snapshot design v2' $apiRestPath
    Test-Pattern $apiRestText 'build_mining_pipeline_snapshot_schema_response[\s\S]*"promotion_checklist_schema":\s*"dcentos\.mining\.pipeline\.publisher\.promotion\.checklist\.v1"[\s\S]*"publisher_promotion_checklist":\s*mining_pipeline_publisher_promotion_checklist_contract\(\)' 'mining pipeline snapshot schema promotion checklist surface' 'schema endpoint exposes publisher promotion checklist without mounting live telemetry' 'schema endpoint does not visibly expose publisher promotion checklist' $apiRestPath
    Test-Pattern $apiRestText 'build_mining_pipeline_snapshot_schema_response[\s\S]*"fleet_parser_notes_schema":\s*"dcentos\.mining\.pipeline\.fleet_parser_notes\.v1"[\s\S]*"fleet_parser_notes":\s*mining_pipeline_fleet_parser_notes_contract\(\)' 'mining pipeline snapshot schema fleet parser notes surface' 'schema endpoint exposes fleet parser notes without mounting live telemetry' 'schema endpoint does not visibly expose fleet parser notes' $apiRestPath
    Test-Pattern $apiRestText 'build_mining_pipeline_snapshot_schema_response[\s\S]*"freshness_classifier_schema":\s*crate::MINING_PIPELINE_FRESHNESS_CLASSIFIER_SCHEMA[\s\S]*"freshness_classifier":\s*mining_pipeline_freshness_classifier_contract\(\)' 'mining pipeline snapshot schema freshness classifier surface' 'schema endpoint exposes pure freshness classifier contract without mounting live telemetry' 'schema endpoint does not visibly expose the freshness classifier contract' $apiRestPath
    Test-Pattern $apiRestText 'fn\s+mining_pipeline_freshness_classifier_contract\(\)[\s\S]*"example_fixtures_schema":\s*"dcentos\.mining\.pipeline\.freshness\.classifier\.fixture\.v1"[\s\S]*"example_fixture_count":\s*5[\s\S]*"example_fixtures_are_design_only":\s*true[\s\S]*"example_fixtures_live_telemetry":\s*false[\s\S]*"example_fixtures":\s*mining_pipeline_freshness_classifier_example_fixtures\(\)' 'mining pipeline freshness classifier fixture contract' 'backend exposes static design-only classifier fixtures through schema metadata' 'backend freshness classifier fixture contract is missing or not static metadata' $apiRestPath
    Test-Pattern $apiRestText 'fn\s+mining_pipeline_freshness_classifier_example_fixtures\(\)[\s\S]*"unavailable"[\s\S]*"live"[\s\S]*"stale"[\s\S]*"future_clock_skew"[\s\S]*"invalid"[\s\S]*MiningPipelineFreshnessClassifierStatus::classify_domain_timestamp[\s\S]*"design_only":\s*true[\s\S]*"non_telemetry":\s*true[\s\S]*"telemetry_source":\s*"none"[\s\S]*"dispatcher_reads":\s*false[\s\S]*"hardware_reads":\s*false[\s\S]*"pool_socket_reads":\s*false[\s\S]*"snapshot_available"' 'mining pipeline freshness classifier fixture states' 'backend classifier fixtures cover all five states without telemetry or runtime reads' 'backend classifier fixtures do not visibly cover all states with no-runtime-read flags' $apiRestPath
    Test-Pattern $apiRestText 'build_mining_pipeline_snapshot_schema_response[\s\S]*"freshness_contract":\s*mining_pipeline_snapshot_freshness_contract\(\)[\s\S]*"validation_required":[\s\S]*Freshness fixture tests for unavailable, live, and stale classifications[\s\S]*Any future live /api/mining/pipeline/snapshot route must remain gated by mining\.pipeline_snapshot\.enabled and hardware-validation docs[\s\S]*S9, S19 Pro, and S21 hardware smoke' 'mining pipeline snapshot future live gate docs' 'schema documents freshness tests plus config-gated hardware-validation requirements before live route promotion' 'schema does not visibly document future live route config gate and hardware-validation requirements' $apiRestPath
    Test-Pattern $apiRestText 'build_mining_pipeline_snapshot_schema_response[\s\S]*"config_gate"[\s\S]*"toml_path":\s*"mining\.pipeline_snapshot\.enabled"[\s\S]*"default_enabled":\s*false[\s\S]*"current_config_read":\s*false[\s\S]*"enabled_configs_rejected":\s*false[\s\S]*"live_snapshot_endpoint":\s*"/api/mining/pipeline/snapshot"' 'mining pipeline snapshot schema config gate' 'schema endpoint declares the default-off config gate and mounted read-only snapshot route without reading current config' 'schema endpoint does not visibly declare the default-off config gate and read-only snapshot route' $apiRestPath
    Test-Pattern $apiRestText 'ApiCompatibilityRouteEntry[\s\S]*path:\s*"/api/mining/pipeline/snapshot/schema"[\s\S]*support:\s*"implemented_schema_only"[\s\S]*mutates:\s*false[\s\S]*unsupported_fields:[\s\S]*current_job_id[\s\S]*nonce_bursts_total[\s\S]*local_validation_drops_total' 'mining pipeline snapshot schema compatibility registry' 'API compatibility manifest declares snapshot schema as read-only schema-only metadata' 'API compatibility manifest does not visibly include /api/mining/pipeline/snapshot/schema' $apiRestPath
    $getMiningPipelineSnapshotSchemaFn = Get-RustFunctionText $apiRestText 'get_mining_pipeline_snapshot_schema'
    $buildMiningPipelineSnapshotSchemaFn = Get-RustFunctionText $apiRestText 'build_mining_pipeline_snapshot_schema_response'
    $miningPipelineSnapshotSchemaBackendText = "$getMiningPipelineSnapshotSchemaFn`n$buildMiningPipelineSnapshotSchemaFn"
    if ($null -ne $miningPipelineSnapshotSchemaBackendText -and $miningPipelineSnapshotSchemaBackendText -match 'std::fs::write|tokio::fs::write|OpenOptions|atomic_write|Command::new|tokio::process|state_tx|state_rx|job_tx|share_tx|work_dispatcher|subscribe\(|recv\(|try_recv\(|post_pools|configure_pool|switch_pool|reconnect\(|disconnect\(|enter_sleep\(|wake\(|set_voltage|set_speed|FreqCommand|VoltageCommand|PsuController|enable_watchdog|disable_watchdog|feed_watchdog|sysupgrade|fw_setenv|ubiupdatevol|reboot|restart\s*\(|/dev/mem|/dev/uio|devmem|I2cBus::open|read_register|read_nonce|flush_work_rx') {
        Add-Result FAIL 'mining pipeline snapshot schema mutation-free' 'get_mining_pipeline_snapshot_schema appears to call channels, dispatcher, control paths, commands, hardware reads, or filesystem writes' $apiRestPath
    } else {
        Add-Result PASS 'mining pipeline snapshot schema mutation-free' 'get_mining_pipeline_snapshot_schema is schema-only and does not visibly read runtime dispatcher or hardware paths' $apiRestPath
    }
    if ($null -ne $miningPipelineSnapshotSchemaBackendText -and $miningPipelineSnapshotSchemaBackendText -match '/dev/mem|/dev/uio|/dev/i2c|devmem|i2cget|i2cset|I2cBus::open|Uio|mmap|read_register|write_register|read_nonce|flush_work_rx|PsuController|FanController|set_voltage|set_speed|set_frequency') {
        Add-Result FAIL 'mining pipeline snapshot schema no hardware access' 'schema endpoint appears to touch hardware/HAL paths' $apiRestPath
    } else {
        Add-Result PASS 'mining pipeline snapshot schema no hardware access' 'schema endpoint has no visible hardware/HAL reads or writes' $apiRestPath
    }
    if ($null -ne $buildMiningPipelineSnapshotSchemaFn -and $buildMiningPipelineSnapshotSchemaFn -match 'mock|fake|synthetic|simulated|placeholder|demo|sampleData|Math\.random|rand::|"current_job_id":\s*"[0-9a-f]{4,}"|"dispatch_bursts_total":\s*[1-9][0-9]*|"nonce_bursts_total":\s*[1-9][0-9]*|"stale_nonce_drops_total":\s*[1-9][0-9]*|"unsupported_version_drops_total":\s*[1-9][0-9]*|"local_validation_drops_total":\s*[1-9][0-9]*') {
        Add-Result FAIL 'mining pipeline snapshot schema no fabricated counters' 'build_mining_pipeline_snapshot_schema_response appears to contain fabricated job or pipeline counter values' $apiRestPath
    } else {
        Add-Result PASS 'mining pipeline snapshot schema no fabricated counters' 'build_mining_pipeline_snapshot_schema_response leaves live pipeline counters null/default-off' $apiRestPath
    }
    Test-Pattern $apiTypesText 'export\s+interface\s+MiningPipelineSnapshot[\s\S]*schema:[\s\S]*dcentos\.mining\.pipeline\.snapshot\.v1[\s\S]*publisher_enabled:\s*boolean[\s\S]*snapshot_available:\s*boolean[\s\S]*read_only:\s*true[\s\S]*control_actions:\s*false[\s\S]*hardware_writes:\s*false[\s\S]*current_job_id\??:\s*string\s*\|\s*null[\s\S]*nonce_bursts_total\??:\s*number\s*\|\s*null[\s\S]*local_validation_drops_total\??:\s*number\s*\|\s*null' 'mining pipeline snapshot TS contract' 'dashboard models nullable/default-off MiningPipelineSnapshot contract' 'dashboard API types do not visibly model MiningPipelineSnapshot' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+MiningPipelineSnapshotFreshnessContract[\s\S]*default_stale_after_ms:\s*number[\s\S]*status_unavailable_when:\s*string\[\][\s\S]*status_live_when:\s*string\[\][\s\S]*status_stale_when:\s*string\[\][\s\S]*snapshot_available_only_when:\s*string[\s\S]*does_not_populate:\s*string\[\]' 'mining pipeline snapshot freshness TS contract' 'dashboard models the snapshot freshness/stale-state contract' 'dashboard API types do not visibly model the snapshot freshness contract' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+MiningPipelineSnapshotSchemaResponse[\s\S]*freshness_classifier_schema:\s*''dcentos\.mining\.pipeline\.freshness\.classifier\.v1''\s*\|\s*string[\s\S]*freshness_classifier:\s*MiningPipelineFreshnessClassifierContract' 'mining pipeline snapshot schema freshness classifier TS surface' 'dashboard schema response models the freshness classifier contract' 'dashboard schema response does not visibly expose the freshness classifier contract' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+MiningPipelineSnapshotSchemaResponse[\s\S]*fleet_parser_notes_schema:\s*''dcentos\.mining\.pipeline\.fleet_parser_notes\.v1''\s*\|\s*string[\s\S]*fleet_parser_notes:\s*MiningPipelineFleetParserNotesContract[\s\S]*export\s+interface\s+MiningPipelineManifestResponse[\s\S]*fleet_parser_notes_schema:\s*''dcentos\.mining\.pipeline\.fleet_parser_notes\.v1''\s*\|\s*string[\s\S]*fleet_parser_notes:\s*MiningPipelineFleetParserNotesContract' 'mining pipeline fleet parser notes TS response surfaces' 'dashboard response types expose fleet parser notes on schema and manifest responses' 'dashboard response types do not visibly expose fleet parser notes' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+MiningPipelineFreshnessClassifierFixture[\s\S]*design_only:\s*true[\s\S]*non_telemetry:\s*true[\s\S]*telemetry_source:[\s\S]*none[\s\S]*dispatcher_reads:\s*false[\s\S]*hardware_reads:\s*false[\s\S]*pool_socket_reads:\s*false[\s\S]*expected_classifier_status:\s*MiningPipelineFreshnessClassifierStatus[\s\S]*expected_snapshot_status:\s*MiningPipelineSnapshotStatus[\s\S]*export\s+interface\s+MiningPipelineFreshnessClassifierContract[\s\S]*example_fixtures_schema:[\s\S]*dcentos\.mining\.pipeline\.freshness\.classifier\.fixture\.v1[\s\S]*example_fixture_count:\s*number[\s\S]*example_fixtures_live_telemetry:\s*false[\s\S]*example_fixtures:\s*MiningPipelineFreshnessClassifierFixture\[\]' 'mining pipeline freshness classifier fixture TS contract' 'dashboard models static non-telemetry freshness classifier fixtures' 'dashboard API types do not visibly model classifier fixtures as non-telemetry' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+MiningPipelinePublisherDesignContract[\s\S]*status:\s*''implemented_default_off''[\s\S]*''design_only''[\s\S]*implemented:\s*boolean[\s\S]*publisher_enabled:\s*boolean[\s\S]*live_route_mounted:\s*boolean[\s\S]*config_gate:\s*string[\s\S]*bounded_publish_cadence:[\s\S]*max_hz:\s*number[\s\S]*min_interval_ms:\s*number[\s\S]*publish_per_nonce:\s*boolean[\s\S]*hardware_smoke_required:[\s\S]*model:\s*string[\s\S]*promotion_requires:\s*string\[\]' 'mining pipeline publisher design TS contract' 'dashboard models the implemented-default-off publisher promotion gate' 'dashboard API types do not visibly model the publisher promotion gate' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+MiningPipelineSnapshotSchemaResponse[\s\S]*snapshot_design_schema:\s*''dcentos\.mining\.pipeline\.snapshot\.design\.v2''\s*\|\s*string[\s\S]*snapshot_design:\s*MiningPipelineSnapshotDesignV2Contract[\s\S]*export\s+interface\s+MiningPipelineManifestResponse[\s\S]*snapshot_design_schema:\s*''dcentos\.mining\.pipeline\.snapshot\.design\.v2''\s*\|\s*string[\s\S]*snapshot_design:\s*MiningPipelineSnapshotDesignV2Contract' 'mining pipeline snapshot design v2 TS response surfaces' 'dashboard models snapshot design v2 on schema and manifest responses' 'dashboard response types do not visibly expose snapshot design v2' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+MiningPipelineSnapshotSchemaResponse[\s\S]*schema:\s*''dcentos\.mining\.pipeline\.snapshot\.schema\.v1''[\s\S]*snapshot_schema:\s*''dcentos\.mining\.pipeline\.snapshot\.v1''[\s\S]*status:\s*''default_off''[\s\S]*publisher_default_enabled:\s*false[\s\S]*live_snapshot_endpoint:\s*string\s*\|\s*null[\s\S]*default_snapshot:\s*MiningPipelineSnapshot[\s\S]*fields:\s*MiningPipelineSnapshotSchemaField\[\]' 'mining pipeline snapshot schema TS contract' 'dashboard models default-off snapshot schema response with read-only route' 'dashboard API types do not visibly model snapshot schema response' $apiTypesPath
    Test-Pattern $apiTypesText 'export\s+interface\s+MiningPipelineSnapshotSchemaResponse[\s\S]*config_gate:[\s\S]*toml_path:\s*string[\s\S]*default_enabled:\s*false[\s\S]*current_config_read:\s*false[\s\S]*enabled_configs_rejected:\s*false[\s\S]*live_snapshot_endpoint:\s*string\s*\|\s*null' 'mining pipeline snapshot schema config gate TS contract' 'dashboard models snapshot schema config gate metadata' 'dashboard API types do not visibly model snapshot config gate metadata' $apiTypesPath
    Test-Pattern $apiClientText 'getMiningPipelineSnapshot:\s*\(\)\s*=>\s*get<MiningPipelineSnapshot>\(''\/api\/mining\/pipeline\/snapshot''\)' 'mining pipeline snapshot API client' 'typed client exposes /api/mining/pipeline/snapshot' 'typed client does not expose /api/mining/pipeline/snapshot' $apiClientPath
    Test-Pattern $apiClientText 'getMiningPipelineSnapshotSchema:\s*\(\)\s*=>\s*get<MiningPipelineSnapshotSchemaResponse>\(''\/api\/mining\/pipeline\/snapshot\/schema''\)' 'mining pipeline snapshot schema API client' 'typed client exposes /api/mining/pipeline/snapshot/schema' 'typed client does not expose /api/mining/pipeline/snapshot/schema' $apiClientPath
    Test-Pattern $miningPipelineManifestText 'data-contract-schema[\s\S]*data-contract-read-only[\s\S]*data-contract-control-actions[\s\S]*data-contract-hardware-writes[\s\S]*data-contract-content-collected[\s\S]*data-contract-probe-performed[\s\S]*data-contract-handlers-executed[\s\S]*data-contract-snapshot-available' 'mining pipeline manifest contract data markers' 'MiningPipelineManifestCard exposes machine-checkable read-only/default-off contract markers' 'MiningPipelineManifestCard lacks machine-checkable contract markers' $miningPipelineManifestPath
    Test-Pattern $miningPipelineManifestText 'data-contract-snapshot-schema[\s\S]*data-contract-publisher-default-enabled[\s\S]*data-contract-publisher-enabled[\s\S]*data-contract-publisher-receiver-configured[\s\S]*data-contract-live-snapshot-endpoint[\s\S]*data-contract-live-telemetry="false"' 'mining pipeline manifest publisher gate data markers' 'MiningPipelineManifestCard exposes machine-checkable publisher/live-endpoint disabled markers' 'MiningPipelineManifestCard lacks publisher/live-endpoint disabled markers' $miningPipelineManifestPath
    Test-Pattern $miningPipelineManifestText 'data-contract-snapshot-status[\s\S]*data-contract-default-stale-after-ms[\s\S]*data-contract-publisher-last-update-ms[\s\S]*data-contract-snapshot-age-ms[\s\S]*data-contract-snapshot-available-only-when[\s\S]*data-contract-does-not-populate' 'mining pipeline manifest freshness data markers' 'MiningPipelineManifestCard exposes machine-checkable freshness and stale-state markers' 'MiningPipelineManifestCard lacks freshness/stale-state data markers' $miningPipelineManifestPath
    Test-Pattern $miningPipelineManifestText 'data-contract-freshness-classifier-schema[\s\S]*data-contract-freshness-classifier-status[\s\S]*data-contract-freshness-classifier-runtime-wired=\{String\(freshnessClassifier\?\.runtime_wired \?\? false\)\}[\s\S]*data-contract-freshness-classifier-live-telemetry="false"[\s\S]*data-contract-freshness-classifier-outputs[\s\S]*data-contract-freshness-classifier-future-clock-skew-maps-to[\s\S]*data-contract-freshness-classifier-invalid-maps-to[\s\S]*Freshness Classifier Contract[\s\S]*Design-only pure classifier[\s\S]*create live telemetry' 'mining pipeline freshness classifier UI markers' 'MiningPipelineManifestCard exposes design-only freshness classifier markers without implying live telemetry' 'MiningPipelineManifestCard lacks design-only freshness classifier markers or fail-closed copy' $miningPipelineManifestPath
    Test-Pattern $miningPipelineManifestText 'data-contract-freshness-classifier-fixture=[\s\S]*data-contract-freshness-classifier-fixture-design-only[\s\S]*data-contract-freshness-classifier-fixture-non-telemetry[\s\S]*data-contract-freshness-classifier-fixture-dispatcher-reads[\s\S]*data-contract-freshness-classifier-fixtures-loaded[\s\S]*data-contract-freshness-classifier-fixtures-schema[\s\S]*data-contract-freshness-classifier-fixture-count[\s\S]*data-contract-freshness-classifier-fixtures-non-telemetry="true"[\s\S]*data-contract-freshness-classifier-fixtures-live-telemetry="false"[\s\S]*Freshness Classifier Example Fixtures[\s\S]*Static design-only examples[\s\S]*not telemetry[\s\S]*not sourced from[\s\S]*dispatcher' 'mining pipeline freshness classifier fixture UI markers' 'MiningPipelineManifestCard exposes static non-telemetry fixture markers without implying live data' 'MiningPipelineManifestCard lacks classifier fixture markers or non-telemetry copy' $miningPipelineManifestPath
    Test-Pattern $miningPipelineManifestText 'data-contract-publisher-design-status[\s\S]*data-contract-publisher-design-implemented[\s\S]*data-contract-publisher-design-live-route-mounted[\s\S]*data-contract-publisher-design-max-hz[\s\S]*data-contract-publisher-design-publish-per-nonce[\s\S]*data-contract-publisher-design-promotion-requires[\s\S]*Publisher Promotion Gate[\s\S]*no per-nonce publication[\s\S]*no REST reconstruction from mining_sync[\s\S]*S9/S19 Pro/S21 smoke' 'mining pipeline publisher design UI markers' 'MiningPipelineManifestCard exposes machine-checkable design-only publisher promotion blockers' 'MiningPipelineManifestCard lacks publisher promotion gate markers or blocker copy' $miningPipelineManifestPath
    Test-Pattern $miningPipelineManifestText 'data-contract-snapshot-design-v2-status[\s\S]*data-contract-snapshot-design-v2-implemented[\s\S]*data-contract-snapshot-design-v2-live-route-mounted[\s\S]*data-contract-domain-freshness-status[\s\S]*data-contract-job-freshness-status[\s\S]*data-contract-nonce-freshness-status[\s\S]*data-contract-share-freshness-status[\s\S]*data-contract-work-freshness-status[\s\S]*Snapshot Design v2 Domain Freshness[\s\S]*unavailable/null state only[\s\S]*does not infer freshness from pool status' 'mining pipeline snapshot design v2 UI markers' 'MiningPipelineManifestCard exposes design-only domain freshness markers without implying live telemetry' 'MiningPipelineManifestCard lacks snapshot design v2 domain freshness markers or honesty copy' $miningPipelineManifestPath
    Test-Pattern $miningPipelineManifestText 'data-contract-publisher-promotion-checklist[\s\S]*data-contract-publisher-promotion-state[\s\S]*data-contract-publisher-promotion-ready="false"[\s\S]*data-contract-publisher-promotion-blocker-count[\s\S]*data-contract-publisher-hardware-smoke-status[\s\S]*data-contract-publisher-promotion-route-required[\s\S]*data-contract-publisher-promotion-dispatcher-reads[\s\S]*data-contract-publisher-promotion-hardware-reads[\s\S]*data-contract-publisher-promotion-pool-socket-reads[\s\S]*Publisher Promotion Checklist[\s\S]*Read-only design checklist[\s\S]*Promotion readiness remains false' 'mining pipeline publisher promotion checklist UI markers' 'MiningPipelineManifestCard exposes blocked design-only promotion checklist markers' 'MiningPipelineManifestCard lacks publisher promotion checklist markers or readiness-false copy' $miningPipelineManifestPath
    Test-Pattern $miningPipelineManifestText 'data-contract-publisher-promotion-blocker=[\s\S]*data-contract-publisher-promotion-blocker-active[\s\S]*data-contract-publisher-promotion-blockers-loaded[\s\S]*data-contract-publisher-promotion-blockers[\s\S]*data-contract-publisher-promotion-active-blocker-count[\s\S]*data-contract-publisher-promotion-all-blockers-active[\s\S]*Publisher Promotion Blockers[\s\S]*Static promotion blockers from the manifest contract[\s\S]*does not read dispatcher' 'mining pipeline publisher promotion blocker UI markers' 'MiningPipelineManifestCard exposes static active blocker markers without implying telemetry' 'MiningPipelineManifestCard lacks static promotion blocker markers or no-telemetry copy' $miningPipelineManifestPath
    Test-Pattern $miningPipelineManifestText 'data-contract-publisher-promotion-active-blocker-ids[\s\S]*data-contract-publisher-promotion-active-blocker-ids-loaded[\s\S]*data-contract-publisher-promotion-active-blocker-ids-source="static_manifest"[\s\S]*data-contract-publisher-promotion-active-blocker-ids-readiness-evidence="false"[\s\S]*Active Blocker IDs[\s\S]*Static alias for fleet tooling[\s\S]*does not read[\s\S]*not[\s\S]*readiness evidence[\s\S]*data-contract-publisher-promotion-active-blocker-id=' 'mining pipeline active blocker ids UI markers' 'MiningPipelineManifestCard exposes static active_blocker_ids markers without implying telemetry' 'MiningPipelineManifestCard lacks active_blocker_ids markers or no-telemetry copy' $miningPipelineManifestPath
    Test-Pattern $miningPipelineManifestText 'data-contract-fleet-parser-notes-schema[\s\S]*data-contract-fleet-parser-notes-read-only[\s\S]*data-contract-fleet-parser-notes-live-telemetry[\s\S]*data-contract-fleet-parser-notes-readiness-evidence[\s\S]*data-contract-fleet-parser-active-blocker-source[\s\S]*data-contract-fleet-parser-fixtures-source[\s\S]*data-contract-fleet-parser-fixtures-miner-state[\s\S]*Fleet Parser Notes[\s\S]*Schema-only parser hints[\s\S]*not live telemetry[\s\S]*not miner state[\s\S]*Detailed[\s\S]*blockers remain authoritative' 'mining pipeline fleet parser notes UI markers' 'MiningPipelineManifestCard exposes fleet parser hints without implying telemetry or readiness' 'MiningPipelineManifestCard lacks fleet parser notes markers or honesty copy' $miningPipelineManifestPath
    Test-Pattern $miningPipelineManifestText 'data-contract-publisher-hardware-smoke-plan="docs_only"[\s\S]*data-contract-publisher-hardware-smoke-plan-read-only="true"[\s\S]*data-contract-publisher-hardware-smoke-plan-readiness-evidence="false"[\s\S]*data-contract-publisher-hardware-smoke-plan-live-route-mounted="true"[\s\S]*Publisher Hardware Smoke Plan[\s\S]*Docs-only validation plan[\s\S]*not readiness evidence[\s\S]*does[\s\S]*not enable the default-off publisher[\s\S]*data-contract-publisher-hardware-smoke-plan-model=\{model\.id\}[\s\S]*docs only' 'mining pipeline hardware smoke plan UI markers' 'MiningPipelineManifestCard surfaces the docs-only S9/S19 Pro/S21 smoke plan without implying readiness' 'MiningPipelineManifestCard lacks docs-only hardware smoke plan markers or honesty copy' $miningPipelineManifestPath
    Test-Pattern $pipelineSmokePlanText 'read-only plan[\s\S]*not a test result[\s\S]*not readiness evidence[\s\S]*Global Preconditions[\s\S]*bounded latest-value snapshots at or below 1 Hz[\s\S]*Forbidden Telemetry Shortcuts[\s\S]*mining_sync[\s\S]*dispatcher internals[\s\S]*pool sockets[\s\S]*hardware registers[\s\S]*State Checks[\s\S]*Unavailable[\s\S]*Live[\s\S]*Stale[\s\S]*Fail-closed[\s\S]*Antminer S9 Smoke[\s\S]*Antminer S19 Pro Smoke[\s\S]*Antminer S21 Smoke[\s\S]*Required Artifacts[\s\S]*Promotion remains blocked' 'mining pipeline publisher smoke plan docs' 'Loop 30 docs define read-only S9/S19 Pro/S21 publisher smoke plan with forbidden shortcuts and state checks' 'Loop 30 smoke plan docs are missing required models, state checks, or safety boundaries' $pipelineSmokePlanPath
    Test-Pattern $pipelineRollbackProofText 'Rollback Proof Plan[\s\S]*not firmware rollback[\s\S]*disable `mining\.pipeline_snapshot\.enabled`[\s\S]*keep `/api/mining/pipeline/snapshot` mounted as a read-only unavailable/default-off endpoint[\s\S]*Baseline Proof[\s\S]*Enable-Attempt Proof[\s\S]*Disable-Path Proof[\s\S]*Route Proof[\s\S]*Recovery Proof[\s\S]*No Buildroot[\s\S]*No sysupgrade' 'mining pipeline publisher rollback proof docs' 'Loop 30 docs separate publisher disable-path proof from firmware rollback and keep route mounted read-only/default-off' 'Loop 30 rollback proof docs are missing disable-path or route default-off requirements' $pipelineRollbackProofPath
    Test-Pattern $pipelineEvidenceTemplateText 'Evidence Template[\s\S]*model[\s\S]*Antminer S9[\s\S]*Antminer S19 Pro[\s\S]*Antminer S21[\s\S]*git_commit[\s\S]*artifact_sha256[\s\S]*baseline[\s\S]*candidate_window[\s\S]*freshness_transitions[\s\S]*rollback_proof[\s\S]*forbidden_shortcut_attestation[\s\S]*final_verdict' 'mining pipeline publisher smoke evidence template' 'Loop 30 docs provide exact future smoke evidence fields' 'Loop 30 evidence template is missing model/build/baseline/candidate/rollback/verdict fields' $pipelineEvidenceTemplatePath
    if ($pipelineSmokePlanText -match '```') {
        Add-Result FAIL 'mining pipeline smoke plan no command fences' 'hardware smoke plan must stay an evidence checklist and must not include copy-pastable command blocks before promotion evidence exists' $pipelineSmokePlanPath
    } else {
        Add-Result PASS 'mining pipeline smoke plan no command fences' 'hardware smoke plan stays docs-only with no fenced command blocks' $pipelineSmokePlanPath
    }
    if ($pipelineRollbackProofText -match '```') {
        Add-Result FAIL 'mining pipeline rollback proof no command fences' 'publisher rollback proof must stay descriptive and must not include copy-pastable command blocks before hardware validation exists' $pipelineRollbackProofPath
    } else {
        Add-Result PASS 'mining pipeline rollback proof no command fences' 'publisher rollback proof stays descriptive with no fenced command blocks' $pipelineRollbackProofPath
    }
    Test-NoPromptStyleCommandLines $pipelineSmokePlanText $pipelineSmokePlanPath 'mining pipeline smoke plan no prompt commands' 'hardware smoke plan contains no prompt-style executable command lines' 'hardware smoke plan must not include prompt-style executable command lines before supervised hardware evidence exists'
    Test-NoPromptStyleCommandLines $pipelineRollbackProofText $pipelineRollbackProofPath 'mining pipeline rollback proof no prompt commands' 'publisher rollback proof contains no prompt-style executable command lines' 'publisher rollback proof must not include prompt-style executable command lines before supervised hardware validation exists'
    Test-Pattern $miningPipelineManifestText 'Read-only default-off snapshot contract[\s\S]*Snapshot unavailable[\s\S]*Schema declared[\s\S]*Declared Existing Surfaces[\s\S]*Proposed Snapshot Contract[\s\S]*requires publisher[\s\S]*snapshot_available' 'mining pipeline manifest default-off UI copy' 'MiningPipelineManifestCard presents the default-off snapshot contract without implying live readiness' 'MiningPipelineManifestCard still appears to promote the default-off snapshot as live readiness' $miningPipelineManifestPath

    Test-Pattern $apiRestText '\.route\(\s*"/api/autotuner/visibility",\s*get\(get_autotuner_visibility\)' 'autotuner visibility backend route' 'backend mounts read-only /api/autotuner/visibility' 'backend does not visibly mount /api/autotuner/visibility' $apiRestPath
    Test-Pattern $apiRestText 'async\s+fn\s+get_autotuner_visibility[\s\S]*"schema":\s*"dcentos\.autotuner\.visibility\.v1"[\s\S]*"read_only":\s*true[\s\S]*"control_actions":\s*false[\s\S]*"hardware_writes":\s*false[\s\S]*"filesystem_mutation":\s*false[\s\S]*"simulation"[\s\S]*"available":\s*false[\s\S]*"limitations"' 'autotuner visibility read-only contract' 'backend exposes read-only autotuner evidence, rollback, telemetry, and simulator-unavailable contract' 'backend autotuner visibility contract is missing read-only or unavailable-state markers' $apiRestPath
    $getAutotunerVisibilityFn = Get-RustFunctionText $apiRestText 'get_autotuner_visibility'
    if ($null -ne $getAutotunerVisibilityFn -and $getAutotunerVisibilityFn -match 'std::fs::write|tokio::fs::write|atomic_write|rename|remove_file|remove_dir|create_dir_all|Command::new|state_tx|action_tx|freq_cmd|FreqCommand|apply_profile|save_profile|save_backup|set_voltage|set_speed|enter_sleep|wake\(|sysupgrade|reboot|restart\s*\(') {
        Add-Result FAIL 'autotuner visibility mutation-free' 'get_autotuner_visibility appears to mutate profile files, runtime control, or hardware state' $apiRestPath
    } else {
        Add-Result PASS 'autotuner visibility mutation-free' 'get_autotuner_visibility is read-only and does not visibly call tuning or hardware-control paths' $apiRestPath
    }
    Test-Pattern $apiTypesText 'export\s+interface\s+AutotunerVisibilityResponse[\s\S]*read_only:\s*boolean[\s\S]*control_actions:\s*false[\s\S]*hardware_writes:\s*false[\s\S]*filesystem_mutation:\s*false[\s\S]*saved_profiles:[\s\S]*telemetry:[\s\S]*rollback:[\s\S]*simulation:[\s\S]*limitations:\s*string\[\]' 'autotuner visibility TS contract' 'dashboard models read-only autotuner visibility response' 'dashboard API types do not visibly model autotuner visibility' $apiTypesPath
    Test-Pattern $apiClientText 'getAutotunerVisibility:\s*\(\)\s*=>\s*get<AutotunerVisibilityResponse>\(''\/api\/autotuner\/visibility''\)' 'autotuner visibility API client' 'typed client fetches /api/autotuner/visibility' 'typed client does not expose /api/autotuner/visibility' $apiClientPath
    Test-Pattern $standardDashboardText 'AutotunerEvidencePanel[\s\S]*<AutotunerEvidencePanel\s*/>[\s\S]*<TuningProfiles\s*/>' 'autotuner visibility dashboard placement' 'StandardDashboard renders AutotunerEvidencePanel before tuning controls' 'StandardDashboard does not visibly render AutotunerEvidencePanel before TuningProfiles' $standardDashboardPath
    Test-Pattern $autotunerEvidenceText 'api\.getAutotunerVisibility\(\)[\s\S]*Read-only[\s\S]*No tuning control applied[\s\S]*Unavailable[\s\S]*profile_backup_disk[\s\S]*not_implemented' 'autotuner visibility panel honesty' 'AutotunerEvidencePanel uses typed read-only endpoint and honest unavailable/source labels' 'AutotunerEvidencePanel lacks typed fetch or honest read-only/unavailable/source copy' $autotunerEvidencePath
    if ($null -ne $autotunerEvidenceText -and $autotunerEvidenceText -match 'fetch\(|XMLHttpRequest|api\.(saveProfile|setChipFrequency|restart|reboot|uploadFirmware|setFan|configurePools|sleep|wake)\(|/api/debug|voltage|set_voltage|frequency.*POST|profile apply|applyProfile') {
        Add-Result FAIL 'autotuner visibility panel no controls' 'AutotunerEvidencePanel appears to call raw fetch, debug, tuning, fan, voltage, reboot, or pool-control APIs' $autotunerEvidencePath
    } else {
        Add-Result PASS 'autotuner visibility panel no controls' 'AutotunerEvidencePanel contains no raw fetch or tuning/hardware-control calls' $autotunerEvidencePath
    }
    Test-Pattern $standardCssText '\.mode-standard \.autotuner-evidence-panel[\s\S]*#FAA500[\s\S]*\.autotuner-evidence-grid[\s\S]*@media \(max-width: (1024|1023\.98)px\)[\s\S]*@media \(max-width: (768|767\.98|640)px\)' 'autotuner visibility UI kit styling' 'AutotunerEvidencePanel has scoped Standard UI kit styling and responsive layout' 'AutotunerEvidencePanel styling or responsive layout is missing' $standardCssPath

    Test-Pattern $sharesPageText 'api\.getShareHistory\(\)[\s\S]*Recent Share Events[\s\S]*/api/history/shares[\s\S]*event\.timestamp_ms[\s\S]*event\.result[\s\S]*event\.job_id[\s\S]*event\.difficulty' 'shares page real history fetch' 'SharesPage renders real recent share events from /api/history/shares' 'SharesPage does not visibly render real share history events' $sharesPagePath
    Test-Pattern $sharesPageText 'event\.error_code[\s\S]*event\.error_msg[\s\S]*event\.worker_name[\s\S]*event\.nonce[\s\S]*event\.ntime[\s\S]*event\.version_bits' 'shares page share detail fields' 'SharesPage surfaces optional real share diagnostics without synthetic rows' 'SharesPage does not visibly surface real optional share diagnostics' $sharesPagePath
    Test-Pattern $sharesPageText 'No recent share events reported by /api/history/shares[\s\S]*will not infer per-share rows' 'shares page honest empty state' 'SharesPage explains empty share history without inventing rows' 'SharesPage lacks honest empty state for real share history' $sharesPagePath
    if ($null -ne $sharesPageText -and $sharesPageText -match 'function\s+ShareTimeline\(\)[\s\S]*hashrateHistory|Hashrate History|Hashrate \(GH/s\)|Share submission rate correlates with hashrate|Per-share tracking requires') {
        Add-Result FAIL 'shares page no hashrate timeline fallback' 'SharesPage still presents hashrate as share timeline/history' $sharesPagePath
    } else {
        Add-Result PASS 'shares page no hashrate timeline fallback' 'SharesPage does not use hashrate history as a share timeline' $sharesPagePath
    }
    if ($null -ne $sharesPageText -and $sharesPageText -match '(?i)synthetic|simulated|fabricated|mock|fake|estimated share|best difficulty encountered|bestDiff|poolDifficulty[\s\S]{0,120}Best Difficulty|accepted[\s\S]{0,120}setShareEvents|rejected[\s\S]{0,120}setShareEvents|hashrateHistory[\s\S]{0,120}setShareEvents') {
        Add-Result FAIL 'shares page no synthetic share analytics' 'SharesPage still contains synthetic/simulated share analytics or status-derived share rows' $sharesPagePath
    } else {
        Add-Result PASS 'shares page no synthetic share analytics' 'SharesPage avoids synthetic share timelines and difficulty claims' $sharesPagePath
    }

    Test-Pattern $apiTypesText 'export\s+interface\s+PoolInfo[\s\S]*telemetry_source\??:\s*string[\s\S]*health_limitations\??:\s*string\[\][\s\S]*no_notify_age_s\??:\s*number\s*\|\s*null[\s\S]*failover_policy\??:\s*string[\s\S]*auto_fallback_reason\??:\s*string\s*\|\s*null' 'pool health provenance type contract' 'dashboard models read-only pool-health provenance and limitations' 'PoolInfo is missing read-only pool-health provenance fields' $apiTypesPath
    Test-Pattern $apiRestText 'async\s+fn\s+get_pools[\s\S]*"telemetry_source"[\s\S]*"health_limitations"[\s\S]*"no_notify_age_s"[\s\S]*"failover_policy":\s*"observability_only"[\s\S]*"auto_fallback_reason"' 'pool health provenance backend contract' 'GET /api/pools returns read-only pool health provenance and fallback context' 'GET /api/pools does not visibly return pool-health provenance fields' $apiRestPath
    Test-Pattern $apiRestText 'last_share_s is accepted-share age, not mining\.notify age[\s\S]*no_notify_age_s is unavailable[\s\S]*GET /api/pools is read-only and does not switch pools or trigger failover' 'pool health limitation honesty' 'GET /api/pools documents last-share/no-notify/failover limitations' 'GET /api/pools lacks honest limitations for pool-health fields' $apiRestPath
    Test-Pattern $sharesPageText 'Recent Share Events[\s\S]*/api/history/shares' 'share history remains separate from pool health' 'share history remains sourced from /api/history/shares' 'share history source marker missing after pool-health changes' $sharesPagePath
    $poolConfigPath = 'dashboard\src\components\standard\PoolConfig.tsx'
    $poolConfigText = Get-RepoText $poolConfigPath
    Test-Pattern $poolConfigText 'Read-only pool health from /api/pools[\s\S]*does not switch pools or trigger failover[\s\S]*No mining\.notify age reported[\s\S]*No automatic fallback active[\s\S]*observability_only' 'pool config read-only health surface' 'PoolConfig surfaces pool-health provenance without implying failover control' 'PoolConfig does not visibly surface read-only pool-health provenance' $poolConfigPath
    Test-Pattern $poolConfigText 'p\.health_limitations[\s\S]*slice\(0,\s*2\)[\s\S]*limitation' 'pool config health limitations render' 'PoolConfig renders backend health limitations separately from live counters' 'PoolConfig does not render backend pool-health limitations' $poolConfigPath
    $getPoolsFn = Get-RustFunctionText $apiRestText 'get_pools'
    if ($null -ne $getPoolsFn -and $getPoolsFn -match 'std::fs::write|tokio::fs::write|OpenOptions|write_pool_to_table|post_pools|Command::new|state_tx|action_tx|enter_sleep|wake\(') {
        Add-Result FAIL 'pool health get no mutation calls' 'get_pools appears to write config, execute commands, or send control actions' $apiRestPath
    } else {
        Add-Result PASS 'pool health get no mutation calls' 'get_pools remains read-only and does not visibly mutate pool/miner state' $apiRestPath
    }

    if ($null -ne $maintenanceText -and $maintenanceText -match 'Factory Reset|factory reset|Reset & Reboot') {
        Add-Result FAIL 'maintenance reboot wording' 'MaintenanceMode still labels reboot as factory reset' $maintenancePath
    } else {
        Add-Result PASS 'maintenance reboot wording' 'MaintenanceMode labels the action as reboot and does not promise a reset' $maintenancePath
    }

    $dashboardHonestyChecks = @(
        @{ Path = 'dashboard\src\components\basic\SatsCounter.tsx'; Bad = 'sats earned today'; Good = 'sats reported today'; Check = 'sats counter reported wording' },
        @{ Path = 'dashboard\src\components\basic\HeaterStatus.tsx'; Bad = 'BTC Earned'; Good = 'BTC Estimate|BTC Reported'; Check = 'heater BTC estimate wording' },
        @{ Path = 'dashboard\src\components\standard\EarningsPage.tsx'; Bad = 'label="Revenue"|label="Net Profit"'; Good = 'Estimated Revenue[\s\S]*Estimated Net'; Check = 'earnings estimate labels' },
        @{ Path = 'dashboard\src\components\standard\TempFansPage.tsx'; Bad = 'SoC Die Temp'; Good = 'Average Chain Temp'; Check = 'temperature label honesty' },
        @{ Path = 'dashboard\src\components\standard\KitStatsKpiGrid.tsx'; Bad = '5s \{f1m\.value\}'; Good = 'label="Hashrate 1m"[\s\S]*f1m\.value'; Check = 'hashrate window label honesty' },
        @{ Path = 'dashboard\src\components\standard\ChipHeatMap.tsx'; Bad = 'const\s+expectedChips\s*=\s*63'; Good = 'Estimated Chip Map[\s\S]*chip-count match[\s\S]*Estimated Details'; Check = 'chip map estimate labeling' }
    )

    foreach ($item in $dashboardHonestyChecks) {
        $text = Get-RepoText $item.Path
        if ($null -ne $text -and $text -match $item.Bad) {
            Add-Result FAIL $item.Check 'dashboard still contains misleading production wording or hardcoded estimate' $item.Path
        } else {
            Add-Result PASS $item.Check 'dashboard wording/provenance avoids unsupported production claims' $item.Path
        }
        Test-Pattern $text $item.Good ("{0} positive marker" -f $item.Check) 'dashboard contains the expected honest wording/provenance marker' 'dashboard is missing expected honest wording/provenance marker' $item.Path
    }
}

function Test-PublicS19ProClaimHonesty {
    Write-Output ''
    Write-Output '=== Public S19 Pro Claim Honesty ==='

    $faqPath = 'docs\FAQ.md'
    $platformsPath = 'docs\PLATFORMS.md'
    $customFirmwarePath = 'docs\CUSTOM_FIRMWARE.md'
    $publicBetaPath = 'docs\PUBLIC_BETA_READINESS_REPORT.md'
    $competitivePath = 'docs\COMPETITIVE_FEATURE_MATRIX.md'
    $readmePath = 'README.md'

    $publicDocs = @(
        $faqPath,
        $platformsPath,
        $customFirmwarePath,
        $publicBetaPath,
        $competitivePath,
        $readmePath
    )

    $customerLanguagePattern = '(?i)\buntested\b|\bunvalidated\b|\bunproven\b|needs live validation|not yet validated|no live proof|\bscaffold(?:ed|ing)?\b|\bstubs?\b|\bbroken\b|\bincomplete\b|not implemented|not wired|not available yet|not exposed by|\bno API\b'
    $languageFindings = New-Object System.Collections.Generic.List[string]
    foreach ($docPath in $publicDocs) {
        $text = Get-RepoText $docPath
        if ($null -eq $text) {
            continue
        }

        $lineNumber = 0
        foreach ($line in ($text -split '\r?\n')) {
            $lineNumber++
            if ($line -match $customerLanguagePattern) {
                $languageFindings.Add(('{0}: line {1}: {2}' -f $docPath, $lineNumber, (($line -replace '\s+', ' ').Trim())))
            }
        }
    }

    if ($languageFindings.Count -gt 0) {
        Add-Result FAIL 'public customer language standard' ("public docs contain dev-negative customer wording. Examples: {0}" -f (($languageFindings | Select-Object -First 3) -join ' | ')) 'docs'
    } else {
        Add-Result PASS 'public customer language standard' 'public docs use Experimental/In development/proof-pending wording instead of dev-negative terms' 'docs'
    }

    $boardReadmeRoot = Join-RepoPath 'br2_external_dcentos\board'
    $boardReadmeFindings = New-Object System.Collections.Generic.List[string]
    if (Test-Path -LiteralPath $boardReadmeRoot) {
        foreach ($readme in (Get-ChildItem -LiteralPath $boardReadmeRoot -Recurse -Filter README.md -File)) {
            $relativePath = $readme.FullName.Substring($repoRoot.Length + 1)
            $text = [System.IO.File]::ReadAllText($readme.FullName)
            $lineNumber = 0
            foreach ($line in ($text -split '\r?\n')) {
                $lineNumber++
                if ($line -match $customerLanguagePattern) {
                    $boardReadmeFindings.Add(('{0}: line {1}: {2}' -f $relativePath, $lineNumber, (($line -replace '\s+', ' ').Trim())))
                }
            }
        }
    }

    if ($boardReadmeFindings.Count -gt 0) {
        Add-Result FAIL 'board README customer language standard' ("board README copy contains dev-negative customer wording. Examples: {0}" -f (($boardReadmeFindings | Select-Object -First 3) -join ' | ')) 'br2_external_dcentos\board'
    } else {
        Add-Result PASS 'board README customer language standard' 'board README copy uses Experimental/In development/proof-pending wording instead of dev-negative terms' 'br2_external_dcentos\board'
    }

    $badClaims = New-Object System.Collections.Generic.List[string]
    foreach ($docPath in $publicDocs) {
        $text = Get-RepoText $docPath
        if ($null -eq $text) {
            continue
        }

        $lineNumber = 0
        foreach ($line in ($text -split '\r?\n')) {
            $lineNumber++
            if ($line -notmatch '(?i)\bS19\s*Pro\b') {
                continue
            }

            $normalized = (($line -replace '\s+', ' ').Trim())
            $gateOpen = $normalized -match '(?i)accepted[- ]share(?:\s+and\s+persistent-install)?\s+(?:promotion\s+)?gates?\s+(?:remain\s+|stay\s+)?open'
            $miningProvenClaim = $normalized -match '(?i)\bmining[- ]proven\b'
            $coldBootMiningClaim = $normalized -match '(?i)cold-boot\s+mining'
            $acceptedShareClaim = $normalized -match '(?i)accepted[- ]share\s+evidence|accepted\s+pool\s+shares'

            if ($miningProvenClaim -or $coldBootMiningClaim -or ($acceptedShareClaim -and -not $gateOpen)) {
                $badClaims.Add(('{0}: line {1}: {2}' -f $docPath, $lineNumber, $normalized))
            }
        }
    }

    if ($badClaims.Count -gt 0) {
        Add-Result FAIL 'S19 Pro public claim honesty' ("S19 Pro public copy still implies mining-proven or accepted-share status. Examples: {0}" -f (($badClaims | Select-Object -First 3) -join ' | ')) 'docs'
    } else {
        Add-Result PASS 'S19 Pro public claim honesty' 'public docs keep S19 Pro as Experimental bring-up with accepted-share promotion gates open' 'docs'
    }

    $faqText = Get-RepoText $faqPath
    $platformsText = Get-RepoText $platformsPath
    $customFirmwareText = Get-RepoText $customFirmwarePath
    $publicBetaText = Get-RepoText $publicBetaPath
    $competitiveText = Get-RepoText $competitivePath

    Test-Pattern $faqText 'S19 Pro[\s\S]*Experimental feature[\s\S]*cold-boot and nonce evidence[\s\S]*accepted-share and persistent-install[\s\S]*promotion gates remain open' 'S19 Pro FAQ tier wording' 'FAQ describes S19 Pro as Experimental bring-up with open promotion gates' 'FAQ does not visibly keep S19 Pro out of the mining-proven accepted-share set' $faqPath
    Test-Pattern $platformsText '\|\s*\*\*Antminer S19 Pro\*\*[\s\S]*Experimental bring-up:[^|\r\n]*cold boot[^|\r\n]*146K nonces[^|\r\n]*accepted-share gate open' 'S19 Pro platforms row honesty' 'PLATFORMS lists S19 Pro as Experimental bring-up with accepted-share gate open' 'PLATFORMS still appears to promote S19 Pro beyond bring-up evidence' $platformsPath
    Test-Pattern $customFirmwareText 'mining-proven on multiple platforms[\s\S]*S9, S19j Pro[\s\S]*S21[\s\S]*S19 Pro remains an Experimental feature[\s\S]*accepted-share and[\s\S]*persistent-install promotion gates stay open' 'S19 Pro custom firmware caveat' 'CUSTOM_FIRMWARE excludes S19 Pro from the mining-proven list and states its open gates' 'CUSTOM_FIRMWARE does not visibly separate S19 Pro bring-up from mining-proven platforms' $customFirmwarePath
    Test-Pattern $publicBetaText 'am2-s19pro \(S19/S19 Pro\)[^\r\n]*Cold-boot \+ 146K nonces; accepted-share and persistent-install promotion gates remain open' 'S19 Pro public beta matrix honesty' 'Public beta matrix keeps S19 Pro Experimental with accepted-share and install gates open' 'Public beta matrix does not visibly keep S19 Pro in the Experimental/open-gate tier' $publicBetaPath
    Test-Pattern $competitiveText 'Antminer S19/S19j/S19 Pro[\s\S]*S19 Pro cold-boot/nonce bring-up exists with accepted-share gate open[\s\S]*S19j Pro share evidence exists' 'S19 Pro competitive matrix honesty' 'Competitive matrix separates S19 Pro bring-up from S19j Pro share evidence' 'Competitive matrix still appears to promote S19 Pro share/mining proof' $competitivePath
}

function Test-FleetDiscoveryHonesty {
    Write-Output ''
    Write-Output '=== Fleet Discovery Honesty ==='

    $fleetDiscoveryPath = 'dashboard\src\components\features\FleetDiscovery.tsx'
    $featureTypesPath = 'dashboard\src\api\feature-types.ts'
    $enLocalePath = 'dashboard\src\i18n\locales\en.ts'
    $esLocalePath = 'dashboard\src\i18n\locales\es.ts'
    $frLocalePath = 'dashboard\src\i18n\locales\fr.ts'
    $zhLocalePath = 'dashboard\src\i18n\locales\zh.ts'
    $dcentraldRestPath = 'dcentrald\dcentrald-api\src\rest.rs'

    $fleetDiscoveryText = Get-RepoText $fleetDiscoveryPath
    $featureTypesText = Get-RepoText $featureTypesPath
    $dcentraldRestText = Get-RepoText $dcentraldRestPath
    $localeText = @(
        (Get-RepoText $enLocalePath),
        (Get-RepoText $esLocalePath),
        (Get-RepoText $frLocalePath),
        (Get-RepoText $zhLocalePath)
    ) -join "`n"
    $combined = "$fleetDiscoveryText`n$localeText"

    Test-Pattern $fleetDiscoveryText '(?s)(?=.*read-only local state)(?=.*does not\s+scan subnets or contact peer miners)(?=.*DCENT_Toolbox)(?=.*Snapshot scope)(?=.*fleet-discovery-limitations)(?=.*does not scan the LAN or proxy manual probes)' 'fleet discovery local-snapshot UI honesty' 'FleetDiscovery describes firmware data as local snapshot plus manual probes and renders backend limitations' 'FleetDiscovery still lacks local-snapshot/manual-probe limitation copy' $fleetDiscoveryPath
    Test-Pattern $featureTypesText 'export\s+interface\s+FleetDiscoverResponse[\s\S]*source\?:\s*string[\s\S]*miners:\s*DiscoveredMiner\[\][\s\S]*limitations\?:\s*string\[\][\s\S]*request\?:\s*FleetDiscoverRequest' 'fleet discovery limitations TS contract' 'dashboard models backend fleet discovery limitations and echoed request metadata' 'FleetDiscoverResponse does not visibly model backend limitations' $featureTypesPath
    Test-Pattern $localeText 'Fleet Snapshot[\s\S]*LAN discovery is not linked yet[\s\S]*Actualizar Estado Local[\s\S]*d\\u00E9couverte LAN' 'fleet discovery locale honesty' 'Fleet locale copy uses local-snapshot wording and says LAN discovery is not linked' 'Fleet locale copy still appears to claim LAN discovery' 'dashboard\src\i18n\locales'
    Test-Pattern $featureTypesText 'powerWatts\?:\s*number\s*\|\s*null[\s\S]*totalPowerWatts:\s*number\s*\|\s*null' 'fleet discovery optional power TS contract' 'FleetDiscovery models power as optional/null reported telemetry' 'FleetDiscovery power contract is not visibly optional/null' $featureTypesPath
    Test-Pattern $fleetDiscoveryText '(?s)(?=.*reportedPowerReadings)(?=.*Power not reported by current sources)(?=.*Reported by)(?=.*stats\.totalPowerWatts === null)' 'fleet discovery reported-power UI contract' 'FleetDiscovery renders reported-power aggregation with an unavailable state' 'FleetDiscovery does not visibly render reported-power unavailable/source states' $fleetDiscoveryPath
    Test-Pattern $dcentraldRestText '(?s)(?=.*reported_power_watts\.filter\(\|watts\| watts\.is_finite\(\) && \*watts > 0\.0\))(?=.*"powerWatts":\s*reported_power_watts)' 'fleet discovery backend power finite-positive contract' '/api/fleet/discover only serializes finite positive reported power' 'Fleet discovery backend power field is missing or not visibly finite-positive gated' $dcentraldRestPath

    if ($combined -match '(?i)scans?\s+your\s+local\s+network|scanning\s+local\s+network|discover\s+DCENT_OS\s+miners\s+on\s+your\s+local\s+network|network\s+scan\s+above|no\s+competitor|feature\s+no\s+competitor|Descubra\s+mineros\s+DCENT_OS\s+en\s+su\s+red\s+local|D\\u00E9couvrez\s+les\s+mineurs\s+DCENT_OS\s+sur\s+votre\s+r\\u00E9seau\s+local') {
        Add-Result FAIL 'fleet discovery no LAN-scan overclaim' 'FleetDiscovery or locale copy still claims daemon-backed LAN scanning or competitor superiority' $fleetDiscoveryPath
    } else {
        Add-Result PASS 'fleet discovery no LAN-scan overclaim' 'FleetDiscovery and locale copy avoid LAN-scan and competitor-superiority claims' $fleetDiscoveryPath
    }

    if ("$fleetDiscoveryText`n$featureTypesText`n$dcentraldRestText" -match '(?i)hashrateThs\s*\*\s*80|80\s*W/TH|Estimated\s+at\s+80\s+W/TH') {
        Add-Result FAIL 'fleet discovery no hashrate-derived power' 'FleetDiscovery still derives or labels power from a fixed W/TH estimate' $fleetDiscoveryPath
    } else {
        Add-Result PASS 'fleet discovery no hashrate-derived power' 'FleetDiscovery does not derive fleet power from hashrate' $fleetDiscoveryPath
    }
}

function Test-RunAllGatesReproducibility {
    Write-Output ''
    Write-Output '=== run_all_gates Reproducibility ==='

    $scriptPath = 'scripts\run_all_gates.sh'
    $scriptText = Get-RepoText $scriptPath

    Test-Pattern $scriptText 'RUST_TOOLCHAIN="\$\{DCENT_RUST_TOOLCHAIN:-1\.90\.0\}"' 'run_all_gates rust toolchain default' 'run_all_gates defaults host Rust checks to the Phase-0 baseline toolchain' 'run_all_gates does not pin the baseline Rust toolchain' $scriptPath
    Test-Pattern $scriptText 'HOME:-[\s\S]*\.cargo/bin[\s\S]*export PATH' 'run_all_gates non-login cargo path' 'run_all_gates repairs the standard cargo/rustup PATH for non-login WSL shells' 'run_all_gates may miss rustup when invoked from a non-login shell' $scriptPath
    Test-Pattern $scriptText 'rustup run "\$RUST_TOOLCHAIN" cargo' 'run_all_gates rustup cargo execution' 'run_all_gates executes cargo through the selected rustup toolchain when available' 'run_all_gates can still fall through to an arbitrary host cargo' $scriptPath
    Test-Pattern $scriptText 'invalid DCENT_RUST_TOOLCHAIN' 'run_all_gates toolchain input guard' 'run_all_gates rejects unsafe toolchain override strings before shell execution' 'run_all_gates does not visibly validate the toolchain override string' $scriptPath
    Test-Pattern $scriptText 'DCENT_RUN_ALL_TARGET_DIR[\s\S]*CARGO_TARGET_DIR="/tmp/dcentos-run-all-gates-target"' 'run_all_gates transient target dir' 'run_all_gates supports explicit transient target dirs and defaults WSL artifacts away from the Windows workspace' 'run_all_gates does not visibly support deterministic transient target directories' $scriptPath
    Test-Pattern $scriptText 'run_gate "rust-host-tests" false' 'run_all_gates no rust fail-closed' 'run_all_gates fails rust-host-tests when the pinned toolchain cannot be configured' 'run_all_gates may skip rust host tests when the pinned toolchain is unavailable' $scriptPath
    Test-Pattern $scriptText 'ERROR: npm is required[\s\S]*run_gate "dashboard-build" false[\s\S]*run_gate "dashboard-vitest" false' 'run_all_gates dashboard prerequisites fail closed' 'run_all_gates fails dashboard gates when npm is unavailable instead of reporting all-pass with skips' 'run_all_gates may skip dashboard build/vitest when npm is unavailable' $scriptPath
    Test-Pattern $scriptText 'ERROR: python or python3 is required[\s\S]*run_gate "toolbox-pytest" false' 'run_all_gates toolbox prerequisites fail closed' 'run_all_gates fails toolbox-pytest when Python is unavailable in full mode' 'run_all_gates may skip toolbox pytest when Python is unavailable in full mode' $scriptPath
}

function Test-ProxyHardwareWriteGates {
    Write-Output ''
    Write-Output '=== Proxy/Hybrid Hardware Write Gates ==='

    $advancedComponents = @(
        @{ Path = 'dashboard\src\components\advanced\RegisterInspector.tsx'; Name = 'register inspector'; Pattern = 'Raw register writes disabled|Blocked:\s*bosminer owns hardware' },
        @{ Path = 'dashboard\src\components\advanced\I2cScanner.tsx'; Name = 'I2C scanner'; Pattern = 'Raw I2C writes disabled|Blocked:\s*bosminer owns I2C' },
        @{ Path = 'dashboard\src\components\advanced\AsicCommander.tsx'; Name = 'ASIC commander'; Pattern = 'Blocked:\s*bosminer owns ASIC hardware' },
        @{ Path = 'dashboard\src\components\advanced\VoltageControl.tsx'; Name = 'voltage control'; Pattern = 'Voltage writes disabled|Blocked:\s*bosminer owns voltage hardware' },
        @{ Path = 'dashboard\src\components\advanced\PsuLab.tsx'; Name = 'PSU lab'; Pattern = 'PSU control actions disabled|Blocked:\s*bosminer owns PSU hardware' },
        @{ Path = 'dashboard\src\components\advanced\PidTuner.tsx'; Name = 'PID tuner'; Pattern = 'PID writes disabled|Blocked:\s*bosminer owns fan control' },
        @{ Path = 'dashboard\src\components\advanced\ApiExplorer.tsx'; Name = 'API explorer'; Pattern = 'POST requests disabled|Blocked:\s*bosminer owns hardware' }
    )

    foreach ($component in $advancedComponents) {
        $text = Get-RepoText $component.Path
        Test-Pattern $text 'useSystemHealth' ("{0} health context" -f $component.Name) 'component reads honest system-health mode' 'component does not visibly read system-health mode' $component.Path
        Test-Pattern $text 'isProxyMode|proxyBlocked|proxyWriteBlocked' ("{0} proxy gate flag" -f $component.Name) 'component derives a proxy/hybrid write gate' 'component does not visibly derive a proxy/hybrid write gate' $component.Path
        Test-Pattern $text $component.Pattern ("{0} blocked copy" -f $component.Name) 'component shows operator-visible proxy/hybrid write blocking' 'component lacks visible proxy/hybrid write-blocking copy' $component.Path
    }
}

function Test-PreFlashInactiveSlotGates {
    Write-Output ''
    Write-Output '=== Pre-Flash Inactive Slot Gates ==='

    $preFlashPath = 'scripts\pre_flash_validate.sh'
    $preFlashText = Get-RepoText $preFlashPath

    Test-Pattern $preFlashText 'LEB_REPORT=.*reserved_ebs.*usable_eb_size' 'inactive UBI volume fields' 'pre-flash validation records inactive volume size fields' 'pre-flash validation does not visibly record inactive volume sizes' $preFlashPath
    Test-Pattern $preFlashText 'could not read inactive UBI volume layout' 'inactive UBI hard fail' 'pre-flash validation hard-fails when inactive UBI layout is unreadable' 'pre-flash validation does not hard-fail unreadable inactive UBI layout' $preFlashPath
    Test-Pattern $preFlashText 'check_leb_range\s*\(\)[\s\S]*rootfs[\s\S]*179[\s\S]*rootfs_data[\s\S]*210' 'inactive UBI LEB range checks' 'pre-flash validation checks expected inactive kernel/rootfs/rootfs_data LEB counts' 'pre-flash validation lacks expected inactive LEB range checks' $preFlashPath
    Test-Pattern $preFlashText 'PACKAGE_ROOT_ENTRY=.*tar\s+tf\s+"\$TARBALL"[\s\S]*\/root[\s\S]*PACKAGE_ROOT_SIZE=.*tar\s+tvf\s+"\$TARBALL"' 'packaged root size extraction' 'pre-flash validation extracts packaged rootfs size from sysupgrade tar metadata' 'pre-flash validation does not visibly extract packaged rootfs size' $preFlashPath
    Test-Pattern $preFlashText 'exceeds inactive rootfs capacity' 'inactive rootfs capacity gate' 'pre-flash validation rejects rootfs payloads larger than inactive rootfs capacity' 'pre-flash validation lacks inactive rootfs capacity rejection' $preFlashPath
}

function Get-CargoPackageName {
    param([string]$CrateDir)

    $manifest = Join-Path $CrateDir 'Cargo.toml'
    if (-not (Test-Path -LiteralPath $manifest)) {
        return $null
    }

    $inPackage = $false
    foreach ($line in (Get-Content -LiteralPath $manifest)) {
        if ($line -match '^\[(?<section>[^\]]+)\]') {
            $inPackage = ($matches['section'] -eq 'package')
            continue
        }

        if ($inPackage -and $line -match '^\s*name\s*=\s*"(?<name>[^"]+)"') {
            return $matches['name']
        }
    }

    return $null
}

function Get-TouchedRustPackages {
    if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
        Add-Result WARN 'touched Rust crates' 'git is not available; cannot inspect touched crates'
        return @()
    }

    $statusLines = & git -C $repoRoot status --porcelain -- dcentrald 2>$null
    if ($LASTEXITCODE -ne 0) {
        Add-Result WARN 'touched Rust crates' 'git status failed; cannot inspect touched crates'
        return @()
    }

    $packages = New-Object System.Collections.Generic.HashSet[string]
    foreach ($line in $statusLines) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }

        $path = $line.Substring(3).Trim()
        if ($path -match ' -> ') {
            $path = ($path -split ' -> ')[-1]
        }

        $path = $path -replace '/', '\'
        if ($path -notmatch '(^|\\)dcentrald\\(?<dir>[^\\]+)\\') {
            continue
        }

        $dir = $matches['dir']
        $crateDir = Join-Path (Join-RepoPath 'dcentrald') $dir
        $packageName = Get-CargoPackageName $crateDir
        if (-not [string]::IsNullOrWhiteSpace($packageName)) {
            [void]$packages.Add($packageName)
        }
    }

    return ($packages | Sort-Object)
}

function Get-RustEnvSetupCommand {
    param([string]$Triple)

    $dcentraldDir = Join-RepoPath 'dcentrald'

    if ($Triple -eq 'armv7-unknown-linux-musleabihf') {
        $cc = (Join-Path $dcentraldDir 'zig-cc-arm.bat').Replace("'", "''")
        $ar = (Join-Path $dcentraldDir 'zig-ar-arm.bat').Replace("'", "''")
        return "`$env:CC_armv7_unknown_linux_musleabihf = '$cc'; `$env:AR_armv7_unknown_linux_musleabihf = '$ar'"
    }

    if ($Triple -eq 'aarch64-unknown-linux-musl') {
        $cc = (Join-Path $dcentraldDir 'zig-cc-aarch64.bat').Replace("'", "''")
        $ar = (Join-Path $dcentraldDir 'zig-ar-aarch64.bat').Replace("'", "''")
        return "`$env:CC_aarch64_unknown_linux_musl = '$cc'; `$env:AR_aarch64_unknown_linux_musl = '$ar'"
    }

    return ''
}

function Test-TouchedRustCrates {
    Write-Output ''
    Write-Output '=== Touched Rust Crates ==='

    $packages = @(Get-TouchedRustPackages)
    if ($packages.Count -eq 0) {
        Add-Result PASS 'touched Rust crates' 'no changed Rust crate paths detected under dcentrald/'
        return
    }

    $dcentraldDir = Join-RepoPath 'dcentrald'
    $envSetup = Get-RustEnvSetupCommand $TargetTriple
    Add-Result PASS 'touched Rust crates' ("detected packages: {0}" -f ($packages -join ', '))

    foreach ($package in $packages) {
        $prefix = if ([string]::IsNullOrWhiteSpace($envSetup)) { 'cd dcentrald' } else { "cd dcentrald; $envSetup" }
        $command = "$prefix; cargo check -p $package --target $TargetTriple"
        $rustCheckCommands.Add($command)
        Add-Result PASS 'rust check command' $command
    }

    if (-not $RunRustChecks) {
        Add-Result WARN 'rust check execution' 'not run; pass -RunRustChecks to execute the generated cargo check commands'
        return
    }

    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Add-Result FAIL 'rust check execution' 'cargo is not available on PATH'
        return
    }

    Push-Location $dcentraldDir
    try {
        if ($TargetTriple -eq 'armv7-unknown-linux-musleabihf') {
            $env:CC_armv7_unknown_linux_musleabihf = (Join-Path $dcentraldDir 'zig-cc-arm.bat')
            $env:AR_armv7_unknown_linux_musleabihf = (Join-Path $dcentraldDir 'zig-ar-arm.bat')
        } elseif ($TargetTriple -eq 'aarch64-unknown-linux-musl') {
            $env:CC_aarch64_unknown_linux_musl = (Join-Path $dcentraldDir 'zig-cc-aarch64.bat')
            $env:AR_aarch64_unknown_linux_musl = (Join-Path $dcentraldDir 'zig-ar-aarch64.bat')
        }

        foreach ($package in $packages) {
            Write-Output ''
            Write-Output "Running cargo check for $package..."
            & cargo check -p $package --target $TargetTriple
            if ($LASTEXITCODE -eq 0) {
                Add-Result PASS 'rust check execution' "cargo check passed for $package"
            } else {
                Add-Result FAIL 'rust check execution' "cargo check failed for $package"
            }
        }
    } finally {
        Pop-Location
    }
}

function Test-ManifestPubkeyEmbeddedInBinary {
    # W1.2 (2026-05-07): production builds bake DCENT_MANIFEST_PUBLIC_KEY_HEX
    # into dcentrald via `option_env!()` (see dcentrald-api/src/ota_signature.rs).
    # If the env var was unset at `cargo build` time, the binary silently
    # ships with no at-rest manifest pin and falls back to fail-open. This
    # check uses `strings | grep` against a built binary to prove the pin
    # is actually embedded.
    #
    # Inputs:
    #   $ManifestPubkeyHex   - hex64 pubkey from -ManifestPubkeyHex arg or
    #                          $env:DCENT_MANIFEST_PUBLIC_KEY_HEX
    #   $DcentraldBinaryPath - explicit binary path, else auto-locate the
    #                          most recently built dcentrald under
    #                          dcentrald/target/<triple>/release/dcentrald
    if ([string]::IsNullOrWhiteSpace($ManifestPubkeyHex)) {
        Add-Result WARN 'manifest pubkey pin' 'no DCENT_MANIFEST_PUBLIC_KEY_HEX provided; skipping binary-embed check (dev/lab build)'
        return
    }

    if ($ManifestPubkeyHex -notmatch '^[0-9a-fA-F]{64}$') {
        Add-Result FAIL 'manifest pubkey pin' "DCENT_MANIFEST_PUBLIC_KEY_HEX must be 64 hex chars; got length $($ManifestPubkeyHex.Length)"
        return
    }

    $binaryPath = $DcentraldBinaryPath
    if ([string]::IsNullOrWhiteSpace($binaryPath)) {
        $candidates = @(
            "dcentrald/target/$TargetTriple/release/dcentrald",
            'dcentrald/target/armv7-unknown-linux-musleabihf/release/dcentrald',
            'dcentrald/target/aarch64-unknown-linux-musl/release/dcentrald'
        )
        foreach ($candidate in $candidates) {
            $abs = Join-RepoPath $candidate
            if (Test-Path -LiteralPath $abs) { $binaryPath = $abs; break }
        }
    } elseif (-not [System.IO.Path]::IsPathRooted($binaryPath)) {
        $binaryPath = Join-RepoPath $binaryPath
    }

    if ([string]::IsNullOrWhiteSpace($binaryPath) -or -not (Test-Path -LiteralPath $binaryPath)) {
        Add-Result FAIL 'manifest pubkey pin' 'dcentrald binary not found; build it before running this validator (cargo build --release --target <triple>)'
        return
    }

    # `strings` is the canonical Unix tool. On Windows hosts it's typically
    # provided by Sysinternals or Git for Windows. Fall back to a streaming
    # PowerShell scan when `strings` is missing.
    $stringsExe = (Get-Command strings -ErrorAction SilentlyContinue)
    $found = $false
    if ($stringsExe) {
        $matches = & $stringsExe.Source $binaryPath 2>$null | Select-String -SimpleMatch -Pattern $ManifestPubkeyHex
        if ($matches) { $found = $true }
    } else {
        # Fallback: byte-scan for the ASCII hex string in the binary.
        $bytes = [System.IO.File]::ReadAllBytes($binaryPath)
        $needle = [System.Text.Encoding]::ASCII.GetBytes($ManifestPubkeyHex)
        $needleLen = $needle.Length
        $limit = $bytes.Length - $needleLen
        for ($i = 0; $i -le $limit; $i++) {
            if ($bytes[$i] -eq $needle[0]) {
                $match = $true
                for ($j = 1; $j -lt $needleLen; $j++) {
                    if ($bytes[$i + $j] -ne $needle[$j]) { $match = $false; break }
                }
                if ($match) { $found = $true; break }
            }
        }
    }

    if ($found) {
        Add-Result PASS 'manifest pubkey pin' "DCENT_MANIFEST_PUBLIC_KEY_HEX is embedded in dcentrald (verified via strings)" $binaryPath
    } else {
        Add-Result FAIL 'manifest pubkey pin' 'DCENT_MANIFEST_PUBLIC_KEY_HEX is set but the hex string is NOT present in the dcentrald binary; rebuild with the env var exported' $binaryPath
    }
}

Write-Output 'DCENT_OS production readiness validation'
Write-Output ("Repo: {0}" -f $repoRoot)

Test-DashboardOwnsPort80
Test-DcentraldPort8080
Test-S99UpgradeHealthGate
Test-ServiceSupervision
Test-FanPwmScale
Test-DonationFeeReadiness
Test-NoVendorSshKeys
Test-NoVendorPhoneHomeEndpoints
Test-FirmwareIntakePackagingGates
Test-HonestTelemetryContracts
Test-DashboardReadOnlyUpgradeStatusUi
Test-DashboardApiCompatibilityManifestUi
Test-CompatibilityApiHonesty
Test-DashboardNoFabricatedOperatorData
Test-PublicS19ProClaimHonesty
Test-FleetDiscoveryHonesty
Test-RunAllGatesReproducibility
Test-ProxyHardwareWriteGates
Test-PreFlashInactiveSlotGates
Test-ManifestPubkeyEmbeddedInBinary
Test-TouchedRustCrates

Write-Output ''
Write-Output '=== Summary ==='
$failures = @($results | Where-Object { $_.Status -eq 'FAIL' })
$warnings = @($results | Where-Object { $_.Status -eq 'WARN' })
$passes = @($results | Where-Object { $_.Status -eq 'PASS' })
Write-Output ("Passes:   {0}" -f $passes.Count)
Write-Output ("Warnings: {0}" -f $warnings.Count)
Write-Output ("Failures: {0}" -f $failures.Count)

if ($rustCheckCommands.Count -gt 0) {
    Write-Output ''
    Write-Output 'Rust check commands:'
    foreach ($command in $rustCheckCommands) {
        Write-Output ("  {0}" -f $command)
    }
}

if ($failures.Count -gt 0) {
    exit 1
}

exit 0
