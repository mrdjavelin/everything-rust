mod actions;
mod events;
mod state;

use std::{io, time::Duration};

use crossterm::event::{Event, KeyEventKind};

use crate::{
    database,
    indexing::{IndexingService, IndexingUpdate},
    roots::get_scan_roots,
    services::SearchService,
    ui::TerminalUi,
};

pub use state::App;

pub fn run() -> io::Result<()> {
    let database_path = database::get_database_path()?;
    drop(database::open_database(&database_path).map_err(io::Error::other)?);
    let mut terminal = TerminalUi::new()?;

    let mut search = SearchService::start(database_path.clone())?;
    let roots = get_scan_roots();
    let mut indexing = IndexingService::start(database_path, roots.clone())?;
    let mut app = state::App::new(roots);
    let ui_result = run_ui(&mut terminal, &mut app, &mut search, &indexing);
    let terminal_result = terminal.restore();
    drop(terminal);
    let search_result = search.shutdown();
    let indexing_result = indexing.shutdown();

    ui_result?;
    terminal_result?;
    search_result.map_err(io::Error::other)?;
    indexing_result.map_err(io::Error::other)
}

fn run_ui(
    terminal: &mut TerminalUi,
    app: &mut state::App,
    search: &mut SearchService,
    indexing: &IndexingService,
) -> io::Result<()> {
    while !app.should_quit {
        if let Some(response) = search.try_recv() {
            app.apply_search_response(response);
        }
        for update in indexing.try_recv() {
            match update {
                IndexingUpdate::Status(status) => app.set_indexing_status(status),
                IndexingUpdate::Warning(warning) => app.set_indexing_warning(warning),
                IndexingUpdate::Activity(active) => app.set_indexing_active(active),
                IndexingUpdate::DatabaseChanged if !app.query.trim().is_empty() => {
                    match search.request(app.query.clone()) {
                        Ok(request_id) => app.set_active_search(request_id),
                        Err(error) => app.set_error(error),
                    }
                }
                IndexingUpdate::DatabaseChanged => {}
            }
        }

        app.advance_indexing_animation();
        terminal.draw(app)?;
        if let Some(Event::Key(key)) = terminal.poll_event(Duration::from_millis(50))? {
            if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                let action = events::handle_key(app, key);
                actions::apply(app, action, search, indexing);
            }
        }
    }
    Ok(())
}
