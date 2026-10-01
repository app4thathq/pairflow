# Pairflow architecture

Pairflow shares one physical keyboard and mouse between two computers. It is a
small, from-scratch cousin of Barrier and Deskflow: a host owns the devices, a
client injects what the host forwards, and the two machines find each other
with a short pairing code instead of an IP address.

This document describes the MVP that is in the repository and the pieces that
are deliberately not built yet.

## MVP and roadmap

**In this version**

- Two roles: `host` (physical keyboard and mouse) and `join` (the other machine).
- One peer. The peer sits on one edge of the host screen: left, right, top, or bottom.
- Relative mouse motion, buttons, wheel, and a practical set of keys.
- Pairing with a 5-character code. The code is not sent on the wire.
- Discovery by mDNS and by UDP broadcast. Neither advertisement contains the code.
- An encrypted TCP session (X25519, HKDF, ChaCha20-Poly1305).
- Reconnect when the TCP session drops. The host keeps the same code and a stable
  host id, so a new DHCP address does not force a new pairing.
- Linux (X11), Windows, and macOS input backends.
- Installable artifacts: a Windows `.exe`, a macOS `.dmg` containing `Pairflow.app`,
  and a Linux AppImage. CI builds all three.

**Not in this version**

- More than two computers, or a free-form screen grid.
- Clipboard, drag-and-drop, and file transfer.
- Native Wayland capture (see below). XWayland can work, with weaker grabs.
- Choosing which physical monitor borders the peer. The peer is the outer
  edge of the whole desktop. Mixed-DPI gaps inside that rectangle are not
  modeled.
- A full GUI. The macOS app opens Terminal; other platforms are a command-line
  program with a status log.
- The optional relay. The design is below; there is no server.
- Code signing, notarization, and a Windows MSI. The Windows build is a portable
  executable. The macOS build is unsigned.

## Components

```
apps/pairflow            CLI: host and join loops, stdin controls
crates/pairflow-input    capture and injection per OS
crates/pairflow-core     codes, discovery, handshake, share state machine
crates/pairflow-proto    message layout and logical key ids
```

`pairflow-core` does not link a window system. The edge-crossing rules live in
`share` and `gate` and are unit-tested without devices. `pairflow-input` turns
OS events into `InputEvent` values and injects the same enum. The binary is the
only place that owns both a socket and an input backend.

```
                 host machine                         peer machine
        +---------------------------+         +---------------------------+
        | pairflow host             |         | pairflow join CODE        |
        |  input capture            |  TCP    |  input inject             |
        |  edge state machine       | <-----> |  edge state machine       |
        |  mDNS + UDP advertise     |         |  mDNS + UDP browse        |
        +---------------------------+         +---------------------------+
```

## Pairing flow

1. The host loads or creates a 16-byte `host_id` and a 5-character code. Both
   are stored in the per-user state file (`state.json` under the OS data
   directory, mode `0600` on Unix). `--new-code` rotates the code. `--code`
   sets it. Restarting the host without those flags keeps the same code, which
   is what makes a later reconnect possible.
2. The host listens on TCP `24816` (override with `--listen`) and advertises
   `host_id`, TCP port, and a display name. It does not advertise the code.
3. The client browses for a few seconds. Candidates whose `host_id` matches the
   pinned peer are tried first. `--direct HOST:PORT` skips discovery.
4. The client opens TCP and runs the handshake below, using the code only as
   key material. A wrong code fails the MAC. The client then stores the code
   separately from its own host code, and pins the server's `host_id`.
5. Later, if the socket dies, the client browses again and repeats the
   handshake with the saved code. A changed IP is fine. A rotated code is not:
   the client must be given the new code.

The alphabet is `ABCDEFGHJKLMNPQRSTUVWXYZ23456789` (32 symbols: no `0/O/1/I/L`).
`32^5 = 33,554,432`, about 25 bits. See the threat notes for what that means.

## Discovery

Two independent channels, both optional (`--no-mdns`, `--no-udp`):

| Channel | What is published | Port |
| --- | --- | --- |
| mDNS `_pairflow._tcp.local.` | instance name, port, TXT `id` (hex host id) and `v=1` | 5353 |
| UDP announce, magic `PFD1` | host id, TCP port, display name | 24817 |

