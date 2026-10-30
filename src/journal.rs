#[derive(Clone, Debug)]
pub struct JournalRecord {
    pub file_reference_number: u64,
    pub parent_file_reference_number: u64,
    pub usn: i64,
    pub reason: u32,
    pub attributes: u32,
    pub name: String,
}

pub const REASON_FILE_DELETE: u32 = 0x0000_0200;
pub const REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
pub const REASON_RENAME_NEW_NAME: u32 = 0x0000_2000;
pub const ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
pub const REASON_DATA_OVERWRITE: u32 = 0x0000_0001;
pub const REASON_DATA_EXTEND: u32 = 0x0000_0002;
pub const REASON_DATA_TRUNCATION: u32 = 0x0000_0004;
pub const REASON_BASIC_INFO_CHANGE: u32 = 0x0000_8000;
pub const REASON_CLOSE: u32 = 0x8000_0000;
