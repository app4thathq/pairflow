//! Local discovery.
//!
//! Two mechanisms, both of which advertise a host id and a TCP port and never
//! the pairing code:
//!
//! * mDNS service `_pairflow._tcp.local.` (works across many home networks,
//!   including some Wi-Fi setups that drop broadcasts).
//! * UDP announcements to the subnet broadcast address on port 24817.
//!
//! A client collects candidates for a few seconds and tries the encrypted
//! handshake against each one. Only the host that knows the code will accept.

use crate::state::{hex_encode, parse_hex16};
use if_addrs::IfAddr;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use pairflow_proto::SERVICE_TYPE;
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const MAGIC: &[u8; 4] = b"PFD1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub host_id: [u8; 16],
    pub addr: SocketAddr,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announce {
    pub host_id: [u8; 16],
    pub port: u16,
    pub name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoverError {
    #[error("discovery packet: {0}")]
    Packet(&'static str),
}

pub fn encode_announce(msg: &Announce) -> Vec<u8> {
    let name = msg.name.as_bytes();
    let n = name.len().min(48);
    let mut out = Vec::with_capacity(4 + 1 + 16 + 2 + 1 + n);
    out.extend_from_slice(MAGIC);
    out.push(1); // announce
    out.extend_from_slice(&msg.host_id);
    out.extend_from_slice(&msg.port.to_le_bytes());
    out.push(n as u8);
    out.extend_from_slice(&name[..n]);
    out
}

pub fn decode_announce(buf: &[u8]) -> Result<Announce, DiscoverError> {
    if buf.len() < 4 + 1 + 16 + 2 + 1 || &buf[0..4] != MAGIC {
        return Err(DiscoverError::Packet("bad header"));
    }
    if buf[4] != 1 {
        return Err(DiscoverError::Packet("not an announce"));
    }
    let mut host_id = [0u8; 16];
    host_id.copy_from_slice(&buf[5..21]);
    let port = u16::from_le_bytes([buf[21], buf[22]]);
    let n = buf[23] as usize;
    if buf.len() < 24 + n {
        return Err(DiscoverError::Packet("truncated name"));
    }
    let name = String::from_utf8_lossy(&buf[24..24 + n]).to_string();
    Ok(Announce {
        host_id,
        port,
        name,
    })
}

pub struct Advertiser {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Advertiser {
    pub fn start(announce: Announce, udp_port: u16, enable_mdns: bool, enable_udp: bool) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();
        if enable_udp {
            let stop_udp = stop.clone();
            let ann = announce.clone();
            threads.push(thread::spawn(move || {
                udp_advertise(ann, udp_port, stop_udp)
            }));
        }
        if enable_mdns {
            let stop_mdns = stop.clone();
            threads.push(thread::spawn(move || mdns_advertise(announce, stop_mdns)));
        }
        Self { stop, threads }
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn udp_advertise(announce: Announce, udp_port: u16, stop: Arc<AtomicBool>) {
    let socket = match udp_socket(0) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("pairflow: UDP announce socket: {err}");
            return;
        }
    };
    let _ = socket.set_broadcast(true);
    let packet = encode_announce(&announce);
    while !stop.load(Ordering::Relaxed) {
        let _ = socket.send_to(&packet, SocketAddrV4::new(Ipv4Addr::BROADCAST, udp_port));
        if let Ok(list) = if_addrs::get_if_addrs() {
            for iface in list {
                if let IfAddr::V4(v4) = iface.addr {
                    if v4.ip.is_loopback() {
                        continue;
                    }
                    if let Some(bcast) = v4.broadcast {
                        let dest = SocketAddrV4::new(bcast, udp_port);
                        let _ = socket.send_to(&packet, dest);
                    }
                }
            }
        }
        for _ in 0..10 {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

fn mdns_advertise(announce: Announce, stop: Arc<AtomicBool>) {
    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(err) => {
            eprintln!("pairflow: mDNS unavailable ({err}); UDP discovery still runs if enabled");
            return;
        }
    };
    let instance = format!("pairflow-{}", &hex_encode(&announce.host_id)[..8]);
    let host = format!("{instance}.local.");
    let props = [("id", hex_encode(&announce.host_id)), ("v", "1".into())];
    match ServiceInfo::new(
        SERVICE_TYPE,
        &instance,
        &host,
        "",
        announce.port,
        &props[..],
    ) {
        Ok(info) => {
            let info = info.enable_addr_auto();
            if let Err(err) = daemon.register(info) {
                eprintln!("pairflow: mDNS register failed: {err}");
            }
        }
        Err(err) => eprintln!("pairflow: mDNS service info: {err}"),
    }
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(200));
    }
    let _ = daemon.shutdown();
}

/// Collect host candidates until `wait` elapses. Matching `prefer` ids are
/// ordered first.
pub fn browse(
    wait: Duration,
    udp_port: u16,
    enable_mdns: bool,
    enable_udp: bool,
    prefer: Option<[u8; 16]>,
) -> Vec<Candidate> {
    let found = Arc::new(Mutex::new(HashMap::<String, Candidate>::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    if enable_udp {
        let found_udp = found.clone();
        let stop_udp = stop.clone();
        threads.push(thread::spawn(move || {
            udp_listen(found_udp, udp_port, stop_udp)
        }));
    }
    if enable_mdns {
        let found_mdns = found.clone();
        let stop_mdns = stop.clone();
        threads.push(thread::spawn(move || mdns_browse(found_mdns, stop_mdns)));
    }
    let start = Instant::now();
    while start.elapsed() < wait {
        thread::sleep(Duration::from_millis(50));
    }
    stop.store(true, Ordering::Relaxed);
    for t in threads {
        let _ = t.join();
    }
    let map = found.lock().unwrap();
    let mut list: Vec<Candidate> = map.values().cloned().collect();
    list.sort_by_key(|c| {
        let preferred = prefer == Some(c.host_id);
        (!preferred, c.name.clone())
    });
    list
}

fn udp_listen(found: Arc<Mutex<HashMap<String, Candidate>>>, udp_port: u16, stop: Arc<AtomicBool>) {
    let socket = match udp_socket(udp_port) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("pairflow: UDP discovery listen on {udp_port}: {err}");
            return;
        }
    };
    let _ = socket.set_read_timeout(Some(Duration::from_millis(200)));
    let mut buf = [0u8; 512];
    while !stop.load(Ordering::Relaxed) {
        match socket.recv_from(&mut buf) {
            Ok((n, src)) => {
                if let Ok(ann) = decode_announce(&buf[..n]) {
                    let addr = SocketAddr::new(src.ip(), ann.port);
                    let key = format!("{}:{addr}", hex_encode(&ann.host_id));
                    let mut guard = found.lock().unwrap();
                    guard.insert(
                        key,
                        Candidate {
                            host_id: ann.host_id,
                            addr,
                            name: ann.name,
                        },
                    );
                }
            }
            Err(err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
}

fn mdns_browse(found: Arc<Mutex<HashMap<String, Candidate>>>, stop: Arc<AtomicBool>) {
    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(err) => {
            eprintln!("pairflow: mDNS browse unavailable: {err}");
            return;
        }
    };
    let receiver = match daemon.browse(SERVICE_TYPE) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("pairflow: mDNS browse: {err}");
            return;
        }
    };
    while !stop.load(Ordering::Relaxed) {
        match receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let Some(id_txt) = info.get_property_val_str("id") else {
                    continue;
                };
                let Ok(host_id) = parse_hex16(id_txt) else {
                    continue;
                };
                let port = info.get_port();
                let name = info.get_fullname().to_string();
                for ip in info.get_addresses() {
                    if ip.is_unspecified() {
                        continue;
                    }
                    let addr = SocketAddr::new(*ip, port);
                    let key = format!("{}:{addr}", hex_encode(&host_id));
                    let mut guard = found.lock().unwrap();
                    guard.insert(
                        key,
                        Candidate {
                            host_id,
                            addr,
                            name: name.clone(),
                        },
                    );
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    let _ = daemon.shutdown();
}

fn udp_socket(port: u16) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(false)?;
    let addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port);
    socket.bind(&addr.into())?;
    Ok(socket.into())
}

pub fn machine_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "pairflow".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_roundtrip() {
        let msg = Announce {
            host_id: [9u8; 16],
            port: 24816,
            name: "desk".into(),
        };
        let bytes = encode_announce(&msg);
        assert_eq!(decode_announce(&bytes).unwrap(), msg);
    }

    #[test]
    fn ignores_garbage() {
        assert!(decode_announce(b"nope").is_err());
    }
}
