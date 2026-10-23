//! Loading an EPUB and distilling its XHTML into a small, typographic document model.
//!
//! We deliberately keep only what matters for reading: paragraphs, headings, emphasis,
//! lists, quotations, images and scene breaks. Publisher CSS is consulted just enough to
//! pick up italics, bold, small caps, centring and hidden elements.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use eframe::egui::ColorImage;
use epub::doc::{EpubDoc, NavPoint};
use scraper::{ElementRef, Html, Node};

use crate::picture;

/// Inline character styling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub italic: bool,
    pub bold: bool,
    pub mono: bool,
    pub small_caps: bool,
    pub sup: bool,
    pub sub: bool,
}

#[derive(Clone, Debug)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BlockKind {
    Paragraph,
    Heading(u8),
    Pre,
    ListItem { marker: String },
    Caption,
    Rule,
    Image(String),
}

#[derive(Clone, Debug)]
pub struct Block {
    pub kind: BlockKind,
    pub spans: Vec<Span>,
    pub align: Option<Align>,
    /// Blockquote nesting depth.
    pub quote: u8,
    /// List nesting depth.
    pub list: u8,
    /// The publisher asked for no first-line indent.
    pub no_indent: bool,
    /// The publisher left a wide gap above: a break between scenes.
    pub break_before: bool,
}

impl Block {
    /// Length in characters, never zero so every block owns a distinct position.
    pub fn char_len(&self) -> usize {
        self.spans.iter().map(|s| s.text.chars().count()).sum::<usize>().max(1)
    }

    pub fn plain_text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

pub struct Chapter {
    pub blocks: Vec<Block>,
    /// Character offset at which each block begins.
    pub block_starts: Vec<usize>,
    pub chars: usize,
}

#[derive(Clone, Debug)]
pub struct TocEntry {
    pub label: String,
    pub chapter: usize,
    pub offset: usize,
    pub depth: usize,
}

pub struct Book {
    pub title: String,
    pub author: Option<String>,
    pub language: Option<String>,
    pub chapters: Vec<Chapter>,
    pub chapter_starts: Vec<usize>,
    pub total_chars: usize,
    pub toc: Vec<TocEntry>,
    pub images: HashMap<String, BookImage>,
}

/// An illustration, kept compressed until it is about to be shown.
#[derive(Clone)]
pub struct BookImage {
    pub bytes: Arc<[u8]>,
    pub size: [usize; 2],
}

impl BookImage {
    fn new(bytes: Vec<u8>) -> Option<BookImage> {
        let (w, h) = image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format()
            .ok()?
            .into_dimensions()
            .ok()?;
        Some(BookImage { bytes: bytes.into(), size: [w as usize, h as usize] })
    }

