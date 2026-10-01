# Pairflow

Pairflow shares one keyboard and mouse across two computers. One machine is the
**host** (the keyboard and mouse stay there). The other **joins** with a
5-character code. Slide the pointer off the edge of the host screen and it
continues on the peer. `Ctrl+Alt+F12` brings it back.

Windows, macOS, and Linux. The session is encrypted. The code is never sent
over the network. If a laptop gets a new address, the same code reconnects.

This is an MVP: two computers, one edge, keyboard and mouse. Clipboard, a full
window layout, Wayland, and a public relay are described as follow-up work in
[ARCHITECTURE.md](ARCHITECTURE.md).

## Download

CI builds the installable files. They are not checked into git.

- **GitHub Actions artifacts** on every push and pull request:
  <https://github.com/app4thathq/pairflow/actions>
  Open a successful run and download:
  - `pairflow-windows-x86_64` → `pairflow-windows-x86_64.exe`
  - `pairflow-macos` → `pairflow-macos.dmg`
  - `pairflow-linux-x86_64` → `pairflow-linux-x86_64.AppImage`
- **Draft GitHub Release** when a `v*` tag is pushed:
  <https://github.com/app4thathq/pairflow/releases>
  The workflow attaches the same three files. The release is left as a draft
  so it can be checked before it is published.

The macOS app is unsigned. The Windows file is a portable executable, not an
MSI. The AppImage is built on Ubuntu 22.04.

## Pair

On the computer whose keyboard and mouse you want to share:

```sh
pairflow host
```

It prints a code:

```text
  Pairing code:  K7NQ2

  On the other computer:  pairflow join K7NQ2
```

On the other computer:

```sh
pairflow join K7NQ2
```

The peer is to the **right** of the host by default. Put it on another edge
with `pairflow host --side left` (also `top` or `bottom`). Move the pointer
through that edge. Keys follow the pointer. `Ctrl+Alt+F12` on the host returns
the pointer and releases modifiers on the peer.

The host remembers the code in the per-user state file, so the next `pairflow
host` shows the same code and a client can reconnect after an IP change.
`pairflow host --new-code` rotates it. `pairflow join` with no code reuses the
last code that this machine joined with.

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
| `--no-mdns`, `--no-udp` | Turn off one discovery channel. |

On the host, stdin can drive the pointer. That is how `--dry-run` works, and
it also works alongside a real display:

`help`, `edge`, `pos X Y`, `move DX DY`, `btn left down`, `key A down`, `quit`.

`quit` stops either role.

Codes use `A–Z` and `2–9` without `I`, `L`, `O`, `0`, or `1`, so they can be
read aloud.

## Permissions and privileges

**macOS.** The disk image is unsigned. Control-click Pairflow, choose Open,
then Open again. Sharing input needs Accessibility permission, and recent
macOS versions also ask for Input Monitoring:

System Settings → Privacy & Security → Accessibility → enable Pairflow.

The command-line binary inside the app is
`Pairflow.app/Contents/MacOS/pairflow-cli`.

**Windows.** The `.exe` does not need to run as administrator. Low-level hooks
do not receive input on the secure desktop (the lock screen and UAC prompts).
You may need to allow `pairflow.exe` through the firewall for private networks
so TCP `24816` and UDP `24817` work.

**Linux.** An X11 session is required (`DISPLAY` must be set). Wayland is not
supported yet; XWayland sometimes works, and grabs there are best-effort. The
XTest extension must be enabled, which it is on ordinary X.Org. If capture
cannot start, Pairflow says so and falls back to `--dry-run`. Allow TCP
`24816` and UDP `24817` on the local network.

## Build from source

Install a recent stable Rust toolchain, then:

```sh
cargo test --workspace
cargo build --release
./target/release/pairflow host
```

Linux AppImage, after the release build, on x86_64:

```sh
packaging/linux/build-appimage.sh
# dist/pairflow-linux-x86_64.AppImage
```

The script downloads `appimagetool` and extracts it, so FUSE is not required.

macOS disk image, from a Mac, using a universal binary:

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
mkdir -p dist
lipo -create -output dist/pairflow \
  target/aarch64-apple-darwin/release/pairflow \
  target/x86_64-apple-darwin/release/pairflow
packaging/macos/build-dmg.sh dist/pairflow
# dist/pairflow-macos.dmg
```

Windows: `cargo build --release` produces `target\release\pairflow.exe`. Copy
it anywhere and run it. That file is the installable artifact CI uploads.

## Layout

```text
apps/pairflow                 command-line program
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
