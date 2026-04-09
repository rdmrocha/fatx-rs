# File Hygiene

## Temporary & Generated Files

All LLM-generated files that are NOT intended to be committed to the repo MUST be placed in `.tmp/` at the project root. This directory is gitignored.

This includes:
- Shell scripts used for one-off operations (delete after use)
- Scratch files, debug output, intermediate analysis
- Test images or data files created during debugging
- Agent RPC request/response files (already handled by `.agent/`)

**Never** place temporary files in the project root, `src/`, or any other tracked directory.

**After use:** Delete temporary files from `.tmp/` when they are no longer needed. Don't let the directory accumulate stale artifacts.

## Build Artifacts

Build artifacts go in `target/` (managed by Cargo, already gitignored). Never manually place files there.
