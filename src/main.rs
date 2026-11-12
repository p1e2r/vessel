//! Vessel — a native macOS terminal with tabs, saved SSH / RDP / VNC
//! connections (passwords in the Keychain) and a translucent window.
//! UI: GPUI (Zed's GPU-accelerated framework). 100% Rust.

mod editor;
mod input;
mod palette;
mod probe;
mod profiles;
mod remote;
mod runtime;
mod settings;
mod settings_panel;
mod terminal;
mod theme;
mod updates;
mod workspace;

use gpui::{
    point, px, size, App, AppContext, Application, Bounds, Menu, MenuItem, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowOptions,
};

use settings::Settings;
use workspace::Workspace;

fn open_window(cx: &mut App) {
    let settings = cx.global::<Settings>().clone();
    let bounds = Bounds::centered(None, size(px(980.), px(640.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Vessel".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(14.), px(12.))),
        }),
        window_background: if settings.blur { WindowBackgroundAppearance::Blurred } else { WindowBackgroundAppearance::Transparent },
        window_min_size: Some(size(px(420.), px(260.))),
        app_id: Some("dev.vessel.terminal".into()),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| cx.new(|cx| Workspace::new(window, cx)))
        .expect("failed to open window");
}

fn main() {
    // Start the network runtime early so the first SSH/RDP tab connects quickly.
    let _ = runtime::rt();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|a| a == "--probe").unwrap_or(false) {
        std::process::exit(probe::run(&args[1..]));
    }

    // Several TLS users (RDP, updater) share rustls; pick one crypto provider for all of them.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    Application::new().run(|cx: &mut App| {
        cx.set_global(Settings::load());

        // Updates: repaint windows whenever the updater's state changes, and check
        // GitHub at launch and every 6 hours (unless turned off in Settings).
        let (updater, wake) = updates::Updater::new();
        cx.set_global(updater);
        cx.spawn(async move |cx| {
            while wake.recv_async().await.is_ok() {
                while wake.try_recv().is_ok() {}
                if cx.update(|cx| cx.refresh_windows()).is_err() {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |cx| loop {
            let _ = cx.update(|cx| {
                if cx.global::<Settings>().auto_update_check {
                    cx.global::<updates::Updater>().check(true);
                }
            });
            cx.background_executor().timer(std::time::Duration::from_secs(6 * 60 * 60)).await;
        })
        .detach();

        input::bind_keys(cx);
        terminal::view::bind_keys(cx);
        palette::bind_keys(cx);
        editor::bind_keys(cx);
        settings_panel::bind_keys(cx);
        workspace::bind_keys(cx);

        cx.set_menus(vec![
            Menu {
                name: "Vessel".into(),
                items: vec![
                    MenuItem::action("Settings…", workspace::OpenSettings),
                    MenuItem::action("Check for Updates…", workspace::CheckForUpdates),
                    MenuItem::action("Toggle Blur", workspace::ToggleBlur),
                    MenuItem::separator(),
                    MenuItem::action("Quit Vessel", workspace::Quit),
                ],
            },
            Menu {
                name: "Shell".into(),
                items: vec![
                    MenuItem::action("New Tab", workspace::NewTab),
                    MenuItem::action("Quick Connect…", workspace::QuickConnect),
                    MenuItem::action("New Connection…", workspace::NewConnection),
                    MenuItem::separator(),
                    MenuItem::action("Close Tab", workspace::CloseTab),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Connections Sidebar", workspace::ToggleSidebar),
                    MenuItem::separator(),
                    MenuItem::action("Bigger", workspace::FontBigger),
                    MenuItem::action("Smaller", workspace::FontSmaller),
                    MenuItem::action("Actual Size", workspace::FontReset),
                    MenuItem::separator(),
                    MenuItem::action("Next Tab", workspace::NextTab),
                    MenuItem::action("Previous Tab", workspace::PrevTab),
                ],
            },
        ]);

        // Quit from the menu bar even when no window is focused.
        cx.on_action(|_: &workspace::Quit, cx| cx.quit());
        cx.on_action(|_: &workspace::NewTab, cx| open_window(cx));

        open_window(cx);
        cx.activate(true);
    });
}
