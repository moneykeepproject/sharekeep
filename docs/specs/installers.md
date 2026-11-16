# Installing, the hub as a service, releases (milestone 4d)

## Windows

`sharekeep-<version>-windows-x64-setup.exe` (NSIS, `installer/windows/sharekeep.nsi`) installs
`sharekeep.exe` and `sharekeep-hub.exe` into Program Files, with Start-menu and desktop
shortcuts. The component **Household hub on this computer** (off by default; kept on when an
upgrade finds a hub) runs, elevated:

1. `sharekeep-hub setup --data "%ProgramData%\ShareKeep Hub" --address auto`, the first time
   only (the address is the one this PC uses on the home network; an upgrade keeps the
   certificate, so devices stay paired).
2. `sharekeep-hub install --data … --user <DOMAIN\user>`: `sc.exe` registers the service
   `ShareKeepHub` under its own virtual account `NT SERVICE\ShareKeepHub` (never LocalSystem),
   restarting on failure; `icacls.exe` makes Administrators the folder's owner, removes inherited
   permissions, gives SYSTEM and Administrators full control and the service modify, and lets the
   installing user list the folder and read only `admin.token` and `hub.json` (the app's Hub tab);
   `netsh.exe` opens TCP 15443 and 15080 for the hub program on private and domain networks; the
   service starts. No PowerShell (MoneyKeep's `PSModulePath` lesson).

The service logs to `hub.log` in the data folder (1 MB, then `hub.log.1`). Uninstalling removes
the service, the firewall rule and the program, never the books. MoneyKeep Launcher runs the
setup silently (`/S`).

The app finds the installed hub by itself (`%ProgramData%\ShareKeep Hub`), so the Hub tab
appears without `--hub-data`.

## macOS

`sharekeep-<version>-macos-<arch>.dmg` holds `ShareKeep.app` (both programs inside, signed ad hoc
until there is a Developer ID) and **Install the household hub.command**, which runs
`sharekeep-hub setup` as the user (data in `~/Library/Application Support/ShareKeep Hub`,
owner-only) and then, with sudo, `sharekeep-hub install`: a launchd daemon
(`/Library/LaunchDaemons/org.moneykeep.sharekeep-hub.plist`) that runs **as that user**, not root,
starts at boot and restarts if it stops.

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
`cargo xtask local sync` (they read `SHAREKEEP_VERSION` for dev builds).

## MoneyKeep Launcher

ShareKeep is in the launcher's catalog (`keep-launcher`, `crates/core/src/catalog.rs`): asset
stem `sharekeep`, Windows `-setup.exe`, the DMG once the apps have a Developer ID (until then the
launcher opens a copy in Applications and links to the releases, as for the other apps).
