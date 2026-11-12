//! `vessel --probe <ssh|rdp|vnc> <host> <port> <user> <password>`
//! Headless connection check: exercises the same protocol code as the UI and
//! prints what happened. Handy for debugging a host without opening a window.

use std::time::{Duration, Instant};

use crate::profiles::{Kind, Profile};
use crate::remote::{Frame, Status};
use crate::terminal::{ssh, TermCore, TransportMsg, UiEvent};

pub fn run(args: &[String]) -> i32 {
    if args.len() < 5 {
        eprintln!("usage: vessel --probe <ssh|rdp|vnc> <host> <port> <user> [password]");
        return 2;
    }
    let kind = match args[0].as_str() {
        "ssh" => Kind::Ssh,
        "rdp" => Kind::Rdp,
        "vnc" => Kind::Vnc,
        _ => return 2,
    };
    let password = args.get(4).cloned().unwrap_or_default();
    let p = Profile {
        kind,
        host: args[1].clone(),
        port: args[2].parse().unwrap_or(kind.default_port()),
        username: args[3].clone(),
        ..Default::default()
    };
    match kind {
        Kind::Ssh => probe_ssh(p, password),
        _ => probe_remote(p, password),
    }
}

fn probe_ssh(p: Profile, password: String) -> i32 {
    let (core, rx) = TermCore::new(1000);
    ssh::spawn_ssh(&core, rx, p);
    let start = Instant::now();
    let mut sent_pw = false;
    let mut sent_cmd = false;
    let mut code = 1;
    while start.elapsed() < Duration::from_secs(20) {
        match core.ui_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(UiEvent::Exited(r)) => {
                println!("session ended: {r}");
                break;
            }
            _ => {}
        }
        let text = screen_text(&core);
        if !sent_pw && text.contains("password:") {
            let _ = core.transport.send(TransportMsg::Data(format!("{password}\r").into_bytes()));
            sent_pw = true;
        }
        if text.contains("Save password") {
            let _ = core.transport.send(TransportMsg::Data(b"n\r".to_vec()));
        }
        if !sent_cmd && (text.contains("$ ") || text.contains("# ")) {
            let _ = core.transport.send(TransportMsg::Data(b"echo VESSEL_OK $((6*7)); exit\r".to_vec()));
            sent_cmd = true;
        }
        if text.contains("VESSEL_OK 42") {
            code = 0;
        }
    }
    println!("----- screen -----\n{}\n------------------", screen_text(&core).trim_end());
    code
}

fn screen_text(core: &TermCore) -> String {
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line, Point};
    let term = core.term.lock();
    let mut out = String::new();
    for line in 0..term.grid().screen_lines() as i32 {
        out.push_str(term.bounds_to_string(Point::new(Line(line), Column(0)), Point::new(Line(line), term.grid().last_column())).trim_end());
        out.push('\n');
    }
    out
}

fn probe_remote(mut p: Profile, password: String) -> i32 {
    p.width = 1024;
    p.height = 768;
    let frame = std::sync::Arc::new(parking_lot::Mutex::new(Frame::new()));
    let (tx, rx) = flume::unbounded();
    match p.kind {
        Kind::Rdp => crate::remote::rdp::spawn(p, Some(password), (1024, 768), frame.clone(), rx),
        _ => crate::remote::vnc::spawn(p, Some(password), (1024, 768), frame.clone(), rx),
    }
    let start = Instant::now();
    let mut connected_at = None;
    while start.elapsed() < Duration::from_secs(25) {
        std::thread::sleep(Duration::from_millis(250));
        let f = frame.lock();
        match &f.status {
            Status::Error(e) => {
                println!("error: {e}");
                return 1;
            }
            Status::Closed(r) => {
                println!("closed: {r}");
                return 1;
            }
            Status::Connected => {
                let lit = f.data.chunks_exact(4).filter(|px| px[0] > 8 || px[1] > 8 || px[2] > 8).count();
                connected_at.get_or_insert(Instant::now());
                if lit > 1000 {
                    println!("connected: {}x{}, {} non-black pixels", f.width, f.height, lit);
                    if let Some(img) = image::RgbaImage::from_raw(f.width, f.height, {
                        let mut d = f.data.clone();
                        for px in d.chunks_exact_mut(4) {
                            px.swap(0, 2);
                        }
                        d
                    }) {
                        let _ = img.save("/tmp/vessel-probe.png");
                        println!("saved /tmp/vessel-probe.png");
                    }
                    drop(f);
                    let _ = tx.send(crate::remote::RemoteInput::Close);
                    std::thread::sleep(Duration::from_millis(500));
                    return 0;
                }
            }
            Status::Connecting(s) => println!("… {s}"),
        }
    }
    println!("timed out");
    1
}