Announces go to `255.255.255.255` and to each interface's directed broadcast.
The listener uses `SO_REUSEADDR` so more than one process can bind the UDP
port. mDNS uses the `mdns-sd` crate, which speaks the protocol itself and does
not require Avahi or Bonjour to be installed. If mDNS cannot join the multicast
group, UDP discovery still runs.

A passive observer learns that a Pairflow host exists, its name, its address,
and its host id. They do not learn the code. They can still attempt the
handshake; the host rate-limits that.

## Transport

The session is one TCP connection with `TCP_NODELAY`. TCP is the right default
for keys: a dropped key-up sticks a modifier, and Barrier made the same choice.
Mouse moves are small and are sent as they happen. The host coalesces nothing
beyond what the OS already did.

Frames are length-prefixed (`u32` little-endian length, then a type byte and a
body), capped at 64 KiB.

Handshake messages are cleartext:

| Type | Body |
| --- | --- |
| ClientHello | version, X25519 public key, display name |
| ServerHello | version, X25519 public key, host id, screen size, display name |
| ClientAuth | 32-byte MAC |
| ServerAuth | 32-byte MAC |
| AuthReject | reason: version, MAC, or rate limit |

After both MACs verify, every application message is a frame of type `0xE1`:
an 8-byte counter and ChaCha20-Poly1305 ciphertext. Each direction has its own
key and its own counter, starting at 1. The receiver accepts only
`last + 1`, so reordering or replay fails closed. Heartbeats every 2 seconds;
silence for 8 seconds drops the session.

Application messages: heartbeat, relative mouse move, button, wheel (positive
`dy` is scroll up), key down/up, enter-screen, leave-screen, bye.

Logical keys use USB HID Keyboard usages (`KeyId` in `pairflow-proto`). `A` is
usage 4 on every platform. Each backend maps that id to a local keycode, scan
code, or virtual key. Letters follow the host's keysym or virtual key, so
typing tracks the character more than the physical position. Modifiers,
arrows, function keys, and the common punctuation keys are mapped explicitly.
Unmapped keys are dropped.

## Cryptography

This is not TLS. It is a short Noise-like pattern:

1. Each side generates an ephemeral X25519 key.
2. The shared secret is the HKDF input. The pairing code is the salt.
3. HKDF-SHA256 expands `pairflow-c2s`, `pairflow-s2c`, and `pairflow-auth`.
4. The MAC key authenticates a transcript:
   `b"pairflow-v1" || client_pub || server_pub || host_id`.
5. The client sends `HMAC(auth, "client" || transcript)`. The server checks it
   in constant time and replies with `HMAC(auth, "server" || transcript)`.

A wrong code produces a different shared key schedule, so the MAC does not
match and no traffic key is confirmed. The code never appears in a handshake
field. Ephemeral X25519 means a recorded session cannot be decrypted later if
the code leaks, and it cannot be replayed.

The host answers at most 8 failed handshakes per source address per minute,
then waits before rejecting. Failed attempts also sleep briefly so an online
guess is not free.

## Share model

The host polls (or receives) the pointer in screen coordinates, y downward.
The desktop rectangle is every attached monitor, not only the primary, and it
may start at a negative origin. `--side right` means the peer is past the
outer right edge of that rectangle (not the seam between two local monitors).
When the pointer sits in the outer 2 pixels of that edge, or a relative move
from the last known position crosses it, the host:

1. Tells the input backend to capture exclusively (grab or swallow).
2. Sends `Enter` with the opposite edge and a fraction `0..=10000` along it.
3. Replays currently held modifiers so shortcuts survive the crossing.

While the pointer is remote, relative motion, buttons, wheel, and keys are
forwarded. The client warps its cursor to the entry point and injects clamped
deltas. If a delta would cross back over the entry edge, the client injects
only the part that stays on screen and sends `Leave` with the new fraction.
The host drops the grab and warps its cursor 24 pixels inside the same edge.

`Ctrl+Alt+F12` on the host forces the same return and releases modifiers on
the client so they do not stick. Stdin accepts the same controls for dry-run
and for machines where a grab is unavailable: `edge`, `pos`, `move`, `btn`,
`key`, `quit`.

