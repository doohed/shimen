//! Lays out a `Document` into lines of styled segments. Links become
//! *anchors* (one per occurrence) so the interactive browser can select,
//! highlight and click them; `Layout::to_text` turns the result into ANSI
//! text or plain Markdown for printing.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::document::{Block, Document, ImageRef, Inline, Kind, Style};
use crate::images::ImageState;
use crate::pixels::{self, ColorDepth};

pub struct Options {
    pub width: usize,
    pub markdown: bool,
    /// List link URLs at the end (for printed output with `[n]` markers).
    pub footer: bool,
    pub image: ImageOptions,
}

#[derive(Clone, Copy)]
pub struct ImageOptions {
    pub max_cols: u32,
    pub max_rows: u32,
    pub pixel: u32,
    pub depth: ColorDepth,
}

#[derive(Clone)]
pub struct Seg {
    pub text: String,
    /// SGR parameters, e.g. `1;35`. Empty = default style.
    pub sgr: String,
    /// Display width in terminal columns.
    pub width: usize,
    /// Link occurrence this segment belongs to (index into `Layout::anchors`).
    pub anchor: Option<usize>,
    /// Pre-rendered ANSI (image rows): print as-is.
    pub raw: bool,
    link: Option<usize>,
}

#[derive(Default)]
pub struct Line {
    pub segs: Vec<Seg>,
}

pub struct Anchor {
    /// 1-based index into `Document::links`.
    pub link: usize,
    /// First line the anchor appears on.
    pub line: usize,
}

#[derive(Default)]
pub struct Layout {
    pub lines: Vec<Line>,
    pub anchors: Vec<Anchor>,
}

impl Layout {
    pub fn to_text(&self, markdown: bool) -> String {
        let mut out = String::new();
        for line in &self.lines {
            let mut s = String::new();
            for seg in &line.segs {
                paint(&mut s, seg, "");
            }
            out.push_str(if markdown { s.trim_end() } else { &s });
            out.push('\n');
        }
        out
    }
}

/// Appends `seg` to `out` with its style plus `extra` SGR parameters.
pub fn paint(out: &mut String, seg: &Seg, extra: &str) {
    if seg.raw || (seg.sgr.is_empty() && extra.is_empty()) {
        out.push_str(&seg.text);
        return;
    }
    let sep = if seg.sgr.is_empty() || extra.is_empty() { "" } else { ";" };
    out.push_str(&format!("\x1b[{}{sep}{extra}m{}\x1b[0m", seg.sgr, seg.text));
}

pub fn layout(doc: &Document, images: &[ImageState], opts: &Options) -> Layout {
    let mut r = Renderer { opts, md: opts.markdown, out: Layout::default(), current: None };
    r.header(doc);

    let mut prev_tight: Option<bool> = None;
    for block in &doc.blocks {
        let body = r.block(block, doc, images);
        if body.is_empty() {
            continue;
        }
        if let Some(pt) = prev_tight
            && !(pt && block.tight)
        {
            r.blank();
        }
        prev_tight = Some(block.tight);
        for (i, segs) in body.into_iter().enumerate() {
            let prefix = if i == 0 { &block.first } else { &block.rest };
            r.emit(prefix, segs);
        }
    }

    if opts.footer && !doc.links.is_empty() {
        r.blank();
        if !r.md {
            let rule = r.seg(&"─".repeat(opts.width), "2");
            r.emit("", vec![rule]);
        }
        for (i, url) in doc.links.iter().enumerate() {
            let line = if r.md {
                vec![r.seg(&format!("[{}]: {url}", i + 1), "")]
            } else {
                vec![r.seg(&format!("{:>5} ", format!("[{}]", i + 1)), "2"), r.seg(url, "34")]
            };
            r.emit("", line);
        }
    }
    r.out
}

struct Renderer<'a> {
    opts: &'a Options,
    md: bool,
    out: Layout,
    /// (link id, anchor index) of the link currently being laid out.
    current: Option<(usize, usize)>,
}

