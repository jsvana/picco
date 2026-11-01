mod convert;
mod device;
mod discover;
mod epub;
mod shelf;
mod state;

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use device::{Device, FileType};
use state::{sha256_file, State};

/// Push books, images and fonts to a BOOX Picco over Wi-Fi.
///
/// Put the Picco in Transfer mode (home screen -> Transfer -> join your
/// Wi-Fi) first; its web server only runs while that screen is open.
#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Device IP or host[:port]. Falls back to $PICCO_HOST, then the saved
    /// default, then mDNS/LAN discovery.
    #[arg(long, global = true, env = "PICCO_HOST")]
    host: Option<String>,

    /// Destination folder on the card, sent as the upload's `path` parameter
    /// (e.g. /Mistborn). Experimental: the firmware may ignore it.
    #[arg(long, global = true)]
    path: Option<String>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Find Piccos in Transfer mode on the LAN and save the first as default.
    Discover {
        /// Any address in the /24 to sweep (default: this machine's subnet).
        #[arg(long)]
        subnet: Option<Ipv4Addr>,
        /// Seconds to wait for mDNS answers before sweeping.
        #[arg(long, default_value_t = 3)]
        mdns_secs: u64,
        /// Do not save the result as the default host.
        #[arg(long)]
        no_save: bool,
    },
    /// Check that the device is listening.
    Ping,
    /// Upload files. Type is inferred from the extension unless --type is given.
    Push {
        files: Vec<PathBuf>,
        #[arg(long = "type", value_enum)]
        file_type: Option<FileType>,
        /// Display title on the Picco (books only; rewrites the EPUB title
        /// before conversion). Lead with the series number: "1 Alloy of Law".
        #[arg(long)]
        title: Option<String>,
        /// Upload even if this exact file was pushed before.
        #[arg(long)]
        force: bool,
        /// No progress bars.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Push every book in a directory that has not been pushed yet. Honours
    /// shelf.toml (see `picco shelf init`) for order and display titles.
    Sync {
        dir: PathBuf,
        /// Recurse into subdirectories.
        #[arg(short, long)]
        recursive: bool,
        /// Show what would be pushed and stop.
        #[arg(long)]
        dry_run: bool,
        #[arg(short, long)]
        quiet: bool,
    },
    /// Convert books to Boox's .opub without uploading (needs Google Chrome).
    Convert {
        files: Vec<PathBuf>,
        /// Write .opub files here instead of the cache.
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(short, long)]
        quiet: bool,
    },
    /// Manage a folder's shelf.toml (reading order + display titles).
    Shelf {
        #[command(subcommand)]
        cmd: ShelfCmd,
    },
    /// Show or set the saved default host.
    Config {
        /// New default host; omit to print the current one.
        set_host: Option<String>,
    },
    /// List files recorded as pushed.
    Pushed,
}

#[derive(Subcommand)]
enum ShelfCmd {
    /// Write a shelf.toml listing every book in the folder with its own
    /// title and author, for you to reorder and rename.
    Init {
        dir: PathBuf,
        /// Overwrite an existing shelf.toml.
        #[arg(long)]
        force: bool,
    },
    /// Print the push plan for a folder (what `sync` would do, in order).
    Show { dir: PathBuf },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut st = State::load()?;

