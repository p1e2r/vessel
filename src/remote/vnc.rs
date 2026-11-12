//! VNC (RFB) client built on vnc-rs: Tight/ZRLE/Raw/CopyRect, VNC password
//! auth, optional server-side resize (ExtendedDesktopSize).

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, Context as _};
use tokio::net::TcpStream;
use vnc::{ClientKeyEvent, ClientMouseEvent, PixelFormat, VncConnector, VncEncoding, VncEvent, X11Event};

use super::{keymap, Button, RemoteInput, SharedFrame, Status};
use crate::profiles::{Profile, VncEncodingPref};
use crate::runtime::rt;

pub fn spawn(profile: Profile, password: Option<String>, size: (u16, u16), frame: SharedFrame, rx: flume::Receiver<RemoteInput>) {
    rt().spawn(async move {
        let result = run(&profile, password.unwrap_or_default(), size, &frame, rx).await;
        let status = match result {
            Ok(reason) => Status::Closed(reason),
            Err(e) => Status::Error(format!("{e:#}")),
        };
        frame.lock().set_status(status);
    });
}

struct InputState {
    buttons: u8,
    pos: (u16, u16),
    /// keysym sent for each pressed key name, so the release matches the press.
    pressed: HashMap<String, u32>,
    mods: (bool, bool), // ctrl, alt held
}

async fn run(p: &Profile, password: String, size: (u16, u16), frame: &SharedFrame, rx: flume::Receiver<RemoteInput>) -> anyhow::Result<String> {
    frame.lock().set_status(Status::Connecting(format!("Connecting to {}:{} …", p.host, p.port)));
    let tcp = tokio::time::timeout(Duration::from_secs(15), TcpStream::connect((p.host.as_str(), p.port)))
        .await
        .map_err(|_| anyhow!("Timed out connecting to {}:{}", p.host, p.port))?
        .context("TCP connect")?;
    let _ = tcp.set_nodelay(true);

    let mut connector = VncConnector::new(tcp)
        .set_auth_method(async move { Ok(password) })
        .allow_shared(p.shared)
        .set_pixel_format(PixelFormat::bgra());
    let order: &[VncEncoding] = match p.vnc_encoding {
        VncEncodingPref::Tight => &[VncEncoding::Tight, VncEncoding::Zrle, VncEncoding::CopyRect, VncEncoding::Raw],
        VncEncodingPref::Zrle => &[VncEncoding::Zrle, VncEncoding::CopyRect, VncEncoding::Raw],
        VncEncodingPref::Raw => &[VncEncoding::CopyRect, VncEncoding::Raw],
    };
    for e in order {
        connector = connector.add_encoding(*e);
    }
    connector = connector
        .add_encoding(VncEncoding::DesktopSizePseudo)
        .add_encoding(VncEncoding::ExtendedDesktopSizePseudo);

    frame.lock().set_status(Status::Connecting("Authenticating…".into()));
    let vnc = connector
        .build()
        .context("VNC setup")?
        .try_start()
        .await
        .context("VNC handshake (wrong password, or the server needs a different auth type?)")?
        .finish()
        .context("VNC handshake")?;

    frame.lock().set_status(Status::Connected);

    // Ask the server for the configured resolution (only some servers allow it).
    if p.width > 0 && p.height > 0 {
        let _ = tokio::time::timeout(Duration::from_secs(5), vnc.resize_desktop(size.0, size.1)).await;
    }

    let fps = p.fps.clamp(1, 240) as u64;
    let mut tick = tokio::time::interval(Duration::from_micros(1_000_000 / fps));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut st = InputState { buttons: 0, pos: (0, 0), pressed: HashMap::new(), mods: (false, false) };

    loop {
        // Drain everything the server has sent.
        loop {
            match vnc.poll_event().await {
                Ok(Some(ev)) => handle_event(ev, frame)?,
                Ok(None) => break,
                Err(e) => {
                    let _ = vnc.close().await;
                    return Err(anyhow!("{e}")).context("connection lost");
                }
            }
        }

        tokio::select! {
            m = rx.recv_async() => {
                let Ok(m) = m else { let _ = vnc.close().await; return Ok("Disconnected".into()) };
                let mut msgs = vec![m];
                msgs.extend(rx.try_iter());
                for m in msgs {
                    if matches!(m, RemoteInput::Close) {
                        let _ = vnc.close().await;
                        return Ok("Disconnected".into());
                    }
                    for ev in to_x11(m, &mut st) {
                        vnc.input(ev).await.map_err(|e| anyhow!("{e}"))?;
                    }
                }
            }
            _ = tick.tick() => {
                vnc.input(X11Event::Refresh).await.map_err(|e| anyhow!("{e}"))?;
            }
            _ = tokio::time::sleep(Duration::from_millis(4)) => {}
        }
    }
}

