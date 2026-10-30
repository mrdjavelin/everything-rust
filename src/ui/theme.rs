use ratatui::style::{Color, Modifier, Style};

const BACKGROUND: Color = Color::Rgb(15, 18, 24);
const PANEL: Color = Color::Rgb(21, 25, 33);
const BORDER: Color = Color::Rgb(55, 63, 76);
const TEXT: Color = Color::Rgb(220, 226, 235);
const MUTED: Color = Color::Rgb(123, 134, 151);
const ACCENT: Color = Color::Rgb(104, 183, 255);
const DIRECTORY: Color = Color::Rgb(125, 196, 255);
const FILE: Color = Color::Rgb(168, 178, 194);
const SELECTED: Color = Color::Rgb(38, 51, 69);
const ERROR: Color = Color::Rgb(255, 126, 128);

pub fn base() -> Style {
    Style::default().bg(BACKGROUND).fg(TEXT)
}

pub fn panel() -> Style {
    Style::default().bg(PANEL)
}

pub fn accent() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn text() -> Style {
    Style::default().fg(TEXT)
}

pub fn muted() -> Style {
    Style::default().fg(MUTED)
}

pub fn directory() -> Style {
    Style::default().fg(DIRECTORY).add_modifier(Modifier::BOLD)
}

pub fn file() -> Style {
    Style::default().fg(FILE)
}

pub fn selected() -> Style {
    Style::default()
        .bg(SELECTED)
        .fg(TEXT)
        .add_modifier(Modifier::BOLD)
}

pub fn error() -> Style {
    Style::default().fg(ERROR)
}

pub fn border() -> Style {
    Style::default().fg(BORDER)
}
