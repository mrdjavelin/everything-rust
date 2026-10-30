use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::App;

use super::super::theme;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let status = app
        .search_error
        .as_deref()
        .or_else(|| {
            app.message_error
                .then_some(app.message.as_deref())
                .flatten()
        })
        .or(app.indexing_warning.as_deref())
        .or(app.message.as_deref())
        .unwrap_or(&app.indexing_status);
    let shortcuts = "Enter open  Up/Down move  Ctrl+R index  Esc quit";
    let result_count = format!("{} results", app.results.len());
    let reserved = result_count.chars().count() + shortcuts.chars().count() + 6;
    let available_status_width = usize::from(area.width).saturating_sub(reserved);
    let status = truncate(status, available_status_width);
    let status_style =
        if app.search_error.is_some() || app.message_error || app.indexing_warning.is_some() {
            theme::error()
        } else {
            theme::muted()
        };
    let line = Line::from(vec![
        Span::styled(result_count, theme::accent()),
        Span::raw("  "),
        Span::styled(status, status_style),
        Span::raw("    "),
        Span::styled(shortcuts, theme::muted()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn truncate(value: &str, width: usize) -> String {
    let char_count = value.chars().count();
    if char_count <= width {
        return value.to_owned();
    }
    if width <= 3 {
        return value.chars().take(width).collect();
    }
    let mut truncated = value.chars().take(width - 3).collect::<String>();
    truncated.push_str("...");
    truncated
}
