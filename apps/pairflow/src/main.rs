//! Pairflow command line.
//!
//! `pairflow host` shows a 5-character code. `pairflow join CODE` on the other
//! computer attaches. With no arguments, a console menu offers the same two
//! roles so a double-clicked Windows executable stays open. Move the pointer
//! off the chosen edge to control the peer. Ctrl+Alt+F12 brings the pointer
//! back. The same code keeps working if an address changes; the client
//! rediscovers the host and handshakes again.

use clap::{Parser, Subcommand};
use pairflow_core::discovery::{browse, machine_name, Advertiser, Announce};
use pairflow_core::session::{connect_authenticated, HostListener, Session};
use pairflow_core::share::{ClientEffect, ClientShare, HostEffect, HostShare};
use pairflow_core::{hex_encode, normalize_code, Identity, Screen};
use pairflow_input::Input;
use pairflow_proto::{InputEvent, KeyId, MouseButton, SecureMsg, Side, DEFAULT_UDP_PORT};
use std::io::{BufRead, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(
    name = "pairflow",
    version,
    about = "Share a keyboard and mouse with a 5-character pairing code"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Own the physical keyboard and mouse. Prints a pairing code.
    Host {
        /// Use this code instead of the saved one.
        #[arg(long)]
        code: Option<String>,
        /// Rotate the saved pairing code.
        #[arg(long)]
        new_code: bool,
        /// Side of this screen where the other computer sits.
        #[arg(long, default_value = "right")]
        side: String,
        /// TCP address to listen on.
        #[arg(long, default_value = "0.0.0.0:24816")]
        listen: String,
        /// Do not capture real devices. Stdin controls the pointer.
        #[arg(long)]
        dry_run: bool,
        /// Skip mDNS advertisement.
        #[arg(long)]
        no_mdns: bool,
        /// Skip UDP broadcast advertisement.
        #[arg(long)]
        no_udp: bool,
    },
    /// Attach to a host that is showing `code`.
    Join {
        /// Five-character code. Omit to reuse the last code this machine saved.
        code: Option<String>,
        /// Skip discovery and connect to HOST:PORT.
        #[arg(long)]
        direct: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        no_mdns: bool,
        #[arg(long)]
        no_udp: bool,
        /// How long to look for the host on each attempt.
        #[arg(long, default_value_t = 4)]
        discover_secs: u64,
    },
}

fn main() {
    let running = Arc::new(AtomicBool::new(true));
    let flag = running.clone();
    let _ = ctrlc::set_handler(move || {
        flag.store(false, Ordering::Relaxed);
    });

    // No subcommand: Explorer (and any other double-click) would otherwise
    // print clap help and exit, and Windows would close the console immediately.
    let (command, from_menu) = if user_args().is_empty() {
        match interactive_menu() {
            Some(command) => (command, true),
            None => return,
        }
    } else {
        match Cli::try_parse() {
            Ok(cli) => (cli.command, false),
            Err(err) => {
                let code = err.exit_code();
                let _ = err.print();
                if code != 0 {
                    pause_windows_console();
                }
                std::process::exit(code);
            }
        }
    };

    let result = dispatch(command, &running);
    if let Err(err) = result {
        eprintln!("pairflow: {err}");
        if from_menu {
            pause_windows_console();
        }
        std::process::exit(1);
    }
}

fn user_args() -> Vec<String> {
    std::env::args().skip(1).collect()
}

fn dispatch(command: Command, running: &Arc<AtomicBool>) -> Result<(), String> {
    match command {
        Command::Host {
            code,
            new_code,
            side,
            listen,
            dry_run,
            no_mdns,
            no_udp,
        } => run_host(
            code.as_deref(),
            new_code,
            &side,
            &listen,
            dry_run,
            !no_mdns,
            !no_udp,
            running,
        ),
        Command::Join {
            code,
            direct,
            dry_run,
            no_mdns,
            no_udp,
            discover_secs,
        } => run_join(
            code.as_deref(),
            direct.as_deref(),
            dry_run,
            !no_mdns,
            !no_udp,
            discover_secs,
            running,
        ),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuChoice {
    Host,
    Join,
    Quit,
    Unknown,
}

fn interpret_menu_choice(line: &str) -> MenuChoice {
    match line.trim().to_ascii_lowercase().as_str() {
        "1" | "h" | "host" => MenuChoice::Host,
        "2" | "j" | "join" => MenuChoice::Join,
        "q" | "quit" | "exit" => MenuChoice::Quit,
        _ => MenuChoice::Unknown,
    }
}

/// `Ok(None)` reuses the saved peer code. `Ok(Some)` is a normalized code.
fn interpret_code_line(line: &str, have_saved: bool) -> Result<Option<String>, String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        if have_saved {
            return Ok(None);
        }
        return Err("enter a 5-character code".into());
    }
    normalize_code(trimmed)
        .map(Some)
        .map_err(|err| err.to_string())
}

fn interactive_menu() -> Option<Command> {
    let saved_peer = Identity::load(false, None)
        .ok()
        .and_then(|identity| identity.peer_code);
    loop {
        println!();
        println!("Pairflow");
        println!();
        println!("  [1] Host — share this keyboard and mouse");
        match &saved_peer {
            Some(code) => {
                println!("  [2] Join — connect with a 5-character code (Enter reuses {code})");
            }
            None => println!("  [2] Join — connect with a 5-character code"),
        }
        println!("  [q] Quit");
        println!();
        match read_menu_line("Choice: ") {
            None => return None,
            Some(line) => match interpret_menu_choice(&line) {
                MenuChoice::Host => return Some(default_host()),
                MenuChoice::Join => {
                    return prompt_join_code(saved_peer.as_deref()).map(default_join);
                }
                MenuChoice::Quit => return None,
                MenuChoice::Unknown => {
                    println!("Enter 1, 2, or q.");
                }
            },
        }
    }
}

fn prompt_join_code(saved: Option<&str>) -> Option<Option<String>> {
    loop {
        let prompt = match saved {
            Some(code) => format!("Pairing code [{code}]: "),
            None => "Pairing code: ".to_string(),
        };
        let Some(line) = read_menu_line(&prompt) else {
            return None;
        };
        match interpret_code_line(&line, saved.is_some()) {
            Ok(code) => return Some(code),
            Err(err) => println!("pairflow: {err}"),
        }
    }
}

fn read_menu_line(prompt: &str) -> Option<String> {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line),
    }
}