    match cli.cmd {
        Cmd::Discover { subnet, mdns_secs, no_save } => {
            eprintln!("Browsing mDNS for {} ...", discover::MDNS_SERVICE);
            let mut found = discover::mdns(Duration::from_secs(mdns_secs))?;
            if found.is_empty() {
                let base = match subnet.or_else(discover::local_ipv4) {
                    Some(ip) => ip,
                    None => bail!("could not determine local subnet; pass --subnet"),
                };
                eprintln!("Nothing via mDNS; sweeping {}/24 for /ping ...", base);
                found = discover::sweep(base, Duration::from_millis(400))?;
            }
            if found.is_empty() {
                bail!(
                    "no Picco found. Is the device on the Transfer screen and on this Wi-Fi?"
                );
            }
            for ip in &found {
                println!("{ip}");
            }
            if !no_save {
                let host = found[0].to_string();
                st.host = Some(host.clone());
                st.save()?;
                eprintln!("Saved {host} as default host ({})", State::path()?.display());
            }
        }
        Cmd::Ping => {
            let dev = resolve_device(cli.host.as_deref(), &st)?;
            dev.ping(Duration::from_secs(5))?;
            println!("{} is listening", dev.base());
        }
        Cmd::Push { files, file_type, title, force, quiet } => {
            if title.is_some() && files.len() != 1 {
                bail!("--title applies to exactly one file");
            }
            if files.is_empty() {
                bail!("nothing to push");
            }
            let dev = resolve_device(cli.host.as_deref(), &st)?;
            dev.ping(Duration::from_secs(5))?;
            let mut failures = 0;
            for path in &files {
                let ft = match file_type.or_else(|| FileType::from_path(path)) {
                    Some(ft) => ft,
                    None => {
                        eprintln!("skip {}: unknown type, pass --type", path.display());
                        failures += 1;
                        continue;
                    }
                };
                if let Err(e) = push_one(&dev, &mut st, path, ft, title.as_deref(), cli.path.as_deref(), force, quiet) {
                    eprintln!("FAILED {}: {e:#}", path.display());
                    failures += 1;
                }
            }
            if failures > 0 {
                bail!("{failures} file(s) not pushed");
            }
        }
        Cmd::Sync { dir, recursive, dry_run, quiet } => {
            let dev = resolve_device(cli.host.as_deref(), &st)?;
            let mut books = Vec::new();
            collect_books(&dir, recursive, &mut books)?;
            books.sort();
            let plan = match shelf::Shelf::load(&dir)? {
                Some(sh) => sh.plan(&dir, &books),
                None => books.into_iter().rev().map(|p| (p, None)).collect(),
            };
            let mut todo = Vec::new();
            for (path, title) in plan {
                let (src, _) = titled_source(&path, title.as_deref(), quiet)?;
                let hash = sha256_file(&src)?;
                if st.pushed.contains_key(&hash) {
                    continue;
                }
                todo.push((path, title));
            }
            if todo.is_empty() {
                println!("everything in {} is already on the device", dir.display());
                return Ok(());
            }
            for (p, t) in &todo {
                println!(
                    "{}{}{}",
                    if dry_run { "would push " } else { "" },
                    p.display(),
                    t.as_ref().map(|t| format!("  as \"{t}\"")).unwrap_or_default()
                );
            }
            if dry_run {
                return Ok(());
            }
            dev.ping(Duration::from_secs(5))?;
            let mut failures = 0;
            for (path, title) in &todo {
                if let Err(e) = push_one(
                    &dev,
                    &mut st,
                    path,
                    FileType::Book,
                    title.as_deref(),
                    cli.path.as_deref(),
                    false,
                    quiet,
                ) {
                    eprintln!("FAILED {}: {e:#}", path.display());
                    failures += 1;
                }
            }
            if failures > 0 {
                bail!("{failures} file(s) not pushed");
            }
        }
        Cmd::Shelf { cmd } => match cmd {
            ShelfCmd::Init { dir, force } => {
                let target = shelf::Shelf::path(&dir);
                if target.exists() && !force {
                    bail!("{} exists; pass --force to overwrite", target.display());
                }
                let mut books = Vec::new();
                collect_books(&dir, false, &mut books)?;
                books.sort();
                let mut sh = shelf::Shelf::default();
                for b in books {
                    let meta = epub::read_meta(&b).unwrap_or_default();
                    sh.books.push(shelf::Book {
                        file: b.file_name().unwrap().to_string_lossy().into_owned(),
                        title: Some(meta.title),
                        note: if meta.author.is_empty() { None } else { Some(meta.author) },
                    });
                }
                let p = sh.save(&dir)?;
                println!("wrote {} ({} books); edit order and titles, then `picco sync`", p.display(), sh.books.len());
            }
            ShelfCmd::Show { dir } => {
                let mut books = Vec::new();
                collect_books(&dir, false, &mut books)?;
                books.sort();
                let sh = shelf::Shelf::load(&dir)?.unwrap_or_default();
                println!("push order (last = top of Bookshelf):");
                for (p, t) in sh.plan(&dir, &books) {
                    println!("  {}  {}", p.file_name().unwrap().to_string_lossy(), t.map(|t| format!("-> \"{t}\"")).unwrap_or_default());
                }
            }
        },
        Cmd::Convert { files, out, quiet } => {
            if files.is_empty() {
                bail!("nothing to convert");
            }
            let mut failures = 0;
            for f in &files {
                match convert::convert(f, out.as_deref(), quiet) {
                    Ok(p) => println!("{}", p.display()),
                    Err(e) => {
                        eprintln!("FAILED {}: {e:#}", f.display());
                        failures += 1;
                    }
                }
            }
            if failures > 0 {
                bail!("{failures} file(s) not converted");
            }
        }
        Cmd::Config { set_host } => match set_host {
            Some(h) => {
                st.host = Some(h.clone());
                st.save()?;
                println!("default host: {h}");
            }
            None => match &st.host {
                Some(h) => println!("default host: {h}"),
                None => println!("no default host; run `picco discover` or `picco config <ip>`"),
            },
        },
        Cmd::Pushed => {
            if st.pushed.is_empty() {
                println!("nothing recorded");
            }
            for (hash, p) in &st.pushed {
                println!("{}  {:<5} {}  {}  {}", &hash[..12], p.file_type, p.at, p.host, p.name);
            }
        }
    }
    Ok(())
}