fn handle_event(ev: VncEvent, frame: &SharedFrame) -> anyhow::Result<()> {
    match ev {
        VncEvent::SetResolution(s) => frame.lock().resize(s.width as u32, s.height as u32),
        VncEvent::RawImage(r, data) => {
            frame.lock().blit_bgra(r.x as u32, r.y as u32, r.width as u32, r.height as u32, &data, r.width as usize * 4);
        }
        VncEvent::Copy(dst, src) => {
            frame.lock().copy_rect(dst.x as u32, dst.y as u32, src.x as u32, src.y as u32, dst.width as u32, dst.height as u32);
        }
        VncEvent::JpegImage(r, data) => {
            if let Ok(img) = image::load_from_memory_with_format(&data, image::ImageFormat::Jpeg) {
                let mut rgba = img.to_rgba8();
                for px in rgba.chunks_exact_mut(4) {
                    px.swap(0, 2);
                }
                frame.lock().blit_bgra(r.x as u32, r.y as u32, r.width as u32, r.height as u32, &rgba, r.width as usize * 4);
            }
        }
        VncEvent::Text(t) => frame.lock().clipboard = Some(t),
        VncEvent::Error(e) => anyhow::bail!(e),
        _ => {}
    }
    Ok(())
}

fn to_x11(m: RemoteInput, st: &mut InputState) -> Vec<X11Event> {
    let mouse = |st: &InputState| X11Event::PointerEvent(ClientMouseEvent { position_x: st.pos.0, position_y: st.pos.1, bottons: st.buttons });
    let key = |sym: u32, down: bool| X11Event::KeyEvent(ClientKeyEvent { keycode: sym, down });
    match m {
        RemoteInput::MouseMove(x, y) => {
            st.pos = (x, y);
            vec![mouse(st)]
        }
        RemoteInput::Button(b, down) => {
            let bit = match b {
                Button::Left => 1,
                Button::Middle => 2,
                Button::Right => 4,
            };
            if down {
                st.buttons |= bit;
            } else {
                st.buttons &= !bit;
            }
            vec![mouse(st)]
        }
        RemoteInput::Wheel { dx, dy } => {
            let mut v = Vec::new();
            let mut notch = |bit: u8, n: i16| {
                for _ in 0..(n.unsigned_abs() / 120).max(1) {
                    v.push(X11Event::PointerEvent(ClientMouseEvent { position_x: st.pos.0, position_y: st.pos.1, bottons: st.buttons | bit }));
                    v.push(X11Event::PointerEvent(ClientMouseEvent { position_x: st.pos.0, position_y: st.pos.1, bottons: st.buttons }));
                }
            };
            if dy != 0 {
                notch(if dy > 0 { 8 } else { 16 }, dy);
            }
            if dx != 0 {
                notch(if dx > 0 { 32 } else { 64 }, dx);
            }
            v
        }
        RemoteInput::Key { name, ch, down } => {
            match name.as_str() {
                "control" => st.mods.0 = down,
                "alt" => st.mods.1 = down,
                _ => {}
            }
            if down {
                // Typed characters use their own keysym (handles shift and non-US layouts);
                // with Ctrl/Alt held, send the bare key so shortcuts work.
                let sym = match ch {
                    Some(c) if !c.is_control() && !st.mods.0 && !st.mods.1 && name != "space" => Some(keymap::char_keysym(c)),
                    _ => keymap::keysym(&name),
                };
                let Some(sym) = sym else { return vec![] };
                st.pressed.insert(name, sym);
                vec![key(sym, true)]
            } else {
                let sym = st.pressed.remove(&name).or_else(|| keymap::keysym(&name));
                sym.map(|s| vec![key(s, false)]).unwrap_or_default()
            }
        }
        RemoteInput::Text(t) => t
            .chars()
            .flat_map(|c| {
                let sym = if c == '\n' { 0xff0d } else { keymap::char_keysym(c) };
                [key(sym, true), key(sym, false)]
            })
            .collect(),
        RemoteInput::CtrlAltDel => vec![
            key(0xffe3, true),
            key(0xffe9, true),
            key(0xffff, true),
            key(0xffff, false),
            key(0xffe9, false),
            key(0xffe3, false),
        ],
        RemoteInput::Clipboard(t) => vec![X11Event::CopyText(t)],
        RemoteInput::Close => vec![],
    }
}
