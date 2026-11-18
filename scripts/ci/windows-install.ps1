<#
  The Windows setup run for real on a GitHub-hosted runner (administrator, services,
  %ProgramData%): one scenario of .github/workflows/windows.yml. The scenarios share one runner
  and run in order. Modelled on MoneyKeep's scripts/ci/windows-upgrade.ps1.

  -Scenario install     Setup /S /HUB: the app and the hub service; checks the service (its own
                        account, running), the data folder, the firewall rule, Installed apps,
                        the Start menu shortcut (and no desktop one), and the hub's status.
  -Scenario upgrade     Setup /S again: still running, same certificate (paired devices keep
                        working), the hub still ticked without /HUB.
  -Scenario app-open    ShareKeep open, then Setup /S: Setup asks it to close and carries on
                        (exit 0).
  -Scenario uninstall   uninstall.exe /S: the service, the firewall rule, the programs and the
                        registration go; the hub's data (the books) stays.
  -Scenario reinstall   Setup /S /HUB after uninstalling: the same certificate again.
  -Scenario clean       uninstall.exe /S, then the hub's data removed too (a fresh computer).
  -Scenario plain       Setup /S without /HUB (as MoneyKeep Launcher installs): no service.
  -Scenario make-hub    sharekeep-hub make-hub --user <this account>, as the app's "Make this
                        computer the hub" runs it (0.1.4): the same checks as install, the
                        HubData value, the data folder owned by the administrators; --data is
                        refused.
  -Scenario make-hub-upgrade  Setup /S without /HUB: the hub made by the app stays, with the
                        same certificate.

  This is a CI script: PowerShell runs here, never in ShareKeep or its setup. Needs
  `cargo xtask installer` first (dist\). Each scenario's notes go to $Artifacts\<scenario>.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('install', 'upgrade', 'app-open', 'uninstall', 'reinstall', 'clean', 'plain', 'make-hub', 'make-hub-upgrade')]
    [string]$Scenario,
    [string]$Artifacts = (Join-Path $env:RUNNER_TEMP 'sk-artifacts')
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Service = 'ShareKeepHub'
$Account = 'NT SERVICE\ShareKeepHub'
$Data = Join-Path $env:ProgramData 'ShareKeep Hub'
$InstallDir = Join-Path $env:ProgramFiles 'ShareKeep'
$UninstallKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\ShareKeep'
$Rule = 'ShareKeep Hub'
$Fingerprint = Join-Path $Artifacts 'fingerprint.txt'
$Out = Join-Path $Artifacts $Scenario
New-Item -ItemType Directory -Force -Path $Out | Out-Null

function Fail([string]$why) { throw "FAILED ($Scenario): $why" }
function Note([string]$line) { Write-Output $line; Add-Content -Path (Join-Path $Out 'steps.log') -Value $line }

function Get-Setup {
    $found = @(Get-ChildItem -Path (Join-Path $Root 'dist') -Filter 'sharekeep-*-windows-x64-setup.exe')
    if ($found.Count -ne 1) { Fail "expected one setup in dist\, found $($found.Count): run cargo xtask installer" }
    return $found[0].FullName
}

function Invoke-Setup([string[]]$Arguments, [int]$Expect = 0) {
    $setup = Get-Setup
    Note "> $(Split-Path $setup -Leaf) $($Arguments -join ' ')"
    $p = Start-Process -FilePath $setup -ArgumentList $Arguments -Wait -PassThru
    Note "  exit code $($p.ExitCode)"
    if ($p.ExitCode -ne $Expect) { Fail "Setup exited $($p.ExitCode), expected $Expect" }
}

function Get-Hub { Get-CimInstance -ClassName Win32_Service -Filter "Name='$Service'" }

function Assert-HubRunning {
    $s = Get-Hub
    if (-not $s) { Fail "the service $Service isn't installed" }
    if ($s.StartName -ne $Account) { Fail "the service runs as $($s.StartName), not $Account" }
    for ($i = 0; $i -lt 20 -and (Get-Hub).State -ne 'Running'; $i++) { Start-Sleep -Seconds 1 }
    if ((Get-Hub).State -ne 'Running') { Fail "the service is $((Get-Hub).State)" }
    Note "  service $Service running as $Account"
}

