//! Downloading and decoding images, in parallel.

use std::sync::atomic::{AtomicUsize, Ordering};

use image::DynamicImage;
use reqwest::blocking::Client;

use crate::net;

pub enum ImageState {
    /// Images disabled, or the image has no URL.
    Off,
    Loading,
    /// Decoded and downscaled (terminals need few pixels).
    Ready(DynamicImage),
    Failed(String),
}

/// Max stored size; rasterization downsamples further to the terminal size.
const KEEP_W: u32 = 480;
const KEEP_H: u32 = 960;
const WORKERS: usize = 8;

pub fn load(client: &Client, url: &str) -> ImageState {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    if path.ends_with(".svg") || path.ends_with(".svgz") {
        return ImageState::Failed("SVG not supported".into());
    }
    match net::fetch_image(client, url) {
        Ok(bytes) => decode(&bytes),
        Err(e) => ImageState::Failed(e),
    }
}

pub fn decode(bytes: &[u8]) -> ImageState {
    match image::load_from_memory(bytes) {
        Ok(img) if img.width() > KEEP_W || img.height() > KEEP_H => ImageState::Ready(img.thumbnail(KEEP_W, KEEP_H)),
        Ok(img) => ImageState::Ready(img),
        Err(_) => ImageState::Failed("unsupported format".into()),
    }
}

/// Loads `jobs` (image index, URL) on a small worker pool, calling `done`
/// as each finishes. Blocks until all are loaded.
pub fn load_parallel(client: &Client, jobs: &[(usize, String)], done: &(dyn Fn(usize, ImageState) + Sync)) {
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..jobs.len().min(WORKERS) {
            s.spawn(|| {
                while let Some((i, url)) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                    done(*i, load(client, url));
                }
            });
        }
    });
}

/// Initial states plus the jobs needed to load them.
pub fn plan(doc: &crate::document::Document, enabled: bool) -> (Vec<ImageState>, Vec<(usize, String)>) {
    let mut states = Vec::with_capacity(doc.images.len());
    let mut jobs = Vec::new();
    for (i, img) in doc.images.iter().enumerate() {
        match &img.url {
            Some(url) if enabled => {
                states.push(ImageState::Loading);
                jobs.push((i, url.clone()));
            }
            _ => states.push(ImageState::Off),
        }
    }
    (states, jobs)
}
