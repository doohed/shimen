//! Turns an HTML document into a flat list of blocks (paragraphs, headings,
//! code, images…) with styled inline words — roughly what Markdown can express.

use std::collections::HashMap;

use ego_tree::NodeRef;
use scraper::node::Element;
use scraper::{ElementRef, Html, Node, Selector};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use url::Url;

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    /// 1-based index into `Document::links`.
    pub link: Option<usize>,
    pub strike: bool,
    pub dim: bool,
}

#[derive(Debug)]
pub struct Word {
    pub text: String,
    pub style: Style,
    /// A space separates this word from the previous one.
    pub space: bool,
    /// No line break is allowed before this word (e.g. `<b>foo</b>bar`).
    pub glue: bool,
    cjk: bool,
}

impl Word {
    pub fn new(text: String, style: Style, space: bool) -> Self {
        Self { text, style, space, glue: false, cjk: false }
    }
}

#[derive(Debug)]
pub enum Inline {
    Word(Word),
    Break,
}

#[derive(Debug)]
pub enum Kind {
    Para,
    Heading(u8),
    Code(Vec<String>),
    Image(usize),
    Rule,
}

#[derive(Debug)]
pub struct Block {
    pub kind: Kind,
    pub inlines: Vec<Inline>,
    /// Prefix for the block's first line (may contain a list bullet).
    pub first: String,
    /// Prefix for every following line.
    pub rest: String,
    /// List items / table rows: no blank line between consecutive tight blocks.
    pub tight: bool,
}

pub struct ImageRef {
    pub url: Option<String>,
    pub alt: String,
    /// Link the image sits inside (cards), so it can be clicked too.
    pub link: Option<usize>,
}

pub struct Document {
    pub title: Option<String>,
    pub description: Option<String>,
    pub blocks: Vec<Block>,
    pub links: Vec<String>,
    pub images: Vec<ImageRef>,
}

/// Elements whose content is never shown.
const SKIP: &[&str] = &[
    "head", "script", "style", "noscript", "template", "svg", "canvas", "iframe", "object", "embed",
    "video", "audio", "source", "track", "map", "button", "input", "select", "textarea", "dialog",
];

/// Elements that start a new paragraph.
const BLOCKS: &[&str] = &[
    "p", "div", "section", "article", "main", "header", "footer", "nav", "aside", "table", "thead",
    "tbody", "tfoot", "figure", "address", "details", "dl", "form", "fieldset", "hgroup", "center",
];

pub struct ParseOptions {
    /// Render the whole `<body>` instead of the main content.
    pub full: bool,
    /// Emit Markdown syntax (`**`, `[text][n]`, `- `…) instead of styles.
    pub markdown: bool,
    /// Append `[n]` reference markers after links (non-interactive output).
    pub markers: bool,
}

pub fn parse(html: &str, page_url: &Url, opts: &ParseOptions) -> Document {
    let full = opts.full;
    let doc = Html::parse_document(html);
    let sel = |s: &str| Selector::parse(s).expect("static selector");
    let first_attr = |selector: &str, attr: &str| {
        doc.select(&sel(selector))
            .find_map(|e| e.value().attr(attr))
            .map(collapse_ws)
            .filter(|s| !s.is_empty())
    };

    let title = doc
        .select(&sel("title"))
        .next()
        .map(|t| collapse_ws(&t.text().collect::<String>()))
        .filter(|s| !s.is_empty());
    let description = first_attr("meta[name=description]", "content")
        .or_else(|| first_attr("meta[property='og:description']", "content"));
    let base = first_attr("base[href]", "href")
        .and_then(|h| page_url.join(&h).ok())
        .unwrap_or_else(|| page_url.clone());

    let mut b = Builder::new(base, opts.markdown);
    b.skip_chrome = !full;
    b.markers = opts.markers || opts.markdown;

    // Prefer the main content area unless the whole page was requested.
    let articles: Vec<_> = doc.select(&sel("article")).collect();
    let root = if full {
        None
    } else {
        doc.select(&sel("main, [role=main]"))
            .next()
            .or_else(|| (articles.len() == 1).then(|| articles[0]))
    };
    match root {
        Some(root) => {
            // Site navigation often lives in a <header> outside <main>: show
            // the navs that come before the content, compactly, on top.
            for nav in navs_before(&doc, root) {
                b.walk(*nav);
            }
            if !b.blocks.is_empty() || !b.inl.is_empty() {
                b.push_block(Kind::Rule);
            }
            b.walk(*root);
        }
        None => match doc.select(&sel("body")).next() {
            Some(body) => b.walk(*body),
            None => b.walk(doc.tree.root()),
        },
    }
    b.flush();

    Document {
        title,
        description,
        blocks: b.blocks,
        links: b.links,
        images: b.images,
    }
}