fn default_host() -> Command {
    Command::Host {
        code: None,
        new_code: false,
        side: "right".to_string(),
        listen: "0.0.0.0:24816".to_string(),
        dry_run: false,
        no_mdns: false,
        no_udp: false,
    }
}

fn default_join(code: Option<String>) -> Command {
    Command::Join {
        code,
        direct: None,
        dry_run: false,
        no_mdns: false,
        no_udp: false,
        discover_secs: 4,
    }
}

/// Keep a Windows console open long enough to read a clap usage error.
/// Piped stdin does not wait, so scripts that pass arguments stay unchanged.
fn pause_windows_console() {
    #[cfg(windows)]
    {
        use std::io::IsTerminal;
        if std::io::stdin().is_terminal() {
            eprintln!("Press Enter to exit");
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
        }
    }
}

fn run_host(
    explicit: Option<&str>,
    new_code: bool,
    side_name: &str,
    listen: &str,
    dry_run: bool,
    mdns: bool,
    udp: bool,
    running: &Arc<AtomicBool>,
) -> Result<(), String> {
    let side = Side::parse(side_name).ok_or_else(|| format!("unknown side '{side_name}'"))?;
    let identity = Identity::load(new_code, explicit).map_err(|e| e.to_string())?;
    let addr: SocketAddr = listen.parse().map_err(|e| format!("listen address: {e}"))?;
    let input = Input::open(dry_run);
    let screen = Screen::with_origin(input.origin_x, input.origin_y, input.width, input.height);
    spawn_stdin(input.emitter(), screen, Some(side), running.clone());
    let listener = HostListener::bind(
        addr,
        identity.code.clone(),
        identity.host_id,
        (screen.width as u16, screen.height as u16),
        machine_name(),
    )
    .map_err(|e| e.to_string())?;
    let bound = listener.local_addr().map_err(|e| e.to_string())?;
    let _advertiser = Advertiser::start(
        Announce {
            host_id: identity.host_id,
            port: bound.port(),
            name: machine_name(),
        },
        DEFAULT_UDP_PORT,
        mdns,
        udp,
    );
    print_host_banner(&identity.code, side, bound.port(), screen);
    while running.load(Ordering::Relaxed) {
        let Some((session, peer)) = listener
            .accept_authenticated(running)
            .map_err(|e| e.to_string())?
        else {
            break;
        };
        println!("paired with {} ({peer})", session.peer_name);
        println!(
            "move the pointer to the {} edge. Ctrl+Alt+F12 returns.",
            side.name()
        );
        let mut share = HostShare::new(screen, side);
        if let Err(err) = host_session(session, &input, &mut share, running) {
            eprintln!("pairflow: session ended: {err}");
        }
        input.set_exclusive(false);
        if running.load(Ordering::Relaxed) {
            println!(
                "peer disconnected. Waiting for the same code {}.",
                identity.code
            );
        }
    }
    Ok(())
}

