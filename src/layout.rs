//! A small book typesetter.
//!
//! egui shapes individual words; everything above that is done here: Knuth–Plass line
//! breaking with hyphenation, first-line indents, small caps, baseline alignment and
//! pagination with widow, orphan and keep-with-next control.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use eframe::egui::epaint::text::{FontsView, LayoutJob, TextFormat, VariationCoords};
use eframe::egui::{Color32, FontFamily, FontId, Galley, Vec2, vec2};
use hyphenation::{Hyphenator, Standard};

use crate::book::{Align, Block, BlockKind, Book, Style};
use crate::theme::Typeface;

/// Everything that influences where words land on a page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub width: f32,
    pub height: f32,
    pub typeface: Typeface,
    pub size: f32,
    pub spacing: f32,
    pub justify: bool,
    pub hyphenate: bool,
    pub indent: bool,
}

impl Params {
    fn leading(&self) -> f32 {
        self.size * self.spacing
    }
}

pub struct Frag {
    /// Top-left of the fragment, relative to the start of its line.
    pub pos: Vec2,
    /// Laid out again through egui's galley cache whenever it is painted, so it always
    /// refers to the current glyph atlas; we only keep what is needed to place it.
    pub job: LayoutJob,
    width: f32,
    ascent: f32,
    descent: f32,
}

pub enum Content {
    Line(Vec<Frag>),
    Image { key: String, x: f32, size: Vec2 },
    Ornament,
}

pub struct Element {
    /// Top of the element in continuous chapter space.
    pub y: f32,
    pub height: f32,
    pub content: Content,
    pub block: usize,
    pub line: usize,
    pub lines: usize,
    pub keep_with_next: bool,
    /// Character offset in the chapter where this element begins.
    pub offset: usize,
}

pub struct Page {
    pub start: usize,
    pub end: usize,
    pub top: f32,
    pub offset: usize,
    /// Extra vertical offset, used to centre pages that only hold an illustration.
    pub shift: f32,
}

pub struct ChapterLayout {
    pub params: Params,
    pub elements: Vec<Element>,
    pub pages: Vec<Page>,
}

impl ChapterLayout {
    pub fn page_at(&self, offset: usize) -> usize {
        self.pages.partition_point(|p| p.offset <= offset).saturating_sub(1)
    }
}

// ---------------------------------------------------------------------------------------
// Shaping

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Fmt {
    mono: bool,
    italic: bool,
    weight: u16,
    /// Size in sixteenths of a point.
    size_q: u32,
    /// Letter spacing in hundredths of a point.
    tracking_q: i32,
}

impl Fmt {
    fn new(size: f32) -> Fmt {
        Fmt { mono: false, italic: false, weight: 400, size_q: (size * 16.0).round() as u32, tracking_q: 0 }
    }

    fn size(self) -> f32 {
        self.size_q as f32 / 16.0
    }

    fn with_size(mut self, size: f32) -> Fmt {
        self.size_q = (size * 16.0).round() as u32;
        self
    }
}

/// Caches shaped words for one typeface.
pub struct Shaper {
    typeface: Typeface,
    galleys: HashMap<(Fmt, String), Arc<Galley>>,
}

impl Shaper {
    pub fn new(typeface: Typeface) -> Shaper {
        Shaper { typeface, galleys: HashMap::new() }
    }

    fn format(&self, fmt: Fmt) -> TextFormat {
        let size = fmt.size();
        let family = if fmt.mono { FontFamily::Monospace } else { self.typeface.family(fmt.italic) };
        let mut coords = VariationCoords::default();
        if !fmt.mono {
            coords.push(b"wght", fmt.weight as f32);
            if let Some((lo, hi)) = self.typeface.opsz_range() {
                coords.push(b"opsz", size.clamp(lo, hi));
            }
        }
        TextFormat {
            font_id: FontId::new(size, family),
            color: Color32::PLACEHOLDER,
            coords,
            extra_letter_spacing: fmt.tracking_q as f32 / 100.0,
            ..Default::default()
        }
    }

    fn galley(&mut self, fonts: &mut FontsView<'_>, fmt: Fmt, text: &str) -> Arc<Galley> {
        if let Some(g) = self.galleys.get(&(fmt, text.to_owned())) {
            return g.clone();
        }
        let mut job = LayoutJob::default();
        job.append(text, 0.0, self.format(fmt));
        job.wrap.max_width = f32::INFINITY;
        let galley = fonts.layout_job(job);
        self.galleys.insert((fmt, text.to_owned()), galley.clone());
        galley
    }

