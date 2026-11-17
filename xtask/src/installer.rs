//! `cargo xtask installer`: ShareKeep's Windows setup (docs/specs/installers.md).
//!
//! Builds the programs, puts them with the licence files in `dist/ShareKeep-<version>`, signs them
//! when signing is set up, runs makensis on `installer/windows/sharekeep.nsi`, signs the setup,
//! and writes its checksums. The version numbering, signing and checksums are MoneyKeep's
//! (`xtask/src/release.rs` there), so the two families of setups behave alike.
//!
//! ```text
//! cargo xtask installer                    build, then make the setup (Windows)
//! cargo xtask installer --skip-build --bin <folder>
//!                                          make it from programs built already (any system
//!                                          with makensis: how the script is checked off Windows)
//! ```
//!
//! The version is the workspace's, or `SHAREKEEP_VERSION` (dev builds for MoneyKeep Launcher).
//! Signing: `SK_SIGN_THUMBPRINT` (a certificate in the user's store) or `SK_SIGN_CERT` (a .pfx
//! without a password, readable only by the user); `SK_SIGN_TIMESTAMP` the timestamp server.
//! A password is never put on a command line: `SK_SIGN_PASSWORD` is refused.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The programs in the setup: (cargo package, file name without `.exe`).
pub const PROGRAMS: [(&str, &str); 2] =
    [("sharekeep-app", "sharekeep"), ("sk-hub", "sharekeep-hub")];

pub fn exe(name: &str) -> String {
    format!("{name}.exe")
}

// ----- versions ---------------------------------------------------------------------------------

/// A semver version's parts: (major, minor, patch, pre-release identifiers); build metadata
/// (`+...`) is left out.
fn semver_parts(v: &str) -> Result<([u64; 3], Vec<&str>), String> {
    let v = v.split_once('+').map_or(v, |(a, _)| a);
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, p.split('.').collect()),
        None => (v, Vec::new()),
    };
    let nums: Vec<u64> = core
        .split('.')
        .map(|n| n.parse::<u64>())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("{v} is not a semver version (major.minor.patch)"))?;
    let &[a, b, c] = nums.as_slice() else {
        return Err(format!("{v} is not a semver version (major.minor.patch)"));
    };
    Ok(([a, b, c], pre))
}

/// The pre-release stage and number: `alpha.1` is (1, 1), `beta` (2, 0), `rc.2` (3, 2), and a
/// dev build `dev.5` (0, 5), below every alpha; a release is (9, 0).
fn stage(v: &str, pre: &[&str]) -> Result<(u64, u64), String> {
    let refuse = || {
        format!("{v}: the installer orders releases and dev.N, alpha.N, beta.N and rc.N pre-releases only")
    };
    let rank = match pre.first() {
        None => return Ok((9, 0)),
        Some(&"dev") => 0,
        Some(&"alpha") => 1,
        Some(&"beta") => 2,
        Some(&"rc") => 3,
        Some(_) => return Err(refuse()),
    };
    match pre.get(1..) {
        Some([]) | None => Ok((rank, 0)),
        Some([n]) => n.parse::<u64>().map(|n| (rank, n)).map_err(|_| refuse()),
        Some(_) => Err(refuse()),
    }
}

/// The four numbers Windows file versions use: `0.1.0-alpha.1` is `0.1.0.1`, `0.1.0` is
/// `0.1.0.0`.
pub fn version_quad(v: &str) -> Result<String, String> {
    let ([a, b, c], pre) = semver_parts(v)?;
    let (_, n) = stage(v, &pre)?;
    for x in [a, b, c, n] {
        if x > u64::from(u16::MAX) {
            return Err(format!("{v}: Windows versions take numbers up to 65535"));
        }
    }
    Ok(format!("{a}.{b}.{c}.{n}"))
}

