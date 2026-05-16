//! Shared policy decisions for ISO-derived operations.

use super::image::XisoFile;

#[derive(Debug, Clone)]
pub struct FilteredIsoFiles {
    pub kept: Vec<XisoFile>,
    pub skipped: Vec<XisoFile>,
    pub kept_bytes: u64,
    pub skipped_bytes: u64,
}

impl FilteredIsoFiles {
    pub fn kept_files(&self) -> usize {
        self.kept.len()
    }

    pub fn skipped_files(&self) -> usize {
        self.skipped.len()
    }
}

pub fn is_systemupdate_path(path: &str) -> bool {
    path.trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("")
        .eq_ignore_ascii_case("$SystemUpdate")
}

pub fn filter_entries(entries: &[XisoFile], keep_systemupdate: bool) -> FilteredIsoFiles {
    let (kept, skipped): (Vec<_>, Vec<_>) = if keep_systemupdate {
        (entries.to_vec(), Vec::new())
    } else {
        entries
            .iter()
            .cloned()
            .partition(|entry| !is_systemupdate_path(&entry.path))
    };

    FilteredIsoFiles {
        kept_bytes: kept.iter().map(|entry| entry.size).sum(),
        skipped_bytes: skipped.iter().map(|entry| entry.size).sum(),
        kept,
        skipped,
    }
}
