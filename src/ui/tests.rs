use ratatui::{Terminal, backend::TestBackend};

use crate::app::App;

use super::render;

#[test]
fn rendering_handles_a_tiny_terminal_area() {
    let app = App::new(Vec::new());
    let backend = TestBackend::new(1, 1);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| render::draw(frame, &app)).unwrap();
}

#[test]
fn rendering_shows_centered_indexing_indicator_when_active() {
    let mut app = App::new(Vec::new());
    app.indexing_active = true;
    app.indexing_status = "Scanning C:\\ (512 entries)".to_owned();

    let backend = TestBackend::new(60, 16);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| render::draw(frame, &app)).unwrap();

    let buffer = terminal.backend().buffer();
    let rendered = (0..buffer.area.height)
        .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
        .collect::<String>();
    assert!(rendered.contains("Indexing files"));
    assert!(rendered.contains("Scanning C:\\ (512 entries)"));
}
