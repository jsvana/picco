---
name: picco
description: Put books on Jay's BOOX Picco e-reader (ESP32 CrossPoint fork, not Android) using the `picco` Rust CLI in ~/projects/picco. Use when asked to transfer, push, sync or install ebooks/fonts/wallpapers on the Picco, or to find where Jay's ebooks live.
---

# Picco transfers

## Before anything
1. Jay must open **Transfer** on the Picco's home screen and keep it open; the
   device's web server (port 80) only runs on that screen. Ask for that first
   if `picco ping` fails.
2. Build once: `cd ~/projects/picco && cargo build --release`; binary at
   `target/release/picco` (or `cargo install --path .`).

## Commands
- `picco discover` — mDNS `_nanoreader._tcp`, then /24 sweep; saves default host.
- `picco ping` — confirm the device is listening.
- `picco convert <files> [-o dir]` — EPUB -> `.opub` only.
- `picco push <files>` — type inferred from extension (`.epub/.opub` book,
  `.png/.jpg` image, `.ttf/.otf` font). Skips files already pushed
  (SHA-256 in `~/Library/Application Support/picco/state.json`); `--force` resends.
- `picco sync <dir> [-r] [--dry-run]` — push every unpushed book in a folder,
  honouring `shelf.toml` (order + display titles).
- `picco shelf init <dir>` / `picco shelf show <dir>` — draft / inspect `shelf.toml`.
- `picco pushed` — history.

## Organizing
The Bookshelf is flat, newest-first, title-only (truncated ~10 chars), no
folders. Organization = short numbered titles in `shelf.toml` + push order.
Canonical manifest lives next to the books: `/mnt/storage/ebooks/shelf.toml`
on blackpearl, mirrored in `~/tmp/picco-books/shelf.toml`. Keep both in sync
(rsync the folder). A retitled book is a new tile; the old one must be deleted
on the device.

## Where Jay's books are
- DRM-free library: `/mnt/storage/ebooks` on blackpearl (also in Audiobookshelf).
  Mirror on the Mac mini: `~/tmp/picco-books`.
- Phone downloads land in iCloud Drive, not `~/Downloads`:
  `~/Library/Mobile Documents/com~apple~CloudDocs/Downloads`.
- Kobo purchases are Adobe-DRM and cannot be moved to the Picco. Do not offer
  DRM removal.

## Format caveat
The Picco cannot open raw EPUBs. `picco push`/`sync` convert books to `.opub`
automatically by driving Google Chrome headless against booxtiles.com's WASM
engine (needs Chrome installed and internet access; 740 KB EPUB -> ~4 MB
.opub in ~20 s; big image-heavy books take minutes). `picco convert` does
just the conversion. Never push a raw `.epub` with `--type book` by hand.
PDFs are not supported at all. Stray files on the card can only be deleted
on the device; there is no delete endpoint.

## Don't
- Don't scan for adb, BooxDrop (:8085) or WebDAV; the Picco has none.
- Don't commit or push the repo without Jay asking.
