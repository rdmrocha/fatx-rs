//! Integration tests for fatxlib.
//!
//! These tests create in-memory FATX images and exercise the full read/write API.
//! No hardware required — all tests use Cursor-backed images.

use std::io::Cursor;

use fatxlib::types::*;
use fatxlib::volume::FatxVolume;

// ===========================================================================
// Test image helpers
// ===========================================================================

/// Create a minimal FATX (little-endian) image in memory.
fn create_test_image(size_mb: usize) -> Cursor<Vec<u8>> {
    create_image_with_format(size_mb, false)
}

/// Create a minimal XTAF (big-endian, Xbox 360) image in memory.
fn create_xtaf_image(size_mb: usize) -> Cursor<Vec<u8>> {
    create_image_with_format(size_mb, true)
}

/// Create a FATX or XTAF image with proper layout.
fn create_image_with_format(size_mb: usize, is_xtaf: bool) -> Cursor<Vec<u8>> {
    let size = size_mb * 1024 * 1024;
    let mut data = vec![0u8; size];

    // Write superblock
    if is_xtaf {
        data[0..4].copy_from_slice(b"XTAF");
        data[4..8].copy_from_slice(&0xDEADBEEFu32.to_be_bytes());
        data[8..12].copy_from_slice(&32u32.to_be_bytes());
        data[12..14].copy_from_slice(&1u16.to_be_bytes());
    } else {
        data[0..4].copy_from_slice(b"FATX");
        data[4..8].copy_from_slice(&0xDEADBEEFu32.to_le_bytes());
        data[8..12].copy_from_slice(&32u32.to_le_bytes());
        data[12..14].copy_from_slice(&1u16.to_le_bytes());
    }

    // Mark cluster 1 (root dir) as EOC in the FAT
    let fat_offset = 0x1000usize;

    // Determine FAT type for this size
    let sector_size = 512u64;
    let spc = 32u64;
    let cluster_size = spc * sector_size;
    let superblock_size = 0x1000u64;
    let total_sectors = (size as u64 / sector_size) - (superblock_size / sector_size);
    let cluster_estimate = total_sectors.saturating_sub(260) / spc;
    let is_fat32 = cluster_estimate >= 65_525;

    if is_fat32 {
        let eoc = if is_xtaf {
            FAT32_EOC.to_be_bytes()
        } else {
            FAT32_EOC.to_le_bytes()
        };
        data[fat_offset + 4..fat_offset + 8].copy_from_slice(&eoc);
    } else {
        let eoc = if is_xtaf {
            FAT16_EOC.to_be_bytes()
        } else {
            FAT16_EOC.to_le_bytes()
        };
        data[fat_offset + 2..fat_offset + 4].copy_from_slice(&eoc);
    }

    // Calculate data offset (same as volume.rs)
    let entry_size = if is_fat32 { 4u64 } else { 2u64 };
    let total_clusters = if is_xtaf {
        ((size as u64 - superblock_size) / cluster_size) as u32
    } else {
        (total_sectors * sector_size / (cluster_size + entry_size)) as u32
    };
    let raw_fat_size = total_clusters as u64 * entry_size;
    let fat_size = (raw_fat_size + 0xFFF) & !0xFFF;
    let data_offset = (superblock_size + fat_size) as usize;

    // Fill root directory cluster with 0xFF (end-of-directory markers)
    for i in 0..cluster_size as usize {
        if data_offset + i < data.len() {
            data[data_offset + i] = 0xFF;
        }
    }

    Cursor::new(data)
}

// ===========================================================================
// Volume open / basics
// ===========================================================================

#[test]
fn test_open_volume() {
    let cursor = create_test_image(2);
    let vol = FatxVolume::open(cursor, 0, 0).expect("Failed to open volume");
    assert!(vol.superblock.is_valid());
    assert_eq!(vol.superblock.volume_id, 0xDEADBEEF);
    assert_eq!(vol.superblock.sectors_per_cluster, 32);
    assert_eq!(vol.fat_type, FatType::Fat16);
}

