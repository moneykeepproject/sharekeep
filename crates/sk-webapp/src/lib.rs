//! ShareKeep's web app for phones and PC browsers: a small installable web app (PWA) the hub
//! serves on its HTTPS port. The screens are plain HTML, CSS and JavaScript with no outside
//! code; everything the screens show is worked out on the hub (`/api/v1/view`, the expense
//! preview, the rule examples), by the same engine and view models as the desktop app.
//!
//! The files are built into the hub, so there is nothing to install or build separately.

/// A file of the web app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asset {
    pub content_type: &'static str,
    pub body: &'static [u8],
}

macro_rules! file {
    ($name:literal, $ctype:literal) => {
        (
            concat!("/", $name),
            Asset {
                content_type: $ctype,
                body: include_bytes!(concat!("../web/", $name)),
            },
        )
    };
}

/// Every file, by the path it's served at.
pub const FILES: [(&str, Asset); 6] = [
    file!("index.html", "text/html; charset=utf-8"),
    file!("app.css", "text/css; charset=utf-8"),
    file!("app.js", "text/javascript; charset=utf-8"),
    file!("sw.js", "text/javascript; charset=utf-8"),
    file!("manifest.webmanifest", "application/manifest+json"),
    file!("icon.svg", "image/svg+xml"),
];

/// The file served at `path` (`/` is the app itself).
pub fn asset(path: &str) -> Option<Asset> {
    let path = if path == "/" { "/index.html" } else { path };
    FILES.iter().find(|(p, _)| *p == path).map(|(_, a)| *a)
}

/// The security headers for the web app's files: its own files only, no inline script, no
/// framing, no referrer.
pub const HEADERS: [(&str, &str); 5] = [
    (
        "Content-Security-Policy",
        "default-src 'self'; img-src 'self' data: blob:; script-src 'self'; style-src 'self'; connect-src 'self'; manifest-src 'self'; worker-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
    ),
    ("X-Content-Type-Options", "nosniff"),
    ("Referrer-Policy", "no-referrer"),
    ("X-Frame-Options", "DENY"),
    // the service worker decides what's cached; the hub always answers fresh
    ("Cache-Control", "no-cache"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn text(path: &str) -> String {
        String::from_utf8(asset(path).unwrap().body.to_vec()).unwrap()
    }

    #[test]
    fn the_app_and_its_files_are_served() {
        assert_eq!(asset("/").unwrap(), asset("/index.html").unwrap());
        assert!(asset("/nope.js").is_none());
        assert!(asset("/../Cargo.toml").is_none());
        let html = text("/");
        for (p, _) in FILES
            .iter()
            .filter(|(p, _)| *p != "/index.html" && *p != "/sw.js")
        {
            assert!(
                html.contains(&format!("\"{p}\"")),
                "index.html doesn't use {p}"
            );
        }
        // the service worker caches exactly the app's files
        let sw = text("/sw.js");
        for (p, _) in FILES.iter().filter(|(p, _)| *p != "/sw.js") {
            assert!(sw.contains(&format!("'{p}'")), "sw.js doesn't cache {p}");
        }
    }

    #[test]
    fn no_inline_script_no_outside_code_no_html_injection() {
        let html = text("/");
        assert!(!html.contains("<script>"), "inline script breaks the CSP");
        assert!(!html.contains(" on"), "inline handlers break the CSP");
        for (p, a) in FILES {
            let t = String::from_utf8_lossy(a.body);
            assert!(
                !t.contains("http://") && !t.contains("https://") || p == "/icon.svg",
                "{p} reaches outside the hub"
            );
        }
        let js = text("/app.js");
        assert!(
            !js.contains("innerHTML"),
            "text goes in with textContent only"
        );
        assert!(!js.contains("eval("));
        // inline styles are blocked by the CSP too
        assert!(!js.contains("style:") && !html.contains("style="));
    }
}
