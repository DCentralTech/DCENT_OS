<# 
.SYNOPSIS
Low-overhead DCENT_axe dispatcher soak collector.

.DESCRIPTION
Polls /metrics and the compact /api/mining endpoint, then writes JSON Lines for
hardware soak analysis. This intentionally avoids repeated /api/system/info
polls because that endpoint builds the largest dashboard payload.

.EXAMPLE
.\scripts\soak-dispatcher-metrics.ps1 -BaseUrl http://203.0.113.139 -Seconds 900

.EXAMPLE
.\scripts\soak-dispatcher-metrics.ps1 -BaseUrl http://dcentaxe.local -BearerToken $env:DCENTAXE_TOKEN -MaxStaleDelta 0
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$BaseUrl,

    [int]$Seconds = 600,

    [int]$IntervalSeconds = 15,

    [string]$BearerToken = "",

    [string]$OutFile = "",

    [switch]$SkipMiningApi,

    [double]$MinHashrateGhs = 0,

    [int]$MaxStaleDelta = -1,

    [double]$MaxChipTempC = 0,

    [double]$MaxBoardTempC = 0,

    [double]$MaxVregTempC = 0,

    [int]$MinFanRpm = -1,

    [double]$MinInputVoltageMv = 0,

    [double]$MaxPowerW = 0,

    [double]$MaxCurrentA = 0,

    [double]$MaxAirDeltaC = 0,

    [double]$MaxChipTempSpreadC = 0,

    [int]$MaxPendingShares = -1,

    [int]$MaxOldestPendingSubmitAgeMs = -1,

    [int]$MaxUnresolvedDelta = -1,

    [double]$MinFreeHeapBytes = 0,

    [switch]$RequireMiningEnabled,

    [switch]$RequireSensorsOk,

    [switch]$RequireFan2,

    [switch]$DryRun
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = "Stop"

function Normalize-BaseUrl {
    param([string]$Url)
    $trimmed = $Url.Trim()
    if (-not ($trimmed.StartsWith("http://") -or $trimmed.StartsWith("https://"))) {
        $trimmed = "http://$trimmed"
    }
    return $trimmed.TrimEnd("/")
}

function New-RequestHeaders {
    param([string]$Token)
    $headers = @{
        "X-Requested-With" = "XMLHttpRequest"
    }
    if ($Token.Length -gt 0) {
        $headers["Authorization"] = "Bearer $Token"
    }
    return $headers
}

function Add-Metric {
    param(
        [hashtable]$Map,
        [string]$Key,
        [double]$Value
    )
    $Map[$Key] = $Value
}

