//! `shelf.toml`: the reading order and display titles for a folder of books.
//!
//! ```toml
//! # Top of the list lands at the top of the Picco's Bookshelf.
//! [[book]]
//! file = "1_The_Alloy_of_Law.epub"
//! title = "1 Alloy of Law"
//! ```
//! The Bookshelf is newest-first, so `sync` pushes the list bottom-up.
//! Books in the folder but not in the manifest are pushed first (they end up
//! below the curated ones) with their original titles.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const FILE: &str = "shelf.toml";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Shelf {
    #[serde(default, rename = "book")]
    pub books: Vec<Book>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Book {
    pub file: String,
    /// Display title on the Picco; omitted = keep the EPUB's own title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Free-form note for humans (author, why it is here). Ignored by the CLI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Shelf {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join(FILE)
    }

    pub fn load(dir: &Path) -> Result<Option<Self>> {
        let p = Self::path(dir);
        match fs::read_to_string(&p) {
            Ok(s) => Ok(Some(
                toml::from_str(&s).with_context(|| format!("parse {}", p.display()))?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("read {}", p.display())),
        }
    }

    pub fn save(&self, dir: &Path) -> Result<PathBuf> {
        let p = Self::path(dir);
        let header = "# picco shelf: top of this list = top of the Picco's Bookshelf.\n\
                      # `title` is what the shelf shows (it truncates after ~10 chars,\n\
                      # so lead with the series number). Omit `title` to keep the book's own.\n\n";
        fs::write(&p, format!("{header}{}", toml::to_string_pretty(self)?))?;
        Ok(p)
    }

    /// Resolved push plan for `dir`: (source path, display title) in the
    /// order they should be uploaded (unlisted first, then manifest bottom-up).
    pub fn plan(&self, dir: &Path, all_books: &[PathBuf]) -> Vec<(PathBuf, Option<String>)> {
        let listed: Vec<PathBuf> = self.books.iter().map(|b| dir.join(&b.file)).collect();
        let mut out: Vec<(PathBuf, Option<String>)> = all_books
            .iter()
            .filter(|p| !listed.contains(p))
            .map(|p| (p.clone(), None))
            .collect();
        for b in self.books.iter().rev() {
            let p = dir.join(&b.file);
            if p.is_file() {
                out.push((p, b.title.clone()));
            } else {
                eprintln!("shelf.toml lists missing file {}", p.display());
            }
        }
        out
    }
}
