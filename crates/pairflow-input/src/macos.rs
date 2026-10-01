//! Quartz event taps for capture and `CGEvent` posts for injection.
//!
//! The process must be trusted for Accessibility. An unsigned `.app` is added
//! from System Settings → Privacy & Security → Accessibility. Input Monitoring
//! may also be required on newer macOS releases.

use super::{add_motion, Input, InputError, Platform};
use core_foundation::runloop::{kCFRunLoopCommonModes, kCFRunLoopDefaultMode, CFRunLoop};
use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType, CallbackResult, EventField, ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use pairflow_core::union_desktop;
use pairflow_proto::{InputEvent, KeyId, MouseButton};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::Duration;

static EXCLUSIVE: AtomicBool = AtomicBool::new(false);
static ANCHOR_X: AtomicI32 = AtomicI32::new(0);
static ANCHOR_Y: AtomicI32 = AtomicI32::new(0);
static LAST_FLAGS: AtomicU64 = AtomicU64::new(0);
static STOP: AtomicBool = AtomicBool::new(false);
static TAP_DISABLED: AtomicBool = AtomicBool::new(false);

struct SharedTx {
    tx: Option<SyncSender<InputEvent>>,
}

static TX: Mutex<SharedTx> = Mutex::new(SharedTx { tx: None });

const SHIFT: u64 = 0x0002_0000;
const CONTROL: u64 = 0x0004_0000;
const ALTERNATE: u64 = 0x0008_0000;
const COMMAND: u64 = 0x0010_0000;

/// Union of every active display in Quartz global points.
///
/// `CGDisplay::pixels_wide` is the framebuffer size. Mouse warps and event
/// locations use points (`bounds`), and a display to the left of the main
/// panel has a negative origin. Measuring only the main display in pixels
/// confined the guest cursor to a box on one screen.
fn desktop_points() -> (i32, i32, i32, i32) {
    let mut rects = Vec::new();
    if let Ok(ids) = CGDisplay::active_displays() {
        for id in ids {
            let bounds = CGDisplay::new(id).bounds();
            let w = bounds.size.width.round() as i32;
            let h = bounds.size.height.round() as i32;
            if w > 0 && h > 0 {
                rects.push((
                    bounds.origin.x.round() as i32,
                    bounds.origin.y.round() as i32,
                    w,
                    h,
                ));
            }
        }
    }
    if let Some(desktop) = union_desktop(&rects) {
        return desktop;
    }
    let bounds = CGDisplay::main().bounds();
    (
        bounds.origin.x.round() as i32,
        bounds.origin.y.round() as i32,
        (bounds.size.width.round() as i32).max(2),
        (bounds.size.height.round() as i32).max(2),
    )
}

pub fn open() -> Result<Input, InputError> {
    let (origin_x, origin_y, width, height) = desktop_points();
    let (event_tx, event_rx) = sync_channel(1024);
    {
        TX.lock().unwrap().tx = Some(event_tx.clone());
    }
    STOP.store(false, Ordering::Relaxed);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let join = thread::spawn(move || tap_thread(ready_tx));
    match ready_rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(Ok(())) => {}
        Ok(Err(err)) => return Err(InputError::Message(err)),
        Err(_) => {
            return Err(InputError::Message(
                "macOS event tap did not start. Grant Accessibility permission to Pairflow.".into(),
            ))
        }
    }
    Ok(Input::from_channel(
        origin_x,
        origin_y,
        width,
        height,
        event_tx,
        event_rx,
        Box::new(MacOps {
            join: Mutex::new(Some(join)),
        }),
    ))
}

struct MacOps {
    join: Mutex<Option<JoinHandle<()>>>,
}

