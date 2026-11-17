//! ShareKeep's statement PDFs: a closed period's statement, saved by the hub and sealed into
//! a Hyperbook by its SHA-256.

mod writer;

use sk_core::Currency;
use sk_ledger::Statement;
use writer::*;

fn period_label(s: &Statement) -> String {
    let f = |d: sk_core::NaiveDate| d.format("%b %-d, %Y").to_string();
    format!("{} to {}", f(s.period.first), f(s.period.last))
}

/// The statement as PDF bytes.
pub fn statement_pdf(s: &Statement, book: &str, currency: Currency) -> Result<Vec<u8>, String> {
    let money = |v: i64| currency.format(v);
    let title = format!("{} statement", s.agreement_name);
    let mut w = Writer::new(&title)?;

    // header band
    w.rect(0.0, PAGE_H - 34.0, PAGE_W, 34.0, ACCENT);
    w.text_at(
        "SHAREKEEP • MONEYKEEP FAMILY",
        8.0,
        MARGIN,
        PAGE_H - 11.0,
        true,
        (204, 251, 241),
    );
    w.text_at(&title, 20.0, MARGIN, PAGE_H - 20.0, true, (255, 255, 255));
    let sub = format!("{} • {} • {}", book, period_label(s), currency.label());
    w.text_at(&sub, 9.5, MARGIN, PAGE_H - 27.5, false, (220, 252, 245));
    w.y = PAGE_H - 40.0;

    w.para(
        &format!(
            "Shared expenses this period: {}. Closed by record {} in the book's history.",
            money(s.total),
            s.closed_by
        ),
        10.0,
        false,
        INK,
        0.0,
    );
    // the history's head at closing: every saved or printed copy witnesses the history
    w.para(
        &format!(
            "History at closing: record {} • fingerprint {}",
            s.closed_by, s.closed_hash
        ),
        7.5,
        false,
        MUTED,
        0.0,
    );

    w.heading("Who owes whom");
    if s.settle.is_empty() {
        w.para("Everyone is settled up.", 10.0, false, INK, 0.0);
    } else {
        let rows: Vec<Vec<String>> = s
            .settle
            .iter()
            .map(|(from, to, a)| vec![format!("{from} pays {to}"), money(*a)])
            .collect();
        w.table(
            &[
                Col {
                    title: "Payment",
                    width: CONTENT_W - 45.0,
                    align: Align::Left,
                },
                Col {
                    title: "Amount",
                    width: 45.0,
                    align: Align::Right,
                },
            ],
            &rows,
            &[],
        );
    }

    w.heading("Members");
    let rows: Vec<Vec<String>> = s
        .members
        .iter()
        .map(|m| {
            vec![
                m.name.clone(),
                money(m.paid),
                money(m.share),
                money(m.settled),
                money(m.adjusted),
                money(m.balance),
            ]
        })
        .collect();
    let num = |t: &'static str| Col {
        title: t,
        width: 25.0,
        align: Align::Right,
    };
    w.table(
        &[
            Col {
                title: "Member",
                width: CONTENT_W - 125.0,
                align: Align::Left,
            },
            num("Paid"),
            num("Share"),
            num("Settled"),
            num("Caps"),
            num("Balance"),
        ],
        &rows,
        &[],
    );
    w.para(
        "Balance: what the household owes the member (positive) or the member owes (negative) after this period, including earlier periods.",
        8.5,
        false,
        MUTED,
        0.0,
    );

    if !s.by_category.is_empty() {
        w.heading("By category");
        let rows: Vec<Vec<String>> = s
            .by_category
            .iter()
            .map(|(c, a)| vec![c.clone(), money(*a)])
            .collect();
        w.table(
            &[
                Col {
                    title: "Category",
                    width: CONTENT_W - 45.0,
                    align: Align::Left,
                },
                Col {
                    title: "Total",
                    width: 45.0,
                    align: Align::Right,
                },
            ],
            &rows,
            &[],
        );
    }

    w.heading("Entries");
    let rows: Vec<Vec<String>> = s
        .lines
        .iter()
        .map(|l| {
            vec![
                l.date.clone(),
                if l.disputed {
                    format!("{} (disputed)", l.what)
                } else {
                    l.what.clone()
                },
                l.by.clone(),
                money(l.amount),
            ]
        })
        .collect();
    let highlight: Vec<bool> = s.lines.iter().map(|l| l.disputed).collect();
    w.table(
        &[
            Col {
                title: "Date",
                width: 24.0,
                align: Align::Left,
            },
            Col {
                title: "What",
                width: CONTENT_W - 24.0 - 55.0 - 32.0,
                align: Align::Left,
            },
            Col {
                title: "Paid by",
                width: 55.0,
                align: Align::Left,
            },
            Col {
                title: "Amount",
                width: 32.0,
                align: Align::Right,
            },
        ],
        &rows,
        &highlight,
    );
    w.footer(&s.agreement_name);
    w.doc.save_to_bytes().map_err(|e| e.to_string())
}

