# MoneyKeep Launcher's `cargo xtask local sync` runs this to build ShareKeep's Windows setup into
# $env:DIST. $env:SHAREKEEP_VERSION, when set, replaces Cargo.toml's version (dev builds).
# Needs NSIS (makensis) installed.
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
Set-Location $root
$version = $env:SHAREKEEP_VERSION
if (-not $version) {
  $version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
}
$numeric = ($version -split '-')[0]
cargo build --release --locked -p sharekeep-app -p sk-hub
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$nsis = Join-Path ${env:ProgramFiles(x86)} 'NSIS\makensis.exe'
if (-not (Test-Path $nsis)) { $nsis = 'makensis' }
Push-Location installer\windows
& $nsis -V2 "-DVERSION=$numeric" "-DFULLVERSION=$version" "-DBIN=..\..\target\release" sharekeep.nsi
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Pop-Location
$dist = if ($env:DIST) { $env:DIST } else { Join-Path $root 'dist' }
New-Item -ItemType Directory -Force -Path $dist | Out-Null
Move-Item -Force "installer\windows\sharekeep-$version-windows-x64-setup.exe" $dist
