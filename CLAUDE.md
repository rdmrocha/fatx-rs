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
- Write buffering: NFS writes accumulate in `dirty_files` HashMap in memory (sub-millisecond). A periodic flush task (every 5s) batch-writes dirty files to disk and flushes the FAT. This prevents the catastrophic slowdown where each 128KB NFS chunk triggered a full file delete+recreate+231MB FAT flush.
- macOS metadata files (.DS_Store, ._, .Spotlight-V100, .Trashes, .fseventsd) are allowed through the NFS mount (Finder manages its own metadata). CLI `copy` and TUI upload automatically skip these files. The `fatx cleanup` command can scan and remove them from existing volumes.
- Mount options include `soft,intr,retrans=2,timeo=10` to prevent macOS from hanging on stale NFS mounts
- **CRITICAL**: Shutdown must unmount BEFORE stopping the NFS server. If the server dies first, umount hangs, Finder freezes, and the user may need to reboot. The signal handler on a dedicated thread handles this.
- Auto-mount is OFF by default (`--mount` to enable). This prevents stale mount disasters during development.
- `--cleanup` flag kills stale mount_nfs processes and force-unmounts localhost NFS mounts

### ⚠️ KNOWN CATASTROPHIC FAILURE: Stale NFS Mount Deadlock (2025-04-08)
**Symptoms**: Finder won't launch. Force Quit doesn't list Finder. `open -a Finder` returns but Finder never appears. `sudo umount -f /Volumes/Xbox\ Drive` hangs. `ls /Volumes/` hangs. ANY command touching the mount path hangs. Even `sudo rm -rf` hangs.

**Root Cause**: fatx-mount NFS server died (killed, crashed, or hung overnight during a long transfer) while Finder had the volume open. The kernel NFS client entered an uninterruptible wait (D-state) trying to talk to the dead server. Once in this state:
1. Any process that touches the mount path blocks in the kernel (uninterruptible, cannot be killed)
2. Finder tries to enumerate /Volumes/ on launch → blocked → never starts
3. `umount -f` tries to access the mount → blocked
4. Even `ls /Volumes/` blocks because it stats every entry including the dead mount
5. macOS has no `umount -l` (lazy unmount) equivalent — **only a reboot clears it**

**Key finding**: `/sbin/mount -t nfs` returned EMPTY (the mount was already gone from the mount table) but the mountpoint directory still caused kernel hangs. The stale *directory* at `/Volumes/Xbox Drive` was the problem, not a registered mount.

**Prevention (MUST be implemented)**:
1. Watchdog: if NFS write operations stall for >30s, auto-shutdown (unmount + exit)
2. Startup cleanup: on launch, force-unmount and rm any leftover `/Volumes/Xbox Drive` before creating a new mount
3. Heartbeat: periodic check that the mount is responsive; if not, trigger clean shutdown
4. Never let the NFS server die without unmounting first — ALL exit paths must unmount

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

### Bug-Driven Testing Rule
**Every bug fix MUST include a regression test.** When a bug is found — whether from user reports, logs, or code review — write a test that reproduces the failure BEFORE fixing it, then verify the fix makes it pass. This applies to all crates. No exceptions. Claude should do this automatically without being asked.

Test locations:
- `fatxlib` bugs → `fatxlib/tests/integration.rs`
- `fatx-mkimage` bugs → `fatx-mkimage/src/main.rs` (in `#[cfg(test)] mod tests`)
- `fatx-mount` bugs → `fatx-mount/src/main.rs` (in `#[cfg(test)] mod tests`)
- CLI bugs → `tests/cli_integration.rs`

### Agent (Claude ↔ Drive Bridge)
A file-based RPC agent (`/.agent/agent.sh`) runs on the Mac with sudo, watching for `request.json`, executing `fatx --json`, and writing `response.json`. The sandbox helper is at `/sessions/zealous-busy-pascal/fatx-cmd.sh`. Agent state files are gitignored. When using shell scripts via the agent (placed in `.tmp/`), delete them after use to keep the directory clean. **All LLM-generated temporary files MUST go in `.tmp/`** — see `.claude/rules/file-hygiene.md`.

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
