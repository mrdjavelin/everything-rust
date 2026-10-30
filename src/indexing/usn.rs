use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::Connection;

use crate::{
    database::{
        VolumeSyncState, apply_usn_batch, begin_volume_rebuild, finish_volume_rebuild,
        load_volume_rebuild_checkpoint, load_volume_sync_state, resume_volume_rebuild,
        stage_mft_page, start_volume_rebuild,
    },
    platform::windows_usn::{JournalInfo, Volume, identify_volume},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncSummary {
    pub skipped_records: usize,
    pub changed: bool,
}

pub fn sync_volume(
    connection: &mut Connection,
    root: &Path,
    cancelled: &AtomicBool,
) -> Result<SyncSummary, String> {
    let identity = identify_volume(root)
        .map_err(|error| format!("Could not identify {}: {error}", root.display()))?;
    if !identity.file_system.eq_ignore_ascii_case("NTFS") {
        return Err(format!(
            "{} uses {}, not NTFS",
            root.display(),
            identity.file_system
        ));
    }

    let volume = Volume::open(root)
        .map_err(|error| format!("Could not open {} for USN access: {error}", root.display()))?;
    let root_path = root.to_string_lossy().into_owned();
    let mut rebuilt = false;
    let mut summary = SyncSummary::default();

    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err("Indexing cancelled".to_owned());
        }
        let journal = volume
            .query_journal()
            .map_err(|error| format!("Could not query the USN journal: {error}"))?;
        let state = load_volume_sync_state(connection, &identity.id)
            .map_err(|error| format!("Could not load volume state: {error}"))?;

        if !state
            .as_ref()
            .is_some_and(|state| is_valid_state(state, &root_path, journal))
        {
            if rebuilt {
                return Err("The USN journal changed repeatedly during resynchronization".into());
            }
            summary.skipped_records += rebuild_volume(
                connection,
                &volume,
                &identity.id,
                &root_path,
                journal,
                cancelled,
            )?;
            summary.changed = true;
            rebuilt = true;
            continue;
        }

        let Some(state) = state else {
            return Err("Volume synchronization state disappeared".to_owned());
        };
        let mut cursor = state.next_usn;
        loop {
            if cancelled.load(Ordering::Relaxed) {
                return Err("Indexing cancelled".to_owned());
            }
            let current = volume
                .query_journal()
                .map_err(|error| format!("Could not refresh USN journal metadata: {error}"))?;
            if state.journal_id != current.journal_id.to_string()
                || state.root_path != root_path
                || cursor < current.lowest_valid_usn
                || cursor < current.first_usn
                || cursor > current.next_usn
            {
                if rebuilt {
                    return Err(
                        "The USN journal was reset or truncated during synchronization".into(),
                    );
                }
                summary.skipped_records += rebuild_volume(
                    connection,
                    &volume,
                    &identity.id,
                    &root_path,
                    current,
                    cancelled,
                )?;
                summary.changed = true;
                rebuilt = true;
                break;
            }
            if cursor == current.next_usn {
                return Ok(summary);
            }

            let (next_usn, records) = match volume.read_journal(cursor, current.journal_id) {
                Ok(batch) => batch,
                Err(error) => {
                    let refreshed = volume.query_journal().map_err(|query_error| {
                        format!("{error}; metadata query failed: {query_error}")
                    })?;
                    if rebuilt
                        || state.journal_id == refreshed.journal_id.to_string()
                            && state.root_path == root_path
                            && cursor >= refreshed.lowest_valid_usn
                            && cursor >= refreshed.first_usn
                            && cursor <= refreshed.next_usn
                    {
                        return Err(error);
                    }
                    summary.skipped_records += rebuild_volume(
                        connection,
                        &volume,
                        &identity.id,
                        &root_path,
                        refreshed,
                        cancelled,
                    )?;
                    summary.changed = true;
                    rebuilt = true;
                    break;
                }
            };
            if next_usn < cursor {
                return Err("USN journal returned an invalid cursor".into());
            }
            if next_usn == cursor && records.is_empty() {
                return Err("USN journal made no progress before its current end".into());
            }
            apply_usn_batch(
                connection,
                &identity.id,
                &root_path,
                &current.journal_id.to_string(),
                cursor,
                next_usn,
                &records,
            )
            .map_err(|error| format!("Could not apply USN changes: {error}"))?;
            summary.changed |= !records.is_empty();
            cursor = next_usn;
        }
    }
}

fn is_valid_state(state: &VolumeSyncState, root_path: &str, journal: JournalInfo) -> bool {
    state.journal_id == journal.journal_id.to_string()
        && state.root_path == root_path
        && state.next_usn >= 0
        && state.next_usn >= journal.lowest_valid_usn
        && state.next_usn >= journal.first_usn
        && state.next_usn <= journal.next_usn
}

fn rebuild_volume(
    connection: &mut Connection,
    volume: &Volume,
    volume_id: &str,
    root_path: &str,
    starting_journal: JournalInfo,
    cancelled: &AtomicBool,
) -> Result<usize, String> {
    let journal_id = starting_journal.journal_id.to_string();
    let checkpoint = load_volume_rebuild_checkpoint(connection, volume_id)
        .map_err(|error| format!("Could not load MFT resume checkpoint: {error}"))?;
    let resume_cursor = checkpoint.as_ref().and_then(|checkpoint| {
        (checkpoint.root_path == root_path
            && checkpoint.journal_id == journal_id
            && checkpoint.next_usn >= starting_journal.lowest_valid_usn
            && checkpoint.next_usn >= starting_journal.first_usn
            && checkpoint.next_usn <= starting_journal.next_usn)
            .then(|| checkpoint.next_file_reference.parse::<u64>().ok())
            .flatten()
    });
    let start_reference = match resume_cursor {
        Some(cursor) => {
            resume_volume_rebuild(connection)
                .map_err(|error| format!("Could not prepare MFT resume: {error}"))?;
            cursor
        }
        None => {
            start_volume_rebuild(
                connection,
                volume_id,
                root_path,
                &journal_id,
                starting_journal.next_usn,
            )
            .map_err(|error| format!("Could not prepare MFT resynchronization: {error}"))?;
            0
        }
    };

    volume
        .enumerate_mft_from(start_reference, |records, next_reference| {
            if cancelled.load(Ordering::Relaxed) {
                return Err("Indexing cancelled".to_owned());
            }
            stage_mft_page(connection, volume_id, &records, next_reference)
                .map_err(|error| format!("Could not stage MFT entries: {error}"))
        })
        .map_err(|error| format!("MFT enumeration failed: {error}"))?;
    if cancelled.load(Ordering::Relaxed) {
        return Err("Indexing cancelled".to_owned());
    }

    let latest = volume
        .query_journal()
        .map_err(|error| format!("Could not verify journal after MFT enumeration: {error}"))?;
    if latest.journal_id != starting_journal.journal_id
        || starting_journal.next_usn < latest.lowest_valid_usn
        || starting_journal.next_usn < latest.first_usn
    {
        begin_volume_rebuild(connection, volume_id)
            .map_err(|error| format!("Could not discard stale MFT checkpoint: {error}"))?;
        return Err("The journal changed or truncated during MFT enumeration".into());
    }

    finish_volume_rebuild(
        connection,
        volume_id,
        root_path,
        &starting_journal.journal_id.to_string(),
        starting_journal.next_usn,
    )
    .map_err(|error| format!("Could not commit the MFT index: {error}"))
}
