# fatx-rs

A Rust command-line tool for reading and writing FATX and XTAF file systems used by Xbox and Xbox 360 consoles. Connect a drive via USB to macOS and browse, extract, or modify files directly.

## Supported Formats

- **FATX** (Original Xbox) -- Little-endian on-disk format
- **XTAF** (Xbox 360) -- Big-endian on-disk format

Both FAT16 and FAT32 variants are handled automatically based on cluster count.

## Features

- Automatic partition detection at standard Xbox and Xbox 360 offsets
- Full read/write support: list directories, read files, write files, create directories, delete, rename
- JSON output mode (`--json`) for scripting and programmatic use
- Interactive guided mode when run with no arguments
- TUI file browser for navigating volumes visually (ratatui-based)
- Hex dump for low-level debugging
- Sector-aligned I/O for macOS raw block devices (`/dev/rdiskN`)
- Endian-aware handling of all on-disk structures

## Building

Requires Rust (stable). Build with:

```bash
cargo build --release
```

The binary is written to `target/release/fatx-cli`.

## Usage

### Interactive Mode

Run with no arguments for a guided walkthrough that detects your drive and lets you choose a partition:

```bash
sudo ./target/release/fatx-cli
```

### Scan for Partitions

```bash
sudo ./target/release/fatx-cli scan /dev/rdisk4
```

This probes standard Xbox partition offsets and reports which ones have valid FATX/XTAF headers.

### List Files

```bash
sudo ./target/release/fatx-cli ls --partition "360 Data" /dev/rdisk4 /
sudo ./target/release/fatx-cli ls -l --partition "360 Data" /dev/rdisk4 /Apps
```

### Read a File

```bash
# Print base64-encoded content (useful with --json)
sudo ./target/release/fatx-cli read --partition "360 Data" /dev/rdisk4 /name.txt

# Extract to a local file
sudo ./target/release/fatx-cli read --partition "360 Data" /dev/rdisk4 /name.txt -o name.txt
```

### Write a File

```bash
sudo ./target/release/fatx-cli write --partition "360 Data" /dev/rdisk4 /hello.txt -i hello.txt
```

### Create a Directory

```bash
sudo ./target/release/fatx-cli mkdir --partition "360 Data" /dev/rdisk4 /MyFolder
```

### Delete a File or Directory

```bash
sudo ./target/release/fatx-cli rm --partition "360 Data" /dev/rdisk4 /hello.txt
```

### Rename

```bash
sudo ./target/release/fatx-cli rename --partition "360 Data" /dev/rdisk4 /old.txt new.txt
```

### Volume Info

```bash
sudo ./target/release/fatx-cli info --partition "360 Data" /dev/rdisk4
```

Shows FAT type, cluster size, total/used/free space, and cluster counts.

### TUI Browser

```bash
sudo ./target/release/fatx-cli browse /dev/rdisk4
```

Opens an interactive terminal UI for navigating the filesystem.

### JSON Output

Add `--json` to any command for machine-readable output:

```bash
sudo ./target/release/fatx-cli --json ls --partition "360 Data" /dev/rdisk4 /
```

### Hex Dump

```bash
sudo ./target/release/fatx-cli hexdump /dev/rdisk4 --offset 0x80080000 --count 512
```

## macOS Notes

Raw device access requires `sudo`. Use `/dev/rdiskN` (the raw device node), not `/dev/diskN`. Identify your drive with `diskutil list` -- look for the disk that macOS shows as unformatted.

## Project Structure

This is a Cargo workspace with two crates:

- `fatxlib` -- Library crate with the core FATX/XTAF volume implementation, partition detection, type definitions, and platform I/O
- `fatx-cli` (root) -- Binary crate with the CLI interface, JSON output, and TUI browser

## Testing

Unit and integration tests use in-memory FATX images and run anywhere:

```bash
cargo test --workspace
```

Hardware tests verify against a real Xbox 360 formatted drive. See `fatxlib/tests/TESTING.md` for setup instructions, including how to collect reference data from the Xbox via FTP.

## License

See LICENSE file for details.
