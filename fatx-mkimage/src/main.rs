//! fatx-mkimage: Create blank FATX/XTAF disk images for testing.
//!
//! Generates a file-backed FATX volume that can be used with fatx
//! and fatx-mount without needing a real Xbox hard drive.
//!
//! Usage:
//!   fatx-mkimage test.img --size 1G
//!   fatx-mkimage test.img --size 1G --format xtaf --populate
//!   fatx-mount test.img -v          # mount it in Finder

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;

use clap::Parser;
use fatxlib::types::*;
use fatxlib::volume::FatxVolume;
use rand::Rng;

/// Minimum image size: 2 MB (enough for superblock + FAT + a few clusters)
const MIN_SIZE: u64 = 2 * 1024 * 1024;

#[derive(Parser)]
#[command(
    name = "fatx-mkimage",
    about = "Create blank FATX/XTAF disk images for testing",
    version
)]
struct Cli {
    /// Output image file path
    output: PathBuf,

    /// Image size (e.g. "1G", "512M", "2G")
    #[arg(long, default_value = "1G")]
    size: String,

    /// Format: "fatx" (original Xbox, little-endian) or "xtaf" (Xbox 360, big-endian)
    #[arg(long, default_value = "fatx")]
    format: String,

    /// Sectors per cluster (must be power of 2, 1-128). Default: 32 (16KB clusters)
    #[arg(long, default_value = "32")]
    spc: u32,

    /// Populate with sample files and directories for testing
    #[arg(long)]
    populate: bool,

    /// Overwrite existing file without prompting
    #[arg(long, short = 'f')]
    force: bool,
}

fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num_part, multiplier) =
        if let Some(n) = s.strip_suffix('G').or_else(|| s.strip_suffix('g')) {
            (n, 1024 * 1024 * 1024u64)
        } else if let Some(n) = s.strip_suffix('M').or_else(|| s.strip_suffix('m')) {
            (n, 1024 * 1024u64)
        } else if let Some(n) = s.strip_suffix('K').or_else(|| s.strip_suffix('k')) {
            (n, 1024u64)
        } else {
            (s, 1u64)
        };

    let num: f64 = num_part
        .parse()
        .map_err(|_| format!("invalid size: '{}'", s))?;
    Ok((num * multiplier as f64) as u64)
}

