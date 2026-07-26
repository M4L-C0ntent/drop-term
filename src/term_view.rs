use crate::terminal::{Terminal, CELL_HEIGHT, CELL_WIDTH};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};
use cosmic::iced::widget::canvas::{self, Canvas, Geometry};
use cosmic::iced::{Color, Rectangle};
use cosmic::{Element, Renderer, Theme};

pub struct TerminalView<'a> {
    pub terminal: &'a Terminal,
    pub pane_id: crate::pane::PaneId,
    pub user_host_color: Color,
    pub directory_color: Color,
}

// Simplification: named colors map to the standard 16-color palette and
// Foreground/Background fall back to sensible defaults. True 256-color/RGB
// cells still render correctly since those carry an explicit Spec(Rgb).
// Green (index 2/10, NamedColor::Green/BrightGreen/DimGreen) and Blue
// (index 4/12, NamedColor::Blue/BrightBlue/DimBlue) are overridden by the
// user's configured colors — the conventional ANSI slots the default
fn resolve_color(
    c: AnsiColor,
    default: Color,
    user_host_color: Color,
    directory_color: Color,
) -> Color {
    match c {
        AnsiColor::Spec(rgb) => Color::from_rgb8(rgb.r, rgb.g, rgb.b),
        AnsiColor::Indexed(i) => match i {
            2 | 10 => user_host_color,
            4 | 12 => directory_color,
            _ => {
                const PALETTE: [(u8, u8, u8); 16] = [
                    (0, 0, 0), (205, 0, 0), (0, 205, 0), (205, 205, 0),
                    (0, 0, 238), (205, 0, 205), (0, 205, 205), (229, 229, 229),
                    (127, 127, 127), (255, 0, 0), (0, 255, 0), (255, 255, 0),
                    (92, 92, 255), (255, 0, 255), (0, 255, 255), (255, 255, 255),
                ];
                if let Some(&(r, g, b)) = PALETTE.get(i as usize) {
                    Color::from_rgb8(r, g, b)
                } else {
                    default
                }
            }
        },
        AnsiColor::Named(n) => match n {
            NamedColor::Green | NamedColor::DimGreen | NamedColor::BrightGreen => {
                user_host_color
            }
            NamedColor::Blue | NamedColor::DimBlue | NamedColor::BrightBlue => directory_color,
            _ => {
                let (r, g, b) = match n {
                    NamedColor::Black | NamedColor::DimBlack => (0, 0, 0),
                    NamedColor::Red | NamedColor::DimRed => (205, 0, 0),
                    NamedColor::Yellow | NamedColor::DimYellow => (205, 205, 0),
                    NamedColor::Magenta | NamedColor::DimMagenta => (205, 0, 205),
                    NamedColor::Cyan | NamedColor::DimCyan => (0, 205, 205),
                    NamedColor::White | NamedColor::DimWhite => (229, 229, 229),
                    NamedColor::BrightBlack => (127, 127, 127),
                    NamedColor::BrightRed => (255, 0, 0),
                    NamedColor::BrightYellow => (255, 255, 0),
                    NamedColor::BrightMagenta => (255, 0, 255),
                    NamedColor::BrightCyan => (0, 255, 255),
                    NamedColor::BrightWhite => (255, 255, 255),
                    _ => return default,
                };
                Color::from_rgb8(r, g, b)
            }
        },
    }
}

// Converts a canvas-local pixel position to a grid cell, accounting for the
// current scroll offset the same way rendering does (in reverse).
fn position_to_point(
    position: cosmic::iced::Point,
    bounds: Rectangle,
    display_offset: usize,
) -> GridPoint {
    let col = ((position.x - bounds.x) / CELL_WIDTH).max(0.0) as usize;
    let row = ((position.y - bounds.y) / CELL_HEIGHT) as i32 - display_offset as i32;
    GridPoint::new(Line(row), Column(col))
}

// Accumulates consecutive same-(fg,bg) cells on a row so they render as one
// glyph run instead of one shaping call per character. Positioned by grid
// column (start_x), not by cumulative glyph advance, so a run always starts
// exactly on-grid regardless of font metrics — only intra-run spacing
// depends on the monospace font's advance matching CELL_WIDTH, which this
// file already assumes everywhere (PTY sizing, mouse-to-cell mapping).
struct GlyphRun {
    start_x: f32,
    y: f32,
    fg: Color,
    bg: Color,
    text: String,
}

fn flush_run(frame: &mut canvas::Frame, run: GlyphRun, default_bg: Color) {
    let len = run.text.chars().count() as f32;
    if run.bg != default_bg {
        frame.fill_rectangle(
            cosmic::iced::Point::new(run.start_x, run.y),
            cosmic::iced::Size::new(CELL_WIDTH * len, CELL_HEIGHT),
            run.bg,
        );
    }
    if run.text.chars().any(|c| c != ' ') {
        frame.fill_text(canvas::Text {
            content: run.text,
            position: cosmic::iced::Point::new(run.start_x, run.y),
            color: run.fg,
            size: cosmic::iced::Pixels(CELL_HEIGHT * 0.85),
            font: cosmic::iced::Font::MONOSPACE,
            ..Default::default()
        });
    }
}

#[derive(Default)]
pub struct TermViewState {
    dragging: bool,
}

impl<'a> canvas::Program<crate::app::Message, Theme, Renderer> for TerminalView<'a> {
    type State = TermViewState;

