<#
.SYNOPSIS
  REL-05 soak run: starts fennec (and checks FennecService), drives soak-churn and
  samples memory / CPU / handles / threads / index size / crash files / compactions
  into soak-<start>.csv. See tools/soak/README.md.

.DESCRIPTION
  * Runs best as the interactive (console) user, elevated: from a SYSTEM job or a
    remote shell use -Detach, which re-launches the script with the same parameters
    through an interactive scheduled task (= schtasks /RU <console user> /IT /RL
    HIGHEST, but registered with the cmdlets: no 261-char /TR limit, no 72 h limit)
    and returns at once. fennec.exe then starts on the interactive desktop.
  * Started from session 0 without -Detach, the script launches only fennec.exe
    through such an interactive task, so it still lands on the console desktop.
  * fennec.exe is restarted if it exits (recorded in the note column, and as a PID
    change in the report); stop early by creating <OutDir>\soak.stop or Ctrl+C.
#>
[CmdletBinding()]
param(
    # Folder with fennec.exe and fes.exe (the MSI install, or a portable copy).
    [string]$InstallDir = 'C:\Program Files\Fennec',
    # fennec instance (-instance); '' = the default instance.
    [string]$Instance = '',
    # Data folder of that instance (logs\ is scanned for crash-*.log and compactions).
    # Default: <InstallDir>\data[\instances\<i>] when InstallDir has a `portable` marker,
    # else <console user>\AppData\Roaming\Fennec[\instances\<i>].
    [string]$DataDir = '',
    # Root folder soak-churn works in (created; everything below it is churned).
    [string]$ChurnRoot = 'D:\soak',
    # Run length (soak-churn syntax: 24h, 90m, 1h30m).
    [string]$Duration = '24h',
    # Sampling interval in minutes.
    [double]$IntervalMinutes = 5,
    # Steady change rate (changes per second).
    [double]$Rate = 50,
    # Extra soak-churn options, one string (works with powershell -File), e.g.
    # '--subtree 20000 --subtree-every 10m'.
    [string]$ChurnArgs = '',
    # soak-churn.exe (default: next to this script).
    [string]$ChurnExe = '',
    # Where the CSV, churn log and report go (default: the script folder).
    [string]$OutDir = '',
    # Fail unless FennecService is running (the service path: run fennec non-elevated).
    [switch]$ExpectService,
    # Extra fennec.exe arguments, one string (default: start hidden in the tray).
    [string]$AppArgs = '-startup',
    # User whose desktop gets fennec.exe when this script runs in session 0.
    [string]$InteractiveUser = '',
    # Run level of that interactive task: HIGHEST (elevated, indexes NTFS directly) or LIMITED.
    [ValidateSet('HIGHEST', 'LIMITED')][string]$RunLevel = 'HIGHEST',
    # Leave fennec running at the end.
    [switch]$KeepApp,
    # Do not restart fennec.exe when it exits.
    [switch]$NoRestart,
    # Maximum minutes to wait for the index to be ready before the churn starts.
    [double]$LoadTimeoutMinutes = 20,
    # Re-launch this script (same parameters) on the console user's desktop, elevated,
    # through a scheduled task, and return at once. Use from SYSTEM jobs / remote shells.
    [switch]$Detach
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$inv = [Globalization.CultureInfo]::InvariantCulture

$ChurnArgv = @($ChurnArgs -split '[\s,]+' | Where-Object { $_ })
$AppArgv = @($AppArgs -split '\s+' | Where-Object { $_ })
if (-not $ChurnExe) { $ChurnExe = Join-Path $PSScriptRoot 'soak-churn.exe' }
if (-not $OutDir) { $OutDir = $PSScriptRoot }
$fennec = Join-Path $InstallDir 'fennec.exe'
$fes = Join-Path $InstallDir 'fes.exe'
foreach ($f in @($fennec, $fes, $ChurnExe)) { if (-not (Test-Path $f)) { throw "missing $f" } }
New-Item -ItemType Directory -Force $OutDir | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$csv = Join-Path $OutDir "soak-$stamp.csv"
$runLog = Join-Path $OutDir "soak-$stamp.log"
$churnLog = Join-Path $OutDir "soak-$stamp-churn.log"
$stopFile = Join-Path $OutDir 'soak.stop'
Remove-Item $stopFile -ErrorAction SilentlyContinue

function Log([string]$m) {
    $line = '[{0}] {1}' -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $m
    Write-Host $line
    Add-Content -Path $runLog -Value $line -Encoding UTF8
}

# ---- who / where -------------------------------------------------------------------
$mySession = (Get-Process -Id $PID).SessionId
function Get-ConsoleUser {
    if ($InteractiveUser) { return $InteractiveUser }
    $u = (Get-CimInstance Win32_ComputerSystem).UserName   # DOMAIN\user of the console
    if (-not $u) { throw 'nobody is logged on at the console; pass -InteractiveUser or log on' }
    return $u
}
function Get-ProfileDir([string]$user) {
    $sid = (New-Object Security.Principal.NTAccount($user)).Translate([Security.Principal.SecurityIdentifier]).Value
    (Get-CimInstance Win32_UserProfile -Filter "SID='$sid'").LocalPath
}
if ($Detach) {
    # Same parameters minus -Detach, as one argument string (no /TR 261-char limit).
    $parts = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', ('"' + $PSCommandPath + '"'))
    foreach ($k in $PSBoundParameters.Keys) {
        if ($k -eq 'Detach') { continue }
        $v = $PSBoundParameters[$k]
        if ($v -is [switch]) { if ($v) { $parts += "-$k" } } else { $parts += "-$k"; $parts += ('"' + [string]$v + '"') }
    }
    $user = Get-ConsoleUser
    $tn = 'fennec-soak-' + ($(if ($Instance) { $Instance } else { 'default' }))
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument ($parts -join ' ') -WorkingDirectory $PSScriptRoot
    $principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    Register-ScheduledTask -TaskName $tn -Action $action -Principal $principal -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName $tn
    Write-Host "soak started as task '$tn' on the desktop of $user; log: $OutDir\soak-*.log; stop early: New-Item $OutDir\soak.stop"
    return
}
if (-not $DataDir) {
    if (Test-Path (Join-Path $InstallDir 'portable')) {
        $base = Join-Path $InstallDir 'data'
    } elseif ($mySession -ne 0) {
        $base = Join-Path $env:APPDATA 'Fennec'
    } else {
        $base = Join-Path (Get-ProfileDir (Get-ConsoleUser)) 'AppData\Roaming\Fennec'
    }
    $DataDir = $base
    if ($Instance) { $DataDir = Join-Path $base ('instances\' + ($Instance -replace '[\\/:*?"<>|]', '_')) }
}
$logDirs = @((Join-Path $DataDir 'logs'), (Join-Path $env:ProgramData 'Fennec\logs'))
$instArgs = @()
if ($Instance) { $instArgs = @('-instance', $Instance) }

# ---- helpers -----------------------------------------------------------------------
function Invoke-Tool([string]$exe, [string[]]$argv, [int]$timeoutMs = 60000) {
    # Runs a console tool with a timeout; returns @{ code; out }.
    $psi = New-Object Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.Arguments = ($argv | ForEach-Object { if ($_ -match '[\s"]' -or $_ -eq '') { '"' + ($_ -replace '"', '\"') + '"' } else { $_ } }) -join ' '
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.CreateNoWindow = $true
    $p = [Diagnostics.Process]::Start($psi)
    $out = $p.StandardOutput.ReadToEndAsync()
    if (-not $p.WaitForExit($timeoutMs)) {
        try { $p.Kill() } catch { }
        return @{ code = -1; out = '' }
    }
    return @{ code = $p.ExitCode; out = $out.Result.Trim() }
}

function Get-Count([string]$search) {
    $a = $instArgs + @('-get-result-count')
    if ($search) { $a += $search }
    $r = Invoke-Tool $fes $a 120000
    if ($r.code -eq 0 -and $r.out -match '^\d+$') { return [int64]$r.out }
    return $null
}

function Find-App {
    # fennec.exe started from $fennec with our instance.
    $exe = (Resolve-Path $fennec).Path
    $procs = Get-CimInstance Win32_Process -Filter "Name='fennec.exe'" | Where-Object { $_.ExecutablePath -eq $exe }
    foreach ($p in $procs) {
        $cl = [string]$p.CommandLine
        if ($Instance) {
            if ($cl -match ('-instance\s+"?' + [regex]::Escape($Instance) + '"?(\s|$)')) { return [int]$p.ProcessId }
        } elseif ($cl -notmatch '-instance\s') { return [int]$p.ProcessId }
    }
    return $null
}

function Start-App {
    $argv = $instArgs + $AppArgv
    if ($mySession -ne 0) {
        if ($argv.Count -gt 0) {
            Start-Process -FilePath $fennec -ArgumentList $argv -WorkingDirectory $InstallDir | Out-Null
        } else {
            Start-Process -FilePath $fennec -WorkingDirectory $InstallDir | Out-Null
        }
    } else {
        # Session 0: hand it to the console user's desktop through an interactive task.
        # (= schtasks /Create /RU <user> /IT /RL HIGHEST; the cmdlets avoid /TR quoting
        # trouble with "C:\Program Files" and lift the 72 h execution limit.)
        $user = Get-ConsoleUser
        $tn = 'fennec-soak-app-' + ($(if ($Instance) { $Instance } else { 'default' }))
        $level = $(if ($RunLevel -eq 'HIGHEST') { 'Highest' } else { 'Limited' })
        $action = New-ScheduledTaskAction -Execute $fennec -Argument ($argv -join ' ') -WorkingDirectory $InstallDir
        $principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel $level
        $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
        Register-ScheduledTask -TaskName $tn -Action $action -Principal $principal -Settings $settings -Force | Out-Null
        Start-ScheduledTask -TaskName $tn
    }
    for ($i = 0; $i -lt 60; $i++) {
        Start-Sleep -Seconds 1
        $id = Find-App
        if ($id) { return $id }
    }
    throw 'fennec.exe did not start'
}

function Stop-App([int]$id) {
    Invoke-Tool $fes ($instArgs + @('-exit')) 30000 | Out-Null
    for ($i = 0; $i -lt 60; $i++) {
        if (-not (Get-Process -Id $id -ErrorAction SilentlyContinue)) { return 'exited' }
        Start-Sleep -Seconds 1
    }
    Stop-Process -Id $id -Force -ErrorAction SilentlyContinue
    return 'killed after 60 s'
}

function Get-ServicePid {
    $s = Get-CimInstance Win32_Service -Filter "Name='FennecService'" -ErrorAction SilentlyContinue
    if ($s -and $s.State -eq 'Running' -and $s.ProcessId) { return [int]$s.ProcessId }
    return $null
}

$compactSeen = @{}   # log file -> highest count seen (survives rotation)
function Get-Compactions {
    foreach ($f in Get-ChildItem -Path (Join-Path $DataDir 'logs') -Filter 'fennec*.log' -ErrorAction SilentlyContinue) {
        $n = @(Select-String -Path $f.FullName -SimpleMatch 'index compacted while running' -ErrorAction SilentlyContinue).Count
        if (-not $compactSeen.ContainsKey($f.Name) -or $compactSeen[$f.Name] -lt $n) { $compactSeen[$f.Name] = $n }
    }
    $t = 0
    foreach ($v in $compactSeen.Values) { $t += $v }
    return $t
}

$runStart = Get-Date
function Get-CrashFiles {
    $n = 0
    foreach ($d in $logDirs) {
        $n += @(Get-ChildItem -Path $d -Filter 'crash-*.log' -ErrorAction SilentlyContinue | Where-Object { $_.LastWriteTime -ge $runStart }).Count
    }
    return $n
}

function Fmt($v, [string]$f = '0.0') {
    if ($null -eq $v) { return '' }
    return ([double]$v).ToString($f, $inv)
}

$prev = @{}
function Get-ProcSample([string]$key, $id) {
    # @(ws, private, cpu_s, cpu_pct, handles, threads) or empties.
    if (-not $id) { return @('', '', '', '', '', '') }
    $p = Get-Process -Id $id -ErrorAction SilentlyContinue
    if (-not $p) { return @('', '', '', '', '', '') }
    $now = Get-Date
    $cpu = $p.TotalProcessorTime.TotalSeconds
    $pct = ''
    if ($prev.ContainsKey($key) -and $prev[$key].id -eq $id) {
        $dt = ($now - $prev[$key].t).TotalSeconds
        if ($dt -gt 0) { $pct = Fmt (($cpu - $prev[$key].cpu) / $dt * 100) '0.00' }
    }
    $prev[$key] = @{ id = $id; t = $now; cpu = $cpu }
    return @((Fmt ($p.WorkingSet64 / 1MB)), (Fmt ($p.PrivateMemorySize64 / 1MB)), (Fmt $cpu '0.00'), $pct, $p.HandleCount, $p.Threads.Count)
}

$header = 'time,elapsed_s,app_pid,app_ws_mb,app_private_mb,app_cpu_s,app_cpu_pct,app_handles,app_threads,svc_pid,svc_ws_mb,svc_private_mb,svc_cpu_s,svc_cpu_pct,svc_handles,svc_threads,entries,soak_entries,crash_files,compactions,note'
Set-Content -Path $csv -Value $header -Encoding ASCII
$compactBase = 0
function Write-Sample($appId, [string]$note) {
    $svcId = Get-ServicePid
    $a = Get-ProcSample 'app' $appId
    $s = Get-ProcSample 'svc' $svcId
    $entries = $null; $soak = $null
    if ($appId) { $entries = Get-Count ''; $soak = Get-Count ($ChurnRoot.TrimEnd('\') + '\') }
    $row = @(
        (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'),
        (Fmt ((Get-Date) - $runStart).TotalSeconds '0'),
        $(if ($a[0] -ne '') { $appId } else { '' })
    ) + $a + @($(if ($s[0] -ne '') { $svcId } else { '' })) + $s + @(
        (Fmt $entries '0'), (Fmt $soak '0'), (Get-CrashFiles), ((Get-Compactions) - $script:compactBase),
        ('"' + ($note -replace '"', "'") + '"')
    )
    Add-Content -Path $csv -Value ($row -join ',') -Encoding ASCII
    Log ("sample app={0} ws={1}MB priv={2}MB cpu={3}% h={4} thr={5} svc={6} ws={7}MB h={8} entries={9} soak={10} {11}" -f `
            $appId, $a[0], $a[1], $a[3], $a[4], $a[5], $svcId, $s[0], $s[4], $entries, $soak, $note)
}

# ---- run ---------------------------------------------------------------------------
Log "soak.ps1: install=$InstallDir instance='$Instance' data=$DataDir churn=$ChurnRoot duration=$Duration interval=${IntervalMinutes}m rate=$Rate session=$mySession csv=$csv"
if ($ExpectService -and -not (Get-ServicePid)) { throw 'FennecService is not running (-ExpectService)' }
$durSec = [double](& $ChurnExe seconds $Duration)
if (-not $durSec) { throw "bad -Duration $Duration" }

$churn = $null
$appId = $null
trap { Log "ERROR: $_"; break }
try {
    Log 'preparing the churn tree (soak-churn prepare)'
    & $ChurnExe prepare --root $ChurnRoot @ChurnArgv --log $churnLog | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "soak-churn prepare failed ($LASTEXITCODE)" }

    $appId = Find-App
    if ($appId) { Log "fennec already running (pid $appId); attaching" } else { $appId = Start-App; Log "started fennec (pid $appId)" }
    # Loaded = a non-zero entry count that holds still for 10 s (the churn is not
    # running yet, so only the initial scan / USN catch-up moves it).
    $deadline = (Get-Date).AddMinutes($LoadTimeoutMinutes)
    $last = $null; $stable = 0; $n = $null
    while ((Get-Date) -lt $deadline) {
        $n = Get-Count ''
        if ($n -gt 0 -and $last -gt 0 -and [math]::Abs($n - $last) -le [math]::Max(100, $last * 0.001)) { $stable++ } else { $stable = 0 }
        if ($stable -ge 2) { break }
        $last = $n
        Start-Sleep -Seconds 5
    }
    Log "index ready: $n entries"
    $script:compactBase = Get-Compactions
    $runStart = Get-Date
    Write-Sample $appId 'start'

    $cargs = @('run', '--root', $ChurnRoot, '--rate', ([string]$Rate), '--duration', $Duration, '--log', $churnLog) + $ChurnArgv
    $churn = Start-Process -FilePath $ChurnExe -ArgumentList $cargs -WindowStyle Hidden -PassThru
    Log "started soak-churn (pid $($churn.Id))"

    $end = $runStart.AddSeconds($durSec)
    $next = $runStart.AddMinutes($IntervalMinutes)
    $why = 'duration reached'
    while ($true) {
        Start-Sleep -Seconds 5
        $note = ''
        if (-not (Get-Process -Id $appId -ErrorAction SilentlyContinue)) {
            $note = "fennec pid $appId exited"
            Log $note
            Write-Sample $null $note
            if ($NoRestart) { $why = 'fennec exited'; break }
            $appId = Start-App
            $note = "restarted fennec (pid $appId)"
            Log $note
        }
        if ($churn.HasExited -and (Get-Date) -lt $end.AddSeconds(-30)) { $why = "soak-churn exited early ($($churn.ExitCode))"; break }
        if (Test-Path $stopFile) { $why = 'stop file'; break }
        if ((Get-Date) -ge $end) { break }
        if ((Get-Date) -ge $next -or $note) {
            Write-Sample $appId $note
            $next = $next.AddMinutes($IntervalMinutes)
            if ($next -lt (Get-Date)) { $next = (Get-Date).AddMinutes($IntervalMinutes) }
        }
    }
    Log "ending: $why"
    Write-Sample $appId "end: $why"
} finally {
    if ($churn -and -not $churn.HasExited) {
        New-Item -ItemType File -Force (Join-Path $ChurnRoot 'soak-churn.stop') | Out-Null
        if (-not $churn.WaitForExit(60000)) { Stop-Process -Id $churn.Id -Force -ErrorAction SilentlyContinue }
    }
    if ($appId -and -not $KeepApp) { Log ("fennec: " + (Stop-App $appId)) }
    Remove-Item $stopFile -ErrorAction SilentlyContinue
}
$md = Join-Path $OutDir "soak-$stamp.md"
& $ChurnExe report $csv --out $md --svg-dir (Join-Path $OutDir "soak-$stamp-charts")
Log "CSV $csv; report $md"
