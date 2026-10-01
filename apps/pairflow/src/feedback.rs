//! Optional status channel for the tray UI. The CLI ignores it.

use std::sync::mpsc::Sender;
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiEvent {
    Hosting {
        code: String,
    },
    Joining {
        code: String,
    },
    Paired {
        peer: String,
    },
    Waiting,
    Reconnecting {
        code: String,
    },
    Message(String),
    Stopped,
    /// A newer GitHub release is ready to install.
    UpdateReady {
        version: String,
    },
    /// Status that belongs to the updater, not the pairing session.
    UpdateNote(String),
    /// The background check finished (success or failure).
    UpdateIdle,
    /// The installer downloaded a file and the UI thread should apply it.
    UpdateStaged,
}

static SINK: Mutex<Option<Sender<UiEvent>>> = Mutex::new(None);

pub fn set_sink(tx: Option<Sender<UiEvent>>) {
    *SINK.lock().unwrap() = tx;
}

pub fn emit(ev: UiEvent) {
    if let Some(tx) = SINK.lock().unwrap().as_ref() {
        let _ = tx.send(ev);
    }
}
