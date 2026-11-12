//! Remote desktop tabs (RDP and VNC). Protocol tasks write decoded pixels into
//! a shared BGRA framebuffer; the view uploads it to the GPU at the profile's
//! frame rate and forwards mouse / keyboard input back.

pub mod keymap;
pub mod rdp;
pub mod vnc;

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    canvas, div, point, prelude::*, px, size, App, Bounds, ClipboardItem, Context, Corners,
    EventEmitter, FocusHandle, Focusable, KeyDownEvent, KeyUpEvent, Modifiers,
    ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    RenderImage, ScrollDelta, ScrollWheelEvent, SharedString, Subscription, Task, Window,
};
use parking_lot::Mutex;

use crate::profiles::{Kind, Profile, ScaleMode};
use crate::theme;

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Connecting(String),
    Connected,
    Closed(String),
    Error(String),
}

/// Decoded remote screen, shared between the protocol task and the view.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// BGRA, alpha forced to 255.
    pub data: Vec<u8>,
    pub dirty: bool,
    pub status: Status,
    /// Text the remote put on its clipboard (VNC ServerCutText).
    pub clipboard: Option<String>,
}

impl Frame {
    pub fn new() -> Self {
        Self { width: 0, height: 0, data: Vec::new(), dirty: false, status: Status::Connecting("Connecting…".into()), clipboard: None }
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        self.width = w;
        self.height = h;
        self.data = vec![0; (w * h * 4) as usize];
        for px in self.data.chunks_exact_mut(4) {
            px[3] = 255;
        }
        self.dirty = true;
    }

    /// Copy a BGRA(X) rectangle in, forcing opaque alpha.
    pub fn blit_bgra(&mut self, x: u32, y: u32, w: u32, h: u32, src: &[u8], src_stride: usize) {
        if x >= self.width || y >= self.height {
            return;
        }
        let w = w.min(self.width - x) as usize;
        let h = h.min(self.height - y) as usize;
        let dst_stride = self.width as usize * 4;
        for row in 0..h {
            let s = row * src_stride;
            let d = (y as usize + row) * dst_stride + x as usize * 4;
            if s + w * 4 > src.len() {
                break;
            }
            let dst = &mut self.data[d..d + w * 4];
            dst.copy_from_slice(&src[s..s + w * 4]);
            for px in dst.chunks_exact_mut(4) {
                px[3] = 255;
            }
        }
        self.dirty = true;
    }

    /// Copy an RGBA rectangle in, swapping to BGRA.
    pub fn blit_rgba(&mut self, x: u32, y: u32, w: u32, h: u32, src: &[u8], src_stride: usize) {
        if x >= self.width || y >= self.height {
            return;
        }
        let w = w.min(self.width - x) as usize;
        let h = h.min(self.height - y) as usize;
        let dst_stride = self.width as usize * 4;
        for row in 0..h {
            let s = row * src_stride;
            let d = (y as usize + row) * dst_stride + x as usize * 4;
            if s + w * 4 > src.len() {
                break;
            }
            let srow = &src[s..s + w * 4];
            let drow = &mut self.data[d..d + w * 4];
            for (dp, sp) in drow.chunks_exact_mut(4).zip(srow.chunks_exact(4)) {
                dp[0] = sp[2];
                dp[1] = sp[1];
                dp[2] = sp[0];
                dp[3] = 255;
            }
        }
        self.dirty = true;
    }

    pub fn copy_rect(&mut self, dx: u32, dy: u32, sx: u32, sy: u32, w: u32, h: u32) {
        let stride = self.width as usize * 4;
        let w = w.min(self.width.saturating_sub(dx)).min(self.width.saturating_sub(sx)) as usize;
        let h = h.min(self.height.saturating_sub(dy)).min(self.height.saturating_sub(sy)) as usize;
        let mut tmp = vec![0u8; w * h * 4];
        for row in 0..h {
            let s = (sy as usize + row) * stride + sx as usize * 4;
            tmp[row * w * 4..(row + 1) * w * 4].copy_from_slice(&self.data[s..s + w * 4]);
        }
        for row in 0..h {
            let d = (dy as usize + row) * stride + dx as usize * 4;
            self.data[d..d + w * 4].copy_from_slice(&tmp[row * w * 4..(row + 1) * w * 4]);
        }
        self.dirty = true;
    }

    pub fn set_status(&mut self, s: Status) {
        self.status = s;
        self.dirty = true;
    }
}

pub type SharedFrame = Arc<Mutex<Frame>>;

#[derive(Clone, Copy, Debug)]
pub enum Button {
    Left,
    Right,
    Middle,
}