/// Visible top-level `<nav>` elements preceding `root` in document order.
fn navs_before<'a>(doc: &'a Html, root: ElementRef<'a>) -> Vec<ElementRef<'a>> {
    let mut navs = Vec::new();
    for node in doc.tree.root().descendants() {
        if node.id() == root.id() {
            break;
        }
        let Some(el) = ElementRef::wrap(node) else { continue };
        if el.value().name() != "nav" {
            continue;
        }
        let mut chain = std::iter::once(el).chain(node.ancestors().filter_map(ElementRef::wrap));
        let visible = chain.all(|a| {
            let v = a.value();
            !SKIP.contains(&v.name()) && !is_hidden(v) && !is_chrome(v)
        });
        let nested = node
            .ancestors()
            .filter_map(ElementRef::wrap)
            .any(|a| a.value().name() == "nav" || a.id() == root.id());
        if visible && !nested {
            navs.push(el);
        }
    }
    navs
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Words in class/id names that mark page chrome rather than content.
const CHROME: &[&str] = &[
    "dropdown", "sidebar", "breadcrumb",
    "breadcrumbs", "cookie", "cookies", "consent", "banner", "popup", "modal", "share", "sharing",
    "social", "skiplink", "newsletter", "toc", "editsection",
];

fn is_chrome(el: &Element) -> bool {
    match el.name() {
        "main" | "article" | "body" | "html" => return false,
        _ => {}
    }
    let names = el.attr("class").unwrap_or("").split_whitespace().chain(el.attr("id"));
    names.into_iter().any(|name| {
        let name = name.to_ascii_lowercase();
        // `has-sidebar`, `no-menu` etc. describe layout wrappers, not chrome.
        let modifier = ["has-", "no-", "with-", "without-", "is-"].iter().any(|m| name.starts_with(m));
        !modifier && name.split(['-', '_']).any(|part| CHROME.contains(&part))
    })
}

fn is_hidden(el: &Element) -> bool {
    if el.attr("hidden").is_some() || el.attr("aria-hidden") == Some("true") {
        return true;
    }
    el.attr("style").is_some_and(|s| {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
        s.contains("display:none") || s.contains("visibility:hidden")
    })
}

/// Picks a modest-resolution candidate: terminals only need ~100 px of width.
fn pick_src(el: &Element) -> Option<String> {
    if let Some(set) = el.attr("srcset").or_else(|| el.attr("data-srcset"))
        && let Some(url) = best_from_srcset(set)
    {
        return Some(url);
    }
    ["src", "data-src", "data-lazy-src", "data-original"]
        .iter()
        .filter_map(|a| el.attr(a))
        .map(str::trim)
        .find(|v| !v.is_empty() && !v.starts_with("data:"))
        .map(String::from)
}

fn best_from_srcset(set: &str) -> Option<String> {
    let mut candidates: Vec<(u32, &str)> = Vec::new();
    let mut rest = set;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == ',');
        if rest.is_empty() {
            break;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let mut url = &rest[..end];
        rest = &rest[end..];
        let mut desc = "";
        if url.ends_with(',') {
            url = url.trim_end_matches(',');
        } else {
            let e = rest.find(',').unwrap_or(rest.len());
            desc = rest[..e].trim();
            rest = &rest[e..];
        }
        let width = if let Some(w) = desc.strip_suffix('w') {
            w.parse().unwrap_or(0)
        } else if let Some(x) = desc.strip_suffix('x') {
            (x.parse::<f32>().unwrap_or(1.0) * 400.0) as u32
        } else {
            400
        };
        if !url.starts_with("data:") {
            candidates.push((width, url));
        }
    }
    candidates
        .iter()
        .filter(|(w, _)| *w >= 320)
        .min_by_key(|(w, _)| *w)
        .or_else(|| candidates.iter().max_by_key(|(w, _)| *w))
        .map(|(_, u)| u.to_string())
}

