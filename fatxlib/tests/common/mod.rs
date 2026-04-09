//! Shared test fixture helpers for fatxlib integration tests.
//!
//! Generates temporary FATX/XTAF images by invoking the `fatx-mkimage` binary.
//! Each helper returns a `TempDir` (auto-deletes on drop) and an opened `FatxVolume<File>`.

use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::process::Command;

use fatxlib::volume::FatxVolume;
use tempfile::TempDir;

/// Find the fatx-mkimage binary. Uses the workspace target directory.
fn mkimage_bin() -> PathBuf {
    // Walk up from the manifest dir to find the workspace root
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // fatxlib/Cargo.toml -> go up one level to workspace root
    dir.pop();

    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };

    let bin = dir.join("target").join(profile).join("fatx-mkimage");
    if !bin.exists() {
        // Build it
        let status = Command::new("cargo")
            .args(["build", "-p", "fatx-mkimage"])
            .current_dir(&dir)
            .status()
            .expect("failed to run cargo build for fatx-mkimage");
        assert!(status.success(), "failed to build fatx-mkimage");
    }
    assert!(bin.exists(), "fatx-mkimage binary not found at {:?}", bin);
    bin
}

/// Create a temporary FATX/XTAF image and open it as a FatxVolume.
///
/// # Arguments
/// - `size_mb`: Image size in megabytes
/// - `format`: "fatx" (little-endian) or "xtaf" (big-endian, Xbox 360)
/// - `populate`: If true, populate with sample Xbox-like directory structure
///
/// # Returns
/// `(TempDir, FatxVolume<File>)` — TempDir must be kept alive for the duration of the test.
pub fn create_image(size_mb: u32, format: &str, populate: bool) -> (TempDir, FatxVolume<File>) {
    let tmp_dir = TempDir::new().expect("create temp dir");
    let img_path = tmp_dir.path().join("test.img");

    let mut args = vec![
        img_path.to_str().unwrap().to_string(),
        "--size".to_string(),
        format!("{}M", size_mb),
        "--format".to_string(),
        format.to_string(),
        "--force".to_string(),
    ];
    if populate {
        args.push("--populate".to_string());
    }

    let output = Command::new(mkimage_bin())
        .args(&args)
        .output()
        .expect("failed to run fatx-mkimage");

    assert!(
        output.status.success(),
        "fatx-mkimage failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&img_path)
        .expect("open generated image");

    let vol = FatxVolume::open(file, 0, 0).expect("open FATX volume from generated image");

    (tmp_dir, vol)
}

/// Create a blank FATX (little-endian, original Xbox) image.
pub fn create_fatx_image(size_mb: u32) -> (TempDir, FatxVolume<File>) {
    create_image(size_mb, "fatx", false)
}

/// Create a blank XTAF (big-endian, Xbox 360) image.
pub fn create_xtaf_image(size_mb: u32) -> (TempDir, FatxVolume<File>) {
    create_image(size_mb, "xtaf", false)
}

/// Create a FATX image populated with sample Xbox-like content.
pub fn create_populated_image(size_mb: u32) -> (TempDir, FatxVolume<File>) {
    create_image(size_mb, "fatx", true)
}

/// Create a populated XTAF image.
pub fn create_populated_xtaf_image(size_mb: u32) -> (TempDir, FatxVolume<File>) {
    create_image(size_mb, "xtaf", true)
}

/// Get just the image file path from a TempDir (for CLI tests that need the path).
pub fn image_path(tmp_dir: &TempDir) -> PathBuf {
    tmp_dir.path().join("test.img")
}
