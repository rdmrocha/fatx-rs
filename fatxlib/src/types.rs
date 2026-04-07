//! On-disk FATX/XTAF format types and constants.
//!
//! Both the original Xbox ("FATX") and the Xbox 360 ("XTAF") use variants of
//! the same filesystem. The on-disk layout is identical — the only differences
//! are the magic bytes, partition offsets, and timestamp epoch.
//!
//! Key properties shared by both:
//!   - 4 KB superblock with 4-byte magic ("FATX" or "XTAF")
//!   - Single FAT copy (FAT16 or FAT32 depending on cluster count)
//!   - 64-byte directory entries with 42-character filename limit
//!   - Timestamps use packed FAT date/time encoding

use bitflags::bitflags;
use std::fmt;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Magic bytes for original Xbox volumes: "FATX"
pub const FATX_MAGIC: [u8; 4] = [b'F', b'A', b'T', b'X'];

/// Magic bytes for Xbox 360 volumes: "XTAF"
pub const XTAF_MAGIC: [u8; 4] = [b'X', b'T', b'A', b'F'];

/// Returns true if the given 4 bytes are a valid FATX or XTAF magic signature.
pub fn is_valid_magic(magic: &[u8; 4]) -> bool {
    *magic == FATX_MAGIC || *magic == XTAF_MAGIC
}

/// Size of the FATX/XTAF superblock (header) in bytes.
pub const SUPERBLOCK_SIZE: u64 = 0x1000; // 4 KiB

/// Fixed sector size used by FATX/XTAF.
pub const SECTOR_SIZE: u64 = 512;

/// Maximum filename length in a directory entry.
pub const MAX_FILENAME_LEN: usize = 42;

/// Size of a single directory entry in bytes.
pub const DIRENT_SIZE: usize = 64;

/// Marker indicating a deleted directory entry.
pub const DIRENT_DELETED: u8 = 0xE5;

/// Marker indicating end-of-directory (no more entries).
pub const DIRENT_END: u8 = 0xFF;

/// Threshold: partitions with fewer than this many clusters use 16-bit FAT entries.
pub const FAT16_CLUSTER_THRESHOLD: u32 = 65_520;

/// FAT16 end-of-chain marker (>= this value means end of chain).
pub const FAT16_EOC: u16 = 0xFFF8;

/// FAT16 free cluster marker.
pub const FAT16_FREE: u16 = 0x0000;

/// FAT16 reserved/bad cluster marker.
pub const FAT16_BAD: u16 = 0xFFF7;

/// FAT32 end-of-chain marker (>= this value means end of chain).
pub const FAT32_EOC: u32 = 0x0FFFFFF8;

/// FAT32 free cluster marker.
pub const FAT32_FREE: u32 = 0x00000000;

/// FAT32 bad cluster marker.
pub const FAT32_BAD: u32 = 0x0FFFFFF7;

/// First valid cluster index (clusters 0 and 1 are reserved).
pub const FIRST_CLUSTER: u32 = 1;

// ---------------------------------------------------------------------------
// Console generation
// ---------------------------------------------------------------------------

/// Which Xbox generation this volume/partition belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XboxGeneration {
    /// Original Xbox (2001) — uses "FATX" magic, year-2000 epoch
    Original,
    /// Xbox 360 (2005) — uses "XTAF" magic, year-1980 epoch
    Xbox360,
}

impl fmt::Display for XboxGeneration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            XboxGeneration::Original => write!(f, "Xbox (Original)"),
            XboxGeneration::Xbox360 => write!(f, "Xbox 360"),
        }
    }
}

// ---------------------------------------------------------------------------
// Superblock (on-disk header)
// ---------------------------------------------------------------------------

/// Parsed FATX/XTAF superblock — the first 4096 bytes of a volume.
#[derive(Debug, Clone)]
pub struct Superblock {
    /// Magic bytes: "FATX" or "XTAF".
    pub magic: [u8; 4],
    /// Volume identifier (arbitrary 32-bit value).
    pub volume_id: u32,
    /// Number of sectors per cluster.
    pub sectors_per_cluster: u32,
    /// Number of FAT copies (typically 1).
    pub fat_copies: u16,
}

impl Superblock {
    /// Returns the cluster size in bytes.
    pub fn cluster_size(&self) -> u64 {
        self.sectors_per_cluster as u64 * SECTOR_SIZE
    }

    /// Returns true if the magic bytes are valid (FATX or XTAF).
    pub fn is_valid(&self) -> bool {
        is_valid_magic(&self.magic)
    }