function Get-HubFingerprint {
    $status = & (Join-Path $InstallDir 'sharekeep-hub.exe') status --data $Data
    if ($LASTEXITCODE -ne 0) { Fail "sharekeep-hub status failed" }
    $status | Out-File -FilePath (Join-Path $Out 'status.json')
    $fp = ($status | ConvertFrom-Json).certificate.fingerprint
    if (-not $fp) { Fail "the hub's status has no certificate fingerprint" }
    return $fp
}

function Assert-FirewallRule([bool]$present) {
    $null = & netsh.exe advfirewall firewall show rule name="$Rule"
    $has = ($LASTEXITCODE -eq 0)
    # netsh exits 1 when the rule isn't there. GitHub's pwsh step ends with that last native exit
    # code, so a passed scenario would fail the step: clear it once it has been read.
    $global:LASTEXITCODE = 0
    if ($has -ne $present) { Fail "the firewall rule '$Rule' is $(if ($has) { 'there' } else { 'missing' })" }
    Note "  firewall rule '$Rule' $(if ($present) { 'there' } else { 'gone' })"
}

function Save-Logs {
    $log = Join-Path $Data 'hub.log'
    if (Test-Path $log) { Copy-Item $log (Join-Path $Out 'hub.log') -ErrorAction SilentlyContinue }
}