#[test]
fn test_open_xtaf_volume() {
    let cursor = create_xtaf_image(2);
    let vol = FatxVolume::open(cursor, 0, 0).expect("Failed to open XTAF volume");
    assert!(vol.superblock.is_valid());
    assert_eq!(&vol.superblock.magic, b"XTAF");
    assert_eq!(vol.superblock.volume_id, 0xDEADBEEF);
    assert_eq!(vol.fat_type, FatType::Fat16);
}

#[test]
fn test_open_volume_with_offset() {
    // Embed a FATX image at a non-zero offset (simulating a partition)
    let inner = create_test_image(2);
    let raw = inner.into_inner();

    let offset = 0x10000usize; // 64 KB offset
    let mut padded = vec![0u8; offset + raw.len()];
    padded[offset..offset + raw.len()].copy_from_slice(&raw);

    let cursor = Cursor::new(padded);
    let vol = FatxVolume::open(cursor, offset as u64, raw.len() as u64).expect("open with offset");
    assert!(vol.superblock.is_valid());
}

#[test]
fn test_invalid_magic_fails() {
    let mut data = vec![0u8; 2 * 1024 * 1024];
    data[0..4].copy_from_slice(b"NOPE");
    let cursor = Cursor::new(data);
    assert!(FatxVolume::open(cursor, 0, 0).is_err());
}

#[test]
fn test_too_small_volume_fails() {
    let data = vec![0u8; 512]; // way too small
    let cursor = Cursor::new(data);
    assert!(FatxVolume::open(cursor, 0, 0).is_err());
}

// ===========================================================================
// Directory operations
// ===========================================================================

#[test]
fn test_read_empty_root() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("Failed to open volume");
    let entries = vol.read_root_directory().expect("Failed to read root dir");
    assert!(entries.is_empty(), "Root directory should be empty");
}

#[test]
fn test_create_directory() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    vol.create_directory("/saves").expect("mkdir");
    let entries = vol.read_root_directory().expect("readdir");
    assert_eq!(entries.len(), 1);
    assert!(entries[0].is_directory());
    assert_eq!(entries[0].filename(), "saves");
}

#[test]
fn test_create_nested_directories() {
    let cursor = create_test_image(4);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    vol.create_directory("/a").expect("mkdir a");
    vol.create_directory("/a/b").expect("mkdir a/b");
    vol.create_directory("/a/b/c").expect("mkdir a/b/c");

    let cluster = vol.resolve_path("/a/b").unwrap().first_cluster;
    let entries = vol.read_directory(cluster).expect("readdir a/b");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].filename(), "c");
}

#[test]
fn test_create_multiple_entries_in_directory() {
    let cursor = create_test_image(4);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    for i in 0..20 {
        let name = format!("/file_{:02}.txt", i);
        vol.create_file(&name, format!("data {}", i).as_bytes())
            .expect("create");
    }

    let entries = vol.read_root_directory().expect("readdir");
    assert_eq!(entries.len(), 20);
}

// ===========================================================================
// File operations
// ===========================================================================

#[test]
fn test_create_and_read_file() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    let test_data = b"Hello, Xbox FATX filesystem!";
    vol.create_file("/test.txt", test_data).expect("create");

    let read_data = vol.read_file_by_path("/test.txt").expect("read");
    assert_eq!(read_data, test_data);
}

#[test]
fn test_create_empty_file() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    vol.create_file("/empty.txt", &[]).expect("create empty");

    let read_data = vol.read_file_by_path("/empty.txt").expect("read");
    assert!(read_data.is_empty());
}

#[test]
fn test_create_file_in_subdirectory() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    vol.create_directory("/saves").expect("mkdir");
    let save_data = b"save game data here";
    vol.create_file("/saves/game1.sav", save_data)
        .expect("create");

    let read_data = vol.read_file_by_path("/saves/game1.sav").expect("read");
    assert_eq!(read_data, save_data);
}

#[test]
fn test_file_spanning_multiple_clusters() {
    // 16 KB clusters, so a 64 KB file spans 4 clusters
    let cursor = create_test_image(4);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    let big_data: Vec<u8> = (0..65536u32).map(|i| (i % 256) as u8).collect();
    vol.create_file("/big.bin", &big_data).expect("create big");

    let read_data = vol.read_file_by_path("/big.bin").expect("read big");
    assert_eq!(read_data.len(), 65536);
    assert_eq!(read_data, big_data);
}

