//! The window: titlebar tabs, optional connections sidebar, the active tab,
//! and the quick-connect / editor overlays.

use gpui::{
    actions, div, prelude::*, px, AnyView, App, Context, Entity, FocusHandle, Focusable,
    KeyBinding, MouseButton, SharedString, Subscription, WindowBackgroundAppearance, Window,
};

use crate::editor::{Editor, EditorEvent};
use crate::palette::{Palette, PaletteEvent};
use crate::profiles::{secret_delete, secret_set, Kind, Profile, ProfileStore};
use crate::remote::{RemoteEvent, RemoteView, Status};
use crate::settings::Settings;
use crate::settings_panel::{SettingsEvent, SettingsPanel};
use crate::terminal::view::{TerminalEvent, TerminalKind, TerminalView};
use crate::theme;
use crate::updates::{UpdateStatus, Updater};

actions!(
    workspace,
    [
        NewTab,
        CloseTab,
        NextTab,
        PrevTab,
        QuickConnect,
        NewConnection,
        ToggleSidebar,
        FontBigger,
        FontSmaller,
        FontReset,
        OpenSettings,
        CheckForUpdates,
        ToggleBlur,
        Quit,
        Tab1,
        Tab2,
        Tab3,
        Tab4,
        Tab5,
        Tab6,
        Tab7,
        Tab8,
        Tab9
    ]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-t", NewTab, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("cmd-shift-]", NextTab, None),
        KeyBinding::new("cmd-shift-[", PrevTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PrevTab, None),
        KeyBinding::new("cmd-p", QuickConnect, None),
        KeyBinding::new("cmd-o", QuickConnect, None),
        KeyBinding::new("cmd-n", NewConnection, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("cmd-=", FontBigger, None),
        KeyBinding::new("cmd-+", FontBigger, None),
        KeyBinding::new("cmd--", FontSmaller, None),
        KeyBinding::new("cmd-0", FontReset, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-shift-u", ToggleBlur, None),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-1", Tab1, None),
        KeyBinding::new("cmd-2", Tab2, None),
        KeyBinding::new("cmd-3", Tab3, None),
        KeyBinding::new("cmd-4", Tab4, None),
        KeyBinding::new("cmd-5", Tab5, None),
        KeyBinding::new("cmd-6", Tab6, None),
        KeyBinding::new("cmd-7", Tab7, None),
        KeyBinding::new("cmd-8", Tab8, None),
        KeyBinding::new("cmd-9", Tab9, None),
    ]);
}

pub const TITLEBAR_H: f32 = 38.0;
const SIDEBAR_W: f32 = 240.0;

enum Content {
    Term(Entity<TerminalView>),
    Remote(Entity<RemoteView>),
}

struct Tab {
    content: Content,
    _subs: Vec<Subscription>,
}

impl Tab {
    fn view(&self) -> AnyView {
        match &self.content {
            Content::Term(v) => v.clone().into(),
            Content::Remote(v) => v.clone().into(),
        }
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.content {
            Content::Term(v) => v.focus_handle(cx),
            Content::Remote(v) => v.focus_handle(cx),
        }
    }

    fn title(&self, cx: &App) -> (Option<Kind>, String, bool) {
        match &self.content {
            Content::Term(v) => {
                let v = v.read(cx);
                let kind = match &v.kind {
                    TerminalKind::Local => None,
                    TerminalKind::Ssh(_) => Some(Kind::Ssh),
                };
                (kind, v.title.clone(), v.exited.is_some())
            }
            Content::Remote(v) => {
                let v = v.read(cx);
                let dead = matches!(v.status, Status::Closed(_) | Status::Error(_));
                (Some(v.profile.kind), v.profile.display_name(), dead)
            }
        }
    }

    fn shutdown(&self, cx: &App) {
        match &self.content {
            Content::Term(v) => v.read(cx).shutdown(),
            Content::Remote(v) => v.read(cx).shutdown(),
        }
    }
}

#[allow(dead_code)] // subscriptions are held only to keep them alive
enum Overlay {
    None,
    Palette(Entity<Palette>, Subscription),
    Editor(Entity<Editor>, Subscription),
    Settings(Entity<SettingsPanel>, Subscription),
}