/// A period's entries as a PDF: the same sheet as the spreadsheet export (`sk_view::view::
/// period_sheet`), for any period, open or closed: each person's opening, change and closing
/// balance, then every entry with what it moved for each person (deleted and disputed ones
/// highlighted), and in a Hyperbook the history's head at export.
pub fn period_pdf(
    sheet: &sk_view::view::PeriodSheet,
    book: &str,
    currency: Currency,
) -> Result<Vec<u8>, String> {
    let money = |v: i64| currency.format(v);
    let signed = |v: i64| match v {
        0 => String::new(),
        v if v > 0 => format!("+{}", currency.format(v)),
        v => currency.format(v),
    };
    let f = |d: sk_core::NaiveDate| d.format("%b %-d, %Y").to_string();
    let title = format!(
        "{}: {} to {}",
        sheet.agreement,
        f(sheet.first),
        f(sheet.last)
    );
    let mut w = Writer::new(&title)?;
    w.rect(0.0, PAGE_H - 34.0, PAGE_W, 34.0, ACCENT);
    w.text_at(
        "SHAREKEEP • MONEYKEEP FAMILY",
        8.0,
        MARGIN,
        PAGE_H - 11.0,
        true,
        (204, 251, 241),
    );
    w.text_at(
        &sheet.agreement,
        20.0,
        MARGIN,
        PAGE_H - 20.0,
        true,
        (255, 255, 255),
    );
    let sub = format!(
        "{} • {} to {} • {}",
        book,
        f(sheet.first),
        f(sheet.last),
        currency.label()
    );
    w.text_at(&sub, 9.5, MARGIN, PAGE_H - 27.5, false, (220, 252, 245));
    w.y = PAGE_H - 40.0;
    let closed = sheet.rows.iter().any(|r| r.what == "Period closed");
    w.para(
        if closed {
            "This period is closed. A positive balance means the household owes that person; a negative one, that they owe the household. The statement saved at closing is the record of the settlement."
        } else {
            "This period is still open: entries can still be added, changed or disputed until it closes. A positive balance means the household owes that person; a negative one, that they owe the household."
        },
        9.5,
        false,
        INK,
        0.0,
    );

    // balances: opening, change, closing for each person
    w.heading("Balances");
    let (Some(open), Some(close)) = (sheet.rows.first(), sheet.rows.last()) else {
        return Err("The period has no rows.".into());
    };
    let rows: Vec<Vec<String>> = sheet
        .people
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let o = open.balance.get(i).copied().unwrap_or(0);
            let c = close.balance.get(i).copied().unwrap_or(0);
            vec![p.clone(), money(o), signed(c - o), money(c)]
        })
        .collect();
    let num = 36.0;
    w.table(
        &[
            Col {
                title: "Person",
                width: CONTENT_W - 3.0 * num,
                align: Align::Left,
            },
            Col {
                title: "Opening",
                width: num,
                align: Align::Right,
            },
            Col {
                title: "This period",
                width: num,
                align: Align::Right,
            },
            Col {
                title: "Closing",
                width: num,
                align: Align::Right,
            },
        ],
        &rows,
        &[],
    );

    // the entries, with what each moved for each person
    w.heading("Entries");
    let entries: Vec<&sk_view::view::SheetRow> = sheet
        .rows
        .iter()
        .filter(|r| r.what != "Opening balances" && r.what != "Closing balances")
        .collect();
    if entries.is_empty() {
        w.para("No entries in this period.", 10.0, false, MUTED, 0.0);
    } else {
        let n = sheet.people.len().max(1) as f32;
        let date_w = 22.0;
        let amount_w = 26.0;
        let person_w = ((CONTENT_W - date_w - amount_w - 56.0) / n).clamp(16.0, 30.0);
        let what_w = CONTENT_W - date_w - amount_w - person_w * n;
        let mut cols = vec![
            Col {
                title: "Date",
                width: date_w,
                align: Align::Left,
            },
            Col {
                title: "What",
                width: what_w,
                align: Align::Left,
            },
            Col {
                title: "Amount",
                width: amount_w,
                align: Align::Right,
            },
        ];
        for p in &sheet.people {
            cols.push(Col {
                title: p.as_str(),
                width: person_w,
                align: Align::Right,
            });
        }
        let rows: Vec<Vec<String>> = entries
            .iter()
            .map(|r| {
                let mut what = r.what.clone();
                if !r.category.is_empty() {
                    what.push_str(&format!(" • {}", r.category));
                }
                if !r.by.is_empty() {
                    what.push_str(&format!(" • {}", r.by));
                }
                if !r.note.is_empty() {
                    what.push_str(&format!(" • {}", r.note));
                }
                let mut row = vec![
                    r.date.format("%b %-d").to_string(),
                    what,
                    r.amount.map(money).unwrap_or_default(),
                ];
                row.extend(r.change.iter().map(|c| signed(*c)));
                row
            })
            .collect();
        let flagged: Vec<bool> = entries.iter().map(|r| r.flagged).collect();
        w.table(&cols, &rows, &flagged);
        w.para(
            "The person columns show what each entry moved on that person's balance: + the household owes them more, - they owe more. Highlighted: deleted or disputed.",
            8.0,
            false,
            MUTED,
            0.0,
        );
    }
    if let Some(h) = sheet.head {
        w.para(
            &format!(
                "History at export: record {} • fingerprint {}",
                h.seq,
                h.hash.to_hex()
            ),
            7.5,
            false,
            MUTED,
            0.0,
        );
    }
    w.footer(&sheet.agreement);
    w.doc.save_to_bytes().map_err(|e| e.to_string())
}

