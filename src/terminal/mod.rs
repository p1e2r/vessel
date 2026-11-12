//! Terminal emulation shared by local shells and SSH sessions.
//!
//! Bytes from a transport (a local PTY or an SSH channel) are parsed into an
//! `alacritty_terminal::Term` grid on a background thread; the GPUI view only
//! reads that grid when painting.

pub mod pty;
pub mod ssh;
pub mod view;

use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

/// Messages from the UI to whichever transport backs the terminal.
pub enum TransportMsg {
    Data(Vec<u8>),
    Resize { cols: u16, rows: u16, px_w: u16, px_h: u16 },
    Close,
}

/// Events from the backend to the UI.
pub enum UiEvent {
    Wakeup,
    Title(String),
    ResetTitle,
    Bell,
    Clipboard(String),
    /// The session ended. The string is shown in the tab.
    Exited(String),
    /// SSH asked to save a password the user typed (profile id, password).
    SavePassword(String, String),
}

#[derive(Clone)]
pub struct Listener {
    pub transport: flume::Sender<TransportMsg>,
    pub ui: flume::Sender<UiEvent>,
    pub size: Arc<parking_lot::Mutex<WindowSize>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(s) => {
                let _ = self.transport.send(TransportMsg::Data(s.into_bytes()));
            }
            Event::Title(t) => {
                let _ = self.ui.send(UiEvent::Title(t));
            }
            Event::ResetTitle => {
                let _ = self.ui.send(UiEvent::ResetTitle);
            }
            Event::Bell => {
                let _ = self.ui.send(UiEvent::Bell);
            }
            Event::ClipboardStore(_, s) => {
                let _ = self.ui.send(UiEvent::Clipboard(s));
            }
            Event::TextAreaSizeRequest(f) => {
                let s = f(*self.size.lock());
                let _ = self.transport.send(TransportMsg::Data(s.into_bytes()));
            }
            Event::Wakeup => {
                let _ = self.ui.send(UiEvent::Wakeup);
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

pub type SharedTerm = Arc<FairMutex<Term<Listener>>>;

/// A terminal grid plus the channels to its transport.
pub struct TermCore {
    pub term: SharedTerm,
    pub transport: flume::Sender<TransportMsg>,
    pub ui_rx: flume::Receiver<UiEvent>,
    pub listener: Listener,
}

impl TermCore {
    /// Creates the grid and returns it with the receiving end of the transport channel.
    pub fn new(scrollback: usize) -> (Self, flume::Receiver<TransportMsg>) {
        let (ttx, trx) = flume::unbounded();
        let (utx, urx) = flume::unbounded();
        let size = GridSize { cols: 100, rows: 30 };
        let listener = Listener {
            transport: ttx.clone(),
            ui: utx,
            size: Arc::new(parking_lot::Mutex::new(WindowSize { num_lines: 30, num_cols: 100, cell_width: 8, cell_height: 17 })),
        };
        let config = Config { scrolling_history: scrollback, ..Config::default() };
        let term = Term::new(config, &size, listener.clone());
        (Self { term: Arc::new(FairMutex::new(term)), transport: ttx, ui_rx: urx, listener }, trx)
    }

    pub fn sink(&self) -> Sink {
        Sink { term: self.term.clone(), processor: Processor::new(), ui: self.listener.ui.clone() }
    }
}

/// Feeds transport output into the grid. Lives on the reader thread/task.
pub struct Sink {
    term: SharedTerm,
    processor: Processor<StdSyncHandler>,
    pub ui: flume::Sender<UiEvent>,
}

impl Sink {
    pub fn feed(&mut self, bytes: &[u8]) {
        {
            let mut term = self.term.lock();
            self.processor.advance(&mut *term, bytes);
        }
        let _ = self.ui.send(UiEvent::Wakeup);
    }

    /// Print a dimmed status line (used for connection progress messages).
    pub fn info(&mut self, msg: &str) {
        let s = format!("\x1b[2m{}\x1b[0m\r\n", msg.replace('\n', "\r\n"));
        self.feed(s.as_bytes());
    }

    pub fn error(&mut self, msg: &str) {
        let s = format!("\x1b[31m{}\x1b[0m\r\n", msg.replace('\n', "\r\n"));
        self.feed(s.as_bytes());
    }

    pub fn exited(&self, reason: impl Into<String>) {
        let _ = self.ui.send(UiEvent::Exited(reason.into()));
    }
}