/// Dotted numbers whose numeric comparison (NSIS's `VersionCompare`) is semver order:
/// `major.minor.patch.stage.number`.
pub fn version_order(v: &str) -> Result<String, String> {
    let ([a, b, c], pre) = semver_parts(v)?;
    let (st, n) = stage(v, &pre)?;
    Ok(format!("{a}.{b}.{c}.{st}.{n}"))
}

/// The workspace's version, or `SHAREKEEP_VERSION` when set.
pub fn version(root: &Path) -> Result<String, String> {
    if let Some(v) = std::env::var("SHAREKEEP_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        return Ok(v.trim().to_string());
    }
    let toml = std::fs::read_to_string(root.join("Cargo.toml"))
        .map_err(|e| format!("cannot read Cargo.toml: {e}"))?;
    let mut in_package = false;
    for line in toml.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_package = l == "[workspace.package]";
            continue;
        }
        if in_package {
            if let Some(v) = l.strip_prefix("version") {
                let v = v.trim().trim_start_matches('=').trim().trim_matches('"');
                return Ok(v.to_string());
            }
        }
    }
    Err("Cargo.toml has no [workspace.package] version".into())
}

/// The setup's file name, the one MoneyKeep Launcher installs.
pub fn setup_name(version: &str) -> String {
    format!("sharekeep-{version}-windows-x64-setup.exe")
}

// ----- checksums ---------------------------------------------------------------------------------

pub const SUMS_FILE: &str = "SHA256SUMS.txt";

pub fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// `<sha256>  <name>` lines (two spaces, as `sha256sum` writes).
pub fn sums_text(dir: &Path, names: &[String]) -> Result<String, String> {
    let mut text = String::new();
    for name in names {
        text.push_str(&sha256_file(&dir.join(name))?);
        text.push_str("  ");
        text.push_str(name);
        text.push('\n');
    }
    Ok(text)
}

// ----- makensis ---------------------------------------------------------------------------------

/// makensis's arguments (the `-` option form works on Windows and elsewhere).
pub fn makensis_args(
    version: &str,
    source: &Path,
    out_file: &Path,
    script: &Path,
) -> Result<Vec<OsString>, String> {
    let define = |k: &str, v: &str| OsString::from(format!("-D{k}={v}"));
    Ok(vec![
        OsString::from("-V2"),
        define("VERSION", version),
        define("VERSION_QUAD", &version_quad(version)?),
        define("VERSION_ORDER", &version_order(version)?),
        define("SOURCE_DIR", &source.display().to_string()),
        define("OUT_FILE", &out_file.display().to_string()),
        script.as_os_str().to_owned(),
    ])
}

/// makensis on `PATH`, or in NSIS's usual folders on Windows.
pub fn find_makensis() -> Option<PathBuf> {
    let names = ["makensis.exe", "makensis"];
    let on_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .flat_map(|d| names.iter().map(move |n| d.join(n)))
            .find(|p| p.is_file())
    });
    on_path.or_else(|| {
        ["ProgramFiles(x86)", "ProgramFiles"]
            .iter()
            .filter_map(std::env::var_os)
            .map(|d| PathBuf::from(d).join("NSIS").join("makensis.exe"))
            .find(|p| p.is_file())
    })
}

pub const NO_MAKENSIS: &str = "makensis (NSIS) was not found. Install NSIS 3 from https://nsis.sourceforge.io/Download (or: winget install NSIS.NSIS), open a new terminal, and run cargo xtask installer again.";

/// The licence page's text: NOTICE with Windows line ends.
pub fn licence_text(root: &Path) -> Result<String, String> {
    let notice = std::fs::read_to_string(root.join("NOTICE"))
        .map_err(|e| format!("cannot read NOTICE: {e}"))?;
    Ok(notice.replace("\r\n", "\n").replace('\n', "\r\n"))
}

