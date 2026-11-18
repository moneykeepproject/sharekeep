# Installing, the hub as a service, releases (milestone 4d)

## Windows

`cargo xtask installer` (xtask/src/installer.rs) makes `dist/sharekeep-<version>-windows-x64-setup.exe`
with its `.sha256` and `SHA256SUMS.txt`: it builds the two programs, puts them in
`dist/ShareKeep-<version>` with the licences (`licence.txt` from NOTICE, `THIRD-PARTY-LICENSES.txt`
from `cargo metadata` and the crates' own licence files, xtask/src/licences.rs) and the icon, signs
the programs and the setup when `SK_SIGN_THUMBPRINT` or `SK_SIGN_CERT` is set (a password is never
put on a command line: `SK_SIGN_PASSWORD` is refused), and runs makensis on
`installer/windows/sharekeep.nsi`. The version numbering (`VERSION_QUAD`, `VERSION_ORDER` in semver
order), the signing rules and the checksums are MoneyKeep's, and the setup follows MoneyKeep's:

- per-machine in Program Files, with administrator permission; pages: welcome (which version is
  installed or updated, in semver order), licence, options, folder (skipped when updating),
  progress, finish, with no "Open ShareKeep" (Setup runs as administrator; ShareKeep should not);
- options: Start menu shortcut (on), Desktop shortcut (off), **Household hub on this computer**
  (off; on when this computer already is the hub, or with `/HUB`);
- ShareKeep open: asks to close it (Retry / Cancel); silently, asks it to close (taskkill without
  /F), waits up to 30 s, then stops with exit code 2;
- the hub: `net stop` (which waits) before its program is replaced; then
  `sharekeep-hub prepare --data …`: a data folder that is already there is used only if it and
  everything in it belong to Administrators, SYSTEM or a service account and nothing in it is a
  link or junction; otherwise it is renamed `… (set aside <date> <n>)` and never read (any
  user can create a folder in `%ProgramData%` before ShareKeep is installed); if it can't be
  moved the hub option stops with a message. The first time,
  `sharekeep-hub setup --data "%ProgramData%\ShareKeep Hub" --address auto`; then
  `sharekeep-hub install --data … --user <DOMAIN\user>`: `sc.exe` registers the service
  `ShareKeepHub` under its own virtual account `NT SERVICE\ShareKeepHub` (never LocalSystem),
  restarting on failure; `icacls.exe` locks the folder (Administrators own everything in it;
  `/reset` removes every permission anyone put on anything inside; the folder passes down only
  SYSTEM and Administrators full control and the service modify; the installing user may read
  `admin.token` and `hub.json` for the app's Hub tab; `/L` everywhere, so a link is never
  followed). The desktop app believes `admin.token` only if the folder and both files belong to
  Administrators, SYSTEM or a service; `netsh.exe` opens TCP 15443 and 15080 on private and domain
  networks; the service starts. An update keeps the certificate, so devices stay paired;
- the hub option turned off on a hub computer: its service and firewall rule are removed, the
  books stay;
- Installed apps: name, version (and `VersionOrder`), publisher, icon, size, location, the
  uninstaller and `QuietUninstallString`;
- the uninstaller removes the programs, shortcuts, registration, the service and its firewall
  rule; the household's books and the hub's data are never removed (it says where they are);
- silent (`/S`, MoneyKeep Launcher): no questions; the hub part only on a hub computer or with
  `/HUB`; exit code 0 done, 2 ShareKeep stayed open, 3 anything else.

No PowerShell anywhere in ShareKeep or its setup (MoneyKeep's `PSModulePath` lesson): sc.exe,
net.exe, tasklist.exe, taskkill.exe, icacls.exe and netsh.exe only. The service logs to `hub.log`
in the data folder (1 MB, then `hub.log.1`).

**Tested on Windows** by `.github/workflows/windows.yml` on every push: the setup task's tests,
then `cargo xtask installer` and, with `scripts/ci/windows-install.ps1`: install with the hub
(service account and state, data folder, firewall rule, Installed apps, shortcuts, the hub's
status); upgrade (still running, same certificate, hub kept without `/HUB`); upgrade with ShareKeep
open (Setup closes it); uninstall (service, rule, programs and registration gone; the data stays);
install again (same certificate). PowerShell runs only in that CI script.

The app finds the installed hub by itself (`%ProgramData%\ShareKeep Hub`), so the Hub tab
appears without `--hub-data`. The app shows Slint's "About Slint" in Settings > About, as the
Slint Royalty-free License 2.0 asks (NOTICE).

## macOS

`sharekeep-<version>-macos-<arch>.dmg` holds `ShareKeep.app` (both programs inside, signed ad hoc
until there is a Developer ID) and **Install the household hub.command**, which runs
`sharekeep-hub setup` as the user (data in `~/Library/Application Support/ShareKeep Hub`,
owner-only) and then writes launchd's file itself (the same file as
`sk_hub::service::macos_plist`; a test compares them) and, with sudo, only places it
(`/usr/bin/install -o root -g wheel -m 644` to
`/Library/LaunchDaemons/org.moneykeep.sharekeep-hub.plist`) and runs `launchctl`: nothing of
ShareKeep's runs as root (its programs are in an app the user owns). The daemon runs **as that
user**, starts at boot and restarts if it stops. **Stop the household hub.command** removes it
(the books stay). `sharekeep-hub install`/`uninstall` on a Mac only point to the two scripts.

## Keys

- The hub's private keys (`keys/*.key`): on Windows sealed by DPAPI for this computer
  (`sk_client::seal`, machine scope; the folder permissions keep other accounts out); on the Mac
  owner-only files. A key file from before sealing is sealed the first time it's read.
- A device's token (`device.token` beside the app's settings): on Windows sealed by DPAPI for the
  user; elsewhere owner-only. Settings files from 4b that held the token are moved on first start.
