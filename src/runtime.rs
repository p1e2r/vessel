use std::sync::OnceLock;
use tokio::runtime::Runtime;

/// Shared Tokio runtime for network sessions (SSH, RDP, VNC). The UI runs on
/// GPUI's own executor; the two talk over channels.
pub fn rt() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(3)
            .thread_name("vessel-net")
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}