function Parse-DcentMetrics {
    param([string]$Text)
    $metrics = @{}
    foreach ($rawLine in ($Text -split "`n")) {
        $line = $rawLine.Trim()
        if ($line.Length -eq 0 -or $line.StartsWith("#")) {
            continue
        }

        $parts = $line -split "\s+"
        if ($parts.Length -lt 2) {
            continue
        }

        $name = $parts[0]
        $valueText = $parts[1].Trim()
        $value = 0.0
        $parsed = [double]::TryParse(
            $valueText,
            [Globalization.NumberStyles]::Float,
            [Globalization.CultureInfo]::InvariantCulture,
            [ref]$value
        )
        if (-not $parsed) {
            continue
        }

        if ($name -like 'dcentaxe_hashrate_ghs*window="1m"*') {
            Add-Metric $metrics "hashrate_1m_ghs" $value
        } elseif ($name -like 'dcentaxe_hashrate_ghs*window="5m"*') {
            Add-Metric $metrics "hashrate_5m_ghs" $value
        } elseif ($name -like 'dcentaxe_hashrate_ghs*window="15m"*') {
            Add-Metric $metrics "hashrate_15m_ghs" $value
        } elseif ($name -eq "dcentaxe_shares_accepted_total") {
            Add-Metric $metrics "shares_accepted" $value
        } elseif ($name -eq "dcentaxe_shares_rejected_total") {
            Add-Metric $metrics "shares_rejected" $value
        } elseif ($name -eq "dcentaxe_stratum_shares_pending") {
            Add-Metric $metrics "shares_pending" $value
        } elseif ($name -eq "dcentaxe_stratum_shares_unresolved_total") {
            Add-Metric $metrics "shares_unresolved" $value
        } elseif ($name -eq "dcentaxe_stratum_oldest_pending_submit_age_ms") {
            Add-Metric $metrics "oldest_pending_submit_age_ms" $value
        } elseif ($name -eq "dcentaxe_dispatcher_stale_nonces_total") {
            Add-Metric $metrics "stale_nonces" $value
        } elseif ($name -eq "dcentaxe_dispatcher_slot_recoveries_total") {
            Add-Metric $metrics "slot_recoveries" $value
        } elseif ($name -eq "dcentaxe_dispatcher_filtered_nonces_total") {
            Add-Metric $metrics "filtered_nonces" $value
        } elseif ($name -eq "dcentaxe_dispatcher_ticket_difficulty") {
            Add-Metric $metrics "ticket_difficulty" $value
        } elseif ($name -eq "dcentaxe_power_watts") {
            Add-Metric $metrics "power_watts" $value
        } elseif ($name -eq "dcentaxe_current_ma") {
            Add-Metric $metrics "current_ma" $value
        } elseif ($name -eq "dcentaxe_voltage_mv") {
            Add-Metric $metrics "voltage_mv" $value
        } elseif ($name -eq "dcentaxe_input_voltage_mv") {
            Add-Metric $metrics "input_voltage_mv" $value
        } elseif ($name -eq "dcentaxe_frequency_mhz") {
            Add-Metric $metrics "frequency_mhz" $value
        } elseif ($name -eq "dcentaxe_fan_speed_pct") {
            Add-Metric $metrics "fan_speed_pct" $value
        } elseif ($name -like 'dcentaxe_fan_rpm*fan="1"*') {
            Add-Metric $metrics "fan_rpm_1" $value
        } elseif ($name -like 'dcentaxe_fan_rpm*fan="2"*') {
            Add-Metric $metrics "fan_rpm_2" $value
        } elseif ($name -eq "dcentaxe_sensors_ok") {
            Add-Metric $metrics "sensors_ok" $value
        } elseif ($name -eq "dcentaxe_thermal_sensors_ok") {
            Add-Metric $metrics "sensors_ok" $value
        } elseif ($name -eq "dcentaxe_mining_enabled") {
            Add-Metric $metrics "mining_enabled" $value
        } elseif ($name -eq "dcentaxe_uptime_seconds") {
            Add-Metric $metrics "uptime_seconds" $value
        } elseif ($name -eq "dcentaxe_free_heap_bytes") {
            Add-Metric $metrics "free_heap_bytes" $value
        } elseif ($name -like 'dcentaxe_temperature_celsius*sensor="chip"*') {
            Add-Metric $metrics "chip_temp_c" $value
        } elseif ($name -like 'dcentaxe_temperature_celsius*sensor="board"*') {
            Add-Metric $metrics "board_temp_c" $value
        } elseif ($name -like 'dcentaxe_temperature_celsius*sensor="vreg"*') {
            Add-Metric $metrics "vreg_temp_c" $value
        } elseif ($name -like 'dcentaxe_temperature_celsius*sensor="inlet"*') {
            Add-Metric $metrics "inlet_temp_c" $value
        } elseif ($name -like 'dcentaxe_temperature_celsius*sensor="outlet"*') {
            Add-Metric $metrics "outlet_temp_c" $value
        } elseif ($name -like 'dcentaxe_chip_temperature_summary_celsius*stat="min"*') {
            Add-Metric $metrics "chip_temp_min_c" $value
        } elseif ($name -like 'dcentaxe_chip_temperature_summary_celsius*stat="max"*') {
            Add-Metric $metrics "chip_temp_max_c" $value
        } elseif ($name -like 'dcentaxe_chip_temperature_summary_celsius*stat="spread"*') {
            Add-Metric $metrics "chip_temp_spread_c" $value
        } elseif ($name -eq "dcentaxe_temperature_max_celsius") {
            Add-Metric $metrics "temp_max_c" $value
        } elseif ($name -eq "dcentaxe_air_temperature_delta_celsius") {
            Add-Metric $metrics "air_delta_c" $value
        }
    }
    return $metrics
}

