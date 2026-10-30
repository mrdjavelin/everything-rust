use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::app::App;

use super::{
    components::{empty_state, results_list, search_bar, status_bar},
    layout, theme,
};

pub fn draw(frame: &mut Frame, app: &App) {
    frame.render_widget(Block::default().style(theme::base()), frame.area());
    let areas = layout::app_areas(frame.area());

    search_bar::render(frame, areas.search, &app.query);
    if app.search_error.is_some() || app.results.is_empty() || app.loading {
        empty_state::render(
            frame,
            areas.results,
            &app.query,
            app.loading,
            app.search_error.as_deref().or_else(|| {
                app.message_error
                    .then_some(app.message.as_deref())
                    .flatten()
            }),
        );
    } else {
        results_list::render(frame, areas.results, app);
    }
    if app.indexing_active {
        render_indexing_overlay(frame, areas.results, app);
    }
    status_bar::render(frame, areas.footer, app);
}

fn render_indexing_overlay(frame: &mut Frame, area: Rect, app: &App) {
    if area.width < 18 || area.height < 5 {
        return;
    }

    let width = area.width.min(48);
    let height = 5;
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(width),
            Constraint::Min(0),
        ])
        .split(vertical[1]);
    let card = horizontal[1];

    const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];
    let title = Line::from(vec![
        Span::styled(SPINNER[app.indexing_frame], theme::accent()),
        Span::raw("  "),
        Span::styled("Indexing files", theme::text()),
    ]);
    let status_width = usize::from(card.width).saturating_sub(4);
    let status = truncate_status(&app.indexing_status, status_width);
    let content = Paragraph::new(vec![
        title,
        Line::from(Span::styled(status, theme::muted())),
    ])
    .style(Style::default().bg(theme::panel().bg.unwrap_or_default()))
    .alignment(ratatui::layout::Alignment::Center)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border())
            .style(theme::panel()),
    );

    frame.render_widget(Clear, card);
    frame.render_widget(content, card);
}

fn truncate_status(status: &str, width: usize) -> String {
    if status.chars().count() <= width {
        return status.to_owned();
    }
    status.chars().take(width).collect()
}