/// Write a properly formatted FATX/XTAF superblock + FAT + empty root directory.
///
/// This replicates the exact layout that FatxVolume::open() expects:
///   [0x0000] Superblock (4 KB)
///   [0x1000] FAT table (rounded to 4 KB)
///   [FAT end] Data area — cluster 1 = root directory
fn format_image(file: &mut File, size: u64, is_xtaf: bool, spc: u32) -> Result<(), String> {
    // Extend the file to the desired size (sparse on most filesystems)
    file.set_len(size).map_err(|e| format!("set_len: {}", e))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|e| format!("seek: {}", e))?;

    // -- Superblock --
    let mut sb = [0u8; SUPERBLOCK_SIZE as usize];
    if is_xtaf {
        sb[0..4].copy_from_slice(b"XTAF");
    } else {
        sb[0..4].copy_from_slice(b"FATX");
    }

    let volume_id: u32 = rand::thread_rng().gen();

    if is_xtaf {
        sb[4..8].copy_from_slice(&volume_id.to_be_bytes());
        sb[8..12].copy_from_slice(&spc.to_be_bytes());
        sb[12..14].copy_from_slice(&1u16.to_be_bytes());
    } else {
        sb[4..8].copy_from_slice(&volume_id.to_le_bytes());
        sb[8..12].copy_from_slice(&spc.to_le_bytes());
        sb[12..14].copy_from_slice(&1u16.to_le_bytes());
    }

    file.write_all(&sb)
        .map_err(|e| format!("write sb: {}", e))?;

    // -- Calculate layout (same formulas as volume.rs) --
    let cluster_size = spc as u64 * SECTOR_SIZE;
    let total_sectors = (size / SECTOR_SIZE) - (SUPERBLOCK_SIZE / SECTOR_SIZE);

    let total_clusters = if is_xtaf {
        ((size - SUPERBLOCK_SIZE) / cluster_size) as u32
    } else {
        let entry_size_est = if total_sectors.saturating_sub(260) / spc as u64 >= 65_525 {
            4u64
        } else {
            2u64
        };
        (total_sectors * SECTOR_SIZE / (cluster_size + entry_size_est)) as u32
    };

    let fat_type = if total_sectors.saturating_sub(260) / spc as u64 >= 65_525 {
        FatType::Fat32
    } else {
        FatType::Fat16
    };
    let entry_size = fat_type.entry_size();

    let raw_fat_size = total_clusters as u64 * entry_size;
    let fat_size = (raw_fat_size + 0xFFF) & !0xFFF;
    let data_offset = SUPERBLOCK_SIZE + fat_size;

    println!(
        "  Layout: {} clusters, {} FAT, cluster_size={}, data starts at 0x{:X}",
        total_clusters,
        fat_type,
        format_bytes(cluster_size),
        data_offset,
    );

    // -- Write FAT --
    // We need to mark cluster 1 (root directory) as end-of-chain.
    // The rest of the FAT is already zero (free) from set_len.
    let fat_abs = SUPERBLOCK_SIZE;
    let cluster1_offset = fat_abs + entry_size; // cluster 1 entry

    file.seek(SeekFrom::Start(cluster1_offset))
        .map_err(|e| format!("seek FAT: {}", e))?;

    match fat_type {
        FatType::Fat16 => {
            let eoc = if is_xtaf {
                FAT16_EOC.to_be_bytes()
            } else {
                FAT16_EOC.to_le_bytes()
            };
            file.write_all(&eoc)
                .map_err(|e| format!("write FAT16 EOC: {}", e))?;
        }
        FatType::Fat32 => {
            let eoc = if is_xtaf {
                FAT32_EOC.to_be_bytes()
            } else {
                FAT32_EOC.to_le_bytes()
            };
            file.write_all(&eoc)
                .map_err(|e| format!("write FAT32 EOC: {}", e))?;
        }
    }

    // -- Initialize root directory cluster (cluster 1) with 0xFF --
    // 0xFF in the first byte of a directory entry = end-of-directory marker
    let root_offset = data_offset; // cluster 1 = first cluster in data area
    file.seek(SeekFrom::Start(root_offset))
        .map_err(|e| format!("seek root: {}", e))?;

    let ff_buf = vec![0xFFu8; cluster_size as usize];
    file.write_all(&ff_buf)
        .map_err(|e| format!("write root dir: {}", e))?;

    file.flush().map_err(|e| format!("flush: {}", e))?;

    Ok(())
}