    /// Decode to pixels, scaling very large images down, as a [`picture::ladder`]. Slow; call
    /// off the UI thread.
    pub fn decode(&self) -> Option<Vec<ColorImage>> {
        let image = image::load_from_memory(&self.bytes).ok()?;
        const MAX: u32 = 2048;
        let image = if image.width() > MAX || image.height() > MAX {
            image.resize(MAX, MAX, image::imageops::FilterType::Triangle)
        } else {
            image
        };
        Some(picture::ladder(image))
    }
}

/// What the shelf needs to know about a book, without reading the book itself.
pub struct Metadata {
    pub title: String,
    pub author: Option<String>,
    /// Recognises the same book under another file name: its identifier (an ISBN or UUID)
    /// plus its title, as some publishers reuse one placeholder identifier for every book.
    /// Without an identifier, title and author have to do.
    pub key: String,
}

pub fn metadata(path: &Path) -> Option<Metadata> {
    let doc = EpubDoc::new(path).ok()?;
    let title = collapse_whitespace(&doc.get_title().filter(|t| !t.trim().is_empty())?);
    let author = doc.mdata("creator").map(|m| collapse_whitespace(&m.value)).filter(|a| !a.is_empty());
    let identifier = doc.unique_identifier.as_deref().map(str::trim).filter(|i| !i.is_empty());
    let second = identifier.map(str::to_owned).or_else(|| author.clone()).unwrap_or_default();
    let key = format!("{}|{}", title.to_lowercase(), second.to_lowercase());
    Some(Metadata { title, author, key })
}

/// Just the cover, scaled down for the shelf, as a [`picture::ladder`]. Opens the archive afresh; call off the UI thread.
pub fn cover_thumbnail(path: &Path, max_height: u32) -> Option<Vec<ColorImage>> {
    let mut doc = EpubDoc::new(path).ok()?;
    let bytes = match doc.get_cover() {
        Some((bytes, _)) => bytes,
        None => {
            // No declared cover: fall back to an image that calls itself one.
            let mut candidates: Vec<String> = doc
                .resources
                .iter()
                .filter(|(id, res)| {
                    res.mime.starts_with("image/")
                        && (id.to_lowercase().contains("cover")
                            || path_to_string(&res.path).to_lowercase().contains("cover"))
                })
                .map(|(_, res)| path_to_string(&res.path))
                .collect();
            candidates.sort();
            doc.get_resource_by_path(candidates.first()?)?
        }
    };
    let image = image::load_from_memory(&bytes).ok()?;
    let image = if image.height() > max_height {
        image.resize(u32::MAX, max_height, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    Some(picture::ladder(image))
}

impl Book {
    pub fn open(path: &Path) -> Result<Book, String> {
        let mut doc = EpubDoc::new(path).map_err(|e| format!("Could not open this book ({e})"))?;

        let title = doc
            .get_title()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "Untitled".into());
        let author = doc.mdata("creator").map(|m| m.value.trim().to_string());
        let language = doc.mdata("language").map(|m| m.value.trim().to_lowercase());

        let spine_paths: Vec<String> = doc
            .spine
            .iter()
            .filter_map(|item| doc.resources.get(&item.idref))
            .filter(|res| res.mime.contains("html") || res.mime.contains("xml"))
            .map(|res| path_to_string(&res.path))
            .collect();

        let mut css_cache: HashMap<String, Css> = HashMap::new();
        let mut chapters = Vec::new();
        let mut anchors: Vec<HashMap<String, usize>> = Vec::new();
        let mut image_paths: Vec<String> = Vec::new();

        for chapter_path in &spine_paths {
            let Some(source) = doc.get_resource_str_by_path(chapter_path) else {
                continue;
            };
            let html = Html::parse_document(&expand_self_closing(&source));
            let base = parent_dir(chapter_path);

            let mut css = Css::default();
            for link in select(&html, "link") {
                let is_css = link.value().attr("rel").is_some_and(|r| r.contains("stylesheet"))
                    || link.value().attr("href").is_some_and(|h| h.ends_with(".css"));
                if let (true, Some(href)) = (is_css, link.value().attr("href")) {
                    let css_path = resolve(&base, href);
                    let sheet = css_cache.entry(css_path.clone()).or_insert_with(|| {
                        doc.get_resource_str_by_path(&css_path)
                            .map(|text| Css::parse(&text))
                            .unwrap_or_default()
                    });
                    css.extend(sheet);
                }
            }
            for style in select(&html, "style") {
                css.extend(&Css::parse(&style.text().collect::<String>()));
            }

            let mut builder = Builder::new(&css, base);
            if let Some(body) = select(&html, "body").next() {
                builder.walk(*body, &Ctx::default());
            } else {
                builder.walk(*html.root_element(), &Ctx::default());
            }
            builder.flush(&Ctx::default());

            image_paths.extend(builder.blocks.iter().filter_map(|b| match &b.kind {
                BlockKind::Image(src) => Some(src.clone()),
                _ => None,
            }));

            let mut block_starts = Vec::with_capacity(builder.blocks.len());
            let mut chars = 0;
            for block in &builder.blocks {
                block_starts.push(chars);
                chars += block.char_len();
            }
            chapters.push(Chapter { blocks: builder.blocks, block_starts, chars: chars.max(1) });
            anchors.push(builder.anchors);
        }

        if chapters.is_empty() {
            return Err("This book has no readable chapters".into());
        }

        let mut chapter_starts = Vec::with_capacity(chapters.len());
        let mut total_chars = 0;
        for chapter in &chapters {
            chapter_starts.push(total_chars);
            total_chars += chapter.chars;
        }

        // Table of contents: prefer the publisher's, fall back to headings.
        let path_to_chapter: HashMap<&str, usize> =
            spine_paths.iter().enumerate().map(|(i, p)| (p.as_str(), i)).collect();
        let mut toc = Vec::new();
        flatten_toc(&doc.toc, 0, &mut |nav, depth| {
            let full = path_to_string(&nav.content);
            let (file, fragment) = match full.split_once('#') {
                Some((f, frag)) => (f.to_string(), Some(frag.to_string())),
                None => (full.clone(), None),
            };
            let file = normalize(&percent_decode(&file));
            let Some(&chapter) = path_to_chapter.get(file.as_str()) else { return };
            let offset = fragment
                .and_then(|f| anchors[chapter].get(&percent_decode(&f)).copied())
                .and_then(|block| chapters[chapter].block_starts.get(block).copied())
                .unwrap_or(0);
            let label = collapse_whitespace(&nav.label);
            if !label.is_empty() {
                toc.push(TocEntry { label, chapter, offset, depth });
            }
        });
        if toc.is_empty() {
            for (i, chapter) in chapters.iter().enumerate() {
                if let Some((bi, block)) =
                    chapter.blocks.iter().enumerate().find(|(_, b)| matches!(b.kind, BlockKind::Heading(_)))
                {
                    let label = collapse_whitespace(&block.plain_text());
                    toc.push(TocEntry { label, chapter: i, offset: chapter.block_starts[bi], depth: 0 });
                }
            }
        }
        toc.sort_by_key(|e| (e.chapter, e.offset));
        join_numbers(&mut toc);

        let mut images = HashMap::new();
        image_paths.sort();
        image_paths.dedup();
        for img_path in image_paths {
            if let Some(image) = doc.get_resource_by_path(&img_path).and_then(BookImage::new) {
                images.insert(img_path, image);
            }
        }

        Ok(Book { title, author, language, chapters, chapter_starts, total_chars, toc, images })
    }

    /// Fraction of the book that lies before this position.
    pub fn progress(&self, chapter: usize, offset: usize) -> f32 {
        let at = self.chapter_starts.get(chapter).copied().unwrap_or(0) + offset;
        (at as f32 / self.total_chars.max(1) as f32).clamp(0.0, 1.0)
    }

    /// Inverse of [`Book::progress`].
    pub fn locate(&self, fraction: f32) -> (usize, usize) {
        let at = (fraction.clamp(0.0, 1.0) * self.total_chars as f32) as usize;
        let chapter = self.chapter_starts.partition_point(|&s| s <= at).saturating_sub(1);
        (chapter, at - self.chapter_starts[chapter])
    }

    /// The table-of-contents entry this position belongs to.
    pub fn section_at(&self, chapter: usize, offset: usize) -> Option<usize> {
        let idx = self.toc.partition_point(|e| (e.chapter, e.offset) <= (chapter, offset));
        idx.checked_sub(1)
    }
}

fn flatten_toc(points: &[NavPoint], depth: usize, f: &mut impl FnMut(&NavPoint, usize)) {
    for point in points {
        f(point, depth);
        flatten_toc(&point.children, depth + 1, f);
    }
}

// ---------------------------------------------------------------------------------------
// XHTML → blocks

#[derive(Clone)]
struct Ctx {
    style: Style,
    kind: BlockKind,
    align: Option<Align>,
    quote: u8,
    list: u8,
    pre: bool,
    no_indent: bool,
    link: bool,
}

impl Default for Ctx {
    fn default() -> Self {
        Ctx {
            style: Style::default(),
            kind: BlockKind::Paragraph,
            align: None,
            quote: 0,
            list: 0,
            pre: false,
            no_indent: false,
            link: false,
        }
    }
}

struct Builder<'a> {
    css: &'a Css,
    base: String,
    blocks: Vec<Block>,
    spans: Vec<Span>,
    space_pending: bool,
    /// Whether the pending block has any text outside a link (lists of links are not prose).
    plain_text: bool,
    pending_ids: Vec<String>,
    pending_marker: Option<String>,
    /// The next block opens a new scene (see `Block::break_before`).
    pending_break: bool,
    anchors: HashMap<String, usize>,
}

const BLOCK_ELEMENTS: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "header",
    "footer",
    "main",
    "aside",
    "nav",
    "figure",
    "figcaption",
    "blockquote",
    "pre",
    "li",
    "ul",
    "ol",
    "dl",
    "dt",
    "dd",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "table",
    "tr",
    "address",
    "center",
    "body",
    "hgroup",
    "caption",
];

