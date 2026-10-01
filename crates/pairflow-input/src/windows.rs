//! Win32 low-level hooks for capture and `SendInput` for injection.
//!
//! Low-level hooks do not need an administrator account. They do not see input
//! on the secure desktop (UAC prompts, the lock screen).

use super::{Input, InputError, Platform};
use pairflow_proto::{InputEvent, KeyId, MouseButton};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT,
    VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetCursorPos, GetMessageW, GetSystemMetrics, PostThreadMessageW, SetCursorPos,
    SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT,
    SM_CXSCREEN, SM_CYSCREEN, WH_KEYBOARD_LL, WH_MOUSE_LL,
};

const WM_QUIT: u32 = 0x0012;
const WM_MOUSEMOVE: usize = 0x0200;
const WM_LBUTTONDOWN: usize = 0x0201;
const WM_LBUTTONUP: usize = 0x0202;
const WM_RBUTTONDOWN: usize = 0x0204;
const WM_RBUTTONUP: usize = 0x0205;
const WM_MBUTTONDOWN: usize = 0x0207;
const WM_MBUTTONUP: usize = 0x0208;
const WM_MOUSEWHEEL: usize = 0x020A;
const WM_MOUSEHWHEEL: usize = 0x020E;
const WM_KEYDOWN: usize = 0x0100;
const WM_KEYUP: usize = 0x0101;
const WM_SYSKEYDOWN: usize = 0x0104;
const WM_SYSKEYUP: usize = 0x0105;
const LLMHF_INJECTED: u32 = 0x0000_0001;
const LLKHF_EXTENDED: u32 = 0x01;
const LLKHF_INJECTED: u32 = 0x10;
const LLKHF_UP: u32 = 0x80;

struct HookShared {
    tx: Option<SyncSender<InputEvent>>,
    thread_id: u32,
}

static HOOK: Mutex<HookShared> = Mutex::new(HookShared {
    tx: None,
    thread_id: 0,
});
static EXCLUSIVE: AtomicBool = AtomicBool::new(false);
static ANCHOR_X: AtomicI32 = AtomicI32::new(0);
static ANCHOR_Y: AtomicI32 = AtomicI32::new(0);
static IN_WARP: AtomicBool = AtomicBool::new(false);

pub fn open() -> Result<Input, InputError> {
    let (width, height) = unsafe {
        (
            GetSystemMetrics(SM_CXSCREEN).max(2),
            GetSystemMetrics(SM_CYSCREEN).max(2),
        )
    };
    let (event_tx, event_rx) = sync_channel(1024);
    let thread_tx = event_tx.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let join = thread::spawn(move || hook_thread(thread_tx, ready_tx));
    if ready_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .ok()
        .flatten()
        .is_none()
    {
        return Err(InputError::Message(
            "Windows input hooks did not start".into(),
        ));
    }
    Ok(Input::from_channel(
        width,
        height,
        event_tx,
        event_rx,
        Box::new(WinOps {
            join: std::sync::Mutex::new(Some(join)),
        }),
    ))
}

struct WinOps {
    join: Mutex<Option<JoinHandle<()>>>,
}

impl Platform for WinOps {
    fn inject(&self, ev: InputEvent) {
        if let Err(err) = inject_event(&ev) {
            eprintln!("pairflow: inject: {err}");
        }
    }
    fn warp(&self, x: i32, y: i32) {
        ANCHOR_X.store(x, Ordering::Relaxed);
        ANCHOR_Y.store(y, Ordering::Relaxed);
        IN_WARP.store(true, Ordering::Relaxed);
        unsafe {
            let _ = SetCursorPos(x, y);
        }
        IN_WARP.store(false, Ordering::Relaxed);
    }
    fn set_exclusive(&self, on: bool) {
        if on {
            let mut pt = windows::Win32::Foundation::POINT::default();
            unsafe {
                if GetCursorPos(&mut pt).is_ok() {
                    ANCHOR_X.store(pt.x, Ordering::Relaxed);
                    ANCHOR_Y.store(pt.y, Ordering::Relaxed);
                }
            }
        }
        EXCLUSIVE.store(on, Ordering::Relaxed);
    }
    fn shutdown(&self) {
        let tid = HOOK.lock().unwrap().thread_id;
        if tid != 0 {
            unsafe {
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(join) = self.join.lock().unwrap().take() {
            let _ = join.join();
        }
    }
}

fn hook_thread(tx: SyncSender<InputEvent>, ready: std::sync::mpsc::Sender<Option<String>>) {
    let installed = unsafe { install() };
    let (mouse, key) = match installed {
        Ok(hooks) => hooks,
        Err(err) => {
            let _ = ready.send(Some(err));
            return;
        }
    };
    {
        let mut guard = HOOK.lock().unwrap();
        guard.tx = Some(tx);
        guard.thread_id = unsafe { GetCurrentThreadId() };
    }
    let _ = ready.send(None);
    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&msg);
            windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&msg);
        }
        let _ = UnhookWindowsHookEx(mouse);
        let _ = UnhookWindowsHookEx(key);
        HOOK.lock().unwrap().tx = None;
    }
}

