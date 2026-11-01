# picco

Push books, images and fonts to a BOOX Picco over Wi-Fi from the command line.

```
cargo install --path .
picco discover            # finds the Picco (Transfer screen must be open), saves it
picco ping
picco push book.epub cover.png
picco sync ~/Books        # push every book not pushed before
picco pushed              # what has been sent
```

`--host 192.168.1.29` or `PICCO_HOST` overrides the saved default.

## Organizing the shelf

The Picco's Bookshelf is a flat grid, newest upload first, showing each
book's title truncated after about ten characters. No folders, no sorting.
So `picco` organizes through the two knobs that exist: title text and push
order.

```
picco shelf init ~/Books     # drafts shelf.toml from the EPUBs' metadata
$EDITOR ~/Books/shelf.toml   # reorder; give short titles like "1 Alloy of Law"
picco shelf show ~/Books     # the push plan
picco sync ~/Books           # pushes bottom-up so the top entry lands first
```

A `title` in `shelf.toml` is written into a copy of the EPUB before
conversion, so the device shows it. Changing a title makes it a new book on
the device; delete the old tile there (no delete endpoint exists).
`picco push --title "..." one.epub` does the same for a single file.

## Device protocol

While the Picco's Transfer screen is open it serves HTTP on port 80:
`HEAD /ping` answers 200; `POST /upload?fileType=0|1|2` (book/image/font)
takes one multipart part named `file`. It advertises itself over mDNS as
`_nanoreader._tcp`. Nothing else is exposed.

## Format notes

The Picco cannot open a raw EPUB ("Open Failed: file is corrupted or the
format is not supported", confirmed 2026-11-01). booxtiles.com converts books
in the browser with Boox's WebAssembly engine into `.opub` (pre-laid-out for
the 480x800 panel) and uploads that. `picco` does the same: `push`/`sync`
convert any book that is not already `.opub` by driving the locally installed
Google Chrome headless against booxtiles.com, then upload the result. Converted
files are cached in `~/Library/Caches/picco/opub/` keyed by source hash.
`picco convert` runs the conversion alone.

The engine is a 12 MB Emscripten module served brotli-compressed with a SHA-1
pin, loaded by an Emscripten runtime plus a Web Worker. Running it without
Chrome would mean embedding a JS engine with WebAssembly support (V8 via
`deno_core`) and shimming `Worker`, `fetch` and `caches`; see CLAUDE.md.