fn run_join(
    explicit: Option<&str>,
    direct: Option<&str>,
    dry_run: bool,
    mdns: bool,
    udp: bool,
    discover_secs: u64,
    running: &Arc<AtomicBool>,
) -> Result<(), String> {
    let mut identity = Identity::load(false, None).map_err(|e| e.to_string())?;
    let code = if let Some(code) = explicit {
        let code = normalize_code(code).map_err(|e| e.to_string())?;
        identity
            .remember_peer_code(&code)
            .map_err(|e| e.to_string())?;
        code
    } else if let Some(code) = identity.peer_code.clone() {
        code
    } else {
        return Err("pass a 5-character code, for example: pairflow join K7NQ2".into());
    };
    let input = Input::open(dry_run);
    let screen = Screen::with_origin(input.origin_x, input.origin_y, input.width, input.height);
    spawn_stdin(input.emitter(), screen, None, running.clone());
    println!("joining with code {code}");
    if direct.is_none() {
        println!("looking for a host on the local network (mDNS and UDP). Ctrl+C quits.");
    }
    let mut backoff = Duration::from_millis(400);
    while running.load(Ordering::Relaxed) {
        let addr = if let Some(direct) = direct {
            direct.parse().map_err(|e| format!("--direct: {e}"))?
        } else {
            match find_host(discover_secs, mdns, udp, identity.peer_host_id) {
                Some(candidate) => candidate,
                None => {
                    println!("no host found yet. Still looking for code {code}...");
                    thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                    continue;
                }
            }
        };
        println!("connecting to {addr}");
        match connect_authenticated(addr, &code, &machine_name(), Duration::from_secs(4)) {
            Ok(session) => {
                println!(
                    "paired with {} (id {})",
                    session.peer_name,
                    hex_encode(&session.peer_host_id)
                );
                identity
                    .remember_peer(session.peer_host_id)
                    .map_err(|e| e.to_string())?;
                let mut share = ClientShare::new(screen);
                if let Err(err) = client_session(session, &input, &mut share, running) {
                    eprintln!("pairflow: session ended: {err}");
                }
                backoff = Duration::from_millis(400);
                if running.load(Ordering::Relaxed) {
                    println!("reconnecting with code {code}...");
                }
            }
            Err(err) => {
                eprintln!("pairflow: {err}");
                thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        }
    }
    Ok(())
}

fn find_host(secs: u64, mdns: bool, udp: bool, prefer: Option<[u8; 16]>) -> Option<SocketAddr> {
    let found = browse(
        Duration::from_secs(secs.max(1)),
        DEFAULT_UDP_PORT,
        mdns,
        udp,
        prefer,
    );
    found.first().map(|c| c.addr)
}

fn host_session(
    mut session: Session,
    input: &Input,
    share: &mut HostShare,
    running: &AtomicBool,
) -> Result<(), String> {
    let mut last_hb = Instant::now();
    let mut last_rx = Instant::now();
    while running.load(Ordering::Relaxed) {
        if last_hb.elapsed() >= Duration::from_secs(2) {
            session
                .send(&SecureMsg::Heartbeat { unix_ms: unix_ms() })
                .map_err(|e| e.to_string())?;
            last_hb = Instant::now();
        }
        while let Some(ev) = input.try_recv() {
            let was = share.remote;
            for effect in share.on_input(ev) {
                apply_host(&mut session, input, effect)?;
            }
            note_focus(was, share.remote);
        }
        match session
            .recv_timeout(Duration::from_millis(20))
            .map_err(|e| e.to_string())?
        {
            Some(SecureMsg::Heartbeat { .. }) => last_rx = Instant::now(),
            Some(msg) => {
                last_rx = Instant::now();
                let was = share.remote;
                let bye = matches!(msg, SecureMsg::Bye);
                for effect in share.on_net(msg) {
                    apply_host(&mut session, input, effect)?;
                }
                note_focus(was, share.remote);
                if bye {
                    break;
                }
            }
            None => {
                if last_rx.elapsed() > Duration::from_secs(8) {
                    return Err("heartbeat timed out".into());
                }
            }
        }
    }
    let _ = session.send(&SecureMsg::Bye);
    Ok(())
}

fn apply_host(session: &mut Session, input: &Input, effect: HostEffect) -> Result<(), String> {
    match effect {
        HostEffect::Send(msg) => session.send(&msg).map_err(|e| e.to_string()),
        HostEffect::SetExclusive(on) => {
            input.set_exclusive(on);
            Ok(())
        }
        HostEffect::Warp { x, y } => {
            input.warp(x, y);
            Ok(())
        }
    }
}

fn client_session(
    mut session: Session,
    input: &Input,
    share: &mut ClientShare,
    running: &AtomicBool,
) -> Result<(), String> {
    let mut last_hb = Instant::now();
    let mut last_rx = Instant::now();
    while running.load(Ordering::Relaxed) {
        if last_hb.elapsed() >= Duration::from_secs(2) {
            session
                .send(&SecureMsg::Heartbeat { unix_ms: unix_ms() })
                .map_err(|e| e.to_string())?;
            last_hb = Instant::now();
        }
        match session
            .recv_timeout(Duration::from_millis(200))
            .map_err(|e| e.to_string())?
        {
            Some(SecureMsg::Heartbeat { .. }) => last_rx = Instant::now(),
            Some(msg) => {
                last_rx = Instant::now();
                let was = share.active();
                let bye = matches!(msg, SecureMsg::Bye);
                for effect in share.on_msg(msg) {
                    apply_client(&mut session, input, effect)?;
                }
                if was != share.active() {
                    if share.active() {
                        println!("pointer is on this computer");
                    } else {
                        println!("pointer returned to the other computer");
                    }
                }
                if bye {
                    break;
                }
            }
            None => {
                if last_rx.elapsed() > Duration::from_secs(8) {
                    return Err("heartbeat timed out".into());
                }
            }
        }
    }
    let _ = session.send(&SecureMsg::Bye);
    Ok(())
}

fn apply_client(session: &mut Session, input: &Input, effect: ClientEffect) -> Result<(), String> {
    match effect {
        ClientEffect::Send(msg) => session.send(&msg).map_err(|e| e.to_string()),
        ClientEffect::Inject(ev) => {
            input.inject(ev);
            Ok(())
        }
        ClientEffect::Warp { x, y } => {
            input.warp(x, y);
            Ok(())
        }
    }
}

fn note_focus(was: bool, now: bool) {
    if was == now {
        return;
    }
    if now {
        println!("pointer is on the other computer");
    } else {
        println!("pointer is local");
    }
}

fn print_host_banner(code: &str, side: Side, port: u16, screen: Screen) {
    println!();
    println!("  Pairing code:  {code}");
    println!();
    println!("  On the other computer:  pairflow join {code}");
    println!(
        "  Desktop: {}x{} at ({},{}). Peer is the outer {} edge.",
        screen.width,
        screen.height,
        screen.x,
        screen.y,
        side.name()
    );
    println!("  Listening on TCP {port} (UDP discovery {DEFAULT_UDP_PORT}).");
    println!("  Release hotkey: Ctrl+Alt+F12");
    println!("  Stdin: help, edge, pos X Y, move DX DY, key NAME down|up, quit");
    println!();
    let _ = std::io::stdout().flush();
}

fn spawn_stdin(
    tx: SyncSender<InputEvent>,
    screen: Screen,
    side: Option<Side>,
    running: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            if !handle_line(&tx, screen, side, &running, line.trim()) {
                break;
            }
        }
    });
}