impl Platform for MacOps {
    fn inject(&self, ev: InputEvent) {
        if let Err(err) = inject_event(&ev) {
            eprintln!("pairflow: inject: {err}");
        }
    }
    fn warp(&self, x: i32, y: i32) {
        ANCHOR_X.store(x, Ordering::Relaxed);
        ANCHOR_Y.store(y, Ordering::Relaxed);
        let _ = CGDisplay::warp_mouse_cursor_position(CGPoint::new(x as f64, y as f64));
    }
    fn set_exclusive(&self, on: bool) {
        if on {
            if let Some((x, y)) = current_pointer() {
                ANCHOR_X.store(x, Ordering::Relaxed);
                ANCHOR_Y.store(y, Ordering::Relaxed);
            }
        }
        EXCLUSIVE.store(on, Ordering::Relaxed);
    }
    fn shutdown(&self) {
        STOP.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.lock().unwrap().take() {
            let _ = join.join();
        }
        TX.lock().unwrap().tx = None;
    }
}

fn tap_thread(ready: std::sync::mpsc::Sender<Result<(), String>>) {
    let tap = CGEventTap::new(
        CGEventTapLocation::HID,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        vec![
            CGEventType::MouseMoved,
            CGEventType::LeftMouseDragged,
            CGEventType::RightMouseDragged,
            CGEventType::OtherMouseDragged,
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseUp,
            CGEventType::RightMouseDown,
            CGEventType::RightMouseUp,
            CGEventType::OtherMouseDown,
            CGEventType::OtherMouseUp,
            CGEventType::ScrollWheel,
            CGEventType::KeyDown,
            CGEventType::KeyUp,
            CGEventType::FlagsChanged,
        ],
        |_proxy, ty, event| on_event(ty, event),
    );
    let tap = match tap {
        Ok(tap) => tap,
        Err(()) => {
            let _ = ready.send(Err(
                "CGEventTapCreate failed. Grant Accessibility permission to Pairflow.".into(),
            ));
            return;
        }
    };
    let source = match tap.mach_port().create_runloop_source(0) {
        Ok(source) => source,
        Err(()) => {
            let _ = ready.send(Err(
                "failed to create a run-loop source for the event tap".into()
            ));
            return;
        }
    };
    let loop_ = CFRunLoop::get_current();
    loop_.add_source(&source, unsafe { kCFRunLoopCommonModes });
    tap.enable();
    let _ = ready.send(Ok(()));
    while !STOP.load(Ordering::Relaxed) {
        if TAP_DISABLED.swap(false, Ordering::Relaxed) {
            tap.enable();
        }
        let _ = CFRunLoop::run_in_mode(
            unsafe { kCFRunLoopDefaultMode },
            Duration::from_millis(10),
            false,
        );
    }
}

fn on_event(ty: CGEventType, event: &CGEvent) -> CallbackResult {
    if matches!(
        ty,
        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
    ) {
        TAP_DISABLED.store(true, Ordering::Relaxed);
        return CallbackResult::Keep;
    }
    let exclusive = EXCLUSIVE.load(Ordering::Relaxed);
    let loc = event.location();
    let x = loc.x.round() as i32;
    let y = loc.y.round() as i32;
    match ty {
        CGEventType::MouseMoved
        | CGEventType::LeftMouseDragged
        | CGEventType::RightMouseDragged
        | CGEventType::OtherMouseDragged => {
            if exclusive {
                let dx = x - ANCHOR_X.load(Ordering::Relaxed);
                let dy = y - ANCHOR_Y.load(Ordering::Relaxed);
                if dx != 0 || dy != 0 {
                    add_motion(dx, dy);
                    let _ = CGDisplay::warp_mouse_cursor_position(CGPoint::new(
                        ANCHOR_X.load(Ordering::Relaxed) as f64,
                        ANCHOR_Y.load(Ordering::Relaxed) as f64,
                    ));
                }
                CallbackResult::Drop
            } else {
                emit(InputEvent::PointerAt { x, y });
                CallbackResult::Keep
            }
        }
        CGEventType::LeftMouseDown | CGEventType::LeftMouseUp => button(
            MouseButton::Left,
            same_type(ty, CGEventType::LeftMouseDown),
            exclusive,
        ),
        CGEventType::RightMouseDown | CGEventType::RightMouseUp => button(
            MouseButton::Right,
            same_type(ty, CGEventType::RightMouseDown),
            exclusive,
        ),
        CGEventType::OtherMouseDown | CGEventType::OtherMouseUp => button(
            MouseButton::Middle,
            same_type(ty, CGEventType::OtherMouseDown),
            exclusive,
        ),
        CGEventType::ScrollWheel => {
            if exclusive {
                let dy = event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1)
                    as i32;
                let dx = event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2)
                    as i32;
                if dx != 0 || dy != 0 {
                    emit(InputEvent::Wheel { dx, dy });
                }
                CallbackResult::Drop
            } else {
                CallbackResult::Keep
            }
        }
        CGEventType::KeyDown | CGEventType::KeyUp => {
            let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
            if let Some(key) = mac_to_key(code) {
                if !key.is_modifier() {
                    emit(InputEvent::Key {
                        key,
                        down: same_type(ty, CGEventType::KeyDown),
                    });
                }
            }
            if exclusive {
                CallbackResult::Drop
            } else {
                CallbackResult::Keep
            }
        }
        CGEventType::FlagsChanged => {
            let bits = event.get_flags().bits();
            let prev = LAST_FLAGS.swap(bits, Ordering::Relaxed);
            diff_flag(prev, bits, SHIFT, KeyId::LeftShift);
            diff_flag(prev, bits, CONTROL, KeyId::LeftControl);
            diff_flag(prev, bits, ALTERNATE, KeyId::LeftAlt);
            diff_flag(prev, bits, COMMAND, KeyId::LeftMeta);
            if exclusive {
                CallbackResult::Drop
            } else {
                CallbackResult::Keep
            }
        }
        _ => CallbackResult::Keep,
    }
}

