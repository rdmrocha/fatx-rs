# fatx-rs

A Rust toolkit for reading, writing, and mounting FATX and XTAF file systems used by Xbox and Xbox 360 consoles. Connect a drive via USB to macOS and browse, extract, modify, or mount files directly in Finder.

## Supported Formats

- **FATX** (Original Xbox) -- Little-endian on-disk format
- **XTAF** (Xbox 360) -- Big-endian on-disk format

Both FAT16 and FAT32 variants are handled automatically based on cluster count.

## Features

- Automatic partition detection at standard Xbox and Xbox 360 offsets
- Full read/write support: list directories, read files, write files, create directories, delete, rename
- **NFS mount** — mount FATX volumes in Finder via a local NFS server
- **Test image creation** — generate blank FATX disk images for testing without hardware
- JSON output mode (`--json`) for scripting and programmatic use
- Interactive guided mode when run with no arguments
- TUI file browser for navigating volumes visually (ratatui-based)
- File data and directory caching for fast repeated reads
- Hex dump for low-level debugging
- Sector-aligned I/O for macOS raw block devices (`/dev/rdiskN`)
- Endian-aware handling of all on-disk structures

## Install

One-liner for macOS (downloads the latest release):

```bash
curl -fsSL https://raw.githubusercontent.com/joshuareisbord/fatx-rs/main/install.sh | bash
```

Or install a specific version:

```bash
FATX_VERSION=v0.2.1-alpha bash install.sh
```

### Build from Source

Requires Rust (stable). Clone and run the setup script:

```bash
git clone https://github.com/joshuareisbord/fatx-rs.git
cd fatx-rs
bash setup.sh
```

Or build manually:

```bash
cargo build --release
```

This produces a single entry point at `target/release/fatx`. All functionality is accessed through subcommands.

## Usage

### Interactive Mode

Run with no arguments for a guided walkthrough that detects your drive and lets you choose a partition:

```bash
sudo ./target/release/fatx
```

### Scan for Partitions

```bash
sudo fatx scan /dev/rdisk4
```

This probes standard Xbox partition offsets and reports which ones have valid FATX/XTAF headers.

### List Files

```bash
sudo fatx ls --partition "360 Data" /dev/rdisk4 /
sudo fatx ls -l --partition "360 Data" /dev/rdisk4 /Apps
```

### Read a File

```bash
# Print base64-encoded content (useful with --json)
sudo fatx read --partition "360 Data" /dev/rdisk4 /name.txt

# Extract to a local file
sudo fatx read --partition "360 Data" /dev/rdisk4 /name.txt -o name.txt
```

### Write a File

```bash
sudo fatx write --partition "360 Data" /dev/rdisk4 /hello.txt -i hello.txt
```

### Create a Directory

```bash
sudo fatx mkdir --partition "360 Data" /dev/rdisk4 /MyFolder
```

### Delete a File or Directory

```bash
sudo fatx rm --partition "360 Data" /dev/rdisk4 /hello.txt
```

### Rename

```bash
sudo fatx rename --partition "360 Data" /dev/rdisk4 /old.txt new.txt
```

### Volume Info

```bash
sudo fatx info --partition "360 Data" /dev/rdisk4
```

Shows FAT type, cluster size, total/used/free space, and cluster counts.

### TUI Browser

```bash
sudo fatx browse /dev/rdisk4
```

Opens an interactive terminal UI for navigating the filesystem.

### Mount in Finder

Mount a FATX volume so it appears as a regular drive in Finder:

```bash
# Start NFS server only (safe, no Finder mount)
sudo fatx mount /dev/rdisk4 --partition "360 Data" -v

# Start NFS server and mount in Finder
sudo fatx mount /dev/rdisk4 --partition "360 Data" -v --mount

# Mount a test image
sudo fatx mount test.img --trace

# Emergency cleanup if a mount goes stale
sudo fatx mount --cleanup
```

The mount uses a local NFSv3 server with `soft,intr` options so macOS won't hang if the server stops. Ctrl+C cleanly unmounts before exiting.

### Create Test Images

Generate blank FATX disk images for testing without hardware:

```bash
# 1 GB FATX image with sample content
fatx mkimage test.img --size 1G --populate

# 512 MB Xbox 360 (XTAF) image
fatx mkimage test360.img --size 512M --format xtaf

# Overwrite existing image
fatx mkimage test.img --size 1G --populate --force
```

### JSON Output

Add `--json` to any command for machine-readable output:

```bash
sudo fatx --json ls --partition "360 Data" /dev/rdisk4 /
```

### Hex Dump

```bash
sudo fatx hexdump /dev/rdisk4 --offset 0x80080000 --count 512
```

## macOS Notes

Raw device access requires `sudo`. Use `/dev/rdiskN` (the raw device node), not `/dev/diskN`. Identify your drive with `diskutil list` -- look for the disk that macOS shows as unformatted.

## Project Structure

This is a Cargo workspace:

- `fatxlib` -- Library crate with the core FATX/XTAF volume implementation, partition detection, type definitions, and platform I/O
- `fatx-cli` (root) -- Main binary (`fatx`) with the CLI interface, JSON output, TUI browser, and subcommand dispatch
- `fatx-mount` -- NFS mount server (invoked via `fatx mount`)
- `fatx-mkimage` -- Test image generator (invoked via `fatx mkimage`)

## Testing

Unit and integration tests use in-memory FATX images and run anywhere:

```bash
cargo test --workspace
```

Hardware tests verify against a real Xbox 360 formatted drive. See `fatxlib/tests/TESTING.md` for setup instructions, including how to collect reference data from the Xbox via FTP.

## License

See LICENSE file for details.