/// Populate the image with sample Xbox-like directory structure and files.
fn populate_image(path: &PathBuf) -> Result<(), String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("reopen: {}", e))?;

    let mut vol =
        FatxVolume::open(file, 0, 0).map_err(|e| format!("open volume for populate: {}", e))?;

    println!("  Populating with sample content...");

    // Create Xbox-like directory structure
    vol.create_directory("/Content")
        .map_err(|e| format!("mkdir Content: {}", e))?;
    vol.create_directory("/Content/0000000000000000")
        .map_err(|e| format!("mkdir profile: {}", e))?;
    vol.create_directory("/Cache")
        .map_err(|e| format!("mkdir Cache: {}", e))?;
    vol.create_directory("/Apps")
        .map_err(|e| format!("mkdir Apps: {}", e))?;
    vol.create_directory("/Apps/Aurora")
        .map_err(|e| format!("mkdir Aurora: {}", e))?;

    // Create some test files of various sizes
    vol.create_file("/name.txt", b"Test Xbox 360\n")
        .map_err(|e| format!("create name.txt: {}", e))?;
    vol.create_file("/launch.ini", b"[QuickLaunch]\nDefault = Aurora\n")
        .map_err(|e| format!("create launch.ini: {}", e))?;

    // Create a medium-sized file (64 KB) to test multi-cluster reads
    let medium_data: Vec<u8> = (0..65536u32).map(|i| (i % 256) as u8).collect();
    vol.create_file("/Apps/Aurora/config.bin", &medium_data)
        .map_err(|e| format!("create config.bin: {}", e))?;

    // Create a larger file (1 MB) to benchmark read performance
    let large_data: Vec<u8> = (0..1_048_576u32).map(|i| (i % 256) as u8).collect();
    vol.create_file("/Content/testfile_1mb.bin", &large_data)
        .map_err(|e| format!("create testfile_1mb.bin: {}", e))?;

    // Create several small files in a directory to test readdir performance
    vol.create_directory("/Content/0000000000000000/FFFE07D1")
        .map_err(|e| format!("mkdir game title: {}", e))?;
    for i in 0..20 {
        let name = format!("/Content/0000000000000000/FFFE07D1/save_{:03}.dat", i);
        let data = format!("Save game data #{}\n", i);
        vol.create_file(&name, data.as_bytes())
            .map_err(|e| format!("create {}: {}", name, e))?;
    }

    vol.flush().map_err(|e| format!("flush: {}", e))?;

    let stats = vol.stats().map_err(|e| format!("stats: {}", e))?;
    println!(
        "  Populated: {} total clusters, {} free ({} used)",
        stats.total_clusters,
        stats.free_clusters,
        stats.total_clusters - stats.free_clusters
    );

    Ok(())
}

fn format_bytes(n: u64) -> String {
    if n >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", n as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{} B", n)
    }
}

