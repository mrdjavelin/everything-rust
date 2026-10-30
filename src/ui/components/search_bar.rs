use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::ui::theme;

pub fn render(frame: &mut Frame, area: Rect, query: &str) {
    let line = Line::from(vec![
        Span::styled("> ", theme::accent()),
        Span::styled(query, theme::text()),
        Span::styled("|", theme::accent()),
    ]);
    let input = Paragraph::new(line).style(theme::panel()).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border())
            .title(" Search "),
    );
    frame.render_widget(input, area);
}
