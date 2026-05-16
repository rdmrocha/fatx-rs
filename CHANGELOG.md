# Changelog

All notable changes to `xtafkit` will be documented in this file.

## [1.1.0] - 2026-05-16

First release under the `xtafkit` name. Forked from
[joshuareisbord/fatx-rs](https://github.com/joshuareisbord/fatx-rs); the
FATX/XTAF filesystem core traces back to that work. Everything below was
added or rewritten in the fork.

### Project identity
- Crate + binary renamed to `xtafkit` (was `fatx-cli` + `fatx`). Library crate `fatxlib` retained.
- NOTICE rewritten with dual attribution (xtafkit + upstream fatx-rs), both Apache 2.0.
- Cache file paths migrated from `~/.config/fatx-rs/` to `~/.config/xtafkit/`.
- Devcontainer name + workdir, githook header, integration-test header brought in line with the new name.

### Pre-fork work carried in
- NFS performance: extracted common NFS flushing/cache behavior to reduce redundant disk hits on read paths.
- `fatx copy` directory semantics + `create_file` data integrity: regression tests added first, then the fix; plus a `type_complexity` clippy cleanup.
- macOS metadata cleanup, guided mount/browse, dry-run safety for destructive commands, TUI cleanup, ANSI color compatibility.

### Title resolution & folder display
- Compiled-in title catalog of ~5,500 entries, merged at build time from two community sources: [AdrianCassar's Xbox 360 gist](https://gist.github.com/AdrianCassar/c0d05a14608168259232b3ed8c77f28c) and [jeltaqq's Original Xbox list](https://github.com/jeltaqq/Xbox-Original-GameList).
- Smart conflict normalization (whitespace, subtitle markers, etc.). Conflict report written to `target/<…>/title_conflicts.txt` (gitignored), not the build log.
- Slot-aware folder display in `fatxlib::display`: `Xuid` / `TitleId` / `ContentType` / `StfsFile` / `File`. Format: `<name> [<raw>]`, with raw case preserved.
- Content-type folder labels from the free60 STFS spec.
- All-zeros XUID labeled `Shared`.
- TUI keybinding cleanup: `m` is mkdir, `→` enters folders (mirroring `←` for go-up), top-of-file keymap comment regenerated.

### On-demand STFS + profile gamertag extraction
- STFS header parser (`CON ` / `LIVE` / `PIRS`) in `fatxlib::stfs`.
- On-demand title resolution: when the catalog misses, parse the STFS header of a file inside the unresolved title folder.
- Per-file STFS resolution for Arcade (`0x000D0000`), XNA (`0x000E0000`), Marketplace (`0x00000002`), and Installer (`0x000B0000`) content types.
- Profile gamertag extraction: decrypts the embedded Account file (ARC4 + HMAC-SHA1) using the public PROD + OTHER keys to recover the real gamertag from profile XUID folders.
- TUI: `R` keybinding (slot-aware resolve, dispatches to title-resolve / bulk-scan / single-file), `?` marker on unresolvable entries, sort toggle `s` (by name ⇄ by ID, with bracket-order flip).
- Three persistent caches at `~/.config/xtafkit/`: `user_titles.txt`, `user_files.txt`, `user_profiles.txt`. Plain text, human-editable, self-healing on load.
- Diagnostic helper: `cargo run -p fatxlib --example check_profile -- <file>` to inspect gamertag extraction against a raw STFS file.

### NFS hardening
- Fix NFS write recheck-race panic.
- Remove stale path-based write-session API; align tests with entry-based writes.
- Move NFS dirty-buffer seed reads off the async runtime thread to prevent runtime starvation.
- Reject corrupt FAT next pointers and cyclic cluster chains instead of silently reading garbage.
- Reject FATX rename collisions instead of creating duplicate directory entries.
- NFS exclusive create no longer truncates existing files.
- Flush deferred writes by stable file identity rather than stale paths.
- Keep deferred overwrite sessions unpublished until commit; cancel-rollback regression test added.
- Roll back failed directory creates and parent directory expansions.
- `cargo fmt` + `clippy` pass.

### Scope simplification & TUI-first
- Removed the NFS Finder-mount server entirely, along with the catastrophic stale-mount deadlock that haunted it.
- Dropped 10+ CLI subcommands now subsumed by the TUI: `read`, `write`, `mkdir`, `rm`, `rmr`, `rename`, `copy`, `info`, `hexdump`, `cleanup`, `mount`, `shell`.
- Dropped the interactive numbered-menu shell mode; no-args entry point now lands you in the TUI via guided picker.
- New CLI surface: `browse`, `ls`, `scan`, `mkimage`, `resolve` (5 subcommands, down from 15+).
- TTY-aware `ls` output: text when stdout is a terminal, JSON when piped or redirected. New `--text` / `--json` flags force either mode.
- Auto-pick single disk in the guided no-args flow — if exactly one external disk is detected, skip the picker and use it directly.
- ~3,000 LOC removed, 10 runtime dependencies dropped (including `nfsserve`, `tokio`, `async-trait`, `parking_lot`, `quick_cache`, `bytes`, `ctrlc`, `core-foundation`, `core-foundation-sys`, `io-kit-sys`, `mach2`).

### Toolchain & dependencies
- Rust edition bumped from 2021 to 2024.
- Major dependency bumps: `rand` 0.8 → 0.10, `nix` 0.29 → 0.31, `phf` 0.11 → 0.13, `phf_codegen` 0.11 → 0.13, `hmac` 0.12 → 0.13, `sha1` 0.10 → 0.11. Smaller bumps via `cargo update`.

### Infrastructure
- macOS release pipeline: GitHub Actions builds `x86_64-apple-darwin` and `aarch64-apple-darwin` binaries on tag push, generates a draft release whose notes are sourced from this changelog.
