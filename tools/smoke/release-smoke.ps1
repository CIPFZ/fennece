# Release smoke test: downloads the packages of a GitHub release tag and checks them on a
# Windows machine (REL-08/REL-11; first used for v0.2.0-rc.2, docs/reports/0.2.0-rc2-smoke.md).
# Run as an interactive administrator, not SYSTEM: it starts the GUI and talks to it with fes.
# It uninstalls any installed Fennec first and leaves the standard MSI installed.
#   powershell -ExecutionPolicy Bypass -File tools\smoke\release-smoke.ps1 -Tag v0.2.0-rc.2 `
#       -OldData D:\rel03-snap010\instances\rel03 -IpcLister D:\smoke-ipc\target\release\smoke-ipc.exe
# -OldData: a data folder written by the previous release with a filter "REL03 Filter"
# (macro rel03f) and a bookmark "REL03 Bookmark" (REL-03 fixture). -IpcLister: build
# tools\smoke\ipc-lists.rs as a standalone crate (see its header) to print what the running
# app loaded; without it the IPC checks are skipped.
param([string]$Tag = 'v0.2.0-rc.2', [string]$Work = 'D:\rc2', [string]$OldData = 'D:\rel03-snap010\instances\rel03',
    [string]$IpcLister = 'D:\smoke-ipc\target\release\smoke-ipc.exe')
$ErrorActionPreference = 'Continue'
$ProgressPreference = 'SilentlyContinue'
$v = $Tag.TrimStart('v') -replace '-rc\.\d+$', ''
$log = Join-Path $Work 'smoke.log'
$ev = Join-Path $Work 'evidence'
Remove-Item -Recurse -Force $Work -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $Work, $ev | Out-Null
$script:fails = 0
function Say($m) { $l = "[{0:HH:mm:ss}] $m" -f (Get-Date); Add-Content -Encoding utf8 $log $l }
function Check($name, [bool]$ok, $detail = '') {
    if (-not $ok) { $script:fails++ }
    Say ("{0} {1}{2}" -f $(if ($ok) { 'PASS' } else { 'FAIL' }), $name, $(if ($detail) { " - $detail" } else { '' }))
}
$pf = 'C:\Program Files\Fennec'
$appData = Join-Path $env:APPDATA 'Fennec'

function Get-FennecProducts {
    Get-ChildItem HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall, HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall -ErrorAction SilentlyContinue |
        ForEach-Object { Get-ItemProperty $_.PSPath } | Where-Object { $_.DisplayName -like 'Fennec*' }
}
function Stop-Fennec { Get-Process fennec, fes -ErrorAction SilentlyContinue | Stop-Process -Force; Start-Sleep 2 }
function Msi($argList, $name) {
    $p = Start-Process msiexec.exe -ArgumentList $argList -Wait -PassThru
    Check "$name (msiexec exit $($p.ExitCode))" ($p.ExitCode -eq 0)
}
function Leftovers($label) {
    $items = @()
    if (Test-Path $pf) { $items += $pf }
    if (Test-Path 'C:\ProgramData\Fennec') { $items += 'C:\ProgramData\Fennec' }
    if (Get-Service FennecService -ErrorAction SilentlyContinue) { $items += 'service FennecService' }
    foreach ($k in 'Directory\shell\Fennec', 'Directory\Background\shell\Fennec', 'Drive\shell\Fennec', '*\shell\FennecExplore') {
        reg query "HKLM\Software\Classes\$k" *> $null
        if ($LASTEXITCODE -eq 0) { $items += "HKLM\Software\Classes\$k" }
    }
    reg query 'HKLM\Software\Microsoft\Windows\CurrentVersion\Run' /v Fennec *> $null
    if ($LASTEXITCODE -eq 0) { $items += 'HKLM Run\Fennec' }
    if (Test-Path 'C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Fennec.lnk') { $items += 'Start menu Fennec.lnk' }
    if (Get-FennecProducts) { $items += 'Uninstall entry' }
    Check "${label}: no leftovers" ($items.Count -eq 0) ($items -join '; ')
    Check "${label}: %APPDATA%\Fennec kept (default)" (Test-Path $appData)
}
function Wait-Ipc($inst, $secs = 120) {
    $t = Get-Date
    while (((Get-Date) - $t).TotalSeconds -lt $secs) {
        $n = & "$pf\fes.exe" -instance $inst -get-result-count '' 2>$null
        if ($LASTEXITCODE -eq 0 -and [int64]"0$n" -gt 0) { return [int64]$n }
        Start-Sleep 3
    }
    return 0
}

