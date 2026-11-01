//! Finding the Picco on the LAN.
//!
//! Two strategies, tried in order:
//! 1. mDNS browse for `_nanoreader._tcp` (what the BOOX Tiles phone app
//!    does; "NanoReader" is Boox's internal name for the Picco firmware).
//! 2. Sweep the local /24: TCP-connect to port 80, then `HEAD /ping`.
//!    The device's httpd is the only thing on a home LAN that answers that.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent};

use crate::device::{Device, PORT};

pub const MDNS_SERVICE: &str = "_nanoreader._tcp.local.";

pub fn mdns(timeout: Duration) -> Result<Vec<IpAddr>> {
    let daemon = ServiceDaemon::new().context("start mDNS daemon")?;
    let rx = daemon.browse(MDNS_SERVICE).context("browse mDNS")?;
    let deadline = Instant::now() + timeout;
    let mut found = BTreeSet::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                for addr in info.get_addresses() {
                    if addr.is_ipv4() {
                        found.insert(*addr);
                    }
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = daemon.shutdown();
    Ok(found.into_iter().collect())
}

/// Sweep `subnet` (a /24 given by any address in it) for hosts whose port 80
/// answers `HEAD /ping`.
pub fn sweep(subnet: Ipv4Addr, per_host: Duration) -> Result<Vec<IpAddr>> {
    let [a, b, c, _] = subnet.octets();
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();
    for d in 1..=254u8 {
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            let ip = Ipv4Addr::new(a, b, c, d);
            let sock = SocketAddr::new(IpAddr::V4(ip), PORT);
            if TcpStream::connect_timeout(&sock, per_host).is_err() {
                return;
            }
            if let Ok(dev) = Device::new(&ip.to_string()) {
                if dev.ping(Duration::from_secs(2)).is_ok() {
                    let _ = tx.send(IpAddr::V4(ip));
                }
            }
        }));
        // Cap concurrency so macOS does not run out of ephemeral sockets.
        if handles.len() >= 64 {
            for h in handles.drain(..) {
                let _ = h.join();
            }
        }
    }
    for h in handles {
        let _ = h.join();
    }
    drop(tx);
    let mut found: Vec<IpAddr> = rx.iter().collect();
    found.sort();
    Ok(found)
}

pub fn local_ipv4() -> Option<Ipv4Addr> {
    match local_ip_address::local_ip().ok()? {
        IpAddr::V4(v4) => Some(v4),
        IpAddr::V6(_) => None,
    }
}
