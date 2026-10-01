//! X11 capture and injection through the pure-Rust `x11rb` client.
//!
//! Wayland compositors do not allow this kind of global grab. Pairflow needs
//! an X11 session, or XWayland with `DISPLAY` set (grabs are best-effort there).

use super::{Input, InputError, Platform};
use pairflow_proto::{InputEvent, KeyId, MouseButton};
use std::collections::HashSet;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use x11rb::connection::Connection;
use x11rb::protocol::xinput::{self, ConnectionExt as _, Device, DeviceId, XIEventMask};
use x11rb::protocol::xproto::{ConnectionExt as _, EventMask, GrabMode, GrabStatus};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const BUTTON_PRESS: u8 = 4;
const BUTTON_RELEASE: u8 = 5;
const MOTION_NOTIFY: u8 = 6;

enum Cmd {
    Inject(InputEvent),
    Warp(i32, i32),
    Exclusive(bool),
    Stop,
}

struct ThreadState {
    exclusive: bool,
    grabbed: bool,
    anchor_x: i16,
    anchor_y: i16,
    last_pos: Option<(i32, i32)>,
    masters: HashSet<u16>,
    min_keycode: u8,
    per: usize,
    keysyms: Vec<u32>,
}

pub fn open() -> Result<Input, InputError> {
    let (conn, screen_num) = RustConnection::connect(None).map_err(|err| {
        InputError::Message(format!(
            "X11 connection failed ({err}). Wayland is not supported in this MVP; use an X11 session or XWayland, or pass --dry-run"
        ))
    })?;
    let _ = conn
        .xtest_get_version(2, 2)
        .map_err(|err| InputError::Message(format!("XTest request failed: {err}")))?
        .reply()
        .map_err(|err| InputError::Message(format!("XTest extension missing: {err}")))?;
    let _ = conn
        .xinput_xi_query_version(2, 2)
        .map_err(|err| InputError::Message(err.to_string()))?
        .reply()
        .map_err(|err| InputError::Message(format!("XInput2 missing: {err}")))?;

    let screen = &conn.setup().roots[screen_num];
    let width = screen.width_in_pixels as i32;
    let height = screen.height_in_pixels as i32;
    let (event_tx, event_rx) = sync_channel(1024);
    let (cmd_tx, cmd_rx) = sync_channel(256);
    let thread_tx = event_tx.clone();
    let join = thread::spawn(move || {
        if let Err(err) = run(conn, screen_num, thread_tx, cmd_rx) {
            eprintln!("pairflow: X11 input thread stopped: {err}");
        }
    });
    let ops = LinuxOps {
        cmd_tx,
        join: std::sync::Mutex::new(Some(join)),
    };
    Ok(Input::from_channel(
        0,
        0,
        width,
        height,
        event_tx,
        event_rx,
        Box::new(ops),
    ))
}

