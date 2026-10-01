//! Pairflow core: pairing codes, discovery, encrypted sessions, and the
//! keyboard/mouse share state machine.

pub mod code;
pub mod crypto;
pub mod discovery;
pub mod gate;
pub mod session;
pub mod share;
pub mod state;

pub use code::{generate_code, normalize_code, CODE_LEN};
pub use discovery::{browse, machine_name, Advertiser, Announce, Candidate};
pub use gate::{union_desktop, Screen};
pub use session::{connect_authenticated, HostListener, Session};
pub use share::{ClientEffect, ClientShare, HostEffect, HostShare};
pub use state::{hex_encode, Identity, LaunchAction};
