# fatx-rs — Project Rules

## Overview
Rust program for reading/writing FATX/XTAF file systems on Xbox/Xbox 360 formatted drives connected via USB to macOS.

## Architecture
- **Cargo workspace**: `fatxlib` (library crate) + `fatx-cli` (binary crate, root)
- **fatxlib**: Core FATX/XTAF volume implementation, types, partition detection, platform I/O
- **fatx-cli**: CLI interface with clap subcommands, `--json` output mode, TUI browser (ratatui)

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

### macOS Raw Device I/O
- Raw devices (`/dev/rdiskN`) require ALL I/O to be 512-byte sector-aligned
- `seek(SeekFrom::End(0))` returns 0 for raw block devices; use platform ioctls instead
- The `read_at`/`write_at` methods in volume.rs handle sector alignment transparently

## Development Workflow

### Building
```bash
cargo build --release
```
The binary lands at `target/release/fatx-cli`.

### Testing
```bash
cargo test --workspace
```
Integration tests in `fatxlib/tests/integration.rs` use in-memory Cursor-based FATX images (little-endian).

### Agent (Claude ↔ Drive Bridge)
A file-based RPC agent (`/.agent/agent.sh`) runs on the Mac with sudo, watching for `request.json`, executing `fatx-cli --json`, and writing `response.json`. The sandbox helper is at `/sessions/zealous-busy-pascal/fatx-cmd.sh`. Agent state files are gitignored.

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