fn handle_line(
    tx: &SyncSender<InputEvent>,
    screen: Screen,
    default_side: Option<Side>,
    running: &AtomicBool,
    line: &str,
) -> bool {
    if line.is_empty() {
        return true;
    }
    let mut parts = line.split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let send = |ev: InputEvent| match tx.try_send(ev) {
        Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
    };
    match cmd {
        "quit" | "exit" => {
            running.store(false, Ordering::Relaxed);
            false
        }
        "help" => {
            println!("commands: edge [left|right|top|bottom] | pos X Y | move DX DY | btn left|right|middle down|up | key NAME down|up | quit");
            true
        }
        "edge" => {
            let side = parts
                .next()
                .and_then(Side::parse)
                .or(default_side)
                .unwrap_or(Side::Right);
            let (x, y) = match side {
                Side::Right => (screen.right() - 1, screen.y + screen.height / 2),
                Side::Left => (screen.x, screen.y + screen.height / 2),
                Side::Top => (screen.x + screen.width / 2, screen.y),
                Side::Bottom => (screen.x + screen.width / 2, screen.bottom() - 1),
            };
            send(InputEvent::PointerAt { x, y });
            true
        }
        "pos" => match (parse_i32(parts.next()), parse_i32(parts.next())) {
            (Some(x), Some(y)) => {
                send(InputEvent::PointerAt { x, y });
                true
            }
            _ => {
                eprintln!("usage: pos X Y");
                true
            }
        },
        "move" => match (parse_i32(parts.next()), parse_i32(parts.next())) {
            (Some(dx), Some(dy)) => {
                send(InputEvent::MouseMove { dx, dy });
                true
            }
            _ => {
                eprintln!("usage: move DX DY");
                true
            }
        },
        "btn" => {
            let button = match parts.next().unwrap_or("") {
                "left" | "l" => Some(MouseButton::Left),
                "right" | "r" => Some(MouseButton::Right),
                "middle" | "m" => Some(MouseButton::Middle),
                _ => None,
            };
            let down = match parts.next().unwrap_or("") {
                "down" | "d" | "1" => Some(true),
                "up" | "u" | "0" => Some(false),
                _ => None,
            };
            if let (Some(button), Some(down)) = (button, down) {
                send(InputEvent::MouseButton { button, down });
            } else {
                eprintln!("usage: btn left|right|middle down|up");
            }
            true
        }
        "key" => {
            let name = parts.next().unwrap_or("");
            let down = match parts.next().unwrap_or("") {
                "down" | "d" | "1" => Some(true),
                "up" | "u" | "0" => Some(false),
                _ => None,
            };
            match (KeyId::parse(name), down) {
                (Some(key), Some(down)) => send(InputEvent::Key { key, down }),
                _ => eprintln!("usage: key NAME down|up"),
            }
            true
        }
        other => {
            eprintln!("unknown command '{other}'. Type help.");
            true
        }
    }
}