/// Input from the view to a protocol task, in remote pixel coordinates.
#[derive(Clone, Debug)]
pub enum RemoteInput {
    MouseMove(u16, u16),
    Button(Button, bool),
    /// Vertical / horizontal wheel in "notches" ×120 (Windows convention).
    Wheel { dx: i16, dy: i16 },
    /// GPUI key name (e.g. "a", "enter", "shift"), the character it typed if any, and press state.
    Key { name: String, ch: Option<char>, down: bool },
    /// Type text as unicode keystrokes.
    Text(String),
    CtrlAltDel,
    /// Send local clipboard to the remote clipboard (VNC only).
    Clipboard(String),
    Close,
}

pub enum RemoteEvent {
    TitleChanged,
    Reconnect,
}

pub struct RemoteView {
    pub profile: Profile,
    frame: SharedFrame,
    input: flume::Sender<RemoteInput>,
    focus: FocusHandle,
    image: Option<Arc<RenderImage>>,
    to_drop: Arc<Mutex<Vec<Arc<RenderImage>>>>,
    /// Where the image was drawn last frame, plus the remote size at that time.
    dest: Arc<Mutex<Option<(Bounds<Pixels>, u32, u32)>>>,
    pub status: Status,
    scale: ScaleMode,
    mods: Modifiers,
    wheel: (f32, f32),
    _pump: Task<()>,
    _blur: Subscription,
}

impl EventEmitter<RemoteEvent> for RemoteView {}

const TOOLBAR_H: f32 = 30.0;

