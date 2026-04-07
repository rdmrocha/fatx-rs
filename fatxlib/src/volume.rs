//! FATX volume — the main interface for reading and writing a FATX filesystem.
//!
//! A `FatxVolume` wraps a seekable reader/writer (file, block device, or disk image)
//! and provides methods to navigate directories, read files, and perform write operations.

use std::io::{Read, Seek, SeekFrom, Write};

use log::{info, warn};

use crate::error::{FatxError, Result};
use crate::types::*;

/// A mounted FATX volume backed by a seekable stream.
pub struct FatxVolume<T: Read + Write + Seek> {
    /// The underlying device / file handle.
    inner: T,
    /// Byte offset where this FATX partition starts within the device.
    /// (0 if the device *is* the partition.)
    partition_offset: u64,
    /// Parsed superblock.
    pub superblock: Superblock,
    /// Whether this volume uses FAT16 or FAT32.
    pub fat_type: FatType,
    /// Total number of clusters in the data area.
    pub total_clusters: u32,
    /// Byte offset of the FAT table (relative to partition start).
    fat_offset: u64,
    /// Size of the FAT in bytes (rounded up to 4KB boundary per original Xbox driver).
    #[allow(dead_code)]
    fat_size: u64,
    /// Byte offset of the data/cluster area (relative to partition start).
    data_offset: u64,
    /// Total size of this partition in bytes.
    partition_size: u64,
    /// Whether this volume uses big-endian on-disk format (Xbox 360 XTAF).
    big_endian: bool,
}

impl<T: Read + Write + Seek> FatxVolume<T> {
    /// Open a FATX volume.
    ///
    /// - `inner`: A seekable read/write handle to the device or image file.
    /// - `partition_offset`: Byte offset where the FATX partition begins.
    /// - `partition_size`: Size of the partition in bytes (0 = auto-detect from stream length).
    pub fn open(mut inner: T, partition_offset: u64, partition_size: u64) -> Result<Self> {
        // Determine actual partition size if not provided.
        let partition_size = if partition_size == 0 {
            let end = inner.seek(SeekFrom::End(0))?;
            end.saturating_sub(partition_offset)
        } else {
            partition_size
        };

        if partition_size < SUPERBLOCK_SIZE + SECTOR_SIZE {
            return Err(FatxError::VolumeTooSmall);
        }

        // Read the entire 4 KB superblock at once.
        // macOS raw devices require sector-aligned reads (512 bytes minimum),
        // so we read the full superblock rather than individual fields.
        inner.seek(SeekFrom::Start(partition_offset))?;
        let mut sb_buf = [0u8; SUPERBLOCK_SIZE as usize];
        inner.read_exact(&mut sb_buf)?;

        let magic: [u8; 4] = [sb_buf[0], sb_buf[1], sb_buf[2], sb_buf[3]];
        info!(
            "Read magic at offset 0x{:X}: {:02X} {:02X} {:02X} {:02X} (\"{}\")",
            partition_offset, magic[0], magic[1], magic[2], magic[3],
            String::from_utf8_lossy(&magic)
        );
        if !is_valid_magic(&magic) {
            return Err(FatxError::BadMagic(magic));
        }

        // Xbox 360 XTAF uses big-endian for superblock fields;
        // original Xbox FATX uses little-endian.
        let is_xtaf = &magic == b"XTAF";
        let (volume_id, sectors_per_cluster, fat_copies) = if is_xtaf {
            (
                u32::from_be_bytes([sb_buf[4], sb_buf[5], sb_buf[6], sb_buf[7]]),
                u32::from_be_bytes([sb_buf[8], sb_buf[9], sb_buf[10], sb_buf[11]]),
                u16::from_be_bytes([sb_buf[12], sb_buf[13]]),
            )
        } else {
            (
                u32::from_le_bytes([sb_buf[4], sb_buf[5], sb_buf[6], sb_buf[7]]),
                u32::from_le_bytes([sb_buf[8], sb_buf[9], sb_buf[10], sb_buf[11]]),
                u16::from_le_bytes([sb_buf[12], sb_buf[13]]),
            )
        };

        // Validate sectors_per_cluster (must be a power of 2, typically 1..128)
        if sectors_per_cluster == 0
            || sectors_per_cluster > 128
            || !sectors_per_cluster.is_power_of_two()
        {
            return Err(FatxError::BadSectorsPerCluster(sectors_per_cluster));
        }

        let superblock = Superblock {
            magic,
            volume_id,
            sectors_per_cluster,
            fat_copies,
        };

        let cluster_size = superblock.cluster_size();
        info!(
            "FATX volume: id=0x{:08X}, cluster_size={}, fat_copies={}",
            volume_id, cluster_size, fat_copies
        );

        // Calculate layout — based on the original Xbox FATX driver:
        //   1. FAT starts immediately after the 4KB superblock
        //   2. FAT size is rounded UP to 4KB boundary
        //   3. Data clusters begin right after the (rounded) FAT
        //   4. The root directory occupies the first cluster in the data area
        let fat_offset = SUPERBLOCK_SIZE;

        // Total sectors available after superblock (superblock = 8 sectors)
        let total_sectors = (partition_size / SECTOR_SIZE) - (SUPERBLOCK_SIZE / SECTOR_SIZE);
        let spc = sectors_per_cluster as u64;

        // Determine FAT type using the original driver's formula:
        //   if (total_sectors - 260) / sectors_per_cluster >= 65525 => FAT32
        // The "260" accounts for the root directory overhead estimate.
        let cluster_estimate = total_sectors.saturating_sub(260) / spc;
        let fat_type = if cluster_estimate >= 65_525 {
            FatType::Fat32
        } else {
            FatType::Fat16
        };

        let entry_size = fat_type.entry_size();

        // Calculate cluster count and FAT size.
        //
        // The Xbox 360 XTAF driver uses a naive formula that does NOT subtract
        // FAT space from the cluster count:
        //     total_clusters = (partition_size - superblock) / cluster_size
        //
        // The original Xbox FATX driver subtracts FAT overhead:
        //     total_clusters ≈ total_data_bytes / (cluster_size + entry_size)
        //
        // Using the wrong formula shifts the data_offset and causes the root
        // directory (and all data) to be read from the wrong location.
        let total_clusters = if is_xtaf {
            ((partition_size - SUPERBLOCK_SIZE) / cluster_size) as u32
        } else {
            (total_sectors * SECTOR_SIZE / (cluster_size + entry_size)) as u32
        };
        let raw_fat_size = total_clusters as u64 * entry_size;

        // Round FAT size UP to 4KB boundary (as the original driver does)
        let fat_size = (raw_fat_size + 0xFFF) & !0xFFF;

        // Data area begins right after the rounded FAT
        let data_offset = fat_offset + fat_size;

        info!(
            "FAT type: {}, clusters: {}, FAT size: {} bytes, data offset: 0x{:X}",
            fat_type, total_clusters, fat_size, data_offset
        );
        info!(
            "Layout: partition=0x{:X}+{}, superblock=0x{:X}..0x{:X}, FAT=0x{:X}..0x{:X} (raw {}), data=0x{:X}..end",
            partition_offset, crate::partition::format_size(partition_size),
            partition_offset, partition_offset + SUPERBLOCK_SIZE,
            partition_offset + fat_offset, partition_offset + fat_offset + fat_size,
            crate::partition::format_size(raw_fat_size),
            partition_offset + data_offset,
        );

        Ok(FatxVolume {
            inner,
            partition_offset,
            superblock,
            fat_type,
            total_clusters,
            fat_offset,
            fat_size,
            data_offset,
            partition_size,
            big_endian: is_xtaf,
        })
    }

