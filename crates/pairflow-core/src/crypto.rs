//! Ephemeral X25519 handshake authenticated by the pairing code.
//!
//! The code never travels on the wire. It is the HKDF salt that derives the
//! MAC key and the traffic keys, so a wrong code fails the MAC check and
//! produces different session keys. Each side uses a fresh X25519 key, so a
//! recorded session cannot be replayed later even if the code leaks.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey};

type HmacSha256 = Hmac<Sha256>;

pub struct EphemeralKey {
    secret: EphemeralSecret,
    pub public: [u8; 32],
}

impl EphemeralKey {
    pub fn generate() -> Self {
        let secret = EphemeralSecret::random_from_rng(OsRng);
        let public = PublicKey::from(&secret);
        Self {
            secret,
            public: public.to_bytes(),
        }
    }

    pub fn diffie_hellman(self, peer_public: &[u8; 32]) -> [u8; 32] {
        let peer = PublicKey::from(*peer_public);
        self.secret.diffie_hellman(&peer).to_bytes()
    }
}

#[derive(Clone)]
pub struct SessionKeys {
    pub send: [u8; 32],
    pub recv: [u8; 32],
    pub client_mac: [u8; 32],
    pub server_mac: [u8; 32],
}

pub fn transcript(client_pub: &[u8; 32], server_pub: &[u8; 32], host_id: &[u8; 16]) -> Vec<u8> {
    let mut t = Vec::with_capacity(11 + 32 + 32 + 16);
    t.extend_from_slice(b"pairflow-v1");
    t.extend_from_slice(client_pub);
    t.extend_from_slice(server_pub);
    t.extend_from_slice(host_id);
    t
}

pub fn derive_keys(
    shared: &[u8; 32],
    code: &str,
    transcript: &[u8],
    is_server: bool,
) -> SessionKeys {
    let hk = Hkdf::<Sha256>::new(Some(code.as_bytes()), shared);
    let mut c2s = [0u8; 32];
    let mut s2c = [0u8; 32];
    let mut auth = [0u8; 32];
    hk.expand(b"pairflow-c2s", &mut c2s).expect("hkdf length");
    hk.expand(b"pairflow-s2c", &mut s2c).expect("hkdf length");
    hk.expand(b"pairflow-auth", &mut auth).expect("hkdf length");
    let client_mac = hmac_tag(&auth, b"client", transcript);
    let server_mac = hmac_tag(&auth, b"server", transcript);
    if is_server {
        SessionKeys {
            send: s2c,
            recv: c2s,
            client_mac,
            server_mac,
        }
    } else {
        SessionKeys {
            send: c2s,
            recv: s2c,
            client_mac,
            server_mac,
        }
    }
}

fn hmac_tag(key: &[u8], label: &[u8], transcript: &[u8]) -> [u8; 32] {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("hmac key");
    mac.update(label);
    mac.update(transcript);
    let bytes = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    out
}

pub fn mac_ok(expected: &[u8; 32], presented: &[u8; 32]) -> bool {
    expected_eq(expected, presented)
}

fn expected_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

pub fn seal(key: &[u8; 32], counter: u64, plaintext: &[u8]) -> Result<Vec<u8>, ()> {
    let cipher = ChaCha20Poly1305::new(key.into());
    let nonce = nonce_for(counter);
    cipher.encrypt(&nonce, plaintext).map_err(|_| ())
}

pub fn open(key: &[u8; 32], counter: u64, ciphertext: &[u8]) -> Result<Vec<u8>, ()> {
    let cipher = ChaCha20Poly1305::new(key.into());
    let nonce = nonce_for(counter);
    cipher.decrypt(&nonce, ciphertext).map_err(|_| ())
}

fn nonce_for(counter: u64) -> Nonce {
    let mut raw = [0u8; 12];
    raw[4..].copy_from_slice(&counter.to_le_bytes());
    *Nonce::from_slice(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_code_agrees_and_wrong_code_does_not() {
        let client = EphemeralKey::generate();
        let server = EphemeralKey::generate();
        let client_pub = client.public;
        let server_pub = server.public;
        let host_id = [1u8; 16];
        let shared_c = client.diffie_hellman(&server_pub);
        let shared_s = server.diffie_hellman(&client_pub);
        assert_eq!(shared_c, shared_s);
        let t = transcript(&client_pub, &server_pub, &host_id);
        let c = derive_keys(&shared_c, "K7NQ2", &t, false);
        let s = derive_keys(&shared_s, "K7NQ2", &t, true);
        assert_eq!(c.send, s.recv);
        assert_eq!(c.recv, s.send);
        assert_eq!(c.client_mac, s.client_mac);
        assert!(mac_ok(&s.client_mac, &c.client_mac));

        let bad = derive_keys(&shared_s, "ZZZZZ", &t, true);
        assert_ne!(bad.client_mac, c.client_mac);
        assert!(!mac_ok(&bad.client_mac, &c.client_mac));
    }

    #[test]
    fn seal_open_and_reject_tamper() {
        let key = [7u8; 32];
        let ct = seal(&key, 1, b"mouse").unwrap();
        assert_eq!(open(&key, 1, &ct).unwrap(), b"mouse");
        assert!(open(&key, 2, &ct).is_err());
        let mut tampered = ct.clone();
        tampered[0] ^= 1;
        assert!(open(&key, 1, &tampered).is_err());
    }
}
