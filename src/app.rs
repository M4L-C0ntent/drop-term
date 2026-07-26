use crate::pane::{PaneId, SplitDirection};
use crate::terminal::{Terminal, CELL_HEIGHT, CELL_WIDTH};
use alacritty_terminal::event::Event as TermEvent;
use cosmic::app::{Core, Task};
use cosmic::iced::futures::stream;
use cosmic::iced::keyboard;
use cosmic::iced::platform_specific::shell::commands::layer_surface as ls;
use cosmic::iced::runtime::platform_specific::wayland::layer_surface::SctkLayerSurfaceSettings;
use cosmic::iced::widget::scrollable::{Direction as ScrollDirection, Scrollbar};
use cosmic::iced::widget::{mouse_area, Column, Row};
use cosmic::iced::window;
use cosmic::iced::{Background, Border, Color, Length, Subscription};
use cosmic::widget;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc::UnboundedReceiver, Mutex as TokioMutex};

pub const APP_ID: &str = "com.github.drop-term";
const MAX_TABS: usize = 15;
const MAX_GRID_COLS: usize = 4;
const MAX_GRID_ROWS: usize = 4;
const TAB_BAR_HEIGHT: f32 = 32.0;

struct PaneEntry {
    terminal: Terminal,
    rx: Arc<TokioMutex<UnboundedReceiver<TermEvent>>>,
    scroll_remainder: f32,
}

struct Tab {
    title: String,
    panes: HashMap<PaneId, PaneEntry>,
    /// Each inner Vec is one column's panes, top to bottom. Columns can have
    /// different lengths — a vertical split only grows the one column the
    /// active pane lives in, not the whole tab.
    columns: Vec<Vec<PaneId>>,
    active_pane: PaneId,
}

pub struct App {
    core: Core,
    window_id: Option<window::Id>,
    showing_preferences: bool,
    config: crate::config::AppConfig,
    tabs: Vec<Tab>,
    active_tab: usize,
    next_pane_id: PaneId,
    pinned: bool,
    surface_opened_at: Option<Instant>,
    /// Bumped on every resize event; a debounced save only writes if this
    /// still matches its own generation after the delay, so a live drag
    /// resize (many events/sec) doesn't do a synchronous disk write per
    /// frame — only once, ~300ms after resizing settles.
    resize_save_generation: Arc<std::sync::atomic::AtomicU64>,
}

#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    TogglePreferences,
    UserHostColorInput(String),
    DirectoryColorInput(String),
    SurfaceClosed(window::Id),
    SurfaceUnfocused(window::Id),
    TogglePin,
    Resized(window::Id, u32, u32),
    TerminalUpdated(PaneId),
    KeyPressed(keyboard::Key, keyboard::Modifiers, Option<String>),
    SplitPane(SplitDirection),
    ClosePane(PaneId),
    SelectPane(PaneId),
    NewTab,
    CloseTab(usize),
    SelectTab(usize),
    CycleTabNext,
    CycleTabPrev,
    CyclePaneNext,
    CyclePanePrev,
    CopySelection,
    PasteClipboard,
    ClipboardPasted(Option<String>),
    PaneScroll(PaneId, cosmic::iced::mouse::ScrollDelta),
}

fn cols_rows(width: u32, height: u32) -> (u16, u16) {
    let usable_height = (height as f32 - TAB_BAR_HEIGHT).max(CELL_HEIGHT);
    let cols = ((width as f32 / CELL_WIDTH) as u16).max(1);
    let rows = ((usable_height / CELL_HEIGHT) as u16).max(1);
    (cols, rows)
}

