//! GPUI view that paints an `alacritty_terminal` grid and turns keyboard /
//! mouse input into bytes for the transport.

use std::sync::Arc;

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor};
use gpui::{
    actions, canvas, div, fill, font, outline, point, prelude::*, px, size, App, Bounds,
    ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, FontStyle, FontWeight, Hsla,
    KeyDownEvent, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PaintQuad, Pixels, Point, ScrollDelta, ScrollWheelEvent, ShapedLine, SharedString,
    StrikethroughStyle, Task, TextRun, UnderlineStyle, Window,
};
use parking_lot::Mutex;

use super::{pty, ssh, GridSize, TermCore, TransportMsg, UiEvent};
use crate::profiles::Profile;
use crate::settings::Settings;
use crate::theme;

actions!(terminal, [Copy, Paste, Clear, SelectAll, ScrollPageUp, ScrollPageDown, ScrollTop, ScrollBottom]);

pub fn bind_keys(cx: &mut App) {
    use gpui::KeyBinding;
    let ctx = Some("Terminal");
    cx.bind_keys([
        KeyBinding::new("cmd-c", Copy, ctx),
        KeyBinding::new("cmd-v", Paste, ctx),
        KeyBinding::new("cmd-k", Clear, ctx),
        KeyBinding::new("cmd-a", SelectAll, ctx),
        KeyBinding::new("shift-pageup", ScrollPageUp, ctx),
        KeyBinding::new("shift-pagedown", ScrollPageDown, ctx),
        KeyBinding::new("cmd-up", ScrollTop, ctx),
        KeyBinding::new("cmd-down", ScrollBottom, ctx),
    ]);
}

const PAD: f32 = 10.0;

#[derive(Clone)]
pub enum TerminalKind {
    Local,
    Ssh(Profile),
}

pub enum TerminalEvent {
    TitleChanged,
    CloseRequested,
    Reconnect,
    SavePassword(String, String),
}

#[derive(Clone, Copy, Default)]
struct Metrics {
    origin: Point<Pixels>,
    cell_w: Pixels,
    line_h: Pixels,
    cols: usize,
    rows: usize,
}

pub struct TerminalView {
    core: TermCore,
    focus: FocusHandle,
    pub kind: TerminalKind,
    pub title: String,
    pub exited: Option<String>,
    metrics: Arc<Mutex<Metrics>>,
    selecting: bool,
    scroll_px: f32,
    _pump: Task<()>,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl TerminalView {
    pub fn new(kind: TerminalKind, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = cx.global::<Settings>().clone();
        let (core, rx) = TermCore::new(settings.scrollback);
        let title = match &kind {
            TerminalKind::Local => {
                if let Err(e) = pty::spawn_local(&core, rx, &settings.shell) {
                    core.sink().error(&format!("Failed to start {}: {e:#}", settings.shell));
                }
                std::path::Path::new(&settings.shell)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "shell".into())
            }
            TerminalKind::Ssh(p) => {
                ssh::spawn_ssh(&core, rx, p.clone());
                p.display_name()
            }
        };

        let ui_rx = core.ui_rx.clone();
        let pump = cx.spawn(async move |this, cx| {
            while let Ok(ev) = ui_rx.recv_async().await {
                let mut batch = vec![ev];
                batch.extend(ui_rx.try_iter());
                if this.update(cx, |view, cx| view.handle_events(batch, cx)).is_err() {
                    break;
                }
            }
        });

        Self {
            core,
            focus: cx.focus_handle(),
            kind,
            title,
            exited: None,
            metrics: Arc::new(Mutex::new(Metrics::default())),
            selecting: false,
            scroll_px: 0.0,
            _pump: pump,
        }
    }

