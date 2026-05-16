//! Integration smoke test for [`fatxlib::iso2god::convert_iso`].
//!
//! Runs end-to-end against the bundled `tiny.xiso` fixture — a synthetic
//! XISO packed via `xdvdfs pack` that contains a real `default.xex`
//! (XellLaunch2_retail, a public homebrew launcher from the Free60
//! project). The XEX has valid `XEX2` magic + execution-info fields, so
//! `TitleInfo::from_image` parses it cleanly and the full pipeline runs.
//!
//! Plan C already proved byte-identical output across iliazeus, QAston,
//! and the Python port on a real game ISO, so this test focuses on
//! "the pipeline runs to completion and the output is shaped correctly",
//! not byte-equality.

use std::fs;
use std::path::PathBuf;

use fatxlib::iso2god::{ConvertOptions, TrimMode, convert_iso};

fn fixture_path() -> Option<PathBuf> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny.xiso");
    if p.exists() { Some(p) } else { None }
}

#[test]
fn converts_fixture_into_valid_god_package() {
    let Some(iso) = fixture_path() else {
        eprintln!("skipping: fatxlib/tests/fixtures/tiny.xiso missing");
        return;
    };

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dest = tmp.path();

    let mut opts = ConvertOptions {
        trim: TrimMode::FromEnd,
        game_title: Some("XellLaunch2 fixture"),
        dry_run: false,
        progress: None,
    };

    let report = convert_iso(&iso, dest, &mut opts).expect("convert_iso");

    assert!(report.title_id != 0, "title id should be non-zero");
    assert!(
        report.part_count >= 1,
        "fixture must produce at least one Data part; got {:?}",
        report
    );
    assert!(report.block_count >= 1);

    // CON header lives at <dest>/<title_id>/<content_type>/<media_id>
    let title_hex = format!("{:08X}", report.title_id);
    let ctype_hex = format!("{:08X}", report.content_type as u32);
    let media_hex = if matches!(
        report.content_type,
        fatxlib::iso2god::god::ContentType::XboxOriginal
    ) {
        title_hex.clone()
    } else {
        format!("{:08X}", report.media_id)
    };

    let con_header_path = dest.join(&title_hex).join(&ctype_hex).join(&media_hex);
    let data_dir = dest
        .join(&title_hex)
        .join(&ctype_hex)
        .join(format!("{}.data", media_hex));
    let first_part = data_dir.join("Data0000");

    assert!(
        con_header_path.exists(),
        "CON header missing at {}",
        con_header_path.display()
    );
    assert!(
        first_part.exists(),
        "Data0000 missing at {}",
        first_part.display()
    );

    let con_header_size = fs::metadata(&con_header_path).expect("stat header").len();
    assert_eq!(
        con_header_size, 0xB000,
        "CON header should be 45 056 bytes (empty_live template)"
    );

    let first_part_size = fs::metadata(&first_part).expect("stat data").len();
    assert!(
        first_part_size > 0,
        "Data0000 should be non-empty; got {} bytes",
        first_part_size
    );

    // CON header should start with "LIVE" (`empty_live.bin` magic).
    let head = fs::read(&con_header_path).expect("read header");
    assert_eq!(
        &head[..4],
        b"LIVE",
        "CON header missing LIVE magic; got {:?}",
        &head[..4]
    );
}

#[test]
fn fixture_dry_run_does_not_create_files() {
    let Some(iso) = fixture_path() else {
        return;
    };

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dest = tmp.path();

    let mut opts = ConvertOptions {
        trim: TrimMode::FromEnd,
        game_title: None,
        dry_run: true,
        progress: None,
    };

    let report = convert_iso(&iso, dest, &mut opts).expect("dry-run convert");
    assert!(report.part_count >= 1);

    let entries: Vec<_> = fs::read_dir(dest)
        .expect("readdir")
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        entries.is_empty(),
        "dry_run should not write anything; found {:?}",
        entries.iter().map(|e| e.path()).collect::<Vec<_>>()
    );
}

#[test]
fn fixture_extracts_expected_title_id() {
    // XellLaunch2_retail's TitleID is 0xFFFF011D (homebrew/dev range).
    // If this assertion fires, either the fixture changed or the XEX
    // parser drifted.
    let Some(iso) = fixture_path() else {
        return;
    };

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut opts = ConvertOptions {
        dry_run: true,
        ..Default::default()
    };

    let report = convert_iso(&iso, tmp.path(), &mut opts).expect("dry-run convert");
    assert_eq!(
        report.title_id, 0xFFFF011D,
        "expected XellLaunch2_retail TitleID; fixture may have changed"
    );
}