#[test]
fn test_file_exact_cluster_size() {
    // File exactly 16384 bytes = 1 cluster
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    let data = vec![0xAB; 16384];
    vol.create_file("/exact.bin", &data).expect("create");

    let read = vol.read_file_by_path("/exact.bin").expect("read");
    assert_eq!(read.len(), 16384);
    assert_eq!(read, data);
}

#[test]
fn test_file_one_byte() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    vol.create_file("/one.bin", &[42]).expect("create");
    let read = vol.read_file_by_path("/one.bin").expect("read");
    assert_eq!(read, vec![42]);
}

#[test]
fn test_large_file_256kb() {
    let cursor = create_test_image(4);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    let data: Vec<u8> = (0..262144u32).map(|i| (i % 251) as u8).collect();
    vol.create_file("/large.bin", &data).expect("create");

    let read = vol.read_file_by_path("/large.bin").expect("read");
    assert_eq!(read.len(), 262144);
    assert_eq!(read, data);
}

// ===========================================================================
// Delete operations
// ===========================================================================

#[test]
fn test_delete_file() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    vol.create_file("/deleteme.txt", b"temporary data")
        .expect("create");
    assert_eq!(vol.read_root_directory().unwrap().len(), 1);

    vol.delete("/deleteme.txt").expect("delete");
    assert_eq!(vol.read_root_directory().unwrap().len(), 0);
}

#[test]
fn test_delete_empty_directory() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    vol.create_directory("/emptydir").expect("mkdir");
    vol.delete("/emptydir").expect("delete empty dir");
    assert_eq!(vol.read_root_directory().unwrap().len(), 0);
}

#[test]
fn test_delete_nonexistent_fails() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    assert!(vol.delete("/nonexistent.txt").is_err());
}

#[test]
fn test_delete_recursive() {
    let cursor = create_test_image(4);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    vol.create_directory("/parent").expect("mkdir");
    vol.create_directory("/parent/child").expect("mkdir child");
    vol.create_file("/parent/file.txt", b"data")
        .expect("create file");
    vol.create_file("/parent/child/nested.txt", b"nested")
        .expect("create nested");

    vol.delete_recursive("/parent").expect("delete recursive");
    assert_eq!(vol.read_root_directory().unwrap().len(), 0);
}

#[test]
fn test_delete_frees_clusters() {
    let cursor = create_test_image(4);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    let stats_before = vol.stats().expect("stats");
    let data = vec![0u8; 65536]; // 4 clusters
    vol.create_file("/temp.bin", &data).expect("create");

    let stats_during = vol.stats().expect("stats");
    assert!(stats_during.free_clusters < stats_before.free_clusters);

    vol.delete("/temp.bin").expect("delete");
    let stats_after = vol.stats().expect("stats");
    assert_eq!(stats_after.free_clusters, stats_before.free_clusters);
}

// ===========================================================================
// Rename operations
// ===========================================================================

#[test]
fn test_rename_file() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    vol.create_file("/old.txt", b"some data").expect("create");
    vol.rename("/old.txt", "new.txt").expect("rename");

    assert!(vol.resolve_path("/old.txt").is_err());
    let data = vol.read_file_by_path("/new.txt").expect("read renamed");
    assert_eq!(data, b"some data");
}

#[test]
fn test_rename_directory() {
    let cursor = create_test_image(4);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    vol.create_directory("/olddir").expect("mkdir");
    vol.create_file("/olddir/inside.txt", b"inside")
        .expect("create");

    vol.rename("/olddir", "newdir").expect("rename dir");

    assert!(vol.resolve_path("/olddir").is_err());
    let data = vol.read_file_by_path("/newdir/inside.txt").expect("read");
    assert_eq!(data, b"inside");
}

#[test]
fn test_rename_preserves_data() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    let original_data: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();
    vol.create_file("/before.bin", &original_data)
        .expect("create");
    vol.rename("/before.bin", "after.bin").expect("rename");

    let read = vol.read_file_by_path("/after.bin").expect("read");
    assert_eq!(read, original_data);
}

