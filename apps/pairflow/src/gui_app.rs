//! Tray and status window. Closing the window leaves Pairflow running.

use crate::{run_host, run_join, UiEvent};
use eframe::egui;
use pairflow_core::{Identity, LaunchAction};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

struct Live {
    flag: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

static LIVE: Mutex<Option<Live>> = Mutex::new(None);
static GEN: AtomicU64 = AtomicU64::new(0);

fn spawn_session<F>(task: F)
where
    F: FnOnce(Arc<AtomicBool>) + Send + 'static,
{
    let generation = GEN.fetch_add(1, Ordering::Relaxed) + 1;
    std::thread::spawn(move || {
        let previous = LIVE.lock().unwrap().take();
        if let Some(prev) = previous {
            prev.flag.store(false, Ordering::Relaxed);
            let _ = prev.handle.join();
        }
        if GEN.load(Ordering::Relaxed) != generation {
            return;
        }
        let flag = Arc::new(AtomicBool::new(true));
        let flag_thread = flag.clone();
        let handle = std::thread::spawn(move || task(flag_thread));
        if GEN.load(Ordering::Relaxed) == generation {
            *LIVE.lock().unwrap() = Some(Live { flag, handle });
        }
    });
}

fn disconnect() {
    GEN.fetch_add(1, Ordering::Relaxed);
    if let Some(live) = LIVE.lock().unwrap().take() {
        live.flag.store(false, Ordering::Relaxed);
        std::thread::spawn(move || {
            let _ = live.handle.join();
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrayCmd {
    Show,
    Host,
    Join,
    Disconnect,
    Quit,
}

pub fn run() -> Result<(), String> {
    let (ui_tx, ui_rx) = mpsc::channel();
    crate::set_sink(Some(ui_tx));
    let (tray_tx, tray_rx) = mpsc::channel();
    let tray = Tray::install(tray_tx)?;
    // A saved peer (or a previous host session) starts in the tray. Idle
    // launches show the window so the first code can be entered.
    let resume = Identity::load(false, None)
        .map(|id| !matches!(id.launch_action(), LaunchAction::Idle))
        .unwrap_or(false);
    let app = PairflowApp {
        status: "Starting…".into(),
        detail: String::new(),
        code: String::new(),
        host_code: String::new(),
        hosting: false,
        booted: false,
        ui_rx,
        tray_rx,
        tray,
        scale: 1.0,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([440.0, 360.0])
            .with_min_inner_size([360.0, 280.0])
            .with_visible(!resume),
        ..Default::default()
    };
    eframe::run_native("Pairflow", options, Box::new(|_cc| Ok(Box::new(app))))
        .map_err(|err| err.to_string())
}

struct PairflowApp {
    status: String,
    detail: String,
    code: String,
    host_code: String,
    hosting: bool,
    booted: bool,
    ui_rx: Receiver<UiEvent>,
    tray_rx: Receiver<TrayCmd>,
    tray: Tray,
    scale: f32,
}

impl PairflowApp {
    fn boot(&mut self) {
        self.booted = true;
        match Identity::load(false, None) {
            Ok(id) => {
                self.host_code = id.code.clone();
                self.scale = id.pointer_scale();
                match id.launch_action() {
                    LaunchAction::Host => self.start_host(),
                    LaunchAction::Join(code) => {
                        self.code = code.clone();
                        self.start_join(code);
                    }
                    LaunchAction::Idle => {
                        self.status = "Choose Host or Join".into();
                        self.detail = "A saved join code reconnects by itself next time.".into();
                    }
                }
            }
            Err(err) => self.status = err.to_string(),
        }
        self.refresh_tray();
    }

    fn start_host(&mut self) {
        self.hosting = true;
        self.status = "Hosting…".into();
        self.detail = "Waiting for the other computer.".into();
        self.refresh_tray();
        spawn_session(|running| {
            if let Err(err) = run_host(
                None,
                false,
                "right",
                "0.0.0.0:24816",
                false,
                true,
                true,
                false,
                &running,
            ) {
                crate::feedback::emit(UiEvent::Message(err));
                crate::feedback::emit(UiEvent::Stopped);
            }
        });
    }

    fn start_join(&mut self, code: String) {
        let code = match pairflow_core::normalize_code(&code) {
            Ok(code) => code,
            Err(err) => {
                self.status = err.to_string();
                self.refresh_tray();
                return;
            }
        };
        self.hosting = false;
        self.code = code.clone();
        self.status = format!("Reconnecting to {code}…");
        self.detail.clear();
        self.refresh_tray();
        spawn_session(move |running| {
            if let Err(err) = run_join(Some(&code), None, false, true, true, 4, None, &running) {
                crate::feedback::emit(UiEvent::Message(err));
                crate::feedback::emit(UiEvent::Stopped);
            }
        });
    }

    fn apply_ui(&mut self, ev: UiEvent) {
        match ev {
            UiEvent::Hosting { code } => {
                self.hosting = true;
                self.host_code = code.clone();
                self.status = format!("Hosting {code}");
                self.detail = "Waiting for the other computer.".into();
            }
            UiEvent::Joining { code } | UiEvent::Reconnecting { code } => {
                self.hosting = false;
                self.code = code.clone();
                self.status = format!("Reconnecting to {code}…");
            }
            UiEvent::Paired { peer } => {
                self.status = format!("Paired with {peer}");
                self.detail.clear();
            }
            UiEvent::Waiting => {
                self.detail = "Peer disconnected. Still hosting.".into();
            }
            UiEvent::Message(text) => {
                self.detail = text;
            }
            UiEvent::Stopped => {
                if !self.status.starts_with("Paired") {
                    self.detail = "Session stopped.".into();
                }
            }
        }
        self.refresh_tray();
    }

    fn refresh_tray(&mut self) {
        let code = if self.hosting {
            self.host_code.clone()
        } else {
            String::new()
        };
        self.tray.update(&self.status, &code);
    }

    fn handle_tray(&mut self, ctx: &egui::Context, cmd: TrayCmd) {
        match cmd {
            TrayCmd::Show => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            TrayCmd::Host => self.start_host(),
            TrayCmd::Join => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            TrayCmd::Disconnect => {
                disconnect();
                self.hosting = false;
                self.status = "Disconnected".into();
                self.detail = "The saved code is kept for the next launch.".into();
                self.refresh_tray();
            }
            TrayCmd::Quit => {
                disconnect();
                std::thread::sleep(Duration::from_millis(150));
                std::process::exit(0);
            }
        }
    }
}

impl eframe::App for PairflowApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.booted {
            self.boot();
        }
        while let Ok(ev) = self.ui_rx.try_recv() {
            self.apply_ui(ev);
        }
        while let Ok(cmd) = self.tray_rx.try_recv() {
            self.handle_tray(ctx, cmd);
        }
        if ctx.input(|input| input.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        ctx.request_repaint_after(Duration::from_millis(200));

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Pairflow");
            ui.label(format!(
                "{}  ·  pointer scale {:.2}",
                env!("CARGO_PKG_VERSION"),
                self.scale
            ));
            ui.add_space(8.0);
            ui.heading(&self.status);
            if !self.detail.is_empty() {
                ui.label(&self.detail);
            }
            if self.hosting && !self.host_code.is_empty() {
                ui.add_space(8.0);
                ui.label("Pairing code");
                ui.heading(&self.host_code);
                ui.label("On the other computer, Join and enter this code.");
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Host").clicked() {
                    self.start_host();
                }
                if ui.button("Disconnect").clicked() {
                    self.handle_tray(ctx, TrayCmd::Disconnect);
                }
                if ui.button("Quit").clicked() {
                    self.handle_tray(ctx, TrayCmd::Quit);
                }
            });
            ui.add_space(12.0);
            ui.label("Join a different computer");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.code)
                        .hint_text("5-character code")
                        .desired_width(140.0),
                );
                if ui.button("Join").clicked() {
                    self.start_join(self.code.clone());
                }
            });
            ui.add_space(12.0);
            ui.label("Closing this window keeps Pairflow in the tray. Quit exits.");
            ui.label(
                "The peer sits on the outer right of the Windows desktop. The guest pointer covers every monitor on this computer.",
            );
        });
    }
}