// Minimal viable key -> byte translation: printable text, Enter/Tab/Backspace/Escape,
// arrow keys (ANSI cursor sequences), Ctrl+letter control codes, Ctrl+_ (undo), and
// Alt+f/b/d/. (readline word-movement/delete/last-arg, sent as ESC + letter).
fn key_to_bytes(
    key: &keyboard::Key,
    modifiers: keyboard::Modifiers,
    text: &Option<String>,
) -> Vec<u8> {
    use keyboard::key::{Key, Named};

    if modifiers.alt() && !modifiers.control() {
        if let Key::Character(c) = key {
            if let Some(ch) = c.chars().next() {
                let lower = ch.to_ascii_lowercase();
                if matches!(lower, 'f' | 'b' | 'd' | '.') {
                    return vec![0x1b, lower as u8];
                }
            }
        }
    }

    if modifiers.control() {
        if let Key::Character(c) = key {
            if let Some(ch) = c.chars().next() {
                if ch == '_' || ch == '-' {
                    return vec![0x1f];
                }
                let upper = ch.to_ascii_uppercase();
                if upper.is_ascii_alphabetic() {
                    return vec![(upper as u8) - b'A' + 1];
                }
            }
        }
    }

    match key {
        Key::Named(Named::Enter) => vec![b'\r'],
        Key::Named(Named::Tab) => vec![b'\t'],
        Key::Named(Named::Backspace) => vec![0x7f],
        Key::Named(Named::Escape) => vec![0x1b],
        Key::Named(Named::ArrowUp) => vec![0x1b, b'[', b'A'],
        Key::Named(Named::ArrowDown) => vec![0x1b, b'[', b'B'],
        Key::Named(Named::ArrowRight) => vec![0x1b, b'[', b'C'],
        Key::Named(Named::ArrowLeft) => vec![0x1b, b'[', b'D'],
        _ => text.as_ref().map(|t| t.as_bytes().to_vec()).unwrap_or_default(),
    }
}

// Subscription::run_with needs a Hash data value and a plain (non-capturing) fn
// pointer builder. We hash only the pane id and carry the receiver unhashed.
#[derive(Clone)]
struct TermSubData {
    id: PaneId,
    rx: Arc<TokioMutex<UnboundedReceiver<TermEvent>>>,
}

impl std::hash::Hash for TermSubData {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

// NOTE: `Event::Exit` is alacritty_terminal's signal that the pty/child has
// gone away (e.g. the shell ran `exit`). Verify this variant name against
// docs.rs for the pinned version if it doesn't match.
fn term_event_stream(data: &TermSubData) -> impl cosmic::iced::futures::Stream<Item = Message> {
    let rx = data.rx.clone();
    let pane_id = data.id;
    stream::unfold(rx, move |rx| async move {
        let mut guard = rx.lock().await;
        let first = guard.recv().await?;
        if matches!(first, TermEvent::Exit) {
            drop(guard);
            return Some((Message::ClosePane(pane_id), rx));
        }
        // A full-screen repaint (nano, vim scrolling, a big `cat`) can queue
        // many wakeups within one frame; they'd all collapse to the same
        // redraw anyway via the cache from Section 1, so drain the rest
        // (non-blocking) and emit just one message instead of running a
        // full update/view/layout pass per PTY chunk.
        while let Ok(next) = guard.try_recv() {
            if matches!(next, TermEvent::Exit) {
                drop(guard);
                return Some((Message::ClosePane(pane_id), rx));
            }
        }
        drop(guard);
        Some((Message::TerminalUpdated(pane_id), rx))
    })
}

// Lets an external OS-level custom keyboard shortcut toggle this already-
// running applet instance (e.g. bound to `pkill -SIGUSR1 -x drop-term` in
// COSMIC Settings > Keyboard > Custom Shortcuts) — a shortcut can only run a
// command, and running the binary again would just launch a second
// instance rather than talk to this one, so an external signal is the
// bridge between "OS-level hotkey" and "this specific running process".
fn sigusr1_stream() -> impl cosmic::iced::futures::Stream<Item = Message> {
    use tokio::signal::unix::{signal, SignalKind};
    let sig = signal(SignalKind::user_defined1()).expect("failed to install SIGUSR1 handler");
    stream::unfold(sig, |mut sig| async move {
        sig.recv().await;
        Some((Message::TogglePopup, sig))
    })
}

impl App {
    fn reserve_pane_id(&mut self) -> PaneId {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        id
    }