struct Indent {
    first: String,
    rest: String,
    used: bool,
}

struct List {
    ordered: bool,
    next: u32,
}

struct Builder {
    base: Url,
    md: bool,
    /// Drop navigation menus, sidebars, cookie banners and similar UI.
    skip_chrome: bool,
    blocks: Vec<Block>,
    inl: Vec<Inline>,
    pending_space: bool,
    /// A separator (table cell `│`, nav item `·`) is due before the next word.
    pending_sep: Option<&'static str>,
    markers: bool,
    /// Inside a <nav>: lists and blocks are laid out inline.
    nav: usize,
    /// Markdown markers (`**`, `[`…) waiting to be prepended to the next word.
    pending_open: String,
    style: Style,
    indents: Vec<Indent>,
    lists: Vec<List>,
    pre: Option<String>,
    tight: usize,
    cell: usize,
    links: Vec<String>,
    link_ids: HashMap<String, usize>,
    images: Vec<ImageRef>,
}

impl Builder {
    fn new(base: Url, md: bool) -> Self {
        Self {
            base,
            md,
            skip_chrome: false,
            blocks: Vec::new(),
            inl: Vec::new(),
            pending_space: false,
            pending_sep: None,
            markers: false,
            nav: 0,
            pending_open: String::new(),
            style: Style::default(),
            indents: Vec::new(),
            lists: Vec::new(),
            pre: None,
            tight: 0,
            cell: 0,
            links: Vec::new(),
            link_ids: HashMap::new(),
            images: Vec::new(),
        }
    }

    fn walk(&mut self, node: NodeRef<'_, Node>) {
        match node.value() {
            Node::Text(t) => self.text(t),
            Node::Element(el) => self.element(node, el),
            Node::Document | Node::Fragment => self.children(node),
            _ => {}
        }
    }

    fn children(&mut self, node: NodeRef<'_, Node>) {
        if self.pre.is_some() {
            return node.children().for_each(|c| self.walk(c));
        }
        // Previous sibling element: (tag, text), reset by intervening text.
        let mut prev: Option<(&str, String)> = None;
        for child in node.children() {
            let Some(el) = ElementRef::wrap(child) else {
                if let Node::Text(t) = child.value()
                    && !t.trim().is_empty()
                {
                    prev = None;
                }
                self.walk(child);
                continue;
            };
            let name = el.value().name();
            let text = collapse_ws(&el.text().collect::<String>());
            if let Some((prev_name, prev_text)) = &prev
                && prev_text.chars().count() >= 2
                && text.chars().count() >= 2
            {
                // Hover-animation clones repeat the previous sibling verbatim.
                if *prev_name == name && *prev_text == text && matches!(name, "span" | "a" | "div") {
                    continue;
                }
                // Flex/grid items are visually separated even without whitespace.
                self.pending_space = true;
            }
            if matches!(name, "small" | "time") {
                // Usually dates/bylines styled apart from the preceding text.
                self.pending_space = true;
            }
            self.walk(child);
            if !text.is_empty() {
                prev = Some((name, text));
            }
        }
    }

