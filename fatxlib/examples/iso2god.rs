//! Minimal CLI wrapper around [`fatxlib::iso2god::convert_iso`]. Argument
//! shape:
//!
//! ```text
//! iso2god [--trim | --no-trim] [--dry-run] [--game-title TITLE] <source.iso> <dest_dir>
//! ```
//!
//! `--trim` (from-end) is the default — almost everyone wants the trimmed
//! output. Pass `--no-trim` to convert the full source partition.
//!
//! `-j N` isn't exposed; `convert_iso` is single-threaded.

use std::env;
use std::path::PathBuf;
use std::process;
use std::time::Instant;

use fatxlib::iso2god::{ConvertOptions, TrimMode, convert_iso};

fn usage_and_exit() -> ! {
    eprintln!(
        "usage: iso2god [--trim | --no-trim] [--dry-run] [--game-title TITLE] <source.iso> <dest_dir>"
    );
    process::exit(2);
}

fn main() {
    let mut args = env::args().skip(1);
    // Default to from-end trim. Pass --no-trim to convert the full
    // source partition.
    let mut trim = TrimMode::FromEnd;
    let mut dry_run = false;
    let mut game_title: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--trim" => trim = TrimMode::FromEnd,
            "--no-trim" => trim = TrimMode::None,
            "--dry-run" => dry_run = true,
            "--game-title" => {
                game_title = Some(args.next().unwrap_or_else(|| usage_and_exit()));
            }
            "-h" | "--help" => usage_and_exit(),
            _ => positional.push(arg),
        }
    }

    if positional.len() != 2 {
        usage_and_exit();
    }
    let source = PathBuf::from(&positional[0]);
    let dest = PathBuf::from(&positional[1]);

    let started = Instant::now();
    let mut last_stage = String::new();
    let mut progress_cb = |stage: &str, current: u64, total: u64| {
        if stage != last_stage {
            eprintln!("[{stage}] {current}/{total}");
            last_stage = stage.to_string();
        } else if total > 0 && (current == total || current.is_multiple_of(total.max(1) / 10 + 1)) {
            eprintln!("[{stage}] {current}/{total}");
        }
    };

    let mut opts = ConvertOptions {
        trim,
        game_title: game_title.as_deref(),
        dry_run,
        progress: Some(&mut progress_cb),
        should_abort: None,
    };

    match convert_iso(&source, &dest, &mut opts) {
        Ok(report) => {
            let elapsed = started.elapsed();
            eprintln!();
            eprintln!("Title ID:    {:08X}", report.title_id);
            eprintln!("Media ID:    {:08X}", report.media_id);
            eprintln!("Content:     {:?}", report.content_type);
            eprintln!("Block count: {}", report.block_count);
            eprintln!("Part count:  {}", report.part_count);
            eprintln!("Data size:   {} bytes", report.data_size);
            eprintln!("Elapsed:     {:?}", elapsed);
        }
        Err(e) => {
            eprintln!("convert_iso failed: {e}");
            process::exit(1);
        }
    }
}
