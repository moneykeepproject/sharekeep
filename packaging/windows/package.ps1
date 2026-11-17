# MoneyKeep Launcher's `cargo xtask local sync` runs this to build ShareKeep's Windows setup into
# $env:DIST. $env:SHAREKEEP_VERSION, when set, replaces Cargo.toml's version (dev builds).
# Needs NSIS (makensis). The setup itself is made by `cargo xtask installer`
# (xtask/src/installer.rs), the same as a release.
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
Set-Location $root
cargo xtask installer
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$dist = if ($env:DIST) { $env:DIST } else { Join-Path $root 'dist' }
if ((Resolve-Path $dist -ErrorAction SilentlyContinue).Path -ne (Join-Path $root 'dist')) {
  New-Item -ItemType Directory -Force -Path $dist | Out-Null
  Get-ChildItem (Join-Path $root 'dist') -Filter 'sharekeep-*-windows-x64-setup.exe*' |
    ForEach-Object { Copy-Item -Force $_.FullName $dist }
}