fn parse_i32(text: Option<&str>) -> Option<i32> {
    text?.parse().ok()
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_accepts_number_or_word() {
        assert_eq!(interpret_menu_choice("1"), MenuChoice::Host);
        assert_eq!(interpret_menu_choice(" Host "), MenuChoice::Host);
        assert_eq!(interpret_menu_choice("2"), MenuChoice::Join);
        assert_eq!(interpret_menu_choice("j"), MenuChoice::Join);
        assert_eq!(interpret_menu_choice("q"), MenuChoice::Quit);
        assert_eq!(interpret_menu_choice("quit"), MenuChoice::Quit);
        assert_eq!(interpret_menu_choice(""), MenuChoice::Unknown);
        assert_eq!(interpret_menu_choice("help"), MenuChoice::Unknown);
    }

    #[test]
    fn join_prompt_reuses_saved_code_or_normalizes() {
        assert_eq!(interpret_code_line("", true).unwrap(), None);
        assert!(interpret_code_line("", false).is_err());
        assert_eq!(
            interpret_code_line("k7nq2", false).unwrap().as_deref(),
            Some("K7NQ2")
        );
        assert!(interpret_code_line("ABCDO", true).is_err());
    }

    #[test]
    fn menu_defaults_match_the_cli() {
        match default_host() {
            Command::Host {
                code,
                new_code,
                side,
                listen,
                dry_run,
                no_mdns,
                no_udp,
            } => {
                assert!(code.is_none());
                assert!(!new_code);
                assert_eq!(side, "right");
                assert_eq!(listen, "0.0.0.0:24816");
                assert!(!dry_run);
                assert!(!no_mdns && !no_udp);
            }
            Command::Join { .. } => panic!("host"),
        }
        match default_join(None) {
            Command::Join {
                code,
                direct,
                discover_secs,
                dry_run,
                ..
            } => {
                assert!(code.is_none());
                assert!(direct.is_none());
                assert_eq!(discover_secs, 4);
                assert!(!dry_run);
            }
            Command::Host { .. } => panic!("join"),
        }
    }
}