    fn frag(&mut self, fonts: &mut FontsView<'_>, fmt: Fmt, text: &str) -> Frag {
        let galley = self.galley(fonts, fmt, text);
        let (ascent, descent) = metrics(&galley);
        let mut job = LayoutJob::default();
        job.append(text, 0.0, self.format(fmt));
        job.wrap.max_width = f32::INFINITY;
        Frag { pos: Vec2::ZERO, job, width: galley.size().x, ascent, descent }
    }

    fn width(&mut self, fonts: &mut FontsView<'_>, fmt: Fmt, text: &str) -> f32 {
        self.galley(fonts, fmt, text).size().x
    }

    fn space(&mut self, fonts: &mut FontsView<'_>, fmt: Fmt) -> f32 {
        let fmt = Fmt { tracking_q: 0, ..fmt };
        (self.width(fonts, fmt, "n n") - self.width(fonts, fmt, "nn")).max(fmt.size() * 0.2)
    }
}

/// Distance from the top of a single-line galley to its baseline, and the galley height.
fn metrics(galley: &Galley) -> (f32, f32) {
    let height = galley.size().y;
    let ascent = galley
        .rows
        .first()
        .and_then(|r| r.row.glyphs.first().map(|g| r.pos.y + g.pos.y))
        .unwrap_or(height * 0.8);
    (ascent, height - ascent)
}

// ---------------------------------------------------------------------------------------
// Paragraph building

struct Run {
    text: String,
    fmt: Fmt,
    rise: f32,
}

/// The typographic role of a block, before inline styling.
struct BlockFmt {
    fmt: Fmt,
    leading: f32,
    align: Align,
    inset_left: f32,
    inset_right: f32,
    indent: f32,
    hyphenate: bool,
    justify: bool,
    keep_with_next: bool,
}

fn runs_for(block: &Block, base: Fmt, lead_words: usize) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut words_seen = 0;
    let mut in_word = false;
    let size = base.size();

    let push = |text: char, fmt: Fmt, rise: f32, runs: &mut Vec<Run>| match runs.last_mut() {
        Some(last) if last.fmt == fmt && last.rise == rise => last.text.push(text),
        _ => runs.push(Run { text: text.to_string(), fmt, rise }),
    };

    for span in &block.spans {
        let Style { italic, bold, mono, small_caps, sup, sub } = span.style;
        let mut fmt = base;
        fmt.italic = base.italic ^ italic;
        if bold {
            fmt.weight = (base.weight + 250).min(800);
        }
        if mono {
            fmt.mono = true;
            fmt = fmt.with_size(size * 0.86);
        }
        let mut rise = 0.0;
        if sup || sub {
            fmt = fmt.with_size(fmt.size() * 0.68);
            rise = if sup { -size * 0.36 } else { size * 0.16 };
        }
        for ch in span.text.chars() {
            if ch.is_whitespace() {
                if in_word {
                    words_seen += 1;
                }
                in_word = false;
            } else {
                in_word = true;
            }
            let caps = small_caps || words_seen < lead_words;
            if caps && ch.is_lowercase() {
                let small = Fmt { tracking_q: (fmt.size() * 4.0) as i32, ..fmt.with_size(fmt.size() * 0.8) };
                for upper in ch.to_uppercase() {
                    push(upper, small, rise, &mut runs);
                }
            } else {
                let f = if caps { Fmt { tracking_q: (fmt.size() * 3.0) as i32, ..fmt } } else { fmt };
                push(ch, f, rise, &mut runs);
            }
        }
    }
    runs
}

#[derive(Clone, Debug)]
enum Kind {
    Indent,
    Box { run: usize, range: Range<usize> },
    Glue { stretch: f32, shrink: f32 },
    Penalty { cost: f32, flagged: bool, hyphen: bool },
}

#[derive(Clone, Debug)]
struct Item {
    w: f32,
    kind: Kind,
}

const FILL: f32 = 100_000.0;

impl Item {
    fn is_box(&self) -> bool {
        matches!(self.kind, Kind::Box { .. } | Kind::Indent)
    }

    fn is_forced(&self) -> bool {
        matches!(self.kind, Kind::Penalty { cost, .. } if cost == f32::NEG_INFINITY)
    }
}

struct Paragraph<'a> {
    runs: &'a [Run],
    items: Vec<Item>,
}

struct Setter<'s, 'f, 'v> {
    fonts: &'f mut FontsView<'v>,
    shaper: &'s mut Shaper,
    hyph: Option<&'s Standard>,
}