    fn handle_events(&mut self, events: Vec<UiEvent>, cx: &mut Context<Self>) {
        let mut dirty = false;
        for ev in events {
            match ev {
                UiEvent::Wakeup => dirty = true,
                UiEvent::Title(t) => {
                    if !t.trim().is_empty() {
                        self.title = t;
                        cx.emit(TerminalEvent::TitleChanged);
                    }
                }
                UiEvent::ResetTitle => {}
                UiEvent::Bell => {}
                UiEvent::Clipboard(s) => cx.write_to_clipboard(ClipboardItem::new_string(s)),
                UiEvent::SavePassword(id, pw) => cx.emit(TerminalEvent::SavePassword(id, pw)),
                UiEvent::Exited(reason) => {
                    self.exited = Some(reason);
                    dirty = true;
                    cx.emit(TerminalEvent::TitleChanged);
                    if matches!(self.kind, TerminalKind::Local) {
                        cx.emit(TerminalEvent::CloseRequested);
                    }
                }
            }
        }
        if dirty {
            cx.notify();
        }
    }

    pub fn shutdown(&self) {
        let _ = self.core.transport.send(TransportMsg::Close);
    }

    fn write(&self, bytes: impl Into<Vec<u8>>) {
        let _ = self.core.transport.send(TransportMsg::Data(bytes.into()));
    }

    fn scroll_to_bottom(&self) {
        let mut term = self.core.term.lock();
        term.scroll_display(Scroll::Bottom);
        term.selection = None;
    }

