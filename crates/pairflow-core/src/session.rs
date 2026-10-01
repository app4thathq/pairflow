//! TCP session: cleartext handshake, then encrypted frames.
//!
//! The pairing code is never sent. Both sides do X25519, derive keys with
//! HKDF using the code as salt, and exchange HMACs over the transcript.

use crate::crypto::{self, EphemeralKey};
use pairflow_proto::{
    decode_handshake, decode_secure, encode_handshake, encode_secure, take_frame, CodecError,
    HandshakeMsg, SecureMsg, ENCRYPTED_FRAME, MAX_FRAME, PROTOCOL_VERSION, REJECT_MAC, REJECT_RATE,
    REJECT_VERSION,
};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("protocol: {0}")]
    Codec(#[from] CodecError),
    #[error("protocol version mismatch")]
    Version,
    #[error("authentication failed")]
    Auth,
    #[error("too many failed attempts")]
    RateLimited,
    #[error("peer closed the connection")]
    Closed,
    #[error("decryption failed")]
    Crypto,
}

struct FailWindow {
    start: Instant,
    count: u32,
}

fn limiter() -> std::sync::MutexGuard<'static, HashMap<IpAddr, FailWindow>> {
    static FAILS: LazyLock<Mutex<HashMap<IpAddr, FailWindow>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    FAILS.lock().unwrap()
}

fn rate_limited(ip: IpAddr) -> bool {
    let mut map = limiter();
    let now = Instant::now();
    let entry = map.entry(ip).or_insert(FailWindow {
        start: now,
        count: 0,
    });
    if now.duration_since(entry.start) > Duration::from_secs(60) {
        entry.start = now;
        entry.count = 0;
    }
    entry.count >= 8
}

fn note_failure(ip: IpAddr) {
    let mut map = limiter();
    let now = Instant::now();
    let entry = map.entry(ip).or_insert(FailWindow {
        start: now,
        count: 0,
    });
    if now.duration_since(entry.start) > Duration::from_secs(60) {
        entry.start = now;
        entry.count = 0;
    }
    entry.count = entry.count.saturating_add(1);
}

pub struct Session {
    reader: TcpStream,
    writer: TcpStream,
    send_key: [u8; 32],
    recv_key: [u8; 32],
    send_n: u64,
    recv_n: u64,
    pub peer_name: String,
    pub peer_host_id: [u8; 16],
    read_buf: Vec<u8>,
}

impl Session {
    pub fn send(&mut self, msg: &SecureMsg) -> Result<(), SessionError> {
        self.send_n = self.send_n.checked_add(1).ok_or(SessionError::Crypto)?;
        let plain = encode_secure(msg);
        let ct =
            crypto::seal(&self.send_key, self.send_n, &plain).map_err(|_| SessionError::Crypto)?;
        let mut body = Vec::with_capacity(1 + 8 + ct.len());
        body.push(ENCRYPTED_FRAME);
        body.extend_from_slice(&self.send_n.to_le_bytes());
        body.extend_from_slice(&ct);
        write_frame(&mut self.writer, &body)?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn recv_timeout(&mut self, timeout: Duration) -> Result<Option<SecureMsg>, SessionError> {
        self.reader.set_read_timeout(Some(timeout))?;
        match self.recv_one() {
            Err(SessionError::Io(err)) if is_timeout(&err) => Ok(None),
            other => other.map(Some),
        }
    }

    fn recv_one(&mut self) -> Result<SecureMsg, SessionError> {
        let body = read_body(&mut self.reader, &mut self.read_buf)?;
        if body.first().copied() != Some(ENCRYPTED_FRAME) || body.len() < 1 + 8 + 16 {
            return Err(SessionError::Codec(CodecError::Malformed));
        }
        let mut nbuf = [0u8; 8];
        nbuf.copy_from_slice(&body[1..9]);
        let counter = u64::from_le_bytes(nbuf);
        if counter != self.recv_n + 1 {
            return Err(SessionError::Crypto);
        }
        let plain =
            crypto::open(&self.recv_key, counter, &body[9..]).map_err(|_| SessionError::Crypto)?;
        self.recv_n = counter;
        Ok(decode_secure(&plain)?)
    }
}

pub struct HostListener {
    pub listener: TcpListener,
    code: String,
    host_id: [u8; 16],
    screen: (u16, u16),
    name: String,
}

impl HostListener {
    pub fn bind(
        addr: SocketAddr,
        code: String,
        host_id: [u8; 16],
        screen: (u16, u16),
        name: String,
    ) -> Result<Self, SessionError> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            code,
            host_id,
            screen,
            name,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, SessionError> {
        Ok(self.listener.local_addr()?)
    }