pub struct Workspace {
    tabs: Vec<Tab>,
    active: usize,
    store: ProfileStore,
    overlay: Overlay,
    sidebar: bool,
    focus: FocusHandle,
    _settings_sub: Subscription,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut ws = Self {
            tabs: Vec::new(),
            active: 0,
            store: ProfileStore::load(),
            overlay: Overlay::None,
            sidebar: false,
            focus: cx.focus_handle(),
            // Settings changes (opacity, font…) repaint every tab right away.
            _settings_sub: cx.observe_global::<Settings>(|this: &mut Self, cx| {
                for t in &this.tabs {
                    match &t.content {
                        Content::Term(v) => v.update(cx, |_, cx| cx.notify()),
                        Content::Remote(v) => v.update(cx, |_, cx| cx.notify()),
                    }
                }
                cx.notify();
            }),
        };
        ws.new_local_tab(window, cx);
        ws
    }

    // ---------- tabs ----------

    fn push_tab(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.insert((self.active + 1).min(self.tabs.len()), tab);
        if self.tabs.len() > 1 {
            self.active = (self.active + 1).min(self.tabs.len() - 1);
        } else {
            self.active = 0;
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = self.tabs.get(self.active) {
            window.focus(&t.focus_handle(cx));
        } else {
            window.focus(&self.focus);
        }
    }

    fn make_terminal(&mut self, kind: TerminalKind, window: &mut Window, cx: &mut Context<Self>) -> Tab {
        let view = cx.new(|cx| TerminalView::new(kind, window, cx));
        let sub = cx.subscribe_in(&view, window, |this, view, ev: &TerminalEvent, window, cx| match ev {
            TerminalEvent::TitleChanged => cx.notify(),
            TerminalEvent::CloseRequested => {
                if let Some(i) = this.index_of_term(view) {
                    this.close_tab_at(i, window, cx);
                }
            }
            TerminalEvent::Reconnect => {
                let kind = view.read(cx).kind.clone();
                if let (Some(i), TerminalKind::Ssh(p)) = (this.index_of_term(view), kind) {
                    let fresh = this.store.get(&p.id).cloned().unwrap_or(p);
                    let tab = this.make_terminal(TerminalKind::Ssh(fresh), window, cx);
                    let old = std::mem::replace(&mut this.tabs[i], tab);
                    old.shutdown(cx);
                    this.focus_active(window, cx);
                    cx.notify();
                }
            }
            TerminalEvent::SavePassword(id, pw) => {
                if this.store.get(id).is_some() {
                    secret_set(id, pw);
                }
            }
        });
        Tab { content: Content::Term(view), _subs: vec![sub] }
    }

    fn make_remote(&mut self, p: Profile, window: &mut Window, cx: &mut Context<Self>) -> Tab {
        let vp = window.viewport_size();
        let sidebar = if self.sidebar { SIDEBAR_W } else { 0.0 };
        let auto = ((f32::from(vp.width) - sidebar) as u16, (f32::from(vp.height) - TITLEBAR_H) as u16);
        let view = cx.new(|cx| RemoteView::new(p, auto, window, cx));
        let sub = cx.subscribe_in(&view, window, |this, view, ev: &RemoteEvent, window, cx| match ev {
            RemoteEvent::TitleChanged => cx.notify(),
            RemoteEvent::Reconnect => {
                let p = view.read(cx).profile.clone();
                if let Some(i) = this.tabs.iter().position(|t| matches!(&t.content, Content::Remote(v) if v == view)) {
                    let fresh = this.store.get(&p.id).cloned().unwrap_or(p);
                    let tab = this.make_remote(fresh, window, cx);
                    let old = std::mem::replace(&mut this.tabs[i], tab);
                    old.shutdown(cx);
                    this.focus_active(window, cx);
                    cx.notify();
                }
            }
        });
        Tab { content: Content::Remote(view), _subs: vec![sub] }
    }

    fn index_of_term(&self, view: &Entity<TerminalView>) -> Option<usize> {
        self.tabs.iter().position(|t| matches!(&t.content, Content::Term(v) if v == view))
    }

    fn new_local_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.make_terminal(TerminalKind::Local, window, cx);
        self.push_tab(tab, window, cx);
    }

    pub fn open_profile(&mut self, p: Profile, window: &mut Window, cx: &mut Context<Self>) {
        if self.store.get(&p.id).is_some() {
            self.store.touch(&p.id);
        }
        let tab = match p.kind {
            Kind::Ssh => self.make_terminal(TerminalKind::Ssh(p), window, cx),
            Kind::Rdp | Kind::Vnc => self.make_remote(p, window, cx),
        };
        self.push_tab(tab, window, cx);
    }

    fn close_tab_at(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(i);
        tab.shutdown(cx);
        if self.tabs.is_empty() {
            window.remove_window();
            return;
        }
        if self.active >= self.tabs.len() || (i < self.active) {
            self.active = self.active.saturating_sub(1);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    fn activate(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i < self.tabs.len() {
            self.active = i;
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    // ---------- overlays ----------

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let profiles = self.store.sorted();
        let palette = cx.new(|cx| Palette::new(profiles, window, cx));
        let sub = cx.subscribe_in(&palette, window, |this, _, ev: &PaletteEvent, window, cx| {
            this.close_overlay(window, cx);
            match ev {
                PaletteEvent::Open(p) => this.open_profile(p.clone(), window, cx),
                PaletteEvent::Edit(p) => {
                    let is_new = this.store.get(&p.id).is_none();
                    this.open_editor(p.clone(), is_new, window, cx)
                }
                PaletteEvent::New => this.open_editor(Profile::default(), true, window, cx),
                PaletteEvent::LocalShell => this.new_local_tab(window, cx),
                PaletteEvent::Dismiss => {}
            }
        });
        self.overlay = Overlay::Palette(palette, sub);
        cx.notify();
    }

    fn open_editor(&mut self, p: Profile, is_new: bool, window: &mut Window, cx: &mut Context<Self>) {
        let editor = cx.new(|cx| Editor::new(p, is_new, window, cx));
        let sub = cx.subscribe_in(&editor, window, |this, _, ev: &EditorEvent, window, cx| match ev {
            EditorEvent::Saved { profile, connect, password, forget_password } => {
                if *forget_password {
                    secret_delete(&profile.id);
                }
                if let Some(pw) = password {
                    secret_set(&profile.id, pw);
                }
                this.store.upsert(profile.clone());
                this.close_overlay(window, cx);
                if *connect {
                    this.open_profile(profile.clone(), window, cx);
                }
            }
            EditorEvent::Deleted(id) => {
                this.store.remove(id);
                this.close_overlay(window, cx);
            }
            EditorEvent::Cancel => this.close_overlay(window, cx),
        });
        self.overlay = Overlay::Editor(editor, sub);
        cx.notify();
    }

    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = Overlay::None;
        self.focus_active(window, cx);
        cx.notify();
    }

    // ---------- actions ----------

    fn act_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        self.new_local_tab(window, cx);
    }
    fn act_close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.overlay, Overlay::None) {
            self.close_overlay(window, cx);
            return;
        }
        self.close_tab_at(self.active, window, cx);
    }
    fn act_next(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() {
            self.activate((self.active + 1) % self.tabs.len(), window, cx);
        }
    }
    fn act_prev(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() {
            self.activate((self.active + self.tabs.len() - 1) % self.tabs.len(), window, cx);
        }
    }
    fn act_quick(&mut self, _: &QuickConnect, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, Overlay::Palette(..)) {
            self.close_overlay(window, cx);
        } else {
            self.open_palette(window, cx);
        }
    }
    fn act_new_conn(&mut self, _: &NewConnection, window: &mut Window, cx: &mut Context<Self>) {
        self.open_editor(Profile::default(), true, window, cx);
    }
    fn act_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar = !self.sidebar;
        cx.notify();
    }
    fn change_font(&mut self, delta: f32, cx: &mut Context<Self>) {
        let mut s = cx.global::<Settings>().clone();
        s.font_size = if delta == 0.0 { Settings::default().font_size } else { (s.font_size + delta).clamp(8.0, 32.0) };
        s.save();
        cx.set_global(s);
        cx.notify();
        for t in &self.tabs {
            if let Content::Term(v) = &t.content {
                v.update(cx, |_, cx| cx.notify());
            }
        }
    }
    fn act_bigger(&mut self, _: &FontBigger, _: &mut Window, cx: &mut Context<Self>) {
        self.change_font(1.0, cx);
    }
    fn act_smaller(&mut self, _: &FontSmaller, _: &mut Window, cx: &mut Context<Self>) {
        self.change_font(-1.0, cx);
    }
    fn act_reset(&mut self, _: &FontReset, _: &mut Window, cx: &mut Context<Self>) {
        self.change_font(0.0, cx);
    }
    fn act_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, Overlay::Settings(..)) {
            self.close_overlay(window, cx);
        } else {
            self.open_settings(window, cx);
        }
    }

    fn act_check_updates(&mut self, _: &CheckForUpdates, window: &mut Window, cx: &mut Context<Self>) {
        cx.global::<Updater>().check(false);
        if !matches!(self.overlay, Overlay::Settings(..)) {
            self.open_settings(window, cx);
        }
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = cx.new(|cx| SettingsPanel::new(window, cx));
        let sub = cx.subscribe_in(&panel, window, |this, _, ev: &SettingsEvent, window, cx| match ev {
            SettingsEvent::Close => this.close_overlay(window, cx),
        });
        self.overlay = Overlay::Settings(panel, sub);
        cx.notify();
    }
    fn act_blur(&mut self, _: &ToggleBlur, window: &mut Window, cx: &mut Context<Self>) {
        let mut s = cx.global::<Settings>().clone();
        s.blur = !s.blur;
        window.set_background_appearance(if s.blur { WindowBackgroundAppearance::Blurred } else { WindowBackgroundAppearance::Transparent });
        s.save();
        cx.set_global(s);
        cx.notify();
    }
    fn act_quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        for t in &self.tabs {
            t.shutdown(cx);
        }
        cx.quit();
    }

    // ---------- rendering ----------

    fn render_titlebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self.tabs.iter().enumerate().map(|(i, t)| {
            let (kind, title, dead) = t.title(cx);
            let active = i == self.active;
            div()
                .id(("tab", i))
                .group("tab")
                .flex()
                .items_center()
                .gap_2()
                .h(px(26.))
                .min_w(px(110.))
                .max_w(px(220.))
                .flex_shrink()
                .pl_3()
                .pr_1()
                .rounded_md()
                .cursor_pointer()
                .when(active, |d| d.bg(theme::active_tab()))
                .when(!active, |d| d.hover(|s| s.bg(theme::hover())))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| this.activate(i, window, cx)))
                .on_mouse_down(MouseButton::Middle, cx.listener(move |this, _, window, cx| this.close_tab_at(i, window, cx)))
                .when_some(kind, |d, k| d.child(kind_dot(k, dead)))
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(px(12.))
                        .text_color(if active { theme::hex(theme::FG) } else { theme::hex(theme::MUTED) })
                        .child(SharedString::from(title)),
                )
                .child(
                    div()
                        .id(("close", i))
                        .size(px(18.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_sm()
                        .text_size(px(12.))
                        .text_color(theme::hex(theme::MUTED))
                        .invisible()
                        .when(active, |d| d.visible())
                        .group_hover("tab", |s| s.visible())
                        .hover(|s| s.bg(theme::hover()).text_color(theme::hex(theme::FG)))
                        .child("✕")
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_tab_at(i, window, cx)
                        })),
                )
        });

        let _ = window;
        div()
            .id("titlebar")
            .h(px(TITLEBAR_H))
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .pl(px(80.)) // room for the traffic lights
            .pr_2()
            .border_b_1()
            .border_color(theme::border())
            .on_click(|ev, window, _| {
                if ev.click_count() == 2 {
                    window.titlebar_double_click();
                }
            })
            .child(div().flex().items_center().gap_1().overflow_hidden().children(tabs))
            .child(icon_button("new-tab", "+", "New tab  ⌘T").on_click(cx.listener(|this, _, window, cx| this.new_local_tab(window, cx))))
            .child(div().flex_1().h_full())
            .children(update_pill(cx))
            .child(
                icon_button("quick", "⌕  Connect", "Quick connect  ⌘P")
                    .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx))),
            )
            .child(icon_button("settings", "⚙", "Settings  ⌘,").on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx))))
            .child(
                icon_button("sidebar", "☰", "Connections  ⌘B")
                    .when(self.sidebar, |d| d.bg(theme::active_tab()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar = !this.sidebar;
                        cx.notify();
                    })),
            )
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let profiles = self.store.sorted();
        let groups = [Kind::Ssh, Kind::Rdp, Kind::Vnc];
        let mut list = div().flex().flex_col().gap(px(2.));
        for kind in groups {
            let items: Vec<Profile> = profiles.iter().filter(|p| p.kind == kind).cloned().collect();
            if items.is_empty() {
                continue;
            }
            list = list.child(
                div()
                    .px_2()
                    .pt_3()
                    .pb_1()
                    .text_size(px(10.))
                    .text_color(theme::hex(theme::SUBTLE))
                    .child(match kind {
                        Kind::Ssh => "SSH",
                        Kind::Rdp => "REMOTE DESKTOP",
                        Kind::Vnc => "VNC",
                    }),
            );
            for p in items {
                let p_open = p.clone();
                let p_edit = p.clone();
                let id: SharedString = p.id.clone().into();
                list = list.child(
                    div()
                        .id(id.clone())
                        .group("conn")
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py(px(5.))
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(move |this, _, window, cx| this.open_profile(p_open.clone(), window, cx)))
                        .child(kind_dot(p.kind, false))
                        .child(
                            div()
                                .flex_1()
                                .overflow_hidden()
                                .flex()
                                .flex_col()
                                .child(div().text_size(px(12.)).text_color(theme::hex(theme::FG)).whitespace_nowrap().text_ellipsis().child(p.display_name()))
                                .child(div().text_size(px(10.)).text_color(theme::hex(theme::MUTED)).whitespace_nowrap().text_ellipsis().child(format!("{}:{}", p.host, p.port))),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("edit-{}", p.id)))
                                .invisible()
                                .group_hover("conn", |s| s.visible())
                                .px_1()
                                .rounded_sm()
                                .text_size(px(11.))
                                .text_color(theme::hex(theme::MUTED))
                                .hover(|s| s.bg(theme::hover()).text_color(theme::hex(theme::FG)))
                                .child("Edit")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.open_editor(p_edit.clone(), false, window, cx)
                                })),
                        ),
                );
            }
        }
        if profiles.is_empty() {
            list = list.child(
                div()
                    .p_3()
                    .text_size(px(12.))
                    .text_color(theme::hex(theme::MUTED))
                    .child("No saved connections yet. Add SSH, RDP or VNC hosts and their passwords are kept in your Keychain."),
            );
        }

        div()
            .w(px(SIDEBAR_W))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme::border())
            .bg(theme::hexa(0x000000, 0.12))
            .child(
                div()
                    .flex()
                    .items_center()
                    .px_3()
                    .pt_2()
                    .child(div().flex_1().text_size(px(12.)).text_color(theme::hex(theme::MUTED)).child("Connections"))
                    .child(icon_button("add-conn", "+ New", "New connection  ⌘N").on_click(cx.listener(|this, _, window, cx| {
                        this.open_editor(Profile::default(), true, window, cx)
                    }))),
            )
            .child(div().id("conn-list").flex_1().overflow_y_scroll().px_1().child(list))
    }
}