    fn spawn_pane_with_id(cols: u16, rows: u16) -> PaneEntry {
        let (terminal, rx) = Terminal::spawn(cols, rows);
        PaneEntry {
            terminal,
            rx: Arc::new(TokioMutex::new(rx)),
            scroll_remainder: 0.0,
        }
    }

    fn renumber_tabs(&mut self) {
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            tab.title = crate::fl!("tab-title", number = ((i + 1) as i32));
        }
    }

    /// Width is split evenly across columns (uniform); height is split
    /// evenly within each column independently, since columns can have
    /// different row counts. This only needs to be a reasonable starting
    /// guess — the canvas in term_view.rs measures its own real rendered
    /// bounds every frame and self-corrects, so precision here isn't
    /// load-bearing.
    fn recompute_layout(&mut self) {
        let (window_cols, window_rows) =
            cols_rows(self.config.window_width, self.config.window_height);
        if let Some(tab) = self.tabs.get(self.active_tab) {
            let num_cols = (tab.columns.len().max(1)) as u16;
            let pane_cols = (window_cols / num_cols).max(1);
            for column in &tab.columns {
                let num_rows = (column.len().max(1)) as u16;
                let pane_rows = (window_rows / num_rows).max(1);
                for id in column {
                    if let Some(entry) = tab.panes.get(id) {
                        entry.terminal.resize_if_changed(pane_cols, pane_rows);
                    }
                }
            }
        }
    }

    fn render_pane_cell(
        id: PaneId,
        panes: &HashMap<PaneId, PaneEntry>,
        active_pane: PaneId,
        user_host_color: Color,
        directory_color: Color,
    ) -> cosmic::Element<'_, Message> {
        let Some(entry) = panes.get(&id) else {
            return widget::text("").into();
        };
        let is_active = id == active_pane;
        let framed = widget::container(crate::term_view::terminal_canvas(
            &entry.terminal,
            id,
            user_host_color,
            directory_color,
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(2.0)
        .style(move |theme: &cosmic::Theme| {
            let border_color = if is_active {
                theme.cosmic().accent_color().into()
            } else {
                Color::TRANSPARENT
            };
            widget::container::Style {
                border: Border {
                    color: border_color,
                    width: 2.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            }
        });
        mouse_area(framed)
            .on_press(Message::SelectPane(id))
            .on_scroll(move |delta| Message::PaneScroll(id, delta))
            .into()
    }

    fn render_pane_grid(
        tab: &Tab,
        user_host_color: Color,
        directory_color: Color,
    ) -> cosmic::Element<'_, Message> {
        let mut row = Row::new().width(Length::Fill).height(Length::Fill);
        for column in &tab.columns {
            let mut col_widget = Column::new().width(Length::FillPortion(1)).height(Length::Fill);
            for &id in column {
                col_widget = col_widget.push(
                    widget::container(Self::render_pane_cell(
                        id,
                        &tab.panes,
                        tab.active_pane,
                        user_host_color,
                        directory_color,
                    ))
                    .width(Length::Fill)
                    .height(Length::FillPortion(1)),
                );
            }
            row = row.push(col_widget);
        }
        row.into()
    }

    fn find_active_column(tab: &Tab) -> Option<(usize, usize)> {
        tab.columns.iter().enumerate().find_map(|(ci, column)| {
            column
                .iter()
                .position(|&id| id == tab.active_pane)
                .map(|ri| (ci, ri))
        })
    }

    fn can_split_horizontal(tab: &Tab) -> bool {
        tab.columns.len() < MAX_GRID_COLS
    }

    fn can_split_vertical(tab: &Tab) -> bool {
        Self::find_active_column(tab)
            .map(|(ci, _)| tab.columns[ci].len() < MAX_GRID_ROWS)
            .unwrap_or(false)
    }

    /// Uses mouse_area rather than a real Button widget on purpose: buttons
    /// participate in keyboard Tab-focus traversal, which is exactly what we
    /// don't want — Tab should only ever reach the terminal for shell
    /// autocomplete, never cycle focus between these controls. mouse_area has
    /// no keyboard-focus semantics at all, so there's nothing for Tab to land
    /// on. Since mouse_area doesn't auto-dim like Button does when disabled,
    /// we fade the text color manually when `message` is None.
    fn click_label(label: impl Into<String>, message: Option<Message>) -> cosmic::Element<'static, Message> {
        let enabled = message.is_some();
        let mut color: Color = cosmic::theme::active().cosmic().on_bg_color().into();
        if !enabled {
            color.a *= 0.35;
        }
        let text = widget::text(label.into()).class(cosmic::theme::Text::Color(color));
        let content = widget::container(text).padding([2, 6]);
        match message {
            Some(m) => mouse_area(content).on_press(m).into(),
            None => content.into(),
        }
    }