fn same_type(ty: CGEventType, expected: CGEventType) -> bool {
    ty as u32 == expected as u32
}

fn button(button: MouseButton, down: bool, exclusive: bool) -> CallbackResult {
    if exclusive {
        emit(InputEvent::MouseButton { button, down });
        CallbackResult::Drop
    } else {
        CallbackResult::Keep
    }
}

fn diff_flag(prev: u64, bits: u64, mask: u64, key: KeyId) {
    if prev & mask != bits & mask {
        emit(InputEvent::Key {
            key,
            down: bits & mask != 0,
        });
    }
}

fn emit(ev: InputEvent) {
    let guard = TX.lock().unwrap();
    if let Some(tx) = &guard.tx {
        match tx.try_send(ev) {
            Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

fn current_pointer() -> Option<(i32, i32)> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()?;
    let event = CGEvent::new(source).ok()?;
    let loc = event.location();
    Some((loc.x.round() as i32, loc.y.round() as i32))
}

fn source() -> Result<CGEventSource, ()> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState)
}

fn inject_event(ev: &InputEvent) -> Result<(), String> {
    let source = source().map_err(|_| "CGEventSource".to_string())?;
    match ev {
        InputEvent::MouseMove { dx, dy } | InputEvent::PointerAt { x: dx, y: dy } => {
            let (x, y) = if matches!(ev, InputEvent::PointerAt { .. }) {
                (*dx, *dy)
            } else {
                let (x, y) = current_pointer().unwrap_or((0, 0));
                (x + dx, y + dy)
            };
            let event = CGEvent::new_mouse_event(
                source,
                CGEventType::MouseMoved,
                CGPoint::new(x as f64, y as f64),
                core_graphics::event::CGMouseButton::Left,
            )
            .map_err(|_| "mouse event".to_string())?;
            event.post(CGEventTapLocation::HID);
        }
        InputEvent::MouseButton { button, down } => {
            let (x, y) = current_pointer().unwrap_or((0, 0));
            let (ty, which) = match (button, *down) {
                (MouseButton::Left, true) => (
                    CGEventType::LeftMouseDown,
                    core_graphics::event::CGMouseButton::Left,
                ),
                (MouseButton::Left, false) => (
                    CGEventType::LeftMouseUp,
                    core_graphics::event::CGMouseButton::Left,
                ),
                (MouseButton::Right, true) => (
                    CGEventType::RightMouseDown,
                    core_graphics::event::CGMouseButton::Right,
                ),
                (MouseButton::Right, false) => (
                    CGEventType::RightMouseUp,
                    core_graphics::event::CGMouseButton::Right,
                ),
                (MouseButton::Middle, true) => (
                    CGEventType::OtherMouseDown,
                    core_graphics::event::CGMouseButton::Center,
                ),
                (MouseButton::Middle, false) => (
                    CGEventType::OtherMouseUp,
                    core_graphics::event::CGMouseButton::Center,
                ),
            };
            let event =
                CGEvent::new_mouse_event(source, ty, CGPoint::new(x as f64, y as f64), which)
                    .map_err(|_| "button event".to_string())?;
            event.post(CGEventTapLocation::HID);
        }
        InputEvent::Wheel { dx, dy } => {
            let event = CGEvent::new_scroll_event(source, ScrollEventUnit::LINE, 2, *dy, *dx, 0)
                .map_err(|_| "scroll event".to_string())?;
            event.post(CGEventTapLocation::HID);
        }
        InputEvent::Key { key, down } => {
            let Some(code) = key_to_mac(*key) else {
                return Ok(());
            };
            let event = CGEvent::new_keyboard_event(source, code, *down)
                .map_err(|_| "key event".to_string())?;
            event.post(CGEventTapLocation::HID);
        }
    }
    Ok(())
}

