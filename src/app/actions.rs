use crate::{
    app::{events::Action, state::App},
    indexing::IndexingService,
    platform,
    services::SearchService,
};

pub fn apply(
    app: &mut App,
    action: Action,
    search: &mut SearchService,
    indexing: &IndexingService,
) {
    match action {
        Action::None => {}
        Action::Search(query) => match search.request(query) {
            Ok(request_id) => app.set_active_search(request_id),
            Err(error) => app.set_error(error),
        },
        Action::Open(path) => {
            if let Err(error) = platform::open::open_path(&path) {
                app.set_error(format!("Could not open {}: {error}", path.display()));
            }
        }
        Action::Reindex => {
            if let Err(error) = indexing.request_reindex() {
                app.set_error(error);
            } else {
                app.set_message("Reindex queued".to_owned());
            }
        }
        Action::Quit => app.should_quit = true,
    }
}
