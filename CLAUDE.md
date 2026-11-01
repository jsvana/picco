# CLAUDE.md — picco

`picco` is a small Rust CLI that pushes books, images and fonts to Jay's
BOOX Picco over Wi-Fi, replacing the booxtiles.com web app for the LAN leg.
Single binary: clap + reqwest (blocking) + mdns-sd.

## What the device is

The Picco is **not an Android Boox**. It is an ESP32-S3 running Boox's fork of
CrossPoint ("NanoReader" internally). No BooxDrop, no adb, no Play Store, no
WebDAV. Books live on the microSD card. Its only network surface is a tiny
ESP-IDF httpd on port 80 that runs **only while the home-screen Transfer mode
is open**. The protocol (lifted from the booxtiles.com JS bundle):

| Request | Meaning |
|---|---|
| `HEAD /ping` | 200 when listening (GET is 405) |
| `POST /upload?fileType=N` | multipart/form-data, one part named `file`; N: 0 book, 1 image, 2 font |
| mDNS `_nanoreader._tcp` | how the phone app discovers it |

Everything else is a 404 ("Nothing matches the given URI").

A raw `.epub` uploads fine but the device shows "Open Failed" (tested
2026-11-01). Books must be converted to `.opub` first. `src/convert.rs` does
it the way booxtiles.com does: headless Google Chrome (`headless_chrome`
crate) opens `https://booxtiles.com/download/`, imports
`/assets/enginejs.es-<hash>.js` (name discovered from the live index bundle
each run), calls `configureEnginejsAssets`, `DocumentManager.document(name,
true, bytes)`, `openDocument({displayMetrics: 480x800 @96dpi})`,
`convertToReflowNano({output, covers, bookImageOptions})`. Bytes go in and
out as base64 chunks via `Runtime.evaluate`; the site's CSP blocks fetches
to loopback, so a local HTTP relay does not work. Output is cached under
`~/Library/Caches/picco/opub/<sha12>-<name>.opub`.

If Chrome ever becomes unacceptable: the engine is an Emscripten module
(`enginejs-static/engine_sdk-<sha1>.wasm`, brotli on the wire, SHA-1 pinned
in the loader) plus `fileWorker-*.js`. Going browser-free means embedding V8
(`deno_core`) and shimming Worker/fetch/caches/crypto.subtle for the glue.
Not a weekend job; do not start it without Jay asking.

## Layout

- `src/device.rs` — `Device` (ping/upload), `FileType` with extension inference.
- `src/convert.rs` — EPUB -> OPUB via headless Chrome + booxtiles engine; cache.
- `src/epub.rs` — read title/author from the OPF; write a title-patched copy (deterministic zip).
- `src/shelf.rs` — `shelf.toml` manifest: reading order + display titles; `plan()` gives push order.
- `src/discover.rs` — mDNS browse, then /24 sweep (`TCP:80` + `/ping`).
- `src/state.rs` — `~/Library/Application Support/picco/state.json`: default
  host plus a map of SHA-256 -> pushed record, so `sync` never re-sends a book.
- `skills/picco/SKILL.md` — the Claude Code skill; symlinked from
  `~/.claude/skills/picco`.

## Bookshelf facts (observed 2026-11-01)

Flat grid, 6 per page, newest upload first, title from OPUB metadata,
truncated after ~10 chars. `path=` on upload is ignored (no folders). No sort,
no author view, no delete over the network. Therefore: series position goes in
the title ("1 Alloy of Law"), and `sync` pushes shelf.toml bottom-up.

## Rules

- Keep it dependency-light and blocking; this is a tool run by hand or by an
  agent, not a daemon.
- Never strip DRM or touch Kobo/ADE files. DRM'd store purchases stay in
  their store apps.
- `cargo build` must stay warning-free; `cargo test` runs the unit tests.