// ===========================================================================
// Volume stats
// ===========================================================================

#[test]
fn test_volume_stats() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    let stats = vol.stats().expect("stats");
    assert!(stats.total_clusters > 0);
    assert!(stats.free_clusters > 0);
    assert_eq!(stats.bad_clusters, 0);
    // Root directory uses 1 cluster
    assert_eq!(
        stats.total_clusters - stats.free_clusters,
        1, // just root dir
        "Only root cluster should be used on empty volume"
    );
}

#[test]
fn test_stats_after_writes() {
    let cursor = create_test_image(4);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    let initial = vol.stats().expect("stats");
    let data = vec![0u8; 16384 * 5]; // 5 clusters
    vol.create_file("/big.bin", &data).expect("create");

    let after = vol.stats().expect("stats");
    // Used clusters increased by at least 5 (file) + possibly 0 (root already counted)
    assert!(after.free_clusters < initial.free_clusters);
}

// ===========================================================================
// Filename validation and edge cases
// ===========================================================================

#[test]
fn test_filename_too_long() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    let long_name = "/".to_string() + &"a".repeat(50); // >42 chars
    assert!(vol.create_file(&long_name, b"data").is_err());
}

#[test]
fn test_filename_max_length() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    let name = "/".to_string() + &"x".repeat(42); // exactly 42 chars
    vol.create_file(&name, b"data")
        .expect("create 42-char name");

    let entries = vol.read_root_directory().expect("readdir");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].filename().len(), 42);
}

#[test]
fn test_case_insensitive_lookup() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    vol.create_file("/Hello.TXT", b"data").expect("create");

    // These should all resolve (FATX is case-insensitive for lookup)
    assert!(vol.resolve_path("/Hello.TXT").is_ok());
    assert!(vol.resolve_path("/hello.txt").is_ok());
    assert!(vol.resolve_path("/HELLO.TXT").is_ok());
}

#[test]
fn test_resolve_nonexistent_path() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    assert!(vol.resolve_path("/does_not_exist").is_err());
    assert!(vol.resolve_path("/a/b/c").is_err());
}

// ===========================================================================
// Timestamp encoding/decoding
// ===========================================================================

#[test]
fn test_timestamp_roundtrip() {
    let date = DirectoryEntry::encode_date(2024, 3, 15);
    let (y, m, d) = DirectoryEntry::decode_date(date);
    assert_eq!((y, m, d), (2024, 3, 15));

    let time = DirectoryEntry::encode_time(14, 30, 22);
    let (h, min, s) = DirectoryEntry::decode_time(time);
    assert_eq!((h, min, s), (14, 30, 22));
}

#[test]
fn test_timestamp_boundary_values() {
    // Minimum date: 1980-01-01
    let date = DirectoryEntry::encode_date(1980, 1, 1);
    let (y, m, d) = DirectoryEntry::decode_date(date);
    assert_eq!((y, m, d), (1980, 1, 1));

    // Midnight
    let time = DirectoryEntry::encode_time(0, 0, 0);
    let (h, min, s) = DirectoryEntry::decode_time(time);
    assert_eq!((h, min, s), (0, 0, 0));

    // End of day
    let time = DirectoryEntry::encode_time(23, 59, 58);
    let (h, min, s) = DirectoryEntry::decode_time(time);
    assert_eq!((h, min, s), (23, 59, 58));
}

// ===========================================================================
// XTAF (Xbox 360) format tests
// ===========================================================================

#[test]
fn test_xtaf_create_and_read_file() {
    let cursor = create_xtaf_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open XTAF");

    let data = b"Xbox 360 XTAF test data!";
    vol.create_file("/test360.txt", data).expect("create");

    let read = vol.read_file_by_path("/test360.txt").expect("read");
    assert_eq!(read, data);
}