struct LinuxOps {
    cmd_tx: SyncSender<Cmd>,
    join: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl Platform for LinuxOps {
    fn inject(&self, ev: InputEvent) {
        let _ = self.cmd_tx.try_send(Cmd::Inject(ev));
    }
    fn warp(&self, x: i32, y: i32) {
        let _ = self.cmd_tx.try_send(Cmd::Warp(x, y));
    }
    fn set_exclusive(&self, on: bool) {
        let _ = self.cmd_tx.try_send(Cmd::Exclusive(on));
    }
    fn shutdown(&self) {
        let _ = self.cmd_tx.try_send(Cmd::Stop);
        if let Some(join) = self.join.lock().unwrap().take() {
            let _ = join.join();
        }
    }
}

fn run(
    conn: RustConnection,
    screen_num: usize,
    events: SyncSender<InputEvent>,
    cmds: Receiver<Cmd>,
) -> Result<(), InputError> {
    let root = conn.setup().roots[screen_num].root;
    let min = conn.setup().min_keycode;
    let max = conn.setup().max_keycode;
    let count = max.saturating_sub(min).saturating_add(1);
    let mapping = conn
        .get_keyboard_mapping(min, count)
        .map_err(|e| InputError::Message(e.to_string()))?
        .reply()
        .map_err(|e| InputError::Message(e.to_string()))?;
    let mut state = ThreadState {
        exclusive: false,
        grabbed: false,
        anchor_x: 0,
        anchor_y: 0,
        last_pos: None,
        masters: master_devices(&conn),
        min_keycode: min,
        per: mapping.keysyms_per_keycode.max(1) as usize,
        keysyms: mapping.keysyms.iter().map(|k| *k as u32).collect(),
    };
    select_raw(&conn, root)?;
    let _ = conn.flush();

    loop {
        loop {
            match cmds.try_recv() {
                Ok(Cmd::Stop) => return Ok(()),
                Ok(Cmd::Exclusive(on)) => {
                    if let Err(err) = set_exclusive(&conn, root, on, &mut state) {
                        eprintln!("pairflow: exclusive mode: {err}");
                    }
                }
                Ok(Cmd::Warp(x, y)) => {
                    let _ = warp(&conn, root, x, y);
                }
                Ok(Cmd::Inject(ev)) => {
                    if let Err(err) = inject(&conn, &state, ev) {
                        eprintln!("pairflow: inject: {err}");
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }

        match conn.poll_for_event() {
            Ok(Some(event)) => handle_event(&conn, root, &events, &mut state, event)?,
            Ok(None) => {
                if !state.exclusive {
                    sample_pointer(&conn, root, &events, &mut state)?;
                }
                thread::sleep(Duration::from_millis(4));
            }
            Err(err) => return Err(InputError::Message(err.to_string())),
        }
    }
}

fn master_devices(conn: &RustConnection) -> HashSet<u16> {
    let mut masters = HashSet::new();
    let device_all = DeviceId::from(Device::ALL);
    let Ok(cookie) = conn.xinput_xi_query_device(device_all) else {
        return masters;
    };
    let Ok(reply) = cookie.reply() else {
        return masters;
    };
    for info in reply.infos {
        if matches!(
            info.type_,
            xinput::DeviceType::MASTER_POINTER | xinput::DeviceType::MASTER_KEYBOARD
        ) {
            masters.insert(info.deviceid);
        }
    }
    masters
}

fn select_raw(conn: &RustConnection, root: u32) -> Result<(), InputError> {
    let bits = XIEventMask::RAW_KEY_PRESS
        | XIEventMask::RAW_KEY_RELEASE
        | XIEventMask::RAW_BUTTON_PRESS
        | XIEventMask::RAW_BUTTON_RELEASE
        | XIEventMask::RAW_MOTION;
    let mask = xinput::EventMask {
        deviceid: DeviceId::from(Device::ALL),
        mask: vec![bits],
    };
    conn.xinput_xi_select_events(root, &[mask])
        .map_err(|e| InputError::Message(e.to_string()))?
        .check()
        .map_err(|e| InputError::Message(e.to_string()))?;
    Ok(())
}

fn set_exclusive(
    conn: &RustConnection,
    root: u32,
    on: bool,
    state: &mut ThreadState,
) -> Result<(), InputError> {
    state.exclusive = on;
    if on {
        let pointer = conn
            .query_pointer(root)
            .map_err(|e| InputError::Message(e.to_string()))?
            .reply()
            .map_err(|e| InputError::Message(e.to_string()))?;
        state.anchor_x = pointer.root_x;
        state.anchor_y = pointer.root_y;
        let pointer_grab = conn
            .grab_pointer(
                false,
                root,
                EventMask::POINTER_MOTION | EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                x11rb::NONE,
                x11rb::NONE,
                x11rb::CURRENT_TIME,
            )
            .map_err(|e| InputError::Message(e.to_string()))?
            .reply()
            .map_err(|e| InputError::Message(e.to_string()))?;
        let keyboard_grab = conn
            .grab_keyboard(
                false,
                root,
                x11rb::CURRENT_TIME,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .map_err(|e| InputError::Message(e.to_string()))?
            .reply()
            .map_err(|e| InputError::Message(e.to_string()))?;
        state.grabbed = pointer_grab.status == GrabStatus::SUCCESS
            && keyboard_grab.status == GrabStatus::SUCCESS;
        if !state.grabbed {
            eprintln!(
                "pairflow: X11 grab was not granted; forwarding raw events without swallowing"
            );
        }
    } else if state.grabbed {
        let _ = conn.ungrab_pointer(x11rb::CURRENT_TIME);
        let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
        state.grabbed = false;
    }
    let _ = conn.flush();
    Ok(())
}

fn sample_pointer(
    conn: &RustConnection,
    root: u32,
    events: &SyncSender<InputEvent>,
    state: &mut ThreadState,
) -> Result<(), InputError> {
    let pointer = conn
        .query_pointer(root)
        .map_err(|e| InputError::Message(e.to_string()))?
        .reply()
        .map_err(|e| InputError::Message(e.to_string()))?;
    let pos = (pointer.root_x as i32, pointer.root_y as i32);
    if state.last_pos != Some(pos) {
        state.last_pos = Some(pos);
        let _ = events.try_send(InputEvent::PointerAt { x: pos.0, y: pos.1 });
    }
    Ok(())
}

fn handle_event(
    conn: &RustConnection,
    root: u32,
    events: &SyncSender<InputEvent>,
    state: &mut ThreadState,
    event: Event,
) -> Result<(), InputError> {
    match event {
        Event::MotionNotify(ev) if state.exclusive && state.grabbed => {
            let dx = ev.root_x as i32 - state.anchor_x as i32;
            let dy = ev.root_y as i32 - state.anchor_y as i32;
            if dx != 0 || dy != 0 {
                push(events, InputEvent::MouseMove { dx, dy });
                let _ = warp(conn, root, state.anchor_x as i32, state.anchor_y as i32);
            }
        }
        Event::KeyPress(ev) if state.exclusive && state.grabbed => {
            if let Some(key) = keycode_to_key(state, ev.detail) {
                push(events, InputEvent::Key { key, down: true });
            }
        }
        Event::KeyRelease(ev) if state.exclusive && state.grabbed => {
            if let Some(key) = keycode_to_key(state, ev.detail) {
                push(events, InputEvent::Key { key, down: false });
            }
        }
        Event::ButtonPress(ev) if state.exclusive && state.grabbed => {
            emit_button(events, ev.detail, true);
        }
        Event::ButtonRelease(ev) if state.exclusive && state.grabbed => {
            emit_button(events, ev.detail, false);
        }
        Event::XinputRawKeyPress(ev)
            if !state.exclusive && !state.masters.contains(&ev.deviceid) =>
        {
            if let Some(key) = keycode_to_key(state, ev.detail as u8) {
                push(events, InputEvent::Key { key, down: true });
            }
        }
        Event::XinputRawKeyRelease(ev)
            if !state.exclusive && !state.masters.contains(&ev.deviceid) =>
        {
            if let Some(key) = keycode_to_key(state, ev.detail as u8) {
                push(events, InputEvent::Key { key, down: false });
            }
        }
        Event::XinputRawMotion(ev)
            if state.exclusive && !state.grabbed && !state.masters.contains(&ev.deviceid) =>
        {
            if let (Some(dx), Some(dy)) = (
                axis_delta(&ev.valuator_mask, &ev.axisvalues_raw, 0),
                axis_delta(&ev.valuator_mask, &ev.axisvalues_raw, 1),
            ) {
                if dx != 0 || dy != 0 {
                    push(events, InputEvent::MouseMove { dx, dy });
                    let _ = warp(
                        conn,
                        root,
                        state.anchor_x as i32 - dx,
                        state.anchor_y as i32 - dy,
                    );
                }
            }
        }
        Event::XinputRawButtonPress(ev)
            if state.exclusive && !state.grabbed && !state.masters.contains(&ev.deviceid) =>
        {
            emit_button(events, ev.detail as u8, true);
        }
        Event::XinputRawButtonRelease(ev)
            if state.exclusive && !state.grabbed && !state.masters.contains(&ev.deviceid) =>
        {
            emit_button(events, ev.detail as u8, false);
        }
        Event::Error(err) => eprintln!("pairflow: X11 error {err:?}"),
        _ => {}
    }
    Ok(())
}

fn emit_button(events: &SyncSender<InputEvent>, detail: u8, down: bool) {
    match detail {
        1 => push(
            events,
            InputEvent::MouseButton {
                button: MouseButton::Left,
                down,
            },
        ),
        2 => push(
            events,
            InputEvent::MouseButton {
                button: MouseButton::Middle,
                down,
            },
        ),
        3 => push(
            events,
            InputEvent::MouseButton {
                button: MouseButton::Right,
                down,
            },
        ),
        4 if down => push(events, InputEvent::Wheel { dx: 0, dy: 1 }),
        5 if down => push(events, InputEvent::Wheel { dx: 0, dy: -1 }),
        6 if down => push(events, InputEvent::Wheel { dx: -1, dy: 0 }),
        7 if down => push(events, InputEvent::Wheel { dx: 1, dy: 0 }),
        _ => {}
    }
}

fn push(events: &SyncSender<InputEvent>, ev: InputEvent) {
    match events.try_send(ev) {
        Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
    }
}

fn axis_delta(
    mask: &[u32],
    values: &[x11rb::protocol::xinput::Fp3232],
    axis: usize,
) -> Option<i32> {
    let mut idx = 0usize;
    for (word_i, word) in mask.iter().enumerate() {
        for bit in 0..32 {
            if word & (1 << bit) == 0 {
                continue;
            }
            if word_i * 32 + bit == axis {
                return values.get(idx).map(|v| v.integral);
            }
            idx += 1;
        }
    }
    None
}

fn warp(conn: &RustConnection, root: u32, x: i32, y: i32) -> Result<(), InputError> {
    let x = x.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    let y = y.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    conn.warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, x, y)
        .map_err(|e| InputError::Message(e.to_string()))?;
    let _ = conn.flush();
    Ok(())
}

fn inject(conn: &RustConnection, state: &ThreadState, ev: InputEvent) -> Result<(), InputError> {
    match ev {
        InputEvent::MouseMove { dx, dy } => fake_rel(conn, dx, dy)?,
        InputEvent::MouseButton { button, down } => {
            let detail = match button {
                MouseButton::Left => 1,
                MouseButton::Middle => 2,
                MouseButton::Right => 3,
            };
            let kind = if down { BUTTON_PRESS } else { BUTTON_RELEASE };
            fake(conn, kind, detail, 0, 0)?;
        }
        InputEvent::Wheel { dx, dy } => {
            let clicks = dy.clamp(-8, 8);
            let button = if clicks > 0 { 4 } else { 5 };
            for _ in 0..clicks.abs() {
                fake(conn, BUTTON_PRESS, button, 0, 0)?;
                fake(conn, BUTTON_RELEASE, button, 0, 0)?;
            }
            let h = dx.clamp(-8, 8);
            let button = if h > 0 { 7 } else { 6 };
            for _ in 0..h.abs() {
                fake(conn, BUTTON_PRESS, button, 0, 0)?;
                fake(conn, BUTTON_RELEASE, button, 0, 0)?;
            }
        }
        InputEvent::Key { key, down } => {
            if let Some(keycode) = key_to_keycode(state, key) {
                let kind = if down { KEY_PRESS } else { KEY_RELEASE };
                fake(conn, kind, keycode, 0, 0)?;
            }
        }
        InputEvent::PointerAt { .. } => {}
    }
    let _ = conn.flush();
    Ok(())
}

fn fake(conn: &RustConnection, kind: u8, detail: u8, x: i16, y: i16) -> Result<(), InputError> {
    conn.xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, x11rb::NONE, x, y, 0)
        .map_err(|e| InputError::Message(e.to_string()))?;
    Ok(())
}

fn fake_rel(conn: &RustConnection, mut dx: i32, mut dy: i32) -> Result<(), InputError> {
    while dx != 0 || dy != 0 {
        let sx = dx.clamp(-3000, 3000);
        let sy = dy.clamp(-3000, 3000);
        fake(conn, MOTION_NOTIFY, 1, sx as i16, sy as i16)?;
        dx -= sx;
        dy -= sy;
    }
    Ok(())
}

fn keycode_to_key(state: &ThreadState, keycode: u8) -> Option<KeyId> {
    let start = keycode.checked_sub(state.min_keycode)? as usize * state.per;
    let end = (start + state.per).min(state.keysyms.len());
    if start >= state.keysyms.len() {
        return None;
    }
    state.keysyms[start..end]
        .iter()
        .find_map(|ks| keysym_to_key(*ks))
}

fn key_to_keycode(state: &ThreadState, key: KeyId) -> Option<u8> {
    let targets = key_to_keysyms(key);
    let count = state.keysyms.len() / state.per.max(1);
    for index in 0..count {
        let start = index * state.per;
        let chunk = &state.keysyms[start..(start + state.per).min(state.keysyms.len())];
        if chunk.iter().any(|ks| targets.contains(ks)) {
            return u8::try_from(state.min_keycode as usize + index).ok();
        }
    }
    None
}

fn keysym_to_key(ks: u32) -> Option<KeyId> {
    Some(match ks {
        0x0061..=0x007a => KeyId::from_u16((ks - 0x0061) as u16 + KeyId::A.as_u16())?,
        0x0041..=0x005a => KeyId::from_u16((ks - 0x0041) as u16 + KeyId::A.as_u16())?,
        0x0031..=0x0039 => KeyId::from_u16((ks - 0x0031) as u16 + KeyId::Digit1.as_u16())?,
        0x0030 => KeyId::Digit0,
        0xff0d | 0xff8d => KeyId::Enter,
        0xff1b => KeyId::Escape,
        0xff08 => KeyId::Backspace,
        0xff09 => KeyId::Tab,
        0x0020 => KeyId::Space,
        0x002d => KeyId::Minus,
        0x003d => KeyId::Equal,
        0x005b => KeyId::LeftBracket,
        0x005d => KeyId::RightBracket,
        0x005c => KeyId::Backslash,
        0x003b => KeyId::Semicolon,
        0x0027 => KeyId::Apostrophe,
        0x0060 => KeyId::Grave,
        0x002c => KeyId::Comma,
        0x002e => KeyId::Period,
        0x002f => KeyId::Slash,
        0xffe5 => KeyId::CapsLock,
        0xffbe..=0xffc9 => KeyId::from_u16((ks - 0xffbe) as u16 + KeyId::F1.as_u16())?,
        0xff63 => KeyId::Insert,
        0xff50 => KeyId::Home,
        0xff55 => KeyId::PageUp,
        0xffff => KeyId::Delete,
        0xff57 => KeyId::End,
        0xff56 => KeyId::PageDown,
        0xff53 => KeyId::Right,
        0xff51 => KeyId::Left,
        0xff54 => KeyId::Down,
        0xff52 => KeyId::Up,
        0xffe3 => KeyId::LeftControl,
        0xffe4 => KeyId::RightControl,
        0xffe1 => KeyId::LeftShift,
        0xffe2 => KeyId::RightShift,
        0xffe9 => KeyId::LeftAlt,
        0xffea => KeyId::RightAlt,
        0xffeb => KeyId::LeftMeta,
        0xffec => KeyId::RightMeta,
        _ => return None,
    })
}

fn key_to_keysyms(key: KeyId) -> Vec<u32> {
    let one = match key {
        KeyId::A => 0x0061,
        KeyId::B => 0x0062,
        KeyId::C => 0x0063,
        KeyId::D => 0x0064,
        KeyId::E => 0x0065,
        KeyId::F => 0x0066,
        KeyId::G => 0x0067,
        KeyId::H => 0x0068,
        KeyId::I => 0x0069,
        KeyId::J => 0x006a,
        KeyId::K => 0x006b,
        KeyId::L => 0x006c,
        KeyId::M => 0x006d,
        KeyId::N => 0x006e,
        KeyId::O => 0x006f,
        KeyId::P => 0x0070,
        KeyId::Q => 0x0071,
        KeyId::R => 0x0072,
        KeyId::S => 0x0073,
        KeyId::T => 0x0074,
        KeyId::U => 0x0075,
        KeyId::V => 0x0076,
        KeyId::W => 0x0077,
        KeyId::X => 0x0078,
        KeyId::Y => 0x0079,
        KeyId::Z => 0x007a,
        KeyId::Digit1 => 0x0031,
        KeyId::Digit2 => 0x0032,
        KeyId::Digit3 => 0x0033,
        KeyId::Digit4 => 0x0034,
        KeyId::Digit5 => 0x0035,
        KeyId::Digit6 => 0x0036,
        KeyId::Digit7 => 0x0037,
        KeyId::Digit8 => 0x0038,
        KeyId::Digit9 => 0x0039,
        KeyId::Digit0 => 0x0030,
        KeyId::Enter => 0xff0d,
        KeyId::Escape => 0xff1b,
        KeyId::Backspace => 0xff08,
        KeyId::Tab => 0xff09,
        KeyId::Space => 0x0020,
        KeyId::Minus => 0x002d,
        KeyId::Equal => 0x003d,
        KeyId::LeftBracket => 0x005b,
        KeyId::RightBracket => 0x005d,
        KeyId::Backslash => 0x005c,
        KeyId::Semicolon => 0x003b,
        KeyId::Apostrophe => 0x0027,
        KeyId::Grave => 0x0060,
        KeyId::Comma => 0x002c,
        KeyId::Period => 0x002e,
        KeyId::Slash => 0x002f,
        KeyId::CapsLock => 0xffe5,
        KeyId::F1 => 0xffbe,
        KeyId::F2 => 0xffbf,
        KeyId::F3 => 0xffc0,
        KeyId::F4 => 0xffc1,
        KeyId::F5 => 0xffc2,
        KeyId::F6 => 0xffc3,
        KeyId::F7 => 0xffc4,
        KeyId::F8 => 0xffc5,
        KeyId::F9 => 0xffc6,
        KeyId::F10 => 0xffc7,
        KeyId::F11 => 0xffc8,
        KeyId::F12 => 0xffc9,
        KeyId::Insert => 0xff63,
        KeyId::Home => 0xff50,
        KeyId::PageUp => 0xff55,
        KeyId::Delete => 0xffff,
        KeyId::End => 0xff57,
        KeyId::PageDown => 0xff56,
        KeyId::Right => 0xff53,
        KeyId::Left => 0xff51,
        KeyId::Down => 0xff54,
        KeyId::Up => 0xff52,
        KeyId::LeftControl => 0xffe3,
        KeyId::RightControl => 0xffe4,
        KeyId::LeftShift => 0xffe1,
        KeyId::RightShift => 0xffe2,
        KeyId::LeftAlt => 0xffe9,
        KeyId::RightAlt => 0xffea,
        KeyId::LeftMeta => 0xffeb,
        KeyId::RightMeta => 0xffec,
    };
    vec![one]
}