impl Setter<'_, '_, '_> {
    fn items(&mut self, runs: &[Run], bf: &BlockFmt, pre: bool) -> Vec<Item> {
        let mut items = Vec::new();
        if bf.indent > 0.0 {
            items.push(Item { w: bf.indent, kind: Kind::Indent });
        }
        let mut word: Vec<(usize, Range<usize>)> = Vec::new();
        for (ri, run) in runs.iter().enumerate() {
            let mut start = 0;
            for (bi, ch) in run.text.char_indices() {
                if ch == ' ' || ch == '\n' {
                    if bi > start {
                        word.push((ri, start..bi));
                    }
                    self.word(&mut items, runs, &word, bf);
                    word.clear();
                    if ch == ' ' {
                        let w = self.shaper.space(self.fonts, run.fmt);
                        let (stretch, shrink) = if pre { (0.0, 0.0) } else { (w * 0.6, w * 0.22) };
                        items.push(Item { w, kind: Kind::Glue { stretch, shrink } });
                    } else {
                        items.push(Item { w: 0.0, kind: Kind::Glue { stretch: FILL, shrink: 0.0 } });
                        items.push(Item {
                            w: 0.0,
                            kind: Kind::Penalty { cost: f32::NEG_INFINITY, flagged: false, hyphen: false },
                        });
                    }
                    start = bi + ch.len_utf8();
                }
            }
            if start < run.text.len() {
                word.push((ri, start..run.text.len()));
            }
        }
        self.word(&mut items, runs, &word, bf);
        items.push(Item { w: 0.0, kind: Kind::Glue { stretch: FILL, shrink: 0.0 } });
        items.push(Item {
            w: 0.0,
            kind: Kind::Penalty { cost: f32::NEG_INFINITY, flagged: false, hyphen: false },
        });
        items
    }

    /// Emit the boxes (and internal break opportunities) for one word.
    fn word(&mut self, items: &mut Vec<Item>, runs: &[Run], pieces: &[(usize, Range<usize>)], bf: &BlockFmt) {
        if pieces.is_empty() {
            return;
        }
        if let [(ri, range)] = pieces {
            let run = &runs[*ri];
            let text = &run.text[range.clone()];
            let breaks = self.breaks(text, bf);
            if !breaks.is_empty() {
                let galley = self.shaper.galley(self.fonts, run.fmt, text);
                let x_at = |byte: usize| -> f32 {
                    let ci = text[..byte].chars().count();
                    let row = &galley.rows[0].row;
                    row.glyphs.get(ci).map_or(row.size.x, |g| g.pos.x)
                };
                let hyphen_w = self.shaper.width(self.fonts, run.fmt, "-");
                let mut last = 0;
                let mut last_x = 0.0;
                for (at, explicit) in breaks {
                    let x = x_at(at);
                    items.push(Item {
                        w: x - last_x,
                        kind: Kind::Box { run: *ri, range: range.start + last..range.start + at },
                    });
                    let (w, cost) = if explicit { (0.0, 50.0) } else { (hyphen_w, 60.0) };
                    items.push(Item { w, kind: Kind::Penalty { cost, flagged: true, hyphen: !explicit } });
                    last = at;
                    last_x = x;
                }
                let total = galley.size().x;
                items.push(Item {
                    w: total - last_x,
                    kind: Kind::Box { run: *ri, range: range.start + last..range.end },
                });
                return;
            }
        }
        for (ri, range) in pieces {
            let run = &runs[*ri];
            let w = self.shaper.width(self.fonts, run.fmt, &run.text[range.clone()]);
            items.push(Item { w, kind: Kind::Box { run: *ri, range: range.clone() } });
        }
    }

    /// Byte offsets inside `word` where it may be broken, and whether the break follows
    /// an explicit dash (no hyphen added) or is a hyphenation point.
    fn breaks(&self, word: &str, bf: &BlockFmt) -> Vec<(usize, bool)> {
        let dashes: Vec<(usize, bool)> = word
            .char_indices()
            .filter(|&(i, c)| matches!(c, '-' | '–' | '—' | '/') && i > 0 && i + c.len_utf8() < word.len())
            .map(|(i, c)| (i + c.len_utf8(), true))
            .collect();
        if !dashes.is_empty() {
            return dashes;
        }
        let Some(hyph) = self.hyph.filter(|_| bf.hyphenate) else { return Vec::new() };
        // Only hyphenate the alphabetic core of the word, leaving punctuation attached.
        let Some(core_start) = word.find(char::is_alphabetic) else { return Vec::new() };
        let core_end = word
            .rfind(char::is_alphabetic)
            .map(|i| i + word[i..].chars().next().unwrap().len_utf8())
            .unwrap();
        let core = &word[core_start..core_end];
        if core.chars().count() < 6 || !core.chars().all(char::is_alphabetic) {
            return Vec::new();
        }
        if core.chars().next().is_some_and(char::is_uppercase) && core.chars().skip(1).any(char::is_uppercase)
        {
            return Vec::new(); // acronyms and small caps
        }
        hyph.hyphenate(core).breaks.into_iter().map(|b| (core_start + b, false)).collect()
    }

    fn set(
        &mut self,
        block: &Block,
        bf: &BlockFmt,
        width: f32,
        lead_words: usize,
    ) -> Vec<(f32, Vec<Frag>, usize)> {
        let runs = runs_for(block, bf.fmt, lead_words);
        let pre = matches!(block.kind, BlockKind::Pre);
        let para = Paragraph { items: self.items(&runs, bf, pre), runs: &runs };
        let lw = (width - bf.inset_left - bf.inset_right).max(bf.fmt.size() * 4.0);

        let breaks = knuth_plass(&para.items, lw, 2.0)
            .or_else(|| knuth_plass(&para.items, lw, 8.0))
            .unwrap_or_else(|| greedy(&para.items, lw));

        let mut lines = Vec::with_capacity(breaks.len());
        let mut start = 0;
        let mut chars = 0;
        for &b in &breaks {
            let s = start;
            start = next_start(&para.items, b);
            let (frags, n_chars) = self.line(&para, s, b, lw, bf);
            let (frags, height) = self.place_line(frags, bf);
            lines.push((height, frags, chars));
            chars += n_chars.max(1);
        }

        // List markers hang in the margin of the first line.
        if let (BlockKind::ListItem { marker }, Some(first)) = (&block.kind, lines.first_mut())
            && !marker.is_empty()
        {
            let mut frag = self.shaper.frag(self.fonts, bf.fmt, marker);
            let baseline = first.1.first().map_or(frag.ascent, |f| f.pos.y + f.ascent);
            frag.pos = vec2(bf.inset_left - frag.width - bf.fmt.size() * 0.5, baseline - frag.ascent);
            first.1.push(frag);
        }
        lines
    }

    /// Lay out the items `s..b` of a paragraph as one line, returning fragments with
    /// horizontal positions (vertical positions are filled in by `place_line`).
    fn line(
        &mut self,
        para: &Paragraph<'_>,
        s: usize,
        b: usize,
        lw: f32,
        bf: &BlockFmt,
    ) -> (Vec<(Frag, f32)>, usize) {
        let items = &para.items;
        let (mut natural, mut stretch, mut shrink) = (0.0, 0.0, 0.0);
        for item in &items[s..b] {
            match item.kind {
                Kind::Glue { stretch: st, shrink: sh } if st < FILL => {
                    natural += item.w;
                    stretch += st;
                    shrink += sh;
                }
                Kind::Glue { .. } | Kind::Penalty { .. } => {}
                _ => natural += item.w,
            }
        }
        let hyphen_at_end = matches!(items[b].kind, Kind::Penalty { hyphen: true, .. });
        if hyphen_at_end {
            natural += items[b].w;
        }
        let last_line = items[b].is_forced();
        let ratio = if natural > lw && shrink > 0.0 {
            ((lw - natural) / shrink).max(-1.0)
        } else if natural < lw && stretch > 0.0 && bf.justify && !last_line && bf.align == Align::Start {
            ((lw - natural) / stretch).min(3.0)
        } else {
            0.0
        };

        let mut pieces: Vec<(usize, Range<usize>, f32)> = Vec::new();
        let mut current: Option<(usize, Range<usize>, f32)> = None;
        let mut x = 0.0;
        let mut chars = 0;
        for item in &items[s..b] {
            match &item.kind {
                Kind::Indent => {
                    pieces.extend(current.take());
                    x += item.w;
                }
                Kind::Box { run, range } => {
                    chars += para.runs[*run].text[range.clone()].chars().count();
                    match &mut current {
                        Some((r, rg, _)) if r == run && rg.end == range.start => rg.end = range.end,
                        _ => {
                            pieces.extend(current.take());
                            current = Some((*run, range.clone(), x));
                        }
                    }
                    x += item.w;
                }
                Kind::Glue { stretch: st, shrink: sh } => {
                    pieces.extend(current.take());
                    chars += 1;
                    if *st < FILL {
                        x += item.w + if ratio >= 0.0 { ratio * st } else { ratio * sh };
                    }
                }
                Kind::Penalty { .. } => {}
            }
        }
        pieces.extend(current.take());
        let line_width = natural + if ratio < 0.0 { ratio * shrink } else { 0.0 };

        let offset = bf.inset_left
            + match bf.align {
                Align::Start => 0.0,
                Align::Center => ((lw - line_width) * 0.5).max(0.0),
                Align::End => (lw - line_width).max(0.0),
            };

        let n = pieces.len();
        let frags = pieces
            .into_iter()
            .enumerate()
            .map(|(i, (run, range, x))| {
                let run_ref = &para.runs[run];
                let mut text = run_ref.text[range].to_owned();
                if hyphen_at_end && i + 1 == n {
                    text.push('-');
                }
                let mut frag = self.shaper.frag(self.fonts, run_ref.fmt, &text);
                frag.pos.x = offset + x;
                (frag, run_ref.rise)
            })
            .collect();
        (frags, chars)
    }

    /// Align fragments on a common baseline and return the line height.
    fn place_line(&self, frags: Vec<(Frag, f32)>, bf: &BlockFmt) -> (Vec<Frag>, f32) {
        let (mut asc, mut desc) = (0.0f32, 0.0f32);
        for (frag, rise) in &frags {
            asc = asc.max(frag.ascent - rise);
            desc = desc.max(frag.descent + rise);
        }
        let height = bf.leading.max(asc + desc);
        let baseline = (height - (asc + desc)) * 0.5 + asc;
        let frags = frags
            .into_iter()
            .map(|(mut frag, rise)| {
                frag.pos.y = baseline + rise - frag.ascent;
                frag
            })
            .collect();
        (frags, height)
    }
}

