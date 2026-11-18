//! Help and questions: one list of answers, shown in the desktop app's Settings and on the hub's
//! help page for phones and PC browsers (`/help`, linked from the web app's Settings and from its
//! "couldn't start" screen). Plain text; the hub's page is drawn from it by [`page_html`].

/// One question and its answer, a paragraph per entry. `id` is the anchor on the help page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Faq {
    pub id: &'static str,
    pub question: &'static str,
    pub answer: &'static [&'static str],
}

/// The questions, most asked first.
pub const FAQ: &[Faq] = &[
    Faq {
        id: "which-hub",
        question: "Which computer should be the hub?",
        answer: &[
            "One computer in the household keeps the books: the hub. Pick one that stays on and stays on the home network, like a desktop. Every other phone and computer joins it.",
            "Choose it once: when ShareKeep opens on a computer that isn't set up yet, it asks \"Join my household's hub\" or \"Make this computer the hub\". Make only one computer the hub.",
        ],
    },
    Faq {
        id: "launcher-hub",
        question: "I installed from MoneyKeep Launcher and want this computer to be the hub",
        answer: &[
            "Open ShareKeep and choose Make this computer the hub. On Windows it asks once for administrator permission, then the hub runs in the background. On a Mac it opens Terminal, which asks for the Mac's password.",
            "If ShareKeep already opened the joining steps, go back to the first screen with Back on step 1.",
            "A computer that is already the hub can't join another household's hub. To stop being the hub: on Windows, run ShareKeep's setup again with \"Household hub on this computer\" turned off; on a Mac, open \"Stop the household hub\" from the disk image. The books stay.",
        ],
    },
    Faq {
        id: "blank",
        question: "A phone shows “ShareKeep” at the top and nothing under it",
        answer: &[
            "Close ShareKeep completely and open it again. If the page says “ShareKeep couldn't start on this device”, tap Try again; if it's still stuck, tap Start over on this device and pair the phone again.",
            "If the page stays empty, the browser is holding on to an old or damaged copy of ShareKeep. Restarting the hub, removing the home-screen icon or pairing again doesn't clear it. Clear the browser's data for ShareKeep (the next question), then pair the phone again.",
        ],
    },
    Faq {
        id: "clear",
        question: "How do I clear the browser's data for ShareKeep?",
        answer: &[
            "This removes only what the phone keeps for ShareKeep: its pairing and its copy of the book. The household's book lives on the hub computer and isn't touched. Anything entered on the phone that hadn't reached the hub yet is lost; enter it again afterwards.",
            "iPhone or iPad (Safari): open the Settings app, then Safari (on newer versions, Apps, then Safari), then Advanced, then Website Data. Find the hub's address, swipe left on it and tap Delete.",
            "Android (Chrome): open the hub's address in Chrome, tap the icon just left of the address, then Site settings (or Cookies and site data), then Delete data (or Clear & reset). Other browsers have the same, usually under Settings, then Privacy or Site settings.",
            "Quicker, but it reaches further: in the browser's menu, choose Clear browsing data (Delete browsing data), set the time range to cover when ShareKeep was last set up (Last hour is often enough), and tick Cookies and site data and Cached images and files. This also clears other sites visited in that time, which may sign you out of them.",
            "Then pair again: on the hub computer, open the Hub tab, pick the person under Pair a device, and scan the new code with the phone.",
        ],
    },
    Faq {
        id: "offline",
        question: "It says the household hub can't be reached",
        answer: &[
            "The phone has to be on the home Wi-Fi (not mobile data), and the hub computer has to be on and awake.",
            "On a Windows hub computer, the home network must be a private network: Settings, then Network & internet, then Wi-Fi (or Ethernet), then the network's properties, then Private network. Windows marks a new network public, and then it lets no phone reach the hub.",
            "Expenses, payments and approvals entered meanwhile wait on the phone and are sent when it reaches the hub. Settings, then Sync now, sends them at once.",
        ],
    },
    Faq {
        id: "code",
        question: "The pairing code doesn't work",
        answer: &[
            "A code works once, and only for 10 minutes. On the hub computer, open the Hub tab and pick the person under Pair a device again for a new code. The household PIN is the one set on the hub computer.",
        ],
    },
    Faq {
        id: "person",
        question: "A device is paired as the wrong person, or is being replaced",
        answer: &[
            "On that device, open Settings and tap Forget this device (twice), then pair it again with a code made for the right person. A new phone simply pairs with its own code.",
            "A lost or old device is removed on the hub computer: Hub tab, Devices, Remove.",
        ],
    },
    Faq {
        id: "lost",
        question: "Is anything lost if a phone is forgotten, reset or lost?",
        answer: &[
            "No. The household's book lives on the hub computer, and every paired device gets its copy from there. Only what a phone hadn't sent to the hub yet is lost with it.",
        ],
    },
];

/// The answers as plain text, for the desktop app.
pub fn plain_text() -> String {
    FAQ.iter()
        .map(|f| format!("{}\n{}", f.question, f.answer.join("\n")))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The hub's help page: no script, the web app's own style sheet, so it opens even when the app
/// itself can't start.
pub fn page_html() -> String {
    let mut out = String::from(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">\n\
         <meta name=\"theme-color\" content=\"#0f766e\">\n<title>ShareKeep help</title>\n\
         <link rel=\"icon\" href=\"/icon.svg\" type=\"image/svg+xml\">\n\
         <link rel=\"stylesheet\" href=\"/app.css\">\n</head>\n<body>\n\
         <header id=\"top\"><a class=\"link light\" href=\"/\">‹ ShareKeep</a><h1 id=\"title\">Help</h1></header>\n\
         <main id=\"page\">\n",
    );
    out.push_str("<div class=\"card\"><h2>Questions</h2>");
    for f in FAQ {
        out.push_str(&format!(
            "<p class=\"hint\"><a href=\"#{}\">{}</a></p>",
            f.id,
            esc(f.question)
        ));
    }
    out.push_str("</div>\n");
    for f in FAQ {
        out.push_str(&format!(
            "<div class=\"card\" id=\"{}\"><h2>{}</h2>",
            f.id,
            esc(f.question)
        ));
        for p in f.answer {
            out.push_str(&format!("<p>{}</p>", esc(p)));
        }
        out.push_str("</div>\n");
    }
    out.push_str(
        "<p class=\"hint\"><a href=\"/\">Back to ShareKeep</a></p>\n</main>\n</body>\n</html>\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_unique_and_page_has_every_question() {
        let page = page_html();
        for (i, f) in FAQ.iter().enumerate() {
            assert!(FAQ[..i].iter().all(|g| g.id != f.id), "{}", f.id);
            assert!(page.contains(&format!("id=\"{}\"", f.id)));
            assert!(page.contains(&esc(f.question)));
            assert!(!f.answer.is_empty());
        }
        assert!(!page.contains("<script"));
        assert!(plain_text().contains("Clear browsing data"));
    }
}
