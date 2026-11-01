//! HTTP client for the BOOX Picco's transfer-mode web server.
//!
//! The Picco (Boox's CrossPoint fork on an ESP32-S3) runs a tiny ESP-IDF
//! httpd on port 80 while its home-screen "Transfer" mode is active. The
//! protocol was lifted from the booxtiles.com web app:
//!
//! - `HEAD /ping`                       -> 200 when the device is listening
//! - `POST /upload?fileType=<0|1|2>`    -> multipart/form-data, one part named
//!   `file`; 0 = book, 1 = image, 2 = font. CORS is wide open.
//!
//! Everything else (`GET /`, `/api/*`) is a 404.

use std::fmt;
use std::fs::File;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::blocking::multipart::{Form, Part};
use reqwest::blocking::Client;

pub const PORT: u16 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum FileType {
    Book,
    Image,
    Font,
}

impl FileType {
    /// Value of the `fileType` query parameter.
    pub fn code(self) -> u8 {
        match self {
            FileType::Book => 0,
            FileType::Image => 1,
            FileType::Font => 2,
        }
    }

    /// Guess the type from the file extension. Mirrors what the web app
    /// accepts; unknown extensions are not uploaded without `--type`.
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "epub" | "opub" | "mobi" | "azw" | "azw3" | "azw5" | "fb2" | "fb3" | "fbz" | "txt"
            | "pdb" | "prc" | "rtf" | "html" | "xhtml" | "htm" | "chm" => FileType::Book,
            "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif" | "opg" => FileType::Image,
            "ttf" | "otf" | "ttc" => FileType::Font,
            _ => return None,
        })
    }
}

impl fmt::Display for FileType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FileType::Book => "book",
            FileType::Image => "image",
            FileType::Font => "font",
        })
    }
}

pub struct Device {
    base: String,
    client: Client,
}

impl Device {
    pub fn new(host: &str) -> Result<Self> {
        let host = host.trim().trim_start_matches("http://").trim_end_matches('/');
        let base = if host.contains(':') {
            format!("http://{host}")
        } else {
            format!("http://{host}:{PORT}")
        };
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .build()?;
        Ok(Self { base, client })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// `HEAD /ping`. Ok(()) when the device answers 2xx.
    pub fn ping(&self, timeout: Duration) -> Result<()> {
        let resp = self
            .client
            .head(format!("{}/ping", self.base))
            .timeout(timeout)
            .send()
            .with_context(|| format!("{} did not answer", self.base))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            bail!("{} answered /ping with HTTP {}", self.base, resp.status())
        }
    }

    /// Upload one file under `name` (defaults to the file's own name).
    /// `dir`, if given, is sent as the `path` query parameter; stock
    /// CrossPoint honours it as the destination folder, the Boox fork may not.
    /// Returns the device's response body on success.
    pub fn upload(
        &self,
        path: &Path,
        name: Option<&str>,
        file_type: FileType,
        dir: Option<&str>,
        quiet: bool,
    ) -> Result<String> {
        let name = match name {
            Some(n) => n.to_owned(),
            None => path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| anyhow!("{} has no usable file name", path.display()))?
                .to_owned(),
        };
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let len = file.metadata()?.len();

        let bar = if quiet {
            ProgressBar::hidden()
        } else {
            let bar = ProgressBar::new(len);
            bar.set_style(
                ProgressStyle::with_template(
                    "{msg:.bold} [{bar:30}] {bytes}/{total_bytes} {bytes_per_sec} {eta}",
                )?
                .progress_chars("=> "),
            );
            bar.set_message(name.clone());
            bar
        };
        let reader = bar.wrap_read(file);

        let part = Part::reader_with_length(reader, len)
            .file_name(name.clone())
            .mime_str("application/octet-stream")?;
        let form = Form::new().part("file", part);

        let mut query = vec![("fileType", file_type.code().to_string())];
        if let Some(d) = dir {
            query.push(("path", d.to_owned()));
        }
        let resp = self
            .client
            .post(format!("{}/upload", self.base))
            .query(&query)
            .multipart(form)
            // The Picco has been seen taking 8 KB/s on a bad Wi-Fi day.
            // Allow 4 KB/s plus slack rather than a fixed cap.
            .timeout(Duration::from_secs(10 * 60 + len / 4096))
            .send()
            .with_context(|| format!("upload {name} to {}", self.base))?;
        bar.finish_and_clear();

        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        if status.is_success() {
            Ok(body.trim().to_owned())
        } else {
            bail!("device rejected {name}: HTTP {status} {}", body.trim())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_type_from_extension() {
        assert_eq!(FileType::from_path(Path::new("a/B.EPUB")), Some(FileType::Book));
        assert_eq!(FileType::from_path(Path::new("x.opub")), Some(FileType::Book));
        assert_eq!(FileType::from_path(Path::new("cover.JPG")), Some(FileType::Image));
        assert_eq!(FileType::from_path(Path::new("font.ttf")), Some(FileType::Font));
        assert_eq!(FileType::from_path(Path::new("doc.pdf")), None);
        assert_eq!(FileType::from_path(Path::new("noext")), None);
    }

    #[test]
    fn codes_match_booxtiles() {
        assert_eq!(FileType::Book.code(), 0);
        assert_eq!(FileType::Image.code(), 1);
        assert_eq!(FileType::Font.code(), 2);
    }

    #[test]
    fn base_url_adds_default_port() {
        assert_eq!(Device::new("192.168.1.29").unwrap().base(), "http://192.168.1.29:80");
        assert_eq!(Device::new("http://picco.local:8080/").unwrap().base(), "http://picco.local:8080");
    }
}
