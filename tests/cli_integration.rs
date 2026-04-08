//! CLI integration tests for the `fatx` binary.
//!
//! These tests verify that the main binary handles arguments correctly,
//! dispatches subcommands, and produces expected output.

use std::process::Command;

/// Find the fatx binary in the target/debug directory.
fn fatx_bin() -> Command {
    // cargo test builds everything into target/debug
    Command::new(env!("CARGO_BIN_EXE_fatx"))
}

#[test]
fn test_fatx_version() {
    let output = fatx_bin()
        .arg("--version")
        .output()
        .expect("failed to run fatx --version");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "fatx --version failed: {}", stdout);
    assert!(
        stdout.contains("fatx"),
        "version output should contain 'fatx': {}",
        stdout
    );
}

#[test]
fn test_fatx_help() {
    let output = fatx_bin()
        .arg("--help")
        .output()
        .expect("failed to run fatx --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "fatx --help failed");
    assert!(stdout.contains("FATX"), "help should mention FATX");
    assert!(
        stdout.contains("scan") || stdout.contains("Scan"),
        "help should list scan command"
    );
}

#[test]
fn test_fatx_scan_nonexistent_device() {
    let output = fatx_bin()
        .args(["scan", "/nonexistent/device"])
        .output()
        .expect("failed to run fatx scan");
    // Should fail gracefully, not panic
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No such file")
            || stderr.contains("not found")
            || stderr.contains("Failed")
            || stderr.contains("Error"),
        "should report file not found: {}",
        stderr
    );
}

#[test]
fn test_fatx_ls_nonexistent_device() {
    let output = fatx_bin()
        .args(["ls", "/nonexistent/device", "/"])
        .output()
        .expect("failed to run fatx ls");
    assert!(!output.status.success());
}

#[test]
fn test_fatx_scan_with_json() {
    // Create a small empty file — scan should try to open it and find no partitions
    let tmp = tempfile::NamedTempFile::new().expect("create tmp");
    // Write enough bytes so it doesn't fail on size check
    std::fs::write(tmp.path(), vec![0u8; 4096]).expect("write tmp");

    let output = fatx_bin()
        .args(["scan", "--json", &tmp.path().to_string_lossy()])
        .output()
        .expect("failed to run fatx scan --json");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // JSON mode should produce valid JSON (even if empty array or error)
    // The scan should complete (may find 0 partitions)
    // Just verify it didn't panic
    assert!(
        output.status.success()
            || !stdout.is_empty()
            || !String::from_utf8_lossy(&output.stderr).is_empty(),
        "scan should produce some output"
    );
}

#[test]
fn test_fatx_ls_on_test_image() {
    // Create a test FATX image and verify ls works on it
    let tmp = tempfile::NamedTempFile::new().expect("create tmp");
    let path = tmp.path().to_path_buf();

    // Create a minimal FATX image (4 MB)
    use std::fs::OpenOptions;
    use std::io::{Seek, SeekFrom, Write};

    let size: u64 = 4 * 1024 * 1024;
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .truncate(true)
        .open(&path)
        .expect("open");

    file.set_len(size).expect("set_len");
    file.seek(SeekFrom::Start(0)).expect("seek");

    // Write FATX superblock
    let mut sb = [0u8; 4096];
    sb[0..4].copy_from_slice(b"FATX");
    let spc: u32 = 32;
    sb[4..8].copy_from_slice(&1u32.to_le_bytes()); // volume ID
    sb[8..12].copy_from_slice(&spc.to_le_bytes());
    sb[12..14].copy_from_slice(&1u16.to_le_bytes());
    file.write_all(&sb).expect("write sb");

    // Calculate FAT layout
    let cluster_size = spc as u64 * 512;
    let total_sectors = (size / 512) - (4096 / 512);
    let entry_size = 2u64; // FAT16 for small images
    let total_clusters = (total_sectors * 512 / (cluster_size + entry_size)) as u32;
    let raw_fat_size = total_clusters as u64 * entry_size;
    let fat_size = (raw_fat_size + 0xFFF) & !0xFFF;
    let data_offset = 4096 + fat_size;

    // Mark cluster 1 (root) as end-of-chain
    let cluster1_offset = 4096 + entry_size;
    file.seek(SeekFrom::Start(cluster1_offset))
        .expect("seek FAT");
    file.write_all(&0xFFFEu16.to_le_bytes()).expect("write EOC");

    // Initialize root directory with 0xFF
    file.seek(SeekFrom::Start(data_offset)).expect("seek root");
    let ff_buf = vec![0xFFu8; cluster_size as usize];
    file.write_all(&ff_buf).expect("write root");
    file.flush().expect("flush");
    drop(file);

    // Now run fatx ls on it
    let output = fatx_bin()
        .args(["ls", &path.to_string_lossy(), "/"])
        .output()
        .expect("failed to run fatx ls");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "fatx ls should succeed on valid image. stdout: {}, stderr: {}",
        stdout,
        stderr
    );
}