/// A checkpoint to print or keep: the line, the fingerprint in groups for retyping, and a
/// QR code of the line. `line` is `sk_chain::checkpoint::text`.
pub fn checkpoint_pdf(
    book: &str,
    line: &str,
    seq: u64,
    fingerprint: &str,
) -> Result<Vec<u8>, String> {
    use qrcodegen::{QrCode, QrCodeEcc};
    let mut w = Writer::new("ShareKeep checkpoint")?;
    w.rect(0.0, PAGE_H - 34.0, PAGE_W, 34.0, ACCENT);
    w.text_at(
        "SHAREKEEP • MONEYKEEP FAMILY",
        8.0,
        MARGIN,
        PAGE_H - 11.0,
        true,
        (204, 251, 241),
    );
    w.text_at(
        "Checkpoint",
        20.0,
        MARGIN,
        PAGE_H - 20.0,
        true,
        (255, 255, 255),
    );
    w.text_at(
        &format!("{book} • record {seq}"),
        9.5,
        MARGIN,
        PAGE_H - 27.5,
        false,
        (220, 252, 245),
    );
    w.y = PAGE_H - 42.0;
    w.para(
        "Keep this page somewhere outside the house. Later, anyone in the household can check \
         that the book's history still contains this record, unchanged: ShareKeep > Settings > \
         Hyperbook > Check a checkpoint, then type or paste the line below, or scan the code.",
        10.0,
        false,
        INK,
        0.0,
    );
    w.heading("The checkpoint");
    w.para(line, 9.0, true, INK, 0.0);
    w.heading("The fingerprint, for typing");
    let groups: Vec<String> = fingerprint
        .as_bytes()
        .chunks(16)
        .map(|c| {
            c.chunks(4)
                .map(|g| String::from_utf8_lossy(g).into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    for g in groups {
        w.para(&g, 12.0, true, INK, 0.0);
    }
    w.para(
        &format!(
            "Record {seq}. Spaces, line breaks and capitals don't matter when you type it back."
        ),
        8.5,
        false,
        MUTED,
        0.0,
    );
    let qr_text = line.replace('·', "|");
    if let Ok(qr) = QrCode::encode_text(&qr_text, QrCodeEcc::Medium) {
        let n = qr.size();
        let side = 70.0_f32;
        let m = side / n as f32;
        w.need(side + 10.0);
        w.y -= side + 8.0;
        let x0 = (PAGE_W - side) / 2.0;
        // one rectangle per run of dark modules in a row keeps the file small
        for y in 0..n {
            let mut x = 0;
            while x < n {
                if qr.get_module(x, y) {
                    let start = x;
                    while x < n && qr.get_module(x, y) {
                        x += 1;
                    }
                    w.rect(
                        x0 + start as f32 * m,
                        w.y + (n - 1 - y) as f32 * m,
                        (x - start) as f32 * m + 0.05,
                        m + 0.05,
                        INK,
                    );
                } else {
                    x += 1;
                }
            }
        }
    }
    w.footer(book);
    w.doc.save_to_bytes().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sk_core::{AgreementId, DateSpan, MemberId, NaiveDate};
    use sk_ledger::statement::{Line, MemberLine};

    fn d(m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2027, m, day).unwrap()
    }

    #[test]
    fn a_statement_renders_on_as_many_pages_as_it_needs() {
        let s = Statement {
            agreement: AgreementId(1),
            agreement_name: "Apartment".into(),
            period: DateSpan::new(d(1, 1), d(1, 31)).unwrap(),
            closed_by: 42,
            closed_hash: "4fb9a7d544d82d55f05fdb3bab56d5c5345e5cfababd63eaa3f7d87e3ffd2489".into(),
            members: vec![MemberLine {
                member: MemberId(1),
                name: "Jerry".into(),
                paid: 45000,
                share: 17500,
                settled: 0,
                adjusted: 2500,
                balance: 27500,
            }],
            by_category: vec![("Utilities".into(), 45000)],
            total: 45000,
            lines: (0..120)
                .map(|i| Line {
                    seq: i,
                    date: "2027-01-10".into(),
                    what: "Groceries — weekly shop with a long description that wraps".into(),
                    by: "Jerry".into(),
                    amount: 1234,
                    disputed: i % 7 == 0,
                })
                .collect(),
            settle: vec![("Sam".into(), "Jerry".into(), 17500)],
        };
        let pdf = statement_pdf(&s, "Home", Currency::USD).unwrap();
        if let Ok(out) = std::env::var("SK_REPORT_SAMPLE") {
            std::fs::write(out, &pdf).unwrap();
        }
        assert!(pdf.starts_with(b"%PDF"));
        let text = String::from_utf8_lossy(&pdf);
        assert!(
            text.matches("/Type /Page").count() >= 3 || text.matches("/Type/Page").count() >= 3
        );
    }

    #[test]
    fn a_checkpoint_prints_as_a_one_page_pdf() {
        let fp = "ab".repeat(32);
        let line = format!("ShareKeep checkpoint · Home · record 17 · 2027-02-03 · {fp}");
        let pdf = checkpoint_pdf("Home", &line, 17, &fp).unwrap();
        assert!(pdf.starts_with(b"%PDF"));
        assert!(pdf.len() < 200_000, "{} bytes", pdf.len());
        std::fs::write(std::env::temp_dir().join("sk-checkpoint-test.pdf"), &pdf).unwrap();
    }

    #[test]
    fn a_period_exports_as_a_pdf() {
        let today = sk_core::NaiveDate::from_ymd_opt(2027, 3, 14).unwrap();
        let l = sk_view::demo::ledger(today);
        let first = sk_core::NaiveDate::from_ymd_opt(2027, 3, 1).unwrap();
        let sheet = sk_view::view::period_sheet(&l, sk_core::AgreementId(1), first, None).unwrap();
        assert!(sheet.rows.len() > 2, "the sample period has entries");
        let pdf = period_pdf(&sheet, "Home", Currency::USD).unwrap();
        assert!(pdf.starts_with(b"%PDF"));
        std::fs::write(std::env::temp_dir().join("sk-period-test.pdf"), &pdf).unwrap();
    }
}
