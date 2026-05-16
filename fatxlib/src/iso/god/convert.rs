//! Public entry point for ISO → Games-on-Demand conversion.
//!
//! Walks the source ISO via xdvdfs, computes the GoD file layout, writes
//! each Data part with its embedded hash tree, and finalizes the CON
//! header. See `NOTICE` for the upstream sources this code descends from.
//!
//! Single-threaded. The metadata pre-pass uses a 1 MiB `BufReader` to cut
//! syscall tax on the file-tree walk; per-part data reads go straight
//! against the file (a fixed-size subpart read into a pre-allocated
//! buffer makes an interposing reader pure overhead). A multi-threaded
//! mode could land later as an opt-in flag.

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::error::{FatxError, Result};
use crate::executable::TitleInfo;
use crate::iso::compact::build_compact_source;
use crate::volume::FatxVolume;

use super::{
    self as god, BLOCK_SIZE, BLOCKS_PER_PART, ConHeaderBuilder, ContentType, FileLayout, HashList,
    SUBPART_SIZE, SUBPARTS_PER_PART,
};

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
    /// Walk the existing directory tree, find the max `(offset + size)`,
    /// and pack only that many bytes. Preserves any mastered holes inside
    /// the XDVDFS layout while trimming trailing slack after the highest
    /// file extent.
    PreserveLayout,
    /// Pack every byte from the start of the data partition to the end of
    /// the source file. Larger output, but useful when the directory tree
    /// is suspect.
    None,
    /// Rebuild the XDVDFS image densely as a virtual layout and stream
    /// those bytes directly through the GoD pipeline.
    #[default]
    Compact,
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

trait ReadSeek: Read + Seek {}

impl<T: Read + Seek> ReadSeek for T {}

struct PreparedSource {
    report: ConvertReport,
    exe_info: crate::executable::TitleExecutionInfo,
    content_type: ContentType,
    reader: ReaderSource,
}

enum ReaderSource {
    Raw {
        source_iso: PathBuf,
        root_offset: u64,
    },
    Compact {
        source_iso: PathBuf,
        compact: crate::iso::compact::CompactSource,
    },
}

impl PreparedSource {
    fn open_reader(&self) -> Result<Box<dyn ReadSeek + '_>> {
        self.reader.open_reader()
    }
}

impl ReaderSource {
    fn open_reader(&self) -> Result<Box<dyn ReadSeek + '_>> {
        match self {
            Self::Raw {
                source_iso,
                root_offset,
            } => {
                let mut iso = File::open(source_iso).map_err(FatxError::Io)?;
                iso.seek(SeekFrom::Start(*root_offset))
                    .map_err(FatxError::Io)?;
                Ok(Box::new(iso))
            }
            Self::Compact {
                source_iso,
                compact,
            } => Ok(Box::new(compact.open_reader(source_iso)?)),
        }
    }
}

trait GodSink {
    fn begin(&mut self, source: &PreparedSource) -> Result<()>;
    fn write_part<'a>(
        &mut self,
        source: &PreparedSource,
        part_index: u64,
        opts: &mut ConvertOptions<'a>,
    ) -> Result<()>;
    fn read_master_hash(&mut self, source: &PreparedSource, part_index: u64) -> Result<HashList>;
    fn write_master_hash(
        &mut self,
        source: &PreparedSource,
        part_index: u64,
        mht: &HashList,
    ) -> Result<()>;
    fn last_part_size(&self, source: &PreparedSource) -> Result<u64>;
    fn write_con_header(&mut self, source: &PreparedSource, con_bytes: Vec<u8>) -> Result<()>;
    fn flush_after_parts(&mut self) -> Result<()> {
        Ok(())
    }
    fn flush_after_mht(&mut self) -> Result<()> {
        Ok(())
    }
    fn flush_after_header(&mut self) -> Result<()> {
        Ok(())
    }
}

struct HostFsSink<'a> {
    dest_dir: &'a Path,
}

impl<'a> HostFsSink<'a> {
    fn data_dir_path(&self, source: &PreparedSource) -> std::path::PathBuf {
        FileLayout::new(self.dest_dir, &source.exe_info, source.content_type).data_dir_path()
    }

    fn part_file_path(&self, source: &PreparedSource, part_index: u64) -> std::path::PathBuf {
        FileLayout::new(self.dest_dir, &source.exe_info, source.content_type)
            .part_file_path(part_index)
    }

