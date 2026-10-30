use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
};

use crate::{app::App, core::EntryKind};

use super::super::theme;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let items = app
        .results
        .iter()
        .map(|entry| {
            let (kind, kind_style) = match entry.kind {
                EntryKind::File => ("FILE", theme::file()),
                EntryKind::Directory => ("DIR ", theme::directory()),
            };
            let detail = match entry.kind {
                EntryKind::File => entry.size.map(format_size),
                EntryKind::Directory => entry.item_count.map(|count| format!("{count} items")),
            };
            let mut first_line = vec![
                Span::styled(format!("{kind}  "), kind_style),
                Span::styled(entry.name.as_str(), theme::text()),
            ];
            if let Some(detail) = detail {
                first_line.push(Span::styled(format!("  {detail}"), theme::muted()));
            }

            ListItem::new(vec![
                Line::from(first_line),
                Line::from(Span::styled(
                    format!("      {}", entry.path.display()),
                    theme::muted(),
                )),
            ])
        })
        .collect::<Vec<_>>();

    let list = List::new(items)
        .style(theme::panel())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border())
                .title(format!(" Results  {} ", app.results.len())),
        )
        .highlight_style(theme::selected())
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn format_size(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