unsafe fn install() -> Result<(HHOOK, HHOOK), String> {
    let module = GetModuleHandleW(None).map_err(|err| err.to_string())?;
    let instance = HINSTANCE(module.0);
    let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), instance, 0)
        .map_err(|err| err.to_string())?;
    let key = SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), instance, 0)
        .map_err(|err| err.to_string())?;
    Ok((mouse, key))
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 || IN_WARP.load(Ordering::Relaxed) {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
    if info.flags & LLMHF_INJECTED != 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let msg = wparam.0;
    if EXCLUSIVE.load(Ordering::Relaxed) {
        match msg {
            WM_MOUSEMOVE => {
                let ax = ANCHOR_X.load(Ordering::Relaxed);
                let ay = ANCHOR_Y.load(Ordering::Relaxed);
                let dx = info.pt.x - ax;
                let dy = info.pt.y - ay;
                if dx != 0 || dy != 0 {
                    emit(InputEvent::MouseMove { dx, dy });
                    IN_WARP.store(true, Ordering::Relaxed);
                    let _ = SetCursorPos(ax, ay);
                    IN_WARP.store(false, Ordering::Relaxed);
                }
            }
            WM_LBUTTONDOWN => emit_button(MouseButton::Left, true),
            WM_LBUTTONUP => emit_button(MouseButton::Left, false),
            WM_RBUTTONDOWN => emit_button(MouseButton::Right, true),
            WM_RBUTTONUP => emit_button(MouseButton::Right, false),
            WM_MBUTTONDOWN => emit_button(MouseButton::Middle, true),
            WM_MBUTTONUP => emit_button(MouseButton::Middle, false),
            WM_MOUSEWHEEL => emit(InputEvent::Wheel {
                dx: 0,
                dy: wheel_notches(info.mouseData),
            }),
            WM_MOUSEHWHEEL => emit(InputEvent::Wheel {
                dx: wheel_notches(info.mouseData),
                dy: 0,
            }),
            _ => {}
        }
        return LRESULT(1);
    }
    if msg == WM_MOUSEMOVE {
        emit(InputEvent::PointerAt {
            x: info.pt.x,
            y: info.pt.y,
        });
    }
    CallNextHookEx(None, code, wparam, lparam)
}

unsafe extern "system" fn key_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
    let flags = info.flags.0;
    if flags & LLKHF_INJECTED != 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let down = match wparam.0 {
        WM_KEYDOWN | WM_SYSKEYDOWN => true,
        WM_KEYUP | WM_SYSKEYUP => false,
        _ => flags & LLKHF_UP == 0,
    };
    if let Some(mut key) = vk_to_key(info.vkCode as u16) {
        if flags & LLKHF_EXTENDED != 0 {
            key = match key {
                KeyId::LeftControl => KeyId::RightControl,
                KeyId::LeftAlt => KeyId::RightAlt,
                other => other,
            };
        }
        let swallow = EXCLUSIVE.load(Ordering::Relaxed);
        emit(InputEvent::Key { key, down });
        if swallow {
            return LRESULT(1);
        }
    } else if EXCLUSIVE.load(Ordering::Relaxed) {
        return LRESULT(1);
    }
    CallNextHookEx(None, code, wparam, lparam)
}