    fn con_header_file_path(&self, source: &PreparedSource) -> std::path::PathBuf {
        FileLayout::new(self.dest_dir, &source.exe_info, source.content_type).con_header_file_path()
    }
}

impl GodSink for HostFsSink<'_> {
    fn begin(&mut self, source: &PreparedSource) -> Result<()> {
        ensure_empty_dir(&self.data_dir_path(source))
    }

    fn write_part<'a>(
        &mut self,
        source: &PreparedSource,
        part_index: u64,
        _opts: &mut ConvertOptions<'a>,
    ) -> Result<()> {
        let part_path = self.part_file_path(source, part_index);
        let part_file = File::options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&part_path)
            .map_err(FatxError::Io)?;
        let part_file = BufWriter::with_capacity(SOURCE_BUFFER_SIZE, part_file);
        let remaining_bytes = part_payload_bytes(source.report.data_size, part_index);
        let iso_data_volume = source.open_reader()?;
        god::write_part(iso_data_volume, part_index, remaining_bytes, part_file)
    }

    fn read_master_hash(&mut self, source: &PreparedSource, part_index: u64) -> Result<HashList> {
        let part_path = self.part_file_path(source, part_index);
        read_part_mht(&part_path)
    }

    fn write_master_hash(
        &mut self,
        source: &PreparedSource,
        part_index: u64,
        mht: &HashList,
    ) -> Result<()> {
        let part_path = self.part_file_path(source, part_index);
        write_part_mht(&part_path, mht)
    }

    fn last_part_size(&self, source: &PreparedSource) -> Result<u64> {
        fs::metadata(self.part_file_path(source, source.report.part_count - 1))
            .map_err(FatxError::Io)
            .map(|meta| meta.len())
    }

    fn write_con_header(&mut self, source: &PreparedSource, con_bytes: Vec<u8>) -> Result<()> {
        let mut con_header_file = File::options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(self.con_header_file_path(source))
            .map_err(FatxError::Io)?;
        con_header_file.write_all(&con_bytes).map_err(FatxError::Io)
    }
}

struct FatxSink<'a, T: Read + Seek + Write> {
    vol: &'a mut FatxVolume<T>,
    dest_dir: &'a str,
    data_dir: Option<String>,
    con_header_path: Option<String>,
    part_buf: Vec<u8>,
    master_lists: Vec<HashList>,
    last_part_size: u64,
}

impl<'a, T: Read + Seek + Write> FatxSink<'a, T> {
    fn new(vol: &'a mut FatxVolume<T>, dest_dir: &'a str) -> Self {
        Self {
            vol,
            dest_dir,
            data_dir: None,
            con_header_path: None,
            part_buf: vec![0u8; MAX_PART_BYTES],
            master_lists: Vec::new(),
            last_part_size: 0,
        }
    }

    fn data_dir(&self) -> Result<&str> {
        self.data_dir
            .as_deref()
            .ok_or_else(|| FatxError::Other("fatx sink not initialized".to_string()))
    }

    fn con_header_path(&self) -> Result<&str> {
        self.con_header_path
            .as_deref()
            .ok_or_else(|| FatxError::Other("fatx sink not initialized".to_string()))
    }

    fn part_path(&self, part_index: u64) -> Result<String> {
        Ok(format!("{}/Data{:04}", self.data_dir()?, part_index))
    }
}

