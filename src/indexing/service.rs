use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use rusqlite::Connection;

use crate::{database, roots::get_scan_roots};

use super::ScanSummary;

enum Command {
    Reindex,
    Shutdown,
}

pub enum IndexingUpdate {
    Status(String),
    Warning(String),
    Activity(bool),
    DatabaseChanged,
}

const JOURNAL_POLL_INTERVAL: Duration = Duration::from_secs(2);

struct ActivityGuard<'a>(&'a Sender<IndexingUpdate>);

impl<'a> ActivityGuard<'a> {
    fn start(updates: &'a Sender<IndexingUpdate>) -> Self {
        let _ = updates.send(IndexingUpdate::Activity(true));
        Self(updates)
    }
}

impl Drop for ActivityGuard<'_> {
    fn drop(&mut self) {
        let _ = self.0.send(IndexingUpdate::Activity(false));
    }
}

pub struct IndexingService {
    commands: Sender<Command>,
    updates: Receiver<IndexingUpdate>,
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl IndexingService {
    pub fn start(database_path: PathBuf, initial_roots: Vec<PathBuf>) -> std::io::Result<Self> {
        let (command_tx, command_rx) = mpsc::channel();
        let (update_tx, update_rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let worker = thread::Builder::new()
            .name("everything-rust-indexer".to_owned())
            .spawn(move || {
                let mut connection = match database::open_database(&database_path) {
                    Ok(connection) => connection,
                    Err(error) => {
                        warning(&update_tx, format!("Indexing unavailable: {error}"));
                        return;
                    }
                };

                status(&update_tx, "Initial indexing started");
                let mut journal_roots = run_indexing(
                    &mut connection,
                    &update_tx,
                    &initial_roots,
                    &worker_cancelled,
                    false,
                );

                loop {
                    match command_rx.recv_timeout(JOURNAL_POLL_INTERVAL) {
                        Ok(Command::Reindex) => {
                            status(&update_tx, "Reindexing started");
                            journal_roots = run_indexing(
                                &mut connection,
                                &update_tx,
                                &get_scan_roots(),
                                &worker_cancelled,
                                true,
                            );
                        }
                        Ok(Command::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            #[cfg(windows)]
                            sync_journal_roots(
                                &mut connection,
                                &update_tx,
                                &mut journal_roots,
                                &worker_cancelled,
                            );
                        }
                    }
                }
                status(&update_tx, "Indexing stopped");
            })?;

        Ok(Self {
            commands: command_tx,
            updates: update_rx,
            cancelled,
            worker: Some(worker),
        })
    }

    pub fn request_reindex(&self) -> Result<(), String> {
        self.commands
            .send(Command::Reindex)
            .map_err(|error| format!("Could not request reindexing: {error}"))
    }

    pub fn try_recv(&self) -> Vec<IndexingUpdate> {
        self.updates.try_iter().collect()
    }

    pub fn shutdown(&mut self) -> Result<(), String> {
        if self.worker.is_none() {
            return Ok(());
        }
        self.cancelled.store(true, Ordering::Relaxed);
        let _ = self.commands.send(Command::Shutdown);
        if self
            .worker
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            return Err("The indexing worker panicked".to_owned());
        }
        Ok(())
    }
}

impl Drop for IndexingService {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        let _ = self.commands.send(Command::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_indexing(
    connection: &mut Connection,
    updates: &Sender<IndexingUpdate>,
    roots: &[PathBuf],
    cancelled: &AtomicBool,
    force_fallback_scan: bool,
) -> Vec<PathBuf> {
    let _activity = ActivityGuard::start(updates);
    if roots.is_empty() {
        warning(updates, "No accessible file-system roots found");
        return Vec::new();
    }

    let mut all_complete = true;
    let mut journal_roots = Vec::new();
    for (index, root) in roots.iter().enumerate() {
        if cancelled.load(Ordering::Relaxed) {
            status(updates, "Indexing stopped");
            return journal_roots;
        }
        status(
            updates,
            format!(
                "Indexing root {}/{}: {}",
                index + 1,
                roots.len(),
                root.display()
            ),
        );

        #[cfg(windows)]
        {
            match super::usn::sync_volume(connection, root, cancelled) {
                Ok(summary) => {
                    status(
                        updates,
                        format!(
                            "Synchronized {} from the USN journal ({} unresolvable MFT records skipped)",
                            root.display(),
                            summary.skipped_records
                        ),
                    );
                    if summary.skipped_records > 0 {
                        warning(
                            updates,
                            format!(
                                "{}: {} MFT records could not be resolved to a path",
                                root.display(),
                                summary.skipped_records
                            ),
                        );
                    }
                    if summary.changed {
                        let _ = updates.send(IndexingUpdate::DatabaseChanged);
                    }
                    journal_roots.push(root.clone());
                    continue;
                }
                Err(error) => {
                    if cancelled.load(Ordering::Relaxed) {
                        status(updates, "Indexing stopped");
                        return journal_roots;
                    }
                    warning(
                        updates,
                        format!(
                            "USN synchronization failed for {}: {error}; falling back to scan",
                            root.display()
                        ),
                    );
                    if let Err(error) = database::invalidate_volume_sync_for_root(connection, root)
                    {
                        warning(
                            updates,
                            format!(
                                "Could not invalidate journal state for {}: {error}",
                                root.display()
                            ),
                        );
                        all_complete = false;
                        continue;
                    }
                    if !force_fallback_scan {
                        match database::has_completed_fallback_scan(connection, root) {
                            Ok(true) => {
                                warning(
                                    updates,
                                    format!(
                                        "Using saved index for {} because USN is unavailable; press Ctrl+R to rescan, and live changes are unavailable",
                                        root.display()
                                    ),
                                );
                                continue;
                            }
                            Ok(false) => {}
                            Err(error) => {
                                warning(
                                    updates,
                                    format!(
                                        "Could not check saved scan state for {}: {error}",
                                        root.display()
                                    ),
                                );
                                all_complete = false;
                                continue;
                            }
                        }
                    }
                }
            }
        }

        match scan_fallback_root(connection, root, updates, cancelled) {
            Ok(summary) => {
                if !summary.complete {
                    all_complete = false;
                }
            }
            Err(error) => {
                if cancelled.load(Ordering::Relaxed) {
                    status(updates, "Indexing stopped");
                    return journal_roots;
                }
                warning(
                    updates,
                    format!("Fallback scan failed for {}: {error}", root.display()),
                );
                all_complete = false;
            }
        }
    }

    status(
        updates,
        if cancelled.load(Ordering::Relaxed) {
            "Indexing stopped".to_owned()
        } else if all_complete {
            "Indexing complete".to_owned()
        } else {
            "Indexing complete with skipped or failed roots".to_owned()
        },
    );
    journal_roots
}

#[cfg(windows)]
fn sync_journal_roots(
    connection: &mut Connection,
    updates: &Sender<IndexingUpdate>,
    roots: &mut Vec<PathBuf>,
    cancelled: &AtomicBool,
) {
    let mut index = 0;
    while index < roots.len() {
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        match super::usn::sync_volume(connection, &roots[index], cancelled) {
            Ok(summary) => {
                if summary.changed {
                    let _ = updates.send(IndexingUpdate::DatabaseChanged);
                }
                index += 1;
            }
            Err(error) => {
                warning(
                    updates,
                    format!(
                        "Could not continue live USN updates for {}: {error}; press Ctrl+R to retry",
                        roots[index].display()
                    ),
                );
                roots.remove(index);
            }
        }
    }
}

fn scan_fallback_root(
    connection: &mut Connection,
    root: &Path,
    updates: &Sender<IndexingUpdate>,
    cancelled: &AtomicBool,
) -> Result<ScanSummary, String> {
    database::clear_fallback_scan_state(connection, root)
        .map_err(|error| format!("Could not clear previous fallback scan state: {error}"))?;
    let scan_id = database::begin_scanned_root(connection)
        .map_err(|error| format!("Could not start fallback scan: {error}"))?;
    let mut indexed = 0usize;
    let summary = super::scanner::scan_root(
        root,
        || cancelled.load(Ordering::Relaxed),
        |batch| {
            database::save_scanned_batch(connection, scan_id, batch)
                .map_err(|error| error.to_string())?;
            indexed += batch.len();
            status(
                updates,
                format!("Scanning {} ({} entries)", root.display(), indexed),
            );
            Ok::<(), String>(())
        },
    )
    .map_err(|error| match error {
        super::scanner::ScanError::Cancelled => "Indexing cancelled".to_owned(),
        super::scanner::ScanError::Save(error) => error,
    })?;

    database::finish_scanned_root(connection, root, scan_id, summary.complete)
        .map_err(|error| format!("Could not finalize scan: {error}"))?;
    if summary.discovered > 0 {
        let _ = updates.send(IndexingUpdate::DatabaseChanged);
    }
    status(
        updates,
        format!(
            "Scanned {} entries from {} ({} skipped){}",
            summary.discovered,
            root.display(),
            summary.skipped,
            if summary.complete {
                ""
            } else {
                "; partial results retained"
            }
        ),
    );
    if !summary.complete {
        warning(
            updates,
            format!(
                "Scan of {} was partial; {} entries were skipped",
                root.display(),
                summary.skipped
            ),
        );
    }
    Ok(summary)
}

fn status(updates: &Sender<IndexingUpdate>, message: impl Into<String>) {
    let _ = updates.send(IndexingUpdate::Status(message.into()));
}

fn warning(updates: &Sender<IndexingUpdate>, message: impl Into<String>) {
    let _ = updates.send(IndexingUpdate::Warning(message.into()));
}