function Get-Number {
    param(
        [hashtable]$Map,
        [string]$Key
    )
    if ($null -eq $Map) {
        return $null
    }
    if ($Map.ContainsKey($Key)) {
        return [double]$Map[$Key]
    }
    return $null
}

function Get-Delta {
    param(
        [hashtable]$First,
        [hashtable]$Last,
        [string]$Key
    )
    if ($null -eq $First -or $null -eq $Last) {
        return $null
    }
    $a = Get-Number $First $Key
    $b = Get-Number $Last $Key
    if ($null -eq $a -or $null -eq $b) {
        return $null
    }
    return $b - $a
}

function Update-Extrema {
    param(
        [hashtable]$MinMap,
        [hashtable]$MaxMap,
        [hashtable]$Sample
    )
    foreach ($key in $Sample.Keys) {
        $value = [double]$Sample[$key]
        if (-not $MinMap.ContainsKey($key) -or $value -lt [double]$MinMap[$key]) {
            $MinMap[$key] = $value
        }
        if (-not $MaxMap.ContainsKey($key) -or $value -gt [double]$MaxMap[$key]) {
            $MaxMap[$key] = $value
        }
    }
}

function Max-Observed {
    param([string[]]$Keys)
    $best = $null
    foreach ($key in $Keys) {
        $value = Get-Number $maxMetrics $key
        if ($null -ne $value -and ($null -eq $best -or $value -gt $best)) {
            $best = $value
        }
    }
    return $best
}

function Min-Observed {
    param([string[]]$Keys)
    $best = $null
    foreach ($key in $Keys) {
        $value = Get-Number $minMetrics $key
        if ($null -ne $value -and ($null -eq $best -or $value -lt $best)) {
            $best = $value
        }
    }
    return $best
}

function Assert-MaxObserved {
    param(
        [string]$Name,
        [Nullable[double]]$Observed,
        [double]$Limit,
        [string]$Unit
    )
    if ($Limit -gt 0) {
        if ($null -eq $Observed) {
            throw "$Name was not observed; cannot enforce max limit $Limit $Unit."
        }
        if ($Observed -gt $Limit) {
            throw "$Name max $Observed $Unit exceeds limit $Limit $Unit."
        }
    }
}

function Assert-MinObserved {
    param(
        [string]$Name,
        [Nullable[double]]$Observed,
        [double]$Limit,
        [string]$Unit
    )
    if ($Limit -gt 0) {
        if ($null -eq $Observed) {
            throw "$Name was not observed; cannot enforce min limit $Limit $Unit."
        }
        if ($Observed -lt $Limit) {
            throw "$Name min $Observed $Unit is below limit $Limit $Unit."
        }
    }
}

$base = Normalize-BaseUrl $BaseUrl
if ($IntervalSeconds -lt 5) {
    throw "IntervalSeconds must be >= 5 to avoid loading the ESP32."
}
if ($Seconds -lt 0) {
    throw "Seconds must be >= 0."
}
if ($OutFile.Length -eq 0) {
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
    $OutFile = Join-Path (Get-Location) "soak-dispatcher-$stamp.jsonl"
}

