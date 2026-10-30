use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub struct Areas {
    pub search: Rect,
    pub results: Rect,
    pub footer: Rect,
}

pub fn app_areas(area: Rect) -> Areas {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    Areas {
        search: chunks[0],
        results: chunks[1],
        footer: chunks[2],
    }
}
