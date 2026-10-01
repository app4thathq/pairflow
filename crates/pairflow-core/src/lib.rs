//! Pairflow core: pairing codes, discovery, encrypted sessions, and the
//! keyboard/mouse share state machine.

pub mod code;
pub mod crypto;
pub mod diag;
pub mod discovery;
pub mod gate;
pub mod session;
pub mod share;
pub mod state;

pub use code::{generate_code, normalize_code, CODE_LEN};
pub use diag::{fact as diag_fact, note as diag_note, snapshot as diag_snapshot};
pub use discovery::{browse, machine_name, Advertiser, Announce, Candidate};
pub use gate::{display_hit, union_desktop, DisplayHit, DisplayRect, Screen};
pub use session::{connect_authenticated, HostListener, Session};
pub use share::{ClientEffect, ClientShare, HostEffect, HostShare};
pub use state::{diagnostics_path, hex_encode, Identity, LaunchAction};