#[test]
fn test_xtaf_directory_operations() {
    let cursor = create_xtaf_image(4);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open XTAF");

    vol.create_directory("/Content").expect("mkdir");
    vol.create_file("/Content/game.bin", b"game data")
        .expect("create");

    let cluster = vol.resolve_path("/Content").unwrap().first_cluster;
    let entries = vol.read_directory(cluster).expect("readdir");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].filename(), "game.bin");
}

#[test]
fn test_xtaf_delete_and_stats() {
    let cursor = create_xtaf_image(4);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open XTAF");

    let stats_before = vol.stats().expect("stats");
    vol.create_file("/temp.bin", &vec![0u8; 32768])
        .expect("create");
    vol.delete("/temp.bin").expect("delete");

    let stats_after = vol.stats().expect("stats");
    assert_eq!(stats_after.free_clusters, stats_before.free_clusters);
}

// ===========================================================================
// Stress / fill tests
// ===========================================================================

#[test]
fn test_fill_root_directory() {
    // Fill root dir with many small files — tests directory entry packing
    let cursor = create_test_image(4);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    // Each dir entry is 64 bytes. One 16 KB cluster fits 256 entries.
    // Create 100 files — well within one cluster.
    for i in 0..100 {
        let name = format!("/f{:04}.dat", i);
        vol.create_file(&name, &[i as u8; 4]).expect("create");
    }

    let entries = vol.read_root_directory().expect("readdir");
    assert_eq!(entries.len(), 100);

    // Verify a sampling of files
    for i in [0, 49, 99] {
        let name = format!("/f{:04}.dat", i);
        let data = vol.read_file_by_path(&name).expect("read");
        assert_eq!(data, vec![i as u8; 4]);
    }
}

#[test]
fn test_create_delete_create_reuses_space() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    // Fill and delete repeatedly
    for _ in 0..5 {
        vol.create_file("/cycle.bin", &vec![0xAA; 16384])
            .expect("create");
        vol.delete("/cycle.bin").expect("delete");
    }

    // Should still have space — clusters were freed each time
    let stats = vol.stats().expect("stats");
    assert!(stats.free_clusters > 0);

    // Final create should succeed
    vol.create_file("/final.bin", b"still works")
        .expect("create final");
    let data = vol.read_file_by_path("/final.bin").expect("read");
    assert_eq!(data, b"still works");
}

// ===========================================================================
// FAT chain operations
// ===========================================================================

#[test]
fn test_chain_allocation_and_read() {
    let cursor = create_test_image(4);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    // Create a file that spans 3 clusters (48 KB)
    let data = vec![0xBB; 16384 * 3];
    vol.create_file("/chain.bin", &data).expect("create");

    // Read back and verify
    let entry = vol.resolve_path("/chain.bin").expect("resolve");
    let chain = vol.read_chain(entry.first_cluster).expect("read chain");
    assert_eq!(chain.len(), 3, "Should be exactly 3 clusters in chain");

    let read = vol.read_file(&entry).expect("read file");
    assert_eq!(read, data);
}

// ===========================================================================
// Flush / persistence
// ===========================================================================

#[test]
fn test_flush_persists_fat() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    vol.create_file("/persist.txt", b"persistent data")
        .expect("create");
    vol.flush().expect("flush");

    // The FAT should be dirty=false after flush
    // We can verify by reading the file back (it depends on the FAT being correct)
    let data = vol.read_file_by_path("/persist.txt").expect("read");
    assert_eq!(data, b"persistent data");
}

// ===========================================================================
// Deep recursive directory deletion (1–10 layers)
// ===========================================================================

