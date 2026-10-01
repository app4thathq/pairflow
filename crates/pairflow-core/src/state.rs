//! Persisted host identity, the host pairing code, and the last code used to join.
//!
//! `host_id` is random and stable for this OS user. Clients pin it so a later
//! IP change still refers to the same machine. The host pairing code is stored
//! so a restarted host keeps the same code. `--new-code` rotates it. The code
//! used by `pairflow join` is stored separately so joining does not replace the
//! code this machine shows when it is the host.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::code::{generate_code, normalize_code, CodeError};

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("state file: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Code(#[from] CodeError),
    #[error("host id is malformed")]
    HostId,
}

#[derive(Clone, Debug)]
pub struct Identity {
    pub host_id: [u8; 16],
    pub code: String,
    pub peer_code: Option<String>,
    pub peer_host_id: Option<[u8; 16]>,
    last_role: Option<String>,
    pointer_scale: Option<f32>,
    path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct FileShape {
    host_id_hex: String,
    code: Option<String>,
    #[serde(default)]
    peer_code: Option<String>,
    peer_host_id_hex: Option<String>,
    #[serde(default)]
    last_role: Option<String>,
    #[serde(default)]
    pointer_scale: Option<f32>,
}

impl Identity {
    pub fn load(new_code: bool, explicit_code: Option<&str>) -> Result<Self, StateError> {
        let path = default_path();
        Self::load_at(&path, new_code, explicit_code)
    }

    pub fn load_at(
        path: &Path,
        new_code: bool,
        explicit_code: Option<&str>,
    ) -> Result<Self, StateError> {
        let mut shape = read_shape(path).unwrap_or(FileShape {
            host_id_hex: String::new(),
            code: None,
            peer_code: None,
            peer_host_id_hex: None,
            last_role: None,
            pointer_scale: None,
        });
        let host_id = parse_hex16(&shape.host_id_hex).unwrap_or_else(|_| random_id());
        let code = if let Some(code) = explicit_code {
            normalize_code(code)?
        } else if new_code || shape.code.is_none() {
            generate_code()
        } else {
            match shape.code.as_deref().map(normalize_code) {
                Some(Ok(code)) => code,
                _ => generate_code(),
            }
        };
        let peer_code = match shape.peer_code.as_deref().map(normalize_code) {
            Some(Ok(code)) => Some(code),
            _ => None,
        };
        let peer_host_id = shape
            .peer_host_id_hex
            .as_deref()
            .and_then(|h| parse_hex16(h).ok());
        shape.host_id_hex = hex_encode(&host_id);
        shape.code = Some(code.clone());
        shape.peer_code = peer_code.clone();
        shape.peer_host_id_hex = peer_host_id.map(|id| hex_encode(&id));
        let last_role = shape
            .last_role
            .clone()
            .filter(|role| role == "host" || role == "join");
        let pointer_scale = shape.pointer_scale.filter(|s| s.is_finite() && *s > 0.0);
        shape.last_role = last_role.clone();
        shape.pointer_scale = pointer_scale;
        write_shape(path, &shape)?;
        Ok(Self {
            host_id,
            code,
            peer_code,
            peer_host_id,
            last_role,
            pointer_scale,
            path: path.to_path_buf(),
        })
    }

    pub fn remember_peer(&mut self, peer: [u8; 16]) -> Result<(), StateError> {
        self.peer_host_id = Some(peer);
        self.flush()
    }

    pub fn remember_peer_code(&mut self, code: &str) -> Result<(), StateError> {
        self.peer_code = Some(normalize_code(code)?);
        self.flush()
    }

    pub fn last_role(&self) -> Option<&str> {
        self.last_role.as_deref()
    }

    /// `host` or `join`. Anything else is ignored.
    pub fn remember_role(&mut self, role: &str) -> Result<(), StateError> {
        if role == "host" || role == "join" {
            self.last_role = Some(role.to_string());
            self.flush()
        } else {
            Ok(())
        }
    }

    pub fn pointer_scale(&self) -> f32 {
        self.pointer_scale.unwrap_or(1.0).clamp(0.1, 8.0)
    }

    pub fn remember_pointer_scale(&mut self, scale: f32) -> Result<(), StateError> {
        if scale.is_finite() && (0.1..=8.0).contains(&scale) {
            self.pointer_scale = Some(scale);
            self.flush()
        } else {
            Ok(())
        }
    }

    /// Tray launch: resume hosting, otherwise reconnect to the saved peer.
    pub fn launch_action(&self) -> LaunchAction {
        if self.last_role.as_deref() == Some("host") {
            LaunchAction::Host
        } else if let Some(code) = self.peer_code.clone() {
            LaunchAction::Join(code)
        } else {
            LaunchAction::Idle
        }
    }

    fn flush(&self) -> Result<(), StateError> {
        let shape = FileShape {
            host_id_hex: hex_encode(&self.host_id),
            code: Some(self.code.clone()),
            peer_code: self.peer_code.clone(),
            peer_host_id_hex: self.peer_host_id.map(|id| hex_encode(&id)),
            last_role: self.last_role.clone(),
            pointer_scale: self.pointer_scale,
        };
        write_shape(&self.path, &shape)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchAction {
    Host,
    Join(String),
    Idle,
}

fn default_path() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("pairflow").join("state.json")
}

fn read_shape(path: &Path) -> Option<FileShape> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_shape(path: &Path, shape: &FileShape) -> Result<(), StateError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(shape)?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(text.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        fs::write(path, text)?;
    }
    Ok(())
}

pub fn random_id() -> [u8; 16] {
    use rand::rngs::OsRng;
    use rand::RngCore;
    let mut id = [0u8; 16];
    OsRng.fill_bytes(&mut id);
    id
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

pub fn parse_hex16(text: &str) -> Result<[u8; 16], StateError> {
    let text = text.trim();
    if text.len() != 32 {
        return Err(StateError::HostId);
    }
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).map_err(|_| StateError::HostId)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_code_across_loads() {
        let dir = std::env::temp_dir().join(format!("pairflow-state-{}", std::process::id()));
        let path = dir.join("state.json");
        let _ = fs::remove_dir_all(&dir);
        let first = Identity::load_at(&path, true, None).unwrap();
        let second = Identity::load_at(&path, false, None).unwrap();
        assert_eq!(first.host_id, second.host_id);
        assert_eq!(first.code, second.code);
        let third = Identity::load_at(&path, true, None).unwrap();
        assert_eq!(first.host_id, third.host_id);
        assert_ne!(first.code, third.code);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remembers_peer_and_reloads_the_join_role() {
        let dir = std::env::temp_dir().join(format!("pairflow-role-{}", std::process::id()));
        let path = dir.join("state.json");
        let _ = fs::remove_dir_all(&dir);
        let mut id = Identity::load_at(&path, true, None).unwrap();
        id.remember_peer_code("K7NQ2").unwrap();
        id.remember_role("join").unwrap();
        id.remember_pointer_scale(1.25).unwrap();
        let again = Identity::load_at(&path, false, None).unwrap();
        assert_eq!(again.launch_action(), LaunchAction::Join("K7NQ2".into()));
        assert!((again.pointer_scale() - 1.25).abs() < f32::EPSILON);
        again_role_host(&path);
        let _ = fs::remove_dir_all(&dir);
    }

    fn again_role_host(path: &Path) {
        let mut id = Identity::load_at(path, false, None).unwrap();
        id.remember_role("host").unwrap();
        let again = Identity::load_at(path, false, None).unwrap();
        assert_eq!(again.launch_action(), LaunchAction::Host);
    }
}
