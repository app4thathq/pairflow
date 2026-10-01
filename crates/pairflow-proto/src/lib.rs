//! Pairflow protocol types shared by every platform.
//!
//! Handshake messages are length-prefixed and unencrypted. After both sides
//! accept the pairing code, application messages are ChaCha20-Poly1305 frames
//! (see `pairflow-core`). This crate only defines the plaintext layout.

mod keys;

pub use keys::KeyId;

pub const PROTOCOL_VERSION: u16 = 1;
pub const DEFAULT_TCP_PORT: u16 = 24816;
pub const DEFAULT_UDP_PORT: u16 = 24817;
pub const SERVICE_TYPE: &str = "_pairflow._tcp.local.";
pub const MAX_FRAME: usize = 64 * 1024;
pub const MAX_NAME: usize = 48;

/// Which edge of a screen faces the other computer.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left = 0,
    Right = 1,
    Top = 2,
    Bottom = 3,
}

impl Side {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Left,
            1 => Self::Right,
            2 => Self::Top,
            3 => Self::Bottom,
            _ => return None,
        })
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "left" | "l" => Some(Self::Left),
            "right" | "r" => Some(Self::Right),
            "top" | "t" | "up" => Some(Self::Top),
            "bottom" | "b" | "down" => Some(Self::Bottom),
            _ => None,
        }
    }

    pub fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left = 1,
    Right = 2,
    Middle = 3,
}

impl MouseButton {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::Left,
            2 => Self::Right,
            3 => Self::Middle,
            _ => return None,
        })
    }
}

/// Events observed on a machine or injected into one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// Absolute pointer position in screen pixels, origin top-left, y down.
    PointerAt {
        x: i32,
        y: i32,
    },
    MouseMove {
        dx: i32,
        dy: i32,
    },
    MouseButton {
        button: MouseButton,
        down: bool,
    },
    /// Wheel notches. Positive `dy` is away from the user (scroll up).
    Wheel {
        dx: i32,
        dy: i32,
    },
    Key {
        key: KeyId,
        down: bool,
    },
}

/// Messages inside the encrypted channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecureMsg {
    Heartbeat {
        unix_ms: u64,
    },
    MouseMove {
        dx: i32,
        dy: i32,
    },
    MouseButton {
        button: MouseButton,
        down: bool,
    },
    Wheel {
        dx: i32,
        dy: i32,
    },
    Key {
        key: KeyId,
        down: bool,
    },
    /// `edge` is the edge of the *receiving* screen the pointer enters from.
    /// `frac` is 0..=10000 along that edge.
    Enter {
        edge: Side,
        frac: u16,
    },
    Leave {
        edge: Side,
        frac: u16,
    },
    Bye,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandshakeMsg {
    ClientHello {
        version: u16,
        pubkey: [u8; 32],
        name: String,
    },
    ServerHello {
        version: u16,
        pubkey: [u8; 32],
        host_id: [u8; 16],
        screen_w: u16,
        screen_h: u16,
        name: String,
    },
    ClientAuth {
        mac: [u8; 32],
    },
    ServerAuth {
        mac: [u8; 32],
    },
    AuthReject {
        reason: u8,
    },
}

pub const REJECT_VERSION: u8 = 1;
pub const REJECT_MAC: u8 = 2;
pub const REJECT_RATE: u8 = 3;

const T_CLIENT_HELLO: u8 = 1;
const T_SERVER_HELLO: u8 = 2;
const T_CLIENT_AUTH: u8 = 3;
const T_SERVER_AUTH: u8 = 4;
const T_AUTH_REJECT: u8 = 5;

const S_HEARTBEAT: u8 = 1;
const S_MOUSE: u8 = 2;
const S_BUTTON: u8 = 3;
const S_WHEEL: u8 = 4;
const S_KEY: u8 = 5;
const S_ENTER: u8 = 6;
const S_LEAVE: u8 = 7;
const S_BYE: u8 = 8;

/// Encrypted application frames use this type byte inside the length prefix.
pub const ENCRYPTED_FRAME: u8 = 0xE1;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodecError {
    #[error("frame too short")]
    Short,
    #[error("frame too large")]
    TooLarge,
    #[error("unknown message type {0}")]
    Unknown(u8),
    #[error("malformed message")]
    Malformed,
    #[error("name is not valid UTF-8")]
    Name,
}

struct R<'a> {
    buf: &'a [u8],
    i: usize,
}