$headers = New-RequestHeaders $BearerToken
if ($DryRun) {
    [pscustomobject]@{
        baseUrl = $base
        seconds = $Seconds
        intervalSeconds = $IntervalSeconds
        outFile = $OutFile
        skipMiningApi = [bool]$SkipMiningApi
        minHashrateGhs = $MinHashrateGhs
        maxStaleDelta = $MaxStaleDelta
        maxChipTempC = $MaxChipTempC
        maxBoardTempC = $MaxBoardTempC
        maxVregTempC = $MaxVregTempC
        minFanRpm = $MinFanRpm
        minInputVoltageMv = $MinInputVoltageMv
        maxPowerW = $MaxPowerW
        maxCurrentA = $MaxCurrentA
        maxAirDeltaC = $MaxAirDeltaC
        maxChipTempSpreadC = $MaxChipTempSpreadC
        maxPendingShares = $MaxPendingShares
        maxOldestPendingSubmitAgeMs = $MaxOldestPendingSubmitAgeMs
        maxUnresolvedDelta = $MaxUnresolvedDelta
        minFreeHeapBytes = $MinFreeHeapBytes
        requireMiningEnabled = [bool]$RequireMiningEnabled
        requireSensorsOk = [bool]$RequireSensorsOk
        requireFan2 = [bool]$RequireFan2
    } | ConvertTo-Json -Compress
    exit 0
}

$started = Get-Date
$deadline = $started.AddSeconds($Seconds)
$firstMetrics = $null
$lastMetrics = $null
$minMetrics = @{}
$maxMetrics = @{}
$sample = 0
$successfulMetricSamples = 0
$previousUptime = $null
$uptimeResetDetected = $false

Write-Host "Polling $base/metrics every $IntervalSeconds seconds. Output: $OutFile"

