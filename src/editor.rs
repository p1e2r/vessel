//! Connection editor sheet (create / edit SSH, RDP and VNC profiles).

use gpui::{
    actions, div, prelude::*, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    KeyBinding, SharedString, Window,
};

use crate::input::TextInput;
use crate::profiles::{Kind, Profile, ScaleMode, VncEncodingPref};
use crate::theme;

actions!(editor, [FocusNext, FocusPrev, SaveConnect, Save, Cancel]);

pub fn bind_keys(cx: &mut App) {
    let ctx = Some("Editor");
    cx.bind_keys([
        KeyBinding::new("tab", FocusNext, ctx),
        KeyBinding::new("shift-tab", FocusPrev, ctx),
        KeyBinding::new("enter", SaveConnect, ctx),
        KeyBinding::new("cmd-s", Save, ctx),
        KeyBinding::new("escape", Cancel, ctx),
    ]);
}

pub enum EditorEvent {
    Saved { profile: Profile, connect: bool, password: Option<String>, forget_password: bool },
    Deleted(String),
    Cancel,
}

struct Fields {
    name: Entity<TextInput>,
    host: Entity<TextInput>,
    port: Entity<TextInput>,
    user: Entity<TextInput>,
    password: Entity<TextInput>,
    key_path: Entity<TextInput>,
    remote_cmd: Entity<TextInput>,
    domain: Entity<TextInput>,
    width: Entity<TextInput>,
    height: Entity<TextInput>,
    fps: Entity<TextInput>,
}

pub struct Editor {
    p: Profile,
    is_new: bool,
    had_secret: bool,
    forget_secret: bool,
    f: Fields,
    error: Option<SharedString>,
    focus: FocusHandle,
}

impl EventEmitter<EditorEvent> for Editor {}

impl Editor {
    pub fn new(p: Profile, is_new: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let had_secret = !is_new && p.password().is_some();
        let mk = |cx: &mut Context<Self>, ph: &str, v: String| cx.new(|cx| TextInput::new(cx, ph.to_string(), v));
        let num = |n: u16| if n == 0 { String::new() } else { n.to_string() };
        let f = Fields {
            name: mk(cx, "My server (optional)", p.name.clone()),
            host: mk(cx, "hostname or IP", p.host.clone()),
            port: mk(cx, "port", p.port.to_string()),
            user: mk(cx, "username", p.username.clone()),
            password: cx.new(|cx| {
                TextInput::new(cx, if had_secret { "•••••• saved in Keychain — type to replace" } else { "password (saved to Keychain)" }, "")
                    .masked(true)
            }),
            key_path: mk(cx, "~/.ssh/id_ed25519 (optional — agent & default keys are tried)", p.key_path.clone()),
            remote_cmd: mk(cx, "run a command instead of the shell (optional)", p.remote_command.clone()),
            domain: mk(cx, "domain (optional)", p.domain.clone()),
            width: mk(cx, "auto", num(p.width)),
            height: mk(cx, "auto", num(p.height)),
            fps: mk(cx, "60", p.fps.to_string()),
        };
        window.focus(&f.host.focus_handle(cx));
        Self { p, is_new, had_secret, forget_secret: false, f, error: None, focus: cx.focus_handle() }
    }

    fn visible_inputs(&self) -> Vec<Entity<TextInput>> {
        let f = &self.f;
        let mut v = vec![f.name.clone(), f.host.clone(), f.port.clone(), f.user.clone(), f.password.clone()];
        match self.p.kind {
            Kind::Ssh => v.extend([f.key_path.clone(), f.remote_cmd.clone()]),
            Kind::Rdp => v.extend([f.domain.clone(), f.width.clone(), f.height.clone(), f.fps.clone()]),
            Kind::Vnc => v.extend([f.width.clone(), f.height.clone(), f.fps.clone()]),
        }
        v
    }

    fn cycle(&mut self, dir: i32, window: &mut Window, cx: &mut Context<Self>) {
        let inputs = self.visible_inputs();
        let cur = inputs.iter().position(|i| i.focus_handle(cx).is_focused(window)).unwrap_or(0) as i32;
        let n = inputs.len() as i32;
        let next = ((cur + dir) % n + n) % n;
        window.focus(&inputs[next as usize].focus_handle(cx));
    }

