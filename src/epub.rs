//! Minimal EPUB metadata access: read title/author, and rewrite the title.
//!
//! The Picco's Bookshelf shows only the OPUB's title (truncated after ~10
//! characters) in a flat, newest-first grid. Series position therefore has
//! to live in the title text, so we patch `<dc:title>` in the OPF before the
//! book goes through Boox's converter.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use regex::Regex;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

#[derive(Debug, Clone, Default)]
pub struct Meta {
    pub title: String,
    pub author: String,
}

fn opf_path(archive: &mut ZipArchive<File>) -> Result<String> {
    let mut xml = String::new();
    archive
        .by_name("META-INF/container.xml")
        .context("not an EPUB: no META-INF/container.xml")?
        .read_to_string(&mut xml)?;
    let re = Regex::new(r#"full-path\s*=\s*"([^"]+)""#).unwrap();
    re.captures(&xml)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_owned())
        .ok_or_else(|| anyhow!("container.xml has no rootfile full-path"))
}

fn read_entry(archive: &mut ZipArchive<File>, name: &str) -> Result<String> {
    let mut s = String::new();
    archive
        .by_name(name)
        .with_context(|| format!("EPUB has no {name}"))?
        .read_to_string(&mut s)?;
    Ok(s)
}

fn tag_text(xml: &str, tag: &str) -> Option<String> {
    let re = Regex::new(&format!(r"(?s)<{tag}\b[^>]*>(.*?)</{tag}>")).unwrap();
    re.captures(xml)
        .and_then(|c| c.get(1))
        .map(|m| unescape(m.as_str().trim()))
}

pub fn read_meta(path: &Path) -> Result<Meta> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut archive = ZipArchive::new(file)?;
    let opf = opf_path(&mut archive)?;
    let xml = read_entry(&mut archive, &opf)?;
    Ok(Meta {
        title: tag_text(&xml, "dc:title").unwrap_or_default(),
        author: tag_text(&xml, "dc:creator").unwrap_or_default(),
    })
}

/// Write a copy of `src` into `out_dir` (same file name) with its first
/// `<dc:title>` replaced by `title`. Entries are copied raw, so the result is
/// byte-for-byte deterministic for a given (source, title) pair.
pub fn with_title(src: &Path, title: &str, out_dir: &Path) -> Result<PathBuf> {
    let file = File::open(src).with_context(|| format!("open {}", src.display()))?;
    let mut archive = ZipArchive::new(file)?;
    let opf = opf_path(&mut archive)?;
    let xml = read_entry(&mut archive, &opf)?;
    let re = Regex::new(r"(?s)(<dc:title\b[^>]*>)(.*?)(</dc:title>)").unwrap();
    if !re.is_match(&xml) {
        return Err(anyhow!("{} has no <dc:title> to rewrite", src.display()));
    }
    let escaped = escape(title);
    let patched = re.replacen(&xml, 1, |c: &regex::Captures| {
        format!("{}{}{}", &c[1], escaped, &c[3])
    });

    fs::create_dir_all(out_dir)?;
    let dest = out_dir.join(src.file_name().unwrap());
    let mut writer = ZipWriter::new(File::create(&dest)?);
    let fixed = zip::DateTime::default();
    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        let name = entry.name().to_owned();
        if name == opf {
            let opts = SimpleFileOptions::default()
                .compression_method(CompressionMethod::Deflated)
                .last_modified_time(fixed);
            writer.start_file(&name, opts)?;
            writer.write_all(patched.as_bytes())?;
        } else {
            writer.raw_copy_file(entry)?;
        }
    }
    writer.finish()?;
    Ok(dest)
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_epub(dir: &Path, title: &str) -> PathBuf {
        let p = dir.join("t.epub");
        let mut w = ZipWriter::new(File::create(&p).unwrap());
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        w.start_file("mimetype", stored).unwrap();
        w.write_all(b"application/epub+zip").unwrap();
        let def = SimpleFileOptions::default();
        w.start_file("META-INF/container.xml", def).unwrap();
        w.write_all(br#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#).unwrap();
        w.start_file("OEBPS/content.opf", def).unwrap();
        w.write_all(format!(r#"<package><metadata><dc:title id="t">{title}</dc:title><dc:creator>Someone &amp; Co</dc:creator></metadata></package>"#).as_bytes()).unwrap();
        w.finish().unwrap();
        p
    }

    #[test]
    fn reads_and_rewrites_title() {
        let dir = std::env::temp_dir().join(format!("picco-epub-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let src = make_epub(&dir, "Original");
        let m = read_meta(&src).unwrap();
        assert_eq!(m.title, "Original");
        assert_eq!(m.author, "Someone & Co");
        let out = with_title(&src, "1 <New> & Co", &dir.join("out")).unwrap();
        let m2 = read_meta(&out).unwrap();
        assert_eq!(m2.title, "1 <New> & Co");
        assert_eq!(m2.author, "Someone & Co");
        // deterministic output
        let a = fs::read(&out).unwrap();
        let out2 = with_title(&src, "1 <New> & Co", &dir.join("out2")).unwrap();
        assert_eq!(a, fs::read(out2).unwrap());
        fs::remove_dir_all(&dir).unwrap();
    }
}