fn wheel_notches(mouse_data: u32) -> i32 {
    let delta = ((mouse_data >> 16) as i16) as i32;
    if delta == 0 {
        0
    } else {
        delta.signum() * (delta.abs() / 120).max(1)
    }
}

fn emit(ev: InputEvent) {
    let guard = HOOK.lock().unwrap();
    if let Some(tx) = &guard.tx {
        match tx.try_send(ev) {
            Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

fn emit_button(button: MouseButton, down: bool) {
    emit(InputEvent::MouseButton { button, down });
}

fn inject_event(ev: &InputEvent) -> Result<(), String> {
    let input = match ev {
        InputEvent::MouseMove { dx, dy } => mouse_input(*dx, *dy, MOUSEEVENTF_MOVE, 0),
        InputEvent::PointerAt { x, y } => {
            let w = unsafe { GetSystemMetrics(SM_CXSCREEN).max(1) };
            let h = unsafe { GetSystemMetrics(SM_CYSCREEN).max(1) };
            let ax = (*x as f32 / w as f32 * 65535.0) as i32;
            let ay = (*y as f32 / h as f32 * 65535.0) as i32;
            mouse_input(ax, ay, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, 0)
        }
        InputEvent::MouseButton { button, down } => {
            let flags = match (button, down) {
                (MouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
                (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
                (MouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
                (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
                (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
                (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
            };
            mouse_input(0, 0, flags, 0)
        }
        InputEvent::Wheel { dx, dy } => {
            if *dy != 0 {
                mouse_input(0, 0, MOUSEEVENTF_WHEEL, (*dy * 120) as u32)
            } else {
                mouse_input(0, 0, MOUSEEVENTF_HWHEEL, (*dx * 120) as u32)
            }
        }
        InputEvent::Key { key, down } => {
            let Some(vk) = key_to_vk(*key) else {
                return Ok(());
            };
            let mut flags = KEYBD_EVENT_FLAGS(0);
            if !down {
                flags |= KEYEVENTF_KEYUP;
            }
            if is_extended(*key) {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(vk),
                        wScan: 0,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            }
        }
    };
    let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
    if sent == 0 {
        return Err("SendInput failed".into());
    }
    Ok(())
}

fn mouse_input(
    dx: i32,
    dy: i32,
    flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS,
    data: u32,
) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn is_extended(key: KeyId) -> bool {
    matches!(
        key,
        KeyId::Right
            | KeyId::Left
            | KeyId::Up
            | KeyId::Down
            | KeyId::Home
            | KeyId::End
            | KeyId::Insert
            | KeyId::Delete
            | KeyId::PageUp
            | KeyId::PageDown
            | KeyId::RightControl
            | KeyId::RightAlt
            | KeyId::RightMeta
    )
}

fn vk_to_key(vk: u16) -> Option<KeyId> {
    Some(match vk {
        0x41..=0x5A => KeyId::from_u16((vk - 0x41) + KeyId::A.as_u16())?,
        0x30 => KeyId::Digit0,
        0x31..=0x39 => KeyId::from_u16((vk - 0x31) + KeyId::Digit1.as_u16())?,
        0x0D => KeyId::Enter,
        0x1B => KeyId::Escape,
        0x08 => KeyId::Backspace,
        0x09 => KeyId::Tab,
        0x20 => KeyId::Space,
        0xBD => KeyId::Minus,
        0xBB => KeyId::Equal,
        0xDB => KeyId::LeftBracket,
        0xDD => KeyId::RightBracket,
        0xDC => KeyId::Backslash,
        0xBA => KeyId::Semicolon,
        0xDE => KeyId::Apostrophe,
        0xC0 => KeyId::Grave,
        0xBC => KeyId::Comma,
        0xBE => KeyId::Period,
        0xBF => KeyId::Slash,
        0x14 => KeyId::CapsLock,
        0x70..=0x7B => KeyId::from_u16((vk - 0x70) + KeyId::F1.as_u16())?,
        0x2D => KeyId::Insert,
        0x24 => KeyId::Home,
        0x21 => KeyId::PageUp,
        0x2E => KeyId::Delete,
        0x23 => KeyId::End,
        0x22 => KeyId::PageDown,
        0x27 => KeyId::Right,
        0x25 => KeyId::Left,
        0x28 => KeyId::Down,
        0x26 => KeyId::Up,
        0x11 | 0xA2 => KeyId::LeftControl,
        0xA3 => KeyId::RightControl,
        0x10 | 0xA0 => KeyId::LeftShift,
        0xA1 => KeyId::RightShift,
        0x12 | 0xA4 => KeyId::LeftAlt,
        0xA5 => KeyId::RightAlt,
        0x5B => KeyId::LeftMeta,
        0x5C => KeyId::RightMeta,
        _ => return None,
    })
}

fn key_to_vk(key: KeyId) -> Option<u16> {
    Some(match key {
        KeyId::A => 0x41,
        KeyId::B => 0x42,
        KeyId::C => 0x43,
        KeyId::D => 0x44,
        KeyId::E => 0x45,
        KeyId::F => 0x46,
        KeyId::G => 0x47,
        KeyId::H => 0x48,
        KeyId::I => 0x49,
        KeyId::J => 0x4A,
        KeyId::K => 0x4B,
        KeyId::L => 0x4C,
        KeyId::M => 0x4D,
        KeyId::N => 0x4E,
        KeyId::O => 0x4F,
        KeyId::P => 0x50,
        KeyId::Q => 0x51,
        KeyId::R => 0x52,
        KeyId::S => 0x53,
        KeyId::T => 0x54,
        KeyId::U => 0x55,
        KeyId::V => 0x56,
        KeyId::W => 0x57,
        KeyId::X => 0x58,
        KeyId::Y => 0x59,
        KeyId::Z => 0x5A,
        KeyId::Digit0 => 0x30,
        KeyId::Digit1 => 0x31,
        KeyId::Digit2 => 0x32,
        KeyId::Digit3 => 0x33,
        KeyId::Digit4 => 0x34,
        KeyId::Digit5 => 0x35,
        KeyId::Digit6 => 0x36,
        KeyId::Digit7 => 0x37,
        KeyId::Digit8 => 0x38,
        KeyId::Digit9 => 0x39,
        KeyId::Enter => 0x0D,
        KeyId::Escape => 0x1B,
        KeyId::Backspace => 0x08,
        KeyId::Tab => 0x09,
        KeyId::Space => 0x20,
        KeyId::Minus => 0xBD,
        KeyId::Equal => 0xBB,
        KeyId::LeftBracket => 0xDB,
        KeyId::RightBracket => 0xDD,
        KeyId::Backslash => 0xDC,
        KeyId::Semicolon => 0xBA,
        KeyId::Apostrophe => 0xDE,
        KeyId::Grave => 0xC0,
        KeyId::Comma => 0xBC,
        KeyId::Period => 0xBE,
        KeyId::Slash => 0xBF,
        KeyId::CapsLock => 0x14,
        KeyId::F1 => 0x70,
        KeyId::F2 => 0x71,
        KeyId::F3 => 0x72,
        KeyId::F4 => 0x73,
        KeyId::F5 => 0x74,
        KeyId::F6 => 0x75,
        KeyId::F7 => 0x76,
        KeyId::F8 => 0x77,
        KeyId::F9 => 0x78,
        KeyId::F10 => 0x79,
        KeyId::F11 => 0x7A,
        KeyId::F12 => 0x7B,
        KeyId::Insert => 0x2D,
        KeyId::Home => 0x24,
        KeyId::PageUp => 0x21,
        KeyId::Delete => 0x2E,
        KeyId::End => 0x23,
        KeyId::PageDown => 0x22,
        KeyId::Right => 0x27,
        KeyId::Left => 0x25,
        KeyId::Down => 0x28,
        KeyId::Up => 0x26,
        KeyId::LeftControl | KeyId::RightControl => 0x11,
        KeyId::LeftShift | KeyId::RightShift => 0x10,
        KeyId::LeftAlt | KeyId::RightAlt => 0x12,
        KeyId::LeftMeta => 0x5B,
        KeyId::RightMeta => 0x5C,
    })
}