    fn focus_next(&mut self, _: &FocusNext, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle(1, window, cx);
    }
    fn focus_prev(&mut self, _: &FocusPrev, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle(-1, window, cx);
    }

    fn set_kind(&mut self, kind: Kind, cx: &mut Context<Self>) {
        let port_txt = self.f.port.read(cx).text();
        if port_txt.trim().is_empty() || port_txt.trim() == self.p.kind.default_port().to_string() {
            self.f.port.update(cx, |i, cx| i.set_text(kind.default_port().to_string(), cx));
        }
        self.p.kind = kind;
        cx.notify();
    }

    fn collect(&mut self, cx: &mut Context<Self>) -> Option<(Profile, Option<String>)> {
        let t = |e: &Entity<TextInput>, cx: &App| e.read(cx).text().trim().to_string();
        let mut p = self.p.clone();
        p.name = t(&self.f.name, cx);
        p.host = t(&self.f.host, cx);
        p.username = t(&self.f.user, cx);
        p.key_path = t(&self.f.key_path, cx);
        p.remote_command = t(&self.f.remote_cmd, cx);
        p.domain = t(&self.f.domain, cx);
        if p.host.is_empty() {
            self.error = Some("Host is required".into());
            return None;
        }
        // Allow "user@host" in the host field.
        if let Some((u, h)) = p.host.clone().split_once('@') {
            if p.username.is_empty() {
                p.username = u.to_string();
            }
            p.host = h.to_string();
        }
        let port = t(&self.f.port, cx);
        p.port = if port.is_empty() {
            p.kind.default_port()
        } else {
            match port.parse() {
                Ok(n) => n,
                Err(_) => {
                    self.error = Some("Port must be a number".into());
                    return None;
                }
            }
        };
        let parse_dim = |s: String| -> Result<u16, ()> { if s.is_empty() || s == "auto" { Ok(0) } else { s.parse().map_err(|_| ()) } };
        match (parse_dim(t(&self.f.width, cx)), parse_dim(t(&self.f.height, cx))) {
            (Ok(w), Ok(h)) => {
                if (w == 0) != (h == 0) {
                    self.error = Some("Set both width and height, or leave both on auto".into());
                    return None;
                }
                p.width = w;
                p.height = h;
            }
            _ => {
                self.error = Some("Resolution must be numbers (e.g. 1920 × 1080) or auto".into());
                return None;
            }
        }
        let fps = t(&self.f.fps, cx);
        p.fps = if fps.is_empty() { 60 } else { fps.parse::<u16>().unwrap_or(60).clamp(1, 240) };
        let pw = self.f.password.read(cx).text();
        Some((p, (!pw.is_empty()).then_some(pw)))
    }

    fn save_inner(&mut self, connect: bool, cx: &mut Context<Self>) {
        if let Some((profile, password)) = self.collect(cx) {
            cx.emit(EditorEvent::Saved { profile, connect, password, forget_password: self.forget_secret });
        } else {
            cx.notify();
        }
    }

    fn save_connect(&mut self, _: &SaveConnect, _: &mut Window, cx: &mut Context<Self>) {
        self.save_inner(true, cx);
    }
    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        self.save_inner(false, cx);
    }
    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(EditorEvent::Cancel);
    }
}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn label(text: &'static str) -> impl IntoElement {
    div().w(px(110.)).flex_none().text_size(px(12.)).text_color(theme::hex(theme::MUTED)).child(text)
}

fn row(l: &'static str, child: impl IntoElement) -> impl IntoElement {
    div().flex().items_center().gap_3().child(label(l)).child(div().flex_1().child(child))
}

fn section(title: &'static str) -> impl IntoElement {
    div()
        .pt_2()
        .text_size(px(11.))
        .text_color(theme::hex(theme::SUBTLE))
        .child(title.to_uppercase())
}

