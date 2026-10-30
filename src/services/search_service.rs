use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{core::FsEntry, database};

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(140);

enum Command {
    Search { request_id: u64, query: String },
    Shutdown,
}

pub struct SearchResponse {
    pub request_id: u64,
    pub result: Result<Vec<FsEntry>, String>,
}

pub struct SearchService {
    commands: Sender<Command>,
    responses: Receiver<SearchResponse>,
    next_request_id: u64,
    worker: Option<JoinHandle<()>>,
}

impl SearchService {
    pub fn start(database_path: PathBuf) -> std::io::Result<Self> {
        let (command_tx, command_rx) = mpsc::channel();
        let (response_tx, response_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("everything-rust-search".to_owned())
            .spawn(move || {
                let connection = database::open_database(database_path);
                while let Ok(command) = command_rx.recv() {
                    let mut request = match command {
                        Command::Search { request_id, query } => (request_id, query),
                        Command::Shutdown => break,
                    };

                    loop {
                        match command_rx.recv_timeout(SEARCH_DEBOUNCE) {
                            Ok(Command::Search { request_id, query }) => {
                                request = (request_id, query);
                            }
                            Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                                return;
                            }
                            Err(RecvTimeoutError::Timeout) => break,
                        }
                    }

                    let (request_id, query) = request;
                    let result = if query.trim().is_empty() {
                        Ok(Vec::new())
                    } else {
                        match &connection {
                            Ok(connection) => database::search_entries(connection, &query)
                                .map_err(|error| error.to_string()),
                            Err(error) => {
                                Err(format!("Could not open the search database: {error}"))
                            }
                        }
                    };
                    if response_tx
                        .send(SearchResponse { request_id, result })
                        .is_err()
                    {
                        return;
                    }
                }
            })?;

        Ok(Self {
            commands: command_tx,
            responses: response_rx,
            next_request_id: 0,
            worker: Some(worker),
        })
    }

    pub fn request(&mut self, query: String) -> Result<u64, String> {
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| "Search request counter exhausted".to_owned())?;
        let request_id = self.next_request_id;
        self.commands
            .send(Command::Search { request_id, query })
            .map_err(|error| format!("Could not submit search: {error}"))?;
        Ok(request_id)
    }

    pub fn try_recv(&self) -> Option<SearchResponse> {
        let mut latest = None;
        while let Ok(response) = self.responses.try_recv() {
            latest = Some(response);
        }
        latest
    }

    pub fn shutdown(&mut self) -> Result<(), String> {
        if self.worker.is_none() {
            return Ok(());
        }
        let _ = self.commands.send(Command::Shutdown);
        if self
            .worker
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            return Err("The search worker panicked".to_owned());
        }
        Ok(())
    }
}

impl Drop for SearchService {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_returns_without_a_database_query() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let database_path =
            std::env::temp_dir().join(format!("everything-rust-search-dir-{suffix}"));
        std::fs::create_dir(&database_path).unwrap();
        let mut service = SearchService::start(database_path.clone()).unwrap();
        let request_id = service.request("  ".to_owned()).unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let response = loop {
            if let Some(response) = service.try_recv() {
                break response;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "search worker timed out"
            );
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(response.request_id, request_id);
        assert!(matches!(response.result, Ok(results) if results.is_empty()));
        service.shutdown().unwrap();
        std::fs::remove_dir_all(database_path).unwrap();
    }
}
