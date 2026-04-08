# fatx-rs — Project Rules

## Overview
Rust toolkit for reading, writing, and mounting FATX/XTAF file systems on Xbox/Xbox 360 formatted drives connected via USB to macOS. Includes a Finder-mountable NFS server and test image generator.

## Architecture
- **Cargo workspace** with four crates:
  - `fatxlib` — Library crate with core FATX/XTAF volume implementation, types, partition detection, platform I/O
  - `fatx-cli` (root) — Main binary (`fatx`) with CLI interface, clap subcommands, `--json` output mode, TUI browser (ratatui). Dispatches `mount` and `mkimage` subcommands to their respective binaries.
  - `fatx-mount` — NFS mount server binary (`fatx-mount`). Invoked via `fatx mount`. Uses tokio + nfsserve to serve FATX volumes over NFSv3 on localhost.
  - `fatx-mkimage` — Test image generator binary (`fatx-mkimage`). Invoked via `fatx mkimage`. Creates properly formatted FATX/XTAF disk images.

## Key Technical Details

### Endianness
- **FATX** (Original Xbox): Little-endian on-disk format. Magic: `46 41 54 58` ("FATX")
- **XTAF** (Xbox 360): **Big-endian** on-disk format. Magic: `58 54 41 46` ("XTAF")
- The `big_endian` field on `FatxVolume` controls byte order for ALL on-disk fields: superblock, FAT entries, and directory entries
- Always use the endian-aware helpers (`read_u16`, `read_u32`, `write_u16_bytes`, `write_u32_bytes`) — never raw `from_le_bytes`/`from_be_bytes` outside of those helpers

### Disk Format
- 4KB superblock, single FAT copy, 64-byte directory entries, 42-char filename max
- FAT16 (< 65,520 clusters) vs FAT32 (larger partitions)
- FAT size rounded UP to 4KB boundary
- Xbox 360 partition offsets: Game Content @ 0x80080000, Data @ 0x130EB0000
- **XTAF cluster count**: Xbox 360 uses `(partition_size - superblock) / cluster_size` — it does NOT subtract FAT space. Using the wrong formula shifts data_offset on large partitions.
- **XTAF timestamp layout**: Directory entry offsets 52-55 store `date(2) + time(2)` (date first), whereas FATX stores `time(2) + date(2)` (time first). Same packed FAT format, just swapped field order. Timestamps are stored in UTC.

### macOS Raw Device I/O
- Raw devices (`/dev/rdiskN`) require ALL I/O to be 512-byte sector-aligned
- `seek(SeekFrom::End(0))` returns 0 for raw block devices; use platform ioctls instead
- The `read_at`/`write_at` methods in volume.rs handle sector alignment transparently

### NFS Mount (fatx-mount)
- Uses `tokio::task::spawn_blocking` for all FATX volume I/O — blocking USB reads must NOT run on the async event loop or the NFS server freezes
- File data cache (`file_cache`) and directory cache (`dir_cache`) avoid redundant USB reads. NFS reads come in 128KB chunks; without caching, each chunk re-reads the entire file.
- macOS metadata files (.DS_Store, ._, .Spotlight-V100, .Trashes, .fseventsd) are blocked from creation
- Mount options include `soft,intr,retrans=2,timeo=10` to prevent macOS from hanging on stale NFS mounts
- **CRITICAL**: Shutdown must unmount BEFORE stopping the NFS server. If the server dies first, umount hangs, Finder freezes, and the user may need to reboot. The signal handler on a dedicated thread handles this.
- Auto-mount is OFF by default (`--mount` to enable). This prevents stale mount disasters during development.
- `--cleanup` flag kills stale mount_nfs processes and force-unmounts localhost NFS mounts

## Development Workflow

### Building
```bash
cargo build --release
```
Produces three binaries in `target/release/`: `fatx`, `fatx-mount`, `fatx-mkimage`.

### Testing
```bash
cargo test --workspace
```
Integration tests in `fatxlib/tests/integration.rs` use in-memory Cursor-based FATX images (little-endian).

For NFS mount testing, use a file-backed test image instead of a real drive:
```bash
fatx mkimage test.img --size 1G --populate
sudo fatx mount test.img --trace
```

### Agent (Claude ↔ Drive Bridge)
A file-based RPC agent (`/.agent/agent.sh`) runs on the Mac with sudo, watching for `request.json`, executing `fatx --json`, and writing `response.json`. The sandbox helper is at `/sessions/zealous-busy-pascal/fatx-cmd.sh`. Agent state files are gitignored. When using shell scripts via the agent (placed in `.tmp/`), delete them after use to keep the directory clean.

### Test drive
- 1TB Xbox 360 formatted drive at `/dev/rdisk4` (may change between sessions — verify with `diskutil list`)
- Two XTAF partitions: "360 Game Content" and "360 Data"

## Git Conventions
- **Default branch**: `main`
- **Working branch**: `develop`
- Commit and push at each milestone (working feature, major fix, etc.)
- Keep `.agent/response.json`, `.agent/request.json`, and `.agent/processing` in `.gitignore`

## Future Work (Deferred)
- TUI file browser polish (ratatui-based, code exists in `src/tui.rs`)
- Possible Yazi-based UI integration