impl Renderer<'_> {
    fn seg(&self, text: &str, sgr: &str) -> Seg {
        Seg {
            text: text.to_string(),
            sgr: if self.md { String::new() } else { sgr.to_string() },
            width: text.width(),
            anchor: None,
            raw: false,
            link: None,
        }
    }

    fn blank(&mut self) {
        self.out.lines.push(Line::default());
    }

    /// Pushes a line, assigning link segments to anchors. Consecutive
    /// segments of the same link — even across lines and blocks, as in a
    /// card whose title and blurb are one <a> — form a single anchor.
    fn emit(&mut self, prefix: &str, segs: Vec<Seg>) {
        let mut line = Line::default();
        if !prefix.is_empty() {
            line.segs.push(self.seg(prefix, "2"));
        }
        for mut seg in segs {
            match seg.link {
                Some(id) => {
                    let anchor = match self.current {
                        Some((cur, a)) if cur == id => a,
                        _ => {
                            self.out.anchors.push(crate::render::Anchor { link: id, line: self.out.lines.len() });
                            self.out.anchors.len() - 1
                        }
                    };
                    self.current = Some((id, anchor));
                    seg.anchor = Some(anchor);
                }
                None if !seg.text.trim().is_empty() => self.current = None,
                None => {}
            }
            line.segs.push(seg);
        }
        self.out.lines.push(line);
    }

    fn header(&mut self, doc: &Document) {
        let Some(title) = &doc.title else { return };
        if self.md {
            let t = self.seg(&format!("# {title}"), "");
            self.emit("", vec![t]);
            self.blank();
            if let Some(d) = &doc.description {
                let d = self.seg(&format!("> {d}"), "");
                self.emit("", vec![d]);
                self.blank();
            }
            return;
        }
        let t = self.seg(title, "1;35");
        self.emit("", vec![t]);
        if let Some(d) = &doc.description {
            let style = Style { italic: true, dim: true, ..Style::default() };
            for line in wrap(&words(d, style), self.opts.width) {
                let segs = self.segments(&line, "");
                self.emit("", segs);
            }
        }
        let rule = self.seg(&"═".repeat(self.opts.width), "2");
        self.emit("", vec![rule]);
        self.blank();
    }

    fn block(&self, b: &Block, doc: &Document, images: &[ImageState]) -> Vec<Vec<Seg>> {
        let avail = self.opts.width.saturating_sub(b.rest.width()).max(20);
        match &b.kind {
            Kind::Para => wrap(&b.inlines, avail).iter().map(|l| self.segments(l, "")).collect(),
            Kind::Heading(level) => {
                let marker = format!("{} ", "#".repeat(usize::from(*level)));
                let base = match level {
                    1 => "1;35",
                    2 => "1;36",
                    3 => "1;32",
                    _ => "1;33",
                };
                // Markdown headings must stay on one line.
                let w = if self.md { usize::MAX } else { avail.saturating_sub(marker.len()).max(10) };
                wrap(&b.inlines, w)
                    .iter()
                    .enumerate()
                    .map(|(i, l)| {
                        let lead = if i == 0 { self.seg(&marker, "2") } else { self.seg(&" ".repeat(marker.len()), "") };
                        let mut v = vec![lead];
                        v.extend(self.segments(l, base));
                        v
                    })
                    .collect()
            }
            Kind::Code(lines) => {
                if self.md {
                    let mut v = vec![vec![self.seg("```", "")]];
                    v.extend(lines.iter().map(|l| vec![self.seg(l, "")]));
                    v.push(vec![self.seg("```", "")]);
                    v
                } else {
                    lines.iter().map(|l| vec![self.seg("│ ", "2"), self.seg(l, "33")]).collect()
                }
            }
            Kind::Rule => {
                let rule = if self.md { "---".to_string() } else { "─".repeat(avail) };
                vec![vec![self.seg(&rule, "2")]]
            }
            Kind::Image(i) => self.image(&doc.images[*i], images.get(*i), avail),
        }
    }

    fn image(&self, img: &ImageRef, state: Option<&ImageState>, avail: usize) -> Vec<Vec<Seg>> {
        let linked = |mut seg: Seg| {
            seg.link = img.link;
            seg
        };
        if self.md {
            let text = match &img.url {
                Some(url) => format!("![{}]({url})", img.alt),
                None => format!("_{}_", img.alt),
            };
            return vec![vec![self.seg(&text, "")]];
        }
        let label = if img.alt.is_empty() { "image" } else { &img.alt };
        let note = |s: &str| vec![vec![linked(self.seg(&format!("▣ {label}{s}"), "2"))]];
        match state {
            Some(ImageState::Ready(image)) => {
                let o = &self.opts.image;
                let cols = (avail as u32).min(o.max_cols);
                let rows = pixels::render(image, cols, o.max_rows, o.pixel, o.depth);
                if rows.is_empty() {
                    return Vec::new(); // tracking pixel etc.
                }
                let mut v: Vec<Vec<Seg>> = rows
                    .into_iter()
                    .map(|row| {
                        let width = visible_width(&row);
                        vec![linked(Seg { text: row, sgr: String::new(), width, anchor: None, raw: true, link: None })]
                    })
                    .collect();
                if !img.alt.is_empty() {
                    v.push(vec![linked(self.seg(&img.alt, "2;3"))]);
                }
                v
            }
            Some(ImageState::Loading) => note(" …"),
            Some(ImageState::Failed(e)) => note(&format!(" ({e})")),
            Some(ImageState::Off) | None => note(""),
        }
    }

    /// Styled words → segments, merging runs of identical style.
    fn segments(&self, line: &[(String, Style)], base: &str) -> Vec<Seg> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < line.len() {
            let style = line[i].1;
            let mut text = String::new();
            while i < line.len() && line[i].1 == style {
                text.push_str(&line[i].0);
                i += 1;
            }
            let mut seg = self.seg(&text, &codes(style, base));
            seg.link = style.link;
            out.push(seg);
        }
        out
    }
}

