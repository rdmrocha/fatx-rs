//! Public entry point for ISO → Games-on-Demand conversion.
//!
//! Walks the source ISO via xdvdfs, computes the GoD file layout, writes
//! each Data part with its embedded hash tree, and finalizes the CON
//! header. See `NOTICE` and the [`crate::iso2god`] module doc for the
//! upstream sources this code descends from.
//!
//! Single-threaded. The metadata pre-pass uses a 1 MiB `BufReader` to cut
//! syscall tax on the file-tree walk; per-part data reads go straight
//! against the file (a fixed-size subpart read into a pre-allocated
//! buffer makes an interposing reader pure overhead). A multi-threaded
//! mode could land later as an opt-in flag.

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use crate::error::{FatxError, Result};
use crate::iso2god::executable::TitleInfo;
use crate::iso2god::god::{self, ConHeaderBuilder, ContentType, FileLayout, HashList};

/// Buffer capacity for the metadata-pass source-ISO reader. 1 MiB —
/// large enough that the default 8 KiB capacity's syscall tax disappears
/// on multi-GiB ISOs, without requiring OS-level read-ahead tuning.
pub const SOURCE_BUFFER_SIZE: usize = 1 << 20;

/// Progress callback shape: `(stage, current, total)` where `stage` is one
/// of `"parts"`, `"mht"`, `"header"`.
pub type ProgressFn<'a> = &'a mut dyn FnMut(&str, u64, u64);

/// How to size the output GoD relative to the source ISO.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrimMode {
    /// Walk the directory tree, find the max `(offset + size)`, and pack
    /// only that many bytes. The default — yields the smallest output
    /// without changing on-disk meaning.
    #[default]
    FromEnd,
    /// Pack every byte from the start of the data partition to the end of
    /// the source file. Larger output, but useful when the directory tree
    /// is suspect.
    None,
}

/// Knobs the caller can adjust per conversion.
#[derive(Default)]
pub struct ConvertOptions<'a> {
    pub trim: TrimMode,
    /// Override the human-readable game title written into the CON header.
    /// `None` leaves the slot blank — fatxlib's [`crate::titles`] catalog is
    /// not consulted here; callers that want auto-fill should resolve the
    /// title ID themselves and pass the result through.
    pub game_title: Option<&'a str>,
    /// When true, read metadata and return the [`ConvertReport`] without
    /// touching `dest_dir`.
    pub dry_run: bool,
    /// Optional progress callback. Stages: "scan", "parts", "mht", "header".
    /// `current`/`total` are stage-relative.
    pub progress: Option<ProgressFn<'a>>,
    /// Optional cancellation hook. Checked before each part write and
    /// before each MHT-chain step; returning `true` aborts the conversion
    /// with a clean error rather than partial silent failure. Mid-part
    /// cancellation is not supported.
    pub should_abort: Option<&'a dyn Fn() -> bool>,
}

/// Metadata extracted from the source ISO and the resulting layout sizing.
#[derive(Debug, Clone, Copy)]
pub struct ConvertReport {
    pub title_id: u32,
    pub media_id: u32,
    pub content_type: ContentType,
    pub part_count: u64,
    pub block_count: u64,
    /// Bytes of the source partition packed into the GoD parts (post-trim).
    pub data_size: u64,
}

