// NOTE: alacritty_terminal's API shifts between releases. Verify these paths
// against docs.rs for whichever 0.26.x actually resolves before building.
use alacritty_terminal::event::{Event as TermEvent, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::tty;
use cosmic::iced::widget::canvas;
use std::sync::Arc;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

pub const CELL_WIDTH: f32 = 9.0;
pub const CELL_HEIGHT: f32 = 18.0;

#[derive(Clone)]
pub struct EventProxy(UnboundedSender<TermEvent>);

impl EventListener for EventProxy {
    fn send_event(&self, event: TermEvent) {
        let _ = self.0.send(event);
    }
}

struct TermDimensions {
    cols: u16,
    rows: u16,
}

impl Dimensions for TermDimensions {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

pub struct Terminal {
    pub term: Arc<FairMutex<Term<EventProxy>>>,
    notifier: Notifier,
    last_size: std::cell::Cell<(u16, u16)>,
    /// Caches the rendered glyph/rect geometry so an idle pane costs nothing
    /// to redraw. Without this, canvas::Program::draw reshapes every glyph
    /// on the grid on every wakeup from ANY pane, which is what made nano's
    /// constant full-screen repaints lag. Cleared via `invalidate()` only
    /// when this specific pane's content, selection, or scroll actually
    /// changes.
    pub redraw_cache: canvas::Cache,
}

fn default_shell() -> String {
    let from_env = std::env::var("SHELL").unwrap_or_default();
    for candidate in [from_env.as_str(), "/bin/bash", "/bin/sh"] {
        if !candidate.is_empty() && std::path::Path::new(candidate).exists() {
            return candidate.to_string();
        }
    }
    "/bin/sh".to_string()
}

fn window_size(cols: u16, rows: u16) -> WindowSize {
    WindowSize {
        num_lines: rows,
        num_cols: cols,
        cell_width: CELL_WIDTH as u16,
        cell_height: CELL_HEIGHT as u16,
    }
}

impl Terminal {
    /// Spawns a shell over a PTY and starts the alacritty event loop on its own
    /// thread. Returns the terminal handle plus a channel of terminal events
    /// (redraw wakeups, title changes, exit) for the caller to subscribe to.
    pub fn spawn(cols: u16, rows: u16) -> (Self, UnboundedReceiver<TermEvent>) {
        let (tx, rx) = unbounded_channel();
        let proxy = EventProxy(tx);

        // Every real terminal emulator sets TERM (and usually COLORTERM) for
        // the shell it spawns — that's how the shell and tools like `ls`,
        // and color-detection logic in .bashrc, know color output is safe.
        // Without this, a shell spawned in a minimal environment (like
        // cosmic-panel's, which isn't itself a terminal) inherits no TERM at
        // all and most prompts/tools quietly stay monochrome.
        let mut env = std::collections::HashMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert("COLORTERM".to_string(), "truecolor".to_string());

        let pty_options = tty::Options {
            shell: Some(tty::Shell::new(default_shell(), Vec::new())),
            working_directory: None,
            drain_on_exit: false,
            env,
        };

        let pty = tty::new(&pty_options, window_size(cols, rows), 0)
            .expect("failed to spawn pty");

        let dims = TermDimensions { cols, rows };
        // NOTE: field name for scrollback limit may differ across
        // alacritty_terminal versions — verify against docs.rs if this
        // doesn't compile against the pinned 0.26.x.
        let mut term_config = TermConfig::default();
        term_config.scrolling_history = 100_000;
        let term = Arc::new(FairMutex::new(Term::new(term_config, &dims, proxy.clone())));

        let event_loop =
            EventLoop::new(term.clone(), proxy, pty, false, false).expect("event loop init");
        let notifier = Notifier(event_loop.channel());
        let _ = event_loop.spawn();

        (
            Self {
                term,
                notifier,
                last_size: std::cell::Cell::new((cols, rows)),
                redraw_cache: canvas::Cache::new(),
            },
            rx,
        )
    }

    /// Drops the cached geometry so the next draw() call rebuilds it. Call
    /// whenever this pane's visible content, selection, or scroll offset
    /// changes — not on unrelated app messages, which is the whole point.
    pub fn invalidate(&self) {
        self.redraw_cache.clear();
    }

    pub fn write_input(&self, data: &[u8]) {
        self.notifier.notify(data.to_vec());
    }

    pub fn paste(&self, text: &str) {
        let sanitized = text.replace("\x1b[201~", "");
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        if bracketed {
            let mut data = Vec::with_capacity(sanitized.len() + 12);
            data.extend_from_slice(b"\x1b[200~");
            data.extend_from_slice(sanitized.as_bytes());
            data.extend_from_slice(b"\x1b[201~");
            self.write_input(&data);
        } else {
            self.write_input(sanitized.as_bytes());
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        self.last_size.set((cols, rows));
        self.term.lock().resize(TermDimensions { cols, rows });
        let _ = self
            .notifier
            .0
            .send(Msg::Resize(window_size(cols, rows)));
        self.invalidate();
    }

    /// Only issues an actual resize if the size changed since last applied —
    /// safe to call unconditionally every render frame from the real
    /// on-screen bounds, which is what makes pane sizing self-correcting
    /// instead of depending on a separately-tracked config value that can
    /// drift out of sync with reality.
    pub fn resize_if_changed(&self, cols: u16, rows: u16) {
        if self.last_size.get() != (cols, rows) {
            self.resize(cols, rows);
        }
    }

    pub fn scroll(&self, lines: i32) {
        self.term
            .lock()
            .scroll_display(alacritty_terminal::grid::Scroll::Delta(lines));
        self.invalidate();
    }

    // NOTE: `selection_to_string` is assumed to exist on Term for pulling the
    // current selection's text out for clipboard copy — verify against
    // docs.rs for the pinned alacritty_terminal version if this doesn't
    // compile.
    pub fn selection_text(&self) -> Option<String> {
        self.term.lock().selection_to_string()
    }

    pub fn shutdown(&self) {
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}
