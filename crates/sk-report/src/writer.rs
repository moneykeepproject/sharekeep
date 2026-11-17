//! The page writer (copied from GroupKeep's `pdf.rs`): US Letter, printpdf's built-in
//! Helvetica, so no font files are needed.

use printpdf::*;

pub(crate) const PAGE_W: f32 = 215.9;
pub(crate) const PAGE_H: f32 = 279.4;
pub(crate) const MARGIN: f32 = 16.0;
pub(crate) const CONTENT_W: f32 = PAGE_W - 2.0 * MARGIN;

/// Helvetica advance widths (1/1000 em) for ASCII 32..=126, from the standard AFM.
pub(crate) const HELV: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

pub(crate) fn text_width_mm(s: &str, size: f32, bold: bool) -> f32 {
    let units: u32 = s
        .chars()
        .map(|c| {
            let c = c as u32;
            if (32..=126).contains(&c) {
                HELV[(c - 32) as usize] as u32
            } else {
                556
            }
        })
        .sum();
    let w = units as f32 / 1000.0 * size * 0.352_778; // pt → mm
    if bold {
        w * 1.06
    } else {
        w
    }
}

/// Replace characters WinAnsi cannot show.
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{2013}' | '\u{2014}' => '-',
            '\u{2018}' | '\u{2019}' => '\'',
            '\u{201C}' | '\u{201D}' => '"',
            c if (c as u32) < 256 || "€£¥•…".contains(c) => c,
            _ => '?',
        })
        .collect()
}

fn wrap(s: &str, size: f32, bold: bool, width: f32) -> Vec<String> {
    let mut lines = vec![];
    for para in s.split('\n') {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{} {}", line, word)
            };
            if text_width_mm(&candidate, size, bold) > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                line = word.to_string();
            } else {
                line = candidate;
            }
        }
        lines.push(line);
    }
    lines
}

#[derive(Clone, Copy)]
pub(crate) enum Align {
    Left,
    Right,
}

pub(crate) struct Col<'a> {
    pub title: &'a str,
    pub width: f32,
    pub align: Align,
}

fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(Rgb::new(
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        None,
    ))
}

pub(crate) const INK: (u8, u8, u8) = (30, 41, 59);
pub(crate) const MUTED: (u8, u8, u8) = (100, 116, 139);
pub(crate) const ACCENT: (u8, u8, u8) = (15, 118, 110);
pub(crate) const ACCENT_BG: (u8, u8, u8) = (220, 242, 239);
pub(crate) const AMBER_BG: (u8, u8, u8) = (254, 243, 199);
pub(crate) const RULE: (u8, u8, u8) = (226, 232, 240);

pub(crate) struct Writer {
    pub(crate) doc: PdfDocumentReference,
    font: IndirectFontRef,
    bold: IndirectFontRef,
    pages: Vec<(PdfPageIndex, PdfLayerIndex)>,
    layer: PdfLayerReference,
    pub(crate) y: f32,
}

impl Writer {
    pub(crate) fn new(title: &str) -> Result<Writer, String> {
        let (doc, p, l) = PdfDocument::new(clean(title), Mm(PAGE_W), Mm(PAGE_H), "Layer 1");
        let font = doc
            .add_builtin_font(BuiltinFont::Helvetica)
            .map_err(|e| e.to_string())?;
        let bold = doc
            .add_builtin_font(BuiltinFont::HelveticaBold)
            .map_err(|e| e.to_string())?;
        let layer = doc.get_page(p).get_layer(l);
        Ok(Writer {
            doc,
            font,
            bold,
            pages: vec![(p, l)],
            layer,
            y: PAGE_H - MARGIN,
        })
    }

    pub(crate) fn new_page(&mut self) {
        let (p, l) = self.doc.add_page(Mm(PAGE_W), Mm(PAGE_H), "Layer 1");
        self.layer = self.doc.get_page(p).get_layer(l);
        self.pages.push((p, l));
        self.y = PAGE_H - MARGIN;
    }

    /// Make sure `h` mm fit before the bottom margin (leaving room for the footer).
    pub(crate) fn need(&mut self, h: f32) {
        if self.y - h < MARGIN + 8.0 {
            self.new_page();
        }
    }

    pub(crate) fn color(&self, c: (u8, u8, u8)) {
        self.layer.set_fill_color(rgb(c.0, c.1, c.2));
    }