    fn update(
        &self,
        state: &mut TermViewState,
        event: &cosmic::iced::Event,
        bounds: Rectangle,
        cursor: cosmic::iced::mouse::Cursor,
    ) -> Option<canvas::Action<crate::app::Message>> {
        use cosmic::iced::mouse::{Button, Event as MouseEvent};
        let cosmic::iced::Event::Mouse(mouse_event) = event else {
            return None;
        };

        match mouse_event {
            MouseEvent::ButtonPressed(Button::Left) => {
                let position = cursor.position_over(bounds)?;
                let display_offset = self.terminal.term.lock().renderable_content().display_offset;
                let point = position_to_point(position, bounds, display_offset);
                self.terminal.term.lock().selection =
                    Some(Selection::new(SelectionType::Simple, point, Side::Left));
                state.dragging = true;
                self.terminal.invalidate();
                Some(canvas::Action::publish(crate::app::Message::TerminalUpdated(
                    self.pane_id,
                )))
            }
            MouseEvent::CursorMoved { .. } if state.dragging => {
                let position = cursor.position()?;
                let display_offset = self.terminal.term.lock().renderable_content().display_offset;
                let point = position_to_point(position, bounds, display_offset);
                let mut term = self.terminal.term.lock();
                if let Some(selection) = term.selection.as_mut() {
                    selection.update(point, Side::Left);
                }
                drop(term);
                self.terminal.invalidate();
                Some(canvas::Action::publish(crate::app::Message::TerminalUpdated(
                    self.pane_id,
                )))
            }
            MouseEvent::ButtonReleased(Button::Left) => {
                state.dragging = false;
                None
            }
            _ => None,
        }
    }

    fn mouse_interaction(
        &self,
        _state: &TermViewState,
        bounds: Rectangle,
        cursor: cosmic::iced::mouse::Cursor,
    ) -> cosmic::iced::mouse::Interaction {
        if cursor.is_over(bounds) {
            cosmic::iced::mouse::Interaction::Text
        } else {
            cosmic::iced::mouse::Interaction::default()
        }
    }

    fn draw(
        &self,
        _state: &TermViewState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: cosmic::iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let cols = ((bounds.width / CELL_WIDTH) as u16).max(1);
        let rows = ((bounds.height / CELL_HEIGHT) as u16).max(1);
        self.terminal.resize_if_changed(cols, rows);

        let geometry = self.terminal.redraw_cache.draw(renderer, bounds.size(), |frame| {
            self.draw_frame(frame, bounds);
        });

        vec![geometry]
    }
}

impl<'a> TerminalView<'a> {
    /// The actual per-glyph render, pulled out of draw() so it only runs
    /// through redraw_cache.draw() above — i.e. only when invalidate() was
    /// called for THIS pane, not on every unrelated app message.
    fn draw_frame(&self, frame: &mut canvas::Frame, bounds: Rectangle) {
        frame.fill_rectangle(
            cosmic::iced::Point::ORIGIN,
            bounds.size(),
            Color::from_rgb8(30, 30, 30),
        );

        let term = self.terminal.term.lock();
        let selection_range = term.selection.as_ref().and_then(|s| s.to_range(&term));
        let content = term.renderable_content();
        let cursor_point = content.cursor.point;
        let display_offset = content.display_offset;
        let default_bg = Color::from_rgb8(30, 30, 30);

        let mut run: Option<GlyphRun> = None;
        let mut last_cell: Option<(i32, usize)> = None;

        for cell in content.display_iter {
            let line = cell.point.line.0;
            let col = cell.point.column.0;
            let x = col as f32 * CELL_WIDTH;
            let y = (line + display_offset as i32) as f32 * CELL_HEIGHT;
            let is_cursor = cell.point == cursor_point;
            let is_selected = selection_range
                .map(|r| r.contains(cell.point))
                .unwrap_or(false);

            let mut fg = resolve_color(cell.fg, Color::WHITE, self.user_host_color, self.directory_color);
            let mut bg = resolve_color(cell.bg, default_bg, self.user_host_color, self.directory_color);
            if cell.flags.contains(Flags::INVERSE) || is_cursor || is_selected {
                std::mem::swap(&mut fg, &mut bg);
            }

            let contiguous = last_cell.map_or(false, |(l, c)| l == line && c + 1 == col);
            let same_run = contiguous
                && run.as_ref().map_or(false, |r| r.fg == fg && r.bg == bg);

            if !same_run {
                if let Some(r) = run.take() {
                    flush_run(frame, r, default_bg);
                }
                run = Some(GlyphRun { start_x: x, y, fg, bg, text: String::new() });
            }
            run.as_mut().unwrap().text.push(cell.c);
            last_cell = Some((line, col));
        }
        if let Some(r) = run.take() {
            flush_run(frame, r, default_bg);
        }

        let total_lines = term.grid().total_lines();
        let screen_lines = term.grid().screen_lines();
        if total_lines > screen_lines {
            let track_height = bounds.height;
            let thumb_height =
                (screen_lines as f32 / total_lines as f32 * track_height).max(20.0);
            let max_offset = (total_lines - screen_lines) as f32;
            let scroll_fraction = if max_offset > 0.0 {
                display_offset as f32 / max_offset
            } else {
                0.0
            };
            let thumb_y = (1.0 - scroll_fraction) * (track_height - thumb_height);
            frame.fill_rectangle(
                cosmic::iced::Point::new(bounds.width - 6.0, thumb_y),
                cosmic::iced::Size::new(4.0, thumb_height),
                Color::from_rgba(1.0, 1.0, 1.0, 0.35),
            );
        }
    }
}

pub fn terminal_canvas(
    terminal: &Terminal,
    pane_id: crate::pane::PaneId,
    user_host_color: Color,
    directory_color: Color,
) -> Element<'_, crate::app::Message> {
    Canvas::new(TerminalView {
        terminal,
        pane_id,
        user_host_color,
        directory_color,
    })
    .width(cosmic::iced::Length::Fill)
    .height(cosmic::iced::Length::Fill)
    .into()
}