    fn tab_bar(&self) -> cosmic::Element<'_, Message> {
        let mut tabs_row = Row::new().spacing(4);
        tabs_row = tabs_row.push(Self::click_label("⚙", Some(Message::TogglePreferences)));
        for (i, tab) in self.tabs.iter().enumerate() {
            let is_active = i == self.active_tab;
            let entry = Row::new()
                .spacing(2)
                .push(Self::click_label(
                    tab.title.clone(),
                    Some(Message::SelectTab(i)),
                ))
                .push(Self::click_label("×", Some(Message::CloseTab(i))));
            tabs_row = tabs_row.push(widget::container(entry).style(move |theme: &cosmic::Theme| {
                let bg = if is_active {
                    let mut c: Color = theme.cosmic().accent_color().into();
                    c.a = 0.18;
                    c
                } else {
                    Color::TRANSPARENT
                };
                widget::container::Style {
                    background: Some(Background::Color(bg)),
                    ..Default::default()
                }
            }));
        }
        tabs_row = tabs_row.push(Self::click_label("+", Some(Message::NewTab)));

        let tabs_scroll = widget::scrollable(tabs_row)
            .direction(ScrollDirection::Horizontal(Scrollbar::default()))
            .width(Length::Fill);

        let active_pane = self.tabs.get(self.active_tab).map(|t| t.active_pane);
        let mut controls = Row::new().spacing(4);

        let h_enabled = self
            .tabs
            .get(self.active_tab)
            .map(Self::can_split_horizontal)
            .unwrap_or(false);
        controls = controls.push(Self::click_label(
            "↔",
            h_enabled.then_some(Message::SplitPane(SplitDirection::Horizontal)),
        ));

        let v_enabled = self
            .tabs
            .get(self.active_tab)
            .map(Self::can_split_vertical)
            .unwrap_or(false);
        controls = controls.push(Self::click_label(
            "↕",
            v_enabled.then_some(Message::SplitPane(SplitDirection::Vertical)),
        ));

        if let Some(id) = active_pane {
            controls = controls.push(Self::click_label("×", Some(Message::ClosePane(id))));
        }

        Row::new()
            .spacing(4)
            .padding(4)
            .height(Length::Fixed(TAB_BAR_HEIGHT))
            .push(tabs_scroll)
            .push(controls)
            .into()
    }
}

impl cosmic::Application for App {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: ()) -> (Self, Task<Message>) {
        (
            Self {
                core,
                window_id: None,
                showing_preferences: false,
                config: crate::config::load(),
                tabs: Vec::new(),
                active_tab: 0,
                next_pane_id: 0,
                pinned: false,
                surface_opened_at: None,
                resize_save_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
            Task::none(),
        )
    }

