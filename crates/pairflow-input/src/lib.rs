//! OS input capture and injection.
//!
//! The host captures pointer position and, once the pointer crosses onto the
//! peer, grabs the keyboard and mouse so local applications stop seeing those
//! events. The client injects the stream it is sent.
//!
//! If a real backend cannot be opened (no X11 display, missing Accessibility
//! permission, and so on) callers can use [`Input::dry_run`], which only
//! forwards events pushed with [`Input::emit`].

use pairflow_proto::{InputEvent, Side};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::time::Duration;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("{0}")]
    Message(String),
}

pub(crate) trait Platform: Send {
    fn inject(&self, ev: InputEvent);
    fn warp(&self, x: i32, y: i32);
    fn set_exclusive(&self, on: bool);
    fn shutdown(&self);
    /// Latest absolute cursor sample, if it changed since the previous call.
    /// Backends use this so a full event queue cannot drop the edge position.
    fn take_pointer(&self) -> Option<InputEvent> {
        None
    }
    /// Which desktop edge faces the peer. Windows uses this to claim that edge.
    fn set_stick_side(&self, _side: Side) {}
}

pub struct Input {
    pub origin_x: i32,
    pub origin_y: i32,
    pub width: i32,
    pub height: i32,
    /// False when capture could not be opened and events come only from stdin.
    pub live: bool,
    /// Short label for the startup banner (`Windows hooks active`, `DRY-RUN`, ...).
    pub backend: &'static str,
    event_tx: SyncSender<InputEvent>,
    event_rx: Receiver<InputEvent>,
    ops: Box<dyn Platform>,
}

impl Input {
    pub fn open(dry_run: bool) -> Self {
        if !dry_run {
            #[cfg(target_os = "linux")]
            match linux::open() {
                Ok(input) => return input,
                Err(err) => eprintln!("pairflow: {err}"),
            }
            #[cfg(target_os = "windows")]
            match windows::open() {
                Ok(input) => return input,
                Err(err) => eprintln!("pairflow: {err}"),
            }
            #[cfg(target_os = "macos")]
            match macos::open() {
                Ok(input) => return input,
                Err(err) => eprintln!("pairflow: {err}"),
            }
            eprintln!(
                "pairflow: INPUT CAPTURE FAILED. Real mouse and keyboard will not be shared."
            );
        }
        Self::dry_run()
    }

    pub fn dry_run() -> Self {
        let (event_tx, event_rx) = sync_channel(1024);
        Self {
            origin_x: 0,
            origin_y: 0,
            width: 1920,
            height: 1080,
            live: false,
            backend: "DRY-RUN (stdin only, real mouse ignored)",
            event_tx,
            event_rx,
            ops: Box::new(NullPlatform),
        }
    }

    pub(crate) fn from_channel(
        origin_x: i32,
        origin_y: i32,
        width: i32,
        height: i32,
        event_tx: SyncSender<InputEvent>,
        event_rx: Receiver<InputEvent>,
        ops: Box<dyn Platform>,
    ) -> Self {
        Self {
            origin_x,
            origin_y,
            width,
            height,
            live: true,
            backend: backend_name(),
            event_tx,
            event_rx,
            ops,
        }
    }

    pub fn emitter(&self) -> SyncSender<InputEvent> {
        self.event_tx.clone()
    }

    pub fn emit(&self, ev: InputEvent) {
        match self.event_tx.try_send(ev) {
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    pub fn try_recv(&self) -> Option<InputEvent> {
        // Pointer samples must not wait behind a burst of key events.
        if let Some(ev) = self.ops.take_pointer() {
            return Some(ev);
        }
        self.event_rx.try_recv().ok()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Option<InputEvent> {
        match self.event_rx.recv_timeout(timeout) {
            Ok(ev) => Some(ev),
            Err(_) => self.ops.take_pointer(),
        }
    }

    pub fn inject(&self, ev: InputEvent) {
        self.ops.inject(ev);
    }

    pub fn warp(&self, x: i32, y: i32) {
        self.ops.warp(x, y);
    }

    pub fn set_exclusive(&self, on: bool) {
        self.ops.set_exclusive(on);
    }

    /// Tell the capture backend which edge the peer sits on.
    pub fn set_stick_side(&self, side: Side) {
        self.ops.set_stick_side(side);
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        self.ops.shutdown();
    }
}

fn backend_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "Windows hooks active"
    }
    #[cfg(target_os = "macos")]
    {
        "Quartz event tap active"
    }
    #[cfg(target_os = "linux")]
    {
        "X11 capture active"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        "capture active"
    }
}

struct NullPlatform;

impl Platform for NullPlatform {
    fn inject(&self, ev: InputEvent) {
        eprintln!("pairflow dry-run inject: {ev:?}");
    }
    fn warp(&self, x: i32, y: i32) {
        eprintln!("pairflow dry-run warp: {x},{y}");
    }
    fn set_exclusive(&self, on: bool) {
        eprintln!("pairflow dry-run exclusive: {on}");
    }
    fn shutdown(&self) {}
}