fn visible_width(ansi: &str) -> usize {
    let mut w = 0;
    let mut in_escape = false;
    for ch in ansi.chars() {
        match (in_escape, ch) {
            (false, '\x1b') => in_escape = true,
            (true, 'm') => in_escape = false,
            (true, _) => {}
            (false, c) => w += c.width().unwrap_or(0),
        }
    }
    w
}

fn codes(s: Style, base: &str) -> String {
    let mut c: Vec<&str> = Vec::new();
    if !base.is_empty() {
        c.push(base);
    }
    if s.bold {
        c.push("1");
    }
    if s.dim {
        c.push("2");
    }
    if s.italic {
        c.push("3");
    }
    if s.strike {
        c.push("9");
    }
    if s.link.is_some() {
        c.push("4");
        if base.is_empty() {
            c.push("34");
        }
    }
    if s.code && base.is_empty() {
        c.push("33");
    }
    c.join(";")
}

/// Plain text → words, for wrapping header text.
fn words(text: &str, style: Style) -> Vec<Inline> {
    text.split_whitespace()
        .enumerate()
        .map(|(i, w)| Inline::Word(crate::document::Word::new(w.to_string(), style, i > 0)))
        .collect()
}

/// Greedy word wrap. Glued words move together; over-long words are split.
fn wrap(inlines: &[Inline], width: usize) -> Vec<Vec<(String, Style)>> {
    struct Unit {
        space: bool,
        parts: Vec<(String, Style)>,
        w: usize,
    }
    let width = width.max(1);
    let mut units: Vec<Option<Unit>> = Vec::new(); // None = hard break
    for inline in inlines {
        match inline {
            Inline::Break => units.push(None),
            Inline::Word(word) => {
                let w = word.text.width();
                let part = (word.text.clone(), word.style);
                match units.last_mut() {
                    Some(Some(u)) if word.glue => {
                        u.parts.push(part);
                        u.w += w;
                    }
                    _ => units.push(Some(Unit { space: word.space, parts: vec![part], w })),
                }
            }
        }
    }

    let mut lines = Vec::new();
    let mut cur: Vec<(String, Style)> = Vec::new();
    let mut cur_w = 0;
    for unit in units {
        let Some(unit) = unit else {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
            continue;
        };
        let space = usize::from(cur_w > 0 && unit.space);
        if cur_w > 0 && cur_w + space + unit.w > width {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
        } else if space == 1 {
            let prev = cur.last().map(|s| s.1);
            let next = unit.parts.first().map(|s| s.1);
            let style = if prev == next { prev.unwrap_or_default() } else { Style::default() };
            cur.push((" ".into(), style));
            cur_w += 1;
        }
        if unit.w <= width - cur_w {
            cur.extend(unit.parts);
            cur_w += unit.w;
            continue;
        }
        // Longer than a whole line (e.g. a URL): hard-split by character.
        for (text, style) in unit.parts {
            for ch in text.chars() {
                let cw = ch.width().unwrap_or(0);
                if cur_w + cw > width && cur_w > 0 {
                    lines.push(std::mem::take(&mut cur));
                    cur_w = 0;
                }
                match cur.last_mut() {
                    Some((t, s)) if *s == style => t.push(ch),
                    _ => cur.push((ch.to_string(), style)),
                }
                cur_w += cw;
            }
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}
