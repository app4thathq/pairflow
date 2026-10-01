//! Ring buffer the tray can copy after a roam attempt.
//!
//! Facts are the latest value of each key (screen size, injection path).
//! Lines are the recent motion samples, oldest dropped after [`CAP`] entries.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::time::Instant;

const CAP: usize = 400;

struct Buf {
    start: Instant,
    lines: VecDeque<String>,
    facts: BTreeMap<String, String>,
}

static BUF: Mutex<Option<Buf>> = Mutex::new(None);

fn with_buf<T>(f: impl FnOnce(&mut Buf) -> T) -> T {
    let mut guard = BUF.lock().unwrap();
    if guard.is_none() {
        *guard = Some(Buf {
            start: Instant::now(),
            lines: VecDeque::new(),
            facts: BTreeMap::new(),
        });
    }
    f(guard.as_mut().unwrap())
}

pub fn fact(key: &str, value: impl AsRef<str>) {
    with_buf(|buf| {
        buf.facts
            .insert(key.to_string(), value.as_ref().to_string());
    });
}

pub fn note(line: impl AsRef<str>) {
    with_buf(|buf| {
        let ms = buf.start.elapsed().as_millis();
        let row = format!("{ms:>8} {}", line.as_ref());
        if buf.lines.len() == CAP {
            buf.lines.pop_front();
        }
        buf.lines.push_back(row);
    });
}

/// Text blob for the clipboard and `diagnostics.txt`.
pub fn snapshot(version: &str) -> String {
    with_buf(|buf| {
        let mut out = String::new();
        out.push_str(&format!(
            "pairflow {version}\nos {} {}\n",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
        out.push_str("Copy this whole blob after moving the pointer on the limited rectangle.\n");
        for (key, value) in &buf.facts {
            out.push_str(key);
            out.push_str(": ");
            out.push_str(value);
            out.push('\n');
        }
        out.push_str("--- recent ---\n");
        for line in &buf.lines {
            out.push_str(line);
            out.push('\n');
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_contains_version_facts_and_recent_lines() {
        let token = format!("diag-{}", std::process::id());
        fact("unit", &token);
        note(format!("sample {token}"));
        let text = snapshot("9.9.9");
        assert!(text.contains("pairflow 9.9.9"));
        assert!(text.contains(&format!("unit: {token}")));
        assert!(text.contains(&format!("sample {token}")));
        assert!(text.contains("os "));
    }
}
