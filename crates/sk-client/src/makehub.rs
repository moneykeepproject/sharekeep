//! "Make this computer the hub" (0.1.4): what `sharekeep-hub make-hub` answers, and the rule
//! it and the app share about where the programs must be. The elevated program's output isn't
//! visible to the app, so the exit code is all it gets; both sides read it from here.

use std::path::{Path, PathBuf};

/// `sharekeep-hub make-hub`'s exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Done,
    /// The hub program isn't under Program Files (an ordinary program could have replaced it).
    NotInProgramFiles,
    /// A hub folder ShareKeep didn't make is there and can't be moved aside.
    FolderInTheWay,
    /// `setup --address auto` found no home network.
    NoHomeNetwork,
    /// The hub couldn't be set up for another reason.
    SetupFailed,
    /// The service couldn't be registered or started.
    ServiceDidntStart,
    /// Called wrongly (`--data` given, `--user` missing, not Windows).
    BadArguments,
}

impl Exit {
    pub const ALL: [Exit; 7] = [
        Exit::Done,
        Exit::NotInProgramFiles,
        Exit::FolderInTheWay,
        Exit::NoHomeNetwork,
        Exit::SetupFailed,
        Exit::ServiceDidntStart,
        Exit::BadArguments,
    ];

    pub fn code(self) -> u8 {
        match self {
            Exit::Done => 0,
            Exit::NotInProgramFiles => 10,
            Exit::FolderInTheWay => 11,
            Exit::NoHomeNetwork => 12,
            Exit::SetupFailed => 13,
            Exit::ServiceDidntStart => 14,
            Exit::BadArguments => 15,
        }
    }

    pub fn from_code(code: i64) -> Option<Exit> {
        Exit::ALL.into_iter().find(|e| i64::from(e.code()) == code)
    }

    /// What the app says (Setup says the same).
    pub fn message(self, data: &Path) -> String {
        match self {
            Exit::Done => "This computer is now the household's hub.".into(),
            Exit::NotInProgramFiles => "The hub needs ShareKeep installed in Program Files (the service runs it from there, where ordinary programs can't change it). Install ShareKeep with its setup and keep the suggested folder.".into(),
            Exit::FolderInTheWay => format!("The household hub's folder ({}) wasn't made by ShareKeep's setup and can't be moved aside. Move or delete that folder, then try again.", data.display()),
            Exit::NoHomeNetwork => "The hub couldn't be set up: this computer doesn't seem to be on the home network. Connect it to the home Wi-Fi or network, then try again.".into(),
            Exit::SetupFailed => format!("The hub couldn't be set up. {} may say more.", data.join("hub.log").display()),
            Exit::ServiceDidntStart => format!("The household hub service couldn't be started. {} may say more.", data.join("hub.log").display()),
            Exit::BadArguments => "ShareKeep couldn't ask Windows to make this computer the hub (an unexpected answer). Try again, or install with the setup's \"Household hub on this computer\" option.".into(),
        }
    }
}

/// Program Files on this computer (the 64-bit folder first).
pub fn program_files() -> Vec<PathBuf> {
    ["ProgramW6432", "ProgramFiles"]
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .collect()
}

/// Whether `exe` is inside one of `roots` (Windows paths compare without regard to case).
pub fn under(exe: &Path, roots: &[PathBuf]) -> bool {
    let lower = |p: &Path| -> Vec<String> {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect()
    };
    let e = lower(exe);
    roots.iter().any(|r| {
        let r = lower(r);
        !r.is_empty() && e.len() > r.len() && e[..r.len()] == r[..]
    })
}

/// The hub's data folder on Windows: `%ProgramData%\ShareKeep Hub`, never given by the caller.
pub fn windows_data() -> Option<PathBuf> {
    std::env::var_os("ProgramData").map(|p| PathBuf::from(p).join("ShareKeep Hub"))
}

/// A `--user` value the elevated program accepts: `DOMAIN\name` or `name`, nothing that could
/// read as another argument.
pub fn user_ok(user: &str) -> bool {
    !user.is_empty()
        && user.len() <= 256
        && !user.starts_with('-')
        // `"name\"` would escape the closing quote on Windows' command line
        && !user.ends_with('\\')
        && !user.contains(['"', ':', '\0', '\n', '\r', '*', '?'])
        && user.matches('\\').count() <= 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_round_trip_and_say_something() {
        for e in Exit::ALL {
            assert_eq!(Exit::from_code(i64::from(e.code())), Some(e));
            assert!(!e
                .message(Path::new("C:/ProgramData/ShareKeep Hub"))
                .is_empty());
        }
        assert_eq!(Exit::from_code(1), None);
        assert_eq!(Exit::Done.code(), 0);
    }

    #[test]
    fn only_programs_inside_program_files_count() {
        let roots = [PathBuf::from("/Program Files")];
        assert!(under(
            Path::new("/program files/ShareKeep/sharekeep-hub.exe"),
            &roots
        ));
        assert!(!under(Path::new("/Program Files"), &roots));
        assert!(!under(Path::new("/Program Files Evil/x.exe"), &roots));
        assert!(!under(
            Path::new("/Users/jerry/ShareKeep/sharekeep-hub.exe"),
            &roots
        ));
        assert!(!under(Path::new("/x.exe"), &[]));
    }

    #[test]
    fn a_user_name_cant_carry_arguments() {
        assert!(user_ok(r"JERRY-PC\jerry"));
        assert!(user_ok("jerry"));
        assert!(!user_ok(""));
        assert!(!user_ok("--data"));
        assert!(!user_ok("a\" --data \"x"));
        assert!(!user_ok(r"a\b\c"));
        assert!(!user_ok("jerry\\"));
    }
}
