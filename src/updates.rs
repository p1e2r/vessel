// SPDX-License-Identifier: MIT
//! One-click updates from GitHub Releases (same scheme as Ferrum):
//! check `p1e2r/vessel` for a newer `x.y.z`, download the `.dmg` for this
//! Mac's processor, verify it against the release's SHA256SUMS, mount it,
//! swap the new `Vessel.app` in place of the running one, then restart.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::runtime::rt;

pub const VERSION: &str = match option_env!("VESSEL_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
const RELEASES_API: &str = "https://api.github.com/repos/p1e2r/vessel/releases?per_page=30";
pub const RELEASES_PAGE: &str = "https://github.com/p1e2r/vessel/releases";

#[derive(Clone, Debug, PartialEq)]
pub enum UpdateStatus {
    Idle,
    Checking,
    UpToDate,
    Available { version: String, url: String, asset: Option<String>, sums: Option<String> },
    Downloading { version: String, received: u64, total: Option<u64> },
    Installing { version: String },
    /// Installed; restart to run it.
    Installed { version: String, program: PathBuf },
    Failed(String),
}

/// App-wide updater state. The UI re-renders when `wake` fires.
pub struct Updater {
    pub status: Arc<Mutex<UpdateStatus>>,
    wake: flume::Sender<()>,
}

impl gpui::Global for Updater {}

impl Updater {
    pub fn new() -> (Self, flume::Receiver<()>) {
        let (tx, rx) = flume::unbounded();
        (Self { status: Arc::new(Mutex::new(UpdateStatus::Idle)), wake: tx }, rx)
    }

    pub fn get(&self) -> UpdateStatus {
        self.status.lock().clone()
    }

    fn setter(&self) -> impl Fn(UpdateStatus) + Send + Sync + Clone + 'static {
        let slot = self.status.clone();
        let wake = self.wake.clone();
        move |s| {
            *slot.lock() = s;
            let _ = wake.send(());
        }
    }

    /// Ask GitHub for a newer release. `quiet` keeps a failed background check from showing an error.
    pub fn check(&self, quiet: bool) {
        if matches!(self.get(), UpdateStatus::Checking | UpdateStatus::Downloading { .. } | UpdateStatus::Installing { .. } | UpdateStatus::Installed { .. }) {
            return;
        }
        let set = self.setter();
        if !quiet {
            set(UpdateStatus::Checking);
        }
        rt().spawn(async move {
            let status = match fetch_releases().await {
                Ok(json) => match newest(&json) {
                    Some(f) if parse_version(&f.version) > parse_version(VERSION) => {
                        UpdateStatus::Available { version: f.version, url: f.page, asset: f.package, sums: f.sums }
                    }
                    _ => UpdateStatus::UpToDate,
                },
                Err(e) if quiet => {
                    log_err(&e);
                    UpdateStatus::Idle
                }
                Err(e) => {
                    log_err(&e);
                    UpdateStatus::Failed("Couldn't reach GitHub to check for updates. Check your connection and try again.".into())
                }
            };
            set(status);
        });
    }

    /// Download and install the available update.
    pub fn install(&self) {
        let UpdateStatus::Available { version, asset, sums, url } = self.get() else { return };
        let set = self.setter();
        let Some(asset) = asset else {
            set(UpdateStatus::Failed(format!("Vessel {version} has no download for this Mac. Get it from {url}")));
            return;
        };
        rt().spawn(async move {
            set(UpdateStatus::Downloading { version: version.clone(), received: 0, total: None });
            let work = std::env::temp_dir().join(format!("vessel-update-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&work);
            let result = async {
                std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
                let v = version.clone();
                let s2 = set.clone();
                let package = download(&asset, &work, move |received, total| {
                    s2(UpdateStatus::Downloading { version: v.clone(), received, total })
                })
                .await?;
                set(UpdateStatus::Installing { version: version.clone() });
                if let Some(sums) = &sums {
                    verify(&package, sums).await?;
                }
                let (pkg, w) = (package.clone(), work.clone());
                tokio::task::spawn_blocking(move || put_in_place(&pkg, &w)).await.map_err(|e| e.to_string())?
            }
            .await;
            let _ = std::fs::remove_dir_all(&work);
            match result {
                Ok(program) => set(UpdateStatus::Installed { version, program }),
                Err(e) => set(UpdateStatus::Failed(e)),
            }
        });
    }
}

fn log_err(e: &str) {
    eprintln!("update check: {e}");
}

/// The part of a package name that matches this computer.
fn package_suffix() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("macos-apple-silicon.dmg"),
        ("macos", "x86_64") => Some("macos-intel.dmg"),
        _ => None,
    }
}

/// `"v0.6.2"` → `(0, 6, 2)`.
pub fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let v = v.trim().trim_start_matches('v');
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next().flatten().unwrap_or(0), parts.next().flatten().unwrap_or(0)))
}

struct Found {
    version: String,
    page: String,
    package: Option<String>,
    sums: Option<String>,
}

/// The newest release (drafts and pre-releases skipped).
fn newest(json: &serde_json::Value) -> Option<Found> {
    json.as_array()?
        .iter()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false) && !r["prerelease"].as_bool().unwrap_or(false))
        .filter_map(|r| {
            let tag = r["tag_name"].as_str()?;
            let assets = r["assets"].as_array();
            let find = |pred: &dyn Fn(&str) -> bool| {
                assets?.iter().find_map(|a| {
                    let name = a["name"].as_str()?;
                    pred(name).then(|| a["browser_download_url"].as_str().map(str::to_owned)).flatten()
                })
            };
            let found = Found {
                version: tag.trim_start_matches('v').to_string(),
                page: r["html_url"].as_str().unwrap_or(RELEASES_PAGE).to_string(),
                package: package_suffix().and_then(|suffix| find(&|n: &str| n.ends_with(suffix))),
                sums: find(&|n: &str| n == "SHA256SUMS"),
            };
            Some((parse_version(tag)?, found))
        })
        .max_by_key(|(v, _)| *v)
        .map(|(_, f)| f)
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(format!("Vessel/{VERSION}"))
        .connect_timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())
}