    fn view(&self) -> cosmic::Element<'_, Message> {
        self.core
            .applet
            .icon_button("utilities-terminal-symbolic")
            .on_press_down(Message::TogglePopup)
            .into()
    }

    fn view_window(&self, _id: window::Id) -> cosmic::Element<'_, Message> {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return widget::text("").into();
        };
        if tab.panes.is_empty() {
            return widget::text("").into();
        }
        let body: cosmic::Element<'_, Message> = if self.showing_preferences {
            crate::preferences::view(&self.config)
        } else {
            let user_host_color = Color::from_rgb8(
                self.config.user_host_color.0,
                self.config.user_host_color.1,
                self.config.user_host_color.2,
            );
            let directory_color = Color::from_rgb8(
                self.config.directory_color.0,
                self.config.directory_color.1,
                self.config.directory_color.2,
            );
            Self::render_pane_grid(tab, user_host_color, directory_color)
        };
        let content = Column::new()
            .width(Length::Fill)
            .height(Length::Fill)
            .push(self.tab_bar())
            .push(body);

        widget::container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|theme: &cosmic::Theme| widget::container::Style {
                background: Some(Background::Color(theme.cosmic().bg_color().into())),
                ..Default::default()
            })
            .into()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::TogglePopup => {
                if let Some(id) = self.window_id.take() {
                    return ls::destroy_layer_surface(id);
                }
                let id = window::Id::unique();
                self.window_id = Some(id);
                self.surface_opened_at = Some(Instant::now());

                if self.tabs.is_empty() {
                    let (cols, rows) =
                        cols_rows(self.config.window_width, self.config.window_height);
                    let pane_id = self.reserve_pane_id();
                    let entry = Self::spawn_pane_with_id(cols, rows);
                    let mut panes = HashMap::new();
                    panes.insert(pane_id, entry);
                    self.tabs = vec![Tab {
                        title: crate::fl!("tab-title", number = 1),
                        panes,
                        columns: vec![vec![pane_id]],
                        active_pane: pane_id,
                    }];
                    self.active_tab = 0;
                } else {
                    self.recompute_layout();
                }

                ls::get_layer_surface(SctkLayerSurfaceSettings {
                    id,
                    layer: ls::Layer::Top,
                    anchor: ls::Anchor::TOP | ls::Anchor::LEFT | ls::Anchor::RIGHT,
                    keyboard_interactivity: ls::KeyboardInteractivity::OnDemand,
                    size: Some((Some(self.config.window_width), Some(self.config.window_height))),
                    exclusive_zone: 0,
                    ..Default::default()
                })
            }
            Message::TogglePreferences => {
                self.showing_preferences = !self.showing_preferences;
                Task::none()
            }
            Message::UserHostColorInput(text) => {
                if let Some(rgb) = crate::preferences::parse_hex_color(&text) {
                    self.config.user_host_color = rgb;
                    crate::config::save(&self.config);
                }
                Task::none()
            }
            Message::DirectoryColorInput(text) => {
                if let Some(rgb) = crate::preferences::parse_hex_color(&text) {
                    self.config.directory_color = rgb;
                    crate::config::save(&self.config);
                }
                Task::none()
            }
            Message::SurfaceClosed(id) => {
                if self.window_id == Some(id) {
                    self.window_id = None;
                }
                Task::none()
            }
            Message::SurfaceUnfocused(id) => {
                let just_opened = self
                    .surface_opened_at
                    .map(|t| t.elapsed() < Duration::from_millis(200))
                    .unwrap_or(false);
                if self.window_id == Some(id) && !self.pinned && !just_opened {
                    self.window_id = None;
                    return ls::destroy_layer_surface(id);
                }
                Task::none()
            }
            Message::TogglePin => {
                self.pinned = !self.pinned;
                Task::none()
            }
            Message::Resized(id, width, height) => {
                log::debug!("Resized event: {id:?} {width}x{height} px");
                if self.window_id != Some(id) {
                    return Task::none();
                }
                let just_opened = self
                    .surface_opened_at
                    .map(|t| t.elapsed() < Duration::from_millis(300))
                    .unwrap_or(false);
                if just_opened {
                    return Task::none();
                }
                self.config.window_width = width.max(500);
                self.config.window_height = height.max(300);
                self.recompute_layout();

                // A live drag-resize fires this event continuously; only
                // persist once it's been quiet for a moment, off the UI
                // thread, instead of a synchronous serialize+fs::write per
                // frame. If a newer resize supersedes this one before the
                // delay elapses, the generation check below skips the
                // stale write — the newest one always wins.
                // NOTE: assumes cosmic::executor::Default keeps a Tokio
                // runtime entered for the app's lifetime (tokio is already
                // a direct dependency here for exactly this reason); if
                // tokio::spawn ever panics with "no reactor running", this
                // needs a Handle threaded in explicitly instead.
                let generation = self
                    .resize_save_generation
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1;
                let generation_handle = self.resize_save_generation.clone();
                let config_snapshot = self.config.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    if generation_handle.load(std::sync::atomic::Ordering::SeqCst) == generation {
                        crate::config::save(&config_snapshot);
                    }
                });
                Task::none()
            }
            Message::TerminalUpdated(id) => {
                for tab in &self.tabs {
                    if let Some(entry) = tab.panes.get(&id) {
                        entry.terminal.invalidate();
                        break;
                    }
                }
                Task::none()
            }
            Message::SplitPane(SplitDirection::Horizontal) => {
                let Some(tab) = self.tabs.get(self.active_tab) else {
                    return Task::none();
                };
                if !Self::can_split_horizontal(tab) {
                    return Task::none();
                }
                let (window_cols, window_rows) =
                    cols_rows(self.config.window_width, self.config.window_height);
                let num_cols = (tab.columns.len() + 1) as u16;
                let approx_cols = (window_cols / num_cols).max(1);

                let id = self.reserve_pane_id();
                let entry = Self::spawn_pane_with_id(approx_cols, window_rows);
                let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                    return Task::none();
                };
                tab.panes.insert(id, entry);
                tab.columns.push(vec![id]);
                tab.active_pane = id;
                self.recompute_layout();
                Task::none()
            }
            Message::SplitPane(SplitDirection::Vertical) => {
                let Some(tab) = self.tabs.get(self.active_tab) else {
                    return Task::none();
                };
                let Some((col_idx, row_idx)) = Self::find_active_column(tab) else {
                    return Task::none();
                };
                if !Self::can_split_vertical(tab) {
                    return Task::none();
                }
                let column_len = tab.columns[col_idx].len();
                let (window_cols, window_rows) =
                    cols_rows(self.config.window_width, self.config.window_height);
                let approx_cols = (window_cols / tab.columns.len().max(1) as u16).max(1);
                let approx_rows = (window_rows / (column_len + 1) as u16).max(1);

                let id = self.reserve_pane_id();
                let entry = Self::spawn_pane_with_id(approx_cols, approx_rows);
                let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                    return Task::none();
                };
                tab.panes.insert(id, entry);
                tab.columns[col_idx].insert(row_idx + 1, id);
                tab.active_pane = id;
                self.recompute_layout();
                Task::none()
            }
            Message::ClosePane(id) => {
                let Some(tab_idx) = self.tabs.iter().position(|t| t.panes.contains_key(&id))
                else {
                    return Task::none();
                };
                let total_panes: usize =
                    self.tabs[tab_idx].columns.iter().map(|c| c.len()).sum();
                if total_panes <= 1 {
                    return self.update(Message::CloseTab(tab_idx));
                }
                let tab = &mut self.tabs[tab_idx];
                if let Some(col_idx) = tab.columns.iter().position(|c| c.contains(&id)) {
                    tab.columns[col_idx].retain(|&pid| pid != id);
                    if tab.columns[col_idx].is_empty() {
                        tab.columns.remove(col_idx);
                    }
                }
                if let Some(entry) = tab.panes.remove(&id) {
                    entry.terminal.shutdown();
                }
                if !tab.columns.iter().any(|c| c.contains(&tab.active_pane)) {
                    tab.active_pane = tab
                        .columns
                        .iter()
                        .flatten()
                        .next()
                        .copied()
                        .unwrap_or(id);
                }
                self.recompute_layout();
                Task::none()
            }
            Message::SelectPane(id) => {
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    tab.active_pane = id;
                }
                Task::none()
            }
            Message::NewTab => {
                if self.tabs.len() >= MAX_TABS {
                    return Task::none();
                }
                let (cols, rows) = cols_rows(self.config.window_width, self.config.window_height);
                let pane_id = self.reserve_pane_id();
                let entry = Self::spawn_pane_with_id(cols, rows);
                let mut panes = HashMap::new();
                panes.insert(pane_id, entry);
                let title = crate::fl!("tab-title", number = ((self.tabs.len() + 1) as i32));
                self.tabs.push(Tab {
                    title,
                    panes,
                    columns: vec![vec![pane_id]],
                    active_pane: pane_id,
                });
                self.active_tab = self.tabs.len() - 1;
                Task::none()
            }
            Message::CloseTab(idx) => {
                if idx >= self.tabs.len() {
                    return Task::none();
                }
                if self.tabs.len() <= 1 {
                    let tab = self.tabs.remove(idx);
                    for (_, entry) in tab.panes {
                        entry.terminal.shutdown();
                    }
                    if let Some(id) = self.window_id.take() {
                        return ls::destroy_layer_surface(id);
                    }
                    return Task::none();
                }
                let tab = self.tabs.remove(idx);
                for (_, entry) in tab.panes {
                    entry.terminal.shutdown();
                }
                if self.active_tab >= self.tabs.len() {
                    self.active_tab = self.tabs.len() - 1;
                } else if idx < self.active_tab {
                    self.active_tab -= 1;
                }
                self.renumber_tabs();
                self.recompute_layout();
                Task::none()
            }
            Message::SelectTab(idx) => {
                if idx < self.tabs.len() {
                    self.active_tab = idx;
                    self.recompute_layout();
                }
                Task::none()
            }
            Message::KeyPressed(key, modifiers, text) => {
                // F12: hide the dropdown. This only works while the surface
                // already has keyboard focus — a true system-wide toggle
                // would need the XDG Global Shortcuts portal, out of scope
                // here (same limitation noted for the applet click-to-open
                // design generally).
                if key == keyboard::Key::Named(keyboard::key::Named::F12) {
                    return self.update(Message::TogglePopup);
                }
                if modifiers.control() && key == keyboard::Key::Named(keyboard::key::Named::Tab) {
                    return self.update(if modifiers.shift() {
                        Message::CycleTabPrev
                    } else {
                        Message::CycleTabNext
                    });
                }
                if modifiers.control() && modifiers.shift() {
                    if let keyboard::Key::Character(c) = &key {
                        match c.as_str() {
                            "o" | "O" => {
                                return self.update(Message::SplitPane(SplitDirection::Horizontal))
                            }
                            "e" | "E" => {
                                return self.update(Message::SplitPane(SplitDirection::Vertical))
                            }
                            "q" | "Q" => {
                                let active_pane =
                                    self.tabs.get(self.active_tab).map(|t| t.active_pane);
                                if let Some(id) = active_pane {
                                    return self.update(Message::ClosePane(id));
                                }
                                return Task::none();
                            }
                            "t" | "T" => return self.update(Message::NewTab),
                            "w" | "W" => return self.update(Message::CloseTab(self.active_tab)),
                            "p" | "P" => return self.update(Message::TogglePin),
                            "c" | "C" => return self.update(Message::CopySelection),
                            "v" | "V" => return self.update(Message::PasteClipboard),
                            "z" | "Z" => return self.update(Message::CyclePaneNext),
                            "x" | "X" => return self.update(Message::CyclePanePrev),
                            _ => {}
                        }
                    }
                }
                if let Some(tab) = self.tabs.get(self.active_tab) {
                    if let Some(entry) = tab.panes.get(&tab.active_pane) {
                        let bytes = key_to_bytes(&key, modifiers, &text);
                        if !bytes.is_empty() {
                            entry.terminal.write_input(&bytes);
                        }
                    }
                }
                Task::none()
            }
            Message::CycleTabNext => {
                if !self.tabs.is_empty() {
                    self.active_tab = (self.active_tab + 1) % self.tabs.len();
                    self.recompute_layout();
                }
                Task::none()
            }
            Message::CycleTabPrev => {
                if !self.tabs.is_empty() {
                    self.active_tab = (self.active_tab + self.tabs.len() - 1) % self.tabs.len();
                    self.recompute_layout();
                }
                Task::none()
            }
            Message::CyclePaneNext => {
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    let ids: Vec<PaneId> = tab.columns.iter().flatten().copied().collect();
                    if let Some(pos) = ids.iter().position(|&id| id == tab.active_pane) {
                        tab.active_pane = ids[(pos + 1) % ids.len()];
                    }
                }
                Task::none()
            }
            Message::CyclePanePrev => {
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    let ids: Vec<PaneId> = tab.columns.iter().flatten().copied().collect();
                    if let Some(pos) = ids.iter().position(|&id| id == tab.active_pane) {
                        tab.active_pane = ids[(pos + ids.len() - 1) % ids.len()];
                    }
                }
                Task::none()
            }
            Message::CopySelection => {
                if let Some(tab) = self.tabs.get(self.active_tab) {
                    if let Some(entry) = tab.panes.get(&tab.active_pane) {
                        if let Some(text) = entry.terminal.selection_text() {
                            return cosmic::iced::clipboard::write(text);
                        }
                    }
                }
                Task::none()
            }
            Message::PasteClipboard => cosmic::iced::clipboard::read()
                .map(Message::ClipboardPasted)
                .map(cosmic::Action::App),
            Message::ClipboardPasted(content) => {
                if let Some(text) = content {
                    if let Some(tab) = self.tabs.get(self.active_tab) {
                        if let Some(entry) = tab.panes.get(&tab.active_pane) {
                            entry.terminal.paste(&text);
                        }
                    }
                }
                Task::none()
            }
            Message::PaneScroll(id, delta) => {
                let raw_lines = match delta {
                    cosmic::iced::mouse::ScrollDelta::Lines { y, .. } => y,
                    cosmic::iced::mouse::ScrollDelta::Pixels { y, .. } => y / CELL_HEIGHT,
                };
                for tab in &mut self.tabs {
                    if let Some(entry) = tab.panes.get_mut(&id) {
                        entry.scroll_remainder += raw_lines;
                        let whole = entry.scroll_remainder.trunc();
                        entry.scroll_remainder -= whole;
                        if whole.abs() >= 1.0 {
                            entry.terminal.scroll(whole as i32);
                        }
                        break;
                    }
                }
                Task::none()
            }
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let window_events = cosmic::iced::event::listen_with(|event, _status, id| match event {
            cosmic::iced::Event::Window(cosmic::iced::window::Event::CloseRequested) => {
                Some(Message::SurfaceClosed(id))
            }
            cosmic::iced::Event::Window(cosmic::iced::window::Event::Unfocused) => {
                Some(Message::SurfaceUnfocused(id))
            }
            cosmic::iced::Event::Window(cosmic::iced::window::Event::Resized(size)) => {
                Some(Message::Resized(id, size.width as u32, size.height as u32))
            }
            cosmic::iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers,
                text,
                ..
            }) => Some(Message::KeyPressed(key, modifiers, text.map(|t| t.to_string()))),
            _ => None,
        });

        let mut subs = vec![window_events, Subscription::run(sigusr1_stream)];
        for tab in &self.tabs {
            for (id, entry) in &tab.panes {
                subs.push(Subscription::run_with(
                    TermSubData {
                        id: *id,
                        rx: entry.rx.clone(),
                    },
                    term_event_stream,
                ));
            }
        }
        Subscription::batch(subs)
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}