/// Titlebar button shown while an update is available / downloading / ready.
fn update_pill(cx: &mut Context<Workspace>) -> Option<impl IntoElement> {
    let status = cx.global::<Updater>().get();
    let (label, clickable): (String, bool) = match &status {
        UpdateStatus::Available { version, .. } => (format!("↑ Update to {version}"), true),
        UpdateStatus::Downloading { received, total: Some(t), .. } if *t > 0 => {
            (format!("Downloading {}%", received * 100 / t), false)
        }
        UpdateStatus::Downloading { .. } => ("Downloading…".into(), false),
        UpdateStatus::Installing { .. } => ("Installing…".into(), false),
        UpdateStatus::Installed { .. } => ("↻ Restart to update".into(), true),
        _ => return None,
    };
    Some(
        div()
            .id("update-pill")
            .h(px(24.))
            .px_3()
            .flex()
            .items_center()
            .rounded_full()
            .text_size(px(12.))
            .text_color(theme::hex(0x0e1116))
            .bg(theme::hex(theme::ACCENT))
            .when(clickable, |d| d.cursor_pointer().hover(|s| s.opacity(0.85)))
            .when(!clickable, |d| d.opacity(0.75))
            .child(label)
            .on_click(|_, _, cx| match cx.global::<Updater>().get() {
                UpdateStatus::Available { .. } => cx.global::<Updater>().install(),
                UpdateStatus::Installed { .. } => restart_into_update(cx),
                _ => {}
            }),
    )
}

