//! Local shell in a pseudo-terminal (zsh by default).

use std::io::{Read, Write};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use super::{TermCore, TransportMsg};

pub fn spawn_local(core: &TermCore, rx: flume::Receiver<TransportMsg>, shell: &str, start_dir: &str) -> anyhow::Result<()> {
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize { rows: 30, cols: 100, pixel_width: 0, pixel_height: 0 })?;

    let shell = if std::path::Path::new(shell).exists() {
        shell.to_string()
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into())
    };
    let mut cmd = CommandBuilder::new(&shell);
    cmd.arg("-l"); // login shell, like Terminal.app
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "Vessel");
    cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
    // Apps launched from Finder don't inherit a locale; zsh needs one for UTF-8.
    if std::env::var("LANG").map(|l| l.is_empty()).unwrap_or(true) {
        cmd.env("LANG", "en_US.UTF-8");
    }
    let wanted = start_dir.trim();
    let dir = if wanted.is_empty() || wanted == "~" {
        dirs::home_dir()
    } else {
        Some(crate::profiles::expand_tilde(wanted)).filter(|d| d.is_dir()).or_else(dirs::home_dir)
    };
    if let Some(dir) = dir {
        cmd.cwd(dir);
    }

    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let master = pair.master;

    // Reader: PTY output -> terminal grid.
    let mut sink = core.sink();
    std::thread::Builder::new().name("pty-read".into()).spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => sink.feed(&buf[..n]),
            }
        }
        sink.exited("Process exited");
    })?;

    // Writer: keystrokes/resizes -> PTY.
    std::thread::Builder::new().name("pty-write".into()).spawn(move || {
        while let Ok(msg) = rx.recv() {
            match msg {
                TransportMsg::Data(bytes) => {
                    if writer.write_all(&bytes).is_err() {
                        break;
                    }
                    let _ = writer.flush();
                }
                TransportMsg::Resize { cols, rows, px_w, px_h } => {
                    let _ = master.resize(PtySize { rows, cols, pixel_width: px_w, pixel_height: px_h });
                }
                TransportMsg::Close => break,
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    })?;

    Ok(())
}
