# Pairflow

Pairflow shares one keyboard and mouse across two computers. One machine is the
**host** (the keyboard and mouse stay there). The other **joins** with a
5-character code. Slide the pointer off the edge of the host screen and it
continues on the peer. `Ctrl+Alt+F12` brings it back.

Windows, macOS, and Linux. Double-click opens a tray app (no terminal).
`pairflow host` and `pairflow join` remain for a terminal. The session is
encrypted. The code is never sent over the network. If a laptop gets a new
address, the same code reconnects. Closing the guest and opening it later
reconnects to that host without typing the code again.

This is an MVP: two computers, one edge, keyboard and mouse. Clipboard, a full
window layout, Wayland, and a public relay are described as follow-up work in
[ARCHITECTURE.md](ARCHITECTURE.md).

## Download

CI builds the installable files. They are not checked into git.

- **GitHub Actions artifacts** on every push and pull request:
  <https://github.com/app4thathq/pairflow/actions>
  Open a successful run and download:
  - `pairflow-windows-x86_64` → `pairflow-windows-x86_64.exe` (tray app) and
    `pairflow-cli.exe` (terminal)
  - `pairflow-macos` → `pairflow-macos.dmg`
  - `pairflow-linux-x86_64` → `pairflow-linux-x86_64.AppImage`
- **Draft GitHub Release** when a `v*` tag is pushed:
  <https://github.com/app4thathq/pairflow/releases>
  The workflow attaches the same files. The release is left as a draft
  so it can be checked before it is published.

The macOS app is unsigned. The Windows file is a portable executable, not an
MSI. The AppImage is built on Ubuntu 22.04. The tray app links the desktop
libraries already present on a normal Ubuntu session (`libdbus-1`, `libX11`,
`libGL`, `libxkbcommon`).

## Pair

Double-click Pairflow. It sits in the tray: the Windows notification area,
the macOS menu bar, or the Linux status icon.

- **Host** starts sharing and shows the 5-character code in the tray and in
  the status window.
- **Join…** opens the window. Type the code from the host. That code is saved.
- **Open** brings the status window back. Closing the window hides it; Pairflow
  keeps running.
- **Disconnect** stops the session and keeps the saved code.
- **Quit** exits.

The next time the guest opens Pairflow, it starts joining that saved code in
the background. The tray says `Reconnecting to K7NQ2…`, then `Paired with …`.
The host keeps advertising the same code, so a new IP address still matches.
Choosing **Join…** with a different code overwrites the saved peer. If the
last session on that machine was Host, opening Pairflow hosts again with the
same code.

From a terminal, the same roles are:

```sh
pairflow host
```

```text
  Pairing code:  K7NQ2

  On the other computer:  pairflow join K7NQ2
```

```sh
pairflow join K7NQ2
```

`pairflow join` with no code reuses the last code this machine joined with.
The Windows console menu (`1` Host, `2` Join, `q` Quit) is only on
`pairflow-cli.exe`, not on the tray executable.

The peer is to the **right** of the host by default. Put it on another edge
with `pairflow host --side left` (also `top` or `bottom`). Move the pointer
through that edge. Keys follow the pointer. `Ctrl+Alt+F12` on the host returns
the pointer and releases modifiers on the peer. `Ctrl+Alt+Enter` or
`Ctrl+Alt+Right` forces the same crossing as pushing through the edge.

The host remembers the code in the per-user state file, so the next host
session shows the same code and a client can reconnect after an IP change.
`pairflow host --new-code` rotates it.

## Guest screens and pointer speed

Once the pointer has crossed, it can reach every pixel of every monitor on
the guest, not only the primary display. Pairflow measures the guest desktop
in the same coordinate space the OS uses for the pointer. On macOS that is
the union of the active displays in Quartz points. A display to the left of
the built-in panel has a negative origin and is included. The join log prints
the rectangle, for example `guest desktop: 4072x1440 at (-2560,0)`.

A typical desk, left to right, is the Windows laptop, the Windows external
screen, the guest's external screen, then the Mac. Leave Windows through the
outer right edge of the Windows desktop. The guest cursor enters on the left
of its own desktop and can move across the external screen onto the Mac.

