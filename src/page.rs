//! Fetch a URL and turn whatever comes back into a `Document`.

use anyhow::Result;
use reqwest::blocking::Client;
use url::Url;

use crate::document::{self, Document, ParseOptions};
use crate::images::{self, ImageState};
use crate::net::{self, Body};

pub struct Loaded {
    /// Final URL after redirects.
    pub url: Url,
    pub doc: Document,
    pub images: Vec<ImageState>,
    /// Images still to download: (index, URL).
    pub jobs: Vec<(usize, String)>,
}

pub enum Opened {
    Page(Box<Loaded>),
    /// Not displayable here (PDF, archive…): the content type.
    External(String),
}

pub fn open(client: &Client, url: &Url, opts: &ParseOptions, load_images: bool) -> Result<Opened> {
    let page = net::fetch_page(client, url)?;
    let name = page
        .url
        .path_segments()
        .and_then(|mut s| s.next_back())
        .filter(|s| !s.is_empty())
        .unwrap_or("")
        .to_string();
    let (doc, image) = match page.body {
        Body::Html(html) => (document::parse(&html, &page.url, opts), None),
        Body::Text(text) => {
            let html = format!("<title>{}</title><pre>{}</pre>", escape(&name), escape(&text));
            (document::parse(&html, &page.url, opts), None)
        }
        Body::Image(bytes) => {
            let html = format!("<title>{}</title><img src=\"{}\">", escape(&name), escape(page.url.as_str()));
            (document::parse(&html, &page.url, opts), Some(images::decode(&bytes)))
        }
        Body::Other(ctype) => return Ok(Opened::External(ctype)),
    };
    let (mut states, mut jobs) = images::plan(&doc, load_images);
    if let Some(img) = image {
        // Already downloaded: no need to fetch it again.
        jobs.clear();
        if let Some(first) = states.first_mut() {
            *first = img;
        }
    }
    Ok(Opened::Page(Box::new(Loaded { url: page.url, doc, images: states, jobs })))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
