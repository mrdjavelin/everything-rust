use std::{mem::size_of, path::Path};

use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
            GetVolumeInformationW, GetVolumeNameForVolumeMountPointW, OPEN_EXISTING,
        },
        System::{
            IO::DeviceIoControl,
            Ioctl::{
                FSCTL_ENUM_USN_DATA, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL,
                MFT_ENUM_DATA_V0, READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
            },
        },
    },
    core::HSTRING,
};

use crate::{
    database::{
        VolumeSyncState, apply_usn_batch, begin_volume_rebuild, finish_volume_rebuild,
        load_volume_sync_state, stage_mft_records,
    },
    journal::JournalRecord,
};

const GENERIC_READ: u32 = 0x8000_0000;
const ENUM_BUFFER_SIZE: usize = 1024 * 1024;
const ENUM_HEADER_SIZE: usize = size_of::<u64>();
const USN_V2_HEADER_SIZE: usize = 60;
const ERROR_HANDLE_EOF_HRESULT: u32 = 0x8007_0026;

pub struct Volume {
    handle: HANDLE,
}

impl Drop for Volume {
    fn drop(&mut self) {
        // The handle is owned by this wrapper and closed exactly once.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct JournalInfo {
    pub journal_id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
}

pub struct VolumeIdentity {
    pub id: String,
    pub file_system: String,
}

impl Volume {
    pub fn open(root: &std::path::Path) -> windows::core::Result<Self> {
        let drive = root
            .to_str()
            .and_then(|path| path.as_bytes().get(0..2))
            .and_then(|drive| std::str::from_utf8(drive).ok())
            .filter(|drive| {
                drive.len() == 2
                    && drive.as_bytes()[1] == b':'
                    && drive.as_bytes()[0].is_ascii_alphabetic()
            })
            .ok_or_else(|| {
                windows::core::Error::from_hresult(windows::core::HRESULT(0x80070057u32 as i32))
            })?;
        let path = HSTRING::from(format!(r"\\.\{drive}"));

        // SAFETY: CreateFileW receives an owned, NUL-terminated HSTRING and returns
        // a handle that is closed by Volume::drop.
        let handle = unsafe {
            CreateFileW(
                &path,
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )?
        };

        Ok(Self { handle })
    }