    pub fn accept_authenticated(
        &self,
        running: &AtomicBool,
    ) -> Result<Option<(Session, SocketAddr)>, SessionError> {
        loop {
            if !running.load(Ordering::Relaxed) {
                return Ok(None);
            }
            match self.listener.accept() {
                Ok((sock, addr)) => {
                    sock.set_nonblocking(false)?;
                    configure(&sock)?;
                    match handshake_server(sock, &self.code, self.host_id, self.screen, &self.name)
                    {
                        Ok(session) => return Ok(Some((session, addr))),
                        Err(SessionError::Auth)
                        | Err(SessionError::RateLimited)
                        | Err(SessionError::Version) => {
                            eprintln!("pairflow: rejected {addr}");
                        }
                        Err(err) => eprintln!("pairflow: handshake with {addr} failed: {err}"),
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(err) => return Err(err.into()),
            }
        }
    }
}

pub fn connect_authenticated(
    addr: SocketAddr,
    code: &str,
    name: &str,
    timeout: Duration,
) -> Result<Session, SessionError> {
    let sock = if timeout.is_zero() {
        TcpStream::connect(addr)?
    } else {
        TcpStream::connect_timeout(&addr, timeout)?
    };
    configure(&sock)?;
    handshake_client(sock, code, name)
}

fn configure(sock: &TcpStream) -> Result<(), SessionError> {
    sock.set_nodelay(true)?;
    sock.set_read_timeout(Some(Duration::from_secs(8)))?;
    Ok(())
}

fn handshake_server(
    mut sock: TcpStream,
    code: &str,
    host_id: [u8; 16],
    screen: (u16, u16),
    name: &str,
) -> Result<Session, SessionError> {
    let ip = sock.peer_addr().ok().map(|a| a.ip());
    if ip.map(rate_limited).unwrap_or(false) {
        thread::sleep(Duration::from_millis(300));
        let _ = write_plain(
            &mut sock,
            &HandshakeMsg::AuthReject {
                reason: REJECT_RATE,
            },
        );
        return Err(SessionError::RateLimited);
    }

    let mut buf = Vec::new();
    let hello = read_handshake(&mut sock, &mut buf)?;
    let HandshakeMsg::ClientHello {
        version,
        pubkey: client_pub,
        name: peer_name,
    } = hello
    else {
        return Err(SessionError::Codec(CodecError::Malformed));
    };
    if version != PROTOCOL_VERSION {
        let _ = write_plain(
            &mut sock,
            &HandshakeMsg::AuthReject {
                reason: REJECT_VERSION,
            },
        );
        return Err(SessionError::Version);
    }

    let eph = EphemeralKey::generate();
    let server_pub = eph.public;
    write_plain(
        &mut sock,
        &HandshakeMsg::ServerHello {
            version: PROTOCOL_VERSION,
            pubkey: server_pub,
            host_id,
            screen_w: screen.0,
            screen_h: screen.1,
            name: name.to_string(),
        },
    )?;

    let shared = eph.diffie_hellman(&client_pub);
    let transcript = crypto::transcript(&client_pub, &server_pub, &host_id);
    let keys = crypto::derive_keys(&shared, code, &transcript, true);

    let auth = read_handshake(&mut sock, &mut buf)?;
    let HandshakeMsg::ClientAuth { mac } = auth else {
        return Err(SessionError::Codec(CodecError::Malformed));
    };
    if !crypto::mac_ok(&keys.client_mac, &mac) {
        if let Some(ip) = ip {
            note_failure(ip);
        }
        thread::sleep(Duration::from_millis(200));
        let _ = write_plain(&mut sock, &HandshakeMsg::AuthReject { reason: REJECT_MAC });
        return Err(SessionError::Auth);
    }
    write_plain(
        &mut sock,
        &HandshakeMsg::ServerAuth {
            mac: keys.server_mac,
        },
    )?;

    Ok(finish(sock, keys.send, keys.recv, peer_name, [0u8; 16]))
}

fn handshake_client(mut sock: TcpStream, code: &str, name: &str) -> Result<Session, SessionError> {
    let eph = EphemeralKey::generate();
    let client_pub = eph.public;
    write_plain(
        &mut sock,
        &HandshakeMsg::ClientHello {
            version: PROTOCOL_VERSION,
            pubkey: client_pub,
            name: name.to_string(),
        },
    )?;
    let mut buf = Vec::new();
    let hello = read_handshake(&mut sock, &mut buf)?;
    let HandshakeMsg::ServerHello {
        version,
        pubkey: server_pub,
        host_id,
        name: peer_name,
        ..
    } = hello
    else {
        if let HandshakeMsg::AuthReject { reason } = hello {
            if reason == REJECT_RATE {
                return Err(SessionError::RateLimited);
            }
            if reason == REJECT_VERSION {
                return Err(SessionError::Version);
            }
        }
        return Err(SessionError::Codec(CodecError::Malformed));
    };
    if version != PROTOCOL_VERSION {
        return Err(SessionError::Version);
    }
    let shared = eph.diffie_hellman(&server_pub);
    let transcript = crypto::transcript(&client_pub, &server_pub, &host_id);
    let keys = crypto::derive_keys(&shared, code, &transcript, false);
    write_plain(
        &mut sock,
        &HandshakeMsg::ClientAuth {
            mac: keys.client_mac,
        },
    )?;
    let auth = read_handshake(&mut sock, &mut buf)?;
    match auth {
        HandshakeMsg::ServerAuth { mac } => {
            if !crypto::mac_ok(&keys.server_mac, &mac) {
                return Err(SessionError::Auth);
            }
        }
        HandshakeMsg::AuthReject { .. } => return Err(SessionError::Auth),
        _ => return Err(SessionError::Codec(CodecError::Malformed)),
    }
    Ok(finish(sock, keys.send, keys.recv, peer_name, host_id))
}

fn finish(
    sock: TcpStream,
    send_key: [u8; 32],
    recv_key: [u8; 32],
    peer_name: String,
    peer_host_id: [u8; 16],
) -> Session {
    let writer = sock.try_clone().expect("clone tcp stream");
    Session {
        reader: sock,
        writer,
        send_key,
        recv_key,
        send_n: 0,
        recv_n: 0,
        peer_name,
        peer_host_id,
        read_buf: Vec::new(),
    }
}

fn write_plain(sock: &mut TcpStream, msg: &HandshakeMsg) -> Result<(), SessionError> {
    sock.write_all(&encode_handshake(msg))?;
    sock.flush()?;
    Ok(())
}

fn read_handshake(sock: &mut TcpStream, buf: &mut Vec<u8>) -> Result<HandshakeMsg, SessionError> {
    let body = read_body(sock, buf)?;
    Ok(decode_handshake(&body)?)
}

fn write_frame(sock: &mut TcpStream, body: &[u8]) -> Result<(), SessionError> {
    if body.len() > MAX_FRAME {
        return Err(SessionError::Codec(CodecError::TooLarge));
    }
    sock.write_all(&(body.len() as u32).to_le_bytes())?;
    sock.write_all(body)?;
    Ok(())
}

fn read_body(sock: &mut TcpStream, buf: &mut Vec<u8>) -> Result<Vec<u8>, SessionError> {
    loop {
        if let Some((body, n)) = take_frame(buf)? {
            let owned = body.to_vec();
            buf.drain(..n);
            return Ok(owned);
        }
        let mut tmp = [0u8; 2048];
        let n = sock.read(&mut tmp)?;
        if n == 0 {
            return Err(SessionError::Closed);
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > MAX_FRAME + 4 {
            return Err(SessionError::Codec(CodecError::TooLarge));
        }
    }
}

fn is_timeout(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn loopback_auth_and_mouse() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let code = "K7NQ2".to_string();
        let host_id = [4u8; 16];
        let running = Arc::new(AtomicBool::new(true));
        let host = HostListener {
            listener,
            code: code.clone(),
            host_id,
            screen: (1920, 1080),
            name: "host".into(),
        };
        let flag = running.clone();
        let server = thread::spawn(move || {
            let (mut session, _) = host.accept_authenticated(&flag).unwrap().unwrap();
            let msg = loop {
                if let Some(msg) = session.recv_timeout(Duration::from_secs(2)).unwrap() {
                    break msg;
                }
            };
            assert_eq!(msg, SecureMsg::MouseMove { dx: 4, dy: -2 });
            session.send(&SecureMsg::Bye).unwrap();
        });
        let mut client =
            connect_authenticated(addr, &code, "client", Duration::from_secs(2)).unwrap();
        assert_eq!(client.peer_host_id, host_id);
        assert_eq!(client.peer_name, "host");
        client
            .send(&SecureMsg::MouseMove { dx: 4, dy: -2 })
            .unwrap();
        let bye = loop {
            if let Some(msg) = client.recv_timeout(Duration::from_secs(2)).unwrap() {
                break msg;
            }
        };
        assert_eq!(bye, SecureMsg::Bye);
        server.join().unwrap();
    }

    #[test]
    fn wrong_code_is_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let host = HostListener {
            listener,
            code: "K7NQ2".into(),
            host_id: [1u8; 16],
            screen: (800, 600),
            name: "host".into(),
        };
        let running = Arc::new(AtomicBool::new(true));
        let flag = running.clone();
        let server = thread::spawn(move || {
            let _ = host.accept_authenticated(&flag);
        });
        let err = connect_authenticated(addr, "ZZZZZ", "client", Duration::from_secs(2));
        assert!(matches!(err, Err(SessionError::Auth)));
        running.store(false, Ordering::Relaxed);
        server.join().unwrap();
    }
}