/// Convert an Xbox 360 / original-Xbox ISO into a Games-on-Demand package.
///
/// Writes:
/// - `<dest_dir>/<title_id>/<content_type>/<media_id>.data/Data0000..DataN`
/// - `<dest_dir>/<title_id>/<content_type>/<media_id>` (CON header)
///
/// Returns a [`ConvertReport`] describing what was produced (or what *would*
/// have been, when `opts.dry_run` is set).
pub fn convert_iso(
    source_iso: &Path,
    dest_dir: &Path,
    opts: &mut ConvertOptions<'_>,
) -> Result<ConvertReport> {
    let source_iso_file_meta = fs::metadata(source_iso).map_err(FatxError::Io)?;

    let img = File::open(source_iso).map_err(FatxError::Io)?;
    let xiso = BufReader::with_capacity(SOURCE_BUFFER_SIZE, img);
    let mut xiso = xdvdfs::blockdev::OffsetWrapper::new(xiso)
        .map_err(|e| FatxError::Other(format!("xdvdfs offset detect: {e:?}")))?;

    let volume = xdvdfs::read::read_volume(&mut xiso)
        .map_err(|e| FatxError::Other(format!("xdvdfs read_volume: {e:?}")))?;

    let title_info = TitleInfo::from_image(&mut xiso, volume)?;
    let exe_info = title_info.execution_info;
    let content_type = title_info.content_type;

    // Pull the partition offset out from the wrapper; the per-part
    // readers use it as their `seek` target.
    let root_offset = {
        xiso.seek(SeekFrom::Start(0)).map_err(FatxError::Io)?;
        xiso.get_mut().stream_position().map_err(FatxError::Io)?
    };

    let data_size = match opts.trim {
        TrimMode::FromEnd => volume
            .root_table
            .file_tree(&mut xiso)
            .map_err(|e| FatxError::Other(format!("xdvdfs file_tree: {e:?}")))?
            .iter()
            .map(|dirent| {
                if dirent.1.node.dirent.data.is_empty() {
                    return 0;
                }
                let offset = dirent
                    .1
                    .node
                    .dirent
                    .data
                    .offset::<std::io::Error>(0)
                    .unwrap_or(0);
                offset + dirent.1.node.dirent.data.size() as u64
            })
            .max()
            .unwrap_or(0),
        TrimMode::None => source_iso_file_meta.len() - root_offset,
    };

    let block_count = data_size.div_ceil(god::BLOCK_SIZE);
    let part_count = block_count.div_ceil(god::BLOCKS_PER_PART);

    let report = ConvertReport {
        title_id: exe_info.title_id,
        media_id: exe_info.media_id,
        content_type,
        part_count,
        block_count,
        data_size,
    };

    if opts.dry_run {
        return Ok(report);
    }

    let file_layout = FileLayout::new(dest_dir, &exe_info, content_type);

    ensure_empty_dir(&file_layout.data_dir_path())?;

    // ---- Write the Data parts (sequential) ------------------------------

    if let Some(cb) = opts.progress.as_deref_mut() {
        cb("parts", 0, part_count);
    }

    for part_index in 0..part_count {
        if let Some(abort) = opts.should_abort
            && abort()
        {
            return Err(FatxError::Other("convert_iso: cancelled".to_string()));
        }
        let part_path = file_layout.part_file_path(part_index);
        let part_file = File::options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&part_path)
            .map_err(FatxError::Io)?;
        // Wrap the part output in a 1 MiB BufWriter so the interleaved
        // 4 KiB hash-list writes and the larger subpart writes don't
        // each turn into separate syscalls. The subpart writes themselves
        // bypass the buffer (they're larger than the free space), but
        // the hash writes ride on top of them for free.
        let part_file = BufWriter::with_capacity(SOURCE_BUFFER_SIZE, part_file);

        // Fresh source reader per part so we can `seek` from a known
        // starting point (root_offset). Deliberately UNbuffered — the
        // inner hot loop in `god::write_part` reads exactly SUBPART_SIZE
        // (~832 KiB) per pass into a pre-allocated buffer; an interposing
        // BufReader at that read size just adds an extra memcpy through
        // its internal buffer with no syscall-batching benefit.
        let mut iso_data_volume = File::open(source_iso).map_err(FatxError::Io)?;
        iso_data_volume
            .seek(SeekFrom::Start(root_offset))
            .map_err(FatxError::Io)?;

        god::write_part(iso_data_volume, part_index, part_file)?;

        if let Some(cb) = opts.progress.as_deref_mut() {
            cb("parts", part_index + 1, part_count);
        }
    }

    // ---- MHT hash chain (last part → first; in-place update) ------------

    if let Some(cb) = opts.progress.as_deref_mut() {
        cb("mht", 0, part_count);
    }

    let mut mht = read_part_mht(&file_layout, part_count - 1)?;
    for prev_part_index in (0..part_count - 1).rev() {
        if let Some(abort) = opts.should_abort
            && abort()
        {
            return Err(FatxError::Other("convert_iso: cancelled".to_string()));
        }
        let mut prev_mht = read_part_mht(&file_layout, prev_part_index)?;
        prev_mht.add_hash(&mht.digest());
        write_part_mht(&file_layout, prev_part_index, &prev_mht)?;
        mht = prev_mht;

        if let Some(cb) = opts.progress.as_deref_mut() {
            cb("mht", part_count - prev_part_index, part_count);
        }
    }

    let last_part_size = fs::metadata(file_layout.part_file_path(part_count - 1))
        .map_err(FatxError::Io)?
        .len();

    // ---- CON header (final step) ----------------------------------------

    if let Some(cb) = opts.progress.as_deref_mut() {
        cb("header", 0, 1);
    }

    let mut con_header = ConHeaderBuilder::new()
        .with_execution_info(&exe_info)
        .with_block_counts(block_count as u32, 0)
        .with_data_parts_info(
            part_count as u32,
            last_part_size + (part_count - 1) * god::BLOCK_SIZE * 0xa290,
        )
        .with_content_type(content_type)
        .with_mht_hash(&mht.digest());

    if let Some(game_title) = opts.game_title {
        con_header = con_header.with_game_title(game_title);
    }

    let con_header = con_header.finalize();

    let mut con_header_file = File::options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(file_layout.con_header_file_path())
        .map_err(FatxError::Io)?;

    con_header_file
        .write_all(&con_header)
        .map_err(FatxError::Io)?;

    if let Some(cb) = opts.progress.as_deref_mut() {
        cb("header", 1, 1);
    }

    Ok(report)
}

// --- internal helpers --------------------------------------------------

fn ensure_empty_dir(path: &Path) -> Result<()> {
    if fs::exists(path).map_err(FatxError::Io)? {
        fs::remove_dir_all(path).map_err(FatxError::Io)?;
    }
    fs::create_dir_all(path).map_err(FatxError::Io)?;
    Ok(())
}

fn read_part_mht(file_layout: &FileLayout, part_index: u64) -> Result<HashList> {
    let part_file = file_layout.part_file_path(part_index);
    let mut part_file = File::options()
        .read(true)
        .open(part_file)
        .map_err(FatxError::Io)?;
    HashList::read(&mut part_file)
}

fn write_part_mht(file_layout: &FileLayout, part_index: u64, mht: &HashList) -> Result<()> {
    let part_file = file_layout.part_file_path(part_index);
    let mut part_file = File::options()
        .write(true)
        .open(part_file)
        .map_err(FatxError::Io)?;
    mht.write(&mut part_file)?;
    Ok(())
}
