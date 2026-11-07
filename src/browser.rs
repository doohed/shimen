//! Interactive full-screen browser: vim-style keys, link selection and
//! hints, mouse support, and a history with back/forward.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use anyhow::{Result, bail};
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::terminal::{self, ClearType};
use crossterm::{cursor, execute, queue, style::Print};
use reqwest::blocking::Client;
use unicode_width::UnicodeWidthChar;
use url::Url;

use crate::document::{Document, ParseOptions};
use crate::images::{self, ImageState};
use crate::page::{self, Opened};
use crate::pixels::ColorDepth;
use crate::render::{self, Layout};

pub struct Config {
    pub width: Option<usize>,
    pub pixel: u32,
    pub image_width: Option<u32>,
    pub image_height: Option<u32>,
    pub images: bool,
    pub full: bool,
}

const HINT_CHARS: &[u8] = b"asdfghjklqwertyuiopzxcvbnm";

const HELP: &[&str] = &[
    "  j k  ↓ ↑     scroll             Tab  S-Tab   next / previous link  ",
    "  d u          half page          Enter        follow selected link  ",
    "  space b      full page          f            follow link by hint   ",
    "  gg G         top / bottom       click        follow link           ",
    "  H L          back / forward     o  O         open URL / edit URL   ",
    "  r            reload             y            copy link URL         ",
    "  Esc          clear selection    q            quit                  ",
];

struct Page {
    url: Url,
    doc: Document,
    images: Vec<ImageState>,
    /// Images still downloading.
    loading: usize,
    layout: Layout,
    /// Terminal size the layout was made for; `None` forces a relayout.
    layout_for: Option<(u16, u16)>,
    scroll: usize,
    selected: Option<usize>,
    /// Identifies this page to the background image loader.
    id: u64,
}

enum Mode {
    Normal,
    Hint(String),
    Prompt(String),
}

type ImageMsg = (u64, usize, ImageState);

pub struct Browser {
    client: Client,
    cfg: Config,
    depth: ColorDepth,
    history: Vec<Page>,
    cur: usize,
    mode: Mode,
    message: Option<String>,
    pending_g: bool,
    help: bool,
    quit: bool,
    next_id: u64,
    tx: Sender<ImageMsg>,
    rx: Receiver<ImageMsg>,
}

/// Puts the terminal in full-screen raw mode; restores it on drop (and on panic).
struct TermGuard;

impl TermGuard {
    fn enter() -> io::Result<Self> {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            default_hook(info);
        }));
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), terminal::EnterAlternateScreen, event::EnableMouseCapture, cursor::Hide)?;
        Ok(Self)
    }
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn restore_terminal() {
    let _ = execute!(io::stdout(), event::DisableMouseCapture, terminal::LeaveAlternateScreen, cursor::Show);
    let _ = terminal::disable_raw_mode();
}

/// Loads `start`, then runs the interactive browser until the user quits.
pub fn run(client: Client, start: Url, cfg: Config, status: impl Fn(&str)) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let mut b = Browser {
        client,
        cfg,
        depth: ColorDepth::detect(),
        history: Vec::new(),
        cur: 0,
        mode: Mode::Normal,
        message: None,
        pending_g: false,
        help: false,
        quit: false,
        next_id: 0,
        tx,
        rx,
    };

    // Load the first page before taking over the screen, so errors print normally.
    status(&format!("fetching {start}"));
    let first = b.load(&start);
    status("");
    match first? {
        Some(page) => b.push(page),
        None => bail!("{start} is not a web page"),
    }

    let _guard = TermGuard::enter()?;
    let mut out = io::stdout();
    let mut redraw = true;
    while !b.quit {
        if redraw {
            b.draw(&mut out)?;
            redraw = false;
        }
        if event::poll(Duration::from_millis(50))? {
            b.handle(event::read()?);
            redraw = true;
        }
        redraw |= b.receive_images();
    }
    Ok(())
}

impl Browser {
    // ── loading & history ──────────────────────────────────────────────

    /// Fetches a page and starts its images loading in the background.
    /// `Ok(None)` means it was handed to the system opener instead.
    fn load(&mut self, url: &Url) -> Result<Option<Page>> {
        let opts = ParseOptions { full: self.cfg.full, markdown: false, markers: false };
        let loaded = match page::open(&self.client, url, &opts, self.cfg.images)? {
            Opened::Page(p) => p,
            Opened::External(_) => return Ok(None),
        };
        let id = self.next_id;
        self.next_id += 1;
        if !loaded.jobs.is_empty() {
            let (client, tx, jobs) = (self.client.clone(), self.tx.clone(), loaded.jobs.clone());
            std::thread::spawn(move || {
                images::load_parallel(&client, &jobs, &|i, state| {
                    let _ = tx.send((id, i, state));
                });
            });
        }
        Ok(Some(Page {
            url: loaded.url,
            doc: loaded.doc,
            images: loaded.images,
            loading: loaded.jobs.len(),
            layout: Layout::default(),
            layout_for: None,
            scroll: 0,
            selected: None,
            id,
        }))
    }

