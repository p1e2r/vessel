//! RDP client built on IronRDP (pure Rust): TLS + NLA/CredSSP, fast-path
//! graphics (incl. RemoteFX), fast-path keyboard/mouse input.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context as _};
use ironrdp_connector::connection_activation::ConnectionActivationState;
use ironrdp_connector::{self as connector, ClientConnector, ConnectionResult, Credentials, DesktopSize, ServerName};
use ironrdp_core::WriteBuf;
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_input::{Database, MousePosition, Operation, Scancode, WheelRotations};
use ironrdp_pdu::gcc::KeyboardType;
use ironrdp_pdu::rdp::capability_sets::MajorPlatformType;
use ironrdp_pdu::rdp::client_info::{PerformanceFlags, TimezoneInfo};
use ironrdp_session::image::DecodedImage;
use ironrdp_session::{ActiveStage, ActiveStageBuilder, ActiveStageOutput, GracefulDisconnectReason};
use ironrdp_tokio::reqwest::ReqwestNetworkClient;
use ironrdp_tokio::{single_sequence_step_read, split_tokio_framed, FramedWrite, TokioFramed};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;

use super::{keymap, Button, RemoteInput, SharedFrame, Status};
use crate::profiles::Profile;
use crate::runtime::rt;
use crate::settings::config_dir;

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

fn build_config(p: &Profile, password: String, size: (u16, u16)) -> connector::Config {
    let mut perf = PerformanceFlags::empty();
    if !p.show_wallpaper {
        perf |= PerformanceFlags::DISABLE_WALLPAPER;
    }
    if p.font_smoothing {
        perf |= PerformanceFlags::ENABLE_FONT_SMOOTHING;
    }
    if p.desktop_composition {
        perf |= PerformanceFlags::ENABLE_DESKTOP_COMPOSITION;
    } else {
        perf |= PerformanceFlags::DISABLE_MENUANIMATIONS | PerformanceFlags::DISABLE_FULLWINDOWDRAG;
    }

    let bitmap = ironrdp_pdu::rdp::capability_sets::client_codecs_capabilities(&[])
        .ok()
        .map(|codecs| connector::BitmapConfig { lossy_compression: true, color_depth: p.color_depth, codecs });

    connector::Config {
        credentials: Credentials::UsernamePassword { username: p.username.clone(), password },
        domain: (!p.domain.trim().is_empty()).then(|| p.domain.trim().to_string()),
        enable_tls: true,
        enable_credssp: p.nla,
        enable_standard_rdp_security: false,
        keyboard_type: KeyboardType::IBM_ENHANCED,
        keyboard_subtype: 0,
        keyboard_layout: 0x0409,
        keyboard_functional_keys_count: 12,
        connection_type: ironrdp_pdu::gcc::ConnectionType::Lan,
        ime_file_name: String::new(),
        dig_product_id: String::new(),
        desktop_size: DesktopSize { width: size.0 & !1, height: size.1 },
        monitor_layout: None,
        bitmap,
        client_build: 0,
        client_name: "Vessel".to_owned(),
        client_dir: "C:\\Windows\\System32\\mstscax.dll".to_owned(),
        platform: MajorPlatformType::MACINTOSH,
        enable_server_pointer: false,
        request_data: None,
        autologon: false,
        enable_audio_playback: false,
        enable_audio_capture: false,
        compression_type: None,
        pointer_software_rendering: true,
        multitransport_flags: None,
        support_dyn_vc_gfx_protocol: false,
        performance_flags: perf,
        desktop_scale_factor: 0,
        desktop_physical_size: None,
        desktop_orientation: None,
        device_scale_factor: None,
        monitor_extended_layout: None,
        relative_mouse_input: false,
        hardware_id: None,
        license_cache: None,
        timezone_info: TimezoneInfo::default(),
        alternate_shell: String::new(),
        work_dir: String::new(),
        remote_application_mode: false,
        rail_support_level: ironrdp_pdu::rdp::capability_sets::RailSupportLevel::empty(),
    }
}

/// Trust-on-first-use pinning of the server's TLS certificate (RDP servers
/// almost always use self-signed certs).
fn cert_pins_path() -> std::path::PathBuf {
    config_dir().join("rdp_known_hosts.json")
}

fn tofu_callback(endpoint: String, notes: Arc<Mutex<Vec<String>>>) -> ironrdp_tls::CertificateValidationCallback {
    Arc::new(move |der: &[u8], _endpoint: &str, _err: &str| {
        let fp: String = Sha256::digest(der).iter().map(|b| format!("{b:02x}")).collect();
        let path = cert_pins_path();
        let mut pins: HashMap<String, String> =
            std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        match pins.get(&endpoint) {
            Some(known) if *known == fp => true,
            Some(_) => {
                notes.lock().push(format!(
                    "The certificate for {endpoint} changed since last time (SHA-256 {fp}). Refusing to connect. \
                     If this is expected, remove the entry from {}.",
                    path.display()
                ));
                false
            }
            None => {
                pins.insert(endpoint.clone(), fp);
                if let Ok(s) = serde_json::to_string_pretty(&pins) {
                    let _ = std::fs::write(&path, s);
                }
                true
            }
        }
    })
}

