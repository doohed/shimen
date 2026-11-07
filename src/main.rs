mod browser;
mod document;
mod images;
mod net;
mod page;
mod pixels;
mod render;

use std::io::{self, IsTerminal, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Result, bail};
use clap::Parser;

use document::ParseOptions;
use page::Opened;
use pixels::ColorDepth;

/// Read web pages in the terminal, like Markdown, with pixel-art images.
///
/// In a terminal, opens an interactive browser (press ? for keys).
/// When piped, prints Markdown instead (e.g. `shimen example.com > page.md`).
#[derive(Parser)]
#[command(version, before_help = concat!("紙面 · shimen v", env!("CARGO_PKG_VERSION")))]
struct Args {
    /// Page to read (the https:// is optional)
    url: String,

    /// Text width in columns [default: terminal width, max 100]
    #[arg(short, long)]
    width: Option<usize>,

    /// Pixel size: 1 = finest, larger = chunkier pixels
    #[arg(short, long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=16))]
    pixel: u32,

    /// Maximum image width in columns [default: text width]
    #[arg(long)]
    image_width: Option<u32>,

    /// Maximum image height in rows [default: fits the terminal]
    #[arg(long)]
    image_height: Option<u32>,

    /// Don't download or draw images
    #[arg(long)]
    no_images: bool,

    /// Print the page and exit instead of browsing interactively
    #[arg(long, alias = "no-pager")]
    print: bool,

    /// Print plain Markdown (no colors, no images)
    #[arg(long)]
    markdown: bool,

    /// Render the whole <body>, not just the main content
    #[arg(long)]
    full: bool,
}

fn main() {
    if let Err(e) = run(Args::parse()) {
        eprintln!("shimen: {e:#}");
        std::process::exit(1);
    }
}

fn run(args: Args) -> Result<()> {
    let tty = io::stdout().is_terminal();
    let markdown = args.markdown || !tty || std::env::var_os("NO_COLOR").is_some();
    let client = net::client()?;
    let url = net::normalize_url(&args.url)?;

    if !markdown && !args.print && io::stdin().is_terminal() {
        let cfg = browser::Config {
            width: args.width,
            pixel: args.pixel,
            image_width: args.image_width,
            image_height: args.image_height,
            images: !args.no_images,
            full: args.full,
        };
        return browser::run(client, url, cfg, status);
    }
    print_page(&args, client, url, markdown)
}

fn print_page(args: &Args, client: reqwest::blocking::Client, url: url::Url, markdown: bool) -> Result<()> {
    let term = terminal_size::terminal_size();
    let width = args
        .width
        .unwrap_or_else(|| term.map_or(80, |(w, _)| usize::from(w.0).min(100)))
        .max(20);
    let max_rows = args
        .image_height
        .unwrap_or_else(|| term.map_or(24, |(_, h)| u32::from(h.0).saturating_sub(4).clamp(8, 40)));

    status(&format!("fetching {url}"));
    let opened = page::open(&client, &url, &ParseOptions { full: args.full, markdown, markers: true }, !markdown && !args.no_images);
    status("");
    let mut loaded = match opened? {
        Opened::Page(p) => p,
        Opened::External(ctype) => bail!("can't display {ctype}"),
    };

    if !loaded.jobs.is_empty() {
        let states = Mutex::new(std::mem::take(&mut loaded.images));
        let done = AtomicUsize::new(0);
        let total = loaded.jobs.len();
        images::load_parallel(&client, &loaded.jobs, &|i, state| {
            states.lock().unwrap()[i] = state;
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            status(&format!("loading images {d}/{total}"));
        });
        status("");
        loaded.images = states.into_inner().unwrap();
    }

    let opts = render::Options {
        width,
        markdown,
        footer: true,
        image: render::ImageOptions {
            max_cols: args.image_width.unwrap_or(width as u32),
            max_rows,
            pixel: args.pixel,
            depth: ColorDepth::detect(),
        },
    };
    let text = render::layout(&loaded.doc, &loaded.images, &opts).to_text(markdown);
    match io::stdout().lock().write_all(text.as_bytes()) {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        r => Ok(r?),
    }
}

/// Transient progress line on stderr (only when it's a terminal).
fn status(msg: &str) {
    let mut err = io::stderr();
    if err.is_terminal() {
        let _ = write!(err, "\r\x1b[2K");
        if !msg.is_empty() {
            let _ = write!(err, "\x1b[2m{msg}…\x1b[0m");
        }
        let _ = err.flush();
    }
}