    pub(crate) fn text_at(&self, s: &str, size: f32, x: f32, y: f32, bold: bool, c: (u8, u8, u8)) {
        self.color(c);
        let f = if bold { &self.bold } else { &self.font };
        self.layer.use_text(clean(s), size, Mm(x), Mm(y), f);
    }

    pub(crate) fn rect(&self, x: f32, y: f32, w: f32, h: f32, c: (u8, u8, u8)) {
        self.color(c);
        self.layer.add_rect(
            Rect::new(Mm(x), Mm(y), Mm(x + w), Mm(y + h)).with_mode(path::PaintMode::Fill),
        );
    }

    pub(crate) fn para(&mut self, s: &str, size: f32, bold: bool, c: (u8, u8, u8), indent: f32) {
        let lh = size * 0.42 + 1.2;
        for line in wrap(s, size, bold, CONTENT_W - indent) {
            self.need(lh);
            self.y -= lh;
            self.text_at(&line, size, MARGIN + indent, self.y, bold, c);
        }
    }

    pub(crate) fn heading(&mut self, s: &str) {
        self.need(16.0);
        self.y -= 6.0;
        self.rect(MARGIN, self.y - 1.6, 1.4, 6.0, ACCENT);
        self.text_at(s, 13.0, MARGIN + 3.5, self.y, true, ACCENT);
        self.y -= 3.0;
    }

    pub(crate) fn table(&mut self, cols: &[Col], rows: &[Vec<String>], highlight: &[bool]) {
        let size = 9.0;
        let lh = 5.2;
        let draw_header = |w: &mut Writer| {
            w.need(lh * 2.0);
            w.y -= lh;
            w.rect(MARGIN, w.y - 1.6, CONTENT_W, lh, ACCENT_BG);
            let mut x = MARGIN + 1.5;
            for c in cols {
                let tx = match c.align {
                    Align::Left => x,
                    Align::Right => x + c.width - 3.0 - text_width_mm(c.title, 8.5, true),
                };
                w.text_at(c.title, 8.5, tx, w.y, true, ACCENT);
                x += c.width;
            }
        };
        draw_header(self);
        for (ri, row) in rows.iter().enumerate() {
            // Wrap left-aligned cells.
            let cells: Vec<Vec<String>> = row
                .iter()
                .zip(cols)
                .map(|(s, c)| match c.align {
                    Align::Left => wrap(s, size, false, c.width - 3.0),
                    Align::Right => vec![s.clone()],
                })
                .collect();
            let n = cells.iter().map(|c| c.len()).max().unwrap_or(1);
            if self.y - lh * (n as f32) < MARGIN + 8.0 {
                self.new_page();
                draw_header(self);
            }
            let hl = highlight.get(ri).copied().unwrap_or(false);
            if hl {
                self.rect(
                    MARGIN,
                    self.y - lh * n as f32 - 1.6,
                    CONTENT_W,
                    lh * n as f32,
                    AMBER_BG,
                );
            }
            let top = self.y;
            let mut x = MARGIN + 1.5;
            for (ci, c) in cols.iter().enumerate() {
                for (li, line) in cells[ci].iter().enumerate() {
                    let y = top - lh * (li as f32 + 1.0);
                    let tx = match c.align {
                        Align::Left => x,
                        Align::Right => x + c.width - 3.0 - text_width_mm(line, size, false),
                    };
                    self.text_at(line, size, tx, y, false, INK);
                }
                x += c.width;
            }
            self.y = top - lh * n as f32;
            self.rect(MARGIN, self.y - 1.7, CONTENT_W, 0.2, RULE);
        }
        self.y -= 2.0;
    }

    pub(crate) fn footer(&self, group_name: &str) {
        let n = self.pages.len();
        for (i, (p, l)) in self.pages.iter().enumerate() {
            let layer = self.doc.get_page(*p).get_layer(*l);
            layer.set_fill_color(rgb(MUTED.0, MUTED.1, MUTED.2));
            let left = clean(&format!("ShareKeep • {}", group_name));
            layer.use_text(left, 8.0, Mm(MARGIN), Mm(MARGIN - 4.0), &self.font);
            let right = format!("Page {} of {}", i + 1, n);
            let w = text_width_mm(&right, 8.0, false);
            layer.use_text(
                right,
                8.0,
                Mm(PAGE_W - MARGIN - w),
                Mm(MARGIN - 4.0),
                &self.font,
            );
        }
    }
}
