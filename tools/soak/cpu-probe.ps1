<#
.SYNOPSIS
  Measures fennec.exe CPU in four UI states (window hidden in the tray / visible x
  empty search / typed query), each idle and under soak-churn at -Rate changes/s.
  Run elevated on the interactive desktop (e.g. via soak.ps1-style scheduled task).
  Writes cpu-probe-<stamp>.csv and prints a table. See tools/soak/README.md.
#>
param(
    [string]$InstallDir = 'C:\Program Files\Fennec',
    [string]$Instance = 'cpuprobe',
    [string]$ChurnRoot = 'D:\soak',
    [string]$ChurnExe = '',
    [string]$Query = 'notepad',
    [double]$Rate = 50,
    [int]$Seconds = 60,
    [string]$OutDir = ''
)
$ErrorActionPreference = 'Stop'
if (-not $ChurnExe) { $ChurnExe = Join-Path $PSScriptRoot 'soak-churn.exe' }
if (-not $OutDir) { $OutDir = $PSScriptRoot }
$fennec = Join-Path $InstallDir 'fennec.exe'
$fes = Join-Path $InstallDir 'fes.exe'
$csv = Join-Path $OutDir ('cpu-probe-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.csv')
Set-Content $csv 'state,window,search,churn,seconds,cpu_pct_of_one_core,ws_mb,churn_ops' -Encoding ASCII

function Count { $o = & $fes -instance $Instance -get-result-count 2>$null; if ($LASTEXITCODE -eq 0) { [int64]$o } else { 0 } }
function Measure-Cpu([Diagnostics.Process]$p, [int]$secs) {
    $p.Refresh(); $c0 = $p.TotalProcessorTime.TotalSeconds; $t0 = Get-Date
    Start-Sleep -Seconds $secs
    $p.Refresh()
    return @{ pct = ($p.TotalProcessorTime.TotalSeconds - $c0) / ((Get-Date) - $t0).TotalSeconds * 100; ws = $p.WorkingSet64 / 1MB }
}

& $ChurnExe prepare --root $ChurnRoot | Out-Null
$states = @(
    @{ window = 'hidden'; search = 'empty' }, @{ window = 'hidden'; search = 'query' },
    @{ window = 'visible'; search = 'empty' }, @{ window = 'visible'; search = 'query' }
)
foreach ($s in $states) {
    $argv = @('-instance', $Instance)
    if ($s.window -eq 'hidden') { $argv += '-startup' }
    if ($s.search -eq 'query') { $argv += @('-s', $Query) }
    $p = Start-Process -FilePath $fennec -ArgumentList $argv -PassThru
    $last = 0; for ($i = 0; $i -lt 120; $i++) { Start-Sleep 2; $n = Count; if ($n -gt 0 -and [math]::Abs($n - $last) -lt 200) { break }; $last = $n }
    Start-Sleep 10
    foreach ($churn in @('off', 'on')) {
        $cp = $null
        if ($churn -eq 'on') {
            $log = Join-Path $OutDir 'cpu-probe-churn.log'
            Remove-Item $log -ErrorAction SilentlyContinue
            $cp = Start-Process -FilePath $ChurnExe -ArgumentList @('run', '--root', $ChurnRoot, '--rate', $Rate, '--duration', "$($Seconds + 15)s", '--subtree-every', '24h', '--log', $log) -WindowStyle Hidden -PassThru
            Start-Sleep 5
        }
        $m = Measure-Cpu $p $Seconds
        $ops = ''
        if ($cp) { $cp.WaitForExit(); $ops = ((Get-Content $log | Select-String 'stopped') -replace '.*ops=(\d+).*', '$1') }
        $row = '{0}-{1}-{2},{0},{1},{2},{3},{4:F1},{5:F1},{6}' -f $s.window, $s.search, $churn, $Seconds, $m.pct, $m.ws, $ops
        Add-Content $csv $row -Encoding ASCII
        Write-Host $row
    }
    & $fes -instance $Instance -exit 2>$null | Out-Null
    if (-not $p.WaitForExit(30000)) { Stop-Process -Id $p.Id -Force }
}
Write-Host "CSV $csv"
