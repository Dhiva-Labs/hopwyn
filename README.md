# Hopwyn

One mouse and keyboard for every computer and phone in your home. Move the cursor off the edge of one screen and it appears on the next. The machine you left goes inactive; the one you entered becomes active.

- **Cross-platform:** Windows, macOS and Linux (X11 and Wayland) in v1.0; Android and iOS in v1.1.
- **One-click setup:** devices find each other on your home network. Type the 6-digit code shown on the other device once and you're paired.
- **Two-way:** any device's mouse and keyboard can drive any other. No fixed server.
- **Private and secure:** no account, no cloud, no telemetry. Everything stays on your home network, end-to-end encrypted. See [SECURITY.md](SECURITY.md).

> Status: working on Linux (GNOME/KDE on Wayland), developer preview. Windows, macOS and phones are not built yet.

## Install (Linux)

Requires Rust 1.88+ and the GTK 4 / libadwaita development files (`sudo apt install libgtk-4-dev libadwaita-1-dev` on Ubuntu).

```bash
scripts/install-linux.sh                 # builds and installs for your user, no sudo; starts at login
scripts/install-linux.sh --no-autostart  # don't start Hopwyn at login
scripts/install-linux.sh --uninstall
```

Then open **Hopwyn** from your app launcher on both computers:

1. Allow the two desktop permission dialogs (capture input, then remote control). Remote control is asked only once. GNOME 50 and older can't remember the capture permission, so it is asked once each time you log in; Hopwyn keeps running when you close its window so it isn't asked again. Desktops with InputCapture portal version 2 remember it too.
2. On one computer click **Add computer**; it shows a 6-digit code for 2 minutes.
3. On the other, pick that computer under **Nearby**, click **Pair…** and type the code.
4. Choose which side the other screen is on, then move the cursor off that edge. Repeat with **Add computer** for up to 4 computers, one on each side.

**Ctrl+Alt+Esc** always gives your cursor back. The firewall must allow UDP 4242 (link), UDP 5353 (discovery) and, while pairing, TCP 4243 between the computers.

## Releases

| Version | Scope |
| --- | --- |
| v1.0 | Desktop: discovery, pairing, two-way control, active/inactive handoff, layout editor, multi-monitor, clipboard text, hotkeys, tray app, installers |
| v1.1 | Phones (Android control, iOS remote), clipboard images, file drag-and-drop, passing the cursor through one computer to the next |

## Known limitations

- **iOS cannot be controlled from a computer.** Apple does not allow apps to inject input into the system, so an iPhone or iPad can act as a trackpad and keyboard for your computers and share the clipboard and files, but your computer's mouse cannot move onto it. Android does not have this limit.
- **Linux Wayland** needs a desktop with the InputCapture portal (GNOME 45+, KDE Plasma 6.1+) to send input. Older desktops can still receive input.
- **Home network only.** Hopwyn refuses connections from public IP addresses and has no relay servers.
- **Up to 4 paired computers**, one beyond each edge of the screen. Each computer's own mouse can reach its neighbours; passing the cursor on through a neighbour to the computer beyond it is not supported yet.
- **Hidden permission dialogs.** GNOME may open the permission dialogs behind other windows; press the Super key to find them.

## Repository layout

| Path | What it is |
| --- | --- |
| `crates/hopwyn-proto` | Wire protocol: message types and a bounded binary encoding |
| `crates/hopwyn-pair` | Device identity (Ed25519) and 6-digit-code pairing (SPAKE2), with pairing window and lockout |
| `crates/hopwyn-transport` | QUIC + TLS 1.3 transport with pinned device keys, home-network-only policy, framed messages |
| `crates/hopwyn-discovery` | mDNS / DNS-SD announce and browse (`_hopwyn._udp`), with strict validation of what it hears |
| `crates/hopwyn-input` | Input events, HID-to-native key mapping, injection (Linux: RemoteDesktop portal + libei) and screen-edge capture (Linux: InputCapture portal + libei), with a Ctrl+Alt+Esc panic release |
| `crates/hopwyn-core` | Edge handoff state machine: who is active, what to send, what to inject |
| `crates/hopwyn-app` | The desktop app (`hopwyn-app`): GTK 4 / libadwaita window, network pairing, settings, and the service tying everything together |
| `crates/hopwyn-daemon` | `hopwyn`, a command-line developer tool for testing the handoff without the app |
| `fuzz/` | Fuzz targets (`cargo +nightly fuzz run decode`) |

## Command-line developer tool

`hopwyn` skips pairing: each machine trusts the other's key by hand.

1. On each machine, print its key: `cargo run -p hopwyn-daemon -- id`
2. On the machine on the left: `cargo run -p hopwyn-daemon -- run --edge right --trust <right machine's key>`
3. On the machine on the right: `cargo run -p hopwyn-daemon -- run --edge left --trust <left machine's key>`
4. Allow both desktop dialogs on each machine. The machines find each other automatically (or add `--connect <ip>:4242`).
5. Push the cursor off the shared edge: it appears on the other machine, and the machine you left goes inactive. Ctrl+Alt+Esc always returns control.

The identity key is kept in `~/.config/hopwyn/identity` (mode 0600) until the OS key store is wired up.

## Building

Requires Rust 1.88 or newer.

```bash
cargo test --workspace
```

The live discovery test uses real multicast, so it is skipped by default:

```bash
cargo test -p hopwyn-discovery --test live -- --ignored
```

## License

Apache-2.0. See [LICENSE](LICENSE).