do {
    $now = Get-Date
    $sample += 1
    $metricsError = $null
    try {
        $metricsText = (Invoke-WebRequest -UseBasicParsing -TimeoutSec 8 -Uri "$base/metrics" -Headers $headers).Content
        $metrics = Parse-DcentMetrics $metricsText
    } catch {
        $metrics = @{}
        $metricsError = $_.Exception.Message
    }
    if ($metrics.Count -gt 0 -and $null -eq $firstMetrics) {
        $firstMetrics = $metrics.Clone()
    }
    if ($metrics.Count -gt 0) {
        $successfulMetricSamples += 1
        $lastMetrics = $metrics.Clone()
        Update-Extrema $minMetrics $maxMetrics $metrics
        $uptime = Get-Number $metrics "uptime_seconds"
        if ($null -ne $uptime) {
            if ($null -ne $previousUptime -and $uptime -lt $previousUptime) {
                $uptimeResetDetected = $true
            }
            $previousUptime = $uptime
        }
    }

    $mining = $null
    if (-not $SkipMiningApi) {
        try {
            $mining = Invoke-RestMethod -TimeoutSec 8 -Uri "$base/api/mining" -Headers $headers
        } catch {
            $mining = [pscustomobject]@{
                error = $_.Exception.Message
            }
        }
    }

    $row = [ordered]@{
        ts = $now.ToUniversalTime().ToString("o")
        elapsedSeconds = [int]($now - $started).TotalSeconds
        sample = $sample
        metrics = $metrics
        metricsError = $metricsError
        mining = $mining
    }
    $row | ConvertTo-Json -Depth 8 -Compress | Add-Content -LiteralPath $OutFile

    $hr5 = Get-Number $metrics "hashrate_5m_ghs"
    $accepted = Get-Number $metrics "shares_accepted"
    $rejected = Get-Number $metrics "shares_rejected"
    $stale = Get-Number $metrics "stale_nonces"
    $recovered = Get-Number $metrics "slot_recoveries"
    $filtered = Get-Number $metrics "filtered_nonces"
    $temp = Get-Number $metrics "chip_temp_c"
    $fan = Get-Number $metrics "fan_rpm_1"
    if ($metricsError) {
        Write-Host ("{0,5}s metrics error: {1}" -f [int]($now - $started).TotalSeconds, $metricsError)
    } else {
        Write-Host ("{0,5}s hr5={1,9:n2} acc={2,6:n0} rej={3,5:n0} stale={4,5:n0} recovered={5,5:n0} filtered={6,6:n0} temp={7,5:n1}C fan={8,5:n0}rpm" -f `
            [int]($now - $started).TotalSeconds, $hr5, $accepted, $rejected, $stale, $recovered, $filtered, $temp, $fan)
    }

    if ($Seconds -eq 0) {
        break
    }
    $remainingSeconds = [int][Math]::Ceiling(($deadline - (Get-Date)).TotalSeconds)
    if ($remainingSeconds -le 0) {
        break
    }
    Start-Sleep -Seconds ([Math]::Min($IntervalSeconds, $remainingSeconds))
} while ((Get-Date) -lt $deadline)

$staleDelta = Get-Delta $firstMetrics $lastMetrics "stale_nonces"
$recoveryDelta = Get-Delta $firstMetrics $lastMetrics "slot_recoveries"
$acceptedDelta = Get-Delta $firstMetrics $lastMetrics "shares_accepted"
$rejectedDelta = Get-Delta $firstMetrics $lastMetrics "shares_rejected"
$unresolvedDelta = Get-Delta $firstMetrics $lastMetrics "shares_unresolved"
$lastHashrate = Get-Number $lastMetrics "hashrate_5m_ghs"
$maxChipTemp = Max-Observed @("chip_temp_c", "chip_temp_max_c", "temp_max_c")
$maxBoardTemp = Max-Observed @("board_temp_c")
$maxVregTemp = Max-Observed @("vreg_temp_c")
$maxPower = Max-Observed @("power_watts")
$maxCurrentMa = Max-Observed @("current_ma")
$maxAirDelta = Max-Observed @("air_delta_c")
$maxChipSpread = Max-Observed @("chip_temp_spread_c")
$maxPendingSharesObserved = Max-Observed @("shares_pending")
$maxOldestPendingAge = Max-Observed @("oldest_pending_submit_age_ms")
$minFan1RpmObserved = Min-Observed @("fan_rpm_1")
$minFan2RpmObserved = Min-Observed @("fan_rpm_2")
$maxFan2RpmObserved = Max-Observed @("fan_rpm_2")
$gateFan2 = [bool]$RequireFan2 -or ($null -ne $maxFan2RpmObserved -and $maxFan2RpmObserved -gt 0)
$minFanRpmObserved = $minFan1RpmObserved
if ($gateFan2 -and $null -ne $minFan2RpmObserved -and ($null -eq $minFanRpmObserved -or $minFan2RpmObserved -lt $minFanRpmObserved)) {
    $minFanRpmObserved = $minFan2RpmObserved
}
$minInputVoltage = Min-Observed @("input_voltage_mv")
$minSensorsOk = Min-Observed @("sensors_ok")
$minMiningEnabled = Min-Observed @("mining_enabled")
$minFreeHeap = Min-Observed @("free_heap_bytes")
$minInletTemp = Min-Observed @("inlet_temp_c")
$minOutletTemp = Min-Observed @("outlet_temp_c")

Write-Host "Summary:"
Write-Host ("  accepted_delta={0:n0} rejected_delta={1:n0} stale_delta={2:n0} recovery_delta={3:n0} last_hr5={4:n2} GH/s" -f `
    $acceptedDelta, $rejectedDelta, $staleDelta, $recoveryDelta, $lastHashrate)
Write-Host ("  max_chip={0:n1}C max_board={1:n1}C max_vreg={2:n1}C max_power={3:n1}W min_fan={4:n0}rpm min_input={5:n0}mV sensors_min={6:n0}" -f `
    $maxChipTemp, $maxBoardTemp, $maxVregTemp, $maxPower, $minFanRpmObserved, $minInputVoltage, $minSensorsOk)
Write-Host ("  pending_max={0:n0} oldest_pending_max={1:n0}ms unresolved_delta={2:n0} min_heap={3:n0}B mining_min={4:n0}" -f `
    $maxPendingSharesObserved, $maxOldestPendingAge, $unresolvedDelta, $minFreeHeap, $minMiningEnabled)

if ($successfulMetricSamples -eq 0) {
    throw "No successful /metrics samples were collected."
}
if ($uptimeResetDetected) {
    throw "uptime_seconds decreased during soak; device likely rebooted."
}
if ($MinHashrateGhs -gt 0) {
    if ($null -eq $lastHashrate) {
        throw "hashrate_5m_ghs was not observed; cannot enforce MinHashrateGhs $MinHashrateGhs."
    }
    if ($lastHashrate -lt $MinHashrateGhs) {
        throw "Last 5m hashrate $lastHashrate GH/s is below MinHashrateGhs $MinHashrateGhs."
    }
}
if ($MaxStaleDelta -ge 0) {
    if ($null -eq $staleDelta) {
        throw "stale_nonces was not observed; cannot enforce MaxStaleDelta $MaxStaleDelta."
    }
    if ($staleDelta -gt $MaxStaleDelta) {
        throw "stale_nonces delta $staleDelta exceeds MaxStaleDelta $MaxStaleDelta."
    }
}
if ($MaxUnresolvedDelta -ge 0) {
    if ($null -eq $unresolvedDelta) {
        throw "shares_unresolved was not observed; cannot enforce MaxUnresolvedDelta $MaxUnresolvedDelta."
    }
    if ($unresolvedDelta -gt $MaxUnresolvedDelta) {
        throw "shares_unresolved delta $unresolvedDelta exceeds MaxUnresolvedDelta $MaxUnresolvedDelta."
    }
}
if ($MaxPendingShares -ge 0) {
    if ($null -eq $maxPendingSharesObserved) {
        throw "shares_pending was not observed; cannot enforce MaxPendingShares $MaxPendingShares."
    }
    if ($maxPendingSharesObserved -gt $MaxPendingShares) {
        throw "shares_pending max $maxPendingSharesObserved exceeds MaxPendingShares $MaxPendingShares."
    }
}
if ($MaxOldestPendingSubmitAgeMs -ge 0) {
    if ($null -eq $maxOldestPendingAge) {
        throw "oldest_pending_submit_age_ms was not observed; cannot enforce MaxOldestPendingSubmitAgeMs $MaxOldestPendingSubmitAgeMs."
    }
    if ($maxOldestPendingAge -gt $MaxOldestPendingSubmitAgeMs) {
        throw "oldest_pending_submit_age_ms max $maxOldestPendingAge exceeds MaxOldestPendingSubmitAgeMs $MaxOldestPendingSubmitAgeMs."
    }
}
if ($MaxAirDeltaC -gt 0 -and (($null -eq $minInletTemp -or $minInletTemp -le 0) -or ($null -eq $minOutletTemp -or $minOutletTemp -le 0))) {
    throw "inlet/outlet temperatures were not both observed as positive; cannot enforce MaxAirDeltaC $MaxAirDeltaC."
}
Assert-MaxObserved "chip temperature" $maxChipTemp $MaxChipTempC "C"
Assert-MaxObserved "board temperature" $maxBoardTemp $MaxBoardTempC "C"
Assert-MaxObserved "vreg temperature" $maxVregTemp $MaxVregTempC "C"
Assert-MaxObserved "power" $maxPower $MaxPowerW "W"
Assert-MaxObserved "current" $maxCurrentMa ($MaxCurrentA * 1000.0) "mA"
Assert-MaxObserved "air temperature delta" $maxAirDelta $MaxAirDeltaC "C"
Assert-MaxObserved "chip temperature spread" $maxChipSpread $MaxChipTempSpreadC "C"
Assert-MinObserved "fan 1 rpm" $minFan1RpmObserved $MinFanRpm "rpm"
if ($gateFan2) {
    Assert-MinObserved "fan 2 rpm" $minFan2RpmObserved $MinFanRpm "rpm"
}
Assert-MinObserved "input voltage" $minInputVoltage $MinInputVoltageMv "mV"
Assert-MinObserved "free heap" $minFreeHeap $MinFreeHeapBytes "bytes"
if ($RequireSensorsOk) {
    if ($null -eq $minSensorsOk) {
        throw "sensors_ok was not observed; cannot enforce RequireSensorsOk."
    }
    if ($minSensorsOk -lt 1) {
        throw "sensors_ok dropped below 1 during soak."
    }
}
if ($RequireMiningEnabled) {
    if ($null -eq $minMiningEnabled) {
        throw "mining_enabled was not observed; cannot enforce RequireMiningEnabled."
    }
    if ($minMiningEnabled -lt 1) {
        throw "mining_enabled dropped below 1 during soak."
    }
}
