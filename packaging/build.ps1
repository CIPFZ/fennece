# Builds the release artifacts (SPEC ch.4 §9, REL-07) into dist\:
#   fennec-<version>-x64-standard.msi            fennec-<version>-x64-standard-portable.zip
#   fennec-<version>-x64-experimental.msi        fennec-<version>-x64-experimental-portable.zip
#   SHA256SUMS.txt (`sha256sum -c` format)
# standard = fnc-app --no-default-features (no semantic search / llama.cpp);
# experimental = default features (semantic search). fes.exe and fennec-service.exe do not
# depend on the variant: they are built once (target\standard) and shipped in both.
#   powershell -ExecutionPolicy Bypass -File packaging\build.ps1 [-Variant standard|experimental|all] [-Wix D:\wix] [-SkipBuild]
param(
    [string]$Wix = $env:WIX,
    [switch]$SkipBuild,
    [ValidateSet('standard', 'experimental', 'all')][string]$Variant = 'all'
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$variants = if ($Variant -eq 'all') { @('standard', 'experimental') } else { @($Variant) }
# Separate target dirs: the two fennec.exe builds must not overwrite each other.
$targets = @{ standard = Join-Path $root 'target\standard'; experimental = Join-Path $root 'target\experimental' }

if (-not $SkipBuild) {
    # Always the same command for target\standard, so fes/service are byte-identical
    # whichever variants are requested (features unify across the -p list).
    cargo build --release --target-dir $targets.standard --no-default-features -p fnc-app -p fnc-fes -p fnc-service
    if ($LASTEXITCODE -ne 0) { throw "cargo build (standard) failed" }
    if ($variants -contains 'experimental') {
        cargo build --release --target-dir $targets.experimental -p fnc-app
        if ($LASTEXITCODE -ne 0) { throw "cargo build (experimental) failed" }
    }
}
$shared = Join-Path $targets.standard 'release'
$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null

$haveWix = $Wix -and (Test-Path (Join-Path $Wix 'candle.exe'))
if (-not $haveWix) { Write-Warning "WiX 3 not found (pass -Wix <folder> or set WIX); skipping the MSIs" }
$obj = Join-Path $dist 'obj'
New-Item -ItemType Directory -Force $obj | Out-Null
if ($haveWix) {
    # The WiX license page needs RTF.
    $rtf = Join-Path $obj 'license.rtf'
    $mit = (Get-Content LICENSE-MIT -Raw) -replace '\\', '\\\\' -replace '\{', '\{' -replace '\}', '\}' -replace "`r?`n", '\par '
    Set-Content -Path $rtf -Value "{\rtf1\ansi\deff0{\fonttbl{\f0 Segoe UI;}}\fs18 $mit}" -Encoding ascii
}
$icon = Join-Path $root 'assets\icon\fennec.ico'

$artifacts = @()
foreach ($v in $variants) {
    $base = "fennec-$version-x64-$v"
    # Binaries of this variant: fennec.exe from its own target dir, fes/service shared.
    $bin = Join-Path $obj "bin-$v"
    Remove-Item -Recurse -Force $bin -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $bin | Out-Null
    Copy-Item (Join-Path $targets[$v] 'release\fennec.exe') $bin
    Copy-Item "$shared\fes.exe", "$shared\fennec-service.exe" $bin

    # Portable: the `portable` marker keeps settings and the index in .\data
    # (languages are embedded in fennec.exe).
    $stage = Join-Path $dist "$base-portable"
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $stage | Out-Null
    Copy-Item "$bin\fennec.exe", "$bin\fes.exe", "$bin\fennec-service.exe", LICENSE-MIT, LICENSE-APACHE, README.md $stage
    Set-Content -Path (Join-Path $stage 'portable') -Value '' -Encoding ascii
    $zip = "$stage.zip"
    Remove-Item -Force $zip -ErrorAction SilentlyContinue
    Compress-Archive -Path "$stage\*" -DestinationPath $zip
    Remove-Item -Recurse -Force $stage
    Write-Host "portable: $zip"
    $artifacts += $zip

    if ($haveWix) {
        # Quoted: PowerShell would pass `-dName=$var` unexpanded.
        & (Join-Path $Wix 'candle.exe') -nologo -arch x64 "-dVersion=$version" "-dVariant=$v" "-dBinDir=$bin" "-dSrcDir=$root" `
            "-dIconFile=$icon" "-dLicenseRtf=$rtf" -ext WixUtilExtension -out "$obj\fennec-$v.wixobj" packaging\wix\fennec.wxs
        if ($LASTEXITCODE -ne 0) { throw "candle failed ($v)" }
        $msi = Join-Path $dist "$base.msi"
        & (Join-Path $Wix 'light.exe') -nologo -ext WixUIExtension -ext WixUtilExtension -cultures:en-us -sval `
            -out $msi "$obj\fennec-$v.wixobj"
        if ($LASTEXITCODE -ne 0) { throw "light failed ($v)" }
        Remove-Item -Force ([IO.Path]::ChangeExtension($msi, 'wixpdb')) -ErrorAction SilentlyContinue
        Write-Host "msi: $msi"
        $artifacts += $msi
    }
}

# `<lowercase hex>  <file name>` with LF line ends, so `sha256sum -c SHA256SUMS.txt` works.
$sums = $artifacts | Sort-Object | ForEach-Object {
    "$((Get-FileHash -Algorithm SHA256 $_).Hash.ToLowerInvariant())  $(Split-Path -Leaf $_)"
}
[IO.File]::WriteAllText((Join-Path $dist 'SHA256SUMS.txt'), (($sums -join "`n") + "`n"), [Text.Encoding]::ASCII)
Get-Content (Join-Path $dist 'SHA256SUMS.txt')
exit 0
