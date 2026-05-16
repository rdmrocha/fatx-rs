//! ISO extraction planning shared by CLI/TUI callers.

use std::io::{Read, Seek};

use crate::error::Result;

use super::image::{XisoFile, XisoImage};
use super::policy::{FilteredIsoFiles, filter_entries, is_systemupdate_path};

#[derive(Debug, Clone)]
pub struct ExtractPlan {
    pub layout: String,
    pub entries: Vec<XisoFile>,
    pub kept: Vec<XisoFile>,
    pub skipped: Vec<XisoFile>,
    pub kept_bytes: u64,
    pub skipped_bytes: u64,
}

impl ExtractPlan {
    pub fn kept_files(&self) -> usize {
        self.kept.len()
    }

    pub fn skipped_files(&self) -> usize {
        self.skipped.len()
    }

    pub fn is_skipped(&self, entry: &XisoFile, keep_systemupdate: bool) -> bool {
        !keep_systemupdate && is_systemupdate_path(&entry.path)
    }
}

pub fn plan_extract<R: Read + Seek + Send + Sync>(
    img: &mut XisoImage<R>,
    keep_systemupdate: bool,
) -> Result<ExtractPlan> {
    let entries = img.walk_files()?;
    let FilteredIsoFiles {
        kept,
        skipped,
        kept_bytes,
        skipped_bytes,
    } = filter_entries(&entries, keep_systemupdate);

    let layout = img
        .layout()
        .map(|layout| format!("{} (0x{:08X})", layout.name, layout.offset))
        .unwrap_or_else(|| format!("unknown @ 0x{:08X}", img.partition_offset()));

    Ok(ExtractPlan {
        layout,
        entries,
        kept,
        skipped,
        kept_bytes,
        skipped_bytes,
    })
}