impl<'a> R<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, i: 0 }
    }
    fn rest(&self) -> usize {
        self.buf.len().saturating_sub(self.i)
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        if self.rest() < n {
            return Err(CodecError::Short);
        }
        let s = &self.buf[self.i..self.i + n];
        self.i += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, CodecError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, CodecError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, CodecError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes(b.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, CodecError> {
        Ok(self.u32()? as i32)
    }
    fn arr32(&mut self) -> Result<[u8; 32], CodecError> {
        let b = self.take(32)?;
        let mut a = [0u8; 32];
        a.copy_from_slice(b);
        Ok(a)
    }
    fn arr16(&mut self) -> Result<[u8; 16], CodecError> {
        let b = self.take(16)?;
        let mut a = [0u8; 16];
        a.copy_from_slice(b);
        Ok(a)
    }
    fn name(&mut self) -> Result<String, CodecError> {
        let n = self.u8()? as usize;
        if n > MAX_NAME {
            return Err(CodecError::Malformed);
        }
        let b = self.take(n)?;
        String::from_utf8(b.to_vec()).map_err(|_| CodecError::Name)
    }
}

fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn push_i32(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn push_name(out: &mut Vec<u8>, name: &str) {
    let b = name.as_bytes();
    let n = b.len().min(MAX_NAME);
    out.push(n as u8);
    out.extend_from_slice(&b[..n]);
}

pub fn encode_handshake(msg: &HandshakeMsg) -> Vec<u8> {
    let mut body = Vec::with_capacity(96);
    match msg {
        HandshakeMsg::ClientHello {
            version,
            pubkey,
            name,
        } => {
            body.push(T_CLIENT_HELLO);
            push_u16(&mut body, *version);
            body.extend_from_slice(pubkey);
            push_name(&mut body, name);
        }
        HandshakeMsg::ServerHello {
            version,
            pubkey,
            host_id,
            screen_w,
            screen_h,
            name,
        } => {
            body.push(T_SERVER_HELLO);
            push_u16(&mut body, *version);
            body.extend_from_slice(pubkey);
            body.extend_from_slice(host_id);
            push_u16(&mut body, *screen_w);
            push_u16(&mut body, *screen_h);
            push_name(&mut body, name);
        }
        HandshakeMsg::ClientAuth { mac } => {
            body.push(T_CLIENT_AUTH);
            body.extend_from_slice(mac);
        }
        HandshakeMsg::ServerAuth { mac } => {
            body.push(T_SERVER_AUTH);
            body.extend_from_slice(mac);
        }
        HandshakeMsg::AuthReject { reason } => {
            body.push(T_AUTH_REJECT);
            body.push(*reason);
        }
    }
    frame(&body)
}

pub fn encode_secure(msg: &SecureMsg) -> Vec<u8> {
    let mut body = Vec::with_capacity(16);
    match msg {
        SecureMsg::Heartbeat { unix_ms } => {
            body.push(S_HEARTBEAT);
            push_u64(&mut body, *unix_ms);
        }
        SecureMsg::MouseMove { dx, dy } => {
            body.push(S_MOUSE);
            push_i32(&mut body, *dx);
            push_i32(&mut body, *dy);
        }
        SecureMsg::MouseButton { button, down } => {
            body.push(S_BUTTON);
            body.push(*button as u8);
            body.push(u8::from(*down));
        }
        SecureMsg::Wheel { dx, dy } => {
            body.push(S_WHEEL);
            push_i32(&mut body, *dx);
            push_i32(&mut body, *dy);
        }
        SecureMsg::Key { key, down } => {
            body.push(S_KEY);
            push_u16(&mut body, key.as_u16());
            body.push(u8::from(*down));
        }
        SecureMsg::Enter { edge, frac } => {
            body.push(S_ENTER);
            body.push(*edge as u8);
            push_u16(&mut body, *frac);
        }
        SecureMsg::Leave { edge, frac } => {
            body.push(S_LEAVE);
            body.push(*edge as u8);
            push_u16(&mut body, *frac);
        }
        SecureMsg::Bye => body.push(S_BYE),
    }
    body
}

pub fn decode_secure(bytes: &[u8]) -> Result<SecureMsg, CodecError> {
    let mut r = R::new(bytes);
    let t = r.u8()?;
    let msg = match t {
        S_HEARTBEAT => SecureMsg::Heartbeat { unix_ms: r.u64()? },
        S_MOUSE => SecureMsg::MouseMove {
            dx: r.i32()?,
            dy: r.i32()?,
        },
        S_BUTTON => {
            let button = MouseButton::from_u8(r.u8()?).ok_or(CodecError::Malformed)?;
            let down = r.u8()? != 0;
            SecureMsg::MouseButton { button, down }
        }
        S_WHEEL => SecureMsg::Wheel {
            dx: r.i32()?,
            dy: r.i32()?,
        },
        S_KEY => {
            let key = KeyId::from_u16(r.u16()?).ok_or(CodecError::Malformed)?;
            let down = r.u8()? != 0;
            SecureMsg::Key { key, down }
        }
        S_ENTER => {
            let edge = Side::from_u8(r.u8()?).ok_or(CodecError::Malformed)?;
            SecureMsg::Enter {
                edge,
                frac: r.u16()?,
            }
        }
        S_LEAVE => {
            let edge = Side::from_u8(r.u8()?).ok_or(CodecError::Malformed)?;
            SecureMsg::Leave {
                edge,
                frac: r.u16()?,
            }
        }
        S_BYE => SecureMsg::Bye,
        other => return Err(CodecError::Unknown(other)),
    };
    Ok(msg)
}

fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + body.len());
    push_u32(&mut out, body.len() as u32);
    out.extend_from_slice(body);
    out
}