/// Index of the first item that can begin a line after a break at `b`.
fn next_start(items: &[Item], b: usize) -> usize {
    let mut i = b + 1;
    while i < items.len() && !items[i].is_box() && !items[i].is_forced() {
        i += 1;
    }
    i
}

fn is_legal_break(items: &[Item], b: usize) -> bool {
    match items[b].kind {
        Kind::Penalty { .. } => true,
        Kind::Glue { .. } => b > 0 && items[b - 1].is_box(),
        _ => false,
    }
}

/// Total-fit line breaking (Knuth & Plass, 1981), without the refinements we don't need.
fn knuth_plass(items: &[Item], lw: f32, tolerance: f32) -> Option<Vec<usize>> {
    let n = items.len();
    let (mut sw, mut sy, mut sz) = (vec![0.0f32; n + 1], vec![0.0f32; n + 1], vec![0.0f32; n + 1]);
    for (i, item) in items.iter().enumerate() {
        let (w, y, z) = match item.kind {
            Kind::Glue { stretch, shrink } => (item.w, stretch, shrink),
            Kind::Penalty { .. } => (0.0, 0.0, 0.0),
            _ => (item.w, 0.0, 0.0),
        };
        sw[i + 1] = sw[i] + w;
        sy[i + 1] = sy[i] + y;
        sz[i + 1] = sz[i] + z;
    }

    struct Node {
        pos: usize,
        start: usize,
        demerits: f32,
        prev: usize,
        flagged: bool,
        fitness: u8,
    }
    let mut nodes =
        vec![Node { pos: usize::MAX, start: 0, demerits: 0.0, prev: usize::MAX, flagged: false, fitness: 1 }];
    let mut first_active = 0;

    for b in 0..n {
        if !is_legal_break(items, b) {
            continue;
        }
        let (pen_w, pen_cost, flagged) = match items[b].kind {
            Kind::Penalty { cost, flagged, .. } => (items[b].w, cost, flagged),
            _ => (0.0, 0.0, false),
        };
        let forced = pen_cost == f32::NEG_INFINITY;

        let mut best: Option<(f32, usize, u8)> = None;
        for k in (first_active..nodes.len()).rev() {
            let node = &nodes[k];
            if node.start > b {
                continue;
            }
            let natural = sw[b] - sw[node.start] + pen_w;
            let stretch = sy[b] - sy[node.start];
            let shrink = sz[b] - sz[node.start];
            if natural - shrink > lw + 0.01 {
                first_active = k + 1;
                break;
            }
            let ratio = if natural < lw {
                if stretch > 0.0 { (lw - natural) / stretch } else { f32::INFINITY }
            } else if natural > lw {
                (lw - natural) / shrink
            } else {
                0.0
            };
            if ratio > tolerance {
                continue;
            }
            let badness = 100.0 * ratio.abs().powi(3);
            let mut demerits = (10.0 + badness).powi(2);
            if pen_cost > 0.0 {
                demerits += pen_cost * pen_cost;
            }
            if flagged && node.flagged {
                demerits += 3000.0;
            }
            let fitness: u8 = match ratio {
                r if r < -0.5 => 0,
                r if r <= 0.5 => 1,
                r if r <= 1.0 => 2,
                _ => 3,
            };
            if !forced && fitness.abs_diff(node.fitness) > 1 {
                demerits += 3000.0;
            }
            let total = node.demerits + demerits;
            if best.is_none_or(|(t, _, _)| total < t) {
                best = Some((total, k, fitness));
            }
        }

        match best {
            Some((demerits, prev, fitness)) => {
                nodes.push(Node { pos: b, start: next_start(items, b), demerits, prev, flagged, fitness });
                if forced {
                    first_active = nodes.len() - 1;
                }
            }
            None if forced => return None,
            None => {}
        }
    }

    let mut breaks = Vec::new();
    let mut k = nodes.len() - 1;
    while k != 0 {
        breaks.push(nodes[k].pos);
        k = nodes[k].prev;
    }
    breaks.reverse();
    (breaks.last() == Some(&(n - 1))).then_some(breaks)
}