struct Tray {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    icon: tray_icon::TrayIcon,
    #[cfg(target_os = "linux")]
    handle: ksni::Handle<LinuxTray>,
}

impl Tray {
    fn install(tx: Sender<TrayCmd>) -> Result<Self, String> {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            let icon = native_tray(tx, "Pairflow", "")?;
            Ok(Self { icon })
        }
        #[cfg(target_os = "linux")]
        {
            let service = ksni::TrayService::new(LinuxTray {
                tx: tx.clone(),
                status: "Pairflow".into(),
                code: String::new(),
            });
            let handle = service.handle();
            std::thread::spawn(move || {
                if let Err(err) = service.run() {
                    eprintln!("pairflow: tray unavailable ({err}). The window still works.");
                    let _ = tx.send(TrayCmd::Show);
                }
            });
            Ok(Self { handle })
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            let _ = tx;
            Err("no tray backend on this platform".into())
        }
    }

    fn update(&mut self, status: &str, code: &str) {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            if let Ok(menu) = tray_menu(status, code) {
                self.icon.set_menu(Some(Box::new(menu)));
            }
            let _ = self.icon.set_tooltip(Some(format!("Pairflow — {status}")));
        }
        #[cfg(target_os = "linux")]
        {
            let status = status.to_string();
            let code = code.to_string();
            self.handle.update(move |tray| {
                tray.status = status;
                tray.code = code;
            });
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn native_tray(
    tx: Sender<TrayCmd>,
    status: &str,
    code: &str,
) -> Result<tray_icon::TrayIcon, String> {
    let menu = tray_menu(status, code)?;
    let icon = tray_icon::TrayIconBuilder::new()
        .with_tooltip(format!("Pairflow — {status}"))
        .with_menu(Box::new(menu))
        .with_icon(tray_icon_image())
        .build()
        .map_err(|err| err.to_string())?;

    std::thread::spawn(move || {
        let receiver = tray_icon::menu::MenuEvent::receiver();
        while let Ok(event) = receiver.recv() {
            let cmd = match event.id.0.as_str() {
                "show" => TrayCmd::Show,
                "host" => TrayCmd::Host,
                "join" => TrayCmd::Join,
                "disconnect" => TrayCmd::Disconnect,
                "quit" => TrayCmd::Quit,
                _ => continue,
            };
            if tx.send(cmd).is_err() {
                break;
            }
        }
    });
    Ok(icon)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn tray_menu(status: &str, code: &str) -> Result<tray_icon::menu::Menu, String> {
    use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
    let menu = Menu::new();
    let status_item = MenuItem::with_id("status", status, false, None);
    let open = MenuItem::with_id("show", "Open", true, None);
    let host = MenuItem::with_id("host", "Host", true, None);
    let join = MenuItem::with_id("join", "Join…", true, None);
    let disconnect = MenuItem::with_id("disconnect", "Disconnect", true, None);
    let quit = MenuItem::with_id("quit", "Quit", true, None);
    menu.append(&status_item).map_err(|e| e.to_string())?;
    if !code.is_empty() {
        let code_item = MenuItem::with_id("code", format!("Code {code}"), false, None);
        menu.append(&code_item).map_err(|e| e.to_string())?;
    }
    menu.append(&PredefinedMenuItem::separator())
        .map_err(|e| e.to_string())?;
    menu.append(&open).map_err(|e| e.to_string())?;
    menu.append(&host).map_err(|e| e.to_string())?;
    menu.append(&join).map_err(|e| e.to_string())?;
    menu.append(&disconnect).map_err(|e| e.to_string())?;
    menu.append(&quit).map_err(|e| e.to_string())?;
    Ok(menu)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn tray_icon_image() -> tray_icon::Icon {
    let size = 32u32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let border = x < 2 || y < 2 || x >= size - 2 || y >= size - 2;
            let (r, g, b) = if border {
                (20, 90, 160)
            } else {
                (70, 150, 230)
            };
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    tray_icon::Icon::from_rgba(rgba, size, size).expect("icon")
}

#[cfg(target_os = "linux")]
struct LinuxTray {
    tx: Sender<TrayCmd>,
    status: String,
    code: String,
}

#[cfg(target_os = "linux")]
impl ksni::Tray for LinuxTray {
    fn id(&self) -> String {
        "pairflow".into()
    }
    fn title(&self) -> String {
        "Pairflow".into()
    }
    fn icon_name(&self) -> String {
        "input-mouse".into()
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let mut items = vec![StandardItem {
            label: self.status.clone(),
            enabled: false,
            ..Default::default()
        }
        .into()];
        if !self.code.is_empty() {
            items.push(
                StandardItem {
                    label: format!("Code {}", self.code),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Open".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.tx.send(TrayCmd::Show);
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Host".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.tx.send(TrayCmd::Host);
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Join…".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.tx.send(TrayCmd::Join);
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Disconnect".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.tx.send(TrayCmd::Disconnect);
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.tx.send(TrayCmd::Quit);
                }),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}