- Not yet: the macOS Keychain (a daemon running as the user can't reach the login keychain before
  login); a backup of a Windows hub's data folder restored on another PC needs its keys made again
  (`sharekeep-hub setup`) and its devices paired again.

## Releases

`.github/workflows/release.yml`: a pushed tag `v<version>` (on the `public` commit) or a manual
run makes a draft release; the Windows job (GitHub-hosted) builds and uploads the setup; the Mac
job runs on the project's Mac (runner group `mac-release`, environment `mac-release`, approval
needed; skipped with `macos: skip`) and uploads the DMG. Each file has a `.sha256` beside it. The
owner publishes the draft.

`packaging/<os>/package.(sh|ps1)` build the same files into `$DIST` for MoneyKeep Launcher's
`cargo xtask local sync` (they read `SHAREKEEP_VERSION` for dev builds); on Windows through
`cargo xtask installer`. The release's Windows job signs when the `SK_SIGN_PFX` secret (a .pfx
without a password, base64) is set, and warns that the setup is unsigned otherwise.

## MoneyKeep Launcher

ShareKeep is in the launcher's catalog (`keep-launcher`, `crates/core/src/catalog.rs`): asset
stem `sharekeep`, Windows `-setup.exe`, the DMG once the apps have a Developer ID (until then the
launcher opens a copy in Applications and links to the releases, as for the other apps).

## Known limits (review L14)

- The firewall rule covers private and domain networks only, never public ones (a hub must
  not answer on a café's Wi-Fi). Windows marks a new network public: Help and questions tells
  the household to make the home network private.
- Setup's hub option gives the installing account read access to `admin.token`; on a PC with a
  separate administrator account the everyday account then gets no Hub tab. "Make this
  computer the hub" in the app (0.1.4) passes the account running the app instead: run it from
  the everyday account (again, if the hub exists already) to fix this.
- The hub option needs ShareKeep in Program Files: the service runs the hub program from the
  install folder, and only Program Files is locked against ordinary programs.

## make-hub (0.1.4)

`sharekeep-hub make-hub --user <DOMAIN\user>` (Windows, elevated) does Setup's hub section in
one program: `prepare` (a planted folder moved aside), `setup --address auto` the first time
only, `install`, and `HKLM\Software\ShareKeep\HubData` (64-bit view, `reg.exe`). Setup's hub
section and the app's "Make this computer the hub" both run it, so the two routes can't drift
apart. It takes no `--data` (refused): the folder is always `%ProgramData%\ShareKeep Hub`, so
an ordinary program asking for the same prompt can't point it elsewhere. It refuses unless its
own program is under Program Files. `--user` must be `name` or `DOMAIN\name` (no quotes or
leading dash). Exit codes (`sk_client::makehub::Exit`): 0 done, 10 not under Program Files,
11 the folder can't be moved aside, 12 not on a home network, 13 setup failed, 14 the service
couldn't start, 15 called wrongly. The service being registered is what keeps the hub through
later silent updates (HubInstalled), so launcher updates need nothing more. windows.yml runs it:
a silent install without /HUB, then make-hub, then a silent upgrade that keeps it.