    // ---------- actions ----------

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.core.term.lock().selection_to_string() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) else { return };
        let bracketed = self.core.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        let mut out = Vec::new();
        if bracketed {
            out.extend_from_slice(b"\x1b[200~");
            out.extend_from_slice(text.replace('\x1b', "").as_bytes());
            out.extend_from_slice(b"\x1b[201~");
        } else {
            out.extend_from_slice(text.as_bytes());
        }
        self.scroll_to_bottom();
        self.write(out);
        cx.notify();
    }

    fn clear(&mut self, _: &Clear, _: &mut Window, cx: &mut Context<Self>) {
        // Like Terminal.app's ⌘K: wipe scrollback and redraw the prompt.
        {
            let mut term = self.core.term.lock();
            term.grid_mut().clear_history();
            term.selection = None;
        }
        self.write(b"\x0c".to_vec());
        cx.notify();
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        let mut term = self.core.term.lock();
        let top = term.grid().topmost_line();
        let bottom = term.grid().bottommost_line();
        let last = term.grid().last_column();
        let mut sel = Selection::new(SelectionType::Simple, GridPoint::new(top, Column(0)), Side::Left);
        sel.update(GridPoint::new(bottom, last), Side::Right);
        term.selection = Some(sel);
        drop(term);
        cx.notify();
    }

    fn scroll_page_up(&mut self, _: &ScrollPageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.core.term.lock().scroll_display(Scroll::PageUp);
        cx.notify();
    }
    fn scroll_page_down(&mut self, _: &ScrollPageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.core.term.lock().scroll_display(Scroll::PageDown);
        cx.notify();
    }
    fn scroll_top(&mut self, _: &ScrollTop, _: &mut Window, cx: &mut Context<Self>) {
        self.core.term.lock().scroll_display(Scroll::Top);
        cx.notify();
    }
    fn scroll_bottom(&mut self, _: &ScrollBottom, _: &mut Window, cx: &mut Context<Self>) {
        self.core.term.lock().scroll_display(Scroll::Bottom);
        cx.notify();
    }

    // ---------- keyboard ----------

    fn on_key_down(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(reason) = &self.exited {
            let _ = reason;
            match ev.keystroke.key.as_str() {
                "enter" | "escape" => cx.emit(TerminalEvent::CloseRequested),
                "r" if matches!(self.kind, TerminalKind::Ssh(_)) => cx.emit(TerminalEvent::Reconnect),
                _ => {}
            }
            cx.stop_propagation();
            return;
        }
        if ev.keystroke.modifiers.platform {
            // ⌘-shortcuts that reached us unbound: a few mac-style editing helpers.
            let bytes: Option<&[u8]> = match ev.keystroke.key.as_str() {
                "left" => Some(b"\x01"),       // start of line
                "right" => Some(b"\x05"),      // end of line
                "backspace" => Some(b"\x15"),  // kill line
                _ => None,
            };
            if let Some(b) = bytes {
                self.scroll_to_bottom();
                self.write(b.to_vec());
                cx.stop_propagation();
            }
            return;
        }
        let mode = *self.core.term.lock().mode();
        let option_as_meta = cx.global::<Settings>().option_as_meta;
        if let Some(bytes) = key_to_bytes(&ev.keystroke, mode, option_as_meta) {
            self.scroll_to_bottom();
            self.write(bytes);
            cx.stop_propagation();
            cx.notify();
        }
    }

    // ---------- mouse ----------

    fn grid_point(&self, pos: Point<Pixels>) -> (GridPoint, Side) {
        let m = *self.metrics.lock();
        let display_offset = self.core.term.lock().grid().display_offset() as i32;
        let x = f32::from(pos.x - m.origin.x).max(0.0);
        let y = f32::from(pos.y - m.origin.y).max(0.0);
        let cw = f32::from(m.cell_w).max(1.0);
        let lh = f32::from(m.line_h).max(1.0);
        let col = ((x / cw) as usize).min(m.cols.saturating_sub(1));
        let row = ((y / lh) as usize).min(m.rows.saturating_sub(1));
        let side = if (x / cw).fract() < 0.5 { Side::Left } else { Side::Right };
        (GridPoint::new(Line(row as i32 - display_offset), Column(col)), side)
    }

    fn mouse_reporting(&self, shift: bool) -> bool {
        if shift {
            return false;
        }
        let mode = *self.core.term.lock().mode();
        mode.intersects(TermMode::MOUSE_MODE) && mode.contains(TermMode::SGR_MOUSE)
    }

    fn report_mouse(&self, button: u8, pos: Point<Pixels>, pressed: bool) {
        let m = *self.metrics.lock();
        let col = (f32::from(pos.x - m.origin.x) / f32::from(m.cell_w).max(1.0)).max(0.0) as usize + 1;
        let row = (f32::from(pos.y - m.origin.y) / f32::from(m.line_h).max(1.0)).max(0.0) as usize + 1;
        let s = format!("\x1b[<{};{};{}{}", button, col.min(m.cols), row.min(m.rows), if pressed { 'M' } else { 'm' });
        self.write(s.into_bytes());
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        if self.mouse_reporting(ev.modifiers.shift) {
            self.report_mouse(0, ev.position, true);
            return;
        }
        let (pt, side) = self.grid_point(ev.position);
        let ty = match ev.click_count {
            2 => SelectionType::Semantic,
            3 => SelectionType::Lines,
            _ if ev.modifiers.alt => SelectionType::Block,
            _ => SelectionType::Simple,
        };
        let mut term = self.core.term.lock();
        if ev.modifiers.shift && term.selection.is_some() {
            if let Some(sel) = term.selection.as_mut() {
                sel.update(pt, side);
            }
        } else {
            term.selection = Some(Selection::new(ty, pt, side));
        }
        drop(term);
        self.selecting = true;
        cx.notify();
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || ev.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let (pt, side) = self.grid_point(ev.position);
        if let Some(sel) = self.core.term.lock().selection.as_mut() {
            sel.update(pt, side);
        }
        cx.notify();
    }

    fn on_mouse_up(&mut self, ev: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.mouse_reporting(ev.modifiers.shift) {
            self.report_mouse(0, ev.position, false);
            return;
        }
        self.selecting = false;
        let mut term = self.core.term.lock();
        let empty = term.selection.as_ref().map(|s| s.is_empty()).unwrap_or(true);
        if empty {
            term.selection = None;
        } else if cx.global::<Settings>().copy_on_select {
            if let Some(text) = term.selection_to_string() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }
        drop(term);
        cx.notify();
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let line_h = f32::from(self.metrics.lock().line_h).max(1.0);
        let dy = match ev.delta {
            ScrollDelta::Pixels(p) => f32::from(p.y),
            ScrollDelta::Lines(l) => l.y * line_h,
        };
        self.scroll_px += dy;
        let lines = (self.scroll_px / line_h) as i32;
        if lines == 0 {
            return;
        }
        self.scroll_px -= lines as f32 * line_h;

        let mode = *self.core.term.lock().mode();
        if self.mouse_reporting(ev.modifiers.shift) {
            let button = if lines > 0 { 64 } else { 65 };
            for _ in 0..lines.unsigned_abs().min(10) {
                self.report_mouse(button, ev.position, true);
            }
        } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) && !ev.modifiers.shift {
            // less/vim/man without mouse mode: translate the wheel into arrow keys.
            let seq: &[u8] = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            let mut out = Vec::new();
            for _ in 0..lines.unsigned_abs().min(20) {
                out.extend_from_slice(seq);
            }
            self.write(out);
        } else {
            self.core.term.lock().scroll_display(Scroll::Delta(lines));
            cx.notify();
        }
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = cx.global::<Settings>().clone();
        let focused = self.focus.is_focused(window);
        let term = self.core.term.clone();
        let transport = self.core.transport.clone();
        let metrics = self.metrics.clone();
        let win_size = self.core.listener.size.clone();
        let exited = self.exited.is_some();

        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus)
            .size_full()
            .cursor(gpui::CursorStyle::IBeam)
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::clear))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::scroll_page_up))
            .on_action(cx.listener(Self::scroll_page_down))
            .on_action(cx.listener(Self::scroll_top))
            .on_action(cx.listener(Self::scroll_bottom))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(
                canvas(
                    move |bounds, window, _cx| {
                        prepaint_grid(bounds, window, &settings, &term, &transport, &metrics, &win_size, focused && !exited)
                    },
                    move |_bounds, frame, window, cx| {
                        for q in frame.quads {
                            window.paint_quad(q);
                        }
                        for (origin, line) in frame.lines {
                            let _ = line.paint(origin, frame.line_h, window, cx);
                        }
                        if let Some(q) = frame.cursor {
                            window.paint_quad(q);
                        }
                        if let Some((origin, line)) = frame.cursor_text {
                            let _ = line.paint(origin, frame.line_h, window, cx);
                        }
                    },
                )
                .size_full(),
            )
    }
}