impl<'a> Builder<'a> {
    fn new(css: &'a Css, base: String) -> Self {
        Builder {
            css,
            base,
            blocks: Vec::new(),
            spans: Vec::new(),
            space_pending: false,
            plain_text: false,
            pending_ids: Vec::new(),
            pending_marker: None,
            pending_break: false,
            anchors: HashMap::new(),
        }
    }

    fn walk(&mut self, node: ego_tree::NodeRef<'_, Node>, ctx: &Ctx) {
        for child in node.children() {
            match child.value() {
                Node::Text(text) => self.text(text, ctx),
                Node::Element(_) => {
                    if let Some(el) = ElementRef::wrap(child) {
                        self.element(el, ctx);
                    }
                }
                _ => {}
            }
        }
    }

    fn element(&mut self, el: ElementRef<'_>, ctx: &Ctx) {
        let e = el.value();
        let name = e.name();
        if matches!(name, "head" | "script" | "style" | "title" | "template" | "noscript") {
            return;
        }
        let props = self.css.props_for(name, e.classes(), e.attr("style"));
        if props.hidden {
            return;
        }
        if let Some(id) = e.id() {
            self.pending_ids.push(id.to_string());
        }
        if let Some(name_attr) = e.attr("name").filter(|_| name == "a") {
            self.pending_ids.push(name_attr.to_string());
        }

        let mut c = ctx.clone();
        match name {
            "br" => {
                self.push_raw("\n", ctx.style);
                self.space_pending = false;
                return;
            }
            "hr" => {
                self.flush(ctx);
                self.push_block(BlockKind::Rule, Vec::new(), ctx);
                return;
            }
            "img" | "image" => {
                // SVG covers use `xlink:href`, which a namespace-strict lookup would miss.
                let any_ns = |name: &str| e.attrs.iter().find(|(k, _)| &*k.local == name).map(|(_, v)| &**v);
                if let Some(src) = any_ns("src").or_else(|| any_ns("href")) {
                    self.flush(ctx);
                    let path = resolve(&self.base, src);
                    self.push_block(BlockKind::Image(path), Vec::new(), ctx);
                }
                return;
            }
            "b" | "strong" => c.style.bold = true,
            "i" | "em" | "cite" | "dfn" | "var" => c.style.italic = !ctx.style.italic,
            "code" | "tt" | "kbd" | "samp" => c.style.mono = true,
            "sup" => c.style.sup = true,
            "a" => c.link = true,
            "sub" => c.style.sub = true,
            "td" | "th" if self.spans.iter().any(|s| !s.text.trim().is_empty()) => {
                self.space_pending = true;
                self.push_raw("  ", ctx.style);
            }
            _ => {}
        }

        if let Some(italic) = props.italic {
            c.style.italic = italic;
        }
        if let Some(bold) = props.bold {
            c.style.bold = bold;
        }
        if let Some(sc) = props.small_caps {
            c.style.small_caps = sc;
        }

        if BLOCK_ELEMENTS.contains(&name) {
            self.flush(ctx);
            c.align = props.align.or(match name {
                "center" => Some(Align::Center),
                _ => ctx.align,
            });
            c.no_indent = props.no_indent.unwrap_or(false);
            self.pending_break |= props.break_before.unwrap_or(false);
            match name {
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    c.kind = BlockKind::Heading(name.as_bytes()[1] - b'0');
                }
                "pre" => {
                    c.kind = BlockKind::Pre;
                    c.pre = true;
                }
                "figcaption" | "caption" => c.kind = BlockKind::Caption,
                "blockquote" | "dd" => {
                    c.quote = ctx.quote + 1;
                    c.kind = BlockKind::Paragraph;
                }
                "li" => {
                    c.kind = BlockKind::ListItem { marker: String::new() };
                }
                "ul" | "ol" => {
                    c.list = ctx.list + 1;
                    let ordered = name == "ol";
                    let mut n: u32 = e.attr("start").and_then(|s| s.parse().ok()).unwrap_or(1);
                    for child in el.children() {
                        match ElementRef::wrap(child) {
                            Some(li) if li.value().name() == "li" => {
                                self.pending_marker =
                                    Some(if ordered { format!("{n}.") } else { "•".into() });
                                n += 1;
                                self.element(li, &c);
                            }
                            Some(other) => self.element(other, &c),
                            None => {
                                if let Node::Text(t) = child.value() {
                                    self.text(t, &c);
                                }
                            }
                        }
                    }
                    self.flush(&c);
                    return;
                }
                _ => {
                    if !matches!(c.kind, BlockKind::Heading(_) | BlockKind::Pre | BlockKind::Caption) {
                        c.kind = BlockKind::Paragraph;
                    }
                }
            }
            self.walk(*el, &c);
            self.flush(&c);
        } else {
            self.walk(*el, &c);
        }
    }

    fn text(&mut self, text: &str, ctx: &Ctx) {
        if ctx.pre {
            self.push_raw(&text.replace("\r\n", "\n"), ctx.style);
            return;
        }
        let mut out = String::with_capacity(text.len());
        for ch in text.chars() {
            if ch == '\u{ad}' || ch == '\u{200b}' {
                continue;
            }
            if ch.is_whitespace() && ch != '\u{a0}' {
                let at_start = self.spans.iter().all(|s| s.text.is_empty()) && out.is_empty();
                let after_newline = out.ends_with('\n')
                    || (out.is_empty() && self.spans.last().is_some_and(|s| s.text.ends_with('\n')));
                if !at_start && !after_newline {
                    self.space_pending = true;
                }
            } else {
                if self.space_pending {
                    out.push(' ');
                    self.space_pending = false;
                }
                out.push(ch);
            }
        }
        if !out.is_empty() {
            self.plain_text |= !ctx.link && !out.trim().is_empty();
            self.push_raw(&out, ctx.style);
        }
    }

    fn push_raw(&mut self, text: &str, style: Style) {
        match self.spans.last_mut() {
            Some(last) if last.style == style => last.text.push_str(text),
            _ => self.spans.push(Span { text: text.to_string(), style }),
        }
    }

    fn flush(&mut self, ctx: &Ctx) {
        self.space_pending = false;
        let plain_text = std::mem::take(&mut self.plain_text);
        let mut spans = std::mem::take(&mut self.spans);
        // Trim trailing whitespace and line breaks.
        while let Some(last) = spans.last_mut() {
            let trimmed = last.text.trim_end().len();
            last.text.truncate(trimmed);
            if last.text.is_empty() {
                spans.pop();
            } else {
                break;
            }
        }
        // Trim leading line breaks.
        while let Some(first) = spans.first_mut() {
            let trimmed = first.text.trim_start().to_string();
            first.text = trimmed;
            if first.text.is_empty() {
                spans.remove(0);
            } else {
                break;
            }
        }
        if spans.is_empty() {
            return;
        }
        let mut kind = ctx.kind.clone();
        if let BlockKind::ListItem { marker } = &mut kind {
            *marker = self.pending_marker.take().unwrap_or_default();
        }
        let mut ctx = ctx.clone();
        ctx.no_indent |= !plain_text;
        self.push_block(kind, spans, &ctx);
    }

    fn push_block(&mut self, kind: BlockKind, spans: Vec<Span>, ctx: &Ctx) {
        for id in self.pending_ids.drain(..) {
            self.anchors.insert(id, self.blocks.len());
        }
        self.blocks.push(Block {
            kind,
            spans,
            align: ctx.align,
            quote: ctx.quote,
            list: ctx.list,
            no_indent: ctx.no_indent,
            break_before: std::mem::take(&mut self.pending_break),
        });
    }
}