/// First-fit line breaking; always succeeds.
fn greedy(items: &[Item], lw: f32) -> Vec<usize> {
    let width = |s: usize, b: usize| -> f32 {
        let mut w = 0.0;
        for item in &items[s..b] {
            if !matches!(item.kind, Kind::Penalty { .. })
                && !matches!(item.kind, Kind::Glue { stretch, .. } if stretch >= FILL)
            {
                w += item.w;
            }
        }
        w + if matches!(items[b].kind, Kind::Penalty { .. }) { items[b].w } else { 0.0 }
    };
    let mut breaks = Vec::new();
    let mut start = 0;
    let mut last_fit: Option<usize> = None;
    for b in 0..items.len() {
        if !is_legal_break(items, b) {
            continue;
        }
        if width(start, b) > lw {
            if let Some(fit) = last_fit.take() {
                breaks.push(fit);
                start = next_start(items, fit);
            }
            if width(start, b) > lw && !items[b].is_forced() {
                breaks.push(b);
                start = next_start(items, b);
                continue;
            }
        }
        if items[b].is_forced() {
            breaks.push(b);
            start = next_start(items, b);
            last_fit = None;
        } else {
            last_fit = Some(b);
        }
    }
    breaks
}

// ---------------------------------------------------------------------------------------
// Chapters and pages