    fn push(&mut self, page: Page) {
        if !self.history.is_empty() {
            self.history.truncate(self.cur + 1);
        }
        self.history.push(page);
        self.cur = self.history.len() - 1;
    }

    fn page(&self) -> &Page {
        &self.history[self.cur]
    }

    fn page_mut(&mut self) -> &mut Page {
        &mut self.history[self.cur]
    }

    fn navigate(&mut self, url: Url, replace: bool) {
        self.mode = Mode::Normal;
        if !matches!(url.scheme(), "http" | "https") {
            return self.open_external(&url);
        }
        self.flash_status(&format!("loading {url} …"));
        match self.load(&url) {
            Ok(Some(page)) if replace => {
                let (scroll, selected) = (self.page().scroll, self.page().selected);
                self.history[self.cur] = page;
                let p = self.page_mut();
                (p.scroll, p.selected) = (scroll, selected);
                self.message = Some("reloaded".into());
            }
            Ok(Some(page)) => {
                self.push(page);
                self.message = None;
            }
            Ok(None) => self.open_external(&url),
            Err(e) => self.message = Some(format!("{e:#}")),
        }
    }

    fn open_external(&mut self, url: &Url) {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(windows) {
            "explorer"
        } else {
            "xdg-open"
        };
        let ok = Command::new(opener)
            .arg(url.as_str())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok();
        self.message = Some(if ok {
            format!("opened externally: {url}")
        } else {
            format!("can't open {url}")
        });
    }

    fn follow(&mut self, anchor: usize) {
        let page = self.page();
        let Some(a) = page.layout.anchors.get(anchor) else { return };
        let Some(href) = page.doc.links.get(a.link - 1) else { return };
        match Url::parse(href) {
            Ok(url) => self.navigate(url, false),
            Err(_) => self.message = Some(format!("bad link: {href}")),
        }
    }

    /// Applies finished image downloads. Returns true if a redraw is needed.
    fn receive_images(&mut self) -> bool {
        let mut redraw = false;
        while let Ok((id, i, state)) = self.rx.try_recv() {
            let current = self.page().id;
            if let Some(p) = self.history.iter_mut().find(|p| p.id == id) {
                p.images[i] = state;
                p.loading = p.loading.saturating_sub(1);
                p.layout_for = None;
                redraw |= id == current;
            }
        }
        redraw
    }

    // ── layout & drawing ───────────────────────────────────────────────

    fn text_width(&self, cols: u16) -> usize {
        let cap = self.cfg.width.unwrap_or(100);
        usize::from(cols).saturating_sub(4).min(cap).max(20)
    }

    fn ensure_layout(&mut self) {
        let size = terminal::size().unwrap_or((80, 24));
        if self.page().layout_for == Some(size) {
            return;
        }
        let width = self.text_width(size.0);
        let view_h = u32::from(size.1.saturating_sub(1));
        let opts = render::Options {
            width,
            markdown: false,
            footer: false,
            image: render::ImageOptions {
                max_cols: self.cfg.image_width.unwrap_or(width as u32),
                max_rows: self.cfg.image_height.unwrap_or((view_h * 2 / 3).clamp(6, 40)),
                pixel: self.cfg.pixel,
                depth: self.depth,
            },
        };
        let page = &mut self.history[self.cur];
        page.layout = render::layout(&page.doc, &page.images, &opts);
        page.layout_for = Some(size);
        if page.selected.is_some_and(|s| s >= page.layout.anchors.len()) {
            page.selected = None;
        }
        self.scroll_by(0); // clamp
    }

    fn view_height(&self) -> usize {
        usize::from(terminal::size().map_or(24, |s| s.1).saturating_sub(1)).max(1)
    }

    fn margin(&self) -> usize {
        let cols = terminal::size().map_or(80, |s| s.0);
        usize::from(cols).saturating_sub(self.text_width(cols)) / 2
    }