/// Regression test for Finder directory deletion.
/// Finder sends a single NFS remove for a non-empty directory. The NFS layer
/// falls back to delete_recursive, which must handle arbitrarily deep trees.
/// This test builds nested directories from 1 to 10 levels deep, each with a
/// file at every level, then verifies delete_recursive removes everything and
/// frees all clusters.
#[test]
fn test_delete_recursive_deep_1_to_10_layers() {
    // Use a larger image — deep trees with files at every level need space
    let cursor = create_test_image(8);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    for depth in 1..=10 {
        // Build the nested path: /d1/d2/d3/.../dN
        let mut dir_path = String::new();
        for level in 1..=depth {
            dir_path.push_str(&format!("/d{}", level));
            vol.create_directory(&dir_path)
                .unwrap_or_else(|_| panic!("mkdir {} (depth={})", dir_path, depth));

            // Put a file at every level so delete_recursive must handle mixed content
            let file_path = format!("{}/f{}.bin", dir_path, level);
            let data = vec![level as u8; 256]; // small file with recognizable content
            vol.create_file(&file_path, &data)
                .unwrap_or_else(|_| panic!("create {} (depth={})", file_path, depth));
        }

        // Snapshot free clusters before delete
        let stats_before = vol.stats().expect("stats before delete");

        // delete_recursive on the root of the tree
        vol.delete_recursive("/d1")
            .unwrap_or_else(|_| panic!("delete_recursive depth={}", depth));

        // Root directory should be empty
        let root = vol.read_root_directory().expect("read root");
        assert!(
            root.is_empty(),
            "root not empty after delete_recursive depth={}",
            depth
        );

        // All clusters should be freed
        let stats_after = vol.stats().expect("stats after delete");
        assert!(
            stats_after.free_clusters > stats_before.free_clusters,
            "clusters not freed after delete_recursive depth={}: before={}, after={}",
            depth,
            stats_before.free_clusters,
            stats_after.free_clusters
        );

        // Verify the deepest path is truly gone
        assert!(
            vol.resolve_path("/d1").is_err(),
            "/d1 still exists after delete_recursive depth={}",
            depth
        );
    }
}

/// Verify that delete_recursive on a tree with wide + deep branches works.
/// Simulates an Xbox game content directory: a parent with multiple subdirs,
/// each containing files.
#[test]
fn test_delete_recursive_wide_and_deep() {
    let cursor = create_test_image(8);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    // Create structure:
    //   /game/
    //     save1/ -> data.bin
    //     save2/ -> data.bin, meta.txt
    //     save3/
    //       sub/ -> deep.bin
    vol.create_directory("/game").expect("mkdir game");

    vol.create_directory("/game/save1").expect("mkdir save1");
    vol.create_file("/game/save1/data.bin", &[0xAA; 1024])
        .expect("create save1/data.bin");

    vol.create_directory("/game/save2").expect("mkdir save2");
    vol.create_file("/game/save2/data.bin", &[0xBB; 2048])
        .expect("create save2/data.bin");
    vol.create_file("/game/save2/meta.txt", b"save metadata")
        .expect("create save2/meta.txt");

    vol.create_directory("/game/save3").expect("mkdir save3");
    vol.create_directory("/game/save3/sub")
        .expect("mkdir save3/sub");
    vol.create_file("/game/save3/sub/deep.bin", &[0xCC; 512])
        .expect("create save3/sub/deep.bin");

    let stats_before = vol.stats().expect("stats");

    vol.delete_recursive("/game")
        .expect("delete_recursive /game");

    // Everything should be gone
    assert!(vol.resolve_path("/game").is_err());
    assert!(vol.read_root_directory().unwrap().is_empty());

    // Clusters freed
    let stats_after = vol.stats().expect("stats");
    assert!(stats_after.free_clusters > stats_before.free_clusters);
}

// ===========================================================================
// In-place file writes (write_file_in_place)
// ===========================================================================

/// Basic in-place write: overwrite a file with same-size data.
/// No cluster allocation or freeing needed — pure data overwrite.
#[test]
fn test_write_in_place_same_size() {
    let cursor = create_test_image(4);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    vol.create_file("/test.bin", &[0xAA; 1024]).expect("create");
    let stats_before = vol.stats().expect("stats");

    vol.write_file_in_place("/test.bin", &[0xBB; 1024])
        .expect("write in-place");

    let data = vol.read_file_by_path("/test.bin").expect("read");
    assert_eq!(data, vec![0xBB; 1024]);

    // No cluster changes — free count should be the same
    let stats_after = vol.stats().expect("stats");
    assert_eq!(stats_after.free_clusters, stats_before.free_clusters);
}

