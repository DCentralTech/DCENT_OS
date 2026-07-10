param(
    [Parameter(Mandatory = $true)]
    [string]$Ip,
    [int]$Minutes = 5,
    [int]$IntervalSeconds = 30,
    [int]$WarmupSeconds = 120,
    [double]$MinHashrateGhs = 3000,
    [int]$MaxWallWatts = 1300,
    [string]$User = 'root',
    [string]$Password = $env:DCENT_PASSWORD
)

$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($Password)) {
    throw 'Set DCENT_PASSWORD or pass -Password for soak checks.'
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptDir '..\..\..')).Path
$sshCmdJs = Join-Path $repoRoot 'tools\ssh_cmd.js'

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

function Get-FirstToken {
    param([string]$Text)

    if ([string]::IsNullOrWhiteSpace($Text)) {
        return ''
    }

    return (($Text.Trim() -split '\s+')[0]).Trim()
}

function Get-Status {
    $json = Invoke-SshOutput 'wget -qO- http://127.0.0.1:80/api/status 2>/dev/null'
    return $json | ConvertFrom-Json
}

function Get-LatestSummaryLine {
    $result = Invoke-SshTry "grep 'Hashrate:' /tmp/dcentrald.log 2>/dev/null | tail -n 1"
    if ($result.Ok) {
        return $result.Output.Trim()
    }
    return ''
}

function Parse-SummaryLine {
    param([string]$Line)

    $parsed = [ordered]@{}
    if ([string]::IsNullOrWhiteSpace($Line)) {
        return $parsed
    }

    $patterns = @{
        shares_submitted = 'sub:(?<v>\d+)'
        pool_accepted    = 'acc:(?<v>\d+)'
        pool_rejected    = 'rej:(?<v>\d+)'
        dedup            = 'dedup:(?<v>\d+)'
        stale            = 'stale:(?<v>\d+)'
        stale_overwrite  = 'ovr:(?<v>\d+)'
        stale_empty      = 'empty:(?<v>\d+)'
        local_legacy     = 'local:(?<v>\d+)'
        hw_errors        = 'hw_errors=(?<v>\d+)'
        pool_diff        = 'pool_diff=(?<v>[0-9.]+)'
        work_rate        = '\|\s*(?<v>\d+) work/sec\s*\|'
    }

    foreach ($name in $patterns.Keys) {
        $match = [regex]::Match($Line, $patterns[$name])
        if ($match.Success) {
            $parsed[$name] = $match.Groups['v'].Value
        }
    }

    return $parsed
}

Step "Connecting to $Ip"
$probe = Invoke-SshTry 'echo OK'
if (-not $probe.Ok -or $probe.Output.Trim() -ne 'OK') {
    throw ("Cannot SSH to root@{0}: {1}" -f $Ip, $probe.Output)
}
Write-Output '  SSH OK'

$status = Get-Status
$summaryLine = Get-LatestSummaryLine
$summary = Parse-SummaryLine $summaryLine

$startingAccepted = [int]($status.accepted)
$startingRejected = [int]($status.rejected)
$samples = @()
$warnings = New-Object System.Collections.Generic.List[string]
$failures = New-Object System.Collections.Generic.List[string]

Step 'Starting soak monitor'
Write-Output ("  Duration:        {0} min" -f $Minutes)
Write-Output ("  Interval:        {0} s" -f $IntervalSeconds)
Write-Output ("  Warmup:          {0} s" -f $WarmupSeconds)
Write-Output ("  Start accepted:  {0}" -f $startingAccepted)
Write-Output ("  Start rejected:  {0}" -f $startingRejected)
Write-Output ("  Pool:            {0}" -f $status.pool.url)

$deadline = (Get-Date).AddMinutes($Minutes)
$sampleIndex = 0