impl<T: Read + Seek + Write> GodSink for FatxSink<'_, T> {
    fn begin(&mut self, source: &PreparedSource) -> Result<()> {
        let title_id_str = format!("{:08X}", source.exe_info.title_id);
        let content_type_str = format!("{:08X}", source.content_type as u32);
        let media_id_str = match source.content_type {
            ContentType::GamesOnDemand => format!("{:08X}", source.exe_info.media_id),
            ContentType::XboxOriginal => format!("{:08X}", source.exe_info.title_id),
        };
        let dest_root = self.dest_dir.trim_end_matches('/');
        let title_dir = format!("{}/{}", dest_root, title_id_str);
        let content_dir = format!("{}/{}", title_dir, content_type_str);
        let con_header_path = format!("{}/{}", content_dir, media_id_str);
        let data_dir = format!("{}/{}.data", content_dir, media_id_str);

        ensure_fatx_dir(self.vol, &title_dir)?;
        ensure_fatx_dir(self.vol, &content_dir)?;
        ensure_fatx_dir(self.vol, &data_dir)?;
        self.data_dir = Some(data_dir);
        self.con_header_path = Some(con_header_path);
        self.master_lists.clear();
        self.master_lists.reserve(source.report.part_count as usize);
        self.last_part_size = 0;
        Ok(())
    }

    fn write_part<'a>(
        &mut self,
        source: &PreparedSource,
        part_index: u64,
        opts: &mut ConvertOptions<'a>,
    ) -> Result<()> {
        let remaining_bytes = part_payload_bytes(source.report.data_size, part_index);
        let mut iso = source.open_reader()?;
        let (len, master) =
            fill_part_buf(&mut iso, part_index, remaining_bytes, &mut self.part_buf)?;
        let part_path = self.part_path(part_index)?;
        let reader = Cursor::new(&self.part_buf[..len]);

        let mut outer = opts.progress.take();
        let part_idx_now = part_index;
        let part_count_now = source.report.part_count;
        {
            let mut inner = |bytes: u64, total: u64| {
                if let Some(cb) = outer.as_deref_mut() {
                    let stage = format!("part {}/{}", part_idx_now + 1, part_count_now);
                    cb(&stage, bytes, total);
                }
            };
            self.vol
                .create_file_from_reader(&part_path, len as u64, reader, Some(&mut inner))?;
        }
        opts.progress = outer;

        self.master_lists.push(master);
        self.last_part_size = len as u64;
        Ok(())
    }

    fn read_master_hash(&mut self, _source: &PreparedSource, part_index: u64) -> Result<HashList> {
        self.master_lists
            .get(part_index as usize)
            .cloned()
            .ok_or_else(|| FatxError::Other(format!("missing FATX part {}", part_index)))
    }

    fn write_master_hash(
        &mut self,
        _source: &PreparedSource,
        part_index: u64,
        mht: &HashList,
    ) -> Result<()> {
        let slot = self
            .master_lists
            .get_mut(part_index as usize)
            .ok_or_else(|| FatxError::Other(format!("missing FATX part {}", part_index)))?;
        *slot = mht.clone();
        let part_path = self.part_path(part_index)?;
        overwrite_part_master(self.vol, &part_path, mht.bytes())
    }

    fn last_part_size(&self, _source: &PreparedSource) -> Result<u64> {
        Ok(self.last_part_size)
    }

    fn write_con_header(&mut self, _source: &PreparedSource, con_bytes: Vec<u8>) -> Result<()> {
        let con_len = con_bytes.len() as u64;
        let path = self.con_header_path()?.to_string();
        self.vol
            .create_file_from_reader(&path, con_len, Cursor::new(con_bytes), None)
    }

    fn flush_after_parts(&mut self) -> Result<()> {
        let _ = self.vol.flush();
        Ok(())
    }

    fn flush_after_mht(&mut self) -> Result<()> {
        let _ = self.vol.flush();
        Ok(())
    }

    fn flush_after_header(&mut self) -> Result<()> {
        let _ = self.vol.flush();
        Ok(())
    }
}

/// Convert an Xbox 360 / original-Xbox ISO into a Games-on-Demand package.
///
/// Writes:
/// - `<dest_dir>/<title_id>/<content_type>/<media_id>.data/Data0000..DataN`
/// - `<dest_dir>/<title_id>/<content_type>/<media_id>` (CON header)
///
/// Returns a [`ConvertReport`] describing what was produced (or what *would*
/// have been, when `opts.dry_run` is set).
pub fn convert_iso<'a>(
    source_iso: &Path,
    dest_dir: &Path,
    opts: &'a mut ConvertOptions<'a>,
) -> Result<ConvertReport> {
    let source = prepare_source(source_iso, opts)?;
    if opts.dry_run {
        return Ok(source.report);
    }
    let mut sink = HostFsSink { dest_dir };
    run_conversion(&source, &mut sink, opts, "convert_iso")
}

// --- internal helpers --------------------------------------------------

fn ensure_empty_dir(path: &Path) -> Result<()> {
    if fs::exists(path).map_err(FatxError::Io)? {
        fs::remove_dir_all(path).map_err(FatxError::Io)?;
    }
    fs::create_dir_all(path).map_err(FatxError::Io)?;
    Ok(())
}

fn read_part_mht(path: &Path) -> Result<HashList> {
    let mut part_file = File::options()
        .read(true)
        .open(path)
        .map_err(FatxError::Io)?;
    HashList::read(&mut part_file)
}

