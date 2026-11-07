use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use url::Url;

const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;

pub struct Page {
    /// Final URL after redirects; used to resolve relative links.
    pub url: Url,
    pub body: Body,
}

pub enum Body {
    Html(String),
    /// Plain text, shown preformatted.
    Text(String),
    /// A bare image (e.g. following a link to a .png).
    Image(Vec<u8>),
    /// Something we can't display (PDF, zip…), with its content type.
    Other(String),
}

pub fn client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent(concat!("shimen/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()?)
}

/// Accepts `example.com` as well as full URLs.
pub fn normalize_url(input: &str) -> Result<Url> {
    let s = if input.contains("://") {
        input.to_string()
    } else {
        format!("https://{input}")
    };
    Url::parse(&s).with_context(|| format!("invalid URL: {input}"))
}

pub fn fetch_page(client: &Client, url: &Url) -> Result<Page> {
    let mut target = url.clone();
    target.set_fragment(None);
    let resp = client
        .get(target)
        .header(ACCEPT, "text/html,application/xhtml+xml;q=0.9,*/*;q=0.8")
        .send()
        .with_context(|| format!("could not reach {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!("HTTP {status}");
    }
    let url = resp.url().clone();
    let ctype = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/html")
        .to_ascii_lowercase();
    let body = if ctype.contains("html") || ctype.contains("xml") {
        Body::Html(resp.text().context("could not read page body")?)
    } else if ctype.starts_with("text/") || ctype.contains("json") {
        Body::Text(resp.text().context("could not read page body")?)
    } else if ctype.starts_with("image/") {
        Body::Image(resp.bytes().context("could not read image")?.to_vec())
    } else {
        Body::Other(ctype.split(';').next().unwrap_or_default().trim().to_string())
    };
    Ok(Page { url, body })
}

/// Fetches image bytes. Errors are short, human-readable strings meant for captions.
pub fn fetch_image(client: &Client, url: &str) -> Result<Vec<u8>, String> {
    // AVIF is deliberately left out of Accept: we can't decode it, and image
    // CDNs (e.g. Next.js /_next/image) will negotiate WebP/PNG/JPEG instead.
    let resp = client
        .get(url)
        .header(ACCEPT, "image/webp,image/png,image/jpeg,image/gif,image/*;q=0.5")
        .send()
        .map_err(|e| {
            if e.is_timeout() {
                "timed out".to_string()
            } else {
                "network error".to_string()
            }
        })?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    if resp.content_length().is_some_and(|n| n > MAX_IMAGE_BYTES) {
        return Err("too large".into());
    }
    let bytes = resp.bytes().map_err(|_| "network error".to_string())?;
    Ok(bytes.to_vec())
}
