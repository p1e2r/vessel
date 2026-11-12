//! Native SSH client (russh) driving a terminal tab.
//!
//! Auth order: explicit key file → ssh-agent → default keys in ~/.ssh →
//! saved password (Keychain) → interactive password prompt in the terminal.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use russh::client::{self, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::{self, PrivateKeyWithHashAlg};
use russh::{ChannelMsg, Disconnect};

use super::{Sink, TermCore, TransportMsg, UiEvent};
use crate::profiles::{expand_tilde, Profile};
use crate::runtime::rt;

struct Client {
    host: String,
    port: u16,
    notes: Arc<Mutex<Vec<String>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, pubkey: &keys::ssh_key::PublicKey) -> Result<bool, Self::Error> {
        let pubkey = pubkey.clone();
        let fp = pubkey.fingerprint(Default::default());
        match keys::check_known_hosts(&self.host, self.port, &pubkey) {
            Ok(true) => Ok(true),
            Ok(false) => {
                self.notes.lock().push(format!("New host key for {} ({fp}) — added to ~/.ssh/known_hosts", self.host));
                let _ = keys::known_hosts::learn_known_hosts(&self.host, self.port, &pubkey);
                Ok(true)
            }
            Err(keys::Error::KeyChanged { line }) => {
                self.notes.lock().push(format!(
                    "WARNING: host key for {} has CHANGED (now {fp}).\nThis could be a man-in-the-middle attack. If the change is expected, remove line {line} of ~/.ssh/known_hosts.",
                    self.host
                ));
                Ok(false)
            }
            Err(e) => {
                // Unreadable known_hosts: accept, but say so.
                self.notes.lock().push(format!("Could not check known_hosts ({e}); host key {fp}"));
                Ok(true)
            }
        }
    }
}

pub fn spawn_ssh(core: &TermCore, rx: flume::Receiver<TransportMsg>, profile: Profile) {
    let mut sink = core.sink();
    rt().spawn(async move {
        let reason = match run(&mut sink, rx, &profile).await {
            Ok(r) => r,
            Err(e) => {
                sink.error(&format!("\r\n{e:#}"));
                "Connection failed".to_string()
            }
        };
        sink.info(&format!("\r\n[{reason}] — press Enter to close, R to reconnect"));
        sink.exited(reason);
    });
}

async fn run(sink: &mut Sink, rx: flume::Receiver<TransportMsg>, p: &Profile) -> anyhow::Result<String> {
    let mut size = (100u32, 30u32);
    sink.info(&format!("Connecting to {}:{} …", p.host, p.port));

    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 4,
        ..Default::default()
    });
    let notes = Arc::new(Mutex::new(Vec::new()));
    let handler = Client { host: p.host.clone(), port: p.port, notes: notes.clone() };
    let connect = client::connect(config, (p.host.as_str(), p.port), handler);
    let res = tokio::time::timeout(Duration::from_secs(15), connect).await;
    for n in notes.lock().drain(..) {
        sink.info(&n);
    }
    let mut session = match res {
        Err(_) => anyhow::bail!("Timed out connecting to {}:{}", p.host, p.port),
        Ok(r) => r?,
    };

    let user = if p.username.is_empty() {
        std::env::var("USER").unwrap_or_else(|_| "root".into())
    } else {
        p.username.clone()
    };

    if !authenticate(&mut session, sink, &rx, p, &user, &mut size).await? {
        anyhow::bail!("Authentication failed for {user}@{}", p.host);
    }

    let mut channel = session.channel_open_session().await?;
    // Absorb any resize that happened while we were connecting.
    while let Ok(m) = rx.try_recv() {
        if let TransportMsg::Resize { cols, rows, .. } = m {
            size = (cols as u32, rows as u32);
        }
    }
    channel.request_pty(false, "xterm-256color", size.0, size.1, 0, 0, &[]).await?;
    let _ = channel.set_env(false, "COLORTERM", "truecolor").await;
    let _ = channel.set_env(false, "LANG", std::env::var("LANG").unwrap_or_else(|_| "en_US.UTF-8".into())).await;
    if p.remote_command.trim().is_empty() {
        channel.request_shell(false).await?;
    } else {
        channel.exec(false, p.remote_command.as_bytes()).await?;
    }

    let mut exit_code: Option<u32> = None;
    loop {
        tokio::select! {
            msg = channel.wait() => match msg {
                Some(ChannelMsg::Data { data }) => sink.feed(&data),
                Some(ChannelMsg::ExtendedData { data, .. }) => sink.feed(&data),
                Some(ChannelMsg::ExitStatus { exit_status }) => exit_code = Some(exit_status),
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                _ => {}
            },
            m = rx.recv_async() => match m {
                Ok(TransportMsg::Data(d)) => { channel.data(&d[..]).await?; }
                Ok(TransportMsg::Resize { cols, rows, px_w, px_h }) => {
                    let _ = channel.window_change(cols as u32, rows as u32, px_w as u32, px_h as u32).await;
                }
                Ok(TransportMsg::Close) | Err(_) => {
                    let _ = channel.close().await;
                    let _ = session.disconnect(Disconnect::ByApplication, "", "en").await;
                    return Ok("Disconnected".into());
                }
            }
        }
    }
    let _ = session.disconnect(Disconnect::ByApplication, "", "en").await;
    Ok(match exit_code {
        Some(0) | None => "Connection closed".into(),
        Some(c) => format!("Connection closed (exit {c})"),
    })
}