/// In-place write where file grows — must extend the cluster chain.
#[test]
fn test_write_in_place_file_grows() {
    let cursor = create_test_image(4);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    // Create a small file (1 cluster)
    vol.create_file("/grow.bin", &[0x11; 100]).expect("create");
    let stats_small = vol.stats().expect("stats");

    // Write much larger data (multiple clusters)
    let big_data = vec![0x22; 50000];
    vol.write_file_in_place("/grow.bin", &big_data)
        .expect("write in-place grow");

    let read_back = vol.read_file_by_path("/grow.bin").expect("read");
    assert_eq!(read_back, big_data);

    // Should have used more clusters
    let stats_big = vol.stats().expect("stats");
    assert!(stats_big.free_clusters < stats_small.free_clusters);
}

/// In-place write where file shrinks — must free excess clusters.
#[test]
fn test_write_in_place_file_shrinks() {
    let cursor = create_test_image(4);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    // Create a large file (multiple clusters)
    let big_data = vec![0x33; 50000];
    vol.create_file("/shrink.bin", &big_data).expect("create");
    let stats_big = vol.stats().expect("stats");

    // Overwrite with small data
    vol.write_file_in_place("/shrink.bin", &[0x44; 100])
        .expect("write in-place shrink");

    let read_back = vol.read_file_by_path("/shrink.bin").expect("read");
    assert_eq!(read_back, vec![0x44; 100]);

    // Should have freed clusters
    let stats_small = vol.stats().expect("stats");
    assert!(stats_small.free_clusters > stats_big.free_clusters);
}

/// In-place write updates the directory entry file_size correctly.
#[test]
fn test_write_in_place_updates_dirent_size() {
    let cursor = create_test_image(4);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    vol.create_file("/size.bin", &[0x55; 1000]).expect("create");

    // Verify initial size
    let entry = vol.resolve_path("/size.bin").expect("resolve");
    assert_eq!(entry.file_size, 1000);

    // Write larger data
    vol.write_file_in_place("/size.bin", &[0x66; 5000])
        .expect("write in-place");

    let entry = vol.resolve_path("/size.bin").expect("resolve after");
    assert_eq!(entry.file_size, 5000);

    // Write smaller data
    vol.write_file_in_place("/size.bin", &[0x77; 200])
        .expect("write in-place shrink");

    let entry = vol.resolve_path("/size.bin").expect("resolve after shrink");
    assert_eq!(entry.file_size, 200);
}

/// Multiple in-place writes simulate the NFS flush cycle:
/// write, flush, write more, flush again.
#[test]
fn test_write_in_place_repeated_overwrites() {
    let cursor = create_test_image(4);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    vol.create_file("/cycle.bin", &[0x00; 100]).expect("create");

    // Simulate 10 flush cycles with growing data (like NFS buffered writes)
    for i in 1..=10u8 {
        let size = (i as usize) * 1000;
        let data = vec![i; size];
        vol.write_file_in_place("/cycle.bin", &data)
            .expect(&format!("write cycle {}", i));

        let read_back = vol.read_file_by_path("/cycle.bin").expect("read");
        assert_eq!(read_back.len(), size);
        assert_eq!(read_back[0], i);
        assert_eq!(read_back[size - 1], i);
    }
}

/// In-place write on a file in a subdirectory.
#[test]
fn test_write_in_place_subdirectory() {
    let cursor = create_test_image(4);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    vol.create_directory("/Content").expect("mkdir");
    vol.create_directory("/Content/Game").expect("mkdir game");
    vol.create_file("/Content/Game/save.dat", &[0xAA; 2048])
        .expect("create");

    vol.write_file_in_place("/Content/Game/save.dat", &[0xBB; 4096])
        .expect("write in-place subdir");

    let data = vol
        .read_file_by_path("/Content/Game/save.dat")
        .expect("read");
    assert_eq!(data, vec![0xBB; 4096]);
}

/// In-place write on nonexistent file should return FileNotFound.
#[test]
fn test_write_in_place_nonexistent_fails() {
    let cursor = create_test_image(2);
    let mut vol = FatxVolume::open(cursor, 0, 0).expect("open");

    let result = vol.write_file_in_place("/nope.bin", &[0xFF; 100]);
    assert!(result.is_err());
}
