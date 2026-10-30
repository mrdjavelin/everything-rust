use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::ui::theme;

pub fn render(frame: &mut Frame, area: Rect, query: &str, loading: bool, error: Option<&str>) {
    let (message, style) = if let Some(error) = error {
        (error, theme::error())
    } else if loading {
        ("Searching...", theme::muted())
    } else if query.trim().is_empty() {
        ("Type to search indexed files and folders", theme::muted())
    } else {
        ("No matching results", theme::muted())
    };

    let state = Paragraph::new(message)
        .style(style)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border())
                .title(" Results "),
        );
    frame.render_widget(state, area);
}