Remote motion follows the host cursor. Pairflow sums the host's pointer
deltas (on Windows those already include the system's mouse acceleration) and
places the guest cursor with an absolute warp, so a fast swipe is not dropped
and macOS does not clamp the move to a single display. `--sensitivity 1.0`
(the default) keeps that 1:1 travel. `pairflow join --sensitivity 1.5` scales
it and is remembered. The useful range is `0.1` to `8`.

If multicast and broadcast are blocked:

```sh
pairflow join K7NQ2 --direct 192.168.1.20:24816
```

The handshake is the same. Discovery is skipped.

Useful flags:

| Flag | Meaning |
| --- | --- |
| `--side right\|left\|top\|bottom` | Where the other screen sits (host). |
| `--listen 0.0.0.0:24816` | Host TCP address. |
| `--direct HOST:PORT` | Join without mDNS or UDP. |
| `--new-code`, `--code K7NQ2` | Rotate or set the host code. |
| `--dry-run` | Do not capture real devices. Drive the pointer from stdin. |
| `--sensitivity 1.0` | Guest pointer travel relative to the host cursor. Saved. |
| `--debug` | On the host, print the cursor whenever it changes. |
| `--no-mdns`, `--no-udp` | Turn off one discovery channel. |

On the host, stdin can drive the pointer. That is how `--dry-run` works, and
it also works alongside a real display:

`help`, `edge`, `pos X Y`, `move DX DY`, `btn left down`, `key A down`, `quit`.

`quit` stops either role.

Codes use `A–Z` and `2–9` without `I`, `L`, `O`, `0`, or `1`, so they can be
read aloud.

## Permissions and privileges

**macOS.** The disk image is unsigned. Control-click Pairflow, choose Open,
then Open again. The app is a menu-bar extra: no Dock icon and no Terminal
window. Sharing input needs Accessibility permission, and recent macOS
versions also ask for Input Monitoring. Grant both to **Pairflow** (the app),
not to Terminal:

System Settings → Privacy & Security → Accessibility → enable Pairflow.

The command-line binary inside the app is
`Pairflow.app/Contents/MacOS/pairflow-cli`.

**Windows.** Double-click `pairflow-windows-x86_64.exe`. There is no console.
The tray icon is the UI. `pairflow-cli.exe` is the terminal build; starting
that one with incomplete arguments prints the error and waits for Enter
before the window closes. Neither executable needs to run as administrator.
Low-level hooks do not receive input on the secure desktop (the lock screen
and UAC prompts). You may need to allow Pairflow through the firewall for
private networks so TCP `24816` and UDP `24817` work.

The other computer sits on the **outer** edge of the whole Windows desktop.
With two monitors side by side and `--side right`, push the pointer off the
right side of the right-hand monitor. The seam between the two Windows
monitors stays on Windows. The executable's manifest is per-monitor DPI aware,
and the hook thread measures the desktop in that same coordinate space as
`GetCursorPos`. Within 32 pixels of that outer edge, Pairflow claims the
pointer. The host banner prints the version, whether Windows hooks are
actually active, and the desktop rectangle. After pairing it prints the cursor
every 2 seconds (`--debug` prints it as it changes). `edge=true` is the same
test that crosses. The host prints `pointer is on the other computer` when
the crossing succeeds. If capture cannot start, the host stops with an error
instead of quietly ignoring the mouse. If that line appears and the Mac still
does not move, grant Accessibility and Input Monitoring to Pairflow on the
Mac. `Ctrl+Alt+Enter` crosses even when the edge geometry is still wrong.

**Linux.** An X11 session is required (`DISPLAY` must be set). Wayland is not
supported yet; XWayland sometimes works, and grabs there are best-effort. The
XTest extension must be enabled, which it is on ordinary X.Org. If capture
cannot start, the host stops and says why. `--dry-run` is the explicit way to
drive the pointer from stdin. Allow TCP `24816` and UDP `24817` on the local
network. The tray icon uses the desktop StatusNotifier (AppIndicator). If that
service is missing, Pairflow opens the status window instead. Opening the
AppImage with no arguments starts the tray app. `./pairflow-linux-x86_64.AppImage host`
runs the CLI.

## Build from source

Install a recent stable Rust toolchain, then:

The tray app needs a few system libraries on Linux: `pkg-config`,
`libdbus-1-dev`, `libx11-dev`, `libxkbcommon-dev`, `libgl1-mesa-dev`,
`libegl1-mesa-dev`, and `libxcb1-dev`.

```sh
cargo test --workspace
cargo build --release
./target/release/pairflow-gui
./target/release/pairflow host
```

Linux AppImage, after the release build, on x86_64:

```sh
packaging/linux/build-appimage.sh
# dist/pairflow-linux-x86_64.AppImage
```

The script downloads `appimagetool` and extracts it, so FUSE is not required.
The image contains `pairflow-gui` and `pairflow`.

macOS disk image, from a Mac, using universal binaries:

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
mkdir -p dist
lipo -create -output dist/pairflow-gui \
  target/aarch64-apple-darwin/release/pairflow-gui \
  target/x86_64-apple-darwin/release/pairflow-gui
lipo -create -output dist/pairflow \
  target/aarch64-apple-darwin/release/pairflow \
  target/x86_64-apple-darwin/release/pairflow
packaging/macos/build-dmg.sh dist/pairflow-gui dist/pairflow
# dist/pairflow-macos.dmg
```

Windows: `cargo build --release` produces `target\release\pairflow-gui.exe`
(tray, no console) and `target\release\pairflow.exe` (CLI). Embed the DPI
manifest into both (CI does this):

```powershell
packaging/windows/embed-manifest.ps1
```

`pairflow-gui.exe` is the file CI publishes as
`pairflow-windows-x86_64.exe`.

## Layout

```text
apps/pairflow                 tray app (pairflow-gui) and CLI (pairflow)
crates/pairflow-proto         messages and key ids
crates/pairflow-core          pairing, discovery, encryption, edge rules
crates/pairflow-input         X11, Win32, and Quartz backends
packaging/                    AppImage, dmg, and the macOS app bundle
.github/workflows/ci.yml      test, and build the three artifacts
```

State is stored in the OS data directory as `pairflow/state.json`
(`~/.local/share/pairflow/state.json` on Linux,
`~/Library/Application Support/pairflow/state.json` on macOS,
`%APPDATA%\pairflow\state.json` on Windows).

## License

MIT. See [LICENSE](LICENSE).