    fn draw(&mut self, out: &mut impl Write) -> io::Result<()> {
        self.ensure_layout();
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        let view_h = self.view_height();
        let margin = " ".repeat(self.margin());
        let hints: HashMap<usize, String> = match &self.mode {
            Mode::Hint(typed) => self.hints().into_iter().filter(|(_, l)| l.starts_with(typed.as_str())).collect(),
            _ => HashMap::new(),
        };
        let page = self.page();

        queue!(out, terminal::BeginSynchronizedUpdate)?;
        let mut labelled = HashSet::new();
        for row in 0..view_h {
            let mut s = margin.clone();
            if let Some(line) = page.layout.lines.get(page.scroll + row) {
                for seg in &line.segs {
                    let selected = seg.anchor.is_some() && seg.anchor == page.selected;
                    let extra = if selected { "7" } else { "" };
                    if let Some(a) = seg.anchor
                        && let Some(label) = hints.get(&a)
                        && labelled.insert(a)
                    {
                        s.push_str(&format!("\x1b[1;30;43m{label}\x1b[0m"));
                        // Overlay the label on the link text (like Vimium) so
                        // the layout doesn't shift.
                        if !seg.raw && seg.width >= label.len() {
                            let mut covered = 0;
                            let mut rest = seg.clone();
                            rest.text = seg
                                .text
                                .chars()
                                .skip_while(|c| {
                                    let skip = covered < label.len();
                                    covered += if skip { c.width().unwrap_or(0) } else { 0 };
                                    skip
                                })
                                .collect();
                            if covered > label.len() {
                                rest.text.insert(0, ' '); // a wide char was half covered
                            }
                            render::paint(&mut s, &rest, extra);
                            continue;
                        }
                    }
                    render::paint(&mut s, seg, extra);
                }
            }
            queue!(out, cursor::MoveTo(0, row as u16), Print(s), terminal::Clear(ClearType::UntilNewLine))?;
        }

        if self.help {
            let top = (view_h.saturating_sub(HELP.len() + 2) / 2) as u16;
            let left = (usize::from(cols).saturating_sub(70) / 2) as u16;
            let blank = " ".repeat(68);
            let box_lines = std::iter::once(blank.as_str()).chain(HELP.iter().copied()).chain(std::iter::once(blank.as_str()));
            for (i, l) in box_lines.enumerate() {
                queue!(out, cursor::MoveTo(left, top + i as u16), Print(format!("\x1b[48;5;236;37m {l} \x1b[0m")))?;
            }
        }

        let status = self.status_line(usize::from(cols));
        queue!(out, cursor::MoveTo(0, rows.saturating_sub(1)), Print(status), terminal::EndSynchronizedUpdate)?;
        out.flush()
    }

    fn status_line(&self, cols: usize) -> String {
        let page = self.page();
        let left = match &self.mode {
            Mode::Prompt(buf) => format!("open: {buf}▏"),
            Mode::Hint(typed) => format!("follow: {typed}"),
            Mode::Normal => match (&self.message, page.selected) {
                (Some(m), _) => m.clone(),
                (None, Some(a)) => {
                    let link = page.layout.anchors[a].link;
                    format!("→ {}", page.doc.links[link - 1])
                }
                (None, None) => page.url.to_string(),
            },
        };
        let total = page.layout.lines.len();
        let view_h = self.view_height();
        let pos = if total <= view_h {
            "All".to_string()
        } else if page.scroll == 0 {
            "Top".to_string()
        } else if page.scroll + view_h >= total {
            "Bot".to_string()
        } else {
            format!("{}%", (page.scroll + view_h) * 100 / total)
        };
        let mut right = String::new();
        if page.loading > 0 {
            right.push_str(&format!("images {}… ", page.loading));
        }
        right.push_str(&format!("{pos}  ?:help "));
        let right_w = right.chars().count();
        let left = truncate(&format!(" {left}"), cols.saturating_sub(right_w + 1));
        let pad = cols.saturating_sub(left.1 + right_w);
        format!("\x1b[7m{}{}{right}\x1b[0m", left.0, " ".repeat(pad))
    }

    /// Draws just the status bar (used before blocking work like fetching).
    fn flash_status(&self, msg: &str) {
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        let (text, w) = truncate(&format!(" {msg}"), usize::from(cols));
        let pad = " ".repeat(usize::from(cols).saturating_sub(w));
        let mut out = io::stdout();
        let _ = queue!(out, cursor::MoveTo(0, rows.saturating_sub(1)), Print(format!("\x1b[7m{text}{pad}\x1b[0m")));
        let _ = out.flush();
    }

    // ── links & hints ──────────────────────────────────────────────────