    pub fn query_journal(&self) -> windows::core::Result<JournalInfo> {
        let mut data = USN_JOURNAL_DATA_V0::default();
        let mut bytes_returned = 0;

        // SAFETY: The input is empty, the output points to a correctly sized
        // initialized USN_JOURNAL_DATA_V0, and the call is synchronous.
        unsafe {
            DeviceIoControl(
                self.handle,
                FSCTL_QUERY_USN_JOURNAL,
                None,
                0,
                Some((&mut data as *mut USN_JOURNAL_DATA_V0).cast()),
                size_of::<USN_JOURNAL_DATA_V0>() as u32,
                Some(&mut bytes_returned),
                None,
            )?;
        }

        if (bytes_returned as usize) < size_of::<USN_JOURNAL_DATA_V0>() {
            return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                0x8007001fu32 as i32,
            )));
        }

        Ok(JournalInfo {
            journal_id: data.UsnJournalID,
            first_usn: data.FirstUsn,
            next_usn: data.NextUsn,
            lowest_valid_usn: data.LowestValidUsn,
        })
    }

    pub fn read_journal(
        &self,
        start_usn: i64,
        journal_id: u64,
    ) -> Result<(i64, Vec<JournalRecord>), String> {
        let query = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: start_usn,
            ReasonMask: u32::MAX,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 0,
            UsnJournalID: journal_id,
        };
        let mut buffer = vec![0u8; ENUM_BUFFER_SIZE];
        let mut bytes_returned = 0;
        // SAFETY: The input and output buffers remain valid for the duration of
        // the synchronous call, and the output buffer length is supplied.
        unsafe {
            DeviceIoControl(
                self.handle,
                FSCTL_READ_USN_JOURNAL,
                Some((&query as *const READ_USN_JOURNAL_DATA_V0).cast()),
                size_of::<READ_USN_JOURNAL_DATA_V0>() as u32,
                Some(buffer.as_mut_ptr().cast()),
                buffer.len() as u32,
                Some(&mut bytes_returned),
                None,
            )
            .map_err(|error| format!("FSCTL_READ_USN_JOURNAL failed: {error}"))?;
        }

        let used = bytes_returned as usize;
        if used < ENUM_HEADER_SIZE || used > buffer.len() {
            return Err("USN journal read returned an invalid buffer length".to_owned());
        }
        let next_usn = i64::from_ne_bytes(
            buffer[..ENUM_HEADER_SIZE]
                .try_into()
                .map_err(|_| "USN journal returned an invalid cursor".to_owned())?,
        );
        if next_usn < start_usn {
            return Err("USN journal returned a cursor before the requested position".to_owned());
        }
        let records = parse_record_buffer(&buffer[ENUM_HEADER_SIZE..used])?;
        Ok((next_usn, records))
    }

    pub fn enumerate_mft(
        &self,
        mut visit: impl FnMut(JournalRecord) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut query = MFT_ENUM_DATA_V0 {
            StartFileReferenceNumber: 0,
            LowUsn: 0,
            HighUsn: i64::MAX,
        };
        let mut buffer = vec![0u8; ENUM_BUFFER_SIZE];

        loop {
            let mut bytes_returned = 0;
            // SAFETY: The query and output buffer remain valid for the duration
            // of the synchronous call; the driver writes no more than the given size.
            let result = unsafe {
                DeviceIoControl(
                    self.handle,
                    FSCTL_ENUM_USN_DATA,
                    Some((&query as *const MFT_ENUM_DATA_V0).cast()),
                    size_of::<MFT_ENUM_DATA_V0>() as u32,
                    Some(buffer.as_mut_ptr().cast()),
                    buffer.len() as u32,
                    Some(&mut bytes_returned),
                    None,
                )
            };

            if let Err(error) = result {
                // ERROR_HANDLE_EOF marks the normal end of MFT enumeration.
                if error.code().0 as u32 == ERROR_HANDLE_EOF_HRESULT {
                    return Ok(());
                }
                return Err(format!("FSCTL_ENUM_USN_DATA failed: {error}"));
            }

            let used = bytes_returned as usize;
            if used < ENUM_HEADER_SIZE || used > buffer.len() {
                return Err("MFT enumeration returned a truncated header".to_owned());
            }

            let next_reference = u64::from_ne_bytes(
                buffer[..ENUM_HEADER_SIZE]
                    .try_into()
                    .map_err(|_| "MFT enumeration returned an invalid cursor".to_owned())?,
            );
            if used == ENUM_HEADER_SIZE {
                if next_reference <= query.StartFileReferenceNumber {
                    return Ok(());
                }
                query.StartFileReferenceNumber = next_reference;
                continue;
            }
            for record in parse_record_buffer(&buffer[ENUM_HEADER_SIZE..used])? {
                visit(record)?;
            }

            if next_reference <= query.StartFileReferenceNumber {
                return Err("MFT enumeration cursor did not advance".to_owned());
            }
            query.StartFileReferenceNumber = next_reference;
        }
    }
}

pub fn identify_volume(root: &Path) -> windows::core::Result<VolumeIdentity> {
    let root = root.to_string_lossy();
    let root = HSTRING::from(root.as_ref());
    let mut name = vec![0u16; 64];
    let mut serial = 0u32;
    let mut file_system = vec![0u16; 32];
    // SAFETY: The HSTRING and writable buffers live through the synchronous
    // call; all optional output pointers reference initialized local values.
    unsafe {
        GetVolumeInformationW(
            &root,
            Some(&mut name),
            Some(&mut serial),
            None,
            None,
            Some(&mut file_system),
        )?;
    }

    let mut volume_name = vec![0u16; 64];
    // SAFETY: Both strings are valid, NUL-terminated buffers; the API writes
    // only within the supplied volume-name buffer.
    unsafe {
        GetVolumeNameForVolumeMountPointW(&root, &mut volume_name)?;
    }
    let volume_name = String::from_utf16_lossy(
        &volume_name[..volume_name
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(volume_name.len())],
    );
    let file_system = String::from_utf16_lossy(
        &file_system[..file_system
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(file_system.len())],
    );

    Ok(VolumeIdentity {
        id: format!("{}-{serial:08X}", volume_name.to_ascii_uppercase()),
        file_system,
    })
}