fn mac_to_key(code: u16) -> Option<KeyId> {
    Some(match code {
        0x00 => KeyId::A,
        0x01 => KeyId::S,
        0x02 => KeyId::D,
        0x03 => KeyId::F,
        0x04 => KeyId::H,
        0x05 => KeyId::G,
        0x06 => KeyId::Z,
        0x07 => KeyId::X,
        0x08 => KeyId::C,
        0x09 => KeyId::V,
        0x0B => KeyId::B,
        0x0C => KeyId::Q,
        0x0D => KeyId::W,
        0x0E => KeyId::E,
        0x0F => KeyId::R,
        0x10 => KeyId::Y,
        0x11 => KeyId::T,
        0x12 => KeyId::Digit1,
        0x13 => KeyId::Digit2,
        0x14 => KeyId::Digit3,
        0x15 => KeyId::Digit4,
        0x16 => KeyId::Digit6,
        0x17 => KeyId::Digit5,
        0x18 => KeyId::Equal,
        0x19 => KeyId::Digit9,
        0x1A => KeyId::Digit7,
        0x1B => KeyId::Minus,
        0x1C => KeyId::Digit8,
        0x1D => KeyId::Digit0,
        0x1E => KeyId::RightBracket,
        0x1F => KeyId::O,
        0x20 => KeyId::U,
        0x21 => KeyId::LeftBracket,
        0x22 => KeyId::I,
        0x23 => KeyId::P,
        0x24 => KeyId::Enter,
        0x25 => KeyId::L,
        0x26 => KeyId::J,
        0x27 => KeyId::Apostrophe,
        0x28 => KeyId::K,
        0x29 => KeyId::Semicolon,
        0x2A => KeyId::Backslash,
        0x2B => KeyId::Comma,
        0x2C => KeyId::Slash,
        0x2D => KeyId::N,
        0x2E => KeyId::M,
        0x2F => KeyId::Period,
        0x30 => KeyId::Tab,
        0x31 => KeyId::Space,
        0x32 => KeyId::Grave,
        0x33 => KeyId::Backspace,
        0x35 => KeyId::Escape,
        0x37 => KeyId::LeftMeta,
        0x38 => KeyId::LeftShift,
        0x39 => KeyId::CapsLock,
        0x3A => KeyId::LeftAlt,
        0x3B => KeyId::LeftControl,
        0x36 => KeyId::RightMeta,
        0x3C => KeyId::RightShift,
        0x3D => KeyId::RightAlt,
        0x3E => KeyId::RightControl,
        0x60 => KeyId::F5,
        0x61 => KeyId::F6,
        0x62 => KeyId::F7,
        0x63 => KeyId::F3,
        0x64 => KeyId::F8,
        0x65 => KeyId::F9,
        0x67 => KeyId::F11,
        0x6D => KeyId::F10,
        0x6F => KeyId::F12,
        0x76 => KeyId::F4,
        0x78 => KeyId::F2,
        0x7A => KeyId::F1,
        0x73 => KeyId::Home,
        0x74 => KeyId::PageUp,
        0x75 => KeyId::Delete,
        0x77 => KeyId::End,
        0x79 => KeyId::PageDown,
        0x7B => KeyId::Left,
        0x7C => KeyId::Right,
        0x7D => KeyId::Down,
        0x7E => KeyId::Up,
        _ => return None,
    })
}