    /// Returns which Xbox generation this volume belongs to based on magic.
    pub fn generation(&self) -> XboxGeneration {
        if self.magic == XTAF_MAGIC {
            XboxGeneration::Xbox360
        } else {
            XboxGeneration::Original
        }
    }

    /// Returns the magic bytes as a readable string.
    pub fn magic_str(&self) -> &str {
        if self.magic == XTAF_MAGIC {
            "XTAF"
        } else if self.magic == FATX_MAGIC {
            "FATX"
        } else {
            "????"
        }
    }
}

// ---------------------------------------------------------------------------
// FAT entry types
// ---------------------------------------------------------------------------

/// Describes whether this volume uses 16-bit or 32-bit FAT entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatType {
    Fat16,
    Fat32,
}

impl FatType {
    /// Size of a single FAT entry in bytes.
    pub fn entry_size(self) -> u64 {
        match self {
            FatType::Fat16 => 2,
            FatType::Fat32 => 4,
        }
    }
}

impl fmt::Display for FatType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FatType::Fat16 => write!(f, "FAT16"),
            FatType::Fat32 => write!(f, "FAT32"),
        }
    }
}

/// A resolved FAT entry value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatEntry {
    /// Cluster is free / unallocated.
    Free,
    /// Next cluster in the chain.
    Next(u32),
    /// End of cluster chain (last cluster of a file/directory).
    EndOfChain,
    /// Bad / reserved cluster.
    Bad,
}

// ---------------------------------------------------------------------------
// Directory entry
// ---------------------------------------------------------------------------

bitflags! {
    /// File attribute flags stored in byte 0x01 of a directory entry.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct FileAttributes: u8 {
        const READ_ONLY  = 0x01;
        const HIDDEN     = 0x02;
        const SYSTEM     = 0x04;
        const VOLUME_ID  = 0x08;
        const DIRECTORY  = 0x10;
        const ARCHIVE    = 0x20;
    }
}

/// A parsed FATX/XTAF directory entry.
#[derive(Debug, Clone)]
pub struct DirectoryEntry {
    /// Length of the filename (0xE5 = deleted, 0xFF/0x00 = end of directory).
    pub filename_len: u8,
    /// File attribute flags.
    pub attributes: FileAttributes,
    /// Raw filename bytes (up to 42 bytes, may contain padding 0xFF).
    pub filename_raw: [u8; MAX_FILENAME_LEN],
    /// First cluster of the file data (or directory contents).
    pub first_cluster: u32,
    /// File size in bytes (0 for directories).
    pub file_size: u32,
    /// Creation timestamp (packed FAT format).
    pub creation_time: u16,
    pub creation_date: u16,
    /// Last-write timestamp.
    pub write_time: u16,
    pub write_date: u16,
    /// Last-access timestamp.
    pub access_time: u16,
    pub access_date: u16,
}

impl DirectoryEntry {
    /// Returns the filename as a UTF-8 string.
    pub fn filename(&self) -> String {
        let len = (self.filename_len as usize).min(MAX_FILENAME_LEN);
        let bytes = &self.filename_raw[..len];
        // Filter out padding bytes
        let clean: Vec<u8> = bytes.iter().copied().filter(|&b| b != 0xFF && b != 0x00).collect();
        String::from_utf8_lossy(&clean).to_string()
    }

    /// Returns true if this entry represents a directory.
    pub fn is_directory(&self) -> bool {
        self.attributes.contains(FileAttributes::DIRECTORY)
    }

    /// Returns true if this entry has been deleted.
    pub fn is_deleted(&self) -> bool {
        self.filename_len == DIRENT_DELETED
    }

    /// Returns true if this marks the end of the directory listing.
    pub fn is_end(&self) -> bool {
        self.filename_len == DIRENT_END || self.filename_len == 0x00
    }

    /// Decode a packed FAT date into (year, month, day).
    /// Xbox 360 (XTAF) uses the standard FAT epoch of 1980.
    /// Original Xbox (FATX) also uses 1980 in the packed format.
    pub fn decode_date(date: u16) -> (u16, u8, u8) {
        let day = (date & 0x1F) as u8;
        let month = ((date >> 5) & 0x0F) as u8;
        let year = ((date >> 9) & 0x7F) + 1980;
        (year, month, day)
    }

    /// Decode a packed FAT time into (hour, minute, second).
    pub fn decode_time(time: u16) -> (u8, u8, u8) {
        let second = ((time & 0x1F) * 2) as u8;
        let minute = ((time >> 5) & 0x3F) as u8;
        let hour = ((time >> 11) & 0x1F) as u8;
        (hour, minute, second)
    }