/// EPUB chapters are XHTML, where `<a id="x"/>` is a complete element. An HTML parser
/// would leave it open and swallow the rest of the chapter, so spell such tags out.
fn expand_self_closing(source: &str) -> String {
    const VOID: &[&str] = &[
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
        "track", "wbr",
    ];
    let mut out = String::with_capacity(source.len() + 256);
    let mut rest = source;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        // Find the end of this tag, skipping over quoted attribute values.
        let mut quote = None;
        let mut end = None;
        for (i, c) in rest.char_indices().skip(1) {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (None, '"' | '\'') => quote = Some(c),
                (None, '>') => {
                    end = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        let tag = &rest[..=end];
        let name: String =
            tag[1..].chars().take_while(|c| c.is_alphanumeric() || *c == ':' || *c == '-').collect();
        if tag.ends_with("/>") && !name.is_empty() && !VOID.contains(&name.to_ascii_lowercase().as_str()) {
            out.push_str(&tag[..tag.len() - 2]);
            out.push_str("></");
            out.push_str(&name);
            out.push('>');
        } else {
            out.push_str(tag);
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

fn select<'a>(html: &'a Html, selector: &str) -> impl Iterator<Item = ElementRef<'a>> {
    let selector = scraper::Selector::parse(selector).expect("valid selector");
    html.select(&selector).collect::<Vec<_>>().into_iter()
}

// ---------------------------------------------------------------------------------------
// A deliberately tiny CSS reader

#[derive(Clone, Copy, Default, Debug)]
struct Props {
    italic: Option<bool>,
    bold: Option<bool>,
    small_caps: Option<bool>,
    align: Option<Align>,
    no_indent: Option<bool>,
    break_before: Option<bool>,
    hidden: bool,
}

impl Props {
    fn merge(&mut self, other: &Props) {
        self.italic = other.italic.or(self.italic);
        self.bold = other.bold.or(self.bold);
        self.small_caps = other.small_caps.or(self.small_caps);
        self.align = other.align.or(self.align);
        self.no_indent = other.no_indent.or(self.no_indent);
        self.break_before = other.break_before.or(self.break_before);
        self.hidden |= other.hidden;
    }

    fn parse(declarations: &str) -> Props {
        let mut props = Props::default();
        for decl in declarations.split(';') {
            let Some((key, value)) = decl.split_once(':') else { continue };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim().trim_end_matches("!important").trim().to_ascii_lowercase();
            match key.as_str() {
                "font-style" => props.italic = Some(value == "italic" || value == "oblique"),
                "font-weight" => {
                    props.bold = Some(
                        value == "bold" || value == "bolder" || value.parse::<u32>().is_ok_and(|w| w >= 600),
                    )
                }
                "font-variant" | "font-variant-caps" => props.small_caps = Some(value.contains("small-caps")),
                "text-align" => {
                    props.align = match value.as_str() {
                        "center" => Some(Align::Center),
                        "right" | "end" => Some(Align::End),
                        "left" | "start" | "justify" => Some(Align::Start),
                        _ => None,
                    }
                }
                "text-indent" => {
                    let zero =
                        value.trim_start_matches(['0', '.']).chars().all(|c| c.is_alphabetic() || c == '%');
                    props.no_indent = Some(zero);
                }
                "margin-top" | "padding-top" => props.break_before = Some(wide_gap(&value)),
                "margin" | "padding" => {
                    props.break_before = value.split_whitespace().next().map(wide_gap);
                }
                "display" if value == "none" => props.hidden = true,
                _ => {}
            }
        }
        props
    }
}

/// Whether a top margin is wide enough to mark a scene break, rather than ordinary spacing:
/// a line and a half or more.
fn wide_gap(value: &str) -> bool {
    let number = |unit: &str| value.strip_suffix(unit).and_then(|n| n.trim().parse::<f32>().ok());
    let ems = number("rem").or_else(|| number("em")).or_else(|| number("px").map(|px| px / 16.0));
    let ems = ems.or_else(|| number("pt").map(|pt| pt / 12.0));
    ems.is_some_and(|em| em >= 1.5)
}

#[derive(Default, Clone)]
struct Css {
    /// Keyed by `tag`, `.class` or `tag.class`.
    rules: HashMap<String, Props>,
}

impl Css {
    fn parse(text: &str) -> Css {
        let mut css = Css::default();
        let text = strip_comments(text);
        for rule in text.split('}') {
            let Some((selectors, body)) = rule.split_once('{') else { continue };
            if selectors.trim_start().starts_with('@') {
                continue;
            }
            let props = Props::parse(body);
            for selector in selectors.split(',') {
                // Only the rightmost compound selector, and only simple tag/class forms.
                let last = selector
                    .split(|c: char| c.is_whitespace() || c == '>' || c == '+' || c == '~')
                    .rfind(|s| !s.is_empty())
                    .unwrap_or("");
                if last.is_empty() || last.contains([':', '[', '#', '*']) {
                    continue;
                }
                let key = match last.split_once('.') {
                    Some((tag, class)) if !class.contains('.') => {
                        if tag.is_empty() {
                            format!(".{class}")
                        } else {
                            format!("{}.{class}", tag.to_ascii_lowercase())
                        }
                    }
                    Some(_) => continue,
                    None => last.to_ascii_lowercase(),
                };
                css.rules.entry(key).or_default().merge(&props);
            }
        }
        css
    }

    fn extend(&mut self, other: &Css) {
        for (key, props) in &other.rules {
            self.rules.entry(key.clone()).or_default().merge(props);
        }
    }

    fn props_for<'c>(
        &self,
        tag: &str,
        classes: impl Iterator<Item = &'c str>,
        inline: Option<&str>,
    ) -> Props {
        let mut props = Props::default();
        if let Some(p) = self.rules.get(tag) {
            props.merge(p);
            // A blanket `p { text-indent: 0 }` is a house style; the reader's preference wins.
            props.no_indent = None;
            props.break_before = None;
        }
        for class in classes {
            if let Some(p) = self.rules.get(&format!(".{class}")) {
                props.merge(p);
            }
            if let Some(p) = self.rules.get(&format!("{tag}.{class}")) {
                props.merge(p);
            }
            let lower = class.to_ascii_lowercase();
            if lower == "smallcaps" || lower == "small-caps" || lower == "sc" {
                props.small_caps = Some(true);
            }
        }
        if let Some(style) = inline {
            props.merge(&Props::parse(style));
        }
        props
    }
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        rest = match rest[start + 2..].find("*/") {
            Some(end) => &rest[start + 2 + end + 2..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------------------
// Paths inside the archive

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn parent_dir(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..i].to_string(),
        None => String::new(),
    }
}

/// Resolve `href` relative to the directory `base`, dropping fragments and queries.
pub fn resolve(base: &str, href: &str) -> String {
    let href = href.split(['#', '?']).next().unwrap_or("");
    let href = percent_decode(href);
    if let Some(absolute) = href.strip_prefix('/') {
        return normalize(absolute);
    }
    if base.is_empty() { normalize(&href) } else { normalize(&format!("{base}/{href}")) }
}

fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Some books list a chapter's number and its name as two entries ("13", then "An Offer and a
/// Departure"): make them one, "13 An Offer and a Departure", where the number is.
fn join_numbers(toc: &mut Vec<TocEntry>) {
    let mut i = 0;
    while i + 1 < toc.len() {
        let (number, name) = (&toc[i], &toc[i + 1]);
        if is_number(&number.label)
            && !is_number(&name.label)
            && name.chapter == number.chapter
            && name.depth == number.depth
        {
            let name = toc.remove(i + 1);
            toc[i].label = format!("{} {}", toc[i].label, name.label);
        }
        i += 1;
    }
}

/// "13", "XIII", "Chapter 13", "Part Two"… a label that only numbers its section.
fn is_number(label: &str) -> bool {
    const WORDS: &str = "one two three four five six seven eight nine ten eleven twelve thirteen fourteen \
                         fifteen sixteen seventeen eighteen nineteen twenty";
    let label = label.trim_end_matches(['.', ':']);
    let lower = label.to_lowercase();
    let prefix = ["chapter ", "part ", "book "].iter().find(|p| lower.starts_with(*p)).map_or(0, |p| p.len());
    let (rest, lower_rest) = (&label[prefix..], &lower[prefix..]);
    !rest.is_empty()
        && (rest.chars().all(|c| c.is_ascii_digit())
            // Capitals only, so a name like "Mix" isn't taken for a numeral.
            || rest.chars().all(|c| "IVXLCDM".contains(c))
            || (prefix > 0 && WORDS.split(' ').any(|w| w == lower_rest)))
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(xhtml: &str) -> Vec<Block> {
        let html = Html::parse_document(&expand_self_closing(xhtml));
        let css = Css::parse(".it { font-style: italic } p.first { text-indent: 0 } .gone { display: none }");
        let mut builder = Builder::new(&css, String::new());
        builder.walk(*select(&html, "body").next().unwrap(), &Ctx::default());
        builder.flush(&Ctx::default());
        builder.blocks
    }

    #[test]
    fn joins_numbers_to_their_names() {
        let entry = |label: &str, chapter| TocEntry { label: label.into(), chapter, offset: 0, depth: 0 };
        let mut toc = vec![
            entry("Prologue", 0),
            entry("13", 1),
            entry("An Offer and a Departure", 1),
            entry("XIV", 2),
            entry("A Box Opens", 3),
            entry("Chapter Two", 4),
            entry("Mixed", 4),
            entry("Mix", 5),
            entry("Up", 5),
            entry("1", 5),
            entry("2", 5),
        ];
        join_numbers(&mut toc);
        let labels: Vec<&str> = toc.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Prologue",
                "13 An Offer and a Departure",
                "XIV",
                "A Box Opens",
                "Chapter Two Mixed",
                "Mix",
                "Up",
                "1",
                "2"
            ]
        );
    }

    #[test]
    fn resolves_relative_paths() {
        assert_eq!(resolve("OEBPS/Text", "../Images/a%20b.jpg#x"), "OEBPS/Images/a b.jpg");
        assert_eq!(resolve("", "./cover.jpg"), "cover.jpg");
        assert_eq!(resolve("OEBPS", "/root.css"), "root.css");
    }

    #[test]
    fn wide_top_margins_break_scenes() {
        let css = Css::parse(
            "p { margin-top: 3em } .para2 { margin-top: 2.5em } .near { margin: 0.5em 0 } .gap { margin: 24px 0 }",
        );
        let html = Html::parse_document(
            r#"<body><p>a</p><p class="para2">b</p><p class="near">c</p><div class="gap"><p>d</p><p>e</p></div></body>"#,
        );
        let mut builder = Builder::new(&css, String::new());
        builder.walk(*select(&html, "body").next().unwrap(), &Ctx::default());
        builder.flush(&Ctx::default());
        let breaks: Vec<bool> = builder.blocks.iter().map(|b| b.break_before).collect();
        // A blanket `p` margin is house style, not a scene break.
        assert_eq!(breaks, [false, true, false, true, false]);
    }

    #[test]
    fn spells_out_self_closing_tags() {
        assert_eq!(
            expand_self_closing(r#"<p>a<a id="x"/>b<br/><img src="i.png" /></p>"#),
            r#"<p>a<a id="x"></a>b<br/><img src="i.png" /></p>"#
        );
        assert_eq!(expand_self_closing(r#"<a title="1 > 0"/>"#), r#"<a title="1 > 0"></a>"#);
    }

    #[test]
    fn extracts_blocks_and_styles() {
        let b = blocks(
            r#"<body><h1>One</h1><p class="first">Hello <span class="it">dear</span>   world.</p>
               <a id="note"/><p>Second<br/>line</p><p class="gone">hidden</p><hr/></body>"#,
        );
        assert_eq!(b.len(), 4);
        assert_eq!(b[0].kind, BlockKind::Heading(1));
        assert_eq!(b[1].plain_text(), "Hello dear world.");
        assert!(b[1].spans[1].style.italic);
        assert!(b[1].no_indent);
        assert_eq!(b[2].plain_text(), "Second\nline");
        assert!(!b[2].no_indent, "an empty self-closed anchor must not swallow what follows");
        assert_eq!(b[3].kind, BlockKind::Rule);
    }

    #[test]
    fn tag_wide_indent_rules_defer_to_the_reader() {
        let css = Css::parse("p { text-indent: 0; font-style: italic }");
        let props = css.props_for("p", std::iter::empty(), None);
        assert_eq!(props.no_indent, None);
        assert_eq!(props.italic, Some(true));
    }
}