async fn run(p: &Profile, password: String, size: (u16, u16), frame: &SharedFrame, rx: flume::Receiver<RemoteInput>) -> anyhow::Result<String> {
    let set = |s: &str| frame.lock().set_status(Status::Connecting(s.into()));
    set(&format!("Connecting to {}:{} …", p.host, p.port));

    let addr: SocketAddr = tokio::net::lookup_host((p.host.as_str(), p.port))
        .await
        .context("DNS lookup")?
        .next()
        .ok_or_else(|| anyhow!("{} did not resolve", p.host))?;
    let stream = tokio::time::timeout(Duration::from_secs(15), TcpStream::connect(addr))
        .await
        .map_err(|_| anyhow!("Timed out connecting to {addr}"))?
        .context("TCP connect")?;
    let client_addr = stream.local_addr()?;
    let _ = stream.set_nodelay(true);

    let mut framed = TokioFramed::new(stream);
    let mut connector = ClientConnector::new(build_config(p, password, size), client_addr);
    let should_upgrade = ironrdp_tokio::connect_begin(&mut framed, &mut connector).await.context("RDP negotiation")?;

    set("Securing connection (TLS)…");
    let initial = framed.into_inner_no_leftover();
    let notes = Arc::new(Mutex::new(Vec::new()));
    let endpoint = format!("{}:{}", p.host, p.port);
    let tls = ironrdp_tls::upgrade_with_certificate_validation_callback(initial, &p.host, tofu_callback(endpoint, notes.clone())).await;
    let (tls_stream, cert) = match tls {
        Ok(v) => v,
        Err(e) => {
            let n = notes.lock().join("\n");
            if !n.is_empty() {
                anyhow::bail!(n);
            }
            return Err(e).context("TLS handshake");
        }
    };
    let server_public_key = ironrdp_tls::extract_tls_server_public_key(&cert)
        .ok_or_else(|| anyhow!("server certificate has no public key"))?
        .to_owned();
    let upgraded = ironrdp_tokio::mark_as_upgraded(should_upgrade, &mut connector);
    let mut upgraded_framed = TokioFramed::new(tls_stream);

    set(if p.nla { "Authenticating (NLA)…" } else { "Logging in…" });
    let mut network_client = ReqwestNetworkClient::new();
    let result: ConnectionResult = ironrdp_tokio::connect_finalize(
        upgraded,
        connector,
        &mut upgraded_framed,
        &mut network_client,
        ServerName::new(p.host.clone()),
        server_public_key,
        None,
    )
    .await
    .context("RDP login")?
    .into_connected()
    .context("RDP login")?;

    let mut image = DecodedImage::new(PixelFormat::RgbA32, result.desktop_size.width, result.desktop_size.height);
    {
        let mut f = frame.lock();
        f.resize(result.desktop_size.width as u32, result.desktop_size.height as u32);
        f.set_status(Status::Connected);
    }

    let activation_factory = result.activation_factory.clone();
    let mut stage: ActiveStage = ActiveStageBuilder {
        static_channels: result.static_channels,
        user_channel_id: result.user_channel_id,
        io_channel_id: result.io_channel_id,
        message_channel_id: result.message_channel_id,
        share_id: result.share_id,
        compression_type: result.compression_type,
        enable_server_pointer: result.enable_server_pointer,
        pointer_software_rendering: result.pointer_software_rendering,
    }
    .build();

    let (mut reader, mut writer) = split_tokio_framed(upgraded_framed);
    let mut input_db = Database::new();

    loop {
        let outputs = tokio::select! {
            pdu = reader.read_pdu() => {
                let (action, payload) = pdu.context("connection lost")?;
                stage.process(&mut image, action, &payload, reader.last_read_at())?
            }
            msg = rx.recv_async() => {
                let Ok(msg) = msg else { return Ok("Disconnected".into()) };
                let mut ops = Vec::new();
                let mut closing = false;
                to_ops(msg, &mut ops, &mut closing);
                for m in rx.try_iter() {
                    to_ops(m, &mut ops, &mut closing);
                }
                if closing {
                    for out in stage.graceful_shutdown().unwrap_or_default() {
                        if let ActiveStageOutput::ResponseFrame(f) = out {
                            let _ = writer.write_all(&f).await;
                        }
                    }
                    return Ok("Disconnected".into());
                }
                let events = input_db.apply(ops);
                stage.process_fastpath_input(&mut image, &events)?
            }
        };

        for out in outputs {
            match out {
                ActiveStageOutput::ResponseFrame(f) => writer.write_all(&f).await.context("send")?,
                ActiveStageOutput::GraphicsUpdate(r) => {
                    let w = (r.right.saturating_sub(r.left) + 1) as u32;
                    let h = (r.bottom.saturating_sub(r.top) + 1) as u32;
                    let stride = image.stride();
                    let data = image.data();
                    let start = r.top as usize * stride + r.left as usize * 4;
                    if start < data.len() {
                        frame.lock().blit_rgba(r.left as u32, r.top as u32, w, h, &data[start..], stride);
                    }
                }
                ActiveStageOutput::Terminate(reason) => {
                    return Ok(match reason {
                        GracefulDisconnectReason::UserInitiated => "Disconnected".into(),
                        GracefulDisconnectReason::ServerInitiated => "Server ended the session".into(),
                        GracefulDisconnectReason::Other(s) => format!("Session ended: {s}"),
                        #[allow(unreachable_patterns)]
                        other => format!("Session ended: {other:?}"),
                    });
                }
                ActiveStageOutput::DeactivateAll => {
                    // Deactivation–reactivation (e.g. server changed resolution).
                    let mut seq = activation_factory.create();
                    let mut buf = WriteBuf::new();
                    loop {
                        let written = single_sequence_step_read(&mut reader, &mut seq, &mut buf).await?;
                        if written.size().is_some() {
                            writer.write_all(buf.filled()).await?;
                        }
                        if let ConnectionActivationState::Finalized {
                            desktop_size,
                            share_id,
                            enable_server_pointer,
                            pointer_software_rendering,
                            static_channel_chunk_size,
                            ..
                        } = seq.connection_activation_state()
                        {
                            image = DecodedImage::new(PixelFormat::RgbA32, desktop_size.width, desktop_size.height);
                            frame.lock().resize(desktop_size.width as u32, desktop_size.height as u32);
                            if !stage.reactivate(
                                seq.io_channel_id(),
                                seq.user_channel_id(),
                                share_id,
                                enable_server_pointer,
                                pointer_software_rendering,
                                static_channel_chunk_size,
                            ) {
                                anyhow::bail!("reactivation failed");
                            }
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn sc(code: u16) -> Scancode {
    Scancode::from_u16(code)
}

fn to_ops(msg: RemoteInput, ops: &mut Vec<Operation>, closing: &mut bool) {
    match msg {
        RemoteInput::MouseMove(x, y) => ops.push(Operation::MouseMove(MousePosition { x, y })),
        RemoteInput::Button(b, down) => {
            let b = match b {
                Button::Left => ironrdp_input::MouseButton::Left,
                Button::Right => ironrdp_input::MouseButton::Right,
                Button::Middle => ironrdp_input::MouseButton::Middle,
            };
            ops.push(if down { Operation::MouseButtonPressed(b) } else { Operation::MouseButtonReleased(b) });
        }
        RemoteInput::Wheel { dx, dy } => {
            if dy != 0 {
                ops.push(Operation::WheelRotations(WheelRotations { is_vertical: true, rotation_units: dy }));
            }
            if dx != 0 {
                ops.push(Operation::WheelRotations(WheelRotations { is_vertical: false, rotation_units: -dx }));
            }
        }
        RemoteInput::Key { name, ch, down } => {
            if let Some(code) = keymap::scancode(&name) {
                ops.push(if down { Operation::KeyPressed(sc(code)) } else { Operation::KeyReleased(sc(code)) });
            } else if let Some(c) = ch {
                ops.push(if down { Operation::UnicodeKeyPressed(c) } else { Operation::UnicodeKeyReleased(c) });
            }
        }
        RemoteInput::Text(t) => {
            for c in t.chars() {
                let c = if c == '\n' { '\r' } else { c };
                if c == '\r' {
                    ops.push(Operation::KeyPressed(sc(0x1C)));
                    ops.push(Operation::KeyReleased(sc(0x1C)));
                } else {
                    ops.push(Operation::UnicodeKeyPressed(c));
                    ops.push(Operation::UnicodeKeyReleased(c));
                }
            }
        }
        RemoteInput::CtrlAltDel => {
            for c in [0x1D, 0x38, 0xE053] {
                ops.push(Operation::KeyPressed(sc(c)));
            }
            for c in [0xE053, 0x38, 0x1D] {
                ops.push(Operation::KeyReleased(sc(c)));
            }
        }
        RemoteInput::Clipboard(_) => {}
        RemoteInput::Close => *closing = true,
    }
}