fn write_part_mht(path: &Path, mht: &HashList) -> Result<()> {
    let mut part_file = File::options()
        .write(true)
        .open(path)
        .map_err(FatxError::Io)?;
    mht.write(&mut part_file)?;
    Ok(())
}

// ===========================================================================
// Streaming variant: write the GoD package straight into a FatxVolume.
// ===========================================================================

/// Maximum bytes a single Data part file can occupy. Equals `4 KiB
/// master_hash_list + SUBPARTS_PER_PART × (4 KiB sub_hash_list +
/// SUBPART_SIZE)`, which is exactly `BLOCK_SIZE * 0xa290` — the magic
/// constant the CON header uses to describe a full part.
const MAX_PART_BYTES: usize = 4096 + (SUBPARTS_PER_PART as usize) * (4096 + SUBPART_SIZE as usize);

fn part_payload_bytes(data_size: u64, part_index: u64) -> u64 {
    let part_start = part_index
        .saturating_mul(BLOCKS_PER_PART)
        .saturating_mul(BLOCK_SIZE);
    data_size
        .saturating_sub(part_start)
        .min(BLOCKS_PER_PART * BLOCK_SIZE)
}

/// Convert an ISO directly into a Games-on-Demand package rooted at
/// `dest_dir` on a FATX volume — no local staging.
///
/// Same output as [`convert_iso`] but bypasses the local filesystem
/// entirely: each Data part is built in a reused in-memory buffer
/// (~163 MiB) and streamed into FATX via
/// [`FatxVolume::create_file_from_reader`]. After all parts are written,
/// the MHT chain pass happens in memory and each part's first 4 KiB
/// (the master hash list) is patched on disk with a single
/// read-modify-write at the cluster level.
///
/// Peak RAM: one part buffer (~163 MiB) plus the per-part master hash
/// list vector (~108 KiB total for a 27-part game).
pub fn convert_iso_to_fatx<'a, T>(
    source_iso: &Path,
    vol: &mut FatxVolume<T>,
    dest_dir: &str,
    opts: &'a mut ConvertOptions<'a>,
) -> Result<ConvertReport>
where
    T: Read + Seek + Write,
{
    let source = prepare_source(source_iso, opts)?;
    if opts.dry_run {
        return Ok(source.report);
    }
    if source.report.part_count == 0 {
        return Err(FatxError::Other(
            "convert_iso_to_fatx: source has no data to convert".to_string(),
        ));
    }
    let mut sink = FatxSink::new(vol, dest_dir);
    run_conversion(&source, &mut sink, opts, "convert_iso_to_fatx")
}

fn prepare_source(source_iso: &Path, opts: &ConvertOptions<'_>) -> Result<PreparedSource> {
    if matches!(opts.trim, TrimMode::Compact) {
        let compact = build_compact_source(source_iso, opts.should_abort)?;
        let report = build_report(
            compact.exe_info().title_id,
            compact.exe_info().media_id,
            compact.content_type(),
            compact.data_size(),
        );
        return Ok(PreparedSource {
            exe_info: compact.exe_info().clone(),
            content_type: compact.content_type(),
            report,
            reader: ReaderSource::Compact {
                source_iso: source_iso.to_path_buf(),
                compact,
            },
        });
    }

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
    let root_offset = {
        xiso.seek(SeekFrom::Start(0)).map_err(FatxError::Io)?;
        xiso.get_mut().stream_position().map_err(FatxError::Io)?
    };
    let data_size = match opts.trim {
        TrimMode::PreserveLayout => volume
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
        TrimMode::Compact => unreachable!("compact handled before metadata pass"),
    };
    let report = build_report(
        exe_info.title_id,
        exe_info.media_id,
        content_type,
        data_size,
    );
    Ok(PreparedSource {
        exe_info,
        content_type,
        report,
        reader: ReaderSource::Raw {
            source_iso: source_iso.to_path_buf(),
            root_offset,
        },
    })
}

fn build_report(
    title_id: u32,
    media_id: u32,
    content_type: ContentType,
    data_size: u64,
) -> ConvertReport {
    let block_count = data_size.div_ceil(BLOCK_SIZE);
    let part_count = block_count.div_ceil(BLOCKS_PER_PART);
    ConvertReport {
        title_id,
        media_id,
        content_type,
        part_count,
        block_count,
        data_size,
    }
}

