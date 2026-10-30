mod scan;
mod schema;
mod search;
mod volume;

#[cfg(test)]
mod tests;

pub use scan::{
    begin_scanned_root, clear_fallback_scan_state, finish_scanned_root,
    has_completed_fallback_scan, save_scanned_batch,
};
pub use schema::{get_database_path, open_database};
pub use search::search_entries;
pub use volume::{
    VolumeSyncState, apply_usn_batch, begin_volume_rebuild, finish_volume_rebuild,
    invalidate_volume_sync_for_root, load_volume_rebuild_checkpoint, load_volume_sync_state,
    resume_volume_rebuild, stage_mft_page, start_volume_rebuild,
};

#[cfg(test)]
pub use volume::stage_mft_records;
