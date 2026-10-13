# Builds the portable zip and the MSI (SPEC ch.4 §9) into dist\.
#   powershell -ExecutionPolicy Bypass -File packaging\build.ps1 [-Wix D:\wix] [-SkipBuild]
param([string]$Wix = $env:WIX, [switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $SkipBuild) {
    cargo build --release -p fnc-app -p fnc-fes -p fnc-service
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}
$bin = Join-Path $root 'target\release'
$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null

# Portable: the `portable` marker keeps settings and the index in .\data.
$stage = Join-Path $dist "fennec-$version-x64-portable"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item "$bin\fennec.exe", "$bin\fes.exe", "$bin\fennec-service.exe", LICENSE-MIT, LICENSE-APACHE, README.md $stage
Set-Content -Path (Join-Path $stage 'portable') -Value '' -Encoding ascii
$zip = "$stage.zip"
Remove-Item -Force $zip -ErrorAction SilentlyContinue
Compress-Archive -Path "$stage\*" -DestinationPath $zip
Write-Host "portable: $zip"

# MSI.
if (-not $Wix -or -not (Test-Path (Join-Path $Wix 'candle.exe'))) {
    Write-Warning "WiX 3 not found (pass -Wix <folder> or set WIX); skipping the MSI"
    exit 0
}
$obj = Join-Path $dist 'obj'
New-Item -ItemType Directory -Force $obj | Out-Null
# The WiX license page needs RTF.
$rtf = Join-Path $obj 'license.rtf'
$mit = (Get-Content LICENSE-MIT -Raw) -replace '\\', '\\\\' -replace '\{', '\{' -replace '\}', '\}' -replace "`r?`n", '\par '
Set-Content -Path $rtf -Value "{\rtf1\ansi\deff0{\fonttbl{\f0 Segoe UI;}}\fs18 $mit}" -Encoding ascii
$icon = Join-Path $root 'assets\icon\fennec.ico'
# Quoted: PowerShell would pass `-dName=$var` unexpanded.
& (Join-Path $Wix 'candle.exe') -nologo -arch x64 "-dVersion=$version" "-dBinDir=$bin" "-dSrcDir=$root" "-dIconFile=$icon" `
    "-dLicenseRtf=$rtf" -out "$obj\fennec.wixobj" packaging\wix\fennec.wxs
if ($LASTEXITCODE -ne 0) { throw "candle failed" }
$msi = Join-Path $dist "fennec-$version-x64.msi"
& (Join-Path $Wix 'light.exe') -nologo -ext WixUIExtension -cultures:en-us -sval -out $msi "$obj\fennec.wixobj"
if ($LASTEXITCODE -ne 0) { throw "light failed" }
Write-Host "msi: $msi"