    /// Format the creation datetime as a human-readable string.
    pub fn creation_datetime_str(&self) -> String {
        let (y, m, d) = Self::decode_date(self.creation_date);
        let (h, min, s) = Self::decode_time(self.creation_time);
        format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, m, d, h, min, s)
    }

    /// Format the last-write datetime as a human-readable string.
    pub fn write_datetime_str(&self) -> String {
        let (y, m, d) = Self::decode_date(self.write_date);
        let (h, min, s) = Self::decode_time(self.write_time);
        format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, m, d, h, min, s)
    }

    /// Format the last-access datetime as a human-readable string.
    pub fn access_datetime_str(&self) -> String {
        let (y, m, d) = Self::decode_date(self.access_date);
        let (h, min, s) = Self::decode_time(self.access_time);
        format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, m, d, h, min, s)
    }

    /// Encode a (year, month, day) into packed FAT date format.
    pub fn encode_date(year: u16, month: u8, day: u8) -> u16 {
        let y = (year.saturating_sub(1980) & 0x7F) as u16;
        (y << 9) | ((month as u16 & 0x0F) << 5) | (day as u16 & 0x1F)
    }

    /// Encode (hour, minute, second) into packed FAT time format.
    pub fn encode_time(hour: u8, minute: u8, second: u8) -> u16 {
        ((hour as u16 & 0x1F) << 11) | ((minute as u16 & 0x3F) << 5) | ((second as u16 / 2) & 0x1F)
    }
}

// ---------------------------------------------------------------------------
// Xbox partition tables
// ---------------------------------------------------------------------------

/// A partition entry in a known Xbox drive layout.
#[derive(Debug, Clone)]
pub struct XboxPartition {
    pub name: &'static str,
    /// Byte offset from the start of the disk.
    pub offset: u64,
    /// Size in bytes (0 means "rest of disk").
    pub size: u64,
    /// Which console generation this partition belongs to.
    pub generation: XboxGeneration,
}

// ---------------------------------------------------------------------------
// Original Xbox partitions (8 GB / 10 GB drives)
// ---------------------------------------------------------------------------

pub const OG_XBOX_PARTITIONS: &[XboxPartition] = &[
    XboxPartition { name: "Config (CACHE0)",        offset: 0x0008_0000, size: 0x02EE_0000, generation: XboxGeneration::Original },
    XboxPartition { name: "Game Cache (CACHE1)",     offset: 0x02F6_0000, size: 0x02EE_0000, generation: XboxGeneration::Original },
    XboxPartition { name: "Cache (CACHE2)",          offset: 0x05E4_0000, size: 0x02EE_0000, generation: XboxGeneration::Original },
    XboxPartition { name: "System (C)",              offset: 0x08CA_0000, size: 0x01F4_0000, generation: XboxGeneration::Original },
    XboxPartition { name: "Data (E)",                offset: 0x0ABE_0000, size: 0x1312_D000, generation: XboxGeneration::Original },
    XboxPartition { name: "Extended (F)",            offset: 0x1DD1_D000, size: 0,           generation: XboxGeneration::Original },
];

// ---------------------------------------------------------------------------
// Xbox 360 partitions — fixed offsets hardcoded in the kernel.
// Applies to ALL retail drive sizes (20GB through 1TB).
// ---------------------------------------------------------------------------

pub const XBOX360_PARTITIONS: &[XboxPartition] = &[
    XboxPartition {
        name: "360 System Cache",
        offset: 0x0008_0000,
        size: 0x8000_0000,   // 2 GB
        generation: XboxGeneration::Xbox360,
    },
    XboxPartition {
        name: "360 Game Content",
        offset: 0x8008_0000,
        size: 0xA0E3_0000,   // ~2.5 GB
        generation: XboxGeneration::Xbox360,
    },
    XboxPartition {
        name: "360 Xbox 1 Compat",
        offset: 0x1_20EB_0000,
        size: 0x1000_0000,   // 256 MB
        generation: XboxGeneration::Xbox360,
    },
    XboxPartition {
        name: "360 Data",
        offset: 0x1_30EB_0000,
        size: 0,             // rest of drive
        generation: XboxGeneration::Xbox360,
    },
];

/// All known partition offsets to check when scanning a drive.
/// We check both original Xbox and Xbox 360 layouts.
pub fn all_known_partitions() -> Vec<&'static XboxPartition> {
    let mut all: Vec<&XboxPartition> = Vec::new();
    for p in OG_XBOX_PARTITIONS {
        all.push(p);
    }
    for p in XBOX360_PARTITIONS {
        all.push(p);
    }
    all
}