fn segmented<T: PartialEq + Copy + 'static>(
    id: &'static str,
    options: &[(T, &'static str)],
    current: T,
    cx: &mut Context<Editor>,
    on_pick: impl Fn(&mut Editor, T, &mut Context<Editor>) + Clone + 'static,
) -> impl IntoElement {
    div()
        .flex()
        .p(px(2.))
        .gap(px(2.))
        .rounded_md()
        .bg(theme::hexa(0x000000, 0.25))
        .border_1()
        .border_color(theme::border())
        .children(options.iter().enumerate().map(|(i, (v, text))| {
            let v = *v;
            let on_pick = on_pick.clone();
            let active = v == current;
            div()
                .id((id, i))
                .px_3()
                .py(px(3.))
                .rounded_sm()
                .text_size(px(12.))
                .cursor_pointer()
                .text_color(if active { theme::hex(theme::FG) } else { theme::hex(theme::MUTED) })
                .when(active, |d| d.bg(theme::hexa(theme::ACCENT, 0.25)))
                .hover(|s| s.bg(theme::hover()))
                .child(*text)
                .on_click(cx.listener(move |this, _, _, cx| {
                    on_pick(this, v, cx);
                    cx.notify();
                }))
        }))
}

fn toggle(
    id: &'static str,
    text: &'static str,
    on: bool,
    cx: &mut Context<Editor>,
    flip: impl Fn(&mut Editor) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .text_size(px(12.))
        .text_color(theme::hex(theme::FG))
        .child(
            div()
                .w(px(28.))
                .h(px(16.))
                .rounded_full()
                .p(px(2.))
                .bg(if on { theme::hex(theme::ACCENT) } else { theme::hexa(0xffffff, 0.15) })
                .flex()
                .when(on, |d| d.justify_end())
                .child(div().size(px(12.)).rounded_full().bg(theme::hex(0xffffff))),
        )
        .child(text)
        .on_click(cx.listener(move |this, _, _, cx| {
            flip(this);
            cx.notify();
        }))
}

fn button(id: &'static str, text: &'static str, primary: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_3()
        .py(px(5.))
        .rounded_md()
        .text_size(px(12.))
        .cursor_pointer()
        .text_color(if primary { theme::hex(0x0e1116) } else { theme::hex(theme::FG) })
        .bg(if primary { theme::hex(theme::ACCENT) } else { theme::hexa(0xffffff, 0.08) })
        .hover(|s| s.opacity(0.85))
        .child(text)
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let max_h = window.viewport_size().height - px(60.);
        let kind = self.p.kind;
        let f = &self.f;
        let title = if self.is_new { "New connection" } else { "Edit connection" };

        let mut body = div().flex().flex_col().gap_2();
        body = body
            .child(row("Type", segmented("kind", &[(Kind::Ssh, "SSH"), (Kind::Rdp, "RDP (Windows)"), (Kind::Vnc, "VNC")], kind, cx, |e, k, cx| e.set_kind(k, cx))))
            .child(row("Name", f.name.clone()))
            .child(row("Host", div().flex().gap_2().child(div().flex_1().child(f.host.clone())).child(div().w(px(80.)).child(f.port.clone()))))
            .child(row("Username", f.user.clone()))
            .child(row(
                if kind == Kind::Ssh { "Password / passphrase" } else { "Password" },
                div().flex().flex_col().gap_1().child(f.password.clone()).when(self.had_secret, |d| {
                    d.child(toggle("forget", "Forget saved password", self.forget_secret, cx, |e| e.forget_secret = !e.forget_secret))
                }),
            ));

        match kind {
            Kind::Ssh => {
                body = body
                    .child(section("SSH"))
                    .child(row("Private key", f.key_path.clone()))
                    .child(row("Command", f.remote_cmd.clone()));
            }
            Kind::Rdp | Kind::Vnc => {
                if kind == Kind::Rdp {
                    body = body.child(section("Windows")).child(row("Domain", f.domain.clone()));
                }
                body = body
                    .child(section("Display"))
                    .child(row(
                        "Resolution",
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().w(px(80.)).child(f.width.clone()))
                            .child(div().text_color(theme::hex(theme::MUTED)).child("×"))
                            .child(div().w(px(80.)).child(f.height.clone()))
                            .child(div().text_size(px(11.)).text_color(theme::hex(theme::SUBTLE)).child(if kind == Kind::Vnc {
                                "auto = server size"
                            } else {
                                "auto = tab size"
                            })),
                    ))
                    .child(row("Frame rate", div().flex().items_center().gap_2().child(div().w(px(80.)).child(f.fps.clone())).child(div().text_size(px(11.)).text_color(theme::hex(theme::SUBTLE)).child("fps"))))
                    .child(row("Scaling", segmented("scale", &[(ScaleMode::Fit, "Fit to tab"), (ScaleMode::Actual, "1:1 pixels")], self.p.scale, cx, |e, v, _| e.p.scale = v)));
                if kind == Kind::Rdp {
                    body = body
                        .child(row("Colour depth", segmented("depth", &[(16u32, "16-bit"), (24, "24-bit"), (32, "32-bit")], self.p.color_depth, cx, |e, v, _| e.p.color_depth = v)))
                        .child(section("Options"))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_x_4()
                                .gap_y_2()
                                .child(toggle("nla", "Network Level Auth (NLA)", self.p.nla, cx, |e| e.p.nla = !e.p.nla))
                                .child(toggle("wall", "Show wallpaper", self.p.show_wallpaper, cx, |e| e.p.show_wallpaper = !e.p.show_wallpaper))
                                .child(toggle("smooth", "Font smoothing", self.p.font_smoothing, cx, |e| e.p.font_smoothing = !e.p.font_smoothing))
                                .child(toggle("comp", "Desktop effects", self.p.desktop_composition, cx, |e| e.p.desktop_composition = !e.p.desktop_composition))
                                .child(toggle("cmd", "⌘ acts as Ctrl", self.p.cmd_as_ctrl, cx, |e| e.p.cmd_as_ctrl = !e.p.cmd_as_ctrl)),
                        );
                } else {
                    body = body
                        .child(row(
                            "Encoding",
                            segmented(
                                "enc",
                                &[(VncEncodingPref::Tight, "Tight (fast)"), (VncEncodingPref::Zrle, "ZRLE (lossless)"), (VncEncodingPref::Raw, "Raw (LAN)")],
                                self.p.vnc_encoding,
                                cx,
                                |e, v, _| e.p.vnc_encoding = v,
                            ),
                        ))
                        .child(section("Options"))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_x_4()
                                .gap_y_2()
                                .child(toggle("view", "View only", self.p.view_only, cx, |e| e.p.view_only = !e.p.view_only))
                                .child(toggle("shared", "Shared session", self.p.shared, cx, |e| e.p.shared = !e.p.shared))
                                .child(toggle("cmd", "⌘ acts as Ctrl", self.p.cmd_as_ctrl, cx, |e| e.p.cmd_as_ctrl = !e.p.cmd_as_ctrl)),
                        );
                }
            }
        }

        let id = self.p.id.clone();
        div()
            .key_context("Editor")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::focus_next))
            .on_action(cx.listener(Self::focus_prev))
            .on_action(cx.listener(Self::save_connect))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::cancel))
            .w(px(600.))
            .max_h(max_h)
            .p_4()
            .rounded_xl()
            .bg(theme::panel())
            .border_1()
            .border_color(theme::border())
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_3()
            .text_color(theme::hex(theme::FG))
            .child(div().text_size(px(15.)).child(title))
            .child(div().id("editor-body").flex_1().min_h_0().overflow_y_scroll().pr_1().child(body))
            .when_some(self.error.clone(), |d, e| d.child(div().text_size(px(12.)).text_color(theme::hex(theme::DANGER)).child(e)))
            .child(
                div()
                    .pt_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(!self.is_new, |d| {
                        d.child(button("delete", "Delete", false).text_color(theme::hex(theme::DANGER)).on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(EditorEvent::Deleted(id.clone()))
                        })))
                    })
                    .child(div().flex_1())
                    .child(button("cancel", "Cancel", false).on_click(cx.listener(|_, _, _, cx| cx.emit(EditorEvent::Cancel))))
                    .child(button("save", "Save", false).on_click(cx.listener(|this, _, _, cx| this.save_inner(false, cx))))
                    .child(button("connect", "Save & Connect", true).on_click(cx.listener(|this, _, _, cx| this.save_inner(true, cx)))),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme::hex(theme::SUBTLE))
                    .child("Passwords are stored in the macOS Keychain. ⇥ next field · ↵ save & connect · esc cancel"),
            )
    }
}