    /// Anchors with at least one segment on screen, in reading order.
    fn visible_anchors(&self) -> Vec<usize> {
        let page = self.page();
        let mut seen = HashSet::new();
        let end = (page.scroll + self.view_height()).min(page.layout.lines.len());
        page.layout.lines[page.scroll.min(end)..end]
            .iter()
            .flat_map(|l| l.segs.iter().filter_map(|s| s.anchor))
            .filter(|a| seen.insert(*a))
            .collect()
    }

    /// Hint labels for visible anchors: one letter each, or two if needed.
    fn hints(&self) -> Vec<(usize, String)> {
        let anchors = self.visible_anchors();
        let n = HINT_CHARS.len();
        let two = anchors.len() > n;
        anchors
            .into_iter()
            .take(n * n)
            .enumerate()
            .map(|(i, a)| {
                let label = if two {
                    format!("{}{}", HINT_CHARS[i / n] as char, HINT_CHARS[i % n] as char)
                } else {
                    (HINT_CHARS[i] as char).to_string()
                };
                (a, label)
            })
            .collect()
    }

    fn select_link(&mut self, forward: bool) {
        let view_h = self.view_height();
        let page = self.page();
        let anchors = &page.layout.anchors;
        if anchors.is_empty() {
            self.message = Some("no links on this page".into());
            return;
        }
        let on_screen = |i: usize| (page.scroll..page.scroll + view_h).contains(&anchors[i].line);
        let next = match page.selected {
            Some(i) if on_screen(i) => {
                if forward {
                    (i + 1 < anchors.len()).then_some(i + 1)
                } else {
                    i.checked_sub(1)
                }
            }
            _ if forward => anchors.iter().position(|a| a.line >= page.scroll),
            _ => anchors.iter().rposition(|a| a.line < page.scroll + view_h),
        };
        let Some(next) = next else { return };
        let line = anchors[next].line;
        self.page_mut().selected = Some(next);
        // Keep the selection on screen, with some context above it.
        let scroll = self.page().scroll;
        if line < scroll || line >= scroll + view_h {
            let target = line.saturating_sub(view_h / 3) as isize;
            self.scroll_by(target - scroll as isize);
        }
    }

    fn anchor_at(&self, col: u16, row: u16) -> Option<usize> {
        let page = self.page();
        let line = page.layout.lines.get(page.scroll + usize::from(row))?;
        let x = usize::from(col).checked_sub(self.margin())?;
        let mut acc = 0;
        for seg in &line.segs {
            if x < acc + seg.width {
                return seg.anchor;
            }
            acc += seg.width;
        }
        None
    }

    // ── input ──────────────────────────────────────────────────────────

