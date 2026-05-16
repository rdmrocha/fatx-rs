use std::io::{Read, Seek, SeekFrom, Write};

use crate::error::{FatxError, Result};

mod con_header;
pub use con_header::*;

mod file_layout;
pub use file_layout::*;

mod gdf_sector;
pub use gdf_sector::*;

mod hash_list;
pub use hash_list::*;

pub const BLOCKS_PER_PART: u64 = 0xa1c4;
pub const BLOCKS_PER_SUBPART: u64 = 0xcc;
pub const BLOCK_SIZE: u64 = 0x1000;
pub const SUBPARTS_PER_PART: u32 = 0xcb;
pub const SUBPART_SIZE: u64 = BLOCK_SIZE * BLOCKS_PER_SUBPART;

pub fn write_part<R: Read + Seek, W: Write + Seek>(
    mut data_volume: R,
    part_index: u64,
    mut part_file: W,
) -> Result<()> {
    data_volume
        .seek_relative((part_index * BLOCKS_PER_PART * BLOCK_SIZE) as i64)
        .map_err(FatxError::Io)?;

    let mut master_hash_list = HashList::new();

    let master_hash_list_position = part_file.stream_position().map_err(FatxError::Io)?;
    master_hash_list.write(&mut part_file)?;

    // Pre-allocated subpart buffer — avoids `take + read_to_end`'s repeated
    // grow/check ceremony and the Vec-append work that came with it. We read
    // straight into a fixed-size buffer and slice off the actual length.
    let mut subpart_buf = vec![0u8; SUBPART_SIZE as usize];

    for _subpart_index in 0..SUBPARTS_PER_PART {
        // Fill subpart_buf one read at a time. The last subpart may be
        // short — that's fine, we slice with `got` below.
        let mut got = 0usize;
        while got < subpart_buf.len() {
            let n = data_volume
                .read(&mut subpart_buf[got..])
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

        let mut sub_hash_list = HashList::new();

        for block in subpart.chunks(BLOCK_SIZE as usize) {
            sub_hash_list.add_block_hash(block);
        }

        sub_hash_list.write(&mut part_file)?;
        master_hash_list.add_block_hash(sub_hash_list.bytes());

        // Write the subpart we already buffered. An earlier shape
        // seeked back and re-read via `io::copy` (a `reflink` hint for
        // CoW filesystems), but APFS doesn't honor reflink on partial-
        // file writes — the re-read just doubled I/O without benefit.
        part_file.write_all(subpart).map_err(FatxError::Io)?;

        if got < SUBPART_SIZE as usize {
            break;
        }
    }

    part_file
        .seek(SeekFrom::Start(master_hash_list_position))
        .map_err(FatxError::Io)?;
    master_hash_list.write(&mut part_file)?;

    Ok(())
}