fn build_con_header(
    source: &PreparedSource,
    mht_digest: &[u8; 20],
    game_title: Option<&str>,
    last_part_size: u64,
) -> Vec<u8> {
    let mut con_header = ConHeaderBuilder::new()
        .with_execution_info(&source.exe_info)
        .with_block_counts(source.report.block_count as u32, 0)
        .with_data_parts_info(
            source.report.part_count as u32,
            last_part_size + (source.report.part_count - 1) * BLOCK_SIZE * 0xa290,
        )
        .with_content_type(source.content_type)
        .with_mht_hash(mht_digest);
    if let Some(title) = game_title {
        con_header = con_header.with_game_title(title);
    }
    con_header.finalize()
}

fn run_conversion<'a, S: GodSink>(
    source: &PreparedSource,
    sink: &mut S,
    opts: &mut ConvertOptions<'a>,
    cancel_ctx: &str,
) -> Result<ConvertReport> {
    if source.report.part_count == 0 {
        return Err(FatxError::Other(format!(
            "{cancel_ctx}: source has no data to convert"
        )));
    }

    sink.begin(source)?;

    if let Some(cb) = opts.progress.as_deref_mut() {
        cb("parts", 0, source.report.part_count);
    }
    for part_index in 0..source.report.part_count {
        if let Some(abort) = opts.should_abort
            && abort()
        {
            return Err(FatxError::Other(format!("{cancel_ctx}: cancelled")));
        }
        sink.write_part(source, part_index, opts)?;
        if let Some(cb) = opts.progress.as_deref_mut() {
            cb("parts", part_index + 1, source.report.part_count);
        }
    }
    sink.flush_after_parts()?;

    if let Some(cb) = opts.progress.as_deref_mut() {
        cb("mht", 0, source.report.part_count);
    }
    let mut mht = sink.read_master_hash(source, source.report.part_count - 1)?;
    for prev_part_index in (0..source.report.part_count - 1).rev() {
        if let Some(abort) = opts.should_abort
            && abort()
        {
            return Err(FatxError::Other(format!("{cancel_ctx}: cancelled")));
        }
        let mut prev_mht = sink.read_master_hash(source, prev_part_index)?;
        prev_mht.add_hash(&mht.digest());
        sink.write_master_hash(source, prev_part_index, &prev_mht)?;
        mht = prev_mht;
        if let Some(cb) = opts.progress.as_deref_mut() {
            cb(
                "mht",
                source.report.part_count - prev_part_index,
                source.report.part_count,
            );
        }
    }
    sink.flush_after_mht()?;

    if let Some(cb) = opts.progress.as_deref_mut() {
        cb("header", 0, 1);
    }
    let last_part_size = sink.last_part_size(source)?;
    let con_header = build_con_header(source, &mht.digest(), opts.game_title, last_part_size);
    sink.write_con_header(source, con_header)?;
    sink.flush_after_header()?;
    if let Some(cb) = opts.progress.as_deref_mut() {
        cb("header", 1, 1);
    }

    Ok(source.report)
}

/// Build one Data part directly in `out`. Returns the actual number of
/// bytes used (the last part is usually shorter than [`MAX_PART_BYTES`])
/// and the master hash list for that part. `out` must be at least
/// [`MAX_PART_BYTES`] long.
fn fill_part_buf<R: Read + Seek>(
    data_volume: &mut R,
    part_index: u64,
    remaining_bytes: u64,
    out: &mut [u8],
) -> Result<(usize, HashList)> {
    data_volume
        .seek_relative((part_index * BLOCKS_PER_PART * BLOCK_SIZE) as i64)
        .map_err(FatxError::Io)?;

    let mut master = HashList::new();

    // First 4 KiB reserved for the master hash list — filled in at the end.
    let mut cursor = 4096usize;
    let mut subpart_buf = vec![0u8; SUBPART_SIZE as usize];
    let mut bytes_left = remaining_bytes;

    for _ in 0..SUBPARTS_PER_PART {
        if bytes_left == 0 {
            break;
        }
        let want = (subpart_buf.len() as u64).min(bytes_left) as usize;
        let mut got = 0usize;
        while got < want {
            let n = data_volume
                .read(&mut subpart_buf[got..want])
                .map_err(FatxError::Io)?;
            if n == 0 {
                break;
            }
            got += n;
        }
        if got == 0 {
            break;
        }
        let subpart = &subpart_buf[..got];

        let mut sub_hash = HashList::new();
        for block in subpart.chunks(BLOCK_SIZE as usize) {
            sub_hash.add_block_hash(block);
        }

        out[cursor..cursor + 4096].copy_from_slice(sub_hash.bytes());
        cursor += 4096;
        out[cursor..cursor + got].copy_from_slice(subpart);
        cursor += got;
        bytes_left -= got as u64;

        master.add_block_hash(sub_hash.bytes());

        if got < want {
            break;
        }
    }

    out[0..4096].copy_from_slice(master.bytes());
    Ok((cursor, master))
}