/// Quit and reopen the freshly installed Vessel.app.
pub fn restart_into_update(cx: &mut App) {
    if let UpdateStatus::Installed { program, .. } = cx.global::<Updater>().get() {
        if crate::updates::relaunch_after_exit(&program).is_ok() {
            cx.quit();
        }
    }
}

fn kind_dot(kind: Kind, dead: bool) -> impl IntoElement {
    let color = match kind {
        Kind::Ssh => theme::OK,
        Kind::Rdp => theme::ACCENT,
        Kind::Vnc => 0xbb9af7,
    };
    div().flex_none().size(px(7.)).rounded_full().bg(if dead { theme::hex(theme::SUBTLE) } else { theme::hex(color) })
}

fn icon_button(id: &'static str, label: &'static str, _tooltip: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(26.))
        .px_2()
        .flex()
        .items_center()
        .rounded_md()
        .cursor_pointer()
        .text_size(px(13.))
        .text_color(theme::hex(theme::MUTED))
        .hover(|s| s.bg(theme::hover()).text_color(theme::hex(theme::FG)))
        .child(label)
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = cx.global::<Settings>().clone();
        let bg = theme::hexa(theme::BG, settings.opacity.clamp(0.0, 1.0));
        let active_view = self.tabs.get(self.active).map(|t| t.view());
        let titlebar = self.render_titlebar(window, cx);
        let sidebar = self.sidebar.then(|| self.render_sidebar(cx));

        let overlay = match &self.overlay {
            Overlay::None => None,
            Overlay::Palette(p, _) => Some((p.clone().into_any_element(), true)),
            Overlay::Editor(e, _) => Some((e.clone().into_any_element(), false)),
            Overlay::Settings(p, _) => Some((p.clone().into_any_element(), false)),
        };

        div()
            .key_context("Workspace")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::act_new_tab))
            .on_action(cx.listener(Self::act_close_tab))
            .on_action(cx.listener(Self::act_next))
            .on_action(cx.listener(Self::act_prev))
            .on_action(cx.listener(Self::act_quick))
            .on_action(cx.listener(Self::act_new_conn))
            .on_action(cx.listener(Self::act_sidebar))
            .on_action(cx.listener(Self::act_bigger))
            .on_action(cx.listener(Self::act_smaller))
            .on_action(cx.listener(Self::act_reset))
            .on_action(cx.listener(Self::act_settings))
            .on_action(cx.listener(Self::act_check_updates))
            .on_action(cx.listener(Self::act_blur))
            .on_action(cx.listener(Self::act_quit))
            .on_action(cx.listener(|this, _: &Tab1, w, cx| this.activate(0, w, cx)))
            .on_action(cx.listener(|this, _: &Tab2, w, cx| this.activate(1, w, cx)))
            .on_action(cx.listener(|this, _: &Tab3, w, cx| this.activate(2, w, cx)))
            .on_action(cx.listener(|this, _: &Tab4, w, cx| this.activate(3, w, cx)))
            .on_action(cx.listener(|this, _: &Tab5, w, cx| this.activate(4, w, cx)))
            .on_action(cx.listener(|this, _: &Tab6, w, cx| this.activate(5, w, cx)))
            .on_action(cx.listener(|this, _: &Tab7, w, cx| this.activate(6, w, cx)))
            .on_action(cx.listener(|this, _: &Tab8, w, cx| this.activate(7, w, cx)))
            .on_action(cx.listener(|this, _: &Tab9, w, cx| {
                let last = this.tabs.len().saturating_sub(1);
                this.activate(last, w, cx)
            }))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(bg)
            .text_color(theme::hex(theme::FG))
            .font_family(".SystemUIFont")
            .child(titlebar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .children(sidebar)
                    .child(div().flex_1().min_w_0().h_full().children(active_view)),
            )
            .when_some(overlay, |d, (el, top)| {
                d.child(
                    div()
                        .id("overlay-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(theme::hexa(0x000000, 0.35))
                        .flex()
                        .flex_col()
                        .items_center()
                        .when(top, |d| d.pt(px(70.)))
                        .when(!top, |d| d.justify_center())
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close_overlay(window, cx)))
                        .child(div().id("overlay-body").on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(el)),
                )
            })
    }
}