The client does not grab its own physical mouse in this MVP. That is acceptable
because the person is touching the host's mouse. A second person at the client
keyboard can still type locally and fight the injected stream.

## Input capture and injection

### Linux (X11)

`x11rb`'s pure-Rust connection talks to the X server on `DISPLAY`. There is no
libX11 dependency, which keeps the AppImage small.

- Screen size comes from the connection setup.
- While the pointer is local, the backend polls `QueryPointer` and selects
  XInput2 raw key events so the hotkey works without a grab.
- On entering the peer, it `GrabPointer` and `GrabKeyboard` on the root window
  (async, not owner-events). Motion events are turned into deltas from an
  anchor, then `WarpPointer` puts the cursor back so the local pointer does not
  drift. If the grab is refused, raw motion is forwarded but not swallowed.
- Injection uses the XTest extension: relative motion, buttons, and keycodes
  found from the current keymap. Wheel notches are button 4/5 and 6/7.

**Wayland.** A normal client cannot grab the compositor's pointer. This MVP
does not speak the libei or InputCapture portal protocols. On a Wayland
session, set `DISPLAY` to an XWayland server if one is available; grabs there
are best-effort and some compositors will not honor them. Otherwise Pairflow
prints the error and falls back to dry-run stdin. Native Wayland is roadmap.

### Windows

A background thread installs `WH_MOUSE_LL` and `WH_KEYBOARD_LL` and pumps
messages. Low-level hooks do **not** require an administrator account. They do
not see input on the secure desktop (the lock screen, UAC prompts).

- Before any metric or hook call, the process enables per-monitor DPI
  awareness (falling back to system DPI awareness). `WH_MOUSE_LL` positions
  are physical pixels; `GetSystemMetrics(SM_CXSCREEN)` on a DPI-unaware
  process is not. v0.1.1 compared those two spaces, so a cursor pushed to the
  right edge often never satisfied the edge test.
- The desktop comes from `SM_XVIRTUALSCREEN` / `SM_CXVIRTUALSCREEN` (and the
  Y pair), so a second monitor is inside the rectangle and the peer is the
  outer edge.
- Local mode publishes the latest cursor position (hook and an 8 ms
  `GetCursorPos` poll). The main thread reads that sample directly, so a full
  event queue cannot drop the position that sits on the edge.
- Remote mode swallows the event (the hook returns 1), forwards the delta from
  an anchor, and `SetCursorPos`s back. A re-entry flag ignores the warp.
- Events with the injected flag are ignored so the client's own `SendInput`
  is not captured again.
- Injection is `SendInput`: relative `MOUSEEVENTF_MOVE`, button flags, wheel
  data in `WHEEL_DELTA` units, and virtual keys. Extended keys (arrows, right
  Ctrl, and so on) set `KEYEVENTF_EXTENDEDKEY`.

The artifact is `pairflow-windows-x86_64.exe`, a portable executable. No MSI.

### macOS

A `CGEventTap` at the HID insertion point watches mouse, key, scroll, and
flag-changed events. Returning `CallbackResult::Drop` swallows them while the
pointer is remote. `CGDisplay::warp_mouse_cursor_position` parks the cursor
without generating a new event. Injection posts `CGEvent`s: absolute mouse
position, buttons, line-based scroll, and virtual keycodes.

**Permission.** `CGEventTapCreate` fails unless the process is trusted for
Accessibility. An `.app` bundle is what System Settings lists. The dmg's
"How to open" file says to Control-click → Open because the build is unsigned,
then enable Pairflow under Privacy & Security → Accessibility. Recent macOS
versions may also ask for Input Monitoring. There is no notarization and no
Developer ID signature in CI.

Modifier keys on macOS arrive as `FlagsChanged` rather than key up/down. The
backend diffs the flag word and emits the logical modifier keys.

The dmg contains a universal binary (`lipo` of `aarch64-apple-darwin` and
`x86_64-apple-darwin`). Double-click runs an AppleScript prompt and then the
CLI in Terminal. `Pairflow.app/Contents/MacOS/pairflow-cli` is the same binary
for shell use.

## Reconnect model