    // -----------------------------------------------------------------------
    // Endian-aware integer helpers
    // -----------------------------------------------------------------------

    fn read_u16(&self, buf: &[u8]) -> u16 {
        if self.big_endian {
            u16::from_be_bytes([buf[0], buf[1]])
        } else {
            u16::from_le_bytes([buf[0], buf[1]])
        }
    }

    fn read_u32(&self, buf: &[u8]) -> u32 {
        if self.big_endian {
            u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]])
        } else {
            u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]])
        }
    }

    fn write_u16_bytes(&self, val: u16) -> [u8; 2] {
        if self.big_endian { val.to_be_bytes() } else { val.to_le_bytes() }
    }

    fn write_u32_bytes(&self, val: u32) -> [u8; 4] {
        if self.big_endian { val.to_be_bytes() } else { val.to_le_bytes() }
    }

    // -----------------------------------------------------------------------
    // Low-level I/O helpers (sector-aligned for macOS raw devices)
    // -----------------------------------------------------------------------

    /// Absolute byte offset within the device for a partition-relative offset.
    fn abs_offset(&self, partition_rel: u64) -> u64 {
        self.partition_offset + partition_rel
    }

    /// Read `buf.len()` bytes from a partition-relative offset.
    /// Handles sector alignment automatically for raw devices.
    fn read_at(&mut self, partition_rel: u64, buf: &mut [u8]) -> Result<()> {
        let abs = self.abs_offset(partition_rel);
        // Align to 512-byte sector boundary
        let aligned_start = abs & !0x1FF;
        let pre_skip = (abs - aligned_start) as usize;
        let total_needed = pre_skip + buf.len();
        let aligned_len = (total_needed + 511) & !511; // round up to sector

        self.inner.seek(SeekFrom::Start(aligned_start))?;
        let mut aligned_buf = vec![0u8; aligned_len];
        self.inner.read_exact(&mut aligned_buf)?;
        buf.copy_from_slice(&aligned_buf[pre_skip..pre_skip + buf.len()]);
        Ok(())
    }

    /// Write `buf` at a partition-relative offset.
    /// For raw devices, does a read-modify-write if the write isn't sector-aligned.
    fn write_at(&mut self, partition_rel: u64, buf: &[u8]) -> Result<()> {
        let abs = self.abs_offset(partition_rel);
        let aligned_start = abs & !0x1FF;
        let pre_skip = (abs - aligned_start) as usize;
        let total_needed = pre_skip + buf.len();
        let aligned_len = (total_needed + 511) & !511;

        if pre_skip == 0 && buf.len() % 512 == 0 {
            // Already aligned — write directly
            self.inner.seek(SeekFrom::Start(abs))?;
            self.inner.write_all(buf)?;
        } else {
            // Read-modify-write
            self.inner.seek(SeekFrom::Start(aligned_start))?;
            let mut aligned_buf = vec![0u8; aligned_len];
            self.inner.read_exact(&mut aligned_buf)?;
            aligned_buf[pre_skip..pre_skip + buf.len()].copy_from_slice(buf);
            self.inner.seek(SeekFrom::Start(aligned_start))?;
            self.inner.write_all(&aligned_buf)?;
        }
        Ok(())
    }

    /// Returns the byte offset of the given cluster's data (partition-relative).
    fn cluster_offset(&self, cluster: u32) -> Result<u64> {
        if cluster < FIRST_CLUSTER || cluster >= FIRST_CLUSTER + self.total_clusters {
            return Err(FatxError::ClusterOutOfRange(
                cluster,
                FIRST_CLUSTER + self.total_clusters - 1,
            ));
        }
        Ok(self.data_offset + (cluster - FIRST_CLUSTER) as u64 * self.superblock.cluster_size())
    }

    // -----------------------------------------------------------------------
    // FAT operations
    // -----------------------------------------------------------------------

    /// Read a single FAT entry for the given cluster.
    pub fn read_fat_entry(&mut self, cluster: u32) -> Result<FatEntry> {
        let entry_offset = self.fat_offset + (cluster as u64) * self.fat_type.entry_size();

        match self.fat_type {
            FatType::Fat16 => {
                let mut buf = [0u8; 2];
                self.read_at(entry_offset, &mut buf)?;
                let val = self.read_u16(&buf);
                Ok(match val {
                    FAT16_FREE => FatEntry::Free,
                    FAT16_BAD => FatEntry::Bad,
                    v if v >= FAT16_EOC => FatEntry::EndOfChain,
                    v => FatEntry::Next(v as u32),
                })
            }
            FatType::Fat32 => {
                let mut buf = [0u8; 4];
                self.read_at(entry_offset, &mut buf)?;
                let val = self.read_u32(&buf);
                Ok(match val {
                    FAT32_FREE => FatEntry::Free,
                    FAT32_BAD => FatEntry::Bad,
                    v if v >= FAT32_EOC => FatEntry::EndOfChain,
                    v => FatEntry::Next(v),
                })
            }
        }
    }

    /// Write a FAT entry for the given cluster.
    pub fn write_fat_entry(&mut self, cluster: u32, entry: FatEntry) -> Result<()> {
        let entry_offset = self.fat_offset + (cluster as u64) * self.fat_type.entry_size();

        match self.fat_type {
            FatType::Fat16 => {
                let val: u16 = match entry {
                    FatEntry::Free => FAT16_FREE,
                    FatEntry::EndOfChain => FAT16_EOC,
                    FatEntry::Bad => FAT16_BAD,
                    FatEntry::Next(c) => c as u16,
                };
                self.write_at(entry_offset, &self.write_u16_bytes(val))?;
            }
            FatType::Fat32 => {
                let val: u32 = match entry {
                    FatEntry::Free => FAT32_FREE,
                    FatEntry::EndOfChain => FAT32_EOC,
                    FatEntry::Bad => FAT32_BAD,
                    FatEntry::Next(c) => c,
                };
                self.write_at(entry_offset, &self.write_u32_bytes(val))?;
            }
        }
        Ok(())
    }

    /// Follow the cluster chain starting from `start_cluster` and return
    /// the list of clusters in order.
    pub fn read_chain(&mut self, start_cluster: u32) -> Result<Vec<u32>> {
        let mut chain = Vec::new();
        let mut current = start_cluster;
        let max_iters = self.total_clusters as usize + 1; // safety bound

        for _ in 0..max_iters {
            chain.push(current);
            match self.read_fat_entry(current)? {
                FatEntry::EndOfChain => break,
                FatEntry::Next(next) => current = next,
                FatEntry::Free => {
                    warn!("Cluster chain hit free cluster at {}", current);
                    return Err(FatxError::CorruptChain(current));
                }
                FatEntry::Bad => {
                    warn!("Cluster chain hit bad cluster at {}", current);
                    return Err(FatxError::CorruptChain(current));
                }
            }
        }

        Ok(chain)
    }

    /// Find a free cluster and mark it as end-of-chain. Returns the cluster index.
    pub fn allocate_cluster(&mut self) -> Result<u32> {
        for cluster in FIRST_CLUSTER..(FIRST_CLUSTER + self.total_clusters) {
            if let FatEntry::Free = self.read_fat_entry(cluster)? {
                self.write_fat_entry(cluster, FatEntry::EndOfChain)?;
                return Ok(cluster);
            }
        }
        Err(FatxError::DiskFull)
    }

    /// Allocate `count` clusters and chain them together. Returns the first cluster.
    pub fn allocate_chain(&mut self, count: usize) -> Result<u32> {
        if count == 0 {
            return Err(FatxError::DiskFull);
        }

        let mut allocated = Vec::with_capacity(count);
        for cluster in FIRST_CLUSTER..(FIRST_CLUSTER + self.total_clusters) {
            if let FatEntry::Free = self.read_fat_entry(cluster)? {
                allocated.push(cluster);
                if allocated.len() == count {
                    break;
                }
            }
        }

        if allocated.len() < count {
            return Err(FatxError::DiskFull);
        }

        // Chain them together
        for i in 0..allocated.len() - 1 {
            self.write_fat_entry(allocated[i], FatEntry::Next(allocated[i + 1]))?;
        }
        self.write_fat_entry(*allocated.last().unwrap(), FatEntry::EndOfChain)?;

        Ok(allocated[0])
    }

    /// Free all clusters in a chain starting at `start_cluster`.
    pub fn free_chain(&mut self, start_cluster: u32) -> Result<()> {
        let chain = self.read_chain(start_cluster)?;
        for cluster in chain {
            self.write_fat_entry(cluster, FatEntry::Free)?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Cluster I/O
    // -----------------------------------------------------------------------

    /// Read a full cluster into `buf`. The buffer must be `cluster_size` bytes.
    pub fn read_cluster(&mut self, cluster: u32, buf: &mut [u8]) -> Result<()> {
        let offset = self.cluster_offset(cluster)?;
        self.read_at(offset, buf)?;
        Ok(())
    }

    /// Write a full cluster from `buf`.
    pub fn write_cluster(&mut self, cluster: u32, buf: &[u8]) -> Result<()> {
        let offset = self.cluster_offset(cluster)?;
        self.write_at(offset, buf)?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Directory entry I/O
    // -----------------------------------------------------------------------

    /// Parse a single 64-byte directory entry at the given partition-relative offset.
    /// Uses sector-aligned I/O so it works on macOS raw devices.
    fn read_dirent_at(&mut self, partition_rel: u64) -> Result<DirectoryEntry> {
        let mut buf = [0u8; DIRENT_SIZE];
        self.read_at(partition_rel, &mut buf)?;

        let filename_len = buf[0];
        let attributes = FileAttributes::from_bits_truncate(buf[1]);
        let mut filename_raw = [0u8; MAX_FILENAME_LEN];
        filename_raw.copy_from_slice(&buf[2..2 + MAX_FILENAME_LEN]);
        let first_cluster = self.read_u32(&[buf[44], buf[45], buf[46], buf[47]]);
        let file_size = self.read_u32(&[buf[48], buf[49], buf[50], buf[51]]);
        // XTAF (Xbox 360) stores timestamps as date-then-time at each pair of offsets,
        // while original FATX stores time-then-date. Both are 2-byte fields.
        let (creation_time, creation_date) = if self.big_endian {
            (self.read_u16(&[buf[54], buf[55]]), self.read_u16(&[buf[52], buf[53]]))
        } else {
            (self.read_u16(&[buf[52], buf[53]]), self.read_u16(&[buf[54], buf[55]]))
        };
        let (write_time, write_date) = if self.big_endian {
            (self.read_u16(&[buf[58], buf[59]]), self.read_u16(&[buf[56], buf[57]]))
        } else {
            (self.read_u16(&[buf[56], buf[57]]), self.read_u16(&[buf[58], buf[59]]))
        };
        let (access_time, access_date) = if self.big_endian {
            (self.read_u16(&[buf[62], buf[63]]), self.read_u16(&[buf[60], buf[61]]))
        } else {
            (self.read_u16(&[buf[60], buf[61]]), self.read_u16(&[buf[62], buf[63]]))
        };

        Ok(DirectoryEntry {
            filename_len,
            attributes,
            filename_raw,
            first_cluster,
            file_size,
            creation_time,
            creation_date,
            write_time,
            write_date,
            access_time,
            access_date,
        })
    }

    /// Read all valid directory entries from a directory cluster chain.
    pub fn read_directory(&mut self, first_cluster: u32) -> Result<Vec<DirectoryEntry>> {
        let chain = self.read_chain(first_cluster)?;
        let cluster_size = self.superblock.cluster_size() as usize;
        let entries_per_cluster = cluster_size / DIRENT_SIZE;
        let mut entries = Vec::new();

        for &cluster in &chain {
            let base_offset = self.cluster_offset(cluster)?;

            for slot in 0..entries_per_cluster {
                let slot_offset = base_offset + (slot * DIRENT_SIZE) as u64;
                let entry = self.read_dirent_at(slot_offset)?;
                if entry.is_end() {
                    return Ok(entries);
                }
                if !entry.is_deleted() {
                    entries.push(entry);
                }
            }
        }

        Ok(entries)
    }

    /// Read the root directory entries (root directory starts at cluster 1).
    pub fn read_root_directory(&mut self) -> Result<Vec<DirectoryEntry>> {
        self.read_directory(FIRST_CLUSTER)
    }

    /// Resolve a path like "/saves/game1.sav" into directory entries along the way,
    /// returning the final entry.
    pub fn resolve_path(&mut self, path: &str) -> Result<DirectoryEntry> {
        let parts: Vec<&str> = path
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();

        if parts.is_empty() {
            // Root directory pseudo-entry
            return Ok(DirectoryEntry {
                filename_len: 1,
                attributes: FileAttributes::DIRECTORY,
                filename_raw: [0xFF; MAX_FILENAME_LEN],
                first_cluster: FIRST_CLUSTER,
                file_size: 0,
                creation_time: 0,
                creation_date: 0,
                write_time: 0,
                write_date: 0,
                access_time: 0,
                access_date: 0,
            });
        }

        let mut current_cluster = FIRST_CLUSTER;

        for (i, part) in parts.iter().enumerate() {
            let entries = self.read_directory(current_cluster)?;
            let found = entries
                .into_iter()
                .find(|e| e.filename().eq_ignore_ascii_case(part));

            match found {
                Some(entry) => {
                    if i < parts.len() - 1 {
                        // Intermediate path component must be a directory
                        if !entry.is_directory() {
                            return Err(FatxError::NotADirectory(part.to_string()));
                        }
                        current_cluster = entry.first_cluster;
                    } else {
                        return Ok(entry);
                    }
                }
                None => return Err(FatxError::FileNotFound(part.to_string())),
            }
        }

        unreachable!()
    }

    // -----------------------------------------------------------------------
    // File reading
    // -----------------------------------------------------------------------

    /// Read the full contents of a file given its directory entry.
    pub fn read_file(&mut self, entry: &DirectoryEntry) -> Result<Vec<u8>> {
        if entry.is_directory() {
            return Err(FatxError::IsADirectory(entry.filename()));
        }

        let chain = self.read_chain(entry.first_cluster)?;
        let cluster_size = self.superblock.cluster_size() as usize;
        let file_size = entry.file_size as usize;
        let mut data = Vec::with_capacity(file_size);
        let mut remaining = file_size;

        for &cluster in &chain {
            let to_read = remaining.min(cluster_size);
            let mut buf = vec![0u8; cluster_size];
            self.read_cluster(cluster, &mut buf)?;
            data.extend_from_slice(&buf[..to_read]);
            remaining -= to_read;
            if remaining == 0 {
                break;
            }
        }

        Ok(data)
    }

    /// Read a file by path.
    pub fn read_file_by_path(&mut self, path: &str) -> Result<Vec<u8>> {
        let entry = self.resolve_path(path)?;
        self.read_file(&entry)
    }

    // -----------------------------------------------------------------------
    // Write operations
    // -----------------------------------------------------------------------

    /// Validate a filename for FATX.
    fn validate_filename(name: &str) -> Result<()> {
        if name.len() > MAX_FILENAME_LEN {
            return Err(FatxError::FilenameTooLong(name.len(), MAX_FILENAME_LEN));
        }
        if name.is_empty() {
            return Err(FatxError::FilenameTooLong(0, MAX_FILENAME_LEN));
        }
        for ch in name.chars() {
            if ch == '\0' || ch as u32 > 127 {
                return Err(FatxError::InvalidFilenameChar(ch));
            }
        }
        Ok(())
    }

    /// Serialize a DirectoryEntry back to its 64-byte on-disk form.
    fn serialize_dirent(&self, entry: &DirectoryEntry) -> [u8; DIRENT_SIZE] {
        let mut buf = [0u8; DIRENT_SIZE];
        buf[0] = entry.filename_len;
        buf[1] = entry.attributes.bits();
        buf[2..2 + MAX_FILENAME_LEN].copy_from_slice(&entry.filename_raw);
        buf[44..48].copy_from_slice(&self.write_u32_bytes(entry.first_cluster));
        buf[48..52].copy_from_slice(&self.write_u32_bytes(entry.file_size));
        // XTAF stores date-then-time; FATX stores time-then-date
        if self.big_endian {
            buf[52..54].copy_from_slice(&self.write_u16_bytes(entry.creation_date));
            buf[54..56].copy_from_slice(&self.write_u16_bytes(entry.creation_time));
            buf[56..58].copy_from_slice(&self.write_u16_bytes(entry.write_date));
            buf[58..60].copy_from_slice(&self.write_u16_bytes(entry.write_time));
            buf[60..62].copy_from_slice(&self.write_u16_bytes(entry.access_date));
            buf[62..64].copy_from_slice(&self.write_u16_bytes(entry.access_time));
        } else {
            buf[52..54].copy_from_slice(&self.write_u16_bytes(entry.creation_time));
            buf[54..56].copy_from_slice(&self.write_u16_bytes(entry.creation_date));
            buf[56..58].copy_from_slice(&self.write_u16_bytes(entry.write_time));
            buf[58..60].copy_from_slice(&self.write_u16_bytes(entry.write_date));
            buf[60..62].copy_from_slice(&self.write_u16_bytes(entry.access_time));
            buf[62..64].copy_from_slice(&self.write_u16_bytes(entry.access_date));
        }
        buf
    }

    /// Create a new directory entry in the given parent directory cluster chain.
    fn add_dirent_to_directory(
        &mut self,
        parent_cluster: u32,
        entry: &DirectoryEntry,
    ) -> Result<()> {
        let chain = self.read_chain(parent_cluster)?;
        let cluster_size = self.superblock.cluster_size() as usize;
        let entries_per_cluster = cluster_size / DIRENT_SIZE;

        // Search for a free slot (deleted or end-of-directory)
        for &cluster in &chain {
            let base_offset = self.cluster_offset(cluster)?;
            for slot in 0..entries_per_cluster {
                let slot_offset = base_offset + (slot * DIRENT_SIZE) as u64;
                // Read just the first byte (marker) via a full dirent read
                let mut marker_buf = [0u8; 1];
                self.read_at(slot_offset, &mut marker_buf)?;
                let marker = marker_buf[0];

                if marker == DIRENT_END || marker == DIRENT_DELETED || marker == 0x00 {
                    // Found a free slot — write the entry
                    let raw = self.serialize_dirent(entry);
                    self.write_at(slot_offset, &raw)?;

                    // If we overwrote an end marker and there's space after, write a new end marker
                    if marker == DIRENT_END || marker == 0x00 {
                        let next_slot = slot + 1;
                        if next_slot < entries_per_cluster {
                            let next_offset = base_offset + (next_slot * DIRENT_SIZE) as u64;
                            self.write_at(next_offset, &[DIRENT_END])?;
                        }
                    }

                    return Ok(());
                }
            }
        }

        // No free slot in existing clusters — allocate a new cluster for the directory
        let new_cluster = self.allocate_cluster()?;

        // Extend the chain: update the last cluster to point to the new one
        let last_cluster = *chain.last().unwrap();
        self.write_fat_entry(last_cluster, FatEntry::Next(new_cluster))?;

        // Initialize the new cluster with 0xFF (end markers)
        let blank = vec![0xFF; cluster_size];
        self.write_cluster(new_cluster, &blank)?;

        // Write the entry at the first slot of the new cluster
        let base_offset = self.cluster_offset(new_cluster)?;
        let raw = self.serialize_dirent(entry);
        self.write_at(base_offset, &raw)?;

        // Write end marker at slot 1
        if entries_per_cluster > 1 {
            self.write_at(base_offset + DIRENT_SIZE as u64, &[DIRENT_END])?;
        }

        Ok(())
    }

    /// Create a new file with the given data at the specified path.
    pub fn create_file(&mut self, path: &str, data: &[u8]) -> Result<()> {
        let (parent_path, filename) = split_path(path);
        Self::validate_filename(filename)?;

        // Resolve parent directory
        let parent = self.resolve_path(parent_path)?;
        if !parent.attributes.contains(FileAttributes::DIRECTORY) {
            return Err(FatxError::NotADirectory(parent_path.to_string()));
        }

        // Allocate clusters for the file data
        let cluster_size = self.superblock.cluster_size() as usize;
        let clusters_needed = if data.is_empty() {
            1
        } else {
            (data.len() + cluster_size - 1) / cluster_size
        };

        let first_cluster = self.allocate_chain(clusters_needed)?;

        // Write the data
        let chain = self.read_chain(first_cluster)?;
        let mut offset = 0;
        for &cluster in &chain {
            let end = (offset + cluster_size).min(data.len());
            if offset < data.len() {
                let mut cluster_buf = vec![0u8; cluster_size];
                let len = end - offset;
                cluster_buf[..len].copy_from_slice(&data[offset..end]);
                self.write_cluster(cluster, &cluster_buf)?;
            }
            offset += cluster_size;
        }

        // Create directory entry
        let now = chrono::Local::now();
        let date = DirectoryEntry::encode_date(now.format("%Y").to_string().parse().unwrap_or(2025), now.format("%m").to_string().parse().unwrap_or(1), now.format("%d").to_string().parse().unwrap_or(1));
        let time = DirectoryEntry::encode_time(now.format("%H").to_string().parse().unwrap_or(0), now.format("%M").to_string().parse().unwrap_or(0), now.format("%S").to_string().parse().unwrap_or(0));

        let mut filename_raw = [0xFFu8; MAX_FILENAME_LEN];
        let name_bytes = filename.as_bytes();
        filename_raw[..name_bytes.len()].copy_from_slice(name_bytes);

        let entry = DirectoryEntry {
            filename_len: name_bytes.len() as u8,
            attributes: FileAttributes::ARCHIVE,
            filename_raw,
            first_cluster,
            file_size: data.len() as u32,
            creation_time: time,
            creation_date: date,
            write_time: time,
            write_date: date,
            access_time: time,
            access_date: date,
        };

        self.add_dirent_to_directory(parent.first_cluster, &entry)?;
        info!("Created file '{}' ({} bytes, {} clusters)", filename, data.len(), clusters_needed);
        Ok(())
    }

    /// Create a new directory at the specified path.
    pub fn create_directory(&mut self, path: &str) -> Result<()> {
        let (parent_path, dirname) = split_path(path);
        Self::validate_filename(dirname)?;

        let parent = self.resolve_path(parent_path)?;
        if !parent.attributes.contains(FileAttributes::DIRECTORY) {
            return Err(FatxError::NotADirectory(parent_path.to_string()));
        }

        // Allocate one cluster for the new directory
        let cluster = self.allocate_cluster()?;

        // Initialize with end markers
        let cluster_size = self.superblock.cluster_size() as usize;
        let blank = vec![0xFFu8; cluster_size];
        self.write_cluster(cluster, &blank)?;

        let now = chrono::Local::now();
        let date = DirectoryEntry::encode_date(
            now.format("%Y").to_string().parse().unwrap_or(2025),
            now.format("%m").to_string().parse().unwrap_or(1),
            now.format("%d").to_string().parse().unwrap_or(1),
        );
        let time = DirectoryEntry::encode_time(
            now.format("%H").to_string().parse().unwrap_or(0),
            now.format("%M").to_string().parse().unwrap_or(0),
            now.format("%S").to_string().parse().unwrap_or(0),
        );

        let mut filename_raw = [0xFFu8; MAX_FILENAME_LEN];
        let name_bytes = dirname.as_bytes();
        filename_raw[..name_bytes.len()].copy_from_slice(name_bytes);

        let entry = DirectoryEntry {
            filename_len: name_bytes.len() as u8,
            attributes: FileAttributes::DIRECTORY,
            filename_raw,
            first_cluster: cluster,
            file_size: 0,
            creation_time: time,
            creation_date: date,
            write_time: time,
            write_date: date,
            access_time: time,
            access_date: date,
        };

        self.add_dirent_to_directory(parent.first_cluster, &entry)?;
        info!("Created directory '{}'", dirname);
        Ok(())
    }

    /// Delete a file or empty directory at the specified path.
    pub fn delete(&mut self, path: &str) -> Result<()> {
        let (parent_path, target_name) = split_path(path);

        let parent = self.resolve_path(parent_path)?;
        let target = self.resolve_path(path)?;

        // If target is a directory, ensure it's empty
        if target.is_directory() {
            let contents = self.read_directory(target.first_cluster)?;
            if !contents.is_empty() {
                return Err(FatxError::DirectoryNotEmpty(path.to_string()));
            }
        }

        // Free the cluster chain
        self.free_chain(target.first_cluster)?;

        // Mark the directory entry as deleted
        self.mark_dirent_deleted(parent.first_cluster, target_name)?;

        info!("Deleted '{}'", path);
        Ok(())
    }

    /// Find and mark a directory entry as deleted (set filename_len to 0xE5).
    fn mark_dirent_deleted(&mut self, parent_cluster: u32, name: &str) -> Result<()> {
        let chain = self.read_chain(parent_cluster)?;
        let cluster_size = self.superblock.cluster_size() as usize;
        let entries_per_cluster = cluster_size / DIRENT_SIZE;

        for &cluster in &chain {
            let base_offset = self.cluster_offset(cluster)?;
            for slot in 0..entries_per_cluster {
                let slot_offset = base_offset + (slot * DIRENT_SIZE) as u64;
                let entry = self.read_dirent_at(slot_offset)?;

                if entry.is_end() {
                    return Err(FatxError::FileNotFound(name.to_string()));
                }
                if !entry.is_deleted() && entry.filename().eq_ignore_ascii_case(name) {
                    // Mark as deleted by writing 0xE5 to the first byte
                    self.write_at(slot_offset, &[DIRENT_DELETED])?;
                    return Ok(());
                }
            }
        }

        Err(FatxError::FileNotFound(name.to_string()))
    }

    /// Rename a file or directory.
    pub fn rename(&mut self, old_path: &str, new_name: &str) -> Result<()> {
        Self::validate_filename(new_name)?;

        let (parent_path, old_name) = split_path(old_path);
        let parent = self.resolve_path(parent_path)?;

        let chain = self.read_chain(parent.first_cluster)?;
        let cluster_size = self.superblock.cluster_size() as usize;
        let entries_per_cluster = cluster_size / DIRENT_SIZE;

        for &cluster in &chain {
            let base_offset = self.cluster_offset(cluster)?;
            for slot in 0..entries_per_cluster {
                let slot_offset = base_offset + (slot * DIRENT_SIZE) as u64;
                let entry = self.read_dirent_at(slot_offset)?;

                if entry.is_end() {
                    return Err(FatxError::FileNotFound(old_name.to_string()));
                }
                if !entry.is_deleted() && entry.filename().eq_ignore_ascii_case(old_name) {
                    // Read the full 64-byte entry, update filename fields, write back
                    let mut raw = [0u8; DIRENT_SIZE];
                    self.read_at(slot_offset, &mut raw)?;

                    let name_bytes = new_name.as_bytes();
                    raw[0] = name_bytes.len() as u8;
                    // Clear filename area and write new name
                    raw[2..2 + MAX_FILENAME_LEN].fill(0xFF);
                    raw[2..2 + name_bytes.len()].copy_from_slice(name_bytes);

                    self.write_at(slot_offset, &raw)?;

                    info!("Renamed '{}' -> '{}'", old_name, new_name);
                    return Ok(());
                }
            }
        }

        Err(FatxError::FileNotFound(old_name.to_string()))
    }

    /// Flush any buffered writes to the underlying device.
    pub fn flush(&mut self) -> Result<()> {
        self.inner.flush()?;
        Ok(())
    }

    /// Get volume statistics.
    pub fn stats(&mut self) -> Result<VolumeStats> {
        let mut free_clusters = 0u32;
        let mut used_clusters = 0u32;
        let mut bad_clusters = 0u32;

        for cluster in FIRST_CLUSTER..(FIRST_CLUSTER + self.total_clusters) {
            match self.read_fat_entry(cluster)? {
                FatEntry::Free => free_clusters += 1,
                FatEntry::Bad => bad_clusters += 1,
                _ => used_clusters += 1,
            }
        }

        let cluster_size = self.superblock.cluster_size();
        Ok(VolumeStats {
            total_clusters: self.total_clusters,
            free_clusters,
            used_clusters,
            bad_clusters,
            cluster_size,
            total_size: self.partition_size,
            free_size: free_clusters as u64 * cluster_size,
            used_size: used_clusters as u64 * cluster_size,
        })
    }
}

/// Volume usage statistics.
#[derive(Debug)]
pub struct VolumeStats {
    pub total_clusters: u32,
    pub free_clusters: u32,
    pub used_clusters: u32,
    pub bad_clusters: u32,
    pub cluster_size: u64,
    pub total_size: u64,
    pub free_size: u64,
    pub used_size: u64,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Split a path into (parent, basename).
/// "/saves/game1.sav" -> ("/saves", "game1.sav")
/// "/readme.txt" -> ("/", "readme.txt")
fn split_path(path: &str) -> (&str, &str) {
    let path = path.trim_end_matches('/');
    match path.rfind('/') {
        Some(0) => ("/", &path[1..]),
        Some(pos) => (&path[..pos], &path[pos + 1..]),
        None => ("/", path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_path() {
        assert_eq!(split_path("/foo/bar.txt"), ("/foo", "bar.txt"));
        assert_eq!(split_path("/bar.txt"), ("/", "bar.txt"));
        assert_eq!(split_path("bar.txt"), ("/", "bar.txt"));
        assert_eq!(split_path("/a/b/c"), ("/a/b", "c"));
    }
}