    fn element(&mut self, node: NodeRef<'_, Node>, el: &Element) {
        let name = el.name();
        if SKIP.contains(&name) || is_hidden(el) || (self.skip_chrome && is_chrome(el)) {
            return;
        }
        if self.pre.is_some() {
            // Inside <pre>: keep the raw text, ignore formatting.
            if name == "br" {
                self.text("\n");
            } else {
                self.children(node);
            }
            return;
        }
        if self.nav > 0 {
            match name {
                "li" => {
                    self.pending_sep = Some("·");
                    return self.children(node);
                }
                "ul" | "ol" | "menu" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    self.pending_space = true;
                    return self.children(node);
                }
                _ if BLOCKS.contains(&name) => {
                    self.pending_space = true;
                    return self.children(node);
                }
                _ => {}
            }
        }
        match name {
            "nav" => {
                self.flush();
                self.nav += 1;
                self.children(node);
                self.flush();
                self.nav -= 1;
            }
            "br" => {
                self.inl.push(Inline::Break);
                self.pending_space = false;
            }
            "hr" => self.push_block(Kind::Rule),
            "img" => self.image(el),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush();
                self.children(node);
                self.flush_as(Kind::Heading(name.as_bytes()[1] - b'0'));
            }
            "pre" => self.pre(node),
            "ul" | "ol" | "menu" => {
                self.flush();
                let next = el.attr("start").and_then(|s| s.trim().parse().ok()).unwrap_or(1);
                self.lists.push(List { ordered: name == "ol", next });
                self.children(node);
                self.flush();
                self.lists.pop();
            }
            "li" => {
                let marker = self.bullet();
                let rest = " ".repeat(marker.width());
                self.indented(node, marker, rest, true);
            }
            "blockquote" => {
                let bar = if self.md { "> " } else { "│ " };
                self.indented(node, bar.into(), bar.into(), false);
            }
            "dd" => self.indented(node, "    ".into(), "    ".into(), true),
            "tr" => {
                self.flush();
                self.cell = 0;
                self.tight += 1;
                self.children(node);
                self.flush();
                self.tight -= 1;
            }
            "td" | "th" => {
                // Emitted lazily, so empty spacer cells don't leave stray bars.
                self.pending_sep = (self.cell > 0).then_some(if self.md { "|" } else { "│" });
                self.cell += 1;
                if name == "th" {
                    self.styled(node, |s| s.bold = true, "**");
                } else {
                    self.children(node);
                }
            }
            "a" => self.link(node, el),
            "strong" | "b" => self.styled(node, |s| s.bold = true, "**"),
            "em" | "i" | "cite" | "dfn" | "var" => self.styled(node, |s| s.italic = true, "_"),
            "code" | "kbd" | "samp" | "tt" => self.styled(node, |s| s.code = true, "`"),
            "del" | "s" | "strike" => self.styled(node, |s| s.strike = true, "~~"),
            "dt" | "summary" | "caption" => {
                self.flush();
                self.styled(node, |s| s.bold = true, "**");
                self.flush();
            }
            "figcaption" => {
                self.flush();
                self.styled(node, |s| s.italic = true, "_");
                self.flush();
            }
            _ if BLOCKS.contains(&name) => {
                self.flush();
                self.children(node);
                self.flush();
            }
            _ => self.children(node),
        }
    }

    fn indented(&mut self, node: NodeRef<'_, Node>, first: String, rest: String, tight: bool) {
        self.flush();
        self.indents.push(Indent { first, rest, used: false });
        self.tight += usize::from(tight);
        self.children(node);
        self.flush();
        self.tight -= usize::from(tight);
        self.indents.pop();
    }

    fn bullet(&mut self) -> String {
        let depth = self.lists.iter().filter(|l| !l.ordered).count().saturating_sub(1);
        match self.lists.last_mut() {
            Some(list) if list.ordered => {
                let s = format!("{}. ", list.next);
                list.next += 1;
                s
            }
            _ if self.md => "- ".into(),
            _ => format!("{} ", ["•", "◦", "▪"][depth % 3]),
        }
    }

    fn styled(&mut self, node: NodeRef<'_, Node>, f: impl Fn(&mut Style), mark: &str) {
        let saved = self.style;
        f(&mut self.style);
        let mark = if self.md { mark } else { "" };
        let since = self.blocks.len();
        self.pending_open.push_str(mark);
        self.children(node);
        if !mark.is_empty() {
            if self.pending_open.ends_with(mark) {
                // Nothing was written inside: drop the opener.
                self.pending_open.truncate(self.pending_open.len() - mark.len());
            } else if let Some(word) = self.last_word_mut(since) {
                word.text.push_str(mark);
            }
        }
        self.style = saved;
    }

    /// Last word in the current paragraph, or — if the paragraph was already
    /// flushed (e.g. a link wrapping a whole card) — in the last text block
    /// created at or after index `since`.
    fn last_word_mut(&mut self, since: usize) -> Option<&mut Word> {
        let in_current = self.inl.iter().any(|i| matches!(i, Inline::Word(_)));
        let inlines = if in_current {
            &mut self.inl
        } else {
            &mut self.blocks[since..]
                .iter_mut()
                .rev()
                .find(|b| matches!(b.kind, Kind::Para | Kind::Heading(_)))?
                .inlines
        };
        inlines.iter_mut().rev().find_map(|i| match i {
            Inline::Word(w) => Some(w),
            Inline::Break => None,
        })
    }

    fn link(&mut self, node: NodeRef<'_, Node>, el: &Element) {
        let href = el.attr("href").map(str::trim).unwrap_or("");
        let target = if href.is_empty() || href.starts_with('#') || href.starts_with("javascript:") {
            None
        } else {
            self.base.join(href).ok()
        };
        let Some(target) = target else {
            return self.children(node);
        };

        let url = target.to_string();
        let known = self.link_ids.contains_key(&url);
        let id = self.link_id(&url);
        let saved = self.style;
        self.style.link = Some(id);
        let before = (self.blocks.len(), self.inl.len());
        if self.md {
            self.pending_open.push('[');
        }
        self.children(node);
        if (self.blocks.len(), self.inl.len()) == before {
            // Icon-only links: fall back to their accessible label.
            if let Some(label) = el.attr("aria-label").or_else(|| el.attr("title")) {
                self.text(label);
            }
        }
        self.style = saved;

        let wrote_text = if self.md {
            // `[` still pending means nothing was written inside the link.
            !self.pending_open.ends_with('[') || {
                self.pending_open.pop();
                false
            }
        } else {
            (self.blocks.len(), self.inl.len()) != before
        };
        if !wrote_text {
            if !known && self.links.len() == id {
                // Forget links that produced nothing, to keep numbering dense.
                self.links.pop();
                self.link_ids.remove(&url);
            }
            return;
        }
        if !self.markers {
            return; // interactive mode: links are selectable instead
        }

        if self.blocks.len() == before.0 {
            // Ordinary inline link.
            if self.md {
                if let Some(word) = self.last_word_mut(before.0) {
                    word.text.push_str(&format!("][{id}]"));
                }
            } else {
                self.push_word(format!("[{id}]"), Style { dim: true, ..Style::default() }, false, true);
            }
            return;
        }
        // The link wrapped whole blocks (e.g. a card with a title and a blurb).
        // Markdown links can't span blocks, so close it in the first text
        // block it produced (where the `[` landed); tag that block in color
        // mode too, so the title carries the reference number.
        let marker = if self.md { format!("][{id}]") } else { format!("[{id}]") };
        let first = self.blocks[before.0..]
            .iter_mut()
            .find(|b| matches!(b.kind, Kind::Para | Kind::Heading(_)))
            .and_then(|b| {
                b.inlines.iter_mut().rev().find_map(|i| match i {
                    Inline::Word(w) => Some(w),
                    Inline::Break => None,
                })
            });
        match first {
            Some(word) => word.text.push_str(&marker),
            None => {
                if let Some(word) = self.last_word_mut(before.0) {
                    word.text.push_str(&marker);
                }
            }
        }
    }

    fn link_id(&mut self, url: &str) -> usize {
        if let Some(&id) = self.link_ids.get(url) {
            return id;
        }
        self.links.push(url.to_string());
        let id = self.links.len();
        self.link_ids.insert(url.to_string(), id);
        id
    }

    fn image(&mut self, el: &Element) {
        let tiny = |a: &str| {
            el.attr(a)
                .and_then(|v| v.trim().trim_end_matches("px").parse::<u32>().ok())
                .is_some_and(|v| v <= 24)
        };
        if tiny("width") || tiny("height") {
            return;
        }
        let alt = collapse_ws(el.attr("alt").unwrap_or(""));
        let url = pick_src(el)
            .and_then(|s| self.base.join(&s).ok())
            .map(String::from);
        if url.is_none() && alt.is_empty() {
            return;
        }
        let link = if self.md { None } else { self.style.link };
        self.images.push(ImageRef { url, alt, link });
        self.push_block(Kind::Image(self.images.len() - 1));
    }

    fn pre(&mut self, node: NodeRef<'_, Node>) {
        self.flush();
        self.pre = Some(String::new());
        self.children(node);
        let raw = self.pre.take().unwrap_or_default();
        let raw = raw.strip_prefix('\n').unwrap_or(&raw).trim_end();
        if raw.trim().is_empty() {
            return;
        }
        let lines = raw.lines().map(|l| l.replace('\t', "    ")).collect();
        self.push_block(Kind::Code(lines));
    }

    fn text(&mut self, s: &str) {
        if let Some(buf) = &mut self.pre {
            buf.push_str(s);
            return;
        }
        for ch in s.chars() {
            if ch.is_whitespace() {
                self.pending_space = true;
                continue;
            }
            if let Some(sep) = self.pending_sep.take()
                && self.inl.iter().any(|i| matches!(i, Inline::Word(_)))
            {
                self.push_word(sep.into(), Style { dim: true, ..Style::default() }, true, false);
                self.pending_space = true;
            }
            // Wide (CJK) characters may be broken between, without spaces.
            let cjk = ch.width() == Some(2);
            let style = self.style;
            let append = !self.pending_space
                && !cjk
                && self.pending_open.is_empty()
                && matches!(self.inl.last(), Some(Inline::Word(w)) if !w.cjk && w.style == style);
            if append {
                if let Some(Inline::Word(w)) = self.inl.last_mut() {
                    w.text.push(ch);
                }
            } else {
                let prev = match self.inl.last() {
                    Some(Inline::Word(w)) => Some(w.cjk),
                    _ => None,
                };
                let space = self.pending_space && prev.is_some();
                let glue = !self.pending_space && prev == Some(false) && !cjk;
                let mut text = std::mem::take(&mut self.pending_open);
                text.push(ch);
                self.inl.push(Inline::Word(Word { text, style, space, glue, cjk }));
            }
            self.pending_space = false;
        }
    }

    fn push_word(&mut self, text: String, style: Style, space: bool, glue: bool) {
        let has_prev = matches!(self.inl.last(), Some(Inline::Word(_)));
        self.inl.push(Inline::Word(Word {
            text,
            style,
            space: space && has_prev,
            glue: glue && has_prev,
            cjk: false,
        }));
    }

    fn prefixes(&mut self) -> (String, String) {
        let mut first = String::new();
        let mut rest = String::new();
        for ind in &mut self.indents {
            first.push_str(if ind.used { &ind.rest } else { &ind.first });
            ind.used = true;
            rest.push_str(&ind.rest);
        }
        (first, rest)
    }

    fn flush(&mut self) {
        self.flush_as(Kind::Para);
    }

    fn flush_as(&mut self, kind: Kind) {
        self.pending_space = false;
        self.pending_sep = None;
        while matches!(self.inl.first(), Some(Inline::Break)) {
            self.inl.remove(0);
        }
        while matches!(self.inl.last(), Some(Inline::Break)) {
            self.inl.pop();
        }
        if self.inl.is_empty() {
            return;
        }
        let inlines = std::mem::take(&mut self.inl);
        let (first, rest) = self.prefixes();
        self.blocks.push(Block { kind, inlines, first, rest, tight: self.tight > 0 });
    }

    fn push_block(&mut self, kind: Kind) {
        self.flush();
        let (first, rest) = self.prefixes();
        self.blocks.push(Block { kind, inlines: Vec::new(), first, rest, tight: self.tight > 0 });
    }
}