| Event | What happens |
| --- | --- |
| TCP drop, same code, same host id, new IP | Client browses, handshakes again, resumes. |
| Host process restart, code not rotated | Same. The code and host id were loaded from disk. |
| `--new-code` or a different `--code` | Old clients fail the MAC until they are told the new code. |
| Different machine reuses a stale host id | Not possible unless someone copies the state file. The host id is the pin. |
| Client restart | `pairflow join` with no argument reuses the saved peer code and prefers the pinned host id. |

There is no long-lived reconnect token in the ciphertext. Re-authentication is
a full handshake, which is cheap and does not depend on the old socket. Backoff
between attempts caps at 5 seconds.

## Security and threats

The pairing code is a convenience secret for a local network, not a password
for the public internet. Do not forward TCP 24816 or UDP 24817 to the internet.

| Threat | Outcome in this MVP |
| --- | --- |
| Passive LAN sniff | Sees discovery metadata and ciphertext. Does not see the code or events. |
| Active wrong code | MAC failure. No session. Rate limit slows guessing. |
| Online brute force of 25-bit code | Possible for a patient attacker who can open many connections from many addresses. Rotate the code if the LAN is hostile. A longer code is roadmap. |
| Stolen state file | The thief has the code and the host id. Treat the file like a password. |
| Recorded session, code leaked later | Ephemeral X25519. Past traffic stays confidential. |
| Replay of an old frame | Counter must be exactly `last + 1`. |
| Host replaced by an impostor at a new IP | Client pins `host_id`. A new id does not match the saved pin and is tried only after pinned candidates. A copied state file defeats the pin. |
| Malicious client after a good handshake | Can inject anything the host forwards, which is the point of the product. There is no per-event authorization beyond the session. |
| Code in mDNS or broadcast | Not included. A fast hash of the code would be brute-forced offline, so discovery carries only the random host id. |

The MAC compare walks all 32 bytes. HKDF and ChaCha20-Poly1305 come from the
RustCrypto crates. There is no certificate authority and no TOFU prompt beyond
the host-id pin written after the first successful join.

## Relay (designed, not built)

Some networks block multicast and broadcast (certain guest Wi-Fi, VPN, and
container setups). A relay would be a dumb rendezvous:

1. Both peers dial out to a public host over TCP (TLS to the relay, for
   transport privacy from the path, not as the end-to-end layer).
2. They present a slot id `HMAC(code, "pairflow-relay-slot")` truncated to
   16 bytes, plus a random connection nonce.
3. The relay splices the two TCP streams that presented the same slot and then
   forgets them. Today's handshake begins in the clear, so a relay on the path
   would see public keys and display names. The end-to-end MAC still stops the
   relay from finishing a pairing without the code. A relay implementation
   should encrypt from the first byte (a PAKE such as SPAKE2, or Noise IK with
   a pre-published host key) before the splice carries the session.
4. The slot id is a fast MAC of a 25-bit code. The relay, or anyone who sees
   the slot id, can brute-force the code offline. A relay deployment must
   either use a longer code or a password-authenticated key exchange (SPAKE2)
   whose transcript is not checkable offline. That is why the relay is not
   implemented: shipping it with the 5-character code would weaken the LAN
   design, where the code is not exposed to offline search.

Until then, `--direct HOST:PORT` is the escape hatch when discovery is blocked.
The peers still run the same end-to-end handshake.

## Packaging and CI

GitHub Actions (`.github/workflows/ci.yml`):

- `ubuntu-22.04` runs `cargo test` and builds the AppImage. 22.04 keeps the
  glibc requirement modest. The binary does not link libX11.
- `windows-latest` tests and uploads `pairflow-windows-x86_64.exe`.
- `macos-latest` tests, builds a universal binary, and uploads
  `pairflow-macos.dmg`.
- A tag `v*` publishes a **draft** GitHub Release with those three files.
  Untagged pushes and pull requests upload the same files as workflow
  artifacts only.

Nothing in CI is signed. Signing is a follow-up: a Developer ID certificate
and notarization for macOS, and Authenticode for Windows.

## Roadmap

1. Wayland via the XDG input-capture portal or libei, with X11 kept as a fallback.
2. Optional longer codes and SPAKE2, then the relay above.
3. Multi-monitor geometry and a second peer.
4. Clipboard.
5. A small status window on all three platforms, still driving this core.
6. Signed, notarized releases.
