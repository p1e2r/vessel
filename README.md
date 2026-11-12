# Vessel

A native macOS terminal written entirely in Rust with [GPUI](https://www.gpui.rs) (Zed's GPU UI framework). No web views.

- **Tabs**: local **zsh** login shells, SSH sessions, and Windows RDP / VNC desktops, all in one window
- **Saved connections** for SSH, RDP and VNC. Passwords and key passphrases are stored in the **macOS Keychain**; the other details go in `~/Library/Application Support/Vessel/profiles.json`
- **Quick connect** (⌘P): fuzzy-search your saved hosts, or type `user@host`, `rdp://host`, `vnc://host:5901`
- **Translucent window** with background blur and a clean titlebar with the tabs in it
- **RDP and VNC are configurable per connection**: resolution (or "auto" to match the tab), frame rate, scaling (fit or 1:1), colour depth, NLA, wallpaper, font smoothing, desktop effects, ⌘-as-Ctrl, VNC encoding (Tight/ZRLE/Raw), view-only and shared mode

## Download

Grab the latest `.dmg` from [Releases](https://github.com/p1e2r/vessel/releases/latest):
**Vessel-x.y.z-macos-apple-silicon.dmg** (M-series Macs) or **…-macos-intel.dmg**.
The app is ad-hoc signed, so the first time, right-click → Open.

## Updates

Vessel checks GitHub Releases at launch and every 6 hours. When a newer version exists, an **↑ Update to x.y.z** button appears in the titlebar. Click it to download the `.dmg` for your Mac (Apple Silicon or Intel), check it against the release's `SHA256SUMS`, and swap it in place of the running app. Then click **↻ Restart to update**. You can also use **Vessel → Check for Updates…** or the Updates row in Settings. Vessel.app must be in a folder you can write to, such as `/Applications`.

## Build (on your Mac)

Needs Rust (https://rustup.rs) and the Xcode Command Line Tools (`xcode-select --install`).

```sh
scripts/build-macos.sh                               # both .dmgs in dist/
ARCHS=aarch64-apple-darwin scripts/build-macos.sh    # Apple Silicon only
cargo run                                            # dev build
```

## Branches & releases

- **dev** — day-to-day development. Every push is built and tested by GitHub Actions; nothing is published.
- **main** — releases. Every push publishes the next version `x.y.z` (Cargo.toml version, then the next free patch number) as GitHub's **Latest** release, with an Apple Silicon and an Intel `.dmg` plus `SHA256SUMS`.

Work on `dev`, then merge into `main` to ship.

## Shortcuts

| Keys | Action |
|---|---|
| ⌘T / ⌘W | New tab / close tab |
| ⌘1…⌘9, ⌘⇧[ ⌘⇧] | Switch tabs |
| ⌘P (or ⌘O) | Quick connect |
| ⌘N | New saved connection |
| ⌘B | Connections sidebar |
| ⌘C / ⌘V / ⌘A | Copy / paste / select all (terminal) |
| ⌘K | Clear the screen and scrollback |
| ⌘+ / ⌘− / ⌘0 | Font size |
| ⌘, | Settings |
| ⌘⇧U | Toggle blur |
| ⌥← / ⌥→, ⌘← / ⌘→ | Move by word / to start or end of the line |

Mouse: drag to select, double-click to select a word, triple-click to select a line, ⌥-drag for a block selection, ⇧-drag inside apps that use the mouse (vim, htop).

## SSH

This is a native client ([russh](https://github.com/Eugeny/russh)); it doesn't run `/usr/bin/ssh`. It tries these in order:

1. The key file set in the connection
2. ssh-agent
3. `~/.ssh/id_ed25519`, `id_ecdsa`, `id_rsa`
4. The password saved in the Keychain
5. A password prompt in the tab, which offers to save the password to the Keychain

Host keys are checked against `~/.ssh/known_hosts`. A new host's key is added (like `accept-new`). If a host's key has changed, the connection is refused.

## RDP

This uses [IronRDP](https://github.com/Devolutions/IronRDP) with TLS + NLA (CredSSP), RemoteFX and fast-path input.

- ⌘ is sent as Ctrl by default, so ⌘C / ⌘V work in Windows.
- The toolbar has **Ctrl+Alt+Del**, **Type clipboard** (types the Mac clipboard as keystrokes), Fit/1:1 and Disconnect.
- Server certificates are pinned the first time you connect (trust on first use). The pins are in `rdp_known_hosts.json`.
- The username can be `user`, `DOMAIN\user` or `user@domain`. You can also fill in the Domain field instead.

## VNC

This uses [vnc-rs](https://github.com/HsuJv/vnc-rs) with VNC password auth and Tight/ZRLE/Raw/CopyRect encodings. **Send clipboard** puts your Mac clipboard on the server's clipboard. A width × height set in the connection is requested from the server, but only servers with ExtendedDesktopSize (such as TigerVNC) accept it.

To use macOS Screen Sharing as the server, enable *"VNC viewers may control screen with password"* in its settings, because Apple's own login method isn't supported.

## Settings

Open **Settings** with ⌘, or the ⚙ button in the titlebar. Changes apply immediately:

- **Transparency:** Solid, 95%, 90%, 80%, 70%, 60%, 50%, or fine-tune it in 5% steps
- **Blur** what's behind the window
- **Start new tabs in:** any folder (type a path, use **Choose…**, or **Home**)
- **Startup command:** typed into each new local tab once the shell is up (e.g. `neofetch` or `cd ~/code && git status`), optionally only in the first tab
- **Font size**
- **Updates:** check now, install with one click, or turn off automatic checks

Everything is also saved in `~/Library/Application Support/Vessel/settings.json` (the panel’s **Edit JSON** button opens it):

```json
{
  "font_family": "Menlo",
  "font_size": 13.0,
  "line_height": 1.3,
  "opacity": 0.8,
  "blur": true,
  "shell": "/bin/zsh",
  "start_dir": "~/Projects",
  "startup_command": "",
  "startup_first_tab_only": false,
  "auto_update_check": true,
  "option_as_meta": false,
  "scrollback": 10000,
  "copy_on_select": false
}
```

## Troubleshooting

You can test a connection without the UI:

```sh
dist/Vessel.app/Contents/MacOS/vessel --probe ssh myhost 22 me 'password'
dist/Vessel.app/Contents/MacOS/vessel --probe rdp 10.0.0.5 3389 Administrator 'password'
dist/Vessel.app/Contents/MacOS/vessel --probe vnc mac-mini.local 5900 '' 'vncpass'
```

## Layout

```
src/main.rs            app, window, menus
src/workspace.rs       tabs, titlebar, sidebar, overlays
src/terminal/          alacritty_terminal grid, GPUI renderer, PTY (zsh), SSH
src/remote/            shared framebuffer view, RDP (IronRDP), VNC (vnc-rs), keymaps
src/profiles.rs        saved connections + Keychain
src/palette.rs         ⌘P quick connect
src/editor.rs          connection editor
src/input.rs           text field widget
```