fn key_to_mac(key: KeyId) -> Option<u16> {
    Some(match key {
        KeyId::A => 0x00,
        KeyId::S => 0x01,
        KeyId::D => 0x02,
        KeyId::F => 0x03,
        KeyId::H => 0x04,
        KeyId::G => 0x05,
        KeyId::Z => 0x06,
        KeyId::X => 0x07,
        KeyId::C => 0x08,
        KeyId::V => 0x09,
        KeyId::B => 0x0B,
        KeyId::Q => 0x0C,
        KeyId::W => 0x0D,
        KeyId::E => 0x0E,
        KeyId::R => 0x0F,
        KeyId::Y => 0x10,
        KeyId::T => 0x11,
        KeyId::Digit1 => 0x12,
        KeyId::Digit2 => 0x13,
        KeyId::Digit3 => 0x14,
        KeyId::Digit4 => 0x15,
        KeyId::Digit6 => 0x16,
        KeyId::Digit5 => 0x17,
        KeyId::Equal => 0x18,
        KeyId::Digit9 => 0x19,
        KeyId::Digit7 => 0x1A,
        KeyId::Minus => 0x1B,
        KeyId::Digit8 => 0x1C,
        KeyId::Digit0 => 0x1D,
        KeyId::RightBracket => 0x1E,
        KeyId::O => 0x1F,
        KeyId::U => 0x20,
        KeyId::LeftBracket => 0x21,
        KeyId::I => 0x22,
        KeyId::P => 0x23,
        KeyId::Enter => 0x24,
        KeyId::L => 0x25,
        KeyId::J => 0x26,
        KeyId::Apostrophe => 0x27,
        KeyId::K => 0x28,
        KeyId::Semicolon => 0x29,
        KeyId::Backslash => 0x2A,
        KeyId::Comma => 0x2B,
        KeyId::Slash => 0x2C,
        KeyId::N => 0x2D,
        KeyId::M => 0x2E,
        KeyId::Period => 0x2F,
        KeyId::Tab => 0x30,
        KeyId::Space => 0x31,
        KeyId::Grave => 0x32,
        KeyId::Backspace => 0x33,
        KeyId::Escape => 0x35,
        KeyId::LeftMeta | KeyId::RightMeta => 0x37,
        KeyId::LeftShift | KeyId::RightShift => 0x38,
        KeyId::CapsLock => 0x39,
        KeyId::LeftAlt | KeyId::RightAlt => 0x3A,
        KeyId::LeftControl | KeyId::RightControl => 0x3B,
        KeyId::F5 => 0x60,
        KeyId::F6 => 0x61,
        KeyId::F7 => 0x62,
        KeyId::F3 => 0x63,
        KeyId::F8 => 0x64,
        KeyId::F9 => 0x65,
        KeyId::F11 => 0x67,
        KeyId::F10 => 0x6D,
        KeyId::F12 => 0x6F,
        KeyId::F4 => 0x76,
        KeyId::F2 => 0x78,
        KeyId::F1 => 0x7A,
        KeyId::Home => 0x73,
        KeyId::PageUp => 0x74,
        KeyId::Delete => 0x75,
        KeyId::End => 0x77,
        KeyId::PageDown => 0x79,
        KeyId::Left => 0x7B,
        KeyId::Right => 0x7C,
        KeyId::Down => 0x7D,
        KeyId::Up => 0x7E,
        KeyId::Insert => return None,
    })
}

#[allow(dead_code)]
fn _flags(flags: CGEventFlags) -> CGEventFlags {
    flags
}
