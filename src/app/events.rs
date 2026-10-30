use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::state::App;

pub enum Action {
    None,
    Search(String),
    Open(PathBuf),
    Reindex,
    Quit,
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> Action {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return Action::Quit;
    }

    match key.code {
        KeyCode::Esc => {
            app.should_quit = true;
            Action::Quit
        }
        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => Action::Reindex,
        KeyCode::Char(character)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            app.query.push(character);
            app.query_changed()
        }
        KeyCode::Backspace => {
            if app.query.pop().is_some() {
                app.query_changed()
            } else {
                Action::None
            }
        }
        KeyCode::Down => {
            app.move_selection(1);
            Action::None
        }
        KeyCode::Up => {
            app.move_selection(-1);
            Action::None
        }
        KeyCode::Enter => app
            .selected_path()
            .map(Action::Open)
            .unwrap_or(Action::None),
        _ => Action::None,
    }
}