fn resolve_device(flag: Option<&str>, st: &State) -> Result<Device> {
    if let Some(h) = flag {
        return Device::new(h);
    }
    if let Some(h) = &st.host {
        return Device::new(h);
    }
    eprintln!("No host configured; discovering ...");
    let found = discover::mdns(Duration::from_secs(3))?;
    let ip = match found.first() {
        Some(ip) => *ip,
        None => {
            let base = discover::local_ipv4().context("no local IPv4 address")?;
            *discover::sweep(base, Duration::from_millis(400))?
                .first()
                .context("no Picco found; is it on the Transfer screen?")?
        }
    };
    Device::new(&ip.to_string())
}

/// For books with a display title, the source becomes a title-patched copy
/// of the EPUB (cached by original hash + title). Returns (source, is_patched).
fn titled_source(path: &Path, title: Option<&str>, quiet: bool) -> Result<(PathBuf, bool)> {
    let Some(title) = title else {
        return Ok((path.to_path_buf(), false));
    };
    let key = sha256_file(path)?;
    let mut h = <sha2::Sha256 as sha2::Digest>::new();
    sha2::Digest::update(&mut h, key.as_bytes());
    sha2::Digest::update(&mut h, b"|");
    sha2::Digest::update(&mut h, title.as_bytes());
    let tag = format!("{:x}", sha2::Digest::finalize(h));
    let dir = dirs::cache_dir()
        .context("no cache dir")?
        .join("picco")
        .join("titled")
        .join(&tag[..12]);
    let dest = dir.join(path.file_name().unwrap());
    if !dest.is_file() {
        if !quiet {
            eprintln!("retitling {} -> \"{title}\"", path.display());
        }
        epub::with_title(path, title, &dir)?;
    }
    Ok((dest, true))
}

#[allow(clippy::too_many_arguments)]
fn push_one(
    dev: &Device,
    st: &mut State,
    path: &Path,
    ft: FileType,
    title: Option<&str>,
    dir: Option<&str>,
    force: bool,
    quiet: bool,
) -> Result<()> {
    if title.is_some() && ft != FileType::Book {
        bail!("--title only applies to books");
    }
    let (source, _) = titled_source(path, title, quiet)?;
    let path = source.as_path();
    let hash = sha256_file(path)?;
    if !force {
        if let Some(prev) = st.pushed.get(&hash) {
            println!(
                "skip {} (pushed {} as {})",
                path.display(),
                prev.at,
                prev.name
            );
            return Ok(());
        }
    }
    let is_opub = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("opub"))
        .unwrap_or(false);
    // Converted books live in the cache under a hash-prefixed name; upload
    // them under the clean name the device should show.
    let (upload_path, name) = if ft == FileType::Book && !is_opub {
        (convert::convert(path, None, quiet)?, convert::opub_name(path))
    } else {
        (
            path.to_path_buf(),
            path.file_name().unwrap().to_string_lossy().into_owned(),
        )
    };
    // Transient failures (timeouts, the device dropping the socket) are
    // common on a weak Wi-Fi link; re-ping and retry a few times.
    let mut attempt = 0;
    let body = loop {
        attempt += 1;
        match dev.upload(&upload_path, Some(&name), ft, dir, quiet) {
            Ok(b) => break b,
            Err(e) if attempt < 4 => {
                eprintln!("attempt {attempt} failed for {name}: {e:#}; retrying in 10s");
                std::thread::sleep(Duration::from_secs(10));
                let mut waited = 0;
                while dev.ping(Duration::from_secs(5)).is_err() && waited < 120 {
                    std::thread::sleep(Duration::from_secs(5));
                    waited += 5;
                }
            }
            Err(e) => return Err(e),
        }
    };
    println!(
        "pushed {name} ({ft}){}{}",
        title.map(|t| format!(" titled \"{t}\"")).unwrap_or_default(),
        if body.is_empty() { String::new() } else { format!(" -> {body}") }
    );
    st.pushed.insert(
        hash,
        state::Pushed {
            name,
            host: dev.base().to_owned(),
            file_type: ft.to_string(),
            at: state::now_rfc3339(),
        },
    );
    st.save()?;
    Ok(())
}

fn collect_books(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if recursive {
                collect_books(&path, true, out)?;
            }
        } else if FileType::from_path(&path) == Some(FileType::Book) {
            out.push(path);
        }
    }
    Ok(())
}
