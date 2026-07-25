mod app;
mod config;
mod i18n;
mod pane;
mod preferences;
mod term_view;
mod terminal;

fn main() -> cosmic::iced::Result {
    env_logger::init();
    cosmic::applet::run::<app::App>(())
}