// ----- signing ----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignWith {
    /// `SK_SIGN_THUMBPRINT`: a certificate in the user's certificate store.
    Thumbprint(String),
    /// `SK_SIGN_CERT`: a .pfx without a password, in a file only the user can read.
    File(OsString),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signing {
    pub with: SignWith,
    pub timestamp: String,
}

pub const SIGN_PASSWORD_REFUSED: &str = "SK_SIGN_PASSWORD is set, but signtool takes a certificate's password only on its command line, where other programs can read it. Install the certificate in your certificate store (certutil -user -importpfx <file>), set SK_SIGN_THUMBPRINT to its thumbprint and unset SK_SIGN_PASSWORD; or keep a copy without a password in a file only you can read and set SK_SIGN_CERT to it.";

/// Signing settings from the environment through `var` (the thumbprint wins over a file);
/// `None` when neither is set.
pub fn signing_from(var: impl Fn(&str) -> Option<OsString>) -> Result<Option<Signing>, String> {
    let get = |k: &str| var(k).filter(|v| !v.is_empty());
    if get("SK_SIGN_PASSWORD").is_some() {
        return Err(SIGN_PASSWORD_REFUSED.into());
    }
    let timestamp = get("SK_SIGN_TIMESTAMP")
        .map(|t| t.to_string_lossy().into_owned())
        .unwrap_or_else(|| "http://timestamp.digicert.com".to_owned());
    if let Some(t) = get("SK_SIGN_THUMBPRINT") {
        let t: String = t
            .to_string_lossy()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if t.is_empty() || !t.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("SK_SIGN_THUMBPRINT is not a certificate thumbprint (hex digits)".into());
        }
        return Ok(Some(Signing {
            with: SignWith::Thumbprint(t),
            timestamp,
        }));
    }
    Ok(get("SK_SIGN_CERT").map(|cert| Signing {
        with: SignWith::File(cert),
        timestamp,
    }))
}

/// signtool's arguments for `file`.
pub fn sign_args(s: &Signing, file: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "sign",
        "/fd",
        "SHA256",
        "/td",
        "SHA256",
        "/tr",
        s.timestamp.as_str(),
    ]
    .iter()
    .map(OsString::from)
    .collect();
    match &s.with {
        SignWith::Thumbprint(t) => {
            args.push("/sha1".into());
            args.push(t.into());
        }
        SignWith::File(f) => {
            args.push("/f".into());
            args.push(f.clone());
        }
    }
    args.push(file.as_os_str().to_owned());
    args
}

/// signtool on `PATH`, or the newest one in the Windows SDK.
fn find_signtool() -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join("signtool.exe"))
            .find(|p| p.is_file())
    });
    on_path.or_else(|| {
        let kits = PathBuf::from(std::env::var_os("ProgramFiles(x86)")?)
            .join("Windows Kits")
            .join("10")
            .join("bin");
        let mut found: Vec<PathBuf> = std::fs::read_dir(&kits)
            .ok()?
            .flatten()
            .map(|e| e.path().join("x64").join("signtool.exe"))
            .filter(|p| p.is_file())
            .collect();
        found.sort();
        found.pop()
    })
}

fn sign(tool: &Path, s: &Signing, file: &Path) -> Result<(), String> {
    println!("==> sign {}", file.display());
    let ok = Command::new(tool)
        .args(sign_args(s, file))
        .status()
        .map_err(|e| format!("cannot run signtool: {e}"))?
        .success();
    if ok {
        Ok(())
    } else {
        Err(format!("signing {} failed", file.display()))
    }
}

// ----- the task ---------------------------------------------------------------------------------

/// Copy a file, leaving the copy writable: a read-only checkout's licence files would
/// otherwise make the next build fail to replace them (Windows).
fn copy(from: &Path, to: &Path) -> Result<(), String> {
    let writable = |p: &Path| {
        if let Ok(m) = std::fs::metadata(p) {
            let mut perm = m.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            let _ = std::fs::set_permissions(p, perm);
        }
    };
    if to.exists() {
        writable(to);
    }
    std::fs::copy(from, to)
        .map_err(|e| format!("cannot copy {} to {}: {e}", from.display(), to.display()))?;
    writable(to);
    Ok(())
}

