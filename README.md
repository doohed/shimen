# shimen

**紙面** (*shimen*, "printed page") is a terminal web browser that renders pages as readable, Markdown-style text with pixel-art images.

```sh
shimen <url>
```

<img width="1000" height="738" alt="shimen" src="https://github.com/user-attachments/assets/65f3bfcd-1ac8-40cc-81bb-d01eec4b15c1" />


## Features

- Extracts the main content of a page and keeps site navigation compact
- Headings, lists, quotes, tables and code blocks styled for the terminal
- Images drawn as half-block pixel art, in truecolor or 256 colors
- Keyboard and mouse navigation with link hints and history
- Correct wrapping of Japanese, Chinese and Korean text
- Plain Markdown output when piped

## Installation

Requires Rust 1.88 or newer.

```sh
git clone https://github.com/doohed/shimen.git
cd shimen
cargo install --path .
```

The binary is installed to `~/.cargo/bin`, which must be on your `PATH`.

## Usage

```sh
shimen <url> [options]
```

The scheme is optional: `shimen example.com` opens `https://example.com`.

In a terminal, shimen opens an interactive browser. When its output is redirected, it prints Markdown instead:

```sh
shimen example.com > page.md
```

### Options

| Option | Description |
|---|---|
| `-w, --width <cols>` | Text width. Default: terminal width, up to 100 |
| `-p, --pixel <n>` | Pixel size, 1–16. Larger values give a chunkier look. Default: 1 |
| `--image-width <cols>` | Maximum image width. Default: text width |
| `--image-height <rows>` | Maximum image height. Default: fits the terminal |
| `--no-images` | Do not download images |
| `--print` | Print the page and exit instead of browsing |
| `--markdown` | Print plain Markdown |
| `--full` | Render the whole page, not only the main content |

## Keys

<img width="1118" height="842" alt="Screenshot 2026-11-07 at 3 22 21 a m" src="https://github.com/user-attachments/assets/ff361b74-1dbc-4d0a-aca1-045b0443d545" />

| Key | Action |
|---|---|
| `j` `k` / `↓` `↑` | Scroll one line |
| `d` `u` / `Ctrl-d` `Ctrl-u` | Scroll half a page |
| `Space` `b` / `PgDn` `PgUp` | Scroll a full page |
| `gg` `G` / `Home` `End` | Go to top / bottom |
| `Tab` `Shift-Tab` / `]` `[` | Select next / previous link |
| `Enter` | Follow selected link |
| `f` | Show link hints; type a hint to follow it |
| `H` `L` / `←` `→` / `Backspace` | Back / forward |
| `o` | Open a URL |
| `O` | Edit the current URL |
| `r` | Reload |
| `y` | Copy the selected link, or the page URL |
| `Esc` | Clear selection |
| `?` | Show help |
| `q` | Quit |

Click a link to follow it, and use the scroll wheel to scroll. Because shimen captures the mouse, selecting text requires a modifier key in most terminals (`Option`-drag in iTerm2, `Shift`-drag elsewhere).

Links that are not web pages, such as `mailto:` links and PDFs, open in the system's default application.

## Environment

| Variable | Effect |
|---|---|
| `COLORTERM=truecolor` | Enables 24-bit color images. Detected automatically in most modern terminals |
| `NO_COLOR` | Disables colors and images, and prints Markdown |

## Limitations

- JavaScript is not executed, so pages that render their content client-side may appear empty.
- SVG and AVIF images are not supported.
- Links within the same page (`#section`) are not followed.

## Project structure

| File | Responsibility |
|---|---|
| `src/main.rs` | Command-line interface and print mode |
| `src/browser.rs` | Interactive browser |
| `src/page.rs` | Loading a URL into a document |
| `src/net.rs` | HTTP client |
| `src/document.rs` | HTML to document model |
| `src/render.rs` | Layout and styling |
| `src/images.rs` | Parallel image loading |
| `src/pixels.rs` | Image to terminal pixel art |
