//! Integration tests for the xdvdfs-backed XISO reader.

use std::fs::File;
use std::io::Cursor;

use fatxlib::xiso::XisoImage;

// ---------------------------------------------------------------------------
// Negative paths — always runnable
// ---------------------------------------------------------------------------

#[test]
fn rejects_obviously_non_xiso_source() {
    let buf = vec![0u8; 4096];
    let cursor = Cursor::new(buf);
    assert!(
        XisoImage::open(cursor).is_err(),
        "all-zero buffer should not parse as XDVDFS"
    );
}

#[test]
fn rejects_too_small_source() {
    // The XDVDFS volume descriptor lives at sector 32 (offset 0x10000).
    // Anything smaller than that can't possibly be valid.
    let buf = vec![0u8; 1024];
    let cursor = Cursor::new(buf);
    assert!(
        XisoImage::open(cursor).is_err(),
        "tiny buffer should be rejected"
    );
}

// ---------------------------------------------------------------------------
// Positive paths — require a fixture XISO at tests/fixtures/tiny.xiso
// ---------------------------------------------------------------------------

const FIXTURE: &str = "tests/fixtures/tiny.xiso";

fn open_fixture() -> Option<XisoImage<File>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    if !path.exists() {
        eprintln!("skipping: fixture missing at {}", path.display());
        return None;
    }
    let file = File::open(&path).expect("open fixture");
    Some(XisoImage::open(file).expect("parse fixture"))
}

#[test]
fn walks_fixture_image() {
    let Some(mut img) = open_fixture() else {
        return;
    };
    let files = img.walk_files().expect("walk");
    assert!(
        !files.is_empty(),
        "fixture should contain at least one file"
    );
    let names: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert!(
        names.iter().any(|n| n.ends_with("default.xbe")),
        "expected default.xbe in fixture; got {:?}",
        names
    );
}

#[test]
fn streams_a_file_into_buffer() {
    let Some(mut img) = open_fixture() else {
        return;
    };
    let files = img.walk_files().expect("walk");
    let first = files.first().expect("at least one file in fixture");

    let mut sink = Vec::new();
    let n = img
        .read_into(first, &mut sink, Some(64 * 1024), None)
        .expect("read_into");
    assert_eq!(n as usize, sink.len());
    assert_eq!(n, first.size);
}

#[test]
fn streams_invokes_progress_callback() {
    let Some(mut img) = open_fixture() else {
        return;
    };
    let files = img.walk_files().expect("walk");
    let first = files.first().expect("at least one file in fixture");

    let mut progress_calls: Vec<(u64, u64)> = Vec::new();
    let mut sink = Vec::new();
    {
        let mut cb = |read: u64, total: u64| progress_calls.push((read, total));
        img.read_into(first, &mut sink, Some(64), Some(&mut cb))
            .expect("read_into with progress");
    }
    if first.size > 0 {
        assert!(
            !progress_calls.is_empty(),
            "progress should fire at least once"
        );
        let (last_read, last_total) = *progress_calls.last().unwrap();
        assert_eq!(last_read, first.size);
        assert_eq!(last_total, first.size);
    }
}