try {
    switch ($Scenario) {
        'install' {
            Invoke-Setup @('/S', '/HUB')
            foreach ($f in @('sharekeep.exe', 'sharekeep-hub.exe', 'THIRD-PARTY-LICENSES.txt', 'uninstall.exe')) {
                if (-not (Test-Path (Join-Path $InstallDir $f))) { Fail "$f isn't installed" }
            }
            Assert-HubRunning
            if (-not (Test-Path (Join-Path $Data 'hub.json'))) { Fail "the hub's data folder has no hub.json" }
            Assert-FirewallRule $true
            $k = Get-ItemProperty -Path $UninstallKey
            if (-not $k.DisplayVersion -or -not $k.VersionOrder -or -not $k.QuietUninstallString) { Fail "Installed apps lacks the version, its order or the silent uninstall" }
            Note "  Installed apps: ShareKeep $($k.DisplayVersion) ($($k.VersionOrder))"
            $menu = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\ShareKeep.lnk'
            $desk = Join-Path $env:PUBLIC 'Desktop\ShareKeep.lnk'
            if (-not (Test-Path $menu)) { Fail "no Start menu shortcut" }
            if (Test-Path $desk) { Fail "a desktop shortcut was made (it's off by default)" }
            Get-HubFingerprint | Out-File -FilePath $Fingerprint -NoNewline
            Note "  certificate $(Get-Content $Fingerprint)"
        }
        'upgrade' {
            # the hub stays ticked without /HUB: this computer is the hub
            Invoke-Setup @('/S')
            Assert-HubRunning
            $fp = Get-HubFingerprint
            if ($fp -ne (Get-Content $Fingerprint)) { Fail "the certificate changed: $fp" }
            Note "  same certificate: paired devices keep working"
        }
        'app-open' {
            $app = Start-Process -FilePath (Join-Path $InstallDir 'sharekeep.exe') -PassThru
            Start-Sleep -Seconds 5
            if ($app.HasExited) { Fail "ShareKeep didn't stay open (exit code $($app.ExitCode))" }
            Note "  ShareKeep is open (process $($app.Id))"
            Invoke-Setup @('/S')
            if (-not $app.HasExited) { Fail "ShareKeep is still open after Setup" }
            Assert-HubRunning
        }
        'uninstall' {
            $uninstaller = Join-Path $InstallDir 'uninstall.exe'
            Note "> uninstall.exe /S"
            # NSIS runs the uninstaller from a copy and returns at once: wait for the copy too
            $p = Start-Process -FilePath $uninstaller -ArgumentList @('/S', "_?=$InstallDir") -Wait -PassThru
            Note "  exit code $($p.ExitCode)"
            if ($p.ExitCode -ne 0) { Fail "the uninstaller exited $($p.ExitCode)" }
            Remove-Item -Force (Join-Path $InstallDir 'uninstall.exe') -ErrorAction SilentlyContinue
            Remove-Item -Force $InstallDir -ErrorAction SilentlyContinue
            if (Get-Hub) { Fail "the service is still installed" }
            Assert-FirewallRule $false
            if (Test-Path (Join-Path $InstallDir 'sharekeep.exe')) { Fail "the programs are still there" }
            if (Test-Path $UninstallKey) { Fail "Installed apps still lists ShareKeep" }
            if (-not (Test-Path (Join-Path $Data 'hub.json'))) { Fail "the hub's data (the books) was removed" }
            Note "  the hub's data stays in $Data"
        }
        'reinstall' {
            Invoke-Setup @('/S', '/HUB')
            Assert-HubRunning
            $fp = Get-HubFingerprint
            if ($fp -ne (Get-Content $Fingerprint)) { Fail "the certificate changed after reinstalling: $fp" }
            Note "  same certificate after uninstalling and installing again"
        }
        'clean' {
            $p = Start-Process -FilePath (Join-Path $InstallDir 'uninstall.exe') -ArgumentList @('/S', "_?=$InstallDir") -Wait -PassThru
            if ($p.ExitCode -ne 0) { Fail "the uninstaller exited $($p.ExitCode)" }
            Remove-Item -Force (Join-Path $InstallDir 'uninstall.exe') -ErrorAction SilentlyContinue
            Remove-Item -Force $InstallDir -ErrorAction SilentlyContinue
            Remove-Item -Recurse -Force $Data
            Remove-ItemProperty -Path 'HKLM:\SOFTWARE\ShareKeep' -Name 'HubData' -ErrorAction SilentlyContinue
            if (Get-Hub) { Fail "the service is still installed" }
            Note "  no ShareKeep and no hub data left"
        }
        'plain' {
            Invoke-Setup @('/S')
            if (Get-Hub) { Fail "a silent install without /HUB made a hub" }
            if (Test-Path (Join-Path $Data 'hub.json')) { Fail "a silent install without /HUB set up a hub" }
            Note "  the app only, as MoneyKeep Launcher installs it"
        }
        'make-hub' {
            $hub = Join-Path $InstallDir 'sharekeep-hub.exe'
            # the folder is never the caller's
            & $hub make-hub --user "$env:USERDOMAIN\$env:USERNAME" --data (Join-Path $env:TEMP 'elsewhere')
            if ($LASTEXITCODE -ne 15) { Fail "make-hub with --data exited $LASTEXITCODE, expected 15" }
            Note "> sharekeep-hub make-hub --user $env:USERDOMAIN\$env:USERNAME"
            & $hub make-hub --user "$env:USERDOMAIN\$env:USERNAME" | ForEach-Object { Note "  $_" }
            if ($LASTEXITCODE -ne 0) { Fail "make-hub exited $LASTEXITCODE" }
            $global:LASTEXITCODE = 0
            Assert-HubRunning
            if (-not (Test-Path (Join-Path $Data 'hub.json'))) { Fail "the hub's data folder has no hub.json" }
            Assert-FirewallRule $true
            $owner = (Get-Acl -Path $Data).Owner
            if ($owner -ne 'BUILTIN\Administrators') { Fail "the data folder belongs to $owner" }
            $v = (Get-ItemProperty -Path 'HKLM:\SOFTWARE\ShareKeep' -Name 'HubData').HubData
            if ($v -ne $Data) { Fail "HubData is '$v'" }
            Note "  HubData = $v; the folder belongs to $owner"
            Get-HubFingerprint | Out-File -FilePath $Fingerprint -NoNewline
            Note "  certificate $(Get-Content $Fingerprint)"
        }
        'make-hub-upgrade' {
            Invoke-Setup @('/S')
            Assert-HubRunning
            $fp = Get-HubFingerprint
            if ($fp -ne (Get-Content $Fingerprint)) { Fail "the certificate changed: $fp" }
            Note "  a hub made by the app stays through a silent update, same certificate"
        }
    }
    Note "PASSED ($Scenario)"
}
finally {
    Save-Logs
}
# A scenario that got here passed (a failure throws): don't let a native command's exit code
# from the checks above or from Save-Logs become the step's result.
exit 0