fn block_fmt(block: &Block, prev: Option<&Block>, p: &Params) -> BlockFmt {
    let size = p.size * p.typeface.size_scale();
    let em = p.size;
    let quote_inset = em * 1.6 * block.quote as f32;
    let mut bf = BlockFmt {
        fmt: Fmt::new(size),
        leading: p.leading(),
        align: block.align.unwrap_or(Align::Start),
        inset_left: quote_inset,
        inset_right: quote_inset,
        indent: 0.0,
        hyphenate: p.hyphenate,
        justify: p.justify,
        keep_with_next: false,
    };
    match &block.kind {
        BlockKind::Heading(level) => {
            let (scale, weight) = match level {
                1 => (1.6, 480),
                2 => (1.36, 480),
                3 => (1.16, 580),
                _ => (1.0, 640),
            };
            bf.fmt = Fmt::new(size * scale);
            bf.fmt.weight = weight;
            bf.leading = (size * scale * 1.28).max(p.leading());
            bf.align = block.align.unwrap_or(Align::Center);
            bf.hyphenate = false;
            bf.justify = false;
            bf.keep_with_next = true;
        }
        BlockKind::Caption => {
            bf.fmt = Fmt::new(size * 0.88);
            bf.fmt.italic = true;
            bf.leading = p.leading() * 0.9;
            bf.align = block.align.unwrap_or(Align::Center);
        }
        BlockKind::Pre => {
            bf.fmt = Fmt::new(size * 0.86);
            bf.fmt.mono = true;
            bf.leading = size * 1.35;
            bf.inset_left += em;
            bf.hyphenate = false;
            bf.justify = false;
        }
        BlockKind::ListItem { .. } => {
            bf.inset_left += em * 1.5 * block.list.max(1) as f32;
        }
        BlockKind::Paragraph => {
            let follows_paragraph = prev.is_some_and(|prev| {
                matches!(prev.kind, BlockKind::Paragraph)
                    && prev.quote == block.quote
                    && prev.align.unwrap_or(Align::Start) == Align::Start
            });
            if p.indent && follows_paragraph && !block.no_indent && bf.align == Align::Start {
                bf.indent = em * 1.4;
            }
        }
        BlockKind::Rule | BlockKind::Image(_) => {}
    }
    if bf.align != Align::Start {
        bf.hyphenate = false;
    }
    bf
}