pub fn decode_handshake(body: &[u8]) -> Result<HandshakeMsg, CodecError> {
    let mut r = R::new(body);
    let t = r.u8()?;
    let msg = match t {
        T_CLIENT_HELLO => HandshakeMsg::ClientHello {
            version: r.u16()?,
            pubkey: r.arr32()?,
            name: r.name()?,
        },
        T_SERVER_HELLO => HandshakeMsg::ServerHello {
            version: r.u16()?,
            pubkey: r.arr32()?,
            host_id: r.arr16()?,
            screen_w: r.u16()?,
            screen_h: r.u16()?,
            name: r.name()?,
        },
        T_CLIENT_AUTH => HandshakeMsg::ClientAuth { mac: r.arr32()? },
        T_SERVER_AUTH => HandshakeMsg::ServerAuth { mac: r.arr32()? },
        T_AUTH_REJECT => HandshakeMsg::AuthReject { reason: r.u8()? },
        other => return Err(CodecError::Unknown(other)),
    };
    Ok(msg)
}

/// Split one length-prefixed frame from `buf`. Returns the body (without the
/// length) and the number of bytes consumed including the length prefix.
pub fn take_frame(buf: &[u8]) -> Result<Option<(&[u8], usize)>, CodecError> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(CodecError::TooLarge);
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    Ok(Some((&buf[4..4 + len], 4 + len)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_roundtrip() {
        let msg = HandshakeMsg::ServerHello {
            version: PROTOCOL_VERSION,
            pubkey: [9u8; 32],
            host_id: [3u8; 16],
            screen_w: 1920,
            screen_h: 1080,
            name: "host".into(),
        };
        let bytes = encode_handshake(&msg);
        let (body, n) = take_frame(&bytes).unwrap().unwrap();
        assert_eq!(n, bytes.len());
        assert_eq!(decode_handshake(body).unwrap(), msg);
    }

    #[test]
    fn secure_roundtrip() {
        let msgs = [
            SecureMsg::Heartbeat { unix_ms: 42 },
            SecureMsg::MouseMove { dx: -3, dy: 8 },
            SecureMsg::MouseButton {
                button: MouseButton::Right,
                down: true,
            },
            SecureMsg::Wheel { dx: 0, dy: -1 },
            SecureMsg::Key {
                key: KeyId::F12,
                down: false,
            },
            SecureMsg::Enter {
                edge: Side::Left,
                frac: 5000,
            },
            SecureMsg::Leave {
                edge: Side::Left,
                frac: 1000,
            },
            SecureMsg::Bye,
        ];
        for msg in msgs {
            let bytes = encode_secure(&msg);
            assert_eq!(decode_secure(&bytes).unwrap(), msg);
        }
    }

    #[test]
    fn rejects_huge_frame() {
        let mut buf = 5u32.to_le_bytes().to_vec();
        buf.extend_from_slice(&[0; 5]);
        // legal small frame
        assert!(take_frame(&buf).unwrap().is_some());
        let huge = (MAX_FRAME as u32 + 1).to_le_bytes();
        assert_eq!(take_frame(&huge).unwrap_err(), CodecError::TooLarge);
    }
}