async fn fetch_releases() -> Result<serde_json::Value, String> {
    let resp = client()?
        .get(RELEASES_API)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let text = resp.text().await.map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

async fn download(url: &str, dir: &Path, progress: impl Fn(u64, Option<u64>)) -> Result<PathBuf, String> {
    let resp = client()?
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Download failed: {e}"))?;
    let total = resp.content_length();
    let name = url.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("package.dmg");
    let path = dir.join(name);
    let mut file = tokio::fs::File::create(&path).await.map_err(|e| e.to_string())?;
    let mut stream = resp.bytes_stream();
    let mut received = 0u64;
    let mut last = std::time::Instant::now();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download failed: {e}"))?;
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        received += chunk.len() as u64;
        if last.elapsed() > Duration::from_millis(200) {
            progress(received, total);
            last = std::time::Instant::now();
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    if total.is_some_and(|t| t != received) {
        return Err("The download was incomplete".into());
    }
    Ok(path)
}

/// Check the package against the release's SHA256SUMS (`<hex>  <file name>` lines).
async fn verify(package: &Path, sums_url: &str) -> Result<(), String> {
    let name = package.file_name().and_then(|n| n.to_str()).ok_or("bad file name")?;
    let text = client()?
        .get(sums_url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Couldn't check the download: {e}"))?
        .text()
        .await
        .map_err(|e| format!("Couldn't check the download: {e}"))?;
    let expected = expected_digest(&text, name).ok_or("The download isn't listed in the release's checksums")?;
    let bytes = std::fs::read(package).map_err(|e| e.to_string())?;
    let actual: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if actual == expected {
        Ok(())
    } else {
        Err("The download is damaged (its checksum doesn't match). Try again later.".into())
    }
}

fn expected_digest(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
    })
}

#[allow(dead_code)]
fn run(cmd: &mut std::process::Command) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("{:?}: {e}", cmd.get_program()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{:?} failed: {}", cmd.get_program(), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

#[allow(dead_code)]
fn no_permission(place: &Path) -> String {
    format!("Vessel can't write to {}. Move Vessel.app to a folder you own (like Applications), or install the update by hand.", place.display())
}

#[allow(dead_code)]
fn remove_any(path: &Path) {
    if path.is_dir() {
        let _ = std::fs::remove_dir_all(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}

/// The app bundle Vessel runs from (`…/Vessel.app`).
#[allow(dead_code)]
fn app_bundle() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bundle = exe.parent().and_then(Path::parent).and_then(Path::parent).ok_or("not in an app bundle")?;
    if bundle.extension().is_some_and(|e| e == "app") {
        Ok(bundle.to_path_buf())
    } else {
        Err("This copy of Vessel isn't running from Vessel.app, so it can't update itself. Download the update from the Releases page.".into())
    }
}

#[cfg(target_os = "macos")]
fn put_in_place(package: &Path, work: &Path) -> Result<PathBuf, String> {
    use std::process::Command;
    let bundle = app_bundle()?;
    let parent = bundle.parent().ok_or("no folder")?;
    let mount = work.join("mnt");
    std::fs::create_dir_all(&mount).map_err(|e| e.to_string())?;
    run(Command::new("hdiutil").args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"]).arg(&mount).arg(package))?;
    // Copy next to the current app so the swap is a rename on the same disk.
    let staged = parent.join(".Vessel-update.app");
    remove_any(&staged);
    let copied = run(Command::new("ditto").arg(mount.join("Vessel.app")).arg(&staged));
    let _ = run(Command::new("hdiutil").args(["detach", "-quiet", "-force"]).arg(&mount));
    copied.map_err(|e| if e.contains("ermission") { no_permission(parent) } else { e })?;
    let old = parent.join(".Vessel-old.app");
    remove_any(&old);
    std::fs::rename(&bundle, &old).map_err(|_| no_permission(parent))?;
    if std::fs::rename(&staged, &bundle).is_err() {
        let _ = std::fs::rename(&old, &bundle);
        return Err(no_permission(parent));
    }
    remove_any(&old);
    Ok(bundle)
}

#[cfg(not(target_os = "macos"))]
fn put_in_place(_package: &Path, _work: &Path) -> Result<PathBuf, String> {
    Err("Self-update is only available on macOS.".into())
}

/// Start `program` once this process has quit.
pub fn relaunch_after_exit(program: &Path) -> Result<(), String> {
    let pid = std::process::id();
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("while kill -0 {pid} 2>/dev/null; do sleep 0.2; done; open \"$0\""))
        .arg(program)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.6.2"), Some((0, 6, 2)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
    }

    #[test]
    fn picks_highest_release() {
        let json: serde_json::Value = serde_json::from_str(
            r#"[{"tag_name":"v0.1.0","html_url":"a"},{"tag_name":"v0.1.10","html_url":"b"},{"tag_name":"v0.2.0","html_url":"c","prerelease":true}]"#,
        )
        .unwrap();
        assert_eq!(newest(&json).map(|f| f.page), Some("b".into()));
        let sums = "abc  Vessel-0.1.0-macos-intel.dmg\n";
        assert_eq!(expected_digest(sums, "Vessel-0.1.0-macos-intel.dmg").as_deref(), Some("abc"));
    }
}