Say "smoke $Tag (version $v) on $env:COMPUTERNAME"

# ---- 1. Download the release assets and verify them ------------------------------------
$base = "https://github.com/CIPFZ/fennece/releases/download/$Tag"
$files = 'standard.msi', 'standard-portable.zip', 'experimental.msi', 'experimental-portable.zip' | ForEach-Object { "fennec-$v-x64-$_" }
try { Invoke-WebRequest "$base/SHA256SUMS.txt" -OutFile (Join-Path $Work 'SHA256SUMS.txt') -UseBasicParsing -TimeoutSec 120 } catch { Say "download SHA256SUMS.txt failed: $_" }
$sums = Get-Content (Join-Path $Work 'SHA256SUMS.txt') -ErrorAction SilentlyContinue
$want = @{}; foreach ($line in $sums) { $hash, $f = $line -split '  ', 2; $want[$f] = $hash }
# Downloads from GitHub's CDN occasionally break off: retry until the hash matches.
foreach ($f in $files) {
    $dest = Join-Path $Work $f
    for ($try = 1; $try -le 5; $try++) {
        try { Invoke-WebRequest "$base/$f" -OutFile $dest -UseBasicParsing -TimeoutSec 900 } catch { Say "download $f attempt $try failed: $_" }
        $h = (Get-FileHash $dest -Algorithm SHA256 -ErrorAction SilentlyContinue).Hash
        if ($h -and $h.ToLowerInvariant() -eq $want[$f]) { break }
        Start-Sleep 10
    }
}
foreach ($line in $sums) {
    $hash, $f = $line -split '  ', 2
    $actual = (Get-FileHash (Join-Path $Work $f) -Algorithm SHA256 -ErrorAction SilentlyContinue).Hash
    Check "download $f matches SHA256SUMS" ($actual -and $actual.ToLowerInvariant() -eq $hash) "$hash"
}
Check 'SHA256SUMS lists the four packages' (@($sums).Count -eq 4)

# ---- 2. Start clean: remove whatever Fennec is installed --------------------------------
Stop-Fennec
foreach ($p in Get-FennecProducts) { Say "removing installed $($p.DisplayName) $($p.DisplayVersion)"; Msi "/x $($p.PSChildName) /qn" 'uninstall previous' }

