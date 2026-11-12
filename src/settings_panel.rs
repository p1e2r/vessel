//! ⌘, settings sheet: window transparency, blur, start folder, font size.
//! Changes apply live and are saved to settings.json.

use gpui::{
    actions, div, prelude::*, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    KeyBinding, PathPromptOptions, SharedString, WindowBackgroundAppearance, Window,
};

use crate::input::TextInput;
use crate::settings::{config_dir, Settings};
use crate::theme;

actions!(settings_panel, [Close, Apply]);

pub fn bind_keys(cx: &mut App) {
    let ctx = Some("SettingsPanel");
    cx.bind_keys([KeyBinding::new("escape", Close, ctx), KeyBinding::new("enter", Apply, ctx)]);
}

pub enum SettingsEvent {
    Close,
}

pub struct SettingsPanel {
    start_dir: Entity<TextInput>,
    error: Option<SharedString>,
    focus: FocusHandle,
}

impl EventEmitter<SettingsEvent> for SettingsPanel {}

const OPACITY_STEPS: [f32; 7] = [1.0, 0.95, 0.9, 0.8, 0.7, 0.6, 0.5];

impl SettingsPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dir = cx.global::<Settings>().start_dir.clone();
        let start_dir = cx.new(|cx| TextInput::new(cx, "~ (your home folder)", dir));
        let focus = cx.focus_handle();
        window.focus(&focus);
        Self { start_dir, error: None, focus }
    }

    fn update(cx: &mut App, f: impl FnOnce(&mut Settings)) {
        let mut s = cx.global::<Settings>().clone();
        f(&mut s);
        s.save();
        cx.set_global(s);
    }

    fn set_opacity(&mut self, v: f32, cx: &mut Context<Self>) {
        Self::update(cx, |s| s.opacity = v.clamp(0.2, 1.0));
        cx.notify();
    }

    fn set_blur(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        Self::update(cx, |s| s.blur = on);
        window.set_background_appearance(if on { WindowBackgroundAppearance::Blurred } else { WindowBackgroundAppearance::Transparent });
        cx.notify();
    }

    /// Validate and save the start folder. Returns false (and shows why) if it doesn't exist.
    fn save_dir(&mut self, cx: &mut Context<Self>) -> bool {
        let raw = self.start_dir.read(cx).text().trim().to_string();
        if !raw.is_empty() && !crate::profiles::expand_tilde(&raw).is_dir() {
            self.error = Some(format!("“{raw}” isn't a folder").into());
            cx.notify();
            return false;
        }
        self.error = None;
        Self::update(cx, |s| s.start_dir = raw);
        cx.notify();
        true
    }

    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Start new tabs here".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(p) = paths.into_iter().next() {
                    let home = dirs::home_dir().unwrap_or_default();
                    let shown = match p.strip_prefix(&home) {
                        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
                        Ok(rest) => format!("~/{}", rest.display()),
                        Err(_) => p.display().to_string(),
                    };
                    let _ = this.update(cx, |this, cx| {
                        this.start_dir.update(cx, |i, cx| i.set_text(shown, cx));
                        this.save_dir(cx);
                    });
                }
            }
        })
        .detach();
    }

    fn close(&mut self, _: &Close, _: &mut Window, cx: &mut Context<Self>) {
        if self.save_dir(cx) {
            cx.emit(SettingsEvent::Close);
        }
    }

    fn apply(&mut self, _: &Apply, _: &mut Window, cx: &mut Context<Self>) {
        if self.save_dir(cx) {
            cx.emit(SettingsEvent::Close);
        }
    }
}

impl Focusable for SettingsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn label(text: &'static str) -> impl IntoElement {
    div().w(px(130.)).flex_none().text_size(px(12.)).text_color(theme::hex(theme::MUTED)).child(text)
}

fn row(l: &'static str, child: impl IntoElement) -> impl IntoElement {
    div().flex().items_center().gap_3().child(label(l)).child(div().flex_1().child(child))
}

