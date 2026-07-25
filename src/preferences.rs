use crate::app::Message;
use crate::config::AppConfig;
use cosmic::iced::widget::{mouse_area, Column, Row};
use cosmic::iced::{Background, Border, Color, Length};
use cosmic::widget;

pub fn parse_hex_color(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some((r, g, b))
}

fn hex_string(color: (u8, u8, u8)) -> String {
    format!("#{:02X}{:02X}{:02X}", color.0, color.1, color.2)
}

const PALETTE: &[(u8, u8, u8)] = &[
    (0, 0, 0), (64, 64, 64), (128, 128, 128), (192, 192, 192), (255, 255, 255), (30, 30, 30), (229, 229, 229),
    (205, 0, 0), (255, 0, 0), (255, 105, 97), (139, 0, 0), (255, 69, 0), (255, 127, 80),
    (205, 133, 0), (255, 165, 0), (255, 200, 87), (184, 134, 11), (210, 105, 30), (255, 218, 121),
    (205, 205, 0), (255, 255, 0), (255, 255, 153), (154, 205, 50), (173, 255, 47), (240, 230, 140),
    (0, 205, 0), (0, 255, 0), (144, 238, 144), (0, 100, 0), (34, 139, 34), (46, 204, 113),
    (0, 205, 205), (0, 255, 255), (153, 255, 255), (0, 139, 139), (32, 178, 170), (72, 209, 204),
    (0, 0, 238), (0, 102, 255), (135, 206, 250), (25, 25, 112), (65, 105, 225), (70, 130, 180),
    (85, 26, 139), (148, 0, 211), (218, 112, 214), (75, 0, 130), (138, 43, 226), (186, 85, 211),
    (205, 0, 205), (255, 0, 255), (255, 182, 193), (199, 21, 133), (219, 112, 147), (255, 20, 147),
];

/// A grid of clickable preset swatches — the simplest thing that's honestly
/// still "a color chart" without needing an HSV wheel/slider widget we don't
/// have confident access to. `on_pick` builds the right Message variant
/// (user@host vs directory) for whichever swatch was clicked.
fn color_chart<'a>(
    selected: (u8, u8, u8),
    on_pick: impl Fn((u8, u8, u8)) -> Message + Copy + 'static,
) -> cosmic::Element<'a, Message> {
    let mut column = Column::new().spacing(4);
    for chunk in PALETTE.chunks(7) {
        let mut row = Row::new().spacing(4);
        for &color in chunk {
            let is_selected = color == selected;
            let swatch = widget::container(widget::text(""))
                .width(Length::Fixed(22.0))
                .height(Length::Fixed(22.0))
                .style(move |_: &cosmic::Theme| widget::container::Style {
                    background: Some(Background::Color(Color::from_rgb8(
                        color.0, color.1, color.2,
                    ))),
                    border: Border {
                        color: if is_selected {
                            Color::WHITE
                        } else {
                            Color::from_rgba(0.0, 0.0, 0.0, 0.3)
                        },
                        width: if is_selected { 2.0 } else { 1.0 },
                        radius: 4.0.into(),
                    },
                    ..Default::default()
                });
            row = row.push(mouse_area(swatch).on_press(on_pick(color)));
        }
        column = column.push(row);
    }
    column.into()
}

fn kb_row(shortcut: &'static str, description: String) -> cosmic::Element<'static, Message> {
    Row::new()
        .spacing(8)
        .push(widget::text(shortcut).width(Length::Fixed(130.0)))
        .push(widget::text(description))
        .into()
}

fn keybindings_chart<'a>() -> cosmic::Element<'a, Message> {
    Column::new()
        .spacing(6)
        .push(widget::text(crate::fl!("keybindings")))
        .push(kb_row("Ctrl+Shift+O", crate::fl!("kb-split-h")))
        .push(kb_row("Ctrl+Shift+E", crate::fl!("kb-split-v")))
        .push(kb_row("Ctrl+Shift+Q", crate::fl!("kb-close-pane")))
        .push(kb_row("Ctrl+Shift+Z", crate::fl!("kb-next-pane")))
        .push(kb_row("Ctrl+Shift+X", crate::fl!("kb-prev-pane")))
        .push(kb_row("Ctrl+Shift+T", crate::fl!("kb-new-tab")))
        .push(kb_row("Ctrl+Shift+W", crate::fl!("kb-close-tab")))
        .push(kb_row("Ctrl+Tab", crate::fl!("kb-next-tab")))
        .push(kb_row("Ctrl+Shift+Tab", crate::fl!("kb-prev-tab")))
        .push(kb_row("Ctrl+Shift+P", crate::fl!("kb-toggle-pin")))
        .push(kb_row("Ctrl+Shift+C", crate::fl!("kb-copy")))
        .push(kb_row("Ctrl+Shift+V", crate::fl!("kb-paste")))
        .push(kb_row("F12", crate::fl!("kb-hide")))
        .into()
}

pub fn view(config: &AppConfig) -> cosmic::Element<'_, Message> {
    let uh = config.user_host_color;
    let dc = config.directory_color;

    let swatch = |color: (u8, u8, u8)| {
        widget::container(widget::text(""))
            .width(Length::Fixed(28.0))
            .height(Length::Fixed(28.0))
            .style(move |_: &cosmic::Theme| widget::container::Style {
                background: Some(Background::Color(Color::from_rgb8(
                    color.0, color.1, color.2,
                ))),
                border: Border {
                    color: Color::WHITE,
                    width: 1.0,
                    radius: 4.0.into(),
                },
                ..Default::default()
            })
    };

    let color_settings = Column::new()
        .spacing(8)
        .push(
            Row::new()
                .spacing(8)
                .push(widget::text(crate::fl!("user-host-color")))
                .push(
                    widget::text_input("#00CD00", hex_string(uh))
                        .on_input(Message::UserHostColorInput)
                        .width(Length::Fixed(120.0)),
                )
                .push(swatch(uh)),
        )
        .push(color_chart(uh, |c| Message::UserHostColorInput(hex_string(c))))
        .push(
            Row::new()
                .spacing(8)
                .push(widget::text(crate::fl!("directory-color")))
                .push(
                    widget::text_input("#0000EE", hex_string(dc))
                        .on_input(Message::DirectoryColorInput)
                        .width(Length::Fixed(120.0)),
                )
                .push(swatch(dc)),
        )
        .push(color_chart(dc, |c| Message::DirectoryColorInput(hex_string(c))));

    let content = Column::new()
        .spacing(8)
        .padding(16)
        .push(widget::text(crate::fl!("preferences")))
        .push(
            Row::new()
                .spacing(24)
                .push(color_settings)
                .push(keybindings_chart()),
        );

    widget::scrollable(content).height(Length::Fill).into()
}