foreach ($variant in 'standard', 'experimental') {
    # ---- 3. Install the MSI -----------------------------------------------------------
    $msi = Join-Path $Work "fennec-$v-x64-$variant.msi"
    Msi "/i `"$msi`" /qn /l*v `"$Work\install-$variant.log`"" "${variant}: install"
    $svc = Get-Service FennecService -ErrorAction SilentlyContinue
    Check "${variant}: FennecService running, automatic" ($svc -and $svc.Status -eq 'Running' -and $svc.StartType -eq 'Automatic') "$($svc.Status) $($svc.StartType)"
    $prod = Get-FennecProducts | Select-Object -First 1
    $expectName = if ($variant -eq 'experimental') { 'Fennec (experimental)' } else { 'Fennec' }
    Check "${variant}: Apps entry '$expectName' $v" ($prod.DisplayName -eq $expectName -and $prod.DisplayVersion -eq $v) "$($prod.DisplayName) $($prod.DisplayVersion) $($prod.Publisher)"
    $fesV = (& "$pf\fes.exe" -version) -join ' '
    Check "${variant}: fes -version" ($fesV -eq "fes $v") $fesV
    $fennecV = (cmd /c "`"$pf\fennec.exe`" -version 2>&1") -join ' '
    Check "${variant}: fennec -version" ($fennecV -match "Fennec $([regex]::Escape($v))") $fennecV
    foreach ($k in 'Directory\shell\Fennec', '*\shell\FennecExplore') { reg query "HKLM\Software\Classes\$k" *> $null; Check "${variant}: context menu key $k" ($LASTEXITCODE -eq 0) }
    Check "${variant}: Start menu shortcut" (Test-Path 'C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Fennec.lnk')
    # Byte search (Select-String reads the binary as text and misses strings).
    $text = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes("$pf\fennec.exe"))
    $llama = [regex]::Matches($text, 'llama_[a-z_]{4,40}').Count
    Check "${variant}: llama.cpp linked = $($variant -eq 'experimental')" (($llama -gt 0) -eq ($variant -eq 'experimental')) "$llama llama_* strings"

    # ---- 4. fes -debug-crash writes a native crash report ------------------------------
    $inst = "smoke-$variant"
    $crashDir = Join-Path $appData "instances\$inst\logs"
    New-Item -ItemType Directory -Force $crashDir | Out-Null
    $p = Start-Process "$pf\fes.exe" -ArgumentList "-instance $inst -debug-crash" -Wait -PassThru -WindowStyle Hidden
    $crash = Get-ChildItem $crashDir -Filter 'crash-*-fes.log' -ErrorAction SilentlyContinue | Select-Object -Last 1
    $kind = if ($crash) { (Select-String -Path $crash.FullName -Pattern '^kind:' | Select-Object -First 1).Line } else { '' }
    Check "${variant}: fes -debug-crash -> exit 0xC0000005 and a crash report" (('{0:X8}' -f $p.ExitCode) -eq 'C0000005' -and $kind -match '0xC0000005') ("exit 0x{0:X8}; {1}; {2}" -f $p.ExitCode, $crash.Name, $kind)
    if ($crash) { Copy-Item $crash.FullName (Join-Path $ev "$variant-$($crash.Name)") }

    # ---- 5. Upgrade: a 0.1.0 data folder keeps bookmarks and filters -------------------
    $u = "upg010-$variant"
    $ud = Join-Path $appData "instances\$u"
    Remove-Item -Recurse -Force $ud -ErrorAction SilentlyContinue
    Copy-Item -Recurse $OldData $ud
    Remove-Item -Recurse -Force (Join-Path $ud 'logs') -ErrorAction SilentlyContinue
    $app = Start-Process "$pf\fennec.exe" -ArgumentList "-instance $u -startup" -PassThru
    $n = Wait-Ipc $u 180
    Check "${variant}: 0.1.0 data folder: index searchable ($n entries)" ($n -gt 0)
    # What the running app loaded (native IPC ListFilters / ListBookmarks).
    if (-not (Test-Path $IpcLister)) { Say "SKIP IPC filter/bookmark checks: $IpcLister not found" }
    $loadedLists = @(if (Test-Path $IpcLister) { & $IpcLister $u })
    $loadedLists | Set-Content -Encoding utf8 (Join-Path $ev "$variant-ipc-filters-bookmarks.txt")
    Check "${variant}: running 0.2.0 serves the 0.1.0 filter over IPC" (@($loadedLists | Where-Object { $_ -match '^filter\tREL03 Filter\trel03 ext:txt\trel03f$' }).Count -eq 1) ($loadedLists -join ' | ')
    Check "${variant}: running 0.2.0 serves the 0.1.0 bookmark over IPC" (@($loadedLists | Where-Object { $_ -match '^bookmark\tREL03 Bookmark\trel03 bookmark query ext:txt$' }).Count -eq 1)
    & "$pf\fes.exe" -instance $u -exit 2>$null | Out-Null
    $app.WaitForExit(60000) | Out-Null
    if (-not $app.HasExited) { Say 'fennec did not exit after fes -exit; killing'; Stop-Process -Id $app.Id -Force }
    $bm = Get-Content (Join-Path $ud 'bookmarks.toml') -Raw -ErrorAction SilentlyContinue
    $fl = Get-Content (Join-Path $ud 'filters.toml') -Raw -ErrorAction SilentlyContinue
    Check "${variant}: bookmarks.toml / filters.toml still hold them after exit" ($bm -match 'REL03 Bookmark' -and $fl -match 'macro = "rel03f"')
    $ulog = Get-ChildItem (Join-Path $ud 'logs') -Filter 'fennec*.log' | Select-Object -Last 1
    $loaded = Select-String -Path $ulog.FullName -Pattern 'index published.*loaded=true' -Quiet
    $set = @(Select-String -Path $ulog.FullName -Pattern 'unusable|unreadable|set aside|corrupt' | ForEach-Object { $_.Line })
    Check "${variant}: 0.1.0 index.fnx loaded directly, nothing set aside" ($loaded -and $set.Count -eq 0) ($set -join ' | ')
    $bak = @(Get-ChildItem $ud -Filter '*.bak' | Where-Object { $_.Name -match '\.v\d+\.bak$|corrupt' })
    Check "${variant}: no .v<N>.bak / .corrupt.bak created" ($bak.Count -eq 0) (($bak | ForEach-Object Name) -join ', ')
    Copy-Item (Join-Path $ud 'bookmarks.toml') (Join-Path $ev "$variant-bookmarks.toml"); Copy-Item (Join-Path $ud 'filters.toml') (Join-Path $ev "$variant-filters.toml")

    # ---- 6. Uninstall: nothing left (user data kept by default) ------------------------
    Stop-Fennec
    Msi "/x `"$msi`" /qn" "${variant}: uninstall"
    Leftovers $variant

    # ---- 7. Portable without the service: "cannot open arrived volume" logged once -----
    $pdir = Join-Path $Work "portable-$variant"
    Expand-Archive (Join-Path $Work "fennec-$v-x64-$variant-portable.zip") -DestinationPath $pdir -Force
    Check "${variant} portable: portable marker" (Test-Path (Join-Path $pdir 'portable'))
    # A standard-user token (runas /trustlevel Basic User): no volume handles, no service.
    runas /trustlevel:0x20000 "`"$pdir\fennec.exe`" -startup" | Out-Null
    Start-Sleep 200
    Get-Process fennec -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$pdir*" } | Stop-Process -Force
    Start-Sleep 2
    $plog = Get-ChildItem (Join-Path $pdir 'data\logs') -Filter 'fennec*.log' -ErrorAction SilentlyContinue | Select-Object -Last 1
    $lines = @(if ($plog) { Select-String -Path $plog.FullName -Pattern 'cannot open arrived volume' | ForEach-Object { $_.Line } })
    $vols = @($lines | ForEach-Object { if ($_ -match 'volume=(\S+)') { $Matches[1] } })
    $dupes = @($vols | Group-Object | Where-Object Count -gt 1)
    $mode = if ($plog) { (Select-String -Path $plog.FullName -Pattern 'starting|mode=|folder' | Select-Object -First 4 | ForEach-Object { $_.Line }) -join ' | ' } else { 'no log' }
    # settings.toml is written only when a setting changes; the log names the data folder.
    $dataLine = if ($plog) { Select-String -Path $plog.FullName -Pattern 'starting version=' | Select-Object -First 1 } else { $null }
    Check "${variant} portable: data in .\data" ($plog -and $dataLine -and $dataLine.Line.Contains("data=$pdir\data portable=true")) "$($dataLine.Line)"
    Check "${variant} portable (no service, 200 s): 'cannot open arrived volume' once per volume, not repeated" ($plog -and $lines.Count -ge 1 -and $dupes.Count -eq 0) ("{0} lines for {1}" -f $lines.Count, (($vols | Sort-Object -Unique) -join ','))
    Say "  log: $mode"
    if ($plog) { Copy-Item $plog.FullName (Join-Path $ev "$variant-portable-$($plog.Name)") }
}

# Leave the machine with the standard package installed (trial state).
Msi "/i `"$(Join-Path $Work "fennec-$v-x64-standard.msi")`" /qn" 'final: standard installed again'
Say ("RESULT: {0} failure(s)" -f $script:fails)
Set-Content (Join-Path $Work 'smoke.done') "fails=$($script:fails)"
