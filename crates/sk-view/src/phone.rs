//! What a phone does to join the household: the steps the hub's certificate page shows on
//! the phone, and the desktop's EZ Start shows on the hub computer (step 5a–5f), from this one
//! copy so they never disagree. `**…**` marks words to show in bold.

/// Before downloading anything: the phone must be talking to the real hub (review M8).
pub const COMPARE: &str = "**Compare** the fingerprint on the page with the one on the hub computer, digit for digit. If they differ, stop: don't download anything.";

/// iPhone or iPad: trusting the hub's certificate.
pub const APPLE: [&str; 5] = [
    "Open the page in **Safari** (the Camera app opens Safari).",
    COMPARE,
    "Tap **Download certificate (iPhone or iPad)**, then **Allow**.",
    "Open **Settings**: tap **Profile Downloaded** near the top (or **General** > **VPN & Device Management**), then **Install**, and enter the phone's passcode.",
    "Open **Settings** > **General** > **About** > **Certificate Trust Settings** and turn on full trust for “ShareKeep Hub CA”.",
];

/// Android phone or tablet: trusting the hub's certificate.
pub const ANDROID: [&str; 6] = [
    COMPARE,
    "Tap **Download certificate (Android)**. If Chrome asks, keep the file.",
    "Open **Settings** > **Security** (or **Security & privacy**) > **More security settings** > **Encryption & credentials**. The words differ a little between phone makers; searching Settings for “certificate” also finds it.",
    "Tap **Install a certificate** > **CA certificate**, then **Install anyway**.",
    "Choose the downloaded file **sharekeep-ca.crt** (in Downloads). The phone may ask for its screen lock.",
    "Check under **Trusted credentials** > **User** that “ShareKeep Hub CA” is listed.",
];

/// The last thing the certificate page says when it wasn't opened from a pairing QR code.
pub const THEN_PAIR: &str =
    "Then scan the pairing QR code on the hub computer (ShareKeep > Hub > Pair a device).";

/// A step as plain text (the desktop app).
pub fn plain(step: &str) -> String {
    step.replace("**", "")
}

/// A step as HTML (the certificate page): escaped, with `**…**` in bold.
pub fn html(step: &str) -> String {
    let escaped = step
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let mut out = String::new();
    for (i, part) in escaped.split("**").enumerate() {
        if i % 2 == 1 {
            out.push_str("<b>");
            out.push_str(part);
            out.push_str("</b>");
        } else {
            out.push_str(part);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_read_as_plain_text_and_as_html() {
        assert_eq!(
            plain(APPLE[2]),
            "Tap Download certificate (iPhone or iPad), then Allow."
        );
        assert_eq!(
            html("Open **Settings** > **A & B**"),
            "Open <b>Settings</b> &gt; <b>A &amp; B</b>"
        );
        // every bold mark is closed
        for s in APPLE.iter().chain(ANDROID.iter()).chain([&THEN_PAIR]) {
            assert_eq!(s.matches("**").count() % 2, 0, "{s}");
        }
    }
}