fn chip(id: (&'static str, usize), text: String, active: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_2()
        .py(px(3.))
        .rounded_sm()
        .text_size(px(12.))
        .cursor_pointer()
        .text_color(if active { theme::hex(theme::FG) } else { theme::hex(theme::MUTED) })
        .when(active, |d| d.bg(theme::hexa(theme::ACCENT, 0.25)))
        .hover(|s| s.bg(theme::hover()))
        .child(text)
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

impl Render for SettingsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.global::<Settings>().clone();
        let pct = (s.opacity * 100.0).round() as i32;

        let steps = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(2.))
            .p(px(2.))
            .rounded_md()
            .bg(theme::hexa(0x000000, 0.25))
            .border_1()
            .border_color(theme::border())
            .children(OPACITY_STEPS.iter().enumerate().map(|(i, &v)| {
                let label = if v >= 1.0 { "Solid".to_string() } else { format!("{}%", (v * 100.0) as i32) };
                chip(("op", i), label, (s.opacity - v).abs() < 0.001)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_opacity(v, cx)))
            }));

        let fine = div()
            .flex()
            .items_center()
            .gap_1()
            .child(button("op-less", "−", false).on_click(cx.listener(move |this, _, _, cx| {
                let v = cx.global::<Settings>().opacity - 0.05;
                this.set_opacity(v, cx)
            })))
            .child(div().w(px(44.)).flex().justify_center().text_size(px(12.)).child(format!("{pct}%")))
            .child(button("op-more", "+", false).on_click(cx.listener(move |this, _, _, cx| {
                let v = cx.global::<Settings>().opacity + 0.05;
                this.set_opacity(v, cx)
            })));

        let blur_on = s.blur;
        let blur = div()
            .id("blur")
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .text_size(px(12.))
            .child(
                div()
                    .w(px(28.))
                    .h(px(16.))
                    .rounded_full()
                    .p(px(2.))
                    .bg(if blur_on { theme::hex(theme::ACCENT) } else { theme::hexa(0xffffff, 0.15) })
                    .flex()
                    .when(blur_on, |d| d.justify_end())
                    .child(div().size(px(12.)).rounded_full().bg(theme::hex(0xffffff))),
            )
            .child("Blur what's behind the window")
            .on_click(cx.listener(move |this, _, window, cx| this.set_blur(!blur_on, window, cx)));

        let font = div()
            .flex()
            .items_center()
            .gap_1()
            .child(button("fs-less", "−", false).on_click(cx.listener(|_, _, _, cx| {
                Self::update(cx, |s| s.font_size = (s.font_size - 1.0).max(8.0));
            })))
            .child(div().w(px(44.)).flex().justify_center().text_size(px(12.)).child(format!("{} pt", s.font_size)))
            .child(button("fs-more", "+", false).on_click(cx.listener(|_, _, _, cx| {
                Self::update(cx, |s| s.font_size = (s.font_size + 1.0).min(32.0));
            })));

        div()
            .key_context("SettingsPanel")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::apply))
            .w(px(560.))
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
            .child(div().text_size(px(15.)).child("Settings"))
            .child(row("Transparency", div().flex().flex_col().gap_2().child(steps).child(fine)))
            .child(row("Background", blur))
            .child(row(
                "Start new tabs in",
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(div().flex_1().child(self.start_dir.clone()))
                            .child(button("choose", "Choose…", false).on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx))))
                            .child(button("home", "Home", false).on_click(cx.listener(|this, _, _, cx| {
                                this.start_dir.update(cx, |i, cx| i.set_text("", cx));
                                this.save_dir(cx);
                            }))),
                    )
                    .when_some(self.error.clone(), |d, e| d.child(div().text_size(px(11.)).text_color(theme::hex(theme::DANGER)).child(e))),
            ))
            .child(row("Font size", font))
            .child(
                div()
                    .pt_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.))
                            .text_color(theme::hex(theme::SUBTLE))
                            .child(format!("Saved to {}", config_dir().join("settings.json").display())),
                    )
                    .child(button("edit-json", "Edit JSON", false).on_click(|_, _, _| {
                        let path = config_dir().join("settings.json");
                        #[cfg(target_os = "macos")]
                        let _ = std::process::Command::new("open").arg("-t").arg(path).spawn();
                        #[cfg(not(target_os = "macos"))]
                        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
                    }))
                    .child(button("done", "Done", true).on_click(cx.listener(|this, _, _, cx| {
                        if this.save_dir(cx) {
                            cx.emit(SettingsEvent::Close);
                        }
                    }))),
            )
    }
}