async fn authenticate(
    session: &mut Handle<Client>,
    sink: &mut Sink,
    rx: &flume::Receiver<TransportMsg>,
    p: &Profile,
    user: &str,
    size: &mut (u32, u32),
) -> anyhow::Result<bool> {
    let rsa_hash = session.best_supported_rsa_hash().await.ok().flatten().flatten();
    let saved_secret = p.password();

    // 1. Explicit key file.
    if !p.key_path.trim().is_empty() {
        let path = expand_tilde(p.key_path.trim());
        let key = match keys::load_secret_key(&path, saved_secret.as_deref()) {
            Ok(k) => Some(k),
            Err(_) => {
                // Probably encrypted: ask for the passphrase.
                match prompt(sink, rx, &format!("Passphrase for {}: ", path.display()), false, size).await {
                    Some(pass) => keys::load_secret_key(&path, Some(&pass)).ok(),
                    None => None,
                }
            }
        };
        match key {
            Some(key) => {
                let r = session
                    .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), rsa_hash))
                    .await?;
                if r.success() {
                    return Ok(true);
                }
                sink.info("Key was rejected.");
            }
            None => sink.error(&format!("Could not load key {}", path.display())),
        }
    } else {
        // 2. ssh-agent.
        #[cfg(unix)]
        if let Ok(mut agent) = keys::agent::client::AgentClient::connect_env().await {
            if let Ok(ids) = agent.request_identities().await {
                for id in ids {
                    let pk = id.public_key().into_owned();
                    if let Ok(r) = session.authenticate_publickey_with(user, pk, rsa_hash, &mut agent).await {
                        if r.success() {
                            return Ok(true);
                        }
                    }
                }
            }
        }
        // 3. Default unencrypted keys.
        if let Some(home) = dirs::home_dir() {
            for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
                let path = home.join(".ssh").join(name);
                if !path.exists() {
                    continue;
                }
                if let Ok(key) = keys::load_secret_key(&path, None) {
                    let r = session
                        .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), rsa_hash))
                        .await?;
                    if r.success() {
                        return Ok(true);
                    }
                }
            }
        }
    }

    // 4. Saved password.
    if p.key_path.trim().is_empty() {
        if let Some(pw) = saved_secret.clone() {
            if password_auth(session, user, &pw).await? {
                return Ok(true);
            }
            sink.error("Saved password was rejected.");
        }
    }

    // 5. Ask.
    for _ in 0..3 {
        let Some(pw) = prompt(sink, rx, &format!("{user}@{}'s password: ", p.host), false, size).await else {
            return Ok(false);
        };
        if password_auth(session, user, &pw).await? {
            if saved_secret.as_deref() != Some(pw.as_str()) && p.key_path.trim().is_empty() && !p.host.is_empty() {
                if let Some(ans) = prompt(sink, rx, "Save password to Keychain? [y/N] ", true, size).await {
                    if ans.trim().eq_ignore_ascii_case("y") || ans.trim().eq_ignore_ascii_case("yes") {
                        let _ = sink.ui.send(UiEvent::SavePassword(p.id.clone(), pw));
                    }
                }
            }
            return Ok(true);
        }
        sink.error("Permission denied, please try again.");
    }
    Ok(false)
}

async fn password_auth(session: &mut Handle<Client>, user: &str, pw: &str) -> anyhow::Result<bool> {
    if session.authenticate_password(user, pw).await?.success() {
        return Ok(true);
    }
    // Many servers only allow keyboard-interactive for passwords.
    let mut resp = session.authenticate_keyboard_interactive_start(user, None).await?;
    for _ in 0..4 {
        match resp {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                let answers = prompts.iter().map(|_| pw.to_string()).collect();
                resp = session.authenticate_keyboard_interactive_respond(answers).await?;
            }
        }
    }
    Ok(false)
}

/// Read a line typed into the terminal. Returns None on Ctrl-C / tab closed.
async fn prompt(
    sink: &mut Sink,
    rx: &flume::Receiver<TransportMsg>,
    label: &str,
    echo: bool,
    size: &mut (u32, u32),
) -> Option<String> {
    sink.feed(label.as_bytes());
    let mut line = String::new();
    loop {
        match rx.recv_async().await {
            Ok(TransportMsg::Data(bytes)) => {
                let text = String::from_utf8_lossy(&bytes).to_string();
                for ch in text.chars() {
                    match ch {
                        '\r' | '\n' => {
                            sink.feed(b"\r\n");
                            return Some(line);
                        }
                        '\x03' => {
                            sink.feed(b"^C\r\n");
                            return None;
                        }
                        '\x7f' | '\x08' => {
                            if line.pop().is_some() && echo {
                                sink.feed(b"\x08 \x08");
                            }
                        }
                        '\x15' => {
                            if echo {
                                for _ in 0..line.chars().count() {
                                    sink.feed(b"\x08 \x08");
                                }
                            }
                            line.clear();
                        }
                        c if !c.is_control() => {
                            line.push(c);
                            if echo {
                                let mut b = [0u8; 4];
                                sink.feed(c.encode_utf8(&mut b).as_bytes());
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(TransportMsg::Resize { cols, rows, .. }) => *size = (cols as u32, rows as u32),
            Ok(TransportMsg::Close) | Err(_) => return None,
        }
    }
}