impl RemoteView {
    pub fn new(profile: Profile, auto_size: (u16, u16), window: &mut Window, cx: &mut Context<Self>) -> Self {
        let frame: SharedFrame = Arc::new(Mutex::new(Frame::new()));
        let (tx, rx) = flume::unbounded();
        let password = profile.password();
        let (w, h) = if profile.width == 0 || profile.height == 0 {
            (auto_size.0.max(640), (auto_size.1.saturating_sub(TOOLBAR_H as u16)).max(480))
        } else {
            (profile.width, profile.height)
        };
        match profile.kind {
            Kind::Rdp => rdp::spawn(profile.clone(), password, (w, h), frame.clone(), rx),
            Kind::Vnc => vnc::spawn(profile.clone(), password, (w, h), frame.clone(), rx),
            Kind::Ssh => unreachable!("SSH profiles open in a terminal tab"),
        }

        let fps = profile.fps.clamp(1, 240) as u64;
        let pump = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_micros(1_000_000 / fps)).await;
            if this.update(cx, |v, cx| v.pull_frame(cx)).is_err() {
                break;
            }
        });

        let focus = cx.focus_handle();
        let blur = cx.on_blur(&focus, window, |this: &mut Self, _window, _cx| this.release_modifiers());

        Self {
            scale: profile.scale,
            profile,
            frame,
            input: tx,
            focus,
            image: None,
            to_drop: Arc::new(Mutex::new(Vec::new())),
            dest: Arc::new(Mutex::new(None)),
            status: Status::Connecting("Connecting…".into()),
            mods: Modifiers::default(),
            wheel: (0.0, 0.0),
            _pump: pump,
            _blur: blur,
        }
    }

    pub fn shutdown(&self) {
        let _ = self.input.send(RemoteInput::Close);
    }

    fn send(&self, i: RemoteInput) {
        if self.profile.view_only && !matches!(i, RemoteInput::Close) {
            return;
        }
        if self.status == Status::Connected {
            let _ = self.input.send(i);
        }
    }

    fn pull_frame(&mut self, cx: &mut Context<Self>) {
        let mut f = self.frame.lock();
        if let Some(text) = f.clipboard.take() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        if f.status != self.status {
            self.status = f.status.clone();
            cx.emit(RemoteEvent::TitleChanged);
            cx.notify();
        }
        if !f.dirty || f.width == 0 {
            return;
        }
        f.dirty = false;
        let Some(buf) = image::RgbaImage::from_raw(f.width, f.height, f.data.clone()) else { return };
        drop(f);
        let img = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
        if let Some(old) = self.image.replace(img) {
            self.to_drop.lock().push(old);
        }
        cx.notify();
    }

    fn release_modifiers(&mut self) {
        for (on, name) in [
            (self.mods.shift, "shift"),
            (self.mods.control, "control"),
            (self.mods.alt, "alt"),
            (self.mods.platform, self.platform_key()),
        ] {
            if on {
                self.send(RemoteInput::Key { name: name.into(), ch: None, down: false });
            }
        }
        self.mods = Modifiers::default();
    }

    fn platform_key(&self) -> &'static str {
        if self.profile.cmd_as_ctrl { "control" } else { "platform" }
    }

    fn to_remote(&self, pos: gpui::Point<Pixels>) -> Option<(u16, u16)> {
        let (b, w, h) = (*self.dest.lock())?;
        let x = f32::from(pos.x - b.origin.x) / f32::from(b.size.width);
        let y = f32::from(pos.y - b.origin.y) / f32::from(b.size.height);
        if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
            return None;
        }
        Some((((x * w as f32) as u32).min(w.saturating_sub(1)) as u16, ((y * h as f32) as u32).min(h.saturating_sub(1)) as u16))
    }

    fn on_modifiers(&mut self, ev: &ModifiersChangedEvent, _: &mut Window, _: &mut Context<Self>) {
        let new = ev.modifiers;
        let old = self.mods;
        let pk = self.platform_key();
        for (was, is, name) in [
            (old.shift, new.shift, "shift"),
            (old.control, new.control, "control"),
            (old.alt, new.alt, "alt"),
            (old.platform, new.platform, pk),
        ] {
            if was != is {
                self.send(RemoteInput::Key { name: name.into(), ch: None, down: is });
            }
        }
        self.mods = new;
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        let ch = k.key_char.as_ref().and_then(|s| {
            let mut it = s.chars();
            let c = it.next()?;
            it.next().is_none().then_some(c)
        });
        let name = k.key.clone();
        self.send(RemoteInput::Key { name: name.clone(), ch, down: true });
        if k.modifiers.platform {
            // macOS never delivers key-up while ⌘ is held.
            self.send(RemoteInput::Key { name, ch, down: false });
        }
        cx.stop_propagation();
    }

    fn on_key_up(&mut self, ev: &KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        if k.modifiers.platform {
            return;
        }
        self.send(RemoteInput::Key { name: k.key.clone(), ch: None, down: false });
        cx.stop_propagation();
    }

    fn mouse_down(&mut self, b: Button, ev: &MouseDownEvent, window: &mut Window) {
        window.focus(&self.focus);
        if let Some((x, y)) = self.to_remote(ev.position) {
            self.send(RemoteInput::MouseMove(x, y));
            self.send(RemoteInput::Button(b, true));
        }
    }

    fn mouse_up(&mut self, b: Button, ev: &MouseUpEvent) {
        if let Some((x, y)) = self.to_remote(ev.position) {
            self.send(RemoteInput::MouseMove(x, y));
        }
        self.send(RemoteInput::Button(b, false));
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        if let Some((x, y)) = self.to_remote(ev.position) {
            self.send(RemoteInput::MouseMove(x, y));
        }
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _: &mut Window, _: &mut Context<Self>) {
        let (dx, dy) = match ev.delta {
            ScrollDelta::Pixels(p) => (f32::from(p.x) * 2.0, f32::from(p.y) * 2.0),
            ScrollDelta::Lines(l) => (l.x * 40.0, l.y * 40.0),
        };
        self.wheel.0 += dx;
        self.wheel.1 += dy;
        // VNC only has whole notches; RDP accepts finer steps.
        let step = if self.profile.kind == Kind::Vnc { 120.0 } else { 15.0 };
        let qx = (self.wheel.0 / step).trunc() * step;
        let qy = (self.wheel.1 / step).trunc() * step;
        if qx != 0.0 || qy != 0.0 {
            self.wheel.0 -= qx;
            self.wheel.1 -= qy;
            self.send(RemoteInput::Wheel { dx: qx.clamp(-1200.0, 1200.0) as i16, dy: qy.clamp(-1200.0, 1200.0) as i16 });
        }
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (w, h) = {
            let f = self.frame.lock();
            (f.width, f.height)
        };
        let (dot, label): (u32, String) = match &self.status {
            Status::Connecting(s) => (0xe0af68, s.clone()),
            Status::Connected => (theme::OK, format!("{}×{} · {} fps", w, h, self.profile.fps)),
            Status::Closed(s) => (theme::MUTED, s.clone()),
            Status::Error(s) => (theme::DANGER, s.lines().next().unwrap_or("Error").to_string()),
        };
        let connected = self.status == Status::Connected;
        let is_rdp = self.profile.kind == Kind::Rdp;
        let scale_label = match self.scale {
            ScaleMode::Fit => "Fit",
            ScaleMode::Actual => "1:1",
        };

        div()
            .h(px(TOOLBAR_H))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .border_b_1()
            .border_color(theme::border())
            .text_size(px(12.))
            .text_color(theme::hex(theme::MUTED))
            .child(div().size(px(7.)).rounded_full().bg(theme::hex(dot)))
            .child(div().flex_1().overflow_hidden().child(label))
            .when(connected, |d| {
                d.child(tool_button("scale", scale_label, cx.listener(|this, _, _, cx| {
                    this.scale = match this.scale {
                        ScaleMode::Fit => ScaleMode::Actual,
                        ScaleMode::Actual => ScaleMode::Fit,
                    };
                    cx.notify();
                })))
                .child(tool_button("type", "Type clipboard", cx.listener(|this, _, _, cx| {
                    if let Some(t) = cx.read_from_clipboard().and_then(|i| i.text()) {
                        this.send(RemoteInput::Text(t));
                    }
                })))
                .when(!is_rdp, |d| {
                    d.child(tool_button("clip", "Send clipboard", cx.listener(|this, _, _, cx| {
                        if let Some(t) = cx.read_from_clipboard().and_then(|i| i.text()) {
                            this.send(RemoteInput::Clipboard(t));
                        }
                    })))
                })
                .when(is_rdp, |d| {
                    d.child(tool_button("cad", "Ctrl+Alt+Del", cx.listener(|this, _, _, _| this.send(RemoteInput::CtrlAltDel))))
                })
                .child(tool_button("disc", "Disconnect", cx.listener(|this, _, _, _| this.shutdown())))
            })
            .when(!connected && !matches!(self.status, Status::Connecting(_)), |d| {
                d.child(tool_button("recon", "Reconnect", cx.listener(|_, _, _, cx| cx.emit(RemoteEvent::Reconnect))))
            })
    }
}