fn main() {
    let cli = Cli::parse();

    let size = parse_size(&cli.size).unwrap_or_else(|e| {
        eprintln!("Invalid size '{}': {}", cli.size, e);
        std::process::exit(1);
    });

    if size < MIN_SIZE {
        eprintln!(
            "Image size {} is too small (minimum {})",
            format_bytes(size),
            format_bytes(MIN_SIZE)
        );
        std::process::exit(1);
    }

    let is_xtaf = match cli.format.to_lowercase().as_str() {
        "fatx" | "xbox" => false,
        "xtaf" | "360" | "xbox360" => true,
        other => {
            eprintln!("Unknown format '{}'. Use 'fatx' or 'xtaf'.", other);
            std::process::exit(1);
        }
    };

    if !cli.spc.is_power_of_two() || cli.spc == 0 || cli.spc > 128 {
        eprintln!(
            "Sectors per cluster must be a power of 2 between 1 and 128, got {}",
            cli.spc
        );
        std::process::exit(1);
    }

    if cli.output.exists() && !cli.force {
        eprintln!(
            "Output file '{}' already exists. Use --force to overwrite.",
            cli.output.display()
        );
        std::process::exit(1);
    }

    let format_name = if is_xtaf {
        "XTAF (Xbox 360)"
    } else {
        "FATX (original Xbox)"
    };
    println!(
        "Creating {} image: {} {}",
        format_name,
        cli.output.display(),
        format_bytes(size),
    );

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .truncate(true)
        .open(&cli.output)
        .unwrap_or_else(|e| {
            eprintln!("Failed to create '{}': {}", cli.output.display(), e);
            std::process::exit(1);
        });

    format_image(&mut file, size, is_xtaf, cli.spc).unwrap_or_else(|e| {
        eprintln!("Format failed: {}", e);
        std::process::exit(1);
    });

    // Verify by opening with fatxlib
    drop(file);
    let verify_file = File::open(&cli.output).unwrap_or_else(|e| {
        eprintln!("Failed to reopen for verification: {}", e);
        std::process::exit(1);
    });
    let vol = FatxVolume::open(verify_file, 0, 0).unwrap_or_else(|e| {
        eprintln!(
            "Verification FAILED — image is not a valid FATX volume: {}",
            e
        );
        std::process::exit(1);
    });
    let magic_str = std::str::from_utf8(&vol.superblock.magic).unwrap_or("????");
    println!(
        "  Verified: {} volume, {} clusters, {} FAT",
        magic_str, vol.total_clusters, vol.fat_type,
    );
    drop(vol);

    if cli.populate {
        populate_image(&cli.output).unwrap_or_else(|e| {
            eprintln!("Populate failed: {}", e);
            std::process::exit(1);
        });
    }

    println!("Done! Test with:");
    println!("  fatx ls {} /", cli.output.display());
    println!("  sudo fatx-mount {} -v", cli.output.display());
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    // ── parse_size tests ──

    #[test]
    fn test_parse_size_gigabytes() {
        assert_eq!(parse_size("1G").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_size("2g").unwrap(), 2 * 1024 * 1024 * 1024);
    }

    #[test]
    fn test_parse_size_megabytes() {
        assert_eq!(parse_size("512M").unwrap(), 512 * 1024 * 1024);
        assert_eq!(parse_size("64m").unwrap(), 64 * 1024 * 1024);
    }

    #[test]
    fn test_parse_size_kilobytes() {
        assert_eq!(parse_size("1024K").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("256k").unwrap(), 256 * 1024);
    }

    #[test]
    fn test_parse_size_bytes() {
        assert_eq!(parse_size("4096").unwrap(), 4096);
    }

    #[test]
    fn test_parse_size_fractional() {
        assert_eq!(
            parse_size("1.5G").unwrap(),
            (1.5 * 1024.0 * 1024.0 * 1024.0) as u64
        );
        assert_eq!(parse_size("0.5M").unwrap(), 512 * 1024);
    }

    #[test]
    fn test_parse_size_invalid() {
        assert!(parse_size("abc").is_err());
        assert!(parse_size("").is_err());
        assert!(parse_size("G").is_err());
    }

    #[test]
    fn test_parse_size_whitespace() {
        assert_eq!(parse_size("  1G  ").unwrap(), 1024 * 1024 * 1024);
    }

    // ── format_bytes tests ──

    #[test]
    fn test_format_bytes_display() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GB");
    }

    // ── format_image tests ──

    #[test]
    fn test_format_fatx_image() {
        let tmp = NamedTempFile::new().expect("create tmp");
        let path = tmp.path().to_path_buf();
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(true)
            .open(&path)
            .expect("open");

        let size = 8 * 1024 * 1024; // 8 MB
        format_image(&mut file, size, false, 32).expect("format FATX");

        // Verify the image is a valid FATX volume
        drop(file);
        let f = File::open(&path).expect("reopen");
        let vol = FatxVolume::open(f, 0, 0).expect("open as FATX volume");
        assert_eq!(&vol.superblock.magic, b"FATX");
    }

    #[test]
    fn test_format_xtaf_image() {
        let tmp = NamedTempFile::new().expect("create tmp");
        let path = tmp.path().to_path_buf();
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(true)
            .open(&path)
            .expect("open");

        let size = 8 * 1024 * 1024; // 8 MB
        format_image(&mut file, size, true, 32).expect("format XTAF");

        drop(file);
        let f = File::open(&path).expect("reopen");
        let vol = FatxVolume::open(f, 0, 0).expect("open as XTAF volume");
        assert_eq!(&vol.superblock.magic, b"XTAF");
    }

    #[test]
    fn test_format_image_various_spc() {
        for spc in [1, 2, 4, 8, 16, 32, 64, 128] {
            let tmp = NamedTempFile::new().expect("create tmp");
            let path = tmp.path().to_path_buf();
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .read(true)
                .truncate(true)
                .open(&path)
                .expect("open");

            let size = 8 * 1024 * 1024;
            format_image(&mut file, size, false, spc).expect(&format!("format spc={}", spc));

            drop(file);
            let f = File::open(&path).expect("reopen");
            FatxVolume::open(f, 0, 0).expect(&format!("valid volume spc={}", spc));
        }
    }

    #[test]
    fn test_format_large_image_fat32() {
        // 2GB should trigger FAT32
        let tmp = NamedTempFile::new().expect("create tmp");
        let path = tmp.path().to_path_buf();
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(true)
            .open(&path)
            .expect("open");

        let size = 2u64 * 1024 * 1024 * 1024;
        format_image(&mut file, size, false, 32).expect("format 2GB");

        drop(file);
        let f = File::open(&path).expect("reopen");
        let vol = FatxVolume::open(f, 0, 0).expect("open");
        assert_eq!(vol.fat_type, fatxlib::types::FatType::Fat32);
    }

    #[test]
    fn test_format_small_image_fat16() {
        let tmp = NamedTempFile::new().expect("create tmp");
        let path = tmp.path().to_path_buf();
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(true)
            .open(&path)
            .expect("open");

        let size = 8 * 1024 * 1024; // 8 MB → FAT16
        format_image(&mut file, size, false, 32).expect("format 8MB");

        drop(file);
        let f = File::open(&path).expect("reopen");
        let vol = FatxVolume::open(f, 0, 0).expect("open");
        assert_eq!(vol.fat_type, fatxlib::types::FatType::Fat16);
    }

    // ── populate_image tests ──

    #[test]
    fn test_populate_creates_directories_and_files() {
        let tmp = NamedTempFile::new().expect("create tmp");
        let path = tmp.path().to_path_buf();
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(true)
            .open(&path)
            .expect("open");

        let size = 64 * 1024 * 1024; // 64 MB for populate
        format_image(&mut file, size, false, 32).expect("format");
        drop(file);

        populate_image(&path).expect("populate");

        // Verify the structure by opening and reading
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("reopen");
        let mut vol = FatxVolume::open(f, 0, 0).expect("open");

        // Check directories exist
        vol.resolve_path("/Content").expect("Content exists");
        vol.resolve_path("/Content/0000000000000000")
            .expect("profile exists");
        vol.resolve_path("/Cache").expect("Cache exists");
        vol.resolve_path("/Apps").expect("Apps exists");
        vol.resolve_path("/Apps/Aurora").expect("Aurora exists");

        // Check files exist and read back correctly
        let data = vol.read_file_by_path("/name.txt").expect("read name.txt");
        assert_eq!(&data, b"Test Xbox 360\n");

        // Check the medium file
        let config_entry = vol
            .resolve_path("/Apps/Aurora/config.bin")
            .expect("config.bin exists");
        assert_eq!(config_entry.file_size, 65536);

        // Check the large file
        let large_entry = vol
            .resolve_path("/Content/testfile_1mb.bin")
            .expect("1mb file exists");
        assert_eq!(large_entry.file_size, 1_048_576);

        // Check save files
        let save_dir = vol
            .resolve_path("/Content/0000000000000000/FFFE07D1")
            .expect("game dir exists");
        let entries = vol
            .read_directory(save_dir.first_cluster)
            .expect("readdir saves");
        assert_eq!(entries.len(), 20);
    }

    #[test]
    fn test_populate_xtaf_image() {
        let tmp = NamedTempFile::new().expect("create tmp");
        let path = tmp.path().to_path_buf();
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(true)
            .open(&path)
            .expect("open");

        let size = 64 * 1024 * 1024;
        format_image(&mut file, size, true, 32).expect("format XTAF");
        drop(file);

        populate_image(&path).expect("populate XTAF");

        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("reopen");
        let mut vol = FatxVolume::open(f, 0, 0).expect("open");
        assert_eq!(&vol.superblock.magic, b"XTAF");
        vol.resolve_path("/Content").expect("Content exists");
        vol.resolve_path("/Apps/Aurora").expect("Aurora exists");
    }
}