/// The folder makensis packs: the programs, the licences and the icon.
pub fn stage_folder(root: &Path, bin: &Path, folder: &Path, version: &str) -> Result<(), String> {
    std::fs::create_dir_all(folder)
        .map_err(|e| format!("cannot create {}: {e}", folder.display()))?;
    for (_, name) in PROGRAMS {
        copy(&bin.join(exe(name)), &folder.join(exe(name)))?;
    }
    for f in ["LICENSE-MIT", "LICENSE-APACHE", "NOTICE"] {
        copy(&root.join(f), &folder.join(f))?;
    }
    copy(
        &root.join("assets").join("icon.ico"),
        &folder.join("icon.ico"),
    )?;
    std::fs::write(folder.join("licence.txt"), licence_text(root)?)
        .map_err(|e| format!("cannot write licence.txt: {e}"))?;
    let (_, n) = crate::licences::write(root, folder, version)?;
    println!("==> {} lists {n} crates", crate::licences::FILE);
    Ok(())
}

/// `cargo xtask installer [--skip-build] [--bin <folder>]`.
pub fn installer(root: &Path, args: &[String]) -> Result<(), String> {
    let mut skip_build = false;
    let mut bin = root.join("target").join("release");
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--skip-build" => skip_build = true,
            "--bin" => bin = PathBuf::from(it.next().ok_or("--bin needs a folder")?),
            other => return Err(format!("installer: unknown option {other}")),
        }
    }
    if !cfg!(windows) && !skip_build {
        return Err("cargo xtask installer builds the Windows setup on Windows. Elsewhere, check the script with --skip-build --bin <folder with sharekeep.exe and sharekeep-hub.exe>.".into());
    }
    let makensis = find_makensis().ok_or(NO_MAKENSIS)?;
    let signing = match signing_from(|k| std::env::var_os(k))? {
        Some(s) => Some((
            find_signtool().ok_or("signing is set up (SK_SIGN_THUMBPRINT or SK_SIGN_CERT) but signtool.exe was not found: install the Windows SDK's signing tools, or put signtool on PATH")?,
            s,
        )),
        None => None,
    };
    let version = version(root)?;
    version_order(&version)?;
    if !skip_build {
        println!("==> cargo build --release");
        let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
        cmd.args(["build", "--release", "--locked"]);
        for (p, _) in PROGRAMS {
            cmd.args(["-p", p]);
        }
        if !cmd
            .current_dir(root)
            .status()
            .map_err(|e| e.to_string())?
            .success()
        {
            return Err("cargo build failed".into());
        }
    }
    let dist = root.join("dist");
    let folder = dist.join(format!("ShareKeep-{version}"));
    stage_folder(root, &bin, &folder, &version)?;
    if let Some((tool, s)) = &signing {
        for (_, name) in PROGRAMS {
            sign(tool, s, &folder.join(exe(name)))?;
        }
    }
    let setup = dist.join(setup_name(&version));
    println!("==> makensis installer/windows/sharekeep.nsi");
    let ok = Command::new(&makensis)
        .args(makensis_args(
            &version,
            &folder,
            &setup,
            &root.join("installer").join("windows").join("sharekeep.nsi"),
        )?)
        .current_dir(root)
        .status()
        .map_err(|e| format!("cannot run makensis: {e}"))?
        .success();
    if !ok || !setup.is_file() {
        return Err("makensis failed".into());
    }
    match &signing {
        Some((tool, s)) => sign(tool, s, &setup)?,
        None => println!("Not signed: Windows SmartScreen will warn on other computers."),
    }
    // after signing: the checksums are of the file that is published
    let name = setup_name(&version);
    let sums = sums_text(&dist, std::slice::from_ref(&name))?;
    std::fs::write(dist.join(SUMS_FILE), &sums).map_err(|e| e.to_string())?;
    std::fs::write(dist.join(format!("{name}.sha256")), &sums).map_err(|e| e.to_string())?;
    println!("\nxtask installer: {}", setup.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dotted_cmp(x: &str, y: &str) -> std::cmp::Ordering {
        let parts =
            |s: &str| -> Vec<u64> { s.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
        let (px, py) = (parts(x), parts(y));
        (0..px.len().max(py.len()))
            .map(|i| {
                px.get(i)
                    .copied()
                    .unwrap_or(0)
                    .cmp(&py.get(i).copied().unwrap_or(0))
            })
            .find(|o| o.is_ne())
            .unwrap_or(std::cmp::Ordering::Equal)
    }

    #[test]
    fn versions_for_windows_and_the_upgrade_check() {
        assert_eq!(version_quad("0.1.0").unwrap(), "0.1.0.0");
        assert_eq!(version_quad("0.1.0-alpha.1").unwrap(), "0.1.0.1");
        assert_eq!(version_quad("0.1.0-dev.5").unwrap(), "0.1.0.5");
        assert!(version_quad("1.2").is_err());
        assert!(version_quad("1.2.3-preview.1").is_err());
        // the upgrade check compares these in semver order
        let order = [
            "0.1.0-dev.5",
            "0.1.0-alpha.1",
            "0.1.0-alpha.2",
            "0.1.0-beta",
            "0.1.0-rc.1",
            "0.1.0",
            "0.1.1",
            "0.2.0",
        ];
        for w in order.windows(2) {
            let (a, b) = (version_order(w[0]).unwrap(), version_order(w[1]).unwrap());
            assert_eq!(
                dotted_cmp(&a, &b),
                std::cmp::Ordering::Less,
                "{} < {}",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn signing_never_takes_a_password() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        assert_eq!(signing_from(env(&[])).unwrap(), None);
        assert!(
            signing_from(env(&[("SK_SIGN_CERT", "c.pfx"), ("SK_SIGN_PASSWORD", "x")])).is_err()
        );
        assert!(signing_from(env(&[("SK_SIGN_THUMBPRINT", "not hex!")])).is_err());
        let s = signing_from(env(&[("SK_SIGN_THUMBPRINT", "AB 12 cd")]))
            .unwrap()
            .unwrap();
        assert_eq!(s.with, SignWith::Thumbprint("AB12cd".into()));
        let args: Vec<String> = sign_args(&s, Path::new("setup.exe"))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args.last().unwrap(), "setup.exe");
        assert!(args.contains(&"/sha1".to_string()));
    }

    /// The script compiles, with stand-in programs, wherever makensis is installed (the
    /// Windows CI job always; elsewhere skipped when it isn't).
    #[test]
    fn the_setup_script_compiles() {
        let Some(makensis) = find_makensis() else {
            eprintln!("makensis not found: skipped");
            return;
        };
        let root = crate::workspace_root();
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for (_, name) in PROGRAMS {
            std::fs::write(bin.join(exe(name)), b"MZ stand-in").unwrap();
        }
        let folder = tmp.path().join("ShareKeep-0.1.0");
        stage_folder(&root, &bin, &folder, "0.1.0").unwrap();
        let licences = std::fs::read_to_string(folder.join(crate::licences::FILE)).unwrap();
        assert!(licences.contains("slint"), "Slint is listed");
        let setup = tmp.path().join(setup_name("0.1.0"));
        let ok = Command::new(makensis)
            .args(
                makensis_args(
                    "0.1.0",
                    &folder,
                    &setup,
                    &root.join("installer").join("windows").join("sharekeep.nsi"),
                )
                .unwrap(),
            )
            .status()
            .unwrap()
            .success();
        assert!(ok && setup.is_file(), "makensis failed");
    }
}