fn space_between(prev: &Block, cur: &Block, p: &Params) -> f32 {
    use BlockKind::*;
    let l = p.leading();
    match (&prev.kind, &cur.kind) {
        (Heading(_), Heading(_)) => l * 0.35,
        (_, Heading(_)) => l * 1.6,
        (Heading(_), _) => l * 1.0,
        (Image(_), Caption) => l * 0.3,
        (Image(_), _) | (_, Image(_)) | (Caption, _) => l * 0.7,
        (Pre, Pre) => 0.0,
        (Pre, _) | (_, Pre) => l * 0.5,
        (ListItem { .. }, ListItem { .. }) => l * 0.15,
        (ListItem { .. }, _) | (_, ListItem { .. }) => l * 0.5,
        _ if prev.quote != cur.quote => l * 0.6,
        (Paragraph, Paragraph) => {
            let centred = prev.align.unwrap_or(Align::Start) != Align::Start
                || cur.align.unwrap_or(Align::Start) != Align::Start;
            if !p.indent || cur.no_indent && !centred { l * 0.6 } else { 0.0 }
        }
        _ => 0.0,
    }
}

pub fn layout_chapter(
    fonts: &mut FontsView<'_>,
    shaper: &mut Shaper,
    hyph: Option<&Standard>,
    book: &Book,
    index: usize,
    p: Params,
) -> ChapterLayout {
    let chapter = &book.chapters[index];
    let leading = p.leading();
    let mut setter = Setter { fonts, shaper, hyph };
    let mut elements: Vec<Element> = Vec::new();
    let mut prev: Option<&Block> = None;
    let mut y = 0.0;
    let mut opening = true; // still before the first paragraph of the chapter
    let mut saw_heading = false;

    for (bi, block) in chapter.blocks.iter().enumerate() {
        let offset = chapter.block_starts[bi];
        let gap = prev.map_or(0.0, |prev| space_between(prev, block, &p));
        match &block.kind {
            BlockKind::Rule => {
                y += gap.max(leading * 0.5);
                elements.push(Element {
                    y,
                    height: leading * 1.5,
                    content: Content::Ornament,
                    block: bi,
                    line: 0,
                    lines: 1,
                    keep_with_next: false,
                    offset,
                });
                y += leading * 1.5 + leading * 0.5;
            }
            BlockKind::Image(key) => {
                let Some(image) = book.images.get(key) else { continue };
                let [w, h] = image.size;
                let (w, h) = (w as f32, h as f32);
                let max_h = if chapter.blocks.len() == 1 { p.height } else { p.height * 0.92 };
                let scale = (p.width / w).min(max_h / h).min(1.0);
                let size = vec2(w * scale, h * scale);
                if !elements.is_empty() {
                    y += gap;
                }
                elements.push(Element {
                    y,
                    height: size.y,
                    content: Content::Image { key: key.clone(), x: (p.width - size.x) * 0.5, size },
                    block: bi,
                    line: 0,
                    lines: 1,
                    keep_with_next: false,
                    offset,
                });
                y += size.y;
            }
            _ => {
                let is_heading = matches!(block.kind, BlockKind::Heading(_));
                if is_heading && elements.is_empty() {
                    // Chapter openings sit a little way down the page.
                    y = p.height * 0.14;
                }
                let lead_words = if opening
                    && saw_heading
                    && block.kind == BlockKind::Paragraph
                    && block.align.unwrap_or(Align::Start) == Align::Start
                {
                    3
                } else {
                    0
                };
                if is_heading {
                    saw_heading = true;
                } else if block.kind == BlockKind::Paragraph {
                    opening = false;
                }
                let bf = block_fmt(block, prev, &p);
                if !elements.is_empty() {
                    y += gap;
                }
                let lines = setter.set(block, &bf, p.width, lead_words);
                let count = lines.len();
                for (li, (height, frags, chars)) in lines.into_iter().enumerate() {
                    elements.push(Element {
                        y,
                        height,
                        content: Content::Line(frags),
                        block: bi,
                        line: li,
                        lines: count,
                        keep_with_next: bf.keep_with_next,
                        offset: offset + chars.min(block.char_len() - 1),
                    });
                    y += height;
                }
            }
        }
        prev = Some(block);
    }

    let pages = paginate(&elements, p.height);
    ChapterLayout { params: p, elements, pages }
}

