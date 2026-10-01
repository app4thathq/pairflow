//! Optional status channel for the tray UI. The CLI ignores it.

use std::sync::mpsc::Sender;
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiEvent {
    Hosting { code: String },
    Joining { code: String },
    Paired { peer: String },
    Waiting,
    Reconnecting { code: String },
    Message(String),
    Stopped,
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