pub fn sync_volume(connection: &mut rusqlite::Connection, root: &Path) -> Result<(), String> {
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

    loop {
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
            rebuild_volume(connection, &volume, &identity.id, &root_path, journal)?;
            rebuilt = true;
            continue;
        }

        let state = state.expect("validated volume state");
        let mut cursor = state.next_usn;
        loop {
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
                rebuild_volume(connection, &volume, &identity.id, &root_path, current)?;
                rebuilt = true;
                break;
            }
            if cursor == current.next_usn {
                return Ok(());
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
                    rebuild_volume(connection, &volume, &identity.id, &root_path, refreshed)?;
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
    connection: &mut rusqlite::Connection,
    volume: &Volume,
    volume_id: &str,
    root_path: &str,
    starting_journal: JournalInfo,
) -> Result<(), String> {
    begin_volume_rebuild(connection, volume_id)
        .map_err(|error| format!("Could not prepare MFT resynchronization: {error}"))?;

    let mut batch = Vec::with_capacity(512);
    let enumeration_result = volume.enumerate_mft(|record| {
        batch.push(record);
        if batch.len() >= 512 {
            stage_mft_records(connection, volume_id, &batch)
                .map_err(|error| format!("Could not stage MFT entries: {error}"))?;
            batch.clear();
        }
        Ok(())
    });
    if let Err(error) = enumeration_result {
        return match begin_volume_rebuild(connection, volume_id) {
            Ok(()) => Err(format!("MFT enumeration failed: {error}")),
            Err(cleanup_error) => Err(format!(
                "MFT enumeration failed: {error}; staging cleanup failed: {cleanup_error}"
            )),
        };
    }
    if !batch.is_empty() {
        stage_mft_records(connection, volume_id, &batch)
            .map_err(|error| format!("Could not stage final MFT batch: {error}"))?;
    }

    let latest = volume
        .query_journal()
        .map_err(|error| format!("Could not verify journal after MFT enumeration: {error}"))?;
    if latest.journal_id != starting_journal.journal_id
        || starting_journal.next_usn < latest.lowest_valid_usn
        || starting_journal.next_usn < latest.first_usn
    {
        return match begin_volume_rebuild(connection, volume_id) {
            Ok(()) => Err("The journal changed or truncated during MFT enumeration".into()),
            Err(error) => Err(format!(
                "The journal changed during MFT enumeration; staging cleanup failed: {error}"
            )),
        };
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

fn parse_record_buffer(bytes: &[u8]) -> Result<Vec<JournalRecord>, String> {
    let mut records = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes.len() - offset < 8 {
            return Err("USN data returned a truncated record header".to_owned());
        }
        let record_length = u32::from_ne_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .map_err(|_| "Invalid USN record length".to_owned())?,
        ) as usize;
        if record_length < USN_V2_HEADER_SIZE || record_length > bytes.len() - offset {
            return Err("USN data returned an invalid record length".to_owned());
        }
        records.push(parse_v2_record(&bytes[offset..offset + record_length])?);
        offset += record_length;
    }
    Ok(records)
}

fn parse_v2_record(bytes: &[u8]) -> Result<JournalRecord, String> {
    if bytes.len() < USN_V2_HEADER_SIZE {
        return Err("USN record is shorter than its fixed header".to_owned());
    }

    let read_u16 = |offset: usize| {
        u16::from_ne_bytes(
            bytes[offset..offset + 2]
                .try_into()
                .expect("checked record bounds"),
        )
    };
    let read_u32 = |offset: usize| {
        u32::from_ne_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("checked record bounds"),
        )
    };
    let read_u64 = |offset: usize| {
        u64::from_ne_bytes(
            bytes[offset..offset + 8]
                .try_into()
                .expect("checked record bounds"),
        )
    };
    let read_i64 = |offset: usize| {
        i64::from_ne_bytes(
            bytes[offset..offset + 8]
                .try_into()
                .expect("checked record bounds"),
        )
    };

    if read_u16(4) != 2 {
        return Err(format!("Unsupported USN record version {}", read_u16(4)));
    }

    let name_length = read_u16(56) as usize;
    let name_offset = read_u16(58) as usize;
    let name_end = name_offset
        .checked_add(name_length)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| "USN record has an invalid filename range".to_owned())?;
    if !name_length.is_multiple_of(2) || name_offset < USN_V2_HEADER_SIZE {
        return Err("USN record has an invalid UTF-16 filename".to_owned());
    }
    let (name_units, _) = bytes[name_offset..name_end].as_chunks::<2>();
    let name = String::from_utf16(
        &name_units
            .iter()
            .map(|pair| u16::from_ne_bytes(*pair))
            .collect::<Vec<_>>(),
    )
    .map_err(|_| "USN record filename is not valid UTF-16".to_owned())?;

    Ok(JournalRecord {
        file_reference_number: read_u64(8),
        parent_file_reference_number: read_u64(16),
        usn: read_i64(24),
        reason: read_u32(40),
        attributes: read_u32(52),
        name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::ATTRIBUTE_DIRECTORY;

    fn encoded_record(version: u16, name: &[u16]) -> Vec<u8> {
        let name_bytes = name
            .iter()
            .flat_map(|unit| unit.to_ne_bytes())
            .collect::<Vec<_>>();
        let mut bytes = vec![0; USN_V2_HEADER_SIZE + name_bytes.len()];
        let record_length = bytes.len() as u32;
        bytes[0..4].copy_from_slice(&record_length.to_ne_bytes());
        bytes[4..6].copy_from_slice(&version.to_ne_bytes());
        bytes[8..16].copy_from_slice(&17u64.to_ne_bytes());
        bytes[16..24].copy_from_slice(&9u64.to_ne_bytes());
        bytes[24..32].copy_from_slice(&123i64.to_ne_bytes());
        bytes[40..44].copy_from_slice(&0x100u32.to_ne_bytes());
        bytes[52..56].copy_from_slice(&ATTRIBUTE_DIRECTORY.to_ne_bytes());
        bytes[56..58].copy_from_slice(&(name_bytes.len() as u16).to_ne_bytes());
        bytes[58..60].copy_from_slice(&(USN_V2_HEADER_SIZE as u16).to_ne_bytes());
        bytes[USN_V2_HEADER_SIZE..].copy_from_slice(&name_bytes);
        bytes
    }

    #[test]
    fn parses_v2_record_fields_and_utf16_name() {
        let record = parse_v2_record(&encoded_record(
            2,
            &[b'f' as u16, b'i' as u16, b'l' as u16, b'e' as u16],
        ))
        .unwrap();
        assert_eq!(record.file_reference_number, 17);
        assert_eq!(record.parent_file_reference_number, 9);
        assert_eq!(record.usn, 123);
        assert_eq!(record.reason, 0x100);
        assert_eq!(record.attributes, ATTRIBUTE_DIRECTORY);
        assert_eq!(record.name, "file");
    }

    #[test]
    fn rejects_unsupported_versions_and_invalid_filename_ranges() {
        assert!(parse_v2_record(&encoded_record(3, &[b'x' as u16])).is_err());

        let mut invalid = encoded_record(2, &[b'x' as u16]);
        invalid[58..60].copy_from_slice(&u16::MAX.to_ne_bytes());
        assert!(parse_v2_record(&invalid).is_err());
    }

    #[test]
    fn rejects_truncated_record_buffers() {
        assert!(parse_record_buffer(&[1, 2, 3, 4, 5, 6, 7]).is_err());

        let mut invalid = vec![0; 12];
        invalid[0..4].copy_from_slice(&100u32.to_ne_bytes());
        assert!(parse_record_buffer(&invalid).is_err());
    }
}