    fn scroll_by(&mut self, delta: isize) {
        let view_h = self.view_height();
        let page = self.page_mut();
        let max = page.layout.lines.len().saturating_sub(view_h);
        page.scroll = page.scroll.saturating_add_signed(delta).min(max);
    }

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.key(k),
            Event::Mouse(m) => self.mouse(m),
            _ => {} // resize: the next draw re-lays out
        }
    }

    fn mouse(&mut self, m: MouseEvent) {
        match m.kind {
            MouseEventKind::ScrollDown => self.scroll_by(3),
            MouseEventKind::ScrollUp => self.scroll_by(-3),
            MouseEventKind::Down(MouseButton::Left) => {
                self.mode = Mode::Normal;
                self.help = false;
                if let Some(a) = self.anchor_at(m.column, m.row) {
                    self.page_mut().selected = Some(a);
                    self.follow(a);
                }
            }
            _ => {}
        }
    }

    fn key(&mut self, k: KeyEvent) {
        if self.help {
            self.help = false;
            return;
        }
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Prompt(buf) => self.prompt_key(k, buf),
            Mode::Hint(typed) => self.hint_key(k, typed),
            Mode::Normal => self.normal_key(k),
        }
    }

    fn prompt_key(&mut self, k: KeyEvent, mut buf: String) {
        match k.code {
            KeyCode::Esc => return,
            KeyCode::Enter => {
                let input = buf.trim();
                if input.is_empty() {
                    return;
                }
                let url = self.page().url.join(input).ok().filter(|_| input.starts_with(['/', '.', '?']));
                match url.map_or_else(|| crate::net::normalize_url(input), Ok) {
                    Ok(url) => self.navigate(url, false),
                    Err(e) => self.message = Some(format!("{e:#}")),
                }
                return;
            }
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => buf.clear(),
            KeyCode::Char(c) => buf.push(c),
            _ => {}
        }
        self.mode = Mode::Prompt(buf);
    }

    fn hint_key(&mut self, k: KeyEvent, mut typed: String) {
        match k.code {
            KeyCode::Esc => return,
            KeyCode::Backspace => {
                typed.pop();
            }
            KeyCode::Char(c) => typed.push(c.to_ascii_lowercase()),
            _ => {}
        }
        let matches: Vec<_> = self.hints().into_iter().filter(|(_, l)| l.starts_with(&typed)).collect();
        match matches.as_slice() {
            [] => self.message = Some(format!("no hint '{typed}'")),
            [(anchor, label)] if *label == typed => {
                let anchor = *anchor;
                self.page_mut().selected = Some(anchor);
                self.follow(anchor);
            }
            _ => self.mode = Mode::Hint(typed),
        }
    }

    fn normal_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.view_height() / 2) as isize;
        let full = self.view_height().saturating_sub(2).max(1) as isize;
        let pending_g = std::mem::take(&mut self.pending_g);
        self.message = None;
        match k.code {
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('d') if ctrl => self.scroll_by(half),
            KeyCode::Char('u') if ctrl => self.scroll_by(-half),
            KeyCode::Char('f') if ctrl => self.scroll_by(full),
            KeyCode::Char('b') if ctrl => self.scroll_by(-full),
            KeyCode::Char('e') if ctrl => self.scroll_by(1),
            KeyCode::Char('y') if ctrl => self.scroll_by(-1),
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.scroll_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_by(-1),
            KeyCode::Char('d') => self.scroll_by(half),
            KeyCode::Char('u') => self.scroll_by(-half),
            KeyCode::Char(' ') | KeyCode::PageDown => self.scroll_by(full),
            KeyCode::Char('b') | KeyCode::PageUp => self.scroll_by(-full),
            KeyCode::Char('g') if pending_g => self.page_mut().scroll = 0,
            KeyCode::Char('g') => self.pending_g = true,
            KeyCode::Home => self.page_mut().scroll = 0,
            KeyCode::Char('G') | KeyCode::End => self.scroll_by(isize::MAX / 2),
            KeyCode::Tab | KeyCode::Char(']') => self.select_link(true),
            KeyCode::BackTab | KeyCode::Char('[') => self.select_link(false),
            KeyCode::Enter => match self.page().selected {
                Some(a) => self.follow(a),
                None => self.message = Some("no link selected — Tab to select, f for hints".into()),
            },
            KeyCode::Char('f') => {
                if self.visible_anchors().is_empty() {
                    self.message = Some("no links on screen".into());
                } else {
                    self.mode = Mode::Hint(String::new());
                }
            }
            KeyCode::Char('H' | 'h') | KeyCode::Backspace | KeyCode::Left => {
                if self.cur > 0 {
                    self.cur -= 1;
                } else {
                    self.message = Some("no previous page".into());
                }
            }
            KeyCode::Char('L' | 'l') | KeyCode::Right => {
                if self.cur + 1 < self.history.len() {
                    self.cur += 1;
                } else {
                    self.message = Some("no next page".into());
                }
            }
            KeyCode::Char('o') => self.mode = Mode::Prompt(String::new()),
            KeyCode::Char('O') => self.mode = Mode::Prompt(self.page().url.to_string()),
            KeyCode::Char('r') => {
                let url = self.page().url.clone();
                self.navigate(url, true);
            }
            KeyCode::Char('y') => self.copy_url(),
            KeyCode::Char('?') => self.help = true,
            KeyCode::Esc => self.page_mut().selected = None,
            _ => {}
        }
    }

    fn copy_url(&mut self) {
        let page = self.page();
        let url = match page.selected {
            Some(a) => page.doc.links[page.layout.anchors[a].link - 1].clone(),
            None => page.url.to_string(),
        };
        let tools: &[(&str, &[&str])] = &[
            ("pbcopy", &[]),
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ];
        for (tool, args) in tools {
            let Ok(mut child) = Command::new(tool).args(*args).stdin(Stdio::piped()).spawn() else { continue };
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(url.as_bytes());
            }
            if child.wait().is_ok_and(|s| s.success()) {
                self.message = Some(format!("copied {url}"));
                return;
            }
        }
        self.message = Some("no clipboard tool found".into());
    }
}

/// Truncates to `max` display columns; returns the text and its width.
fn truncate(s: &str, max: usize) -> (String, usize) {
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > max {
            if w < max {
                out.push('…');
                w += 1;
            }
            break;
        }
        out.push(ch);
        w += cw;
    }
    (out, w)
}
