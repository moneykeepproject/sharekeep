//! File dialogs. The only place in the app that uses `rfd` (a test checks).
//!
//! On macOS a blocking dialog runs a nested run loop inside winit's event handler, which
//! makes winit panic inside an Objective-C callback and abort the app (MoneyKeep 0.1.1's
//! crash when creating a book). So on the Mac the dialog is `rfd::AsyncFileDialog`, which
//! starts a sheet on the window and returns at once; the answer arrives later through
//! `slint::spawn_local`. Windows and Linux keep the plain blocking dialog.

use std::path::PathBuf;

/// Ask where to save a file, then call `then` with the answer (`None`: cancelled).
pub fn save_file(
    window: &slint::Window,
    title: &str,
    suggested: &str,
    filter: (&str, &[&str]),
    then: impl FnOnce(Option<PathBuf>) + 'static,
) {
    if cfg!(target_os = "macos") {
        let fut = rfd::AsyncFileDialog::new()
            .set_title(title)
            .set_file_name(suggested)
            .add_filter(filter.0, filter.1)
            .save_file();
        let _ = window;
        // Err only without an event loop, which the running app always has.
        let _ = slint::spawn_local(async move {
            then(fut.await.map(|f| f.path().to_path_buf()));
        });
    } else {
        let _ = window;
        then(
            rfd::FileDialog::new()
                .set_title(title)
                .set_file_name(suggested)
                .add_filter(filter.0, filter.1)
                .save_file(),
        );
    }
}

#[cfg(test)]
mod tests {
    /// Only this file may open a file dialog: anything else would bring back the Mac crash.
    #[test]
    fn rfd_is_used_only_here() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for e in std::fs::read_dir(&src).unwrap() {
            let p = e.unwrap().path();
            if p.file_name().is_some_and(|n| n == "dialogs.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap();
            assert!(
                !text.contains(concat!("rfd", "::")),
                "{} uses rfd directly",
                p.display()
            );
        }
        let ui = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/sk-ui/src");
        for e in std::fs::read_dir(&ui).unwrap() {
            let text = std::fs::read_to_string(e.unwrap().path()).unwrap();
            assert!(!text.contains(concat!("rfd", "::")));
        }
    }
}