// ---------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------

struct GridFrame {
    quads: Vec<PaintQuad>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    cursor: Option<PaintQuad>,
    cursor_text: Option<(Point<Pixels>, ShapedLine)>,
    line_h: Pixels,
}

#[derive(Clone, Copy, PartialEq)]
struct CellStyle {
    fg: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

#[allow(clippy::too_many_arguments)]
fn prepaint_grid(
    bounds: Bounds<Pixels>,
    window: &mut Window,
    settings: &Settings,
    term: &super::SharedTerm,
    transport: &flume::Sender<TransportMsg>,
    metrics: &Arc<Mutex<Metrics>>,
    win_size: &Arc<Mutex<alacritty_terminal::event::WindowSize>>,
    focused: bool,
) -> GridFrame {
    let font_size = px(settings.font_size);
    let base_font = font(SharedString::from(settings.font_family.clone()));
    let text_system = window.text_system().clone();
    let font_id = text_system.resolve_font(&base_font);
    // Measure from a shaped run so the width matches whatever font actually renders (incl. fallbacks).
    let probe_run = TextRun { len: 10, font: base_font.clone(), color: theme::hex(theme::FG), background_color: None, underline: None, strikethrough: None };
    let measured = text_system.shape_line("mmmmmmmmmm".into(), font_size, &[probe_run], None).width / 10.0;
    let cell_w = if f32::from(measured) > 1.0 {
        measured
    } else {
        text_system.advance(font_id, font_size, 'm').map(|s| s.width).unwrap_or(px(settings.font_size * 0.6))
    };
    let line_h = px((settings.font_size * settings.line_height).round());

    let origin = point(bounds.origin.x + px(PAD), bounds.origin.y + px(PAD / 2.0));
    let avail_w = f32::from(bounds.size.width) - PAD * 2.0;
    let avail_h = f32::from(bounds.size.height) - PAD;
    let cols = ((avail_w / f32::from(cell_w)).floor() as usize).max(2);
    let rows = ((avail_h / f32::from(line_h)).floor() as usize).max(1);

    {
        let mut m = metrics.lock();
        let resized = m.cols != cols || m.rows != rows;
        *m = Metrics { origin, cell_w, line_h, cols, rows };
        if resized {
            term.lock().resize(GridSize { cols, rows });
            let px_w = (f32::from(cell_w) * cols as f32) as u16;
            let px_h = (f32::from(line_h) * rows as f32) as u16;
            *win_size.lock() = alacritty_terminal::event::WindowSize {
                num_lines: rows as u16,
                num_cols: cols as u16,
                cell_width: f32::from(cell_w) as u16,
                cell_height: f32::from(line_h) as u16,
            };
            let _ = transport.send(TransportMsg::Resize { cols: cols as u16, rows: rows as u16, px_w, px_h });
        }
    }

    let term = term.lock();
    let content = term.renderable_content();
    let display_offset = content.display_offset as i32;
    let colors = content.colors;
    let selection = content.selection;
    let mode = content.mode;
    let cursor = content.cursor;

    let mut quads = Vec::new();
    let mut lines = Vec::new();
    let mut rows_cells: Vec<Vec<Option<(char, CellStyle)>>> = vec![vec![None; cols]; rows];
    let mut wide: Vec<(usize, usize, char, CellStyle)> = Vec::new();
    let sel_bg = theme::hexa(theme::SELECTION, 0.75);
    let default_fg = named(NamedColor::Foreground, colors);
    let mut cursor_char = ' ';
    let mut cursor_style = CellStyle { fg: default_fg, bold: false, italic: false, underline: false, strike: false };

    // Background runs per row: (row, start_col, end_col, colour)
    let mut bg_run: Option<(usize, usize, usize, Hsla)> = None;
    let flush_bg = |run: &mut Option<(usize, usize, usize, Hsla)>, quads: &mut Vec<PaintQuad>| {
        if let Some((row, c0, c1, color)) = run.take() {
            let x = origin.x + cell_w * c0 as f32;
            let y = origin.y + line_h * row as f32;
            quads.push(fill(Bounds::new(point(x, y), size(cell_w * (c1 - c0) as f32, line_h)), color));
        }
    };

    for indexed in content.display_iter {
        let row = indexed.point.line.0 + display_offset;
        if row < 0 || row as usize >= rows {
            continue;
        }
        let row = row as usize;
        let col = indexed.point.column.0;
        if col >= cols {
            continue;
        }
        let cell = indexed.cell;
        let flags = cell.flags;

        let mut fg = resolve(cell.fg, colors, flags);
        let mut bg = match cell.bg {
            AnsiColor::Named(NamedColor::Background) => None,
            c => Some(resolve(c, colors, Flags::empty())),
        };
        if flags.contains(Flags::INVERSE) {
            let new_bg = fg;
            fg = bg.unwrap_or_else(|| named(NamedColor::Background, colors));
            fg.a = 1.0;
            bg = Some(new_bg);
        }
        if selection.map(|s| s.contains(indexed.point)).unwrap_or(false) {
            bg = Some(sel_bg);
        }
        if flags.contains(Flags::DIM) {
            fg.a *= 0.65;
        }

        let width = if flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
        match bg {
            Some(color) => match &mut bg_run {
                Some((r, _, end, c)) if *r == row && *end == col && *c == color => *end = col + width,
                _ => {
                    flush_bg(&mut bg_run, &mut quads);
                    bg_run = Some((row, col, col + width, color));
                }
            },
            None => flush_bg(&mut bg_run, &mut quads),
        }

        let style = CellStyle {
            fg,
            bold: flags.contains(Flags::BOLD),
            italic: flags.contains(Flags::ITALIC),
            underline: flags.intersects(Flags::ALL_UNDERLINES),
            strike: flags.contains(Flags::STRIKEOUT),
        };
        if indexed.point == cursor.point {
            cursor_char = cell.c;
            cursor_style = style;
        }
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let ch = if flags.contains(Flags::HIDDEN) || cell.c == '\t' { ' ' } else { cell.c };
        if flags.contains(Flags::WIDE_CHAR) {
            wide.push((row, col, ch, style));
        } else {
            rows_cells[row][col] = Some((ch, style));
        }
    }
    flush_bg(&mut bg_run, &mut quads);

    let make_run = |len: usize, s: &CellStyle| TextRun {
        len,
        font: gpui::Font {
            weight: if s.bold { FontWeight::BOLD } else { FontWeight::NORMAL },
            style: if s.italic { FontStyle::Italic } else { FontStyle::Normal },
            ..base_font.clone()
        },
        color: s.fg,
        background_color: None,
        underline: s.underline.then(|| UnderlineStyle { color: Some(s.fg), thickness: px(1.), wavy: false }),
        strikethrough: s.strike.then(|| StrikethroughStyle { color: Some(s.fg), thickness: px(1.) }),
    };

    // Text: one shaped line per row with a forced cell width keeps glyphs on the grid.
    for (row, cells) in rows_cells.iter().enumerate() {
        let last = cells.iter().rposition(|c| matches!(c, Some((ch, s)) if *ch != ' ' || s.underline || s.strike));
        let Some(last) = last else { continue };
        let mut text = String::new();
        let mut runs: Vec<TextRun> = Vec::new();
        let mut cur: Option<(CellStyle, usize)> = None;
        let blank = CellStyle { fg: default_fg, bold: false, italic: false, underline: false, strike: false };
        for c in cells.iter().take(last + 1) {
            let (ch, st) = c.unwrap_or((' ', blank));
            // Spaces adopt the previous style so they don't fragment runs.
            let st = if ch == ' ' && !st.underline && !st.strike { cur.map(|(s, _)| s).unwrap_or(st) } else { st };
            let before = text.len();
            text.push(ch);
            let n = text.len() - before;
            match &mut cur {
                Some((s, len)) if *s == st => *len += n,
                _ => {
                    if let Some((s, len)) = cur.take() {
                        runs.push(make_run(len, &s));
                    }
                    cur = Some((st, n));
                }
            }
        }
        if let Some((s, len)) = cur {
            runs.push(make_run(len, &s));
        }
        let shaped = text_system.shape_line(text.into(), font_size, &runs, Some(cell_w));
        lines.push((point(origin.x, origin.y + line_h * row as f32), shaped));
    }
    for (row, col, ch, st) in wide {
        let s: SharedString = ch.to_string().into();
        let run = make_run(s.len(), &st);
        let shaped = text_system.shape_line(s, font_size, &[run], None);
        lines.push((point(origin.x + cell_w * col as f32, origin.y + line_h * row as f32), shaped));
    }

    // Cursor
    let mut cursor_quad = None;
    let mut cursor_text = None;
    let crow = cursor.point.line.0 + display_offset;
    if mode.contains(TermMode::SHOW_CURSOR) && cursor.shape != CursorShape::Hidden && crow >= 0 && (crow as usize) < rows {
        let x = origin.x + cell_w * cursor.point.column.0 as f32;
        let y = origin.y + line_h * crow as f32;
        let ccolor = colors[NamedColor::Cursor as usize].map(|c| theme::rgb_to_hsla(c.r, c.g, c.b)).unwrap_or(theme::hex(theme::CURSOR));
        let b = Bounds::new(point(x, y), size(cell_w, line_h));
        cursor_quad = Some(if !focused {
            outline(b, ccolor, gpui::BorderStyle::Solid)
        } else {
            match cursor.shape {
                CursorShape::Beam => fill(Bounds::new(point(x, y), size(px(2.), line_h)), ccolor),
                CursorShape::Underline => fill(Bounds::new(point(x, y + line_h - px(2.)), size(cell_w, px(2.))), ccolor),
                CursorShape::HollowBlock => outline(b, ccolor, gpui::BorderStyle::Solid),
                _ => {
                    if cursor_char != ' ' {
                        let mut st = cursor_style;
                        st.fg = theme::hex(theme::BG);
                        let s: SharedString = cursor_char.to_string().into();
                        let run = make_run(s.len(), &st);
                        cursor_text = Some((point(x, y), text_system.shape_line(s, font_size, &[run], None)));
                    }
                    fill(b, ccolor)
                }
            }
        });
    }

    GridFrame { quads, lines, cursor: cursor_quad, cursor_text, line_h }
}

fn named(n: NamedColor, colors: &Colors) -> Hsla {
    if let Some(c) = colors[n as usize] {
        return theme::rgb_to_hsla(c.r, c.g, c.b);
    }
    let idx = n as usize;
    match n {
        _ if idx < 16 => theme::hex(theme::ANSI[idx]),
        NamedColor::Foreground | NamedColor::BrightForeground => theme::hex(theme::FG),
        NamedColor::DimForeground => theme::hexa(theme::FG, 0.7),
        NamedColor::Background => theme::hex(theme::BG),
        NamedColor::Cursor => theme::hex(theme::CURSOR),
        _ => {
            // Dim variants
            let base = idx.saturating_sub(NamedColor::DimBlack as usize).min(7);
            theme::hexa(theme::ANSI[base], 0.7)
        }
    }
}

fn resolve(c: AnsiColor, colors: &Colors, flags: Flags) -> Hsla {
    match c {
        AnsiColor::Spec(rgb) => theme::rgb_to_hsla(rgb.r, rgb.g, rgb.b),
        AnsiColor::Indexed(i) => {
            let i = if flags.contains(Flags::BOLD) && i < 8 { i + 8 } else { i };
            match colors[i as usize] {
                Some(rgb) => theme::rgb_to_hsla(rgb.r, rgb.g, rgb.b),
                None => {
                    let (r, g, b) = theme::indexed(i);
                    theme::rgb_to_hsla(r, g, b)
                }
            }
        }
        AnsiColor::Named(n) => {
            let n = if flags.contains(Flags::BOLD) && (n as usize) < 8 {
                bright(n)
            } else {
                n
            };
            named(n, colors)
        }
    }
}

fn bright(n: NamedColor) -> NamedColor {
    use NamedColor::*;
    match n {
        Black => BrightBlack,
        Red => BrightRed,
        Green => BrightGreen,
        Yellow => BrightYellow,
        Blue => BrightBlue,
        Magenta => BrightMagenta,
        Cyan => BrightCyan,
        White => BrightWhite,
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Keyboard → bytes
// ---------------------------------------------------------------------------

fn modifier_param(k: &Keystroke) -> u8 {
    1 + (k.modifiers.shift as u8) + (k.modifiers.alt as u8) * 2 + (k.modifiers.control as u8) * 4
}

pub fn key_to_bytes(k: &Keystroke, mode: TermMode, option_as_meta: bool) -> Option<Vec<u8>> {
    let m = &k.modifiers;
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let modp = modifier_param(k);
    let csi = |final_: char| -> Vec<u8> {
        if modp > 1 {
            format!("\x1b[1;{modp}{final_}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{final_}").into_bytes()
        } else {
            format!("\x1b[{final_}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modp > 1 {
            format!("\x1b[{n};{modp}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };

    let key = k.key.as_str();
    let out = match key {
        "enter" => {
            if m.alt { b"\x1b\r".to_vec() } else { b"\r".to_vec() }
        }
        "tab" => {
            if m.shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() }
        }
        "backspace" => {
            if m.alt { b"\x1b\x7f".to_vec() } else if m.control { b"\x08".to_vec() } else { b"\x7f".to_vec() }
        }
        "escape" => b"\x1b".to_vec(),
        // Option+←/→ jump words, like Terminal.app.
        "left" if m.alt && !m.control && !m.shift => b"\x1bb".to_vec(),
        "right" if m.alt && !m.control && !m.shift => b"\x1bf".to_vec(),
        "up" => csi('A'),
        "down" => csi('B'),
        "right" => csi('C'),
        "left" => csi('D'),
        "home" => csi('H'),
        "end" => csi('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => if modp > 1 { format!("\x1b[1;{modp}P").into_bytes() } else { b"\x1bOP".to_vec() },
        "f2" => if modp > 1 { format!("\x1b[1;{modp}Q").into_bytes() } else { b"\x1bOQ".to_vec() },
        "f3" => if modp > 1 { format!("\x1b[1;{modp}R").into_bytes() } else { b"\x1bOR".to_vec() },
        "f4" => if modp > 1 { format!("\x1b[1;{modp}S").into_bytes() } else { b"\x1bOS".to_vec() },
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        _ => {
            // Ctrl combinations.
            if m.control {
                let base = if key == "space" { ' ' } else { key.chars().next()? };
                if key.chars().count() == 1 || key == "space" {
                    let b = match base.to_ascii_lowercase() {
                        c @ 'a'..='z' => (c as u8) - b'a' + 1,
                        ' ' | '2' | '@' => 0,
                        '[' | '3' => 0x1b,
                        '\\' | '4' => 0x1c,
                        ']' | '5' => 0x1d,
                        '^' | '6' => 0x1e,
                        '_' | '-' | '/' | '7' => 0x1f,
                        '8' | '?' => 0x7f,
                        _ => return None,
                    };
                    let mut v = Vec::new();
                    if m.alt {
                        v.push(0x1b);
                    }
                    v.push(b);
                    return Some(v);
                }
                return None;
            }
            if m.alt && option_as_meta {
                let ch = if key == "space" { " ".to_string() } else { key.to_string() };
                if ch.chars().count() == 1 {
                    let ch = if m.shift { ch.to_uppercase() } else { ch };
                    let mut v = vec![0x1b];
                    v.extend_from_slice(ch.as_bytes());
                    return Some(v);
                }
            }
            let text = k.key_char.clone().or_else(|| if key == "space" { Some(" ".into()) } else { None })?;
            text.into_bytes()
        }
    };
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn ks(key: &str, ch: Option<&str>, m: Modifiers) -> Keystroke {
        Keystroke { modifiers: m, key: key.into(), key_char: ch.map(|s| s.into()) }
    }

    #[test]
    fn basic_keys() {
        let none = Modifiers::default();
        let ctrl = Modifiers { control: true, ..Default::default() };
        assert_eq!(key_to_bytes(&ks("a", Some("a"), none), TermMode::empty(), false), Some(b"a".to_vec()));
        assert_eq!(key_to_bytes(&ks("c", None, ctrl), TermMode::empty(), false), Some(vec![3]));
        assert_eq!(key_to_bytes(&ks("up", None, none), TermMode::empty(), false), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_to_bytes(&ks("up", None, none), TermMode::APP_CURSOR, false), Some(b"\x1bOA".to_vec()));
        assert_eq!(key_to_bytes(&ks("enter", None, none), TermMode::empty(), false), Some(b"\r".to_vec()));
    }
}