fn tool_button(
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px_2()
        .py(px(2.))
        .rounded_md()
        .text_color(theme::hex(theme::FG))
        .hover(|s| s.bg(theme::hover()))
        .cursor_pointer()
        .child(label)
        .on_click(on_click)
}

impl Focusable for RemoteView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for RemoteView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let image = self.image.clone();
        let to_drop = self.to_drop.clone();
        let dest = self.dest.clone();
        let scale = self.scale;
        let connected = self.status == Status::Connected;
        let overlay: Option<SharedString> = match &self.status {
            Status::Connected => None,
            Status::Connecting(s) => Some(s.clone().into()),
            Status::Closed(s) => Some(s.clone().into()),
            Status::Error(s) => Some(s.clone().into()),
        };
        let is_error = matches!(self.status, Status::Error(_));

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.toolbar(cx))
            .child(
                div()
                    .id("remote-surface")
                    .key_context("Remote")
                    .track_focus(&self.focus)
                    .flex_1()
                    .relative()
                    .overflow_hidden()
                    .on_key_down(cx.listener(Self::on_key_down))
                    .on_key_up(cx.listener(Self::on_key_up))
                    .on_modifiers_changed(cx.listener(Self::on_modifiers))
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, ev, w, _| this.mouse_down(Button::Left, ev, w)))
                    .on_mouse_down(MouseButton::Right, cx.listener(|this, ev, w, _| this.mouse_down(Button::Right, ev, w)))
                    .on_mouse_down(MouseButton::Middle, cx.listener(|this, ev, w, _| this.mouse_down(Button::Middle, ev, w)))
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, ev, _, _| this.mouse_up(Button::Left, ev)))
                    .on_mouse_up(MouseButton::Right, cx.listener(|this, ev, _, _| this.mouse_up(Button::Right, ev)))
                    .on_mouse_up(MouseButton::Middle, cx.listener(|this, ev, _, _| this.mouse_up(Button::Middle, ev)))
                    .on_mouse_move(cx.listener(Self::on_mouse_move))
                    .on_scroll_wheel(cx.listener(Self::on_scroll))
                    .child(
                        canvas(
                            move |bounds, window, _| {
                                let Some(img) = image.as_ref().filter(|_| connected) else {
                                    *dest.lock() = None;
                                    return None;
                                };
                                let s = img.size(0);
                                let (iw, ih) = (s.width.0 as f32, s.height.0 as f32);
                                let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                                let (w, h) = match scale {
                                    ScaleMode::Fit => {
                                        let k = (bw / iw).min(bh / ih);
                                        (iw * k, ih * k)
                                    }
                                    ScaleMode::Actual => {
                                        let sf = window.scale_factor();
                                        (iw / sf, ih / sf)
                                    }
                                };
                                let x = bounds.origin.x + px(((bw - w) / 2.0).max(0.0));
                                let y = bounds.origin.y + px(((bh - h) / 2.0).max(0.0));
                                let b = Bounds::new(point(x, y), size(px(w), px(h)));
                                *dest.lock() = Some((b, iw as u32, ih as u32));
                                Some((b, img.clone()))
                            },
                            move |_, state, window, _| {
                                for old in to_drop.lock().drain(..) {
                                    let _ = window.drop_image(old);
                                }
                                if let Some((b, img)) = state {
                                    let _ = window.paint_image(b, Corners::default(), img, 0, false);
                                }
                            },
                        )
                        .size_full(),
                    )
                    .when_some(overlay, |d, text| {
                        d.child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .max_w(px(520.))
                                        .px_4()
                                        .py_3()
                                        .rounded_lg()
                                        .bg(theme::panel())
                                        .border_1()
                                        .border_color(theme::border())
                                        .text_size(px(13.))
                                        .text_color(if is_error { theme::hex(theme::DANGER) } else { theme::hex(theme::FG) })
                                        .child(text),
                                ),
                        )
                    }),
            )
    }
}