while ((Get-Date) -lt $deadline) {
    $sampleIndex += 1
    $now = Get-Date
    $elapsed = [int]($now - ($deadline.AddMinutes(-$Minutes))).TotalSeconds

    $status = Get-Status
    $summaryLine = Get-LatestSummaryLine
    $summary = Parse-SummaryLine $summaryLine

    $accepted = [int]($status.accepted)
    $rejected = [int]($status.rejected)
    $hashrateGhs = [double]($status.hashrate_ghs)
    $hashrate5s = [double]($status.hashrate_5s_ghs)
    $wallWatts = [int]($status.power.wall_watts)
    $poolStatus = [string]$status.pool.status
    $poolDiff = [double]$status.pool.difficulty
    $hwErrors = 0
    if ($summary.Contains('hw_errors')) {
        $hwErrors = [int]$summary['hw_errors']
    }

    $sample = [PSCustomObject]@{
        Timestamp = $now.ToString('s')
        ElapsedS = $elapsed
        HashrateGhs = [math]::Round($hashrateGhs, 2)
        Hashrate5sGhs = [math]::Round($hashrate5s, 2)
        Accepted = $accepted
        Rejected = $rejected
        PoolDiff = $poolDiff
        PoolStatus = $poolStatus
        WallWatts = $wallWatts
        HwErrors = $hwErrors
        SummaryLine = $summaryLine
    }
    $samples += $sample

    Write-Output (("[{0}] {1} | avg {2} GH/s | 5s {3} GH/s | acc {4} rej {5} | diff {6} | wall {7}W | hw {8}" -f `
        $sampleIndex, $sample.Timestamp, $sample.HashrateGhs, $sample.Hashrate5sGhs, $sample.Accepted, $sample.Rejected, $sample.PoolDiff, $sample.WallWatts, $sample.HwErrors))

    if ($poolStatus -ne 'Alive') {
        $failures.Add("pool not alive at sample $sampleIndex (status=$poolStatus)")
    }

    if ($rejected -gt $startingRejected) {
        $failures.Add("rejected shares increased from $startingRejected to $rejected")
    }

    if ($hwErrors -gt 0) {
        $failures.Add("hw_errors reported as $hwErrors in summary line")
    }

    if ($wallWatts -gt $MaxWallWatts) {
        $failures.Add("wall watts $wallWatts exceeded cap $MaxWallWatts")
    }

    if ($elapsed -ge $WarmupSeconds -and $hashrateGhs -lt $MinHashrateGhs) {
        $failures.Add("hashrate $hashrateGhs GH/s stayed below floor $MinHashrateGhs GH/s after warmup")
    }

    if ($summary.Contains('local_legacy')) {
        $localLegacy = [int]$summary['local_legacy']
        if ($localLegacy -gt 0 -and $accepted -eq $startingAccepted) {
            $warnings.Add("local_legacy is $localLegacy with no new accepted shares yet")
        }
    }

    if ((Get-Date) -ge $deadline) {
        break
    }

    Start-Sleep -Seconds $IntervalSeconds
}

$endingAccepted = [int]($status.accepted)
$endingRejected = [int]($status.rejected)
$acceptedDelta = $endingAccepted - $startingAccepted
$rejectedDelta = $endingRejected - $startingRejected
$avgHashrate = if ($samples.Count -gt 0) { [math]::Round((($samples | Measure-Object -Property HashrateGhs -Average).Average), 2) } else { 0 }
$avgWallWatts = if ($samples.Count -gt 0) { [math]::Round((($samples | Measure-Object -Property WallWatts -Average).Average), 0) } else { 0 }

Step 'Soak summary'
Write-Output ("  Samples:           {0}" -f $samples.Count)
Write-Output ("  Avg hashrate:      {0} GH/s" -f $avgHashrate)
Write-Output ("  Avg wall watts:    {0} W" -f $avgWallWatts)
Write-Output ("  Accepted delta:    {0}" -f $acceptedDelta)
Write-Output ("  Rejected delta:    {0}" -f $rejectedDelta)
Write-Output ("  Final pool status: {0}" -f $status.pool.status)

if ($warnings.Count -gt 0) {
    Write-Output ''
    Write-Output 'Warnings:'
    $warnings | Select-Object -Unique | ForEach-Object { Write-Output ("  - {0}" -f $_) }
}

if ($failures.Count -gt 0) {
    Write-Output ''
    Write-Output 'Failures:'
    $failures | Select-Object -Unique | ForEach-Object { Write-Output ("  - {0}" -f $_) }
    exit 1
}

Write-Output ''
Write-Output 'Soak result: PASS'
