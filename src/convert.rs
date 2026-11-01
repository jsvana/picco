//! EPUB -> OPUB conversion.
//!
//! The Picco cannot open EPUB directly; booxtiles.com converts books in the
//! browser with Boox's WASM engine (`enginejs`) and uploads the `.opub` it
//! produces. We do the same: launch the locally installed Chrome headless,
//! open booxtiles.com (so the engine's own assets load from their origin),
//! import the engine module and call `convertToReflowNano` with the same
//! options the web app uses for the Picco's 480x800 panel. Bytes travel as
//! base64 over the DevTools channel in chunks small enough for its message
//! limits (the site's CSP blocks fetches to anything else, loopback included).

use std::fs;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use headless_chrome::{Browser, LaunchOptions, Tab};

use crate::state::sha256_file;

pub const SITE: &str = "https://booxtiles.com";
/// Public route of the SPA (no login needed); any page on the origin works.
const PAGE: &str = "https://booxtiles.com/download/";

/// Where converted books are cached. Keyed by source hash so a re-run is free.
pub fn cache_dir() -> Result<PathBuf> {
    let dir = dirs::cache_dir()
        .context("no cache directory on this platform")?
        .join("picco")
        .join("opub");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn opub_name(src: &Path) -> String {
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("book");
    format!("{stem}.opub")
}

/// Convert `src` unless a cached result exists. Returns the `.opub` path.
pub fn convert(src: &Path, out_dir: Option<&Path>, quiet: bool) -> Result<PathBuf> {
    let hash = sha256_file(src)?;
    let name = opub_name(src);
    let cache = cache_dir()?.join(format!("{}-{name}", &hash[..12]));
    let dest = match out_dir {
        Some(d) => d.join(&name),
        None => cache.clone(),
    };
    if cache.is_file() && fs::metadata(&cache)?.len() > 0 {
        if dest != cache {
            fs::copy(&cache, &dest)?;
        }
        if !quiet {
            eprintln!("cached {}", dest.display());
        }
        return Ok(dest);
    }

    let bytes = fs::read(src).with_context(|| format!("read {}", src.display()))?;
    let src_name = src
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("{} has no usable name", src.display()))?
        .to_owned();

    if !quiet {
        eprintln!("converting {src_name} ({} bytes) with Boox engine ...", bytes.len());
    }
    let out = run_engine(&src_name, &name, bytes, quiet)?;
    let tmp = cache.with_extension("opub.part");
    fs::write(&tmp, &out)?;
    fs::rename(&tmp, &cache)?;
    if dest != cache {
        fs::copy(&cache, &dest)?;
    }
    if !quiet {
        eprintln!("wrote {} ({} bytes)", dest.display(), out.len());
    }
    Ok(dest)
}

/// Find the current hashed chunk names on booxtiles.com.
fn engine_chunks() -> Result<(String, String)> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let index = client.get(format!("{SITE}/")).send()?.error_for_status()?.text()?;
    let main = find(&index, "/assets/index-", ".js")
        .context("booxtiles.com index.html has no /assets/index-*.js")?;
    let js = client
        .get(format!("{SITE}{main}"))
        .send()?
        .error_for_status()?
        .text()?;
    let engine = find(&js, "enginejs.es-", ".js").context("engine chunk not referenced")?;
    let worker = find(&js, "_virtual_enginejs-worker-", ".js").context("worker chunk missing")?;
    Ok((engine, worker))
}

/// First substring that starts with `prefix` and ends with `suffix`,
/// containing only chunk-name characters.
fn find(hay: &str, prefix: &str, suffix: &str) -> Option<String> {
    let start = hay.find(prefix)?;
    let rest = &hay[start..];
    let end = rest.find(suffix)? + suffix.len();
    let cand = &rest[..end];
    if cand
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    {
        Some(cand.to_owned())
    } else {
        None
    }
}

/// Raw bytes per DevTools message (base64 inflates by 4/3; websocket cap is 64 MiB).
const CHUNK: usize = 8 * 1024 * 1024;