/// Read the file's first cluster, overwrite its first 4 KiB with
/// `new_master`, write the cluster back. Used to patch each Data part's
/// master hash list after the MHT chain pass.
fn overwrite_part_master<T>(
    vol: &mut FatxVolume<T>,
    path: &str,
    new_master: &[u8; 4096],
) -> Result<()>
where
    T: Read + Seek + Write,
{
    let entry = vol.resolve_path(path)?;
    let first_cluster = entry.first_cluster;
    let cluster_size = vol.superblock.cluster_size() as usize;
    let mut cluster_buf = vec![0u8; cluster_size];
    vol.read_cluster(first_cluster, &mut cluster_buf)?;
    cluster_buf[..new_master.len()].copy_from_slice(new_master);
    vol.write_cluster(first_cluster, &cluster_buf)?;
    Ok(())
}

/// Create a directory on the FATX volume if it doesn't already exist.
/// Errors out if the path resolves to a regular file.
fn ensure_fatx_dir<T>(vol: &mut FatxVolume<T>, path: &str) -> Result<()>
where
    T: Read + Seek + Write,
{
    match vol.create_directory(path) {
        Ok(()) => Ok(()),
        Err(FatxError::FileExists(_)) => {
            let existing = vol.resolve_path(path)?;
            if !existing.is_directory() {
                return Err(FatxError::NotADirectory(path.to_string()));
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopSink;

    impl GodSink for NoopSink {
        fn begin(&mut self, _source: &PreparedSource) -> Result<()> {
            unreachable!("zero-part source should fail before sink begin")
        }

        fn write_part<'a>(
            &mut self,
            _source: &PreparedSource,
            _part_index: u64,
            _opts: &mut ConvertOptions<'a>,
        ) -> Result<()> {
            unreachable!("zero-part source should fail before part writes")
        }

        fn read_master_hash(
            &mut self,
            _source: &PreparedSource,
            _part_index: u64,
        ) -> Result<HashList> {
            unreachable!("zero-part source should fail before hash reads")
        }

        fn write_master_hash(
            &mut self,
            _source: &PreparedSource,
            _part_index: u64,
            _mht: &HashList,
        ) -> Result<()> {
            unreachable!("zero-part source should fail before hash writes")
        }

        fn last_part_size(&self, _source: &PreparedSource) -> Result<u64> {
            unreachable!("zero-part source should fail before header build")
        }

        fn write_con_header(
            &mut self,
            _source: &PreparedSource,
            _con_bytes: Vec<u8>,
        ) -> Result<()> {
            unreachable!("zero-part source should fail before header write")
        }
    }

    #[test]
    fn run_conversion_rejects_zero_part_sources() {
        let source = PreparedSource {
            report: ConvertReport {
                title_id: 0,
                media_id: 0,
                content_type: ContentType::GamesOnDemand,
                part_count: 0,
                block_count: 0,
                data_size: 0,
            },
            exe_info: crate::executable::TitleExecutionInfo {
                media_id: 0,
                version: 0,
                base_version: 0,
                title_id: 0,
                platform: 0,
                executable_type: 0,
                disc_number: 0,
                disc_count: 0,
            },
            content_type: ContentType::GamesOnDemand,
            reader: ReaderSource::Raw {
                source_iso: PathBuf::from("/tmp/zero-part.iso"),
                root_offset: 0,
            },
        };
        let mut sink = NoopSink;
        let mut opts = ConvertOptions {
            trim: TrimMode::Compact,
            game_title: None,
            dry_run: false,
            progress: None,
            should_abort: None,
        };

        let err = run_conversion(&source, &mut sink, &mut opts, "convert_iso");
        assert!(
            matches!(err, Err(FatxError::Other(msg)) if msg.contains("source has no data")),
            "zero-part source should be rejected before any sink work"
        );
    }
}
