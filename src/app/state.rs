use std::path::PathBuf;

use crate::{core::FsEntry, services::SearchResponse};

pub struct App {
    pub query: String,
    pub results: Vec<FsEntry>,
    pub selected: usize,
    pub should_quit: bool,
    pub loading: bool,
    pub search_error: Option<String>,
    pub message: Option<String>,
    pub message_error: bool,
    pub indexing_status: String,
    pub indexing_warning: Option<String>,
    pub indexing_active: bool,
    pub indexing_frame: usize,
    active_search_id: u64,
}

impl App {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        let indexing_status = if roots.is_empty() {
            "No accessible roots".to_owned()
        } else {
            "Preparing background indexing".to_owned()
        };
        Self {
            query: String::new(),
            results: Vec::new(),
            selected: 0,
            should_quit: false,
            loading: false,
            search_error: None,
            message: None,
            message_error: false,
            indexing_status,
            indexing_warning: None,
            indexing_active: false,
            indexing_frame: 0,
            active_search_id: 0,
        }
    }

    pub fn query_changed(&mut self) -> super::events::Action {
        self.results.clear();
        self.selected = 0;
        self.search_error = None;
        self.message = None;
        self.message_error = false;
        self.loading = !self.query.trim().is_empty();
        super::events::Action::Search(self.query.clone())
    }

    pub fn set_active_search(&mut self, request_id: u64) {
        self.active_search_id = request_id;
    }

    pub fn apply_search_response(&mut self, response: SearchResponse) {
        if response.request_id != self.active_search_id {
            return;
        }

        self.loading = false;
        match response.result {
            Ok(results) => {
                self.results = results;
                self.selected = self.selected.min(self.results.len().saturating_sub(1));
                self.search_error = None;
            }
            Err(error) => {
                self.results.clear();
                self.selected = 0;
                self.search_error = Some(error);
            }
        }
    }

    pub fn selected_path(&self) -> Option<PathBuf> {
        self.results
            .get(self.selected)
            .map(|entry| entry.path.clone())
    }

    pub fn set_indexing_status(&mut self, status: String) {
        if status == "Initial indexing started" || status == "Reindexing started" {
            self.indexing_warning = None;
        }
        self.indexing_status = status;
        self.message = None;
        self.message_error = false;
    }

    pub fn set_indexing_warning(&mut self, warning: String) {
        self.indexing_warning = Some(warning);
    }

    pub fn set_indexing_active(&mut self, active: bool) {
        self.indexing_active = active;
        if active {
            self.indexing_warning = None;
        }
    }

    pub fn advance_indexing_animation(&mut self) {
        if self.indexing_active {
            self.indexing_frame = (self.indexing_frame + 1) % 4;
        }
    }

    pub fn set_message(&mut self, message: String) {
        self.search_error = None;
        self.message = Some(message);
        self.message_error = false;
    }

    pub fn set_error(&mut self, error: String) {
        self.message = Some(error);
        self.message_error = true;
        self.loading = false;
    }

    pub fn move_selection(&mut self, offset: isize) {
        if self.results.is_empty() {
            self.selected = 0;
            return;
        }

        let max_index = self.results.len() - 1;
        self.selected = self.selected.saturating_add_signed(offset).min(max_index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_outdated_results_and_clamps_selection_after_updates() {
        let mut app = App::new(Vec::new());
        app.query = "current".to_owned();
        app.loading = true;
        app.active_search_id = 2;
        app.apply_search_response(SearchResponse {
            request_id: 1,
            result: Ok(Vec::new()),
        });
        assert!(app.loading);

        app.selected = 8;
        app.apply_search_response(SearchResponse {
            request_id: 2,
            result: Ok(Vec::new()),
        });
        assert_eq!(app.selected, 0);
        assert!(!app.loading);
    }

    #[test]
    fn indexing_animation_advances_only_while_indexing() {
        let mut app = App::new(Vec::new());
        app.advance_indexing_animation();
        assert_eq!(app.indexing_frame, 0);

        app.set_indexing_active(true);
        app.advance_indexing_animation();
        assert_eq!(app.indexing_frame, 1);
        app.set_indexing_active(false);
        app.advance_indexing_animation();
        assert_eq!(app.indexing_frame, 1);
    }
}
