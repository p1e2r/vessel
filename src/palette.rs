//! ⌘P quick-connect palette: fuzzy-search saved connections or type
//! `user@host`, `rdp://host`, `vnc://host` to connect immediately.

use gpui::{
    actions, div, prelude::*, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    KeyBinding, MouseButton, SharedString, Subscription, Window,
};

use crate::input::TextInput;
use crate::profiles::{parse_quick, Kind, Profile};
use crate::theme;

actions!(palette, [SelectPrev, SelectNext, Confirm, Dismiss, EditSelected]);

pub fn bind_keys(cx: &mut App) {
    let ctx = Some("Palette");
    cx.bind_keys([
        KeyBinding::new("up", SelectPrev, ctx),
        KeyBinding::new("down", SelectNext, ctx),
        KeyBinding::new("ctrl-p", SelectPrev, ctx),
        KeyBinding::new("ctrl-n", SelectNext, ctx),
        KeyBinding::new("enter", Confirm, ctx),
        KeyBinding::new("escape", Dismiss, ctx),
        KeyBinding::new("cmd-e", EditSelected, ctx),
    ]);
}

pub enum PaletteEvent {
    Open(Profile),
    Edit(Profile),
    New,
    LocalShell,
    Dismiss,
}

#[derive(Clone)]
enum Item {
    Quick(Profile),
    Saved(Profile),
    Local,
    New,
}

pub struct Palette {
    query: Entity<TextInput>,
    profiles: Vec<Profile>,
    selected: usize,
    _sub: Subscription,
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Palette {
    pub fn new(profiles: Vec<Profile>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| TextInput::new(cx, "Search connections or type user@host, rdp://host, vnc://host", ""));
        let sub = cx.observe(&query, |this: &mut Self, _, cx| {
            this.selected = 0;
            cx.notify();
        });
        window.focus(&query.focus_handle(cx));
        Self { query, profiles, selected: 0, _sub: sub }
    }

    fn items(&self, cx: &App) -> Vec<Item> {
        let q = self.query.read(cx).text();
        let q = q.trim();
        let mut items = Vec::new();
        if let Some(p) = parse_quick(q) {
            if q.contains('@') || q.contains("://") || q.contains('.') || q.contains(':') {
                items.push(Item::Quick(p));
            }
        }
        let words: Vec<String> = q.to_lowercase().split_whitespace().map(String::from).collect();
        for p in &self.profiles {
            let hay = format!("{} {} {}", p.display_name(), p.subtitle(), p.kind.label()).to_lowercase();
            if words.iter().all(|w| hay.contains(w.as_str())) {
                items.push(Item::Saved(p.clone()));
            }
        }
        if q.is_empty() || "local shell zsh terminal".contains(&q.to_lowercase()) {
            items.push(Item::Local);
        }
        items.push(Item::New);
        items
    }

    fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = self.selected.saturating_sub(1);
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let n = self.items(cx).len();
        self.selected = (self.selected + 1).min(n.saturating_sub(1));
        cx.notify();
    }

    fn activate(&mut self, item: Item, cx: &mut Context<Self>) {
        match item {
            Item::Quick(p) | Item::Saved(p) => cx.emit(PaletteEvent::Open(p)),
            Item::Local => cx.emit(PaletteEvent::LocalShell),
            Item::New => cx.emit(PaletteEvent::New),
        }
    }

    fn confirm(&mut self, _: &Confirm, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = self.items(cx).get(self.selected).cloned() {
            self.activate(item, cx);
        }
    }

    fn edit_selected(&mut self, _: &EditSelected, _: &mut Window, cx: &mut Context<Self>) {
        match self.items(cx).get(self.selected).cloned() {
            Some(Item::Saved(p)) | Some(Item::Quick(p)) => cx.emit(PaletteEvent::Edit(p)),
            _ => {}
        }
    }

    fn dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(PaletteEvent::Dismiss);
    }
}

impl Focusable for Palette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

pub fn kind_badge(kind: Kind) -> impl IntoElement {
    let color = match kind {
        Kind::Ssh => theme::OK,
        Kind::Rdp => theme::ACCENT,
        Kind::Vnc => 0xbb9af7,
    };
    div()
        .flex_none()
        .w(px(34.))
        .py(px(1.))
        .rounded_sm()
        .text_size(px(10.))
        .text_color(theme::hex(color))
        .bg(theme::hexa(color, 0.12))
        .flex()
        .justify_center()
        .child(kind.label())
}

impl Render for Palette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let items = self.items(cx);
        let selected = self.selected.min(items.len().saturating_sub(1));

        let rows = items.into_iter().enumerate().take(12).map(|(i, item)| {
            let (badge, title, subtitle): (Option<Kind>, SharedString, SharedString) = match &item {
                Item::Quick(p) => (Some(p.kind), format!("Connect to {}", p.display_name()).into(), "Quick connect (not saved)".into()),
                Item::Saved(p) => (Some(p.kind), p.display_name().into(), p.subtitle().into()),
                Item::Local => (None, "New local shell".into(), "zsh".into()),
                Item::New => (None, "New connection…".into(), "Save an SSH, RDP or VNC host".into()),
            };
            let item2 = item.clone();
            div()
                .id(("palette-item", i))
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py(px(7.))
                .rounded_md()
                .cursor_pointer()
                .when(i == selected, |d| d.bg(theme::hexa(theme::ACCENT, 0.16)))
                .hover(|s| s.bg(theme::hover()))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.activate(item2.clone(), cx)))
                .child(match badge {
                    Some(k) => kind_badge(k).into_any_element(),
                    None => div().w(px(34.)).flex().justify_center().text_color(theme::hex(theme::MUTED)).child(if matches!(item, Item::New) { "+" } else { ">_" }).into_any_element(),
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .child(div().text_size(px(13.)).text_color(theme::hex(theme::FG)).child(title))
                        .child(div().text_size(px(11.)).text_color(theme::hex(theme::MUTED)).child(subtitle)),
                )
        });

        div()
            .key_context("Palette")
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(Self::edit_selected))
            .w(px(580.))
            .p_2()
            .rounded_xl()
            .bg(theme::panel())
            .border_1()
            .border_color(theme::border())
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.query.clone())
            .child(div().flex().flex_col().gap(px(2.)).children(rows))
            .child(
                div()
                    .px_2()
                    .pt_1()
                    .text_size(px(11.))
                    .text_color(theme::hex(theme::SUBTLE))
                    .child("↵ connect   ⌘E edit   ↑↓ move   esc close"),
            )
    }
}
