//! Error types for the FATX library.

use thiserror::Error;

/// All possible errors from fatxlib operations.
#[derive(Error, Debug)]
pub enum FatxError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid FATX magic: expected 'FATX', got {0:?}")]
    BadMagic([u8; 4]),

    #[error("Invalid sectors-per-cluster value: {0} (must be power of 2, 1..128)")]
    BadSectorsPerCluster(u32),

    #[error("Volume is too small to contain a valid FATX filesystem")]
    VolumeTooSmall,

    #[error("Cluster {0} is out of range (max {1})")]
    ClusterOutOfRange(u32, u32),

    #[error("File not found: {0}")]
    FileNotFound(String),

    #[error("Path is not a directory: {0}")]
    NotADirectory(String),

    #[error("Path is a directory: {0}")]
    IsADirectory(String),

    #[error("Directory is not empty: {0}")]
    DirectoryNotEmpty(String),

    #[error("Filename too long: {0} chars (max {1})")]
    FilenameTooLong(usize, usize),

    #[error("Invalid filename character: '{0}'")]
    InvalidFilenameChar(char),

    #[error("File or directory already exists: {0}")]
    FileExists(String),

    #[error("No free clusters available")]
    DiskFull,

    #[error("Directory is full (no free entry slots)")]
    DirectoryFull,

    #[error("Corrupt FAT chain at cluster {0}")]
    CorruptChain(u32),

    #[error("No FATX partition found at the expected offset")]
    NoPartitionFound,
}

pub type Result<T> = std::result::Result<T, FatxError>;