fn run_engine(src_name: &str, out_name: &str, input: Vec<u8>, quiet: bool) -> Result<Vec<u8>> {
    let (engine, worker) = engine_chunks()?;

    let browser = Browser::new(LaunchOptions {
        headless: true,
        idle_browser_timeout: Duration::from_secs(60 * 60),
        ..Default::default()
    })
    .context("launch Chrome (is Google Chrome installed?)")?;
    let tab = browser.new_tab()?;
    tab.set_default_timeout(Duration::from_secs(60 * 60));
    tab.navigate_to(PAGE)?.wait_until_navigated()?;

    // 1. Stream the source in.
    eval_str(&tab, "window.__picco_in = []; 'ok'")?;
    for chunk in input.chunks(CHUNK) {
        let b64 = B64.encode(chunk);
        eval_str(&tab, &format!("window.__picco_in.push({}); 'ok'", js_str(&b64)))?;
    }

    // 2. Convert.
    let script = JS
        .replace("__ENGINE__", &engine)
        .replace("__WORKER__", &worker)
        .replace("__SRC__", &js_str(src_name))
        .replace("__OUT__", &js_str(out_name));
    let summary = eval_str(&tab, &script)?;
    if !quiet {
        eprintln!("engine: {summary}");
    }
    let size: usize = summary
        .strip_prefix("ok ")
        .and_then(|n| n.trim().parse().ok())
        .ok_or_else(|| anyhow!("conversion failed: {summary}"))?;

    // 3. Stream the result out.
    let mut out = Vec::with_capacity(size);
    let mut offset = 0usize;
    while offset < size {
        let b64 = eval_str(
            &tab,
            &format!("window.__picco_b64({offset}, {CHUNK})"),
        )?;
        let bytes = B64.decode(b64.as_bytes()).context("decode engine output")?;
        if bytes.is_empty() {
            bail!("engine output ended early at {offset}/{size}");
        }
        offset += bytes.len();
        out.extend_from_slice(&bytes);
    }
    drop(browser);
    if out.len() != size {
        bail!("engine output size mismatch: {} vs {size}", out.len());
    }
    Ok(out)
}

fn eval_str(tab: &Tab, script: &str) -> Result<String> {
    let result = tab.evaluate(script, true).context("evaluate in page")?;
    Ok(result
        .value
        .as_ref()
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{result:?}")))
}

fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"book\"".into())
}

/// Runs inside booxtiles.com. Mirrors `W6`/`j6`/`Yb`/`Zb`/`e1` in the app
/// bundle: Picco panel 480x800 at 96 dpi, four cover sizes, one book-image
/// profile.
const JS: &str = r#"(async () => {
  try {
    const eng = await import('/assets/__ENGINE__');
    const wk = await import('/assets/__WORKER__');
    eng.configureEnginejsAssets({ workerUrl: '/' + wk.ENGINEJS_STATIC_DIR + '/' + wk.ENGINEJS_WORKER_FILE });
    if (typeof eng.preloadEngine === 'function') { try { await eng.preloadEngine(() => {}); } catch (e) {} }
    const parts = (window.__picco_in || []).map(p => Uint8Array.from(atob(p), c => c.charCodeAt(0)));
    const total = parts.reduce((n, p) => n + p.length, 0);
    const bytes = new Uint8Array(total);
    let off = 0; for (const p of parts) { bytes.set(p, off); off += p.length; }
    window.__picco_in = null;
    const dm = new eng.DocumentManager();
    const doc = await dm.document(__SRC__, true, bytes);
    try {
      await doc.openDocument({ displayMetrics: { density: 1, densityDpi: 96, height: 800, scaledDensity: 1, width: 480, xDpi: 96, yDpi: 96 } });
      const cover = (w, h) => [
        { type: 1, pixelFormat: 0, channelDepth: 3, packedStorage: 1, ditherMode: 0, width: 480, height: 800 },
        { type: 2, pixelFormat: 0, channelDepth: 0, packedStorage: 1, ditherMode: 1, width: 256, height: 384 },
        { type: 3, pixelFormat: 0, channelDepth: 0, packedStorage: 1, ditherMode: 1, width: 176, height: 264 },
        { type: 4, pixelFormat: 0, channelDepth: 0, packedStorage: 1, ditherMode: 1, width: w, height: h },
      ];
      const opts = {
        output: { path: '/work/out/' + __OUT__ },
        covers: { imageOptions: cover(128, 192) },
        bookImageOptions: [ { type: 1, pixelFormat: 0, channelDepth: 3, packedStorage: 1, ditherMode: 0, width: 480, height: 800, contrast: 1, brightness: 0 } ],
      };
      const res = await doc.convertToReflowNano(opts, () => {});
      if (!res || !res.success) return 'error: ' + (res && res.error ? String(res.error) : 'convert returned no success');
      if (!(res.output instanceof Uint8Array) || res.output.byteLength === 0) return 'error: empty output';
      const out = res.output;
      window.__picco_b64 = (start, len) => {
        const slice = out.subarray(start, Math.min(out.length, start + len));
        let s = ''; for (let i = 0; i < slice.length; i += 32768) s += String.fromCharCode.apply(null, slice.subarray(i, i + 32768));
        return btoa(s);
      };
      return 'ok ' + out.byteLength;
    } finally {
      try { await doc.destroy(); } catch (e) {}
    }
  } catch (e) {
    return 'error: ' + (e && e.stack ? e.stack : String(e));
  }
})()"#;
