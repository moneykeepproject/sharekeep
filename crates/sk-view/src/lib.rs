//! What the screens show, worked out from a book's [`sk_ledger::Ledger`]: plain Rust, tested
//! without a window, used by the desktop app's Slint screens and by the hub's JSON for the web
//! app.

pub mod demo;
pub mod phone;
pub mod view;

/// A name made safe to use in a file name.
pub fn file_safe(name: &str) -> String {
    let s: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.()&".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    if s.trim().is_empty() {
        "Agreement".into()
    } else {
        s
    }
}