fn paginate(elements: &[Element], height: f32) -> Vec<Page> {
    let mut pages = Vec::new();
    let mut start = 0;
    let mut top = 0.0;
    let mut i = 0;
    while i < elements.len() {
        let e = &elements[i];
        if e.y + e.height > top + height + 0.5 && i > start {
            let mut brk = i;
            // Widow: never leave the last line of a paragraph alone at the top of a page.
            let widow = |k: usize| elements[k].lines >= 2 && elements[k].line == elements[k].lines - 1;
            if widow(brk) && brk - 1 > start && elements[brk - 1].block == elements[brk].block {
                brk -= 1;
            }
            // Orphan: nor the first line alone at the bottom.
            if brk - 1 > start && elements[brk - 1].line == 0 && elements[brk - 1].lines >= 2 {
                brk -= 1;
            }
            // Headings travel with what follows them.
            while brk - 1 > start && elements[brk - 1].keep_with_next {
                brk -= 1;
            }
            pages.push(Page { start, end: brk, top, offset: elements[start].offset, shift: 0.0 });
            start = brk;
            top = elements[brk].y;
            i = brk;
            continue;
        }
        i += 1;
    }
    pages.push(Page {
        start,
        end: elements.len(),
        top,
        offset: elements.get(start).map_or(0, |e| e.offset),
        shift: 0.0,
    });
    pages[0].offset = 0;

    for page in &mut pages {
        let els = &elements[page.start..page.end];
        if !els.is_empty()
            && els.iter().all(|e| matches!(e.content, Content::Image { .. } | Content::Ornament))
        {
            let last = els.last().unwrap();
            let content = last.y + last.height - els[0].y;
            page.top = els[0].y;
            page.shift = ((height - content) * 0.5).max(0.0);
        }
    }
    pages
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Words of the given widths separated by ordinary spaces.
    fn paragraph(widths: &[f32]) -> Vec<Item> {
        let mut items = Vec::new();
        for (i, &w) in widths.iter().enumerate() {
            if i > 0 {
                items.push(Item { w: 5.0, kind: Kind::Glue { stretch: 3.0, shrink: 1.0 } });
            }
            items.push(Item { w, kind: Kind::Box { run: 0, range: 0..0 } });
        }
        items.push(Item { w: 0.0, kind: Kind::Glue { stretch: FILL, shrink: 0.0 } });
        items.push(Item { w: 0.0, kind: Kind::Penalty { cost: f32::NEG_INFINITY, flagged: false, hyphen: false } });
        items
    }

    fn line_widths(items: &[Item], breaks: &[usize]) -> Vec<f32> {
        let mut start = 0;
        breaks
            .iter()
            .map(|&b| {
                let w = items[start..b]
                    .iter()
                    .filter(|i| !matches!(i.kind, Kind::Glue { stretch, .. } if stretch >= FILL))
                    .filter(|i| !matches!(i.kind, Kind::Penalty { .. }))
                    .map(|i| i.w)
                    .sum();
                start = next_start(items, b);
                w
            })
            .collect()
    }

    #[test]
    fn total_fit_breaks_fit_the_measure() {
        let items = paragraph(&[30.0, 22.0, 41.0, 18.0, 27.0, 35.0, 12.0, 44.0, 29.0, 31.0, 16.0, 38.0]);
        let breaks = knuth_plass(&items, 100.0, 2.0).expect("feasible");
        assert_eq!(*breaks.last().unwrap(), items.len() - 1);
        let lines = line_widths(&items, &breaks);
        for (i, w) in lines.iter().enumerate() {
            let spaces = 3.0; // generous bound on shrinkable spaces per line
            assert!(*w <= 100.0 + spaces * 5.0, "line {i} overfull: {w}");
        }
    }

    #[test]
    fn greedy_always_terminates_even_with_huge_words() {
        let items = paragraph(&[30.0, 250.0, 20.0]);
        assert!(knuth_plass(&items, 100.0, 8.0).is_none());
        let breaks = greedy(&items, 100.0);
        assert_eq!(*breaks.last().unwrap(), items.len() - 1);
        assert_eq!(breaks.len(), 3);
    }

    #[test]
    fn forced_breaks_are_honoured() {
        let mut items = paragraph(&[10.0, 10.0]);
        let tail = paragraph(&[10.0]);
        items.splice(1..1, [
            Item { w: 0.0, kind: Kind::Glue { stretch: FILL, shrink: 0.0 } },
            Item { w: 0.0, kind: Kind::Penalty { cost: f32::NEG_INFINITY, flagged: false, hyphen: false } },
        ]);
        let _ = tail;
        let breaks = knuth_plass(&items, 100.0, 2.0).expect("feasible");
        assert_eq!(breaks, vec![2, items.len() - 1]);
    }
}
