//! 5-character pairing codes.
//!
//! Alphabet is 32 Crockford-like symbols (no 0/O, 1/I/L) so codes can be read
//! aloud. 32^5 = 33,554,432 possibilities, about 25 bits. That is enough to
//! stop casual mistakes on a LAN when attempts are rate-limited. It is not a
//! long-term secret against an attacker who can hammer the TCP port. See
//! ARCHITECTURE.md.

use rand::rngs::OsRng;
use rand::RngCore;

pub const CODE_LEN: usize = 5;
pub const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodeError {
    #[error("pairing code must be {CODE_LEN} characters from {alphabet} (got {got})", alphabet = alphabet_string())]
    Invalid { got: String },
}

fn alphabet_string() -> String {
    String::from_utf8_lossy(ALPHABET).into_owned()
}

pub fn generate_code() -> String {
    let mut rng = OsRng;
    let mut out = String::with_capacity(CODE_LEN);
    for _ in 0..CODE_LEN {
        let idx = (rng.next_u32() as usize) % ALPHABET.len();
        out.push(ALPHABET[idx] as char);
    }
    out
}

pub fn normalize_code(input: &str) -> Result<String, CodeError> {
    let mut out = String::with_capacity(CODE_LEN);
    for c in input.chars() {
        if c.is_whitespace() || c == '-' {
            continue;
        }
        let u = (c.to_ascii_uppercase() as u8) as char;
        if !ALPHABET.contains(&(u as u8)) {
            return Err(CodeError::Invalid {
                got: input.to_string(),
            });
        }
        out.push(u);
    }
    if out.len() != CODE_LEN {
        return Err(CodeError::Invalid {
            got: input.to_string(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generated_codes_are_valid_and_varied() {
        let mut seen = HashSet::new();
        for _ in 0..32 {
            let code = generate_code();
            assert_eq!(normalize_code(&code).unwrap(), code);
            seen.insert(code);
        }
        assert!(seen.len() > 20);
    }

    #[test]
    fn normalize_strips_space_and_rejects_ambiguous() {
        assert_eq!(normalize_code(" ab-cd2 ").unwrap().len(), 5);
        assert!(normalize_code("ABCDO").is_err());
        assert!(normalize_code("ABCD").is_err());
        assert!(normalize_code("ABCDE0").is_err());
    }
}
